use crate::{INTERNAL_SAMPLE_RATE, timeline::CaptureTimeline};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMetadata {
    pub index: u64,
    pub uuid: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_size: u32,
    pub duration_ms: u64,
    pub packet_count: u64,
    pub start_epoch_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Recording,
    Finalizing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryManifest {
    pub schema_version: u32,
    pub checkpoint_sequence: u64,
    pub session_id: String,
    pub sample_rate: u32,
    pub committed_timeline_frames: u64,
    pub status: SessionStatus,
    pub timeline: CaptureTimeline,
    pub chunks: Vec<ChunkMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestError {
    #[error("unsupported manifest schema version {0}")]
    UnsupportedSchema(u32),
    #[error("session id must not be empty")]
    EmptySessionId,
    #[error("internal sample rate must be 48000 Hz, got {0}")]
    InvalidSampleRate(u32),
    #[error("committed timeline exceeds the timeline extent")]
    InvalidCommittedExtent,
    #[error("chunk index {0} occurs more than once")]
    DuplicateChunk(u64),
    #[error("chunk {index} has invalid audio parameters")]
    InvalidChunk { index: u64 },
}

impl RecoveryManifest {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            checkpoint_sequence: 0,
            session_id: session_id.into(),
            sample_rate: INTERNAL_SAMPLE_RATE,
            committed_timeline_frames: 0,
            status: SessionStatus::Recording,
            timeline: CaptureTimeline::default(),
            chunks: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema(self.schema_version));
        }
        if self.session_id.trim().is_empty() {
            return Err(ManifestError::EmptySessionId);
        }
        if self.sample_rate != INTERNAL_SAMPLE_RATE {
            return Err(ManifestError::InvalidSampleRate(self.sample_rate));
        }
        if self.committed_timeline_frames > self.timeline.duration_frames() {
            return Err(ManifestError::InvalidCommittedExtent);
        }

        let mut indices = HashSet::new();
        for chunk in &self.chunks {
            if !indices.insert(chunk.index) {
                return Err(ManifestError::DuplicateChunk(chunk.index));
            }
            if chunk.uuid.is_empty()
                || chunk.sample_rate != self.sample_rate
                || !matches!(chunk.channels, 1 | 2)
                || chunk.frame_size == 0
            {
                return Err(ManifestError::InvalidChunk { index: chunk.index });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_existing_chunk_metadata_contract() {
        let json = include_str!("../../../doc/chunk_00000.json");
        let chunk: ChunkMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.sample_rate, INTERNAL_SAMPLE_RATE);
        assert_eq!(chunk.frame_size, 960);
        assert_eq!(chunk.packet_count, 151);
    }
}
