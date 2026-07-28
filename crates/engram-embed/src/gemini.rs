//! Gemini Embedding 2 client.
//!
//! Uses Google's `:embedContent` REST endpoint with the v1beta API.
//! Reads `GEMINI_API_KEY` or accepts an explicit key.
//!
//! Model override: set `GEMINI_EMBED_MODEL` to force a specific model name
//! (e.g. "gemini-embedding-001" to pin to the legacy stable version).

use crate::{BatchEmbedFailure, EmbedError, Embedder, TaskMode};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL: &str = "gemini-embedding-2";
pub const DEFAULT_DIMS: usize = 1536;
pub const PROMPT_FORMAT: &str = "gemini-embedding-2-v1";
/// Longest input we ship per text: ~1,950 tokens, under Gemini's 2,048 cap.
/// Anything longer is truncated client-side — loudly, see `prepare_inputs`.
pub const MAX_INPUT_CHARS: usize = 7800;
/// Gemini's `:batchEmbedContents` accepts up to 250 texts but enforces a
/// ~20,000 token per-request cap. We approximate tokens as chars/4 and pack
/// under ~15,000 estimated tokens for safety. EACH BATCH IS ONE BILLED
/// REQUEST — which is why the accounting in `embed_batch_accounted` is only as
/// honest as `plan_batches`.
const MAX_CHARS_PER_BATCH: usize = 60_000;
const MAX_TEXTS_PER_BATCH: usize = 100;
const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiEmbedder {
    client: reqwest::Client,
    /// Primary key first, then fallbacks. `active` points at the key all
    /// requests currently use; a quota or auth refusal advances it. The
    /// pointer only moves forward: a depleted key stays retired for the rest
    /// of the process, and the next run starts fresh from the primary.
    keys: Vec<String>,
    active: std::sync::atomic::AtomicUsize,
    /// Why the last rotation happened (http status word) — read by
    /// degradation_note so the warning diagnoses the right condition instead
    /// of telling the user to replace a merely rate-limited key.
    rotation_reason: std::sync::Mutex<Option<String>>,
    model: String,
    dimensions: usize,
}

impl GeminiEmbedder {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .connect_timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .pool_idle_timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("failed to build reqwest client"),
            keys: vec![api_key.into()],
            active: std::sync::atomic::AtomicUsize::new(0),
            rotation_reason: std::sync::Mutex::new(None),
            model: DEFAULT_MODEL.to_string(),
            dimensions: DEFAULT_DIMS,
        }
    }

    /// Append fallback keys, skipping empties and duplicates of keys already
    /// held. Order is priority order.
    pub fn with_fallback_keys<I: IntoIterator<Item = String>>(mut self, extra: I) -> Self {
        for k in extra {
            let k = k.trim().to_string();
            if !k.is_empty() && !self.keys.contains(&k) {
                self.keys.push(k);
            }
        }
        self
    }

    /// Snapshot of the active key index — callers hold this across a request
    /// so a refusal can rotate FROM the key that actually failed.
    fn active_index(&self) -> usize {
        self.active
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(self.keys.len() - 1)
    }

    #[cfg(test)]
    fn active_key(&self) -> &str {
        &self.keys[self.active_index()]
    }

    /// Masked form for logs and notes: enough to recognize, never enough to use.
    fn mask(key: &str) -> String {
        // Threshold 16: at 12, head-6 + tail-4 left only 2 characters hidden.
        if key.chars().count() < 16 {
            "***".to_string()
        } else {
            let head: String = key.chars().take(6).collect();
            let tail: String = key
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            format!("{head}...{tail}")
        }
    }

    /// Retire the key the CALLER just saw fail and advance to the next
    /// fallback. Returns true when there is a key to retry with (either this
    /// call advanced the pointer, or a concurrent caller already rotated past
    /// the failed key). Compare-and-swap FROM THE OBSERVED INDEX: two
    /// concurrent refusals of key N advance the pointer once, and a fallback
    /// that was never tried can never be skipped.
    fn rotate_from(&self, observed: usize, why: &str) -> bool {
        if observed + 1 >= self.keys.len() {
            // The failed key was the last one. Another task may still have
            // rotated TO it from an earlier key; either way there is nothing
            // beyond it to try.
            return false;
        }
        let rotated = self
            .active
            .compare_exchange(
                observed,
                observed + 1,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok();
        if rotated {
            *self
                .rotation_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(why.to_string());
            tracing::warn!(
                "gemini key {} of {} refused ({why}); rotating to fallback key {}",
                observed + 1,
                self.keys.len(),
                observed + 2
            );
        }
        // A lost CAS means another task already moved past the failed key:
        // the retry should proceed on whatever is active now.
        rotated || self.active.load(std::sync::atomic::Ordering::Relaxed) > observed
    }

    /// True when this non-2xx body is Google saying the KEY is bad — the one
    /// 400/401 class where trying the next key is correct. Any other 400 is a
    /// real request error: rotating on it would burn every key in the chain
    /// masking a bug.
    fn body_says_key_invalid(body: &str) -> bool {
        body.contains("API_KEY_INVALID")
            || body.contains("API key not valid")
            || body.contains("API key expired")
            || body.contains("CREDENTIALS_MISSING")
    }

    pub fn from_env() -> Result<Self, EmbedError> {
        let key = std::env::var("GEMINI_API_KEY")
            .map_err(|_| EmbedError::MissingKey { provider: "gemini" })?;
        let mut e = Self::new(key);
        if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
            e = e.with_model(model);
        }
        Ok(e)
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims;
        self
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Format and (loudly) truncate every input to the provider window.
    ///
    /// Truncation is DATA LOSS, not a formatting detail: the tail of the text
    /// never reaches the index and is unretrievable forever. It used to happen
    /// silently. Chunking now caps far below this window, so every hit here is
    /// a real problem — say so.
    fn prepare_inputs(&self, texts: &[&str], mode: TaskMode) -> Vec<String> {
        let mut truncations = 0usize;
        let truncated: Vec<String> = texts
            .iter()
            .map(|t| {
                if t.len() <= MAX_INPUT_CHARS {
                    format_input(t, mode)
                } else {
                    // Truncate at a char boundary.
                    let mut end = MAX_INPUT_CHARS;
                    while !t.is_char_boundary(end) && end > 0 {
                        end -= 1;
                    }
                    truncations += 1;
                    tracing::warn!(
                        "gemini embed: input of {} chars exceeds the {} char window; \
                         {} chars were DROPPED and are not retrievable — lower \
                         ENGRAM_CHUNK_MAX_CHARS or re-chunk this content",
                        t.len(),
                        MAX_INPUT_CHARS,
                        t.len() - end,
                    );
                    format_input(&t[..end], mode)
                }
            })
            .collect();
        if truncations > 0 {
            tracing::warn!(
                "gemini embed: {} of {} inputs were truncated",
                truncations,
                texts.len()
            );
        }
        truncated
    }
}

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    content: Content,
    #[serde(rename = "taskType", skip_serializing_if = "Option::is_none")]
    task_type: Option<&'static str>,
    #[serde(
        rename = "outputDimensionality",
        skip_serializing_if = "Option::is_none"
    )]
    output_dimensionality: Option<usize>,
}

#[derive(Serialize)]
struct BatchEmbedRequest {
    requests: Vec<EmbedRequest>,
}

#[derive(Serialize)]
struct Content {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Part {
    text: String,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embedding: Embedding,
}

#[derive(Deserialize)]
struct BatchEmbedResponse {
    embeddings: Vec<Embedding>,
}

#[derive(Deserialize)]
struct Embedding {
    values: Vec<f32>,
}

fn task_type_str(mode: TaskMode) -> &'static str {
    match mode {
        TaskMode::RetrievalQuery => "RETRIEVAL_QUERY",
        TaskMode::RetrievalDocument => "RETRIEVAL_DOCUMENT",
    }
}

fn supports_task_type(model: &str) -> bool {
    !model.starts_with("gemini-embedding-2")
}

fn format_input(text: &str, mode: TaskMode) -> String {
    match mode {
        TaskMode::RetrievalQuery => format!("query: {}", text.trim()),
        TaskMode::RetrievalDocument => format!("document: {}", text.trim()),
    }
}

/// Split prepared inputs into the HTTP batches this client will send.
///
/// Pure and separately testable on purpose: this batching is precisely what
/// turns ONE logical embed call into SEVERAL billed requests, so the accounting
/// in `embed_batch_accounted` is only as honest as this function.
fn plan_batches(texts: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < texts.len() {
        let mut end = idx;
        let mut batch_chars = 0usize;
        while end < texts.len()
            && (end - idx) < MAX_TEXTS_PER_BATCH
            && batch_chars + texts[end].len() <= MAX_CHARS_PER_BATCH
        {
            batch_chars += texts[end].len();
            end += 1;
        }
        // A single text over the whole batch budget still has to ship.
        if end == idx {
            end = idx + 1;
        }
        out.push(idx..end);
        idx = end;
    }
    out
}

#[async_trait]
impl Embedder for GeminiEmbedder {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn prompt_format(&self) -> &'static str {
        PROMPT_FORMAT
    }

    fn degradation_note(&self) -> Option<String> {
        let i = self.active.load(std::sync::atomic::Ordering::Relaxed);
        (i > 0).then(|| {
            let served = Self::mask(&self.keys[i.min(self.keys.len() - 1)]);
            // Diagnose what actually happened. A 429 can be a transient
            // per-minute rate limit, so "replace the primary" would send the
            // operator to fix a healthy key; only a 400/401/403 refusal
            // means the key itself is dead.
            let diagnosis = match self
                .rotation_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref()
            {
                Some("429") => {
                    "the primary key was rate-limited or out of quota (429) — possibly \
                     transient; verify with `engram config check` before replacing it"
                }
                Some("400") | Some("401") | Some("403") => {
                    "the primary key was refused as invalid or revoked — replace it, \
                     then verify the chain with `engram config check`"
                }
                _ => "the primary key stopped serving — verify with `engram config check`",
            };
            format!(
                "gemini failover: this command ran on fallback key {} of {} ({served}); \
                 {diagnosis}.",
                i + 1,
                self.keys.len()
            )
        })
    }

    fn max_input_chars(&self) -> Option<usize> {
        Some(MAX_INPUT_CHARS)
    }

    async fn embed_one(&self, text: &str, mode: TaskMode) -> Result<Vec<f32>, EmbedError> {
        let formatted = format_input(text, mode);
        let req = EmbedRequest {
            model: format!("models/{}", self.model),
            content: Content {
                parts: vec![Part { text: formatted }],
            },
            task_type: supports_task_type(&self.model).then_some(task_type_str(mode)),
            output_dimensionality: Some(self.dimensions),
        };

        // Full-lifecycle retry: send + 429 + 5xx + body read + JSON parse.
        // The URL is rebuilt every attempt because a quota refusal rotates
        // `active` to the next fallback key.
        // Long benchmark runs (1500+ queries) hit transient drops; without
        // retry the whole bench dies on the first one.
        let delays_secs: [u64; 6] = [2, 4, 8, 16, 32, 64];
        let mut attempt: u32 = 0;
        loop {
            let key_idx = self.active_index();
            let url = format!(
                "{}/{}:embedContent?key={}",
                ENDPOINT, self.model, self.keys[key_idx]
            );
            // 1. send
            let send_result = self.client.post(&url).json(&req).send().await;
            let resp = match send_result {
                Ok(r) => r,
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tracing::warn!(
                            "gemini embed_one network error, backing off {}s (attempt {}): {}",
                            wait,
                            attempt,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(EmbedError::Http {
                        provider: "gemini",
                        source: e,
                    });
                }
            };

            // 2. status
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::FORBIDDEN
            {
                // A fallback key is tried BEFORE any backoff: a depleted or
                // revoked key never recovers inside this call, so sleeping on
                // it (~3 min of ladder) would only delay the key that works.
                // Per-minute rate limits also clear instantly on a different
                // key. Only when no keys remain does the ladder make sense.
                if self.rotate_from(key_idx, status.as_str()) {
                    continue;
                }
                if status == reqwest::StatusCode::FORBIDDEN {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(EmbedError::Api {
                        provider: "gemini",
                        message: format!(
                            "status 403: {}",
                            body.chars().take(200).collect::<String>()
                        ),
                    });
                }
                if (attempt as usize) < delays_secs.len() {
                    let wait = delays_secs[attempt as usize];
                    attempt += 1;
                    tracing::info!(
                        "gemini embed_one 429, backing off {}s (attempt {})",
                        wait,
                        attempt
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(EmbedError::RateLimited { provider: "gemini" });
            }
            if status.is_server_error() {
                let body = resp.text().await.unwrap_or_default();
                if (attempt as usize) < delays_secs.len() {
                    let wait = delays_secs[attempt as usize];
                    attempt += 1;
                    tracing::warn!(
                        "gemini embed_one 5xx ({}), backing off {}s (attempt {}): {}",
                        status,
                        wait,
                        attempt,
                        body
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(EmbedError::Api {
                    provider: "gemini",
                    message: format!("status {}: {}", status, body),
                });
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                // 400/401 with Google's key-invalid markers is a DEAD KEY,
                // not a bad request — the exact condition `config check`
                // labels invalid_or_revoked. Without this arm an invalid
                // primary defeated the whole failover feature: rotation
                // fired on 429/403 but a deleted/regenerated key answers
                // 400 API_KEY_INVALID and hard-failed every surface while
                // a healthy fallback sat configured.
                if (status == reqwest::StatusCode::BAD_REQUEST
                    || status == reqwest::StatusCode::UNAUTHORIZED)
                    && Self::body_says_key_invalid(&body)
                    && self.rotate_from(key_idx, status.as_str())
                {
                    continue;
                }
                // Any other 4xx — real client error, never retry.
                return Err(EmbedError::Api {
                    provider: "gemini",
                    message: format!("status {}: {}", status, body),
                });
            }

            // 3. body read (TLS drop mid-stream lands here)
            let body_text =
                match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        if (attempt as usize) < delays_secs.len() {
                            let wait = delays_secs[attempt as usize];
                            attempt += 1;
                            tracing::warn!(
                            "gemini embed_one body-read error, backing off {}s (attempt {}): {}",
                            wait, attempt, e
                        );
                            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                            continue;
                        }
                        return Err(EmbedError::Http {
                            provider: "gemini",
                            source: e,
                        });
                    }
                };

            // 4. JSON parse
            match serde_json::from_str::<EmbedResponse>(&body_text) {
                Ok(parsed) => return Ok(parsed.embedding.values),
                Err(e) => {
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tracing::warn!(
                            "gemini embed_one parse error, backing off {}s (attempt {}): {}",
                            wait,
                            attempt,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(EmbedError::Api {
                        provider: "gemini",
                        message: format!(
                            "parse failed: {}; body: {}",
                            e,
                            body_text.chars().take(500).collect::<String>()
                        ),
                    });
                }
            }
        }
    }

    /// Override the default loop with a real batch call, and report what the
    /// provider was already PAID for when one of those calls fails.
    ///
    /// Gemini's `:batchEmbedContents` accepts up to 250 texts BUT enforces both
    /// a 2,048 input-token-per-text cap and a 20,000 token-per-request cap, so
    /// ONE logical embed is SEVERAL billed HTTP requests (see `plan_batches`).
    /// Inputs longer than the per-text window are truncated client-side so we
    /// do not even ship the bytes.
    async fn embed_batch_accounted(
        &self,
        texts: &[&str],
        mode: TaskMode,
    ) -> Result<Vec<Vec<f32>>, BatchEmbedFailure> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let truncated = self.prepare_inputs(texts, mode);
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());

        // What the provider has already SERVED, and will therefore bill, by the
        // time this call gives up. A batch counts the moment a 2xx lands —
        // INCLUDING a 2xx we cannot parse or whose arity is wrong: the vectors
        // were computed, so the money is spent whether or not we can use them.
        // A transport error, a 429 and a 4xx/5xx are not billed.
        let mut charged_texts = 0usize;
        let mut charged_requests = 0usize;

        for batch in plan_batches(&truncated) {
            let requests: Vec<EmbedRequest> = truncated[batch.start..batch.end]
                .iter()
                .map(|t| EmbedRequest {
                    model: format!("models/{}", self.model),
                    content: Content {
                        parts: vec![Part { text: t.clone() }],
                    },
                    task_type: supports_task_type(&self.model).then_some(task_type_str(mode)),
                    output_dimensionality: Some(self.dimensions),
                })
                .collect();
            let body = BatchEmbedRequest { requests };

            // Retry with exponential backoff on 429 and transient network
            // errors. Gemini free tier hits the per-minute quota fast on
            // large corpora; we try up to 6 times with backoffs 4s, 8s,
            // 16s, 32s, 60s, 60s (~3 min total) before surfacing the error.
            // Transport errors (connection reset, DNS hiccup) get the same
            // backoff since they're usually transient.
            let mut attempt: u32 = 0;
            let delays_secs: [u64; 6] = [4, 8, 16, 32, 60, 60];
            let parsed: BatchEmbedResponse = loop {
                // Rebuilt per attempt: a quota refusal rotates to a fallback key.
                let key_idx = self.active_index();
                let url = format!(
                    "{}/{}:batchEmbedContents?key={}",
                    ENDPOINT, self.model, self.keys[key_idx]
                );
                let send_result = self.client.post(&url).json(&body).send().await;
                let resp = match send_result {
                    Ok(r) => r,
                    Err(e) => {
                        if (attempt as usize) < delays_secs.len() {
                            let wait = delays_secs[attempt as usize];
                            attempt += 1;
                            tracing::info!(
                                "gemini network error ({}), backing off {}s (attempt {})",
                                e,
                                wait,
                                attempt
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                            continue;
                        }
                        // Never served, so never billed.
                        return Err(BatchEmbedFailure {
                            error: EmbedError::Http {
                                provider: "gemini",
                                source: e,
                            },
                            charged_texts,
                            charged_requests,
                        });
                    }
                };
                let status = resp.status();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    // Rotate to a fallback key before burning the backoff
                    // ladder on a key that cannot recover (depleted credits,
                    // revoked key). A 429/403 is never billed, so the charged
                    // counters are untouched by the switch.
                    if self.rotate_from(key_idx, status.as_str()) {
                        continue;
                    }
                    if status == reqwest::StatusCode::FORBIDDEN {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(BatchEmbedFailure {
                            error: EmbedError::Api {
                                provider: "gemini",
                                message: format!(
                                    "status 403: {}",
                                    body.chars().take(200).collect::<String>()
                                ),
                            },
                            charged_texts,
                            charged_requests,
                        });
                    }
                    if (attempt as usize) < delays_secs.len() {
                        let wait = delays_secs[attempt as usize];
                        attempt += 1;
                        tracing::info!("gemini 429, backing off {}s (attempt {})", wait, attempt);
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(BatchEmbedFailure {
                        error: EmbedError::RateLimited { provider: "gemini" },
                        charged_texts,
                        charged_requests,
                    });
                }
                if !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    // Same dead-key arm as embed_one: a deleted or
                    // regenerated key answers 400 API_KEY_INVALID and must
                    // fail over, not fail the batch. Never billed, so the
                    // charged counters are untouched.
                    if (status == reqwest::StatusCode::BAD_REQUEST
                        || status == reqwest::StatusCode::UNAUTHORIZED)
                        && Self::body_says_key_invalid(&body_text)
                        && self.rotate_from(key_idx, status.as_str())
                    {
                        continue;
                    }
                    return Err(BatchEmbedFailure {
                        error: EmbedError::Api {
                            provider: "gemini",
                            message: format!("status {}: {}", status, body_text),
                        },
                        charged_texts,
                        charged_requests,
                    });
                }
                // 2xx: this request was served, so it is billed even if the
                // body below turns out to be unusable.
                charged_texts += batch.len();
                charged_requests += 1;
                match resp.json::<BatchEmbedResponse>().await {
                    Ok(parsed) => break parsed,
                    Err(e) => {
                        return Err(BatchEmbedFailure {
                            error: EmbedError::Http {
                                provider: "gemini",
                                source: e,
                            },
                            charged_texts,
                            charged_requests,
                        })
                    }
                }
            };
            // Positional zip against the input texts downstream: a short
            // batch would silently mis-align or drop chunks, so a count
            // mismatch is a hard error, never a truncation.
            if parsed.embeddings.len() != batch.len() {
                return Err(BatchEmbedFailure {
                    error: EmbedError::Api {
                        provider: "gemini",
                        message: format!(
                            "batch embedding count mismatch: sent {} texts, got {} embeddings",
                            batch.len(),
                            parsed.embeddings.len()
                        ),
                    },
                    charged_texts,
                    charged_requests,
                });
            }
            for emb in parsed.embeddings {
                out.push(emb.values);
            }
        }
        debug_assert_eq!(out.len(), texts.len());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fallback_keys_dedup_and_keep_priority_order() {
        let e = GeminiEmbedder::new("primary").with_fallback_keys(vec![
            "primary".to_string(),
            "  ".to_string(),
            "second".to_string(),
            "second".to_string(),
            "third".to_string(),
        ]);
        assert_eq!(e.keys, vec!["primary", "second", "third"]);
        assert_eq!(e.active_key(), "primary");
    }

    #[test]
    fn only_key_invalid_bodies_qualify_a_400_for_rotation() {
        assert!(GeminiEmbedder::body_says_key_invalid(
            r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#
        ));
        assert!(GeminiEmbedder::body_says_key_invalid(
            "API key not valid. Please pass a valid API key."
        ));
        // A genuinely malformed request must NOT rotate — it would burn every
        // key in the chain masking a real bug.
        assert!(!GeminiEmbedder::body_says_key_invalid(
            r#"{"error":{"code":400,"message":"Unable to submit request because it has an empty text parameter"}}"#
        ));
    }

    #[test]
    fn the_note_diagnoses_a_rate_limit_differently_from_a_dead_key() {
        let e = GeminiEmbedder::new("primary-key-aaaaaaaa")
            .with_fallback_keys(vec!["fallback-key-bbbbbbbbbb".to_string()]);
        e.rotate_from(0, "429");
        let note = e.degradation_note().expect("rotated");
        assert!(note.contains("rate-limited"), "{note}");
        assert!(
            !note.contains("replace it,"),
            "a 429 must not say the key is dead: {note}"
        );

        let e2 = GeminiEmbedder::new("primary-key-aaaaaaaa")
            .with_fallback_keys(vec!["fallback-key-bbbbbbbbbb".to_string()]);
        e2.rotate_from(0, "400");
        let note2 = e2.degradation_note().expect("rotated");
        assert!(note2.contains("invalid or revoked"), "{note2}");
    }

    #[test]
    fn concurrent_rotations_from_the_same_key_advance_once() {
        // The race: both callers used key 0 and both saw it fail. The first
        // rotate advances 0 -> 1; the second, CASing from the SAME observed
        // index, loses the exchange but still reports "retry is possible" —
        // and crucially does NOT advance again, so the untried fallback "b"
        // can never be skipped.
        let e = GeminiEmbedder::new("a").with_fallback_keys(vec!["b".to_string(), "c".to_string()]);
        assert!(e.rotate_from(0, "429"));
        assert!(
            e.rotate_from(0, "429"),
            "lost race still means a retry key exists"
        );
        assert_eq!(
            e.active_key(),
            "b",
            "fallback b must not be skipped untried"
        );
        // A genuine failure OF b advances to c; c is the end of the chain.
        assert!(e.rotate_from(1, "429"));
        assert_eq!(e.active_key(), "c");
        assert!(!e.rotate_from(2, "429"), "nothing beyond the last key");
    }

    #[test]
    fn a_healthy_embedder_reports_no_degradation_and_a_rotated_one_masks_the_key() {
        let e = GeminiEmbedder::new("primary-key-aaaaaaaa").with_fallback_keys(vec![
            "AQ.REDACTED-REVOKED-KEY".to_string(),
        ]);
        assert!(e.degradation_note().is_none(), "healthy = silent");
        e.rotate_from(0, "test");
        let note = e.degradation_note().expect("rotated = loud");
        assert!(note.contains("fallback key 2 of 2"), "{note}");
        assert!(note.contains("AQ.RED...-KEY"), "recognizable: {note}");
        assert!(
            !note.contains("REDACTED-REVOKED-KEY"),
            "the full key must never appear in a note that reaches stdout: {note}"
        );
        assert!(
            note.contains("config check"),
            "must name the remedy: {note}"
        );
    }

    #[test]
    fn rotation_advances_once_per_key_and_stops_at_the_end() {
        let e = GeminiEmbedder::new("a").with_fallback_keys(vec!["b".to_string()]);
        assert!(e.rotate_from(0, "test"));
        assert_eq!(e.active_key(), "b");
        assert!(!e.rotate_from(1, "test"), "nothing beyond the last key");
        assert_eq!(e.active_key(), "b");
        let single = GeminiEmbedder::new("only");
        assert!(!single.rotate_from(0, "test"));
    }

    use super::*;

    fn texts_of(count: usize, len: usize) -> Vec<String> {
        (0..count).map(|_| "x".repeat(len)).collect()
    }

    #[test]
    fn batches_pack_by_count_then_by_char_budget() {
        let ranges = plan_batches(&texts_of(250, 10));
        assert_eq!(ranges, vec![0..100, 100..200, 200..250]);
        // 25,000 chars each: the per-request char budget bites before the count.
        let ranges = plan_batches(&texts_of(4, 25_000));
        assert_eq!(ranges, vec![0..2, 2..4]);
    }

    #[test]
    fn an_oversized_text_still_gets_its_own_batch() {
        // One text over the whole batch budget must ship rather than be
        // dropped — a silently skipped input is an unindexed chunk.
        assert_eq!(plan_batches(&texts_of(1, 80_000)), vec![0..1]);
    }

    #[test]
    fn no_texts_means_no_requests() {
        assert!(
            plan_batches(&[]).is_empty(),
            "an empty embed is never billed"
        );
    }

    #[test]
    fn a_charged_prefix_is_always_a_prefix() {
        // `charged_texts` is used as `&texts[..charged_texts]`, which is only
        // the right slice while the batches tile the input in order.
        for texts in [texts_of(250, 10), texts_of(7, 25_000), texts_of(1, 80_000)] {
            let ranges = plan_batches(&texts);
            assert_eq!(ranges[0].start, 0);
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
            }
            assert_eq!(ranges.last().unwrap().end, texts.len());
        }
    }
}
