//! `engram ingest <path>` — mine files or directories into memories.

use crate::commands::usage::gemini_embed_cost_usd;
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
    kb: String,
    compile: String,
    dry_run: bool,
    include: Vec<String>,
    exclude: Vec<String>,
    max_files: Option<usize>,
    all: bool,
) -> Result<(), CliError> {
    if !path.exists() {
        return Err(CliError::BadInput(format!(
            "path does not exist: {}",
            path.display()
        )));
    }
    let start = Instant::now();
    let input_path = path.display().to_string();
    let mode_enum = parse_mode(&mode)?;
    let include = if include.is_empty() {
        configured_patterns("ENGRAM_INGEST_INCLUDE", "ingest.include")
    } else {
        include
    };
    let exclude = if exclude.is_empty() {
        configured_patterns("ENGRAM_INGEST_EXCLUDE", "ingest.exclude")
    } else {
        exclude
    };
    let max_files =
        max_files.or_else(|| configured_usize("ENGRAM_INGEST_MAX_FILES", "ingest.max_files"));
    let require_scope =
        configured_bool("ENGRAM_INGEST_REQUIRE_SCOPE", "ingest.require_scope", true);

    let input_is_dir = path.is_dir();
    let files: Vec<PathBuf> = if path.is_file() {
        vec![path]
    } else {
        if require_scope
            && !all
            && include.is_empty()
            && exclude.is_empty()
            && max_files.is_none()
            && !dry_run
        {
            return Err(CliError::BadInput(
                "directory ingest requires --all, --include/--exclude, --max-files, or --dry-run"
                    .into(),
            ));
        }
        walk_ingestible_files(&path, &include, &exclude)?
    };
    if let Some(max) = max_files {
        if files.len() > max {
            return Err(CliError::BadInput(format!(
                "matched {} files, over --max-files {max}; narrow with --include/--exclude or raise the cap",
                files.len()
            )));
        }
    }
    let matched_files = files
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();

    if dry_run {
        let mut meta = Metadata::default();
        meta.elapsed_ms = start.elapsed().as_millis() as u64;
        meta.add("dry_run", true);
        meta.add("matched_files", matched_files.len());
        meta.add("kb", kb.clone());
        meta.add("require_scope", require_scope);
        print_success(
            ctx.format,
            json!({
                "dry_run": true,
                "input_path": input_path,
                "input_kind": if input_is_dir { "directory" } else { "file" },
                "matched_files": matched_files,
                "matched_count": matched_files.len(),
                "include": include,
                "exclude": exclude,
                "max_files": max_files,
                "require_scope": require_scope,
                "kb": kb,
                "mode": mode,
                "compile": compile,
            }),
            meta,
            |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
        );
        return Ok(());
    }

    let mut memories_created = 0u32;
    let mut chunks_created = 0u32;
    let mut skipped_existing = 0u32;
    // Cost accounting for Gemini embeddings: ~4 chars per token heuristic,
    // $0.15 per 1M input tokens for gemini-embedding-2 (priced 2026-04).
    let mut total_embedded_chars: usize = 0;

    // Embedder: env var → config file → stub fallback
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let have_gemini = gemini_key.is_some() && !force_stub;
    let (model_name, dims, prompt_format) = if have_gemini {
        let e = GeminiEmbedder::new(gemini_key.clone().unwrap());
        (e.model(), e.dimensions(), e.prompt_format().to_string())
    } else {
        let e = StubEmbedder::default();
        (e.model(), e.dimensions(), e.prompt_format().to_string())
    };

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

        let source_path = file.display().to_string();
        if let Some(existing_id) = ctx.store.document_id_for_source_path(&kb, &source_path)? {
            let (active_memories, active_chunks) = ctx.store.active_document_counts(existing_id)?;
            if active_memories > 0 || active_chunks > 0 {
                skipped_existing += 1;
                continue;
            }
        }
        let doc_title = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();
        let document_id = ctx.store.insert_document(
            &kb,
            &doc_title,
            Some(&source_path),
            &format!("{:?}", resolved_mode).to_ascii_lowercase(),
            json!({ "ingested_from": source_path }),
        )?;

        let source = match resolved_mode {
            Mode::Papers => MemorySource::Paper {
                doi: None,
                title: doc_title,
                section: None,
            },
            Mode::Takeaways => MemorySource::General,
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
        ctx.store.insert_memory_with_kb(&memory, &kb)?;

        let chunks: Vec<PendingChunk> = match resolved_mode {
            Mode::Papers => papers::chunk_paper(&text),
            Mode::Takeaways => general_mode::chunk_general(&text),
            Mode::Conversations => conversations::chunk_conversation(&text),
            Mode::Repos => repos::chunk_repo_text(&text),
            _ => general_mode::chunk_general(&text),
        };

        // Batch-embed all chunks. If embedding fails (rate limit, network,
        // etc.) roll back the memory row so we don't leave an orphan with
        // no chunks — recall would find nothing anyway and the count would
        // be misleading.
        let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let embed_chars: usize = chunk_texts.iter().map(|t| t.len()).sum();
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
            ctx.store.insert_chunk_with_embedding_meta(
                Uuid::new_v4(),
                memory.id,
                &chunk.text,
                chunk.position,
                chunk.section.as_deref(),
                emb,
                &model_name,
                dims,
                &prompt_format,
                Some(document_id),
            )?;
            chunks_created += 1;
        }
        total_embedded_chars += embed_chars;
        memories_created += 1;
    }

    // Cost summary.
    let tokens_estimated = (total_embedded_chars + 3) / 4;
    let gemini_cost_usd = if have_gemini {
        gemini_embed_cost_usd(tokens_estimated as i64)
    } else {
        0.0
    };
    if have_gemini && chunks_created > 0 {
        ctx.store.record_usage_event(
            "gemini",
            "ingest_embed",
            Some(&model_name),
            Some(&kb),
            Some(&diary),
            1,
            chunks_created as i64,
            tokens_estimated as i64,
            0,
            0.0,
            gemini_cost_usd,
            json!({
                "dimensions": dims,
                "prompt_format": prompt_format.clone(),
                "files": memories_created,
                "estimated": true,
            }),
        )?;
    }

    let compile_stats = match compile.as_str() {
        "none" => None,
        "evidence" => Some(crate::commands::compile::compile_kb(ctx, &kb).await?),
        other => {
            return Err(CliError::BadInput(format!(
                "unknown compile mode: {other} (expected none|evidence)"
            )))
        }
    };

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("memories_created", memories_created);
    meta.add("chunks_created", chunks_created);
    meta.add("skipped_existing", skipped_existing);
    meta.add("matched_files", matched_files.len());
    meta.add("require_scope", require_scope);
    meta.add("embedder", model_name.clone());
    meta.add("embed_dimensions", dims);
    meta.add("embed_prompt_format", prompt_format.clone());
    meta.add("kb", kb.clone());
    if let Some(stats) = &compile_stats {
        meta.add("compiled_claims", stats.claims);
        meta.add("compiled_entities", stats.entities);
        meta.add("compiled_relations", stats.relations);
        meta.add("compiled_wiki_pages", stats.wiki_pages);
    }
    meta.add("embed_chars", total_embedded_chars);
    meta.add("embed_tokens_estimated", tokens_estimated);
    meta.add(
        "embed_cost_usd_estimated",
        format!("{:.6}", gemini_cost_usd),
    );

    print_success(
        ctx.format,
        json!({
            "memories_created": memories_created,
            "chunks_created": chunks_created,
            "skipped_existing": skipped_existing,
            "matched_count": matched_files.len(),
            "matched_files": matched_files,
            "include": include,
            "exclude": exclude,
            "max_files": max_files,
            "require_scope": require_scope,
            "mode": mode,
            "kb": kb,
            "compile": compile,
            "compile_stats": compile_stats,
            "embed_chars": total_embedded_chars,
            "embed_tokens_estimated": tokens_estimated,
            "embed_cost_usd_estimated": gemini_cost_usd,
        }),
        meta,
        |data| println!("Ingested: {}", data),
    );
    Ok(())
}

fn parse_mode(s: &str) -> Result<Mode, CliError> {
    match s.to_ascii_lowercase().as_str() {
        "papers" => Ok(Mode::Papers),
        "takeaways" | "notes" | "curated" => Ok(Mode::Takeaways),
        "conversations" | "convos" | "chats" => Ok(Mode::Conversations),
        "repos" | "code" => Ok(Mode::Repos),
        "general" => Ok(Mode::General),
        "auto" => Ok(Mode::Auto),
        other => Err(CliError::BadInput(format!(
            "unknown mode: {other} (expected papers|takeaways|conversations|repos|general|auto)"
        ))),
    }
}

fn configured_patterns(env: &str, toml_path: &str) -> Vec<String> {
    std::env::var(env)
        .ok()
        .or_else(|| crate::commands::config::resolve_config_string(toml_path))
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn configured_usize(env: &str, toml_path: &str) -> Option<usize> {
    std::env::var(env)
        .ok()
        .or_else(|| crate::commands::config::resolve_config_string(toml_path))
        .and_then(|v| v.parse().ok())
}

fn configured_bool(env: &str, toml_path: &str, default: bool) -> bool {
    std::env::var(env)
        .ok()
        .or_else(|| crate::commands::config::resolve_config_string(toml_path))
        .and_then(|v| match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
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

fn walk_ingestible_files(
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Vec<PathBuf>, CliError> {
    let mut out = Vec::new();
    walk(root, root, include, exclude, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(
    root: &Path,
    dir: &Path,
    include: &[String],
    exclude: &[String],
    out: &mut Vec<PathBuf>,
) -> Result<(), CliError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            // Skip hidden dirs and target/node_modules.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || matches!(name, "target" | "node_modules" | ".git") {
                continue;
            }
            walk(root, &p, include, exclude, out)?;
        } else if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "txt"
                        | "md"
                        | "rst"
                        | "json"
                        | "rs"
                        | "py"
                        | "ts"
                        | "js"
                        | "go"
                        | "java"
                        | "pdf"
                ) {
                    if file_matches(root, &p, include, exclude) {
                        out.push(p);
                    }
                }
            }
        }
    }
    Ok(())
}

fn file_matches(root: &Path, path: &Path, include: &[String], exclude: &[String]) -> bool {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let included = include.is_empty()
        || include
            .iter()
            .any(|pattern| wildcard_match(pattern, &rel) || wildcard_match(pattern, &name));
    let excluded = exclude
        .iter()
        .any(|pattern| wildcard_match(pattern, &rel) || wildcard_match(pattern, &name));
    included && !excluded
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_ti = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(si) = star {
            pi = si + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}
