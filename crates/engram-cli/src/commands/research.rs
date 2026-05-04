//! `engram research` — agentic, evidence-first retrieval plan over a KB.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use crate::retrieval::{
    build_fts_query, hybrid_recall, Filters, HybridParams, RecallMode, RetrievalProfile,
};
use engram_embed::gemini::GeminiEmbedder;
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
    let rerank_candidates = profile_enum.default_rerank_top_n();
    let filters = Filters {
        diary: if diary == "*" {
            None
        } else {
            Some(diary.clone())
        },
        kb: if all_kbs { None } else { Some(kb.clone()) },
        valid_at: None,
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

    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let use_cloud =
        profile_enum != RetrievalProfile::Offline && gemini_key.is_some() && !force_stub;
    let use_cohere =
        profile_enum != RetrievalProfile::Offline && cohere_key.is_some() && !force_stub;

    let (results, embed_model, embed_dims, prompt_format) = if use_cloud {
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

    let query_tokens = estimated_tokens(&query);
    let gemini_cost = if use_cloud {
        gemini_embed_cost_usd(query_tokens)
    } else {
        0.0
    };
    if use_cloud {
        ctx.store.record_usage_event(
            "gemini",
            "research_query_embed",
            Some(&embed_model),
            if all_kbs { None } else { Some(&kb) },
            if diary == "*" { None } else { Some(&diary) },
            1,
            1,
            query_tokens,
            0,
            0.0,
            gemini_cost,
            json!({ "dimensions": embed_dims, "prompt_format": prompt_format, "estimated": true }),
        )?;
    }
    let cohere_units = if use_cohere && !results.is_empty() {
        1.0
    } else {
        0.0
    };
    if cohere_units > 0.0 {
        ctx.store.record_usage_event(
            "cohere",
            "research_rerank",
            Some("rerank-v3.5"),
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
    let answer_context = results
        .iter()
        .map(|r| format!("[{}:{}]\n{}", r.kind, r.id, r.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    let results_json = results
        .iter()
        .map(|r| {
            json!({
                "id": r.id.to_string(),
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

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("status", status);
    meta.add("profile", profile_enum.as_str());
    meta.add("kb", if all_kbs { "*" } else { kb.as_str() });
    meta.add("results", results_json.len());
    meta.add("cited_results", cited);
    print_success(
        ctx.format,
        json!({
            "status": status,
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
                "embed_model": embed_model,
                "embed_dims": embed_dims,
                "reranker": if use_cohere { "cohere/rerank-v3.5" } else { "passthrough" },
                "candidates_considered": rerank_candidates,
            }
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
