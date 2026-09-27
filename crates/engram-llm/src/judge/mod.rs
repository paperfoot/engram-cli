//! Calibrated judge: typed questions about one `state`, probabilities back.
//!
//! A judge is a decision model, not a text generator. One request carries one
//! `state` (the passage, the fact pair) and every question about it; each
//! question is a `Noul` (P(yes)), a `Choice` (distribution over named options)
//! or a `Score` (distribution over 2-10 ordered levels). Batching every
//! question about one state into ONE request is the whole cost model: the
//! state is ingested once and the questions run in parallel server-side.
//!
//! The seam later code writes against:
//! - [`Judge`] — the provider trait (TypeSafe direct or via OpenRouter, and
//!   the offline [`stub::StubJudge`]).
//! - [`questions`] — the ONE module holding every question text, criterion and
//!   threshold. Nothing else in engram writes question wording.
//! - [`ask_batched`] — splits an oversized request, runs the parts with
//!   bounded concurrency and merges the answers.
//! - [`JudgeResponse::noul`] — a missing or mistyped answer is an error for
//!   that id, never a default value.
//! - [`lexical`] — the deterministic grounding rules (numbers, word overlap)
//!   the judge is compared against and falls back to.
//!
//! ```no_run
//! # async fn demo() -> Result<(), engram_llm::judge::JudgeError> {
//! use engram_llm::judge::{ask_batched, questions, stub::StubJudge};
//!
//! let judge = StubJudge::parse("0.9").expect("valid stub spec");
//! let claim = questions::fact_as_claim("Ana", "lives_in", "Lisbon");
//! let request = questions::grounding_request(
//!     "Ana moved to Lisbon in March.",
//!     &[("fact-1".to_string(), claim)],
//! );
//! let response = ask_batched(&judge, &request).await?;
//! match questions::grounding_verdict(response.noul("fact-1")?) {
//!     questions::Grounding::Supported => { /* admit */ }
//!     questions::Grounding::Uncertain => { /* keep today's behaviour, log it */ }
//!     questions::Grounding::Unsupported => { /* reject */ }
//! }
//! # Ok(()) }
//! ```

pub mod lexical;
pub mod questions;
pub mod stub;
pub mod typesafe;

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

/// TypeSafe JEV price per input token (output tokens are free). Used when the
/// provider does not report a cost itself.
pub const USD_PER_INPUT_TOKEN: f64 = 0.042e-6;

/// At most this many questions go into one request. The API documents no
/// count limit; 61 questions were probed fine. 64 keeps a part small enough
/// that one failed request loses little.
pub const MAX_QUESTIONS_PER_REQUEST: usize = 64;

/// Estimated tokens (state + questions) one request may carry. The model's
/// limit is 64k total and 32k for state + the longest question; accuracy also
/// falls as the state grows, so the split happens well before either.
pub const MAX_REQUEST_TOKENS: usize = 24_000;

/// Parts of one split request in flight at once. TypeSafe's cookbooks keep
/// pools small (4 to 12 workers) because the public endpoint rate-limits
/// above roughly eight concurrent requests. This pool multiplies with any
/// the caller runs (the eval harness runs eight requests at once), so it
/// takes the low end.
pub const BATCH_CONCURRENCY: usize = 4;

/// Choice and Score bounds the API enforces (it reports only the first
/// violation, sometimes vaguely, so they are checked before sending).
const MAX_CHOICE_OPTIONS: usize = 255;
const SCORE_LEVELS: std::ops::RangeInclusive<usize> = 2..=10;

/// One question. `instructions` may be a string or an object; object-form
/// instructions carry the item being judged (a claim, an existing fact) next
/// to the question so it is never referenced by array index.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// P(yes). `criteria` says what a yes and a no mean when the boundary is
    /// subtle; it must extend the instruction, never invert it.
    Noul {
        instructions: Value,
        criteria: Option<NoulCriteria>,
    },
    /// One of named options, in a fixed order (order moves answers). An
    /// option's description is `None` when its name says it all.
    Choice {
        instructions: Value,
        options: Vec<(String, Option<Value>)>,
    },
    /// Ordered levels, lowest first, each describing a situation.
    Score {
        instructions: Value,
        levels: Vec<Value>,
    },
}

/// What a yes (value near 1) and a no (value near 0) mean for a Noul.
#[derive(Debug, Clone, PartialEq)]
pub struct NoulCriteria {
    pub yes: Value,
    pub no: Value,
}

impl Question {
    pub fn noul(instructions: impl Into<Value>) -> Self {
        Question::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    pub fn noul_with_criteria(
        instructions: impl Into<Value>,
        yes: impl Into<Value>,
        no: impl Into<Value>,
    ) -> Self {
        Question::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                yes: yes.into(),
                no: no.into(),
            }),
        }
    }

    pub fn instructions(&self) -> &Value {
        match self {
            Question::Noul { instructions, .. }
            | Question::Choice { instructions, .. }
            | Question::Score { instructions, .. } => instructions,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Question::Noul { .. } => "noul",
            Question::Choice { .. } => "choice",
            Question::Score { .. } => "score",
        }
    }
}

/// Wire shape: `{"type": .., "instructions": .., "criteria": ..}`, with Choice
/// options serialized in the order given (serde_json maps would sort them).
impl Serialize for Question {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Options<'a>(&'a [(String, Option<Value>)]);
        impl Serialize for Options<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for (name, description) in self.0 {
                    map.serialize_entry(name, description)?;
                }
                map.end()
            }
        }
        struct Criteria<'a>(&'a NoulCriteria);
        impl Serialize for Criteria<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("true", &self.0.yes)?;
                map.serialize_entry("false", &self.0.no)?;
                map.end()
            }
        }

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", self.kind())?;
        map.serialize_entry("instructions", self.instructions())?;
        match self {
            Question::Noul { criteria, .. } => {
                if let Some(c) = criteria {
                    map.serialize_entry("criteria", &Criteria(c))?;
                }
            }
            Question::Choice { options, .. } => {
                map.serialize_entry("criteria", &Options(options))?;
            }
            Question::Score { levels, .. } => {
                map.serialize_entry("criteria", levels)?;
            }
        }
        map.end()
    }
}

/// One `state` and every question about it, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeRequest {
    pub state: Value,
    pub questions: Vec<(String, Question)>,
}

impl JudgeRequest {
    pub fn new(state: Value) -> Self {
        Self {
            state,
            questions: Vec::new(),
        }
    }

    pub fn push(&mut self, id: impl Into<String>, question: Question) {
        self.questions.push((id.into(), question));
    }

    /// The `questions` object as sent: ids in request order.
    pub fn questions_wire(&self) -> impl Serialize + '_ {
        QuestionMap(&self.questions)
    }

    /// chars/4 over the serialized state and questions: the budget unit for
    /// splitting, not a billing number (the provider reports that).
    pub fn estimated_tokens(&self) -> usize {
        estimate_tokens(&self.state)
            + self
                .questions
                .iter()
                .map(|(id, q)| question_tokens(id, q))
                .sum::<usize>()
    }

    /// The checks the API would fail on, made before a paid round trip.
    pub fn validate(&self) -> Result<(), JudgeError> {
        if self.questions.is_empty() {
            return Err(JudgeError::InvalidRequest("no questions".into()));
        }
        let mut seen = HashSet::new();
        for (id, q) in &self.questions {
            if id.is_empty() {
                return Err(JudgeError::InvalidRequest("empty question id".into()));
            }
            if !seen.insert(id.as_str()) {
                return Err(JudgeError::InvalidRequest(format!(
                    "duplicate question id `{id}`"
                )));
            }
            match q {
                Question::Noul { .. } => {}
                Question::Choice { options, .. } => {
                    if options.is_empty() || options.len() > MAX_CHOICE_OPTIONS {
                        return Err(JudgeError::InvalidRequest(format!(
                            "choice `{id}` has {} options; 1..={MAX_CHOICE_OPTIONS} allowed",
                            options.len()
                        )));
                    }
                    let mut names = HashSet::new();
                    if !options.iter().all(|(name, _)| names.insert(name.as_str())) {
                        return Err(JudgeError::InvalidRequest(format!(
                            "choice `{id}` repeats an option name"
                        )));
                    }
                }
                Question::Score { levels, .. } => {
                    if !SCORE_LEVELS.contains(&levels.len()) {
                        return Err(JudgeError::InvalidRequest(format!(
                            "score `{id}` has {} levels; 2..=10 allowed",
                            levels.len()
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

struct QuestionMap<'a>(&'a [(String, Question)]);

impl Serialize for QuestionMap<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (id, q) in self.0 {
            map.serialize_entry(id, q)?;
        }
        map.end()
    }
}

fn estimate_tokens(value: &impl Serialize) -> usize {
    serde_json::to_string(value)
        .map(|s| s.chars().count() / 4)
        .unwrap_or(0)
}

fn question_tokens(id: &str, q: &Question) -> usize {
    id.chars().count() / 4 + estimate_tokens(q)
}

/// One answer, by question type.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    /// P(yes), two decimals on the wire.
    Noul(f32),
    /// The argmax option, the provider's confidence statistic, and the full
    /// distribution (sums to ~1; a Choice always picks something).
    Choice {
        choice: String,
        confidence: f32,
        probabilities: BTreeMap<String, f32>,
    },
    /// Expected level index (sum of index x p), the provider's confidence
    /// (its formula past three levels is undocumented: read it, never
    /// recompute it) and P(level), lowest first.
    Score {
        value: f32,
        confidence: f32,
        probabilities: Vec<f32>,
    },
}

impl Answer {
    fn kind(&self) -> &'static str {
        match self {
            Answer::Noul(_) => "noul",
            Answer::Choice { .. } => "choice",
            Answer::Score { .. } => "score",
        }
    }
}

/// Answers to one request (or one merged batch).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeResponse {
    /// Who answered: "typesafe", "openrouter" or "stub" (usage ledger).
    pub provider: &'static str,
    /// The versioned model id as the provider returned it. Thresholds are
    /// fitted per model version, so store it next to every verdict.
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    /// Billed input tokens; chars/4 of the request when not reported.
    pub input_tokens: u64,
    /// The provider's reported cost, else `input_tokens` x the list price.
    pub cost_usd: f64,
    /// Client wall time, retries included.
    pub latency_ms: u64,
    /// HTTP requests this response covers (a split batch has several).
    pub requests: u32,
}

impl JudgeResponse {
    pub fn answer(&self, id: &str) -> Result<&Answer, JudgeError> {
        self.answers
            .get(id)
            .ok_or_else(|| JudgeError::MissingAnswer { id: id.to_string() })
    }

    /// The Noul answer for `id`; missing or another type is an error.
    pub fn noul(&self, id: &str) -> Result<f32, JudgeError> {
        match self.answer(id)? {
            Answer::Noul(p) => Ok(*p),
            other => Err(JudgeError::WrongAnswerType {
                id: id.to_string(),
                expected: "noul",
                actual: other.kind(),
            }),
        }
    }
}

/// An answer as the decimal the provider sent, for anything that writes it
/// out. Widening the f32 directly writes 0.9 as 0.8999999761581543, which
/// falls below a 0.9 threshold for whoever calibrates on the output; the
/// shortest f32 repr round-trips the wire value.
pub fn wire_decimal(p: f32) -> f64 {
    p.to_string().parse().unwrap_or(p as f64)
}

/// Judge failures. None of them carries the API key or the request body: the
/// state is someone's memory, and error strings reach agents' stdout.
#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("invalid judge request: {0}")]
    InvalidRequest(String),

    #[error("{provider} judge unreachable: {message}")]
    Transport {
        provider: &'static str,
        message: String,
    },

    #[error("{provider} judge returned status {status}: {message}")]
    Status {
        provider: &'static str,
        status: u16,
        message: String,
    },

    #[error("{provider} judge response unreadable: {message}")]
    InvalidResponse {
        provider: &'static str,
        message: String,
    },

    #[error("judge gave no answer for question `{id}`")]
    MissingAnswer { id: String },

    #[error("judge answer for question `{id}` is a {actual}, expected a {expected}")]
    WrongAnswerType {
        id: String,
        expected: &'static str,
        actual: &'static str,
    },
}

impl JudgeError {
    /// Worth retrying: transport failures and 408/429/5xx (529 included).
    pub fn is_retryable(&self) -> bool {
        match self {
            JudgeError::Transport { .. } => true,
            JudgeError::Status { status, .. } => is_retryable_status(*status),
            _ => false,
        }
    }
}

/// 400/401/403/404/422 are the caller's fault and fail the same way again.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429) || (500..=599).contains(&status)
}

#[async_trait]
pub trait Judge: Send + Sync {
    /// Provider label for the usage ledger: "typesafe" | "openrouter" | "stub".
    fn name(&self) -> &'static str;
    /// The model id requested (the response carries the one that answered).
    fn model_id(&self) -> &str;
    /// One request, one round trip (plus retries). Callers with an unbounded
    /// question list use [`ask_batched`].
    async fn ask(&self, request: &JudgeRequest) -> Result<JudgeResponse, JudgeError>;
}

/// Split `request` into parts of at most [`MAX_QUESTIONS_PER_REQUEST`]
/// questions and about [`MAX_REQUEST_TOKENS`] estimated tokens each, every
/// part carrying the full state. A part always holds at least one question,
/// so an oversized state still yields one request per question rather than
/// none.
pub fn split_request(request: &JudgeRequest) -> Vec<JudgeRequest> {
    let state_tokens = estimate_tokens(&request.state);
    let mut parts = Vec::new();
    let mut current = JudgeRequest::new(request.state.clone());
    let mut current_tokens = state_tokens;
    for (id, q) in &request.questions {
        let tokens = question_tokens(id, q);
        let full = current.questions.len() >= MAX_QUESTIONS_PER_REQUEST
            || current_tokens + tokens > MAX_REQUEST_TOKENS;
        if full && !current.questions.is_empty() {
            parts.push(std::mem::replace(
                &mut current,
                JudgeRequest::new(request.state.clone()),
            ));
            current_tokens = state_tokens;
        }
        current.questions.push((id.clone(), q.clone()));
        current_tokens += tokens;
    }
    if !current.questions.is_empty() {
        parts.push(current);
    }
    parts
}

/// Ask everything in `request`, split into bounded parts run
/// [`BATCH_CONCURRENCY`] at a time, answers merged.
///
/// A part that fails leaves its ids unanswered — [`JudgeResponse::noul`] then
/// errors for exactly those ids — and the parts that succeeded keep their
/// answers and their cost. Only when no part succeeds is the whole call an
/// error (the first part's).
pub async fn ask_batched(
    judge: &dyn Judge,
    request: &JudgeRequest,
) -> Result<JudgeResponse, JudgeError> {
    request.validate()?;
    let parts = split_request(request);
    if parts.len() == 1 {
        return judge.ask(&parts[0]).await;
    }
    let started = Instant::now();
    let results: Vec<Result<JudgeResponse, JudgeError>> = stream::iter(parts.iter())
        .map(|part| judge.ask(part))
        .buffered(BATCH_CONCURRENCY)
        .collect()
        .await;
    let mut merged: Option<JudgeResponse> = None;
    let mut first_error = None;
    for (part, result) in parts.iter().zip(results) {
        match result {
            Ok(resp) => match merged.as_mut() {
                None => merged = Some(resp),
                Some(m) => {
                    m.answers.extend(resp.answers);
                    m.input_tokens += resp.input_tokens;
                    m.cost_usd += resp.cost_usd;
                    m.requests += resp.requests;
                }
            },
            Err(e) => {
                tracing::warn!(
                    questions = part.questions.len(),
                    "judge batch part failed, its questions stay unanswered: {e}"
                );
                first_error.get_or_insert(e);
            }
        }
    }
    match merged {
        Some(mut m) => {
            m.latency_ms = started.elapsed().as_millis() as u64;
            Ok(m)
        }
        None => Err(first_error.expect("at least one part ran")),
    }
}

/// [`ask_batched`] over many independent requests, `concurrency` at a time,
/// results in input order.
pub async fn ask_many(
    judge: &dyn Judge,
    requests: &[JudgeRequest],
    concurrency: usize,
) -> Vec<Result<JudgeResponse, JudgeError>> {
    stream::iter(requests)
        .map(|r| ask_batched(judge, r))
        .buffered(concurrency.max(1))
        .collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn noul(text: &str) -> Question {
        Question::noul(json!(text))
    }

    #[test]
    fn a_written_answer_is_the_providers_decimal() {
        for (p, text) in [
            (0.9f32, "0.9"),
            (0.01, "0.01"),
            (0.97, "0.97"),
            (1.0, "1.0"),
        ] {
            assert_eq!(json!({ "p": wire_decimal(p) })["p"].to_string(), text);
        }
        assert_ne!(json!(f64::from(0.9f32)).to_string(), "0.9");
    }

    #[test]
    fn questions_serialize_in_request_order_with_typed_criteria() {
        let mut req = JudgeRequest::new(json!({"passage": "p"}));
        req.push("z_last_alphabetically", noul("first?"));
        req.push(
            "a_choice",
            Question::Choice {
                instructions: json!("Which?"),
                options: vec![
                    ("zebra".into(), Some(json!("striped"))),
                    ("apple".into(), None),
                ],
            },
        );
        req.push(
            "m_score",
            Question::Score {
                instructions: json!("How much?"),
                levels: vec![json!("none"), json!("all")],
            },
        );
        req.push(
            "b_criteria",
            Question::noul_with_criteria(json!("Yes?"), "it is", "it is not"),
        );
        // A string comparison: `Value == Value` ignores key order.
        assert_eq!(
            serde_json::to_string(&req.questions_wire()).unwrap(),
            r#"{"z_last_alphabetically":{"type":"noul","instructions":"first?"},"a_choice":{"type":"choice","instructions":"Which?","criteria":{"zebra":"striped","apple":null}},"m_score":{"type":"score","instructions":"How much?","criteria":["none","all"]},"b_criteria":{"type":"noul","instructions":"Yes?","criteria":{"true":"it is","false":"it is not"}}}"#
        );
    }

    #[test]
    fn validation_catches_what_the_api_would_reject() {
        let empty = JudgeRequest::new(json!("s"));
        assert!(matches!(
            empty.validate(),
            Err(JudgeError::InvalidRequest(_))
        ));

        let mut dup = JudgeRequest::new(json!("s"));
        dup.push("a", noul("q"));
        dup.push("a", noul("q"));
        assert!(dup.validate().is_err());

        let mut one_level = JudgeRequest::new(json!("s"));
        one_level.push(
            "s",
            Question::Score {
                instructions: json!("q"),
                levels: vec![json!("only")],
            },
        );
        assert!(one_level.validate().is_err());

        let mut no_options = JudgeRequest::new(json!("s"));
        no_options.push(
            "c",
            Question::Choice {
                instructions: json!("q"),
                options: vec![],
            },
        );
        assert!(no_options.validate().is_err());

        let mut ok = JudgeRequest::new(json!("s"));
        ok.push("a", noul("q"));
        ok.validate().unwrap();
    }

    #[test]
    fn split_caps_question_count() {
        let mut req = JudgeRequest::new(json!({"passage": "short"}));
        for i in 0..150 {
            req.push(format!("q{i}"), noul("Is it?"));
        }
        let parts = split_request(&req);
        let sizes: Vec<usize> = parts.iter().map(|p| p.questions.len()).collect();
        assert_eq!(sizes, vec![64, 64, 22]);
        assert!(parts.iter().all(|p| p.state == req.state));
        let ids: Vec<&str> = parts
            .iter()
            .flat_map(|p| p.questions.iter().map(|(id, _)| id.as_str()))
            .collect();
        assert_eq!(ids.len(), 150);
        assert_eq!(ids[0], "q0");
        assert_eq!(ids[149], "q149");
    }

    #[test]
    fn split_caps_estimated_tokens() {
        let mut req = JudgeRequest::new(json!({"passage": "p"}));
        // ~2.5k tokens per question -> 9 fit under 24k.
        let long = "x".repeat(10_000);
        for i in 0..20 {
            req.push(format!("q{i}"), noul(&long));
        }
        let parts = split_request(&req);
        assert!(parts.len() >= 3);
        assert!(parts
            .iter()
            .all(|p| p.estimated_tokens() <= MAX_REQUEST_TOKENS));
    }

    #[test]
    fn oversized_state_still_sends_one_question_per_part() {
        let mut req = JudgeRequest::new(json!({"passage": "y".repeat(200_000)}));
        req.push("a", noul("q"));
        req.push("b", noul("q"));
        let parts = split_request(&req);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.questions.len() == 1));
    }

    #[test]
    fn missing_or_mistyped_answers_are_errors_not_defaults() {
        let resp = JudgeResponse {
            provider: "stub",
            model: "stub".into(),
            answers: BTreeMap::from([(
                "s".to_string(),
                Answer::Score {
                    value: 1.0,
                    confidence: 1.0,
                    probabilities: vec![0.0, 1.0],
                },
            )]),
            input_tokens: 0,
            cost_usd: 0.0,
            latency_ms: 0,
            requests: 1,
        };
        assert!(matches!(
            resp.noul("absent"),
            Err(JudgeError::MissingAnswer { .. })
        ));
        assert!(matches!(
            resp.noul("s"),
            Err(JudgeError::WrongAnswerType { .. })
        ));
    }

    #[test]
    fn retry_classification_follows_the_api() {
        for s in [408, 429, 500, 502, 503, 529] {
            assert!(is_retryable_status(s), "{s}");
        }
        for s in [400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(s), "{s}");
        }
    }

    /// Answers every noul with 0.5, except that it fails any request whose
    /// first question id is in `fail`.
    struct Scripted {
        fail: Vec<&'static str>,
        calls: AtomicU32,
    }

    #[async_trait]
    impl Judge for Scripted {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn model_id(&self) -> &str {
            "scripted"
        }
        async fn ask(&self, request: &JudgeRequest) -> Result<JudgeResponse, JudgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.contains(&request.questions[0].0.as_str()) {
                return Err(JudgeError::Status {
                    provider: "stub",
                    status: 503,
                    message: "down".into(),
                });
            }
            Ok(JudgeResponse {
                provider: "stub",
                model: "scripted".into(),
                answers: request
                    .questions
                    .iter()
                    .map(|(id, _)| (id.clone(), Answer::Noul(0.5)))
                    .collect(),
                input_tokens: 10,
                cost_usd: 0.001,
                latency_ms: 1,
                requests: 1,
            })
        }
    }

    fn many(n: usize) -> JudgeRequest {
        let mut req = JudgeRequest::new(json!({"passage": "p"}));
        for i in 0..n {
            req.push(format!("q{i}"), noul("Is it?"));
        }
        req
    }

    #[tokio::test]
    async fn batched_parts_merge_answers_and_usage() {
        let judge = Scripted {
            fail: vec![],
            calls: AtomicU32::new(0),
        };
        let resp = ask_batched(&judge, &many(130)).await.unwrap();
        assert_eq!(judge.calls.load(Ordering::SeqCst), 3);
        assert_eq!(resp.answers.len(), 130);
        assert_eq!(resp.requests, 3);
        assert_eq!(resp.input_tokens, 30);
        assert!((resp.cost_usd - 0.003).abs() < 1e-12);
    }

    #[tokio::test]
    async fn a_failed_part_leaves_only_its_ids_unanswered() {
        // Parts start at q0, q64, q128.
        let judge = Scripted {
            fail: vec!["q64"],
            calls: AtomicU32::new(0),
        };
        let resp = ask_batched(&judge, &many(130)).await.unwrap();
        assert_eq!(resp.answers.len(), 66);
        assert_eq!(resp.noul("q0").unwrap(), 0.5);
        assert!(matches!(
            resp.noul("q64"),
            Err(JudgeError::MissingAnswer { .. })
        ));
        assert_eq!(resp.noul("q129").unwrap(), 0.5);
    }

    #[tokio::test]
    async fn every_part_failing_is_an_error() {
        let judge = Scripted {
            fail: vec!["q0", "q64"],
            calls: AtomicU32::new(0),
        };
        let err = ask_batched(&judge, &many(100)).await.unwrap_err();
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn ask_many_keeps_input_order() {
        let judge = Scripted {
            fail: vec!["b0"],
            calls: AtomicU32::new(0),
        };
        let mut a = JudgeRequest::new(json!("a"));
        a.push("a0", noul("q"));
        let mut b = JudgeRequest::new(json!("b"));
        b.push("b0", noul("q"));
        let out = ask_many(&judge, &[a, b], 8).await;
        assert!(out[0].as_ref().unwrap().answers.contains_key("a0"));
        assert!(out[1].is_err());
    }
}
