//! TypeSafe JEV over HTTP: the direct API, or the same API served by
//! OpenRouter (identical answers; OpenRouter also reports `usage.cost`).
//!
//! One pooled client per process: server time is ~100 ms, and a fresh TLS
//! handshake costs 0.8-1.1 s, so connection reuse is most of the latency.
//! Retries follow the provider SDKs: two retries on 408/429/5xx and transport
//! errors, 0.5 s doubling, `retry-after` honoured up to 5 s.

use super::{Answer, Judge, JudgeError, JudgeRequest, JudgeResponse, USD_PER_INPUT_TOKEN};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const DIRECT_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// Pinned, never the `jev-latest` alias: thresholds are fitted per model
/// version. The two-part `jev-1.13` is rejected by the direct API.
pub const DIRECT_MODEL: &str = "jev-1.13.0";
pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/systemone";
pub const OPENROUTER_MODEL: &str = "typesafe/jev-1.13";

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RETRIES: u32 = 2;
const BASE_BACKOFF: Duration = Duration::from_millis(500);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);

/// How much of a provider's error message an error may carry. The 422 body
/// echoes the request (someone's memory), so only parsed message fields are
/// kept, and those are capped.
const MAX_ERROR_CHARS: usize = 200;

/// Where to send, with which key. `Debug` never prints the key.
pub enum TypeSafeRoute {
    /// api.typesafe.ai with `TYPESAFE_API_KEY`, model pinned to `jev-1.13.0`.
    Direct { key: String },
    /// openrouter.ai with `OPENROUTER_API_KEY`.
    OpenRouter { key: String },
}

impl std::fmt::Debug for TypeSafeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TypeSafeRoute::Direct { .. } => "Direct { key: *** }",
            TypeSafeRoute::OpenRouter { .. } => "OpenRouter { key: *** }",
        })
    }
}

/// A TypeSafe JEV judge. Cheap to clone (the HTTP pool is shared).
#[derive(Clone)]
pub struct TypeSafeJudge {
    client: reqwest::Client,
    provider: &'static str,
    endpoint: String,
    model: &'static str,
    key: String,
    timeout: Duration,
    base_backoff: Duration,
}

impl std::fmt::Debug for TypeSafeJudge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeSafeJudge")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// One HTTP client per process, shared by every judge built in it: a
/// long-lived process (`engramd`) builds a judge per request, and a client
/// per judge paid a fresh TLS handshake every time.
fn shared_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client")
        })
        .clone()
}

impl TypeSafeJudge {
    pub fn new(route: TypeSafeRoute) -> Self {
        let (provider, endpoint, model, key) = match route {
            TypeSafeRoute::Direct { key } => ("typesafe", DIRECT_ENDPOINT, DIRECT_MODEL, key),
            TypeSafeRoute::OpenRouter { key } => {
                ("openrouter", OPENROUTER_ENDPOINT, OPENROUTER_MODEL, key)
            }
        };
        Self {
            client: shared_client(),
            provider,
            endpoint: endpoint.to_string(),
            model,
            key,
            timeout: DEFAULT_TIMEOUT,
            base_backoff: BASE_BACKOFF,
        }
    }

    /// Per-attempt timeout (default 5 s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The key must never reach an error string, even if a provider echoed it.
    fn redact(&self, message: String) -> String {
        if self.key.is_empty() {
            message
        } else {
            message.replace(&self.key, "***")
        }
    }

    async fn send_once(&self, body: &[u8]) -> Result<WireResponse, Failure> {
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(self.timeout)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        let status = resp.status().as_u16();
        let retry_after = retry_after(resp.headers());
        let text = resp.text().await.map_err(|e| self.transport(e))?;
        if !(200..300).contains(&status) {
            return Err(Failure {
                error: JudgeError::Status {
                    provider: self.provider,
                    status,
                    message: self.redact(status_message(&text)),
                },
                retry_after,
            });
        }
        parse_response(&text, self.provider).map_err(|error| Failure {
            error,
            retry_after: None,
        })
    }

    fn transport(&self, e: reqwest::Error) -> Failure {
        let kind = if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "connection failed"
        } else if e.is_body() || e.is_decode() {
            "response interrupted"
        } else {
            "request failed"
        };
        // The cause chain says what actually failed (refused, DNS, TLS);
        // `without_url` because reqwest appends the URL to its own Display.
        let mut causes = Vec::new();
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            causes.push(s.to_string());
            source = s.source();
        }
        let mut message = format!("{kind}: {}", e.without_url());
        for cause in causes {
            message.push_str(": ");
            message.push_str(&cause);
        }
        Failure {
            error: JudgeError::Transport {
                provider: self.provider,
                message: self.redact(message),
            },
            retry_after: None,
        }
    }
}

struct Failure {
    error: JudgeError,
    retry_after: Option<Duration>,
}

#[derive(Serialize)]
struct WireRequest<'a, Q: Serialize> {
    model: &'a str,
    state: &'a Value,
    questions: Q,
}

#[async_trait]
impl Judge for TypeSafeJudge {
    fn name(&self) -> &'static str {
        self.provider
    }

    fn model_id(&self) -> &str {
        self.model
    }

    async fn ask(&self, request: &JudgeRequest) -> Result<JudgeResponse, JudgeError> {
        request.validate()?;
        let body = serde_json::to_vec(&WireRequest {
            model: self.model,
            state: &request.state,
            questions: request.questions_wire(),
        })
        .map_err(|e| JudgeError::InvalidRequest(format!("unserializable request: {e}")))?;
        let started = Instant::now();
        let mut attempt = 0;
        let wire = loop {
            match self.send_once(&body).await {
                Ok(wire) => break wire,
                Err(Failure { error, retry_after }) => {
                    if !error.is_retryable() || attempt >= MAX_RETRIES {
                        return Err(error);
                    }
                    let wait = retry_after
                        .map(|d| d.min(MAX_RETRY_AFTER))
                        .unwrap_or(self.base_backoff * 2u32.pow(attempt));
                    attempt += 1;
                    tracing::warn!(
                        "judge attempt {attempt} failed, retrying in {} ms: {error}",
                        wait.as_millis()
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        };
        let usage = wire.usage.unwrap_or_default();
        let input_tokens = usage
            .input_tokens
            .unwrap_or(request.estimated_tokens() as u64);
        Ok(JudgeResponse {
            provider: self.provider,
            model: wire.model.unwrap_or_else(|| self.model.to_string()),
            answers: wire.answers,
            input_tokens,
            cost_usd: usage
                .cost
                .unwrap_or(input_tokens as f64 * USD_PER_INPUT_TOKEN),
            latency_ms: started.elapsed().as_millis() as u64,
            requests: 1,
        })
    }
}

/// A parsed 200 body.
#[derive(Debug)]
struct WireResponse {
    model: Option<String>,
    answers: BTreeMap<String, Answer>,
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize, Default)]
struct WireUsage {
    #[serde(default, alias = "prompt_tokens")]
    input_tokens: Option<u64>,
    /// OpenRouter only: what the call was billed.
    #[serde(default)]
    cost: Option<f64>,
}

#[derive(Deserialize)]
struct WireBody {
    #[serde(default)]
    model: Option<String>,
    answers: BTreeMap<String, WireAnswer>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WireAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        #[serde(default)]
        confidence: f64,
        /// Keyed by level index as a string: "0".."n".
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
    },
}

fn parse_response(text: &str, provider: &'static str) -> Result<WireResponse, JudgeError> {
    let invalid = |message: String| JudgeError::InvalidResponse { provider, message };
    // Category and position only: the body holds answers about someone's
    // memory, and serde's Display can quote it.
    let body: WireBody = serde_json::from_str(text).map_err(|e| {
        invalid(format!(
            "decode failed: {} error at line {} column {} ({} bytes withheld)",
            crate::openrouter::classify_json_error(&e),
            e.line(),
            e.column(),
            text.len()
        ))
    })?;
    let mut answers = BTreeMap::new();
    for (id, a) in body.answers {
        let answer = match a {
            // A noul is P(yes) and is compared to a threshold as is: 1.3
            // would admit anything, -0.1 block anything. Out of range, the
            // body is not an answer; the caller falls back as for any bad
            // response.
            WireAnswer::Noul { noul } => {
                if !is_probability(noul) {
                    return Err(invalid(format!(
                        "noul `{id}` is not a probability in [0, 1]"
                    )));
                }
                Answer::Noul(noul as f32)
            }
            WireAnswer::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                if !probabilities.values().all(|p| is_probability(*p)) {
                    return Err(invalid(format!(
                        "choice `{id}` has a probability outside [0, 1]"
                    )));
                }
                Answer::Choice {
                    choice,
                    confidence: confidence as f32,
                    probabilities: probabilities
                        .into_iter()
                        .map(|(k, v)| (k, v as f32))
                        .collect(),
                }
            }
            WireAnswer::Score {
                score,
                confidence,
                probabilities,
            } => {
                let mut levels = Vec::new();
                for (k, p) in probabilities {
                    let i: usize = k
                        .parse()
                        .map_err(|_| invalid(format!("score `{id}` has a non-index level key")))?;
                    if i >= 10 {
                        return Err(invalid(format!("score `{id}` has level {i}")));
                    }
                    if !is_probability(p) {
                        return Err(invalid(format!(
                            "score `{id}` has a probability outside [0, 1]"
                        )));
                    }
                    if levels.len() <= i {
                        levels.resize(i + 1, 0.0);
                    }
                    levels[i] = p as f32;
                }
                Answer::Score {
                    value: score as f32,
                    confidence: confidence as f32,
                    probabilities: levels,
                }
            }
        };
        answers.insert(id, answer);
    }
    Ok(WireResponse {
        model: body.model,
        answers,
        usage: body.usage,
    })
}

/// NaN fails too.
fn is_probability(p: f64) -> bool {
    (0.0..=1.0).contains(&p)
}

/// The provider's diagnosis from the STRUCTURED error fields, never the raw
/// body. TypeSafe's `detail` is a union: `{error_type, message}`, a plain
/// string, or a pydantic list whose items carry `input` — the echoed request
/// — which is dropped. OpenRouter wraps its own errors as
/// `{"error": {"message": ..}}`.
fn error_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let message = match v.get("detail") {
        Some(Value::Object(d)) => {
            let message = d.get("message")?.as_str()?;
            match d.get("error_type").and_then(Value::as_str) {
                Some(kind) => format!("{kind}: {message}"),
                None => message.to_string(),
            }
        }
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let msg = item.get("msg")?.as_str()?;
                let loc = item.get("loc").and_then(Value::as_array).map(|parts| {
                    parts
                        .iter()
                        .map(|p| match p {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(".")
                });
                Some(match loc {
                    Some(loc) => format!("{loc}: {msg}"),
                    None => msg.to_string(),
                })
            })
            .collect::<Vec<_>>()
            .join("; "),
        _ => v.get("error")?.get("message")?.as_str()?.to_string(),
    };
    if message.trim().is_empty() {
        return None;
    }
    let head: String = message.chars().take(MAX_ERROR_CHARS).collect();
    Some(if head.len() == message.len() {
        head
    } else {
        format!("{head}… (truncated)")
    })
}

fn status_message(body: &str) -> String {
    error_message(body).unwrap_or_else(|| format!("provider body withheld ({} bytes)", body.len()))
}

/// `retry-after-ms`, else `retry-after` in seconds. HTTP-date values are
/// ignored (backoff applies).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs = |name: &str, scale: f64| {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| Duration::from_secs_f64(v / scale))
    };
    secs("retry-after-ms", 1000.0).or_else(|| secs("retry-after", 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::{questions, Question};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const KEY: &str = "sk-test-0123456789abcdef-SECRET";

    fn every_type() -> JudgeRequest {
        let mut req = JudgeRequest::new(json!({"passage": "p"}));
        req.push("n", Question::noul(json!("Is it?")));
        req.push(
            "c",
            Question::Choice {
                instructions: json!("Which?"),
                options: vec![("b".into(), None), ("a".into(), Some(json!("first")))],
            },
        );
        req.push(
            "s",
            Question::Score {
                instructions: json!("How?"),
                levels: vec![json!("low"), json!({"what": "high"})],
            },
        );
        req
    }

    #[test]
    fn request_body_snapshot_per_question_type() {
        let req = every_type();
        let body = serde_json::to_string(&WireRequest {
            model: DIRECT_MODEL,
            state: &req.state,
            questions: req.questions_wire(),
        })
        .unwrap();
        assert_eq!(
            body,
            r#"{"model":"jev-1.13.0","state":{"passage":"p"},"questions":{"n":{"type":"noul","instructions":"Is it?"},"c":{"type":"choice","instructions":"Which?","criteria":{"b":null,"a":"first"}},"s":{"type":"score","instructions":"How?","criteria":["low",{"what":"high"}]}}}"#
        );
    }

    #[test]
    fn routes_pin_their_models() {
        let d = TypeSafeJudge::new(TypeSafeRoute::Direct { key: KEY.into() });
        assert_eq!((d.name(), d.model_id()), ("typesafe", "jev-1.13.0"));
        assert_eq!(d.endpoint, DIRECT_ENDPOINT);
        let o = TypeSafeJudge::new(TypeSafeRoute::OpenRouter { key: KEY.into() });
        assert_eq!(
            (o.name(), o.model_id()),
            ("openrouter", "typesafe/jev-1.13")
        );
        assert_eq!(o.endpoint, OPENROUTER_ENDPOINT);
        for dbg in [
            format!("{d:?}"),
            format!("{:?}", TypeSafeRoute::Direct { key: KEY.into() }),
        ] {
            assert!(!dbg.contains(KEY), "{dbg}");
        }
    }

    #[test]
    fn parses_every_answer_type_with_usage() {
        let text = r#"{"model":"jev-1.13.0","answers":{
            "n":{"type":"noul","noul":0.93},
            "c":{"type":"choice","choice":"a","confidence":0.8,"probabilities":{"a":0.87,"b":0.13}},
            "s":{"type":"score","score":1.22,"confidence":0.82,"legend":{"0":"x","1":"y","2":"z"},"probabilities":{"0":0.0,"1":0.78,"2":0.22}}
        },"usage":{"input_tokens":381,"output_tokens":18}}"#;
        let wire = parse_response(text, "typesafe").unwrap();
        assert_eq!(wire.model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(wire.answers["n"], Answer::Noul(0.93));
        assert_eq!(
            wire.answers["c"],
            Answer::Choice {
                choice: "a".into(),
                confidence: 0.8,
                probabilities: BTreeMap::from([("a".into(), 0.87), ("b".into(), 0.13)]),
            }
        );
        assert_eq!(
            wire.answers["s"],
            Answer::Score {
                value: 1.22,
                confidence: 0.82,
                probabilities: vec![0.0, 0.78, 0.22],
            }
        );
        let usage = wire.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(381));
        assert_eq!(usage.cost, None);
    }

    #[test]
    fn openrouter_usage_cost_is_read() {
        let text = r#"{"model":"typesafe/jev-1.13","answers":{"n":{"type":"noul","noul":0.5}},
            "usage":{"input_tokens":333,"output_tokens":21,"cost":0.0000139}}"#;
        let usage = parse_response(text, "openrouter").unwrap().usage.unwrap();
        assert_eq!(usage.cost, Some(0.0000139));
    }

    #[test]
    fn a_probability_outside_0_1_is_an_invalid_response_never_an_answer() {
        let noul = |v: &str| format!(r#"{{"answers":{{"n":{{"type":"noul","noul":{v}}}}}}}"#);
        for bad in ["1.3", "-0.1", "1.0000001"] {
            match parse_response(&noul(bad), "typesafe") {
                Err(JudgeError::InvalidResponse { message, .. }) => {
                    assert!(
                        message.contains("`n`") && message.contains("[0, 1]"),
                        "{message}"
                    );
                    assert!(!message.contains(bad), "{message}");
                }
                Ok(_) => panic!("{bad} was accepted"),
                Err(other) => panic!("{bad}: {other}"),
            }
        }
        for ok in ["0", "1", "0.0", "1.0", "0.5"] {
            assert!(parse_response(&noul(ok), "typesafe").is_ok(), "{ok}");
        }
        let choice = r#"{"answers":{"c":{"type":"choice","choice":"a","confidence":0.9,"probabilities":{"a":1.4,"b":-0.4}}}}"#;
        assert!(matches!(
            parse_response(choice, "typesafe"),
            Err(JudgeError::InvalidResponse { .. })
        ));
        let score = r#"{"answers":{"s":{"type":"score","score":1.0,"confidence":0.5,"probabilities":{"0":-0.2,"1":1.2}}}}"#;
        assert!(matches!(
            parse_response(score, "typesafe"),
            Err(JudgeError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn a_decode_failure_never_quotes_the_body() {
        let text = r#"{"answers":{"n":{"type":"noul","noul":"Ana has HIV"}}}"#;
        let err = parse_response(text, "typesafe").unwrap_err().to_string();
        assert!(!err.contains("HIV"), "{err}");
        assert!(err.contains("schema"), "{err}");
    }

    #[test]
    fn the_error_detail_union_is_rendered_without_the_echo() {
        assert_eq!(
            error_message(
                r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: jev-1.13"}}"#
            )
            .unwrap(),
            "api_usage_error: Unknown model: jev-1.13"
        );
        assert_eq!(
            error_message(r#"{"detail":"Too many score levels. Must have at most 10 levels."}"#)
                .unwrap(),
            "Too many score levels. Must have at most 10 levels."
        );
        let echoed = r#"{"detail":[{"type":"missing","loc":["body","state"],"msg":"Field required",
            "input":{"model":"jev-1.13.0","questions":{"q":{"instructions":"Ana is HIV positive"}}}}]}"#;
        let msg = error_message(echoed).unwrap();
        assert_eq!(msg, "body.state: Field required");
        assert_eq!(
            error_message(r#"{"error":{"message":"No auth credentials found","code":401}}"#)
                .unwrap(),
            "No auth credentials found"
        );
        assert_eq!(
            status_message("<html>Ana is HIV positive</html>"),
            "provider body withheld (32 bytes)"
        );
        let long = format!(r#"{{"detail":"{}"}}"#, "é".repeat(500));
        assert!(error_message(&long).unwrap().ends_with("(truncated)"));
    }

    #[test]
    fn retry_after_prefers_ms_and_ignores_dates() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("retry-after", "2".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(2)));
        h.insert("retry-after-ms", "250".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_millis(250)));
        let mut d = reqwest::header::HeaderMap::new();
        d.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&d), None);
    }

    /// Serves `responses` in order, one connection each, recording requests.
    async fn serve(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).into_owned());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nretry-after-ms: 1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (url, seen)
    }

    fn judge_at(url: String) -> TypeSafeJudge {
        let mut j = TypeSafeJudge::new(TypeSafeRoute::Direct { key: KEY.into() });
        j.endpoint = url;
        j.base_backoff = Duration::from_millis(1);
        j
    }

    fn one_claim() -> JudgeRequest {
        questions::grounding_request(
            "Ana is HIV positive.",
            &[("c1".into(), "Ana diagnosed with HIV".into())],
        )
    }

    const OK: &str = r#"{"model":"jev-1.13.0","answers":{"c1":{"type":"noul","noul":0.97}},"usage":{"input_tokens":400}}"#;

    #[tokio::test]
    async fn retryable_statuses_are_retried_then_answered() {
        let (url, seen) = serve(vec![(503, "{}"), (529, "{}"), (200, OK)]).await;
        let resp = judge_at(url).ask(&one_claim()).await.unwrap();
        assert_eq!(resp.noul("c1").unwrap(), 0.97);
        assert_eq!(resp.input_tokens, 400);
        assert!((resp.cost_usd - 400.0 * USD_PER_INPUT_TOKEN).abs() < 1e-15);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].contains(&format!("authorization: Bearer {KEY}")));
        assert!(seen[0].contains(r#""model":"jev-1.13.0""#));
    }

    #[tokio::test]
    async fn retries_stop_after_two() {
        let (url, seen) = serve(vec![(503, "{}"), (503, "{}"), (503, "{}"), (200, OK)]).await;
        let err = judge_at(url).ask(&one_claim()).await.unwrap_err();
        assert!(matches!(err, JudgeError::Status { status: 503, .. }));
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried_and_leaks_nothing() {
        let echo = r#"{"detail":[{"type":"missing","loc":["body","state"],"msg":"Field required","input":{"state":"Ana is HIV positive."}}]}"#;
        let (url, seen) = serve(vec![(422, echo), (200, OK)]).await;
        let err = judge_at(url).ask(&one_claim()).await.unwrap_err();
        assert_eq!(seen.lock().unwrap().len(), 1);
        let text = err.to_string();
        assert!(
            text.contains("422") && text.contains("Field required"),
            "{text}"
        );
        assert!(!text.contains("HIV"), "{text}");
        assert!(!text.contains(KEY), "{text}");
    }

    #[tokio::test]
    async fn an_unreachable_host_is_a_transport_error_without_the_key() {
        // Bind then drop: the port is closed.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let err = judge_at(format!("http://127.0.0.1:{port}/v1/systemone"))
            .ask(&one_claim())
            .await
            .unwrap_err();
        assert!(matches!(err, JudgeError::Transport { .. }), "{err}");
        let text = err.to_string();
        assert!(!text.contains(KEY) && !text.contains("127.0.0.1"), "{text}");
    }
}
