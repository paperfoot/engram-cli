//! `engram recall <query>` — cloud-quality hybrid retrieval over a KB.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_envelope, Metadata};
use crate::retrieval::{
    hybrid_recall, resolve_kb_scope, Filters, HybridParams, RecallMode, RetrievalProfile,
};
use chrono::{DateTime, Utc};
use engram_core::layers::approx_tokens;
use engram_core::types::Layer;
use engram_embed::gemini::{GeminiEmbedder, DEFAULT_DIMS, DEFAULT_MODEL, PROMPT_FORMAT};
use engram_embed::stub::StubEmbedder;
use engram_embed::Embedder;
use engram_rerank::cohere::CohereReranker;
use engram_rerank::passthrough::PassthroughReranker;
use serde_json::json;
use std::time::Instant;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    ctx: &AppContext,
    query: String,
    top_k: usize,
    layer: String,
    mode: String,
    profile: String,
    kb: Option<String>,
    all_kbs: bool,
    diary: String,
    rerank_top_n: Option<usize>,
    graph_hops: u8,
    allow_mixed_embeddings: bool,
    since: Option<String>,
    until: Option<String>,
) -> Result<(), CliError> {
    if query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }

    let start = Instant::now();
    let layer_enum = parse_layer(&layer)?;
    let recall_mode = RecallMode::parse(&mode)?;
    let profile_enum = RetrievalProfile::parse(&profile)?;
    let since_dt = parse_optional_time(&since)?;
    let until_dt = parse_optional_time(&until)?;
    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    // Scope resolution: explicit --kb > retrieval.default_kb > every KB.
    // The old default ("default") meant a plain recall searched a single,
    // usually near-empty namespace and missed everything filed elsewhere.
    let scope = resolve_kb_scope(kb.as_deref(), all_kbs);

    let rerank_candidates = rerank_top_n.unwrap_or_else(|| profile_enum.default_rerank_top_n());
    let filters = Filters {
        diary: if diary == "*" {
            None
        } else {
            Some(diary.clone())
        },
        kb: scope.kb.clone(),
        since: since_dt,
        until: until_dt,
    };
    let params = HybridParams {
        query: &query,
        top_k,
        rrf_k,
        rerank_top_n: rerank_candidates,
        filters,
        mode: recall_mode,
        graph_hops,
        allow_mixed_embeddings,
    };

    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");

    // A cloud profile (cloud_quality/fast) with no Gemini key used to fall back
    // to the 64-dim hash stub SILENTLY — producing garbage similarity and, on a
    // real gemini-indexed KB, misdiagnosing the missing key as an index
    // mismatch. Fail loudly instead (exit code 2 = config). `--profile offline`
    // and the bench stub-forcing env var opt into the stub explicitly.
    if profile_enum != RetrievalProfile::Offline && gemini_key.is_none() && !force_stub {
        return Err(CliError::Config(format!(
            "profile `{}` needs a Gemini API key for real retrieval, but none is configured. \
             Set it with `engram config set keys.gemini <KEY>` (or GEMINI_API_KEY), \
             or run with `--profile offline` to use the deterministic offline stub.",
            profile_enum.as_str()
        )));
    }

    let use_cloud_embedder =
        profile_enum != RetrievalProfile::Offline && gemini_key.is_some() && !force_stub;

    // Enforce spend guardrails before any paid embed/rerank call.
    if use_cloud_embedder {
        crate::commands::budget::enforce_budget(ctx, scope.as_deref())?;
    }
    let use_cohere = profile_enum != RetrievalProfile::Offline
        && cohere_key.is_some()
        && !force_stub
        && !crate::commands::config::rerank_disabled();
    let cohere_model = engram_rerank::cohere::resolve_model();

    let (outcome, embed_model, embed_dims, prompt_format) = if use_cloud_embedder {
        let embedder = GeminiEmbedder::new(gemini_key.clone().unwrap());
        let meta = (
            embedder.model(),
            embedder.dimensions(),
            embedder.prompt_format().to_string(),
        );
        let results = if use_cohere {
            let reranker = CohereReranker::new(cohere_key.clone().unwrap());
            hybrid_recall(&ctx.store, &embedder, Some(&reranker), params).await?
        } else {
            let reranker: Option<&PassthroughReranker> = None;
            hybrid_recall(&ctx.store, &embedder, reranker, params).await?
        };
        (results, meta.0, meta.1, meta.2)
    } else {
        let embedder = StubEmbedder::default();
        let meta = (
            embedder.model(),
            embedder.dimensions(),
            embedder.prompt_format().to_string(),
        );
        let reranker: Option<&PassthroughReranker> = None;
        let results = hybrid_recall(&ctx.store, &embedder, reranker, params).await?;
        (results, meta.0, meta.1, meta.2)
    };

    let results = outcome.results;
    let notes = outcome.notes;

    // The memory-layer token budget trims the returned set to fit an agent's
    // context window. But it must never silently return FEWER than the caller's
    // explicit --top-k: previously a default `topic` budget (800 tokens) capped
    // a `--top-k 10` recall to ~2-4 paper chunks. Honor the smaller of what the
    // user asked for and the budget, but always deliver at least top_k results
    // when that many were retrieved.
    let budget = layer_enum.default_token_budget();
    let mut used_tokens = 0usize;
    let mut layered = Vec::new();
    for r in &results {
        let cost = approx_tokens(&r.content);
        // Always include up to top_k results regardless of budget; only the
        // budget-driven trim beyond top_k applies.
        if layered.len() >= top_k && used_tokens + cost > budget {
            break;
        }
        layered.push(r);
        used_tokens += cost;
    }

    // Surface contradictions relevant to this query: if the KB records that a
    // fact about a queried subject was superseded (or two active facts about it
    // disagree), report it so the caller can "cite both / note the update"
    // instead of unknowingly answering from a stale fact. This is the read-path
    // half of the contradiction-resolution promise.
    let conflicts = surface_conflicts(ctx, scope.as_deref(), &query)?;

    let answer_context = build_answer_context(&layered);
    let results_json: Vec<_> = layered
        .iter()
        .map(|r| json!({
            "id": r.id.to_string(),
            "kind": r.kind,
            "score": r.score,
            "rerank_score": r.rerank_score,
            "kb": r.kb,
            "content": r.content,
            "citations": r.citations.iter().map(|c| json!({
                "document_id": c.document_id.map(|id: uuid::Uuid| id.to_string()),
                "chunk_id": c.chunk_id.map(|id: uuid::Uuid| id.to_string()),
                "page": c.page,
                "section": c.section,
                "source": c.source,
            })).collect::<Vec<_>>(),
            "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
        }))
        .collect();

    let query_tokens_estimated = estimated_tokens(&query);
    let gemini_query_cost = if use_cloud_embedder {
        gemini_embed_cost_usd(query_tokens_estimated)
    } else {
        0.0
    };
    let cohere_search_units = if use_cohere && !results.is_empty() {
        1.0
    } else {
        0.0
    };
    let cohere_cost = cohere_rerank_cost_usd(cohere_search_units);

    if use_cloud_embedder {
        ctx.store.record_usage_event(
            "gemini",
            "recall_query_embed",
            Some(&embed_model),
            scope.as_deref(),
            if diary == "*" { None } else { Some(&diary) },
            1,
            1,
            query_tokens_estimated,
            0,
            0.0,
            gemini_query_cost,
            json!({
                "dimensions": embed_dims,
                "prompt_format": prompt_format.clone(),
                "estimated": true,
            }),
        )?;
    }
    if use_cohere && !results.is_empty() {
        ctx.store.record_usage_event(
            "cohere",
            "recall_rerank",
            Some(cohere_model.as_str()),
            scope.as_deref(),
            if diary == "*" { None } else { Some(&diary) },
            1,
            rerank_candidates as i64,
            0,
            0,
            cohere_search_units,
            cohere_cost,
            json!({
                "top_k": top_k,
                "candidate_budget": rerank_candidates,
            }),
        )?;
    }

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("profile", profile_enum.as_str());
    meta.add("mode", recall_mode.as_str());
    meta.add("kb", scope.label());
    meta.add("kb_scope", scope.origin.as_str());
    meta.add("diary", if diary == "*" { "*" } else { diary.as_str() });
    // Report anything the diary-isolation guard withheld, so a narrowed result
    // set is explained rather than silently short.
    if let Some(isolation) = notes.diary_isolation_json() {
        meta.add("diary_isolation", isolation);
    }
    meta.add(
        "retriever",
        if use_cloud_embedder {
            "hybrid_gemini"
        } else {
            "hybrid_stub"
        },
    );
    meta.add("embed_model", embed_model.clone());
    meta.add("embed_dims", embed_dims);
    meta.add("embed_prompt_format", prompt_format.clone());
    meta.add(
        "reranker",
        if use_cohere {
            format!("cohere/{}", cohere_model)
        } else {
            "passthrough".to_string()
        },
    );
    meta.add("rrf_k", rrf_k);
    meta.add("layer", format!("{:?}", layer_enum).to_lowercase());
    meta.add("token_budget", budget);
    meta.add("tokens_used", used_tokens);
    meta.add("results_returned", results_json.len());
    meta.add("candidates_considered", rerank_candidates);
    meta.add("gemini_query_tokens_estimated", query_tokens_estimated);
    meta.add("gemini_cost_usd", format!("{:.7}", gemini_query_cost));
    meta.add("cohere_search_units", cohere_search_units);
    meta.add("cohere_cost_usd", format!("{:.5}", cohere_cost));
    meta.add(
        "total_cost_usd_estimated",
        format!("{:.6}", gemini_query_cost + cohere_cost),
    );

    let status = if results_json.is_empty() {
        "no_results"
    } else {
        "success"
    };
    print_envelope(
        ctx.format,
        status,
        json!({
            "status": status,
            "answer_context": answer_context,
            "results": results_json,
            "conflicts": conflicts,
            "metadata": {
                "profile": profile_enum.as_str(),
                "embed_model": if use_cloud_embedder { DEFAULT_MODEL } else { embed_model.as_str() },
                "embed_dims": if use_cloud_embedder { DEFAULT_DIMS } else { embed_dims },
                "prompt_format": if use_cloud_embedder { PROMPT_FORMAT } else { prompt_format.as_str() },
                "reranker": if use_cohere { format!("cohere/{}", cohere_model) } else { "passthrough".to_string() },
                "candidates_considered": rerank_candidates,
                "kb": scope.label(),
                "kb_scope": scope.origin.as_str(),
                "diary_isolation": notes.diary_isolation_json()
            }
        }),
        meta,
        |data| {
            if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
                for r in arr {
                    let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("result");
                    let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let snippet: String = content.chars().take(110).collect();
                    println!("{:.3}  {:10}  {}", score, kind, snippet);
                }
            }
        },
    );
    Ok(())
}

/// Find contradictions relevant to the query and return them as JSON.
/// Two kinds are surfaced: RESOLVED supersessions (a fact was updated —
/// cite the current value) and CONTESTED pairs (an unresolved conflict is
/// queued — both values are active and the caller should not pick one
/// silently). The memory never pretends to know an answer it doesn't have.
fn surface_conflicts(
    ctx: &AppContext,
    kb: Option<&str>,
    query: &str,
) -> Result<Vec<serde_json::Value>, CliError> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let facts = ctx.store.facts_matching_terms(kb, &terms, 20)?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in facts {
        // Only superseded facts represent a recorded contradiction/update.
        let Some(superseded_by) = f.superseded_by else {
            continue;
        };
        if !seen.insert(f.id) {
            continue;
        }
        let new_object = ctx
            .store
            .get_fact(superseded_by)?
            .map(|nf| nf.object)
            .unwrap_or_default();
        out.push(json!({
            "status": "superseded",
            "subject": f.subject,
            "predicate": f.predicate,
            "old_object": f.object,
            "new_object": new_object,
            "superseded_at": f.superseded_at.map(|t| t.to_rfc3339()),
            "old_fact_id": f.id.to_string(),
            "new_fact_id": superseded_by.to_string(),
            "note": "this fact was updated in the knowledge base; cite the current value or surface the change",
        }));
    }

    // Contested: open conflicts touching the query. Both values are active;
    // report both rather than silently returning one side.
    for (row, new_f, old_f) in ctx.store.open_conflicts_matching_terms(kb, &terms, 10)? {
        out.push(json!({
            "status": "contested",
            "conflict_id": row.id.to_string(),
            "subject": new_f.subject,
            "predicate": new_f.predicate,
            "values": [old_f.object, new_f.object],
            "detector": row.detector,
            "suggested_action": row.suggested,
            "resolve_command": format!(
                "engram facts resolve {} --action supersede-old|keep-both|reject-new",
                row.id
            ),
            "note": "unresolved contradiction — do not pick a value silently; resolve it or surface both to the user",
        }));
    }
    Ok(out)
}

/// Lowercase content tokens from a query (length >= 3, no stopwords) used to
/// match fact subjects/objects.
fn query_terms(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "what", "which", "how", "does", "are", "was", "were", "with", "that",
        "this", "from", "into", "who", "why", "when", "where", "current", "latest", "value",
    ];
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        let lower = raw.to_ascii_lowercase();
        if lower.len() < 3 || STOP.contains(&lower.as_str()) {
            continue;
        }
        if seen.insert(lower.clone()) {
            out.push(lower);
        }
    }
    out
}

fn build_answer_context(results: &[&crate::retrieval::HybridResult]) -> String {
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let citation = r
            .citations
            .first()
            .and_then(|c| {
                c.source
                    .clone()
                    .or_else(|| c.chunk_id.map(|id: uuid::Uuid| id.to_string()))
            })
            .unwrap_or_else(|| r.id.to_string());
        out.push_str(&format!(
            "[{}] ({}, kb={}) {}\nsource: {}\n\n",
            i + 1,
            r.kind,
            r.kb,
            r.content.trim(),
            citation
        ));
    }
    out
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
