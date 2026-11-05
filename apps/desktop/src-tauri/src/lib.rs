use magnetic_tape_audio::{AudioLevels, AudioRecording, RecordingResult};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMarker {
    session_id: String,
    started_at_epoch_ms: u64,
    microphone_path: PathBuf,
    system_path: PathBuf,
}

const RECORDING_MARKER: &str = "session-recording.json";
const COMPLETED_MARKER: &str = "session-completed";
const RECOVERED_MARKER: &str = "session-recovered";

#[derive(Debug, Clone)]
struct SessionState {
    status: RecorderStatus,
    session_id: Option<String>,
    started_at_epoch_ms: Option<u64>,
    resumed_at_epoch_ms: Option<u64>,
    accumulated_ms: u64,
    recording: Option<RecordingResult>,
    recovered: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            status: RecorderStatus::Idle,
            session_id: None,
            started_at_epoch_ms: None,
            resumed_at_epoch_ms: None,
            accumulated_ms: 0,
            recording: None,
            recovered: false,
        }
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
    dropped_blocks: u64,
    recovered: bool,
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
        SessionView {
            status: self.status,
            session_id: self.session_id.clone(),
            started_at_epoch_ms: self.started_at_epoch_ms,
            elapsed_ms: self.accumulated_ms.saturating_add(active_ms),
            microphone_ready: active,
            system_audio_ready: active,
            microphone_path: self.recording.as_ref().map(|r| r.microphone_path.clone()),
            system_path: self.recording.as_ref().map(|r| r.system_path.clone()),
            dropped_blocks: self.recording.as_ref().map_or(0, |r| r.dropped_blocks),
            recovered: self.recovered,
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

fn write_session_marker(directory: &Path, marker: &SessionMarker) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(marker).map_err(|error| error.to_string())?;
    write_new_file(&directory.join(RECORDING_MARKER), &bytes)
}

#[tauri::command]
fn recover_latest_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
) -> Result<Option<SessionView>, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("recordings");
    if !root.exists() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        let marker_path = directory.join(RECORDING_MARKER);
        if !marker_path.is_file()
            || directory.join(COMPLETED_MARKER).exists()
            || directory.join(RECOVERED_MARKER).exists()
        {
            continue;
        }
        let mut marker: SessionMarker = serde_json::from_reader(
            fs::File::open(marker_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        marker.microphone_path = directory.join("microphone.wav");
        marker.system_path = directory.join("system.wav");
        if marker.microphone_path.is_file() || marker.system_path.is_file() {
            candidates.push(marker);
        }
    }
    let Some(marker) = candidates
        .into_iter()
        .max_by_key(|marker| marker.started_at_epoch_ms)
    else {
        return Ok(None);
    };
    if let Some(directory) = marker.microphone_path.parent() {
        write_new_file(&directory.join(RECOVERED_MARKER), b"recovered\n")?;
    }
    let elapsed_ms = [&marker.microphone_path, &marker.system_path]
        .into_iter()
        .filter(|path| path.is_file())
        .filter_map(|path| magnetic_tape_audio::wav_duration_ms(path).ok())
        .max()
        .unwrap_or(0);
    let mut state = state.lock().expect("session state poisoned");
    *state = SessionState {
        status: RecorderStatus::Finished,
        session_id: Some(marker.session_id),
        started_at_epoch_ms: Some(marker.started_at_epoch_ms),
        resumed_at_epoch_ms: None,
        accumulated_ms: elapsed_ms,
        recording: Some(RecordingResult {
            microphone_path: marker.microphone_path,
            system_path: marker.system_path,
            dropped_blocks: 0,
        }),
        recovered: true,
    };
    Ok(Some(state.view(epoch_ms())))
}

#[tauri::command]
fn list_audio_devices() -> Result<magnetic_tape_audio::AudioDeviceSnapshot, String> {
    magnetic_tape_audio::enumerate_audio_devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_audio_levels(recorder: tauri::State<'_, Mutex<Option<AudioRecording>>>) -> AudioLevels {
    recorder
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

#[tauri::command]
fn reveal_recording(path: PathBuf, app: tauri::AppHandle) -> Result<(), String> {
    let recordings_root = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("recordings")
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
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, Mutex<Option<AudioRecording>>>,
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
    let output = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("recordings")
        .join(&session_id);
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;
    let marker = SessionMarker {
        session_id: session_id.clone(),
        started_at_epoch_ms: now_ms,
        microphone_path: output.join("microphone.wav"),
        system_path: output.join("system.wav"),
    };
    write_session_marker(&output, &marker)?;
    let next = AudioRecording::start(&microphone_id, &system_output_id, &output)
        .map_err(|e| e.to_string())?;
    *recorder.lock().expect("audio recorder state poisoned") = Some(next);
    *state = SessionState {
        status: RecorderStatus::Recording,
        session_id: Some(session_id),
        started_at_epoch_ms: Some(now_ms),
        resumed_at_epoch_ms: Some(now_ms),
        accumulated_ms: 0,
        recording: None,
        recovered: false,
    };
    Ok(state.view(now_ms))
}

#[tauri::command]
fn pause_session(
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, Mutex<Option<AudioRecording>>>,
) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    state.pause(now_ms).map_err(str::to_owned)?;
    if let Some(active) = recorder
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
    recorder: tauri::State<'_, Mutex<Option<AudioRecording>>>,
) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    state.resume(now_ms).map_err(str::to_owned)?;
    if let Some(active) = recorder
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
    state: tauri::State<'_, Mutex<SessionState>>,
    recorder: tauri::State<'_, Mutex<Option<AudioRecording>>>,
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
        .lock()
        .expect("audio recorder state poisoned")
        .take()
        .ok_or_else(|| "录音器未运行".to_owned())?;
    let result = active.finish().map_err(|e| e.to_string())?;
    if let Some(directory) = result.microphone_path.parent() {
        write_new_file(&directory.join(COMPLETED_MARKER), b"completed\n")?;
    }
    let mut state = state.lock().expect("session state poisoned");
    if state.status == RecorderStatus::Recording {
        state.accumulated_ms = state.view(now_ms).elapsed_ms;
    }
    state.resumed_at_epoch_ms = None;
    state.status = RecorderStatus::Finished;
    state.recording = Some(result);
    Ok(state.view(now_ms))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(SessionState::default()))
        .manage(Mutex::new(None::<AudioRecording>))
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
            recover_latest_session,
            reveal_recording,
            start_session,
            pause_session,
            resume_session,
            stop_session,
            ai::configure_ai,
            ai::test_ai,
            ai::generate_ai
        ])
        .run(tauri::generate_context!())
        .expect("error while running MagneticTape desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_and_resume_exclude_the_paused_interval() {
        let mut state = SessionState {
            status: RecorderStatus::Recording,
            session_id: Some("session".into()),
            started_at_epoch_ms: Some(1_000),
            resumed_at_epoch_ms: Some(1_000),
            accumulated_ms: 0,
            recording: None,
            recovered: false,
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
}
