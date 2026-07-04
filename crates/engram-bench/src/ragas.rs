//! RAGAS metrics: faithfulness, answer relevance, context precision, context recall.
//!
//! All four metrics use an LLM-as-judge approach. They are orthogonal: a
//! high-faithfulness answer with low relevance means the model faithfully
//! stuck to the context but didn't actually answer the question. A high
//! context-recall but low context-precision means we retrieved the right
//! info but also a lot of noise.
//!
//! Reference: https://docs.ragas.io/en/latest/concepts/metrics/
//!
//! Implementation notes:
//! - Faithfulness: extract claims from the answer, ask the judge whether each
//!   claim is supported by the context. Score = supported / total.
//! - Answer relevance: ask the judge to generate N questions that the answer
//!   would plausibly answer, then score by whether those match the original
//!   question. Simplified here to a single LLM-score on a 0-1 scale.
//! - Context precision: for each retrieved chunk, ask the judge if it helps
//!   answer the question. Score = helpful / total.
//! - Context recall: compare the retrieved context to the gold answer; ask
//!   the judge whether every sentence in the gold answer is supported.

use engram_llm::{ChatLlm, ChatMessage, LlmError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RagasMetrics {
    pub faithfulness: f32,
    pub answer_relevance: f32,
    pub context_precision: f32,
    pub context_recall: f32,
}

/// Ask the judge to rate a score 0.0 to 1.0, return f32.
async fn score_01<J: ChatLlm + ?Sized>(
    judge: &J,
    system: &str,
    user: &str,
) -> Result<f32, LlmError> {
    let resp = judge
        .chat(&[ChatMessage::system(system), ChatMessage::user(user)])
        .await?;
    // Pull the first float-looking token.
    let mut best: Option<f32> = None;
    for token in resp
        .content
        .split(|c: char| !c.is_ascii_digit() && c != '.')
    {
        if let Ok(v) = token.parse::<f32>() {
            if (0.0..=1.0).contains(&v) {
                best = Some(v);
                break;
            }
        }
    }
    Ok(best.unwrap_or(0.0))
}

const FAITHFULNESS_DECOMPOSE_SYSTEM: &str =
    "You break an ANSWER down into the minimal set of standalone factual claims it asserts.\n\
     Rules:\n\
     - Each claim must be a single, atomic statement that can be verified on its own.\n\
     - Resolve pronouns and references so every claim is fully self-contained.\n\
     - Do NOT add any information that is not stated in the answer.\n\
     - Output ONE claim per line, with no numbering, bullets, or commentary.\n\
     - If the answer makes no verifiable factual claim (for example it refuses or says it \
       does not know), output nothing at all.";

const FAITHFULNESS_VERIFY_SYSTEM: &str =
    "You decide whether a CLAIM can be directly inferred from the given CONTEXT.\n\
     Answer YES only if the context contains enough information to support the claim.\n\
     Answer NO if the claim contradicts the context, or if the context is silent about it — \
     a claim that merely sounds plausible but is not stated is NOT supported.\n\
     Respond with exactly one word on the final line: YES or NO.";

/// Canonical RAGAS faithfulness (arXiv:2309.15217).
///
/// Two-phase LLM-as-judge, all calls on the same caller-supplied `judge`:
///   1. One judge call decomposes the answer into atomic, self-contained claims.
///   2. One judge call per claim decides whether the retrieved `context` supports it.
///
/// Faithfulness = supported_claims / total_claims. An answer that makes no
/// verifiable claim (e.g. a refusal or "I don't know") scores 1.0, matching the
/// reference metric's "no statements ⇒ vacuously faithful" convention.
pub async fn faithfulness<J: ChatLlm + ?Sized>(
    judge: &J,
    answer: &str,
    context: &str,
) -> Result<f32, LlmError> {
    let claims = decompose_claims(judge, answer).await?;
    if claims.is_empty() {
        // No factual claims to verify ⇒ nothing can be unfaithful.
        return Ok(1.0);
    }
    let mut supported = 0usize;
    for claim in &claims {
        if verify_claim(judge, context, claim).await? {
            supported += 1;
        }
    }
    Ok(supported as f32 / claims.len() as f32)
}

/// Phase 1: ask the judge to split the answer into atomic claims.
async fn decompose_claims<J: ChatLlm + ?Sized>(
    judge: &J,
    answer: &str,
) -> Result<Vec<String>, LlmError> {
    if answer.trim().is_empty() {
        return Ok(Vec::new());
    }
    let user = format!("ANSWER:\n{}\n\nClaims (one per line):", answer.trim());
    let resp = judge
        .chat(&[
            ChatMessage::system(FAITHFULNESS_DECOMPOSE_SYSTEM),
            ChatMessage::user(user),
        ])
        .await?;
    Ok(parse_claims(&resp.content))
}

/// Phase 2: ask the judge whether a single claim is supported by the context.
async fn verify_claim<J: ChatLlm + ?Sized>(
    judge: &J,
    context: &str,
    claim: &str,
) -> Result<bool, LlmError> {
    let user = format!(
        "CONTEXT:\n{}\n\nCLAIM:\n{}\n\nIs the claim supported by the context? Answer YES or NO:",
        context.trim(),
        claim.trim()
    );
    let resp = judge
        .chat(&[
            ChatMessage::system(FAITHFULNESS_VERIFY_SYSTEM),
            ChatMessage::user(user),
        ])
        .await?;
    Ok(parse_supported(&resp.content))
}

/// Parse the decomposition response into atomic claims: one per non-empty line,
/// with any leading list marker ("-", "*", "•", "1.", "2)") stripped. The judge
/// is asked for bare lines, so the stripping is purely defensive.
fn parse_claims(raw: &str) -> Vec<String> {
    raw.lines()
        .map(strip_list_marker)
        .filter(|c| !c.is_empty())
        .collect()
}

fn strip_list_marker(line: &str) -> String {
    let t = line.trim();
    for bullet in ["- ", "* ", "• ", "· ", "– "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            return rest.trim().to_string();
        }
    }
    // Numeric enumerator like "1. " or "2) ". Require a trailing space so we
    // never mangle a claim that legitimately starts with a number ("3.5 mg ...").
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = &t[digits.len()..];
        if let Some(after) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return after.trim().to_string();
        }
        if rest == "." || rest == ")" {
            return String::new();
        }
    }
    t.to_string()
}

/// Parse a YES/NO support verdict. Mirrors `judge.rs`: inspect the last non-empty
/// line first, then fall back to scanning the whole response. Matching is done on
/// whole words so substrings like "NO" inside "KNOWN"/"CANNOT" don't mislead.
/// Anything unparseable is treated as NOT supported (conservative, like the
/// judge's "when in doubt, INCORRECT").
fn parse_supported(raw: &str) -> bool {
    let upper = raw.to_ascii_uppercase();
    let has = |s: &str, word: &str| {
        s.split(|c: char| !c.is_ascii_alphabetic())
            .any(|w| w == word)
    };
    let last = upper
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    if has(last, "YES") && !has(last, "NO") {
        true
    } else if has(last, "NO") {
        false
    } else {
        has(&upper, "YES") && !has(&upper, "NO")
    }
}

pub async fn answer_relevance<J: ChatLlm + ?Sized>(
    judge: &J,
    question: &str,
    answer: &str,
) -> Result<f32, LlmError> {
    let system =
        "You rate how relevant an answer is to a question. A relevant answer directly addresses \
         the question's intent. An irrelevant answer dodges, rambles, or answers a different \
         question. Respond with a single number between 0.0 and 1.0 (1.0 = perfectly on-topic).";
    let user = format!(
        "QUESTION:\n{}\n\nANSWER:\n{}\n\nAnswer relevance score:",
        question.trim(),
        answer.trim()
    );
    score_01(judge, system, &user).await
}

pub async fn context_precision<J: ChatLlm + ?Sized>(
    judge: &J,
    question: &str,
    context: &str,
) -> Result<f32, LlmError> {
    let system =
        "You rate how precise a retrieved context is for answering a question. Precision is high \
         when most of the context is directly useful for answering. Precision is low when the \
         context contains a lot of irrelevant material. \
         Respond with a single number between 0.0 and 1.0 (1.0 = every retrieved passage is relevant).";
    let user = format!(
        "QUESTION:\n{}\n\nRETRIEVED CONTEXT:\n{}\n\nContext precision score:",
        question.trim(),
        context.trim()
    );
    score_01(judge, system, &user).await
}

pub async fn context_recall<J: ChatLlm + ?Sized>(
    judge: &J,
    gold_answer: &str,
    context: &str,
) -> Result<f32, LlmError> {
    let system =
        "You rate how well a retrieved context covers the facts needed to produce a reference \
         answer. Recall is high when every factual claim in the reference answer is supported by \
         the context. Recall is low when the retrieval missed important facts. \
         Respond with a single number between 0.0 and 1.0 (1.0 = every fact in the reference is in the context).";
    let user = format!(
        "REFERENCE ANSWER:\n{}\n\nRETRIEVED CONTEXT:\n{}\n\nContext recall score:",
        gold_answer.trim(),
        context.trim()
    );
    score_01(judge, system, &user).await
}

pub async fn compute_all<J: ChatLlm + ?Sized>(
    judge: &J,
    question: &str,
    gold_answer: &str,
    candidate_answer: &str,
    context: &str,
) -> Result<RagasMetrics, LlmError> {
    let faith = faithfulness(judge, candidate_answer, context).await?;
    let rel = answer_relevance(judge, question, candidate_answer).await?;
    let prec = context_precision(judge, question, context).await?;
    let rec = context_recall(judge, gold_answer, context).await?;
    Ok(RagasMetrics {
        faithfulness: faith,
        answer_relevance: rel,
        context_precision: prec,
        context_recall: rec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use engram_llm::ChatResponse;

    // ---- pure parser tests (no LLM) -------------------------------------

    #[test]
    fn parse_claims_strips_markers_and_blank_lines() {
        let raw = "- First claim.\n2. Second claim.\n\n  • Third claim.\nFourth claim.\n   ";
        assert_eq!(
            parse_claims(raw),
            vec![
                "First claim.".to_string(),
                "Second claim.".to_string(),
                "Third claim.".to_string(),
                "Fourth claim.".to_string(),
            ]
        );
    }

    #[test]
    fn parse_claims_preserves_leading_numbers_that_are_not_enumerators() {
        // A decimal/measurement must survive intact (no "3.5 mg" -> "5 mg" bug).
        assert_eq!(
            parse_claims("3.5 mg of the drug was given.\n1920 was a leap year."),
            vec![
                "3.5 mg of the drug was given.".to_string(),
                "1920 was a leap year.".to_string(),
            ]
        );
    }

    #[test]
    fn parse_claims_empty_when_no_claims() {
        assert!(parse_claims("").is_empty());
        assert!(parse_claims("   \n  \n").is_empty());
    }

    #[test]
    fn parse_supported_handles_verdict_forms() {
        assert!(parse_supported("YES"));
        assert!(parse_supported("Some reasoning about the context.\nYES"));
        assert!(!parse_supported("NO"));
        assert!(!parse_supported("The context is silent on this.\nNO"));
        // Whole-word matching: "KNOWN" must not register as NO; final YES wins.
        assert!(parse_supported("This is KNOWN from the context.\nYES"));
        // Unparseable verdict => conservatively not supported.
        assert!(!parse_supported("maybe"));
        assert!(!parse_supported(""));
    }

    // ---- end-to-end faithfulness with an offline stub judge -------------

    /// Deterministic offline judge. The decomposition response is configurable;
    /// verification answers YES for every claim except the "cheese" one. Because
    /// `faithfulness` is generic over `ChatLlm`, this stub fully exercises the
    /// two-phase flow with no network and no API key.
    struct StubJudge {
        claims: &'static str,
    }

    #[async_trait::async_trait]
    impl ChatLlm for StubJudge {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn model_id(&self) -> &str {
            "stub-judge"
        }
        async fn chat(&self, messages: &[ChatMessage]) -> Result<ChatResponse, LlmError> {
            let user = messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.as_str())
                .unwrap_or("");
            let content = if user.contains("Claims (one per line):") {
                self.claims.to_string()
            } else {
                // Verification call. The test context never mentions "cheese",
                // so its presence uniquely identifies the unsupported claim.
                if user.contains("cheese") {
                    "Reasoning: the context says nothing about that.\nNO".to_string()
                } else {
                    "YES".to_string()
                }
            };
            Ok(ChatResponse {
                content,
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                model: "stub-judge".into(),
            })
        }
    }

    #[test]
    fn faithfulness_is_supported_over_total() {
        // Three claims (with deliberate bullet/number noise to drive the parser);
        // two are supported, the "cheese" one is not => 2/3.
        let judge = StubJudge {
            claims: "- The sky is blue.\n2. Grass is green.\nThe moon is made of cheese.",
        };
        let f = block_on(faithfulness(
            &judge,
            "The sky is blue, grass is green, and the moon is made of cheese.",
            "The sky is blue and the grass is green.",
        ))
        .unwrap();
        assert!((f - 2.0 / 3.0).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn faithfulness_all_supported_is_one() {
        let judge = StubJudge {
            claims: "The sky is blue.\nGrass is green.",
        };
        let f = block_on(faithfulness(
            &judge,
            "The sky is blue and grass is green.",
            "The sky is blue and the grass is green.",
        ))
        .unwrap();
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn faithfulness_no_claims_is_one() {
        // Judge extracts no claims (e.g. a refusal) => vacuously faithful (1.0),
        // and no verification calls are made.
        let judge = StubJudge { claims: "" };
        let f = block_on(faithfulness(&judge, "I don't know.", "irrelevant context")).unwrap();
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn faithfulness_empty_answer_short_circuits_to_one() {
        // A blank answer never reaches the judge at all.
        let judge = StubJudge {
            claims: "should not be used",
        };
        let f = block_on(faithfulness(&judge, "   ", "any context")).unwrap();
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
    }

    /// Minimal single-threaded executor so the async metric can be exercised
    /// without a runtime dependency. The stub judge never yields `Pending`, so a
    /// busy-poll with a no-op waker is sufficient and deterministic. (Same idiom
    /// as `citation.rs`.)
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
}
