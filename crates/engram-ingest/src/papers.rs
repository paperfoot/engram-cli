//! Paper ingestion — section-aware chunking for scientific papers.
//!
//! Delegates to `chunker::section_aware_split`, which tracks Markdown ATX,
//! setext, and numbered section headings so chunks carry `Methods > Cell Culture`
//! style breadcrumbs, then merges tiny adjacent same-section chunks so a single
//! short paragraph doesn't become a context-poor unit that embeds and reranks badly.

use crate::chunker::{merge_small_chunks, section_aware_split, PendingChunk};

/// Target chunk size (characters) for merged paper chunks. ~300 tokens: large
/// enough to carry standalone context, small enough to stay retrieval-precise.
const PAPER_CHUNK_TARGET_CHARS: usize = 1200;

pub fn chunk_paper(text: &str) -> Vec<PendingChunk> {
    merge_small_chunks(section_aware_split(text), PAPER_CHUNK_TARGET_CHARS)
}
