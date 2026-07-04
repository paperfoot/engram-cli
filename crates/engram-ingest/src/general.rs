//! General mode -- section-aware chunks for mixed notes and Markdown.

use crate::chunker::{section_aware_split, split_oversized, PendingChunk};

const MAX_CHARS: usize = 1800;

pub fn chunk_general(text: &str) -> Vec<PendingChunk> {
    // `split_oversized` cuts only at real sentence boundaries (shared with paper
    // mode), so an oversized note never severs a decimal like "0.1 nM".
    split_oversized(section_aware_split(text), MAX_CHARS)
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

    #[test]
    fn general_oversized_split_preserves_decimals() {
        // An oversized note built from decimal-bearing sentences must split into
        // bounded chunks without ever cutting "0.05" in half.
        let text = format!("# Notes\n\n{}", "The p value was 0.05 here. ".repeat(120));
        let chunks = chunk_general(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.text.len() <= MAX_CHARS));
        let total: usize = chunks.iter().map(|c| c.text.matches("0.05").count()).sum();
        assert_eq!(total, 120, "a decimal was split or lost");
    }
}
