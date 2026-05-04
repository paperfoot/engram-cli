//! LoCoMo-Plus dataset loader.
//!
//! Source: https://github.com/xjtuleeyf/Locomo-Plus
//! 401 cognitive cue/trigger pairs added to the original LoCoMo conversations.
//!
//! Each entry is stitched into one of the 10 LoCoMo conversations at a position
//! computed from the `time_gap` field. The judge checks whether the model's
//! response shows awareness of the cue dialogue (binary correct/wrong).

use crate::error::BenchError;
use crate::locomo::{flatten_conversation, LocomoSample};
use chrono::{Duration, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocomoPlusEntry {
    pub relation_type: String,
    pub cue_dialogue: String,
    pub trigger_query: String,
    pub time_gap: String,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub scores: serde_json::Value,
    #[serde(default)]
    pub ranks: serde_json::Value,
    #[serde(default)]
    pub final_similarity_score: Option<f64>,
}

pub struct LocomoPlusDataset {
    pub entries: Vec<LocomoPlusEntry>,
}

impl LocomoPlusDataset {
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, BenchError> {
        let bytes = std::fs::read(path.as_ref())?;
        let entries: Vec<LocomoPlusEntry> = serde_json::from_slice(&bytes)?;
        if entries.is_empty() {
            return Err(BenchError::InvalidDataset("0 LoCoMo-Plus entries".into()));
        }
        Ok(Self { entries })
    }
}

pub fn default_path() -> PathBuf {
    PathBuf::from("data/locomo_plus/locomo_plus.json")
}

#[derive(Debug, Clone)]
pub struct StitchedConversation {
    pub sessions: Vec<(String, String)>,
    pub trigger: String,
    pub evidence_text: String,
}

pub fn parse_time_gap(s: &str) -> i64 {
    let tokens: Vec<String> = s
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();

    for pair in tokens.windows(2) {
        let count = match pair[0].as_str() {
            "a" | "an" => Some(1),
            "one" => Some(1),
            "two" => Some(2),
            "three" => Some(3),
            "four" => Some(4),
            "five" => Some(5),
            "six" => Some(6),
            "seven" => Some(7),
            "eight" => Some(8),
            "nine" => Some(9),
            "ten" => Some(10),
            "eleven" => Some(11),
            "twelve" => Some(12),
            n => n.parse::<i64>().ok(),
        };
        let Some(count) = count else {
            continue;
        };
        let unit = pair[1].as_str();
        if unit.starts_with("week") {
            return count * 7;
        }
        if unit.starts_with("month") {
            return count * 30;
        }
        if unit.starts_with("year") {
            return count * 365;
        }
    }
    0
}

pub fn parse_ab_dialogue(s: &str) -> Vec<(char, String)> {
    let normalized = s.replace("\\n", "\n");
    let mut turns = Vec::new();
    for line in normalized.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("A:") {
            turns.push(('A', rest.trim().to_string()));
        } else if let Some(rest) = line.strip_prefix("B:") {
            turns.push(('B', rest.trim().to_string()));
        }
    }
    turns
}

pub fn build_stitched_dialogue(
    plus_entry: &LocomoPlusEntry,
    locomo_sample: &LocomoSample,
) -> StitchedConversation {
    let conv = &locomo_sample.conversation;
    let speaker_a = conv
        .get("speaker_a")
        .and_then(|v| v.as_str())
        .unwrap_or("A");
    let speaker_b = conv
        .get("speaker_b")
        .and_then(|v| v.as_str())
        .unwrap_or("B");

    let cue_turns = map_dialogue(
        &parse_ab_dialogue(&plus_entry.cue_dialogue),
        speaker_a,
        speaker_b,
    );
    let trigger_turns = map_dialogue(
        &parse_ab_dialogue(&plus_entry.trigger_query),
        speaker_a,
        speaker_b,
    );
    let evidence_text = dialogue_to_text(&cue_turns);
    let trigger = dialogue_to_text(&trigger_turns);
    let cue_id = cue_session_id(plus_entry);

    let flat_sessions = flatten_conversation(conv);
    let mut flat_by_id: HashMap<String, String> = flat_sessions.iter().cloned().collect();
    let Some(obj) = conv.as_object() else {
        let mut sessions = flat_sessions;
        sessions.push((cue_id.clone(), format!("[{cue_id}]\n{evidence_text}")));
        return StitchedConversation {
            sessions,
            trigger,
            evidence_text,
        };
    };

    let mut keys: Vec<&String> = obj
        .keys()
        .filter(|k| {
            k.starts_with("session_") && !k.ends_with("_date_time") && !k.ends_with("_summary")
        })
        .collect();
    keys.sort_by_key(|k| {
        k.strip_prefix("session_")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });

    #[derive(Debug, Clone)]
    struct Event {
        time: NaiveDateTime,
        order: usize,
        session_id: String,
        text: String,
    }

    let mut events = Vec::new();
    let mut last_time = None;
    for (order, key) in keys.iter().enumerate() {
        let dt_key = format!("{key}_date_time");
        let Some(dt) = obj
            .get(&dt_key)
            .and_then(|v| v.as_str())
            .and_then(parse_locomo_session_time)
        else {
            continue;
        };
        if last_time.map(|t| dt > t).unwrap_or(true) {
            last_time = Some(dt);
        }
        let text = flat_by_id
            .remove(*key)
            .unwrap_or_else(|| obj.get(*key).map(|v| v.to_string()).unwrap_or_default());
        events.push(Event {
            time: dt,
            order,
            session_id: (*key).clone(),
            text,
        });
    }

    if let Some(last_session_time) = last_time {
        let query_time = last_session_time + Duration::days(7);
        let cue_time = query_time - Duration::days(parse_time_gap(&plus_entry.time_gap));
        let cue_text = format!(
            "[{} — {}]\n{}",
            cue_id,
            cue_time.format("%Y-%m-%d %H:%M"),
            evidence_text
        );
        events.push(Event {
            time: cue_time,
            order: events.len(),
            session_id: cue_id,
            text: cue_text,
        });
        events.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.order.cmp(&b.order)));
        StitchedConversation {
            sessions: events.into_iter().map(|e| (e.session_id, e.text)).collect(),
            trigger,
            evidence_text,
        }
    } else {
        let mut sessions = flat_sessions;
        sessions.push((cue_id.clone(), format!("[{cue_id}]\n{evidence_text}")));
        StitchedConversation {
            sessions,
            trigger,
            evidence_text,
        }
    }
}

fn map_dialogue(
    turns: &[(char, String)],
    speaker_a: &str,
    speaker_b: &str,
) -> Vec<(String, String)> {
    turns
        .iter()
        .map(|(speaker, text)| {
            let mapped = if *speaker == 'A' {
                speaker_a
            } else {
                speaker_b
            };
            (mapped.to_string(), text.clone())
        })
        .collect()
}

fn dialogue_to_text(turns: &[(String, String)]) -> String {
    turns
        .iter()
        .map(|(speaker, text)| format!("{speaker}: {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cue_session_id(entry: &LocomoPlusEntry) -> String {
    let mut h: u64 = 14695981039346656037;
    for b in format!(
        "{}:{}:{}",
        entry.relation_type, entry.cue_dialogue, entry.trigger_query
    )
    .bytes()
    {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("cue_for_{h:016x}")
}

fn parse_locomo_session_time(s: &str) -> Option<NaiveDateTime> {
    let trimmed = s.trim();
    let formats = [
        "%-I:%M %P on %-d %B, %Y",
        "%-I:%M %p on %-d %B, %Y",
        "%I:%M %P on %d %B, %Y",
        "%I:%M %p on %d %B, %Y",
        "%-I:%M %P on %d %B, %Y",
        "%-I:%M %p on %d %B, %Y",
        "%I:%M %P on %-d %B, %Y",
        "%I:%M %p on %-d %B, %Y",
    ];
    for fmt in formats {
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(dt);
        }
    }

    let normalized = trimmed.replace(" am ", " AM ").replace(" pm ", " PM ");
    for fmt in ["%-I:%M %p on %-d %B, %Y", "%I:%M %p on %d %B, %Y"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&normalized, fmt) {
            return Some(dt);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_time_gaps() {
        assert_eq!(parse_time_gap("two weeks later"), 14);
        assert_eq!(parse_time_gap("about six months later"), 180);
        assert_eq!(parse_time_gap("a year later"), 365);
        assert_eq!(parse_time_gap("garbage"), 0);
    }

    #[test]
    fn parses_ab_dialogue_lines() {
        assert_eq!(
            parse_ab_dialogue("A: hi\nB: hello\nA: how are you"),
            vec![
                ('A', "hi".to_string()),
                ('B', "hello".to_string()),
                ('A', "how are you".to_string())
            ]
        );
    }

    #[test]
    fn parses_escaped_newlines_in_dialogue() {
        let raw =
            r#"A: After learning to say 'no', I've felt less stressed.\nB: That's great to hear!"#;
        let turns = parse_ab_dialogue(raw);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].0, 'A');
        assert_eq!(turns[1].0, 'B');
        assert!(turns[1].1.contains("great to hear"));
    }

    #[test]
    fn parses_locomo_session_times() {
        assert!(parse_locomo_session_time("1:56 pm on 8 May, 2023").is_some());
        assert!(parse_locomo_session_time("11:30 am on 12 December, 2022").is_some());
    }

    #[test]
    fn loads_embedded_json_schema() {
        let path =
            std::env::temp_dir().join(format!("locomo_plus_test_{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"[{
                "relation_type": "causal",
                "cue_dialogue": "A: hi\nB: hello",
                "trigger_query": "A: later",
                "time_gap": "two weeks later",
                "model_name": "gpt-4o-mini",
                "scores": {"mpnet": 0.1},
                "ranks": {},
                "final_similarity_score": 0.2
            }]"#,
        )
        .unwrap();
        let dataset = LocomoPlusDataset::load_from_file(&path).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(dataset.entries.len(), 1);
        assert_eq!(dataset.entries[0].relation_type, "causal");
    }

    #[test]
    fn builds_stitched_conversation_with_mapped_cue() {
        let sample = LocomoSample {
            sample_id: Some("sample0".into()),
            conversation: json!({
                "speaker_a": "Caroline",
                "speaker_b": "Melanie",
                "session_1_date_time": "1:56 pm on 8 May, 2023",
                "session_1": [{"speaker":"Caroline","text":"Hi"}],
                "session_2_date_time": "11:30 am on 12 December, 2022",
                "session_2": [{"speaker":"Melanie","text":"Earlier chat"}]
            }),
            qa: vec![],
        };
        let entry = LocomoPlusEntry {
            relation_type: "causal".into(),
            cue_dialogue: "A: After learning to say no\nB: Good boundary".into(),
            trigger_query: "A: I volunteered and now I am overwhelmed".into(),
            time_gap: "two weeks later".into(),
            model_name: None,
            scores: serde_json::Value::Null,
            ranks: serde_json::Value::Null,
            final_similarity_score: None,
        };

        let stitched = build_stitched_dialogue(&entry, &sample);
        assert!(stitched
            .sessions
            .iter()
            .any(|(sid, text)| sid.starts_with("cue_for_") && text.contains("Caroline:")));
        assert!(stitched.evidence_text.contains("Caroline: After learning"));
        assert!(stitched.evidence_text.contains("Melanie: Good boundary"));
        assert!(stitched.trigger.contains("Caroline: I volunteered"));
    }
}
