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
    "You are a strict scientific evaluator. Precision matters — partial, vague, or \
     summarised answers are NOT acceptable.\n\n\
     You will be given a question, a reference answer, and a candidate answer. Decide \
     whether the candidate is CORRECT using the reference as ground truth.\n\n\
     CORRECT if:\n\
     1. The candidate states the same fact(s) as the reference, possibly worded differently. \
        'Business Administration degree' ≡ 'BA in Business Administration'.\n\
     2. Numeric equivalence. '1.5 hours' ≡ '90 minutes'. '45 minutes each way' ≡ '90 minutes total'.\n\
     3. Date format differences. '2024-03-15' ≡ 'March 15, 2024'. '13 August' ≡ '13 August, 2023' \
        (adding a year that is contextually correct is fine).\n\
     4. LIST QUESTIONS — superset rule: if the reference lists N items and the candidate names \
        ALL N of them, the answer is CORRECT even if the candidate lists a few additional items \
        that are also true and in the same category as what was asked. However, if the extras \
        are wrong, irrelevant, or the candidate lists so many items it is clearly guessing, \
        mark INCORRECT. \
        Example: gold='beach, mountains, forest', candidate='beach, mountains, forest, canyon' → CORRECT. \
        Example: gold='running, pottery', candidate='running, pottery, sleeping, eating, walking' → INCORRECT (padding).\n\n\
     INCORRECT if:\n\
     - LIST QUESTIONS: the reference lists N items and the candidate names fewer than N. \
       Missing ANY gold item makes the answer INCORRECT, even if the items it does name are right. \
       Example: gold='beach, mountains, forest', candidate='beach' → INCORRECT (2 missing).\n\
     - The candidate gives a vague paraphrase instead of the specific fact. \
       Example: gold='mentors, family, friends', candidate='people who support her' → INCORRECT.\n\
     - The candidate hedges or equivocates. 'once or twice' when the gold is '2' → INCORRECT.\n\
     - The candidate contradicts the reference on any key fact.\n\
     - The candidate refuses ('I don't know') when the reference has a definite answer.\n\
     - The candidate gives incorrect specifics (wrong name, wrong number, wrong date).\n\n\
     When in doubt, mark INCORRECT. Scientific memory must be precise.\n\
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
    let correct =
        if last_line_verdict.contains("CORRECT") && !last_line_verdict.contains("INCORRECT") {
            true
        } else if last_line_verdict.contains("INCORRECT") {
            false
        } else {
            upper.contains("CORRECT") && !upper.contains("INCORRECT")
        };
    Ok(JudgeVerdict {
        correct,
        raw_response: raw,
        prompt_tokens: resp.prompt_tokens,
        completion_tokens: resp.completion_tokens,
    })
}

const COGNITIVE_JUDGE_SYSTEM: &str = r#"You are a Memory Awareness Judge.
Your task: Judge whether the Model Prediction considers or is linked to the Evidence. If there is a clear connection, the answer is correct (score 1); if not, it is wrong (no score).

Labels:
- "correct": The prediction explicitly or implicitly reflects/uses the evidence (memory or constraint). Give 1 point.
- "wrong": The prediction does not show such a link to the evidence. No point.

Memory/Evidence:
{evidence}

Model Prediction:
{pred}

Return your judgment strictly in JSON format:
{"label": "correct"|"wrong", "reason": "<Does the prediction relate to the evidence?>"}"#;

#[derive(serde::Deserialize)]
struct CognitiveJudgeJson {
    label: String,
    #[serde(default, rename = "reason")]
    _reason: Option<String>,
}

fn parse_cognitive_correct(raw: &str) -> bool {
    if let Ok(parsed) = serde_json::from_str::<CognitiveJudgeJson>(raw.trim()) {
        return parsed.label.eq_ignore_ascii_case("correct");
    }

    let lower = raw.to_ascii_lowercase();
    let correct_pos = lower.find("correct");
    let wrong_pos = lower.find("wrong");
    match (correct_pos, wrong_pos) {
        (Some(c), Some(w)) => c < w,
        (Some(_), None) => true,
        _ => false,
    }
}

pub async fn judge_answer_cognitive<J: ChatLlm + ?Sized>(
    judge: &J,
    evidence: &str,
    candidate: &str,
) -> Result<JudgeVerdict, engram_llm::LlmError> {
    let prompt = COGNITIVE_JUDGE_SYSTEM
        .replace("{evidence}", evidence.trim())
        .replace("{pred}", candidate.trim());
    let messages = vec![ChatMessage::system(prompt)];
    let resp = judge.chat(&messages).await?;
    let raw = resp.content.clone();
    let correct = parse_cognitive_correct(&raw);
    Ok(JudgeVerdict {
        correct,
        raw_response: raw,
        prompt_tokens: resp.prompt_tokens,
        completion_tokens: resp.completion_tokens,
    })
}

const MAB_STANDARD_JUDGE_SYSTEM: &str = "I will give you a question, a correct answer, and a model-generated answer. Please answer yes if the model-generated answer contains the correct answer. Otherwise, answer no. If the model response is something like 'cannot answer' or 'I don't know', or refuses to answer, please answer no. If the model-generated answer is correct but is just rephrased or has more details, please answer yes. If the model-generated answer only contains a subset of the information required by the answer, please answer no.";

const MAB_TEMPORAL_JUDGE_SYSTEM: &str = "I will give you a question, a correct answer, and a model-generated answer. Please answer yes if the model-generated answer contains the correct answer. Otherwise, answer no. If the model response is something like 'cannot answer' or 'I don't know', or refuses to answer, please answer no. If the model-generated answer is correct but is just rephrased or has more details, please answer yes. If the model-generated answer only contains a subset of the information required by the answer, please answer no. If the model's response is sufficient to answer the question, please answer yes. Please do not penalize off-by-one errors for the number of days when judging temporal questions.";

const MAB_KNOWLEDGE_UPDATE_JUDGE_SYSTEM: &str = "I will give you a question, a correct answer, and a model-generated answer. Please answer yes if the model-generated answer contains the correct answer. Otherwise, answer no. If the model response is something like 'cannot answer' or 'I don't know', or refuses to answer, please answer no. If the model-generated answer is correct but is just rephrased or has more details, please answer yes. If the model-generated answer only contains a subset of the information required by the answer, please answer no. Note that the answer might involve multiple updates to the same fact over time, and we are looking for the latest update. If the model-generated response contains some previous information about the fact along with an updated answer, the response should be considered as correct as long as the updated answer is the required answer.";

const MAB_PREFERENCE_JUDGE_SYSTEM: &str = "I will give you a question, a rubric for desired personalized response, and a model's response. Please answer yes if the model's response is consistent with the desired response. Please answer no otherwise.";

const MAB_ABSTENTION_JUDGE_SYSTEM: &str = "I will give you an unanswerable question, an explanation, and a model-generated answer. Please answer yes if model-generated answer indicates that the question is unanswerable. Please answer no otherwise.";

fn mab_prompt_for_question_type(question_type: &str) -> &'static str {
    match question_type {
        "factual" | "list" | "single_session_user" | "single_session_assistant" => {
            MAB_STANDARD_JUDGE_SYSTEM
        }
        "temporal" | "temporal_reasoning" => MAB_TEMPORAL_JUDGE_SYSTEM,
        "knowledge_update" => MAB_KNOWLEDGE_UPDATE_JUDGE_SYSTEM,
        "preference" => MAB_PREFERENCE_JUDGE_SYSTEM,
        "abstention" => MAB_ABSTENTION_JUDGE_SYSTEM,
        _ => MAB_STANDARD_JUDGE_SYSTEM,
    }
}

fn build_mab_judge_user(question: &str, answers: &[String], candidate: &str) -> String {
    format!(
        "Question: {}\nCorrect answer: {}\nModel response: {}\nIs the model response correct? Answer yes or no only.",
        question.trim(),
        answers.join(" OR "),
        candidate.trim()
    )
}

fn parse_mab_correct(raw: &str) -> bool {
    yes_no_verdict(raw).unwrap_or(false)
}

/// Extract a yes/no verdict by WORD-boundary token match, not substring.
/// Substring matching mis-fired: "NO" matched NOT/CANNOT/NONE/ANNOTATION, and
/// "yes" in first-10-chars matched "yes and no". We scan the last non-empty
/// line first (models put the verdict last), then the whole text; a line with
/// a bare `yes`/`no` token wins, and if both appear we defer to the other
/// scope. Returns None when neither token is present as a word.
fn yes_no_verdict(raw: &str) -> Option<bool> {
    fn has_word(text: &str, word: &str) -> bool {
        text.split(|c: char| !c.is_ascii_alphabetic())
            .any(|tok| tok.eq_ignore_ascii_case(word))
    }
    let lower = raw.to_ascii_lowercase();
    for scope in [
        lower
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(""),
        lower.as_str(),
    ] {
        let yes = has_word(scope, "yes");
        let no = has_word(scope, "no");
        match (yes, no) {
            (true, false) => return Some(true),
            (false, true) => return Some(false),
            _ => continue, // neither or both — try the next (wider) scope
        }
    }
    None
}

pub async fn judge_answer_mab<J: ChatLlm + ?Sized>(
    judge: &J,
    question: &str,
    answers: &[String],
    candidate: &str,
    question_type: &str,
) -> Result<JudgeVerdict, engram_llm::LlmError> {
    let messages = vec![
        ChatMessage::system(mab_prompt_for_question_type(question_type)),
        ChatMessage::user(build_mab_judge_user(question, answers, candidate)),
    ];
    let resp = judge.chat(&messages).await?;
    let raw = resp.content.clone();
    let correct = parse_mab_correct(&raw);
    Ok(JudgeVerdict {
        correct,
        raw_response: raw,
        prompt_tokens: resp.prompt_tokens,
        completion_tokens: resp.completion_tokens,
    })
}

// ---------------------------------------------------------------------------
// ∞Bench-Sum keypoint entailment (Long_Range_Understanding summarisation)
// ---------------------------------------------------------------------------

const KEYPOINT_RECALL_JUDGE_SYSTEM: &str =
    "You are a strict summarisation evaluator. You will be given a SUMMARY and a single \
     KEY POINT. Decide whether the key point is entailed by — i.e. clearly stated or \
     directly implied by — the summary. Paraphrase is fine; the meaning must be present. \
     If the summary omits, contradicts, or only vaguely gestures at the key point, it is \
     NOT entailed. Respond with exactly one word on the final line: YES or NO.";

const KEYPOINT_PRECISION_JUDGE_SYSTEM: &str =
    "You are a strict summarisation evaluator. You will be given a list of reference KEY \
     POINTS and a single SUMMARY SENTENCE. Decide whether the sentence corresponds to (is \
     supported by) at least one of the key points — i.e. it conveys information also present \
     in some key point. If the sentence states something not covered by any key point, it \
     does NOT correspond. Respond with exactly one word on the final line: YES or NO.";

/// Parse a binary YES/NO judge response by word-boundary token match (last
/// non-empty line first, then whole text). Avoids the substring trap where
/// "NO" matches NOT/CANNOT/NONE/ANNOTATION.
fn parse_yes(raw: &str) -> bool {
    yes_no_verdict(raw).unwrap_or(false)
}

/// Judge whether a single gold `keypoint` is entailed by the generated
/// `summary` (the recall half of ∞Bench-Sum keypoint-F1).
pub async fn judge_keypoint_present<J: ChatLlm + ?Sized>(
    judge: &J,
    summary: &str,
    keypoint: &str,
) -> Result<JudgeVerdict, engram_llm::LlmError> {
    let user = format!(
        "SUMMARY:\n{}\n\nKEY POINT:\n{}\n\nIs the key point entailed by the summary? Answer YES or NO.",
        summary.trim(),
        keypoint.trim()
    );
    let messages = vec![
        ChatMessage::system(KEYPOINT_RECALL_JUDGE_SYSTEM),
        ChatMessage::user(user),
    ];
    let resp = judge.chat(&messages).await?;
    let raw = resp.content.clone();
    let correct = parse_yes(&raw);
    Ok(JudgeVerdict {
        correct,
        raw_response: raw,
        prompt_tokens: resp.prompt_tokens,
        completion_tokens: resp.completion_tokens,
    })
}

/// Judge whether a single summary `sentence` corresponds to at least one of the
/// gold `keypoints` (the precision half of ∞Bench-Sum keypoint-F1).
pub async fn judge_sentence_matches_keypoints<J: ChatLlm + ?Sized>(
    judge: &J,
    keypoints: &[String],
    sentence: &str,
) -> Result<JudgeVerdict, engram_llm::LlmError> {
    let joined = keypoints
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{}. {}", i + 1, k.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "KEY POINTS:\n{}\n\nSUMMARY SENTENCE:\n{}\n\nDoes the sentence correspond to at least one key point? Answer YES or NO.",
        joined,
        sentence.trim()
    );
    let messages = vec![
        ChatMessage::system(KEYPOINT_PRECISION_JUDGE_SYSTEM),
        ChatMessage::user(user),
    ];
    let resp = judge.chat(&messages).await?;
    let raw = resp.content.clone();
    let correct = parse_yes(&raw);
    Ok(JudgeVerdict {
        correct,
        raw_response: raw,
        prompt_tokens: resp.prompt_tokens,
        completion_tokens: resp.completion_tokens,
    })
}

// ---------------------------------------------------------------------------
// Generic NLI entailment (BioGen citation support, ALCE citation scoring)
// ---------------------------------------------------------------------------

const ENTAILMENT_JUDGE_SYSTEM: &str =
    "You are a strict natural-language-inference judge. You will be given a PREMISE and a \
     HYPOTHESIS. Decide whether the premise entails the hypothesis — i.e. whether the \
     hypothesis is fully supported by, and follows from, the premise. If the premise does not \
     support the hypothesis, or only partially supports it, or contradicts it, the answer is \
     NO. Respond with exactly one word on the final line: YES or NO.";

/// Judge whether `premise` entails `hypothesis`. Returns the raw `bool` (the
/// shape consumed by [`crate::citation::citation_scores`] and
/// [`crate::biogen::citation_support_rate`]).
pub async fn judge_entails<J: ChatLlm + ?Sized>(
    judge: &J,
    premise: &str,
    hypothesis: &str,
) -> Result<bool, engram_llm::LlmError> {
    let user = format!(
        "PREMISE:\n{}\n\nHYPOTHESIS:\n{}\n\nDoes the premise entail the hypothesis? Answer YES or NO.",
        premise.trim(),
        hypothesis.trim()
    );
    let messages = vec![
        ChatMessage::system(ENTAILMENT_JUDGE_SYSTEM),
        ChatMessage::user(user),
    ];
    let resp = judge.chat(&messages).await?;
    Ok(parse_yes(&resp.content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_correct_verdict() {
        // unit test for the parsing logic only — no network.
        let raw = "The candidate gives the same answer as the reference.\n\nCORRECT".to_string();
        assert!(raw
            .to_ascii_uppercase()
            .lines()
            .rev()
            .any(|l| l.trim() == "CORRECT"));
    }

    #[test]
    fn parses_cognitive_correct_verdict() {
        let raw = r#"{"label":"correct","reason":"uses the evidence"}"#;
        assert!(parse_cognitive_correct(raw));
    }

    #[test]
    fn parses_cognitive_wrong_verdict() {
        let raw = r#"{"label":"wrong","reason":"unrelated"}"#;
        assert!(!parse_cognitive_correct(raw));
    }

    #[test]
    fn parses_mab_yes_verdict() {
        assert!(parse_mab_correct("Yes, it matches."));
    }

    #[test]
    fn parses_mab_no_verdict() {
        assert!(!parse_mab_correct("No, it misses the answer."));
    }

    #[test]
    fn parses_yes_no_verdicts() {
        assert!(parse_yes("Reasoning...\nYES"));
        assert!(!parse_yes("Reasoning...\nNO"));
        // Last-line verdict wins over an earlier mention.
        assert!(!parse_yes("Maybe YES at first glance.\nNO"));
        // Whole-text fallback when the final line is not a bare verdict.
        assert!(parse_yes("YES, the key point is clearly entailed."));
        // Ambiguous / empty -> false (conservative).
        assert!(!parse_yes(""));
    }

    #[test]
    fn yes_no_ignores_substring_traps() {
        // "NOT"/"CANNOT"/"NONE"/"ANNOTATION" must NOT read as a "NO" verdict.
        assert!(parse_yes("This is NOT wrong; the point is present.\nYES"));
        assert!(parse_yes("The summary CANNOT be faulted here.\nYES"));
        assert!(parse_yes("Per the ANNOTATION, this is entailed.\nYES"));
        // A genuine bare "no" token is still detected.
        assert!(!parse_yes("no"));
        // mab: "yes and no" on one line is ambiguous → whole-text also
        // ambiguous → conservative false, not a false "yes" from substring.
        assert!(!parse_mab_correct("yes and no"));
        assert!(parse_mab_correct("YES"));
        assert!(!parse_mab_correct("Notably, the answer is absent."));
    }
}
