use crate::AudioLevels;
use cpal::{
    Device, SampleFormat, Stream, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use serde::Serialize;
use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use thiserror::Error;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error("audio device '{0}' is no longer available")]
    DeviceNotFound(String),
    #[error("cannot access audio device: {0}")]
    Device(#[from] cpal::Error),
    #[error("cannot create WAV recording: {0}")]
    Wav(#[from] hound::Error),
    #[error("cannot create recording directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("sample format {0} is not supported")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("audio writer thread stopped unexpectedly")]
    WriterStopped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingResult {
    pub microphone_path: PathBuf,
    pub system_path: PathBuf,
    pub dropped_blocks: u64,
}

#[derive(Clone, Copy)]
enum Source {
    Microphone,
    System,
}

struct AudioBlock {
    source: Source,
    samples: Vec<f32>,
}

struct SharedState {
    microphone_level: AtomicU32,
    system_level: AtomicU32,
    paused: AtomicBool,
    dropped_blocks: AtomicU64,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            microphone_level: AtomicU32::new(0.0_f32.to_bits()),
            system_level: AtomicU32::new(0.0_f32.to_bits()),
            paused: AtomicBool::new(false),
            dropped_blocks: AtomicU64::new(0),
        }
    }
}

/// An active dual-source recording. Dropping it stops capture; call `finish` to finalize WAV files.
pub struct AudioRecording {
    microphone_stream: Option<Stream>,
    system_stream: Option<Stream>,
    sender: Option<SyncSender<AudioBlock>>,
    writer: Option<JoinHandle<Result<(), RecordingError>>>,
    shared: Arc<SharedState>,
    microphone_path: PathBuf,
    system_path: PathBuf,
}

impl AudioRecording {
    pub fn start(
        microphone_id: &str,
        system_output_id: &str,
        output_directory: impl AsRef<Path>,
    ) -> Result<Self, RecordingError> {
        let output_directory = output_directory.as_ref();
        fs::create_dir_all(output_directory)?;
        let microphone_path = output_directory.join("microphone.wav");
        let system_path = output_directory.join("system.wav");
        let host = cpal::default_host();
        let microphone = find_device(&host, microphone_id)?;
        let system = find_device(&host, system_output_id)?;
        let microphone_config = microphone.default_input_config()?;
        let system_config = system.default_output_config()?;
        let (sender, receiver) = sync_channel(64);
        let writer = spawn_writer(
            receiver,
            &microphone_path,
            &system_path,
            microphone_config,
            system_config,
        )?;
        let shared = Arc::new(SharedState::default());
        let microphone_stream = build_stream(
            &microphone,
            microphone_config,
            sender.clone(),
            Arc::clone(&shared),
            Source::Microphone,
        )?;
        let system_stream = build_stream(
            &system,
            system_config,
            sender.clone(),
            Arc::clone(&shared),
            Source::System,
        )?;
        microphone_stream.play()?;
        system_stream.play()?;
        Ok(Self {
            microphone_stream: Some(microphone_stream),
            system_stream: Some(system_stream),
            sender: Some(sender),
            writer: Some(writer),
            shared,
            microphone_path,
            system_path,
        })
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
        if paused {
            self.shared
                .microphone_level
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
            self.shared
                .system_level
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn levels(&self) -> AudioLevels {
        AudioLevels {
            microphone: f32::from_bits(self.shared.microphone_level.load(Ordering::Relaxed)),
            system: f32::from_bits(self.shared.system_level.load(Ordering::Relaxed)),
        }
    }

    pub fn finish(mut self) -> Result<RecordingResult, RecordingError> {
        self.microphone_stream.take();
        self.system_stream.take();
        self.sender.take();
        self.writer
            .take()
            .ok_or(RecordingError::WriterStopped)?
            .join()
            .map_err(|_| RecordingError::WriterStopped)??;
        Ok(RecordingResult {
            microphone_path: self.microphone_path.clone(),
            system_path: self.system_path.clone(),
            dropped_blocks: self.shared.dropped_blocks.load(Ordering::Relaxed),
        })
    }
}

impl Drop for AudioRecording {
    fn drop(&mut self) {
        self.microphone_stream.take();
        self.system_stream.take();
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn find_device(host: &cpal::Host, id: &str) -> Result<Device, RecordingError> {
    host.devices()?
        .find(|device| {
            device
                .id()
                .is_ok_and(|candidate| candidate.to_string() == id)
        })
        .ok_or_else(|| RecordingError::DeviceNotFound(id.to_owned()))
}

fn spawn_writer(
    receiver: Receiver<AudioBlock>,
    microphone_path: &Path,
    system_path: &Path,
    microphone_config: SupportedStreamConfig,
    system_config: SupportedStreamConfig,
) -> Result<JoinHandle<Result<(), RecordingError>>, RecordingError> {
    let mut microphone = float_writer(microphone_path, microphone_config)?;
    let mut system = float_writer(system_path, system_config)?;
    Ok(thread::Builder::new()
        .name("magnetic-tape-wav-writer".into())
        .spawn(move || {
            let mut checkpoint_at = Instant::now() + CHECKPOINT_INTERVAL;
            loop {
                let timeout = checkpoint_at.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(timeout) {
                    Ok(block) => {
                        let writer = match block.source {
                            Source::Microphone => &mut microphone,
                            Source::System => &mut system,
                        };
                        for sample in block.samples {
                            writer.write_sample(sample)?;
                        }
                        if Instant::now() >= checkpoint_at {
                            checkpoint_writers(&mut microphone, &mut system)?;
                            checkpoint_at = Instant::now() + CHECKPOINT_INTERVAL;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        checkpoint_writers(&mut microphone, &mut system)?;
                        checkpoint_at = Instant::now() + CHECKPOINT_INTERVAL;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            microphone.finalize()?;
            system.finalize()?;
            Ok(())
        })?)
}

type FloatWavWriter = hound::WavWriter<BufWriter<std::fs::File>>;

fn checkpoint_writers(
    microphone: &mut FloatWavWriter,
    system: &mut FloatWavWriter,
) -> Result<(), hound::Error> {
    microphone.flush()?;
    system.flush()?;
    Ok(())
}

fn float_writer(
    path: &Path,
    config: SupportedStreamConfig,
) -> Result<FloatWavWriter, hound::Error> {
    hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
}

fn build_stream(
    device: &Device,
    supported: SupportedStreamConfig,
    sender: SyncSender<AudioBlock>,
    shared: Arc<SharedState>,
    source: Source,
) -> Result<Stream, RecordingError> {
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |samples: &[f32], _| submit(samples.to_vec(), &sender, &shared, source),
            |_| {},
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |samples: &[i16], _| {
                submit(
                    samples
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32)
                        .collect(),
                    &sender,
                    &shared,
                    source,
                )
            },
            |_| {},
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |samples: &[u16], _| {
                submit(
                    samples
                        .iter()
                        .map(|sample| (*sample as f32 - 32_768.0) / 32_768.0)
                        .collect(),
                    &sender,
                    &shared,
                    source,
                )
            },
            |_| {},
            None,
        )?,
        other => return Err(RecordingError::UnsupportedSampleFormat(other)),
    };
    Ok(stream)
}

fn submit(
    samples: Vec<f32>,
    sender: &SyncSender<AudioBlock>,
    shared: &SharedState,
    source: Source,
) {
    if shared.paused.load(Ordering::Acquire) {
        return;
    }
    let rms = if samples.is_empty() {
        0.0
    } else {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32)
            .sqrt()
            .clamp(0.0, 1.0)
            .sqrt()
    };
    match source {
        Source::Microphone => shared
            .microphone_level
            .store(rms.to_bits(), Ordering::Relaxed),
        Source::System => shared.system_level.store(rms.to_bits(), Ordering::Relaxed),
    }
    if let Err(error) = sender.try_send(AudioBlock { source, samples })
        && matches!(error, TrySendError::Full(_))
    {
        shared.dropped_blocks.fetch_add(1, Ordering::Relaxed);
    }
}
