//! Anthropic Messages API client: fact extraction with Claude, direct.
//!
//! Raw HTTP, because Rust has no official Anthropic SDK. Same `ChatLlm`
//! contract as [`crate::openrouter`], with stricter error hygiene: an error
//! string carries the status and Anthropic's documented error type, never the
//! provider's message or body. The message is free text, and a validation
//! error can quote the request, which is someone's memory.
//!
//! Where the request differs from an OpenAI-shaped one:
//! - System messages go in the top-level `system` field.
//! - No `temperature`: Claude Haiku 5.5 rejects a non-default value. Depth is
//!   set with `output_config.effort` instead.
//! - Haiku 5.5 thinks adaptively, and thinking counts against `max_tokens`,
//!   so the cap leaves room for both. 8,192 cut off 9 of 460 backfilled
//!   memories (the long ones, which yield the most facts); the cap is now
//!   32,768, and only generated tokens are billed.
//! - The answer is the `text` blocks joined; `thinking` blocks are skipped.
//!   A `max_tokens` or `refusal` stop is an error, never a silently partial
//!   answer.

use crate::openrouter::decode_failure_message;
use crate::{ChatLlm, ChatMessage, ChatResponse, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The fact-extraction model when an Anthropic key is configured. Measured
/// under remember's request (benchmarks/models-2026-10.md): it ties the
/// OpenRouter default on the safety key at well under half the cost.
pub const DEFAULT_EXTRACTION_MODEL: &str = "claude-haiku-5-5";
/// The effort the extraction default was measured at. `low` was faster but
/// lost three of 28 safety facts that `medium` keeps. Sent only with
/// [`DEFAULT_EXTRACTION_MODEL`]: an overridden model gets the API's default.
pub const DEFAULT_EXTRACTION_EFFORT: &str = "medium";
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const PROVIDER: &str = "anthropic";
/// Anthropic's documented `error.type` values: a closed vocabulary, so one
/// can travel in an error string without carrying request text.
const ERROR_TYPES: &[&str] = &[
    "invalid_request_error",
    "authentication_error",
    "billing_error",
    "permission_error",
    "not_found_error",
    "request_too_large",
    "rate_limit_error",
    "api_error",
    "overloaded_error",
];

pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                // A long memory can take minutes to extract at the raised cap.
                .timeout(std::time::Duration::from_secs(300))
                .connect_timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
            api_key: api_key.into(),
            model: DEFAULT_EXTRACTION_MODEL.to_string(),
            max_tokens: 32_768,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    fn request<'a>(&'a self, messages: &'a [ChatMessage]) -> MessagesRequest<'a> {
        let system: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .collect();
        MessagesRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system: (!system.is_empty()).then(|| system.join("\n\n")),
            messages: messages
                .iter()
                .filter(|m| m.role != "system")
                .map(|m| RequestMessage {
                    role: &m.role,
                    content: &m.content,
                })
                .collect(),
            output_config: (self.model == DEFAULT_EXTRACTION_MODEL).then_some(OutputConfig {
                effort: DEFAULT_EXTRACTION_EFFORT,
            }),
        }
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<RequestMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
}

#[derive(Serialize)]
struct RequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct OutputConfig {
    effort: &'static str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

/// Retry or surface.
enum Attempt {
    Permanent(LlmError),
    Transient(LlmError),
}

#[async_trait]
impl ChatLlm for AnthropicClient {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatResponse, LlmError> {
        let body = self.request(messages);
        // The OpenRouter client's schedule: transport errors, 429, 529
        // (overloaded) and other 5xx are retried; any other 4xx is a real
        // client error and surfaces at once.
        let delays = [2u64, 4, 8, 16, 32, 64];
        let mut attempt = 0usize;
        loop {
            match self.try_once(&body).await {
                Ok(resp) => return Ok(resp),
                Err(Attempt::Permanent(err)) => return Err(err),
                Err(Attempt::Transient(err)) => {
                    let Some(&wait) = delays.get(attempt) else {
                        return Err(err);
                    };
                    attempt += 1;
                    tracing::warn!(
                        "anthropic transient error, backing off {wait}s (attempt {attempt}): {err}"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                }
            }
        }
    }
}

impl AnthropicClient {
    async fn try_once(&self, body: &MessagesRequest<'_>) -> Result<ChatResponse, Attempt> {
        let transport = |e| {
            Attempt::Transient(LlmError::Http {
                provider: PROVIDER,
                source: e,
            })
        };
        let resp = self
            .client
            .post(ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Attempt::Transient(LlmError::RateLimited {
                provider: PROVIDER,
            }));
        }
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let err = LlmError::Api {
                provider: PROVIDER,
                message: status_error(status, &body_text),
            };
            // 529 is Anthropic's "overloaded", a server-side condition.
            return Err(if status.is_server_error() || status.as_u16() == 529 {
                Attempt::Transient(err)
            } else {
                Attempt::Permanent(err)
            });
        }
        let body_text = resp.text().await.map_err(transport)?;
        let parsed: MessagesResponse = serde_json::from_str(&body_text).map_err(|e| {
            Attempt::Transient(LlmError::InvalidResponse {
                provider: PROVIDER,
                message: decode_failure_message(&e, body_text.len()),
            })
        })?;
        match parsed.stop_reason.as_deref() {
            Some("max_tokens") => {
                return Err(Attempt::Permanent(LlmError::InvalidResponse {
                    provider: PROVIDER,
                    message: format!(
                        "the answer was cut off at max_tokens ({}), thinking included",
                        self.max_tokens
                    ),
                }))
            }
            Some("refusal") => {
                return Err(Attempt::Permanent(LlmError::Api {
                    provider: PROVIDER,
                    message: "the model declined the request (stop_reason: refusal)".into(),
                }))
            }
            _ => {}
        }
        let content = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<String>();
        Ok(ChatResponse {
            content,
            prompt_tokens: parsed.usage.as_ref().and_then(|u| u.input_tokens),
            completion_tokens: parsed.usage.as_ref().and_then(|u| u.output_tokens),
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
        })
    }
}

/// A non-2xx answer as an error string: the status and, when the body names
/// one, the documented error type. Nothing else from the body is kept.
fn status_error(status: reqwest::StatusCode, body: &str) -> String {
    let kind = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/type")?.as_str().map(str::to_owned))
        .filter(|t| ERROR_TYPES.contains(&t.as_str()));
    match kind {
        Some(kind) => format!("status {status}: {kind}"),
        None => format!("status {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_the_status_and_type_but_never_the_message() {
        // A validation message can quote the request, and the request is the
        // memory being read.
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Invalid input: QUOKKA-ANTHROPIC-DO-NOT-ECHO"},"request_id":"req_1"}"#;
        let msg = status_error(reqwest::StatusCode::BAD_REQUEST, body);
        assert_eq!(msg, "status 400 Bad Request: invalid_request_error");
        // An undocumented type is free text too.
        let odd = r#"{"error":{"type":"QUOKKA-ANTHROPIC-DO-NOT-ECHO"}}"#;
        assert_eq!(
            status_error(reqwest::StatusCode::BAD_REQUEST, odd),
            "status 400 Bad Request"
        );
        assert_eq!(
            status_error(reqwest::StatusCode::BAD_GATEWAY, "<html>QUOKKA</html>"),
            "status 502 Bad Gateway"
        );
    }

    fn request_json(client: &AnthropicClient, messages: &[ChatMessage]) -> serde_json::Value {
        serde_json::to_value(client.request(messages)).unwrap()
    }

    #[test]
    fn system_goes_top_level_with_the_measured_effort_and_no_temperature() {
        let c = AnthropicClient::new("k");
        let body = request_json(
            &c,
            &[
                ChatMessage::system("extract facts"),
                ChatMessage::user("Ana moved to Lisbon."),
            ],
        );
        assert_eq!(body["model"], DEFAULT_EXTRACTION_MODEL);
        assert_eq!(body["system"], "extract facts");
        assert_eq!(
            body["messages"],
            serde_json::json!([{"role": "user", "content": "Ana moved to Lisbon."}])
        );
        assert_eq!(body["output_config"]["effort"], DEFAULT_EXTRACTION_EFFORT);
        // Haiku 5.5 answers a non-default temperature with a 400.
        assert!(body.get("temperature").is_none());
        // Thinking counts against the cap, and long memories yield many
        // facts: 8,192 truncated real extractions.
        assert!(body["max_tokens"].as_u64().unwrap() >= 32_768);
    }

    #[test]
    fn an_overridden_model_gets_the_apis_default_effort() {
        let c = AnthropicClient::new("k").with_model("claude-sonnet-5-5");
        let body = request_json(&c, &[ChatMessage::user("x")]);
        assert!(body.get("output_config").is_none());
        assert!(body.get("system").is_none());
    }

    #[test]
    fn only_text_blocks_make_the_answer() {
        let r: MessagesResponse = serde_json::from_str(
            r#"{"model":"claude-haiku-5-5","stop_reason":"end_turn",
                "content":[{"type":"thinking","thinking":"","signature":"s"},
                           {"type":"text","text":"[{\"subject\":"},{"type":"text","text":"\"Ana\"}]"}],
                "usage":{"input_tokens":12,"output_tokens":7}}"#,
        )
        .unwrap();
        let text: String = r
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text.as_deref())
            .collect();
        assert_eq!(text, r#"[{"subject":"Ana"}]"#);
    }
}
