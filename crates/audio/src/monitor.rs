use cpal::{
    Device, SampleFormat, Stream, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioLevels {
    pub microphone: f32,
    pub system: f32,
}

#[derive(Debug, Error)]
pub enum MonitorError {
    #[error("audio device '{0}' is no longer available")]
    DeviceNotFound(String),
    #[error("cannot access audio device: {0}")]
    Device(#[from] cpal::Error),
    #[error("sample format {0} is not supported by the level monitor")]
    UnsupportedSampleFormat(SampleFormat),
}

#[derive(Default)]
struct SharedLevels {
    microphone: AtomicU32,
    system: AtomicU32,
}

/// Holds active CPAL streams. Dropping this value stops both streams.
pub struct AudioLevelMonitor {
    _microphone_stream: Stream,
    _system_stream: Stream,
    levels: Arc<SharedLevels>,
}

impl AudioLevelMonitor {
    pub fn start(microphone_id: &str, system_output_id: &str) -> Result<Self, MonitorError> {
        let host = cpal::default_host();
        let microphone = find_device(&host, microphone_id)?;
        let system_output = find_device(&host, system_output_id)?;
        let microphone_config = microphone.default_input_config()?;
        let system_config = system_output.default_output_config()?;
        let levels = Arc::new(SharedLevels::default());

        let microphone_stream = build_level_stream(
            &microphone,
            microphone_config,
            Arc::clone(&levels),
            LevelTarget::Microphone,
        )?;
        let system_stream = build_level_stream(
            &system_output,
            system_config,
            Arc::clone(&levels),
            LevelTarget::System,
        )?;
        microphone_stream.play()?;
        system_stream.play()?;

        Ok(Self {
            _microphone_stream: microphone_stream,
            _system_stream: system_stream,
            levels,
        })
    }

    pub fn levels(&self) -> AudioLevels {
        AudioLevels {
            microphone: f32::from_bits(self.levels.microphone.load(Ordering::Relaxed)),
            system: f32::from_bits(self.levels.system.load(Ordering::Relaxed)),
        }
    }
}

fn find_device(host: &cpal::Host, id: &str) -> Result<Device, MonitorError> {
    host.devices()?
        .find(|device| {
            device
                .id()
                .is_ok_and(|candidate| candidate.to_string() == id)
        })
        .ok_or_else(|| MonitorError::DeviceNotFound(id.to_owned()))
}

#[derive(Clone, Copy)]
enum LevelTarget {
    Microphone,
    System,
}

fn build_level_stream(
    device: &Device,
    supported: SupportedStreamConfig,
    levels: Arc<SharedLevels>,
    target: LevelTarget,
) -> Result<Stream, MonitorError> {
    let format = supported.sample_format();
    let config = supported.config();
    let error_levels = Arc::clone(&levels);
    let on_error = move |_error| store_level(&error_levels, target, 0.0);

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| store_level(&levels, target, rms_f32(data)),
            on_error,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| store_level(&levels, target, rms_i16(data)),
            on_error,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| store_level(&levels, target, rms_u16(data)),
            on_error,
            None,
        )?,
        other => return Err(MonitorError::UnsupportedSampleFormat(other)),
    };
    Ok(stream)
}

fn store_level(levels: &SharedLevels, target: LevelTarget, value: f32) {
    let smoothed = value.clamp(0.0, 1.0).sqrt();
    match target {
        LevelTarget::Microphone => levels
            .microphone
            .store(smoothed.to_bits(), Ordering::Relaxed),
        LevelTarget::System => levels.system.store(smoothed.to_bits(), Ordering::Relaxed),
    }
}

fn rms_f32(samples: &[f32]) -> f32 {
    rms(samples.iter().map(|sample| sample.clamp(-1.0, 1.0)))
}

fn rms_i16(samples: &[i16]) -> f32 {
    rms(samples
        .iter()
        .map(|sample| *sample as f32 / i16::MAX as f32))
}

fn rms_u16(samples: &[u16]) -> f32 {
    rms(samples
        .iter()
        .map(|sample| (*sample as f32 - 32_768.0) / 32_768.0))
}

fn rms(samples: impl Iterator<Item = f32>) -> f32 {
    let (sum, count) = samples.fold((0.0, 0_u64), |(sum, count), sample| {
        (sum + sample * sample, count + 1)
    });
    if count == 0 {
        0.0
    } else {
        (sum / count as f32).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_handles_silence_and_full_scale_samples() {
        assert_eq!(rms_f32(&[]), 0.0);
        assert_eq!(rms_f32(&[0.0, 0.0]), 0.0);
        assert!((rms_f32(&[1.0, -1.0]) - 1.0).abs() < f32::EPSILON);
        assert!((rms_i16(&[i16::MAX, -i16::MAX]) - 1.0).abs() < f32::EPSILON);
    }
}
