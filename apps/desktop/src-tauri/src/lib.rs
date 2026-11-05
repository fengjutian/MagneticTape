use magnetic_tape_audio::{AudioLevels, AudioRecording, RecordingResult};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecorderStatus {
    Idle,
    Recording,
    Paused,
    Finished,
}

#[derive(Debug, Clone)]
struct SessionState {
    status: RecorderStatus,
    session_id: Option<String>,
    started_at_epoch_ms: Option<u64>,
    resumed_at_epoch_ms: Option<u64>,
    accumulated_ms: u64,
    recording: Option<RecordingResult>,
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
    let next = AudioRecording::start(&microphone_id, &system_output_id, output)
        .map_err(|e| e.to_string())?;
    *recorder.lock().expect("audio recorder state poisoned") = Some(next);
    *state = SessionState {
        status: RecorderStatus::Recording,
        session_id: Some(session_id),
        started_at_epoch_ms: Some(now_ms),
        resumed_at_epoch_ms: Some(now_ms),
        accumulated_ms: 0,
        recording: None,
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
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            get_audio_levels,
            get_session_state,
            start_session,
            pause_session,
            resume_session,
            stop_session
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
