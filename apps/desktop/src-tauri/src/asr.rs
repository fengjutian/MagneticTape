//! Speech-to-text through OpenAI-compatible and MiniMax ASR APIs.
//!
//! The recording pipeline stores five-second Ogg Opus chunks. Before upload the
//! chunks are decoded, mixed into one mono track, resampled to 16 kHz and
//! packed into WAV segments that stay below the providers' 500 second / 50 MB
//! limits.

use magnetic_tape_audio::{OPUS_SAMPLE_RATE, Track, decode_ogg_opus};
use reqwest::{
    Client,
    multipart::{Form, Part},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ASR_SERVICE: &str = "com.magnetictape.desktop.asr";
const ASR_SAMPLE_RATE: u32 = 16_000;
const WAV_SEGMENT_SECONDS: u64 = 480;
const BATCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const LIVE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Returned when the user cancels a transcription run.
pub const CANCELLED: &str = "已取消转写";
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrProvider {
    #[serde(rename = "minimax")]
    MiniMax,
    #[serde(rename = "silicon_flow")]
    SiliconFlow,
    #[serde(rename = "openai_realtime")]
    OpenAiRealtime,
    /// whisper.cpp running entirely on this machine. No credentials or network.
    #[serde(rename = "local_whisper")]
    LocalWhisper,
}

impl AsrProvider {
    fn credential_name(self) -> &'static str {
        match self {
            Self::MiniMax => "minimax",
            Self::SiliconFlow => "siliconflow",
            Self::OpenAiRealtime => "openai-realtime",
            Self::LocalWhisper => "local-whisper",
        }
    }

    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::MiniMax => "https://api.minimax.cn/v1/speech_to_text",
            Self::SiliconFlow => "https://api.siliconflow.cn/v1/audio/transcriptions",
            Self::OpenAiRealtime => "wss://api.openai.com/v1/realtime?intent=transcription",
            Self::LocalWhisper => "local://whisper.cpp",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::MiniMax => "asr-1.0",
            Self::SiliconFlow => "FunAudioLLM/SenseVoiceSmall",
            Self::OpenAiRealtime => "gpt-live-transcribe",
            Self::LocalWhisper => crate::local_whisper::DEFAULT_MODEL,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MiniMax => "minimax",
            Self::SiliconFlow => "siliconflow",
            Self::OpenAiRealtime => "openai_realtime",
            Self::LocalWhisper => "local_whisper",
        }
    }
}

#[derive(Clone)]
pub struct AsrCredential {
    pub provider: AsrProvider,
    pub api_key: String,
    pub endpoint: String,
    pub model: String,
    pub language: String,
    /// Hints about names and jargon. Sent to providers that support a prompt
    /// field, and used as the initial prompt for local Whisper.
    pub hotwords: String,
}

#[derive(Default)]
pub struct AsrState(Mutex<HashMap<AsrProvider, AsrCredential>>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrConfigurationView {
    provider: AsrProvider,
    endpoint: String,
    model: String,
    language: String,
    configured: bool,
}

/// One committed chunk that contributes audio to a transcription.
#[derive(Debug, Clone)]
pub struct AudioChunkRef {
    pub track: Track,
    pub path: PathBuf,
    pub start_frame: u64,
    pub frames: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub provider: String,
    pub model: String,
    pub language: String,
    pub duration_ms: u64,
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    pub created_at_epoch_ms: u64,
}

impl AsrState {
    /// Seeds persisted endpoint/model preferences without a cached secret.
    pub fn seed(&self, provider: AsrProvider, endpoint: String, model: String, language: String) {
        self.0
            .lock()
            .expect("ASR state poisoned")
            .entry(provider)
            .or_insert(AsrCredential {
                provider,
                api_key: String::new(),
                endpoint,
                model,
                language,
                hotwords: String::new(),
            });
    }

    /// Stores the hotwords the user configured so every provider can use them.
    pub fn set_hotwords(&self, provider: AsrProvider, hotwords: String) {
        let mut state = self.0.lock().expect("ASR state poisoned");
        if let Some(credential) = state.get_mut(&provider) {
            credential.hotwords = hotwords;
        }
    }

    pub fn credential(&self, provider: AsrProvider) -> Result<AsrCredential, String> {
        if provider == AsrProvider::LocalWhisper {
            return Err("本地 Whisper 不需要 API Key".into());
        }
        let cached = self
            .0
            .lock()
            .expect("ASR state poisoned")
            .get(&provider)
            .cloned();
        let api_key = cached
            .as_ref()
            .map(|credential| credential.api_key.clone())
            .filter(|key| !key.is_empty())
            .or_else(|| stored_api_key(provider))
            .ok_or_else(|| {
                "尚未配置转写 API Key。MiniMax 会复用 AI 助手的密钥，也可以单独保存。".to_owned()
            })?;
        let defaults = AsrCredential {
            provider,
            api_key,
            endpoint: provider.default_endpoint().to_owned(),
            model: provider.default_model().to_owned(),
            language: "auto".to_owned(),
            hotwords: String::new(),
        };
        Ok(match cached {
            Some(cached) => AsrCredential {
                endpoint: cached.endpoint,
                model: cached.model,
                language: cached.language,
                hotwords: cached.hotwords,
                ..defaults
            },
            None => defaults,
        })
    }
}

fn credential_entry(provider: AsrProvider) -> Result<keyring::Entry, String> {
    keyring::Entry::new(ASR_SERVICE, provider.credential_name())
        .map_err(|error| format!("无法访问 Windows 凭据管理器：{error}"))
}

fn stored_api_key(provider: AsrProvider) -> Option<String> {
    let own = credential_entry(provider)
        .ok()?
        .get_password()
        .ok()
        .filter(|key| !key.is_empty());
    own.or_else(|| {
        // MiniMax chat and ASR use the same account, so the AI key can be
        // reused without asking the user for the secret twice.
        (provider == AsrProvider::MiniMax)
            .then(|| crate::ai::stored_api_key(crate::ai::AiProvider::MiniMax))
            .flatten()
    })
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[tauri::command]
pub fn configure_asr(
    provider: AsrProvider,
    api_key: String,
    endpoint: Option<String>,
    model: Option<String>,
    language: Option<String>,
    state: tauri::State<'_, AsrState>,
) -> Result<AsrConfigurationView, String> {
    let api_key = api_key.trim().to_owned();
    let previous = state
        .0
        .lock()
        .expect("ASR state poisoned")
        .get(&provider)
        .cloned();
    if api_key.is_empty() && stored_api_key(provider).is_none() {
        return Err("API Key 不能为空".into());
    }
    if !api_key.is_empty() {
        credential_entry(provider)?
            .set_password(&api_key)
            .map_err(|error| format!("无法保存 API Key：{error}"))?;
    }
    let endpoint = endpoint
        .filter(|value| !value.trim().is_empty())
        .or_else(|| previous.as_ref().map(|value| value.endpoint.clone()))
        .unwrap_or_else(|| provider.default_endpoint().to_owned());
    let model = model
        .filter(|value| !value.trim().is_empty())
        .or_else(|| previous.as_ref().map(|value| value.model.clone()))
        .unwrap_or_else(|| provider.default_model().to_owned());
    let language = language
        .filter(|value| !value.trim().is_empty())
        .or_else(|| previous.as_ref().map(|value| value.language.clone()))
        .unwrap_or_else(|| "auto".to_owned());
    state.0.lock().expect("ASR state poisoned").insert(
        provider,
        AsrCredential {
            provider,
            api_key: if api_key.is_empty() {
                stored_api_key(provider).unwrap_or_default()
            } else {
                api_key
            },
            endpoint: endpoint.clone(),
            model: model.clone(),
            language: language.clone(),
            hotwords: previous
                .as_ref()
                .map(|value| value.hotwords.clone())
                .unwrap_or_default(),
        },
    );
    Ok(AsrConfigurationView {
        provider,
        endpoint,
        model,
        language,
        configured: true,
    })
}

#[tauri::command]
pub fn get_asr_configurations(state: tauri::State<'_, AsrState>) -> Vec<AsrConfigurationView> {
    [
        AsrProvider::MiniMax,
        AsrProvider::SiliconFlow,
        AsrProvider::OpenAiRealtime,
    ]
    .into_iter()
    .map(|provider| {
        let cached = state
            .0
            .lock()
            .expect("ASR state poisoned")
            .get(&provider)
            .cloned();
        AsrConfigurationView {
            provider,
            endpoint: cached
                .as_ref()
                .map(|value| value.endpoint.clone())
                .unwrap_or_else(|| provider.default_endpoint().to_owned()),
            model: cached
                .as_ref()
                .map(|value| value.model.clone())
                .unwrap_or_else(|| provider.default_model().to_owned()),
            language: cached
                .as_ref()
                .map(|value| value.language.clone())
                .unwrap_or_else(|| "auto".to_owned()),
            configured: stored_api_key(provider).is_some(),
        }
    })
    .collect()
}

#[tauri::command]
pub fn clear_asr(provider: AsrProvider, state: tauri::State<'_, AsrState>) -> Result<(), String> {
    match credential_entry(provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(format!("无法清除 API Key：{error}")),
    }
    state
        .0
        .lock()
        .expect("ASR state poisoned")
        .remove(&provider);
    Ok(())
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    segments: Vec<RawSegment>,
}

#[derive(Deserialize)]
struct RawSegment {
    start: f64,
    end: f64,
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    error: Option<ApiErrorDetail>,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

pub(crate) struct AudioSegment {
    pub path: PathBuf,
    pub start_ms: u64,
    pub duration_ms: u64,
}

fn language_header(language: &str) -> Option<&str> {
    match language {
        "" | "auto" => None,
        "zh-CN" => Some("zh"),
        "en-US" => Some("en"),
        other => Some(other),
    }
}

/// Decodes, mixes and downsamples committed chunks into WAV segments small
/// enough for the transcription APIs.
pub(crate) fn build_segments(
    chunks: &[AudioChunkRef],
    directory: &Path,
    segment_seconds: u64,
) -> Result<Vec<AudioSegment>, String> {
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut slots: BTreeMap<u64, [Option<&AudioChunkRef>; 2]> = BTreeMap::new();
    for chunk in chunks {
        let slot = slots.entry(chunk.start_frame).or_default();
        match chunk.track {
            Track::Microphone => slot[0] = Some(chunk),
            Track::System => slot[1] = Some(chunk),
        }
    }
    let mut segments = Vec::new();
    let mut buffer: Vec<f32> = Vec::new();
    let mut segment_start_frame = 0_u64;
    let mut cursor_frame = 0_u64;
    let limit = (segment_seconds.max(1) * u64::from(ASR_SAMPLE_RATE)) as usize;

    let flush = |buffer: &mut Vec<f32>,
                 start_frame: u64,
                 segments: &mut Vec<AudioSegment>|
     -> Result<(), String> {
        if buffer.is_empty() {
            return Ok(());
        }
        let index = segments.len();
        let path = directory.join(format!("segment-{index:03}.wav"));
        write_wav(&path, buffer)?;
        let duration_ms = buffer.len() as u64 * 1_000 / u64::from(ASR_SAMPLE_RATE);
        segments.push(AudioSegment {
            path,
            start_ms: start_frame * 1_000 / u64::from(OPUS_SAMPLE_RATE),
            duration_ms,
        });
        buffer.clear();
        Ok(())
    };

    for (slot_start, slot) in slots {
        if slot_start > cursor_frame {
            let silence = ((slot_start - cursor_frame) / 3) as usize;
            buffer.resize(buffer.len() + silence, 0.0);
        }
        while buffer.len() >= limit {
            let remainder = buffer.split_off(limit);
            let mut chunk = std::mem::take(&mut buffer);
            chunk.truncate(limit);
            flush(&mut chunk, segment_start_frame, &mut segments)?;
            segment_start_frame +=
                (limit as u64 * u64::from(OPUS_SAMPLE_RATE)) / u64::from(ASR_SAMPLE_RATE);
            buffer = remainder;
        }

        let microphone = slot[0].map(decode_chunk).transpose()?;
        let system = slot[1].map(decode_chunk).transpose()?;
        let length = microphone
            .as_ref()
            .map(Vec::len)
            .unwrap_or(0)
            .max(system.as_ref().map(Vec::len).unwrap_or(0));
        if length == 0 {
            continue;
        }
        let mut mixed = Vec::with_capacity(length / 3 + 1);
        for index in 0..length {
            let left = microphone
                .as_ref()
                .map_or(0.0, |samples| samples.get(index).copied().unwrap_or(0.0));
            let right = system
                .as_ref()
                .map_or(0.0, |samples| samples.get(index).copied().unwrap_or(0.0));
            mixed.push((left + right).clamp(-1.0, 1.0));
        }
        buffer.extend(downsample(&mixed));
        cursor_frame = slot_start + length as u64;

        while buffer.len() >= limit {
            let remainder = buffer.split_off(limit);
            let mut chunk = std::mem::take(&mut buffer);
            flush(&mut chunk, segment_start_frame, &mut segments)?;
            segment_start_frame +=
                (limit as u64 * u64::from(OPUS_SAMPLE_RATE)) / u64::from(ASR_SAMPLE_RATE);
            buffer = remainder;
        }
    }
    flush(&mut buffer, segment_start_frame, &mut segments)?;
    Ok(segments)
}

fn decode_chunk(chunk: &AudioChunkRef) -> Result<Vec<f32>, String> {
    let mut samples = decode_ogg_opus(&chunk.path).map_err(|error| error.to_string())?;
    samples.truncate(chunk.frames as usize);
    Ok(samples)
}

fn downsample(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(3)
        .map(|group| group.iter().sum::<f32>() / group.len() as f32)
        .collect()
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&ASR_SAMPLE_RATE.to_le_bytes());
    header.extend_from_slice(&(ASR_SAMPLE_RATE * 2).to_le_bytes());
    header.extend_from_slice(&2_u16.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    file.write_all(&header).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    Ok(())
}

async fn upload_segment(
    client: &Client,
    config: &AsrCredential,
    segment: &AudioSegment,
) -> Result<RawResponse, String> {
    let bytes = fs::read(&segment.path).map_err(|error| format!("无法读取临时音频：{error}"))?;
    let part = Part::bytes(bytes)
        .file_name("segment.wav")
        .mime_str("audio/wav")
        .map_err(|error| error.to_string())?;
    let mut form = Form::new()
        .text("model", config.model.clone())
        .part("file", part);
    if config.provider == AsrProvider::MiniMax {
        form = form.text("response_format", "verbose_json");
    }
    // The prompt field tells the model which words to expect, which is what
    // the hotwords box in the settings is for.
    let hotwords = config.hotwords.trim().to_owned();
    if !hotwords.is_empty() {
        form = form.text("prompt", hotwords);
    }
    let mut request = client
        .post(&config.endpoint)
        .bearer_auth(&config.api_key)
        .multipart(form);
    if let Some(language) = language_header(&config.language) {
        request = request.header("language", language);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("ASR 请求失败：{error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("无法读取 ASR 响应：{error}"))?;
    if !status.is_success() {
        let message = serde_json::from_str::<ApiErrorBody>(&body)
            .ok()
            .and_then(|body| body.error)
            .map(|error| error.message)
            .unwrap_or_else(|| body.chars().take(300).collect());
        return Err(format!("ASR 服务返回 {status}：{message}"));
    }
    serde_json::from_str::<RawResponse>(&body)
        .map_err(|error| format!("无法解析 ASR 响应：{error}"))
}

impl AsrCredential {
    fn provider_name(&self) -> &'static str {
        self.provider.as_str()
    }
}

/// Transcribes a session while reporting `(done, total)` segment progress and
/// observing a cancellation flag between uploads.
pub async fn transcribe_chunks_with(
    chunks: &[AudioChunkRef],
    config: &AsrCredential,
    session_id: &str,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Transcript, String> {
    transcribe_refs_with(
        chunks,
        config,
        session_id,
        WAV_SEGMENT_SECONDS,
        BATCH_REQUEST_TIMEOUT,
        cancel,
        on_progress,
    )
    .await
}

/// Transcribes the newest chunk pair for live subtitles.
///
/// `previous` holds the chunk that precedes the new one on the timeline. It is
/// sent along with the new chunk so the model hears the sentence it is in the
/// middle of instead of treating the chunk edge as a sentence boundary.
/// `previous_text` is what the previous window already reported, and is used
/// to drop the repeated part of the new result.
pub async fn transcribe_pair(
    microphone: Option<AudioChunkRef>,
    system: Option<AudioChunkRef>,
    previous_microphone: Option<AudioChunkRef>,
    previous_system: Option<AudioChunkRef>,
    previous_text: &str,
    config: &AsrCredential,
    session_id: &str,
) -> Result<Transcript, String> {
    let mut chunks: Vec<AudioChunkRef> = previous_microphone
        .into_iter()
        .chain(previous_system)
        .collect();
    let overlap = chunks.iter().map(|chunk| chunk.frames).max().unwrap_or(0);
    chunks.extend(microphone.into_iter().chain(system));
    let mut transcript =
        transcribe_refs(&chunks, config, session_id, 60, LIVE_REQUEST_TIMEOUT).await?;
    if overlap > 0 {
        trim_repeated_overlap(&mut transcript, overlap, previous_text);
    }
    Ok(transcript)
}

/// Removes the part of a rolling-window result that the previous window
/// already reported.
///
/// The window starts `overlap_frames` before the new audio, so the model
/// transcribes that part again. The API returns one long segment for such a
/// window rather than one segment per chunk, so the repeated part is found by
/// text: the new text starts with what the previous window reported for the
/// same audio.
///
/// A segment that sits entirely inside the overlap is dropped. A segment that
/// straddles the boundary keeps only its tail, which is the part that belongs
/// to the new audio.
fn trim_repeated_overlap(transcript: &mut Transcript, overlap_frames: u64, previous_text: &str) {
    let overlap_ms = overlap_frames * 1_000 / u64::from(OPUS_SAMPLE_RATE);
    let previous = previous_text.trim();
    if previous.is_empty() {
        return;
    }
    for segment in transcript.segments.iter_mut() {
        if segment.end_ms <= overlap_ms {
            // Fully inside the part that was already reported.
            segment.text.clear();
            continue;
        }
        if segment.start_ms < overlap_ms {
            // Straddles the boundary: keep only the new part of the text.
            segment.text = trim_leading_text(&segment.text, previous);
            segment.start_ms = overlap_ms;
        }
    }
    transcript
        .segments
        .retain(|segment| !segment.text.trim().is_empty());
}

/// Drops the leading part of `text` that repeats what was already reported.
///
/// The new text begins with the audio of the previous window, so it repeats
/// the tail of `previous`. The longest matching run is removed.
fn trim_leading_text(text: &str, previous: &str) -> String {
    let trimmed = text.trim_start();
    let previous_chars: Vec<char> = previous.chars().collect();
    let text_chars: Vec<char> = trimmed.chars().collect();
    let limit = previous_chars.len().min(text_chars.len());
    for length in (4..=limit).rev() {
        let head = &text_chars[..length];
        let tail = &previous_chars[previous_chars.len() - length..];
        if head == tail {
            return text_chars[length..].iter().collect::<String>();
        }
    }
    trimmed.to_owned()
}

async fn transcribe_refs(
    chunks: &[AudioChunkRef],
    config: &AsrCredential,
    session_id: &str,
    segment_seconds: u64,
    request_timeout: Duration,
) -> Result<Transcript, String> {
    transcribe_refs_with(
        chunks,
        config,
        session_id,
        segment_seconds,
        request_timeout,
        &NEVER_CANCELLED,
        &|_, _| {},
    )
    .await
}

async fn transcribe_refs_with(
    chunks: &[AudioChunkRef],
    config: &AsrCredential,
    session_id: &str,
    segment_seconds: u64,
    request_timeout: Duration,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Transcript, String> {
    let working =
        std::env::temp_dir().join(format!("magnetic-tape-asr-{session_id}-{}", epoch_ms()));
    let segments = build_segments(chunks, &working, segment_seconds)?;
    if segments.is_empty() {
        let _ = fs::remove_dir_all(&working);
        return Err("当前会话没有可转写的音频".into());
    }
    let client = Client::builder()
        .timeout(request_timeout)
        .build()
        .map_err(|error| error.to_string())?;
    let mut transcript = Transcript {
        provider: config.provider_name().into(),
        model: config.model.clone(),
        language: config.language.clone(),
        duration_ms: 0,
        text: String::new(),
        segments: Vec::new(),
        created_at_epoch_ms: epoch_ms(),
    };
    let mut failures = Vec::new();
    let total = segments.len();
    for (index, segment) in segments.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            let _ = fs::remove_dir_all(&working);
            return Err(CANCELLED.into());
        }
        match upload_segment(&client, config, segment).await {
            Ok(response) => {
                transcript.duration_ms = transcript
                    .duration_ms
                    .max(segment.start_ms + segment.duration_ms);
                if !transcript.text.is_empty() && !response.text.is_empty() {
                    transcript.text.push('\n');
                }
                transcript.text.push_str(response.text.trim());
                if response.segments.is_empty() && !response.text.trim().is_empty() {
                    let duration = if response.duration > 0.0 {
                        (response.duration * 1_000.0) as u64
                    } else {
                        segment.duration_ms
                    };
                    transcript.segments.push(TranscriptSegment {
                        start_ms: segment.start_ms,
                        end_ms: segment.start_ms + duration,
                        speaker: None,
                        text: response.text.trim().to_owned(),
                    });
                }
                for raw in response.segments {
                    transcript.segments.push(TranscriptSegment {
                        start_ms: segment.start_ms + (raw.start * 1_000.0) as u64,
                        end_ms: segment.start_ms + (raw.end * 1_000.0) as u64,
                        speaker: raw.speaker,
                        text: raw.text,
                    });
                }
            }
            Err(error) => failures.push(error),
        }
        on_progress(index + 1, total);
    }
    let _ = fs::remove_dir_all(&working);
    if transcript.text.is_empty() && !failures.is_empty() {
        return Err(failures.join("；"));
    }
    Ok(transcript)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magnetic_tape_audio::OpusTrackWriter;
    use std::{
        io::{Read, Write as IoWrite},
        net::{TcpListener, TcpStream},
        sync::Arc,
        thread,
    };

    #[test]
    fn downsamples_48k_to_16k_by_averaging() {
        let input = vec![1.0_f32; 48_000];
        assert_eq!(downsample(&input).len(), 16_000);
    }

    #[test]
    fn language_headers_drop_the_region() {
        assert_eq!(language_header("auto"), None);
        assert_eq!(language_header("zh-CN"), Some("zh"));
        assert_eq!(language_header("en-US"), Some("en"));
    }

    /// A rolling window repeats the previous chunk. The repeated part must be
    /// dropped, otherwise the caption shows the same sentence twice.
    #[test]
    fn drops_the_text_that_the_rolling_window_repeats() {
        let mut transcript = Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![TranscriptSegment {
                start_ms: 0,
                end_ms: 10_000,
                speaker: None,
                text: "研究或者量化这个财，那么因为他这个杜邦分析法的方法呢，它的方法就是把企业的净资产收益率".into(),
            }],
            created_at_epoch_ms: 0,
        };
        // The previous window reported the first half of the same audio.
        let previous = "研究或者量化这个财，那么因为他这个杜邦分析法";
        trim_repeated_overlap(&mut transcript, 5_040, previous);
        assert_eq!(transcript.segments.len(), 1);
        let text = &transcript.segments[0].text;
        assert!(
            text.starts_with("的方法呢"),
            "the repeated prefix should be gone, got {text}"
        );
        assert!(text.contains("净资产收益率"));
    }

    /// A segment that lies entirely inside the already reported overlap must
    /// disappear instead of being shown again.
    #[test]
    fn removes_segments_that_only_cover_the_overlap() {
        let mut transcript = Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![
                TranscriptSegment {
                    start_ms: 0,
                    end_ms: 4_000,
                    speaker: None,
                    text: "已经说过的话".into(),
                },
                TranscriptSegment {
                    start_ms: 5_000,
                    end_ms: 9_000,
                    speaker: None,
                    text: "新的内容".into(),
                },
            ],
            created_at_epoch_ms: 0,
        };
        trim_repeated_overlap(&mut transcript, 5_040, "已经说过的话");
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].text, "新的内容");
    }

    /// Text that does not repeat anything must survive untouched.
    #[test]
    fn keeps_text_that_repeats_nothing() {
        let mut transcript = Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![TranscriptSegment {
                start_ms: 0,
                end_ms: 9_000,
                speaker: None,
                text: "完全不同的另一句话".into(),
            }],
            created_at_epoch_ms: 0,
        };
        trim_repeated_overlap(&mut transcript, 5_040, "毫不相干的上一句");
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].text, "完全不同的另一句话");
    }

    /// Without a previous report there is nothing to compare against, so the
    /// result must be kept as it is.
    #[test]
    fn keeps_everything_when_there_is_no_previous_text() {
        let mut transcript = Transcript {
            provider: "minimax".into(),
            model: "asr-1.0".into(),
            language: "zh".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![TranscriptSegment {
                start_ms: 0,
                end_ms: 9_000,
                speaker: None,
                text: "第一句话".into(),
            }],
            created_at_epoch_ms: 0,
        };
        trim_repeated_overlap(&mut transcript, 5_040, "");
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].text, "第一句话");
    }

    #[test]
    fn writes_a_valid_16k_mono_wav() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("segment.wav");
        write_wav(&path, &[0.0, 0.5, -0.5]).unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(bytes.len(), 44 + 6);
    }

    #[test]
    fn mixes_both_tracks_and_splits_wav_segments() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let mut microphone = OpusTrackWriter::new(
            directory.path(),
            Track::Microphone,
            48_000,
            1,
            Arc::clone(&chunks),
        )
        .unwrap();
        let mut system = OpusTrackWriter::new(
            directory.path(),
            Track::System,
            48_000,
            1,
            Arc::clone(&chunks),
        )
        .unwrap();
        let tone: Vec<f32> = (0..96_000)
            .map(|index| (index as f32 * 0.06).sin() * 0.25)
            .collect();
        microphone.push_interleaved(&tone).unwrap();
        system.push_interleaved(&tone).unwrap();
        microphone.finish().unwrap();
        system.finish().unwrap();

        let refs: Vec<AudioChunkRef> = chunks
            .lock()
            .unwrap()
            .iter()
            .map(|chunk| AudioChunkRef {
                track: chunk.track,
                path: chunk.path.clone(),
                start_frame: chunk.start_frame,
                frames: chunk.frames,
            })
            .collect();
        let working = directory.path().join("segments");
        let segments = build_segments(&refs, &working, 1).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 0);
        assert_eq!(segments[1].start_ms, 1_000);
        for segment in &segments {
            let bytes = fs::read(&segment.path).unwrap();
            assert_eq!(bytes.len(), 44 + 16_000 * 2);
        }
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut data = Vec::new();
        let mut buffer = [0_u8; 8_192];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
            let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&data[..header_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if data.len() >= header_end + 4 + length {
                break;
            }
        }
        data
    }

    #[test]
    fn uploads_a_segment_and_merges_segments_with_global_timestamps() {
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
        let tone: Vec<f32> = (0..96_000)
            .map(|index| (index as f32 * 0.06).sin() * 0.25)
            .collect();
        writer.push_interleaved(&tone).unwrap();
        writer.finish().unwrap();
        let refs: Vec<AudioChunkRef> = chunks
            .lock()
            .unwrap()
            .iter()
            .map(|chunk| AudioChunkRef {
                track: chunk.track,
                path: chunk.path.clone(),
                start_frame: chunk.start_frame,
                frames: chunk.frames,
            })
            .collect();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let body = r#"{"text":"你好，世界。","duration":2.0,"segments":[{"start":0.0,"end":1.0,"speaker":"S1","text":"你好，"},{"start":1.0,"end":2.0,"speaker":"S2","text":"世界。"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            request
        });

        let config = AsrCredential {
            provider: AsrProvider::MiniMax,
            api_key: "test-key".into(),
            endpoint: format!("http://{address}/v1/speech_to_text"),
            model: "asr-1.0".into(),
            language: "zh-CN".into(),
            hotwords: String::new(),
        };
        let cancel = AtomicBool::new(false);
        let transcript = tauri::async_runtime::block_on(transcribe_chunks_with(
            &refs,
            &config,
            "session",
            &cancel,
            &|_, _| {},
        ))
        .unwrap();
        let request = String::from_utf8_lossy(&server.join().unwrap()).into_owned();

        assert!(request.contains(r#"name="model""#));
        assert!(request.contains("asr-1.0"));
        assert!(request.contains("verbose_json"));
        assert!(request.contains(r#"name="file""#));
        assert!(request.contains(r#"filename="segment.wav""#));
        assert!(request.to_ascii_lowercase().contains("\nlanguage: zh"));

        assert_eq!(transcript.provider, "minimax");
        assert_eq!(transcript.text, "你好，世界。");
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].start_ms, 0);
        assert_eq!(transcript.segments[1].start_ms, 1_000);
        assert_eq!(transcript.segments[1].speaker.as_deref(), Some("S2"));
        assert_eq!(transcript.duration_ms, 2_000);
    }
}
