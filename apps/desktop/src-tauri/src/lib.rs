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
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

mod ai;
mod asr;
mod export;
use ai::AiState;
use asr::AsrState;

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
const TRANSCRIPT_FILE: &str = "transcript.json";
const TRANSCRIPT_MARKDOWN: &str = "transcript.md";
const AUDIO_DISCARDED: &str = "audio-discarded";
const SETTINGS_FILE: &str = "settings.json";

/// Non-secret preferences persisted between launches. API keys never enter
/// this file; they stay in the Windows Credential Manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AppSettings {
    microphone_id: Option<String>,
    system_output_id: Option<String>,
    language: String,
    translation: String,
    retain_audio: bool,
    live_captions: bool,
    asr_provider: String,
    asr_profiles: HashMap<String, AsrProfile>,
    auto_cleanup_days: u32,
    storage_quota_mb: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AsrProfile {
    endpoint: String,
    model: String,
    language: String,
}

impl Default for AsrProfile {
    fn default() -> Self {
        Self {
            endpoint: "https://api.minimax.cn/v1/speech_to_text".into(),
            model: "asr-1.0".into(),
            language: "auto".into(),
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            microphone_id: None,
            system_output_id: None,
            language: "auto".into(),
            translation: "none".into(),
            retain_audio: true,
            live_captions: false,
            asr_provider: "minimax".into(),
            asr_profiles: HashMap::new(),
            auto_cleanup_days: 0,
            storage_quota_mb: 0,
        }
    }
}

impl AppSettings {
    fn sanitized(mut self) -> Self {
        if !matches!(self.language.as_str(), "auto" | "zh-CN" | "en-US") {
            self.language = "auto".into();
        }
        if !matches!(self.translation.as_str(), "none" | "zh-CN" | "en-US") {
            self.translation = "none".into();
        }
        if !matches!(self.asr_provider.as_str(), "minimax" | "silicon_flow") {
            self.asr_provider = "minimax".into();
        }
        if !matches!(self.auto_cleanup_days, 0 | 30 | 90 | 180) {
            self.auto_cleanup_days = 0;
        }
        if self.storage_quota_mb > 1_048_576 {
            self.storage_quota_mb = 0;
        }
        self.microphone_id = self
            .microphone_id
            .map(|id| id.chars().take(512).collect::<String>())
            .filter(|id| !id.is_empty());
        self.system_output_id = self
            .system_output_id
            .map(|id| id.chars().take(512).collect::<String>())
            .filter(|id| !id.is_empty());
        self.asr_profiles.retain(|key, profile| {
            if !matches!(key.as_str(), "minimax" | "silicon_flow") {
                return false;
            }
            profile.endpoint = profile.endpoint.chars().take(512).collect();
            profile.model = profile.model.chars().take(128).collect();
            if !matches!(profile.language.as_str(), "auto" | "zh-CN" | "en-US") {
                profile.language = "auto".into();
            }
            !profile.endpoint.trim().is_empty() && !profile.model.trim().is_empty()
        });
        self
    }
}

fn load_settings(data_directory: &Path) -> AppSettings {
    fs::read(data_directory.join(SETTINGS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<AppSettings>(&bytes).ok())
        .unwrap_or_default()
        .sanitized()
}

fn write_settings(data_directory: &Path, settings: &AppSettings) -> Result<(), String> {
    fs::create_dir_all(data_directory).map_err(|error| error.to_string())?;
    let json = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    let path = data_directory.join(SETTINGS_FILE);
    let temporary = data_directory.join(format!(".{SETTINGS_FILE}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(&json).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

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

/// In-memory live subtitle buffer. The frontend polls one chunk at a time so
/// a slow ASR request never blocks recording or stopping.
#[derive(Default)]
struct LiveTranscriptionState {
    active: bool,
    busy: Arc<AtomicBool>,
    processed: HashSet<(u8, u64)>,
    first_seen: HashMap<u64, Instant>,
    segments: Vec<asr::TranscriptSegment>,
    provider: Option<asr::AsrProvider>,
    model: String,
    language: String,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveTranscriptView {
    active: bool,
    segments: Vec<asr::TranscriptSegment>,
    provider: Option<asr::AsrProvider>,
    model: String,
    language: String,
    error: Option<String>,
}

fn live_view(state: &LiveTranscriptionState) -> LiveTranscriptView {
    LiveTranscriptView {
        active: state.active,
        segments: state.segments.clone(),
        provider: state.provider,
        model: state.model.clone(),
        language: state.language.clone(),
        error: state.error.clone(),
    }
}

fn live_transcript(state: &LiveTranscriptionState) -> asr::Transcript {
    let duration_ms = state
        .segments
        .iter()
        .map(|segment| segment.end_ms)
        .max()
        .unwrap_or(0);
    let text = state
        .segments
        .iter()
        .map(|segment| segment.text.trim())
        .collect::<Vec<_>>()
        .join("\n");
    asr::Transcript {
        provider: state
            .provider
            .map(|provider| provider.as_str().to_owned())
            .unwrap_or_else(|| "minimax".into()),
        model: state.model.clone(),
        language: state.language.clone(),
        duration_ms,
        text,
        segments: state.segments.clone(),
        created_at_epoch_ms: epoch_ms(),
    }
}

fn persist_live_transcript(directory: &Path, state: &LiveTranscriptionState) {
    if state.segments.is_empty() {
        return;
    }
    let transcript = live_transcript(state);
    if let Ok(json) = serde_json::to_vec_pretty(&transcript) {
        let path = directory.join("live-transcript.json");
        let temporary = path.with_extension("json.tmp");
        if fs::write(&temporary, json).is_ok() {
            let _ = fs::rename(&temporary, &path);
        }
    }
}

fn finish_live_transcription(directory: &Path, state: &mut LiveTranscriptionState) {
    if !state.segments.is_empty() {
        let transcript = live_transcript(state);
        let _ = write_transcript(directory, &transcript);
    }
    state.active = false;
}

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
        .or_else(|| recorded_from_wav(directory))
        .or_else(|| transcript_only_session(directory))?;
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
            let mut last_health_check = Instant::now();
            loop {
                thread::sleep(Duration::from_millis(500));
                let mut guard = recorder.lock().expect("audio recorder state poisoned");
                let Some(active) = guard.as_mut() else {
                    break;
                };
                // Watchdog: rebuild streams after device loss, sleep/wake, or
                // an audio service restart without restarting the recording.
                if last_health_check.elapsed() >= Duration::from_secs(2) {
                    last_health_check = Instant::now();
                    if active.health().needs_rebuild()
                        && let Err(error) = active.rebuild()
                    {
                        eprintln!("failed to rebuild audio streams: {error}");
                    }
                }
                let snapshot = active.snapshot();
                drop(guard);
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioHealthView {
    active: bool,
    paused: bool,
    stalled: bool,
    microphone_error: Option<String>,
    system_error: Option<String>,
    needs_rebuild: bool,
}

#[tauri::command]
fn get_audio_health(recorder: tauri::State<'_, RecorderState>) -> AudioHealthView {
    let guard = recorder.0.lock().expect("audio recorder state poisoned");
    match guard.as_ref() {
        Some(recording) => {
            let health = recording.health();
            let needs_rebuild = health.needs_rebuild();
            AudioHealthView {
                active: true,
                paused: health.paused,
                stalled: health.stalled,
                microphone_error: health.microphone_error,
                system_error: health.system_error,
                needs_rebuild,
            }
        }
        None => AudioHealthView {
            active: false,
            paused: false,
            stalled: false,
            microphone_error: None,
            system_error: None,
            needs_rebuild: false,
        },
    }
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

fn audio_bytes(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "ogg"))
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn has_audio_files(directory: &Path) -> bool {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "ogg"))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryStats {
    session_count: usize,
    total_bytes: u64,
    audio_bytes: u64,
    transcript_only_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanupResult {
    sessions: usize,
    bytes_freed: u64,
}

fn library_stats(root: &Path) -> Result<LibraryStats, String> {
    let mut stats = LibraryStats {
        session_count: 0,
        total_bytes: 0,
        audio_bytes: 0,
        transcript_only_count: 0,
    };
    if !root.exists() {
        return Ok(stats);
    }
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        if load_session(&directory).is_none() {
            continue;
        }
        stats.session_count += 1;
        stats.total_bytes += directory_size(&directory);
        stats.audio_bytes += audio_bytes(&directory);
        if directory.join(AUDIO_DISCARDED).is_file() {
            stats.transcript_only_count += 1;
        }
    }
    Ok(stats)
}

fn cleanup_old_audio(
    root: &Path,
    cutoff_epoch_ms: u64,
    active_session: Option<&str>,
) -> Result<CleanupResult, String> {
    let mut result = CleanupResult {
        sessions: 0,
        bytes_freed: 0,
    };
    if !root.exists() {
        return Ok(result);
    }
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        let Some(session_id) = directory.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if active_session == Some(session_id)
            || directory.join(AUDIO_DISCARDED).is_file()
            || !directory.join(TRANSCRIPT_FILE).is_file()
            || !has_audio_files(&directory)
        {
            continue;
        }
        let Some(started_at) = load_session(&directory)
            .map(|session| session.started_at_epoch_ms)
            .filter(|started_at| *started_at > 0)
        else {
            continue;
        };
        if started_at >= cutoff_epoch_ms {
            continue;
        }
        result.bytes_freed += discard_audio_files(&directory)?;
        result.sessions += 1;
    }
    Ok(result)
}

#[tauri::command]
fn get_library_stats(app: tauri::AppHandle) -> Result<LibraryStats, String> {
    library_stats(&recordings_root(&app)?)
}

/// Drops the oldest transcript-bearing audio until the library fits the quota.
fn enforce_storage_quota(
    root: &Path,
    quota_bytes: u64,
    active_session: Option<&str>,
) -> Result<CleanupResult, String> {
    let mut result = CleanupResult {
        sessions: 0,
        bytes_freed: 0,
    };
    if quota_bytes == 0 || !root.exists() {
        return Ok(result);
    }
    let mut total = 0_u64;
    let mut candidates = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        let Some(session_id) = directory.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let size = directory_size(&directory);
        total += size;
        if active_session == Some(session_id)
            || directory.join(AUDIO_DISCARDED).is_file()
            || !directory.join(TRANSCRIPT_FILE).is_file()
            || !has_audio_files(&directory)
        {
            continue;
        }
        let started_at = load_session(&directory)
            .map(|session| session.started_at_epoch_ms)
            .unwrap_or(0);
        candidates.push((started_at, session_id.to_owned(), directory));
    }
    if total <= quota_bytes {
        return Ok(result);
    }
    candidates.sort_by_key(|(started_at, _, _)| *started_at);
    for (_, _, directory) in candidates {
        if total <= quota_bytes {
            break;
        }
        let freed = discard_audio_files(&directory)?;
        total = total.saturating_sub(freed);
        result.bytes_freed += freed;
        result.sessions += 1;
    }
    Ok(result)
}

#[tauri::command]
fn cleanup_audio_older_than(
    days: u32,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<CleanupResult, String> {
    if days == 0 {
        return Err("清理天数必须大于 0".into());
    }
    let cutoff = epoch_ms().saturating_sub(u64::from(days) * 86_400_000);
    let active_session = state
        .lock()
        .expect("session state poisoned")
        .session_id
        .clone();
    cleanup_old_audio(&recordings_root(&app)?, cutoff, active_session.as_deref())
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
        let session_id = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = session_title(&directory, &session_id);
        let status = if directory.join(AUDIO_DISCARDED).is_file() {
            "transcript_only"
        } else if directory.join(COMPLETED_MARKER).is_file() {
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
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
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
    *live.lock().expect("live transcription state poisoned") = LiveTranscriptionState::default();
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
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
    settings: tauri::State<'_, Mutex<AppSettings>>,
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
    let mut recorded = finalize_session(&directory, &mut state, &snapshot);
    {
        let mut live = live.lock().expect("live transcription state poisoned");
        finish_live_transcription(&directory, &mut live);
    }
    let discard_audio = !settings
        .lock()
        .expect("settings state poisoned")
        .retain_audio;
    if discard_audio && directory.join(TRANSCRIPT_FILE).is_file() {
        discard_audio_files(&directory)?;
        recorded = RecordedSession {
            duration_ms: recorded.duration_ms,
            ..RecordedSession::default()
        };
    }
    write_new_file(&directory.join(COMPLETED_MARKER), b"completed\n")?;
    state.resumed_at_epoch_ms = None;
    state.status = RecorderStatus::Finished;
    state.accumulated_ms = recorded.duration_ms;
    state.recorded = Some(recorded);
    Ok(state.view(now_ms))
}

fn format_timestamp(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn write_transcript(directory: &Path, transcript: &asr::Transcript) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(transcript).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(directory.join(TRANSCRIPT_FILE))
        .map_err(|error| error.to_string())?;
    file.write_all(&json).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;

    let mut markdown = format!(
        "# 会议转写\n\n- 服务商：{}\n- 模型：{}\n- 时长：{}\n\n",
        transcript.provider,
        transcript.model,
        format_timestamp(transcript.duration_ms)
    );
    if transcript.segments.is_empty() {
        markdown.push_str(&transcript.text);
        markdown.push('\n');
    } else {
        for segment in &transcript.segments {
            let speaker = segment
                .speaker
                .as_deref()
                .map(str::trim)
                .filter(|speaker| !speaker.is_empty());
            let timestamp = format_timestamp(segment.start_ms);
            let prefix = match speaker {
                Some(speaker) => format!("**[{timestamp}] {speaker}**"),
                None => format!("**[{timestamp}]**"),
            };
            markdown.push_str(&format!("{prefix} {}\n\n", segment.text.trim()));
        }
    }
    let mut markdown_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(directory.join(TRANSCRIPT_MARKDOWN))
        .map_err(|error| error.to_string())?;
    markdown_file
        .write_all(markdown.as_bytes())
        .map_err(|error| error.to_string())?;
    markdown_file.sync_all().map_err(|error| error.to_string())
}

fn read_transcript(directory: &Path) -> Option<asr::Transcript> {
    fs::read(directory.join(TRANSCRIPT_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Deletes all Ogg Opus chunks and their sidecars, keeping transcript, notes,
/// title and the recovery manifests. Returns the number of bytes freed.
fn discard_audio_files(directory: &Path) -> Result<u64, String> {
    let mut freed = 0_u64;
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.starts_with("mic-") || name.starts_with("sys-"))
            || !matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("ogg" | "json")
            )
        {
            continue;
        }
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        fs::remove_file(&path).map_err(|error| error.to_string())?;
        freed += size;
    }
    write_new_file(&directory.join(AUDIO_DISCARDED), b"discarded\n")?;
    Ok(freed)
}

fn transcript_only_session(directory: &Path) -> Option<RecordedSession> {
    let transcript = read_transcript(directory)?;
    Some(RecordedSession {
        duration_ms: transcript.duration_ms,
        ..RecordedSession::default()
    })
}

fn audio_chunks(directory: &Path) -> Result<Vec<asr::AudioChunkRef>, String> {
    let mut chunks = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") || name.starts_with("checkpoint-") || name == LEGACY_MARKER {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(info) = serde_json::from_slice::<ChunkInfo>(&bytes) else {
            continue;
        };
        let audio_path = directory.join(format!("{}.ogg", info.uuid));
        if !audio_path.is_file() {
            continue;
        }
        chunks.push(asr::AudioChunkRef {
            track: info.track,
            path: audio_path,
            start_frame: info.start_frame,
            frames: info.frames,
        });
    }
    chunks.sort_by_key(|chunk| {
        (
            match chunk.track {
                Track::Microphone => 0_u8,
                Track::System => 1_u8,
            },
            chunk.start_frame,
        )
    });
    Ok(chunks)
}

#[tauri::command]
async fn transcribe_session(
    provider: asr::AsrProvider,
    language: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    asr_state: tauri::State<'_, AsrState>,
    settings: tauri::State<'_, Mutex<AppSettings>>,
) -> Result<asr::Transcript, String> {
    let (directory, session_id) = {
        let state = state.lock().expect("session state poisoned");
        if state.status != RecorderStatus::Finished {
            return Err("只有已结束的录音才能转写".into());
        }
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| "当前没有录音会话".to_owned())?;
        let directory = recordings_root(&app)?.join(&session_id);
        (directory, session_id)
    };
    let chunks = audio_chunks(&directory)?;
    if chunks.is_empty() {
        return Err("当前会话没有可转写的 Opus 分片".into());
    }
    let mut config = asr_state.credential(provider)?;
    if let Some(language) = language.filter(|value| !value.trim().is_empty()) {
        config.language = language;
    }
    let transcript = asr::transcribe_chunks(&chunks, &config, &session_id).await?;
    write_transcript(&directory, &transcript)?;
    if !settings
        .lock()
        .expect("settings state poisoned")
        .retain_audio
    {
        discard_audio_files(&directory)?;
        let mut state = state.lock().expect("session state poisoned");
        if state.session_id.as_deref() == Some(session_id.as_str()) {
            state.recorded = Some(RecordedSession {
                duration_ms: transcript.duration_ms,
                ..RecordedSession::default()
            });
        }
    }
    Ok(transcript)
}

#[tauri::command]
fn get_transcript(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<Option<asr::Transcript>, String> {
    let directory = current_session_directory(&app, &state)?;
    Ok(read_transcript(&directory))
}

#[tauri::command]
fn discard_session_audio(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    if state.status != RecorderStatus::Finished {
        return Err("只有已结束的录音可以删除音频".into());
    }
    let session_id = state
        .session_id
        .clone()
        .ok_or_else(|| "当前没有录音会话".to_owned())?;
    let directory = recordings_root(&app)?.join(&session_id);
    if read_transcript(&directory).is_none() {
        return Err("请先完成转写，再删除音频；转写是保留内容的唯一副本".into());
    }
    if !directory.join(AUDIO_DISCARDED).is_file() {
        discard_audio_files(&directory)?;
    }
    let duration_ms = read_transcript(&directory)
        .map(|transcript| transcript.duration_ms)
        .unwrap_or(state.accumulated_ms);
    state.recorded = Some(RecordedSession {
        duration_ms,
        ..RecordedSession::default()
    });
    Ok(state.view(now_ms))
}

#[tauri::command]
fn get_settings(settings: tauri::State<'_, Mutex<AppSettings>>) -> AppSettings {
    settings.lock().expect("settings state poisoned").clone()
}
#[tauri::command]
fn save_settings(
    settings: AppSettings,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppSettings>>,
) -> Result<AppSettings, String> {
    let sanitized = settings.sanitized();
    let data_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    write_settings(&data_directory, &sanitized)?;
    *state.lock().expect("settings state poisoned") = sanitized.clone();
    Ok(sanitized)
}

#[tauri::command]
fn start_live_transcription(
    provider: asr::AsrProvider,
    language: Option<String>,
    state: tauri::State<'_, Mutex<SessionState>>,
    asr_state: tauri::State<'_, AsrState>,
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
) -> Result<LiveTranscriptView, String> {
    {
        let state = state.lock().expect("session state poisoned");
        if state.status != RecorderStatus::Recording {
            return Err("只有录音中的会话可以开启实时字幕".into());
        }
    }
    let config = asr_state.credential(provider)?;
    let mut live = live.lock().expect("live transcription state poisoned");
    *live = LiveTranscriptionState {
        active: true,
        busy: Arc::new(AtomicBool::new(false)),
        processed: HashSet::new(),
        first_seen: HashMap::new(),
        segments: Vec::new(),
        provider: Some(provider),
        model: config.model,
        language: language
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(config.language),
        error: None,
    };
    Ok(live_view(&live))
}

#[tauri::command]
fn get_live_transcript(
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
) -> LiveTranscriptView {
    live_view(&live.lock().expect("live transcription state poisoned"))
}

fn track_slot(track: Track) -> u8 {
    match track {
        Track::Microphone => 0,
        Track::System => 1,
    }
}

async fn poll_live_inner(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, Mutex<SessionState>>,
    recorder: &tauri::State<'_, RecorderState>,
    asr_state: &tauri::State<'_, AsrState>,
    live: &tauri::State<'_, Mutex<LiveTranscriptionState>>,
) -> Result<(), String> {
    let (session_id, directory) = {
        let state = state.lock().expect("session state poisoned");
        if !matches!(
            state.status,
            RecorderStatus::Recording | RecorderStatus::Paused
        ) {
            return Ok(());
        }
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| "当前没有录音会话".to_owned())?;
        let directory = recordings_root(app)?.join(&session_id);
        (session_id, directory)
    };
    let snapshot = recorder
        .0
        .lock()
        .expect("audio recorder state poisoned")
        .as_ref()
        .map(AudioRecording::snapshot);
    let Some(snapshot) = snapshot else {
        return Ok(());
    };

    let (ready, provider, language) = {
        let mut live = live.lock().expect("live transcription state poisoned");
        let mut slots: BTreeMap<u64, (Option<asr::AudioChunkRef>, Option<asr::AudioChunkRef>)> =
            BTreeMap::new();
        for chunk in &snapshot.chunks {
            let key = (track_slot(chunk.track), chunk.start_frame);
            if live.processed.contains(&key) {
                continue;
            }
            let entry = slots.entry(chunk.start_frame).or_default();
            let reference = asr::AudioChunkRef {
                track: chunk.track,
                path: chunk.path.clone(),
                start_frame: chunk.start_frame,
                frames: chunk.frames,
            };
            match chunk.track {
                Track::Microphone => entry.0 = Some(reference),
                Track::System => entry.1 = Some(reference),
            }
        }
        for slot in slots.keys() {
            live.first_seen.entry(*slot).or_insert_with(Instant::now);
        }
        let latest = slots.keys().next_back().copied();
        let chosen = slots
            .iter()
            .find(|(slot, entry)| {
                let complete = entry.0.is_some() && entry.1.is_some();
                let superseded = latest.is_some_and(|latest| **slot < latest);
                let stale = live
                    .first_seen
                    .get(slot)
                    .is_some_and(|seen| seen.elapsed() >= Duration::from_secs(1));
                complete || superseded || stale
            })
            .map(|(slot, entry)| (*slot, entry.0.clone(), entry.1.clone()));
        (chosen, live.provider, live.language.clone())
    };

    let Some((slot, microphone, system)) = ready else {
        return Ok(());
    };
    let provider = provider.ok_or_else(|| "实时转写未配置服务商".to_owned())?;
    let mut config = asr_state.credential(provider)?;
    if !language.is_empty() {
        config.language = language;
    }
    let result =
        asr::transcribe_pair(microphone.clone(), system.clone(), &config, &session_id).await;

    let mut live = live.lock().expect("live transcription state poisoned");
    // Mark the slot as consumed even if one track had not committed yet; a
    // late chunk for the same window must not be transcribed twice.
    live.processed.insert((0, slot));
    live.processed.insert((1, slot));
    live.first_seen.remove(&slot);
    match result {
        Ok(transcript) => {
            live.segments.extend(transcript.segments);
            live.error = None;
        }
        Err(error) => {
            live.error = Some(error);
        }
    }
    persist_live_transcript(&directory, &live);
    Ok(())
}

#[tauri::command]
async fn poll_live_transcription(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, RecorderState>,
    asr_state: tauri::State<'_, AsrState>,
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
) -> Result<LiveTranscriptView, String> {
    let busy = {
        let live = live.lock().expect("live transcription state poisoned");
        if !live.active {
            return Ok(live_view(&live));
        }
        Arc::clone(&live.busy)
    };
    if busy.swap(true, Ordering::SeqCst) {
        return Ok(live_view(
            &live.lock().expect("live transcription state poisoned"),
        ));
    }
    let result = poll_live_inner(&app, &state, &recorder, &asr_state, &live).await;
    busy.store(false, Ordering::SeqCst);
    let mut live = live.lock().expect("live transcription state poisoned");
    if let Err(error) = result {
        live.error = Some(error);
    }
    Ok(live_view(&live))
}

#[tauri::command]
async fn translate_transcript(
    target_language: String,
    provider: ai::AiProvider,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    ai_state: tauri::State<'_, AiState>,
) -> Result<String, String> {
    let directory = current_session_directory(&app, &state)?;
    let transcript =
        read_transcript(&directory).ok_or_else(|| "请先完成转写，再生成译文".to_owned())?;
    let prompt = format!(
        "请将以下会议转写翻译为{}。保留时间戳、说话人标记和段落结构，只输出译文，不要解释，不要编造原文没有的信息。\n\n{}",
        target_language, transcript.text
    );
    let translation = ai::generate_text(provider, &prompt, &ai_state).await?;
    let suffix: String = target_language
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    let suffix = if suffix.is_empty() {
        "target"
    } else {
        suffix.as_str()
    };
    let path = directory.join(format!("translation-{suffix}.md"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(translation.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(translation)
}

fn exports_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .document_dir()
        .map_err(|error| error.to_string())
        .map(|directory| directory.join("MagneticTape"))
}

#[tauri::command]
fn export_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<export::ExportResult, String> {
    let (directory, title, session_id, started_at_epoch_ms, duration_ms) = {
        let state = state.lock().expect("session state poisoned");
        if state.status != RecorderStatus::Finished {
            return Err("只有已结束的录音可以导出".into());
        }
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| "当前没有录音会话".to_owned())?;
        (
            recordings_root(&app)?.join(&session_id),
            state.title.clone(),
            session_id,
            state.started_at_epoch_ms.unwrap_or(0),
            state.accumulated_ms,
        )
    };
    let transcript = read_transcript(&directory);
    let notes = fs::read_to_string(directory.join(NOTES_FILE)).unwrap_or_default();
    let chunks = audio_chunks(&directory)?;
    let root = exports_root(&app)?;
    let input = export::ExportInput {
        title: &title,
        session_id: &session_id,
        started_at_epoch_ms,
        duration_ms,
        transcript: transcript.as_ref(),
        notes: &notes,
        chunks: &chunks,
    };
    export::export_all(&input, &root)
}

#[tauri::command]
fn reveal_export(path: PathBuf, app: tauri::AppHandle) -> Result<(), String> {
    let root = exports_root(&app)?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let target = path.canonicalize().map_err(|error| error.to_string())?;
    if !target.starts_with(&root) {
        return Err("只能打开 MagneticTape 导出目录".into());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer.exe")
            .arg(&target)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("无法打开资源管理器：{error}"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = target;
        Err("当前平台暂不支持打开导出目录".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(SessionState::default()))
        .manage(RecorderState(Arc::new(Mutex::new(None::<AudioRecording>))))
        .manage(AiState::default())
        .manage(AsrState::default())
        .manage(Mutex::new(LiveTranscriptionState::default()))
        .setup(|app| {
            let data_directory = app.path().app_data_dir()?;
            let recordings = data_directory.join("recordings");
            fs::create_dir_all(&recordings)?;
            app.asset_protocol_scope()
                .allow_directory(&recordings, true)?;
            let settings = load_settings(&data_directory);
            {
                let asr_state = app.state::<AsrState>();
                for (name, profile) in &settings.asr_profiles {
                    let provider = match name.as_str() {
                        "minimax" => asr::AsrProvider::MiniMax,
                        "silicon_flow" => asr::AsrProvider::SiliconFlow,
                        _ => continue,
                    };
                    asr_state.seed(
                        provider,
                        profile.endpoint.clone(),
                        profile.model.clone(),
                        profile.language.clone(),
                    );
                }
            }
            if settings.auto_cleanup_days > 0 {
                let cutoff =
                    epoch_ms().saturating_sub(u64::from(settings.auto_cleanup_days) * 86_400_000);
                match cleanup_old_audio(&recordings, cutoff, None) {
                    Ok(result) if result.sessions > 0 => eprintln!(
                        "auto cleanup removed audio from {} sessions ({} bytes)",
                        result.sessions, result.bytes_freed
                    ),
                    Ok(_) => {}
                    Err(error) => eprintln!("auto cleanup failed: {error}"),
                }
            }
            if settings.storage_quota_mb > 0 {
                let quota = u64::from(settings.storage_quota_mb) * 1_048_576;
                match enforce_storage_quota(&recordings, quota, None) {
                    Ok(result) if result.sessions > 0 => eprintln!(
                        "quota cleanup removed audio from {} sessions ({} bytes)",
                        result.sessions, result.bytes_freed
                    ),
                    Ok(_) => {}
                    Err(error) => eprintln!("quota cleanup failed: {error}"),
                }
            }
            app.manage(Mutex::new(settings));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            get_audio_levels,
            get_audio_health,
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
            ai::generate_ai,
            ai::translate_live_segments,
            asr::configure_asr,
            asr::get_asr_configurations,
            asr::clear_asr,
            transcribe_session,
            get_transcript,
            translate_transcript,
            start_live_transcription,
            poll_live_transcription,
            get_live_transcript,
            get_settings,
            save_settings,
            export_session,
            reveal_export,
            discard_session_audio,
            get_library_stats,
            cleanup_audio_older_than
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

    #[test]
    fn settings_round_trip_sanitizes_invalid_values() {
        let directory = tempfile::tempdir().unwrap();
        let settings = AppSettings {
            language: "fr".into(),
            translation: "klingon".into(),
            microphone_id: Some(String::new()),
            asr_provider: "other".into(),
            asr_profiles: HashMap::from([
                ("unknown".into(), AsrProfile::default()),
                (
                    "minimax".into(),
                    AsrProfile {
                        endpoint: String::new(),
                        ..AsrProfile::default()
                    },
                ),
            ]),
            ..AppSettings::default()
        };
        write_settings(directory.path(), &settings.sanitized()).unwrap();

        let loaded = load_settings(directory.path());
        assert_eq!(loaded.language, "auto");
        assert_eq!(loaded.translation, "none");
        assert_eq!(loaded.asr_provider, "minimax");
        assert_eq!(loaded.microphone_id, None);
        assert!(loaded.asr_profiles.is_empty());
        assert!(loaded.retain_audio);

        let valid = AppSettings {
            language: "zh-CN".into(),
            translation: "en-US".into(),
            live_captions: true,
            microphone_id: Some("microphone-1".into()),
            asr_profiles: HashMap::from([(
                "silicon_flow".into(),
                AsrProfile {
                    endpoint: "https://api.siliconflow.cn/v1/audio/transcriptions".into(),
                    model: "FunAudioLLM/SenseVoiceSmall".into(),
                    language: "zh-CN".into(),
                },
            )]),
            ..AppSettings::default()
        };
        write_settings(directory.path(), &valid.sanitized()).unwrap();
        let loaded = load_settings(directory.path());
        assert_eq!(loaded.language, "zh-CN");
        assert!(loaded.live_captions);
        assert_eq!(loaded.microphone_id.as_deref(), Some("microphone-1"));
        assert_eq!(loaded.asr_profiles.len(), 1);
    }

    #[test]
    fn discarding_audio_keeps_transcript_and_recovery_metadata() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("mic-00000.ogg"), b"OggS-audio").unwrap();
        fs::write(directory.path().join("mic-00000.json"), b"{}").unwrap();
        fs::write(directory.path().join("sys-00000.ogg"), b"OggS-audio").unwrap();
        fs::write(directory.path().join("sys-00000.json"), b"{}").unwrap();
        fs::write(
            directory
                .path()
                .join("checkpoint-00000000000000000001.json"),
            b"{}",
        )
        .unwrap();
        fs::write(directory.path().join(TITLE_FILE), b"meeting").unwrap();
        fs::write(directory.path().join(NOTES_FILE), b"notes").unwrap();
        let transcript = asr::Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            duration_ms: 5_000,
            text: "hello".into(),
            segments: Vec::new(),
            created_at_epoch_ms: 0,
        };
        fs::write(
            directory.path().join(TRANSCRIPT_FILE),
            serde_json::to_vec(&transcript).unwrap(),
        )
        .unwrap();

        let freed = discard_audio_files(directory.path()).unwrap();
        assert!(freed > 0);
        assert!(!directory.path().join("mic-00000.ogg").exists());
        assert!(!directory.path().join("sys-00000.json").exists());
        assert!(directory.path().join(TRANSCRIPT_FILE).exists());
        assert!(directory.path().join(NOTES_FILE).exists());
        assert!(directory.path().join(TITLE_FILE).exists());
        assert!(
            directory
                .path()
                .join("checkpoint-00000000000000000001.json")
                .exists()
        );
        assert!(directory.path().join(AUDIO_DISCARDED).exists());

        let loaded = load_session(directory.path()).unwrap();
        assert!(loaded.recorded.is_empty());
        assert_eq!(loaded.recorded.duration_ms, 5_000);
    }

    #[test]
    fn cleanup_only_discards_old_sessions_with_a_transcript() {
        let root = tempfile::tempdir().unwrap();
        let transcript = serde_json::to_vec(&asr::Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            duration_ms: 5_000,
            text: "hello".into(),
            segments: Vec::new(),
            created_at_epoch_ms: 0,
        })
        .unwrap();

        for (name, started_at, with_transcript) in [
            ("0000000000000001", 1_000_u64, true),
            ("0000000000000002", epoch_ms(), true),
            ("0000000000000003", 1_000_u64, false),
        ] {
            let directory = root.path().join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(LEGACY_MARKER),
                format!(r#"{{"startedAtEpochMs":{started_at},"title":"{name}"}}"#),
            )
            .unwrap();
            fs::write(directory.join("mic-00000.ogg"), b"OggS-audio").unwrap();
            fs::write(directory.join("mic-00000.json"), b"{}").unwrap();
            if with_transcript {
                fs::write(directory.join(TRANSCRIPT_FILE), &transcript).unwrap();
            }
        }

        let result = cleanup_old_audio(root.path(), 10_000, None).unwrap();
        assert_eq!(result.sessions, 1);
        assert!(result.bytes_freed > 0);
        assert!(
            !root
                .path()
                .join("0000000000000001")
                .join("mic-00000.ogg")
                .exists()
        );
        assert!(
            root.path()
                .join("0000000000000001")
                .join(TRANSCRIPT_FILE)
                .exists()
        );
        assert!(
            root.path()
                .join("0000000000000002")
                .join("mic-00000.ogg")
                .exists()
        );
        assert!(
            root.path()
                .join("0000000000000003")
                .join("mic-00000.ogg")
                .exists(),
            "sessions without a transcript must keep their audio"
        );

        let stats = library_stats(root.path()).unwrap();
        assert_eq!(stats.session_count, 2);
        assert_eq!(stats.transcript_only_count, 1);
        assert!(stats.audio_bytes > 0);
    }

    #[test]
    fn quota_cleanup_drops_the_oldest_audio_first() {
        let root = tempfile::tempdir().unwrap();
        let transcript = serde_json::to_vec(&asr::Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            duration_ms: 5_000,
            text: "hello".into(),
            segments: Vec::new(),
            created_at_epoch_ms: 0,
        })
        .unwrap();
        let mut total = 0_u64;
        for (name, started_at, audio_len) in [
            ("0000000000000001", 1_000_u64, 100_usize),
            ("0000000000000002", 2_000, 200),
            ("0000000000000003", 3_000, 300),
        ] {
            let directory = root.path().join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(LEGACY_MARKER),
                format!(r#"{{"startedAtEpochMs":{started_at},"title":"{name}"}}"#),
            )
            .unwrap();
            fs::write(directory.join(TRANSCRIPT_FILE), &transcript).unwrap();
            fs::write(directory.join("mic-00000.ogg"), vec![0_u8; audio_len]).unwrap();
            fs::write(directory.join("mic-00000.json"), b"{}").unwrap();
            total += directory_size(&directory);
        }
        // Freeing the oldest 100 byte chunk is enough to get back under quota.
        let quota = total.saturating_sub(50);
        let result = enforce_storage_quota(root.path(), quota, None).unwrap();
        assert_eq!(result.sessions, 1);
        assert!(
            !root
                .path()
                .join("0000000000000001")
                .join("mic-00000.ogg")
                .exists()
        );
        assert!(
            root.path()
                .join("0000000000000002")
                .join("mic-00000.ogg")
                .exists()
        );
        assert!(
            root.path()
                .join("0000000000000003")
                .join("mic-00000.ogg")
                .exists()
        );
    }

    #[test]
    fn live_segments_become_a_transcript_on_stop() {
        let directory = tempfile::tempdir().unwrap();
        let mut live = LiveTranscriptionState {
            active: true,
            provider: Some(asr::AsrProvider::MiniMax),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            segments: vec![
                asr::TranscriptSegment {
                    start_ms: 0,
                    end_ms: 1_500,
                    speaker: Some("S1".into()),
                    text: "大家好。".into(),
                },
                asr::TranscriptSegment {
                    start_ms: 1_500,
                    end_ms: 3_200,
                    speaker: Some("S2".into()),
                    text: "开始吧。".into(),
                },
            ],
            ..Default::default()
        };
        finish_live_transcription(directory.path(), &mut live);

        assert!(!live.active);
        let transcript = read_transcript(directory.path()).unwrap();
        assert_eq!(transcript.provider, "minimax");
        assert_eq!(transcript.duration_ms, 3_200);
        assert_eq!(transcript.text, "大家好。\n开始吧。");
        assert_eq!(transcript.segments.len(), 2);
        let markdown = fs::read_to_string(directory.path().join(TRANSCRIPT_MARKDOWN)).unwrap();
        assert!(markdown.contains("**[00:00] S1**"));
        assert!(markdown.contains("开始吧。"));
    }
}
