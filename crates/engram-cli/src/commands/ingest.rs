//! `engram ingest <path>` — mine files or directories into memories.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::Utc;
use engram_core::types::{Memory, MemorySource};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_ingest::{
    chunker::PendingChunk, conversations, general as general_mode, papers, repos, Mode,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

pub async fn run(
    ctx: &AppContext,
    path: PathBuf,
    mode: String,
    diary: String,
) -> Result<(), CliError> {
    if !path.exists() {
        return Err(CliError::BadInput(format!(
            "path does not exist: {}",
            path.display()
        )));
    }
    let start = Instant::now();
    let mode_enum = parse_mode(&mode)?;

    let files: Vec<PathBuf> = if path.is_file() {
        vec![path]
    } else {
        walk_ingestible_files(&path)?
    };

    let mut memories_created = 0u32;
    let mut chunks_created = 0u32;

    // Embedder: env var → config file → stub fallback
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let have_gemini = gemini_key.is_some() && !force_stub;
    let model_name = if have_gemini { "gemini-embedding-001" } else { "stub" };

    for file in files {
        let text = if engram_ingest::pdf::is_pdf(&file) {
            engram_ingest::pdf::extract_text(&file)?
        } else {
            std::fs::read_to_string(&file).map_err(CliError::Io)?
        };
        if text.trim().is_empty() {
            continue;
        }

        let resolved_mode = if mode_enum == Mode::Auto {
            auto_classify(&file, &text)
        } else {
            mode_enum
        };

        let source = match resolved_mode {
            Mode::Papers => MemorySource::Paper {
                doi: None,
                title: file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string(),
                section: None,
            },
            Mode::Conversations => MemorySource::Conversation {
                thread: file.display().to_string(),
                turn: 0,
            },
            Mode::Repos => MemorySource::Repo {
                repo: file
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                path: file.display().to_string(),
                line_start: None,
            },
            _ => MemorySource::General,
        };

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
            source,
            diary: diary.clone(),
            valid_from: None,
            valid_until: None,
            tags: vec![file.display().to_string()],
        };
        ctx.store.insert_memory(&memory)?;

        let chunks: Vec<PendingChunk> = match resolved_mode {
            Mode::Papers => papers::chunk_paper(&text),
            Mode::Conversations => conversations::chunk_conversation(&text),
            Mode::Repos => repos::chunk_repo_text(&text),
            _ => general_mode::chunk_general(&text),
        };

        // Batch-embed all chunks. If embedding fails (rate limit, network,
        // etc.) roll back the memory row so we don't leave an orphan with
        // no chunks — recall would find nothing anyway and the count would
        // be misleading.
        let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let embed_result: Result<Vec<Vec<f32>>, CliError> = if chunk_texts.is_empty() {
            Ok(Vec::new())
        } else if have_gemini {
            let e = GeminiEmbedder::new(gemini_key.clone().unwrap());
            e.embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await
                .map_err(CliError::from)
        } else {
            let e = StubEmbedder::default();
            e.embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await
                .map_err(CliError::from)
        };

        let embeddings = match embed_result {
            Ok(v) => v,
            Err(e) => {
                // Roll back the orphan memory row and bubble the error up.
                let _ = ctx.store.hard_delete_memory(memory.id);
                return Err(e);
            }
        };

        for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
            ctx.store.insert_chunk_with_embedding(
                Uuid::new_v4(),
                memory.id,
                &chunk.text,
                chunk.position,
                chunk.section.as_deref(),
                emb,
                model_name,
            )?;
            chunks_created += 1;
        }
        memories_created += 1;
    }

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memories_created", memories_created);
    meta.add("chunks_created", chunks_created);
    meta.add("embedder", model_name);

    print_success(
        ctx.format,
        json!({
            "memories_created": memories_created,
            "chunks_created": chunks_created,
            "mode": mode,
        }),
        meta,
        |data| println!("Ingested: {}", data),
    );
    Ok(())
}

fn parse_mode(s: &str) -> Result<Mode, CliError> {
    match s.to_ascii_lowercase().as_str() {
        "papers" => Ok(Mode::Papers),
        "conversations" | "convos" | "chats" => Ok(Mode::Conversations),
        "repos" | "code" => Ok(Mode::Repos),
        "general" => Ok(Mode::General),
        "auto" => Ok(Mode::Auto),
        other => Err(CliError::BadInput(format!(
            "unknown mode: {other} (expected papers|conversations|repos|general|auto)"
        ))),
    }
}

fn auto_classify(path: &Path, text: &str) -> Mode {
    let name_lower = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // PDFs and arxiv-style filenames are papers.
    if ext == "pdf"
        || name_lower.contains("paper")
        || name_lower.contains("arxiv")
        // arXiv filename pattern: four digits, dot, four to five digits
        || name_lower
            .split('.')
            .next()
            .map(|s| s.len() >= 4 && s.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
    {
        return Mode::Papers;
    }
    if ext == "rs" || ext == "py" || ext == "ts" || ext == "js" || ext == "go" || ext == "java" {
        return Mode::Repos;
    }
    let turn_markers = text.matches("user:").count() + text.matches("assistant:").count();
    if turn_markers >= 5 {
        return Mode::Conversations;
    }
    Mode::General
}

fn walk_ingestible_files(root: &Path) -> Result<Vec<PathBuf>, CliError> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), CliError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            // Skip hidden dirs and target/node_modules.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || matches!(name, "target" | "node_modules" | ".git") {
                continue;
            }
            walk(&p, out)?;
        } else if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "txt" | "md" | "rst" | "json" | "rs" | "py" | "ts" | "js" | "go" | "java" | "pdf"
                ) {
                    out.push(p);
                }
            }
        }
    }
    Ok(())
}
