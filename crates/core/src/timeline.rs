use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    #[default]
    Microphone,
    System,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSpan {
    pub source: AudioSource,
    /// Increments whenever the capture device is rebuilt.
    pub generation: u64,
    /// First frame in the source capture generation.
    pub source_start_frame: u64,
    /// First frame on the session's shared monotonic timeline.
    pub timeline_start_frame: u64,
    pub frame_count: u64,
}

impl TimelineSpan {
    pub fn timeline_end_frame(&self) -> Option<u64> {
        self.timeline_start_frame.checked_add(self.frame_count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimelineError {
    #[error("timeline spans must contain at least one frame")]
    EmptySpan,
    #[error("timeline frame range overflows u64")]
    FrameOverflow,
    #[error("span overlaps an earlier span for {audio_source:?}")]
    Overlap { audio_source: AudioSource },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureTimeline {
    spans: Vec<TimelineSpan>,
}

impl CaptureTimeline {
    pub fn spans(&self) -> &[TimelineSpan] {
        &self.spans
    }

    pub fn push(&mut self, span: TimelineSpan) -> Result<(), TimelineError> {
        if span.frame_count == 0 {
            return Err(TimelineError::EmptySpan);
        }
        let end = span
            .timeline_end_frame()
            .ok_or(TimelineError::FrameOverflow)?;

        if self.spans.iter().rev().any(|previous| {
            previous.source == span.source
                && previous.timeline_end_frame().is_some_and(|previous_end| {
                    span.timeline_start_frame < previous_end && end > previous.timeline_start_frame
                })
        }) {
            return Err(TimelineError::Overlap {
                audio_source: span.source,
            });
        }

        self.spans.push(span);
        self.spans.sort_by_key(|item| item.timeline_start_frame);
        Ok(())
    }

    pub fn duration_frames(&self) -> u64 {
        self.spans
            .iter()
            .filter_map(TimelineSpan::timeline_end_frame)
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_two_sources_on_the_same_shared_range() {
        let mut timeline = CaptureTimeline::default();
        for source in [AudioSource::Microphone, AudioSource::System] {
            timeline
                .push(TimelineSpan {
                    source,
                    generation: 1,
                    source_start_frame: 0,
                    timeline_start_frame: 0,
                    frame_count: 48_000,
                })
                .unwrap();
        }

        assert_eq!(timeline.duration_frames(), 48_000);
    }

    #[test]
    fn rejects_overlap_within_one_source() {
        let mut timeline = CaptureTimeline::default();
        timeline
            .push(TimelineSpan {
                source: AudioSource::Microphone,
                generation: 1,
                source_start_frame: 0,
                timeline_start_frame: 0,
                frame_count: 100,
            })
            .unwrap();

        assert_eq!(
            timeline.push(TimelineSpan {
                source: AudioSource::Microphone,
                generation: 2,
                source_start_frame: 0,
                timeline_start_frame: 99,
                frame_count: 10,
            }),
            Err(TimelineError::Overlap {
                audio_source: AudioSource::Microphone
            })
        );
    }
}
