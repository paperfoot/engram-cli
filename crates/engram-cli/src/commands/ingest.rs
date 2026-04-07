//! `engram ingest <path>` — v0 baseline reads text files only.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::Utc;
use engram_core::types::{Memory, MemorySource};
use engram_ingest::chunker::naive_split;
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;
use uuid::Uuid;

pub async fn run(
    ctx: &AppContext,
    path: PathBuf,
    _mode: String,
    diary: String,
) -> Result<(), CliError> {
    if !path.exists() {
        return Err(CliError::BadInput(format!("path does not exist: {}", path.display())));
    }
    let start = Instant::now();

    let mut memories_created = 0u32;
    let mut chunks_created = 0u32;

    let files: Vec<PathBuf> = if path.is_file() {
        vec![path]
    } else {
        walk_text_files(&path)?
    };

    for file in files {
        let text = std::fs::read_to_string(&file).map_err(CliError::Io)?;
        let memory = Memory {
            id: Uuid::new_v4(),
            content: text.clone(),
            created_at: Utc::now(),
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::General,
            diary: diary.clone(),
            valid_from: None,
            valid_until: None,
            tags: vec![file.display().to_string()],
        };
        ctx.store.insert_memory(&memory)?;
        memories_created += 1;
        for chunk in naive_split(&text) {
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

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memories_created", memories_created);
    meta.add("chunks_created", chunks_created);

    print_success(
        ctx.format,
        json!({
            "memories_created": memories_created,
            "chunks_created": chunks_created
        }),
        meta,
        |data| println!("Ingested: {}", data),
    );
    Ok(())
}

fn walk_text_files(root: &std::path::Path) -> Result<Vec<PathBuf>, CliError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if matches!(ext.to_ascii_lowercase().as_str(), "txt" | "md" | "rst" | "json") {
                    out.push(p);
                }
            }
        }
    }
    Ok(out)
}
