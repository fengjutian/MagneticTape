use crate::{
    AudioLevels,
    opus::{ChunkError, ChunkInfo, OpusTrackWriter, Track},
};
use cpal::{
    Device, SampleFormat, Stream, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use serde::Serialize;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error("audio device '{0}' is no longer available")]
    DeviceNotFound(String),
    #[error("cannot access audio device: {0}")]
    Device(#[from] cpal::Error),
    #[error("cannot create audio chunk: {0}")]
    Chunk(#[from] ChunkError),
    #[error("cannot create recording directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("sample format {0} is not supported")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("audio device configuration changed; restart the recording")]
    ConfigurationChanged,
    #[error("audio writer thread stopped unexpectedly")]
    WriterStopped,
}

/// A finished or in-progress recording. `chunks` only contains chunks whose
/// sidecar metadata has already been committed to disk.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSnapshot {
    pub chunks: Vec<ChunkInfo>,
    pub dropped_blocks: u64,
    pub microphone_frames: u64,
    pub system_frames: u64,
}

impl RecordingSnapshot {
    pub fn chunks_for(&self, track: Track) -> impl Iterator<Item = &ChunkInfo> {
        self.chunks.iter().filter(move |chunk| chunk.track == track)
    }

    pub fn duration_ms(&self) -> u64 {
        let frames = self.microphone_frames.max(self.system_frames);
        frames.saturating_mul(1_000) / u64::from(crate::opus::OPUS_SAMPLE_RATE)
    }
}

/// Reads the duration of a legacy WAV recording created before chunked Opus.
pub fn wav_duration_ms(path: impl AsRef<Path>) -> Result<u64, hound::Error> {
    let reader = hound::WavReader::open(path)?;
    Ok(u64::from(reader.duration())
        .saturating_mul(1_000)
        .checked_div(u64::from(reader.spec().sample_rate))
        .unwrap_or(0))
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
    chunks: Arc<Mutex<Vec<ChunkInfo>>>,
    microphone_error: Mutex<Option<String>>,
    system_error: Mutex<Option<String>>,
    last_microphone_ms: AtomicU64,
    last_system_ms: AtomicU64,
    live_microphone: Mutex<Vec<f32>>,
    live_system: Mutex<Vec<f32>>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            microphone_level: AtomicU32::new(0.0_f32.to_bits()),
            system_level: AtomicU32::new(0.0_f32.to_bits()),
            paused: AtomicBool::new(false),
            dropped_blocks: AtomicU64::new(0),
            chunks: Arc::new(Mutex::new(Vec::new())),
            microphone_error: Mutex::new(None),
            system_error: Mutex::new(None),
            last_microphone_ms: AtomicU64::new(0),
            last_system_ms: AtomicU64::new(0),
            live_microphone: Mutex::new(Vec::new()),
            live_system: Mutex::new(Vec::new()),
        }
    }
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Snapshot of stream health used by the desktop watchdog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingHealth {
    pub microphone_error: Option<String>,
    pub system_error: Option<String>,
    /// True when the microphone stream stopped delivering blocks while the
    /// recording is running. The system loopback may legitimately be silent.
    pub stalled: bool,
    pub paused: bool,
}

impl RecordingHealth {
    pub fn needs_rebuild(&self) -> bool {
        self.microphone_error.is_some() || self.system_error.is_some() || self.stalled
    }
}

/// An active dual-source recording. Dropping it stops capture; call `finish`
/// to seal the last chunk and its metadata.
pub struct AudioRecording {
    microphone_id: String,
    system_output_id: String,
    microphone_config: SupportedStreamConfig,
    system_config: SupportedStreamConfig,
    microphone_stream: Option<Stream>,
    system_stream: Option<Stream>,
    sender: Option<SyncSender<AudioBlock>>,
    writer: Option<JoinHandle<Result<(), RecordingError>>>,
    shared: Arc<SharedState>,
}

impl AudioRecording {
    /// Drains captured audio and returns mixed 24 kHz mono PCM16 for realtime ASR.
    pub fn drain_realtime_pcm(&self) -> Vec<i16> {
        fn convert(input: &mut Vec<f32>, channels: u16, rate: u32) -> Vec<f32> {
            let source = std::mem::take(input);
            let channels = usize::from(channels.max(1));
            let mono: Vec<f32> = source
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
                .collect();
            if rate == 24_000 {
                return mono;
            }
            let step = f64::from(rate) / 24_000.0;
            let mut output = Vec::with_capacity((mono.len() as f64 / step) as usize + 1);
            let mut position = 0.0;
            while (position as usize) < mono.len() {
                output.push(mono[position as usize]);
                position += step;
            }
            output
        }
        let mut microphone = self
            .shared
            .live_microphone
            .lock()
            .expect("live microphone poisoned");
        let mut system = self
            .shared
            .live_system
            .lock()
            .expect("live system poisoned");
        let microphone = convert(
            &mut microphone,
            self.microphone_config.channels(),
            self.microphone_config.sample_rate(),
        );
        let system = convert(
            &mut system,
            self.system_config.channels(),
            self.system_config.sample_rate(),
        );
        let length = microphone.len().max(system.len());
        (0..length)
            .map(|index| {
                let sample = microphone.get(index).copied().unwrap_or(0.0)
                    + system.get(index).copied().unwrap_or(0.0);
                (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
            })
            .collect()
    }

    pub fn start(
        microphone_id: &str,
        system_output_id: &str,
        output_directory: impl AsRef<Path>,
    ) -> Result<Self, RecordingError> {
        let output_directory = output_directory.as_ref();
        std::fs::create_dir_all(output_directory)?;
        let host = cpal::default_host();
        let microphone = find_device(&host, microphone_id)?;
        let system = find_device(&host, system_output_id)?;
        let microphone_config = microphone.default_input_config()?;
        let system_config = system.default_output_config()?;
        let (sender, receiver) = sync_channel(64);
        let shared = Arc::new(SharedState::default());
        let writer = spawn_writer(
            receiver,
            output_directory,
            microphone_config,
            system_config,
            Arc::clone(&shared),
        )?;
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
        let now = epoch_ms();
        shared.last_microphone_ms.store(now, Ordering::Relaxed);
        shared.last_system_ms.store(now, Ordering::Relaxed);
        Ok(Self {
            microphone_id: microphone_id.to_owned(),
            system_output_id: system_output_id.to_owned(),
            microphone_config,
            system_config,
            microphone_stream: Some(microphone_stream),
            system_stream: Some(system_stream),
            sender: Some(sender),
            writer: Some(writer),
            shared,
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
        } else {
            let now = epoch_ms();
            self.shared.last_microphone_ms.store(now, Ordering::Relaxed);
            self.shared.last_system_ms.store(now, Ordering::Relaxed);
        }
    }

    pub fn levels(&self) -> AudioLevels {
        AudioLevels {
            microphone: f32::from_bits(self.shared.microphone_level.load(Ordering::Relaxed)),
            system: f32::from_bits(self.shared.system_level.load(Ordering::Relaxed)),
        }
    }

    pub fn health(&self) -> RecordingHealth {
        let paused = self.shared.paused.load(Ordering::Acquire);
        let now = epoch_ms();
        let last_microphone = self.shared.last_microphone_ms.load(Ordering::Relaxed);
        let stalled = !paused && last_microphone > 0 && now.saturating_sub(last_microphone) > 3_000;
        RecordingHealth {
            microphone_error: self
                .shared
                .microphone_error
                .lock()
                .expect("microphone error poisoned")
                .clone(),
            system_error: self
                .shared
                .system_error
                .lock()
                .expect("system error poisoned")
                .clone(),
            stalled,
            paused,
        }
    }

    /// Reopens both streams, keeping the encoder/writer alive. Used after a
    /// device loss, sleep/wake, or an audio service restart. The configuration
    /// must stay compatible with the running writer.
    pub fn rebuild(&mut self) -> Result<(), RecordingError> {
        self.microphone_stream.take();
        self.system_stream.take();
        let host = cpal::default_host();
        let microphone = match find_device(&host, &self.microphone_id) {
            Ok(device) => device,
            Err(RecordingError::DeviceNotFound(_)) => host
                .default_input_device()
                .ok_or_else(|| RecordingError::DeviceNotFound(self.microphone_id.clone()))?,
            Err(error) => return Err(error),
        };
        let system = match find_device(&host, &self.system_output_id) {
            Ok(device) => device,
            Err(RecordingError::DeviceNotFound(_)) => host
                .default_output_device()
                .ok_or_else(|| RecordingError::DeviceNotFound(self.system_output_id.clone()))?,
            Err(error) => return Err(error),
        };
        let microphone_config = microphone.default_input_config()?;
        let system_config = system.default_output_config()?;
        if microphone_config.sample_rate() != self.microphone_config.sample_rate()
            || microphone_config.channels() != self.microphone_config.channels()
            || system_config.sample_rate() != self.system_config.sample_rate()
            || system_config.channels() != self.system_config.channels()
        {
            return Err(RecordingError::ConfigurationChanged);
        }
        let sender = self
            .sender
            .as_ref()
            .ok_or(RecordingError::WriterStopped)?
            .clone();
        let microphone_stream = build_stream(
            &microphone,
            microphone_config,
            sender.clone(),
            Arc::clone(&self.shared),
            Source::Microphone,
        )?;
        let system_stream = build_stream(
            &system,
            system_config,
            sender,
            Arc::clone(&self.shared),
            Source::System,
        )?;
        microphone_stream.play()?;
        system_stream.play()?;
        self.microphone_stream = Some(microphone_stream);
        self.system_stream = Some(system_stream);
        let now = epoch_ms();
        self.shared.last_microphone_ms.store(now, Ordering::Relaxed);
        self.shared.last_system_ms.store(now, Ordering::Relaxed);
        *self
            .shared
            .microphone_error
            .lock()
            .expect("microphone error poisoned") = None;
        *self
            .shared
            .system_error
            .lock()
            .expect("system error poisoned") = None;
        Ok(())
    }

    /// Returns the chunks committed so far. Safe to call while recording.
    pub fn snapshot(&self) -> RecordingSnapshot {
        let chunks = self
            .shared
            .chunks
            .lock()
            .expect("chunk list poisoned")
            .clone();
        snapshot_from(chunks, self.shared.dropped_blocks.load(Ordering::Relaxed))
    }

    pub fn finish(mut self) -> Result<RecordingSnapshot, RecordingError> {
        self.microphone_stream.take();
        self.system_stream.take();
        self.sender.take();
        self.writer
            .take()
            .ok_or(RecordingError::WriterStopped)?
            .join()
            .map_err(|_| RecordingError::WriterStopped)??;
        Ok(self.snapshot())
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

fn snapshot_from(chunks: Vec<ChunkInfo>, dropped_blocks: u64) -> RecordingSnapshot {
    let microphone_frames = chunks
        .iter()
        .filter(|chunk| chunk.track == Track::Microphone)
        .map(|chunk| chunk.frames)
        .sum();
    let system_frames = chunks
        .iter()
        .filter(|chunk| chunk.track == Track::System)
        .map(|chunk| chunk.frames)
        .sum();
    RecordingSnapshot {
        chunks,
        dropped_blocks,
        microphone_frames,
        system_frames,
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
    output_directory: &Path,
    microphone_config: SupportedStreamConfig,
    system_config: SupportedStreamConfig,
    shared: Arc<SharedState>,
) -> Result<JoinHandle<Result<(), RecordingError>>, RecordingError> {
    let mut microphone = OpusTrackWriter::new(
        output_directory,
        Track::Microphone,
        microphone_config.sample_rate(),
        microphone_config.channels(),
        Arc::clone(&shared.chunks),
    )?;
    let mut system = OpusTrackWriter::new(
        output_directory,
        Track::System,
        system_config.sample_rate(),
        system_config.channels(),
        Arc::clone(&shared.chunks),
    )?;
    Ok(thread::Builder::new()
        .name("magnetic-tape-opus-writer".into())
        .spawn(move || {
            while let Ok(block) = receiver.recv() {
                match block.source {
                    Source::Microphone => microphone.push_interleaved(&block.samples)?,
                    Source::System => system.push_interleaved(&block.samples)?,
                }
            }
            microphone.finish()?;
            system.finish()?;
            Ok(())
        })?)
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
        SampleFormat::F32 => {
            let data_shared = Arc::clone(&shared);
            let error_shared = Arc::clone(&shared);
            device.build_input_stream(
                config,
                move |samples: &[f32], _| submit(samples.to_vec(), &sender, &data_shared, source),
                move |error| record_stream_error(&error_shared, source, error),
                None,
            )?
        }
        SampleFormat::I16 => {
            let data_shared = Arc::clone(&shared);
            let error_shared = Arc::clone(&shared);
            device.build_input_stream(
                config,
                move |samples: &[i16], _| {
                    submit(
                        samples
                            .iter()
                            .map(|sample| *sample as f32 / i16::MAX as f32)
                            .collect(),
                        &sender,
                        &data_shared,
                        source,
                    )
                },
                move |error| record_stream_error(&error_shared, source, error),
                None,
            )?
        }
        SampleFormat::U16 => {
            let data_shared = Arc::clone(&shared);
            let error_shared = Arc::clone(&shared);
            device.build_input_stream(
                config,
                move |samples: &[u16], _| {
                    submit(
                        samples
                            .iter()
                            .map(|sample| (*sample as f32 - 32_768.0) / 32_768.0)
                            .collect(),
                        &sender,
                        &data_shared,
                        source,
                    )
                },
                move |error| record_stream_error(&error_shared, source, error),
                None,
            )?
        }
        other => return Err(RecordingError::UnsupportedSampleFormat(other)),
    };
    Ok(stream)
}

fn record_stream_error(shared: &SharedState, source: Source, error: cpal::Error) {
    let message = error.to_string();
    match source {
        Source::Microphone => {
            *shared
                .microphone_error
                .lock()
                .expect("microphone error poisoned") = Some(message);
            shared
                .microphone_level
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
        }
        Source::System => {
            *shared.system_error.lock().expect("system error poisoned") = Some(message);
            shared
                .system_level
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
        }
    }
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
    let now = epoch_ms();
    const LIVE_BUFFER_SAMPLES: usize = 1_000_000;
    let append_live = |buffer: &Mutex<Vec<f32>>| {
        let mut buffer = buffer.lock().expect("live audio buffer poisoned");
        buffer.extend_from_slice(&samples);
        if buffer.len() > LIVE_BUFFER_SAMPLES {
            let overflow = buffer.len() - LIVE_BUFFER_SAMPLES;
            buffer.drain(..overflow);
        }
    };
    match source {
        Source::Microphone => {
            append_live(&shared.live_microphone);
            shared
                .microphone_level
                .store(rms.to_bits(), Ordering::Relaxed);
            shared.last_microphone_ms.store(now, Ordering::Relaxed);
        }
        Source::System => {
            append_live(&shared.live_system);
            shared.system_level.store(rms.to_bits(), Ordering::Relaxed);
            shared.last_system_ms.store(now, Ordering::Relaxed);
        }
    }
    if let Err(error) = sender.try_send(AudioBlock { source, samples })
        && matches!(error, TrySendError::Full(_))
    {
        shared.dropped_blocks.fetch_add(1, Ordering::Relaxed);
    }
}
