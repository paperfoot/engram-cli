//! OpenRouter chat client. OpenAI-compatible, routes to 350+ frontier models.
//!
//! Default model is `openai/gpt-4o-mini` — fast and cheap for judging.
//! For better answer generation use `anthropic/claude-sonnet-4.5`
//! or `openai/gpt-5` via the `with_model` builder.

use crate::{ChatLlm, ChatMessage, ChatResponse, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// Default to the latest GPT flagship on OpenRouter. User explicitly wants
// frontier models for QA evaluation.
const DEFAULT_MODEL: &str = "openai/gpt-5.4";
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouterClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    temperature: f32,
    max_tokens: Option<u32>,
}

impl OpenRouterClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            temperature: 0.0,
            max_tokens: Some(2048),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = t;
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatRequestMessage<'a>>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct ChatRequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponseBody {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    /// Optional so we tolerate `content: null` that some models return when
    /// the response was filtered / refused / empty.
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

#[async_trait]
impl ChatLlm for OpenRouterClient {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatResponse, LlmError> {
        let req_msgs: Vec<ChatRequestMessage> = messages
            .iter()
            .map(|m| ChatRequestMessage {
                role: &m.role,
                content: &m.content,
            })
            .collect();

        let body = ChatRequest {
            model: &self.model,
            messages: req_msgs,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };

        // Exponential backoff on rate limits AND transient network errors.
        // Long benchmark runs (1500+ questions) WILL hit transient drops from
        // any of the providers OpenRouter proxies. Without this retry the
        // whole run fails 1+ hour in.
        let delays = [2u64, 4, 8, 16, 32];
        let mut attempt = 0usize;
        loop {
            let send_result = self
                .client
                .post(ENDPOINT)
                .bearer_auth(&self.api_key)
                .header("HTTP-Referer", "https://github.com/199-biotechnologies/engram-2")
                .header("X-Title", "engram v2 benchmark")
                .json(&body)
                .send()
                .await;
            let resp = match send_result {
                Ok(r) => r,
                Err(e) => {
                    // Connection-level error (DNS, TCP reset, timeout, TLS).
                    // These are usually transient — retry with backoff.
                    if attempt < delays.len() {
                        let wait = delays[attempt];
                        attempt += 1;
                        tracing::warn!(
                            "openrouter network error, backing off {}s (attempt {}): {}",
                            wait,
                            attempt,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(LlmError::Http {
                        provider: "openrouter",
                        source: e,
                    });
                }
            };

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt < delays.len() {
                    let wait = delays[attempt];
                    attempt += 1;
                    tracing::info!(
                        "openrouter 429, backing off {}s (attempt {})",
                        wait,
                        attempt
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(LlmError::RateLimited {
                    provider: "openrouter",
                });
            }
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(LlmError::Api {
                    provider: "openrouter",
                    message: format!("status {}: {}", status, body_text),
                });
            }

            // Read the body as text first so we can log it on parse failure.
            let body_text = resp.text().await.map_err(|e| LlmError::Http {
                provider: "openrouter",
                source: e,
            })?;
            let parsed: ChatResponseBody =
                serde_json::from_str(&body_text).map_err(|e| LlmError::InvalidResponse {
                    provider: "openrouter",
                    message: format!(
                        "decode failed: {}; body first 500 chars: {}",
                        e,
                        body_text.chars().take(500).collect::<String>()
                    ),
                })?;

            // Null or missing content is treated as an empty string so one
            // flaky question doesn't abort a whole benchmark run.
            let content = parsed
                .choices
                .first()
                .and_then(|c| c.message.content.clone())
                .unwrap_or_default();

            return Ok(ChatResponse {
                content,
                prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
                completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
                model: parsed.model.unwrap_or_else(|| self.model.clone()),
            });
        }
    }
}
