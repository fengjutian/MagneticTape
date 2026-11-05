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
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            microphone_level: AtomicU32::new(0.0_f32.to_bits()),
            system_level: AtomicU32::new(0.0_f32.to_bits()),
            paused: AtomicBool::new(false),
            dropped_blocks: AtomicU64::new(0),
            chunks: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// An active dual-source recording. Dropping it stops capture; call `finish`
/// to seal the last chunk and its metadata.
pub struct AudioRecording {
    microphone_stream: Option<Stream>,
    system_stream: Option<Stream>,
    sender: Option<SyncSender<AudioBlock>>,
    writer: Option<JoinHandle<Result<(), RecordingError>>>,
    shared: Arc<SharedState>,
}

impl AudioRecording {
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
        Ok(Self {
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
        }
    }

    pub fn levels(&self) -> AudioLevels {
        AudioLevels {
            microphone: f32::from_bits(self.shared.microphone_level.load(Ordering::Relaxed)),
            system: f32::from_bits(self.shared.system_level.load(Ordering::Relaxed)),
        }
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
