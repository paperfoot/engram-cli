//! LongMemEval dataset loader and runner.
//!
//! Dataset:  https://github.com/xiaowu0162/LongMemEval
//! Format:   JSON files of {question_id, question, answer, evidence_session_ids,
//!           haystack_sessions: [...]}
//!
//! v0 supports loading the M (medium) split. Phase 2 wires this up to
//! `engram bench longmemeval` and produces JSON output for autoresearch.

use crate::error::BenchError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongMemEvalQuestion {
    pub question_id: String,
    pub question_type: String,
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub answer_session_ids: Vec<String>,
    #[serde(default)]
    pub haystack_session_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongMemEvalDataset {
    pub questions: Vec<LongMemEvalQuestion>,
}

impl LongMemEvalDataset {
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, BenchError> {
        let bytes = std::fs::read(path.as_ref())?;
        let questions: Vec<LongMemEvalQuestion> = serde_json::from_slice(&bytes)?;
        if questions.is_empty() {
            return Err(BenchError::InvalidDataset("0 questions".into()));
        }
        Ok(Self { questions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_dataset_from_inline_json() {
        let json = serde_json::json!([
            {
                "question_id": "q1",
                "question_type": "single-session-user",
                "question": "What did I say about rapamycin?",
                "answer": "It extends mouse lifespan.",
                "answer_session_ids": ["s1"],
                "haystack_session_ids": ["s1", "s2", "s3"]
            }
        ]);
        let path = std::env::temp_dir().join("lme-test.json");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let ds = LongMemEvalDataset::load_from_file(&path).unwrap();
        assert_eq!(ds.questions.len(), 1);
        assert_eq!(ds.questions[0].question_id, "q1");
    }
}
