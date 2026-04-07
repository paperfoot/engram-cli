//! General mode — auto-classify content into the right mode. v0 stub.

use crate::chunker::{naive_split, PendingChunk};

pub fn chunk_general(text: &str) -> Vec<PendingChunk> {
    naive_split(text)
}
