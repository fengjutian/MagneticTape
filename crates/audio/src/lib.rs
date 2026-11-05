//! Audio device discovery and, eventually, capture primitives.

use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;
use thiserror::Error;

mod monitor;
mod recording;

pub use monitor::{AudioLevelMonitor, AudioLevels, MonitorError};
pub use recording::{AudioRecording, RecordingError, RecordingResult, wav_duration_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Microphone,
    SystemOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceSnapshot {
    pub microphones: Vec<AudioDeviceInfo>,
    pub system_outputs: Vec<AudioDeviceInfo>,
}

#[derive(Debug, Error)]
pub enum AudioDeviceError {
    #[error("cannot enumerate audio devices: {0}")]
    Enumeration(#[from] cpal::Error),
}

pub fn enumerate_audio_devices() -> Result<AudioDeviceSnapshot, AudioDeviceError> {
    let host = cpal::default_host();
    let default_input = host.default_input_device();
    let default_output = host.default_output_device();

    let microphones = host
        .input_devices()?
        .filter_map(|device| {
            describe_device(device, DeviceKind::Microphone, default_input.as_ref())
        })
        .collect();
    let system_outputs = host
        .output_devices()?
        .filter_map(|device| {
            describe_device(device, DeviceKind::SystemOutput, default_output.as_ref())
        })
        .collect();

    Ok(AudioDeviceSnapshot {
        microphones,
        system_outputs,
    })
}

fn describe_device(
    device: cpal::Device,
    kind: DeviceKind,
    default: Option<&cpal::Device>,
) -> Option<AudioDeviceInfo> {
    let id = device.id().ok()?.to_string();
    let name = device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| device.to_string());
    let config = match kind {
        DeviceKind::Microphone => device.default_input_config(),
        DeviceKind::SystemOutput => device.default_output_config(),
    }
    .ok()?;

    Some(AudioDeviceInfo {
        id,
        name,
        kind,
        is_default: default.is_some_and(|candidate| candidate == &device),
        channels: config.channels(),
        sample_rate: config.sample_rate(),
        sample_format: config.sample_format().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_kind_serialization_is_stable() {
        assert_eq!(
            serde_json::to_string(&DeviceKind::SystemOutput).unwrap(),
            "\"system_output\""
        );
    }
}
