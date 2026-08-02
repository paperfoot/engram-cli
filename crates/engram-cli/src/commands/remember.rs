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
//! The two doors stay apart all the way to the row: a `--facts-json` triple is
//! stored as `user_stated`, extractor output as `agent_extracted`, and a run
//! that does both writes two batches rather than averaging their provenance.
//! Until this split, nothing in production could produce `user_stated` at all.
//!
//! Every fact then runs the write-path v2 pipeline in `fact_write`: predicate
//! and subject canonicalization, exact-match dedup/supersession, slot
//! policies, and the embedding novelty gate. Uncertain contradictions are
//! queued for the calling agent under the default `assisted` autonomy —
//! see `facts resolve`.

use crate::commands::usage::estimated_tokens;
use crate::context::AppContext;
use crate::error::CliError;
use crate::fact_llm::FactLlm;
use crate::fact_write::{
    write_fact_batch, Autonomy, FactBatch, FactBatchError, FactExtractionMarker, FactWriteCtx,
    FactWriteOutcome, MemoryTime,
};
use crate::output::{print_success, Metadata};
use crate::retrieval::{resolve_write_embedder, WriteEmbedder};
use chrono::{DateTime, Utc};
use engram_core::temporal::parse_moment;
use engram_core::types::{Memory, MemorySource, SourceKind};
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{extract_facts, ExtractedFact};
use engram_ingest::memory::chunk_memory;
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
    /// `--offline`: embed with the deterministic stub instead of Gemini.
    pub offline: bool,
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
    /// The facts could not be persisted, so NOTHING was — a batch is one unit
    /// of work. The memory is stored and left unprocessed, so `facts backfill`
    /// retries it.
    FailedPersist {
        index: usize,
        total: usize,
        detail: String,
    },
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
            // Renamed from `failed_partial_persist`: a partial persist is
            // exactly what can no longer happen.
            ExtractionStatus::FailedPersist { .. } => "failed_persist",
        }
    }

    /// Whether extraction actually ran (and therefore whether a "done" marker
    /// would be meaningful).
    fn extraction_ran(&self) -> bool {
        matches!(self, ExtractionStatus::Ran | ExtractionStatus::RanStub)
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
            ExtractionStatus::FailedPersist {
                index,
                total,
                detail,
            } => Some(format!(
                "{} ({detail}); a fact batch is one unit of work, so NO fact from this memory \
                 was stored — the memory is stored and left UNPROCESSED, and `engram facts \
                 backfill --kb {kb}` will retry it",
                FactBatchError::render_what_failed(*index, *total)
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

/// Test seam: make the embedding call fail on demand.
///
/// Set to any non-empty message and `remember` (or `edit`) fails exactly where
/// a network embed would — after chunking, after the existence/budget gates,
/// before any spend and before any database write. There is no other way to
/// reproduce a failed embed offline, and "a committed memory with no chunks" is
/// precisely the state these commands must never be able to reach: dense recall
/// can never return such a memory, and the exact-content dedup turns every
/// re-save into a successful-looking no-op, so the content is lost while the
/// CLI reports success. Same shape as `ENGRAM_FACT_WRITE_FAIL`; never set in
/// production use.
pub const EMBED_FAIL_ENV: &str = "ENGRAM_EMBED_FAIL";

pub(crate) fn injected_embed_failure(command: &str) -> Option<CliError> {
    let raw = std::env::var(EMBED_FAIL_ENV).ok()?;
    let message = raw.trim();
    if message.is_empty() {
        return None;
    }
    Some(CliError::Transient(format!(
        "{EMBED_FAIL_ENV} injected an embedding failure ({message}); nothing was written — \
         re-run `engram {command}` once the embedder is reachable"
    )))
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
        offline,
    } = args;
    let autonomy = Autonomy::parse(&autonomy)?;
    crate::commands::validate_diary_for_write(&diary)?;
    let event_time = parse_event_time(&event_time)?;
    if content.trim().is_empty() {
        return Err(CliError::BadInput("content cannot be empty".into()));
    }
    if importance > 10 {
        return Err(CliError::BadInput("importance must be 0..=10".into()));
    }
    // Caller-supplied triples are validated before anything is written, so a
    // typo in --facts-json cannot leave a memory stored but factless. They are
    // kept in their OWN vector from here on: a triple an agent asserted and a
    // triple an extractor guessed are different claims, and merging them into
    // one batch is what made `SourceKind::UserStated` unreachable in
    // production — and the classifier's `correction_by_user` rule dead code.
    let caller_facts: Vec<ExtractedFact> = match &facts_json {
        Some(raw) => serde_json::from_str::<Vec<ExtractedFact>>(raw).map_err(|e| {
            CliError::BadInput(format!("--facts-json is not a valid fact array: {e}"))
        })?,
        None => Vec::new(),
    };
    let mut extracted: Vec<ExtractedFact> = Vec::new();
    // The embedder is chosen — and a missing key refused — BEFORE anything is
    // read or written, so a keyless store can never quietly fill with 64-dim
    // stub vectors that recall will later refuse to search. Deliberately after
    // the argument checks above, so a bad argument is still exit 3 rather than
    // being masked by the environment.
    let write_embedder = resolve_write_embedder("remember", offline)?;

    // The KB budget guards PAID calls: cloud embedding, and LLM extraction /
    // adjudication (which run whenever an OpenRouter key resolves and
    // --no-extract-facts was not passed). A fully offline write spends
    // nothing, so a zero spend cap must not block it — a free operation
    // failing with a budget error is a new failure mode with no money
    // behind it.
    let extraction_may_spend = !no_extract_facts
        && crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
            .is_some();
    if write_embedder.cloud || extraction_may_spend {
        crate::commands::budget::enforce_budget(ctx, Some(&kb))?;
    }

    let start = Instant::now();

    // Dedup: saving an identical fact into the same (kb, diary) is a no-op
    // rather than a second row + a second embedding charge. Opt out with
    // --allow-duplicate if you truly want a second copy.
    if !allow_duplicate {
        if let Some(existing) = ctx.store.find_duplicate_memory(&kb, &diary, &content)? {
            // The CONTENT is already stored; the caller's triples may not be.
            // A `remember --facts-json` whose batch failed is retried with the
            // identical command, and returning here would report success while
            // dropping those triples a second time — the retry the failure
            // message promises has to be the retry that works. Extraction is
            // deliberately NOT run here: this path costs no LLM call, as before.
            let mut outcome = FactWriteOutcome::default();
            let mut caller_batch_committed = false;
            if !caller_facts.is_empty() {
                // Epoch, never now, when the existing memory vanished under
                // us (concurrent forget): a fabricated "now" would date these
                // facts newest-in-store and win recency supersession — the
                // same rule memory_times itself applies to unreadable dates.
                let (stored_event, stored_created) = ctx
                    .store
                    .memory_times(existing)?
                    .unwrap_or((None, chrono::DateTime::UNIX_EPOCH));
                let dedup_llm = crate::fact_llm::resolve()?;
                let plan = FactBatchPlan {
                    memory_id: existing,
                    kb: &kb,
                    diary: &diary,
                    memory_time: MemoryTime {
                        event_time: event_time.or(stored_event),
                        created_at: stored_created,
                    },
                    autonomy,
                    llm: dedup_llm.as_ref(),
                    embedder: write_embedder.embedder.as_ref(),
                    facts: &caller_facts,
                    marker: None,
                    // The caller asserted these, on the retry path exactly as
                    // on the first run: dedup must not downgrade provenance.
                    source_kind: SourceKind::UserStated,
                };
                if let Err(failure) = persist_facts(ctx, plan, &mut outcome).await {
                    return Err(facts_json_failure(failure, existing, &kb));
                }
                caller_batch_committed = true;
            }
            let facts_source_kind = facts_source_kind(caller_batch_committed, false);
            let mut meta = Metadata::default();
            meta.add("deduplicated", true);
            meta.add("existing_id", existing.to_string());
            // The dedup path answers with SOMEONE ELSE'S id, which is exactly
            // where a bare uuid is most likely to be copied into the next
            // command — so it carries the typed handle too.
            meta.add("handle", crate::commands::memory::render_handle(existing));
            meta.add("facts_added", outcome.added);
            meta.add("facts_source_kind", facts_source_kind);
            print_success(
                ctx.format,
                json!({
                    "id": existing.to_string(),
                    "handle": crate::commands::memory::render_handle(existing),
                    "stored": false,
                    "deduplicated": true,
                    "kb": kb,
                    "facts_added": outcome.added,
                    "facts_source_kind": facts_source_kind,
                    "facts_deduped": outcome.deduped,
                    "conflicts": outcome.conflicts,
                    "adjudication_needed": outcome.queued,
                    "date_parse_warnings": outcome.date_warnings,
                    "reason": "identical content already stored in this kb/diary; nothing re-embedded",
                }),
                meta,
                |data| {
                    println!(
                        "Duplicate of {existing} — nothing re-stored ({} facts attached, use \
                         --allow-duplicate to force a second memory)",
                        data["facts_added"]
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

    // CHUNKING. This used to be a bare `naive_split`: paragraph split, no
    // merge, no size cap, no floor. Fed real content (a pasted WhatsApp thread,
    // a doc with date headers) it turned every stray line into its own embedded
    // chunk — a live store reached 986 chunks under 40 characters, and they
    // beat real answers in recall because a tiny chunk is nearly pure signal
    // for the few tokens it holds. `chunk_memory` merges fragments into their
    // neighbours, enforces a floor and a ceiling, and keeps every character.
    let policy = crate::commands::chunks::resolve_policy();
    let chunks = chunk_memory(&content, policy);
    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();

    // Gemini, or the offline stub when `--offline` (or the bench hook) asked
    // for it. There is no third case: a write with no key and no --offline
    // never reaches this line — see the resolution above.
    let WriteEmbedder {
        embedder,
        cloud: have_gemini,
        model: model_name,
        dims,
        prompt_format,
    } = write_embedder;
    // A chunk longer than the provider's window loses its tail with no vector
    // coverage — silently, before this. The cap above makes it unreachable
    // unless someone widened `chunking.max_chars`; if they did, say so.
    let truncation_note = crate::commands::chunks::warn_on_truncation(
        ctx,
        embedder.as_ref(),
        &chunk_texts,
        "remember",
    );
    // NOTHING IS WRITTEN UNTIL THE VECTORS ARE IN HAND. `embed_batch` is a
    // NETWORK call: it can 429, time out, or come back short. Writing the memory
    // row first (as this did) left a committed memory with no chunks whenever it
    // did — dense recall can never return such a memory, and the exact-content
    // dedup above turns every re-save into a successful-looking no-op, so the
    // content is gone for good with the CLI reporting success. Embed, check the
    // arity, and only then open one transaction for the row and its chunks.
    if let Some(injected) = injected_embed_failure("remember") {
        return Err(injected);
    }
    let embeddings = match embedder
        .embed_batch_accounted(&chunk_texts, TaskMode::RetrievalDocument)
        .await
    {
        Ok(v) => v,
        Err(failure) => {
            // A Gemini `embed_batch` is SEVERAL billed HTTP requests. If batch
            // 1 was served and batch 2 failed, batch 1 is on the invoice — so
            // it goes on the ledger before this command dies, or the budget
            // guardrail silently loses ground on every failed write.
            if have_gemini && failure.charged_texts > 0 {
                crate::commands::usage::record_embed_spend(
                    &ctx.store,
                    "remember_embed",
                    &model_name,
                    Some(&kb),
                    Some(&diary),
                    &chunk_texts[..failure.charged_texts],
                    failure.charged_requests as i64,
                    json!({
                        "dimensions": dims,
                        "prompt_format": prompt_format.clone(),
                        "estimated": true,
                        "partial": true,
                        "chunks_requested": chunk_texts.len(),
                    }),
                );
            }
            return Err(failure.error.into());
        }
    };
    // The embedding call is paid the moment it returns, whether or not the write
    // below commits, so the usage row is written FIRST and on its own. A failure
    // to record it must not fail the command: the money is already spent, and
    // losing the memory over a bookkeeping row is the worse lie. Same treatment
    // as the extraction usage row in `extract_into`. Deliberately BEFORE the
    // arity check below — a short batch was still served, and still billed.
    if have_gemini {
        crate::commands::usage::record_embed_spend(
            &ctx.store,
            "remember_embed",
            &model_name,
            Some(&kb),
            Some(&diary),
            &chunk_texts,
            1,
            json!({
                "dimensions": dims,
                "prompt_format": prompt_format.clone(),
                "estimated": true,
                "partial": false,
            }),
        );
    }
    if embeddings.len() != chunks.len() {
        return Err(CliError::Transient(format!(
            "embedder returned {} embeddings for {} chunks; nothing was stored, so no chunk is \
             indexed without a vector — retry `engram remember` with the same content",
            embeddings.len(),
            chunks.len(),
        )));
    }

    // ONE unit of work: the memory row, the KB registration it implies, and
    // every chunk land together or not at all.
    let chunks_stored = ctx.store.with_tx(|store| {
        store.insert_memory_with_kb(&memory, &kb)?;
        for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
            store.insert_chunk_with_embedding_meta(
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
        Ok(chunks.len())
    })?;

    // Fact processing (extraction and/or caller-supplied triples) runs after
    // the memory is safely persisted so a fact failure never blocks `remember`.
    let llm = crate::fact_llm::resolve()?;
    // The model the marker will credit, recorded only if extraction ran.
    let mut extraction_model: Option<String> = None;
    let mut status = if no_extract_facts {
        ExtractionStatus::Disabled
    } else if facts_json.is_some() && !force_extract_facts {
        // --facts-json means the caller already did the extraction. Honoring
        // that keeps an explicit triple from also buying an LLM call.
        // `--extract-facts` opts back into both, as it did before.
        ExtractionStatus::CallerSupplied
    } else {
        match &llm {
            Some(llm) => {
                extraction_model = Some(llm.model());
                match extract_into(
                    ctx,
                    llm,
                    &content,
                    &kb,
                    &diary,
                    event_time.unwrap_or(now),
                    &mut extracted,
                )
                .await
                {
                    Ok(()) if llm.is_live() => ExtractionStatus::Ran,
                    Ok(()) => ExtractionStatus::RanStub,
                    Err(e) => ExtractionStatus::Failed(e),
                }
            }
            None => ExtractionStatus::SkippedNoKey,
        }
    };

    // ONE UNIT OF WORK. `facts backfill` treats a memory as processed only
    // once its MARKER exists (PENDING_EXTRACTION_PREDICATE): the batch and
    // its marker commit together or not at all, so a failed batch leaves the
    // memory pending and a later backfill retries it from scratch. A
    // caller-supplied batch deliberately carries no marker — the extractor
    // has not seen the memory — and stays pending until a backfill runs real
    // extraction over it; dedup absorbs the re-supplied triples.
    let marker = status.extraction_ran().then_some(FactExtractionMarker {
        model: extraction_model.as_deref(),
        source: "remember",
    });
    let mut outcome = FactWriteOutcome::default();
    let mut caller_batch_committed = false;
    let mut extraction_batch_committed = false;
    // The caller's own triples go first, as their own unit of work, stamped
    // `user_stated`. Ordering is deliberate: an extracted triple that
    // contradicts one the caller asserted now meets a `user_stated` incumbent
    // instead of racing it inside a single batch, which is the direction the
    // adjudication rules already assume (the caller's assertion is the
    // stronger claim). No marker — the extractor has not seen this memory, so
    // `facts backfill` must still find it pending.
    if !caller_facts.is_empty() {
        let plan = FactBatchPlan {
            memory_id: memory.id,
            kb: &kb,
            diary: &diary,
            memory_time: MemoryTime {
                event_time,
                created_at: now,
            },
            autonomy,
            llm: llm.as_ref(),
            embedder: embedder.as_ref(),
            facts: &caller_facts,
            marker: None,
            source_kind: SourceKind::UserStated,
        };
        if let Err(failure) = persist_facts(ctx, plan, &mut outcome).await {
            return Err(facts_json_failure(failure, memory.id, &kb));
        }
        caller_batch_committed = true;
    }
    if !extracted.is_empty() || marker.is_some() {
        let plan = FactBatchPlan {
            memory_id: memory.id,
            kb: &kb,
            diary: &diary,
            memory_time: MemoryTime {
                event_time,
                created_at: now,
            },
            autonomy,
            llm: llm.as_ref(),
            embedder: embedder.as_ref(),
            facts: &extracted,
            marker,
            source_kind: SourceKind::AgentExtracted,
        };
        if let Err(failure) = persist_facts(ctx, plan, &mut outcome).await {
            // Keyed on the BATCH, never on the flag. The old guard asked
            // `facts_json.is_some()` because there was only ever one batch;
            // with two, that question hard-errors an EXTRACTION failure on a
            // mixed run and reports it as if the caller's triples — already
            // committed above — had been lost. An extraction failure is soft
            // and reported; a caller-batch failure is fatal, above.
            status = ExtractionStatus::FailedPersist {
                index: failure.index,
                total: failure.total,
                detail: failure.error.to_string(),
            };
        } else {
            extraction_batch_committed = !extracted.is_empty();
        }
    }
    let facts_source_kind = facts_source_kind(caller_batch_committed, extraction_batch_committed);

    let facts_added = outcome.added;
    let conflicts = outcome.conflicts;
    let adjudication_needed = outcome.queued;
    let facts_deduped = outcome.deduped;
    let date_warnings = outcome.date_warnings;

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("memory_id", memory.id.to_string());
    // The typed handle beside the legacy id, so an agent can copy `handle`
    // straight into `edit`/`forget` without having to know which kind of id it
    // is holding.
    meta.add("handle", crate::commands::memory::render_handle(memory.id));
    meta.add("chunks_stored", chunks_stored);
    if let Some(note) = &truncation_note {
        meta.add("embed_truncation_note", note.clone());
    }
    meta.add("embedder", model_name.clone());
    if let Some(note) = embedder.degradation_note() {
        meta.add("embedder_degraded", note.clone());
        eprintln!("warning: {note}");
    }
    meta.add("embed_dimensions", dims);
    meta.add("embed_prompt_format", prompt_format.clone());
    meta.add("kb", kb.clone());
    meta.add("event_time", event_time.map(|t| t.to_rfc3339()));
    meta.add("facts_added", facts_added);
    meta.add("facts_deduped", facts_deduped);
    meta.add("conflicts_detected", conflicts.len());
    meta.add("conflicts_queued", adjudication_needed.len());
    meta.add("date_parse_warnings", date_warnings.len());
    meta.add("fact_extraction", status.as_str());
    meta.add("facts_source_kind", facts_source_kind);
    let note = status.note(&kb);
    if let Some(note) = &note {
        meta.add("fact_extraction_note", note.clone());
        // Loud, not silent: agents read JSON, humans read stderr.
        if !ctx.quiet {
            eprintln!("note: {note}");
        }
    }
    // Loud, not silent: an unreadable date is a fact dated by inference, and
    // the agent that supplied it is the only one who can correct the source.
    if !date_warnings.is_empty() && !ctx.quiet {
        eprintln!(
            "note: {} fact date(s) could not be read and were taken from the memory instead — \
             see data.date_parse_warnings",
            date_warnings.len()
        );
    }

    print_success(
        ctx.format,
        json!({
            "id": memory.id,
            "handle": crate::commands::memory::render_handle(memory.id),
            "stored": true,
            "chunks": chunks_stored,
            "embed_truncation_note": truncation_note,
            "kb": kb,
            "event_time": event_time.map(|t| t.to_rfc3339()),
            "fact_extraction": status.as_str(),
            "fact_extraction_note": note,
            "facts_source_kind": facts_source_kind,
            "facts_added": facts_added,
            "facts_deduped": facts_deduped,
            "date_parse_warnings": date_warnings,
            "conflicts": conflicts,
            "adjudication_needed": adjudication_needed,
        }),
        meta,
        |_| {
            println!(
                "Stored memory {} ({} chunks, {} facts, {} conflicts, {} queued for resolution)",
                memory.id,
                chunks_stored,
                facts_added,
                conflicts.len(),
                adjudication_needed.len()
            )
        },
    );
    Ok(())
}

/// Everything one memory's fact batch needs. Bundled so the duplicate-content
/// path and the main path cannot drift apart on embedder choice, autonomy, or
/// the marker rule.
struct FactBatchPlan<'a> {
    memory_id: Uuid,
    kb: &'a str,
    diary: &'a str,
    memory_time: MemoryTime,
    autonomy: Autonomy,
    llm: Option<&'a FactLlm>,
    /// The SAME embedder the chunks were written with. The novelty gate has to
    /// agree with the index it is gating: `novelty_gate` skips candidates of a
    /// different width, so a gate on another embedder calls every fact novel.
    embedder: &'a dyn Embedder,
    facts: &'a [ExtractedFact],
    marker: Option<FactExtractionMarker<'a>>,
    /// Who claims these triples. `UserStated` for `--facts-json`,
    /// `AgentExtracted` for what the extractor produced — the whole point of
    /// splitting one run into two batches.
    source_kind: SourceKind,
}

/// What provenance actually landed on this memory's facts.
///
/// `None` when no fact batch committed, so the field never claims a provenance
/// for a memory that has no facts. A run whose extraction batch failed reports
/// `user_stated`, not `mixed`: the envelope describes what is in the store.
fn facts_source_kind(caller_committed: bool, extracted_committed: bool) -> Option<&'static str> {
    match (caller_committed, extracted_committed) {
        (true, true) => Some("mixed"),
        (true, false) => Some(SourceKind::UserStated.as_str()),
        (false, true) => Some(SourceKind::AgentExtracted.as_str()),
        (false, false) => None,
    }
}

/// Persist a plan as ONE unit of work.
async fn persist_facts(
    ctx: &AppContext,
    plan: FactBatchPlan<'_>,
    outcome: &mut FactWriteOutcome,
) -> Result<(), FactBatchError> {
    let write_ctx = FactWriteCtx {
        memory_id: plan.memory_id,
        diary: plan.diary,
        kb: plan.kb,
        memory_time: plan.memory_time,
        embedder: Some(plan.embedder),
        llm: plan.llm.map(|l| l.as_chat()),
        autonomy: plan.autonomy,
        source_kind: plan.source_kind,
    };
    let batch = FactBatch {
        facts: plan.facts,
        marker: plan.marker,
    };
    write_fact_batch(ctx, &write_ctx, &batch, outcome).await
}

/// A `--facts-json` batch that rolled back is not a note, it is a failure.
///
/// `facts backfill` re-extracts with an LLM; it never replays triples an agent
/// asserted, so nothing downstream can recover these. The retry that DOES work
/// is the identical command: the duplicate-content path attaches the batch to
/// the memory the content already has.
fn facts_json_failure(failure: FactBatchError, memory_id: Uuid, kb: &str) -> CliError {
    let message = format!(
        "--facts-json: {} ({}); a fact batch is one unit of work, so NO fact from it was \
         stored — memory {memory_id} WAS stored, so re-run the identical `engram remember … \
         --facts-json …` and the facts are attached to it (if this run also extracted, run \
         `engram facts backfill --kb {kb}` first: backfill only retries memories that have no \
         facts yet)",
        failure.what_failed(),
        failure.error
    );
    match failure.error.exit_code() {
        2 => CliError::Config(message),
        4 => CliError::RateLimited(message),
        _ => CliError::Transient(message),
    }
}

/// Run one extraction call and append the triples.
/// An extraction failure is reported, never fatal — the memory is already
/// stored and losing facts must not lose the memory.
///
/// `memory_date` anchors the extraction prompt so relative expressions come
/// back as absolute dates: an extractor that has nothing to resolve "last
/// March" against emits "last March", and no reader downstream may invent a
/// date for it.
///
/// Deliberately does NOT record the "already extracted" marker: that belongs
/// after the facts are persisted, in the caller. See the marker-ordering note
/// in `run`.
async fn extract_into(
    ctx: &AppContext,
    llm: &FactLlm,
    content: &str,
    kb: &str,
    diary: &str,
    memory_date: DateTime<Utc>,
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
    match extract_facts(llm.as_chat(), content, memory_date).await {
        Ok(more) => {
            out.extend(more);
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

        let rolled_back = ExtractionStatus::FailedPersist {
            index: 1,
            total: 3,
            detail: "storage error: disk I/O error".into(),
        }
        .note("personal")
        .unwrap();
        assert!(rolled_back.contains("fact 2 of 3"));
        assert!(rolled_back.contains("NO fact from this memory was stored"));
        assert!(rolled_back.contains("facts backfill --kb personal"));
    }
}
