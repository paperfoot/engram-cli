//! OpenRouter chat client. OpenAI-compatible, routes to 350+ frontier models.
//!
//! Default model is `openai/gpt-4o-mini` — fast and cheap for judging.
//! For better answer generation use `anthropic/claude-sonnet-4.5`
//! or `openai/gpt-5` via the `with_model` builder.

use crate::{ChatLlm, ChatMessage, ChatResponse, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// Default to the latest GPT flagship on OpenRouter for generic chat/bench
// work. Compiler defaults are separated below so evidence extraction can use
// a fast model while synthesis uses a stronger one.
const DEFAULT_MODEL: &str = "openai/gpt-5.4";
pub const DEFAULT_EXTRACTION_MODEL: &str = "google/gemini-3.1-flash-lite-preview";
pub const DEFAULT_SYNTHESIS_MODEL: &str = "google/gemini-3.1-pro-preview";
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

        // Bulletproof retry covering the FULL request lifecycle:
        //   1. send()           — DNS, TCP, TLS handshake errors
        //   2. status check     — 429 backoff
        //   3. resp.text()      — body read can fail mid-stream on TLS drop
        //   4. JSON parse       — truncated body looks like a parse error
        //
        // 5xx server errors are also retried (transient on OpenRouter's side).
        // 4xx (except 429) are NOT retried — those are real client errors.
        //
        // Long benchmark runs (1500+ requests) hit at least one of these
        // failure modes per run. Without full-lifecycle retry the whole bench
        // dies 1+ hour in. Confirmed twice in practice on 2026-04-08.
        let delays = [2u64, 4, 8, 16, 32, 64];
        let mut attempt = 0usize;

        loop {
            let attempt_result = self.try_chat_once(&body).await;
            match attempt_result {
                Ok(resp) => return Ok(resp),
                Err(RetryReason::Permanent(err)) => return Err(err),
                Err(RetryReason::Transient(err)) => {
                    if attempt >= delays.len() {
                        return Err(err);
                    }
                    let wait = delays[attempt];
                    attempt += 1;
                    tracing::warn!(
                        "openrouter transient error, backing off {}s (attempt {}): {}",
                        wait,
                        attempt,
                        err
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                }
            }
        }
    }
}

/// Wrap a single try so the retry loop can pattern-match on the failure mode.
enum RetryReason {
    /// Don't retry — surface the error to the caller.
    Permanent(LlmError),
    /// Transient — retry with backoff.
    Transient(LlmError),
}

impl OpenRouterClient {
    async fn try_chat_once(&self, body: &ChatRequest<'_>) -> Result<ChatResponse, RetryReason> {
        // Step 1: send.
        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/paperfoot/engram-cli")
            .header("X-Title", "engram v2 benchmark")
            .json(body)
            .send()
            .await
            .map_err(|e| {
                RetryReason::Transient(LlmError::Http {
                    provider: "openrouter",
                    source: e,
                })
            })?;

        // Step 2: status.
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(RetryReason::Transient(LlmError::RateLimited {
                provider: "openrouter",
            }));
        }
        if status.is_server_error() {
            // 5xx is a transient OpenRouter / upstream provider failure.
            let body_text = resp.text().await.unwrap_or_default();
            return Err(RetryReason::Transient(LlmError::Api {
                provider: "openrouter",
                message: format!("status {}: {}", status, body_text),
            }));
        }
        if !status.is_success() {
            // 4xx (other than 429) is a real client error — never retry.
            let body_text = resp.text().await.unwrap_or_default();
            return Err(RetryReason::Permanent(LlmError::Api {
                provider: "openrouter",
                message: format!("status {}: {}", status, body_text),
            }));
        }

        // Step 3: body read. TLS drops mid-stream show up here.
        let body_text = resp.text().await.map_err(|e| {
            RetryReason::Transient(LlmError::Http {
                provider: "openrouter",
                source: e,
            })
        })?;

        // Step 4: parse. Treat truncation-flavored failures as transient.
        // A real schema mismatch (e.g. OpenRouter changed the response shape)
        // would also retry, but it would burn through all 6 attempts and then
        // surface — better than silently swallowing.
        let parsed: ChatResponseBody = match serde_json::from_str(&body_text) {
            Ok(p) => p,
            Err(e) => {
                let preview = body_text.chars().take(500).collect::<String>();
                return Err(RetryReason::Transient(LlmError::InvalidResponse {
                    provider: "openrouter",
                    message: format!("decode failed: {}; body first 500 chars: {}", e, preview),
                }));
            }
        };

        // Null or missing content is treated as an empty string so one
        // flaky question doesn't abort a whole benchmark run.
        let content = parsed
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        Ok(ChatResponse {
            content,
            prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
            model: parsed.model.clone().unwrap_or_else(|| self.model.clone()),
        })
    }
}
