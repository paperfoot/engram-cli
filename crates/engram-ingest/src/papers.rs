//! Paper ingestion — section-aware chunking for scientific PDFs/text.
//! v0 stub: forwards to naive splitter. Autoresearch target.

use crate::chunker::{naive_split, PendingChunk};

pub fn chunk_paper(text: &str) -> Vec<PendingChunk> {
    naive_split(text)
}
