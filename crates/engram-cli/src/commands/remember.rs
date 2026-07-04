//! `engram remember <content>` — store + embed a memory.
//!
//! Facts enter through two doors: `--extract-facts` (one LLM extraction
//! call) or `--facts-json` (the caller supplies triples directly, no LLM).
//! Every fact then runs the write-path v2 pipeline in `fact_write`: predicate
//! and subject canonicalization, exact-match dedup/supersession, slot
//! policies, and the embedding novelty gate. Uncertain contradictions are
//! queued for the calling agent under the default `assisted` autonomy —
//! see `facts resolve`.

use crate::commands::usage::{estimated_tokens, estimated_tokens_for_texts, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::Utc;
use crate::fact_write::{write_fact, Autonomy, FactWriteOutcome};
use engram_core::types::{Memory, MemorySource, SourceKind};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{extract_facts, ExtractedFact};
use engram_ingest::chunker::naive_split;
use engram_llm::openrouter::OpenRouterClient;
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    ctx: &AppContext,
    content: String,
    importance: u8,
    tags: Vec<String>,
    diary: String,
    kb: String,
    extract_facts_enabled: bool,
    allow_duplicate: bool,
    autonomy: String,
    facts_json: Option<String>,
) -> Result<(), CliError> {
    let autonomy = Autonomy::parse(&autonomy)?;
    // Paid calls (embedding, extraction) respect the KB budget guardrail.
    crate::commands::budget::enforce_budget(ctx, Some(&kb))?;
    if content.trim().is_empty() {
        return Err(CliError::BadInput("content cannot be empty".into()));
    }
    if importance > 10 {
        return Err(CliError::BadInput("importance must be 0..=10".into()));
    }

    let start = Instant::now();

    // Dedup: saving an identical fact into the same (kb, diary) is a no-op
    // rather than a second row + a second embedding charge. Opt out with
    // --allow-duplicate if you truly want a second copy.
    if !allow_duplicate {
        if let Some(existing) = ctx.store.find_duplicate_memory(&kb, &diary, &content)? {
            let mut meta = Metadata::default();
            meta.add("deduplicated", true);
            meta.add("existing_id", existing.to_string());
            print_success(
                ctx.format,
                json!({
                    "id": existing.to_string(),
                    "stored": false,
                    "deduplicated": true,
                    "kb": kb,
                    "reason": "identical content already stored in this kb/diary; nothing re-embedded",
                }),
                meta,
                |_| println!("Duplicate of {existing} — nothing stored (use --allow-duplicate to force)"),
            );
            return Ok(());
        }
    }

    let now = Utc::now();
    let memory = Memory {
        id: Uuid::new_v4(),
        content: content.clone(),
        created_at: now,
        event_time: None,
        importance,
        emotional_weight: 0,
        access_count: 0,
        last_accessed: None,
        stability: 1.0,
        source: MemorySource::Manual,
        diary: diary.clone(),
        // A stored fact is valid from the moment it is recorded; superseding
        // facts close this window later. Keeps the temporal model populated
        // instead of leaving valid_from perpetually NULL.
        valid_from: Some(now),
        valid_until: None,
        tags,
    };
    ctx.store.insert_memory_with_kb(&memory, &kb)?;

    let chunks = naive_split(&content);
    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

    // Embed every chunk. Gemini if available (env or config file), stub otherwise.
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let have_gemini = !force_stub && gemini_key.is_some();
    let (embeddings, model_name, dims, prompt_format): (Vec<Vec<f32>>, String, usize, String) =
        if have_gemini {
            let e = GeminiEmbedder::new(gemini_key.unwrap());
            let v = e
                .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await?;
            (v, e.model(), e.dimensions(), e.prompt_format().to_string())
        } else {
            let e = StubEmbedder::default();
            let v = e
                .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await?;
            (v, e.model(), e.dimensions(), e.prompt_format().to_string())
        };

    for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
        ctx.store.insert_chunk_with_embedding_meta(
            Uuid::new_v4(),
            memory.id,
            &chunk.text,
            chunk.position,
            chunk.section.as_deref(),
            emb,
            &model_name,
            dims,
            &prompt_format,
            None,
        )?;
    }
    if have_gemini && !chunk_texts.is_empty() {
        let tokens = estimated_tokens_for_texts(&chunk_texts);
        ctx.store.record_usage_event(
            "gemini",
            "remember_embed",
            Some(&model_name),
            Some(&kb),
            Some(&diary),
            1,
            chunk_texts.len() as i64,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({
                "dimensions": dims,
                "prompt_format": prompt_format.clone(),
                "estimated": true,
            }),
        )?;
    }

    // Fact processing (extraction and/or caller-supplied triples) runs after
    // the memory is safely persisted so a fact failure never blocks `remember`.
    let mut extracted: Vec<ExtractedFact> = Vec::new();
    if let Some(raw) = &facts_json {
        extracted.extend(
            serde_json::from_str::<Vec<ExtractedFact>>(raw)
                .map_err(|e| CliError::BadInput(format!("--facts-json is not a valid fact array: {e}")))?,
        );
    }

    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter");
    let extraction_model = std::env::var("ENGRAM_FACT_MODEL").unwrap_or_else(|_| {
        crate::commands::config::resolve_setting(
            "ENGRAM_COMPILER_EXTRACTION_MODEL",
            "compiler.extraction_model",
            engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL,
        )
    });
    let llm = openrouter_key
        .clone()
        .map(|key| OpenRouterClient::new(key).with_model(extraction_model.clone()));

    if extract_facts_enabled {
        if let Some(llm) = &llm {
            ctx.store.record_usage_event(
                "openrouter",
                "remember_fact_extract",
                Some(&extraction_model),
                Some(&kb),
                Some(&diary),
                1,
                1,
                estimated_tokens(&content),
                0,
                0.0,
                0.0,
                json!({ "estimated": true }),
            )?;
            match extract_facts(llm, &content).await {
                Ok(more) => extracted.extend(more),
                Err(e) => {
                    tracing::warn!("fact extraction failed (continuing without facts): {e}");
                }
            }
        } else {
            tracing::debug!("OPENROUTER_API_KEY not set — skipping fact extraction");
        }
    }

    let mut outcome = FactWriteOutcome::default();
    if !extracted.is_empty() {
        // The novelty gate uses the same embedder family as the chunks above.
        let gate_embedder: Option<Box<dyn Embedder>> = if have_gemini {
            crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
                .map(|k| Box::new(GeminiEmbedder::new(k)) as Box<dyn Embedder>)
        } else {
            Some(Box::new(StubEmbedder::default()) as Box<dyn Embedder>)
        };
        for ef in &extracted {
            if let Err(e) = write_fact(
                ctx,
                memory.id,
                &diary,
                &kb,
                ef,
                gate_embedder.as_deref(),
                llm.as_ref().map(|l| l as &dyn engram_llm::ChatLlm),
                autonomy,
                SourceKind::AgentExtracted,
                &mut outcome,
            )
            .await
            {
                tracing::warn!("fact write failed for {:?}: {e}", ef);
            }
        }
    }
    let facts_added = outcome.added;
    let conflicts = outcome.conflicts;
    let adjudication_needed = outcome.queued;
    let facts_deduped = outcome.deduped;

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memory_id", memory.id.to_string());
    meta.add("chunks_stored", chunks.len());
    meta.add("embedder", model_name.clone());
    meta.add("embed_dimensions", dims);
    meta.add("embed_prompt_format", prompt_format.clone());
    meta.add("kb", kb.clone());
    meta.add("facts_added", facts_added);
    meta.add("facts_deduped", facts_deduped);
    meta.add("conflicts_detected", conflicts.len());
    meta.add("conflicts_queued", adjudication_needed.len());

    print_success(
        ctx.format,
        json!({
            "id": memory.id,
            "stored": true,
            "chunks": chunks.len(),
            "kb": kb,
            "facts_added": facts_added,
            "facts_deduped": facts_deduped,
            "conflicts": conflicts,
            "adjudication_needed": adjudication_needed,
        }),
        meta,
        |_| {
            println!(
                "Stored memory {} ({} chunks, {} facts, {} conflicts, {} queued for resolution)",
                memory.id,
                chunks.len(),
                facts_added,
                conflicts.len(),
                adjudication_needed.len()
            )
        },
    );
    Ok(())
}
