//! `engram edit <id>` — update a memory's content or importance.
//!
//! If `--content` is provided, the memory's chunks are deleted, the new
//! content is re-split and re-embedded, then new chunks are inserted. This
//! keeps `chunks.content` (what `recall` actually searches) in sync with
//! `memories.content`.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_ingest::chunker::naive_split;
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
    let uuid = Uuid::parse_str(&id)
        .map_err(|e| CliError::BadInput(format!("invalid UUID {id}: {e}")))?;

    let updated = ctx
        .store
        .update_memory(uuid, content.as_deref(), importance)?;
    if !updated {
        return Err(CliError::BadInput(format!(
            "no memory found for id {id} (already deleted?)"
        )));
    }

    let mut chunks_replaced = 0usize;
    if let Some(new_content) = content.as_deref() {
        // Tear down stale chunks and rebuild so recall sees the edit.
        ctx.store.delete_chunks_for_memory(uuid)?;
        let new_chunks = naive_split(new_content);

        let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
        let gemini_key =
            crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");

        let chunk_texts: Vec<&str> = new_chunks.iter().map(|c| c.text.as_str()).collect();
        let (embeddings, model_name): (Vec<Vec<f32>>, &'static str) =
            if !force_stub && gemini_key.is_some() {
                let e = GeminiEmbedder::new(gemini_key.unwrap());
                let v = e.embed_batch(&chunk_texts, TaskMode::RetrievalDocument).await?;
                (v, "gemini-embedding-001")
            } else {
                let e = StubEmbedder::default();
                let v = e.embed_batch(&chunk_texts, TaskMode::RetrievalDocument).await?;
                (v, "stub")
            };

        for (chunk, emb) in new_chunks.iter().zip(embeddings.iter()) {
            ctx.store.insert_chunk_with_embedding(
                Uuid::new_v4(),
                uuid,
                &chunk.text,
                chunk.position,
                chunk.section.as_deref(),
                emb,
                model_name,
            )?;
            chunks_replaced += 1;
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
