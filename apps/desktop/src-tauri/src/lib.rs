use serde::Serialize;
use std::{
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

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
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            status: RecorderStatus::Idle,
            session_id: None,
            started_at_epoch_ms: None,
            resumed_at_epoch_ms: None,
            accumulated_ms: 0,
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
}

impl SessionState {
    fn view(&self, now_ms: u64) -> SessionView {
        let active_ms = if self.status == RecorderStatus::Recording {
            now_ms.saturating_sub(self.resumed_at_epoch_ms.unwrap_or(now_ms))
        } else {
            0
        };
        SessionView {
            status: self.status,
            session_id: self.session_id.clone(),
            started_at_epoch_ms: self.started_at_epoch_ms,
            elapsed_ms: self.accumulated_ms.saturating_add(active_ms),
            microphone_ready: false,
            system_audio_ready: false,
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
    magnetic_tape_audio::enumerate_audio_devices().map_err(|error| error.to_string())
}

#[tauri::command]
fn start_audio_monitor(
    microphone_id: String,
    system_output_id: String,
    monitor: tauri::State<'_, Mutex<Option<magnetic_tape_audio::AudioLevelMonitor>>>,
) -> Result<magnetic_tape_audio::AudioLevels, String> {
    let next = magnetic_tape_audio::AudioLevelMonitor::start(&microphone_id, &system_output_id)
        .map_err(|error| error.to_string())?;
    let levels = next.levels();
    *monitor.lock().expect("audio monitor state poisoned") = Some(next);
    Ok(levels)
}

#[tauri::command]
fn get_audio_levels(
    monitor: tauri::State<'_, Mutex<Option<magnetic_tape_audio::AudioLevelMonitor>>>,
) -> magnetic_tape_audio::AudioLevels {
    monitor
        .lock()
        .expect("audio monitor state poisoned")
        .as_ref()
        .map_or_else(
            Default::default,
            magnetic_tape_audio::AudioLevelMonitor::levels,
        )
}

#[tauri::command]
fn stop_audio_monitor(
    monitor: tauri::State<'_, Mutex<Option<magnetic_tape_audio::AudioLevelMonitor>>>,
) {
    monitor.lock().expect("audio monitor state poisoned").take();
}

#[tauri::command]
fn get_session_state(state: tauri::State<'_, Mutex<SessionState>>) -> SessionView {
    state
        .lock()
        .expect("session state poisoned")
        .view(epoch_ms())
}

#[tauri::command]
fn start_session(state: tauri::State<'_, Mutex<SessionState>>) -> Result<SessionView, String> {
    let mut state = state.lock().expect("session state poisoned");
    if matches!(
        state.status,
        RecorderStatus::Recording | RecorderStatus::Paused
    ) {
        return Err("已有活动录音会话".into());
    }
    let now_ms = epoch_ms();
    *state = SessionState {
        status: RecorderStatus::Recording,
        session_id: Some(format!("{now_ms:016x}")),
        started_at_epoch_ms: Some(now_ms),
        resumed_at_epoch_ms: Some(now_ms),
        accumulated_ms: 0,
    };
    Ok(state.view(now_ms))
}

#[tauri::command]
fn pause_session(state: tauri::State<'_, Mutex<SessionState>>) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    state.pause(now_ms).map_err(str::to_owned)?;
    Ok(state.view(now_ms))
}

#[tauri::command]
fn resume_session(state: tauri::State<'_, Mutex<SessionState>>) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    state.resume(now_ms).map_err(str::to_owned)?;
    Ok(state.view(now_ms))
}

#[tauri::command]
fn stop_session(state: tauri::State<'_, Mutex<SessionState>>) -> Result<SessionView, String> {
    let now_ms = epoch_ms();
    let mut state = state.lock().expect("session state poisoned");
    if !matches!(
        state.status,
        RecorderStatus::Recording | RecorderStatus::Paused
    ) {
        return Err("没有可以停止的活动录音".into());
    }
    if state.status == RecorderStatus::Recording {
        state.accumulated_ms = state.view(now_ms).elapsed_ms;
    }
    state.resumed_at_epoch_ms = None;
    state.status = RecorderStatus::Finished;
    Ok(state.view(now_ms))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(SessionState::default()))
        .manage(Mutex::new(None::<magnetic_tape_audio::AudioLevelMonitor>))
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            start_audio_monitor,
            get_audio_levels,
            stop_audio_monitor,
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
