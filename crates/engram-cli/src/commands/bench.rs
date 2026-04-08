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
    _ragas: bool,
    _top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::locomo::{default_path, flatten_conversation, LocomoDataset};

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

    // LoCoMo is different enough that we do a simpler eval here: for each
    // sample, flatten the conversation into sessions, embed, recall per QA,
    // have the answerer answer, judge correct.
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| CliError::Config("GEMINI_API_KEY required".into()))?;
    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| CliError::Config("OPENROUTER_API_KEY required".into()))?;

    // limit = max total QAs, not samples. Each LoCoMo sample has ~200 QAs,
    // so capping at sample granularity is too coarse.
    let max_questions = limit.unwrap_or(10);
    let n_samples = dataset.samples.len();

    let mut total_q = 0usize;
    let mut correct_q = 0usize;
    let embedder = GeminiEmbedder::new(gemini_key);
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    use engram_bench::judge::judge_answer;
    use engram_embed::{Embedder, TaskMode};
    use engram_llm::{ChatLlm, ChatMessage};

    let start = std::time::Instant::now();

    'outer: for sample in dataset.samples.iter().take(n_samples) {
        if total_q >= max_questions {
            break;
        }
        let sessions = flatten_conversation(&sample.conversation);
        if sessions.is_empty() {
            continue;
        }

        // Embed all sessions once per sample.
        let texts: Vec<&str> = sessions.iter().map(|(_, t)| t.as_str()).collect();
        let embeddings = embedder
            .embed_batch(&texts, TaskMode::RetrievalDocument)
            .await
            .map_err(|e| CliError::Config(format!("embed: {e}")))?;

        for qa in &sample.qa {
            if total_q >= max_questions {
                break 'outer;
            }
            // Skip questions without a gold answer (adversarial-only entries).
            let gold = match qa.answer.as_deref() {
                Some(a) if !a.is_empty() => a.to_string(),
                _ => continue,
            };
            total_q += 1;
            let q_emb = embedder
                .embed_one(&qa.question, TaskMode::RetrievalQuery)
                .await
                .map_err(|e| CliError::Config(format!("embed query: {e}")))?;

            // Cosine similarity per session, take top 5.
            let mut scored: Vec<(usize, f32)> = embeddings
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let mut dot = 0f32;
                    let mut na = 0f32;
                    let mut nb = 0f32;
                    for (a, b) in q_emb.iter().zip(e.iter()) {
                        dot += a * b;
                        na += a * a;
                        nb += b * b;
                    }
                    let sim = if na == 0.0 || nb == 0.0 {
                        0.0
                    } else {
                        dot / (na.sqrt() * nb.sqrt())
                    };
                    (i, sim)
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(5);

            let context: String = scored
                .iter()
                .map(|(i, _)| format!("[{}]\n{}", sessions[*i].0, sessions[*i].1))
                .collect::<Vec<_>>()
                .join("\n\n");

            let ans_resp = answerer
                .chat(&[
                    ChatMessage::system(
                        "Answer using only the provided conversation context. \
                         If the answer is not in the context, say 'I don't know.' \
                         Be concise.",
                    ),
                    ChatMessage::user(format!("Context:\n{}\n\nQuestion: {}", context, qa.question)),
                ])
                .await
                .map_err(|e| CliError::Config(format!("answerer: {e}")))?;

            let verdict = judge_answer(&judge, &qa.question, &gold, &ans_resp.content)
                .await
                .map_err(|e| CliError::Config(format!("judge: {e}")))?;
            if verdict.correct {
                correct_q += 1;
            }
            tracing::info!(
                "[locomo-qa {}/{}] correct={} q={}",
                total_q,
                max_questions,
                verdict.correct,
                qa.question.chars().take(60).collect::<String>()
            );
        }
    }

    let accuracy = correct_q as f32 / total_q.max(1) as f32;
    let report = json!({
        "suite": "locomo-qa",
        "questions_evaluated": total_q,
        "correct": correct_q,
        "accuracy": accuracy,
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "elapsed_ms": start.elapsed().as_millis() as u64,
    });

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let save_path = save.unwrap_or_else(|| {
        benchmarks_dir.join(format!("locomo-qa-{}-{}.json", timestamp, total_q))
    });
    let _ = std::fs::write(&save_path, serde_json::to_string_pretty(&report)?);

    print_success(
        ctx.format,
        report,
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
