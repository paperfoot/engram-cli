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
use serde_json::json;

pub async fn run(
    ctx: &AppContext,
    suite: String,
    download: bool,
    limit: Option<usize>,
) -> Result<(), CliError> {
    match suite.as_str() {
        "mini" => run_mini(ctx).await,
        "mini-fts" => run_mini_fts(ctx),
        "longmemeval" => run_longmemeval(ctx, download, limit).await,
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

async fn run_longmemeval(
    ctx: &AppContext,
    _download: bool,
    limit: Option<usize>,
) -> Result<(), CliError> {
    use engram_bench::longmemeval::{
        default_oracle_path, default_s_path, run_oracle_hybrid, LongMemEvalDataset,
    };

    // Prefer the S split (real retrieval test, ~48 sessions/question, 96%
    // distractors) over the Oracle split (haystack == answer, trivially 1.0).
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

    let report = if std::env::var("GEMINI_API_KEY").is_ok()
        && std::env::var("ENGRAM_BENCH_FORCE_STUB").is_err()
    {
        let embedder = GeminiEmbedder::from_env()
            .map_err(|e| CliError::Config(format!("gemini: {e}")))?;
        run_oracle_hybrid(&dataset, &embedder, rrf_k, limit).await?
    } else {
        let embedder = StubEmbedder::default();
        run_oracle_hybrid(&dataset, &embedder, rrf_k, limit).await?
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
