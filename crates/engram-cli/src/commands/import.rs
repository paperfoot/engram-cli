//! `engram import <file>` — restore memories from an export JSON file.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_core::types::Memory;
use engram_ingest::chunker::naive_split;
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

    // Accept BOTH shapes:
    //   - raw export body: { "memories": [...], "count": N, "version": 1 }
    //   - full CLI envelope as dumped by `engram export --json > file.json`:
    //       { "version": "1", "status": "success", "data": {"memories":[...]}, ... }
    // so users can pipe `engram export --json > file.json` and re-import
    // without unwrapping by hand.
    let memories_json = payload
        .get("memories")
        .and_then(|v| v.as_array())
        .or_else(|| {
            payload
                .get("data")
                .and_then(|d| d.get("memories"))
                .and_then(|v| v.as_array())
        })
        .ok_or_else(|| {
            CliError::BadInput("expected { memories: [...] } at top level or under .data".into())
        })?;

    let mut imported = 0u32;
    let mut chunks_created = 0u32;
    for m_json in memories_json {
        let memory: Memory = serde_json::from_value(m_json.clone())?;
        // Storage will reject dupes via UNIQUE.
        if ctx.store.insert_memory(&memory).is_ok() {
            imported += 1;
            for chunk in naive_split(&memory.content) {
                ctx.store.insert_chunk(
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
    let mut meta = Metadata::default();
    meta.add("imported", imported);
    meta.add("chunks_created", chunks_created);
    print_success(
        ctx.format,
        json!({ "imported": imported, "chunks_created": chunks_created }),
        meta,
        |data| println!("{}", data),
    );
    Ok(())
}
