//! `engram edit <id>` — update a memory's content or importance.
//!
//! If `--content` is provided, the memory's chunks are deleted, the new
//! content is re-chunked through the SAME policy `remember` uses
//! (`chunk_memory` + `chunks::resolve_policy`), re-embedded, and reinserted.
//! This keeps `chunks.content` (what `recall` actually searches) in sync with
//! `memories.content`, and keeps an edited memory from being the one write
//! path that still emits degenerate sub-60-character fragments.

use crate::commands::usage::{estimated_tokens_for_texts, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_ingest::memory::chunk_memory;
use serde_json::json;
use uuid::Uuid;

pub async fn run(
    ctx: &AppContext,
    id: String,
    content: Option<String>,
    importance: Option<u8>,
) -> Result<(), CliError> {
    if content.is_none() && importance.is_none() {
        return Err(CliError::BadInput(
            "provide --content and/or --importance to update".into(),
        ));
    }
    if let Some(i) = importance {
        if i > 10 {
            return Err(CliError::BadInput("importance must be 0..=10".into()));
        }
    }
    let uuid =
        Uuid::parse_str(&id).map_err(|e| CliError::BadInput(format!("invalid UUID {id}: {e}")))?;

    let mut chunks_replaced = 0usize;
    if let Some(new_content) = content.as_deref() {
        let document_id = ctx.store.document_for_memory(uuid)?;
        // CHUNKING. This was a bare `naive_split`: paragraph split, no merge,
        // no floor, no ceiling. Editing a memory therefore re-created exactly
        // the fragments `remember` stopped producing — a lone `Sep 16, 2025`
        // became its own embedded chunk and, being nearly pure signal for the
        // few tokens it holds, outranked the paragraph that answers the query.
        // One chunker for every write path: same floor, same ceiling, same
        // `chunking.*` config knobs.
        let policy = crate::commands::chunks::resolve_policy();
        let new_chunks = chunk_memory(new_content, policy);

        let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
        let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
        let have_gemini = !force_stub && gemini_key.is_some();

        // Embed the NEW content first. The old chunks are only torn down
        // once the replacement vectors are in hand, so a transient embed
        // failure can never strand the memory with no chunks (invisible to
        // recall, unrecoverable).
        let chunk_texts: Vec<&str> = new_chunks.iter().map(|c| c.text.as_str()).collect();
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
        if embeddings.len() != new_chunks.len() {
            return Err(CliError::Transient(format!(
                "embedder returned {} embeddings for {} chunks; aborting edit",
                embeddings.len(),
                new_chunks.len()
            )));
        }

        // Atomic swap: update content, tear down stale chunks/facts/derived
        // claims, insert the new chunks — all or nothing.
        chunks_replaced = ctx.store.with_tx(|store| {
            let updated = store.update_memory(uuid, content.as_deref(), importance)?;
            if !updated {
                return Err(engram_storage::StorageError::NotFound(format!(
                    "no memory found for id {uuid} (already deleted?)"
                )));
            }
            store.delete_facts_for_memory(uuid)?;
            store.delete_derived_for_memory_chunks(uuid)?;
            store.delete_chunks_for_memory(uuid)?;
            let mut replaced = 0usize;
            for (chunk, emb) in new_chunks.iter().zip(embeddings.iter()) {
                store.insert_chunk_with_embedding_meta(
                    Uuid::new_v4(),
                    uuid,
                    &chunk.text,
                    chunk.position,
                    chunk.section.as_deref(),
                    emb,
                    &model_name,
                    dims,
                    &prompt_format,
                    document_id,
                )?;
                replaced += 1;
            }
            Ok(replaced)
        })?;
        if have_gemini && !chunk_texts.is_empty() {
            let tokens = estimated_tokens_for_texts(&chunk_texts);
            ctx.store.record_usage_event(
                "gemini",
                "edit_embed",
                Some(&model_name),
                None,
                None,
                1,
                chunk_texts.len() as i64,
                tokens,
                0,
                0.0,
                gemini_embed_cost_usd(tokens),
                json!({
                    "memory_id": uuid.to_string(),
                    "dimensions": dims,
                    "prompt_format": prompt_format,
                    "estimated": true,
                }),
            )?;
        }
    } else {
        // Importance-only update: no chunk rebuild needed.
        let updated = ctx.store.update_memory(uuid, None, importance)?;
        if !updated {
            return Err(CliError::BadInput(format!(
                "no memory found for id {id} (already deleted?)"
            )));
        }
    }

    print_success(
        ctx.format,
        json!({
            "id": id,
            "updated": true,
            "chunks_replaced": chunks_replaced,
        }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
