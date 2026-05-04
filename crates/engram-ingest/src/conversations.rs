//! Conversation ingestion -- preserve speaker turns and user/assistant pairs.

use crate::chunker::{section_aware_split, PendingChunk};

const MAX_CHARS: usize = 1800;

#[derive(Debug, Clone)]
struct Turn {
    speaker: String,
    content: String,
}

pub fn chunk_conversation(text: &str) -> Vec<PendingChunk> {
    let turns = parse_turns(text);
    if turns.len() < 2 {
        return section_aware_split(text);
    }

    let mut out = Vec::new();
    let mut position = 0u32;
    let mut i = 0usize;
    while i < turns.len() {
        let end = if is_user(&turns[i].speaker)
            && i + 1 < turns.len()
            && is_assistant(&turns[i + 1].speaker)
        {
            i + 2
        } else {
            i + 1
        };
        push_window(&turns[i..end], i, &mut out, &mut position);
        i = end;
    }
    out
}

fn parse_turns(text: &str) -> Vec<Turn> {
    let mut turns = Vec::new();
    let mut speaker: Option<String> = None;
    let mut buf = String::new();

    for line in text.lines() {
        if let Some((next_speaker, rest)) = parse_speaker_line(line) {
            flush_turn(&mut turns, &mut speaker, &mut buf);
            speaker = Some(next_speaker);
            if !rest.trim().is_empty() {
                buf.push_str(rest.trim());
            }
        } else if speaker.is_some() {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(line);
        }
    }
    flush_turn(&mut turns, &mut speaker, &mut buf);
    turns
}

fn parse_speaker_line(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let (raw, rest) = trimmed.split_once(':')?;
    let normalized = raw.trim().to_ascii_lowercase();
    let speaker = match normalized.as_str() {
        "user" | "human" => "User",
        "assistant" | "claude" | "chatgpt" | "agent" => "Assistant",
        "system" => "System",
        "developer" => "Developer",
        "tool" | "function" => "Tool",
        _ => return None,
    };
    Some((speaker.to_string(), rest))
}

fn flush_turn(turns: &mut Vec<Turn>, speaker: &mut Option<String>, buf: &mut String) {
    let Some(name) = speaker.take() else {
        return;
    };
    let content = buf.trim();
    if !content.is_empty() {
        turns.push(Turn {
            speaker: name,
            content: content.to_string(),
        });
    }
    buf.clear();
}

fn push_window(
    turns: &[Turn],
    start_index: usize,
    out: &mut Vec<PendingChunk>,
    position: &mut u32,
) {
    let mut text = String::new();
    for turn in turns {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&turn.speaker);
        text.push_str(": ");
        text.push_str(turn.content.trim());
    }
    for part in split_window(&text) {
        let speaker_path = turns
            .iter()
            .map(|turn| turn.speaker.as_str())
            .collect::<Vec<_>>()
            .join(" -> ");
        out.push(PendingChunk {
            text: part,
            position: *position,
            section: Some(format!(
                "turns {}-{}: {}",
                start_index + 1,
                start_index + turns.len(),
                speaker_path
            )),
        });
        *position += 1;
    }
}

fn split_window(text: &str) -> Vec<String> {
    if text.len() <= MAX_CHARS {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    for paragraph in text.split("\n\n") {
        if !buf.is_empty() && buf.len() + paragraph.len() + 2 > MAX_CHARS {
            out.push(buf.trim().to_string());
            buf.clear();
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(paragraph);
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

fn is_user(speaker: &str) -> bool {
    speaker == "User"
}

fn is_assistant(speaker: &str) -> bool {
    speaker == "Assistant"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_pairs_user_and_assistant_turns() {
        let text = "User: Can this ingest one file?\nAssistant: Yes, pass the path directly.\nUser: What about folders?\nAssistant: Preview with dry-run first.";
        let chunks = chunk_conversation(text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].text.contains("User: Can this ingest"));
        assert!(chunks[0].text.contains("Assistant: Yes"));
        assert_eq!(
            chunks[0].section.as_deref(),
            Some("turns 1-2: User -> Assistant")
        );
    }

    #[test]
    fn conversation_falls_back_for_plain_notes() {
        let chunks = chunk_conversation("# Meeting\n\nNo speaker labels here.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].section.as_deref(), Some("Meeting"));
    }
}
