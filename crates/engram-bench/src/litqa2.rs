//! LitQA2 benchmark track — loader, parser, and scoring.
//!
//! Source: futurehouse/lab-bench LitQA2 (199 multiple-choice questions over the
//! scientific literature). Each source row carries:
//!
//! ```text
//! { "id": String, "question": String, "ideal": String,
//!   "distractors": [String, ...] }       // optional "key"/"sources" DOI
//! ```
//!
//! We build the answer option set as `[ideal] + distractors`, then ALWAYS append
//! the literal abstention option `"Insufficient information to answer this
//! question."`. The non-abstention options are shuffled **deterministically**
//! from a stable hash of the question `id` (no RNG crate, no global state) so a
//! given dataset always yields the same option ordering across runs and
//! platforms; `ideal_idx` and `abstain_idx` are recorded after the shuffle.
//!
//! Scoring mirrors the PaperQA2 / LitQA2 convention (arXiv:2409.13740, reported
//! accuracy 66.0 / precision 85.2; peer anchor AstaBench arXiv:2510.21652):
//! abstentions are *excluded* from the precision denominator, so a system that
//! abstains rather than guesses is not penalised on precision.
//!
//! This module is the loader/parser/scoring surface only — the retrieval +
//! answerer + judge run loop is wired into `qa.rs` by the integration phase.
//! Everything here is pure and deterministic, with offline unit tests.

use crate::error::BenchError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The literal abstention option appended to every question's option list.
pub const ABSTAIN_OPTION: &str = "Insufficient information to answer this question.";

/// A single LitQA2 multiple-choice question with its (deterministically
/// shuffled) option set fully materialised.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LitQA2Question {
    pub id: String,
    pub question: String,
    /// All answer choices in presentation order: the shuffled
    /// `[ideal] + distractors` followed by the abstention option last.
    pub options: Vec<String>,
    /// Index into `options` of the correct (ideal) answer.
    pub ideal_idx: usize,
    /// Index into `options` of the abstention option (always the last entry).
    pub abstain_idx: usize,
    /// DOI of the paper the question is drawn from, when the source provides it.
    pub target_doi: Option<String>,
}

/// A loaded LitQA2 dataset (newtype over the question vector).
#[derive(Debug, Clone)]
pub struct LitQA2Dataset(pub Vec<LitQA2Question>);

/// Raw on-disk row as published by lab-bench. Field names are aliased
/// generously so either the canonical export or common JSONL variants parse.
#[derive(Debug, Clone, Deserialize)]
struct RawRow {
    #[serde(alias = "question_id", alias = "uid")]
    id: String,
    #[serde(alias = "prompt")]
    question: String,
    #[serde(alias = "ideal_answer", alias = "answer", alias = "correct")]
    ideal: String,
    #[serde(default, alias = "distractor", alias = "incorrect")]
    distractors: Vec<String>,
    /// DOI may appear under several keys depending on the export.
    #[serde(default, alias = "key", alias = "source_doi", alias = "sources")]
    doi: Option<DoiField>,
}

/// DOI can be a bare string or a one-element list of strings, depending on the
/// export. Accept both and normalise to `Option<String>`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum DoiField {
    One(String),
    Many(Vec<String>),
}

impl DoiField {
    fn into_doi(self) -> Option<String> {
        let raw = match self {
            DoiField::One(s) => Some(s),
            DoiField::Many(v) => v.into_iter().next(),
        };
        raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
}

impl LitQA2Question {
    /// Materialise a question from a raw row: build `[ideal] + distractors`,
    /// shuffle deterministically by `id`, append the abstention option last, and
    /// record `ideal_idx` / `abstain_idx`.
    fn from_raw(raw: RawRow) -> Self {
        // Non-abstention options, ideal first.
        let mut options: Vec<String> = Vec::with_capacity(raw.distractors.len() + 2);
        options.push(raw.ideal);
        options.extend(raw.distractors);

        // Deterministic shuffle seeded purely from the id, then locate ideal.
        let order = deterministic_permutation(options.len(), &raw.id);
        let shuffled: Vec<String> = order.iter().map(|&i| options[i].clone()).collect();
        // The ideal answer was at original index 0; find where it landed.
        let ideal_idx = order
            .iter()
            .position(|&i| i == 0)
            .expect("permutation is a bijection over 0..len");

        let mut options = shuffled;
        options.push(ABSTAIN_OPTION.to_string());
        let abstain_idx = options.len() - 1;

        Self {
            id: raw.id,
            question: raw.question,
            options,
            ideal_idx,
            abstain_idx,
            target_doi: raw.doi.and_then(DoiField::into_doi),
        }
    }
}

impl LitQA2Dataset {
    /// Load LitQA2 from a JSON array **or** JSONL file. Returns
    /// [`BenchError::DatasetMissing`] when the file does not exist.
    pub fn load_from_file(path: &Path) -> Result<Self, BenchError> {
        if !path.exists() {
            return Err(BenchError::DatasetMissing);
        }
        let contents = std::fs::read_to_string(path)?;
        let raws = parse_rows(&contents, path)?;
        if raws.is_empty() {
            return Err(BenchError::InvalidDataset(format!(
                "0 LitQA2 questions in {}",
                path.display()
            )));
        }
        let questions = raws.into_iter().map(LitQA2Question::from_raw).collect();
        Ok(Self(questions))
    }
}

/// Parse the file contents as either a single JSON array of rows or as JSONL
/// (one JSON object per non-empty line). The array form is tried first; if the
/// payload is not a single array we fall back to line-by-line JSONL.
fn parse_rows(contents: &str, path: &Path) -> Result<Vec<RawRow>, BenchError> {
    let trimmed = contents.trim_start();
    if trimmed.starts_with('[') {
        let rows: Vec<RawRow> = serde_json::from_str(trimmed).map_err(|e| {
            BenchError::InvalidDataset(format!("failed to parse {}: {}", path.display(), e))
        })?;
        return Ok(rows);
    }

    let mut rows = Vec::new();
    for (idx, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: RawRow = serde_json::from_str(line).map_err(|e| {
            BenchError::InvalidDataset(format!(
                "failed to parse {} line {}: {}",
                path.display(),
                idx + 1,
                e
            ))
        })?;
        rows.push(row);
    }
    Ok(rows)
}

/// Default disk path for the LitQA2 JSONL export.
pub fn default_path() -> PathBuf {
    PathBuf::from("data/litqa2/litqa2.jsonl")
}

// ---------------------------------------------------------------------------
// Deterministic shuffle
// ---------------------------------------------------------------------------

/// FNV-1a 64-bit hash of a string. Stable across runs and platforms (unlike
/// `DefaultHasher`, whose seed is unspecified), which is what makes the option
/// shuffle reproducible. Mirrors the `fingerprint` hash used elsewhere in the
/// bench crate.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
    }
    h
}

/// A deterministic permutation of `0..n`, seeded purely by `id`. We run a
/// Fisher–Yates shuffle driven by a SplitMix64-style counter generator seeded
/// from the id's FNV-1a hash — no RNG crate, no global state, fully
/// reproducible. Returns the identity for `n <= 1`.
fn deterministic_permutation(n: usize, id: &str) -> Vec<usize> {
    let mut perm: Vec<usize> = (0..n).collect();
    if n <= 1 {
        return perm;
    }
    // SplitMix64: a tiny, well-distributed counter-based generator. Seeding the
    // state from the id hash means the whole sequence is a pure function of id.
    let mut state = fnv1a(id);
    let mut next_u64 = || -> u64 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    // Fisher–Yates from the top down. Unbiased modulo: reject the high tail so
    // each in-range value is equally likely.
    for i in (1..n).rev() {
        let bound = (i + 1) as u64;
        let limit = u64::MAX - (u64::MAX % bound);
        let mut r = next_u64();
        while r >= limit {
            r = next_u64();
        }
        let j = (r % bound) as usize;
        perm.swap(i, j);
    }
    perm
}

// ---------------------------------------------------------------------------
// Choice parsing
// ---------------------------------------------------------------------------

/// Deterministically parse a chosen option index from free-form model output.
///
/// Recognises a labelled letter answer for option `A`, `B`, … up to the
/// `n_options`-th letter, in these forms (case-insensitive, first match wins,
/// scanning lines bottom-up so a final "Answer:" line is preferred):
///
/// - an `Answer:`/`Final answer:`-style line whose value starts with a letter
///   (`Answer: C`, `The answer is (B)`),
/// - a standalone letter line, optionally wrapped/parenthesised or followed by a
///   `.`/`)`/`:` delimiter (`C`, `(B)`, `A.`, `D)`).
///
/// Returns `None` if no answer letter in range can be extracted.
pub fn parse_choice(model_output: &str, n_options: usize) -> Option<usize> {
    if n_options == 0 {
        return None;
    }

    // Prefer the last explicit "answer" line, then fall back to any bare letter
    // line, scanning from the bottom so trailing conclusions win.
    for line in model_output.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = strip_answer_prefix(line) {
            if let Some(idx) = first_letter_choice(rest, n_options) {
                return Some(idx);
            }
        }
    }
    for line in model_output.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(idx) = bare_letter_choice(line, n_options) {
            return Some(idx);
        }
    }
    None
}

/// If `line` opens with an answer label (`Answer:`, `Final answer:`, `The answer
/// is`, with optional `final`/`correct`), return the remainder after the label.
fn strip_answer_prefix(line: &str) -> Option<&str> {
    let lower = line.to_ascii_lowercase();
    // Forms ending in a colon: "answer:", "final answer:", "correct answer:".
    for prefix in ["final answer:", "correct answer:", "answer:", "ans:"] {
        if lower.starts_with(prefix) {
            return Some(line[prefix.len()..].trim_start());
        }
    }
    // Phrasal forms: "the answer is", "answer is", "the correct answer is".
    for prefix in [
        "the correct answer is",
        "the answer is",
        "correct answer is",
        "answer is",
    ] {
        if lower.starts_with(prefix) {
            return Some(line[prefix.len()..].trim_start());
        }
    }
    None
}

/// Letter at the very start of `text` (after stripping an opening `(`/`[`),
/// validated against `n_options`. Used for the value after an "Answer:" label,
/// where the letter must lead.
fn first_letter_choice(text: &str, n_options: usize) -> Option<usize> {
    let text = text.trim_start();
    let mut chars = text.chars();
    let first = chars.next()?;
    let candidate = if matches!(first, '(' | '[' | '{' | '*' | '"' | '\'') {
        chars.next()?
    } else {
        first
    };
    letter_to_idx(candidate, n_options)
}

/// A whole line that is *just* an option letter, optionally wrapped in
/// brackets/quotes/asterisks and/or followed by a single `.`/`)`/`:`/`,`
/// delimiter. Rejects lines that are real prose (e.g. "A study showed…").
fn bare_letter_choice(line: &str, n_options: usize) -> Option<usize> {
    let core: String = line
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | '*' | '"' | '\'' | ' '
            )
        })
        .collect();
    let mut chars = core.chars();
    let letter = chars.next()?;
    let idx = letter_to_idx(letter, n_options)?;
    // Whatever remains must be only a trailing delimiter, nothing else.
    let rest = chars.as_str();
    if rest.is_empty() || rest.chars().all(|c| matches!(c, '.' | ':' | ',' | '-')) {
        Some(idx)
    } else {
        None
    }
}

/// Map an `A`/`a`-based option letter to a zero-based index, bounded by
/// `n_options`.
fn letter_to_idx(c: char, n_options: usize) -> Option<usize> {
    if !c.is_ascii_alphabetic() {
        return None;
    }
    let idx = (c.to_ascii_uppercase() as u8 - b'A') as usize;
    if idx < n_options {
        Some(idx)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Per-question outcome, the atomic input to [`score`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LitQa2Record {
    /// The system selected the ideal option.
    pub correct: bool,
    /// The system did not commit to a specific answer option — either it
    /// deliberately selected "Insufficient information" (a *principled*
    /// abstention) or it produced no usable answer (see `no_response`).
    pub abstained: bool,
    /// The answer was empty/unparseable — NOT a principled abstention. Kept
    /// separate so the precision headline can't be inflated by a flaky/empty
    /// API response being scored as a safe decline. When `no_response` is true,
    /// `abstained` is also true.
    pub no_response: bool,
    /// Retrieval surfaced the question's target document.
    pub retrieval_hit: bool,
}

/// Aggregate LitQA2 scores. Definitions follow the PaperQA2 / LitQA2 protocol:
///
/// - `accuracy`   = correct / total
/// - `precision`  = correct / (correct + incorrect), **principled abstentions
///   excluded** from the denominator (1.0 when nothing was answered)
/// - `precision_strict` = correct / (correct + incorrect + no_response):
///   treats empty/unparseable responses as failed commitments rather than safe
///   declines, so a flaky API can't inflate the headline precision. Report BOTH.
/// - `coverage`   = answered / total           (answered = non-abstained)
/// - `abstain_rate` = principled_abstentions / total  (excludes no_response)
/// - `no_response_rate` = no_response / total
/// - `retrieval_success` = retrieval_hits / total
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct LitQa2Scores {
    pub accuracy: f32,
    pub precision: f32,
    pub precision_strict: f32,
    pub coverage: f32,
    pub abstain_rate: f32,
    pub no_response_rate: f32,
    pub retrieval_success: f32,
    pub n: usize,
}

/// Aggregate per-question records into [`LitQa2Scores`].
pub fn score(records: &[LitQa2Record]) -> LitQa2Scores {
    let n = records.len();
    if n == 0 {
        return LitQa2Scores::default();
    }

    let mut correct = 0usize;
    let mut abstained = 0usize; // includes no_response
    let mut no_response = 0usize;
    let mut answered = 0usize;
    let mut hits = 0usize;
    for r in records {
        if r.correct {
            correct += 1;
        }
        if r.abstained {
            abstained += 1;
        } else {
            answered += 1;
        }
        if r.no_response {
            no_response += 1;
        }
        if r.retrieval_hit {
            hits += 1;
        }
    }
    // Incorrect = answered but wrong. Principled abstentions never count as
    // incorrect. Principled abstentions exclude the no_response cases.
    let incorrect = answered.saturating_sub(correct);
    let principled_abstain = abstained.saturating_sub(no_response);
    let answered_for_precision = correct + incorrect; // == answered

    let nf = n as f32;
    let precision = if answered_for_precision == 0 {
        // Nothing was answered: no wrong commitments, so precision is perfect.
        1.0
    } else {
        correct as f32 / answered_for_precision as f32
    };
    // Strict: a no_response is a failed commitment, not a safe decline.
    let strict_denom = answered_for_precision + no_response;
    let precision_strict = if strict_denom == 0 {
        1.0
    } else {
        correct as f32 / strict_denom as f32
    };

    LitQa2Scores {
        accuracy: correct as f32 / nf,
        precision,
        precision_strict,
        coverage: answered as f32 / nf,
        abstain_rate: principled_abstain as f32 / nf,
        no_response_rate: no_response as f32 / nf,
        retrieval_success: hits as f32 / nf,
        n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(id: &str, ideal: &str, distractors: &[&str]) -> RawRow {
        RawRow {
            id: id.to_string(),
            question: format!("Q for {id}?"),
            ideal: ideal.to_string(),
            distractors: distractors.iter().map(|s| s.to_string()).collect(),
            doi: None,
        }
    }

    // ----- option construction -------------------------------------------

    #[test]
    fn builds_options_with_abstain_last_and_correct_indices() {
        let q = LitQA2Question::from_raw(raw("q1", "IDEAL", &["D1", "D2", "D3"]));
        // 4 real options + abstention.
        assert_eq!(q.options.len(), 5);
        assert_eq!(q.abstain_idx, 4);
        assert_eq!(q.options[q.abstain_idx], ABSTAIN_OPTION);
        // The ideal index points at the ideal text, wherever the shuffle put it.
        assert_eq!(q.options[q.ideal_idx], "IDEAL");
        assert!(q.ideal_idx < q.abstain_idx);
        // The original option multiset is preserved (no dupes/drops).
        let mut got: Vec<&str> = q.options.iter().map(|s| s.as_str()).collect();
        got.sort_unstable();
        let mut want = vec!["IDEAL", "D1", "D2", "D3", ABSTAIN_OPTION];
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn single_option_plus_abstain() {
        // Only the ideal, no distractors -> [IDEAL, ABSTAIN].
        let q = LitQA2Question::from_raw(raw("solo", "ONLY", &[]));
        assert_eq!(
            q.options,
            vec!["ONLY".to_string(), ABSTAIN_OPTION.to_string()]
        );
        assert_eq!(q.ideal_idx, 0);
        assert_eq!(q.abstain_idx, 1);
    }

    // ----- deterministic shuffle -----------------------------------------

    #[test]
    fn shuffle_is_stable_across_calls() {
        let a = LitQA2Question::from_raw(raw("stable-id", "IDEAL", &["D1", "D2", "D3", "D4"]));
        let b = LitQA2Question::from_raw(raw("stable-id", "IDEAL", &["D1", "D2", "D3", "D4"]));
        assert_eq!(a.options, b.options);
        assert_eq!(a.ideal_idx, b.ideal_idx);
    }

    #[test]
    fn shuffle_differs_by_id() {
        // Two different ids should (for this option set) produce different
        // orderings — guards against a constant/no-op permutation.
        let a = LitQA2Question::from_raw(raw("id-alpha", "IDEAL", &["D1", "D2", "D3", "D4", "D5"]));
        let b = LitQA2Question::from_raw(raw("id-bravo", "IDEAL", &["D1", "D2", "D3", "D4", "D5"]));
        assert_ne!(a.options, b.options);
    }

    #[test]
    fn permutation_is_a_bijection() {
        // Every index 0..n appears exactly once, for a range of sizes/ids.
        for n in 1..12usize {
            for id in ["x", "another-id", "12345", "ζ-unicode"] {
                let p = deterministic_permutation(n, id);
                assert_eq!(p.len(), n);
                let mut seen = vec![false; n];
                for &v in &p {
                    assert!(v < n, "value {v} out of range for n={n}");
                    assert!(!seen[v], "value {v} repeated for n={n} id={id}");
                    seen[v] = true;
                }
                assert!(seen.iter().all(|&s| s));
            }
        }
    }

    #[test]
    fn permutation_identity_for_trivial_sizes() {
        assert_eq!(deterministic_permutation(0, "x"), Vec::<usize>::new());
        assert_eq!(deterministic_permutation(1, "x"), vec![0]);
    }

    // ----- DOI / format flexibility --------------------------------------

    #[test]
    fn parses_jsonl_with_doi_string_and_list() {
        let jsonl = r#"
{"id":"r1","question":"Q1?","ideal":"I1","distractors":["a","b"],"key":"10.1/abc"}
{"id":"r2","question":"Q2?","ideal":"I2","distractors":["c"],"sources":["10.2/def","10.2/zzz"]}
{"id":"r3","question":"Q3?","ideal":"I3","distractors":[]}
"#;
        let rows = parse_rows(jsonl, Path::new("mem.jsonl")).unwrap();
        assert_eq!(rows.len(), 3);
        let qs: Vec<LitQA2Question> = rows.into_iter().map(LitQA2Question::from_raw).collect();
        assert_eq!(qs[0].target_doi.as_deref(), Some("10.1/abc"));
        assert_eq!(qs[1].target_doi.as_deref(), Some("10.2/def"));
        assert_eq!(qs[2].target_doi, None);
    }

    #[test]
    fn parses_top_level_json_array() {
        let json = r#"[
          {"id":"a","question":"Qa?","ideal":"Ia","distractors":["x"]},
          {"id":"b","question":"Qb?","ideal":"Ib","distractors":["y","z"]}
        ]"#;
        let rows = parse_rows(json, Path::new("mem.json")).unwrap();
        assert_eq!(rows.len(), 2);
    }

    // ----- parse_choice ---------------------------------------------------

    #[test]
    fn parse_choice_labeled_letter() {
        assert_eq!(parse_choice("Answer: C", 5), Some(2));
        assert_eq!(parse_choice("Final answer: A", 5), Some(0));
        assert_eq!(parse_choice("answer: b", 5), Some(1));
        assert_eq!(parse_choice("The answer is (D)", 5), Some(3));
        assert_eq!(parse_choice("The correct answer is E.", 5), Some(4));
    }

    #[test]
    fn parse_choice_bare_letter_line() {
        assert_eq!(parse_choice("C", 5), Some(2));
        assert_eq!(parse_choice("(B)", 5), Some(1));
        assert_eq!(parse_choice("A.", 5), Some(0));
        assert_eq!(parse_choice("D)", 5), Some(3));
        assert_eq!(parse_choice("*E*", 5), Some(4));
    }

    #[test]
    fn parse_choice_prefers_last_answer_line() {
        let out = "Let me think. Maybe A is plausible.\nBut on reflection.\nAnswer: D";
        assert_eq!(parse_choice(out, 5), Some(3));
    }

    #[test]
    fn parse_choice_reasoning_then_final_bare_letter() {
        let out = "Reasoning about the options here...\n\nB";
        assert_eq!(parse_choice(out, 5), Some(1));
    }

    #[test]
    fn parse_choice_rejects_prose_and_out_of_range() {
        // Prose that merely starts with a capital letter is not a choice.
        assert_eq!(
            parse_choice("A study of mitochondria showed growth.", 5),
            None
        );
        // Letter beyond the option count is rejected.
        assert_eq!(parse_choice("Answer: F", 5), None);
        assert_eq!(parse_choice("Z", 5), None);
        // Nothing parseable at all.
        assert_eq!(parse_choice("I'm not sure, it depends.", 5), None);
        assert_eq!(parse_choice("", 5), None);
        // Zero options can never match.
        assert_eq!(parse_choice("A", 0), None);
    }

    // ----- scoring --------------------------------------------------------

    fn rec(correct: bool, abstained: bool, hit: bool) -> LitQa2Record {
        LitQa2Record {
            correct,
            abstained,
            no_response: false,
            retrieval_hit: hit,
        }
    }

    fn rec_no_response(hit: bool) -> LitQa2Record {
        LitQa2Record {
            correct: false,
            abstained: true,
            no_response: true,
            retrieval_hit: hit,
        }
    }

    #[test]
    fn no_response_does_not_inflate_precision() {
        // 2 correct, 1 wrong (answered), 1 principled abstain, 1 empty response.
        let recs = vec![
            rec(true, false, true),
            rec(true, false, true),
            rec(false, false, true), // answered, wrong
            rec(false, true, false), // principled abstain
            rec_no_response(false),  // empty API response
        ];
        let s = score(&recs);
        // Lenient precision excludes BOTH the abstain and the no_response: 2/3.
        assert!((s.precision - 2.0 / 3.0).abs() < 1e-6);
        // Strict precision counts the no_response as a failed commitment: 2/4.
        assert!(
            (s.precision_strict - 2.0 / 4.0).abs() < 1e-6,
            "got {}",
            s.precision_strict
        );
        // abstain_rate counts only the principled abstention (1/5), not the
        // no_response, which has its own rate (1/5).
        assert!(
            (s.abstain_rate - 1.0 / 5.0).abs() < 1e-6,
            "got {}",
            s.abstain_rate
        );
        assert!((s.no_response_rate - 1.0 / 5.0).abs() < 1e-6);
    }

    #[test]
    fn score_basic_mix() {
        // 5 questions: 2 correct, 1 wrong (answered), 2 abstained.
        // hits: 3 of 5.
        let recs = vec![
            rec(true, false, true),
            rec(true, false, true),
            rec(false, false, true), // answered, wrong
            rec(false, true, false), // abstained
            rec(false, true, false), // abstained
        ];
        let s = score(&recs);
        assert_eq!(s.n, 5);
        assert!((s.accuracy - 2.0 / 5.0).abs() < 1e-6);
        // precision = correct / (correct + incorrect) = 2 / 3, abstentions excluded.
        assert!((s.precision - 2.0 / 3.0).abs() < 1e-6);
        // coverage = answered / total = 3 / 5.
        assert!((s.coverage - 3.0 / 5.0).abs() < 1e-6);
        assert!((s.abstain_rate - 2.0 / 5.0).abs() < 1e-6);
        assert!((s.retrieval_success - 3.0 / 5.0).abs() < 1e-6);
    }

    #[test]
    fn score_all_abstain_edge() {
        let recs = vec![rec(false, true, false), rec(false, true, true)];
        let s = score(&recs);
        assert_eq!(s.n, 2);
        assert_eq!(s.accuracy, 0.0);
        // No commitments -> precision is defined as 1.0 (no wrong answers).
        assert_eq!(s.precision, 1.0);
        assert_eq!(s.coverage, 0.0);
        assert_eq!(s.abstain_rate, 1.0);
        assert!((s.retrieval_success - 0.5).abs() < 1e-6);
    }

    #[test]
    fn score_all_correct() {
        let recs = vec![rec(true, false, true), rec(true, false, true)];
        let s = score(&recs);
        assert_eq!(s.accuracy, 1.0);
        assert_eq!(s.precision, 1.0);
        assert_eq!(s.coverage, 1.0);
        assert_eq!(s.abstain_rate, 0.0);
        assert_eq!(s.retrieval_success, 1.0);
    }

    #[test]
    fn score_empty_is_default() {
        assert_eq!(score(&[]), LitQa2Scores::default());
    }

    #[test]
    fn missing_file_is_dataset_missing() {
        let p = Path::new("/nonexistent/litqa2/does-not-exist.jsonl");
        match LitQA2Dataset::load_from_file(p) {
            Err(BenchError::DatasetMissing) => {}
            other => panic!("expected DatasetMissing, got {other:?}"),
        }
    }
}
