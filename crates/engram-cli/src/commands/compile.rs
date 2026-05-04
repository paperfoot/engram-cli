//! `engram compile` — deterministic evidence compiler v1.
//!
//! This compiler preserves raw chunks, then derives cited claims, entities,
//! co-occurrence relations, takeaways, and Markdown wiki pages. It is designed
//! to be idempotent and local-first; LLM synthesis can be layered over these
//! same tables later without changing recall.

use crate::commands::usage::estimated_tokens;
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_graph::extract_entities;
use engram_llm::openrouter::{OpenRouterClient, DEFAULT_EXTRACTION_MODEL, DEFAULT_SYNTHESIS_MODEL};
use engram_llm::{ChatLlm, ChatMessage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileStats {
    pub kb: String,
    pub chunks_scanned: usize,
    pub source_spans: usize,
    pub claims: usize,
    pub entities: usize,
    pub relations: usize,
    pub takeaways: usize,
    pub wiki_pages: usize,
    pub llm_enabled: bool,
    pub llm_extraction_chunks: usize,
    pub llm_claims: usize,
    pub llm_entities: usize,
    pub llm_relations: usize,
    pub llm_takeaways: usize,
    pub llm_wiki_pages: usize,
    pub extraction_model: Option<String>,
    pub synthesis_model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub llm: bool,
    pub extraction_model: String,
    pub synthesis_model: String,
    pub max_llm_chunks: Option<usize>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            llm: false,
            extraction_model: DEFAULT_EXTRACTION_MODEL.to_string(),
            synthesis_model: DEFAULT_SYNTHESIS_MODEL.to_string(),
            max_llm_chunks: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    ctx: &AppContext,
    kb: String,
    all: bool,
    llm: bool,
    extraction_model: Option<String>,
    synthesis_model: Option<String>,
    max_llm_chunks: Option<usize>,
) -> Result<(), CliError> {
    if !all {
        return Err(CliError::BadInput(
            "compile currently requires --all to make regeneration idempotent".into(),
        ));
    }
    let config_llm =
        crate::commands::config::resolve_setting("ENGRAM_COMPILER_LLM", "compiler.llm", "false")
            .eq_ignore_ascii_case("true");
    let options = CompileOptions {
        llm: llm || config_llm,
        extraction_model: extraction_model.unwrap_or_else(|| {
            crate::commands::config::resolve_setting(
                "ENGRAM_COMPILER_EXTRACTION_MODEL",
                "compiler.extraction_model",
                DEFAULT_EXTRACTION_MODEL,
            )
        }),
        synthesis_model: synthesis_model.unwrap_or_else(|| {
            crate::commands::config::resolve_setting(
                "ENGRAM_COMPILER_SYNTHESIS_MODEL",
                "compiler.synthesis_model",
                DEFAULT_SYNTHESIS_MODEL,
            )
        }),
        max_llm_chunks,
    };
    let job_id = ctx.store.create_compile_job(
        &kb,
        "evidence",
        json!({
            "llm": options.llm,
            "extraction_model": options.extraction_model,
            "synthesis_model": options.synthesis_model,
            "max_llm_chunks": options.max_llm_chunks,
        }),
    )?;
    let stats = match compile_kb_with_options(ctx, &kb, options.clone()).await {
        Ok(stats) => {
            ctx.store
                .finish_compile_job(job_id, "completed", None, json!({ "stats": stats }))?;
            stats
        }
        Err(err) => {
            ctx.store.finish_compile_job(
                job_id,
                "failed",
                Some(&err.to_string()),
                json!({
                    "llm": options.llm,
                    "extraction_model": options.extraction_model,
                    "synthesis_model": options.synthesis_model,
                }),
            )?;
            return Err(err);
        }
    };
    let mut meta = Metadata::default();
    meta.add("kb", kb.clone());
    meta.add("job_id", job_id.to_string());
    meta.add("claims", stats.claims);
    meta.add("entities", stats.entities);
    meta.add("relations", stats.relations);
    meta.add("wiki_pages", stats.wiki_pages);
    meta.add("llm_enabled", stats.llm_enabled);
    print_success(
        ctx.format,
        json!({ "job_id": job_id, "compile": stats }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub async fn compile_kb(ctx: &AppContext, kb: &str) -> Result<CompileStats, CliError> {
    compile_kb_with_options(ctx, kb, CompileOptions::default()).await
}

pub async fn compile_kb_with_options(
    ctx: &AppContext,
    kb: &str,
    options: CompileOptions,
) -> Result<CompileStats, CliError> {
    ctx.store.ensure_kb(kb, None)?;
    let chunks = ctx.store.list_chunks_for_kb(kb)?;
    ctx.store.clear_derived_for_kb(kb)?;

    let mut stats = CompileStats {
        kb: kb.to_string(),
        chunks_scanned: chunks.len(),
        source_spans: 0,
        claims: 0,
        entities: 0,
        relations: 0,
        takeaways: 0,
        wiki_pages: 0,
        llm_enabled: options.llm,
        llm_extraction_chunks: 0,
        llm_claims: 0,
        llm_entities: 0,
        llm_relations: 0,
        llm_takeaways: 0,
        llm_wiki_pages: 0,
        extraction_model: options.llm.then_some(options.extraction_model.clone()),
        synthesis_model: options.llm.then_some(options.synthesis_model.clone()),
    };

    let mut entity_ids: HashMap<String, uuid::Uuid> = HashMap::new();
    let mut entity_mentions: BTreeMap<String, usize> = BTreeMap::new();
    let mut doc_takeaway_done: HashSet<Option<uuid::Uuid>> = HashSet::new();
    let mut span_by_chunk: HashMap<uuid::Uuid, uuid::Uuid> = HashMap::new();

    for chunk in &chunks {
        let preview: String = chunk.content.chars().take(500).collect();
        let span_id = ctx.store.insert_source_span(
            kb,
            chunk.document_id,
            Some(chunk.chunk_id),
            chunk.section.as_deref(),
            chunk.source.as_deref(),
            &preview,
        )?;
        stats.source_spans += 1;
        span_by_chunk.insert(chunk.chunk_id, span_id);

        let entities = extract_entities(&chunk.content);
        let mut local_entities = Vec::new();
        for name in entities.into_iter().take(16) {
            let id = ctx
                .store
                .upsert_entity(kb, &name, infer_entity_kind(&name), 1)?;
            entity_ids.insert(name.clone(), id);
            *entity_mentions.entry(name.clone()).or_insert(0) += 1;
            local_entities.push((name, id));
        }

        for pair in local_entities.windows(2) {
            let (a_name, a_id) = &pair[0];
            let (b_name, b_id) = &pair[1];
            if a_id != b_id {
                ctx.store.insert_relation(
                    kb,
                    *a_id,
                    *b_id,
                    "co_occurs_with",
                    1.0,
                    json!({ "chunk_id": chunk.chunk_id.to_string(), "from": a_name, "to": b_name }),
                    Some(span_id),
                )?;
                stats.relations += 1;
            }
        }

        for sentence in evidence_sentences(&chunk.content).into_iter().take(3) {
            let level = infer_evidence_level(&sentence);
            ctx.store.insert_claim(
                kb,
                &sentence,
                level,
                0.72,
                Some(span_id),
                Some(chunk.chunk_id),
                chunk.document_id,
            )?;
            stats.claims += 1;
        }

        if doc_takeaway_done.insert(chunk.document_id) {
            let takeaway = first_sentence(&chunk.content);
            if !takeaway.is_empty() {
                ctx.store.insert_takeaway(
                    kb,
                    chunk.document_id,
                    &takeaway,
                    infer_evidence_level(&chunk.content),
                    json!([{ "chunk_id": chunk.chunk_id.to_string(), "source": chunk.source }]),
                )?;
                stats.takeaways += 1;
            }
        }
    }

    if options.llm {
        run_llm_compiler(
            ctx,
            kb,
            &chunks,
            &span_by_chunk,
            &mut entity_mentions,
            &mut stats,
            &options,
        )
        .await?;
    }

    stats.entities = entity_ids.len();
    stats.wiki_pages = write_wiki(ctx, kb, &chunks, &entity_mentions)?;
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
async fn run_llm_compiler(
    ctx: &AppContext,
    kb: &str,
    chunks: &[engram_storage::ChunkRecord],
    span_by_chunk: &HashMap<uuid::Uuid, uuid::Uuid>,
    entity_mentions: &mut BTreeMap<String, usize>,
    stats: &mut CompileStats,
    options: &CompileOptions,
) -> Result<(), CliError> {
    let key = crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
        .ok_or_else(|| {
            CliError::Config("OPENROUTER_API_KEY is required for `engram compile --llm`".into())
        })?;
    let extraction_llm = OpenRouterClient::new(key.clone())
        .with_model(options.extraction_model.clone())
        .with_max_tokens(4096);
    let synthesis_llm = OpenRouterClient::new(key)
        .with_model(options.synthesis_model.clone())
        .with_max_tokens(8192);
    let max_chunks = options.max_llm_chunks.unwrap_or(chunks.len());
    for chunk in chunks.iter().take(max_chunks) {
        let prompt = extraction_prompt(kb, chunk);
        let response = extraction_llm
            .chat(&[
                ChatMessage::system(
                    "Extract cited scientific/domain evidence. Return one JSON object only.",
                ),
                ChatMessage::user(prompt.clone()),
            ])
            .await
            .map_err(|e| CliError::Transient(format!("compiler extraction failed: {e}")))?;
        record_openrouter_usage(
            ctx,
            "compiler_extract",
            &response.model,
            Some(kb),
            estimated_tokens(&prompt),
            response.prompt_tokens,
            response.completion_tokens,
        )?;
        let evidence = match parse_llm_evidence(&response.content) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    "compiler extraction parse failed for {}: {e}",
                    chunk.chunk_id
                );
                continue;
            }
        };
        let span_id = span_by_chunk.get(&chunk.chunk_id).copied();
        insert_llm_evidence(ctx, kb, chunk, span_id, evidence, entity_mentions, stats)?;
        stats.llm_extraction_chunks += 1;
    }

    if !chunks.is_empty() {
        let overview_prompt = synthesis_prompt(kb, chunks);
        let response = synthesis_llm
            .chat(&[
                ChatMessage::system(
                    "Write concise cited domain wiki pages from retrieved chunks. Preserve chunk citations.",
                ),
                ChatMessage::user(overview_prompt.clone()),
            ])
            .await
            .map_err(|e| CliError::Transient(format!("compiler synthesis failed: {e}")))?;
        record_openrouter_usage(
            ctx,
            "compiler_synthesize",
            &response.model,
            Some(kb),
            estimated_tokens(&overview_prompt),
            response.prompt_tokens,
            response.completion_tokens,
        )?;
        let content = response.content.trim();
        if !content.is_empty() {
            ctx.store.upsert_wiki_page(
                kb,
                "llm/overview.md",
                &format!("{kb} LLM overview"),
                content,
            )?;
            stats.llm_wiki_pages += 1;
        }
    }
    Ok(())
}

fn record_openrouter_usage(
    ctx: &AppContext,
    operation: &str,
    model: &str,
    kb: Option<&str>,
    fallback_prompt_tokens: i64,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
) -> Result<(), CliError> {
    let input = prompt_tokens
        .map(|v| v as i64)
        .unwrap_or(fallback_prompt_tokens);
    let output = completion_tokens.map(|v| v as i64).unwrap_or(0);
    ctx.store.record_usage_event(
        "openrouter",
        operation,
        Some(model),
        kb,
        None,
        1,
        1,
        input,
        output,
        0.0,
        0.0,
        json!({ "estimated_cost": false }),
    )?;
    Ok(())
}

fn insert_llm_evidence(
    ctx: &AppContext,
    kb: &str,
    chunk: &engram_storage::ChunkRecord,
    span_id: Option<uuid::Uuid>,
    evidence: LlmEvidence,
    entity_mentions: &mut BTreeMap<String, usize>,
    stats: &mut CompileStats,
) -> Result<(), CliError> {
    let mut ids: HashMap<String, uuid::Uuid> = HashMap::new();
    for entity in evidence.entities {
        if entity.name.trim().is_empty() {
            continue;
        }
        let id = ctx.store.upsert_entity(
            kb,
            entity.name.trim(),
            entity.kind.as_deref().unwrap_or("concept"),
            1,
        )?;
        ids.insert(entity.name.clone(), id);
        *entity_mentions.entry(entity.name).or_insert(0) += 1;
        stats.llm_entities += 1;
    }
    for claim in evidence.claims {
        if claim.content.trim().is_empty() {
            continue;
        }
        ctx.store.insert_claim(
            kb,
            claim.content.trim(),
            claim.evidence_level.as_deref().unwrap_or("unknown"),
            claim.confidence.unwrap_or(0.75).clamp(0.0, 1.0),
            span_id,
            Some(chunk.chunk_id),
            chunk.document_id,
        )?;
        stats.llm_claims += 1;
    }
    for takeaway in evidence.takeaways {
        if takeaway.trim().is_empty() {
            continue;
        }
        ctx.store.insert_takeaway(
            kb,
            chunk.document_id,
            takeaway.trim(),
            "unknown",
            json!([{ "chunk_id": chunk.chunk_id.to_string(), "source": chunk.source }]),
        )?;
        stats.llm_takeaways += 1;
    }
    for relation in evidence.relations {
        if relation.from.trim().is_empty() || relation.to.trim().is_empty() {
            continue;
        }
        let from_id = if let Some(id) = ids.get(&relation.from).copied() {
            id
        } else {
            ctx.store
                .upsert_entity(kb, relation.from.trim(), "concept", 1)?
        };
        let to_id = if let Some(id) = ids.get(&relation.to).copied() {
            id
        } else {
            ctx.store
                .upsert_entity(kb, relation.to.trim(), "concept", 1)?
        };
        ctx.store.insert_relation(
            kb,
            from_id,
            to_id,
            relation.predicate.as_deref().unwrap_or("related_to"),
            relation.weight.unwrap_or(1.0).clamp(0.1, 10.0),
            json!({
                "chunk_id": chunk.chunk_id.to_string(),
                "extracted_by": "llm_compiler",
            }),
            span_id,
        )?;
        stats.llm_relations += 1;
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
struct LlmEvidence {
    #[serde(default)]
    claims: Vec<LlmClaim>,
    #[serde(default)]
    entities: Vec<LlmEntity>,
    #[serde(default)]
    relations: Vec<LlmRelation>,
    #[serde(default)]
    takeaways: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LlmClaim {
    content: String,
    evidence_level: Option<String>,
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct LlmEntity {
    name: String,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmRelation {
    from: String,
    to: String,
    predicate: Option<String>,
    weight: Option<f32>,
}

fn parse_llm_evidence(text: &str) -> Result<LlmEvidence, serde_json::Error> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str(trimmed) {
        return Ok(v);
    }
    let start = trimmed.find('{').unwrap_or(0);
    let end = trimmed.rfind('}').map(|i| i + 1).unwrap_or(trimmed.len());
    serde_json::from_str(&trimmed[start..end])
}

fn extraction_prompt(kb: &str, chunk: &engram_storage::ChunkRecord) -> String {
    format!(
        "KB: {kb}\nChunk id: {chunk_id}\nDocument id: {document_id}\nSection: {section}\nSource: {source}\n\nExtract evidence from the chunk below.\nReturn JSON only with this shape:\n{{\"claims\":[{{\"content\":\"short factual claim with numbers if present\",\"evidence_level\":\"review|meta_analysis|rct|human_observational|animal|in_vitro|preprint|news|opinion|unknown\",\"confidence\":0.0}}],\"entities\":[{{\"name\":\"canonical entity\",\"kind\":\"gene|protein|pathway|person|organization|drug|concept|method|species|dataset\"}}],\"relations\":[{{\"from\":\"entity\",\"to\":\"entity\",\"predicate\":\"typed_relation\",\"weight\":1.0}}],\"takeaways\":[\"short cited takeaway\"]}}\nEvery claim must be directly supported by this chunk. Do not invent citations.\n\nChunk:\n{content}",
        chunk_id = chunk.chunk_id,
        document_id = chunk
            .document_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string()),
        section = chunk.section.as_deref().unwrap_or("unknown"),
        source = chunk.source.as_deref().unwrap_or("unknown"),
        content = chunk.content
    )
}

fn synthesis_prompt(kb: &str, chunks: &[engram_storage::ChunkRecord]) -> String {
    let excerpts = chunks
        .iter()
        .take(20)
        .map(|chunk| {
            let snippet: String = chunk.content.chars().take(700).collect();
            format!(
                "[chunk:{} section:{} source:{}]\n{}",
                chunk.chunk_id,
                chunk.section.as_deref().unwrap_or("unknown"),
                chunk.source.as_deref().unwrap_or("unknown"),
                snippet.replace('\n', " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "Create a concise Markdown domain overview for KB `{kb}` from these excerpts. Every factual paragraph must include chunk citations like [chunk:<uuid>]. Sections: Scope, Key Takeaways, Entities, Open Questions.\n\n{excerpts}"
    )
}

fn evidence_sentences(text: &str) -> Vec<String> {
    text.split_terminator(['.', '\n'])
        .map(str::trim)
        .filter(|s| s.len() > 40 && s.len() < 700)
        .filter(|s| looks_like_evidence(s))
        .map(|s| format!("{s}."))
        .collect()
}

fn looks_like_evidence(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    s.chars().any(|c| c.is_ascii_digit())
        || [
            "increased",
            "decreased",
            "associated",
            "significant",
            "p-value",
            "cohort",
            "trial",
            "mouse",
            "mice",
            "human",
            "patients",
            "gene",
            "pathway",
            "protein",
            "dose",
            "mortality",
            "lifespan",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn first_sentence(text: &str) -> String {
    text.split_terminator(['.', '\n'])
        .map(str::trim)
        .find(|s| s.len() > 30)
        .map(|s| format!("{s}."))
        .unwrap_or_default()
}

fn infer_evidence_level(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    if lower.contains("meta-analysis") || lower.contains("meta analysis") {
        "meta_analysis"
    } else if lower.contains("randomized") || lower.contains("rct") || lower.contains("trial") {
        "rct"
    } else if lower.contains("cohort") || lower.contains("patients") || lower.contains("human") {
        "human_observational"
    } else if lower.contains("mouse") || lower.contains("mice") || lower.contains("rat") {
        "animal"
    } else if lower.contains("cell") || lower.contains("in vitro") {
        "in_vitro"
    } else if lower.contains("review") {
        "review"
    } else if lower.contains("preprint") {
        "preprint"
    } else {
        "unknown"
    }
}

fn infer_entity_kind(name: &str) -> &'static str {
    if name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
    {
        "gene"
    } else if name.ends_with(" Inc") || name.ends_with(" Ltd") || name.ends_with(" Labs") {
        "organization"
    } else {
        "concept"
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c.is_whitespace() || c == '-' || c == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn write_wiki(
    ctx: &AppContext,
    kb: &str,
    chunks: &[engram_storage::ChunkRecord],
    entity_mentions: &BTreeMap<String, usize>,
) -> Result<usize, CliError> {
    let mut pages = 0usize;
    let mut index = format!("# {kb}\n\n");
    index.push_str("## Scope\n\n");
    index.push_str(&format!(
        "Compiled from {} chunks. Every paragraph below is derived from stored chunks and claims.\n\n",
        chunks.len()
    ));
    index.push_str("## Top entities\n\n");
    for (name, count) in entity_mentions.iter().rev().take(25) {
        index.push_str(&format!(
            "- [{}](entities/{}.md) ({count})\n",
            name,
            slugify(name)
        ));
    }
    ctx.store.upsert_wiki_page(kb, "index.md", kb, &index)?;
    pages += 1;

    for (name, count) in entity_mentions.iter().take(50) {
        let slug = slugify(name);
        let mut content = format!("# {name}\n\nMention count: {count}\n\n## Evidence snippets\n\n");
        for chunk in chunks
            .iter()
            .filter(|c| {
                c.content
                    .to_ascii_lowercase()
                    .contains(&name.to_ascii_lowercase())
            })
            .take(5)
        {
            let snippet: String = chunk.content.chars().take(260).collect();
            content.push_str(&format!(
                "- {} [chunk:{}]\n",
                snippet.replace('\n', " "),
                chunk.chunk_id
            ));
        }
        ctx.store
            .upsert_wiki_page(kb, &format!("entities/{slug}.md"), name, &content)?;
        pages += 1;
    }

    Ok(pages)
}
