//! MemoryAgentBench dataset loader.
//!
//! Source: https://github.com/HUST-AI-HYZ/MemoryAgentBench
//! HuggingFace: https://huggingface.co/datasets/ai-hyz/MemoryAgentBench
//!
//! 4 splits x 22-110 rows x 60-100 questions/row = ~2071 questions total.
//! Each row is "inject once, query many" -- index the context, run all
//! questions for that context against the same memory state.

use crate::error::BenchError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MabSplit {
    AccurateRetrieval,
    TestTimeLearning,
    LongRangeUnderstanding,
    ConflictResolution,
}

impl MabSplit {
    pub fn name(&self) -> &'static str {
        match self {
            Self::AccurateRetrieval => "Accurate_Retrieval",
            Self::TestTimeLearning => "Test_Time_Learning",
            Self::LongRangeUnderstanding => "Long_Range_Understanding",
            Self::ConflictResolution => "Conflict_Resolution",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let normalized = name
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .replace(' ', "_");
        match normalized.as_str() {
            "accurate_retrieval" | "ar" => Some(Self::AccurateRetrieval),
            "test_time_learning" | "ttl" => Some(Self::TestTimeLearning),
            "long_range_understanding" | "lru" => Some(Self::LongRangeUnderstanding),
            "conflict_resolution" | "sf" | "selective_forgetting" => Some(Self::ConflictResolution),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MabHaystackTurn {
    #[serde(default)]
    pub role: String,
    #[serde(default, deserialize_with = "deserialize_string_flex")]
    pub content: String,
    #[serde(default)]
    pub has_answer: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MabRowMetadata {
    #[serde(default, deserialize_with = "deserialize_opt_vec_string")]
    pub qa_pair_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_opt_vec_string")]
    pub question_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_opt_vec_string")]
    pub question_dates: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_opt_vec_string")]
    pub question_types: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_opt_string")]
    pub source: Option<String>,
    /// Opaque structure — varies by split: Vec<Turn> for some, Vec<Vec<Vec<Turn>>>
    /// for LongMemEval rows in Accurate_Retrieval. We don't use this downstream
    /// (context is the haystack), so we store raw JSON and skip strict parsing.
    #[serde(default)]
    pub haystack_sessions: serde_json::Value,
    #[serde(default, deserialize_with = "deserialize_opt_string")]
    pub demo: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_vec_string")]
    pub keypoints: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_opt_vec_string")]
    pub previous_events: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MabRow {
    #[serde(default)]
    pub split: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_answers_flex")]
    pub answers: Vec<Vec<String>>,
    #[serde(default)]
    pub metadata: MabRowMetadata,
}

pub struct MabDataset {
    pub split: MabSplit,
    pub rows: Vec<MabRow>,
}

impl MabDataset {
    pub fn load_jsonl(path: &Path, split: MabSplit) -> Result<Self, BenchError> {
        let contents = std::fs::read_to_string(path)?;
        let mut rows = Vec::new();
        for (idx, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut row: MabRow = serde_json::from_str(line).map_err(|e| {
                BenchError::InvalidDataset(format!(
                    "failed to parse {} line {}: {}",
                    path.display(),
                    idx + 1,
                    e
                ))
            })?;
            if row.split.is_empty() {
                row.split = split.name().to_string();
            }
            rows.push(row);
        }
        if rows.is_empty() {
            return Err(BenchError::InvalidDataset(format!(
                "0 MemoryAgentBench rows in {}",
                path.display()
            )));
        }
        Ok(Self { split, rows })
    }
}

pub fn default_dir() -> PathBuf {
    PathBuf::from("data/memoryagentbench")
}

fn deserialize_string_flex<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(value_to_string(&v))
}

fn deserialize_opt_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Null => None,
        other => Some(value_to_string(&other)),
    })
}

fn deserialize_opt_vec_string<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Null => None,
        serde_json::Value::Array(arr) => Some(arr.iter().map(value_to_string).collect()),
        serde_json::Value::String(s) if s.trim().is_empty() => None,
        serde_json::Value::String(s) if s.trim_start().starts_with('[') => Some(
            serde_json::from_str::<Vec<serde_json::Value>>(&s)
                .map_err(serde::de::Error::custom)?
                .iter()
                .map(value_to_string)
                .collect(),
        ),
        other => Some(vec![value_to_string(&other)]),
    })
}

fn deserialize_answers_flex<'de, D>(d: D) -> Result<Vec<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    let serde_json::Value::Array(rows) = v else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match row {
            serde_json::Value::Array(vals) => {
                out.push(vals.iter().map(value_to_string).collect());
            }
            serde_json::Value::Null => out.push(Vec::new()),
            other => out.push(vec![value_to_string(&other)]),
        }
    }
    Ok(out)
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_missing_jsonl_returns_io_error() {
        let r = MabDataset::load_jsonl(
            Path::new("/tmp/this-memoryagentbench-file-does-not-exist.jsonl"),
            MabSplit::AccurateRetrieval,
        );
        assert!(r.is_err());
    }

    #[test]
    fn parses_small_embedded_jsonl() {
        let path = std::env::temp_dir().join(format!(
            "memoryagentbench_test_{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"context":"ctx","questions":["q1"],"answers":[["a1","alias"]],"metadata":{"qa_pair_ids":["pair1"],"question_ids":["qid1"],"question_dates":["2025-01-01"],"question_types":["factual"],"source":"unit","haystack_sessions":[{"role":"user","content":"hello","has_answer":true}],"demo":null,"keypoints":null,"previous_events":["old"]}}"#,
        )
        .unwrap();

        let dataset = MabDataset::load_jsonl(&path, MabSplit::AccurateRetrieval).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(dataset.rows.len(), 1);
        let row = &dataset.rows[0];
        assert_eq!(row.split, "Accurate_Retrieval");
        assert_eq!(row.questions[0], "q1");
        assert_eq!(row.answers[0], vec!["a1".to_string(), "alias".to_string()]);
        assert_eq!(row.metadata.source.as_deref(), Some("unit"));
        // haystack_sessions is opaque — just verify it's present as an array.
        assert!(row.metadata.haystack_sessions.is_array());
    }

    #[test]
    fn parses_split_aliases() {
        assert_eq!(
            MabSplit::from_name("accurate_retrieval"),
            Some(MabSplit::AccurateRetrieval)
        );
        assert_eq!(
            MabSplit::from_name("long-range-understanding"),
            Some(MabSplit::LongRangeUnderstanding)
        );
        assert_eq!(
            MabSplit::from_name("sf"),
            Some(MabSplit::ConflictResolution)
        );
    }
}
