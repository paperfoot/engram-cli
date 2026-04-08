//! Paper ingestion — section-aware chunking for scientific papers.
//!
//! Delegates to `chunker::section_aware_split`, which tracks Markdown ATX,
//! setext, and numbered section headings so chunks carry `Methods > Cell Culture`
//! style breadcrumbs.

use crate::chunker::{section_aware_split, PendingChunk};

pub fn chunk_paper(text: &str) -> Vec<PendingChunk> {
    section_aware_split(text)
}
