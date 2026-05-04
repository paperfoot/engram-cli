//! `engram serve` — local HTTP API over the same storage/retrieval/compiler code.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::OutputFormat;
use crate::retrieval::{hybrid_recall, Filters, HybridParams, RecallMode, RetrievalProfile};
use engram_embed::gemini::{GeminiEmbedder, DEFAULT_DIMS, DEFAULT_MODEL, PROMPT_FORMAT};
use engram_embed::stub::StubEmbedder;
use engram_rerank::cohere::CohereReranker;
use engram_rerank::passthrough::PassthroughReranker;
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
    if token.is_some() && !authorized(&head, token.as_deref().unwrap()) {
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
                        ctx.store.finish_compile_job(
                            job_id,
                            "failed",
                            Some(&err.to_string()),
                            json!({}),
                        )?;
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

async fn research_api(store: SqliteStore, body: &str) -> Result<serde_json::Value, CliError> {
    let req: ResearchRequest = serde_json::from_str(body)?;
    let profile = RetrievalProfile::parse(req.profile.as_deref().unwrap_or("cloud_quality"))?;
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let use_cloud = profile != RetrievalProfile::Offline && gemini_key.is_some() && !force_stub;
    let use_cohere = profile != RetrievalProfile::Offline && cohere_key.is_some() && !force_stub;
    let rerank_candidates = req
        .rerank_top_n
        .unwrap_or_else(|| profile.default_rerank_top_n());
    let params = HybridParams {
        query: &req.query,
        top_k: req.top_k.unwrap_or(12),
        rrf_k: 60.0,
        rerank_top_n: rerank_candidates,
        filters: Filters {
            diary: req.diary.clone().filter(|d| d != "*"),
            kb: req.kb.clone().filter(|k| k != "*"),
            valid_at: None,
        },
        mode: RecallMode::Explore,
        graph_hops: 2,
        allow_mixed_embeddings: req.allow_mixed_embeddings.unwrap_or(false),
    };
    let results = if use_cloud {
        let embedder = GeminiEmbedder::new(gemini_key.unwrap());
        if use_cohere {
            let reranker = CohereReranker::new(cohere_key.unwrap());
            hybrid_recall(&store, &embedder, Some(&reranker), params).await?
        } else {
            let reranker: Option<&PassthroughReranker> = None;
            hybrid_recall(&store, &embedder, reranker, params).await?
        }
    } else {
        let embedder = StubEmbedder::default();
        let reranker: Option<&PassthroughReranker> = None;
        hybrid_recall(&store, &embedder, reranker, params).await?
    };
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
            "answer_context": results.iter().map(|r| r.content.as_str()).collect::<Vec<_>>().join("\n\n"),
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
                "reranker": if use_cohere { "cohere/rerank-v3.5" } else { "passthrough" },
                "candidates_considered": rerank_candidates
            }
        }
    }))
}

async fn recall_api(store: SqliteStore, body: &str) -> Result<serde_json::Value, CliError> {
    let req: RecallRequest = serde_json::from_str(body)?;
    let req_kb = req.kb.clone();
    let req_diary = req.diary.clone();
    let profile = RetrievalProfile::parse(req.profile.as_deref().unwrap_or("cloud_quality"))?;
    let mode = RecallMode::parse(req.mode.as_deref().unwrap_or("evidence"))?;
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let use_cloud = profile != RetrievalProfile::Offline && gemini_key.is_some() && !force_stub;
    let use_cohere = profile != RetrievalProfile::Offline && cohere_key.is_some() && !force_stub;
    let rerank_candidates = req
        .rerank_top_n
        .unwrap_or_else(|| profile.default_rerank_top_n());
    let (embed_model, embed_dims, prompt_format) = if use_cloud {
        (
            DEFAULT_MODEL.to_string(),
            DEFAULT_DIMS,
            PROMPT_FORMAT.to_string(),
        )
    } else {
        ("stub-64".to_string(), 64, "stub-v1".to_string())
    };
    let params = HybridParams {
        query: &req.query,
        top_k: req.top_k.unwrap_or(10),
        rrf_k: 60.0,
        rerank_top_n: rerank_candidates,
        filters: Filters {
            diary: req_diary.clone().filter(|d| d != "*"),
            kb: req_kb.clone().filter(|k| k != "*"),
            valid_at: None,
        },
        mode,
        graph_hops: req.graph_hops.unwrap_or(1),
        allow_mixed_embeddings: req.allow_mixed_embeddings.unwrap_or(false),
    };
    let results = if use_cloud {
        let embedder = GeminiEmbedder::new(gemini_key.unwrap());
        if use_cohere {
            let reranker = CohereReranker::new(cohere_key.unwrap());
            hybrid_recall(&store, &embedder, Some(&reranker), params).await?
        } else {
            let reranker: Option<&PassthroughReranker> = None;
            hybrid_recall(&store, &embedder, reranker, params).await?
        }
    } else {
        let embedder = StubEmbedder::default();
        let reranker: Option<&PassthroughReranker> = None;
        hybrid_recall(&store, &embedder, reranker, params).await?
    };
    if use_cloud {
        let tokens = estimated_tokens(&req.query);
        store.record_usage_event(
            "gemini",
            "api_recall_query_embed",
            Some(engram_embed::gemini::DEFAULT_MODEL),
            req_kb.as_deref().filter(|k| *k != "*"),
            req_diary.as_deref().filter(|d| *d != "*"),
            1,
            1,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({ "estimated": true, "api": true }),
        )?;
    }
    if use_cohere && !results.is_empty() {
        let search_units = 1.0;
        store.record_usage_event(
            "cohere",
            "api_recall_rerank",
            Some("rerank-v3.5"),
            req_kb.as_deref().filter(|k| *k != "*"),
            req_diary.as_deref().filter(|d| *d != "*"),
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
            "answer_context": results.iter().map(|r| r.content.as_str()).collect::<Vec<_>>().join("\n\n"),
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
                "embed_model": embed_model,
                "embed_dims": embed_dims,
                "prompt_format": prompt_format,
                "reranker": if use_cohere { "cohere/rerank-v3.5" } else { "passthrough" },
                "candidates_considered": rerank_candidates
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
    diary: Option<String>,
    mode: Option<String>,
    profile: Option<String>,
    top_k: Option<usize>,
    rerank_top_n: Option<usize>,
    graph_hops: Option<u8>,
    allow_mixed_embeddings: Option<bool>,
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
