use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex, time::Duration};

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
        max_tokens: (provider == AiProvider::DeepSeek).then_some(1_024),
        max_completion_tokens: (provider == AiProvider::MiniMax).then_some(1_024),
    };
    let response = Client::builder()
        .timeout(Duration::from_secs(60))
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
}
