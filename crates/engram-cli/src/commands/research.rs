//! `engram research` — agentic, evidence-first retrieval plan over a KB.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata, Scope, Verdict};
use crate::read_surface;
use crate::retrieval::{
    build_fts_query, hybrid_recall, render_answer_context, resolve_retrieval_backends, Filters,
    HybridParams, RecallMode, RetrievalProfile,
};
use serde_json::{json, Value};
use std::time::Instant;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    ctx: &AppContext,
    query: String,
    kb: String,
    all_kbs: bool,
    diary: String,
    top_k: usize,
    profile: String,
    allow_mixed_embeddings: bool,
) -> Result<(), CliError> {
    if query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }
    let start = Instant::now();
    let profile_enum = RetrievalProfile::parse(&profile)?;
    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);
    let rerank_candidates = std::env::var("ENGRAM_RERANK_CANDIDATES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| profile_enum.default_rerank_top_n());
    let filters = Filters {
        diary: if diary == "*" {
            None
        } else {
            Some(diary.clone())
        },
        kb: if all_kbs { None } else { Some(kb.clone()) },
        since: None,
        until: None,
        // Research assembles evidence for synthesis; an expired memory is not
        // evidence of current state. Same default as recall.
        include_expired: false,
    };
    let params = HybridParams {
        query: &query,
        top_k,
        rrf_k,
        rerank_top_n: rerank_candidates,
        filters,
        mode: RecallMode::Explore,
        graph_hops: 2,
        allow_mixed_embeddings,
    };

    // Same rule the CLI recall path applies: a cloud profile with no key is a
    // config error, never a silent stub, and the spend guardrail gates the paid
    // call. `research` had neither — it was the original copy of this block.
    let backends = resolve_retrieval_backends(
        &ctx.store,
        profile_enum,
        if all_kbs { None } else { Some(kb.as_str()) },
    )?;
    let outcome = hybrid_recall(
        &ctx.store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;
    let results = outcome.results;
    let stats = outcome.stats;

    let query_tokens = estimated_tokens(&query);
    let gemini_cost = if backends.cloud_embedder {
        gemini_embed_cost_usd(query_tokens)
    } else {
        0.0
    };
    if backends.cloud_embedder {
        ctx.store.record_usage_event(
            "gemini",
            "research_query_embed",
            Some(backends.embed_model.as_str()),
            if all_kbs { None } else { Some(&kb) },
            if diary == "*" { None } else { Some(&diary) },
            1,
            1,
            query_tokens,
            0,
            0.0,
            gemini_cost,
            json!({ "dimensions": backends.embed_dims, "prompt_format": backends.prompt_format, "estimated": true }),
        )?;
    }
    let cohere_units = if backends.use_cohere() && !results.is_empty() {
        1.0
    } else {
        0.0
    };
    if cohere_units > 0.0 {
        ctx.store.record_usage_event(
            "cohere",
            "research_rerank",
            Some(backends.cohere_model.as_str()),
            if all_kbs { None } else { Some(&kb) },
            if diary == "*" { None } else { Some(&diary) },
            1,
            rerank_candidates as i64,
            0,
            0,
            cohere_units,
            cohere_rerank_cost_usd(cohere_units),
            json!({ "top_k": top_k, "candidate_budget": rerank_candidates }),
        )?;
    }

    let status = if results.is_empty() {
        "no_results"
    } else {
        "success"
    };
    let cited = results.iter().filter(|r| !r.citations.is_empty()).count();
    let answer_context = render_answer_context(&results);
    let results_json = results
        .iter()
        .map(|r| {
            json!({
                "id": r.id.to_string(),
                // The id a mutator accepts, and the chain it came down.
                "memory_id": r.provenance.memory_id.map(|m| m.to_string()),
                "handle": read_surface::hit_handle(&r.kind, r.id),
                "provenance": r.provenance.to_json(),
                "kind": r.kind,
                "score": r.score,
                "rerank_score": r.rerank_score,
                "kb": r.kb,
                "content": r.content,
                "citations": r.citations.iter().map(|c| json!({
                    "document_id": c.document_id.map(|id| id.to_string()),
                    "chunk_id": c.chunk_id.map(|id| id.to_string()),
                    "page": c.page,
                    "section": c.section,
                    "source": c.source,
                })).collect::<Vec<_>>(),
                "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("status", status);
    meta.add("profile", profile_enum.as_str());
    if let Some(note) = backends.embedder.degradation_note() {
        meta.add("embedder_degraded", note.clone());
        eprintln!("warning: {note}");
    }
    meta.add("kb", if all_kbs { "*" } else { kb.as_str() });
    meta.add("results", results_json.len());
    meta.add("cited_results", cited);
    meta.command_alias("research");
    let searched_kbs = if all_kbs {
        if stats.kbs_searched.is_empty() {
            vec!["*".to_string()]
        } else {
            stats.kbs_searched.clone()
        }
    } else {
        vec![kb.clone()]
    };
    Scope::retrieval(
        searched_kbs.clone(),
        if diary == "*" { "*" } else { diary.as_str() },
        if all_kbs { "all_kbs" } else { "flag" },
        stats.chunks_considered,
    )
    .attach(&mut meta);
    // Same rule as `recall`: one boolean decides both `status` and `verdict`,
    // and an empty evidence pass says what it looked at rather than shrugging.
    let verdict = if results_json.is_empty() {
        let shape = crate::commands::recall::request_shape(&ctx.store, &diary, false)?;
        Verdict::NoMatch(read_surface::absence_from(&stats, searched_kbs, shape))
    } else {
        Verdict::Results
    };

    let Value::Object(mut data) = json!({
            "status": status,
            // Ranked, not paged: no cursor here either (see recall).
            "returned": results_json.len(),
            "candidates_fused": stats.candidates_fused,
            "query": query,
            "query_plan": {
                "mode": "explore",
                "dense_search": true,
                "lexical_query": build_fts_query(&query),
                "entity_alias_search": true,
                "claim_search": true,
                "graph_hops": 2,
                "rerank_top_n": rerank_candidates,
                "citation_policy": "prefer cited claims/source spans; treat uncited graph/entity hits as leads, not final evidence"
            },
            "answer_context": answer_context,
            "results": results_json,
            "verification": {
                "results_returned": results.len(),
                "cited_results": cited,
                "uncited_results": results.len().saturating_sub(cited),
                "ready_for_synthesis": cited > 0,
            },
            "metadata": {
                "profile": profile_enum.as_str(),
                "embed_model": backends.embed_model,
                "embed_dims": backends.embed_dims,
                "reranker": backends.reranker_label,
                "candidates_considered": rerank_candidates,
            }
    }) else {
        unreachable!("json! object literal");
    };
    verdict.attach(&mut data);

    print_success(ctx.format, Value::Object(data), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}
