//! `engram remember <content>` — store + embed a memory, and (by default)
//! extract the personal facts inside it.
//!
//! Facts enter through two doors: LLM extraction, which now runs on every
//! `remember` whenever an OpenRouter key resolves, or `--facts-json` (the
//! caller supplies triples directly, no LLM). `--no-extract-facts` opts out.
//! With no key, extraction is skipped *loudly* — the envelope says so and
//! points at `facts backfill` — because a silently dormant fact layer is how
//! a store ends up with zero facts after a year of use.
//!
//! Every fact then runs the write-path v2 pipeline in `fact_write`: predicate
//! and subject canonicalization, exact-match dedup/supersession, slot
//! policies, and the embedding novelty gate. Uncertain contradictions are
//! queued for the calling agent under the default `assisted` autonomy —
//! see `facts resolve`.

use crate::commands::usage::{estimated_tokens, estimated_tokens_for_texts, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::fact_llm::FactLlm;
use crate::fact_write::{write_fact, Autonomy, FactWriteCtx, FactWriteOutcome};
use crate::output::{print_success, Metadata};
use chrono::{DateTime, Utc};
use engram_core::temporal::parse_moment;
use engram_core::types::{Memory, MemorySource, SourceKind};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{extract_facts, ExtractedFact};
use engram_ingest::chunker::naive_split;
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

/// Everything `remember` and `memory add` accept. One struct so the two
/// commands cannot drift apart on flag handling.
#[derive(Debug, Default)]
pub struct RememberArgs {
    pub content: String,
    pub importance: u8,
    pub tags: Vec<String>,
    pub diary: String,
    pub kb: String,
    /// `--no-extract-facts`: skip the extraction LLM call entirely.
    pub no_extract_facts: bool,
    /// `--extract-facts`: redundant now that extraction is the default, but
    /// it still forces extraction alongside `--facts-json`, which is what the
    /// flag did before. Existing scripts keep their exact behavior.
    pub force_extract_facts: bool,
    pub allow_duplicate: bool,
    pub autonomy: String,
    pub facts_json: Option<String>,
    /// `--event-time`: when the content actually happened.
    pub event_time: Option<String>,
}

/// Why the fact extractor did or did not run, reported in the envelope so an
/// agent can react instead of assuming facts were captured.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExtractionStatus {
    Ran,
    /// Offline deterministic script (tests/CI).
    RanStub,
    /// `--no-extract-facts`.
    Disabled,
    /// `--facts-json`: the caller already knows the triples, so no LLM call.
    CallerSupplied,
    /// No OpenRouter key resolved — the loud skip.
    SkippedNoKey,
    /// The LLM call was attempted and failed. The memory is still stored.
    Failed(String),
}

impl ExtractionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            ExtractionStatus::Ran => "ran",
            ExtractionStatus::RanStub => "ran_stub",
            ExtractionStatus::Disabled => "disabled",
            ExtractionStatus::CallerSupplied => "skipped_facts_json",
            ExtractionStatus::SkippedNoKey => "skipped_no_openrouter_key",
            ExtractionStatus::Failed(_) => "failed",
        }
    }

    /// The one sentence a caller needs when facts did NOT get captured.
    fn note(&self, kb: &str) -> Option<String> {
        match self {
            ExtractionStatus::SkippedNoKey => Some(format!(
                "fact extraction skipped: {}",
                crate::fact_llm::no_llm_hint()
            )),
            ExtractionStatus::Failed(e) => Some(format!(
                "fact extraction failed ({e}); the memory is stored — retry with \
                 `engram facts backfill --kb {kb}`"
            )),
            _ => None,
        }
    }
}

/// Parse `--event-time`. Strict: a typo must not silently become "now".
pub fn parse_event_time(raw: &Option<String>) -> Result<Option<DateTime<Utc>>, CliError> {
    match raw.as_deref() {
        None => Ok(None),
        Some(s) => parse_moment(s).map(Some).ok_or_else(|| {
            CliError::BadInput(format!(
                "--event-time '{s}' is not a date: expected RFC3339 \
                 (2026-03-01T09:30:00Z) or YYYY-MM-DD"
            ))
        }),
    }
}

pub async fn run(ctx: &AppContext, args: RememberArgs) -> Result<(), CliError> {
    let RememberArgs {
        content,
        importance,
        tags,
        diary,
        kb,
        no_extract_facts,
        force_extract_facts,
        allow_duplicate,
        autonomy,
        facts_json,
        event_time,
    } = args;
    let autonomy = Autonomy::parse(&autonomy)?;
    let event_time = parse_event_time(&event_time)?;
    // Paid calls (embedding, extraction) respect the KB budget guardrail.
    crate::commands::budget::enforce_budget(ctx, Some(&kb))?;
    if content.trim().is_empty() {
        return Err(CliError::BadInput("content cannot be empty".into()));
    }
    if importance > 10 {
        return Err(CliError::BadInput("importance must be 0..=10".into()));
    }
    // Caller-supplied triples are validated before anything is written, so a
    // typo in --facts-json cannot leave a memory stored but factless.
    let mut extracted: Vec<ExtractedFact> = match &facts_json {
        Some(raw) => serde_json::from_str::<Vec<ExtractedFact>>(raw).map_err(|e| {
            CliError::BadInput(format!("--facts-json is not a valid fact array: {e}"))
        })?,
        None => Vec::new(),
    };

    let start = Instant::now();

    // Dedup: saving an identical fact into the same (kb, diary) is a no-op
    // rather than a second row + a second embedding charge. Opt out with
    // --allow-duplicate if you truly want a second copy.
    if !allow_duplicate {
        if let Some(existing) = ctx.store.find_duplicate_memory(&kb, &diary, &content)? {
            let mut meta = Metadata::default();
            meta.add("deduplicated", true);
            meta.add("existing_id", existing.to_string());
            print_success(
                ctx.format,
                json!({
                    "id": existing.to_string(),
                    "stored": false,
                    "deduplicated": true,
                    "kb": kb,
                    "reason": "identical content already stored in this kb/diary; nothing re-embedded",
                }),
                meta,
                |_| {
                    println!(
                        "Duplicate of {existing} — nothing stored (use --allow-duplicate to force)"
                    )
                },
            );
            return Ok(());
        }
    }

    let now = Utc::now();
    let memory = Memory {
        id: Uuid::new_v4(),
        content: content.clone(),
        created_at: now,
        // When the content actually happened. `recall --since/--until` filters
        // on COALESCE(event_time, created_at), so this is what makes "what
        // happened last March" answerable.
        event_time,
        importance,
        emotional_weight: 0,
        access_count: 0,
        last_accessed: None,
        stability: 1.0,
        source: MemorySource::Manual,
        diary: diary.clone(),
        // A stored fact is valid from the moment it happened, or from the
        // moment it is recorded when no event time is given; superseding
        // facts close this window later. Keeps the temporal model populated
        // instead of leaving valid_from perpetually NULL.
        valid_from: Some(event_time.unwrap_or(now)),
        valid_until: None,
        tags,
    };
    ctx.store.insert_memory_with_kb(&memory, &kb)?;

    let chunks = naive_split(&content);
    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

    // Embed every chunk. Gemini if available (env or config file), stub otherwise.
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let have_gemini = !force_stub && gemini_key.is_some();
    let (embeddings, model_name, dims, prompt_format): (Vec<Vec<f32>>, String, usize, String) =
        if have_gemini {
            let e = GeminiEmbedder::new(gemini_key.unwrap());
            let v = e
                .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await?;
            (v, e.model(), e.dimensions(), e.prompt_format().to_string())
        } else {
            let e = StubEmbedder::default();
            let v = e
                .embed_batch(&chunk_texts, TaskMode::RetrievalDocument)
                .await?;
            (v, e.model(), e.dimensions(), e.prompt_format().to_string())
        };

    for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
        ctx.store.insert_chunk_with_embedding_meta(
            Uuid::new_v4(),
            memory.id,
            &chunk.text,
            chunk.position,
            chunk.section.as_deref(),
            emb,
            &model_name,
            dims,
            &prompt_format,
            None,
        )?;
    }
    if have_gemini && !chunk_texts.is_empty() {
        let tokens = estimated_tokens_for_texts(&chunk_texts);
        ctx.store.record_usage_event(
            "gemini",
            "remember_embed",
            Some(&model_name),
            Some(&kb),
            Some(&diary),
            1,
            chunk_texts.len() as i64,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({
                "dimensions": dims,
                "prompt_format": prompt_format.clone(),
                "estimated": true,
            }),
        )?;
    }

    // Fact processing (extraction and/or caller-supplied triples) runs after
    // the memory is safely persisted so a fact failure never blocks `remember`.
    let llm = crate::fact_llm::resolve()?;
    let status = if no_extract_facts {
        ExtractionStatus::Disabled
    } else if facts_json.is_some() && !force_extract_facts {
        // --facts-json means the caller already did the extraction. Honoring
        // that keeps an explicit triple from also buying an LLM call.
        // `--extract-facts` opts back into both, as it did before.
        ExtractionStatus::CallerSupplied
    } else {
        match &llm {
            Some(llm) => {
                match extract_into(ctx, llm, &content, &kb, &diary, memory.id, &mut extracted).await
                {
                    Ok(()) if llm.is_live() => ExtractionStatus::Ran,
                    Ok(()) => ExtractionStatus::RanStub,
                    Err(e) => ExtractionStatus::Failed(e),
                }
            }
            None => ExtractionStatus::SkippedNoKey,
        }
    };

    let mut outcome = FactWriteOutcome::default();
    if !extracted.is_empty() {
        // The novelty gate uses the same embedder family as the chunks above.
        let gate_embedder: Option<Box<dyn Embedder>> = if have_gemini {
            crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini")
                .map(|k| Box::new(GeminiEmbedder::new(k)) as Box<dyn Embedder>)
        } else {
            Some(Box::new(StubEmbedder::default()) as Box<dyn Embedder>)
        };
        let write_ctx = FactWriteCtx {
            memory_id: memory.id,
            diary: &diary,
            kb: &kb,
            event_time,
            embedder: gate_embedder.as_deref(),
            llm: llm.as_ref().map(|l| l.as_chat()),
            autonomy,
            source_kind: SourceKind::AgentExtracted,
        };
        for ef in &extracted {
            if let Err(e) = write_fact(ctx, &write_ctx, ef, &mut outcome).await {
                tracing::warn!("fact write failed for {:?}: {e}", ef);
            }
        }
    }
    let facts_added = outcome.added;
    let conflicts = outcome.conflicts;
    let adjudication_needed = outcome.queued;
    let facts_deduped = outcome.deduped;

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("memory_id", memory.id.to_string());
    meta.add("chunks_stored", chunks.len());
    meta.add("embedder", model_name.clone());
    meta.add("embed_dimensions", dims);
    meta.add("embed_prompt_format", prompt_format.clone());
    meta.add("kb", kb.clone());
    meta.add("event_time", event_time.map(|t| t.to_rfc3339()));
    meta.add("facts_added", facts_added);
    meta.add("facts_deduped", facts_deduped);
    meta.add("conflicts_detected", conflicts.len());
    meta.add("conflicts_queued", adjudication_needed.len());
    meta.add("fact_extraction", status.as_str());
    let note = status.note(&kb);
    if let Some(note) = &note {
        meta.add("fact_extraction_note", note.clone());
        // Loud, not silent: agents read JSON, humans read stderr.
        if !ctx.quiet {
            eprintln!("note: {note}");
        }
    }

    print_success(
        ctx.format,
        json!({
            "id": memory.id,
            "stored": true,
            "chunks": chunks.len(),
            "kb": kb,
            "event_time": event_time.map(|t| t.to_rfc3339()),
            "fact_extraction": status.as_str(),
            "fact_extraction_note": note,
            "facts_added": facts_added,
            "facts_deduped": facts_deduped,
            "conflicts": conflicts,
            "adjudication_needed": adjudication_needed,
        }),
        meta,
        |_| {
            println!(
                "Stored memory {} ({} chunks, {} facts, {} conflicts, {} queued for resolution)",
                memory.id,
                chunks.len(),
                facts_added,
                conflicts.len(),
                adjudication_needed.len()
            )
        },
    );
    Ok(())
}

/// Run one extraction call, append the triples, and record the attempt.
/// An extraction failure is reported, never fatal — the memory is already
/// stored and losing facts must not lose the memory.
async fn extract_into(
    ctx: &AppContext,
    llm: &FactLlm,
    content: &str,
    kb: &str,
    diary: &str,
    memory_id: Uuid,
    out: &mut Vec<ExtractedFact>,
) -> Result<(), String> {
    let model = llm.model();
    if llm.is_live() {
        if let Err(e) = ctx.store.record_usage_event(
            "openrouter",
            "remember_fact_extract",
            Some(&model),
            Some(kb),
            Some(diary),
            1,
            1,
            estimated_tokens(content),
            0,
            0.0,
            0.0,
            json!({ "estimated": true }),
        ) {
            tracing::warn!("could not record fact extraction usage: {e}");
        }
    }
    match extract_facts(llm.as_chat(), content).await {
        Ok(more) => {
            let n = more.len();
            out.extend(more);
            // Mark the memory processed even when it yielded nothing, so
            // `facts backfill` never re-pays for a memory with no facts in it.
            if let Err(e) =
                ctx.store
                    .record_fact_extraction(memory_id, Some(&model), n as i64, "remember")
            {
                tracing::warn!("could not record fact extraction marker: {e}");
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!("fact extraction failed (continuing without facts): {e}");
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_time_accepts_rfc3339_and_iso_date() {
        let d = parse_event_time(&Some("2026-03-01".into()))
            .unwrap()
            .unwrap();
        assert_eq!(d.to_rfc3339(), "2026-03-01T00:00:00+00:00");
        assert!(parse_event_time(&Some("2026-03-01T09:30:00Z".into()))
            .unwrap()
            .is_some());
        assert!(parse_event_time(&None).unwrap().is_none());
    }

    #[test]
    fn event_time_rejects_garbage_instead_of_defaulting_to_now() {
        let err = parse_event_time(&Some("last tuesday".into())).unwrap_err();
        assert!(matches!(err, CliError::BadInput(_)));
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn extraction_status_notes_every_case_where_facts_were_lost() {
        assert!(ExtractionStatus::Ran.note("personal").is_none());
        assert!(ExtractionStatus::RanStub.note("personal").is_none());
        assert!(ExtractionStatus::Disabled.note("personal").is_none());
        assert!(ExtractionStatus::CallerSupplied.note("personal").is_none());

        let skipped = ExtractionStatus::SkippedNoKey.note("personal").unwrap();
        assert!(skipped.contains("OPENROUTER_API_KEY"));
        assert!(skipped.contains("facts backfill"));

        let failed = ExtractionStatus::Failed("429 rate limited".into())
            .note("personal")
            .unwrap();
        assert!(failed.contains("429 rate limited"));
        assert!(failed.contains("facts backfill --kb personal"));
    }
}
