//! `engram edit <id>` — update a memory's content or importance.
//!
//! If `--content` is provided, the memory's chunks are deleted, the new
//! content is re-chunked through the SAME policy `remember` uses
//! (`chunk_memory` + `chunks::resolve_policy`), re-embedded, and reinserted.
//! This keeps `chunks.content` (what `recall` actually searches) in sync with
//! `memories.content`, and keeps an edited memory from being the one write
//! path that still emits degenerate sub-60-character fragments.
//!
//! **An edit no longer deletes facts.** v0.3 called `delete_facts_for_memory`,
//! which `DELETE`d every row derived from the old content and took the
//! adjudication queue with it through the FK cascade — so a typo fix erased
//! the questions a person had already answered about those claims. Facts
//! derived from the superseded text are now CLOSED as intervals
//! (`close_reason = "corrected"`, authority `caller`): they stop asserting
//! anything from the moment of the edit and keep their history, their
//! conflicts and their decisions. Chunks remain the one thing this command
//! still deletes — they are a rebuildable index of `memories.content`, not
//! evidence, and `memory.revised` carries what a rebuild needs.

use crate::commands::memory::{self, MemoryTarget};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use crate::retrieval::{resolve_write_embedder, WriteEmbedder};
use engram_core::interval::{CloseAuthority, CloseReason};
use engram_embed::TaskMode;
use engram_ingest::memory::chunk_memory;
use engram_storage::SqliteStore;
use serde_json::json;
use uuid::Uuid;

/// What the atomic swap did, beyond replacing chunks. The closure used to
/// return a bare `usize`; the envelope now has to report which fact intervals
/// the edit closed and whether the memory is pending re-extraction, and both
/// are decided inside the transaction or not at all.
struct EditTxOut {
    chunks_replaced: usize,
    facts_invalidated: Vec<Uuid>,
    /// This exact content has already been derived by this exact extractor and
    /// those facts are still open, so nothing was invalidated and nothing is
    /// pending — the `"replayed"` half of W4.
    replayed: bool,
}

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
    // ANY handle kind, not just a bare uuid. `mem:<uuid>` is the form every new
    // surface prints, a `chunk:` id copied out of a recall result now edits the
    // memory it belongs to, and a wrong kind gets a typed refusal naming what
    // the id actually is. `memory update` routes here too, so the command named
    // in the message is `edit` — the alias's own name would not be runnable
    // from `forget`'s side of the dispatch.
    //
    // EXISTENCE FIRST, and in the same breath the scope. A syntactically valid
    // but unknown uuid used to reach the embedder: the paid call was made, the
    // transaction then found no row and rolled back, and the caller was billed
    // for editing a memory that does not exist. Resolving up front also
    // supplies the (kb, diary) that the budget guard and the usage row need —
    // `edit_embed` used to carry neither, so edit spend was invisible to
    // `usage --kb` and to every per-KB cap.
    let target = memory::resolve_memory_target(ctx, &id, "edit")?;
    // A retracted memory is not editable: `forget` closed its facts and hid its
    // content, and rewriting it would resurrect a source the caller retired.
    // Exit 3 with the honest code, where v0.3 could only guess "(already
    // deleted?)" and say `bad_input`.
    if target.retracted {
        return Err(CliError::NotFound {
            what: "edit",
            handle: id.clone(),
        });
    }
    let actor = target.actor();
    let MemoryTarget {
        memory_id: uuid,
        kb,
        diary,
        handle,
        resolved_from,
        ..
    } = target;

    let mut chunks_replaced = 0usize;
    let mut facts_invalidated: Vec<Uuid> = Vec::new();
    // "none" until a `--content` edit says otherwise: an importance change
    // invalidates nothing and re-derives nothing.
    let mut reextraction = "none";
    let mut degraded_note: Option<String> = None;
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

        // The KB budget guards PAID calls only, so it is checked after the
        // embedder is resolved: a cloud re-embed spends, an --offline edit
        // spends nothing, and a zero spend cap must not block a free write.
        if have_gemini {
            crate::commands::budget::enforce_budget(ctx, Some(&kb))?;
        }

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

        // Atomic swap: update content, close the facts the old text supported,
        // tear down stale chunks and derived claims, insert the new chunks —
        // all or nothing, and every event lands in the same transaction as the
        // effect that earned it (storage appends them; D7).
        let new_hash = engram_core::keys::content_hash(new_content);
        let out = ctx
            .store
            .with_tx(|store| {
                // The ledger is consulted BEFORE anything closes, or the answer
                // is a description of what this command just did rather than of
                // what was already true.
                let replayed = derivation_is_live(store, uuid, &new_hash)?;
                // `update_memory` appends `memory.revised` itself, with both
                // hashes, inside this transaction — this command must NOT
                // append it a second time (D7). The NotFound below is now only
                // reachable by a `forget` racing this edit, which is a genuine
                // transient (exit 1), not the caller's typo (exit 3, above).
                let updated = store.update_memory(uuid, content.as_deref(), importance)?;
                if !updated {
                    return Err(engram_storage::StorageError::NotFound(format!(
                        "no memory found for id {uuid} (already deleted?)"
                    )));
                }
                // THE MOVE-1 FIX. Not `delete_facts_for_memory`: the old
                // content's claims stop being asserted, they do not stop having
                // existed. `corrected`/`caller` per D5's pinned table — an edit
                // is a correction, and stamping it `retracted_source` would
                // erase the distinction `forget` needs.
                let facts_invalidated = if replayed {
                    Vec::new()
                } else {
                    store.close_facts_for_memory(
                        uuid,
                        chrono::Utc::now(),
                        CloseReason::Corrected,
                        &CloseAuthority::Caller {
                            actor: actor.clone(),
                        },
                        &actor,
                    )?
                };
                // The marker clear is CONDITIONAL and welded to the same
                // transaction: a ledger hit means this content's facts are
                // already derived and still open, so the memory is not pending
                // and `facts backfill` must not pick it up again. A miss means
                // the facts just closed have no replacement, so the marker goes
                // and the memory is pending. Clearing it unconditionally is
                // what made `reextraction: "replayed"` impossible to report
                // honestly.
                if !replayed {
                    store.clear_extraction_marker(uuid)?;
                }
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
                Ok(EditTxOut {
                    chunks_replaced: replaced,
                    facts_invalidated,
                    replayed,
                })
            })
            // R11 maps `StorageError::NotFound` to exit 3, which is right for a
            // caller's typo and wrong here: the id existed when this command
            // resolved it, so a missing row means a `forget` committed while
            // the embedder was working. That is transient, and a retry loop
            // that stops retrying on it has been told the wrong thing.
            .map_err(|e| match e {
                engram_storage::StorageError::NotFound(m) => CliError::Transient(format!(
                    "memory {uuid} was retracted while this edit was in flight: {m}"
                )),
                other => CliError::from(other),
            })?;
        chunks_replaced = out.chunks_replaced;
        facts_invalidated = out.facts_invalidated;
        reextraction = if out.replayed { "replayed" } else { "pending" };
        degraded_note = embedder.degradation_note();
    } else {
        // Importance-only update: no chunk rebuild, no provider call, no spend,
        // and no fact touched. `update_memory` appends `memory.meta_changed`
        // inside its own transaction, so the event and the effect still commit
        // together — today this branch was a bare update with no transaction
        // at all.
        let updated = ctx.store.update_memory(uuid, None, importance)?;
        if !updated {
            // The id resolved a moment ago, so this is a racing `forget`, not a
            // bad argument. Exit 1, and say which race it was.
            return Err(CliError::Transient(format!(
                "memory {uuid} was retracted while this edit was in flight"
            )));
        }
    }

    let invalidated_ids: Vec<String> = facts_invalidated.iter().map(|f| f.to_string()).collect();
    let mut meta = Metadata::default();
    if let Some(note) = degraded_note {
        meta.add("embedder_degraded", note.clone());
        eprintln!("warning: {note}");
    }
    // Half the agent surface reads `metadata` and half reads `data`; both carry
    // the same answer rather than one of them carrying it.
    meta.add("handle", handle.clone());
    if let Some(from) = &resolved_from {
        meta.add("resolved_from", from.clone());
    }
    meta.add("kb", kb.clone());
    meta.add("chunks_replaced", chunks_replaced);
    meta.add("facts_invalidated", facts_invalidated.len());
    meta.add("reextraction", reextraction);
    let mut data = serde_json::Map::new();
    // The canonical uuid, never the caller's raw spelling: `data.id` has always
    // been a bare uuid and scripts feed it straight back in.
    data.insert("id".into(), json!(uuid.to_string()));
    data.insert("handle".into(), json!(handle));
    if let Some(from) = &resolved_from {
        data.insert("resolved_from".into(), json!(from));
    }
    data.insert("updated".into(), json!(true));
    data.insert("chunks_replaced".into(), json!(chunks_replaced));
    // CLOSED, not deleted — the count is of intervals that stopped asserting,
    // and every one of them is still readable with its history.
    data.insert("facts_invalidated".into(), json!(facts_invalidated.len()));
    data.insert("facts_invalidated_ids".into(), json!(invalidated_ids));
    data.insert("reextraction".into(), json!(reextraction));
    print_success(ctx.format, serde_json::Value::Object(data), meta, |data| {
        println!(
            "Updated {} ({} chunks, {} facts closed, re-extraction {})",
            data["handle"].as_str().unwrap_or_default(),
            data["chunks_replaced"],
            data["facts_invalidated"],
            data["reextraction"].as_str().unwrap_or_default(),
        )
    });
    Ok(())
}

/// Has this exact content already been derived by this exact extractor, with
/// the outputs still standing?
///
/// The ledger alone cannot answer it. A hit says the triples were derived once;
/// it does not say they are still asserted, and after an A→B→A edit the row for
/// A's hash points at facts the FIRST edit already closed. Reporting
/// `"replayed"` there would tell an agent the facts are current when nothing in
/// wave 3 re-observes them. So the hit must also belong to THIS memory and its
/// outputs must still be open; anything else is `"pending"`, which is the
/// honest answer and the one that gets the memory re-extracted.
fn derivation_is_live(
    store: &SqliteStore,
    memory_id: Uuid,
    content_hash: &str,
) -> Result<bool, engram_storage::StorageError> {
    let Some(row) =
        store.derivation_for(content_hash, engram_storage::journal::EXTRACTOR_FACT_LLM)?
    else {
        return Ok(false);
    };
    if row.memory_id != memory_id || row.output_fact_ids.is_empty() {
        return Ok(false);
    }
    for id in &row.output_fact_ids {
        match store.get_fact(*id)? {
            Some(f) if engram_core::interval::is_open_fact(&f) => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}
