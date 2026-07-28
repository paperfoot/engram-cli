//! `engram serve` — local HTTP API over the same storage/retrieval/compiler code.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::OutputFormat;
use crate::retrieval::{
    hybrid_recall, render_answer_context, resolve_kb_scope, resolve_retrieval_backends, Filters,
    HybridParams, RecallMode, RetrievalProfile,
};
use chrono::{DateTime, Utc};
use engram_core::layers::approx_tokens;
use engram_core::temporal::{parse_bound, DayBound};
use engram_core::types::Layer;
use engram_storage::{paths, SqliteStore};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub async fn run(
    _ctx: &AppContext,
    host: String,
    port: u16,
    token: Option<String>,
) -> Result<(), CliError> {
    if !is_local_host(&host) && token.is_none() {
        return Err(CliError::Config(
            "binding engram serve outside localhost requires --token".into(),
        ));
    }
    let listener = TcpListener::bind((host.as_str(), port)).await?;
    eprintln!("engramd listening on http://{host}:{port}");
    loop {
        let (stream, _) = listener.accept().await?;
        if let Err(e) = handle_stream(stream, token.clone()).await {
            eprintln!("engramd request error: {e}");
        }
    }
}

async fn handle_stream(mut stream: TcpStream, token: Option<String>) -> Result<(), CliError> {
    let mut buf = vec![0u8; 64 * 1024];
    let n = stream.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let (head, body) = split_request(&req);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        write_json(
            &mut stream,
            400,
            json!({
                "version": "1",
                "status": "error",
                "error": {
                    "code": "bad_request",
                    "message": "bad request",
                    "suggestion": "Send a valid HTTP request line.",
                    "exit_code": 3
                }
            }),
        )
        .await?;
        return Ok(());
    }
    let method = parts[0];
    let (path, query) = split_path_query(parts[1]);
    if token.is_some() && !authorized(head, token.as_deref().unwrap()) {
        write_json(
            &mut stream,
            401,
            json!({
                "version": "1",
                "status": "error",
                "error": {
                    "code": "unauthorized",
                    "message": "unauthorized",
                    "suggestion": "Pass Authorization: Bearer <token>.",
                    "exit_code": 2
                }
            }),
        )
        .await?;
        return Ok(());
    }

    let response = route(method, path, query, body).await;
    match response {
        Ok(payload) => write_json(&mut stream, 200, payload).await?,
        Err(e) => {
            let status = match e.exit_code() {
                2 | 3 => 400,
                4 => 429,
                _ => 500,
            };
            let status = if e.code() == "serde_error" {
                400
            } else {
                status
            };
            write_json(
                &mut stream,
                status,
                json!({
                    "version": "1",
                    "status": "error",
                    "error": {
                        "code": e.code(),
                        "message": e.to_string(),
                        "suggestion": e.suggestion(),
                        "exit_code": if e.code() == "serde_error" { 3 } else { e.exit_code() }
                    }
                }),
            )
            .await?
        }
    }
    Ok(())
}

async fn route(
    method: &str,
    path: &str,
    query: &str,
    body: &str,
) -> Result<serde_json::Value, CliError> {
    let store = SqliteStore::open(paths::db_path())?;
    match (method, path) {
        ("GET", "/health") => Ok(json!({
            "version": "1",
            "status": "success",
            "data": { "ok": true, "schema_version": store.schema_version()? }
        })),
        ("GET", "/v1/kbs") => Ok(json!({
            "version": "1",
            "status": "success",
            "data": { "knowledge_bases": store.list_kbs()? }
        })),
        ("POST", "/v1/kbs") => {
            let req: KbCreateRequest = serde_json::from_str(body)?;
            store.ensure_kb(&req.name, req.description.as_deref())?;
            Ok(json!({ "version": "1", "status": "success", "data": { "name": req.name }}))
        }
        ("POST", "/v1/recall") => recall_api(store, body).await,
        ("POST", "/v1/ingest") => ingest_api(body).await,
        ("POST", "/v1/compile") => {
            let req: CompileRequest = serde_json::from_str(body)?;
            let ctx = AppContext {
                format: OutputFormat::Json,
                quiet: true,
                store,
            };
            let job_id = ctx.store.create_compile_job(
                &req.kb,
                "evidence",
                json!({
                    "api": true,
                    "llm": req.llm.unwrap_or(false),
                    "extraction_model": req.extraction_model,
                    "synthesis_model": req.synthesis_model,
                    "max_llm_chunks": req.max_llm_chunks,
                }),
            )?;
            let options = crate::commands::compile::CompileOptions {
                llm: req.llm.unwrap_or(false),
                extraction_model: req.extraction_model.unwrap_or_else(|| {
                    crate::commands::config::resolve_setting(
                        "ENGRAM_COMPILER_EXTRACTION_MODEL",
                        "compiler.extraction_model",
                        engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL,
                    )
                }),
                synthesis_model: req.synthesis_model.unwrap_or_else(|| {
                    crate::commands::config::resolve_setting(
                        "ENGRAM_COMPILER_SYNTHESIS_MODEL",
                        "compiler.synthesis_model",
                        engram_llm::openrouter::DEFAULT_SYNTHESIS_MODEL,
                    )
                }),
                max_llm_chunks: req.max_llm_chunks,
            };
            let stats =
                match crate::commands::compile::compile_kb_with_options(&ctx, &req.kb, options)
                    .await
                {
                    Ok(stats) => {
                        ctx.store.finish_compile_job(
                            job_id,
                            "completed",
                            None,
                            json!({ "stats": stats }),
                        )?;
                        stats
                    }
                    Err(err) => {
                        // The compile error is the one the caller needs; a
                        // job row that is no longer 'running' must not
                        // replace it.
                        if let Err(bookkeeping) = ctx.store.finish_compile_job(
                            job_id,
                            "failed",
                            Some(&err.to_string()),
                            json!({}),
                        ) {
                            tracing::warn!(
                                "could not mark compile job {job_id} failed: {bookkeeping}"
                            );
                        }
                        return Err(err);
                    }
                };
            Ok(
                json!({ "version": "1", "status": "success", "data": { "job_id": job_id, "compile": stats }}),
            )
        }
        ("POST", "/v1/reindex") => {
            let req: ReindexRequest = serde_json::from_str(body)?;
            let stats = crate::commands::reindex::reindex_store(
                &store,
                req.kb.as_deref(),
                req.all.unwrap_or(false),
                req.offline.unwrap_or(false),
            )
            .await?;
            Ok(
                json!({ "version": "1", "status": "success", "data": { "job_id": null, "reindex": stats }}),
            )
        }
        ("GET", "/v1/entities") => Ok(json!({
            "version": "1",
            "status": "success",
            "data": {
                "entities": store.list_entities(query_param(query, "kb").as_deref(), query_param_usize(query, "limit").unwrap_or(100), 1)?
            }
        })),
        ("GET", "/v1/documents") => Ok(json!({
            "version": "1",
            "status": "success",
            "data": {
                "documents": store.list_documents(query_param(query, "kb").as_deref(), query_param_usize(query, "limit").unwrap_or(100))?
            }
        })),
        _ if method == "GET" && path.starts_with("/v1/documents/") => {
            let id = parse_uuid_path(path.trim_start_matches("/v1/documents/"))?;
            Ok(json!({
                "version": "1",
                "status": "success",
                "data": { "document": store.get_document(id)? }
            }))
        }
        _ if method == "DELETE" && path.starts_with("/v1/documents/") => {
            if query_param(query, "confirm").as_deref() != Some("true") {
                return Err(CliError::BadInput(
                    "DELETE /v1/documents/{id} requires ?confirm=true".into(),
                ));
            }
            let id = parse_uuid_path(path.trim_start_matches("/v1/documents/"))?;
            Ok(json!({
                "version": "1",
                "status": "success",
                "data": { "document_id": id, "deleted": store.delete_document(id)? }
            }))
        }
        ("GET", "/v1/jobs") => Ok(json!({
            "version": "1",
            "status": "success",
            "data": {
                "jobs": store.list_compile_jobs(query_param(query, "kb").as_deref(), query_param_usize(query, "limit").unwrap_or(50))?
            }
        })),
        _ if method == "GET" && path.starts_with("/v1/entities/") => {
            let name = path.trim_start_matches("/v1/entities/");
            Ok(json!({
                "version": "1",
                "status": "success",
                "data": { "entity": store.find_entity(query_param(query, "kb").as_deref(), name)? }
            }))
        }
        _ if method == "GET" && path.starts_with("/v1/jobs/") => {
            let id = parse_uuid_path(path.trim_start_matches("/v1/jobs/"))?;
            Ok(json!({
                "version": "1",
                "status": "success",
                "data": { "job": store.get_compile_job(id)? }
            }))
        }
        ("GET", "/v1/usage") => Ok(json!({
            "version": "1",
            "status": "success",
            "data": {
                "summary": store.usage_summary(query_param(query, "kb").as_deref(), query_param(query, "since").as_deref())?
            }
        })),
        ("GET", "/v1/budget") => {
            let kb = query_param(query, "kb");
            let scope = kb
                .as_ref()
                .map(|k| format!("kb:{k}"))
                .unwrap_or_else(|| "global".to_string());
            Ok(json!({
                "version": "1",
                "status": "success",
                "data": { "scope": scope, "budget": store.get_usage_budget(&scope)? }
            }))
        }
        ("POST", "/v1/budget") => {
            let req: BudgetRequest = serde_json::from_str(body)?;
            let scope = req
                .kb
                .as_ref()
                .map(|k| format!("kb:{k}"))
                .unwrap_or_else(|| "global".to_string());
            store.upsert_usage_budget(&scope, req.kb.as_deref(), req.daily_usd, req.monthly_usd)?;
            Ok(json!({
                "version": "1",
                "status": "success",
                "data": { "scope": scope, "budget": store.get_usage_budget(&scope)? }
            }))
        }
        ("POST", "/v1/research") => research_api(store, body).await,
        _ => Err(CliError::BadInput(format!(
            "unknown endpoint: {method} {path}"
        ))),
    }
}

/// Daemon diary semantics, CLI parity: an omitted `diary` means the diary
/// literally named "default" — exactly what `engram recall` does when no
/// --diary flag is given — and `"*"` is the explicit way to search every
/// diary. Mapping omission to ALL diaries let a minimal `/v1/recall` request
/// return specialist-diary content the equivalent CLI invocation excludes.
fn daemon_diary(diary: Option<String>) -> Option<String> {
    match diary.as_deref() {
        None => Some("default".to_string()),
        Some("*") => None,
        Some(_) => diary,
    }
}

async fn research_api(store: SqliteStore, body: &str) -> Result<serde_json::Value, CliError> {
    let req: ResearchRequest = serde_json::from_str(body)?;
    // An empty query is refused BEFORE any provider or budget work, exactly as
    // `engram research ""` is (exit 3 -> HTTP 400). It used to fall through to
    // hybrid_recall, which embedded the empty string: a paid Gemini call, a
    // recorded usage event and a budget decremented for a query that cannot
    // match anything. With no key it was reported as a config error instead,
    // which sends the caller to fix their keys over a typo.
    if req.query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }
    let profile = RetrievalProfile::parse(req.profile.as_deref().unwrap_or("cloud_quality"))?;
    let kb_filter = req.kb.clone().filter(|k| k != "*");
    let rerank_candidates = req
        .rerank_top_n
        .unwrap_or_else(|| profile.default_rerank_top_n());
    // The daemon is a retrieval surface like any other: a cloud profile with no
    // key is a config error here too, and the caller's budget is checked before
    // the paid call. It used to do neither and answer from the stub instead.
    let backends = resolve_retrieval_backends(&store, profile, kb_filter.as_deref())?;
    let params = HybridParams {
        query: &req.query,
        top_k: req.top_k.unwrap_or(12),
        rrf_k: 60.0,
        rerank_top_n: rerank_candidates,
        filters: Filters {
            diary: daemon_diary(req.diary.clone()),
            kb: kb_filter.clone(),
            since: None,
            until: None,
        },
        mode: RecallMode::Explore,
        graph_hops: 2,
        allow_mixed_embeddings: req.allow_mixed_embeddings.unwrap_or(false),
    };
    let outcome = hybrid_recall(
        &store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;
    let (results, notes) = (outcome.results, outcome.notes);
    // A budget is a guardrail over RECORDED usage, so a surface that spends
    // must record: this endpoint paid Gemini and told the ledger nothing, so
    // daemon research traffic could never move a cap it now enforces.
    if backends.cloud_embedder {
        let tokens = estimated_tokens(&req.query);
        store.record_usage_event(
            "gemini",
            "api_research_query_embed",
            Some(backends.embed_model.as_str()),
            kb_filter.as_deref(),
            daemon_diary(req.diary.clone()).as_deref(),
            1,
            1,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({ "estimated": true, "api": true }),
        )?;
    }
    if backends.use_cohere() && !results.is_empty() {
        let search_units = 1.0;
        store.record_usage_event(
            "cohere",
            "api_research_rerank",
            Some(backends.cohere_model.as_str()),
            kb_filter.as_deref(),
            daemon_diary(req.diary.clone()).as_deref(),
            1,
            rerank_candidates as i64,
            0,
            0,
            search_units,
            cohere_rerank_cost_usd(search_units),
            json!({ "api": true }),
        )?;
    }
    let status = if results.is_empty() {
        "no_results"
    } else {
        "success"
    };
    Ok(json!({
        "version": "1",
        "status": status,
        "data": {
            "status": status,
            "query": req.query,
            "answer_context": render_answer_context(&results),
            "results": results.iter().map(|r| json!({
                "id": r.id.to_string(),
                "kind": r.kind,
                "score": r.score,
                "rerank_score": r.rerank_score,
                "kb": r.kb,
                "content": r.content,
                "citations": r.citations,
                "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "metadata": {
                "profile": profile.as_str(),
                // A caller cannot tell 1536-dim Gemini retrieval from the
                // 64-dim offline stub out of `profile` alone: `offline` and a
                // forced stub render the same word. The CLI's `research` has
                // reported these since Wave 0; the daemon copy did not.
                "embed_model": backends.embed_model,
                "embed_dims": backends.embed_dims,
                "prompt_format": backends.prompt_format,
                "reranker": backends.reranker_label,
                "candidates_considered": rerank_candidates,
                "diary_isolation": notes.diary_isolation_json(),
                "temporal_scope": notes.temporal_scope_json()
            }
        }
    }))
}

async fn recall_api(store: SqliteStore, body: &str) -> Result<serde_json::Value, CliError> {
    let req: RecallRequest = serde_json::from_str(body)?;
    // An empty query is refused BEFORE any provider or budget work, exactly as
    // `engram recall ""` is (exit 3 -> HTTP 400). It used to fall through to
    // hybrid_recall, which embedded the empty string: a paid Gemini call, a
    // recorded usage event and a budget decremented for a query that cannot
    // match anything. With no key it was reported as a config error instead,
    // which sends the caller to fix their keys over a typo.
    if req.query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }
    // Same scope precedence as `engram recall`: explicit kb > retrieval.default_kb
    // > every KB. An omitted `kb` searches everything rather than the single
    // namespace literally named "default".
    let scope = resolve_kb_scope(req.kb.as_deref(), req.all_kbs.unwrap_or(false));
    let req_diary = req.diary.clone();
    let profile = RetrievalProfile::parse(req.profile.as_deref().unwrap_or("cloud_quality"))?;
    let mode = RecallMode::parse(req.mode.as_deref().unwrap_or("evidence"))?;
    // The clap defaults of `engram recall`, so the same request spelled over
    // HTTP answers the same way (cli.rs: --layer topic, --top-k 10).
    let layer = parse_layer(req.layer.as_deref().unwrap_or("topic"))?;
    let since = parse_bound_param(req.since.as_deref(), DayBound::Start)?;
    let until = parse_bound_param(req.until.as_deref(), DayBound::End)?;
    let top_k = req.top_k.unwrap_or(10);
    let rerank_candidates = req
        .rerank_top_n
        .unwrap_or_else(|| profile.default_rerank_top_n());
    // Same provider + budget rule as `engram recall` (see
    // `resolve_retrieval_backends`): no key under a cloud profile is a config
    // error, not stub results dressed up as gemini ones. The embedder is also
    // asked what it is rather than being hardcoded here.
    let backends = resolve_retrieval_backends(&store, profile, scope.as_deref())?;
    let params = HybridParams {
        query: &req.query,
        top_k,
        rrf_k: 60.0,
        rerank_top_n: rerank_candidates,
        filters: Filters {
            diary: daemon_diary(req_diary.clone()),
            kb: scope.kb.clone(),
            since,
            until,
        },
        mode,
        graph_hops: req.graph_hops.unwrap_or(1),
        allow_mixed_embeddings: req.allow_mixed_embeddings.unwrap_or(false),
    };
    let outcome = hybrid_recall(
        &store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;
    let (results, notes) = (outcome.results, outcome.notes);
    // Same layer contract as `engram recall`: the token budget only trims what
    // exceeds the caller's own top_k, so a layer can never return FEWER results
    // than were asked for. Reported so a caller can see which budget applied.
    let budget = layer.default_token_budget();
    let mut used_tokens = 0usize;
    let mut layered = Vec::new();
    for r in &results {
        let cost = approx_tokens(&r.content);
        if layered.len() >= top_k && used_tokens + cost > budget {
            break;
        }
        layered.push(r);
        used_tokens += cost;
    }
    if backends.cloud_embedder {
        let tokens = estimated_tokens(&req.query);
        store.record_usage_event(
            "gemini",
            "api_recall_query_embed",
            Some(backends.embed_model.as_str()),
            scope.as_deref(),
            daemon_diary(req_diary.clone()).as_deref(),
            1,
            1,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({ "estimated": true, "api": true }),
        )?;
    }
    if backends.use_cohere() && !results.is_empty() {
        let search_units = 1.0;
        store.record_usage_event(
            "cohere",
            "api_recall_rerank",
            Some(backends.cohere_model.as_str()),
            scope.as_deref(),
            daemon_diary(req_diary.clone()).as_deref(),
            1,
            rerank_candidates as i64,
            0,
            0,
            search_units,
            cohere_rerank_cost_usd(search_units),
            json!({ "api": true }),
        )?;
    }
    let status = if results.is_empty() {
        "no_results"
    } else {
        "success"
    };
    Ok(json!({
        "version": "1",
        "status": status,
        "data": {
            "status": status,
            // `layered` is a Vec<&HybridResult>; `.iter()` would hand the
            // renderer `&&HybridResult`, so copy the references out.
            "answer_context": render_answer_context(layered.iter().copied()),
            "results": layered.iter().map(|r| json!({
                "id": r.id.to_string(),
                "kind": r.kind,
                "score": r.score,
                "rerank_score": r.rerank_score,
                "kb": r.kb,
                "content": r.content,
                "citations": r.citations,
                "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "metadata": {
                "profile": profile.as_str(),
                "embed_model": backends.embed_model,
                "embed_dims": backends.embed_dims,
                "prompt_format": backends.prompt_format,
                "reranker": backends.reranker_label,
                "candidates_considered": rerank_candidates,
                "layer": format!("{:?}", layer).to_lowercase(),
                "token_budget": budget,
                "tokens_used": used_tokens,
                "kb": scope.label(),
                "kb_scope": scope.origin.as_str(),
                "diary_isolation": notes.diary_isolation_json(),
                "temporal_scope": notes.temporal_scope_json()
            }
        }
    }))
}

async fn ingest_api(body: &str) -> Result<serde_json::Value, CliError> {
    let req: IngestRequest = serde_json::from_str(body)?;
    let mut exe = std::env::current_exe()?;
    if exe
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "engramd")
        .unwrap_or(false)
    {
        exe.set_file_name("engram");
    }
    let mut command = std::process::Command::new(exe);
    command
        .arg("--json")
        .arg("ingest")
        .arg(req.path)
        .arg("--kb")
        .arg(req.kb.unwrap_or_else(|| "default".to_string()))
        .arg("--mode")
        .arg(req.mode.unwrap_or_else(|| "auto".to_string()))
        .arg("--diary")
        .arg(req.diary.unwrap_or_else(|| "default".to_string()))
        .arg("--compile")
        .arg(req.compile.unwrap_or_else(|| "none".to_string()));
    if req.dry_run.unwrap_or(false) {
        command.arg("--dry-run");
    }
    if req.all.unwrap_or(false) {
        command.arg("--all");
    }
    if let Some(max_files) = req.max_files {
        command.arg("--max-files").arg(max_files.to_string());
    }
    for pattern in req.include.unwrap_or_default() {
        command.arg("--include").arg(pattern);
    }
    for pattern in req.exclude.unwrap_or_default() {
        command.arg("--exclude").arg(pattern);
    }
    let output = command.output()?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(CliError::Transient(format!(
            "ingest subprocess failed: {err}"
        )));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(json!({
        "version": "1",
        "status": "success",
        "data": {
            "job_id": null,
            "ingest": parsed.get("data").cloned().unwrap_or(parsed)
        }
    }))
}

async fn write_json(
    stream: &mut TcpStream,
    status: u16,
    payload: serde_json::Value,
) -> Result<(), CliError> {
    let body = serde_json::to_string(&payload)?;
    let reason = if status == 200 { "OK" } else { "ERROR" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

fn split_request(req: &str) -> (&str, &str) {
    if let Some((head, body)) = req.split_once("\r\n\r\n") {
        (head, body)
    } else {
        (req, "")
    }
}

fn split_path_query(path: &str) -> (&str, &str) {
    path.split_once('?').unwrap_or((path, ""))
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.replace("%20", " "))
    })
}

fn query_param_usize(query: &str, key: &str) -> Option<usize> {
    query_param(query, key).and_then(|v| v.parse().ok())
}

fn parse_uuid_path(id: &str) -> Result<uuid::Uuid, CliError> {
    uuid::Uuid::parse_str(id).map_err(|_| CliError::BadInput(format!("invalid UUID: {id}")))
}

fn authorized(head: &str, token: &str) -> bool {
    head.lines().any(|line| {
        line.to_ascii_lowercase().starts_with("authorization:")
            && line.trim_end().ends_with(&format!("Bearer {token}"))
    })
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// `recall --layer`, over HTTP.
///
/// Duplicated rather than shared out of `commands::recall`: that module belongs
/// to another lane this wave, and a six-arm match is cheaper than a cross-file
/// conflict. The error string is byte-identical to `recall.rs` on purpose —
/// one rule, two entry points — so folding the two together is a pure delete.
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

/// `recall --since/--until`, over HTTP: RFC3339, or a bare `YYYY-MM-DD` that
/// means the START of that UTC day for `since` and its END for `until`, so
/// `since == until == D` selects exactly day D.
///
/// Unparseable input is a hard error, never a silent "now" and never a silently
/// ignored field — the daemon used to accept `since` and drop it on the floor,
/// so a windowed request was answered from the whole store and the caller was
/// never told.
fn parse_bound_param(
    raw: Option<&str>,
    bound: DayBound,
) -> Result<Option<DateTime<Utc>>, CliError> {
    match raw {
        None => Ok(None),
        Some(v) => parse_bound(v, bound).map(Some).ok_or_else(|| {
            CliError::BadInput(format!(
                "invalid timestamp {v}: expected RFC3339 (2026-06-01T09:30:00Z) or YYYY-MM-DD"
            ))
        }),
    }
}

#[derive(Debug, Deserialize)]
struct KbCreateRequest {
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompileRequest {
    kb: String,
    llm: Option<bool>,
    extraction_model: Option<String>,
    synthesis_model: Option<String>,
    max_llm_chunks: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ReindexRequest {
    kb: Option<String>,
    all: Option<bool>,
    /// Explicit stub opt-in, mirroring the CLI --offline flag; keyless
    /// reindex without it is a config error, never a silent stub rebuild.
    offline: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct IngestRequest {
    path: String,
    kb: Option<String>,
    mode: Option<String>,
    diary: Option<String>,
    compile: Option<String>,
    dry_run: Option<bool>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    max_files: Option<usize>,
    all: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RecallRequest {
    query: String,
    kb: Option<String>,
    all_kbs: Option<bool>,
    diary: Option<String>,
    mode: Option<String>,
    profile: Option<String>,
    layer: Option<String>,
    top_k: Option<usize>,
    rerank_top_n: Option<usize>,
    graph_hops: Option<u8>,
    allow_mixed_embeddings: Option<bool>,
    since: Option<String>,
    until: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResearchRequest {
    query: String,
    kb: Option<String>,
    diary: Option<String>,
    profile: Option<String>,
    top_k: Option<usize>,
    rerank_top_n: Option<usize>,
    allow_mixed_embeddings: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BudgetRequest {
    kb: Option<String>,
    daily_usd: Option<f64>,
    monthly_usd: Option<f64>,
}
