//! General mode -- section-aware chunks for mixed notes and Markdown.

use crate::chunker::{section_aware_split, PendingChunk};

const MAX_CHARS: usize = 1800;

pub fn chunk_general(text: &str) -> Vec<PendingChunk> {
    let chunks = section_aware_split(text);
    split_oversized(chunks, MAX_CHARS)
}

fn split_oversized(chunks: Vec<PendingChunk>, max_chars: usize) -> Vec<PendingChunk> {
    let mut out = Vec::new();
    let mut position = 0u32;
    for chunk in chunks {
        for text in split_text(&chunk.text, max_chars) {
            out.push(PendingChunk {
                text,
                position,
                section: chunk.section.clone(),
            });
            position += 1;
        }
    }
    out
}

fn split_text(text: &str, max_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.len() <= max_chars {
        return vec![trimmed.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    for sentence in trimmed.split_inclusive(['.', '!', '?', '\n']) {
        if !buf.is_empty() && buf.len() + sentence.len() > max_chars {
            out.push(buf.trim().to_string());
            buf.clear();
        }
        buf.push_str(sentence);
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_preserves_markdown_sections() {
        let chunks = chunk_general("# Project\n\n## Decision\n\nUse scoped directory ingest.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].section.as_deref(), Some("Project > Decision"));
    }

    #[test]
    fn general_splits_oversized_paragraphs() {
        let text = format!("# Notes\n\n{}", "Long sentence. ".repeat(220));
        let chunks = chunk_general(&text);
        assert!(chunks.len() > 1);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.text.len() <= MAX_CHARS + 32));
        assert!(chunks
            .iter()
            .all(|chunk| chunk.section.as_deref() == Some("Notes")));
    }
}
