//! Session export: subtitles, plain text, notes and a mixed 48 kHz WAV.
//!
//! Exports are written to `Documents/MagneticTape/<title>-<session>` so the
//! user can move them anywhere without a native save dialog.

use crate::asr::{AudioChunkRef, Transcript};
use magnetic_tape_audio::{OPUS_SAMPLE_RATE, Track, decode_ogg_opus};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub directory: PathBuf,
    pub files: Vec<PathBuf>,
}

pub struct ExportInput<'a> {
    pub title: &'a str,
    pub session_id: &'a str,
    pub started_at_epoch_ms: u64,
    pub duration_ms: u64,
    pub transcript: Option<&'a Transcript>,
    pub notes: &'a str,
    pub chunks: &'a [AudioChunkRef],
}

fn timestamp(milliseconds: u64, separator: char) -> String {
    let total = milliseconds / 1_000;
    let hours = total / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    let millis = milliseconds % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}{separator}{millis:03}")
}

fn speaker_prefix(speaker: Option<&str>) -> String {
    speaker
        .map(str::trim)
        .filter(|speaker| !speaker.is_empty())
        .map(|speaker| format!("[{speaker}] "))
        .unwrap_or_default()
}

pub fn srt(transcript: &Transcript) -> String {
    let mut output = String::new();
    for (index, segment) in transcript.segments.iter().enumerate() {
        output.push_str(&format!(
            "{}\n{} --> {}\n{}{}\n\n",
            index + 1,
            timestamp(segment.start_ms, ','),
            timestamp(segment.end_ms, ','),
            speaker_prefix(segment.speaker.as_deref()),
            segment.text.trim()
        ));
    }
    output
}

pub fn vtt(transcript: &Transcript) -> String {
    let mut output = String::from("WEBVTT\n\n");
    for segment in &transcript.segments {
        output.push_str(&format!(
            "{} --> {}\n{}{}\n\n",
            timestamp(segment.start_ms, '.'),
            timestamp(segment.end_ms, '.'),
            speaker_prefix(segment.speaker.as_deref()),
            segment.text.trim()
        ));
    }
    output
}

pub fn plain_text(transcript: &Transcript) -> String {
    let mut output = String::new();
    for segment in &transcript.segments {
        let minutes = segment.start_ms / 60_000;
        let seconds = (segment.start_ms % 60_000) / 1_000;
        output.push_str(&format!(
            "[{:02}:{:02}] {}{}\n",
            minutes,
            seconds,
            speaker_prefix(segment.speaker.as_deref()),
            segment.text.trim()
        ));
    }
    if output.is_empty() {
        output.push_str(&transcript.text);
    }
    output
}

pub fn markdown(input: &ExportInput<'_>) -> String {
    let mut output = format!(
        "# {}\n\n- 会话编号：`{}`\n- 开始时间：{} ms\n- 时长：{}\n",
        input.title,
        input.session_id,
        input.started_at_epoch_ms,
        crate::format_timestamp(input.duration_ms)
    );
    if let Some(transcript) = input.transcript {
        output.push_str(&format!(
            "- 转写服务：{} / {}\n",
            transcript.provider, transcript.model
        ));
        output.push_str("\n## 转写\n\n");
        if transcript.segments.is_empty() {
            output.push_str(&transcript.text);
            output.push('\n');
        } else {
            for segment in &transcript.segments {
                output.push_str(&format!(
                    "**[{}] {}{}**\n\n",
                    crate::format_timestamp(segment.start_ms),
                    speaker_prefix(segment.speaker.as_deref()),
                    segment.text.trim()
                ));
            }
        }
    }
    if !input.notes.trim().is_empty() {
        output.push_str("\n## 会议笔记\n\n");
        output.push_str(input.notes.trim());
        output.push('\n');
    }
    output
}

fn write_wav_header(file: &mut File, data_len: u32) -> std::io::Result<()> {
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes());
    header.extend_from_slice(&(OPUS_SAMPLE_RATE * 2).to_le_bytes());
    header.extend_from_slice(&2_u16.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    file.write_all(&header)
}

/// Decodes both tracks and writes one mixed mono 48 kHz WAV. Chunks are paired
/// by their start frame; missing audio is rendered as silence so the timeline
/// stays intact.
pub fn write_mixed_wav(chunks: &[AudioChunkRef], path: &Path) -> Result<u64, String> {
    let mut slots: BTreeMap<u64, (Option<&AudioChunkRef>, Option<&AudioChunkRef>)> =
        BTreeMap::new();
    for chunk in chunks {
        let entry = slots.entry(chunk.start_frame).or_default();
        match chunk.track {
            Track::Microphone => entry.0 = Some(chunk),
            Track::System => entry.1 = Some(chunk),
        }
    }
    let mut file = File::create(path).map_err(|error| error.to_string())?;
    write_wav_header(&mut file, 0).map_err(|error| error.to_string())?;
    let mut cursor = 0_u64;
    let mut samples_written = 0_u64;
    let mut buffer = Vec::new();
    for (start, (microphone, system)) in slots {
        if start > cursor {
            let silence = (start - cursor) as usize;
            buffer.extend(std::iter::repeat_n(0_i16, silence));
            samples_written += silence as u64;
        }
        let microphone = microphone
            .map(decode)
            .transpose()?
            .unwrap_or_default();
        let system = system.map(decode).transpose()?.unwrap_or_default();
        let length = microphone.len().max(system.len());
        for index in 0..length {
            let left = microphone.get(index).copied().unwrap_or(0.0);
            let right = system.get(index).copied().unwrap_or(0.0);
            buffer.push(((left + right).clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
        }
        samples_written += length as u64;
        cursor = start + length as u64;
        if buffer.len() >= 1_000_000 {
            write_samples(&mut file, &buffer)?;
            buffer.clear();
        }
    }
    write_samples(&mut file, &buffer)?;
    let data_len = samples_written * 2;
    let data_len = u32::try_from(data_len).map_err(|_| "导出音频超过 4 GB".to_owned())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    write_wav_header(&mut file, data_len).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(samples_written * 1_000 / u64::from(OPUS_SAMPLE_RATE))
}

fn decode(chunk: &AudioChunkRef) -> Result<Vec<f32>, String> {
    let mut samples = decode_ogg_opus(&chunk.path).map_err(|error| error.to_string())?;
    samples.truncate(chunk.frames as usize);
    Ok(samples)
}

fn write_samples(file: &mut File, samples: &[i16]) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    file.write_all(&bytes).map_err(|error| error.to_string())
}

fn sanitize_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('_');
    if cleaned.is_empty() {
        "recording".into()
    } else {
        cleaned.chars().take(60).collect()
    }
}

/// Writes every available artifact and returns the created directory.
pub fn export_all(
    input: &ExportInput<'_>,
    parent: &Path,
) -> Result<ExportResult, String> {
    let directory = parent.join(format!(
        "{}-{}",
        sanitize_component(input.title),
        &input.session_id[..input.session_id.len().min(8)]
    ));
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let mut files = Vec::new();

    let markdown = markdown(input);
    write_text(&directory.join("meeting.md"), &markdown)?;
    files.push(directory.join("meeting.md"));

    if let Some(transcript) = input.transcript {
        write_text(&directory.join("transcript.txt"), &plain_text(transcript))?;
        files.push(directory.join("transcript.txt"));
        if !transcript.segments.is_empty() {
            write_text(&directory.join("transcript.srt"), &srt(transcript))?;
            write_text(&directory.join("transcript.vtt"), &vtt(transcript))?;
            files.push(directory.join("transcript.srt"));
            files.push(directory.join("transcript.vtt"));
        }
    }

    if !input.notes.trim().is_empty() {
        write_text(&directory.join("notes.md"), input.notes.trim())?;
        files.push(directory.join("notes.md"));
    }

    if !input.chunks.is_empty() {
        let audio = directory.join("audio.wav");
        write_mixed_wav(input.chunks, &audio)?;
        files.push(audio);
    }

    let metadata = serde_json::json!({
        "title": input.title,
        "sessionId": input.session_id,
        "startedAtEpochMs": input.started_at_epoch_ms,
        "durationMs": input.duration_ms,
        "transcriptProvider": input.transcript.map(|transcript| transcript.provider.as_str()),
        "transcriptModel": input.transcript.map(|transcript| transcript.model.as_str()),
        "chunkCount": input.chunks.len(),
    });
    let bytes = serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?;
    write_text_bytes(&directory.join("session.json"), &bytes)?;
    files.push(directory.join("session.json"));

    Ok(ExportResult { directory, files })
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    write_text_bytes(path, content.as_bytes())
}

fn write_text_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    let mut file = File::create(path).map_err(|error| error.to_string())?;
    file.write_all(content).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::TranscriptSegment;
    use magnetic_tape_audio::{ChunkInfo, OpusTrackWriter};
    use std::sync::{Arc, Mutex};

    fn transcript() -> Transcript {
        Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            duration_ms: 3_000,
            text: "大家好。\n开始吧。".into(),
            segments: vec![
                TranscriptSegment {
                    start_ms: 0,
                    end_ms: 1_500,
                    speaker: Some("S1".into()),
                    text: "大家好。".into(),
                },
                TranscriptSegment {
                    start_ms: 1_500,
                    end_ms: 3_200,
                    speaker: None,
                    text: "开始吧。".into(),
                },
            ],
            created_at_epoch_ms: 0,
        }
    }

    #[test]
    fn formats_srt_vtt_and_plain_text() {
        let transcript = transcript();
        let srt = srt(&transcript);
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,500\n[S1] 大家好。"));
        assert!(srt.contains("2\n00:00:01,500 --> 00:00:03,200\n开始吧。"));
        let vtt = vtt(&transcript);
        assert!(vtt.starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:01.500"));
        let text = plain_text(&transcript);
        assert!(text.starts_with("[00:00] [S1] 大家好。"));
        assert!(text.contains("[00:01] 开始吧。"));
    }

    #[test]
    fn exports_markdown_notes_and_a_mixed_wav() {
        let directory = tempfile::tempdir().unwrap();
        let chunks_dir = directory.path().join("chunks");
        std::fs::create_dir_all(&chunks_dir).unwrap();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let mut microphone = OpusTrackWriter::new(
            &chunks_dir,
            Track::Microphone,
            48_000,
            1,
            Arc::clone(&chunks),
        )
        .unwrap();
        let mut system = OpusTrackWriter::new(
            &chunks_dir,
            Track::System,
            48_000,
            1,
            Arc::clone(&chunks),
        )
        .unwrap();
        let tone: Vec<f32> = (0..48_000)
            .map(|index| (index as f32 * 0.05).sin() * 0.2)
            .collect();
        microphone.push_interleaved(&tone).unwrap();
        system.push_interleaved(&tone).unwrap();
        microphone.finish().unwrap();
        system.finish().unwrap();
        let refs: Vec<AudioChunkRef> = chunks
            .lock()
            .unwrap()
            .iter()
            .map(|chunk: &ChunkInfo| AudioChunkRef {
                track: chunk.track,
                path: chunk.path.clone(),
                start_frame: chunk.start_frame,
                frames: chunk.frames,
            })
            .collect();

        let transcript = transcript();
        let input = ExportInput {
            title: "周会 / 2026",
            session_id: "0011223344556677",
            started_at_epoch_ms: 1_000,
            duration_ms: 1_000,
            transcript: Some(&transcript),
            notes: "讨论了下季度计划。",
            chunks: &refs,
        };
        let exported = export_all(&input, directory.path()).unwrap();
        assert!(exported.directory.exists());
        for name in [
            "meeting.md",
            "transcript.txt",
            "transcript.srt",
            "transcript.vtt",
            "notes.md",
            "audio.wav",
            "session.json",
        ] {
            assert!(exported.directory.join(name).exists(), "{name} missing");
        }
        let audio = std::fs::read(exported.directory.join("audio.wav")).unwrap();
        assert_eq!(&audio[..4], b"RIFF");
        let data_len = u32::from_le_bytes(audio[40..44].try_into().unwrap()) as usize;
        assert_eq!(data_len, audio.len() - 44);
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(exported.directory.join("session.json")).unwrap())
                .unwrap();
        assert_eq!(metadata["transcriptProvider"], "minimax");
    }
}
