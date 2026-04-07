//! `engram remember <content>`

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::Utc;
use engram_core::types::{Memory, MemorySource};
use engram_ingest::chunker::naive_split;
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

pub async fn run(
    ctx: &AppContext,
    content: String,
    importance: u8,
    tags: Vec<String>,
    diary: String,
) -> Result<(), CliError> {
    if content.trim().is_empty() {
        return Err(CliError::BadInput("content cannot be empty".into()));
    }
    if importance > 10 {
        return Err(CliError::BadInput("importance must be 0..=10".into()));
    }

    let start = Instant::now();
    let memory = Memory {
        id: Uuid::new_v4(),
        content: content.clone(),
        created_at: Utc::now(),
        event_time: None,
        importance,
        emotional_weight: 0,
        access_count: 0,
        last_accessed: None,
        stability: 1.0,
        source: MemorySource::Manual,
        diary,
        valid_from: None,
        valid_until: None,
        tags,
    };
    ctx.store.insert_memory(&memory)?;

    // Chunk and store. Embedding happens in Phase 2 once Gemini wiring lands.
    for chunk in naive_split(&content) {
        ctx.store.insert_chunk(
            Uuid::new_v4(),
            memory.id,
            &chunk.text,
            chunk.position,
            chunk.section.as_deref(),
        )?;
    }

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memory_id", memory.id.to_string());

    print_success(
        ctx.format,
        json!({ "id": memory.id, "stored": true }),
        meta,
        |_| println!("Stored memory {}", memory.id),
    );
    Ok(())
}
