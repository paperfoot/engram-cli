//! `engram import <file>` — restore a store from an export JSON file.
//!
//! Understands two formats:
//! - **v2** (current): full-fidelity restore of KBs, documents, memories,
//!   chunks (embeddings included, no re-embedding needed), and facts.
//! - **v1** (legacy): memories-only exports. Content is re-chunked through the
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
        2 => import_v2(ctx, &body)?,
        _ => import_v1(ctx, &body)?,
    };
    print_success(ctx.format, data, meta, |data| println!("{}", data));
    Ok(())
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

    // Insert in dependency order (kb → document → memory → chunk → fact)
    // inside one transaction so a malformed file cannot leave a half-restored
    // store. Existing rows are kept (INSERT OR IGNORE), so import is
    // idempotent and safe to re-run.
    let counts = ctx.store.with_tx(|store| {
        let mut c = ImportCounts::default();
        for row in &kbs {
            c.kbs += store.import_kb_row(row)? as usize;
        }
        for row in &documents {
            c.documents += store.import_document_row(row)? as usize;
        }
        for row in &memories {
            c.memories += store.import_memory_row(row)? as usize;
        }
        for row in &chunks {
            c.chunks += store.import_chunk_row(row)? as usize;
        }
        for row in &facts {
            c.facts += store.import_fact_row(row)? as usize;
        }
        Ok(c)
    })?;

    let mut meta = Metadata::default();
    meta.add("format_version", 2);
    meta.add("imported_memories", counts.memories);
    meta.add("imported_chunks", counts.chunks);
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
            "skipped_existing": {
                "kbs": kbs.len() - counts.kbs,
                "documents": documents.len() - counts.documents,
                "memories": memories.len() - counts.memories,
                "chunks": chunks.len() - counts.chunks,
                "facts": facts.len() - counts.facts,
            },
            "next_step": "run `engram compile --kb <name> --all` to regenerate derived claims/entities/wiki",
        }),
        meta,
    ))
}

#[derive(Default)]
struct ImportCounts {
    kbs: usize,
    documents: usize,
    memories: usize,
    chunks: usize,
    facts: usize,
}

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
