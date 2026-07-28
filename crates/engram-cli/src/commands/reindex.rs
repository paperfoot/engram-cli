//! `engram reindex` — migrate chunk embeddings to the active model metadata.

use crate::commands::usage::{estimated_tokens_for_texts, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_storage::SqliteStore;
use serde::Serialize;
use serde_json::json;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct ReindexStats {
    pub chunks_reindexed: usize,
    pub kb: String,
    pub model: String,
    pub dimensions: usize,
    pub prompt_format: String,
    /// Set when a fallback key served the rebuild — the primary needs fixing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
}

pub async fn run(
    ctx: &AppContext,
    kb: Option<String>,
    all: bool,
    offline: bool,
) -> Result<(), CliError> {
    if !all && kb.is_none() {
        return Err(CliError::BadInput(
            "reindex requires --kb <name> or --all".into(),
        ));
    }
    let start = Instant::now();
    let stats = reindex_store(&ctx.store, kb.as_deref(), all, offline).await?;

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("chunks_reindexed", stats.chunks_reindexed);
    meta.add("model", stats.model.clone());
    meta.add("dimensions", stats.dimensions);
    meta.add("prompt_format", stats.prompt_format.clone());
    print_success(ctx.format, json!(stats), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

pub async fn reindex_store(
    store: &SqliteStore,
    kb: Option<&str>,
    all: bool,
    offline: bool,
) -> Result<ReindexStats, CliError> {
    if !all && kb.is_none() {
        return Err(CliError::BadInput("reindex requires kb or all=true".into()));
    }
    let force_stub = crate::commands::config::stub_forced();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    // Reindex is the recovery path the mixed-embedding error tells users to
    // run. Running it keyless used to REPLACE a cloud-embedded index with
    // 64-dim stub vectors and report success — the store it was meant to
    // repair came out unsearchable. Same write policy as remember/edit now:
    // no key means a loud refusal, and --offline (or the truthy test hook)
    // is the explicit way to ask for stub vectors.
    if gemini_key.is_none() && !force_stub && !offline {
        return Err(CliError::Config(
            "`engram reindex` needs a Gemini API key to re-embed the store, but none is \
             configured. Set it with `engram config set keys.gemini <KEY>` (or \
             GEMINI_API_KEY), or pass --offline (API: \"offline\": true) to rebuild with \
             the deterministic offline stub — that replaces every selected embedding with \
             64-dim stub vectors, which only `--profile offline` reads can search."
                .into(),
        ));
    }
    let have_gemini = gemini_key.is_some() && !force_stub && !offline;

    let chunks = store.list_chunks_for_reindex(if all { None } else { kb })?;
    let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let mut degraded_note: Option<String> = None;
    let (embeddings, model, dims, prompt_format) = if have_gemini {
        let embedder = GeminiEmbedder::new(gemini_key.unwrap())
            .with_fallback_keys(crate::commands::config::gemini_fallback_keys());
        let embeddings = embedder
            .embed_batch(&texts, TaskMode::RetrievalDocument)
            .await?;
        degraded_note = engram_embed::Embedder::degradation_note(&embedder);
        (
            embeddings,
            embedder.model(),
            embedder.dimensions(),
            embedder.prompt_format().to_string(),
        )
    } else {
        let embedder = StubEmbedder::default();
        let embeddings = embedder
            .embed_batch(&texts, TaskMode::RetrievalDocument)
            .await?;
        (
            embeddings,
            embedder.model(),
            embedder.dimensions(),
            embedder.prompt_format().to_string(),
        )
    };

    if embeddings.len() != chunks.len() {
        return Err(CliError::Transient(format!(
            "embedder returned {} embeddings for {} chunks; aborting reindex so no chunk is \
             silently left on the old embedding",
            embeddings.len(),
            chunks.len()
        )));
    }
    store.with_tx(|store| {
        for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
            store.set_chunk_embedding_meta(chunk.chunk_id, emb, &model, dims, &prompt_format)?;
        }
        Ok(())
    })?;
    if have_gemini && !texts.is_empty() {
        let tokens = estimated_tokens_for_texts(&texts);
        store.record_usage_event(
            "gemini",
            "reindex_embed",
            Some(&model),
            if all { None } else { kb },
            None,
            1,
            texts.len() as i64,
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

    Ok(ReindexStats {
        chunks_reindexed: chunks.len(),
        kb: if all {
            "*".to_string()
        } else {
            kb.unwrap_or("default").to_string()
        },
        model,
        dimensions: dims,
        prompt_format,
        degraded: degraded_note,
    })
}
