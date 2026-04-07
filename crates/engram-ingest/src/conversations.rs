//! Conversation ingestion — Claude/ChatGPT exports. v0 stub.

use crate::chunker::{naive_split, PendingChunk};

pub fn chunk_conversation(text: &str) -> Vec<PendingChunk> {
    naive_split(text)
}
