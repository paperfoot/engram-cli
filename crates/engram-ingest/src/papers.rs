//! Paper ingestion — section-aware chunking for scientific papers.
//!
//! Delegates to `chunker::section_aware_split`, which tracks Markdown ATX,
//! setext, and numbered section headings so chunks carry `Methods > Cell Culture`
//! style breadcrumbs, then merges tiny adjacent same-section chunks so a single
//! short paragraph doesn't become a context-poor unit that embeds and reranks
//! badly, and finally caps any oversized chunk so nothing overflows the embedder.

use crate::chunker::{merge_small_chunks, section_aware_split, split_oversized, PendingChunk};

/// Target chunk size (characters) for merged paper chunks. ~300 tokens: large
/// enough to carry standalone context, small enough to stay retrieval-precise.
const PAPER_CHUNK_TARGET_CHARS: usize = 1200;

/// Hard upper bound (characters) for a paper chunk. Merging only ever grows
/// chunks, so a badly-extracted full page or a long references list can balloon
/// past the embedder's ~2000-token window and get silently truncated (tail lost
/// forever). We split anything over this cap at sentence boundaries after merge.
const PAPER_CHUNK_MAX_CHARS: usize = 1800;

pub fn chunk_paper(text: &str) -> Vec<PendingChunk> {
    let merged = merge_small_chunks(section_aware_split(text), PAPER_CHUNK_TARGET_CHARS);
    split_oversized(merged, PAPER_CHUNK_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_paper_chunk_splits_at_sentence_boundaries() {
        // One giant paragraph (no blank lines) far exceeding the embedder cap,
        // built from many decimal-bearing sentences that must never be cut.
        let text = "The value was 0.5 nM here. ".repeat(200); // ~5400 chars, one paragraph
        let chunks = chunk_paper(&text);

        assert!(chunks.len() > 1, "oversized paragraph should split");
        assert!(
            chunks.iter().all(|c| c.text.len() <= PAPER_CHUNK_MAX_CHARS),
            "a chunk exceeded the cap: {:?}",
            chunks.iter().map(|c| c.text.len()).collect::<Vec<_>>()
        );
        // Every decimal survives intact and none was dropped at a boundary.
        let total: usize = chunks
            .iter()
            .map(|c| c.text.matches("0.5 nM").count())
            .sum();
        assert_eq!(total, 200, "a decimal was split across chunks or lost");
        // Positions are contiguous from 0.
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.position, i as u32);
        }
    }

    #[test]
    fn normal_paper_chunks_pass_through_unsplit() {
        let text = "# Methods\n\nHeLa cells were grown in DMEM at 37 °C.\n\n# Results\n\nLifespan rose 24%.";
        let chunks = chunk_paper(text);
        assert!(chunks.iter().all(|c| c.text.len() <= PAPER_CHUNK_MAX_CHARS));
        assert!(chunks
            .iter()
            .any(|c| c.section.as_deref() == Some("Methods")));
    }
}
