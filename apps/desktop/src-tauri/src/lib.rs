use magnetic_tape_audio::{
    AudioLevels, AudioRecording, ChunkInfo, RecordingSnapshot, Track, wav_duration_ms,
};
use magnetic_tape_core::{
    manifest::{ChunkMetadata, RecoveryManifest, SessionStatus},
    recovery::RecoveryStore,
    timeline::{AudioSource, TimelineSpan},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

mod ai;
use ai::AiState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecorderStatus {
    Idle,
    Recording,
    Paused,
    Finished,
}

const COMPLETED_MARKER: &str = "session-completed";
const RECOVERED_MARKER: &str = "session-recovered";
const LEGACY_MARKER: &str = "session-recording.json";
const TITLE_FILE: &str = "session-title.txt";
const NOTES_FILE: &str = "meeting-notes.md";

fn default_session_title() -> String {
    "新录音".into()
}

/// Marker written by builds that recorded two plain WAV files. Kept so the
/// library can still open sessions recorded before chunked Opus storage.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySessionMarker {
    started_at_epoch_ms: u64,
    #[serde(default = "default_session_title")]
    title: String,
}

/// Audio paths and duration for a finished, recovered, or opened session.
#[derive(Debug, Clone, Default)]
struct RecordedSession {
    microphone_chunks: Vec<PathBuf>,
    system_chunks: Vec<PathBuf>,
    duration_ms: u64,
    dropped_blocks: u64,
}

impl RecordedSession {
    fn first_microphone(&self) -> Option<PathBuf> {
        self.microphone_chunks.first().cloned()
    }

    fn first_system(&self) -> Option<PathBuf> {
        self.system_chunks.first().cloned()
    }

    fn is_empty(&self) -> bool {
        self.microphone_chunks.is_empty() && self.system_chunks.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionView {
    status: RecorderStatus,
    session_id: Option<String>,
    started_at_epoch_ms: Option<u64>,
    elapsed_ms: u64,
    microphone_ready: bool,
    system_audio_ready: bool,
    microphone_path: Option<PathBuf>,
    system_path: Option<PathBuf>,
    microphone_chunks: Vec<PathBuf>,
    system_chunks: Vec<PathBuf>,
    dropped_blocks: u64,
    recovered: bool,
    title: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordingListItem {
    session_id: String,
    title: String,
    started_at_epoch_ms: u64,
    duration_ms: u64,
    status: &'static str,
    microphone_path: Option<PathBuf>,
    system_path: Option<PathBuf>,
    size_bytes: u64,
}

struct SessionState {
    status: RecorderStatus,
    session_id: Option<String>,
    started_at_epoch_ms: Option<u64>,
    resumed_at_epoch_ms: Option<u64>,
    accumulated_ms: u64,
    recorded: Option<RecordedSession>,
    recovered: bool,
    title: String,
    checkpointer: Option<JoinHandle<()>>,
    sequence: Arc<AtomicU64>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            status: RecorderStatus::Idle,
            session_id: None,
            started_at_epoch_ms: None,
            resumed_at_epoch_ms: None,
            accumulated_ms: 0,
            recorded: None,
            recovered: false,
            title: default_session_title(),
            checkpointer: None,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Shared audio recorder handle. The checkpoint thread keeps its own `Arc`
/// while a session is active.
struct RecorderState(Arc<Mutex<Option<AudioRecording>>>);

impl SessionState {
    fn view(&self, now_ms: u64) -> SessionView {
        let active_ms = if self.status == RecorderStatus::Recording {
            now_ms.saturating_sub(self.resumed_at_epoch_ms.unwrap_or(now_ms))
        } else {
            0
        };
        let active = matches!(
            self.status,
            RecorderStatus::Recording | RecorderStatus::Paused
        );
        let recorded = self.recorded.as_ref();
        SessionView {
            status: self.status,
            session_id: self.session_id.clone(),
            started_at_epoch_ms: self.started_at_epoch_ms,
            elapsed_ms: self.accumulated_ms.saturating_add(active_ms),
            microphone_ready: active
                || recorded.is_some_and(|session| !session.microphone_chunks.is_empty()),
            system_audio_ready: active
                || recorded.is_some_and(|session| !session.system_chunks.is_empty()),
            microphone_path: recorded.and_then(RecordedSession::first_microphone),
            system_path: recorded.and_then(RecordedSession::first_system),
            microphone_chunks: recorded
                .map(|session| session.microphone_chunks.clone())
                .unwrap_or_default(),
            system_chunks: recorded
                .map(|session| session.system_chunks.clone())
                .unwrap_or_default(),
            dropped_blocks: recorded.map_or(0, |session| session.dropped_blocks),
            recovered: self.recovered,
            title: self.title.clone(),
        }
    }

    fn pause(&mut self, now_ms: u64) -> Result<(), &'static str> {
        if self.status != RecorderStatus::Recording {
            return Err("只有录音中的会话可以暂停");
        }
        self.accumulated_ms = self.view(now_ms).elapsed_ms;
        self.resumed_at_epoch_ms = None;
        self.status = RecorderStatus::Paused;
        Ok(())
    }

    fn resume(&mut self, now_ms: u64) -> Result<(), &'static str> {
        if self.status != RecorderStatus::Paused {
            return Err("只有已暂停的会话可以继续");
        }
        self.resumed_at_epoch_ms = Some(now_ms);
        self.status = RecorderStatus::Recording;
        Ok(())
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn recordings_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|error| error.to_string())
        .map(|directory| directory.join("recordings"))
}

fn session_directory(app: &tauri::AppHandle, session_id: &str) -> Result<PathBuf, String> {
    let root = recordings_root(app)?;
    let directory = root.join(session_id);
    if !directory.is_dir() {
        return Err("录音会话目录不存在".into());
    }
    Ok(directory)
}

fn current_session_directory(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, Mutex<SessionState>>,
) -> Result<PathBuf, String> {
    let session_id = state
        .lock()
        .expect("session state poisoned")
        .session_id
        .clone()
        .ok_or_else(|| "当前没有录音会话".to_owned())?;
    session_directory(app, &session_id)
}

fn session_title(directory: &Path, fallback: &str) -> String {
    fs::read_to_string(directory.join(TITLE_FILE))
        .ok()
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty())
        .or_else(|| {
            legacy_marker(directory)
                .map(|marker| marker.title.trim().to_owned())
                .filter(|title| !title.is_empty())
        })
        .unwrap_or_else(|| fallback.to_owned())
}

fn write_session_title(directory: &Path, title: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(directory.join(TITLE_FILE))
        .map_err(|error| error.to_string())?;
    file.write_all(title.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn legacy_marker(directory: &Path) -> Option<LegacySessionMarker> {
    fs::File::open(directory.join(LEGACY_MARKER))
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok())
}

fn latest_manifest(directory: &Path) -> Option<RecoveryManifest> {
    RecoveryStore::open(directory)
        .ok()?
        .load_latest()
        .ok()
        .flatten()
}

fn write_checkpoint(directory: &Path, manifest: &RecoveryManifest) -> Result<(), String> {
    RecoveryStore::open(directory)
        .map_err(|error| error.to_string())?
        .checkpoint(manifest)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn track_of(chunk: &ChunkInfo) -> AudioSource {
    match chunk.track {
        Track::Microphone => AudioSource::Microphone,
        Track::System => AudioSource::System,
    }
}

/// Projects the audio crate output into the durable core manifest.
fn build_manifest(
    session_id: &str,
    started_at_epoch_ms: u64,
    sequence: u64,
    status: SessionStatus,
    snapshot: &RecordingSnapshot,
) -> RecoveryManifest {
    let mut manifest = RecoveryManifest::new(session_id);
    manifest.started_at_epoch_ms = started_at_epoch_ms;
    manifest.checkpoint_sequence = sequence;
    manifest.status = status;
    manifest.committed_timeline_frames = snapshot.microphone_frames.max(snapshot.system_frames);
    for chunk in &snapshot.chunks {
        let source = track_of(chunk);
        let _ = manifest.timeline.push(TimelineSpan {
            source,
            generation: 0,
            source_start_frame: chunk.start_frame,
            timeline_start_frame: chunk.start_frame,
            frame_count: chunk.frames,
        });
        manifest.chunks.push(ChunkMetadata {
            index: chunk.index,
            uuid: chunk.uuid.clone(),
            sample_rate: chunk.sample_rate,
            channels: chunk.channels,
            frame_size: chunk.frame_size,
            duration_ms: chunk.duration_ms,
            packet_count: chunk.packet_count,
            start_epoch_ms: chunk.start_epoch_ms,
            track: source,
        });
    }
    manifest
}

fn recorded_from_snapshot(snapshot: &RecordingSnapshot) -> RecordedSession {
    let mut microphone = Vec::new();
    let mut system = Vec::new();
    for chunk in &snapshot.chunks {
        match chunk.track {
            Track::Microphone => microphone.push((chunk.index, chunk.path.clone())),
            Track::System => system.push((chunk.index, chunk.path.clone())),
        }
    }
    microphone.sort_by_key(|(index, _)| *index);
    system.sort_by_key(|(index, _)| *index);
    RecordedSession {
        microphone_chunks: microphone.into_iter().map(|(_, path)| path).collect(),
        system_chunks: system.into_iter().map(|(_, path)| path).collect(),
        duration_ms: snapshot.duration_ms(),
        dropped_blocks: snapshot.dropped_blocks,
    }
}

fn recorded_from_manifest(
    directory: &Path,
    manifest: &RecoveryManifest,
) -> Option<RecordedSession> {
    let mut microphone = Vec::new();
    let mut system = Vec::new();
    for chunk in &manifest.chunks {
        let path = directory.join(format!("{}.ogg", chunk.uuid));
        if !path.is_file() {
            continue;
        }
        match chunk.track {
            AudioSource::Microphone => microphone.push((chunk.index, path)),
            AudioSource::System => system.push((chunk.index, path)),
            AudioSource::Mixed => {}
        }
    }
    if microphone.is_empty() && system.is_empty() {
        return None;
    }
    microphone.sort_by_key(|(index, _)| *index);
    system.sort_by_key(|(index, _)| *index);
    let duration_ms = if manifest.committed_timeline_frames > 0 {
        manifest.committed_timeline_frames.saturating_mul(1_000)
            / u64::from(magnetic_tape_audio::OPUS_SAMPLE_RATE)
    } else {
        manifest
            .chunks
            .iter()
            .map(|chunk| chunk.duration_ms)
            .max()
            .unwrap_or(0)
    };
    Some(RecordedSession {
        microphone_chunks: microphone.into_iter().map(|(_, path)| path).collect(),
        system_chunks: system.into_iter().map(|(_, path)| path).collect(),
        duration_ms,
        dropped_blocks: 0,
    })
}

/// Rebuilds a session from per-chunk sidecars when the newest checkpoint is
/// missing or older than the last committed chunk.
fn recorded_from_sidecars(directory: &Path) -> Option<RecordedSession> {
    let mut microphone = Vec::new();
    let mut system = Vec::new();
    let mut duration_ms = 0;
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name.starts_with("checkpoint-") || name == LEGACY_MARKER {
            continue;
        }
        let Ok(info) = fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ChunkInfo>(&bytes).ok())
            .ok_or(())
        else {
            continue;
        };
        let path = directory.join(format!("{}.ogg", info.uuid));
        if !path.is_file() {
            continue;
        }
        duration_ms = duration_ms.max(
            info.start_frame.saturating_mul(1_000)
                / u64::from(magnetic_tape_audio::OPUS_SAMPLE_RATE)
                + info.duration_ms,
        );
        match info.track {
            Track::Microphone => microphone.push((info.index, path)),
            Track::System => system.push((info.index, path)),
        }
    }
    if microphone.is_empty() && system.is_empty() {
        return None;
    }
    microphone.sort_by_key(|(index, _)| *index);
    system.sort_by_key(|(index, _)| *index);
    Some(RecordedSession {
        microphone_chunks: microphone.into_iter().map(|(_, path)| path).collect(),
        system_chunks: system.into_iter().map(|(_, path)| path).collect(),
        duration_ms,
        dropped_blocks: 0,
    })
}

fn recorded_from_wav(directory: &Path) -> Option<RecordedSession> {
    let microphone = directory.join("microphone.wav");
    let system = directory.join("system.wav");
    let mut chunks = Vec::new();
    let mut duration_ms = 0;
    if microphone.is_file() {
        duration_ms = duration_ms.max(wav_duration_ms(&microphone).unwrap_or(0));
        chunks.push(microphone);
    }
    let mut system_chunks = Vec::new();
    if system.is_file() {
        duration_ms = duration_ms.max(wav_duration_ms(&system).unwrap_or(0));
        system_chunks.push(system);
    }
    if chunks.is_empty() && system_chunks.is_empty() {
        return None;
    }
    Some(RecordedSession {
        microphone_chunks: chunks,
        system_chunks,
        duration_ms,
        dropped_blocks: 0,
    })
}

struct LoadedSession {
    recorded: RecordedSession,
    started_at_epoch_ms: u64,
}

fn load_session(directory: &Path) -> Option<LoadedSession> {
    let manifest = latest_manifest(directory);
    let recorded = manifest
        .as_ref()
        .and_then(|manifest| recorded_from_manifest(directory, manifest))
        .or_else(|| recorded_from_sidecars(directory))
        .or_else(|| recorded_from_wav(directory))?;
    let started_at_epoch_ms = manifest
        .as_ref()
        .map(|manifest| manifest.started_at_epoch_ms)
        .filter(|value| *value > 0)
        .or_else(|| {
            manifest
                .as_ref()?
                .chunks
                .iter()
                .map(|chunk| chunk.start_epoch_ms)
                .min()
        })
        .or_else(|| legacy_marker(directory).map(|marker| marker.started_at_epoch_ms))
        .unwrap_or(0);
    Some(LoadedSession {
        recorded,
        started_at_epoch_ms,
    })
}

/// Writes the authoritative checkpoint and returns the final snapshot. The
/// checkpoint thread is stopped first so sequences stay monotonic.
fn finalize_session(
    directory: &Path,
    state: &mut SessionState,
    snapshot: &RecordingSnapshot,
) -> RecordedSession {
    if let Some(handle) = state.checkpointer.take() {
        let _ = handle.join();
    }
    let recorded = recorded_from_snapshot(snapshot);
    let sequence = state.sequence.fetch_add(1, Ordering::SeqCst);
    let manifest = build_manifest(
        state.session_id.as_deref().unwrap_or("unknown"),
        state.started_at_epoch_ms.unwrap_or(0),
        sequence,
        SessionStatus::Completed,
        snapshot,
    );
    if let Err(error) = write_checkpoint(directory, &manifest) {
        eprintln!("failed to write final session checkpoint: {error}");
    }
    recorded
}

fn spawn_checkpointer(
    directory: PathBuf,
    session_id: String,
    started_at_epoch_ms: u64,
    recorder: Arc<Mutex<Option<AudioRecording>>>,
    sequence: Arc<AtomicU64>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("magnetic-tape-checkpoint".into())
        .spawn(move || {
            let mut committed = 0_usize;
            loop {
                thread::sleep(Duration::from_millis(500));
                let snapshot = recorder
                    .lock()
                    .expect("audio recorder state poisoned")
                    .as_ref()
                    .map(AudioRecording::snapshot);
                let Some(snapshot) = snapshot else {
                    break;
                };
                if snapshot.chunks.len() <= committed {
                    continue;
                }
                committed = snapshot.chunks.len();
                let sequence = sequence.fetch_add(1, Ordering::SeqCst);
                let manifest = build_manifest(
                    &session_id,
                    started_at_epoch_ms,
                    sequence,
                    SessionStatus::Recording,
                    &snapshot,
                );
                if let Err(error) = write_checkpoint(&directory, &manifest) {
                    eprintln!("failed to write session checkpoint: {error}");
                }
            }
        })
        .expect("cannot spawn session checkpoint thread")
}

#[tauri::command]
fn get_session_notes(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<String, String> {
    let path = current_session_directory(&app, &state)?.join(NOTES_FILE);
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_session_notes(
    content: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<(), String> {
    if content.len() > 2_000_000 {
        return Err("会议笔记不能超过 2 MB".into());
    }
    let path = current_session_directory(&app, &state)?.join(NOTES_FILE);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

#[tauri::command]
fn recover_latest_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<Option<SessionView>, String> {
    let root = recordings_root(&app)?;
    if !root.exists() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        if directory.join(COMPLETED_MARKER).exists() || directory.join(RECOVERED_MARKER).exists() {
            continue;
        }
        let Some(loaded) = load_session(&directory) else {
            continue;
        };
        if loaded.recorded.is_empty() {
            continue;
        }
        candidates.push((loaded.started_at_epoch_ms, directory, loaded.recorded));
    }
    let Some((started_at, directory, recorded)) = candidates
        .into_iter()
        .max_by_key(|(started_at, _, _)| *started_at)
    else {
        return Ok(None);
    };
    write_new_file(&directory.join(RECOVERED_MARKER), b"recovered\n")?;
    let session_id = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| "录音会话目录无效".to_owned())?;
    let title = session_title(&directory, "新录音");
    let mut state = state.lock().expect("session state poisoned");
    *state = SessionState {
        status: RecorderStatus::Finished,
        session_id: Some(session_id),
        started_at_epoch_ms: Some(started_at),
        resumed_at_epoch_ms: None,
        accumulated_ms: recorded.duration_ms,
        recorded: Some(recorded),
        recovered: true,
        title,
        checkpointer: None,
        sequence: Arc::new(AtomicU64::new(0)),
    };
    Ok(Some(state.view(epoch_ms())))
}

#[tauri::command]
fn list_audio_devices() -> Result<magnetic_tape_audio::AudioDeviceSnapshot, String> {
    magnetic_tape_audio::enumerate_audio_devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_audio_levels(recorder: tauri::State<'_, RecorderState>) -> AudioLevels {
    recorder
        .0
        .lock()
        .expect("audio recorder state poisoned")
        .as_ref()
        .map_or_else(Default::default, AudioRecording::levels)
}

#[tauri::command]
fn get_session_state(state: tauri::State<'_, Mutex<SessionState>>) -> SessionView {
    state
        .lock()
        .expect("session state poisoned")
        .view(epoch_ms())
}

fn directory_size(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

#[tauri::command]
fn list_recordings(app: tauri::AppHandle) -> Result<Vec<RecordingListItem>, String> {
    let root = recordings_root(&app)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut recordings = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        let Some(loaded) = load_session(&directory) else {
            continue;
        };
        if loaded.recorded.is_empty() {
            continue;
        }
        let session_id = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = session_title(&directory, &session_id);
        let status = if directory.join(COMPLETED_MARKER).is_file() {
            "completed"
        } else if directory.join(RECOVERED_MARKER).is_file() {
            "recovered"
        } else {
            "interrupted"
        };
        recordings.push(RecordingListItem {
            session_id,
            title,
            started_at_epoch_ms: loaded.started_at_epoch_ms,
            duration_ms: loaded.recorded.duration_ms,
            status,
            microphone_path: loaded.recorded.first_microphone(),
            system_path: loaded.recorded.first_system(),
            size_bytes: directory_size(&directory),
        });
    }
    recordings.sort_by_key(|item| std::cmp::Reverse(item.started_at_epoch_ms));
    Ok(recordings)
}

fn is_session_directory(directory: &Path, root: &Path) -> bool {
    directory.is_dir()
        && directory.starts_with(root)
        && (directory.join(LEGACY_MARKER).is_file()
            || directory.join(TITLE_FILE).is_file()
            || fs::read_dir(directory)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "ogg")))
}

#[tauri::command]
fn trash_recording(
    session_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<(), String> {
    if session_id.is_empty() || !session_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("无效的录音会话编号".into());
    }
    {
        let state = state.lock().expect("session state poisoned");
        if state.session_id.as_deref() == Some(&session_id)
            && matches!(
                state.status,
                RecorderStatus::Recording | RecorderStatus::Paused
            )
        {
            return Err("录音进行中，不能删除当前会话".into());
        }
    }
    let root = recordings_root(&app)?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let directory = root
        .join(&session_id)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !is_session_directory(&directory, &root) {
        return Err("只能删除 MagneticTape 资料库中的录音".into());
    }
    trash::delete(&directory).map_err(|error| format!("无法移入回收站：{error}"))?;
    let mut state = state.lock().expect("session state poisoned");
    if state.session_id.as_deref() == Some(&session_id) {
        *state = SessionState::default();
    }
    Ok(())
}

#[tauri::command]
fn open_recording(
    session_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<SessionView, String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err("无效的录音会话编号".into());
    }
    let directory = session_directory(&app, &session_id)?;
    let loaded = load_session(&directory).ok_or_else(|| "录音文件不存在".to_owned())?;
    let recovered = !directory.join(COMPLETED_MARKER).is_file();
    if recovered && !directory.join(RECOVERED_MARKER).exists() {
        write_new_file(&directory.join(RECOVERED_MARKER), b"recovered\n")?;
    }
    let now_ms = epoch_ms();
    let title = session_title(&directory, "新录音");
    let mut state = state.lock().expect("session state poisoned");
    if matches!(
        state.status,
        RecorderStatus::Recording | RecorderStatus::Paused
    ) {
        return Err("录音进行中，无法打开历史会话".into());
    }
    *state = SessionState {
        status: RecorderStatus::Finished,
        session_id: Some(session_id),
        started_at_epoch_ms: Some(loaded.started_at_epoch_ms),
        resumed_at_epoch_ms: None,
        accumulated_ms: loaded.recorded.duration_ms,
        recorded: Some(loaded.recorded),
        recovered,
        title,
        checkpointer: None,
        sequence: Arc::new(AtomicU64::new(0)),
    };
    Ok(state.view(now_ms))
}

#[tauri::command]
fn reveal_recording(path: PathBuf, app: tauri::AppHandle) -> Result<(), String> {
    let recordings_root = recordings_root(&app)?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let recording = path.canonicalize().map_err(|error| error.to_string())?;
    if !recording.is_file() || !recording.starts_with(&recordings_root) {
        return Err("只能定位 MagneticTape 录音目录中的文件".into());
    }
    reveal_in_file_manager(&recording)
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开资源管理器：{error}"))
}

#[cfg(not(target_os = "windows"))]
fn reveal_in_file_manager(_path: &Path) -> Result<(), String> {
    Err("当前平台暂不支持在文件管理器中定位录音".into())
}

#[tauri::command]
fn start_session(
    microphone_id: String,
    system_output_id: String,
    title: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, RecorderState>,
) -> Result<SessionView, String> {
    let mut state = state.lock().expect("session state poisoned");
    if matches!(
        state.status,
        RecorderStatus::Recording | RecorderStatus::Paused
    ) {
        return Err("已有活动录音会话".into());
    }
    let now_ms = epoch_ms();
    let session_id = format!("{now_ms:016x}");
    let output = recordings_root(&app)?.join(&session_id);
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;
    let title = {
        let title = title.trim().chars().take(80).collect::<String>();
        if title.is_empty() {
            default_session_title()
        } else {
            title
        }
    };
    write_session_title(&output, &title)?;
    let mut initial = RecoveryManifest::new(&session_id);
    initial.started_at_epoch_ms = now_ms;
    write_checkpoint(&output, &initial)?;

    let audio = AudioRecording::start(&microphone_id, &system_output_id, &output)
        .map_err(|error| error.to_string())?;
    let recorder_handle = Arc::clone(&recorder.0);
    *recorder_handle
        .lock()
        .expect("audio recorder state poisoned") = Some(audio);
    let sequence = Arc::new(AtomicU64::new(1));
    let checkpointer = spawn_checkpointer(
        output,
        session_id.clone(),
        now_ms,
        Arc::clone(&recorder_handle),
        Arc::clone(&sequence),
    );
    *state = SessionState {
        status: RecorderStatus::Recording,
        session_id: Some(session_id),
        started_at_epoch_ms: Some(now_ms),
        resumed_at_epoch_ms: Some(now_ms),
        accumulated_ms: 0,
        recorded: None,
        recovered: false,
        title,
        checkpointer: Some(checkpointer),
        sequence,
    };
    Ok(state.view(now_ms))
}

#[tauri::command]
fn set_session_title(
    title: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<SessionView, String> {
    let title = title.trim().chars().take(80).collect::<String>();
    let title = if title.is_empty() {
        default_session_title()
    } else {
        title
    };
    let mut state = state.lock().expect("session state poisoned");
    let session_id = state
        .session_id
        .as_ref()
        .ok_or_else(|| "当前没有可命名的录音会话".to_owned())?;
    let directory = recordings_root(&app)?.join(session_id);
    write_session_title(&directory, &title)?;
    state.title = title;
    Ok(state.view(epoch_ms()))
}

#[tauri::command]
fn pause_session(
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, RecorderState>,
) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    state.pause(now_ms).map_err(str::to_owned)?;
    if let Some(active) = recorder
        .0
        .lock()
        .expect("audio recorder state poisoned")
        .as_ref()
    {
        active.set_paused(true);
    }
    Ok(state.view(now_ms))
}

#[tauri::command]
fn resume_session(
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, RecorderState>,
) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    state.resume(now_ms).map_err(str::to_owned)?;
    if let Some(active) = recorder
        .0
        .lock()
        .expect("audio recorder state poisoned")
        .as_ref()
    {
        active.set_paused(false);
    }
    Ok(state.view(now_ms))
}

#[tauri::command]
fn stop_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, RecorderState>,
) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    {
        let state = state.lock().expect("session state poisoned");
        if !matches!(
            state.status,
            RecorderStatus::Recording | RecorderStatus::Paused
        ) {
            return Err("没有可以停止的活动录音".into());
        }
    }
    let active = recorder
        .0
        .lock()
        .expect("audio recorder state poisoned")
        .take()
        .ok_or_else(|| "录音器未运行".to_owned())?;
    let snapshot = match active.finish() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let mut state = state.lock().expect("session state poisoned");
            state.status = RecorderStatus::Idle;
            return Err(error.to_string());
        }
    };
    let mut state = state.lock().expect("session state poisoned");
    let session_id = state
        .session_id
        .clone()
        .ok_or_else(|| "当前没有录音会话".to_owned())?;
    let directory = recordings_root(&app)?.join(&session_id);
    let recorded = finalize_session(&directory, &mut state, &snapshot);
    write_new_file(&directory.join(COMPLETED_MARKER), b"completed\n")?;
    if state.status == RecorderStatus::Recording {
        state.accumulated_ms = state.view(now_ms).elapsed_ms;
    }
    state.resumed_at_epoch_ms = None;
    state.status = RecorderStatus::Finished;
    state.accumulated_ms = recorded.duration_ms;
    state.recorded = Some(recorded);
    Ok(state.view(now_ms))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(SessionState::default()))
        .manage(RecorderState(Arc::new(Mutex::new(None::<AudioRecording>))))
        .manage(AiState::default())
        .setup(|app| {
            let recordings = app.path().app_data_dir()?.join("recordings");
            fs::create_dir_all(&recordings)?;
            app.asset_protocol_scope()
                .allow_directory(&recordings, true)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            get_audio_levels,
            get_session_state,
            list_recordings,
            open_recording,
            trash_recording,
            recover_latest_session,
            reveal_recording,
            start_session,
            set_session_title,
            get_session_notes,
            save_session_notes,
            pause_session,
            resume_session,
            stop_session,
            ai::configure_ai,
            ai::get_ai_configurations,
            ai::clear_ai,
            ai::test_ai,
            ai::generate_ai
        ])
        .run(tauri::generate_context!())
        .expect("error while running MagneticTape desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(index: u64, track: Track, start_frame: u64, frames: u64) -> ChunkInfo {
        ChunkInfo {
            index,
            uuid: format!("{}-{index:05}", track.stem()),
            track,
            path: PathBuf::from(format!("{}-{index:05}.ogg", track.stem())),
            start_epoch_ms: 1_000 + start_frame * 1_000 / 48_000,
            start_frame,
            duration_ms: frames * 1_000 / 48_000,
            frames,
            packet_count: frames / 2_880,
            channels: 1,
            sample_rate: 48_000,
            frame_size: 2_880,
            bytes: 1,
        }
    }

    #[test]
    fn pause_and_resume_exclude_the_paused_interval() {
        let mut state = SessionState {
            status: RecorderStatus::Recording,
            session_id: Some("session".into()),
            started_at_epoch_ms: Some(1_000),
            resumed_at_epoch_ms: Some(1_000),
            accumulated_ms: 0,
            recorded: None,
            recovered: false,
            title: default_session_title(),
            checkpointer: None,
            sequence: Arc::new(AtomicU64::new(0)),
        };
        state.pause(4_000).unwrap();
        assert_eq!(state.view(10_000).elapsed_ms, 3_000);
        state.resume(10_000).unwrap();
        assert_eq!(state.view(12_000).elapsed_ms, 5_000);
    }

    #[test]
    fn invalid_lifecycle_transitions_are_rejected() {
        let mut state = SessionState::default();
        assert!(state.pause(1_000).is_err());
        assert!(state.resume(1_000).is_err());
    }

    #[test]
    fn manifest_projects_both_tracks_and_a_monotonic_timeline() {
        let snapshot = RecordingSnapshot {
            chunks: vec![
                chunk(0, Track::Microphone, 0, 48_000),
                chunk(0, Track::System, 0, 24_000),
                chunk(1, Track::Microphone, 48_000, 48_000),
            ],
            dropped_blocks: 2,
            microphone_frames: 96_000,
            system_frames: 24_000,
        };
        let manifest = build_manifest("session", 1_000, 3, SessionStatus::Recording, &snapshot);
        manifest.validate().unwrap();
        assert_eq!(manifest.chunks.len(), 3);
        assert_eq!(manifest.committed_timeline_frames, 96_000);
        assert_eq!(manifest.timeline.spans().len(), 3);
        assert_eq!(
            manifest
                .chunks
                .iter()
                .filter(|chunk| chunk.track == AudioSource::System)
                .count(),
            1
        );
    }

    #[test]
    fn loads_chunk_order_and_duration_from_a_recovery_manifest() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["mic-00000.ogg", "mic-00001.ogg", "sys-00000.ogg"] {
            fs::write(directory.path().join(name), b"OggS").unwrap();
        }
        let snapshot = RecordingSnapshot {
            chunks: vec![
                chunk(0, Track::Microphone, 0, 48_000),
                chunk(1, Track::Microphone, 48_000, 48_000),
                chunk(0, Track::System, 0, 48_000),
            ],
            dropped_blocks: 0,
            microphone_frames: 96_000,
            system_frames: 48_000,
        };
        let manifest = build_manifest("session", 1_000, 0, SessionStatus::Completed, &snapshot);
        write_checkpoint(directory.path(), &manifest).unwrap();

        let loaded = load_session(directory.path()).unwrap();
        assert_eq!(loaded.recorded.microphone_chunks.len(), 2);
        assert_eq!(loaded.recorded.system_chunks.len(), 1);
        assert_eq!(loaded.recorded.duration_ms, 2_000);
        assert_eq!(loaded.started_at_epoch_ms, 1_000);
        assert!(
            loaded.recorded.microphone_chunks[0].ends_with("mic-00000.ogg"),
            "chunks must be ordered by index"
        );
    }
}
