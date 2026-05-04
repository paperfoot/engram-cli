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
/// Speaker turns are flattened into a single string per session. The matching
/// `session_N_date_time` sibling key (e.g. "1:56 pm on 8 May, 2023") is
/// prepended to the text as a header so downstream readers can resolve
/// relative time references ("yesterday", "last year") to absolute dates.
/// Skips adversarial / metadata keys like `session_N_date_time` themselves.
pub fn flatten_conversation(conv: &serde_json::Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(obj) = conv.as_object() {
        // Keep only the raw session keys, not their `_date_time` siblings.
        let mut keys: Vec<&String> = obj
            .keys()
            .filter(|k| {
                k.starts_with("session_") && !k.ends_with("_date_time") && !k.ends_with("_summary")
            })
            .collect();
        // Sort numerically so session_10 comes after session_2, not after session_1.
        keys.sort_by_key(|k| {
            k.strip_prefix("session_")
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(u32::MAX)
        });
        for key in keys {
            let val = &obj[key];
            let body = match val {
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
            if body.trim().is_empty() {
                continue;
            }
            // Prepend the session timestamp if present — critical for temporal
            // questions where the conversation uses relative references like
            // "yesterday" or "last year".
            let date_time_key = format!("{key}_date_time");
            let header = obj
                .get(&date_time_key)
                .and_then(|v| v.as_str())
                .map(|dt| format!("[{key} — {dt}]\n"))
                .unwrap_or_else(|| format!("[{key}]\n"));
            let text = format!("{header}{body}");
            out.push((key.clone(), text));
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
        // Without a date_time sibling we still emit a bare header.
        assert!(flat[0].1.contains("[session_1]"));
    }

    #[test]
    fn flatten_conversation_includes_timestamp_header() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "session_1_date_time": "1:56 pm on 8 May, 2023",
                "session_1": [{"speaker":"alice","text":"hi"}]
            }"#,
        )
        .unwrap();
        let flat = flatten_conversation(&v);
        assert_eq!(flat.len(), 1);
        assert!(
            flat[0].1.contains("1:56 pm on 8 May, 2023"),
            "expected timestamp in flattened text, got: {}",
            flat[0].1
        );
        assert!(flat[0].1.contains("[session_1 — 1:56 pm on 8 May, 2023]"));
    }

    #[test]
    fn flatten_conversation_sorts_numerically() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "session_10": [{"speaker":"a","text":"ten"}],
                "session_2":  [{"speaker":"a","text":"two"}],
                "session_1":  [{"speaker":"a","text":"one"}]
            }"#,
        )
        .unwrap();
        let flat = flatten_conversation(&v);
        let keys: Vec<&str> = flat.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["session_1", "session_2", "session_10"]);
    }

    #[test]
    fn flatten_conversation_skips_summary_and_date_time_keys() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "session_1": [{"speaker":"a","text":"hi"}],
                "session_1_date_time": "1:00 pm on 1 Jan, 2024",
                "session_1_summary": "they said hi",
                "session_2": [{"speaker":"b","text":"yo"}],
                "session_2_date_time": "2:00 pm on 2 Jan, 2024"
            }"#,
        )
        .unwrap();
        let flat = flatten_conversation(&v);
        let keys: Vec<&str> = flat.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["session_1", "session_2"]);
    }
}
