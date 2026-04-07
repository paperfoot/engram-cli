//! `engram recall <query>` — v0 baseline using FTS5 only.
//! Phase 2 wires in dense + fusion + rerank.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use std::time::Instant;

pub async fn run(
    ctx: &AppContext,
    query: String,
    top_k: usize,
    _layer: String,
    _diary: String,
) -> Result<(), CliError> {
    if query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }
    let start = Instant::now();
    let hits = ctx.store.fts_search(&query, top_k)?;

    let results: Vec<_> = hits
        .iter()
        .map(|(id, score)| json!({ "chunk_id": id.to_string(), "score": score }))
        .collect();

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("retriever", "fts5_baseline");
    meta.add("results_returned", results.len());

    print_success(
        ctx.format,
        json!({ "results": results }),
        meta,
        |data| {
            if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
                for r in arr {
                    println!("{}", r);
                }
            }
        },
    );
    Ok(())
}
