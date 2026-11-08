use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{SinkExt, StreamExt};
use magnetic_tape_audio::{
    AudioLevels, AudioRecording, ChunkInfo, OPUS_SAMPLE_RATE, RecordingSnapshot, Track,
    wav_duration_ms,
};
use magnetic_tape_core::{
    manifest::{ChunkMetadata, RecoveryManifest, SessionStatus},
    recovery::{CHECKPOINT_RETENTION, RecoveryStore},
    timeline::{AudioSource, TimelineSpan},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
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
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

mod ai;
mod asr;
mod export;
mod local_whisper;
mod logging;
mod sherpa;
use ai::AiState;
use asr::AsrState;
use local_whisper::LocalWhisperState;

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
/// Compact, atomically replaced session metadata (storage v2).
const SESSION_STATE_FILE: &str = "session.json";
/// Append-only journal of committed chunks (storage v2).
const CHUNK_JOURNAL_FILE: &str = "chunks.ndjson";
const SESSION_STATE_SCHEMA: u32 = 2;
/// Per-transcript upload/recognition coverage, written next to transcript.json.
const TRANSCRIPT_COVERAGE_FILE: &str = "transcript-coverage.json";
/// Live captions schedule chunks in five-second buckets. 5,040 ms is the
/// actual duration of one committed chunk (241,920 frames at 48 kHz).
const LIVE_WINDOW_MS: u64 = 5_040;
/// Only drop audio when the ASR provider processed at least 98% of it and no
/// upload segment failed. Older transcripts without coverage fall back to a
/// conservative duration comparison.
const MIN_TRANSCRIPT_COVERAGE_PERMILLE: u64 = 980;

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
    /// Prefer the local sherpa-onnx streaming recognizer for live captions.
    live_streaming: bool,
    asr_provider: String,
    asr_profiles: HashMap<String, AsrProfile>,
    local_whisper_model: String,
    whisper_hotwords: String,
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
            live_streaming: true,
            asr_provider: "minimax".into(),
            asr_profiles: HashMap::new(),
            local_whisper_model: local_whisper::DEFAULT_MODEL.into(),
            whisper_hotwords: String::new(),
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
        if !matches!(
            self.asr_provider.as_str(),
            "minimax" | "silicon_flow" | "openai_realtime" | "local_whisper"
        ) {
            self.asr_provider = "minimax".into();
        }
        if !local_whisper::is_known_model(&self.local_whisper_model) {
            self.local_whisper_model = local_whisper::DEFAULT_MODEL.into();
        }
        self.whisper_hotwords = self
            .whisper_hotwords
            .trim()
            .chars()
            .take(512)
            .collect::<String>();
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
            if !matches!(key.as_str(), "minimax" | "silicon_flow" | "openai_realtime") {
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

/// Small storage-v2 session header. The chunk list lives in the append-only
/// journal, so this file stays small and can be replaced atomically per commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStateFile {
    schema_version: u32,
    session_id: String,
    started_at_epoch_ms: u64,
    status: SessionStatus,
    committed_timeline_frames: u64,
    chunk_count: usize,
    updated_at_epoch_ms: u64,
}

/// Audio actually processed by the ASR provider, used by the deletion guard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptCoverage {
    covered_ms: u64,
    duration_ms: u64,
    failed_segments: u64,
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

/// Cancellation handle for the one long-running transcription at a time.
#[derive(Default)]
struct TranscriptionRun(Mutex<Option<Arc<AtomicBool>>>);

/// Releases the transcription slot even when the command returns early.
struct TranscriptionGuard<'a> {
    run: &'a TranscriptionRun,
    flag: Arc<AtomicBool>,
}

impl TranscriptionGuard<'_> {
    fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

impl Drop for TranscriptionGuard<'_> {
    fn drop(&mut self) {
        self.run.clear();
    }
}

impl TranscriptionRun {
    fn begin(&self) -> Result<TranscriptionGuard<'_>, String> {
        let mut slot = self.0.lock().expect("transcription state poisoned");
        if slot.is_some() {
            return Err("已有转写任务正在运行".into());
        }
        let flag = Arc::new(AtomicBool::new(false));
        *slot = Some(Arc::clone(&flag));
        Ok(TranscriptionGuard { run: self, flag })
    }

    fn cancel(&self) -> bool {
        let slot = self.0.lock().expect("transcription state poisoned");
        slot.as_ref().is_some_and(|flag| {
            flag.store(true, Ordering::SeqCst);
            true
        })
    }

    fn clear(&self) {
        *self.0.lock().expect("transcription state poisoned") = None;
    }
}

/// Payload emitted to the frontend for long-running AI/ASR jobs.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobProgress {
    stage: String,
    done: usize,
    total: usize,
}

/// In-memory live subtitle buffer. Polling schedules at most two background
/// ASR jobs so a slow request never blocks recording or later audio windows.
#[derive(Default)]
struct LiveTranscriptionState {
    active: bool,
    session_id: Option<String>,
    inflight: HashSet<u64>,
    processed: HashSet<(u8, u64)>,
    first_seen: HashMap<u64, Instant>,
    segments: Vec<asr::TranscriptSegment>,
    provider: Option<asr::AsrProvider>,
    model: String,
    language: String,
    error: Option<String>,
    partial_text: String,
    stream_tx: Option<mpsc::UnboundedSender<Vec<i16>>>,
    /// True when captions come from the local sherpa-onnx streaming engine
    /// instead of five-second chunk uploads.
    streaming: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveTranscriptView {
    active: bool,
    pending_chunks: usize,
    processed_chunks: usize,
    segments: Vec<asr::TranscriptSegment>,
    provider: Option<asr::AsrProvider>,
    model: String,
    language: String,
    error: Option<String>,
    partial_text: String,
    streaming: bool,
}

fn live_view(state: &LiveTranscriptionState) -> LiveTranscriptView {
    LiveTranscriptView {
        active: state.active,
        pending_chunks: state.inflight.len(),
        processed_chunks: state.processed.len() / 2,
        segments: state.segments.clone(),
        provider: state.provider,
        model: state.model.clone(),
        language: state.language.clone(),
        error: state.error.clone(),
        partial_text: state.partial_text.clone(),
        streaming: state.streaming,
    }
}

/// Live state owned by the local streaming engine. It lives behind its own
/// lock so polling never holds the chunk state while the model decodes.
struct SherpaLive {
    session_id: String,
    asr: sherpa::StreamingAsr,
    speaker: Option<sherpa::SpeakerEngine>,
    gallery: sherpa::SpeakerGallery,
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
            .unwrap_or_else(|| {
                if state.streaming {
                    "sherpa".into()
                } else {
                    "minimax".into()
                }
            }),
        model: state.model.clone(),
        language: state.language.clone(),
        duration_ms,
        text,
        segments: state.segments.clone(),
        created_at_epoch_ms: epoch_ms(),
    }
}

fn merge_live_transcript(state: &mut LiveTranscriptionState, transcript: asr::Transcript) {
    for mut segment in transcript.segments {
        // The model closes the sentence at the chunk edge, so a chunk that
        // continues the previous sentence starts with a word that cannot begin
        // one. The terminator on the previous segment is removed, otherwise
        // the caption reads "...杜邦分析法。的方法呢？".
        if let Some(previous) = state
            .segments
            .iter_mut()
            .filter(|candidate| candidate.end_ms <= segment.start_ms)
            .max_by_key(|candidate| candidate.end_ms)
        {
            let mut previous_text = previous.text.clone();
            let merged = asr::merge_chunk_text(&mut previous_text, &segment.text);
            if previous_text != previous.text {
                previous.text = previous_text;
            }
            segment.text = merged;
        }
        state.segments.push(segment);
    }
    // Concurrent requests can finish out of order. The UI and persisted
    // transcript must always follow the shared recording timeline.
    state
        .segments
        .sort_by_key(|segment| (segment.start_ms, segment.end_ms));
    state.error = None;
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
    state.stream_tx.take();
    if !state.segments.is_empty() {
        let transcript = live_transcript(state);
        let _ = write_transcript(directory, &transcript);
        let _ = write_transcript_coverage(
            directory,
            &TranscriptCoverage {
                covered_ms: transcript.duration_ms,
                duration_ms: transcript.duration_ms,
                failed_segments: 0,
            },
        );
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

/// Trims legacy sessions that accumulated one checkpoint per five-second chunk
/// before retention was introduced. Runs once at startup; the newest
/// checkpoints are always preserved.
fn prune_session_checkpoints(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        if let Ok(store) = RecoveryStore::open(&directory)
            && let Ok(count) = store.prune(CHECKPOINT_RETENTION)
        {
            removed += count;
        }
    }
    removed
}

/// Replaces a small file atomically. Used for the session header; the chunk
/// journal is append-only and never needs this path.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn write_session_state(
    directory: &Path,
    manifest: &RecoveryManifest,
    chunk_count: usize,
) -> Result<(), String> {
    let state = SessionStateFile {
        schema_version: SESSION_STATE_SCHEMA,
        session_id: manifest.session_id.clone(),
        started_at_epoch_ms: manifest.started_at_epoch_ms,
        status: manifest.status,
        committed_timeline_frames: manifest.committed_timeline_frames,
        chunk_count,
        updated_at_epoch_ms: epoch_ms(),
    };
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    write_bytes_atomic(&directory.join(SESSION_STATE_FILE), &bytes)
}

fn read_session_state(directory: &Path) -> Option<SessionStateFile> {
    let bytes = fs::read(directory.join(SESSION_STATE_FILE)).ok()?;
    let state: SessionStateFile = serde_json::from_slice(&bytes).ok()?;
    (state.schema_version == SESSION_STATE_SCHEMA).then_some(state)
}

fn write_transcript_coverage(
    directory: &Path,
    coverage: &TranscriptCoverage,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(coverage).map_err(|error| error.to_string())?;
    write_bytes_atomic(&directory.join(TRANSCRIPT_COVERAGE_FILE), &bytes)
}

fn read_transcript_coverage(directory: &Path) -> Option<TranscriptCoverage> {
    let bytes = fs::read(directory.join(TRANSCRIPT_COVERAGE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Reads the valid prefix of the append-only journal. A torn final line (power
/// loss during append) is ignored; earlier committed lines stay usable.
fn read_chunk_journal(directory: &Path) -> Vec<ChunkInfo> {
    let Ok(text) = fs::read_to_string(directory.join(CHUNK_JOURNAL_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<ChunkInfo>(line).ok())
        .collect()
}

/// Appends chunks that are not in the journal yet. The journal is keyed by
/// `(track, index)`, so calling this repeatedly is idempotent and a crashed
/// checkpoint thread can simply retry.
fn sync_chunk_journal(directory: &Path, chunks: &[ChunkInfo]) -> Result<usize, String> {
    let existing = read_chunk_journal(directory);
    let mut max_index = [0_u64; 2];
    let mut seen = [false; 2];
    for chunk in &existing {
        let slot = usize::from(track_slot(chunk.track));
        if !seen[slot] || chunk.index > max_index[slot] {
            max_index[slot] = chunk.index;
        }
        seen[slot] = true;
    }

    let mut missing = chunks
        .iter()
        .filter(|chunk| {
            let slot = usize::from(track_slot(chunk.track));
            !seen[slot] || chunk.index > max_index[slot]
        })
        .collect::<Vec<_>>();
    missing.sort_by_key(|chunk| (track_slot(chunk.track), chunk.index));
    if missing.is_empty() {
        return Ok(0);
    }

    let path = directory.join(CHUNK_JOURNAL_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| error.to_string())?;
    // A crash can leave a torn line without its newline. Start the next append
    // on a fresh line so the previous partial record is skipped instead of
    // corrupting the new record.
    if fs::metadata(&path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
        && fs::read(&path).ok().and_then(|bytes| bytes.last().copied()) != Some(b'\n')
    {
        file.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    for chunk in &missing {
        serde_json::to_writer(&mut file, chunk).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(missing.len())
}

fn read_sidecar_chunks(directory: &Path) -> Vec<ChunkInfo> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut chunks = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") || name.starts_with("checkpoint-") || name == LEGACY_MARKER {
            continue;
        }
        if name == SESSION_STATE_FILE {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(info) = serde_json::from_slice::<ChunkInfo>(&bytes) else {
            continue;
        };
        if directory.join(format!("{}.ogg", info.uuid)).is_file() {
            chunks.push(info);
        }
    }
    chunks
}

fn recorded_from_journal(directory: &Path, chunks: &[ChunkInfo]) -> Option<RecordedSession> {
    let mut microphone = Vec::new();
    let mut system = Vec::new();
    let mut duration_ms = 0_u64;
    for chunk in chunks {
        let path = directory.join(format!("{}.ogg", chunk.uuid));
        if !path.is_file() {
            continue;
        }
        duration_ms = duration_ms.max(
            chunk.start_frame.saturating_mul(1_000)
                / u64::from(magnetic_tape_audio::OPUS_SAMPLE_RATE)
                + chunk.duration_ms,
        );
        match chunk.track {
            Track::Microphone => microphone.push((chunk.index, path)),
            Track::System => system.push((chunk.index, path)),
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

/// Creates storage-v2 headers/journals for sessions recorded by an older
/// build. Legacy checkpoints and sidecars stay in place, so the old version can
/// still open the session after a rollback.
fn migrate_legacy_sessions(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let mut migrated = 0;
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() || directory.join(SESSION_STATE_FILE).is_file() {
            continue;
        }
        let chunks = read_sidecar_chunks(&directory);
        if chunks.is_empty() {
            continue;
        }
        let session_id = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let legacy = latest_manifest(&directory);
        let started_at_epoch_ms = legacy
            .as_ref()
            .map(|manifest| manifest.started_at_epoch_ms)
            .filter(|value| *value > 0)
            .or_else(|| {
                chunks
                    .iter()
                    .map(|chunk| chunk.start_epoch_ms)
                    .filter(|value| *value > 0)
                    .min()
            })
            .or_else(|| legacy_marker(&directory).map(|marker| marker.started_at_epoch_ms))
            .unwrap_or(0);
        let status = if directory.join(COMPLETED_MARKER).is_file()
            || directory.join(RECOVERED_MARKER).is_file()
        {
            SessionStatus::Completed
        } else {
            SessionStatus::Recording
        };
        let committed_timeline_frames = chunks
            .iter()
            .map(|chunk| chunk.start_frame.saturating_add(chunk.frames))
            .max()
            .unwrap_or(0);
        let mut manifest = RecoveryManifest::new(&session_id);
        manifest.started_at_epoch_ms = started_at_epoch_ms;
        manifest.status = status;
        manifest.committed_timeline_frames = committed_timeline_frames;

        if sync_chunk_journal(&directory, &chunks).is_ok()
            && write_session_state(&directory, &manifest, chunks.len()).is_ok()
        {
            migrated += 1;
        }
    }
    migrated
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
    let anchors = microphone_anchors(snapshot.chunks.iter());
    let origin = timeline_origin_epoch(snapshot.chunks.iter(), started_at_epoch_ms);
    let mut track_end = [0_u64; 2];
    let mut committed_timeline_frames = 0_u64;
    for chunk in &snapshot.chunks {
        let source = track_of(chunk);
        let slot = usize::from(track_slot(chunk.track));
        let projected_ms = project_epoch_to_ms(chunk.start_epoch_ms, origin, &anchors);
        let timeline_start_frame = projected_ms
            .saturating_mul(TIMELINE_FRAMES_PER_MS)
            .max(track_end[slot]);
        let timeline_end_frame = timeline_start_frame.saturating_add(chunk.frames);
        track_end[slot] = timeline_end_frame;
        committed_timeline_frames = committed_timeline_frames.max(timeline_end_frame);
        let _ = manifest.timeline.push(TimelineSpan {
            source,
            generation: 0,
            source_start_frame: chunk.start_frame,
            timeline_start_frame,
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
    manifest.committed_timeline_frames = committed_timeline_frames;
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
        if name == SESSION_STATE_FILE {
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
    let state = read_session_state(directory);
    let journal = read_chunk_journal(directory);
    let manifest = latest_manifest(directory);
    let mut recorded = recorded_from_journal(directory, &journal)
        .or_else(|| {
            manifest
                .as_ref()
                .and_then(|manifest| recorded_from_manifest(directory, manifest))
        })
        .or_else(|| recorded_from_sidecars(directory))
        .or_else(|| recorded_from_wav(directory))
        .or_else(|| transcript_only_session(directory))?;
    if let Some(state) = &state
        && state.committed_timeline_frames > 0
    {
        recorded.duration_ms = recorded.duration_ms.max(
            state.committed_timeline_frames.saturating_mul(1_000)
                / u64::from(magnetic_tape_audio::OPUS_SAMPLE_RATE),
        );
    }
    let started_at_epoch_ms = state
        .as_ref()
        .map(|state| state.started_at_epoch_ms)
        .filter(|value| *value > 0)
        .or_else(|| {
            manifest
                .as_ref()
                .map(|manifest| manifest.started_at_epoch_ms)
                .filter(|value| *value > 0)
        })
        .or_else(|| {
            journal
                .iter()
                .map(|chunk| chunk.start_epoch_ms)
                .filter(|value| *value > 0)
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
    if let Err(error) = sync_chunk_journal(directory, &snapshot.chunks) {
        logging::warn(
            "recording",
            &format!("failed to append final chunk journal: {error}"),
        );
    }
    let sequence = state.sequence.fetch_add(1, Ordering::SeqCst);
    let manifest = build_manifest(
        state.session_id.as_deref().unwrap_or("unknown"),
        state.started_at_epoch_ms.unwrap_or(0),
        sequence,
        SessionStatus::Completed,
        snapshot,
    );
    if let Err(error) = write_session_state(directory, &manifest, snapshot.chunks.len()) {
        logging::warn(
            "recording",
            &format!("failed to write final session state: {error}"),
        );
    }
    if let Err(error) = write_checkpoint(directory, &manifest) {
        logging::warn(
            "recording",
            &format!("failed to write final checkpoint: {error}"),
        );
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
            let mut last_checkpoint = Instant::now();
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
                        logging::warn(
                            "audio",
                            &format!("failed to rebuild audio streams: {error}"),
                        );
                    }
                }
                let snapshot = active.snapshot();
                drop(guard);
                if snapshot.chunks.len() <= committed {
                    continue;
                }
                // Storage v2: append the new chunks, then replace the small
                // session header. The journal is the source of truth; the
                // legacy checkpoint is written only once a minute so an older
                // build can still open the session.
                if let Err(error) = sync_chunk_journal(&directory, &snapshot.chunks) {
                    logging::warn(
                        "recording",
                        &format!("failed to append chunk journal: {error}"),
                    );
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
                if let Err(error) = write_session_state(&directory, &manifest, committed) {
                    logging::warn(
                        "recording",
                        &format!("failed to write session state: {error}"),
                    );
                }
                if last_checkpoint.elapsed() >= Duration::from_secs(60) {
                    last_checkpoint = Instant::now();
                    if let Err(error) = write_checkpoint(&directory, &manifest) {
                        logging::warn("recording", &format!("failed to write checkpoint: {error}"));
                    }
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

#[tauri::command(async)]
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

#[tauri::command(async)]
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
        let Some(loaded) = load_session(&directory) else {
            continue;
        };
        // A transcript-only fallback has no reliable audio duration; never use
        // it to justify deleting Ogg files.
        if loaded.recorded.is_empty() {
            continue;
        }
        let started_at = loaded.started_at_epoch_ms;
        if started_at == 0
            || started_at >= cutoff_epoch_ms
            || !session_transcript_is_complete(&directory, loaded.recorded.duration_ms)
        {
            continue;
        }
        result.bytes_freed += discard_audio_files(&directory)?;
        result.sessions += 1;
    }
    Ok(result)
}

#[tauri::command(async)]
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
        let Some(loaded) = load_session(&directory) else {
            continue;
        };
        if loaded.recorded.is_empty() {
            continue;
        }
        if !session_transcript_is_complete(&directory, loaded.recorded.duration_ms) {
            continue;
        }
        candidates.push((loaded.started_at_epoch_ms, session_id.to_owned(), directory));
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

#[tauri::command(async)]
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

#[tauri::command(async)]
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

#[tauri::command(async)]
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
    write_session_state(&output, &initial, 0)?;
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
    sherpa_live: tauri::State<'_, Mutex<Option<SherpaLive>>>,
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
            *sherpa_live.lock().expect("sherpa live state poisoned") = None;
            {
                let mut live = live.lock().expect("live transcription state poisoned");
                live.active = false;
                live.stream_tx.take();
            }
            let mut state = state.lock().expect("session state poisoned");
            state.status = RecorderStatus::Idle;
            return Err(error.to_string());
        }
    };
    let mut state = state.lock().expect("session state poisoned");
    let Some(session_id) = state.session_id.clone() else {
        state.status = RecorderStatus::Idle;
        return Err("当前没有录音会话".into());
    };
    let directory = match recordings_root(&app) {
        Ok(root) => root.join(&session_id),
        Err(error) => {
            state.status = RecorderStatus::Idle;
            return Err(error);
        }
    };
    let mut recorded = finalize_session(&directory, &mut state, &snapshot);
    {
        let mut live = live.lock().expect("live transcription state poisoned");
        finish_live_transcription(&directory, &mut live);
    }
    *sherpa_live.lock().expect("sherpa live state poisoned") = None;
    let discard_audio = !settings
        .lock()
        .expect("settings state poisoned")
        .retain_audio;
    if discard_audio {
        if session_transcript_is_complete(&directory, recorded.duration_ms) {
            match discard_audio_files(&directory) {
                Ok(_) => {
                    recorded = RecordedSession {
                        duration_ms: recorded.duration_ms,
                        ..RecordedSession::default()
                    };
                }
                Err(error) => logging::warn(
                    "recording",
                    &format!("failed to discard audio after stop: {error}"),
                ),
            }
        } else {
            logging::warn(
                "recording",
                &format!(
                    "kept audio for session {session_id}: transcript covers less than 90% of the recording"
                ),
            );
        }
    }
    if let Err(error) = write_new_file(&directory.join(COMPLETED_MARKER), b"completed\n")
        && !directory.join(COMPLETED_MARKER).is_file()
    {
        logging::warn(
            "recording",
            &format!("failed to write completion marker: {error}"),
        );
    }
    // The recording is finalized even when a best-effort tail step failed, so
    // the next start/open command never sees a half-finished session.
    state.resumed_at_epoch_ms = None;
    state.status = RecorderStatus::Finished;
    state.accumulated_ms = recorded.duration_ms;
    state.recorded = Some(recorded);
    Ok(state.view(now_ms))
}

/// Copies speaker labels from an older transcript onto new segments whose
/// midpoint falls inside a labeled segment. Returns the number of labels
/// carried so the caller can log it.
fn carry_over_speakers(previous: &asr::Transcript, next: &mut asr::Transcript) -> usize {
    let labeled: Vec<&asr::TranscriptSegment> = previous
        .segments
        .iter()
        .filter(|segment| segment.speaker.is_some())
        .collect();
    if labeled.is_empty() {
        return 0;
    }
    let mut carried = 0;
    for segment in &mut next.segments {
        if segment.speaker.is_some() {
            continue;
        }
        let middle = (segment.start_ms + segment.end_ms) / 2;
        if let Some(source) = labeled
            .iter()
            .find(|source| middle >= source.start_ms && middle < source.end_ms)
        {
            segment.speaker = source.speaker.clone();
            carried += 1;
        }
    }
    carried
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

/// True when the transcript covers most of the expected recording duration.
/// This is a conservative guard: if the transcript is empty, corrupted, or
/// clearly shorter than the audio, the audio is kept.
fn transcript_is_complete(
    transcript: &asr::Transcript,
    expected_ms: u64,
    coverage: Option<&TranscriptCoverage>,
) -> bool {
    if transcript.text.trim().is_empty() && transcript.segments.is_empty() {
        return false;
    }
    let (covered_ms, failed_segments) = match coverage {
        Some(coverage) => (coverage.covered_ms, coverage.failed_segments),
        None => (
            transcript.duration_ms.max(
                transcript
                    .segments
                    .iter()
                    .map(|segment| segment.end_ms)
                    .max()
                    .unwrap_or(0),
            ),
            0,
        ),
    };
    if failed_segments > 0 {
        return false;
    }
    if expected_ms == 0 {
        return covered_ms > 0;
    }
    covered_ms.saturating_mul(1_000) >= expected_ms.saturating_mul(MIN_TRANSCRIPT_COVERAGE_PERMILLE)
}

fn session_transcript_is_complete(directory: &Path, expected_ms: u64) -> bool {
    let Some(transcript) = read_transcript(directory) else {
        return false;
    };
    let coverage = read_transcript_coverage(directory);
    transcript_is_complete(&transcript, expected_ms, coverage.as_ref())
}

/// Source text for AI jobs: the plain transcript when present, otherwise the
/// segment texts (some providers only return timed segments).
fn transcript_source(transcript: &asr::Transcript) -> String {
    if !transcript.text.trim().is_empty() {
        return transcript.text.clone();
    }
    transcript
        .segments
        .iter()
        .map(|segment| segment.text.trim())
        .collect::<Vec<_>>()
        .join("\n")
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

/// Timeline frames per millisecond at the internal 48 kHz rate.
const TIMELINE_FRAMES_PER_MS: u64 = 48;

/// Uses the earliest capture timestamp as a fallback when the session start
/// was not recorded (legacy sessions or a partially written manifest).
fn timeline_origin_epoch<'a>(
    chunks: impl Iterator<Item = &'a ChunkInfo>,
    session_start_epoch_ms: u64,
) -> u64 {
    if session_start_epoch_ms > 0 {
        return session_start_epoch_ms;
    }
    chunks
        .map(|chunk| chunk.start_epoch_ms)
        .filter(|epoch| *epoch > 0)
        .min()
        .unwrap_or(0)
}

/// Dense microphone chunks act as the session clock: they advance only while
/// recording is active, so pauses are excluded without persisting pause events.
fn microphone_anchors<'a>(chunks: impl Iterator<Item = &'a ChunkInfo>) -> Vec<(u64, u64)> {
    let mut anchors: Vec<(u64, u64)> = chunks
        .filter(|chunk| chunk.track == Track::Microphone && chunk.start_epoch_ms > 0)
        .map(|chunk| (chunk.start_epoch_ms, chunk.start_frame))
        .collect();
    anchors.sort_by_key(|(epoch, _)| *epoch);
    anchors
}

/// Maps a chunk's wall-clock capture time to a position on the shared session
/// timeline. Between two anchors the mapping is linear; before the first and
/// after the last anchor it extrapolates at 48 kHz.
fn project_epoch_to_ms(epoch_ms: u64, origin_epoch_ms: u64, anchors: &[(u64, u64)]) -> u64 {
    if epoch_ms == 0 {
        return 0;
    }
    let frame = if anchors.is_empty() {
        epoch_ms
            .saturating_sub(origin_epoch_ms)
            .saturating_mul(TIMELINE_FRAMES_PER_MS)
    } else {
        let (first_epoch, first_frame) = anchors[0];
        let (last_epoch, last_frame) = anchors[anchors.len() - 1];
        if epoch_ms <= first_epoch {
            first_frame.saturating_sub(
                first_epoch
                    .saturating_sub(epoch_ms)
                    .saturating_mul(TIMELINE_FRAMES_PER_MS),
            )
        } else if epoch_ms >= last_epoch {
            last_frame.saturating_add(
                epoch_ms
                    .saturating_sub(last_epoch)
                    .saturating_mul(TIMELINE_FRAMES_PER_MS),
            )
        } else {
            let index = anchors.partition_point(|(epoch, _)| *epoch <= epoch_ms);
            let (start_epoch, start_frame) = anchors[index - 1];
            let (end_epoch, end_frame) = anchors[index];
            if end_epoch <= start_epoch {
                start_frame
            } else {
                let offset = u128::from(epoch_ms - start_epoch);
                let span = u128::from(end_epoch - start_epoch);
                let frame_span = u128::from(end_frame.saturating_sub(start_frame));
                start_frame.saturating_add((offset * frame_span / span) as u64)
            }
        }
    };
    frame.saturating_mul(1_000) / u64::from(OPUS_SAMPLE_RATE)
}

fn audio_chunks(
    directory: &Path,
    session_start_epoch_ms: u64,
) -> Result<Vec<asr::AudioChunkRef>, String> {
    let mut infos = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") || name.starts_with("checkpoint-") || name == LEGACY_MARKER {
            continue;
        }
        if name == SESSION_STATE_FILE {
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
        infos.push((info, audio_path));
    }

    let anchors = microphone_anchors(infos.iter().map(|(info, _)| info));
    let origin = timeline_origin_epoch(infos.iter().map(|(info, _)| info), session_start_epoch_ms);
    let mut chunks = infos
        .into_iter()
        .map(|(info, audio_path)| asr::AudioChunkRef {
            track: info.track,
            path: audio_path,
            frames: info.frames,
            start_ms: project_epoch_to_ms(info.start_epoch_ms, origin, &anchors),
        })
        .collect::<Vec<_>>();
    chunks.sort_by_key(|chunk| (chunk.start_ms, track_slot(chunk.track)));
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
    transcription: tauri::State<'_, TranscriptionRun>,
) -> Result<asr::Transcript, String> {
    let (directory, session_id, expected_ms, started_at_epoch_ms) = {
        let state = state.lock().expect("session state poisoned");
        if state.status != RecorderStatus::Finished {
            return Err("只有已结束的录音才能转写".into());
        }
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| "当前没有录音会话".to_owned())?;
        let directory = recordings_root(&app)?.join(&session_id);
        let expected_ms = state
            .recorded
            .as_ref()
            .map(|recorded| recorded.duration_ms)
            .unwrap_or(state.accumulated_ms);
        let started_at_epoch_ms = state.started_at_epoch_ms.unwrap_or(0);
        (directory, session_id, expected_ms, started_at_epoch_ms)
    };
    let chunks = audio_chunks(&directory, started_at_epoch_ms)?;
    if chunks.is_empty() {
        return Err("当前会话没有可转写的 Opus 分片".into());
    }
    let guard = transcription.begin()?;
    let cancel = guard.flag();
    logging::info(
        "transcribe",
        &format!(
            "start provider={} session={} chunks={}",
            provider.as_str(),
            session_id,
            chunks.len()
        ),
    );
    let on_progress = |done: usize, total: usize| {
        let _ = app.emit(
            "transcription-progress",
            JobProgress {
                stage: "transcribe".into(),
                done,
                total,
            },
        );
    };
    // Kept for offline diarization: the local Whisper branch moves `chunks`
    // into its blocking task.
    let diarization_chunks = chunks.clone();
    let result = if provider == asr::AsrProvider::LocalWhisper {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
        let (model, settings_language, hotwords) = {
            let settings = settings.lock().expect("settings state poisoned");
            (
                settings.local_whisper_model.clone(),
                settings.language.clone(),
                settings.whisper_hotwords.clone(),
            )
        };
        let local_language = language
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(settings_language);
        let local_session = session_id.clone();
        let cancel_flag = Arc::clone(&cancel);
        let progress_app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let options = local_whisper::LocalOptions {
                model: &model,
                language: &local_language,
                initial_prompt: &hotwords,
            };
            let transcript = local_whisper::transcribe(
                &app_data,
                &chunks,
                &local_session,
                &options,
                &cancel_flag,
                &move |done, total| {
                    let _ = progress_app.emit(
                        "transcription-progress",
                        JobProgress {
                            stage: "transcribe".into(),
                            done,
                            total,
                        },
                    );
                },
            )?;
            Ok(asr::TranscriptionOutcome {
                covered_ms: transcript.duration_ms,
                failed_segments: 0,
                transcript,
            })
        })
        .await
        .map_err(|error| format!("本地转写任务失败：{error}"))?
    } else {
        let mut config = asr_state.credential(provider)?;
        if let Some(language) = language.filter(|value| !value.trim().is_empty()) {
            config.language = language;
        }
        asr::transcribe_chunks_with(&chunks, &config, &session_id, cancel.as_ref(), &on_progress)
            .await
    };
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            logging::error(
                "transcribe",
                &format!(
                    "failed provider={} session={session_id}: {error}",
                    provider.as_str()
                ),
            );
            return Err(error);
        }
    };
    let mut transcript = outcome.transcript;
    let coverage = TranscriptCoverage {
        covered_ms: outcome.covered_ms,
        duration_ms: transcript.duration_ms,
        failed_segments: outcome.failed_segments,
    };
    logging::info(
        "transcribe",
        &format!(
            "finished provider={} session={} segments={} covered_ms={} failed={}",
            provider.as_str(),
            session_id,
            transcript.segments.len(),
            coverage.covered_ms,
            coverage.failed_segments
        ),
    );
    // The decoder would need ~460 MB of f32 for a two-hour session, so skip
    // diarization before allocating the buffer; the live labels survive via
    // carry-over below.
    if transcript.duration_ms <= 7_200_000
        && let Ok(app_data) = app.path().app_data_dir()
        && sherpa::diarization_ready(&app_data)
    {
        match sherpa::session_pcm(&diarization_chunks) {
            Ok(pcm) => match sherpa::diarize(&app_data, &pcm) {
                Ok(spans) => {
                    let speakers = spans
                        .iter()
                        .map(|span| span.speaker)
                        .max()
                        .map_or(0, |speaker| speaker + 1);
                    let assigned = sherpa::assign_speakers(&mut transcript, &spans);
                    logging::info(
                        "transcribe",
                        &format!(
                            "offline diarization labeled {assigned} segments across {speakers} speakers"
                        ),
                    );
                }
                Err(error) => logging::error("transcribe", &format!("diarization failed: {error}")),
            },
            Err(error) => {
                logging::error("transcribe", &format!("diarization audio failed: {error}"))
            }
        }
    }
    // The live streaming engine already labeled speakers. A fresh batch
    // transcription has no diarization of its own, so carry the labels over
    // by time overlap instead of dropping them.
    if let Some(previous) = read_transcript(&directory) {
        let carried = carry_over_speakers(&previous, &mut transcript);
        if carried > 0 {
            logging::info(
                "transcribe",
                &format!("carried speaker labels onto {carried} segments"),
            );
        }
    }
    write_transcript(&directory, &transcript)?;
    write_transcript_coverage(&directory, &coverage)?;
    // A new transcript invalidates translations of the old text.
    clear_translations(&directory);
    let retain_audio = settings
        .lock()
        .expect("settings state poisoned")
        .retain_audio;
    if !retain_audio {
        if transcript_is_complete(&transcript, expected_ms, Some(&coverage)) {
            match discard_audio_files(&directory) {
                Ok(_) => {
                    let mut state = state.lock().expect("session state poisoned");
                    if state.session_id.as_deref() == Some(session_id.as_str()) {
                        state.recorded = Some(RecordedSession {
                            duration_ms: transcript.duration_ms,
                            ..RecordedSession::default()
                        });
                    }
                }
                Err(error) => logging::warn(
                    "transcribe",
                    &format!("failed to discard audio after transcription: {error}"),
                ),
            }
        } else {
            logging::warn(
                "transcribe",
                &format!(
                    "kept audio for session {session_id}: transcript covers less than 90% of the recording"
                ),
            );
        }
    }
    Ok(transcript)
}

#[tauri::command]
fn cancel_transcription(transcription: tauri::State<'_, TranscriptionRun>) -> bool {
    transcription.cancel()
}

/// Maps a display language to a stable file suffix.
fn translation_code(target_language: &str) -> String {
    let code: String = match target_language.trim() {
        "zh" | "zh-CN" | "中文" | "Chinese" => "zh".into(),
        "en" | "en-US" | "English" => "en".into(),
        other => other
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(16)
            .collect::<String>()
            .to_lowercase(),
    };
    if code.is_empty() {
        "target".into()
    } else {
        code
    }
}

fn translation_path(directory: &Path, code: &str) -> PathBuf {
    directory.join(format!("translation-{code}.md"))
}

fn write_text_atomic(path: &Path, content: &str) -> Result<(), String> {
    let temporary = path.with_extension("md.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_transcript(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<Option<asr::Transcript>, String> {
    let directory = current_session_directory(&app, &state)?;
    Ok(read_transcript(&directory))
}

/// One 5 second Opus chunk placed on the shared session timeline.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackChunk {
    path: PathBuf,
    start_ms: u64,
    duration_ms: u64,
}

/// Both tracks laid out on one timeline so the frontend can scrub and seek.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackTracks {
    duration_ms: u64,
    microphone: Vec<PlaybackChunk>,
    system: Vec<PlaybackChunk>,
}

#[tauri::command(async)]
fn get_session_playback(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<PlaybackTracks, String> {
    let started_at_epoch_ms = state
        .lock()
        .expect("session state poisoned")
        .started_at_epoch_ms
        .unwrap_or(0);
    let directory = current_session_directory(&app, &state)?;
    let chunks = audio_chunks(&directory, started_at_epoch_ms)?;
    let mut microphone = Vec::new();
    let mut system = Vec::new();
    let mut duration_ms = 0_u64;
    for chunk in chunks {
        let start_ms = chunk.start_ms;
        let chunk_duration_ms = chunk.frames.saturating_mul(1_000) / u64::from(OPUS_SAMPLE_RATE);
        let entry = PlaybackChunk {
            path: chunk.path,
            start_ms,
            duration_ms: chunk_duration_ms,
        };
        duration_ms = duration_ms.max(start_ms.saturating_add(chunk_duration_ms));
        match chunk.track {
            Track::Microphone => microphone.push(entry),
            Track::System => system.push(entry),
        }
    }
    microphone.sort_by_key(|chunk| chunk.start_ms);
    system.sort_by_key(|chunk| chunk.start_ms);
    Ok(PlaybackTracks {
        duration_ms,
        microphone,
        system,
    })
}

#[tauri::command(async)]
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
    let Some(transcript) = read_transcript(&directory) else {
        return Err("请先完成转写，再删除音频；转写是保留内容的唯一副本".into());
    };
    let expected_ms = state
        .recorded
        .as_ref()
        .map(|recorded| recorded.duration_ms)
        .unwrap_or(state.accumulated_ms);
    if !transcript_is_complete(
        &transcript,
        expected_ms,
        read_transcript_coverage(&directory).as_ref(),
    ) {
        return Err("转写覆盖不足 90%，已保留音频；请重新转写后再删除".into());
    }
    if !directory.join(AUDIO_DISCARDED).is_file() {
        discard_audio_files(&directory)?;
    }
    let duration_ms = transcript.duration_ms;
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
    asr_state: tauri::State<'_, AsrState>,
) -> Result<AppSettings, String> {
    let sanitized = settings.sanitized();
    let data_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    write_settings(&data_directory, &sanitized)?;
    *state.lock().expect("settings state poisoned") = sanitized.clone();
    // The hotwords reach the cloud providers through their prompt field, so
    // they have to be handed to every provider that may be used next.
    for provider in [
        asr::AsrProvider::MiniMax,
        asr::AsrProvider::SiliconFlow,
        asr::AsrProvider::OpenAiRealtime,
    ] {
        asr_state.set_hotwords(provider, sanitized.whisper_hotwords.clone());
    }
    Ok(sanitized)
}

fn start_openai_realtime(
    app: tauri::AppHandle,
    config: asr::AsrCredential,
    directory: PathBuf,
) -> Result<mpsc::UnboundedSender<Vec<i16>>, String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<i16>>();
    tauri::async_runtime::spawn(async move {
        let result: Result<(), String> = async {
            let mut request = config.endpoint.clone().into_client_request().map_err(|error| error.to_string())?;
            request.headers_mut().insert("Authorization", HeaderValue::from_str(&format!("Bearer {}", config.api_key)).map_err(|error| error.to_string())?);
            let (socket, _) = connect_async(request).await.map_err(|error| format!("Realtime WebSocket 连接失败：{error}"))?;
            let (mut writer, mut reader) = socket.split();
            let update = serde_json::json!({
                "type": "session.update",
                "session": {"type":"transcription","audio":{"input":{"format":{"type":"audio/pcm","rate":24000},"transcription":{"model":config.model},"turn_detection":null}}}
            });
            writer.send(Message::Text(update.to_string().into())).await.map_err(|error| error.to_string())?;
            const SAMPLE_RATE: usize = 24_000;
            const SILENCE_COMMIT_SAMPLES: usize = SAMPLE_RATE * 3 / 5;
            const MAX_TURN_SAMPLES: usize = SAMPLE_RATE * 15;
            const IDLE_CLEAR_SAMPLES: usize = SAMPLE_RATE * 5;
            const SPEECH_RMS: f32 = 0.012;
            let mut timeline_samples = 0usize;
            let mut turn_start_samples = 0usize;
            let mut turn_samples = 0usize;
            let mut silence_samples = 0usize;
            let mut in_speech = false;
            let mut closing = false;
            let mut pending_turns = VecDeque::<(u64, u64)>::new();
            loop {
                tokio::select! {
                    audio = rx.recv(), if !closing => match audio {
                        Some(samples) => {
                            let rms = if samples.is_empty() { 0.0 } else {
                                (samples.iter().map(|sample| { let value = f32::from(*sample) / f32::from(i16::MAX); value * value }).sum::<f32>() / samples.len() as f32).sqrt()
                            };
                            let bytes: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
                            let append = serde_json::json!({"type":"input_audio_buffer.append","audio":BASE64.encode(bytes)});
                            writer.send(Message::Text(append.to_string().into())).await.map_err(|error| error.to_string())?;
                            timeline_samples += samples.len();
                            turn_samples += samples.len();
                            if rms >= SPEECH_RMS {
                                in_speech = true;
                                silence_samples = 0;
                            } else if in_speech {
                                silence_samples += samples.len();
                            }
                            let should_commit = in_speech && (silence_samples >= SILENCE_COMMIT_SAMPLES || turn_samples >= MAX_TURN_SAMPLES);
                            if should_commit {
                                writer.send(Message::Text(r#"{"type":"input_audio_buffer.commit"}"#.into())).await.map_err(|error| error.to_string())?;
                                pending_turns.push_back(((turn_start_samples * 1_000 / SAMPLE_RATE) as u64, (timeline_samples * 1_000 / SAMPLE_RATE) as u64));
                                turn_start_samples = timeline_samples;
                                turn_samples = 0;
                                silence_samples = 0;
                                in_speech = false;
                            } else if !in_speech && turn_samples >= IDLE_CLEAR_SAMPLES {
                                writer.send(Message::Text(r#"{"type":"input_audio_buffer.clear"}"#.into())).await.map_err(|error| error.to_string())?;
                                turn_start_samples = timeline_samples;
                                turn_samples = 0;
                            }
                        }
                        None => {
                            if in_speech && turn_samples > 0 {
                                writer.send(Message::Text(r#"{"type":"input_audio_buffer.commit"}"#.into())).await.map_err(|error| error.to_string())?;
                                pending_turns.push_back(((turn_start_samples * 1_000 / SAMPLE_RATE) as u64, (timeline_samples * 1_000 / SAMPLE_RATE) as u64));
                            }
                            closing = true;
                            if pending_turns.is_empty() { let _ = writer.close().await; break; }
                        }
                    },
                    message = reader.next() => {
                        let Some(message) = message else { return Err("Realtime WebSocket 已断开".into()); };
                        let message = message.map_err(|error| error.to_string())?;
                        let Message::Text(text) = message else { continue; };
                        let event: serde_json::Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        let event_type = event["type"].as_str().unwrap_or_default();
                        let should_close = {
                            let live_state = app.state::<Mutex<LiveTranscriptionState>>();
                            let mut live = live_state.lock().expect("live transcription state poisoned");
                            match event_type {
                            "conversation.item.input_audio_transcription.delta" => {
                                live.partial_text.push_str(event["delta"].as_str().unwrap_or_default());
                                live.error = None;
                                false
                            }
                            "conversation.item.input_audio_transcription.completed" => {
                                let text = event["transcript"].as_str().unwrap_or_default().trim().to_owned();
                                let timing = pending_turns.pop_front();
                                if !text.is_empty() {
                                    let (start_ms, end_ms) = timing.unwrap_or_else(|| {
                                        let start = live.segments.last().map(|segment| segment.end_ms).unwrap_or(0);
                                        (start, start)
                                    });
                                    live.segments.push(asr::TranscriptSegment { start_ms, end_ms, speaker: None, text });
                                    persist_live_transcript(&directory, &live);
                                }
                                live.partial_text.clear();
                                live.error = None;
                                closing && pending_turns.is_empty()
                            }
                            "error" => {
                                live.error = Some(event["error"]["message"].as_str().unwrap_or("Realtime API 返回错误").to_owned());
                                false
                            }
                            _ => false
                            }
                        };
                        if should_close { let _ = writer.close().await; break; }
                    }
                }
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            app.state::<Mutex<LiveTranscriptionState>>()
                .lock()
                .expect("live transcription state poisoned")
                .error = Some(error);
        }
    });
    Ok(tx)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn start_live_transcription(
    provider: asr::AsrProvider,
    language: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    asr_state: tauri::State<'_, AsrState>,
    settings: tauri::State<'_, Mutex<AppSettings>>,
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
    sherpa_live: tauri::State<'_, Mutex<Option<SherpaLive>>>,
) -> Result<LiveTranscriptView, String> {
    let session_id = {
        let state = state.lock().expect("session state poisoned");
        if state.status != RecorderStatus::Recording {
            return Err("只有录音中的会话可以开启实时字幕".into());
        }
        state
            .session_id
            .clone()
            .ok_or_else(|| "当前没有录音会话".to_owned())?
    };
    let prefer_streaming = settings
        .lock()
        .expect("settings state poisoned")
        .live_streaming;
    if prefer_streaming
        && let Ok(app_data) = app.path().app_data_dir()
        && sherpa::streaming_ready(&app_data)
    {
        let asr_data = app_data.clone();
        let speaker_data = app_data.clone();
        let built = tauri::async_runtime::spawn_blocking(move || {
            let asr = sherpa::StreamingAsr::new(&asr_data)?;
            // Speaker labels are optional: captions still work without the
            // 28 MB embedding model.
            let speaker = sherpa::SpeakerEngine::new(&speaker_data).ok();
            Ok::<_, String>((asr, speaker))
        })
        .await;
        match built {
            Ok(Ok((asr, speaker))) => {
                *sherpa_live.lock().expect("sherpa live state poisoned") = Some(SherpaLive {
                    session_id: session_id.clone(),
                    asr,
                    speaker,
                    gallery: sherpa::SpeakerGallery::default(),
                });
                let mut live = live.lock().expect("live transcription state poisoned");
                *live = LiveTranscriptionState {
                    active: true,
                    session_id: Some(session_id),
                    provider: None,
                    model: "sherpa-onnx zh-14m".into(),
                    language: language
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "auto".into()),
                    streaming: true,
                    ..LiveTranscriptionState::default()
                };
                logging::info("sherpa", "local streaming captions started");
                return Ok(live_view(&live));
            }
            Ok(Err(error)) => logging::error(
                "sherpa",
                &format!("streaming recognizer unavailable, falling back to chunks: {error}"),
            ),
            Err(error) => logging::error(
                "sherpa",
                &format!("streaming recognizer task failed, falling back to chunks: {error}"),
            ),
        }
    }
    let (model, resolved_language, stream_tx) = if provider == asr::AsrProvider::LocalWhisper {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
        let preferred = settings
            .lock()
            .expect("settings state poisoned")
            .local_whisper_model
            .clone();
        let model = local_whisper::pick_live_model(&app_data, &preferred)?;
        let language = language
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "auto".to_owned());
        (model, language, None)
    } else {
        let config = asr_state.credential(provider)?;
        let stream_tx = if provider == asr::AsrProvider::OpenAiRealtime {
            let directory = recordings_root(&app)?.join(&session_id);
            Some(start_openai_realtime(
                app.clone(),
                config.clone(),
                directory,
            )?)
        } else {
            None
        };
        let language = language
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| config.language.clone());
        (config.model, language, stream_tx)
    };
    let mut live = live.lock().expect("live transcription state poisoned");
    *live = LiveTranscriptionState {
        active: true,
        session_id: Some(session_id),
        inflight: HashSet::new(),
        processed: HashSet::new(),
        first_seen: HashMap::new(),
        segments: Vec::new(),
        provider: Some(provider),
        model,
        language: resolved_language,
        error: None,
        partial_text: String::new(),
        stream_tx,
        streaming: false,
    };
    Ok(live_view(&live))
}

#[tauri::command]
fn get_live_transcript(
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
) -> LiveTranscriptView {
    live_view(&live.lock().expect("live transcription state poisoned"))
}

#[tauri::command]
fn stop_live_transcription(
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
    sherpa_live: tauri::State<'_, Mutex<Option<SherpaLive>>>,
) {
    {
        let mut live = live.lock().expect("live transcription state poisoned");
        live.active = false;
        live.stream_tx.take();
    }
    *sherpa_live.lock().expect("sherpa live state poisoned") = None;
}

fn track_slot(track: Track) -> u8 {
    match track {
        Track::Microphone => 0,
        Track::System => 1,
    }
}

fn poll_live_inner(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, Mutex<SessionState>>,
    recorder: &tauri::State<'_, RecorderState>,
    asr_state: &tauri::State<'_, AsrState>,
    settings: &tauri::State<'_, Mutex<AppSettings>>,
    live: &tauri::State<'_, Mutex<LiveTranscriptionState>>,
    sherpa_live: &tauri::State<'_, Mutex<Option<SherpaLive>>>,
) -> Result<(), String> {
    let (session_id, directory, started_at_epoch_ms) = {
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
        let started_at_epoch_ms = state.started_at_epoch_ms.unwrap_or(0);
        (session_id, directory, started_at_epoch_ms)
    };
    if live
        .lock()
        .expect("live transcription state poisoned")
        .streaming
    {
        let pcm = recorder
            .0
            .lock()
            .expect("audio recorder state poisoned")
            .as_ref()
            .map(AudioRecording::drain_realtime_pcm)
            .unwrap_or_default();
        let mut finals = Vec::new();
        let partial;
        {
            let mut guard = sherpa_live.lock().expect("sherpa live state poisoned");
            let Some(session) = guard.as_mut() else {
                return Ok(());
            };
            if session.session_id != session_id {
                return Ok(());
            }
            let chunks = session.asr.push(&pcm)?;
            for chunk in chunks {
                let embedding = session
                    .speaker
                    .as_ref()
                    .and_then(|engine| engine.compute(&chunk.audio).ok());
                let speaker = embedding.map(|values| session.gallery.assign(&values));
                finals.push(sherpa::labeled_segment(&chunk, speaker));
            }
            partial = session.asr.partial().to_owned();
        }
        let mut live = live.lock().expect("live transcription state poisoned");
        if !finals.is_empty() {
            live.segments.extend(finals);
            live.segments
                .sort_by_key(|segment| (segment.start_ms, segment.end_ms));
            persist_live_transcript(&directory, &live);
        }
        live.partial_text = partial;
        live.error = None;
        return Ok(());
    }
    let snapshot = recorder
        .0
        .lock()
        .expect("audio recorder state poisoned")
        .as_ref()
        .map(AudioRecording::snapshot);
    let stream_tx = live
        .lock()
        .expect("live transcription state poisoned")
        .stream_tx
        .clone();
    if let Some(stream_tx) = stream_tx {
        let pcm = recorder
            .0
            .lock()
            .expect("audio recorder state poisoned")
            .as_ref()
            .map(AudioRecording::drain_realtime_pcm)
            .unwrap_or_default();
        if !pcm.is_empty()
            && let Err(failed) = stream_tx.send(pcm)
        {
            let config = asr_state.credential(asr::AsrProvider::OpenAiRealtime)?;
            let replacement = start_openai_realtime(app.clone(), config, directory.clone())?;
            replacement
                .send(failed.0)
                .map_err(|_| "Realtime WebSocket 重连失败".to_owned())?;
            let mut live = live.lock().expect("live transcription state poisoned");
            live.stream_tx = Some(replacement);
            live.error = None;
        }
        return Ok(());
    }
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    let anchors = microphone_anchors(snapshot.chunks.iter());
    let origin = timeline_origin_epoch(snapshot.chunks.iter(), started_at_epoch_ms);
    let live_chunks = snapshot
        .chunks
        .iter()
        .map(|chunk| asr::AudioChunkRef {
            track: chunk.track,
            path: chunk.path.clone(),
            frames: chunk.frames,
            start_ms: project_epoch_to_ms(chunk.start_epoch_ms, origin, &anchors),
        })
        .collect::<Vec<_>>();

    let (ready, provider, language) = {
        let mut live = live.lock().expect("live transcription state poisoned");
        // Local inference is CPU bound, so run a single window at a time.
        let max_inflight: usize = if live.provider == Some(asr::AsrProvider::LocalWhisper) {
            1
        } else {
            2
        };
        let capacity = max_inflight.saturating_sub(live.inflight.len());
        if capacity == 0 {
            return Ok(());
        }
        let pending = live_chunks
            .iter()
            .filter(|chunk| {
                let bucket = chunk.start_ms / LIVE_WINDOW_MS * LIVE_WINDOW_MS;
                !live.processed.contains(&(track_slot(chunk.track), bucket))
                    && !live.inflight.contains(&bucket)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut slots: BTreeMap<u64, (Option<asr::AudioChunkRef>, Option<asr::AudioChunkRef>)> =
            BTreeMap::new();
        for chunk in pending {
            let mut slot = chunk.start_ms / LIVE_WINDOW_MS * LIVE_WINDOW_MS;
            loop {
                let entry = slots.entry(slot).or_default();
                let occupied = if chunk.track == Track::Microphone {
                    entry.0.is_some()
                } else {
                    entry.1.is_some()
                };
                if !occupied {
                    if chunk.track == Track::Microphone {
                        entry.0 = Some(chunk);
                    } else {
                        entry.1 = Some(chunk);
                    }
                    break;
                }
                slot = slot.saturating_add(LIVE_WINDOW_MS);
            }
        }
        for slot in slots.keys() {
            live.first_seen.entry(*slot).or_insert_with(Instant::now);
        }
        let latest = slots.keys().next_back().copied();
        let chosen = slots
            .iter()
            .filter(|(slot, entry)| {
                let complete = entry.0.is_some() && entry.1.is_some();
                let superseded = latest.is_some_and(|latest| **slot < latest);
                let stale = live
                    .first_seen
                    .get(slot)
                    .is_some_and(|seen| seen.elapsed() >= Duration::from_secs(1));
                complete || superseded || stale
            })
            .take(capacity)
            .map(|(slot, entry)| (*slot, entry.0.clone(), entry.1.clone()))
            .collect::<Vec<_>>();
        for (slot, microphone, system) in &chosen {
            live.inflight.insert(*slot);
            // Reserve only the tracks that are actually part of this request:
            // a late second track for the same window must still be able to
            // produce its own request instead of being dropped.
            if microphone.is_some() {
                live.processed.insert((0, *slot));
            }
            if system.is_some() {
                live.processed.insert((1, *slot));
            }
            live.first_seen.remove(slot);
        }
        (chosen, live.provider, live.language.clone())
    };

    if ready.is_empty() {
        return Ok(());
    }
    let provider = provider.ok_or_else(|| "实时转写未配置服务商".to_owned())?;
    let local = if provider == asr::AsrProvider::LocalWhisper {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
        let preferred = settings
            .lock()
            .expect("settings state poisoned")
            .local_whisper_model
            .clone();
        match local_whisper::pick_live_model(&app_data, &preferred) {
            Ok(model) => Some((app_data, model)),
            Err(error) => {
                reset_live_inflight(live, &ready);
                return Err(error);
            }
        }
    } else {
        None
    };
    let config = if local.is_none() {
        match asr_state.credential(provider) {
            Ok(mut config) => {
                if !language.is_empty() {
                    config.language = language.clone();
                }
                Some(config)
            }
            Err(error) => {
                reset_live_inflight(live, &ready);
                return Err(error);
            }
        }
    } else {
        None
    };
    for (slot, microphone, system) in ready {
        let task_app = app.clone();
        let task_session = session_id.clone();
        let task_directory = directory.clone();
        let hotwords = settings
            .lock()
            .expect("settings state poisoned")
            .whisper_hotwords
            .clone();
        if let Some((app_data, model)) = local.clone() {
            let chunks: Vec<asr::AudioChunkRef> = microphone.into_iter().chain(system).collect();
            let language = language.clone();
            tauri::async_runtime::spawn(async move {
                let session = task_session.clone();
                let result = tauri::async_runtime::spawn_blocking(move || {
                    let options = local_whisper::LocalOptions {
                        model: &model,
                        language: if language.is_empty() {
                            "auto"
                        } else {
                            language.as_str()
                        },
                        initial_prompt: &hotwords,
                    };
                    local_whisper::transcribe(
                        &app_data,
                        &chunks,
                        &session,
                        &options,
                        &AtomicBool::new(false),
                        &|_, _| {},
                    )
                })
                .await
                .map_err(|error| format!("本地实时转写任务失败：{error}"))
                .and_then(|result| result);
                apply_live_result(&task_app, slot, &task_session, &task_directory, result);
            });
        } else {
            let Some(task_config) = config.clone() else {
                continue;
            };
            tauri::async_runtime::spawn(async move {
                let result =
                    asr::transcribe_pair(microphone, system, &task_config, &task_session).await;
                apply_live_result(&task_app, slot, &task_session, &task_directory, result);
            });
        }
    }
    Ok(())
}

fn reset_live_inflight(
    live: &tauri::State<'_, Mutex<LiveTranscriptionState>>,
    ready: &[(u64, Option<asr::AudioChunkRef>, Option<asr::AudioChunkRef>)],
) {
    let mut live = live.lock().expect("live transcription state poisoned");
    for (slot, _, _) in ready {
        live.inflight.remove(slot);
        live.processed.remove(&(0, *slot));
        live.processed.remove(&(1, *slot));
    }
}

fn apply_live_result(
    app: &tauri::AppHandle,
    slot: u64,
    session: &str,
    directory: &Path,
    result: Result<asr::Transcript, String>,
) {
    let live_state = app.state::<Mutex<LiveTranscriptionState>>();
    let mut live = live_state
        .lock()
        .expect("live transcription state poisoned");
    live.inflight.remove(&slot);
    if live.session_id.as_deref() != Some(session) {
        return;
    }
    match result {
        Ok(transcript) => merge_live_transcript(&mut live, transcript),
        Err(error) => {
            // A transient failure must not permanently discard this window.
            live.processed.remove(&(0, slot));
            live.processed.remove(&(1, slot));
            live.error = Some(error);
        }
    }
    persist_live_transcript(directory, &live);
    if !live.active && !live.segments.is_empty() {
        let transcript = live_transcript(&live);
        let _ = write_transcript(directory, &transcript);
        let _ = write_transcript_coverage(
            directory,
            &TranscriptCoverage {
                covered_ms: transcript.duration_ms,
                duration_ms: transcript.duration_ms,
                failed_segments: 0,
            },
        );
    }
}

#[tauri::command(async)]
fn poll_live_transcription(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, RecorderState>,
    asr_state: tauri::State<'_, AsrState>,
    settings: tauri::State<'_, Mutex<AppSettings>>,
    live: tauri::State<'_, Mutex<LiveTranscriptionState>>,
    sherpa_live: tauri::State<'_, Mutex<Option<SherpaLive>>>,
) -> Result<LiveTranscriptView, String> {
    let active = live
        .lock()
        .expect("live transcription state poisoned")
        .active;
    if !active {
        return Ok(live_view(
            &live.lock().expect("live transcription state poisoned"),
        ));
    }
    let result = poll_live_inner(
        &app,
        &state,
        &recorder,
        &asr_state,
        &settings,
        &live,
        &sherpa_live,
    );
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
    let source = transcript_source(&transcript);
    let on_progress = |done: usize, total: usize| {
        let _ = app.emit(
            "translation-progress",
            JobProgress {
                stage: "translate".into(),
                done,
                total,
            },
        );
    };
    let cancel = AtomicBool::new(false);
    let translation = ai::translate_long_text(
        provider,
        &target_language,
        &source,
        &ai_state,
        &cancel,
        &on_progress,
    )
    .await?;
    let code = translation_code(&target_language);
    write_text_atomic(&translation_path(&directory, &code), &translation)?;
    Ok(translation)
}

#[tauri::command]
fn get_translation(
    target_language: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<Option<String>, String> {
    let directory = current_session_directory(&app, &state)?;
    let path = translation_path(&directory, &translation_code(&target_language));
    Ok(fs::read_to_string(path).ok())
}

#[tauri::command]
async fn summarize_session(
    provider: ai::AiProvider,
    source: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    ai_state: tauri::State<'_, AiState>,
) -> Result<String, String> {
    let directory = current_session_directory(&app, &state)?;
    let source = source
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| read_transcript(&directory).map(|transcript| transcript_source(&transcript)))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "请先完成转写或输入会议笔记".to_owned())?;
    let on_progress = |done: usize, total: usize| {
        let _ = app.emit(
            "summarize-progress",
            JobProgress {
                stage: "summarize".into(),
                done,
                total,
            },
        );
    };
    let cancel = AtomicBool::new(false);
    ai::summarize_text(provider, &source, &ai_state, &cancel, &on_progress).await
}

fn exports_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .document_dir()
        .map_err(|error| error.to_string())
        .map(|directory| directory.join("MagneticTape"))
}

#[tauri::command(async)]
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
    let chunks = audio_chunks(&directory, started_at_epoch_ms)?;
    let translations = read_translations(&directory);
    let root = exports_root(&app)?;
    let input = export::ExportInput {
        title: &title,
        session_id: &session_id,
        started_at_epoch_ms,
        duration_ms,
        transcript: transcript.as_ref(),
        translations: &translations,
        notes: &notes,
        chunks: &chunks,
    };
    export::export_all(&input, &root)
}

/// Reads every `translation-<code>.md` sidecar saved for a session.
fn read_translations(directory: &Path) -> Vec<export::TranslationDoc> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut translations = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(code) = name
            .strip_prefix("translation-")
            .and_then(|name| name.strip_suffix(".md"))
        else {
            continue;
        };
        if code.is_empty() {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path)
            && !content.trim().is_empty()
        {
            translations.push(export::TranslationDoc {
                code: code.to_owned(),
                content,
            });
        }
    }
    translations.sort_by(|left, right| left.code.cmp(&right.code));
    translations
}

/// Removes translations that no longer match the current transcript.
fn clear_translations(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && name.starts_with("translation-")
            && name.ends_with(".md")
        {
            let _ = fs::remove_file(&path);
        }
    }
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

#[tauri::command]
fn open_log_directory(app: tauri::AppHandle) -> Result<(), String> {
    let directory = logging::logs_dir(
        &app.path()
            .app_data_dir()
            .map_err(|error| format!("无法定位应用数据目录：{error}"))?,
    );
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建日志目录：{error}"))?;
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer.exe")
            .arg(&directory)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("无法打开日志目录：{error}"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = directory;
        Err("当前平台暂不支持打开日志目录".into())
    }
}

#[tauri::command]
fn export_diagnostics(app: tauri::AppHandle) -> Result<String, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    let summary = format!(
        "MagneticTape {}\nOS: {} {}\nlog dir: {}\nexported at (epoch ms): {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        logging::logs_dir(&app_data).display(),
        epoch_ms()
    );
    let destination = exports_root(&app)?.join(format!("diagnostics-{}.zip", epoch_ms()));
    let path = logging::write_diagnostics(&app_data, &destination, &summary)?;
    logging::info(
        "diagnostics",
        &format!("diagnostics exported to {}", path.display()),
    );
    Ok(path.display().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(SessionState::default()))
        .manage(RecorderState(Arc::new(Mutex::new(None::<AudioRecording>))))
        .manage(AiState::default())
        .manage(AsrState::default())
        .manage(TranscriptionRun::default())
        .manage(LocalWhisperState::default())
        .manage(sherpa::SherpaDownloadState::default())
        .manage(Mutex::new(LiveTranscriptionState::default()))
        .manage(Mutex::new(None::<SherpaLive>))
        .setup(|app| {
            let data_directory = app.path().app_data_dir()?;
            logging::init(&data_directory);
            logging::info(
                "app",
                &format!(
                    "MagneticTape {} starting on {} {}",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            );
            let recordings = data_directory.join("recordings");
            fs::create_dir_all(&recordings)?;
            let pruned = prune_session_checkpoints(&recordings);
            if pruned > 0 {
                logging::info(
                    "storage",
                    &format!("pruned {pruned} stale checkpoint files"),
                );
            }
            let migrated = migrate_legacy_sessions(&recordings);
            if migrated > 0 {
                logging::info(
                    "storage",
                    &format!("migrated {migrated} sessions to the chunk journal"),
                );
            }
            app.asset_protocol_scope()
                .allow_directory(&recordings, true)?;
            let settings = load_settings(&data_directory);
            {
                let asr_state = app.state::<AsrState>();
                for (name, profile) in &settings.asr_profiles {
                    let provider = match name.as_str() {
                        "minimax" => asr::AsrProvider::MiniMax,
                        "silicon_flow" => asr::AsrProvider::SiliconFlow,
                        "openai_realtime" => asr::AsrProvider::OpenAiRealtime,
                        _ => continue,
                    };
                    asr_state.seed(
                        provider,
                        profile.endpoint.clone(),
                        profile.model.clone(),
                        profile.language.clone(),
                    );
                }
                // Seed every provider so the saved hotwords apply even when a
                // provider has no profile of its own yet.
                for provider in [
                    asr::AsrProvider::MiniMax,
                    asr::AsrProvider::SiliconFlow,
                    asr::AsrProvider::OpenAiRealtime,
                ] {
                    asr_state.seed(
                        provider,
                        provider.default_endpoint().to_owned(),
                        provider.default_model().to_owned(),
                        settings.language.clone(),
                    );
                    asr_state.set_hotwords(provider, settings.whisper_hotwords.clone());
                }
            }
            if settings.auto_cleanup_days > 0 {
                let cutoff =
                    epoch_ms().saturating_sub(u64::from(settings.auto_cleanup_days) * 86_400_000);
                match cleanup_old_audio(&recordings, cutoff, None) {
                    Ok(result) if result.sessions > 0 => logging::info(
                        "storage",
                        &format!(
                            "auto cleanup removed audio from {} sessions ({} bytes)",
                            result.sessions, result.bytes_freed
                        ),
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        logging::error("storage", &format!("auto cleanup failed: {error}"))
                    }
                }
            }
            if settings.storage_quota_mb > 0 {
                let quota = u64::from(settings.storage_quota_mb) * 1_048_576;
                match enforce_storage_quota(&recordings, quota, None) {
                    Ok(result) if result.sessions > 0 => logging::info(
                        "storage",
                        &format!(
                            "quota cleanup removed audio from {} sessions ({} bytes)",
                            result.sessions, result.bytes_freed
                        ),
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        logging::error("storage", &format!("quota cleanup failed: {error}"))
                    }
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
            sherpa::get_sherpa_status,
            sherpa::download_sherpa,
            transcribe_session,
            cancel_transcription,
            get_transcript,
            get_session_playback,
            translate_transcript,
            get_translation,
            summarize_session,
            start_live_transcription,
            stop_live_transcription,
            poll_live_transcription,
            get_live_transcript,
            get_settings,
            save_settings,
            export_session,
            reveal_export,
            discard_session_audio,
            get_library_stats,
            cleanup_audio_older_than,
            local_whisper::get_local_whisper_status,
            local_whisper::download_local_whisper_engine,
            local_whisper::download_local_whisper_model,
            local_whisper::delete_local_whisper_model,
            open_log_directory,
            export_diagnostics
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

    fn write_test_chunk(directory: &Path, track: Track, index: u64, frames: u64) {
        let info = chunk(index, track, index * frames, frames);
        fs::write(
            directory.join(info.sidecar_path()),
            serde_json::to_vec(&info).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn audio_chunks_projects_a_sparse_system_track_to_session_time() {
        let directory = tempfile::tempdir().unwrap();
        let microphone = [
            chunk(0, Track::Microphone, 0, 241_920),
            chunk(1, Track::Microphone, 241_920, 241_920),
        ];
        let mut system = chunk(0, Track::System, 0, 241_920);
        system.start_epoch_ms = 1_501_000;

        for info in microphone.iter().chain(std::iter::once(&system)) {
            fs::write(
                directory.path().join(info.sidecar_path()),
                serde_json::to_vec(info).unwrap(),
            )
            .unwrap();
            fs::write(directory.path().join(format!("{}.ogg", info.uuid)), b"OggS").unwrap();
        }

        let chunks = audio_chunks(directory.path(), 1_000).unwrap();
        let projected = chunks
            .iter()
            .find(|chunk| chunk.track == Track::System)
            .unwrap();
        assert_eq!(projected.start_ms, 1_500_000);
    }

    #[test]
    fn storage_v2_journal_is_idempotent_and_survives_a_torn_line() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = vec![
            chunk(0, Track::Microphone, 0, 48_000),
            chunk(1, Track::Microphone, 48_000, 48_000),
            chunk(0, Track::System, 0, 48_000),
        ];
        for info in &chunks {
            fs::write(directory.path().join(format!("{}.ogg", info.uuid)), b"OggS").unwrap();
        }

        assert_eq!(
            sync_chunk_journal(directory.path(), &chunks[..2]).unwrap(),
            2
        );
        assert_eq!(sync_chunk_journal(directory.path(), &chunks).unwrap(), 1);
        assert_eq!(sync_chunk_journal(directory.path(), &chunks).unwrap(), 0);

        // Simulate a power loss in the middle of the last append.
        let journal = directory.path().join(CHUNK_JOURNAL_FILE);
        let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(br#"{"index":"#).unwrap();
        drop(file);

        assert_eq!(read_chunk_journal(directory.path()).len(), 3);
        let mut manifest = RecoveryManifest::new("session-1");
        manifest.started_at_epoch_ms = 1_000;
        manifest.status = SessionStatus::Completed;
        manifest.committed_timeline_frames = 96_000;
        write_session_state(directory.path(), &manifest, 3).unwrap();

        let loaded = load_session(directory.path()).unwrap();
        assert_eq!(loaded.recorded.microphone_chunks.len(), 2);
        assert_eq!(loaded.recorded.system_chunks.len(), 1);
        assert_eq!(loaded.recorded.duration_ms, 2_000);
        assert_eq!(loaded.started_at_epoch_ms, 1_000);
    }

    #[test]
    fn journal_continues_after_a_torn_line() {
        let directory = tempfile::tempdir().unwrap();
        let first = chunk(0, Track::Microphone, 0, 48_000);
        fs::write(
            directory.path().join(format!("{}.ogg", first.uuid)),
            b"OggS",
        )
        .unwrap();
        assert_eq!(sync_chunk_journal(directory.path(), &[first]).unwrap(), 1);

        let journal = directory.path().join(CHUNK_JOURNAL_FILE);
        let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(br#"{"index":"#).unwrap();
        drop(file);

        let second = chunk(1, Track::Microphone, 48_000, 48_000);
        fs::write(
            directory.path().join(format!("{}.ogg", second.uuid)),
            b"OggS",
        )
        .unwrap();
        assert_eq!(sync_chunk_journal(directory.path(), &[second]).unwrap(), 1);
        assert_eq!(read_chunk_journal(directory.path()).len(), 2);
    }

    #[test]
    fn legacy_sessions_are_migrated_to_the_chunk_journal() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("0000000000000001");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(LEGACY_MARKER),
            br#"{"startedAtEpochMs":1000,"title":"legacy"}"#,
        )
        .unwrap();
        for (track, index) in [(Track::Microphone, 0), (Track::System, 0)] {
            write_test_chunk(&directory, track, index, 48_000);
            let info = chunk(index, track, index * 48_000, 48_000);
            fs::write(directory.join(format!("{}.ogg", info.uuid)), b"OggS").unwrap();
        }

        assert_eq!(migrate_legacy_sessions(root.path()), 1);
        assert!(directory.join(SESSION_STATE_FILE).is_file());
        assert!(directory.join(CHUNK_JOURNAL_FILE).is_file());
        assert_eq!(read_chunk_journal(&directory).len(), 2);
        let loaded = load_session(&directory).unwrap();
        assert_eq!(loaded.started_at_epoch_ms, 1_000);
        assert_eq!(loaded.recorded.duration_ms, 1_000);
    }

    #[test]
    fn projection_keeps_sparse_system_audio_at_its_wall_time() {
        let origin = 1_000_000_u64;
        // Four dense microphone chunks, one every ~5.04s.
        let anchors = (0..4)
            .map(|index| (origin + index * 5_040, index * 241_920))
            .collect::<Vec<_>>();
        // A system chunk whose first packets only arrive 25 minutes in still
        // has source frame 0; the projection must put it at 25 minutes.
        let projected = project_epoch_to_ms(origin + 1_500_000, origin, &anchors);
        assert_eq!(projected, 1_500_000);
    }

    #[test]
    fn projection_interpolates_between_microphone_anchors() {
        let anchors = vec![(0, 0), (10_000, 48_000)];
        assert_eq!(project_epoch_to_ms(5_000, 0, &anchors), 500);
        assert_eq!(project_epoch_to_ms(25_000, 0, &anchors), 16_000);
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
            write_test_chunk(&directory, Track::Microphone, 0, 48_000);
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
        assert_eq!(stats.session_count, 3);
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
            write_test_chunk(&directory, Track::Microphone, 0, 48_000);
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

    #[test]
    fn concurrent_live_results_are_merged_in_timeline_order() {
        let mut live = LiveTranscriptionState::default();
        for (start_ms, text) in [(5_000, "第二段"), (0, "第一段")] {
            merge_live_transcript(
                &mut live,
                asr::Transcript {
                    provider: "minimax".into(),
                    model: "asr-1.0".into(),
                    language: "zh-CN".into(),
                    duration_ms: start_ms + 5_000,
                    text: text.into(),
                    segments: vec![asr::TranscriptSegment {
                        start_ms,
                        end_ms: start_ms + 5_000,
                        speaker: None,
                        text: text.into(),
                    }],
                    created_at_epoch_ms: 0,
                },
            );
        }
        assert_eq!(
            live.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            ["第一段", "第二段"]
        );
    }

    #[test]
    fn speaker_labels_carry_over_to_a_fresh_transcription() {
        let previous = asr::Transcript {
            provider: "sherpa".into(),
            model: "zh-14m".into(),
            language: "zh-CN".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![
                asr::TranscriptSegment {
                    start_ms: 0,
                    end_ms: 4_000,
                    speaker: Some("S1".into()),
                    text: "第一段".into(),
                },
                asr::TranscriptSegment {
                    start_ms: 4_000,
                    end_ms: 10_000,
                    speaker: Some("S2".into()),
                    text: "第二段".into(),
                },
            ],
            created_at_epoch_ms: 0,
        };
        let mut next = asr::Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![
                asr::TranscriptSegment {
                    start_ms: 500,
                    end_ms: 3_000,
                    speaker: None,
                    text: "第一段".into(),
                },
                asr::TranscriptSegment {
                    start_ms: 5_000,
                    end_ms: 9_000,
                    speaker: None,
                    text: "第二段".into(),
                },
            ],
            created_at_epoch_ms: 0,
        };
        assert_eq!(carry_over_speakers(&previous, &mut next), 2);
        assert_eq!(next.segments[0].speaker.as_deref(), Some("S1"));
        assert_eq!(next.segments[1].speaker.as_deref(), Some("S2"));
    }

    #[test]
    fn carrying_speakers_keeps_provider_labels_and_unmatched_segments() {
        let previous = asr::Transcript {
            provider: "sherpa".into(),
            model: "zh-14m".into(),
            language: "zh-CN".into(),
            duration_ms: 5_000,
            text: String::new(),
            segments: vec![asr::TranscriptSegment {
                start_ms: 0,
                end_ms: 2_000,
                speaker: Some("S1".into()),
                text: "旧".into(),
            }],
            created_at_epoch_ms: 0,
        };
        let mut next = asr::Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            duration_ms: 5_000,
            text: String::new(),
            segments: vec![
                asr::TranscriptSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    speaker: Some("S9".into()),
                    text: "自带标签".into(),
                },
                asr::TranscriptSegment {
                    start_ms: 3_000,
                    end_ms: 5_000,
                    speaker: None,
                    text: "没有重叠".into(),
                },
            ],
            created_at_epoch_ms: 0,
        };
        assert_eq!(carry_over_speakers(&previous, &mut next), 0);
        assert_eq!(next.segments[0].speaker.as_deref(), Some("S9"));
        assert!(next.segments[1].speaker.is_none());
    }

    #[test]
    fn translation_codes_are_stable_across_display_names() {
        assert_eq!(translation_code("中文"), "zh");
        assert_eq!(translation_code("zh-CN"), "zh");
        assert_eq!(translation_code("English"), "en");
        assert_eq!(translation_code("en-US"), "en");
        assert_eq!(translation_code("日本語"), "target");
        assert_eq!(translation_code(""), "target");
    }

    #[test]
    fn reads_and_clears_translation_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("translation-zh.md"), "大家好").unwrap();
        fs::write(directory.path().join("translation-en.md"), "   ").unwrap();
        fs::write(directory.path().join("notes.md"), "笔记").unwrap();
        let translations = read_translations(directory.path());
        assert_eq!(translations.len(), 1);
        assert_eq!(translations[0].code, "zh");
        clear_translations(directory.path());
        assert!(!directory.path().join("translation-zh.md").exists());
        assert!(directory.path().join("notes.md").exists());
    }

    #[test]
    fn transcript_coverage_requires_ninety_eight_percent_without_failures() {
        let transcript = asr::Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "auto".into(),
            duration_ms: 9_000,
            text: "内容".into(),
            segments: vec![asr::TranscriptSegment {
                start_ms: 0,
                end_ms: 9_000,
                speaker: None,
                text: "内容".into(),
            }],
            created_at_epoch_ms: 0,
        };
        let complete = TranscriptCoverage {
            covered_ms: 9_800,
            duration_ms: 10_000,
            failed_segments: 0,
        };
        assert!(transcript_is_complete(&transcript, 10_000, Some(&complete)));
        assert!(!transcript_is_complete(
            &transcript,
            10_000,
            Some(&TranscriptCoverage {
                covered_ms: 9_700,
                ..complete
            })
        ));
        assert!(!transcript_is_complete(
            &transcript,
            10_000,
            Some(&TranscriptCoverage {
                failed_segments: 1,
                ..complete
            })
        ));
        // Old transcripts have no coverage file; the duration fallback is
        // conservative (90% is no longer enough).
        assert!(!transcript_is_complete(&transcript, 10_000, None));

        let empty = asr::Transcript {
            text: String::new(),
            segments: Vec::new(),
            duration_ms: 0,
            ..transcript
        };
        assert!(!transcript_is_complete(&empty, 10_000, Some(&complete)));
    }

    #[test]
    fn transcription_guard_releases_the_slot_on_drop() {
        let run = TranscriptionRun::default();
        {
            let guard = run.begin().unwrap();
            assert!(run.begin().is_err());
            assert!(!guard.flag().load(Ordering::SeqCst));
        }
        assert!(run.begin().is_ok());
    }

    #[test]
    fn cancelling_marks_the_active_transcription() {
        let run = TranscriptionRun::default();
        let guard = run.begin().unwrap();
        assert!(run.cancel());
        assert!(guard.flag().load(Ordering::SeqCst));
    }

    #[test]
    fn startup_pruning_trims_legacy_checkpoint_piles() {
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("0000000000000001");
        fs::create_dir_all(&session).unwrap();

        for sequence in 0..15 {
            let mut manifest = RecoveryManifest::new("0000000000000001");
            manifest.checkpoint_sequence = sequence;
            fs::write(
                session.join(format!("checkpoint-{sequence:020}.json")),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        }

        let removed = prune_session_checkpoints(root.path());
        assert_eq!(removed, 5);
        let remaining = fs::read_dir(&session)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("checkpoint-"))
            })
            .count();
        assert_eq!(remaining, CHECKPOINT_RETENTION);
        let latest = RecoveryStore::open(&session)
            .unwrap()
            .load_latest()
            .unwrap()
            .unwrap();
        assert_eq!(latest.checkpoint_sequence, 14);
    }

    #[test]
    fn cleanup_keeps_audio_when_transcript_coverage_is_low() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("0000000000000001");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(LEGACY_MARKER),
            br#"{"startedAtEpochMs":1000,"title":"short"}"#,
        )
        .unwrap();
        fs::write(directory.join("mic-00000.ogg"), b"OggS-audio").unwrap();
        write_test_chunk(&directory, Track::Microphone, 0, 48_000);
        fs::write(
            directory.join(TRANSCRIPT_FILE),
            serde_json::to_vec(&asr::Transcript {
                provider: "minimax".into(),
                model: "asr-1.0".into(),
                language: "zh-CN".into(),
                duration_ms: 100,
                text: "只识别到开头".into(),
                segments: Vec::new(),
                created_at_epoch_ms: 0,
            })
            .unwrap(),
        )
        .unwrap();

        let result = cleanup_old_audio(root.path(), 10_000, None).unwrap();
        assert_eq!(result.sessions, 0);
        assert!(directory.join("mic-00000.ogg").exists());
    }
}
