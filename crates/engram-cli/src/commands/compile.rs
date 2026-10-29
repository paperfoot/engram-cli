//! `engram compile` — deterministic evidence compiler v1.
//!
//! This compiler preserves raw chunks, then derives cited claims, entities,
//! co-occurrence relations, takeaways, and Markdown wiki pages. It is designed
//! to be idempotent and local-first; LLM synthesis can be layered over these
//! same tables later without changing recall.

use crate::commands::usage::estimated_tokens;
use crate::context::AppContext;
use crate::error::CliError;
use crate::fact_write::{
    write_fact, Autonomy, FactEmbedCache, FactWriteCtx, FactWriteOutcome, MemoryTime,
};
use crate::judge::{JudgeMode, JudgeSite};
use crate::output::{print_success, Metadata};
use engram_core::relations::{canonical_relation, cessation_of, relation_spec};
use engram_core::types::SourceKind;
use engram_graph::facts::ExtractedFact;
use engram_llm::judge::lexical::{
    claim_grounded_lexically, fact_grounded_lexically, negated_object, numbers_ok, numeric_tokens,
    word_set,
};
use engram_llm::judge::{ask_many, questions, wire_decimal, Judge, JudgeError, BATCH_CONCURRENCY};
use engram_llm::openrouter::{OpenRouterClient, DEFAULT_EXTRACTION_MODEL, DEFAULT_SYNTHESIS_MODEL};
use engram_llm::{ChatLlm, ChatMessage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompileStats {
    pub kb: String,
    pub chunks_scanned: usize,
    pub source_spans: usize,
    pub claims: usize,
    pub entities: usize,
    /// Entities typed as `person` after this compile.
    #[serde(default)]
    pub people: usize,
    pub relations: usize,
    /// Typed person↔person edges written (introduced_by, friend_of, …).
    #[serde(default)]
    pub person_relations: usize,
    pub takeaways: usize,
    pub wiki_pages: usize,
    pub llm_enabled: bool,
    pub llm_extraction_chunks: usize,
    pub llm_claims: usize,
    #[serde(default)]
    pub llm_claims_rejected: usize,
    #[serde(default)]
    pub llm_facts: usize,
    #[serde(default)]
    pub llm_facts_rejected: usize,
    /// Compiled facts whose contradiction the compiler refused to settle on its
    /// own (safety slots, two papers with no ordering). Reported because a
    /// silent queue is a backlog nobody knows to read.
    #[serde(default)]
    pub llm_facts_queued: usize,
    pub llm_entities: usize,
    pub llm_relations: usize,
    pub llm_takeaways: usize,
    pub llm_wiki_pages: usize,
    pub extraction_model: Option<String>,
    pub synthesis_model: Option<String>,
    /// `judge.grounding` for this compile: `off` | `shadow` | `on`. Null
    /// when the LLM pass did not run.
    #[serde(default)]
    pub judge_mode: Option<String>,
    /// Who answered the grounding judge: `typesafe` | `openrouter` | `stub`,
    /// or `none` when no judge was reachable and the compile ran as if off.
    /// Null when the mode is off.
    #[serde(default)]
    pub judge_route: Option<String>,
    #[serde(default)]
    pub judge_requests: usize,
    #[serde(default)]
    pub judge_input_tokens: u64,
    #[serde(default)]
    pub judge_cost_usd: f64,
    /// Chunks the judge did not decide (its call failed or left an item
    /// unanswered): the lexical rules decided them.
    #[serde(default)]
    pub judge_fallback_chunks: usize,
    /// Judged claims, by whether the lexical rule and the judge's `on` rule
    /// admit them. In shadow they say what `on` would change.
    #[serde(default)]
    pub judge_claims_admitted_by_both: usize,
    #[serde(default)]
    pub judge_claims_rejected_by_both: usize,
    #[serde(default)]
    pub judge_claims_judge_only_admitted: usize,
    #[serde(default)]
    pub judge_claims_lexical_only_admitted: usize,
    /// The same agreement counts for judged facts.
    #[serde(default)]
    pub judge_facts_admitted_by_both: usize,
    #[serde(default)]
    pub judge_facts_rejected_by_both: usize,
    #[serde(default)]
    pub judge_facts_judge_only_admitted: usize,
    #[serde(default)]
    pub judge_facts_lexical_only_admitted: usize,
}

/// What grounding decides about: an extracted claim or an extracted fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Claim,
    Fact,
}

impl CompileStats {
    /// Whether an extracted item is admitted. `lexical` is the judge-free
    /// rule; `judge` the `on` rule, `None` when the judge did not decide the
    /// item's chunk. Where both exist their agreement is counted, in shadow
    /// as in on.
    fn admit(&mut self, item: Item, mode: JudgeMode, lexical: bool, judge: Option<bool>) -> bool {
        let Some(judge) = judge else {
            return lexical;
        };
        let [both, neither, judge_only, lexical_only] = match item {
            Item::Claim => [
                &mut self.judge_claims_admitted_by_both,
                &mut self.judge_claims_rejected_by_both,
                &mut self.judge_claims_judge_only_admitted,
                &mut self.judge_claims_lexical_only_admitted,
            ],
            Item::Fact => [
                &mut self.judge_facts_admitted_by_both,
                &mut self.judge_facts_rejected_by_both,
                &mut self.judge_facts_judge_only_admitted,
                &mut self.judge_facts_lexical_only_admitted,
            ],
        };
        *match (lexical, judge) {
            (true, true) => both,
            (false, false) => neither,
            (false, true) => judge_only,
            (true, false) => lexical_only,
        } += 1;
        if mode == JudgeMode::On {
            judge
        } else {
            lexical
        }
    }
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
    // Abandon and insert in ONE transaction. The legacy `create_compile_job`
    // clears only `running` rows, so a `queued` row left by `jobs submit
    // --detach` would make this insert fail on the partial unique index with a
    // raw SQL message. Widening the abandon here keeps the synchronous compile
    // working and keeps "at most one live job per (kind, kb)" true.
    let job_id = ctx.with_store_tx(|s| {
        s.abandon_live_jobs(
            "compile",
            &kb,
            "superseded by a synchronous compile of the same KB",
        )?;
        Ok(s.create_compile_job(
            &kb,
            "evidence",
            json!({
                "llm": options.llm,
                "extraction_model": options.extraction_model,
                "synthesis_model": options.synthesis_model,
                "max_llm_chunks": options.max_llm_chunks,
            }),
        )?)
    })?;
    let stats = match compile_kb_with_options(ctx, &kb, options.clone()).await {
        Ok(stats) => {
            ctx.store
                .finish_compile_job(job_id, "completed", None, json!({ "stats": stats }))?;
            stats
        }
        Err(err) => {
            // Bookkeeping must never mask the failure it is recording: if the
            // job row was already abandoned by a newer compile, say so and
            // still return the error the user actually needs to see.
            if let Err(bookkeeping) = ctx.store.finish_compile_job(
                job_id,
                "failed",
                Some(&err.to_string()),
                json!({
                    "llm": options.llm,
                    "extraction_model": options.extraction_model,
                    "synthesis_model": options.synthesis_model,
                }),
            ) {
                tracing::warn!("could not mark compile job {job_id} failed: {bookkeeping}");
            }
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

// ---------------------------------------------------------------------------
// Compile as a job
// ---------------------------------------------------------------------------

/// The runner behind `engram jobs submit --kind compile`, `POST /v1/jobs` and
/// the sleep-time trigger.
///
/// It wraps [`compile_kb_with_options`], which is still monolithic: the
/// build-then-swap is one transaction and there is no mid-compile checkpoint,
/// so this runner returns `Completed` or an error and never `Paused`. Stated
/// rather than papered over — an unresumable job that says so is honest, and a
/// job reporting `paused` with no cursor would tell `jobs resume` there is
/// something to continue from when there is not.
pub struct CompileRunner;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CompileParams {
    #[serde(default)]
    pub llm: bool,
    #[serde(default)]
    pub extraction_model: Option<String>,
    #[serde(default)]
    pub synthesis_model: Option<String>,
    #[serde(default)]
    pub max_llm_chunks: Option<usize>,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// `manual` | `sleep_time` | `api`. Reported by `jobs list`, so a user can
    /// see which compiles they asked for and which the daemon decided on.
    #[serde(default)]
    pub trigger: Option<String>,
}

impl CompileParams {
    pub fn into_options(self) -> CompileOptions {
        CompileOptions {
            llm: self.llm,
            extraction_model: self.extraction_model.unwrap_or_else(|| {
                crate::commands::config::resolve_setting(
                    "ENGRAM_COMPILER_EXTRACTION_MODEL",
                    "compiler.extraction_model",
                    DEFAULT_EXTRACTION_MODEL,
                )
            }),
            synthesis_model: self.synthesis_model.unwrap_or_else(|| {
                crate::commands::config::resolve_setting(
                    "ENGRAM_COMPILER_SYNTHESIS_MODEL",
                    "compiler.synthesis_model",
                    DEFAULT_SYNTHESIS_MODEL,
                )
            }),
            max_llm_chunks: self.max_llm_chunks,
        }
    }
}

impl crate::engine::JobRunner for CompileRunner {
    fn kind(&self) -> &'static str {
        "compile"
    }

    fn validate(&self, params: serde_json::Value) -> Result<serde_json::Value, CliError> {
        let parsed: CompileParams =
            serde_json::from_value(params).map_err(|e| CliError::MalformedJson {
                field: "params",
                detail: e.to_string(),
            })?;
        Ok(serde_json::to_value(parsed)?)
    }

    fn run<'a>(
        &'a self,
        jc: &'a crate::engine::JobCtx,
        params: &'a serde_json::Value,
        _cursor: &'a serde_json::Value,
    ) -> crate::engine::jobs::JobFuture<'a> {
        Box::pin(async move {
            let parsed: CompileParams = serde_json::from_value(params.clone()).unwrap_or_default();
            let stats = compile_kb_with_options(jc.app(), &jc.kb, parsed.into_options()).await?;
            let stats_json = serde_json::to_value(&stats)?;
            // `compile.ran`, in the transaction of the write it describes. A
            // paused compile appends nothing, which is correct: the derived
            // layer was not swapped.
            let kb = jc.kb.clone();
            let job_id = jc.id;
            let payload = stats_json.clone();
            jc.core
                .write("compile.ran", move |w| {
                    w.store.append_event(
                        &engram_storage::journal::EventDraft::new(
                            engram_core::events::EventKind::CompileRan,
                            w.actor,
                            json!({ "kb": kb, "job_id": job_id, "stats": payload }),
                        )
                        .subject(job_id)
                        .scope(Some(&kb), None),
                    )?;
                    Ok(())
                })
                .await?;
            Ok(crate::engine::JobOutcome::Completed { stats: stats_json })
        })
    }
}

pub async fn compile_kb_with_options(
    ctx: &AppContext,
    kb: &str,
    options: CompileOptions,
) -> Result<CompileStats, CliError> {
    ctx.store.ensure_kb(kb, None)?;
    let mut stats = CompileStats {
        kb: kb.to_string(),
        llm_enabled: options.llm,
        extraction_model: options.llm.then_some(options.extraction_model.clone()),
        synthesis_model: options.llm.then_some(options.synthesis_model.clone()),
        ..Default::default()
    };

    // Every provider call happens first, with no transaction open. The write
    // lock used to be held across one extraction call per chunk plus the
    // synthesis call (up to ~16 min per call under the provider retries),
    // and every other writer failed on its 5 s busy_timeout meanwhile. A
    // provider error still aborts the compile, now before anything is written.
    let gathered = if options.llm {
        Some(gather_llm(ctx, kb, &options, &mut stats).await?)
    } else {
        None
    };

    // Build-then-swap: clear + rebuild happen inside ONE transaction. Under
    // WAL, concurrent readers keep seeing the previous compilation until
    // commit; if the rebuild fails partway, rollback restores the previous
    // good compilation instead of leaving the KB half-wiped.
    ctx.store.begin_immediate()?;
    match compile_within_tx(ctx, kb, gathered, &mut stats).await {
        Ok(()) => ctx.store.commit()?,
        Err(e) => {
            let _ = ctx.store.rollback();
            return Err(e);
        }
    }
    Ok(stats)
}

async fn compile_within_tx(
    ctx: &AppContext,
    kb: &str,
    gathered: Option<LlmGathered>,
    stats: &mut CompileStats,
) -> Result<(), CliError> {
    ctx.store.clear_derived_for_kb(kb)?;
    // Listed inside the transaction: the LLM gather may have run for minutes,
    // and a memory forgotten meanwhile must not get claims or entities.
    let chunks = ctx.store.list_chunks_for_kb(kb)?;
    stats.chunks_scanned = chunks.len();

    let mut entity_ids: HashMap<String, uuid::Uuid> = HashMap::new();
    let mut entity_display: HashMap<String, String> = HashMap::new();
    let mut entity_mentions: BTreeMap<String, usize> = BTreeMap::new();
    let mut doc_takeaway_done: HashSet<Option<uuid::Uuid>> = HashSet::new();
    let mut span_by_chunk: HashMap<uuid::Uuid, uuid::Uuid> = HashMap::new();

    // KB-WIDE PERSON PASS. One chunk cannot tell "Noemi" from "Noemi
    // Varkoni" — which is how the real store ended up with the same human
    // twice (Noemi 106 / Noemi Varkoni 66, Avi 52 / Avi Roy 25, Patrick 57 /
    // Patrick Ashcomb Pruett). So scan every chunk once to learn the KB's full
    // names, then analyze each chunk again with that index: a bare first name
    // folds into the one full name it can belong to, and stays its own node
    // when two full names compete for it (see `PersonIndex`).
    let person_index = {
        let mut builder = engram_graph::people::PersonIndexBuilder::default();
        for chunk in &chunks {
            // Each extraction is folded in and dropped: only the name/kind
            // aggregate is kept, never every chunk's edges and sentences.
            builder.add(&engram_graph::people::analyze(&chunk.content));
        }
        builder.finish()
    };

    for chunk in &chunks {
        // Chunk-level span (no char offset — used by the LLM path, which
        // attributes claims to the whole chunk).
        let preview: String = chunk.content.chars().take(500).collect();
        let chunk_span_id = ctx.store.insert_source_span(
            kb,
            chunk.document_id,
            Some(chunk.chunk_id),
            chunk.section.as_deref(),
            chunk.source.as_deref(),
            &preview,
            None,
            None,
            None,
        )?;
        stats.source_spans += 1;
        span_by_chunk.insert(chunk.chunk_id, chunk_span_id);

        // Person-aware extraction: hygienic entity surfaces (no contractions,
        // possessives folded onto their owner, no greetings or contact labels
        // standing up rival nodes), person typing, and typed person↔person
        // edges — all deterministic and offline, now resolved KB-wide.
        let extraction = engram_graph::people::analyze_with_index(&chunk.content, &person_index);
        let mut local_entities = Vec::new();
        let mut kept_other = 0usize;
        let mut kept_people = 0usize;
        for mention in extraction.entities.iter() {
            // People get their own budget: a chunk dense in domain terms must
            // not push the humans out of the graph (and with them their
            // relationship edges, which need both endpoints present).
            let is_person = mention.kind == engram_graph::people::KIND_PERSON;
            if is_person {
                if kept_people >= MAX_PEOPLE_PER_CHUNK {
                    continue;
                }
                kept_people += 1;
            } else {
                if kept_other >= MAX_ENTITIES_PER_CHUNK {
                    continue;
                }
                kept_other += 1;
            }
            let id = ctx.store.upsert_entity_with_aliases(
                kb,
                &mention.name,
                mention.kind,
                mention.mentions as u32,
                &mention.aliases,
            )?;
            // Key on the case-folded name so a recompile counts one entity per
            // real-world thing, whatever casing each chunk happened to use.
            let key = mention.name.to_lowercase();
            let display = entity_display
                .entry(key.clone())
                .or_insert_with(|| mention.name.clone())
                .clone();
            entity_ids.insert(key, id);
            *entity_mentions.entry(display).or_insert(0) += mention.mentions;
            local_entities.push((mention.name.clone(), id));
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
                    Some(chunk_span_id),
                )?;
                stats.relations += 1;
            }
        }

        // Typed relationship edges. Each carries the sentence it was read
        // from plus the chunk/memory/document ids, so `people show` can prove
        // every edge instead of asserting it.
        let by_name: HashMap<String, uuid::Uuid> = local_entities
            .iter()
            .map(|(name, id)| (name.to_lowercase(), *id))
            .collect();
        for edge in &extraction.edges {
            let (Some(from_id), Some(to_id)) = (
                by_name.get(&edge.from.to_lowercase()),
                by_name.get(&edge.to.to_lowercase()),
            ) else {
                continue;
            };
            if from_id == to_id {
                continue;
            }
            let mut source = json!({
                "chunk_id": chunk.chunk_id.to_string(),
                "memory_id": chunk.memory_id.to_string(),
                "sentence": edge.sentence,
                "cue": edge.cue,
            });
            if let Some(document_id) = chunk.document_id {
                source["document_id"] = json!(document_id.to_string());
            }
            if let Some(context) = &edge.context {
                source["context"] = json!(context);
            }
            ctx.store.insert_relation_with_source(
                kb,
                *from_id,
                *to_id,
                edge.predicate,
                1.0,
                source,
                Some(chunk_span_id),
            )?;
            stats.relations += 1;
            stats.person_relations += 1;
        }

        // Deterministic claims: real sentence segmentation (never splits a
        // decimal / p-value / abbreviation), each stored with a PRECISE source
        // span (char offsets into the chunk) so a citation resolves to the
        // exact supporting sentence, not the whole chunk.
        for sent in evidence_sentences(&chunk.content).into_iter().take(3) {
            let level = infer_evidence_level(&sent.text);
            let claim_span_id = ctx.store.insert_source_span(
                kb,
                chunk.document_id,
                Some(chunk.chunk_id),
                chunk.section.as_deref(),
                chunk.source.as_deref(),
                &sent.text,
                Some(sent.start),
                Some(sent.end),
                None,
            )?;
            stats.source_spans += 1;
            ctx.store.insert_claim(
                kb,
                &sent.text,
                level,
                0.72,
                Some(claim_span_id),
                Some(chunk.chunk_id),
                chunk.document_id,
            )?;
            stats.claims += 1;
        }

        if doc_takeaway_done.insert(chunk.document_id) {
            if let Some(takeaway) = first_sentence(&chunk.content) {
                ctx.store.insert_takeaway(
                    kb,
                    chunk.document_id,
                    &takeaway.text,
                    infer_evidence_level(&chunk.content),
                    json!([{ "chunk_id": chunk.chunk_id.to_string(), "source": chunk.source }]),
                )?;
                stats.takeaways += 1;
            }
        }
    }

    if let Some(gathered) = gathered {
        apply_llm_evidence(
            ctx,
            kb,
            &chunks,
            &span_by_chunk,
            gathered,
            &mut entity_mentions,
            stats,
        )
        .await?;
    }

    // Distinct ROWS, not distinct spellings: two spellings of one human that
    // the storage layer merged are one entity, and the report must say so.
    stats.entities = entity_ids.values().collect::<HashSet<_>>().len();
    stats.people = ctx
        .store
        .list_entities_by_kind(Some(kb), engram_graph::people::KIND_PERSON, usize::MAX, 1)?
        .len();
    stats.wiki_pages = write_wiki(ctx, kb, &chunks, &entity_mentions)?;
    Ok(())
}

/// Non-person entities kept per chunk. The chunker caps chunks at ~1800
/// chars, so this is generous for personal notes while bounding the
/// co-occurrence fan-out on dense scientific text.
const MAX_ENTITIES_PER_CHUNK: usize = 16;

/// People kept per chunk, budgeted separately. A note listing everyone at a
/// dinner is exactly the note a networker cares about most.
const MAX_PEOPLE_PER_CHUNK: usize = 32;

/// One usage row per paid compiler call, written as soon as it is paid for:
/// no transaction is open during the gather phase, so a compile that fails
/// later still accounts for the tokens it spent.
fn record_compiler_usage(
    ctx: &AppContext,
    kb: &str,
    operation: &str,
    response: &engram_llm::ChatResponse,
    prompt: &str,
) {
    let input = response
        .prompt_tokens
        .map(i64::from)
        .unwrap_or_else(|| estimated_tokens(prompt));
    let output = response.completion_tokens.map(i64::from).unwrap_or(0);
    if let Err(e) = ctx.store.record_usage_event(
        "openrouter",
        operation,
        Some(&response.model),
        Some(kb),
        None,
        1,
        1,
        input,
        output,
        0.0,
        0.0,
        json!({ "estimated_cost": false }),
    ) {
        tracing::warn!("failed to record compiler usage event: {e}");
    }
}

/// Test seam: a scripted, offline compiler LLM — same shape and precedence as
/// `ENGRAM_FACT_LLM_STUB` (see [`crate::fact_llm`]).
///
/// The compiler was the last LLM surface with no offline seam, so the write
/// path compiled facts take could only be exercised by paying a provider — and
/// was therefore never exercised at all. Never set in production use.
pub const COMPILER_STUB_ENV: &str = "ENGRAM_COMPILER_LLM_STUB";

/// (extraction client, synthesis client, `true` when the calls are paid).
type CompilerLlms = (Box<dyn ChatLlm>, Box<dyn ChatLlm>, bool);

fn resolve_compiler_llms(options: &CompileOptions) -> Result<CompilerLlms, CliError> {
    if let Some(script) = std::env::var(COMPILER_STUB_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let build = || {
            engram_llm::stub::ScriptedChatLlm::parse(&script).map_err(|e| {
                CliError::Config(format!("{COMPILER_STUB_ENV} is not a valid script: {e}"))
            })
        };
        return Ok((Box::new(build()?), Box::new(build()?), false));
    }
    let key = crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter")
        .ok_or_else(|| {
            CliError::Config("OPENROUTER_API_KEY is required for `engram compile --llm`".into())
        })?;
    Ok((
        Box::new(
            OpenRouterClient::new(key.clone())
                .with_model(options.extraction_model.clone())
                .with_max_tokens(4096),
        ),
        Box::new(
            OpenRouterClient::new(key)
                .with_model(options.synthesis_model.clone())
                // The default synthesis model (gemini-3.8-flash) is a
                // REASONING model: its thinking tokens bill against this
                // completion budget before a word of the wiki page is
                // written. 8192 was sized for a non-reasoning pro model and
                // could truncate the overview mid-section; the extra
                // headroom costs nothing unless used.
                .with_max_tokens(16384),
        ),
        true,
    ))
}

/// What the network phase of an LLM compile produced, for the write
/// transaction to apply.
struct LlmGathered {
    /// Parsed extractions, in chunk order.
    extracted: Vec<(uuid::Uuid, LlmEvidence)>,
    /// The synthesis overview, citations not yet validated.
    overview: Option<String>,
    /// The chunks the synthesis call was given: the citations it may make.
    synthesis_chunk_ids: HashSet<uuid::Uuid>,
}

/// Every provider call of an LLM compile, with no transaction open: one
/// extraction per chunk, the grounding judge, the synthesis. Claims and facts
/// come back already grounded.
async fn gather_llm(
    ctx: &AppContext,
    kb: &str,
    options: &CompileOptions,
    stats: &mut CompileStats,
) -> Result<LlmGathered, CliError> {
    // Spend guardrails bind before a provider is chosen, whichever it is.
    crate::commands::budget::enforce_budget(ctx, Some(kb))?;
    // Read before the first paid call: a mistyped mode is exit 2, not a
    // compile that paid for its extractions and then died.
    let mode = crate::judge::judge_mode(JudgeSite::Grounding)?;
    let (extraction_llm, synthesis_llm, live) = resolve_compiler_llms(options)?;
    // `off` never resolves a judge, so none of its settings are read.
    let judge = match mode {
        JudgeMode::Off => None,
        JudgeMode::Shadow | JudgeMode::On => crate::judge::resolve_judge(false)?,
    };
    stats.judge_mode = Some(mode.as_str().to_string());
    if mode != JudgeMode::Off {
        stats.judge_route = Some(judge.as_ref().map_or("none", |j| j.name()).to_string());
    }
    let chunks = ctx.store.list_chunks_for_kb(kb)?;
    let max_chunks = options.max_llm_chunks.unwrap_or(chunks.len());
    let in_scope = &chunks[..max_chunks.min(chunks.len())];
    // Extractions run BATCH_CONCURRENCY at a time; the first failure drops
    // the set, which aborts the calls still in flight.
    let extraction_llm: std::sync::Arc<dyn ChatLlm> = extraction_llm.into();
    let mut pending = tokio::task::JoinSet::new();
    let mut responses = Vec::with_capacity(in_scope.len());
    let mut queued = in_scope.iter().enumerate();
    loop {
        while pending.len() < BATCH_CONCURRENCY {
            let Some((i, chunk)) = queued.next() else {
                break;
            };
            let (llm, prompt) = (extraction_llm.clone(), extraction_prompt(kb, chunk));
            pending.spawn(async move {
                let messages = [
                    ChatMessage::system(
                        "Extract cited scientific/domain evidence. Return one JSON object only.",
                    ),
                    ChatMessage::user(prompt.clone()),
                ];
                (i, prompt, llm.chat(&messages).await)
            });
        }
        let Some(joined) = pending.join_next().await else {
            break;
        };
        let (i, prompt, response) =
            joined.map_err(|e| CliError::Transient(format!("compiler extraction failed: {e}")))?;
        let response = response
            .map_err(|e| CliError::Transient(format!("compiler extraction failed: {e}")))?;
        // Recorded as it lands, so a later failure still leaves the spend.
        // A stub spends nothing and must not write an `openrouter` usage row —
        // the budget guard would then count it against a real cap.
        if live {
            record_compiler_usage(ctx, kb, "compiler_extract", &response, &prompt);
        }
        responses.push((i, response));
    }
    responses.sort_by_key(|(i, _)| *i);
    let mut extracted = Vec::new();
    for (i, response) in responses {
        let chunk = &in_scope[i];
        match parse_llm_evidence(&response.content) {
            Ok(evidence) => extracted.push((chunk, tidy(evidence))),
            Err(e) => tracing::warn!(
                "compiler extraction parse failed for {}: {e}",
                chunk.chunk_id
            ),
        }
    }

    let verdicts = match &judge {
        Some(judge) => judge_chunks(ctx, kb, judge.as_ref(), &extracted, stats).await,
        None => extracted.iter().map(|_| None).collect(),
    };
    let extracted = extracted
        .into_iter()
        .zip(verdicts)
        .map(|((chunk, evidence), verdicts)| {
            let evidence = ground_evidence(chunk, evidence, mode, verdicts.as_ref(), stats);
            (chunk.chunk_id, evidence)
        })
        .collect();

    let mut overview = None;
    if !in_scope.is_empty() {
        let synth_chunks: Vec<&engram_storage::ChunkRecord> = in_scope.iter().collect();
        let overview_prompt = synthesis_prompt(kb, &synth_chunks);
        let response = synthesis_llm
            .chat(&[
                ChatMessage::system(
                    "Write concise cited domain wiki pages from retrieved chunks. Preserve chunk citations.",
                ),
                ChatMessage::user(overview_prompt.clone()),
            ])
            .await
            .map_err(|e| CliError::Transient(format!("compiler synthesis failed: {e}")))?;
        if live {
            record_compiler_usage(ctx, kb, "compiler_synthesize", &response, &overview_prompt);
        }
        let content = response.content.trim();
        if !content.is_empty() {
            overview = Some(content.to_string());
        }
    }
    Ok(LlmGathered {
        extracted,
        overview,
        synthesis_chunk_ids: in_scope.iter().map(|c| c.chunk_id).collect(),
    })
}

/// Write what [`gather_llm`] produced, inside the compile transaction.
/// `chunks` is the list read inside it: a chunk whose memory was forgotten
/// while the gather ran gets nothing.
async fn apply_llm_evidence(
    ctx: &AppContext,
    kb: &str,
    chunks: &[engram_storage::ChunkRecord],
    span_by_chunk: &HashMap<uuid::Uuid, uuid::Uuid>,
    gathered: LlmGathered,
    entity_mentions: &mut BTreeMap<String, usize>,
    stats: &mut CompileStats,
) -> Result<(), CliError> {
    let live: HashMap<uuid::Uuid, &engram_storage::ChunkRecord> =
        chunks.iter().map(|c| (c.chunk_id, c)).collect();
    for (chunk_id, evidence) in gathered.extracted {
        let Some(chunk) = live.get(&chunk_id) else {
            continue;
        };
        let span_id = span_by_chunk.get(&chunk_id).copied();
        insert_llm_evidence(ctx, kb, chunk, span_id, evidence, entity_mentions, stats).await?;
        stats.llm_extraction_chunks += 1;
    }

    if let Some(content) = gathered.overview {
        // Validate [chunk:<uuid>] citations: fabricated ones (not in the
        // set we actually gave the model) are stripped so the wiki never
        // shows a citation that can't be re-verified with `recall`.
        let known: HashSet<uuid::Uuid> = gathered
            .synthesis_chunk_ids
            .into_iter()
            .filter(|id| live.contains_key(id))
            .collect();
        let (validated, dropped) = validate_chunk_citations(&content, &known);
        if dropped > 0 {
            tracing::warn!(
                "compiler synthesis: stripped {dropped} fabricated chunk citation(s) for kb {kb}"
            );
        }
        ctx.store.upsert_wiki_page(
            kb,
            "llm/overview.md",
            &format!("{kb} LLM overview"),
            &validated,
        )?;
        stats.llm_wiki_pages += 1;
    }
    Ok(())
}

/// Replace any `[chunk:<uuid>]` citation whose UUID is not in `known` with
/// `[chunk:unverified]`, so the stored wiki never carries a citation that
/// can't be traced back to a real chunk. Returns (text, dropped_count).
fn validate_chunk_citations(text: &str, known: &HashSet<uuid::Uuid>) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut dropped = 0usize;
    let mut rest = text;
    while let Some(pos) = rest.find("[chunk:") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + "[chunk:".len()..];
        if let Some(close) = after.find(']') {
            let id_str = after[..close].trim();
            let ok = uuid::Uuid::parse_str(id_str)
                .map(|u| known.contains(&u))
                .unwrap_or(false);
            if ok {
                out.push_str(&rest[pos..pos + "[chunk:".len() + close + 1]);
            } else {
                out.push_str("[chunk:unverified]");
                dropped += 1;
            }
            rest = &after[close + 1..];
        } else {
            out.push_str(&rest[pos..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    (out, dropped)
}

async fn insert_llm_evidence(
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
        // LLM-proposed names go through the SAME hygiene as the offline path:
        // a model that answers with "She's" or "Elias's" must not be able to
        // create junk nodes the deterministic extractor would have rejected.
        let Some(canonical) = engram_graph::canonicalize_surface(&entity.name) else {
            continue;
        };
        let mut aliases = canonical.aliases.clone();
        for alias in entity.aliases.into_iter().flatten() {
            if let Some(clean) = engram_graph::canonicalize_surface(&alias) {
                if clean.name != canonical.name && !aliases.contains(&clean.name) {
                    aliases.push(clean.name);
                }
            }
        }
        let kind = normalize_llm_kind(entity.kind.as_deref(), &canonical);
        let id = ctx
            .store
            .upsert_entity_with_aliases(kb, &canonical.name, kind, 1, &aliases)?;
        ids.insert(canonical.name.to_lowercase(), id);
        *entity_mentions.entry(canonical.name).or_insert(0) += 1;
        stats.llm_entities += 1;
    }
    // Claims and facts arrive grounded (see `ground_evidence`).
    for claim in evidence.claims {
        ctx.store.insert_claim(
            kb,
            &claim.content,
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
        let (Some(from), Some(to)) = (
            engram_graph::canonicalize_surface(&relation.from),
            engram_graph::canonicalize_surface(&relation.to),
        ) else {
            continue;
        };
        if from.name.eq_ignore_ascii_case(&to.name) {
            continue;
        }
        let mut resolve = |canonical: &engram_graph::Canonical| -> Result<uuid::Uuid, CliError> {
            if let Some(id) = ids.get(&canonical.name.to_lowercase()).copied() {
                return Ok(id);
            }
            let kind = normalize_llm_kind(None, canonical);
            let id = ctx.store.upsert_entity_with_aliases(
                kb,
                &canonical.name,
                kind,
                1,
                &canonical.aliases,
            )?;
            ids.insert(canonical.name.to_lowercase(), id);
            Ok(id)
        };
        let from_id = resolve(&from)?;
        let to_id = resolve(&to)?;
        let predicate = normalize_predicate(relation.predicate.as_deref());
        ctx.store.insert_relation_with_source(
            kb,
            from_id,
            to_id,
            &predicate,
            relation.weight.unwrap_or(1.0).clamp(0.1, 10.0),
            json!({
                "chunk_id": chunk.chunk_id.to_string(),
                "memory_id": chunk.memory_id.to_string(),
                "sentence": relation.evidence.unwrap_or_default(),
                "cue": "llm_compiler",
            }),
            span_id,
        )?;
        stats.llm_relations += 1;
    }
    // KB-scoped facts through the SAME write path `remember` uses: predicate
    // and subject canonicalization, slot policy, dedup into confirmations,
    // conflict classification, and the conflict queue.
    //
    // This block used to insert directly and blind-supersede every active fact
    // with a different object. That silently retired safety-slot values
    // (allergy, medication, diagnosis, blood type) which NO path may
    // auto-supersede, destroyed the second value of every multi-valued
    // predicate ("Ana knows Bob" is not a correction of "Ana knows Alice"),
    // and dated every fact from the compile clock — a fabricated "now" that
    // wins recency supersession against the older true fact and gets rewritten
    // on every recompile.
    let facts = evidence.facts;
    if facts.is_empty() {
        return Ok(());
    }
    // The fact's event time comes from the SOURCE MEMORY (its `--event-time`,
    // else when the store recorded it), never from a clock read here.
    let Some((event_time, created_at)) = ctx.store.memory_times(chunk.memory_id)? else {
        // The chunk's memory went away underneath this compile: there is
        // nothing left to date its facts by, so write none.
        tracing::warn!(
            "compiler: skipped facts for chunk {} — memory {} is no longer live",
            chunk.chunk_id,
            chunk.memory_id
        );
        return Ok(());
    };
    let write_ctx = FactWriteCtx {
        memory_id: chunk.memory_id,
        diary: &chunk.diary,
        kb,
        memory_time: MemoryTime {
            event_time,
            created_at,
        },
        // No novelty gate in the compiler: the exact (subject, predicate) path
        // plus the classifier do the work, and an embedding call per fact
        // inside the compile transaction is a paid round trip per fact.
        embedder: None,
        llm: None,
        // `assisted`, not `strict`: a compile must still be able to record
        // that a 2024 paper supersedes a 2015 one. `strict` would queue that
        // too, which is how a corpus rebuilds the backlog nobody reads.
        autonomy: Autonomy::default(),
        source_kind: SourceKind::CompileLlm,
    };
    let mut outcome = FactWriteOutcome::default();
    // `compile` already holds its own build-then-swap transaction, so it calls
    // `write_fact` directly rather than `write_fact_batch`. With no embedder
    // the cache is never consulted.
    let cache = FactEmbedCache::empty();
    for fact in facts {
        let ef = ExtractedFact {
            subject: fact.subject,
            predicate: fact.predicate,
            object: fact.object,
            confidence: fact.confidence.unwrap_or(0.7).clamp(0.0, 1.0),
            fact_type: None,
            // The compiler extracts no date of its own, so the memory's is the
            // only honest one (see `MemoryTime`) — and it names no cessation,
            // so `valid_until`/`state` take their defaults rather than
            // asserting that a compiled claim ended.
            valid_from: None,
            ..Default::default()
        };
        write_fact(ctx, &write_ctx, &ef, &cache, &mut outcome).await?;
    }
    stats.llm_facts += outcome.added;
    stats.llm_facts_queued += outcome.queued.len();
    Ok(())
}

/// Trim the claims and facts and drop blank ones, so the judge, the
/// grounding rules and the writer see the same items. A blank item was never
/// stored and is not counted as rejected.
fn tidy(mut evidence: LlmEvidence) -> LlmEvidence {
    for claim in &mut evidence.claims {
        claim.content = claim.content.trim().to_string();
    }
    evidence.claims.retain(|c| !c.content.is_empty());
    for fact in &mut evidence.facts {
        for part in [&mut fact.subject, &mut fact.predicate, &mut fact.object] {
            *part = part.trim().to_string();
        }
    }
    evidence
        .facts
        .retain(|f| !(f.subject.is_empty() || f.predicate.is_empty() || f.object.is_empty()));
    evidence
}

/// The judge's question ids for a chunk's claims and facts.
fn claim_id(i: usize) -> String {
    format!("c{i}")
}

fn fact_id(i: usize) -> String {
    format!("f{i}")
}

/// P(supported) for every claim and fact of one chunk, by question id.
struct ChunkVerdicts {
    model: String,
    p: HashMap<String, f32>,
}

/// Set to a path, appends one JSON line per judged item to that local file,
/// to calibrate the admit threshold on a user's own corpus.
const JUDGE_LOG_ENV: &str = "ENGRAM_JUDGE_LOG";

/// One grounding request per chunk (its claims, and its facts as sentences),
/// [`BATCH_CONCURRENCY`] chunks at a time. A chunk whose request failed or
/// left an item unanswered gets `None` and falls back to the lexical rules:
/// a partial answer set is not a decision.
async fn judge_chunks(
    ctx: &AppContext,
    kb: &str,
    judge: &dyn Judge,
    extracted: &[(&engram_storage::ChunkRecord, LlmEvidence)],
    stats: &mut CompileStats,
) -> Vec<Option<ChunkVerdicts>> {
    let items: Vec<Vec<(String, String)>> = extracted
        .iter()
        .map(|(_, evidence)| {
            let claims = evidence
                .claims
                .iter()
                .enumerate()
                .map(|(i, c)| (claim_id(i), c.content.clone()));
            let facts = evidence.facts.iter().enumerate().map(|(i, f)| {
                let text = questions::fact_as_claim(&f.subject, &f.predicate, &f.object);
                (fact_id(i), text)
            });
            claims.chain(facts).collect()
        })
        .collect();
    // A chunk with nothing to judge sends nothing.
    let asked: Vec<usize> = (0..items.len()).filter(|&i| !items[i].is_empty()).collect();
    let requests: Vec<_> = asked
        .iter()
        .map(|&i| questions::grounding_request(&extracted[i].0.content, &items[i]))
        .collect();
    let responses = ask_many(judge, &requests, BATCH_CONCURRENCY).await;
    let mut verdicts: Vec<Option<ChunkVerdicts>> = extracted.iter().map(|_| None).collect();
    for (i, response) in asked.into_iter().zip(responses) {
        let decided = response.and_then(|resp| {
            crate::judge::record_judge_usage(
                ctx,
                JudgeSite::Grounding.operation(),
                &resp,
                Some(kb),
                None,
            );
            stats.judge_requests += resp.requests as usize;
            stats.judge_input_tokens += resp.input_tokens;
            stats.judge_cost_usd += resp.cost_usd;
            let p = items[i]
                .iter()
                .map(|(id, _)| Ok((id.clone(), resp.noul(id)?)))
                .collect::<Result<_, JudgeError>>()?;
            Ok(ChunkVerdicts {
                model: resp.model,
                p,
            })
        });
        match decided {
            Ok(v) => verdicts[i] = Some(v),
            Err(e) => {
                tracing::warn!(
                    "compiler: the judge did not decide chunk {}, the lexical rules do: {e}",
                    extracted[i].0.chunk_id
                );
                stats.judge_fallback_chunks += 1;
            }
        }
    }
    verdicts
}

/// Keep the claims and facts of one chunk that grounding admits, counting
/// the rest.
///
/// HALLUCINATION GATE: an extracted claim or fact that the chunk does not
/// support would land in the KB carrying a real chunk citation, a
/// fully-sourced-looking falsehood; a fact would also reach `state`. Off, in
/// shadow, and for a chunk the judge did not decide, the lexical rules
/// decide. On, the judge does, with two checks kept in code: every number
/// must be in the chunk (the judge is weak at numbers) and a safety member is
/// never a negation (the judge reads it as supported).
fn ground_evidence(
    chunk: &engram_storage::ChunkRecord,
    mut evidence: LlmEvidence,
    mode: JudgeMode,
    verdicts: Option<&ChunkVerdicts>,
    stats: &mut CompileStats,
) -> LlmEvidence {
    let text = &chunk.content;
    let p_of = |id: String| verdicts.and_then(|v| v.p.get(&id).copied());
    let judge_admits =
        |p: f32, numbers: bool| numbers && questions::admits(p, questions::COMPILE_ADMIT_P);
    let mut log = verdicts
        .and_then(|_| std::env::var_os(JUDGE_LOG_ENV))
        .and_then(|path| {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path);
            file.map_err(|e| tracing::warn!("{JUDGE_LOG_ENV}: {e}"))
                .ok()
        });
    let mut log_item = |kind: &str, item: &str, lexical, numbers, p: Option<f32>, admitted| {
        if let (Some(file), Some(p), Some(v)) = (log.as_mut(), p, verdicts) {
            let line = json!({
                "site": "compile", "kind": kind, "chunk_id": chunk.chunk_id, "text": item,
                "lexical": lexical, "numbers_ok": numbers, "p": wire_decimal(p),
                "admitted": admitted,
                "mode": mode.as_str(), "model": v.model,
            });
            let _ = writeln!(file, "{line}");
        }
    };

    for (i, claim) in std::mem::take(&mut evidence.claims).into_iter().enumerate() {
        let lexical = claim_grounded_lexically(&claim.content, text);
        let numbers = numbers_ok(&claim.content, text);
        let p = p_of(claim_id(i));
        let judge = p.map(|p| judge_admits(p, numbers));
        let admitted = stats.admit(Item::Claim, mode, lexical, judge);
        log_item("claim", &claim.content, lexical, numbers, p, admitted);
        if admitted {
            evidence.claims.push(claim);
        } else {
            tracing::warn!(
                "compiler: dropped ungrounded LLM claim for chunk {}: {:?}",
                chunk.chunk_id,
                claim.content.chars().take(80).collect::<String>()
            );
            stats.llm_claims_rejected += 1;
        }
    }
    for (i, fact) in std::mem::take(&mut evidence.facts).into_iter().enumerate() {
        let (subject, predicate, object) = (&fact.subject, &fact.predicate, &fact.object);
        let negated = negated_safety_member(predicate, object);
        let lexical = fact_grounded_in_chunk(subject, predicate, object, text);
        let sentence = questions::fact_as_claim(subject, predicate, object);
        let numbers = numbers_ok(&sentence, text) && object_numbers_in_chunk(object, text);
        let p = p_of(fact_id(i));
        let judge = p.map(|p| judge_admits(p, numbers) && !negated);
        let admitted = stats.admit(Item::Fact, mode, lexical, judge);
        log_item("fact", &sentence, lexical, numbers, p, admitted);
        if admitted {
            evidence.facts.push(fact);
        } else {
            stats.llm_facts_rejected += 1;
        }
    }
    evidence
}

/// The judge-free rule for a compiled fact, which lands in the canonical
/// store `state` serves. Every subject and object token must be in the chunk:
/// the claim rule's four-character floor let "Bob diagnosed_with HIV" through
/// against a weather note, because it left nothing to check.
///
/// It never admits the END of a medication, allergy or diagnosis
/// ("stopped_taking metformin"): it cannot see the predicate, so a note that
/// only mentions the drug would close a true, open medication. Only the judge
/// may admit a safety cessation.
fn fact_grounded_in_chunk(subject: &str, predicate: &str, object: &str, chunk: &str) -> bool {
    fact_grounded_lexically(subject, predicate, object, chunk)
        && object_numbers_in_chunk(object, chunk)
        && !negated_safety_member(predicate, object)
        && safety_relation(predicate) != Some(true)
}

/// Every number in a fact's object is in the chunk, single digits included.
/// `numbers_ok` lets one-digit numbers through (so "took 5 pills" can match
/// "five pills"), which admitted an invented "warfarin 5 mg" against "2 mg";
/// a fact object is a stored value, and a dose is a number to get right.
fn object_numbers_in_chunk(object: &str, chunk: &str) -> bool {
    let chunk_numbers = numeric_tokens(chunk);
    numeric_tokens(object)
        .iter()
        .all(|n| chunk_numbers.contains(n))
}

/// A medication, allergy or diagnosis that is a negation ("takes_medication:
/// no blood thinners"). The text does say it, so neither the lexical rule nor
/// the judge objects; the shape is wrong, and `state` would list "no blood
/// thinners" as a medication. Checked in every judge mode.
fn negated_safety_member(predicate: &str, object: &str) -> bool {
    safety_relation(predicate).is_some() && negated_object(object)
}

/// `Some(is_cessation)` when the predicate writes a set-valued safety
/// relation, resolved as the fact writer resolves it: a cessation
/// ("stopped_taking", "recovered_from") first, then the canonical relation.
/// Compiled facts carry no `state`, so the predicate is the only way one ends.
fn safety_relation(predicate: &str) -> Option<bool> {
    let cessation = cessation_of(predicate);
    let relation = cessation.map_or_else(|| canonical_relation(predicate), str::to_string);
    let spec = relation_spec(&relation);
    (spec.is_set_valued() && spec.is_safety()).then_some(cessation.is_some())
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
    #[serde(default)]
    facts: Vec<LlmFact>,
}

/// An atomic (subject, predicate, object) fact the extractor found in a chunk.
/// These feed the contradiction table KB-scoped, so an ingested-and-compiled
/// paper can later be seen to be superseded by a newer one.
#[derive(Debug, Deserialize)]
struct LlmFact {
    subject: String,
    predicate: String,
    object: String,
    confidence: Option<f32>,
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
    /// Alternate spellings the model saw for the same entity ("Elias",
    /// "Elias D."), folded onto one node instead of creating rivals.
    #[serde(default)]
    aliases: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct LlmRelation {
    from: String,
    to: String,
    predicate: Option<String>,
    weight: Option<f32>,
    /// Sentence the model read the relation from; stored as provenance so the
    /// edge stays checkable.
    #[serde(default)]
    evidence: Option<String>,
}

/// Entity kinds the compiler is willing to store. Anything else the model
/// invents is mapped back through the offline heuristics, so `kind` stays a
/// small closed vocabulary that `people list` can filter on.
const ALLOWED_KINDS: &[&str] = &[
    "person",
    "organization",
    "gene_or_protein",
    "drug",
    "place",
    "event",
    "method",
    "species",
    "dataset",
    "concept",
];

fn normalize_llm_kind(kind: Option<&str>, canonical: &engram_graph::Canonical) -> &'static str {
    // A model that answers with a greeting or a contact label gets the same
    // repair as the offline path, including its person evidence.
    let heuristic = infer_entity_kind(&canonical.name, canonical.person_hint);
    let Some(kind) = kind.map(|k| k.trim().to_ascii_lowercase()) else {
        return heuristic;
    };
    let matched = ALLOWED_KINDS.iter().find(|k| **k == kind);
    match matched {
        // A model calling someone a person is trusted (it has the sentence);
        // the reverse — a model calling a person a concept — is not, so the
        // more specific of the two readings wins.
        Some(k) => engram_graph::people::merge_kind(k, heuristic),
        None => heuristic,
    }
}

/// Typed relation predicates. Free-form model output is snake_cased and
/// checked against the person vocabulary so the graph stays queryable.
fn normalize_predicate(predicate: Option<&str>) -> String {
    let Some(raw) = predicate else {
        return "related_to".to_string();
    };
    let cleaned: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        return "related_to".to_string();
    }
    cleaned
}

fn parse_llm_evidence(text: &str) -> Result<LlmEvidence, serde_json::Error> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<LlmEvidence>(trimmed) {
        return Ok(v);
    }
    // Strip a ```json fence if present, then find the first BALANCED top-level
    // object. The old first-`{`/last-`}` slice broke whenever a `}` appeared
    // inside a string value or prose followed the JSON.
    let candidate = strip_code_fence(trimmed);
    if let Some(obj) = first_balanced_object(candidate) {
        return serde_json::from_str(obj);
    }
    // Last resort: surface a real parse error from the raw text.
    serde_json::from_str(candidate)
}

fn strip_code_fence(text: &str) -> &str {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // Drop an optional language tag on the first line, and the trailing ```.
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let rest = rest.trim_start_matches('\n');
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    t
}

/// Return the first balanced `{...}` object substring, respecting string
/// literals and escapes so a brace inside a string doesn't miscount.
fn first_balanced_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn extraction_prompt(kb: &str, chunk: &engram_storage::ChunkRecord) -> String {
    format!(
        "KB: {kb}\nChunk id: {chunk_id}\nDocument id: {document_id}\nSection: {section}\nSource: {source}\n\nExtract evidence from the chunk below.\nReturn JSON only with this shape:\n{{\"claims\":[{{\"content\":\"short factual claim with numbers if present\",\"evidence_level\":\"review|meta_analysis|rct|human_observational|animal|in_vitro|preprint|news|opinion|unknown\",\"confidence\":0.0}}],\"entities\":[{{\"name\":\"canonical entity\",\"kind\":\"person|organization|gene_or_protein|drug|place|event|method|species|dataset|concept\",\"aliases\":[\"other spellings of the SAME entity in this chunk\"]}}],\"relations\":[{{\"from\":\"entity\",\"to\":\"entity\",\"predicate\":\"typed_relation\",\"weight\":1.0,\"evidence\":\"the sentence this relation came from\"}}],\"takeaways\":[\"short cited takeaway\"],\"facts\":[{{\"subject\":\"entity\",\"predicate\":\"snake_case_relation\",\"object\":\"value or entity\",\"confidence\":0.0}}]}}\nPeople matter: type every human as \"person\", give their full name as `name` and shorter forms (first name only, possessive, titled) as `aliases`. Never emit a pronoun, contraction (\"She's\", \"I'm\"), weekday, month, or bare common word as an entity.\nFor two people, prefer these predicates: introduced_by (from = the person who was introduced, to = the introducer), friend_of, married_to, dating, family_of, works_with, colleague_of, investor_in, client_of, met_at, knows.\nEvery claim and fact must be directly supported by this chunk. Emit a fact only for a stable, checkable assertion (a dosage, a target, an effect direction), not vague prose. Do not invent citations.\n\nChunk:\n{content}",
        chunk_id = chunk.chunk_id,
        document_id = chunk
            .document_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string()),
        section = chunk.section.as_deref().unwrap_or("unknown"),
        source = chunk.source.as_deref().unwrap_or("unknown"),
        // Cap the inlined chunk so one oversized chunk can't blow the model's
        // context / cost. Paper chunks are ~1800 chars (see the chunker cap);
        // this is a belt-and-suspenders bound for any legacy oversized chunk.
        content = truncate_chars(&chunk.content, 6000)
    )
}

/// Truncate `text` to at most `max` chars on a char boundary, appending an
/// ellipsis marker when it was cut.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str(" …[truncated]");
    out
}

fn synthesis_prompt(kb: &str, chunks: &[&engram_storage::ChunkRecord]) -> String {
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

fn evidence_sentences(text: &str) -> Vec<engram_ingest::sentences::Sentence> {
    engram_ingest::sentences::segment(text)
        .into_iter()
        .filter(|s| s.text.len() > 40 && s.text.len() < 700)
        .filter(|s| looks_like_evidence(&s.text))
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

fn first_sentence(text: &str) -> Option<engram_ingest::sentences::Sentence> {
    engram_ingest::sentences::segment(text)
        .into_iter()
        .find(|s| s.text.len() > 30)
}

fn infer_evidence_level(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    let words = word_set(text);
    let has = |w: &str| words.contains(w);
    // Phrase checks first (multi-word), then word-boundary single tokens.
    if lower.contains("meta-analysis") || lower.contains("meta analysis") {
        "meta_analysis"
    } else if has("randomized") || has("randomised") || has("rct") || has("trial") {
        "rct"
    } else if has("cohort") || has("patients") || has("human") || has("humans") {
        "human_observational"
    } else if has("mouse") || has("mice") || has("rat") || has("rats") || has("murine") {
        "animal"
    } else if has("cell") || has("cells") || lower.contains("in vitro") || has("vitro") {
        "in_vitro"
    } else if has("review") {
        "review"
    } else if has("preprint") {
        "preprint"
    } else {
        "unknown"
    }
}

/// Kind inference without surrounding text (LLM path, entity backfill).
///
/// `person_hint` carries the only evidence a bare surface can give: it was a
/// greeting ("Heyy Elias") or a messaging-app contact label ("Maria Microsoft
/// London Tech"), both of which address a human. The context-aware version
/// lives in `engram_graph::people`.
fn infer_entity_kind(name: &str, person_hint: bool) -> &'static str {
    engram_graph::people::infer_kind(name, person_hint)
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
    // BY MENTIONS, NOT BY SPELLING. `entity_mentions` is a BTreeMap keyed on
    // the display name, so `.rev().take(25)` used to publish the alphabetically
    // LAST 25 entities as "Top entities" and `.take(50)` gave pages to the
    // alphabetically first 50 — on a personal store that is a wiki about
    // Zurich and Lira with no page for the person mentioned 431 times. Ties
    // break on the name so the output stays byte-identical across recompiles.
    let mut ranked: Vec<(&String, &usize)> = entity_mentions.iter().collect();
    ranked.sort_by(|(a_name, a_count), (b_name, b_count)| {
        b_count.cmp(a_count).then_with(|| a_name.cmp(b_name))
    });

    index.push_str("## Top entities\n\n");
    for (name, count) in ranked.iter().take(25) {
        index.push_str(&format!(
            "- [{}](entities/{}.md) ({count})\n",
            name,
            slugify(name)
        ));
    }
    ctx.store.upsert_wiki_page(kb, "index.md", kb, &index)?;
    pages += 1;

    for (name, count) in ranked.iter().take(50) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn evidence_sentences_keep_decimals_intact() {
        let text = "The IC50 of rapamycin was 0.1 nM in the mTOR kinase assay. Filler.";
        let sents = evidence_sentences(text);
        assert!(sents.iter().any(|s| s.text.contains("0.1 nM")));
        // The offset must point back to the exact sentence text.
        let s = &sents[0];
        assert_eq!(&text[s.start..s.end], s.text);
    }

    #[test]
    fn evidence_level_uses_word_boundaries() {
        // "literature"/"temperature" must NOT match "rat"; "excellent" not "cell".
        assert_eq!(
            infer_evidence_level("The literature at ambient temperature was excellent."),
            "unknown"
        );
        assert_eq!(
            infer_evidence_level("Lifespan rose in aged mice given the drug."),
            "animal"
        );
        assert_eq!(
            infer_evidence_level("A randomized controlled trial in humans."),
            "rct"
        );
    }

    #[test]
    fn entity_kind_not_every_caps_token_is_gene() {
        assert_eq!(infer_entity_kind("DNA", false), "concept");
        assert_eq!(infer_entity_kind("FDA", false), "concept");
        assert_eq!(infer_entity_kind("ELISA", false), "concept");
        assert_eq!(infer_entity_kind("BRCA1", false), "gene_or_protein");
        assert_eq!(infer_entity_kind("IL-6", false), "gene_or_protein");
        assert_eq!(infer_entity_kind("Acme Labs", false), "organization");
        // A place is never a person, however strong the surrounding cue.
        assert_eq!(infer_entity_kind("London", true), "concept");
        assert_eq!(infer_entity_kind("Islam", true), "concept");
    }

    #[test]
    fn balanced_object_survives_braces_in_strings() {
        let text = r#"Sure! Here you go: {"takeaways":["uses a } brace"],"claims":[]} — done."#;
        let obj = first_balanced_object(text).unwrap();
        let parsed: LlmEvidence = serde_json::from_str(obj).unwrap();
        assert_eq!(parsed.takeaways.len(), 1);
    }

    #[test]
    fn strip_fence_handles_json_block() {
        let text = "```json\n{\"claims\":[]}\n```";
        let parsed = parse_llm_evidence(text).unwrap();
        assert!(parsed.claims.is_empty());
    }

    #[test]
    fn validate_citations_strips_unknown_uuids() {
        let known: HashSet<uuid::Uuid> = [uuid::Uuid::nil()].into_iter().collect();
        let text = format!(
            "Real [chunk:{}] and fake [chunk:{}].",
            uuid::Uuid::nil(),
            uuid::Uuid::new_v4()
        );
        let (out, dropped) = validate_chunk_citations(&text, &known);
        assert_eq!(dropped, 1);
        assert!(out.contains("[chunk:unverified]"));
        assert!(out.contains(&uuid::Uuid::nil().to_string()));
    }

    #[test]
    fn grounding_follows_the_mode_and_counts_agreement_only_when_judged() {
        let mut stats = CompileStats::default();
        // Not judged (off, no route, or a fallback chunk): the lexical rule.
        assert!(stats.admit(Item::Claim, JudgeMode::On, true, None));
        assert!(!stats.admit(Item::Fact, JudgeMode::On, false, None));
        // Judged: `on` takes the judge, shadow keeps the lexical rule.
        assert!(!stats.admit(Item::Claim, JudgeMode::On, true, Some(false)));
        assert!(stats.admit(Item::Claim, JudgeMode::Shadow, true, Some(false)));
        assert!(stats.admit(Item::Fact, JudgeMode::On, false, Some(true)));
        assert!(!stats.admit(Item::Fact, JudgeMode::Shadow, false, Some(true)));
        assert!(stats.admit(Item::Fact, JudgeMode::On, true, Some(true)));
        assert_eq!(stats.judge_claims_lexical_only_admitted, 2);
        assert_eq!(stats.judge_facts_judge_only_admitted, 2);
        assert_eq!(stats.judge_facts_admitted_by_both, 1);
        assert_eq!(
            stats.judge_claims_admitted_by_both
                + stats.judge_claims_rejected_by_both
                + stats.judge_claims_judge_only_admitted
                + stats.judge_facts_rejected_by_both
                + stats.judge_facts_lexical_only_admitted,
            0
        );
    }

    #[test]
    fn a_negation_is_refused_only_for_the_safety_sets() {
        assert!(negated_safety_member(
            "takes_medication",
            "no blood thinners"
        ));
        assert!(negated_safety_member("allergy", "none"));
        assert!(negated_safety_member(
            "diagnosed_with",
            "ruled out appendicitis"
        ));
        // A blood type of "O negative" is a value, and a non-safety
        // relation is not this check's business.
        assert!(!negated_safety_member("blood_type", "O negative"));
        assert!(!negated_safety_member("prefers", "no meetings before ten"));
        assert!(!negated_safety_member(
            "takes_medication",
            "metformin 500 mg"
        ));
        // Resolved as the writer resolves it: a cessation is its relation.
        assert!(negated_safety_member("stopped_taking", "none"));
    }

    #[test]
    fn the_lexical_rule_never_ends_a_safety_member() {
        let chunk = "At lunch Ana talked about metformin prices and her Paris move.";
        assert!(fact_grounded_in_chunk(
            "Ana",
            "takes_medication",
            "metformin",
            chunk
        ));
        for ends in ["stopped_taking", "no_longer_allergic", "recovered_from"] {
            assert!(
                !fact_grounded_in_chunk("Ana", ends, "metformin", chunk),
                "{ends}"
            );
        }
        // A cessation outside the safety sets is still the lexical rule's.
        assert!(fact_grounded_in_chunk("Ana", "moved_from", "Paris", chunk));
    }

    #[test]
    fn a_one_digit_number_in_a_fact_object_must_be_the_chunks_own() {
        let chunk = "Ana takes warfarin 2 mg daily.";
        assert!(fact_grounded_in_chunk(
            "Ana",
            "takes_medication",
            "warfarin 2 mg",
            chunk
        ));
        assert!(!fact_grounded_in_chunk(
            "Ana",
            "takes_medication",
            "warfarin 5 mg",
            chunk
        ));
    }
}
