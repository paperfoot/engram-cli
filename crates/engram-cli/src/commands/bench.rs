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
        "longmemeval" => run_longmemeval(ctx, download, limit).await,
        "longmemeval-qa" | "lme-qa" => {
            run_longmemeval_qa(ctx, limit, answerer, judge, ragas, top_k, save).await
        }
        "locomo-qa" => run_locomo_qa(ctx, limit, answerer, judge, ragas, top_k, save).await,
        other => Err(CliError::BadInput(format!("unknown suite: {other}"))),
    }
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
            let embedder = GeminiEmbedder::from_env()
                .map_err(|e| CliError::Config(format!("gemini: {e}")))?;
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
    print_success(
        ctx.format,
        payload,
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
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
    print_success(
        ctx.format,
        payload,
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
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

    let embedder = GeminiEmbedder::new(gemini_key);
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

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
        )
        .await?
    };

    // Persist the report to disk if requested, plus always to benchmarks/.
    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir
        .join(format!("longmemeval-qa-{}-{}.json", timestamp, report.questions_evaluated));
    let save_path = save.unwrap_or(default_name);
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
    print_success(
        ctx.format,
        payload,
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
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

    let embedder = GeminiEmbedder::new(gemini_key);
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    // Run LoCoMo through the SAME hybrid pipeline as LongMemEval-QA:
    // dense (Gemini Embed 2) + FTS5 BM25 + RRF fusion + optional reranker.
    //
    // Reranker provider is controlled by ENGRAM_RERANK_PROVIDER:
    //   - "zerank2" : local zerank-2 sidecar (best quality on biomed/STEM)
    //   - "cohere"  : Cohere rerank-v3.5 (default if COHERE_API_KEY set)
    //   - "none"    : passthrough (no rerank)
    let provider = std::env::var("ENGRAM_RERANK_PROVIDER")
        .unwrap_or_else(|_| {
            if cohere_key.is_some() { "cohere".to_string() } else { "none".to_string() }
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
                &dataset, &embedder, Some(&reranker), &answerer, &judge,
                rrf_k, top_k, limit, ragas,
            )
            .await?
        }
        "cohere" => {
            let cohere = cohere_key.ok_or_else(|| {
                CliError::Config(
                    "ENGRAM_RERANK_PROVIDER=cohere but COHERE_API_KEY not set".into(),
                )
            })?;
            let reranker = CohereReranker::new(cohere);
            run_qa(
                &dataset, &embedder, Some(&reranker), &answerer, &judge,
                rrf_k, top_k, limit, ragas,
            )
            .await?
        }
        "none" | "passthrough" | "off" => {
            let no_rerank: Option<&PassthroughReranker> = None;
            run_qa(
                &dataset, &embedder, no_rerank, &answerer, &judge,
                rrf_k, top_k, limit, ragas,
            )
            .await?
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown ENGRAM_RERANK_PROVIDER={other} (expected: zerank2 | cohere | none)"
            )));
        }
    };

    // Persist the full per_question report to disk, same as LongMemEval-QA.
    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir
        .join(format!("locomo-qa-{}-{}.json", timestamp, report.questions_evaluated));
    let save_path = save.unwrap_or(default_name);
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
    print_success(
        ctx.format,
        payload,
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
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
    print_success(
        ctx.format,
        payload,
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
