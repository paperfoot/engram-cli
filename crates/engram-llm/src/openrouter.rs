//! OpenRouter chat client. OpenAI-compatible, routes to 350+ frontier models.
//!
//! Default model is `anthropic/claude-opus-4.8`. Every benchmark/compiler call
//! site overrides this with `with_model(...)`, so the default only applies to
//! ad-hoc callers; pick an explicit slug via the `with_model` builder.

use crate::{ChatLlm, ChatMessage, ChatResponse, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// Default to a current cross-family flagship for generic chat/bench work.
// Compiler defaults are separated below so evidence extraction can use a fast
// model while synthesis uses a stronger one.
const DEFAULT_MODEL: &str = "anthropic/claude-opus-4.8";
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

/// How much of a non-2xx response body an error message may carry.
///
/// The actionable part of a provider error is its head (`{"error":{"message":
/// "No auth credentials found"`), and the status code already carries most of
/// the diagnosis. The tail is not free: a 4xx from a validation failure can
/// echo the request back, and the request contains the memory being read, so
/// an uncapped body puts stored text into a string that reaches an agent's
/// stdout via `fact_extraction_note`.
const MAX_ERROR_BODY_CHARS: usize = 200;

/// Head of an error body, bounded, with the omission stated rather than silent.
fn error_body_head(body: &str) -> String {
    let head: String = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
    if head.len() == body.len() {
        head
    } else {
        format!("{head}… ({} bytes total, rest withheld)", body.len())
    }
}

/// The message a non-2xx response renders into, with the body bounded.
///
/// The head is deliberately KEPT: `{"error":{"message":"No auth credentials
/// found"` is the whole diagnosis. It is the TAIL that has to go — a 4xx from a
/// validation failure can echo the request back, and the request carries the
/// memory being read.
fn status_error_message(status: reqwest::StatusCode, body: &str) -> String {
    format!("status {status}: {}", error_body_head(body))
}

/// The decode failure a caller sees: category, position and size — never the
/// body, and never serde's own Display.
///
/// On a 200 the body IS the model's answer, and the model was reading someone's
/// memory. This string travels up through `FactExtractionError::Llm` into
/// `fact_extraction_note`, which `engram remember` prints on stdout AND stderr.
/// A `Category::Data` failure renders as `invalid type: string "<the value>"`,
/// so dropping a "preview" alone would still copy a slice of the answer.
/// Category, position and size are what a decode failure is actually diagnosed
/// from — the same rule engram-graph applies to fact JSON.
fn decode_failure_message(e: &serde_json::Error, body_len: usize) -> String {
    format!(
        "decode failed: {} error at line {} column {} ({body_len} bytes of model output withheld)",
        classify_json_error(e),
        e.line(),
        e.column(),
    )
}

/// serde_json's own category as a stable word, without its message.
fn classify_json_error(e: &serde_json::Error) -> &'static str {
    use serde_json::error::Category;
    match e.classify() {
        Category::Syntax => "syntax",
        Category::Data => "schema",
        Category::Eof => "truncated",
        Category::Io => "io",
    }
}

impl OpenRouterClient {
    async fn try_chat_once(&self, body: &ChatRequest<'_>) -> Result<ChatResponse, RetryReason> {
        // Step 1: send.
        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/paperfoot/engram-cli")
            .header("X-Title", "engram benchmark")
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
                message: status_error_message(status, &body_text),
            }));
        }
        if !status.is_success() {
            // 4xx (other than 429) is a real client error — never retry.
            let body_text = resp.text().await.unwrap_or_default();
            return Err(RetryReason::Permanent(LlmError::Api {
                provider: "openrouter",
                message: status_error_message(status, &body_text),
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
        //
        // Neither the body nor serde's own Display is echoed — see
        // `decode_failure_message` for why the preview had to go too.
        let parsed: ChatResponseBody = match serde_json::from_str(&body_text) {
            Ok(p) => p,
            Err(e) => {
                return Err(RetryReason::Transient(LlmError::InvalidResponse {
                    provider: "openrouter",
                    message: decode_failure_message(&e, body_text.len()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for the memory a provider error could echo back. Deliberately
    /// different from the sentinel in `engram-cli/tests/wave0_no_raw_leak.rs`,
    /// so a grep for a leaked string says which layer leaked it.
    const SENTINEL: &str = "QUOKKA-9E21-DO-NOT-ECHO";

    /// A body that is mostly filler with the sentinel past the cap.
    fn long_body_ending_in_the_sentinel() -> String {
        format!("{}{SENTINEL}", "f".repeat(5_000))
    }

    /// The failure a body produces when decoded as a chat response.
    ///
    /// Hand-rolled rather than `expect_err` so the response type does not have
    /// to derive `Debug` for a test's convenience — a `Debug` on a struct that
    /// holds model output is exactly the sort of thing that leaks it later.
    fn decode_error(body: &str) -> serde_json::Error {
        match serde_json::from_str::<ChatResponseBody>(body) {
            Ok(_) => panic!("the fixture must not decode: {body}"),
            Err(e) => e,
        }
    }

    #[test]
    fn a_short_error_body_is_reported_whole() {
        // The cap must not cost the actionable head: this is the entire
        // diagnosis for the most common provider failure there is.
        let body = r#"{"error":{"message":"No auth credentials found","code":401}}"#;
        assert_eq!(error_body_head(body), body);
    }

    #[test]
    fn a_body_exactly_at_the_cap_is_not_truncated() {
        // The `head.len() == body.len()` boundary: at the cap nothing is
        // withheld, so claiming otherwise would be a lie.
        let body = "x".repeat(MAX_ERROR_BODY_CHARS);
        let head = error_body_head(&body);
        assert_eq!(head, body);
        assert!(!head.contains("bytes total"), "{head}");
    }

    #[test]
    fn the_error_body_head_is_capped_and_states_the_omission() {
        let body = long_body_ending_in_the_sentinel();
        let head = error_body_head(&body);
        assert!(
            !head.contains(SENTINEL),
            "an echoed request body reached the error string: {head}"
        );
        assert!(head.starts_with(&"f".repeat(MAX_ERROR_BODY_CHARS)));
        assert!(
            head.contains(&format!("({} bytes total, rest withheld)", body.len())),
            "the omission must be stated with its true size: {head}"
        );
        let kept = head.split('…').next().expect("a head before the ellipsis");
        assert_eq!(kept.chars().count(), MAX_ERROR_BODY_CHARS, "{head}");
    }

    #[test]
    fn the_cap_counts_characters_and_never_splits_one() {
        // Pins char-wise truncation against a naive `&body[..200]` rewrite,
        // which would panic mid-character on any non-ASCII provider message.
        let body = "é".repeat(1_000);
        let head = error_body_head(&body);
        let kept = head.split('…').next().expect("a head before the ellipsis");
        assert_eq!(kept, "é".repeat(MAX_ERROR_BODY_CHARS));
        assert!(head.contains("(2000 bytes total, rest withheld)"), "{head}");
    }

    #[test]
    fn a_status_error_names_the_status_without_the_echoed_request() {
        let msg = status_error_message(
            reqwest::StatusCode::BAD_REQUEST,
            &long_body_ending_in_the_sentinel(),
        );
        assert!(msg.contains("status 400"), "{msg}");
        assert!(
            !msg.contains(SENTINEL),
            "a 4xx that echoed the request leaked the memory it carried: {msg}"
        );
    }

    #[test]
    fn a_decode_failure_names_the_category_without_the_model_answer() {
        // The exact leak the cap alone does not close: serde's Display for a
        // `Category::Data` failure quotes the offending VALUE, and on a 200
        // that value is the model's answer to a question about someone's
        // memory. Only the category, the position and the size may travel.
        let body = format!("{{\"choices\":\"{SENTINEL}\"}}");
        let err = decode_error(&body);
        assert!(
            err.to_string().contains(SENTINEL),
            "the fixture must carry the leak serde would print: {err}"
        );

        let msg = decode_failure_message(&err, body.len());
        assert!(msg.contains("schema"), "{msg}");
        assert!(msg.contains("line 1"), "{msg}");
        assert!(msg.contains(&format!("{} bytes", body.len())), "{msg}");
        assert!(
            !msg.contains(SENTINEL),
            "serde's own Display carried the model output out: {msg}"
        );
    }

    #[test]
    fn a_truncated_body_is_classified_truncated_not_echoed() {
        // A TLS drop mid-stream is the common case, and it cuts the body in the
        // middle of the answer — the worst possible place to quote from.
        let body = format!("{{\"choices\":[{{\"message\":{{\"content\":\"{SENTINEL}");
        let err = decode_error(&body);
        let msg = decode_failure_message(&err, body.len());
        assert!(msg.contains("truncated"), "{msg}");
        assert!(!msg.contains(SENTINEL), "{msg}");
    }

    #[test]
    fn each_json_error_category_has_a_stable_word() {
        // An operator greps for these words; changing one silently would break
        // every runbook that mentions it.
        let cases = [
            ("{not json}", "syntax"),
            (r#"{"choices":"a string"}"#, "schema"),
            (r#"{"choices":["#, "truncated"),
        ];
        for (body, expected) in cases {
            assert_eq!(classify_json_error(&decode_error(body)), expected, "{body}");
        }
    }
}
