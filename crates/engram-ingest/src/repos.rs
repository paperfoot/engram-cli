//! Repository ingestion — README, docs, code comments. v0 stub.

use crate::chunker::{naive_split, PendingChunk};

pub fn chunk_repo_text(text: &str) -> Vec<PendingChunk> {
    naive_split(text)
}
