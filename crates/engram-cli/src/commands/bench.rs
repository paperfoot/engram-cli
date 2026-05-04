//! `engram bench [suite]` — autoresearch evaluation entry point.
//!
//! `mini` runs a baked-in 5-question synthetic test (fast, deterministic, used
//! by the autoresearch loop). `longmemeval` runs the full published benchmark
//! once the dataset has been downloaded.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_llm::openrouter::OpenRouterClient;
use serde_json::json;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    ctx: &AppContext,
    suite: String,
    mab_split: String,
    download: bool,
    limit: Option<usize>,
    answerer: String,
    judge: String,
    ragas: bool,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    match suite.as_str() {
        "mini" => run_mini(ctx).await,
        "mini-fts" => run_mini_fts(ctx),
        "scientific-mini" | "science-mini" => run_scientific_mini(ctx).await,
        "longmemeval" => run_longmemeval(ctx, download, limit).await,
        "longmemeval-qa" | "lme-qa" => {
            run_longmemeval_qa(ctx, limit, answerer, judge, ragas, top_k, save).await
        }
        "locomo-qa" => run_locomo_qa(ctx, limit, answerer, judge, ragas, top_k, save).await,
        "locomo-plus" | "locomo-plus-qa" => {
            run_locomo_plus_qa(ctx, limit, answerer, judge, ragas, top_k, save).await
        }
        "memoryagentbench" | "mab" => {
            run_memoryagentbench_qa(ctx, mab_split, limit, answerer, judge, top_k, save).await
        }
        other => Err(CliError::BadInput(format!("unknown suite: {other}"))),
    }
}

async fn run_scientific_mini(ctx: &AppContext) -> Result<(), CliError> {
    use crate::retrieval::{hybrid_recall, Filters, HybridParams, RecallMode};
    use chrono::Utc;
    use engram_core::types::{Memory, MemorySource};
    use engram_embed::{Embedder, TaskMode};
    use engram_ingest::chunker::naive_split;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_storage::SqliteStore;
    use uuid::Uuid;

    let store = SqliteStore::open_in_memory()?;
    let fixture_ctx = AppContext {
        format: ctx.format,
        quiet: true,
        store,
    };
    let kb = "scientific-mini";
    fixture_ctx
        .store
        .ensure_kb(kb, Some("Tiny scientific evidence benchmark"))?;
    let embedder = StubEmbedder::default();

    let docs = [
        (
            "rapamycin-human",
            "Rapamycin inhibits mTORC1 signaling. In a human randomized trial, low-dose everolimus improved influenza vaccine response in older adults, but lifespan was not measured.",
        ),
        (
            "metformin-observational",
            "Metformin activates AMPK in liver and muscle. Observational studies in patients with type 2 diabetes report lower mortality, but confounding remains a major limitation.",
        ),
        (
            "hipporag",
            "HippoRAG uses graph-structured retrieval to improve multi-hop question answering by linking entities across passages before final context assembly.",
        ),
    ];

    for (title, text) in docs {
        let document_id = fixture_ctx.store.insert_document(
            kb,
            title,
            Some(title),
            "papers",
            serde_json::json!({}),
        )?;
        let memory = Memory {
            id: Uuid::new_v4(),
            content: text.to_string(),
            created_at: Utc::now(),
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Paper {
                doi: None,
                title: title.to_string(),
                section: None,
            },
            diary: "default".to_string(),
            valid_from: None,
            valid_until: None,
            tags: vec![title.to_string()],
        };
        fixture_ctx.store.insert_memory_with_kb(&memory, kb)?;
        let chunks = naive_split(text);
        let chunk_texts = chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>();
        let embeddings = embedder
            .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
            .await?;
        for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
            fixture_ctx.store.insert_chunk_with_embedding_meta(
                Uuid::new_v4(),
                memory.id,
                &chunk.text,
                chunk.position,
                chunk.section.as_deref(),
                embedding,
                &embedder.model(),
                embedder.dimensions(),
                &embedder.prompt_format(),
                Some(document_id),
            )?;
        }
    }
    let compile = crate::commands::compile::compile_kb(&fixture_ctx, kb).await?;
    let cases = [
        ("human rapamycin vaccine response", "everolimus"),
        ("metformin mortality confounding", "confounding"),
        ("graph retrieval multi-hop entities", "hipporag"),
    ];
    let mut per_question = Vec::new();
    let mut correct = 0usize;
    for (query, expected) in cases {
        let reranker: Option<&PassthroughReranker> = None;
        let results = hybrid_recall(
            &fixture_ctx.store,
            &embedder,
            reranker,
            HybridParams {
                query,
                top_k: 3,
                rrf_k: 60.0,
                rerank_top_n: 10,
                filters: Filters {
                    diary: Some("default".to_string()),
                    kb: Some(kb.to_string()),
                    valid_at: None,
                },
                mode: RecallMode::Evidence,
                graph_hops: 1,
                allow_mixed_embeddings: false,
            },
        )
        .await?;
        let hit = results
            .iter()
            .any(|r| r.content.to_ascii_lowercase().contains(expected));
        if hit {
            correct += 1;
        }
        per_question.push(serde_json::json!({
            "query": query,
            "expected": expected,
            "hit": hit,
            "top_result_kind": results.first().map(|r| r.kind.clone()),
            "top_result": results.first().map(|r| r.content.clone()),
        }));
    }
    let accuracy = correct as f64 / cases.len() as f64;
    let payload = json!({
        "suite": "scientific-mini",
        "mode": "stub_hybrid_evidence",
        "questions_evaluated": cases.len(),
        "accuracy": accuracy,
        "correct": correct,
        "compile": compile,
        "per_question": per_question,
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

async fn run_mini(ctx: &AppContext) -> Result<(), CliError> {
    // Hybrid path if GEMINI_API_KEY is set; otherwise fall back to FTS-only
    // so the loop still runs deterministically in CI.
    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    let mode = if std::env::var("GEMINI_API_KEY").is_ok()
        && std::env::var("ENGRAM_BENCH_FORCE_STUB").is_err()
    {
        "hybrid_gemini"
    } else if std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok() {
        "hybrid_stub"
    } else {
        "fts_only"
    };

    let report = match mode {
        "hybrid_gemini" => {
            let embedder =
                GeminiEmbedder::from_env().map_err(|e| CliError::Config(format!("gemini: {e}")))?;
            engram_bench::mini::run_hybrid_baseline(&embedder, rrf_k).await?
        }
        "hybrid_stub" => {
            let embedder = StubEmbedder::default();
            engram_bench::mini::run_hybrid_baseline(&embedder, rrf_k).await?
        }
        _ => engram_bench::mini::run_fts_baseline()?,
    };

    let m = &report.metrics;
    let payload = json!({
        "suite": "mini",
        "mode": mode,
        "rrf_k": rrf_k,
        "recall_at_1": m.recall.at_1,
        "recall_at_5": m.recall.at_5,
        "recall_at_10": m.recall.at_10,
        "mrr": m.mrr,
        "p50_latency_ms": m.p50_latency_ms,
        "p95_latency_ms": m.p95_latency_ms,
        "mean_latency_ms": m.mean_latency_ms,
        "questions_evaluated": m.questions_evaluated,
        "per_question": report.per_question,
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

fn run_mini_fts(ctx: &AppContext) -> Result<(), CliError> {
    let report = engram_bench::mini::run_fts_baseline()?;
    let m = &report.metrics;
    let payload = json!({
        "suite": "mini-fts",
        "recall_at_1": m.recall.at_1,
        "recall_at_5": m.recall.at_5,
        "recall_at_10": m.recall.at_10,
        "mrr": m.mrr,
        "p50_latency_ms": m.p50_latency_ms,
        "p95_latency_ms": m.p95_latency_ms,
        "mean_latency_ms": m.mean_latency_ms,
        "questions_evaluated": m.questions_evaluated,
        "per_question": report.per_question,
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_longmemeval_qa(
    ctx: &AppContext,
    limit: Option<usize>,
    answerer_model: String,
    judge_model: String,
    ragas: bool,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::longmemeval::{default_s_path, LongMemEvalDataset};
    use engram_bench::qa::run_longmemeval_qa as run_qa;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;

    let path = default_s_path();
    if !path.exists() {
        return Err(CliError::Config(format!(
            "LongMemEval S not found at {}. Download with curl from \
             https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned",
            path.display()
        )));
    }
    let dataset = LongMemEvalDataset::load_from_file(&path)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| CliError::Config("GEMINI_API_KEY not set — required for QA bench".into()))?;
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| {
                CliError::Config(
                    "OPENROUTER_API_KEY not set — required for LLM answerer + judge".into(),
                )
            })?;

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    // Per-question checkpoint goes next to the final save path with a
    // .checkpoint.jsonl suffix. Always written so a crash leaves at
    // least the partial results recoverable. The user explicitly asked
    // for this after losing 30+ min on a sidecar OOM event.
    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("longmemeval-qa-{}.json", timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let report = if let Some(cohere) = cohere_key {
        let reranker = CohereReranker::new(cohere);
        run_qa(
            &dataset,
            &embedder,
            Some(&reranker),
            &answerer,
            &judge,
            rrf_k,
            top_k,
            limit,
            ragas,
            Some(checkpoint_path.clone()),
        )
        .await?
    } else {
        let no_rerank: Option<&PassthroughReranker> = None;
        run_qa(
            &dataset,
            &embedder,
            no_rerank,
            &answerer,
            &judge,
            rrf_k,
            top_k,
            limit,
            ragas,
            Some(checkpoint_path.clone()),
        )
        .await?
    };

    // Final report file (transactional, written only on full success).
    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "longmemeval-qa",
        "split": "s",
        "questions_evaluated": report.questions_evaluated,
        "accuracy": report.accuracy,
        "correct": report.correct_count,
        "recall_at_5": report.recall_at_5,
        "mrr": report.mrr,
        "ragas": report.ragas,
        "mean_latency_ms": report.mean_latency_ms,
        "p50_latency_ms": report.p50_latency_ms,
        "p95_latency_ms": report.p95_latency_ms,
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "answerer_prompt_tokens": report.answerer_total_prompt_tokens,
        "answerer_completion_tokens": report.answerer_total_completion_tokens,
        "judge_prompt_tokens": report.judge_total_prompt_tokens,
        "judge_completion_tokens": report.judge_total_completion_tokens,
        "by_question_type": report.by_question_type,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_locomo_qa(
    ctx: &AppContext,
    limit: Option<usize>,
    answerer_model: String,
    judge_model: String,
    ragas: bool,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::locomo::{default_path, LocomoDataset};
    use engram_bench::qa::run_locomo_qa as run_qa;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_rerank::zerank_local::ZerankLocalReranker;

    let path = default_path();
    if !path.exists() {
        return Err(CliError::Config(format!(
            "LoCoMo not found at {}. Download with: mkdir -p data/locomo && \
             curl -sL 'https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json' \
             -o {}",
            path.display(),
            path.display()
        )));
    }
    let dataset = LocomoDataset::load_from_file(&path)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| CliError::Config("GEMINI_API_KEY not set — required for QA bench".into()))?;
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| {
                CliError::Config(
                    "OPENROUTER_API_KEY not set — required for LLM answerer + judge".into(),
                )
            })?;

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    // Determine save path + per-question checkpoint up front so we can pass
    // the checkpoint into run_qa(). The checkpoint sits next to the final
    // report path with a .checkpoint.jsonl extension.
    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("locomo-qa-{}.json", timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    // Run LoCoMo through the SAME hybrid pipeline as LongMemEval-QA:
    // dense (Gemini Embed 2) + FTS5 BM25 + RRF fusion + optional reranker.
    //
    // Reranker provider is controlled by ENGRAM_RERANK_PROVIDER:
    //   - "zerank2" : local zerank-2 sidecar (best quality on biomed/STEM)
    //   - "cohere"  : Cohere rerank-v3.5 (default if COHERE_API_KEY set)
    //   - "none"    : passthrough (no rerank)
    let provider = std::env::var("ENGRAM_RERANK_PROVIDER").unwrap_or_else(|_| {
        if cohere_key.is_some() {
            "cohere".to_string()
        } else {
            "none".to_string()
        }
    });
    let report = match provider.as_str() {
        "zerank2" | "zerank-2" | "zerank_local" | "local" => {
            let reranker = ZerankLocalReranker::new();
            // Fail fast if the sidecar isn't reachable.
            if !reranker.health_check().await.unwrap_or(false) {
                return Err(CliError::Config(
                    "ENGRAM_RERANK_PROVIDER=zerank2 but the sidecar is not reachable. \
                     Start it with: uv run --with sentence-transformers --with torch \
                     crates/engram-rerank/python/zerank_server.py"
                        .into(),
                ));
            }
            run_qa(
                &dataset,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "cohere" => {
            let cohere = cohere_key.ok_or_else(|| {
                CliError::Config("ENGRAM_RERANK_PROVIDER=cohere but COHERE_API_KEY not set".into())
            })?;
            let reranker = CohereReranker::new(cohere);
            run_qa(
                &dataset,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "none" | "passthrough" | "off" => {
            let no_rerank: Option<&PassthroughReranker> = None;
            run_qa(
                &dataset,
                &embedder,
                no_rerank,
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown ENGRAM_RERANK_PROVIDER={other} (expected: zerank2 | cohere | none)"
            )));
        }
    };

    // Final transactional report (only written on full success).
    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "locomo-qa",
        "questions_evaluated": report.questions_evaluated,
        "accuracy": report.accuracy,
        "correct": report.correct_count,
        "ragas": report.ragas,
        "mean_latency_ms": report.mean_latency_ms,
        "p50_latency_ms": report.p50_latency_ms,
        "p95_latency_ms": report.p95_latency_ms,
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "answerer_prompt_tokens": report.answerer_total_prompt_tokens,
        "answerer_completion_tokens": report.answerer_total_completion_tokens,
        "judge_prompt_tokens": report.judge_total_prompt_tokens,
        "judge_completion_tokens": report.judge_total_completion_tokens,
        "by_category": report.by_question_type,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_locomo_plus_qa(
    ctx: &AppContext,
    limit: Option<usize>,
    answerer_model: String,
    judge_model: String,
    ragas: bool,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::locomo::{default_path as locomo_default_path, LocomoDataset};
    use engram_bench::locomo_plus::{default_path as locomo_plus_default_path, LocomoPlusDataset};
    use engram_bench::qa::run_locomo_plus_qa as run_qa;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_rerank::zerank_local::ZerankLocalReranker;

    let plus_path = locomo_plus_default_path();
    if !plus_path.exists() {
        return Err(CliError::Config(format!(
            "LoCoMo-Plus not found at {}. Download with:\nmkdir -p data/locomo_plus && curl -sL 'https://raw.githubusercontent.com/xjtuleeyf/Locomo-Plus/main/data/locomo_plus.json' -o data/locomo_plus/locomo_plus.json",
            plus_path.display()
        )));
    }
    let locomo_path = locomo_default_path();
    if !locomo_path.exists() {
        return Err(CliError::Config(format!(
            "LoCoMo not found at {}. Download with: mkdir -p data/locomo && \
             curl -sL 'https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json' \
             -o {}",
            locomo_path.display(),
            locomo_path.display()
        )));
    }
    let plus = LocomoPlusDataset::load_from_file(&plus_path)?;
    let locomo = LocomoDataset::load_from_file(&locomo_path)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| CliError::Config("GEMINI_API_KEY not set — required for QA bench".into()))?;
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| {
                CliError::Config(
                    "OPENROUTER_API_KEY not set — required for LLM answerer + judge".into(),
                )
            })?;

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("locomo-plus-qa-{}.json", timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let provider = std::env::var("ENGRAM_RERANK_PROVIDER").unwrap_or_else(|_| {
        if cohere_key.is_some() {
            "cohere".to_string()
        } else {
            "none".to_string()
        }
    });
    let report = match provider.as_str() {
        "zerank2" | "zerank-2" | "zerank_local" | "local" => {
            let reranker = ZerankLocalReranker::new();
            if !reranker.health_check().await.unwrap_or(false) {
                return Err(CliError::Config(
                    "ENGRAM_RERANK_PROVIDER=zerank2 but the sidecar is not reachable. \
                     Start it with: uv run --with sentence-transformers --with torch \
                     crates/engram-rerank/python/zerank_server.py"
                        .into(),
                ));
            }
            run_qa(
                &plus,
                &locomo,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "cohere" => {
            let cohere = cohere_key.ok_or_else(|| {
                CliError::Config("ENGRAM_RERANK_PROVIDER=cohere but COHERE_API_KEY not set".into())
            })?;
            let reranker = CohereReranker::new(cohere);
            run_qa(
                &plus,
                &locomo,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "none" | "passthrough" | "off" => {
            let no_rerank: Option<&PassthroughReranker> = None;
            run_qa(
                &plus,
                &locomo,
                &embedder,
                no_rerank,
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown ENGRAM_RERANK_PROVIDER={other} (expected: zerank2 | cohere | none)"
            )));
        }
    };

    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "locomo-plus",
        "questions_evaluated": report.questions_evaluated,
        "accuracy": report.accuracy,
        "correct": report.correct_count,
        "recall_at_5": report.recall_at_5,
        "mrr": report.mrr,
        "ragas": report.ragas,
        "mean_latency_ms": report.mean_latency_ms,
        "p50_latency_ms": report.p50_latency_ms,
        "p95_latency_ms": report.p95_latency_ms,
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "answerer_prompt_tokens": report.answerer_total_prompt_tokens,
        "answerer_completion_tokens": report.answerer_total_completion_tokens,
        "judge_prompt_tokens": report.judge_total_prompt_tokens,
        "judge_completion_tokens": report.judge_total_completion_tokens,
        "by_relation_type": report.by_question_type,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_memoryagentbench_qa(
    ctx: &AppContext,
    split_name: String,
    limit: Option<usize>,
    answerer_model: String,
    judge_model: String,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::memoryagentbench::{default_dir, MabDataset, MabSplit};
    use engram_bench::qa::run_memoryagentbench_qa as run_qa;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_rerank::zerank_local::ZerankLocalReranker;

    let split = MabSplit::from_name(&split_name).ok_or_else(|| {
        CliError::BadInput(format!(
            "unknown --mab-split {split_name} (expected: accurate_retrieval | test_time_learning | long_range_understanding | conflict_resolution)"
        ))
    })?;
    let path = default_dir().join(format!("{}.jsonl", split.name()));
    if !path.exists() {
        return Err(CliError::Config(format!(
            "MemoryAgentBench split not found at {}. Download with:\nuv run --with huggingface_hub --with pyarrow scripts/download_memoryagentbench.py",
            path.display()
        )));
    }
    let dataset = MabDataset::load_jsonl(&path, split)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| CliError::Config("GEMINI_API_KEY not set — required for QA bench".into()))?;
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| {
                CliError::Config(
                    "OPENROUTER_API_KEY not set — required for LLM answerer + judge".into(),
                )
            })?;

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!(
        "memoryagentbench-{}-{}.json",
        split.name().to_ascii_lowercase(),
        timestamp
    ));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let provider = std::env::var("ENGRAM_RERANK_PROVIDER").unwrap_or_else(|_| {
        if cohere_key.is_some() {
            "cohere".to_string()
        } else {
            "none".to_string()
        }
    });
    let report = match provider.as_str() {
        "zerank2" | "zerank-2" | "zerank_local" | "local" => {
            let reranker = ZerankLocalReranker::new();
            if !reranker.health_check().await.unwrap_or(false) {
                return Err(CliError::Config(
                    "ENGRAM_RERANK_PROVIDER=zerank2 but the sidecar is not reachable. \
                     Start it with: uv run --with sentence-transformers --with torch \
                     crates/engram-rerank/python/zerank_server.py"
                        .into(),
                ));
            }
            run_qa(
                &dataset,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "cohere" => {
            let cohere = cohere_key.ok_or_else(|| {
                CliError::Config("ENGRAM_RERANK_PROVIDER=cohere but COHERE_API_KEY not set".into())
            })?;
            let reranker = CohereReranker::new(cohere);
            run_qa(
                &dataset,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "none" | "passthrough" | "off" => {
            let no_rerank: Option<&PassthroughReranker> = None;
            run_qa(
                &dataset,
                &embedder,
                no_rerank,
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown ENGRAM_RERANK_PROVIDER={other} (expected: zerank2 | cohere | none)"
            )));
        }
    };

    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "memoryagentbench",
        "split": split.name(),
        "questions_evaluated": report.questions_evaluated,
        "unscored_count": report.unscored_count,
        "accuracy": report.accuracy,
        "correct": report.correct_count,
        "mean_latency_ms": report.mean_latency_ms,
        "p50_latency_ms": report.p50_latency_ms,
        "p95_latency_ms": report.p95_latency_ms,
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "answerer_prompt_tokens": report.answerer_total_prompt_tokens,
        "answerer_completion_tokens": report.answerer_total_completion_tokens,
        "judge_prompt_tokens": report.judge_total_prompt_tokens,
        "judge_completion_tokens": report.judge_total_completion_tokens,
        "by_question_type": report.by_question_type,
        "by_source": report.by_source,
        "notes": report.notes,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

async fn run_longmemeval(
    ctx: &AppContext,
    _download: bool,
    limit: Option<usize>,
) -> Result<(), CliError> {
    use engram_bench::longmemeval::{
        default_oracle_path, default_s_path, run_oracle_hybrid, LongMemEvalDataset,
    };
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;

    let split_choice = std::env::var("ENGRAM_LME_SPLIT").unwrap_or_else(|_| "s".into());
    let path = match split_choice.as_str() {
        "oracle" => default_oracle_path(),
        _ => default_s_path(),
    };
    if !path.exists() {
        return Err(CliError::Config(format!(
            "LongMemEval split not found at {}. Download with: \
             curl -sL 'https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_{}_cleaned.json' \
             -o {}",
            path.display(),
            if split_choice == "oracle" { "oracle".to_string() } else { "s".to_string() },
            path.display()
        )));
    }
    let dataset = LongMemEvalDataset::load_from_file(&path)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let have_gemini = gemini_key.is_some() && !force_stub;
    let have_cohere = cohere_key.is_some() && !force_stub;

    let report = if have_gemini {
        let embedder = GeminiEmbedder::new(gemini_key.clone().unwrap());
        if have_cohere {
            let reranker = CohereReranker::new(cohere_key.clone().unwrap());
            run_oracle_hybrid(&dataset, &embedder, Some(&reranker), rrf_k, limit).await?
        } else {
            let no_rerank: Option<&PassthroughReranker> = None;
            run_oracle_hybrid(&dataset, &embedder, no_rerank, rrf_k, limit).await?
        }
    } else {
        let embedder = StubEmbedder::default();
        let no_rerank: Option<&PassthroughReranker> = None;
        run_oracle_hybrid(&dataset, &embedder, no_rerank, rrf_k, limit).await?
    };

    let m = &report.metrics;
    let payload = json!({
        "suite": "longmemeval",
        "split": split_choice,
        "mode": report.mode,
        "rrf_k": rrf_k,
        "recall_at_1": m.recall.at_1,
        "recall_at_5": m.recall.at_5,
        "recall_at_10": m.recall.at_10,
        "mrr": m.mrr,
        "p50_latency_ms": m.p50_latency_ms,
        "p95_latency_ms": m.p95_latency_ms,
        "mean_latency_ms": m.mean_latency_ms,
        "questions_evaluated": m.questions_evaluated,
        "r1_correct": report.r1_count,
        "r5_correct": report.r5_count,
        "r10_correct": report.r10_count,
        "limit": limit,
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}
