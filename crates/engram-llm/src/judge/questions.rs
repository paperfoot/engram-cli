//! Every question engram asks a judge, with its criteria and thresholds.
//!
//! This is the one reviewable place for judge wording. A threshold is only
//! valid for the exact wording, question type and model version it was fitted
//! on, so a wording change here means re-running the eval
//! (`crates/engram-bench/examples/judge_eval.rs`, which builds its requests
//! from this module, so the evaluated text IS the shipped text) and re-fitting
//! the constants below.
//!
//! What is never asked, because code answers it better or the answer is not
//! the judge's to give:
//! - temporal ordering and dates (which fact is newer, whether an interval
//!   overlaps): `engram_core::interval` and the write/triage rules;
//! - numbers and arithmetic: [`super::lexical::numbers_ok`];
//! - relation arity and set-valued semantics: the relation registry;
//! - safety relations and `may_close`: the authority rules. A judge verdict is
//!   a probability, not permission to close a fact.
//!
//! Layout rules: the item under judgement (a claim, an existing fact) rides in
//! object-form `instructions` next to its question, never as an array index
//! into the state (`candidates[22]` misresolves on long arrays). Object keys
//! are serialized sorted (serde_json maps), and the eval sees the same order.

use super::{JudgeError, JudgeRequest, JudgeResponse, Question};
use serde::Serialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// Grounding: is a claim supported by the passage it was extracted from?
// ---------------------------------------------------------------------------

/// Wording of the `plain` variant in `benchmarks/jev/scifact_grounding_probe.py`.
/// It beat a variant that spelled out every edge case (direction, species,
/// hedging) on the same pairs.
pub const GROUNDING_QUESTION: &str = "Does `passage` directly support `claim`?";
pub const GROUNDING_TRUE: &str = "The passage states or directly implies the claim.";
pub const GROUNDING_FALSE: &str =
    "The passage does not state the claim, contradicts it, or only discusses the topic.";

/// Admit a claim at P(supported) >= this.
///
/// SciFact probe (1,189 human-labelled claim/abstract pairs: 508 support, 265
/// contradict, 416 not enough info; AUC 0.965;
/// `benchmarks/jev-grounding-probe-scifact-20260925T0015.json`):
/// - p >= 0.5: precision 0.944, recall 0.827; 2.3% of contradicting claims
///   admitted, against 69% for the lexical rule (precision 0.587, recall 0.756);
/// - p >= 0.7: precision 0.955, recall 0.750; 1.1% of contradictions admitted.
///
/// Placeholder until re-fitted on claims compile actually extracts: SciFact
/// claims are written by annotators, not by an extraction model.
pub const GROUNDING_ADMIT_P: f32 = 0.5;

/// Admit a claim or fact compile extracted from a chunk at P(supported) >=
/// this.
///
/// Compile-extraction eval (compile's own extraction call over 100 SciFact
/// abstracts: 313 Opus-supported claims/facts, 535 adversarial rewrites of
/// them, AUC 0.99; `benchmarks/jev-grounding-compile-extracted-20260925T1030.json`):
/// - p >= 0.9 keeps 100% of supported claims and 98.9% of supported facts,
///   and admits 1.3% of the rewrites (lexical rule: 99%). Direction flips and
///   negations: none admitted; association->causation 12%, hedge removed
///   14%, prior-study attribution 10% (the classes lower cuts let through:
///   56%, 29%, 30% at 0.5).
///
/// Extracted claims sit near-verbatim on their chunk, so the high cut costs
/// almost no recall; SciFact's annotator paraphrases are why the general
/// default above is lower.
pub const COMPILE_ADMIT_P: f32 = 0.9;

/// Admit a SAFETY-relation fact (takes_medication, allergic_to,
/// diagnosed_with) that engram's own extractor pulled from a memory at
/// P(supported) >= this. Other extracted facts are annotated, never gated.
///
/// Remember-extraction eval (161 facts from 60 LoCoMo windows + 20 health
/// notes, Opus labels; `benchmarks/jev-grounding-remember-20260925T1030.json`,
/// rendered with [`fact_as_claim_with_state`]):
/// - safety relations only at 0.5: 0 true facts dropped; blocks the
///   fabricated "allergic_to ibuprofen" (p 0.04, from "avoid ibuprofen with
///   the kidney issue") and "takes_medication paracetamol" (0.31, from
///   "paracetamol is fine");
/// - gating every fact at 0.5 instead drops 7 true facts (mostly "prefers
///   X", which the judge reads literally) to block 6 more.
pub const REMEMBER_SAFETY_ADMIT_P: f32 = 0.5;

/// On-mode admission: the number rule stays in code and is checked by the
/// caller; this is only the probability cut the evals above measured. No
/// uncertainty band here: a band that fell back to the lexical rule would
/// readmit exactly what the rule gets wrong.
pub fn admits(p: f32, threshold: f32) -> bool {
    p >= threshold
}

/// Values within this distance of [`GROUNDING_ADMIT_P`] (0.45..=0.55) are
/// [`Grounding::Uncertain`]: identical requests are not bit-identical (22 of
/// 30 nouls moved between two sends, mean |delta| 0.018, max 0.12), so a cut
/// with no band flips on noise.
pub const GROUNDING_MARGIN: f32 = 0.05;

/// A grounding verdict. `Uncertain` means "keep today's behaviour and log it",
/// never "admit".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Grounding {
    Supported,
    Uncertain,
    Unsupported,
}

/// One noul per claim against one passage, all in one request. Claim ids are
/// the question ids, so answers come back keyed by claim.
pub fn grounding_request(passage: &str, claims: &[(String, String)]) -> JudgeRequest {
    let mut request = JudgeRequest::new(json!({ "passage": passage }));
    for (id, claim) in claims {
        request.push(
            id.clone(),
            Question::noul_with_criteria(
                json!({ "claim": claim, "question": GROUNDING_QUESTION }),
                GROUNDING_TRUE,
                GROUNDING_FALSE,
            ),
        );
    }
    request
}

/// A fact as the sentence the grounding question checks:
/// `("Bob", "diagnosed_with", "HIV")` -> "Bob diagnosed with HIV".
pub fn fact_as_claim(subject: &str, predicate: &str, object: &str) -> String {
    let predicate = predicate.trim().replace('_', " ");
    [subject.trim(), predicate.trim(), object.trim()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A fact with its extracted state as the sentence the text supports.
/// `ended` renders "Ana no longer takes medication metformin": rendering a
/// cessation as the ongoing claim made the judge reject every correct one
/// (p 0.01-0.24 in the remember eval); this form scored them 0.78-0.98 and
/// lifted AUC from 0.72 to 0.79.
pub fn fact_as_claim_with_state(
    subject: &str,
    predicate: &str,
    object: &str,
    ended: bool,
) -> String {
    if !ended {
        return fact_as_claim(subject, predicate, object);
    }
    let predicate = predicate.trim().replace('_', " ");
    [subject.trim(), "no longer", predicate.trim(), object.trim()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn grounding_verdict(p: f32) -> Grounding {
    // The epsilon keeps the band symmetric under f32 rounding (0.55 - 0.5
    // is 0.05000001).
    if (p - GROUNDING_ADMIT_P).abs() <= GROUNDING_MARGIN + 1e-6 {
        Grounding::Uncertain
    } else if p > GROUNDING_ADMIT_P {
        Grounding::Supported
    } else {
        Grounding::Unsupported
    }
}

// ---------------------------------------------------------------------------
// Relevance: does a recalled passage answer the query?
// ---------------------------------------------------------------------------

/// Plain wording, as the September evals favoured: no edge-case criteria.
pub const RELEVANCE_QUESTION: &str = "Does `passage` contain information that answers `query`?";

/// A recall result is kept at P(answers) >= this; when none is, recall
/// abstains. Fitted on `engram bench personal` (see the commit that set it).
pub const RELEVANCE_FLOOR: f32 = 0.5;

/// A passage is cut to this many characters before it is judged: a wiki page
/// can run to pages, and the answer to a recall query sits in its opening
/// far more often than past it.
pub const RELEVANCE_PASSAGE_CHARS: usize = 2_000;

/// One request per recall: state = the query, one noul per passage, the
/// passage riding in the question's own instructions. `passages` are
/// `(id, text)`; answers come back keyed by id.
pub fn relevance_request(query: &str, passages: &[(String, &str)]) -> JudgeRequest {
    let mut request = JudgeRequest::new(json!({ "query": query }));
    for (id, text) in passages {
        let passage: String = text.chars().take(RELEVANCE_PASSAGE_CHARS).collect();
        request.push(
            id.clone(),
            Question::noul(json!({ "passage": passage, "question": RELEVANCE_QUESTION })),
        );
    }
    request
}

// ---------------------------------------------------------------------------
// Pair judge: a new fact against existing facts (dedupe and adjudication).
// ---------------------------------------------------------------------------

/// A fact as the judge sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FactTriple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

impl FactTriple {
    pub fn new(
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
        }
    }
}

pub const RESTATES_QUESTION: &str = "Does `new_fact` state the same claim as `existing`, even if worded differently or naming the value more fully?";
pub const SAME_SLOT_QUESTION: &str =
    "Do `new_fact` and `existing` give a value for the same attribute of the same subject?";
pub const CONTRADICTS_QUESTION: &str = "Do `new_fact` and `existing` contradict each other?";
pub const CONTRADICTS_TRUE: &str = "They cannot both be true of this subject at the same time";
pub const CONTRADICTS_FALSE: &str =
    "Both can be true at once: different aspects, an additional value, or compatible detail";
pub const OLD_ENDED_QUESTION: &str =
    "Does `new_text` say that `existing` stopped being true, ended, was replaced, or was wrong?";

// Thresholds below are PLACEHOLDERS, to be fitted on the pair eval
// (`judge_eval pairs`). The only evidence so far is one probe request (n=1)
// with an earlier wording (`old_fact` for `existing`): "Ana moved from Lisbon
// to Berlin last month." against `Ana lives_in Lisbon` gave same_slot 0.98,
// contradicts 0.87, old_ended 0.88, restates 0.02, and an unrelated
// `works_at` fact 0.03-0.06; "Started metformin ... stopped omeprazole; still
// on lisinopril." gave old_ended 0.96 for omeprazole while contradicts stayed
// at 0.26, which is why old_ended is asked separately.

/// The new fact restates an existing one (dedupe: the new fact adds nothing).
/// High, because a false yes throws a fact away.
pub const RESTATES_P: f32 = 0.9;
/// The two facts cannot both hold now. Just under the probe's clear move
/// (0.87); closing a fact also needs code's authority rules to agree.
pub const CONTRADICTS_P: f32 = 0.85;
/// The new text says the existing fact ended (probe: 0.88 move, 0.96 stopped
/// medication).
pub const OLD_ENDED_P: f32 = 0.85;
/// Below this, the facts confidently fill different slots and coexist
/// (probe: unrelated attribute 0.03-0.06).
pub const NOT_SAME_SLOT_P: f32 = 0.2;

/// The four questions asked about each existing fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairQuestion {
    Restates,
    SameSlot,
    Contradicts,
    OldEnded,
}

impl PairQuestion {
    pub const ALL: [PairQuestion; 4] = [
        PairQuestion::Restates,
        PairQuestion::SameSlot,
        PairQuestion::Contradicts,
        PairQuestion::OldEnded,
    ];

    pub fn key(self) -> &'static str {
        match self {
            PairQuestion::Restates => "restates",
            PairQuestion::SameSlot => "same_slot",
            PairQuestion::Contradicts => "contradicts",
            PairQuestion::OldEnded => "old_ended",
        }
    }

    fn question(self, existing: &FactTriple) -> Question {
        let instructions = |q: &str| json!({ "existing": existing, "question": q });
        match self {
            PairQuestion::Restates => Question::noul(instructions(RESTATES_QUESTION)),
            PairQuestion::SameSlot => Question::noul(instructions(SAME_SLOT_QUESTION)),
            PairQuestion::Contradicts => Question::noul_with_criteria(
                instructions(CONTRADICTS_QUESTION),
                CONTRADICTS_TRUE,
                CONTRADICTS_FALSE,
            ),
            PairQuestion::OldEnded => Question::noul(instructions(OLD_ENDED_QUESTION)),
        }
    }
}

/// The question id for one signal about one existing fact. The key has no
/// '.', so ids stay unique whatever the candidate id contains.
pub fn pair_question_id(candidate_id: &str, question: PairQuestion) -> String {
    format!("{}.{candidate_id}", question.key())
}

/// One request: state = the new text and fact; four nouls per existing fact.
pub fn pair_request(
    new_text: &str,
    new_fact: &FactTriple,
    existing: &[(String, FactTriple)],
) -> JudgeRequest {
    let mut request = JudgeRequest::new(json!({ "new_text": new_text, "new_fact": new_fact }));
    for (id, fact) in existing {
        for q in PairQuestion::ALL {
            request.push(pair_question_id(id, q), q.question(fact));
        }
    }
    request
}

/// The four raw probabilities for one existing fact.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PairAnswers {
    pub restates: f32,
    pub same_slot: f32,
    pub contradicts: f32,
    pub old_ended: f32,
}

impl PairAnswers {
    /// All four or an error: a partial answer set is not a decision.
    pub fn from_response(response: &JudgeResponse, candidate_id: &str) -> Result<Self, JudgeError> {
        let p = |q| response.noul(&pair_question_id(candidate_id, q));
        Ok(Self {
            restates: p(PairQuestion::Restates)?,
            same_slot: p(PairQuestion::SameSlot)?,
            contradicts: p(PairQuestion::Contradicts)?,
            old_ended: p(PairQuestion::OldEnded)?,
        })
    }
}

/// Thresholded signals. The caller combines them with the registry and the
/// authority rules; nothing here closes or merges a fact on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PairSignal {
    /// The new fact restates the existing one.
    pub restates: bool,
    /// False only when the judge is confident the facts fill different slots.
    pub same_slot: bool,
    /// The two cannot both be true of the subject at once.
    pub contradicts: bool,
    /// The new text says the existing fact ended, was replaced or was wrong.
    pub old_ended: bool,
}

pub fn decide_pair(answers: &PairAnswers) -> PairSignal {
    PairSignal {
        restates: answers.restates >= RESTATES_P,
        same_slot: answers.same_slot >= NOT_SAME_SLOT_P,
        contradicts: answers.contradicts >= CONTRADICTS_P,
        old_ended: answers.old_ended >= OLD_ENDED_P,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ended_fact_reads_as_a_cessation() {
        assert_eq!(
            fact_as_claim_with_state("Ana", "takes_medication", "metformin", true),
            "Ana no longer takes medication metformin"
        );
        assert_eq!(
            fact_as_claim_with_state("Ana", "takes_medication", "metformin", false),
            fact_as_claim("Ana", "takes_medication", "metformin")
        );
    }

    #[test]
    fn site_thresholds_admit_at_the_measured_cut() {
        assert!(admits(0.9, COMPILE_ADMIT_P));
        assert!(!admits(0.89, COMPILE_ADMIT_P));
        assert!(admits(0.5, REMEMBER_SAFETY_ADMIT_P));
        assert!(!admits(0.31, REMEMBER_SAFETY_ADMIT_P));
    }
    use crate::judge::Answer;
    use std::collections::BTreeMap;

    #[test]
    fn grounding_wording_is_the_probed_plain_variant() {
        // Pinned as a string so any wording edit shows up in review; the
        // thresholds above were fitted on exactly this text.
        let req = grounding_request(
            "Rapamycin extended lifespan in mice.",
            &[("c1".to_string(), "Rapamycin extends lifespan.".to_string())],
        );
        assert_eq!(
            serde_json::to_string(&req.state).unwrap(),
            r#"{"passage":"Rapamycin extended lifespan in mice."}"#
        );
        assert_eq!(
            serde_json::to_string(&req.questions_wire()).unwrap(),
            r#"{"c1":{"type":"noul","instructions":{"claim":"Rapamycin extends lifespan.","question":"Does `passage` directly support `claim`?"},"criteria":{"true":"The passage states or directly implies the claim.","false":"The passage does not state the claim, contradicts it, or only discusses the topic."}}}"#
        );
    }

    #[test]
    fn relevance_wording_is_pinned_and_passages_ride_in_instructions() {
        let long = "x".repeat(RELEVANCE_PASSAGE_CHARS + 50);
        let req = relevance_request(
            "Where does Noemi work?",
            &[
                ("r0".to_string(), "Noemi joined Halden Labs."),
                ("r1".to_string(), long.as_str()),
            ],
        );
        req.validate().unwrap();
        assert_eq!(
            serde_json::to_string(&req.state).unwrap(),
            r#"{"query":"Where does Noemi work?"}"#
        );
        assert_eq!(
            serde_json::to_string(&req.questions[0].1).unwrap(),
            r#"{"type":"noul","instructions":{"passage":"Noemi joined Halden Labs.","question":"Does `passage` contain information that answers `query`?"}}"#
        );
        let Question::Noul { instructions, .. } = &req.questions[1].1 else {
            panic!("noul expected");
        };
        assert_eq!(
            instructions["passage"].as_str().unwrap().chars().count(),
            RELEVANCE_PASSAGE_CHARS
        );
    }

    #[test]
    fn facts_read_as_plain_sentences() {
        assert_eq!(
            fact_as_claim("Bob", "diagnosed_with", "HIV"),
            "Bob diagnosed with HIV"
        );
        assert_eq!(
            fact_as_claim(" Ana ", "lives_in", " Lisbon"),
            "Ana lives in Lisbon"
        );
    }

    #[test]
    fn the_uncertain_band_is_symmetric_and_inclusive() {
        assert_eq!(grounding_verdict(0.44), Grounding::Unsupported);
        assert_eq!(grounding_verdict(0.45), Grounding::Uncertain);
        assert_eq!(grounding_verdict(0.5), Grounding::Uncertain);
        assert_eq!(grounding_verdict(0.55), Grounding::Uncertain);
        assert_eq!(grounding_verdict(0.56), Grounding::Supported);
        assert_eq!(grounding_verdict(0.0), Grounding::Unsupported);
        assert_eq!(grounding_verdict(1.0), Grounding::Supported);
    }

    #[test]
    fn pair_request_carries_each_existing_fact_in_its_instructions() {
        let req = pair_request(
            "Ana moved from Lisbon to Berlin last month.",
            &FactTriple::new("Ana", "lives_in", "Berlin"),
            &[
                (
                    "f-1".to_string(),
                    FactTriple::new("Ana", "lives_in", "Lisbon"),
                ),
                (
                    "f.2".to_string(),
                    FactTriple::new("Ana", "works_at", "Siemens"),
                ),
            ],
        );
        assert_eq!(req.questions.len(), 8);
        req.validate().unwrap();
        assert_eq!(
            serde_json::to_string(&req.state).unwrap(),
            r#"{"new_fact":{"object":"Berlin","predicate":"lives_in","subject":"Ana"},"new_text":"Ana moved from Lisbon to Berlin last month."}"#
        );
        let (id, q) = &req.questions[2];
        assert_eq!(id, "contradicts.f-1");
        assert_eq!(
            serde_json::to_string(q).unwrap(),
            r#"{"type":"noul","instructions":{"existing":{"object":"Lisbon","predicate":"lives_in","subject":"Ana"},"question":"Do `new_fact` and `existing` contradict each other?"},"criteria":{"true":"They cannot both be true of this subject at the same time","false":"Both can be true at once: different aspects, an additional value, or compatible detail"}}"#
        );
        // No question refers to a state array by index.
        let wire = serde_json::to_string(&req.questions_wire()).unwrap();
        assert!(!wire.contains('['));
    }

    fn response(pairs: &[(&str, f32)]) -> JudgeResponse {
        JudgeResponse {
            provider: "stub",
            model: "stub".into(),
            answers: pairs
                .iter()
                .map(|(id, p)| (id.to_string(), Answer::Noul(*p)))
                .collect::<BTreeMap<_, _>>(),
            input_tokens: 0,
            cost_usd: 0.0,
            latency_ms: 0,
            requests: 1,
        }
    }

    #[test]
    fn pair_answers_need_all_four_signals() {
        let full = response(&[
            ("restates.f1", 0.02),
            ("same_slot.f1", 0.98),
            ("contradicts.f1", 0.87),
            ("old_ended.f1", 0.88),
        ]);
        let answers = PairAnswers::from_response(&full, "f1").unwrap();
        assert_eq!(
            decide_pair(&answers),
            PairSignal {
                restates: false,
                same_slot: true,
                contradicts: true,
                old_ended: true,
            }
        );
        let partial = response(&[("restates.f1", 0.02), ("same_slot.f1", 0.98)]);
        assert!(matches!(
            PairAnswers::from_response(&partial, "f1"),
            Err(JudgeError::MissingAnswer { .. })
        ));
    }

    #[test]
    fn different_slots_are_only_declared_when_confident() {
        let signal = |same_slot| {
            decide_pair(&PairAnswers {
                restates: 0.0,
                same_slot,
                contradicts: 0.0,
                old_ended: 0.0,
            })
            .same_slot
        };
        assert!(!signal(0.05));
        assert!(!signal(0.19));
        assert!(signal(0.2));
        assert!(signal(0.6));
    }
}
