//! `engram recall <query>` — hybrid retrieval (dense + lexical + rerank) over
//! the persistent store.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use crate::retrieval::{hybrid_recall, Filters, HybridParams};
use chrono::{DateTime, Utc};
use engram_core::types::Layer;
use engram_core::layers::approx_tokens;
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_rerank::cohere::CohereReranker;
use engram_rerank::passthrough::PassthroughReranker;
use serde_json::json;
use std::time::Instant;

pub async fn run(
    ctx: &AppContext,
    query: String,
    top_k: usize,
    layer: String,
    diary: String,
    since: Option<String>,
    until: Option<String>,
) -> Result<(), CliError> {
    if query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }

    let start = Instant::now();

    let layer_enum = parse_layer(&layer)?;
    let _token_budget = layer_enum.default_token_budget();

    let valid_at = parse_optional_time(&since)?;
    let _valid_until = parse_optional_time(&until)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    let filters = Filters {
        diary: if diary == "*" { None } else { Some(diary.clone()) },
        valid_at,
    };

    let params = HybridParams {
        query: &query,
        top_k,
        rrf_k,
        filters,
    };

    // Resolve keys in order: env var → config file → none.
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let have_gemini = gemini_key.is_some() && !force_stub;
    let have_cohere = cohere_key.is_some() && !force_stub;

    let results = if have_gemini {
        let embedder = GeminiEmbedder::new(gemini_key.clone().unwrap());
        if have_cohere {
            let reranker = CohereReranker::new(cohere_key.clone().unwrap());
            hybrid_recall(&ctx.store, &embedder, Some(&reranker), params).await?
        } else {
            let reranker: Option<&PassthroughReranker> = None;
            hybrid_recall(&ctx.store, &embedder, reranker, params).await?
        }
    } else {
        let embedder = StubEmbedder::default();
        let reranker: Option<&PassthroughReranker> = None;
        hybrid_recall(&ctx.store, &embedder, reranker, params).await?
    };

    // Apply memory layer token budget: trim results so their combined content
    // fits. Deep layer is effectively uncapped for top_k=10 under ~16k tokens.
    let budget = layer_enum.default_token_budget();
    let mut used_tokens = 0usize;
    let mut layered: Vec<_> = Vec::new();
    for r in &results {
        let cost = approx_tokens(&r.content);
        if used_tokens + cost > budget {
            if layered.is_empty() {
                // Always include at least one even if over budget.
                layered.push(r);
                used_tokens += cost;
            }
            break;
        }
        layered.push(r);
        used_tokens += cost;
    }

    let results_json: Vec<_> = layered
        .iter()
        .map(|r| json!({
            "chunk_id": r.chunk_id.to_string(),
            "content": r.content,
            "score": r.score,
            "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
        }))
        .collect();

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("retriever", if have_gemini { "hybrid_gemini" } else { "hybrid_stub" });
    meta.add("reranker", if have_cohere { "cohere" } else { "passthrough" });
    meta.add("rrf_k", rrf_k);
    meta.add("layer", format!("{:?}", layer_enum).to_lowercase());
    meta.add("token_budget", budget);
    meta.add("tokens_used", used_tokens);
    meta.add("results_returned", results_json.len());

    let status = if results_json.is_empty() { "no_results" } else { "success" };
    print_success(
        ctx.format,
        json!({ "status": status, "results": results_json }),
        meta,
        |data| {
            if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
                for r in arr {
                    let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let snippet: String = content.chars().take(100).collect();
                    println!("{:.3}  {}", score, snippet);
                }
            }
        },
    );
    Ok(())
}

fn parse_layer(s: &str) -> Result<Layer, CliError> {
    match s.to_ascii_lowercase().as_str() {
        "identity" | "l0" => Ok(Layer::Identity),
        "critical" | "l1" => Ok(Layer::Critical),
        "topic" | "l2" => Ok(Layer::Topic),
        "deep" | "l3" => Ok(Layer::Deep),
        other => Err(CliError::BadInput(format!(
            "unknown layer: {other} (expected identity|critical|topic|deep)"
        ))),
    }
}

fn parse_optional_time(s: &Option<String>) -> Result<Option<DateTime<Utc>>, CliError> {
    match s {
        None => Ok(None),
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|d| Some(d.with_timezone(&Utc)))
            .map_err(|e| CliError::BadInput(format!("invalid timestamp {v}: {e}"))),
    }
}
