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

/// Flattened `bench` CLI arguments. Carrying these in one struct keeps the
/// dispatcher signature small (and clippy happy) now that several suites add
/// their own flags (`--mode`, `--kb`, `--corpus`, `--dataset`, `--rerank`,
/// `--year`); each suite reads only the fields it needs.
#[derive(Debug)]
pub struct BenchArgs {
    pub suite: String,
    pub mab_split: String,
    pub download: bool,
    pub limit: Option<usize>,
    pub answerer: String,
    pub judge: String,
    pub ragas: bool,
    pub top_k: usize,
    pub save: Option<std::path::PathBuf>,
    pub mode: String,
    pub kb: Option<String>,
    pub corpus: Option<std::path::PathBuf>,
    pub dataset: String,
    pub rerank: String,
    pub year: String,
    pub profile: String,
}

pub async fn run(ctx: &AppContext, args: BenchArgs) -> Result<(), CliError> {
    match args.suite.as_str() {
        "mini" => run_mini(ctx).await,
        "personal" => {
            run_personal(
                ctx,
                args.profile,
                top_k_or(args.top_k, PERSONAL_DEFAULT_TOP_K),
                args.save,
            )
            .await
        }
        "mini-fts" => run_mini_fts(ctx),
        "scientific-mini" | "science-mini" => run_scientific_mini(ctx).await,
        "longmemeval" => run_longmemeval(ctx, args.download, args.limit).await,
        "longmemeval-qa" | "lme-qa" => {
            run_longmemeval_qa(
                ctx,
                args.limit,
                args.answerer,
                args.judge,
                args.ragas,
                top_k_or(args.top_k, QA_DEFAULT_TOP_K),
                args.save,
            )
            .await
        }
        "locomo-plus" | "locomo-plus-qa" => {
            run_locomo_plus_qa(
                ctx,
                args.limit,
                args.answerer,
                args.judge,
                args.ragas,
                top_k_or(args.top_k, QA_DEFAULT_TOP_K),
                args.save,
            )
            .await
        }
        "memoryagentbench" | "mab" => {
            run_memoryagentbench_qa(
                ctx,
                args.mab_split,
                args.limit,
                args.answerer,
                args.judge,
                top_k_or(args.top_k, QA_DEFAULT_TOP_K),
                args.save,
            )
            .await
        }
        "litqa2" => {
            run_litqa2(
                ctx,
                args.mode,
                args.limit,
                args.kb,
                args.corpus,
                top_k_or(args.top_k, LITQA2_DEFAULT_TOP_K),
                args.answerer,
                args.judge,
                args.save,
            )
            .await
        }
        "beir" => {
            run_beir(
                ctx,
                args.dataset,
                args.rerank,
                top_k_or(args.top_k, BEIR_DEFAULT_TOP_K),
                args.save,
            )
            .await
        }
        "biogen" => {
            run_biogen(
                ctx,
                args.year,
                args.kb,
                args.corpus,
                top_k_or(args.top_k, QA_DEFAULT_TOP_K),
                args.answerer,
                args.judge,
                args.save,
            )
            .await
        }
        other => Err(CliError::BadInput(format!("unknown suite: {other}"))),
    }
}

/// Per-suite default retrieval/context pool size for the answerer QA suites.
const QA_DEFAULT_TOP_K: usize = 5;
/// Default top-k for LitQA2 full-text retrieval (PaperQA2 uses a wide pool).
const LITQA2_DEFAULT_TOP_K: usize = 30;
/// Default top-k for BEIR retrieval rows.
const BEIR_DEFAULT_TOP_K: usize = 30;
/// Default top-k for the personal suite — matches the `recall` default, so the
/// bench measures what an agent actually gets back.
const PERSONAL_DEFAULT_TOP_K: usize = 10;

/// Resolve the effective top-k: a `--top-k 0` (the "auto" sentinel) selects the
/// suite default; any explicit positive value is used verbatim.
fn top_k_or(value: usize, default: usize) -> usize {
    if value == 0 {
        default
    } else {
        value
    }
}

/// Build a retrieval corpus (doc-id -> full text) from either an ingested KB or a
/// corpus file. Exactly one of `kb` / `corpus` must be supplied.
///
/// - KB: chunks are read via [`SqliteStore::list_chunks_for_kb`], grouped by
///   their document, joined in position order, and keyed by the document's DOI
///   when present (so it lines up with LitQA2 `target_doi`), else by the
///   document title / id.
/// - file: a JSON object `{ "<id>": "<text>", ... }`, a JSON array of
///   `{ _id|id|doi, text|abstract|contents }` rows, or the same one-per-line as
///   JSONL (the BEIR `corpus.jsonl` shape).
fn build_corpus(
    ctx: &AppContext,
    kb: Option<&str>,
    corpus_path: Option<&std::path::Path>,
) -> Result<std::collections::HashMap<String, String>, CliError> {
    match (kb, corpus_path) {
        (Some(_), Some(_)) => Err(CliError::BadInput(
            "pass only one of --kb or --corpus, not both".into(),
        )),
        (None, None) => Err(CliError::BadInput(
            "full-text retrieval requires --kb <name> or --corpus <file>".into(),
        )),
        (Some(kb), None) => build_corpus_from_kb(ctx, kb),
        (None, Some(path)) => load_corpus_file(path),
    }
}

fn build_corpus_from_kb(
    ctx: &AppContext,
    kb: &str,
) -> Result<std::collections::HashMap<String, String>, CliError> {
    use std::collections::HashMap;
    // document_id -> stable corpus key (DOI when available, else title/id).
    let mut doc_key: HashMap<uuid::Uuid, String> = HashMap::new();
    for doc in ctx.store.list_documents(Some(kb), 1_000_000)? {
        let key = doc
            .doi
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| {
                if doc.title.trim().is_empty() {
                    doc.id.to_string()
                } else {
                    doc.title.clone()
                }
            });
        doc_key.insert(doc.id, key);
    }

    // Chunks come back ordered by (position, id); concatenate per corpus key.
    let mut corpus: HashMap<String, String> = HashMap::new();
    for chunk in ctx.store.list_chunks_for_kb(kb)? {
        let key = match chunk.document_id.and_then(|id| doc_key.get(&id).cloned()) {
            Some(k) => k,
            // Chunks with no document row fall back to their memory id so they
            // still contribute a (singleton) corpus entry rather than vanishing.
            None => chunk.memory_id.to_string(),
        };
        let entry = corpus.entry(key).or_default();
        if !entry.is_empty() {
            entry.push_str("\n\n");
        }
        entry.push_str(&chunk.content);
    }

    if corpus.is_empty() {
        return Err(CliError::Config(format!(
            "KB '{kb}' has no ingested chunks — ingest documents first or pass --corpus"
        )));
    }
    Ok(corpus)
}

fn load_corpus_file(
    path: &std::path::Path,
) -> Result<std::collections::HashMap<String, String>, CliError> {
    use std::collections::HashMap;
    if !path.exists() {
        return Err(CliError::Config(format!(
            "corpus file not found: {}",
            path.display()
        )));
    }
    let contents = std::fs::read_to_string(path)?;
    let trimmed = contents.trim_start();

    // Pull (id, text) out of a corpus row object under any of the common keys.
    fn row_to_pair(v: &serde_json::Value) -> Option<(String, String)> {
        let obj = v.as_object()?;
        let id = obj
            .get("_id")
            .or_else(|| obj.get("id"))
            .or_else(|| obj.get("doi"))
            .or_else(|| obj.get("doc_id"))
            .map(|x| match x {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })?;
        let title = obj.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let text = obj
            .get("text")
            .or_else(|| obj.get("abstract"))
            .or_else(|| obj.get("contents"))
            .or_else(|| obj.get("content"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let body = if title.trim().is_empty() {
            text.to_string()
        } else {
            format!("{title}\n{text}")
        };
        Some((id, body))
    }

    let mut corpus: HashMap<String, String> = HashMap::new();
    if trimmed.starts_with('{') {
        // Could be a flat {id: text} map OR a single corpus-row object.
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| CliError::Config(format!("failed to parse {}: {e}", path.display())))?;
        if let Some((id, body)) = row_to_pair(&value) {
            corpus.insert(id, body);
        } else if let Some(map) = value.as_object() {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    corpus.insert(k.clone(), s.to_string());
                }
            }
        }
    } else if trimmed.starts_with('[') {
        let rows: Vec<serde_json::Value> = serde_json::from_str(trimmed)
            .map_err(|e| CliError::Config(format!("failed to parse {}: {e}", path.display())))?;
        for v in &rows {
            if let Some((id, body)) = row_to_pair(v) {
                corpus.insert(id, body);
            }
        }
    } else {
        // JSONL: one corpus-row object per non-empty line.
        for (idx, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                CliError::Config(format!("{} line {}: {e}", path.display(), idx + 1))
            })?;
            if let Some((id, body)) = row_to_pair(&v) {
                corpus.insert(id, body);
            }
        }
    }

    if corpus.is_empty() {
        return Err(CliError::Config(format!(
            "corpus file {} produced 0 documents",
            path.display()
        )));
    }
    Ok(corpus)
}

async fn run_scientific_mini(ctx: &AppContext) -> Result<(), CliError> {
    use crate::retrieval::{hybrid_recall, Filters, HybridParams, RecallMode};
    use chrono::Utc;
    use engram_core::types::{Memory, MemorySource};
    use engram_embed::{Embedder, TaskMode};
    use engram_ingest::chunker::naive_split;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_storage::SqliteStore;
    use uuid::Uuid;

    let store = SqliteStore::open_in_memory()?;
    let fixture_ctx = AppContext {
        format: ctx.format,
        quiet: true,
        store,
    };
    let kb = "scientific-mini";
    fixture_ctx
        .store
        .ensure_kb(kb, Some("Tiny scientific evidence benchmark"))?;
    let embedder = StubEmbedder::default();

    let docs = [
        (
            "rapamycin-human",
            "Rapamycin inhibits mTORC1 signaling. In a human randomized trial, low-dose everolimus improved influenza vaccine response in older adults, but lifespan was not measured.",
        ),
        (
            "metformin-observational",
            "Metformin activates AMPK in liver and muscle. Observational studies in patients with type 2 diabetes report lower mortality, but confounding remains a major limitation.",
        ),
        (
            "hipporag",
            "HippoRAG uses graph-structured retrieval to improve multi-hop question answering by linking entities across passages before final context assembly.",
        ),
    ];

    for (title, text) in docs {
        let document_id = fixture_ctx.store.insert_document(
            kb,
            title,
            Some(title),
            "papers",
            serde_json::json!({}),
        )?;
        let memory = Memory {
            id: Uuid::new_v4(),
            content: text.to_string(),
            created_at: Utc::now(),
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Paper {
                doi: None,
                title: title.to_string(),
                section: None,
            },
            diary: "default".to_string(),
            valid_from: None,
            valid_until: None,
            tags: vec![title.to_string()],
        };
        fixture_ctx.store.insert_memory_with_kb(&memory, kb)?;
        let chunks = naive_split(text);
        let chunk_texts = chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>();
        let embeddings = embedder
            .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
            .await?;
        for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
            fixture_ctx.store.insert_chunk_with_embedding_meta(
                Uuid::new_v4(),
                memory.id,
                &chunk.text,
                chunk.position,
                chunk.section.as_deref(),
                embedding,
                &embedder.model(),
                embedder.dimensions(),
                &embedder.prompt_format(),
                Some(document_id),
            )?;
        }
    }
    let compile = crate::commands::compile::compile_kb(&fixture_ctx, kb).await?;
    let cases = [
        ("human rapamycin vaccine response", "everolimus"),
        ("metformin mortality confounding", "confounding"),
        ("graph retrieval multi-hop entities", "hipporag"),
    ];
    let mut per_question = Vec::new();
    let mut correct = 0usize;
    for (query, expected) in cases {
        let reranker: Option<&PassthroughReranker> = None;
        let results = hybrid_recall(
            &fixture_ctx.store,
            &embedder,
            reranker,
            HybridParams {
                query,
                top_k: 3,
                rrf_k: 60.0,
                rerank_top_n: 10,
                filters: Filters {
                    diary: Some("default".to_string()),
                    kb: Some(kb.to_string()),
                    since: None,
                    until: None,
                },
                mode: RecallMode::Evidence,
                graph_hops: 1,
                allow_mixed_embeddings: false,
            },
        )
        .await?;
        let hit = results
            .iter()
            .any(|r| r.content.to_ascii_lowercase().contains(expected));
        if hit {
            correct += 1;
        }
        per_question.push(serde_json::json!({
            "query": query,
            "expected": expected,
            "hit": hit,
            "top_result_kind": results.first().map(|r| r.kind.clone()),
            "top_result": results.first().map(|r| r.content.clone()),
        }));
    }
    let accuracy = correct as f64 / cases.len() as f64;
    let payload = json!({
        "suite": "scientific-mini",
        "mode": "stub_hybrid_evidence",
        "questions_evaluated": cases.len(),
        "accuracy": accuracy,
        "correct": correct,
        "compile": compile,
        "per_question": per_question,
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

/// `engram bench personal` — the personal-memory quality gate.
///
/// Seeds the committed synthetic dataset into a throwaway in-memory store
/// through the production write path (chunker + embedder + embedding metadata),
/// then runs every query through the *real* `hybrid_recall` pipeline, so the
/// numbers move when retrieval changes. Offline by default: deterministic hash
/// embedder, passthrough reranker, zero network.
///
/// Several categories are expected to score badly today (hint-less cross-KB
/// lookups return nothing at all under the default `kb=default` filter;
/// event-time temporal queries with no `--since/--until` have no signal to go
/// on). Measuring that is the job — the suite asserts nothing about score
/// levels, it reports them.
async fn run_personal(
    ctx: &AppContext,
    profile: String,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use crate::retrieval::{hybrid_recall, Filters, HybridParams, RecallMode, RetrievalProfile};
    use engram_bench::personal::{self, QueryRun};
    use engram_embed::Embedder;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::Reranker;
    use engram_storage::SqliteStore;
    use std::time::Instant;

    let profile_enum = RetrievalProfile::parse(&profile)?;
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");

    // A cloud profile with no key must fail loudly rather than silently
    // benchmarking the 64-dim stub and reporting it as cloud quality.
    if profile_enum != RetrievalProfile::Offline && (gemini_key.is_none() || force_stub) {
        return Err(CliError::Config(format!(
            "profile `{}` needs a Gemini API key for real retrieval, but none is available. \
             Set it with `engram config set keys.gemini <KEY>` (or GEMINI_API_KEY), \
             or run `engram bench personal --profile offline` for the deterministic offline suite.",
            profile_enum.as_str()
        )));
    }

    let use_cloud = profile_enum != RetrievalProfile::Offline;
    let embedder: Box<dyn Embedder> = if use_cloud {
        Box::new(GeminiEmbedder::new(
            gemini_key.clone().expect("checked above"),
        ))
    } else {
        Box::new(StubEmbedder::default())
    };
    let cohere_key = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere");
    let reranker: Option<Box<dyn Reranker>> = match (use_cloud, cohere_key) {
        (true, Some(key)) if !crate::commands::config::rerank_disabled() => {
            Some(Box::new(CohereReranker::new(key)))
        }
        _ => None,
    };
    let reranker_name = reranker
        .as_ref()
        .map(|r| r.name().to_string())
        .unwrap_or_else(|| "passthrough".to_string());

    let store = SqliteStore::open_in_memory()?;
    let seeded = personal::seed_store(&store, embedder.as_ref()).await?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);
    let rerank_top_n = profile_enum.default_rerank_top_n();

    let mut runs: Vec<QueryRun> = Vec::with_capacity(personal::QUERIES.len());
    for q in personal::QUERIES {
        // No hint means the agent searches the `recall --kb` default, which is
        // exactly the path a cross-KB fact falls through today.
        let kb_filter = Some(q.kb_hint.unwrap_or(personal::DEFAULT_KB).to_string());
        let (since, until) = match q.window {
            Some(w) => {
                let (from, to) = personal::window_bounds(w)?;
                (Some(from), Some(to))
            }
            None => (None, None),
        };

        let start = Instant::now();
        let results = hybrid_recall(
            &store,
            embedder.as_ref(),
            reranker.as_deref(),
            HybridParams {
                query: q.text,
                top_k,
                rrf_k,
                rerank_top_n,
                filters: Filters {
                    diary: Some(personal::DIARY.to_string()),
                    kb: kb_filter.clone(),
                    since,
                    until,
                },
                mode: RecallMode::Evidence,
                graph_hops: 1,
                allow_mixed_embeddings: false,
            },
        )
        .await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        let retrieved = to_items(&results, &seeded);

        // Hint-less queries are also run unfiltered to record the ceiling the
        // default-KB behaviour is currently leaving on the table.
        let retrieved_all_kbs = if q.kb_hint.is_none() {
            let results = hybrid_recall(
                &store,
                embedder.as_ref(),
                reranker.as_deref(),
                HybridParams {
                    query: q.text,
                    top_k,
                    rrf_k,
                    rerank_top_n,
                    filters: Filters {
                        diary: Some(personal::DIARY.to_string()),
                        kb: None,
                        since,
                        until,
                    },
                    mode: RecallMode::Evidence,
                    graph_hops: 1,
                    allow_mixed_embeddings: false,
                },
            )
            .await?;
            Some(to_items(&results, &seeded))
        } else {
            None
        };

        runs.push(QueryRun {
            query_id: q.id,
            category: q.category,
            text: q.text,
            kb_filter,
            gold: q.gold,
            retrieved,
            retrieved_all_kbs,
            latency_ms,
        });
    }

    let report = personal::aggregate(
        &runs,
        "personal",
        profile_enum.as_str(),
        &embedder.model(),
        &reranker_name,
        top_k,
        &seeded,
    );

    let mut meta = Metadata::default();
    meta.add("profile", profile_enum.as_str());
    meta.add("embedder", embedder.model());
    meta.add("reranker", reranker_name);
    meta.add("memories_seeded", report.memories_seeded);
    meta.add("queries_evaluated", report.queries_evaluated);
    if let Some(path) = &save {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        meta.add("saved_to", path.to_string_lossy());
    }

    print_success(ctx.format, report, meta, print_personal_table);
    Ok(())
}

/// Map hybrid results back onto dataset memories. Anything that is not a
/// seeded chunk (an entity or claim candidate) keeps its rank but scores as a
/// miss — it really did displace an answer.
fn to_items(
    results: &[crate::retrieval::HybridResult],
    seeded: &engram_bench::personal::SeededDataset,
) -> Vec<engram_bench::personal::RetrievedItem> {
    results
        .iter()
        .map(|r| engram_bench::personal::RetrievedItem {
            dataset_id: seeded.dataset_id_for(&r.id).map(|s| s.to_string()),
            kind: r.kind.clone(),
            score: r.score,
        })
        .collect()
}

/// Human-readable per-category table for `engram bench personal`.
fn print_personal_table(report: &engram_bench::personal::PersonalReport) {
    use comfy_table::{presets::UTF8_FULL, Table};

    println!(
        "personal memory bench — profile={} embedder={} reranker={} top_k={}",
        report.profile, report.embedder, report.reranker, report.top_k
    );
    println!(
        "{} memories / {} chunks across {} KBs; {} queries",
        report.memories_seeded,
        report.chunks_seeded,
        report.kbs.len(),
        report.queries_evaluated
    );

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec![
        "category", "queries", "R@1", "R@3", "R@5", "R@10", "MRR@10", "cov@10", "abstain",
    ]);
    for c in &report.per_category {
        table.add_row(vec![
            c.category.clone(),
            c.queries.to_string(),
            fmt3(c.recall_at_1),
            fmt3(c.recall_at_3),
            fmt3(c.recall_at_5),
            fmt3(c.recall_at_10),
            fmt3(c.mrr_at_10),
            fmt3(c.gold_coverage_at_10),
            c.abstention_rate.map(fmt3).unwrap_or_else(|| "-".into()),
        ]);
    }
    let m = &report.metrics;
    table.add_row(vec![
        "ALL (scored)".to_string(),
        m.queries_scored.to_string(),
        fmt3(m.recall_at_1),
        fmt3(m.recall_at_3),
        fmt3(m.recall_at_5),
        fmt3(m.recall_at_10),
        fmt3(m.mrr_at_10),
        fmt3(m.gold_coverage_at_10),
        fmt3(m.abstention_rate),
    ]);
    println!("{table}");

    if let Some(cmp) = &report.all_kbs_comparison {
        println!(
            "hint-less queries reran with no KB filter ({} queries): R@1 {} R@5 {} R@10 {} MRR {}",
            cmp.queries,
            fmt3(cmp.recall_at_1),
            fmt3(cmp.recall_at_5),
            fmt3(cmp.recall_at_10),
            fmt3(cmp.mrr_at_10),
        );
    }
    println!(
        "negative controls: {} queries, abstained on {}, mean top score {}",
        m.negative_controls,
        fmt3(m.abstention_rate),
        fmt3(m.negative_mean_top_score)
    );
    println!(
        "latency: mean {:.0}ms  p50 {:.0}ms  p95 {:.0}ms",
        m.mean_latency_ms, m.p50_latency_ms, m.p95_latency_ms
    );
}

fn fmt3(v: f32) -> String {
    format!("{v:.3}")
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
            let embedder =
                GeminiEmbedder::from_env().map_err(|e| CliError::Config(format!("gemini: {e}")))?;
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
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
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
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
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

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    // Per-question checkpoint goes next to the final save path with a
    // .checkpoint.jsonl suffix. Always written so a crash leaves at
    // least the partial results recoverable. The user explicitly asked
    // for this after losing 30+ min on a sidecar OOM event.
    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("longmemeval-qa-{}.json", timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

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
            Some(checkpoint_path.clone()),
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
            Some(checkpoint_path.clone()),
        )
        .await?
    };

    // Final report file (transactional, written only on full success).
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
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_locomo_plus_qa(
    ctx: &AppContext,
    limit: Option<usize>,
    answerer_model: String,
    judge_model: String,
    ragas: bool,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::locomo::{default_path as locomo_default_path, LocomoDataset};
    use engram_bench::locomo_plus::{default_path as locomo_plus_default_path, LocomoPlusDataset};
    use engram_bench::qa::run_locomo_plus_qa as run_qa;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_rerank::zerank_local::ZerankLocalReranker;

    let plus_path = locomo_plus_default_path();
    if !plus_path.exists() {
        return Err(CliError::Config(format!(
            "LoCoMo-Plus not found at {}. Download with:\nmkdir -p data/locomo_plus && curl -sL 'https://raw.githubusercontent.com/xjtuleeyf/Locomo-Plus/main/data/locomo_plus.json' -o data/locomo_plus/locomo_plus.json",
            plus_path.display()
        )));
    }
    let locomo_path = locomo_default_path();
    if !locomo_path.exists() {
        return Err(CliError::Config(format!(
            "LoCoMo not found at {}. Download with: mkdir -p data/locomo && \
             curl -sL 'https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json' \
             -o {}",
            locomo_path.display(),
            locomo_path.display()
        )));
    }
    let plus = LocomoPlusDataset::load_from_file(&plus_path)?;
    let locomo = LocomoDataset::load_from_file(&locomo_path)?;

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

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("locomo-plus-qa-{}.json", timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let provider = std::env::var("ENGRAM_RERANK_PROVIDER").unwrap_or_else(|_| {
        if cohere_key.is_some() {
            "cohere".to_string()
        } else {
            "none".to_string()
        }
    });
    let report = match provider.as_str() {
        "zerank2" | "zerank-2" | "zerank_local" | "local" => {
            let reranker = ZerankLocalReranker::new();
            if !reranker.health_check().await.unwrap_or(false) {
                return Err(CliError::Config(
                    "ENGRAM_RERANK_PROVIDER=zerank2 but the sidecar is not reachable. \
                     Start it with: uv run --with sentence-transformers --with torch \
                     crates/engram-rerank/python/zerank_server.py"
                        .into(),
                ));
            }
            run_qa(
                &plus,
                &locomo,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "cohere" => {
            let cohere = cohere_key.ok_or_else(|| {
                CliError::Config("ENGRAM_RERANK_PROVIDER=cohere but COHERE_API_KEY not set".into())
            })?;
            let reranker = CohereReranker::new(cohere);
            run_qa(
                &plus,
                &locomo,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "none" | "passthrough" | "off" => {
            let no_rerank: Option<&PassthroughReranker> = None;
            run_qa(
                &plus,
                &locomo,
                &embedder,
                no_rerank,
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                ragas,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown ENGRAM_RERANK_PROVIDER={other} (expected: zerank2 | cohere | none)"
            )));
        }
    };

    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "locomo-plus",
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
        "by_relation_type": report.by_question_type,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_memoryagentbench_qa(
    ctx: &AppContext,
    split_name: String,
    limit: Option<usize>,
    answerer_model: String,
    judge_model: String,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::memoryagentbench::{default_dir, MabDataset, MabSplit};
    use engram_bench::qa::run_memoryagentbench_qa as run_qa;
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::passthrough::PassthroughReranker;
    use engram_rerank::zerank_local::ZerankLocalReranker;

    let split = MabSplit::from_name(&split_name).ok_or_else(|| {
        CliError::BadInput(format!(
            "unknown --mab-split {split_name} (expected: accurate_retrieval | test_time_learning | long_range_understanding | conflict_resolution)"
        ))
    })?;
    let path = default_dir().join(format!("{}.jsonl", split.name()));
    if !path.exists() {
        return Err(CliError::Config(format!(
            "MemoryAgentBench split not found at {}. Download with:\nuv run --with huggingface_hub --with pyarrow scripts/download_memoryagentbench.py",
            path.display()
        )));
    }
    let dataset = MabDataset::load_jsonl(&path, split)?;

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

    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer = OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!(
        "memoryagentbench-{}-{}.json",
        split.name().to_ascii_lowercase(),
        timestamp
    ));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let provider = std::env::var("ENGRAM_RERANK_PROVIDER").unwrap_or_else(|_| {
        if cohere_key.is_some() {
            "cohere".to_string()
        } else {
            "none".to_string()
        }
    });
    let report = match provider.as_str() {
        "zerank2" | "zerank-2" | "zerank_local" | "local" => {
            let reranker = ZerankLocalReranker::new();
            if !reranker.health_check().await.unwrap_or(false) {
                return Err(CliError::Config(
                    "ENGRAM_RERANK_PROVIDER=zerank2 but the sidecar is not reachable. \
                     Start it with: uv run --with sentence-transformers --with torch \
                     crates/engram-rerank/python/zerank_server.py"
                        .into(),
                ));
            }
            run_qa(
                &dataset,
                &embedder,
                Some(&reranker),
                &answerer,
                &judge,
                rrf_k,
                top_k,
                limit,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "cohere" => {
            let cohere = cohere_key.ok_or_else(|| {
                CliError::Config("ENGRAM_RERANK_PROVIDER=cohere but COHERE_API_KEY not set".into())
            })?;
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
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        "none" | "passthrough" | "off" => {
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
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown ENGRAM_RERANK_PROVIDER={other} (expected: zerank2 | cohere | none)"
            )));
        }
    };

    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "memoryagentbench",
        "split": split.name(),
        "questions_evaluated": report.questions_evaluated,
        "unscored_count": report.unscored_count,
        "accuracy": report.accuracy,
        "correct": report.correct_count,
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
        "by_source": report.by_source,
        "notes": report.notes,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_litqa2(
    ctx: &AppContext,
    mode_str: String,
    limit: Option<usize>,
    kb: Option<String>,
    corpus_path: Option<std::path::PathBuf>,
    top_k: usize,
    answerer_model: String,
    judge_model: String,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::litqa2::{default_path, LitQA2Dataset};
    use engram_bench::qa::{run_litqa2_qa, LitQa2Mode};
    use engram_rerank::passthrough::PassthroughReranker;

    let mode = match mode_str.as_str() {
        "closed-book" | "closed_book" | "closed" => LitQa2Mode::ClosedBook,
        "fulltext" | "full-text" | "full_text" => LitQa2Mode::FullText,
        other => {
            return Err(CliError::BadInput(format!(
                "unknown --mode {other} (expected: closed-book | fulltext)"
            )))
        }
    };

    let path = default_path();
    if !path.exists() {
        return Err(CliError::Config(format!(
            "LitQA2 dataset not found at {}. The source dataset is GATED: first run \
             `huggingface-cli login` and accept the terms at \
             https://huggingface.co/datasets/futurehouse/lab-bench, then download with:\n\
             huggingface-cli download futurehouse/lab-bench --repo-type dataset --include 'LitQA2*' --local-dir data/litqa2",
            path.display()
        )));
    }
    let dataset = LitQA2Dataset::load_from_file(&path)?;

    // The judge model is reserved for future LitQA2 scoring hooks; choice
    // grading is deterministic, so closed-book needs only the answerer API.
    let _ = &judge_model;

    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| {
                CliError::Config("OPENROUTER_API_KEY not set — required for the LLM answerer".into())
            })?;
    let answerer =
        OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);
    let top_k = if top_k == 0 { LITQA2_DEFAULT_TOP_K } else { top_k };

    // closed-book runs with NO embeddings / corpus; full-text builds the corpus
    // and a Gemini embedder.
    let corpus = match mode {
        LitQa2Mode::ClosedBook => std::collections::HashMap::new(),
        LitQa2Mode::FullText => build_corpus(ctx, kb.as_deref(), corpus_path.as_deref())?,
    };

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("litqa2-{}-{}.json", mode_str, timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let no_rerank: Option<&PassthroughReranker> = None;
    let report = match mode {
        LitQa2Mode::ClosedBook => {
            // Closed-book: answerer only. Embedder is never invoked because the
            // corpus is empty, but the generic call still needs a concrete type.
            let embedder = StubEmbedder::default();
            run_litqa2_qa(
                &dataset,
                &corpus,
                mode,
                &embedder,
                no_rerank,
                &answerer,
                rrf_k,
                top_k,
                limit,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
        LitQa2Mode::FullText => {
            let gemini_key =
                crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
                    .ok_or_else(|| {
                        CliError::Config(
                            "GEMINI_API_KEY not set — required for LitQA2 full-text retrieval".into(),
                        )
                    })?;
            let mut embedder = GeminiEmbedder::new(gemini_key);
            if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
                embedder = embedder.with_model(model);
            }
            run_litqa2_qa(
                &dataset,
                &corpus,
                mode,
                &embedder,
                no_rerank,
                &answerer,
                rrf_k,
                top_k,
                limit,
                Some(checkpoint_path.clone()),
            )
            .await?
        }
    };

    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let s = &report.scores;
    let payload = json!({
        "suite": "litqa2",
        "mode": report.mode,
        "questions_evaluated": report.questions_evaluated,
        "accuracy": s.accuracy,
        "precision": s.precision,
        // Strict precision counts empty/unparseable API responses as failed
        // commitments (not safe abstentions), so a flaky API can't inflate the
        // headline. Report both; no_response_rate exposes how much they differ.
        "precision_strict": s.precision_strict,
        "coverage": s.coverage,
        "abstain_rate": s.abstain_rate,
        "no_response_rate": s.no_response_rate,
        "retrieval_success": s.retrieval_success,
        "mean_latency_ms": report.mean_latency_ms,
        "p50_latency_ms": report.p50_latency_ms,
        "p95_latency_ms": report.p95_latency_ms,
        "answerer_model": answerer_model,
        "answerer_prompt_tokens": report.answerer_total_prompt_tokens,
        "answerer_completion_tokens": report.answerer_total_completion_tokens,
        "per_question": report.per_question,
        "notes": report.notes,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

async fn run_beir(
    ctx: &AppContext,
    dataset_name: String,
    rerank_str: String,
    top_k: usize,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use chrono::TimeZone;
    use engram_bench::beir::BeirDataset;
    use engram_core::fusion::{reciprocal_rank_fusion, RankedHit};
    use engram_core::types::{Memory, MemorySource, RetrievalSource};
    use engram_embed::{Embedder, TaskMode};
    use engram_rerank::cohere::CohereReranker;
    use engram_rerank::{RerankCandidate, Reranker};
    use engram_storage::SqliteStore;
    use std::collections::HashMap;
    use uuid::Uuid;

    match dataset_name.as_str() {
        "scifact" | "nfcorpus" | "trec-covid" | "scidocs" => {}
        other => {
            return Err(CliError::BadInput(format!(
                "unknown --dataset {other} (expected: scifact | nfcorpus | trec-covid | scidocs)"
            )))
        }
    }
    let rerank_on = match rerank_str.as_str() {
        "on" | "true" | "yes" => true,
        "off" | "false" | "no" => false,
        other => {
            return Err(CliError::BadInput(format!(
                "unknown --rerank {other} (expected: on | off)"
            )))
        }
    };

    let dir = std::path::PathBuf::from(format!("data/beir/{dataset_name}"));
    if !dir.exists() {
        return Err(CliError::Config(format!(
            "BEIR dataset '{dataset_name}' not found at {}. Download with:\n\
             uv run --with ir_datasets python scripts/download_beir.py {dataset_name}\n\
             or fetch the BEIR release (corpus.jsonl, queries.jsonl, qrels/<split>.tsv) into {}",
            dir.display(),
            dir.display()
        )));
    }
    let dataset = BeirDataset::load_from_dir(&dir)?;

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);
    let top_k = if top_k == 0 { BEIR_DEFAULT_TOP_K } else { top_k };

    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| {
            CliError::Config("GEMINI_API_KEY not set — required for BEIR dense retrieval".into())
        })?;
    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let reranker = if rerank_on {
        let cohere = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere")
            .ok_or_else(|| {
                CliError::Config("--rerank on requires COHERE_API_KEY to be set".into())
            })?;
        Some(CohereReranker::new(cohere))
    } else {
        None
    };

    // Index the corpus once into an in-memory store, mirroring the bench crate's
    // BEIR/QA retrieval plumbing (FTS5 lexical + dense cosine, fused with RRF).
    let store = SqliteStore::open_in_memory()?;
    let epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();
    let mut chunk_to_doc: HashMap<Uuid, String> = HashMap::new();
    let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();
    let mut chunk_text: HashMap<Uuid, String> = HashMap::new();

    let mut doc_ids: Vec<&String> = dataset.corpus.keys().collect();
    doc_ids.sort_unstable();
    let mut texts: Vec<String> = Vec::with_capacity(doc_ids.len());
    let mut chunk_ids: Vec<Uuid> = Vec::with_capacity(doc_ids.len());
    for doc_id in &doc_ids {
        let (title, text) = &dataset.corpus[*doc_id];
        let blob = if title.trim().is_empty() {
            text.clone()
        } else {
            format!("{title}\n{text}")
        };
        let mem_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("beir-mem:{doc_id}").as_bytes());
        let chunk_id =
            Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("beir-chunk:{doc_id}").as_bytes());
        let m = Memory {
            id: mem_id,
            content: blob.clone(),
            created_at: epoch,
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Conversation {
                thread: (*doc_id).clone(),
                turn: 0,
            },
            diary: "beir".into(),
            valid_from: None,
            valid_until: None,
            tags: vec![],
        };
        store.insert_memory(&m)?;
        store.insert_chunk(chunk_id, mem_id, &blob, 0, None)?;
        chunk_to_doc.insert(chunk_id, (*doc_id).clone());
        chunk_text.insert(chunk_id, blob.clone());
        texts.push(blob);
        chunk_ids.push(chunk_id);
    }
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let doc_vecs = embedder
        .embed_batch(&text_refs, TaskMode::RetrievalDocument)
        .await?;
    for (chunk_id, vec) in chunk_ids.iter().zip(doc_vecs) {
        chunk_embeddings.insert(*chunk_id, vec);
    }

    // Two rankings per query: BM25-only (lexical) and hybrid (RRF of lexical +
    // dense), with an optional rerank applied to the hybrid pool.
    let mut bm25_rankings: HashMap<String, Vec<String>> = HashMap::new();
    let mut hybrid_rankings: HashMap<String, Vec<String>> = HashMap::new();

    for (qid, qtext) in dataset.queries.iter() {
        let fts_q = engram_bench::mini::build_fts_query(qtext);
        let fts_hits = if fts_q.is_empty() {
            Vec::new()
        } else {
            store.fts_search(&fts_q, 100).unwrap_or_default()
        };
        let bm25_ranked: Vec<String> = fts_hits
            .iter()
            .filter_map(|(id, _)| chunk_to_doc.get(id).cloned())
            .collect();
        bm25_rankings.insert(qid.clone(), bm25_ranked);

        let lexical_run: Vec<RankedHit> = fts_hits
            .iter()
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Lexical,
            })
            .collect();

        let q_emb = embedder.embed_one(qtext, TaskMode::RetrievalQuery).await?;
        let mut dense_scored: Vec<(Uuid, f32)> = chunk_embeddings
            .iter()
            .map(|(cid, emb)| {
                let mut dot = 0f32;
                let mut na = 0f32;
                let mut nb = 0f32;
                for (x, y) in q_emb.iter().zip(emb.iter()) {
                    dot += x * y;
                    na += x * x;
                    nb += y * y;
                }
                let sim = if na == 0.0 || nb == 0.0 {
                    0.0
                } else {
                    dot / (na.sqrt() * nb.sqrt())
                };
                (*cid, sim)
            })
            .collect();
        dense_scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let dense_run: Vec<RankedHit> = dense_scored
            .iter()
            .take(100)
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Dense,
            })
            .collect();

        let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);
        let hybrid_ranked: Vec<String> = if let Some(ref r) = reranker {
            let cands: Vec<RerankCandidate> = fused
                .iter()
                .take(top_k.max(100))
                .filter_map(|(id, _)| {
                    chunk_text.get(id).map(|text| RerankCandidate {
                        id: id.to_string(),
                        text: text.clone(),
                    })
                })
                .collect();
            if cands.is_empty() {
                Vec::new()
            } else {
                let reranked = r.rerank(qtext, &cands, cands.len()).await?;
                reranked
                    .into_iter()
                    .filter_map(|rr| Uuid::parse_str(&rr.id).ok())
                    .filter_map(|id| chunk_to_doc.get(&id).cloned())
                    .collect()
            }
        } else {
            fused
                .iter()
                .filter_map(|(id, _)| chunk_to_doc.get(id).cloned())
                .collect()
        };
        hybrid_rankings.insert(qid.clone(), hybrid_ranked);
    }

    let bm25_report = dataset.score(&bm25_rankings);
    let hybrid_report = dataset.score(&hybrid_rankings);

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name =
        benchmarks_dir.join(format!("beir-{}-{}.json", dataset_name, timestamp));
    let save_path = save.unwrap_or(default_name);

    let payload = json!({
        "suite": "beir",
        "dataset": dataset_name,
        "rerank": if rerank_on { "on" } else { "off" },
        "n_queries": hybrid_report.n_queries,
        "rows": [
            {
                "system": "bm25",
                "ndcg_at_10": bm25_report.ndcg_at_10,
                "recall_at_10": bm25_report.recall_at_10,
                "recall_at_100": bm25_report.recall_at_100,
            },
            {
                "system": if rerank_on { "hybrid+rerank" } else { "hybrid" },
                "ndcg_at_10": hybrid_report.ndcg_at_10,
                "recall_at_10": hybrid_report.recall_at_10,
                "recall_at_100": hybrid_report.recall_at_100,
            }
        ],
        "saved_to": save_path.to_string_lossy(),
    });
    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&payload)?)?;

    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_biogen(
    ctx: &AppContext,
    year: String,
    kb: Option<String>,
    corpus_path: Option<std::path::PathBuf>,
    top_k: usize,
    answerer_model: String,
    judge_model: String,
    save: Option<std::path::PathBuf>,
) -> Result<(), CliError> {
    use engram_bench::biogen::{default_path, BioGenTopics};
    use engram_bench::qa::run_biogen as run_qa;
    use engram_rerank::passthrough::PassthroughReranker;

    match year.as_str() {
        "2024" | "2025" => {}
        other => {
            return Err(CliError::BadInput(format!(
                "unknown --year {other} (expected: 2024 | 2025)"
            )))
        }
    }

    let topics_path = default_path();
    if !topics_path.exists() {
        return Err(CliError::Config(format!(
            "TREC BioGen {year} topics not found at {}. Download the topic file and corpus with:\n\
             # topics:\n\
             curl -sL 'https://trec.nist.gov/data/biogen/{year}/topics.json' -o {}\n\
             # corpus (PubMed abstracts for the cited docs), then pass via --corpus or ingest into --kb",
            topics_path.display(),
            topics_path.display()
        )));
    }
    let topics = BioGenTopics::load_from_file(&topics_path)?;

    let corpus = build_corpus(ctx, kb.as_deref(), corpus_path.as_deref())?;

    let openrouter_key =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .ok_or_else(|| {
                CliError::Config(
                    "OPENROUTER_API_KEY not set — required for LLM answerer + judge".into(),
                )
            })?;
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
        .ok_or_else(|| {
            CliError::Config("GEMINI_API_KEY not set — required for BioGen retrieval".into())
        })?;
    let mut embedder = GeminiEmbedder::new(gemini_key);
    if let Ok(model) = std::env::var("GEMINI_EMBED_MODEL") {
        embedder = embedder.with_model(model);
    }
    let answerer =
        OpenRouterClient::new(openrouter_key.clone()).with_model(answerer_model.clone());
    let judge = OpenRouterClient::new(openrouter_key).with_model(judge_model.clone());

    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    let benchmarks_dir = std::path::PathBuf::from("benchmarks");
    let _ = std::fs::create_dir_all(&benchmarks_dir);
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let default_name = benchmarks_dir.join(format!("biogen-{}-{}.json", year, timestamp));
    let save_path = save.unwrap_or(default_name);
    let checkpoint_path = save_path.with_extension("checkpoint.jsonl");

    let no_rerank: Option<&PassthroughReranker> = None;
    let report = run_qa(
        &topics,
        &corpus,
        &embedder,
        no_rerank,
        &answerer,
        &judge,
        rrf_k,
        top_k,
        None,
        Some(checkpoint_path.clone()),
    )
    .await?;

    if let Some(parent) = save_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&save_path, serde_json::to_string_pretty(&report)?)?;

    let payload = json!({
        "suite": "biogen",
        "year": year,
        "topics_evaluated": report.topics_evaluated,
        "csr": report.csr,
        "total_sentences": report.total_sentences,
        "mean_latency_ms": report.mean_latency_ms,
        "p50_latency_ms": report.p50_latency_ms,
        "p95_latency_ms": report.p95_latency_ms,
        "answerer_model": answerer_model,
        "judge_model": judge_model,
        "answerer_prompt_tokens": report.answerer_total_prompt_tokens,
        "answerer_completion_tokens": report.answerer_total_completion_tokens,
        "per_topic": report.per_topic,
        "notes": report.notes,
        "saved_to": save_path.to_string_lossy(),
    });
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
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
    print_success(ctx.format, payload, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_bench::personal::{
        aggregate, QueryCategory, QueryRun, RetrievedItem, SeededDataset,
    };

    fn run(category: QueryCategory, gold: &'static [&'static str], hits: &[&str]) -> QueryRun {
        QueryRun {
            query_id: "q",
            category,
            text: "who is who",
            kb_filter: Some("relationships".into()),
            gold,
            retrieved: hits
                .iter()
                .map(|h| RetrievedItem {
                    dataset_id: Some((*h).to_string()),
                    kind: "chunk".into(),
                    score: 0.5,
                })
                .collect(),
            retrieved_all_kbs: None,
            latency_ms: 1,
        }
    }

    /// The human branch is unreachable from the integration tests (piped
    /// stdout always selects JSON), so exercise the table renderer directly.
    #[test]
    fn personal_table_renders_every_category_row() {
        let runs = vec![
            run(QueryCategory::PersonFact, &["g1"], &["g1"]),
            run(QueryCategory::NegativeControl, &[], &[]),
        ];
        let report = aggregate(
            &runs,
            "personal",
            "offline",
            "stub-64",
            "passthrough",
            10,
            &SeededDataset::default(),
        );
        // Rendering must not panic on an empty result set or a `None`
        // abstention rate.
        print_personal_table(&report);
        assert_eq!(report.per_category.len(), 2);
    }

    #[test]
    fn top_k_sentinel_selects_the_personal_default() {
        assert_eq!(top_k_or(0, PERSONAL_DEFAULT_TOP_K), PERSONAL_DEFAULT_TOP_K);
        assert_eq!(top_k_or(3, PERSONAL_DEFAULT_TOP_K), 3);
    }
}
