//! `engram edit <id>` — update a memory's content or importance.
//!
//! If `--content` is provided, the memory's chunks are deleted, the new
//! content is re-chunked through the SAME policy `remember` uses
//! (`chunk_memory` + `chunks::resolve_policy`), re-embedded, and reinserted.
//! This keeps `chunks.content` (what `recall` actually searches) in sync with
//! `memories.content`, and keeps an edited memory from being the one write
//! path that still emits degenerate sub-60-character fragments.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use crate::retrieval::{resolve_write_embedder, WriteEmbedder};
use engram_embed::TaskMode;
use engram_ingest::memory::chunk_memory;
use serde_json::json;
use uuid::Uuid;

pub async fn run(
    ctx: &AppContext,
    id: String,
    content: Option<String>,
    importance: Option<u8>,
    offline: bool,
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
    if let Some(c) = content.as_deref() {
        // An empty rewrite would chunk to nothing: the transaction below
        // deletes every existing chunk and would insert zero, leaving a
        // committed memory invisible to recall — the same black hole the
        // atomic `remember` fix closed. `remember` rejects empty content;
        // `edit` must too. Removing a memory is `forget`'s job.
        if c.trim().is_empty() {
            return Err(CliError::BadInput(
                "content cannot be empty; use `engram forget <id>` to remove a memory".into(),
            ));
        }
    }
    let uuid =
        Uuid::parse_str(&id).map_err(|e| CliError::BadInput(format!("invalid UUID {id}: {e}")))?;

    // EXISTENCE FIRST, and in the same breath the scope. A syntactically valid
    // but unknown uuid used to reach the embedder: the paid call was made, the
    // transaction then found no row and rolled back, and the caller was billed
    // for editing a memory that does not exist. Loading the row up front also
    // supplies the (kb, diary) that the budget guard and the usage row need —
    // `edit_embed` used to carry neither, so edit spend was invisible to
    // `usage --kb` and to every per-KB cap. Exit 3, matching every other
    // "the id you gave names nothing" surface (`forget`, `people show`,
    // `facts resolve`) and the importance-only branch below.
    let Some((kb, diary)) = ctx.store.memory_scope(uuid)? else {
        return Err(CliError::BadInput(format!(
            "no memory found for id {id} (already deleted?)"
        )));
    };

    let mut chunks_replaced = 0usize;
    if let Some(new_content) = content.as_deref() {
        // Paid calls respect the KB budget guardrail — the same rule
        // `remember`, `recall`, `research` and both daemon endpoints follow.
        // Only the content path spends: an importance-only update makes no
        // provider call, and refusing a free metadata edit on a spend cap
        // would be a new failure mode with no money behind it.
        crate::commands::budget::enforce_budget(ctx, Some(&kb))?;

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
        // Invariant, not just input validation: whatever the chunker does in
        // the future, a rewrite that would leave this memory with zero chunks
        // must never reach the delete-and-reinsert below.
        if new_chunks.is_empty() {
            return Err(CliError::BadInput(
                "new content produced no chunks; memory left unchanged".into(),
            ));
        }

        // Same rule as `remember`: no key and no `--offline` is a loud config
        // error, never a silent 64-dim stub written into a cloud-embedded
        // store. This sits INSIDE the --content branch on purpose — an
        // `--importance`-only edit re-embeds nothing and needs no key at all —
        // and after the empty-content guards, so a bad argument is still exit 3
        // rather than being masked by the environment.
        let WriteEmbedder {
            embedder,
            cloud: have_gemini,
            model: model_name,
            dims,
            prompt_format,
        } = resolve_write_embedder("edit", offline)?;

        // Embed the NEW content first. The old chunks are only torn down
        // once the replacement vectors are in hand, so a transient embed
        // failure can never strand the memory with no chunks (invisible to
        // recall, unrecoverable).
        let chunk_texts: Vec<&str> = new_chunks.iter().map(|c| c.text.as_str()).collect();
        // Test seam, shared with `remember`: fail exactly where a network embed
        // would, after chunking and before any spend or any write.
        if let Some(injected) = crate::commands::remember::injected_embed_failure("edit") {
            return Err(injected);
        }
        let embeddings = match embedder
            .embed_batch_accounted(&chunk_texts, TaskMode::RetrievalDocument)
            .await
        {
            Ok(v) => v,
            Err(failure) => {
                // Whatever the provider already served is on the invoice, even
                // though this command is about to die.
                if have_gemini && failure.charged_texts > 0 {
                    crate::commands::usage::record_embed_spend(
                        &ctx.store,
                        "edit_embed",
                        &model_name,
                        Some(&kb),
                        Some(&diary),
                        &chunk_texts[..failure.charged_texts],
                        failure.charged_requests as i64,
                        json!({
                            "memory_id": uuid.to_string(),
                            "dimensions": dims,
                            "prompt_format": prompt_format.clone(),
                            "estimated": true,
                            "partial": true,
                            "chunks_requested": chunk_texts.len(),
                        }),
                    );
                }
                return Err(failure.error.into());
            }
        };
        // Paid the moment it returned, so recorded BEFORE — and independently
        // of — the transaction that uses it. Recording it after with `?` turned
        // a bookkeeping failure into a non-zero exit on an edit that had
        // already committed, and the caller's natural response is a paid retry.
        if have_gemini {
            crate::commands::usage::record_embed_spend(
                &ctx.store,
                "edit_embed",
                &model_name,
                Some(&kb),
                Some(&diary),
                &chunk_texts,
                1,
                json!({
                    "memory_id": uuid.to_string(),
                    "dimensions": dims,
                    "prompt_format": prompt_format.clone(),
                    "estimated": true,
                    "partial": false,
                }),
            );
        }
        if embeddings.len() != new_chunks.len() {
            return Err(CliError::Transient(format!(
                "embedder returned {} embeddings for {} chunks; aborting edit",
                embeddings.len(),
                new_chunks.len()
            )));
        }

        // Atomic swap: update content, tear down stale chunks/facts/derived
        // claims, insert the new chunks — all or nothing. The NotFound below is
        // now only reachable by a `forget` racing this edit, which is a genuine
        // transient (exit 1), not the caller's typo (exit 3, above).
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
    } else {
        // Importance-only update: no chunk rebuild, no provider call, no spend.
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
