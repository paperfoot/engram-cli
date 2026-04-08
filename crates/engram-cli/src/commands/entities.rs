//! `engram entities list|show` — browse the entity graph.
//!
//! v1 scans chunks via the extractor to populate entities on demand.
//! Cheap to compute — the bottleneck is the scan, not the extraction itself.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_graph::extract_entities;
use serde_json::json;
use std::collections::HashMap;

pub fn list(ctx: &AppContext, limit: usize, min_mentions: u32) -> Result<(), CliError> {
    // Walk every chunk, extract entities, count mentions.
    let chunks = ctx.store.iter_chunks_with_embeddings(None)?;
    let mut counts: HashMap<String, u32> = HashMap::new();
    for (_, content, _, _) in &chunks {
        for ent in extract_entities(content) {
            *counts.entry(ent).or_insert(0) += 1;
        }
    }
    let mut entries: Vec<(String, u32)> = counts
        .into_iter()
        .filter(|(_, n)| *n >= min_mentions)
        .collect();
    // Sort by mention count DESC, then alpha ASC for stable output.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries.truncate(limit);

    let mut meta = Metadata::default();
    meta.add("total_chunks_scanned", chunks.len());
    meta.add("entities_returned", entries.len());
    print_success(
        ctx.format,
        json!({
            "entities": entries
                .iter()
                .map(|(name, n)| json!({ "name": name, "mention_count": n }))
                .collect::<Vec<_>>()
        }),
        meta,
        |data| {
            if let Some(arr) = data.get("entities").and_then(|v| v.as_array()) {
                for e in arr {
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let n = e.get("mention_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!("{n:6}  {name}");
                }
            }
        },
    );
    Ok(())
}

pub fn show(ctx: &AppContext, name: String) -> Result<(), CliError> {
    let chunks = ctx.store.iter_chunks_with_embeddings(None)?;
    let mut mentions: Vec<String> = Vec::new();
    let mut total_mentions = 0u32;
    for (_, content, _, _) in &chunks {
        if extract_entities(content)
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&name))
        {
            total_mentions += 1;
            if mentions.len() < 5 {
                let snippet: String = content.chars().take(200).collect();
                mentions.push(snippet);
            }
        }
    }

    let mut meta = Metadata::default();
    meta.add("total_mentions", total_mentions);
    meta.add("total_chunks_scanned", chunks.len());
    print_success(
        ctx.format,
        json!({
            "name": name,
            "total_mentions": total_mentions,
            "sample_mentions": mentions,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
