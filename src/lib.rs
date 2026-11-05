//! Core primitives for reliable meeting recording.

pub mod manifest;
pub mod recovery;
pub mod timeline;

pub const INTERNAL_SAMPLE_RATE: u32 = 48_000;
