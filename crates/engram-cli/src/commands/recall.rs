//! `engram recall <query>` — cloud-quality hybrid retrieval over a KB.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_envelope, Metadata};
use crate::retrieval::{
    hybrid_recall, render_answer_context, resolve_kb_scope, resolve_retrieval_backends, Filters,
    HybridParams, RecallMode, RetrievalProfile,
};
use chrono::{DateTime, Utc};
use engram_core::layers::approx_tokens;
use engram_core::temporal::{parse_bound, DayBound};
use engram_core::types::Layer;
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
    let since_dt = parse_optional_time(&since, DayBound::Start)?;
    let until_dt = parse_optional_time(&until, DayBound::End)?;
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

    // Provider choice, the missing-key hard error and the spend guardrail all
    // live in `resolve_retrieval_backends` now — the three copies of this block
    // drifted, and research and the daemon kept silently falling back to the
    // 64-dim stub long after recall stopped.
    let backends = resolve_retrieval_backends(&ctx.store, profile_enum, scope.as_deref())?;
    let outcome = hybrid_recall(
        &ctx.store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;

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

    // `layered` is a Vec<&HybridResult>; `.iter()` would hand the renderer
    // `&&HybridResult`, so copy the references out.
    let answer_context = render_answer_context(layered.iter().copied());
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
    let gemini_query_cost = if backends.cloud_embedder {
        gemini_embed_cost_usd(query_tokens_estimated)
    } else {
        0.0
    };
    let cohere_search_units = if backends.use_cohere() && !results.is_empty() {
        1.0
    } else {
        0.0
    };
    let cohere_cost = cohere_rerank_cost_usd(cohere_search_units);

    if backends.cloud_embedder {
        ctx.store.record_usage_event(
            "gemini",
            "recall_query_embed",
            Some(backends.embed_model.as_str()),
            scope.as_deref(),
            if diary == "*" { None } else { Some(&diary) },
            1,
            1,
            query_tokens_estimated,
            0,
            0.0,
            gemini_query_cost,
            json!({
                "dimensions": backends.embed_dims,
                "prompt_format": backends.prompt_format.clone(),
                "estimated": true,
            }),
        )?;
    }
    if backends.use_cohere() && !results.is_empty() {
        ctx.store.record_usage_event(
            "cohere",
            "recall_rerank",
            Some(backends.cohere_model.as_str()),
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
    // Same contract for the temporal window: name the sources it withheld, so
    // "no wiki page came back" reads as a scope decision, not an empty store.
    if let Some(temporal) = notes.temporal_scope_json() {
        meta.add("temporal_scope", temporal);
    }
    meta.add(
        "retriever",
        if backends.cloud_embedder {
            "hybrid_gemini"
        } else {
            "hybrid_stub"
        },
    );
    meta.add("embed_model", backends.embed_model.clone());
    meta.add("embed_dims", backends.embed_dims);
    meta.add("embed_prompt_format", backends.prompt_format.clone());
    meta.add("reranker", backends.reranker_label.clone());
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
                "embed_model": backends.embed_model,
                "embed_dims": backends.embed_dims,
                "prompt_format": backends.prompt_format,
                "reranker": backends.reranker_label,
                "candidates_considered": rerank_candidates,
                "kb": scope.label(),
                "kb_scope": scope.origin.as_str(),
                "diary_isolation": notes.diary_isolation_json(),
                "temporal_scope": notes.temporal_scope_json()
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

/// Parse `--since` / `--until`.
///
/// The write side (`remember --event-time`) accepts RFC3339 *or* `YYYY-MM-DD`
/// and its own help text says it drives `recall --since/--until`, so the read
/// side has to accept the same spellings — an RFC3339-only reader turned the
/// documented round-trip (`--event-time 2026-06-01` then `--since 2026-06-01`)
/// into exit 3.
///
/// A bare date is a whole day, so the bound it denotes depends on its side of
/// the range: `--since 2026-06-01` starts at 00:00:00Z and
/// `--until 2026-06-01` ends at 23:59:59.999999999Z, which makes
/// `--since D --until D` select exactly day D. RFC3339 input is an exact
/// instant and is unchanged. Anything else is still a hard error, never a
/// silent "now".
fn parse_optional_time(
    s: &Option<String>,
    bound: DayBound,
) -> Result<Option<DateTime<Utc>>, CliError> {
    match s {
        None => Ok(None),
        Some(v) => parse_bound(v, bound).map(Some).ok_or_else(|| {
            CliError::BadInput(format!(
                "invalid timestamp {v}: expected RFC3339 (2026-06-01T09:30:00Z) or YYYY-MM-DD"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_and_until_accept_a_bare_date_and_bracket_the_day() {
        let since = parse_optional_time(&Some("2026-06-01".into()), DayBound::Start)
            .unwrap()
            .unwrap();
        let until = parse_optional_time(&Some("2026-06-01".into()), DayBound::End)
            .unwrap()
            .unwrap();
        assert_eq!(since.to_rfc3339(), "2026-06-01T00:00:00+00:00");
        assert_eq!(until.to_rfc3339(), "2026-06-01T23:59:59.999999999+00:00");
    }

    #[test]
    fn rfc3339_bounds_are_unchanged() {
        let t = parse_optional_time(&Some("2026-06-01T09:30:00Z".into()), DayBound::End)
            .unwrap()
            .unwrap();
        assert_eq!(t.to_rfc3339(), "2026-06-01T09:30:00+00:00");
    }

    #[test]
    fn garbage_bounds_are_bad_input_exit_3() {
        for raw in ["not a date", "2026-13-45", ""] {
            let err = parse_optional_time(&Some(raw.into()), DayBound::Start).unwrap_err();
            assert_eq!(err.exit_code(), 3, "{raw}");
        }
        assert!(parse_optional_time(&None, DayBound::Start)
            .unwrap()
            .is_none());
    }
}
