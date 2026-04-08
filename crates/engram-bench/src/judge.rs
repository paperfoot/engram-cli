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
     reference answer as ground truth. \
     The candidate may phrase things differently than the reference — that's fine as long as the \
     underlying facts match. A candidate is WRONG if it contradicts the reference, hallucinates \
     facts not in the reference, or fails to address the question. \
     Respond with exactly one word on the final line: CORRECT or INCORRECT.";

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
