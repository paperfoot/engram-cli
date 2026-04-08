//! `engram remember <content>` — store + embed a memory.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::Utc;
use engram_core::types::{Memory, MemorySource};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
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

    let chunks = naive_split(&content);
    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

    // Embed every chunk. Gemini if available, stub otherwise.
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let (embeddings, model_name): (Vec<Vec<f32>>, &'static str) =
        if !force_stub && std::env::var("GEMINI_API_KEY").is_ok() {
            let e = GeminiEmbedder::from_env()
                .map_err(|err| CliError::Config(format!("gemini: {err}")))?;
            let v = e
                .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await?;
            (v, "gemini-embedding-001")
        } else {
            let e = StubEmbedder::default();
            let v = e
                .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await?;
            (v, "stub")
        };

    for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
        ctx.store.insert_chunk_with_embedding(
            Uuid::new_v4(),
            memory.id,
            &chunk.text,
            chunk.position,
            chunk.section.as_deref(),
            emb,
            model_name,
        )?;
    }

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memory_id", memory.id.to_string());
    meta.add("chunks_stored", chunks.len());
    meta.add("embedder", model_name);

    print_success(
        ctx.format,
        json!({ "id": memory.id, "stored": true, "chunks": chunks.len() }),
        meta,
        |_| println!("Stored memory {} ({} chunks)", memory.id, chunks.len()),
    );
    Ok(())
}
