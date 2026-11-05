use serde::Serialize;
use std::{
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionState {
    status: &'static str,
    session_id: Option<String>,
    started_at_epoch_ms: Option<u128>,
    microphone_ready: bool,
    system_audio_ready: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            status: "idle",
            session_id: None,
            started_at_epoch_ms: None,
            microphone_ready: false,
            system_audio_ready: false,
        }
    }
}

#[tauri::command]
fn get_session_state(state: tauri::State<'_, Mutex<SessionState>>) -> SessionState {
    state.lock().expect("session state poisoned").clone()
}

#[tauri::command]
fn start_session(state: tauri::State<'_, Mutex<SessionState>>) -> SessionState {
    let mut state = state.lock().expect("session state poisoned");
    if state.status == "idle" {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        state.status = "recording";
        state.started_at_epoch_ms = Some(now.as_millis());
        state.session_id = Some(format!("{:032x}", now.as_nanos()));
    }
    state.clone()
}

#[tauri::command]
fn stop_session(state: tauri::State<'_, Mutex<SessionState>>) -> SessionState {
    let mut state = state.lock().expect("session state poisoned");
    *state = SessionState::default();
    state.clone()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(SessionState::default()))
        .invoke_handler(tauri::generate_handler![
            get_session_state,
            start_session,
            stop_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running MagneticTape desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_state_does_not_claim_audio_devices_are_ready() {
        let state = SessionState::default();
        assert_eq!(state.status, "idle");
        assert!(!state.microphone_ready);
        assert!(!state.system_audio_ready);
    }
}
