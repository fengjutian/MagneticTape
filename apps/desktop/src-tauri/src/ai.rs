use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
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
    generate(provider, prompt.trim(), &state).await
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
}
