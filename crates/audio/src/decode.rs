//! Decoding of on-disk Ogg Opus chunks back to mono 48 kHz PCM.
//!
//! Used by the transcription pipeline and by tests that verify the encoder
//! round-trips. Chunk files are independent logical streams, so each one is
//! decoded with a fresh decoder.

use audiopus::{Channels, SampleRate, coder::Decoder};
use ogg::PacketReader;
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("cannot open audio chunk: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot read Ogg container: {0}")]
    Ogg(#[from] ogg::OggReadError),
    #[error("cannot decode Opus packet: {0}")]
    Opus(String),
}

/// Decodes one Ogg Opus chunk into mono 48 kHz floating point samples.
///
/// The decoder applies the Opus pre-skip itself; the caller is expected to
/// truncate the result to the chunk's committed frame count so the encoder
/// flush tail used for granularity does not leak into the timeline.
pub fn decode_ogg_opus(path: impl AsRef<Path>) -> Result<Vec<f32>, DecodeError> {
    let path = path.as_ref();
    decode_reader(BufReader::new(File::open(path)?))
}

fn decode_reader(reader: BufReader<File>) -> Result<Vec<f32>, DecodeError> {
    let mut reader = PacketReader::new(reader);
    let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
        .map_err(|error| DecodeError::Opus(error.to_string()))?;
    let mut samples = Vec::new();
    let mut output = [0_i16; 5_760];
    let mut packet_index = 0_usize;
    while let Some(packet) = reader.read_packet()? {
        if packet_index < 2 {
            packet_index += 1;
            continue;
        }
        packet_index += 1;
        let decoded = decoder
            .decode(Some(packet.data.as_slice()), &mut output[..], false)
            .map_err(|error| DecodeError::Opus(error.to_string()))?;
        samples.extend(
            output[..decoded]
                .iter()
                .map(|sample| f32::from(*sample) / 32_768.0),
        );
    }
    Ok(samples)
}

/// Decodes all chunks of one track in index order and concatenates them,
/// truncating each chunk to its committed frame count.
pub fn decode_track(chunks: &[(PathBuf, u64)]) -> Result<Vec<f32>, DecodeError> {
    let mut track = Vec::new();
    for (path, frames) in chunks {
        let mut chunk = decode_ogg_opus(path)?;
        chunk.truncate(*frames as usize);
        track.extend_from_slice(&chunk);
    }
    Ok(track)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus::{ChunkInfo, OpusTrackWriter, Track};
    use std::sync::{Arc, Mutex};

    #[test]
    fn decodes_an_encoded_chunk_to_its_committed_length() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let mut writer = OpusTrackWriter::new(
            directory.path(),
            Track::Microphone,
            48_000,
            1,
            Arc::clone(&chunks),
        )
        .unwrap();
        let samples: Vec<f32> = (0..48_000)
            .map(|index| (index as f32 * 0.05).sin() * 0.3)
            .collect();
        writer.push_interleaved(&samples).unwrap();
        writer.finish().unwrap();

        let chunks = chunks.lock().unwrap();
        let chunk: &ChunkInfo = &chunks[0];
        let decoded = decode_ogg_opus(&chunk.path).unwrap();
        assert!(decoded.len() >= chunk.frames as usize);
        assert!(
            decoded.iter().any(|sample| sample.abs() > 0.05),
            "decoded audio must not be silent"
        );
        decoded[..chunk.frames as usize]
            .iter()
            .for_each(|sample| assert!(sample.abs() <= 1.2));
    }
}
