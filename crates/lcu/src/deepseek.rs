use anyhow::{Context, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub thinking_enabled: bool,
    pub reasoning_effort: String,
    pub stream_enabled: bool,
}

impl DeepSeekConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .context("DEEPSEEK_API_KEY is not set")?;
        let base_url = std::env::var("DEEPSEEK_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let model = std::env::var("DEEPSEEK_MODEL")
            .unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let thinking_enabled = std::env::var("DEEPSEEK_THINKING")
            .ok()
            .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
            .unwrap_or(false);
        let reasoning_effort = std::env::var("DEEPSEEK_REASONING_EFFORT")
            .unwrap_or_else(|_| "high".to_string());
        let stream_enabled = std::env::var("DEEPSEEK_STREAM")
            .ok()
            .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
            .unwrap_or(false);

        Ok(Self {
            api_key,
            base_url,
            model,
            thinking_enabled,
            reasoning_effort,
            stream_enabled,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeepSeekClient {
    client: reqwest::Client,
    config: DeepSeekConfig,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    type_field: &'static str,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
    error: Option<DeepSeekApiError>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct DeepSeekApiError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

impl DeepSeekClient {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(DeepSeekConfig::from_env()?))
    }

    pub fn new(config: DeepSeekConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    pub async fn chat(&self, system_prompt: &str, user_prompt: &str) -> anyhow::Result<String> {
        self.chat_messages(vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(user_prompt),
        ])
        .await
    }

    pub async fn chat_messages(
        &self,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<String> {
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: 0.7,
            max_tokens: 1024,
            stream: self.config.stream_enabled,
            thinking: self
                .config
                .thinking_enabled
                .then_some(Thinking { type_field: "enabled" }),
            reasoning_effort: Some(self.config.reasoning_effort.clone()),
        };

        if self.config.stream_enabled {
            let response = self
                .client
                .post(&endpoint)
                .bearer_auth(&self.config.api_key)
                .json(&request)
                .send()
                .await
                .with_context(|| format!("failed to send DeepSeek request to {endpoint}"))?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                bail!("DeepSeek request failed with status {status}: {text}");
            }

            let mut stream = response.bytes_stream();
            let mut content = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("failed to read DeepSeek stream chunk")?;
                let text = String::from_utf8_lossy(&chunk);
                for line in text.lines() {
                    let line = line.trim();
                    if !line.starts_with("data: ") {
                        continue;
                    }
                    let data = line.trim_start_matches("data: ").trim();
                    if data == "[DONE]" {
                        return if content.is_empty() {
                            bail!("DeepSeek returned an empty stream response")
                        } else {
                            Ok(content)
                        };
                    }
                    if let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) {
                        if let Some(delta) = parsed
                            .choices
                            .into_iter()
                            .next()
                            .and_then(|choice| choice.delta.content)
                        {
                            content.push_str(&delta);
                        }
                    }
                }
            }

            if content.is_empty() {
                bail!("DeepSeek returned an empty stream response")
            }
            return Ok(content);
        }

        let response = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("failed to send DeepSeek request to {endpoint}"))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read DeepSeek response body")?;

        if !status.is_success() {
            bail!("DeepSeek request failed with status {status}: {text}");
        }

        let parsed: ChatCompletionResponse = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse DeepSeek response: {text}"))?;

        if let Some(error) = parsed.error {
            bail!("DeepSeek API error: {}", error.message);
        }

        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .filter(|content| !content.is_empty())
            .context("DeepSeek returned an empty response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_constructors_work() {
        let system = ChatMessage::system("system");
        let user = ChatMessage::user("user");

        assert_eq!(system.role, "system");
        assert_eq!(system.content, "system");
        assert_eq!(user.role, "user");
        assert_eq!(user.content, "user");
    }
}
