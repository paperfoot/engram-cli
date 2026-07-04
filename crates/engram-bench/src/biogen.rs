//! TREC BioGen long-form biomedical generation track.
//!
//! Task & dataset: "Overview of the TREC 2024 Biomedical Generative Retrieval
//! (BioGen) Track" (arXiv:2411.18069). Each topic is a biomedical question; a
//! system answers it in free text where every sentence cites the PubMed
//! documents that support it. The headline metric is the **Citation Support
//! Rate (CSR)**: the fraction of answer sentences whose cited evidence actually
//! entails the sentence.
//!
//! This module provides two pieces:
//!
//! 1. [`BioGenTopics`] — a loader for the topic file (`load_from_file` +
//!    [`default_path`]).
//! 2. [`citation_support_rate`] — the CSR scorer. It reuses
//!    [`crate::citation::split_sentences`] for sentence segmentation and the
//!    same generic async entailment closure φ used by [`crate::citation`], so
//!    the metric stays dependency-free and deterministically unit-testable with
//!    a stub φ (no network, no model, no API keys).
//!
//! The live answerer wiring (retrieve → generate cited answer → score) is added
//! by the Integration/CLI phase; here we only define the offline-testable core.

use crate::error::BenchError;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};

/// Per the BioGen track rules, a sentence may carry at most this many citations.
pub const MAX_CITATIONS_PER_SENTENCE: usize = 3;

/// Per the BioGen track rules, an answer may cite at most this many distinct
/// documents in total.
pub const MAX_DOCS_PER_ANSWER: usize = 30;

/// A single BioGen topic (one biomedical question).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BioGenTopic {
    pub id: String,
    pub question: String,
}

/// The full set of BioGen topics.
pub struct BioGenTopics(pub Vec<BioGenTopic>);

impl BioGenTopics {
    /// Load topics from the official topic file.
    ///
    /// The TREC BioGen release ships JSON that is either a top-level array or an
    /// object wrapping the array under `topics` (some mirrors use `questions`).
    /// Each entry identifies the topic with `id` (or `topic_id` / `number`) and
    /// carries the question text under `question` (or `text` / `narrative`). We
    /// accept all of these spellings so the loader survives the format drift
    /// across yearly releases.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, BenchError> {
        let bytes = std::fs::read(path.as_ref())?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;

        let arr = match v {
            serde_json::Value::Array(arr) => arr,
            serde_json::Value::Object(mut obj) => obj
                .remove("topics")
                .or_else(|| obj.remove("questions"))
                .and_then(|t| match t {
                    serde_json::Value::Array(arr) => Some(arr),
                    _ => None,
                })
                .ok_or_else(|| {
                    BenchError::InvalidDataset(
                        "expected top-level array or { topics: [...] }".into(),
                    )
                })?,
            _ => {
                return Err(BenchError::InvalidDataset(
                    "unexpected top-level JSON value".into(),
                ))
            }
        };

        let mut topics = Vec::with_capacity(arr.len());
        for entry in &arr {
            let obj = entry.as_object().ok_or_else(|| {
                BenchError::InvalidDataset("topic entry is not an object".into())
            })?;
            let id = obj
                .get("id")
                .or_else(|| obj.get("topic_id"))
                .or_else(|| obj.get("number"))
                .map(value_to_id)
                .ok_or_else(|| {
                    BenchError::InvalidDataset("topic entry missing id".into())
                })?;
            let question = obj
                .get("question")
                .or_else(|| obj.get("text"))
                .or_else(|| obj.get("narrative"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    BenchError::InvalidDataset(format!("topic {id} missing question text"))
                })?
                .trim()
                .to_string();
            topics.push(BioGenTopic { id, question });
        }

        if topics.is_empty() {
            return Err(BenchError::InvalidDataset("0 topics".into()));
        }
        Ok(Self(topics))
    }
}

/// Coerce a JSON id (string or number) into a `String`.
fn value_to_id(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Default on-disk path for the BioGen topic file.
pub fn default_path() -> PathBuf {
    PathBuf::from("data/biogen/topics.json")
}

/// Aggregate BioGen score over one or more answered topics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BioGenReport {
    /// Citation Support Rate: supported sentences / total sentences.
    pub csr: f32,
    /// Total number of sentences scored (the CSR denominator).
    pub n_sentences: usize,
    /// Number of topics that contributed to the score.
    pub n_topics: usize,
}

impl Default for BioGenReport {
    fn default() -> Self {
        BioGenReport {
            csr: 0.0,
            n_sentences: 0,
            n_topics: 0,
        }
    }
}

/// Extract the inline citation ids carried by `sentence`.
///
/// Citations are written as bracketed markers — `[id]` or a comma/semicolon
/// separated group `[id1, id2]` — following the convention used by the BioGen
/// reference answers. Ids are returned in first-seen order with duplicates
/// removed, then truncated to [`MAX_CITATIONS_PER_SENTENCE`] (the track caps the
/// number of citations a single sentence may claim).
pub fn extract_citation_ids(sentence: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut rest = sentence;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        let inner = &after[..close];
        for part in inner.split([',', ';']) {
            let id = part.trim();
            if !id.is_empty() && !ids.iter().any(|x| x == id) {
                ids.push(id.to_string());
            }
        }
        rest = &after[close + 1..];
    }
    ids.truncate(MAX_CITATIONS_PER_SENTENCE);
    ids
}

/// Compute the Citation Support Rate for a single generated `answer`.
///
/// `chunks` maps a citation id to the text of the document/chunk it refers to.
/// `entails(premise, hypothesis)` is the NLI judge φ: it returns `true` iff
/// `premise` entails `hypothesis`. It is the same closure shape used by
/// [`crate::citation::citation_scores`], so a single φ implementation drives
/// both metrics.
///
/// Procedure (mirrors the BioGen track):
/// - Segment `answer` with [`crate::citation::split_sentences`].
/// - For each sentence, read its inline citation ids (capped at
///   [`MAX_CITATIONS_PER_SENTENCE`]), drop ids absent from `chunks`, and honour
///   the per-answer [`MAX_DOCS_PER_ANSWER`] budget on *distinct* cited docs
///   (citations beyond the 30th distinct doc are ignored).
/// - Concatenate the surviving cited texts into a premise and ask φ whether it
///   entails the sentence. A sentence with no usable citation is unsupported.
///
/// Returns a [`BioGenReport`] with `n_topics == 1` (single answer). Use
/// [`aggregate`] to combine per-topic reports.
pub async fn citation_support_rate<F, Fut>(
    entails: F,
    answer: &str,
    chunks: &HashMap<String, String>,
) -> Result<BioGenReport, BenchError>
where
    F: Fn(String, String) -> Fut + Copy,
    Fut: Future<Output = Result<bool, BenchError>>,
{
    let sentences = crate::citation::split_sentences(answer);
    let n_sentences = sentences.len();
    if n_sentences == 0 {
        return Ok(BioGenReport {
            csr: 0.0,
            n_sentences: 0,
            n_topics: 1,
        });
    }

    // Track distinct docs cited across the whole answer to enforce the 30-doc
    // budget. Ids admitted earlier in the answer keep their slot.
    let mut admitted: Vec<String> = Vec::new();
    let mut supported = 0usize;

    for sentence in &sentences {
        let mut premise_parts: Vec<String> = Vec::new();
        for id in extract_citation_ids(sentence) {
            let Some(text) = chunks.get(&id) else {
                continue; // citation refers to a doc we don't have — skip it.
            };
            let already = admitted.contains(&id);
            if !already {
                if admitted.len() >= MAX_DOCS_PER_ANSWER {
                    continue; // answer-level doc budget exhausted.
                }
                admitted.push(id.clone());
            }
            premise_parts.push(text.clone());
        }

        let entailed = if premise_parts.is_empty() {
            false
        } else {
            entails(premise_parts.join("\n"), sentence.clone()).await?
        };
        if entailed {
            supported += 1;
        }
    }

    Ok(BioGenReport {
        csr: supported as f32 / n_sentences as f32,
        n_sentences,
        n_topics: 1,
    })
}

/// Combine per-topic reports into a corpus-level report.
///
/// CSR is micro-averaged: the total number of supported sentences divided by
/// the total number of sentences across all topics (so a long answer counts for
/// proportionally more than a short one, matching how the track reports CSR over
/// the full sentence pool). Topics that produced zero sentences still count
/// toward `n_topics`.
pub fn aggregate(reports: &[BioGenReport]) -> BioGenReport {
    let n_topics = reports.len();
    let n_sentences: usize = reports.iter().map(|r| r.n_sentences).sum();
    // Recover the supported count per report from its rate (csr * n_sentences
    // is an exact integer because csr was formed as supported / n_sentences).
    let supported: f32 = reports.iter().map(|r| r.csr * r.n_sentences as f32).sum();
    let csr = if n_sentences == 0 {
        0.0
    } else {
        supported / n_sentences as f32
    };
    BioGenReport {
        csr,
        n_sentences,
        n_topics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic stub φ: `premise` entails `hypothesis` iff every lowercase
    /// whitespace token of the hypothesis (minus bracketed citation markers and
    /// trailing punctuation) appears in the premise's token set. No network,
    /// fully reproducible. Mirrors the stub used in `crate::citation` tests.
    async fn stub_entails(premise: String, hypothesis: String) -> Result<bool, BenchError> {
        let prem: std::collections::HashSet<String> = premise
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        // Drop bracketed citation markers from the hypothesis before tokenising
        // so "[1]" doesn't have to appear in the premise.
        let mut hyp = String::new();
        let mut depth = 0u32;
        for c in hypothesis.to_lowercase().chars() {
            match c {
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                _ if depth == 0 => hyp.push(c),
                _ => {}
            }
        }
        hyp.retain(|c| !matches!(c, '.' | '!' | '?' | ',' | ';' | ':'));
        let ok = hyp.split_whitespace().all(|tok| prem.contains(tok));
        Ok(ok)
    }

    fn chunks(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Minimal single-threaded executor so the async metric can run without a
    /// runtime dependency. The stub φ never yields `Pending`, so a busy-poll
    /// with a no-op waker is sufficient and deterministic. (Same helper as in
    /// `crate::citation` tests.)
    fn block_on<T>(fut: impl Future<Output = T>) -> T {
        use std::pin::pin;
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(std::ptr::null(), &VTABLE),
            |_| {},
            |_| {},
            |_| {},
        );
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => continue,
            }
        }
    }

    // ---- topic loader ----

    #[test]
    fn load_topics_top_level_array() {
        let dir = std::env::temp_dir();
        let path = dir.join("biogen_topics_array.json");
        std::fs::write(
            &path,
            r#"[
                {"id": "1", "question": "What causes scurvy?"},
                {"topic_id": "2", "text": "How does aspirin work?  "}
            ]"#,
        )
        .unwrap();
        let topics = BioGenTopics::load_from_file(&path).unwrap();
        assert_eq!(topics.0.len(), 2);
        assert_eq!(topics.0[0], BioGenTopic { id: "1".into(), question: "What causes scurvy?".into() });
        // Falls back to topic_id + text, and trims trailing whitespace.
        assert_eq!(topics.0[1], BioGenTopic { id: "2".into(), question: "How does aspirin work?".into() });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_topics_wrapped_object_with_numeric_id() {
        let dir = std::env::temp_dir();
        let path = dir.join("biogen_topics_wrapped.json");
        std::fs::write(
            &path,
            r#"{"topics": [{"number": 7, "narrative": "Define homeostasis."}]}"#,
        )
        .unwrap();
        let topics = BioGenTopics::load_from_file(&path).unwrap();
        assert_eq!(topics.0.len(), 1);
        // Numeric id coerced to string, narrative used as the question.
        assert_eq!(topics.0[0], BioGenTopic { id: "7".into(), question: "Define homeostasis.".into() });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_topics_empty_is_error() {
        let dir = std::env::temp_dir();
        let path = dir.join("biogen_topics_empty.json");
        std::fs::write(&path, r#"[]"#).unwrap();
        assert!(BioGenTopics::load_from_file(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_topics_missing_file_is_error() {
        assert!(BioGenTopics::load_from_file("/tmp/biogen-does-not-exist-xyz.json").is_err());
    }

    // ---- citation id extraction ----

    #[test]
    fn extract_ids_handles_groups_dedup_and_cap() {
        assert_eq!(extract_citation_ids("Plain sentence with no cite."), Vec::<String>::new());
        assert_eq!(extract_citation_ids("Vitamin C prevents scurvy [doc1]."), vec!["doc1"]);
        // Grouped, comma/semicolon separated, de-duplicated in first-seen order.
        assert_eq!(
            extract_citation_ids("Claim [a, b; a] and again [b]."),
            vec!["a", "b"]
        );
        // Capped at MAX_CITATIONS_PER_SENTENCE (3).
        assert_eq!(
            extract_citation_ids("Big claim [d1][d2][d3][d4][d5]."),
            vec!["d1", "d2", "d3"]
        );
    }

    // ---- CSR ----

    #[test]
    fn csr_empty_answer_is_zero_one_topic() {
        let r = block_on(citation_support_rate(stub_entails, "   ", &chunks(&[]))).unwrap();
        assert_eq!(r, BioGenReport { csr: 0.0, n_sentences: 0, n_topics: 1 });
    }

    #[test]
    fn csr_all_sentences_supported() {
        // Each sentence's cited chunk text contains all of its (non-marker) tokens.
        let c = chunks(&[
            ("d1", "vitamin c deficiency causes scurvy in humans"),
            ("d2", "aspirin inhibits cyclooxygenase enzymes"),
        ]);
        let answer = "Vitamin C deficiency causes scurvy [d1]. Aspirin inhibits cyclooxygenase [d2].";
        let r = block_on(citation_support_rate(stub_entails, answer, &c)).unwrap();
        assert_eq!(r.n_sentences, 2);
        assert_eq!(r.n_topics, 1);
        assert!((r.csr - 1.0).abs() < 1e-6);
    }

    #[test]
    fn csr_uncited_and_missing_chunk_count_as_unsupported() {
        let c = chunks(&[("d1", "vitamin c deficiency causes scurvy")]);
        // s1: supported by d1. s2: no citation -> unsupported. s3: cites d9 which
        // is absent from the chunk map -> no premise -> unsupported.
        let answer =
            "Vitamin C deficiency causes scurvy [d1]. Scurvy is unpleasant. Aspirin helps headaches [d9].";
        let r = block_on(citation_support_rate(stub_entails, answer, &c)).unwrap();
        assert_eq!(r.n_sentences, 3);
        assert!((r.csr - (1.0 / 3.0)).abs() < 1e-6, "got {}", r.csr);
    }

    #[test]
    fn csr_concatenates_multiple_citations() {
        // Sentence needs tokens from BOTH d1 and d2; neither alone suffices, so
        // the concatenated premise is what makes φ pass.
        let c = chunks(&[("d1", "sky blue"), ("d2", "grass green")]);
        let answer = "sky blue grass green [d1, d2].";
        let r = block_on(citation_support_rate(stub_entails, answer, &c)).unwrap();
        assert_eq!(r.n_sentences, 1);
        assert!((r.csr - 1.0).abs() < 1e-6);
        // Sanity: with only one of the two citations it would fail.
        let answer_one = "sky blue grass green [d1].";
        let r1 = block_on(citation_support_rate(stub_entails, answer_one, &c)).unwrap();
        assert_eq!(r1.csr, 0.0);
    }

    #[test]
    fn csr_respects_per_answer_doc_budget() {
        // Build 31 distinct docs each supporting its own one-token sentence. The
        // 31st distinct doc exceeds MAX_DOCS_PER_ANSWER (30) and is dropped, so
        // its sentence becomes unsupported even though the citation is valid.
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut answer = String::new();
        for i in 0..MAX_DOCS_PER_ANSWER + 1 {
            let id = format!("d{i}");
            let word = format!("term{i}");
            pairs.push((id.clone(), word.clone()));
            answer.push_str(&format!("{word} [{id}]. "));
        }
        let c: HashMap<String, String> = pairs.into_iter().collect();
        let r = block_on(citation_support_rate(stub_entails, &answer, &c)).unwrap();
        assert_eq!(r.n_sentences, MAX_DOCS_PER_ANSWER + 1);
        // Exactly 30 sentences supported, 1 dropped by the budget.
        let expected = MAX_DOCS_PER_ANSWER as f32 / (MAX_DOCS_PER_ANSWER as f32 + 1.0);
        assert!((r.csr - expected).abs() < 1e-6, "got {}", r.csr);
    }

    #[test]
    fn csr_reused_doc_does_not_re_consume_budget() {
        // Same doc cited twice counts once against the 30-doc budget, so a fresh
        // doc after a repeat still fits.
        let c = chunks(&[("d1", "alpha"), ("d2", "beta")]);
        let answer = "alpha [d1]. alpha [d1]. beta [d2].";
        let r = block_on(citation_support_rate(stub_entails, answer, &c)).unwrap();
        assert_eq!(r.n_sentences, 3);
        assert!((r.csr - 1.0).abs() < 1e-6);
    }

    // ---- aggregate ----

    #[test]
    fn aggregate_micro_averages_over_sentences() {
        // Report A: 2/4 supported. Report B: 3/3 supported.
        // Micro CSR = (2 + 3) / (4 + 3) = 5/7.
        let a = BioGenReport { csr: 0.5, n_sentences: 4, n_topics: 1 };
        let b = BioGenReport { csr: 1.0, n_sentences: 3, n_topics: 1 };
        let agg = aggregate(&[a, b]);
        assert_eq!(agg.n_topics, 2);
        assert_eq!(agg.n_sentences, 7);
        assert!((agg.csr - 5.0 / 7.0).abs() < 1e-6, "got {}", agg.csr);
    }

    #[test]
    fn aggregate_empty_is_default_like() {
        let agg = aggregate(&[]);
        assert_eq!(agg, BioGenReport { csr: 0.0, n_sentences: 0, n_topics: 0 });
        // A topic with zero sentences still counts as a topic but adds no
        // sentences and contributes 0 to CSR.
        let agg2 = aggregate(&[BioGenReport { csr: 0.0, n_sentences: 0, n_topics: 1 }]);
        assert_eq!(agg2, BioGenReport { csr: 0.0, n_sentences: 0, n_topics: 1 });
    }
}
