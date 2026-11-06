use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Returned when the user cancels a long-running AI job.
pub(crate) const CANCELLED: &str = "已取消";

/// Character budget for one AI request. Long meetings are split on line
/// boundaries so a single request never overflows the model context window.
pub(crate) const LONG_INPUT_CHUNK_CHARS: usize = 6_000;
/// Upper bound for the final merge pass; summaries are regrouped until they
/// fit this budget.
const FINAL_INPUT_CHUNK_CHARS: usize = 16_000;
/// Output budget per response. 1 024 tokens truncated long meeting minutes.
const MAX_OUTPUT_TOKENS: u32 = 2_048;
/// Long inputs need more than the original 60 seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    #[serde(rename = "minimax")]
    MiniMax,
    DeepSeek,
}

#[derive(Clone)]
struct AiCredential {
    api_key: String,
    model: String,
}

#[derive(Default)]
pub struct AiState(Mutex<HashMap<AiProvider, AiCredential>>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigurationView {
    provider: AiProvider,
    model: String,
    configured: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiResponse {
    provider: AiProvider,
    model: String,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

impl AiProvider {
    fn credential_name(self) -> &'static str {
        match self {
            Self::MiniMax => "minimax",
            Self::DeepSeek => "deepseek",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::MiniMax => "MiniMax-M3.1-Flash",
            Self::DeepSeek => "deepseek-flash",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::MiniMax => "https://api.minimax.cn/v1/chat/completions",
            Self::DeepSeek => "https://api.deepseek.com/chat/completions",
        }
    }
}

fn credential_entry(provider: AiProvider) -> Result<keyring::Entry, String> {
    keyring::Entry::new("com.magnetictape.desktop.ai", provider.credential_name())
        .map_err(|error| format!("无法访问 Windows 凭据管理器：{error}"))
}

/// Reads a previously saved provider key. Used by the ASR module so a MiniMax
/// user does not have to store the same secret twice.
pub(crate) fn stored_api_key(provider: AiProvider) -> Option<String> {
    credential_entry(provider)
        .ok()?
        .get_password()
        .ok()
        .filter(|key| !key.is_empty())
}

#[tauri::command]
pub fn configure_ai(
    provider: AiProvider,
    api_key: String,
    model: Option<String>,
    state: tauri::State<'_, AiState>,
) -> Result<AiConfigurationView, String> {
    let api_key = api_key.trim().to_owned();
    if api_key.is_empty() {
        return Err("API Key 不能为空".into());
    }
    let model = model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.default_model().to_owned());
    credential_entry(provider)?
        .set_password(&api_key)
        .map_err(|error| format!("无法保存 API Key：{error}"))?;
    state.0.lock().expect("AI state poisoned").insert(
        provider,
        AiCredential {
            api_key,
            model: model.clone(),
        },
    );
    Ok(AiConfigurationView {
        provider,
        model,
        configured: true,
    })
}

#[tauri::command]
pub fn get_ai_configurations(state: tauri::State<'_, AiState>) -> Vec<AiConfigurationView> {
    [AiProvider::MiniMax, AiProvider::DeepSeek]
        .into_iter()
        .map(|provider| {
            let model = state
                .0
                .lock()
                .expect("AI state poisoned")
                .get(&provider)
                .map_or_else(
                    || provider.default_model().to_owned(),
                    |value| value.model.clone(),
                );
            let configured = credential_entry(provider)
                .and_then(|entry| entry.get_password().map_err(|error| error.to_string()))
                .is_ok_and(|secret| !secret.is_empty());
            AiConfigurationView {
                provider,
                model,
                configured,
            }
        })
        .collect()
}

#[tauri::command]
pub fn clear_ai(provider: AiProvider, state: tauri::State<'_, AiState>) -> Result<(), String> {
    let entry = credential_entry(provider)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(format!("无法清除 API Key：{error}")),
    }
    state.0.lock().expect("AI state poisoned").remove(&provider);
    Ok(())
}

#[tauri::command]
pub async fn test_ai(
    provider: AiProvider,
    state: tauri::State<'_, AiState>,
) -> Result<AiResponse, String> {
    generate(provider, "请只回复：连接成功", &state).await
}

#[tauri::command]
pub async fn generate_ai(
    provider: AiProvider,
    prompt: String,
    state: tauri::State<'_, AiState>,
) -> Result<AiResponse, String> {
    if prompt.trim().is_empty() {
        return Err("提示词不能为空".into());
    }
    if prompt.len() > 200_000 {
        return Err("发送给 AI 的文本不能超过 200 KB".into());
    }
    generate(provider, prompt.trim(), &state).await
}

#[tauri::command]
pub async fn translate_live_segments(
    segments: Vec<String>,
    target_language: String,
    provider: AiProvider,
    state: tauri::State<'_, AiState>,
) -> Result<Vec<String>, String> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }
    if segments.len() > 32 {
        return Err("单次实时翻译不能超过 32 段".into());
    }
    let segments = segments
        .into_iter()
        .map(|segment| segment.trim().chars().take(4_000).collect::<String>())
        .collect::<Vec<_>>();
    let source = serde_json::to_string(&segments).map_err(|error| error.to_string())?;
    if source.len() > 64_000 {
        return Err("单次实时翻译文本不能超过 64 KB".into());
    }
    let target_language = target_language.trim().chars().take(32).collect::<String>();
    if target_language.is_empty() {
        return Err("翻译目标语言不能为空".into());
    }
    let prompt = format!(
        "把下面 JSON 数组中的每一项翻译为{target_language}。保留原意、说话语气和专有名词，不要解释。只返回长度完全相同的 JSON 字符串数组，不要使用 Markdown 代码块。\n\n{source}"
    );
    let content = generate_text(provider, &prompt, &state).await?;
    parse_translation_array(&content, segments.len())
}

fn parse_translation_array(content: &str, expected: usize) -> Result<Vec<String>, String> {
    let trimmed = content.trim();
    let candidate = if trimmed.starts_with("```") {
        trimmed
            .lines()
            .skip(1)
            .take_while(|line| !line.trim_start().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        trimmed.to_owned()
    };
    // Some models still add a short introduction despite the prompt. Restrict
    // parsing to the outer JSON array while keeping brackets inside strings.
    let json = extract_json_array(&candidate).unwrap_or(candidate.trim());
    let translations: Vec<String> =
        serde_json::from_str(json).map_err(|error| format!("无法解析实时翻译响应：{error}"))?;
    if translations.len() != expected {
        return Err(format!(
            "实时翻译返回了 {} 段，预期 {expected} 段",
            translations.len()
        ));
    }
    Ok(translations)
}

fn extract_json_array(content: &str) -> Option<&str> {
    let bytes = content.as_bytes();
    let start = bytes.iter().position(|byte| *byte == b'[')?;
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0_u32;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return content.get(start..=start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Generates plain text with a configured provider. Shared with transcript
/// translation so the ASR module does not duplicate the HTTP client.
pub(crate) async fn generate_text(
    provider: AiProvider,
    prompt: &str,
    state: &tauri::State<'_, AiState>,
) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err("提示词不能为空".into());
    }
    generate(provider, prompt.trim(), state)
        .await
        .map(|response| response.content)
}

async fn generate(
    provider: AiProvider,
    prompt: &str,
    state: &tauri::State<'_, AiState>,
) -> Result<AiResponse, String> {
    let cached = state
        .0
        .lock()
        .expect("AI state poisoned")
        .get(&provider)
        .cloned();
    let credential = match cached {
        Some(credential) => credential,
        None => AiCredential {
            api_key: credential_entry(provider)?
                .get_password()
                .map_err(|_| "请先配置该 AI Provider 的 API Key".to_owned())?,
            model: provider.default_model().to_owned(),
        },
    };
    let request = ChatRequest {
        model: &credential.model,
        messages: [
            ChatMessage {
                role: "system",
                content: "你是 MagneticTape 的会议助手，回答应准确、简洁，不编造信息。",
            },
            ChatMessage {
                role: "user",
                content: prompt,
            },
        ],
        max_tokens: (provider == AiProvider::DeepSeek).then_some(MAX_OUTPUT_TOKENS),
        max_completion_tokens: (provider == AiProvider::MiniMax).then_some(MAX_OUTPUT_TOKENS),
    };
    let response = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
        .post(provider.endpoint())
        .bearer_auth(&credential.api_key)
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("AI 请求失败：{error}"))?;
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        let message: String = message.chars().take(500).collect();
        return Err(format!("AI 服务返回 {status}：{message}"));
    }
    let response: ChatResponse = response
        .json()
        .await
        .map_err(|error| format!("无法解析 AI 响应：{error}"))?;
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| "AI 服务没有返回文本内容".to_owned())?;
    Ok(AiResponse {
        provider,
        model: credential.model,
        content,
    })
}

fn flush_chunk(chunks: &mut Vec<String>, current: &mut String) {
    let chunk = current.trim_end();
    if !chunk.trim().is_empty() {
        chunks.push(chunk.to_owned());
    }
    current.clear();
}

/// Hard-wraps one oversized line on character boundaries.
fn split_line(line: &str, limit: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (index, _) in line.char_indices() {
        if count == limit {
            pieces.push(line[start..index].to_owned());
            start = index;
            count = 0;
        }
        count += 1;
    }
    if start < line.len() {
        pieces.push(line[start..].to_owned());
    }
    pieces
}

/// Splits long text into request-sized chunks. Splits happen on line
/// boundaries where possible so timestamps and speaker labels stay intact;
/// content is never dropped or reordered.
pub(crate) fn text_chunks(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0_usize;
    for line in text.lines() {
        let line_chars = line.chars().count();
        if line_chars > limit {
            flush_chunk(&mut chunks, &mut current);
            current_chars = 0;
            chunks.extend(split_line(line, limit));
            continue;
        }
        let separator = usize::from(!current.is_empty());
        if !current.is_empty() && current_chars + separator + line_chars > limit {
            flush_chunk(&mut chunks, &mut current);
            current_chars = 0;
        }
        if !current.is_empty() {
            current.push('\n');
            current_chars += 1;
        }
        current.push_str(line);
        current_chars += line_chars;
    }
    flush_chunk(&mut chunks, &mut current);
    chunks
}

/// Groups summary fragments into merge-sized blocks. Oversized fragments are
/// hard-wrapped so the reduce pass always stays inside the request budget.
fn group_texts(parts: &[String], limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut groups = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0_usize;
    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let chars = trimmed.chars().count();
        if chars > limit {
            if !current.trim().is_empty() {
                groups.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            groups.extend(text_chunks(trimmed, limit));
            continue;
        }
        let separator = if current.is_empty() { 0 } else { 2 };
        if !current.is_empty() && current_chars + separator + chars > limit {
            groups.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
            current_chars += 2;
        }
        current.push_str(trimmed);
        current_chars += chars;
    }
    if !current.trim().is_empty() {
        groups.push(current);
    }
    groups
}

/// Translates a whole transcript in request-sized chunks. Segment timestamps
/// and speaker labels are preserved because chunks split on line boundaries.
pub(crate) async fn translate_long_text(
    provider: AiProvider,
    target_language: &str,
    text: &str,
    state: &tauri::State<'_, AiState>,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<String, String> {
    let target_language = target_language.trim();
    if target_language.is_empty() {
        return Err("翻译目标语言不能为空".into());
    }
    let chunks = text_chunks(text, LONG_INPUT_CHUNK_CHARS);
    if chunks.is_empty() {
        return Err("没有可翻译的转写文本".into());
    }
    let total = chunks.len();
    let mut translated = Vec::with_capacity(total);
    for (index, chunk) in chunks.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let prompt = if total == 1 {
            format!(
                "请将以下会议转写翻译为{target_language}。保留时间戳、说话人标记和段落结构，只输出译文，不要解释，不要编造原文没有的信息。\n\n{chunk}"
            )
        } else {
            format!(
                "这是会议转写的第 {} / {total} 部分。请只把这一部分翻译为{target_language}，保留时间戳、说话人标记和段落结构；不要输出解释、标题或“第几部分”之类的前缀。\n\n{chunk}",
                index + 1
            )
        };
        translated.push(generate_text(provider, &prompt, state).await?);
        on_progress(index + 1, total);
    }
    Ok(translated.join("\n\n"))
}

fn minutes_prompt(source: &str) -> String {
    format!(
        "请把以下会议内容整理为 Markdown 会议纪要。必须包含：会议主题、核心结论、讨论要点、待办事项（负责人和截止时间未知时明确写“待确认”）、风险与未决问题。不要编造原文没有的信息，不要写开场白。\n\n{source}"
    )
}

/// Produces meeting minutes with a map-reduce pass so hour-long transcripts
/// do not overflow one request or get cut off by the output budget.
pub(crate) async fn summarize_text(
    provider: AiProvider,
    text: &str,
    state: &tauri::State<'_, AiState>,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<String, String> {
    let chunks = text_chunks(text, LONG_INPUT_CHUNK_CHARS);
    if chunks.is_empty() {
        return Err("没有可整理的文本".into());
    }
    let total = chunks.len();
    if total == 1 {
        let summary = generate_text(provider, &minutes_prompt(&chunks[0]), state).await?;
        on_progress(1, 1);
        return Ok(summary);
    }

    let mut summaries = Vec::with_capacity(total);
    for (index, chunk) in chunks.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let prompt = format!(
            "以下是会议转写的第 {} / {total} 段。请只提取这一段的：核心结论、讨论要点、待办事项（负责人或截止时间未知时写“待确认”）、风险与未决问题。用简洁的 Markdown 条目输出，不要编造原文没有的信息，不要写开场白。\n\n{chunk}",
            index + 1
        );
        summaries.push(generate_text(provider, &prompt, state).await?);
        on_progress(index + 1, total);
    }

    let mut rounds = 0;
    while summaries.join("\n\n").chars().count() > FINAL_INPUT_CHUNK_CHARS && rounds < 4 {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let groups = group_texts(&summaries, FINAL_INPUT_CHUNK_CHARS);
        if groups.len() <= 1 {
            break;
        }
        let mut merged = Vec::with_capacity(groups.len());
        for (index, group) in groups.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Err(CANCELLED.into());
            }
            let prompt = format!(
                "以下是会议纪要的分段摘要（第 {} / {} 组）。请合并为一组更凝练的要点，保留结论、待办、风险和未决问题，不要重复，不要编造。\n\n{group}",
                index + 1,
                groups.len()
            );
            merged.push(generate_text(provider, &prompt, state).await?);
        }
        summaries = merged;
        rounds += 1;
    }

    let joined = summaries.join("\n\n");
    let source = if joined.chars().count() > FINAL_INPUT_CHUNK_CHARS {
        joined.chars().take(FINAL_INPUT_CHUNK_CHARS).collect()
    } else {
        joined
    };
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    let summary = generate_text(provider, &minutes_prompt(&source), state).await?;
    on_progress(total, total);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_have_current_defaults() {
        assert_eq!(AiProvider::MiniMax.default_model(), "MiniMax-M3.1-Flash");
        assert_eq!(AiProvider::DeepSeek.default_model(), "deepseek-flash");
        assert!(AiProvider::DeepSeek.endpoint().starts_with("https://"));
    }

    #[test]
    fn provider_wire_names_match_the_frontend_contract() {
        assert_eq!(
            serde_json::from_str::<AiProvider>("\"minimax\"").unwrap(),
            AiProvider::MiniMax
        );
        assert_eq!(
            serde_json::to_string(&AiProvider::MiniMax).unwrap(),
            "\"minimax\""
        );
        assert_eq!(
            serde_json::from_str::<AiProvider>("\"deep_seek\"").unwrap(),
            AiProvider::DeepSeek
        );
    }

    #[test]
    fn parses_live_translation_arrays_and_code_fences() {
        assert_eq!(
            parse_translation_array("[\"Hello\",\"world\"]", 2).unwrap(),
            ["Hello", "world"]
        );
        assert_eq!(
            parse_translation_array("```json\n[\"你好\"]\n```", 1).unwrap(),
            ["你好"]
        );
        assert_eq!(
            parse_translation_array("翻译如下：\n[\"数组 [A]\", \"完成\"]\n谢谢", 2).unwrap(),
            ["数组 [A]", "完成"]
        );
        assert!(parse_translation_array("[\"one\"]", 2).is_err());
    }

    #[test]
    fn splits_long_text_on_line_boundaries_without_losing_content() {
        let text = (0..40)
            .map(|index| format!("**[00:{index:02}]** 第 {index} 行内容"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = text_chunks(&text, 120);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 120));
        assert_eq!(chunks.join("\n"), text);
    }

    #[test]
    fn hard_wraps_lines_that_exceed_the_budget() {
        let chunks = text_chunks(&"甲".repeat(25), 10);
        assert_eq!(chunks, ["甲".repeat(10), "甲".repeat(10), "甲".repeat(5)]);
    }

    #[test]
    fn blank_text_produces_no_chunks() {
        assert!(text_chunks("  \n\n\t\n", 100).is_empty());
    }

    #[test]
    fn groups_summary_fragments_within_the_merge_budget() {
        let parts = vec!["甲".repeat(8), "乙".repeat(8), "丙".repeat(8)];
        let groups = group_texts(&parts, 20);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|group| group.chars().count() <= 20));
        assert!(groups[0].contains(&"甲".repeat(8)));
        assert!(groups[1].contains(&"丙".repeat(8)));

        let oversized = group_texts(&["丁".repeat(25)], 10);
        assert_eq!(oversized.len(), 3);
        assert!(oversized.iter().all(|group| group.chars().count() <= 10));
    }
}
