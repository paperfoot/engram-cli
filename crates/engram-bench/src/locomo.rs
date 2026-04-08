//! LoCoMo dataset loader.
//!
//! Dataset: https://snap-research.github.io/locomo/
//! Format (from the official repo): an array of samples, each with a
//! `conversation` object that has many `session_N` keys plus a
//! `qa` list of {question, answer, evidence, category}.
//!
//! We flatten each conversation into a set of sessions (one per session_N key)
//! so the same QA harness used for LongMemEval can run on LoCoMo without
//! special-casing.

use crate::error::BenchError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocomoQA {
    pub question: String,
    /// Gold answer. Optional because some adversarial LoCoMo entries omit it
    /// in favor of `adversarial_answer` (which is the "wrong" answer used to
    /// test refusal). We skip those during scoring.
    #[serde(default, deserialize_with = "deserialize_answer_flex_opt")]
    pub answer: Option<String>,
    #[serde(default)]
    pub adversarial_answer: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub category: Option<u32>,
}

fn deserialize_answer_flex_opt<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s)),
        serde_json::Value::Number(n) => Ok(Some(n.to_string())),
        serde_json::Value::Bool(b) => Ok(Some(b.to_string())),
        other => Ok(Some(other.to_string())),
    }
}

#[allow(dead_code)]
fn deserialize_answer_flex<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocomoSample {
    #[serde(default)]
    pub sample_id: Option<String>,
    pub conversation: serde_json::Value,
    #[serde(default)]
    pub qa: Vec<LocomoQA>,
}

pub struct LocomoDataset {
    pub samples: Vec<LocomoSample>,
}

impl LocomoDataset {
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, BenchError> {
        let bytes = std::fs::read(path.as_ref())?;
        // Some releases use a top-level array; others use {"samples": [...]}
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        let samples: Vec<LocomoSample> = match v {
            serde_json::Value::Array(arr) => serde_json::from_value(serde_json::Value::Array(arr))?,
            serde_json::Value::Object(mut obj) => {
                let arr = obj
                    .remove("samples")
                    .or_else(|| obj.remove("data"))
                    .ok_or_else(|| {
                        BenchError::InvalidDataset(
                            "expected top-level array or { samples: [...] }".into(),
                        )
                    })?;
                serde_json::from_value(arr)?
            }
            _ => {
                return Err(BenchError::InvalidDataset(
                    "unexpected top-level JSON value".into(),
                ))
            }
        };
        if samples.is_empty() {
            return Err(BenchError::InvalidDataset("0 samples".into()));
        }
        Ok(Self { samples })
    }
}

pub fn default_path() -> PathBuf {
    PathBuf::from("data/locomo/locomo10.json")
}

/// Extract every session_N key as a flat list of (session_id, text) pairs.
/// Speaker turns are flattened into a single string per session.
pub fn flatten_conversation(conv: &serde_json::Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(obj) = conv.as_object() {
        let mut keys: Vec<&String> = obj.keys().filter(|k| k.starts_with("session_")).collect();
        keys.sort();
        for key in keys {
            let val = &obj[key];
            let text = match val {
                // session is usually an array of {speaker, text} or similar.
                serde_json::Value::Array(turns) => {
                    let mut buf = String::new();
                    for t in turns {
                        if let Some(obj) = t.as_object() {
                            let speaker = obj
                                .get("speaker")
                                .or_else(|| obj.get("role"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let text = obj
                                .get("text")
                                .or_else(|| obj.get("content"))
                                .or_else(|| obj.get("clean_text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            buf.push_str(speaker);
                            buf.push_str(": ");
                            buf.push_str(text);
                            buf.push('\n');
                        } else if let Some(s) = t.as_str() {
                            buf.push_str(s);
                            buf.push('\n');
                        }
                    }
                    buf
                }
                serde_json::Value::String(s) => s.clone(),
                _ => val.to_string(),
            };
            if !text.trim().is_empty() {
                out.push((key.clone(), text));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_missing_dataset_returns_io_error() {
        let r = LocomoDataset::load_from_file("/tmp/this-does-not-exist-xyz.json");
        assert!(r.is_err());
    }

    #[test]
    fn flatten_conversation_basic() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "session_1": [{"speaker":"alice","text":"hi"},{"speaker":"bob","text":"hey"}],
                "session_2": [{"speaker":"alice","text":"how are you"}],
                "metadata": "ignored"
            }"#,
        )
        .unwrap();
        let flat = flatten_conversation(&v);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].0, "session_1");
        assert!(flat[0].1.contains("alice"));
    }
}
