//! `engram import <file>` — restore a store from an export JSON file.
//!
//! Understands three formats:
//! - **v3** (current): the journal plus the v2 sections. The events are
//!   REPLAYED through the same dispatcher `admin rebuild` uses, so an imported
//!   store carries its own history — every interval close, every conflict,
//!   every decision a person made — instead of only the conclusions. Chunk
//!   embeddings ride in the `chunks` section because replay cannot re-embed
//!   without paying a provider.
//! - **v2**: full-fidelity restore of KBs, documents, memories,
//!   chunks (embeddings included, no re-embedding needed), and facts. A v2 file
//!   has no journal, so one is SYNTHESIZED from the rows it does carry, by the
//!   same generator the v9→v10 migration uses — a store restored without a
//!   journal cannot be rebuilt or audited.
//! - **v1** (legacy): memories-only exports. Each memory goes in through
//!   `insert_memory`, which journals its own `memory.written`, so a legacy
//!   restore lands with a journal too. Content is re-chunked through the
//!   same policy `remember` uses, so a legacy restore cannot smuggle in the
//!   degenerate fragments the floor exists to stop. Chunks land WITHOUT
//!   embeddings by design; run `engram reindex --all` afterwards to restore
//!   dense search.
//!
//! Both formats are accepted raw or wrapped in the CLI JSON envelope
//! (`{"status":"success","data":{...}}`), so `engram export --json >
//! backup.json` re-imports without unwrapping by hand.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_core::types::Memory;
use engram_ingest::memory::chunk_memory;
use engram_storage::{
    ChunkExportRow, DocumentExportRow, FactExportRow, KbExportRow, MemoryExportRow,
};
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

pub fn run(ctx: &AppContext, path: PathBuf) -> Result<(), CliError> {
    if !path.exists() {
        return Err(CliError::BadInput(format!(
            "file not found: {}",
            path.display()
        )));
    }
    let bytes = std::fs::read(&path)?;
    let payload: serde_json::Value = serde_json::from_slice(&bytes)?;

    // Unwrap the CLI envelope if present.
    let body = if payload.get("data").is_some() && payload.get("status").is_some() {
        payload.get("data").cloned().unwrap_or(payload.clone())
    } else {
        payload.clone()
    };

    let version = body.get("version").and_then(|v| v.as_i64()).unwrap_or(1);

    let (data, meta) = match version {
        3 => import_v3(ctx, &body)?,
        2 => import_v2(ctx, &body)?,
        // An un-versioned legacy file is still accepted.
        _ => import_v1(ctx, &body)?,
    };
    print_success(ctx.format, data, meta, |data| println!("{}", data));
    Ok(())
}

/// The v3 path: hand the whole document to storage and report what it did.
///
/// Deliberately NOT a hand-rolled pipeline. Shape validation, the refusal of a
/// file written by a newer schema, event-id preservation and the replay order
/// all live in `import_v3`, so a CLI import and a daemon import accept and
/// reject exactly the same files. A local re-validation here would be a second
/// opinion, and two opinions about what a valid backup is means one of them is
/// wrong in the field.
fn import_v3(
    ctx: &AppContext,
    body: &serde_json::Value,
) -> Result<(serde_json::Value, Metadata), CliError> {
    let doc: engram_storage::ExportV3 = serde_json::from_value(body.clone())
        .map_err(|e| CliError::BadInput(format!("malformed v3 export file: {e}")))?;
    let report = ctx.store.import_v3(&doc)?;

    let mut meta = Metadata::default();
    meta.add("format_version", 3);
    meta.add("schema_version", doc.schema_version);
    meta.add("replayed_events", report.events);
    meta.add("imported_memories", report.memories);
    meta.add("imported_chunks", report.chunks);
    meta.add("safety_closes_refused", report.close_authority_rejected);
    Ok((
        json!({
            "format_version": 3,
            "replayed": {
                "events": report.events,
                "events_skipped_duplicate": report.events_skipped_duplicate,
                "conflicts_skipped_orphan": report.conflicts_skipped_orphan,
            },
            // [G8] The closes this store declined to honour, on rows and
            // replayed events alike. Counting them and saying nothing was the
            // standing complaint: an import that quietly does less than the
            // file asked leaves the caller believing a prescription is retired
            // when the store still holds it open.
            "safety_closes_refused": {
                "count": report.close_authority_rejected,
                "meaning": "facts on takes_medication / allergic_to / diagnosed_with imported OPEN; \
                            a safety interval never arrives closed from outside a store",
            },
            // Same key, same meaning as v1/v2: rows this call actually
            // inserted. The journal is authoritative for everything it
            // carries; these sections carry what it does not (embeddings,
            // documents, KB registrations).
            "imported": {
                "kbs": report.kbs,
                "documents": report.documents,
                "memories": report.memories,
                "chunks": report.chunks,
                "facts": report.facts,
            },
            "skipped_existing": {
                "kbs": doc.kbs.len().saturating_sub(report.kbs),
                "documents": doc.documents.len().saturating_sub(report.documents),
                "memories": doc.memories.len().saturating_sub(report.memories),
                "chunks": doc.chunks.len().saturating_sub(report.chunks),
                "facts": doc.facts.len().saturating_sub(report.facts),
                "events": report.events_skipped_duplicate,
            },
        }),
        meta,
    ))
}

fn import_v2(
    ctx: &AppContext,
    body: &serde_json::Value,
) -> Result<(serde_json::Value, Metadata), CliError> {
    let kbs: Vec<KbExportRow> = section(body, "kbs")?;
    let documents: Vec<DocumentExportRow> = section(body, "documents")?;
    let memories: Vec<MemoryExportRow> = section(body, "memories")?;
    let chunks: Vec<ChunkExportRow> = section(body, "chunks")?;
    let facts: Vec<FactExportRow> = section(body, "facts")?;

    // Timestamps are load-bearing: `created_at` seeds fact chronology, and a
    // fabricated date decides recency supersession. A record whose date the
    // store cannot read is rejected up front — every legitimate export writer
    // emits RFC3339, so an unparseable one means the file was edited.
    for row in &memories {
        if chrono::DateTime::parse_from_rfc3339(&row.created_at).is_err() {
            return Err(CliError::BadInput(format!(
                "memory {} has an unreadable created_at {:?}; fix the export file — engram \
                 will not invent a date for it",
                row.id, row.created_at
            )));
        }
    }
    // Fact dates are just as load-bearing: created_at seeds recency
    // supersession and valid_from is the event time. Reject unreadable ones
    // up front — the row parser's epoch fallback keeps an already-stored
    // corrupt date harmless, but an import must not add more.
    for row in &facts {
        if chrono::DateTime::parse_from_rfc3339(&row.created_at).is_err() {
            return Err(CliError::BadInput(format!(
                "fact {} has an unreadable created_at {:?}; fix the export file",
                row.id, row.created_at
            )));
        }
        if let Some(v) = row.valid_from.as_deref() {
            if chrono::DateTime::parse_from_rfc3339(v).is_err() {
                return Err(CliError::BadInput(format!(
                    "fact {} has an unreadable valid_from {v:?}; fix the export file",
                    row.id
                )));
            }
        }
    }

    // Rows in dependency order (kb → document → memory → chunk → fact) AND the
    // synthesized journal, in ONE storage call and therefore one transaction.
    // Genesis has to be generated from the same rows in the same transaction —
    // a separate pass could disagree with what actually landed, and the store
    // would then rebuild into something the file never described. Existing rows
    // are kept (INSERT OR IGNORE), so import is idempotent and safe to re-run.
    let counts = ctx.store.import_v2_with_genesis(
        &kbs,
        &documents,
        &memories,
        &chunks,
        &facts,
        IMPORT_ACTOR,
    )?;

    let mut meta = Metadata::default();
    meta.add("format_version", 2);
    meta.add("imported_memories", counts.memories);
    meta.add("imported_chunks", counts.chunks);
    meta.add("genesis_events", counts.genesis_synthesized);
    meta.add("safety_closes_refused", counts.close_authority_rejected);
    Ok((
        json!({
            "format_version": 2,
            "imported": {
                "kbs": counts.kbs,
                "documents": counts.documents,
                "memories": counts.memories,
                "chunks": counts.chunks,
                "facts": counts.facts,
            },
            // [G8] Same rule, same number, on the older format.
            "safety_closes_refused": {
                "count": counts.close_authority_rejected,
                "meaning": "facts on takes_medication / allergic_to / diagnosed_with imported OPEN; \
                            a safety interval never arrives closed from outside a store",
            },
            // A v2 file carries no history; this is the history synthesized for
            // it, so the restored store is rebuildable rather than frozen.
            "genesis_events": counts.genesis_synthesized,
            "skipped_existing": {
                "kbs": kbs.len() - counts.kbs,
                "documents": documents.len() - counts.documents,
                "memories": memories.len() - counts.memories,
                "chunks": chunks.len() - counts.chunks,
                "facts": facts.len() - counts.facts,
            },
        }),
        meta,
    ))
}

/// Who the synthesized genesis events are attributed to. Not `cli:<diary>`: a
/// v2 restore is not a diary writing memories, and an audit reading the journal
/// must be able to tell a synthesized past from a lived one.
const IMPORT_ACTOR: &str = "import:v2";

fn section<T: serde::de::DeserializeOwned>(
    body: &serde_json::Value,
    key: &str,
) -> Result<Vec<T>, CliError> {
    match body.get(key) {
        None => Ok(Vec::new()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            CliError::BadInput(format!("malformed `{key}` section in export file: {e}"))
        }),
    }
}

fn import_v1(
    ctx: &AppContext,
    body: &serde_json::Value,
) -> Result<(serde_json::Value, Metadata), CliError> {
    let memories_json = body
        .get("memories")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            CliError::BadInput("expected { memories: [...] } at top level or under .data".into())
        })?;

    let mut imported = 0u32;
    let mut chunks_created = 0u32;
    let mut skipped_empty = 0u32;
    // Resolved ONCE, outside the transaction. `resolve_policy` reads three
    // settings, and each one re-reads the config file; doing that per memory
    // would put file IO inside a write transaction holding SQLite's writer
    // lock for the whole import.
    let policy = crate::commands::chunks::resolve_policy();
    ctx.store.with_tx(|store| {
        for m_json in memories_json {
            let Ok(memory) = serde_json::from_value::<Memory>(m_json.clone()) else {
                continue;
            };
            // A whitespace-only record would import a memory row that chunks
            // to nothing — committed but invisible to every recall path, the
            // same black hole the atomic `remember` fix and the `edit` empty
            // guard close. A legacy backup cannot be a third door to it.
            if memory.content.trim().is_empty() {
                skipped_empty += 1;
                continue;
            }
            // Storage will reject dupes via UNIQUE.
            if store.insert_memory(&memory).is_ok() {
                imported += 1;
                // Same chunker as `remember` and `edit`. Restoring a legacy
                // backup must not be a back door that re-fills the index with
                // bare date stamps and speaker labels.
                for chunk in chunk_memory(&memory.content, policy) {
                    store.insert_chunk(
                        Uuid::new_v4(),
                        memory.id,
                        &chunk.text,
                        chunk.position,
                        chunk.section.as_deref(),
                    )?;
                    chunks_created += 1;
                }
            }
        }
        Ok(())
    })?;
    let mut meta = Metadata::default();
    meta.add("format_version", 1);
    meta.add("imported", imported);
    meta.add("chunks_created", chunks_created);
    meta.add("skipped_empty", skipped_empty);
    Ok((
        json!({
            "format_version": 1,
            "imported": imported,
            "chunks_created": chunks_created,
            "skipped_empty": skipped_empty,
            "next_step": "legacy v1 export has no embeddings; run `engram reindex --all` to restore dense search",
        }),
        meta,
    ))
}
