//! Chunked Ogg Opus encoding for a single capture track.
//!
//! Every chunk is an independent Ogg Opus logical stream with its own headers,
//! so a corrupt or truncated chunk can be discarded without losing the whole
//! recording. A JSON sidecar is committed next to each chunk after the audio
//! file has been flushed, which lets the recovery path rebuild the session
//! manifest even when no checkpoint was written before a crash.

use audiopus::{Application, Bitrate, Channels, SampleRate, coder::Encoder};
use ogg::{PacketWriteEndInfo, PacketWriter};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

/// Opus only accepts 48 kHz input for the capture pipeline.
pub const OPUS_SAMPLE_RATE: u32 = 48_000;
/// 60 ms frames keep Ogg page overhead low while staying inside the Opus limits.
pub const FRAME_SAMPLES: usize = 2_880;
/// Target duration of one independently decodable chunk.
pub const CHUNK_DURATION_MS: u64 = 15_000;
const CHUNK_FRAMES: u64 = CHUNK_DURATION_MS * OPUS_SAMPLE_RATE as u64 / 1_000;
const FRAMES_PER_PAGE: usize = 16;
const MAX_PACKET_BYTES: usize = 4_000;
const SERIAL_BASE: u32 = 0x4D54_0000;

#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("cannot write audio chunk: {0}")]
    Io(#[from] std::io::Error),
    #[error("Opus encoder failed: {0}")]
    Opus(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Track {
    Microphone,
    System,
}

impl Track {
    pub fn stem(self) -> &'static str {
        match self {
            Self::Microphone => "mic",
            Self::System => "sys",
        }
    }

    fn serial_offset(self) -> u32 {
        match self {
            Self::Microphone => 0,
            Self::System => 1,
        }
    }
}

/// Metadata for one committed chunk. It is serialized both into the sidecar
/// next to the chunk and into the session recovery manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkInfo {
    pub index: u64,
    pub uuid: String,
    pub track: Track,
    pub path: PathBuf,
    pub start_epoch_ms: u64,
    pub start_frame: u64,
    pub duration_ms: u64,
    pub frames: u64,
    pub packet_count: u64,
    pub channels: u8,
    pub sample_rate: u32,
    pub frame_size: u32,
    pub bytes: u64,
}

impl ChunkInfo {
    pub fn sidecar_path(&self) -> PathBuf {
        self.path.with_extension("json")
    }
}

/// Streaming linear resampler used only when the device does not run at 48 kHz.
struct LinearResampler {
    step: f64,
    position: f64,
    previous: Option<f32>,
}

impl LinearResampler {
    fn new(input_rate: u32) -> Option<Self> {
        (input_rate != OPUS_SAMPLE_RATE).then_some(Self {
            step: f64::from(input_rate) / f64::from(OPUS_SAMPLE_RATE),
            position: 0.0,
            previous: None,
        })
    }

    fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        for &sample in input {
            match self.previous {
                None => self.previous = Some(sample),
                Some(previous) => {
                    while self.position <= 1.0 {
                        output.push(previous + (sample - previous) * self.position as f32);
                        self.position += self.step;
                    }
                    self.position -= 1.0;
                    self.previous = Some(sample);
                }
            }
        }
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

fn opus_error(context: &str, error: audiopus::Error) -> ChunkError {
    ChunkError::Opus(format!("{context}: {error}"))
}

fn create_encoder() -> Result<(Encoder, u16), ChunkError> {
    let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Audio)
        .map_err(|error| opus_error("cannot create Opus encoder", error))?;
    encoder
        .set_bitrate(Bitrate::BitsPerSecond(32_000))
        .map_err(|error| opus_error("cannot configure Opus bitrate", error))?;
    encoder
        .set_vbr(true)
        .map_err(|error| opus_error("cannot configure Opus VBR", error))?;
    encoder
        .set_inband_fec(true)
        .map_err(|error| opus_error("cannot configure Opus in-band FEC", error))?;
    encoder
        .set_packet_loss_perc(10)
        .map_err(|error| opus_error("cannot configure Opus packet loss", error))?;
    let pre_skip = encoder
        .lookahead()
        .map_err(|error| opus_error("cannot read Opus lookahead", error))?
        .try_into()
        .unwrap_or(312);
    Ok((encoder, pre_skip))
}

/// Opus only accepts 2.5, 5, 10, 20, 40, and 60 ms frames. Round the final
/// partial frame up to the next legal size; the padding is shorter than 60 ms.
fn opus_frame_size(samples: usize) -> usize {
    [120, 240, 480, 960, 1920, FRAME_SAMPLES]
        .into_iter()
        .find(|size| *size >= samples)
        .unwrap_or(FRAME_SAMPLES)
}

fn opus_head(channels: u8, pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes());
    head.extend_from_slice(&0_i16.to_le_bytes());
    head.push(0);
    head
}

fn opus_tags() -> Vec<u8> {
    let vendor = b"MagneticTape";
    let mut tags = Vec::with_capacity(16 + vendor.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0_u32.to_le_bytes());
    tags
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// Encodes one track into 15 second Ogg Opus chunks inside `directory`.
pub struct OpusTrackWriter {
    directory: PathBuf,
    track: Track,
    input_channels: u16,
    resampler: Option<LinearResampler>,
    encoder: Option<Encoder>,
    pre_skip: u16,
    serial: u32,
    current_serial: u32,
    chunks: Arc<Mutex<Vec<ChunkInfo>>>,
    writer: Option<PacketWriter<'static, BufWriter<File>>>,
    pending: Vec<(Vec<u8>, u64)>,
    samples: Vec<f32>,
    chunk_frames: u64,
    chunk_packets: u64,
    chunk_start_frame: u64,
    chunk_start_epoch_ms: u64,
    total_frames: u64,
    next_index: u64,
}

impl OpusTrackWriter {
    pub fn new(
        directory: impl Into<PathBuf>,
        track: Track,
        input_sample_rate: u32,
        input_channels: u16,
        chunks: Arc<Mutex<Vec<ChunkInfo>>>,
    ) -> Result<Self, ChunkError> {
        Ok(Self {
            directory: directory.into(),
            track,
            input_channels: input_channels.max(1),
            resampler: LinearResampler::new(input_sample_rate),
            encoder: None,
            pre_skip: 312,
            serial: SERIAL_BASE + track.serial_offset(),
            current_serial: SERIAL_BASE + track.serial_offset(),
            chunks,
            writer: None,
            pending: Vec::with_capacity(FRAMES_PER_PAGE),
            samples: Vec::with_capacity(FRAME_SAMPLES * 2),
            chunk_frames: 0,
            chunk_packets: 0,
            chunk_start_frame: 0,
            chunk_start_epoch_ms: 0,
            total_frames: 0,
            next_index: 0,
        })
    }

    /// Pushes interleaved device samples. The writer downmixes to mono and
    /// resamples to 48 kHz before encoding.
    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<(), ChunkError> {
        if samples.is_empty() {
            return Ok(());
        }
        let channels = usize::from(self.input_channels);
        let mono = if channels == 1 {
            samples.to_vec()
        } else {
            samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect()
        };
        let mut converted = Vec::with_capacity(mono.len() + 64);
        match self.resampler.as_mut() {
            Some(resampler) => resampler.push(&mono, &mut converted),
            None => converted.extend_from_slice(&mono),
        }
        self.samples.extend_from_slice(&converted);
        while self.samples.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = self.samples.drain(..FRAME_SAMPLES).collect();
            self.encode_packet(&frame, FRAME_SAMPLES)?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), ChunkError> {
        if !self.samples.is_empty() {
            let real_frames = self.samples.len();
            let padded = opus_frame_size(real_frames);
            self.samples.resize(padded, 0.0);
            let frame = std::mem::take(&mut self.samples);
            self.encode_packet(&frame, real_frames)?;
        }
        self.finish_chunk()
    }

    /// Encodes one packet. `advance` is the number of *real* source samples in
    /// the frame; padding and the flush tail pass 0 so the final granule keeps
    /// the decoded duration exact.
    fn encode_packet(&mut self, frame: &[f32], advance: usize) -> Result<(), ChunkError> {
        self.ensure_chunk()?;
        let mut packet = vec![0_u8; MAX_PACKET_BYTES];
        let length = self
            .encoder
            .as_ref()
            .ok_or_else(|| ChunkError::Opus("encoder is not open".into()))?
            .encode_float(frame, &mut packet)
            .map_err(|error| {
                opus_error(
                    &format!("cannot encode a {}-sample frame", frame.len()),
                    error,
                )
            })?;
        packet.truncate(length);
        self.chunk_frames += advance as u64;
        self.total_frames += advance as u64;
        self.chunk_packets += 1;
        let granule = u64::from(self.pre_skip) + self.chunk_frames;
        self.pending.push((packet, granule));
        if self.pending.len() > FRAMES_PER_PAGE {
            self.flush_page(PacketWriteEndInfo::EndPage)?;
        }
        if advance > 0 && self.chunk_frames >= CHUNK_FRAMES {
            self.finish_chunk()?;
        }
        Ok(())
    }

    fn ensure_chunk(&mut self) -> Result<(), ChunkError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let (encoder, pre_skip) = create_encoder()?;
        self.encoder = Some(encoder);
        self.pre_skip = pre_skip;
        let index = self.next_index;
        let path = self
            .directory
            .join(format!("{}-{index:05}.ogg", self.track.stem()));
        let file = File::create(&path)?;
        let mut writer = PacketWriter::new(BufWriter::new(file));
        let serial = self.serial.wrapping_add((index as u32 & 0xFFFF) << 1);
        self.current_serial = serial;
        writer
            .write_packet(
                opus_head(1, pre_skip),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .map_err(ChunkError::Io)?;
        writer
            .write_packet(opus_tags(), serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(ChunkError::Io)?;
        self.writer = Some(writer);
        self.chunk_start_frame = self.total_frames;
        self.chunk_start_epoch_ms = epoch_ms();
        self.chunk_frames = 0;
        self.chunk_packets = 0;
        Ok(())
    }

    fn flush_page(&mut self, end: PacketWriteEndInfo) -> Result<(), ChunkError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let last = self.pending.len() - 1;
        let packets = std::mem::take(&mut self.pending);
        for (position, (packet, granule)) in packets.into_iter().enumerate() {
            let info = if position == last {
                end
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            self.writer
                .as_mut()
                .ok_or_else(|| ChunkError::Opus("chunk writer is not open".into()))?
                .write_packet(packet, self.current_serial, info, granule)
                .map_err(ChunkError::Io)?;
        }
        Ok(())
    }

    fn finish_chunk(&mut self) -> Result<(), ChunkError> {
        if self.writer.is_none() {
            return Ok(());
        }
        // Flush the encoder lookahead into the stream, then hide the padding
        // by keeping the final granule at the real frame count. Without this
        // tail every chunk would lose 312 samples (6.5 ms) at its end.
        let tail = [0_f32; FRAME_SAMPLES];
        self.encode_packet(&tail, 0)?;
        self.flush_page(PacketWriteEndInfo::EndStream)?;
        let writer = self.writer.take().expect("chunk writer checked above");
        self.encoder = None;
        let mut buffered = writer.into_inner();
        buffered.flush()?;
        let file = buffered
            .into_inner()
            .map_err(|error| ChunkError::Io(error.into_error()))?;
        file.sync_all()?;
        drop(file);

        let index = self.next_index;
        self.next_index += 1;
        let path = self
            .directory
            .join(format!("{}-{index:05}.ogg", self.track.stem()));
        let bytes = fs::metadata(&path)?.len();
        let info = ChunkInfo {
            index,
            uuid: format!("{}-{index:05}", self.track.stem()),
            track: self.track,
            path,
            start_epoch_ms: self.chunk_start_epoch_ms,
            start_frame: self.chunk_start_frame,
            duration_ms: self.chunk_frames * 1_000 / u64::from(OPUS_SAMPLE_RATE),
            frames: self.chunk_frames,
            packet_count: self.chunk_packets,
            channels: 1,
            sample_rate: OPUS_SAMPLE_RATE,
            frame_size: FRAME_SAMPLES as u32,
            bytes,
        };
        let sidecar = serde_json::to_vec_pretty(&info)
            .map_err(|error| ChunkError::Opus(error.to_string()))?;
        write_new_atomic(&info.sidecar_path(), &sidecar)?;
        self.chunks.lock().expect("chunk list poisoned").push(info);
        self.chunk_frames = 0;
        self.chunk_packets = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audiopus::{Channels as OpusChannels, SampleRate as OpusRate, coder::Decoder};
    use std::io::BufReader;

    fn tone(frames: usize, sample_rate: u32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                let phase = 2.0 * std::f32::consts::PI * 440.0 * index as f32 / sample_rate as f32;
                phase.sin() * 0.4
            })
            .collect()
    }

    #[test]
    fn writes_a_decodable_ogg_chunk_with_sidecar_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let mut writer = OpusTrackWriter::new(
            directory.path(),
            Track::Microphone,
            48_000,
            1,
            chunks.clone(),
        )
        .unwrap();
        writer.push_interleaved(&tone(48_000, 48_000)).unwrap();
        writer.finish().unwrap();

        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 1);
        let chunk = &chunks[0];
        assert!(chunk.path.exists());
        assert!(chunk.sidecar_path().exists());
        assert_eq!(chunk.frames, 48_000);
        assert_eq!(chunk.packet_count, 18);
        assert_eq!(chunk.duration_ms, 1_000);
        assert_eq!(&fs::read(&chunk.path).unwrap()[..4], b"OggS");

        let sidecar: ChunkInfo =
            serde_json::from_slice(&fs::read(chunk.sidecar_path()).unwrap()).unwrap();
        assert_eq!(sidecar, *chunk);

        let mut reader = ogg::PacketReader::new(BufReader::new(File::open(&chunk.path).unwrap()));
        let mut decoder = Decoder::new(OpusRate::Hz48000, OpusChannels::Mono).unwrap();
        let mut decoded_samples = 0_usize;
        let mut index = 0_usize;
        while let Some(packet) = reader.read_packet().unwrap() {
            if index >= 2 {
                let mut output = [0_i16; FRAME_SAMPLES];
                decoded_samples += decoder
                    .decode(Some(packet.data.as_slice()), &mut output[..], false)
                    .unwrap();
            }
            index += 1;
        }
        assert_eq!(decoded_samples, 50_880);
    }

    #[test]
    fn rotates_chunks_without_losing_frames() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let mut writer = OpusTrackWriter::new(
            directory.path(),
            Track::Microphone,
            48_000,
            1,
            chunks.clone(),
        )
        .unwrap();
        writer.push_interleaved(&tone(48_000 * 31, 48_000)).unwrap();
        writer.finish().unwrap();

        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].frames, 720_000);
        assert_eq!(chunks[1].frames, 720_000);
        assert_eq!(chunks[2].frames, 48_000);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.start_frame)
                .collect::<Vec<_>>(),
            vec![0, 720_000, 1_440_000]
        );
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.duration_ms)
                .collect::<Vec<_>>(),
            vec![15_000, 15_000, 1_000]
        );
    }

    #[test]
    fn resamples_non_48_khz_devices() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let mut writer =
            OpusTrackWriter::new(directory.path(), Track::System, 44_100, 1, chunks.clone())
                .unwrap();
        writer.push_interleaved(&tone(44_100, 44_100)).unwrap();
        writer.finish().unwrap();

        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 1);
        let duration = chunks[0].duration_ms;
        assert!((950..=1_050).contains(&duration), "duration was {duration}");
    }
}
