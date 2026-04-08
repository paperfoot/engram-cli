//! `engram remember <content>` — store + embed a memory.
//!
//! Optionally extracts atomic (subject, predicate, object) facts via one LLM
//! call and inserts them into the `facts` table. If a new fact has the same
//! `(subject_norm, predicate)` as an existing active fact and a different
//! `object_norm`, the old fact is superseded by the new one (non-destructive —
//! the old row is preserved with `superseded_by` set).
//!
//! Toggle off with `--no-facts` for cheap bulk imports.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::Utc;
use engram_core::types::{Fact, Memory, MemorySource};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{extract_facts, normalize, ExtractedFact};
use engram_ingest::chunker::naive_split;
use engram_llm::openrouter::OpenRouterClient;
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

pub async fn run(
    ctx: &AppContext,
    content: String,
    importance: u8,
    tags: Vec<String>,
    diary: String,
    no_facts: bool,
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
        diary: diary.clone(),
        valid_from: None,
        valid_until: None,
        tags,
    };
    ctx.store.insert_memory(&memory)?;

    let chunks = naive_split(&content);
    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

    // Embed every chunk. Gemini if available (env or config file), stub otherwise.
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let (embeddings, model_name): (Vec<Vec<f32>>, &'static str) =
        if !force_stub && gemini_key.is_some() {
            let e = GeminiEmbedder::new(gemini_key.unwrap());
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

    // Optional fact extraction + contradiction detection.
    // Runs after the memory is safely persisted so a fact-extraction failure
    // never blocks a `remember` call.
    let mut facts_added = 0usize;
    let mut conflicts: Vec<serde_json::Value> = Vec::new();
    if !no_facts {
        let openrouter_key =
            crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter");
        if let Some(key) = openrouter_key {
            let extraction_model = std::env::var("ENGRAM_FACT_MODEL")
                .unwrap_or_else(|_| "openai/gpt-4o-mini".to_string());
            let llm = OpenRouterClient::new(key).with_model(extraction_model);
            match extract_facts(&llm, &content).await {
                Ok(extracted) => {
                    for ef in extracted {
                        match insert_fact_with_conflict_check(ctx, &memory, &diary, &ef) {
                            Ok(Some(conflict)) => {
                                conflicts.push(conflict);
                                facts_added += 1;
                            }
                            Ok(None) => facts_added += 1,
                            Err(e) => {
                                tracing::warn!("fact insert failed for {:?}: {e}", ef);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "fact extraction failed (continuing without facts): {e}"
                    );
                }
            }
        } else {
            tracing::debug!("OPENROUTER_API_KEY not set — skipping fact extraction");
        }
    }

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memory_id", memory.id.to_string());
    meta.add("chunks_stored", chunks.len());
    meta.add("embedder", model_name);
    meta.add("facts_added", facts_added);
    meta.add("conflicts_detected", conflicts.len());

    print_success(
        ctx.format,
        json!({
            "id": memory.id,
            "stored": true,
            "chunks": chunks.len(),
            "facts_added": facts_added,
            "conflicts": conflicts,
        }),
        meta,
        |_| {
            println!(
                "Stored memory {} ({} chunks, {} facts, {} conflicts)",
                memory.id,
                chunks.len(),
                facts_added,
                conflicts.len()
            )
        },
    );
    Ok(())
}

/// Insert one extracted fact into the store. If an active fact already exists
/// for the same `(subject_norm, predicate)` in the same diary with a different
/// `object_norm`, supersede it. Returns `Some(conflict_json)` describing the
/// supersession, or `None` if there was no conflict.
fn insert_fact_with_conflict_check(
    ctx: &AppContext,
    memory: &Memory,
    diary: &str,
    ef: &ExtractedFact,
) -> Result<Option<serde_json::Value>, CliError> {
    let subject_norm = normalize(&ef.subject);
    let object_norm = normalize(&ef.object);
    if subject_norm.is_empty() || ef.predicate.is_empty() || object_norm.is_empty() {
        return Ok(None);
    }

    let existing = ctx
        .store
        .get_active_facts(&subject_norm, &ef.predicate, diary)?;

    let new_fact = Fact {
        id: Uuid::new_v4(),
        source_memory_id: memory.id,
        subject: ef.subject.clone(),
        subject_norm: subject_norm.clone(),
        predicate: ef.predicate.clone(),
        object: ef.object.clone(),
        object_norm: object_norm.clone(),
        confidence: ef.confidence,
        created_at: Utc::now(),
        superseded_by: None,
        superseded_at: None,
        diary: diary.to_string(),
    };

    // Find any active fact whose object differs from the new one.
    let mut conflict_payload: Option<serde_json::Value> = None;
    for old in &existing {
        if old.object_norm == object_norm {
            // Same fact already known — skip insert (de-dup) and return.
            return Ok(None);
        }
        // Different value for same (subject, predicate) in same diary → conflict.
        if conflict_payload.is_none() {
            conflict_payload = Some(json!({
                "subject": old.subject,
                "predicate": old.predicate,
                "old_object": old.object,
                "new_object": new_fact.object,
                "old_fact_id": old.id.to_string(),
                "new_fact_id": new_fact.id.to_string(),
                "old_created_at": old.created_at.to_rfc3339(),
            }));
        }
        // Supersede every active prior with a different value (typically just one).
        ctx.store.supersede_fact(old.id, new_fact.id)?;
    }

    ctx.store.insert_fact(&new_fact)?;
    Ok(conflict_payload)
}
