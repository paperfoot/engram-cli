//! ALCE-style citation quality metrics (citation recall + citation precision).
//!
//! Implements the citation evaluation from "Enabling Large Language Models to
//! Generate Text with Citations" (Gao et al., arXiv:2305.14627). The model
//! output is decomposed into *statements* (sentences), each carrying the set of
//! chunk texts it cites. An NLI entailment judge φ decides whether a premise
//! (concatenated citations) entails a hypothesis (the statement).
//!
//! Citation **recall** asks: does the statement's cited evidence actually
//! support the statement? Citation **precision** asks: of the citations a
//! statement carries, how many are load-bearing (removing one would break the
//! entailment, and that citation alone does not support the statement)?
//!
//! The entailment judge is injected as a generic async closure so this module
//! stays dependency-free and deterministically unit-testable with a stub φ —
//! no network, no model, no API keys.

use crate::error::BenchError;
use std::future::Future;

/// A single statement (sentence) from a generated answer together with the
/// texts of the chunks it cites. `citations` holds the cited chunk *texts*
/// (the premise material), not ids.
#[derive(Debug, Clone)]
pub struct Statement {
    pub text: String,
    pub citations: Vec<String>,
}

/// Aggregate citation-quality scores over a set of statements.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CitationScores {
    /// Mean citation recall over all statements.
    pub recall: f32,
    /// Citation precision (ALCE): load-bearing citations / total citations,
    /// where the denominator counts the citations of EVERY cited statement
    /// (supported or not) and the numerator only credits load-bearing
    /// citations on statements whose evidence entails them.
    pub precision: f32,
    /// Number of statements scored (the recall denominator).
    pub n: usize,
}

/// Split `text` into sentences deterministically.
///
/// A sentence boundary is a run of one or more of `. ! ?` followed by
/// whitespace or end-of-text. Trailing terminators are kept on the sentence.
/// Whitespace-only fragments are dropped. This is intentionally simple and
/// stable rather than locale-aware — the benchmark only needs reproducible,
/// order-preserving segmentation.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            // Consume any further consecutive terminators (e.g. "?!", "...").
            while let Some(&next) = chars.peek() {
                if matches!(next, '.' | '!' | '?') {
                    current.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            // A boundary only closes the sentence if followed by whitespace or
            // end-of-text, so decimals like "3.5" stay together.
            match chars.peek() {
                Some(&next) if !next.is_whitespace() => {}
                _ => {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        sentences.push(trimmed.to_string());
                    }
                    current.clear();
                }
            }
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        sentences.push(trimmed.to_string());
    }
    sentences
}

/// Join cited chunk texts into a single premise for the entailment judge.
fn concat(citations: &[String]) -> String {
    citations.join("\n")
}

/// Compute ALCE citation recall + precision over `statements`.
///
/// `entails(premise, hypothesis)` is the NLI judge φ: it returns `true` iff
/// `premise` entails `hypothesis`.
///
/// Note: ALCE truncates each statement's citation list to `at_most_citations`
/// (commonly 3) before counting, which affects both the precision denominator
/// and the load-bearing loop. This function does NOT cap — citation-list
/// truncation is the **caller's** responsibility and must be applied to the
/// `Statement`s passed in to match ALCE numbers (e.g. BioGen caps via
/// `MAX_CITATIONS_PER_SENTENCE`).
///
/// - **Recall** for statement *i* is `1` iff `C_i` is non-empty and
///   `φ(concat(C_i), s_i)` holds; otherwise `0`. The reported `recall` is the
///   mean over all statements.
/// - **Precision** is `load_bearing_citations / total_citations` (ALCE). The
///   denominator `total_citations` sums `|C_i|` over EVERY statement that
///   carries citations, supported or not — an unsupported but cited statement
///   adds to the denominator and nothing to the numerator. A citation `c` is
///   *irrelevant* iff `φ(c, s_i) == false` and `φ(concat(C_i \ {c}), s_i) ==
///   true` (i.e. `c` alone does not support the statement, yet the statement
///   is still supported without it); load-bearing citations (those that are
///   not irrelevant) are credited only on statements whose evidence entails
///   them (recall `1`).
pub async fn citation_scores<F, Fut>(
    entails: F,
    statements: &[Statement],
) -> Result<CitationScores, BenchError>
where
    F: Fn(String, String) -> Fut + Copy,
    Fut: Future<Output = Result<bool, BenchError>>,
{
    let n = statements.len();
    if n == 0 {
        return Ok(CitationScores::default());
    }

    let mut recall_sum = 0f32;
    let mut load_bearing_citations = 0usize;
    let mut total_citations = 0usize;

    for stmt in statements {
        let recalled = if stmt.citations.is_empty() {
            false
        } else {
            entails(concat(&stmt.citations), stmt.text.clone()).await?
        };

        // Precision denominator mirrors ALCE (eval.py): EVERY statement that
        // carries citations contributes `len(ref)` to `total_citations`,
        // regardless of whether its evidence entails it. An unsupported but
        // cited statement therefore adds to the denominator and 0 to the
        // numerator, correctly dragging precision down — citing irrelevant
        // material on an unsupported sentence is penalised.
        total_citations += stmt.citations.len();

        if recalled {
            recall_sum += 1.0;

            // ALCE skips the overcite/irrelevance test entirely for
            // single-citation statements (len(ref) <= 1) and credits the lone
            // citation directly: since concat(C_i) == c, the statement being
            // recalled means c alone supports it, so it is always load-bearing.
            // Short-circuiting here avoids a redundant φ call per
            // single-citation sentence (the most common case).
            if stmt.citations.len() == 1 {
                load_bearing_citations += 1;
                continue;
            }

            // The load-bearing numerator is only accumulated for statements
            // whose evidence supports them (joint_entail == 1 in ALCE);
            // unsupported statements contribute 0 here.
            for (idx, c) in stmt.citations.iter().enumerate() {
                // c is irrelevant iff it does not support s on its own AND the
                // remaining citations still support s.
                let c_alone = entails(c.clone(), stmt.text.clone()).await?;
                let irrelevant = if c_alone {
                    false
                } else {
                    // This loop only runs for statements with >= 2 citations
                    // (the single-citation case is short-circuited above), so
                    // `rest` always holds at least one citation.
                    let rest: Vec<String> = stmt
                        .citations
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != idx)
                        .map(|(_, s)| s.clone())
                        .collect();
                    entails(concat(&rest), stmt.text.clone()).await?
                };

                if !irrelevant {
                    load_bearing_citations += 1;
                }
            }
        }
    }

    let recall = recall_sum / n as f32;
    let precision = if total_citations == 0 {
        0.0
    } else {
        load_bearing_citations as f32 / total_citations as f32
    };

    Ok(CitationScores {
        recall,
        precision,
        n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic stub φ: `premise` entails `hypothesis` iff every
    /// lowercase whitespace token of the hypothesis appears in the premise's
    /// lowercase token set. No network, fully reproducible.
    async fn stub_entails(premise: String, hypothesis: String) -> Result<bool, BenchError> {
        let prem: std::collections::HashSet<String> = premise
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let mut hyp = hypothesis.to_lowercase();
        // Tokens compare bare words; strip trailing sentence punctuation so
        // "blue." matches "blue" in the premise.
        hyp.retain(|c| !matches!(c, '.' | '!' | '?' | ',' | ';' | ':'));
        let ok = hyp.split_whitespace().all(|tok| prem.contains(tok));
        Ok(ok)
    }

    fn stmt(text: &str, citations: &[&str]) -> Statement {
        Statement {
            text: text.to_string(),
            citations: citations.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn split_sentences_basic() {
        let out = split_sentences("The sky is blue. Grass is green! Is water wet?");
        assert_eq!(
            out,
            vec![
                "The sky is blue.".to_string(),
                "Grass is green!".to_string(),
                "Is water wet?".to_string(),
            ]
        );
    }

    #[test]
    fn split_sentences_keeps_decimals_together() {
        let out = split_sentences("The dose is 3.5 mg. Take it daily.");
        assert_eq!(
            out,
            vec![
                "The dose is 3.5 mg.".to_string(),
                "Take it daily.".to_string()
            ]
        );
    }

    #[test]
    fn split_sentences_handles_no_terminator_and_whitespace() {
        assert_eq!(
            split_sentences("no terminator here"),
            vec!["no terminator here".to_string()]
        );
        assert!(split_sentences("   \n  ").is_empty());
        assert!(split_sentences("").is_empty());
        // Collapses runs of terminators into one boundary.
        assert_eq!(
            split_sentences("Really?! Yes..."),
            vec!["Really?!".to_string(), "Yes...".to_string()]
        );
    }

    #[test]
    fn empty_statements_yield_default() {
        let scores = futures_executor_block_on(citation_scores(stub_entails, &[])).unwrap();
        assert_eq!(scores, CitationScores::default());
    }

    #[test]
    fn perfect_recall_and_precision() {
        // Each statement is fully supported by its single citation, and that
        // citation is load-bearing (it alone supports the statement, so it is
        // never "irrelevant").
        let statements = vec![
            stmt("the sky is blue", &["the sky is blue today"]),
            stmt("grass is green", &["fresh grass is green"]),
        ];
        let scores = futures_executor_block_on(citation_scores(stub_entails, &statements)).unwrap();
        assert_eq!(scores.n, 2);
        assert!((scores.recall - 1.0).abs() < 1e-6);
        assert!((scores.precision - 1.0).abs() < 1e-6);
    }

    #[test]
    fn missing_citation_drops_recall() {
        // Second statement has no citation -> recall 0 for it; first is fine.
        let statements = vec![
            stmt("the sky is blue", &["the sky is blue today"]),
            stmt("grass is green", &[]),
        ];
        let scores = futures_executor_block_on(citation_scores(stub_entails, &statements)).unwrap();
        assert_eq!(scores.n, 2);
        assert!((scores.recall - 0.5).abs() < 1e-6);
        // The empty-citation statement contributes 0 to the denominator (ALCE
        // excludes len(ref)==0), so precision = 1 load-bearing / 1 total = 1.0.
        assert!((scores.precision - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unsupported_citation_breaks_recall() {
        // The lone citation does NOT contain all tokens of the statement, so
        // φ(concat(C), s) is false -> recall 0. The statement still carries a
        // citation, so (ALCE) total_citations == 1 while the numerator stays 0
        // -> precision 0/1 = 0.0.
        let statements = vec![stmt("the sky is blue", &["water is wet"])];
        let scores = futures_executor_block_on(citation_scores(stub_entails, &statements)).unwrap();
        assert_eq!(scores.n, 1);
        assert_eq!(scores.recall, 0.0);
        assert_eq!(scores.precision, 0.0);
    }

    #[test]
    fn unsupported_cited_statement_drags_precision() {
        // The case that distinguishes faithful ALCE from a recall-gated
        // denominator. S1 is supported by its citation (load-bearing); S2 cites
        // material that does NOT entail it (unsupported). ALCE counts BOTH
        // statements' citations in the denominator (1 + 1 = 2) but only credits
        // the load-bearing citation on the supported S1 (numerator 1), so
        // precision = 1/2 = 0.5. A recall-gated denominator would wrongly report
        // 1/1 = 1.0. Recall is 1 supported of 2 statements = 0.5.
        let statements = vec![
            stmt("the sky is blue", &["the sky is blue"]),
            stmt("the sky is blue", &["water is wet"]),
        ];
        let scores = futures_executor_block_on(citation_scores(stub_entails, &statements)).unwrap();
        assert_eq!(scores.n, 2);
        assert!(
            (scores.recall - 0.5).abs() < 1e-6,
            "recall {}",
            scores.recall
        );
        assert!(
            (scores.precision - 0.5).abs() < 1e-6,
            "precision {} (expected ALCE 0.5, not recall-gated 1.0)",
            scores.precision
        );
    }

    #[test]
    fn irrelevant_citation_lowers_precision() {
        // Statement "the sky is blue".
        //   C0 = "the sky is blue" supports it ALONE -> load-bearing.
        //   C1 = "water is wet"   does NOT support it alone, AND the rest (C0)
        //        still supports it -> irrelevant -> not load-bearing.
        // concat(C0,C1) still contains all statement tokens -> recall 1.
        // precision = 1 load-bearing / 2 total = 0.5.
        let statements = vec![stmt(
            "the sky is blue",
            &["the sky is blue", "water is wet"],
        )];
        let scores = futures_executor_block_on(citation_scores(stub_entails, &statements)).unwrap();
        assert_eq!(scores.n, 1);
        assert!((scores.recall - 1.0).abs() < 1e-6);
        assert!(
            (scores.precision - 0.5).abs() < 1e-6,
            "got {}",
            scores.precision
        );
    }

    #[test]
    fn jointly_required_citations_are_both_load_bearing() {
        // Statement needs tokens from BOTH citations; neither supports it alone
        // and removing either breaks entailment, so neither is irrelevant.
        // precision = 2/2 = 1.0, recall = 1.0.
        let statements = vec![stmt("sky blue grass green", &["sky blue", "grass green"])];
        let scores = futures_executor_block_on(citation_scores(stub_entails, &statements)).unwrap();
        assert!((scores.recall - 1.0).abs() < 1e-6);
        assert!((scores.precision - 1.0).abs() < 1e-6);
    }

    /// Minimal single-threaded executor so the async metric can be exercised
    /// without pulling in a runtime dependency. Polls the future to completion;
    /// the stub φ never yields `Pending`, so a busy-poll with a no-op waker is
    /// sufficient and deterministic.
    fn futures_executor_block_on<T>(fut: impl Future<Output = T>) -> T {
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
}
