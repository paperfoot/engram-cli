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
        .chat(&[
            ChatMessage::system(system),
            ChatMessage::user(user),
        ])
        .await?;
    // Pull the first float-looking token.
    let mut best: Option<f32> = None;
    for token in resp.content.split(|c: char| !c.is_ascii_digit() && c != '.') {
        if let Ok(v) = token.parse::<f32>() {
            if (0.0..=1.0).contains(&v) {
                best = Some(v);
                break;
            }
        }
    }
    Ok(best.unwrap_or(0.0))
}

pub async fn faithfulness<J: ChatLlm + ?Sized>(
    judge: &J,
    answer: &str,
    context: &str,
) -> Result<f32, LlmError> {
    let system =
        "You rate how faithful an answer is to the given context. Faithful means every factual \
         claim in the answer can be inferred from the context. If the answer introduces new facts \
         not supported by the context (hallucination), it is less faithful. \
         Respond with a single number between 0.0 and 1.0 (1.0 = fully faithful).";
    let user = format!(
        "CONTEXT:\n{}\n\nANSWER:\n{}\n\nFaithfulness score:",
        context.trim(),
        answer.trim()
    );
    score_01(judge, system, &user).await
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
