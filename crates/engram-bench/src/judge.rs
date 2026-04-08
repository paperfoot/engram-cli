//! LLM-as-judge scoring for open-ended QA answers.
//!
//! Follows the LongMemEval official evaluation pattern: a judge LLM reads the
//! question, the gold answer, and the candidate answer, and returns a binary
//! correctness verdict. This is more robust than exact-match for free-form
//! text where multiple phrasings can be correct.
//!
//! Prompt template adapted from https://github.com/xiaowu0162/LongMemEval

use engram_llm::{ChatLlm, ChatMessage};

#[derive(Debug, Clone)]
pub struct JudgeVerdict {
    pub correct: bool,
    pub raw_response: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

const JUDGE_SYSTEM: &str =
    "You are an expert evaluator. You will be given a question, a reference answer, and a candidate answer. \
     Decide whether the candidate answer is factually correct and addresses the question, using the \
     reference answer as ground truth.\n\n\
     EQUIVALENCE RULES — the candidate is CORRECT if any of these hold:\n\
     1. Same fact, different wording. 'Business Administration degree' ≡ 'BA in Business Administration'.\n\
     2. Numeric consistency. '45 minutes each way' ≡ '90 minutes round trip' ≡ '90 minutes total' \
        (45 × 2 = 90; one-way and total describe the same commute).\n\
     3. Unit conversions. '1.5 hours' ≡ '90 minutes' ≡ '1 hour 30 minutes'. '2 weeks' ≡ '14 days'.\n\
     4. Partial match that covers the key fact. Gold='Target' and candidate='Target store' or \
        'at Target' is CORRECT. Gold='John Smith' and candidate='John' is CORRECT unless ambiguity matters.\n\
     5. Superset answers. If the gold is a single item from a list and the candidate names that item \
        (possibly alongside others that are also true), it is CORRECT.\n\
     6. Format differences. Date '2024-03-15' ≡ 'March 15, 2024' ≡ '15 March 2024'.\n\n\
     The candidate is INCORRECT if:\n\
     - It contradicts the reference on a key fact (wrong name, wrong number, wrong entity).\n\
     - It hallucinates facts not in the reference.\n\
     - It refuses to answer ('I don't know', 'not in the context', etc.) when the reference contains \
       a specific, answerable fact. Refusal is always INCORRECT when the question has a definite answer.\n\
     - It fails to address the question at all.\n\n\
     Think briefly about whether the candidate satisfies any of the equivalence rules above. \
     Then respond with exactly one word on the final line: CORRECT or INCORRECT.";

fn build_judge_user(question: &str, gold: &str, candidate: &str) -> String {
    format!(
        "Question: {question}\n\nReference answer: {gold}\n\nCandidate answer: {candidate}\n\nIs the candidate correct?",
        question = question.trim(),
        gold = gold.trim(),
        candidate = candidate.trim()
    )
}

pub async fn judge_answer<J: ChatLlm + ?Sized>(
    judge: &J,
    question: &str,
    gold_answer: &str,
    candidate_answer: &str,
) -> Result<JudgeVerdict, engram_llm::LlmError> {
    let messages = vec![
        ChatMessage::system(JUDGE_SYSTEM),
        ChatMessage::user(build_judge_user(question, gold_answer, candidate_answer)),
    ];
    let resp = judge.chat(&messages).await?;
    let raw = resp.content.clone();
    let upper = raw.to_ascii_uppercase();
    // Look at the last line for the verdict; fall back to a substring search
    // if the model was verbose and didn't obey the format.
    let last_line_verdict = upper
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_default();
    let correct = if last_line_verdict.contains("CORRECT") && !last_line_verdict.contains("INCORRECT") {
        true
    } else if last_line_verdict.contains("INCORRECT") {
        false
    } else if upper.contains("CORRECT") && !upper.contains("INCORRECT") {
        true
    } else {
        false
    };
    Ok(JudgeVerdict {
        correct,
        raw_response: raw,
        prompt_tokens: resp.prompt_tokens,
        completion_tokens: resp.completion_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_correct_verdict() {
        // unit test for the parsing logic only — no network.
        let raw = "The candidate gives the same answer as the reference.\n\nCORRECT".to_string();
        assert!(raw.to_ascii_uppercase().lines().rev().any(|l| l.trim() == "CORRECT"));
    }
}
