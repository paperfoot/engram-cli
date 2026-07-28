//! `engram chunks repair` — retire degenerate chunks from an existing store,
//! plus the single place chunk-size policy is resolved from config/env.
//!
//! WHY THIS COMMAND EXISTS. `remember` used to chunk with a bare paragraph
//! split: no merge, no size cap, no floor. Every stray line in ingested or
//! pasted content — `Sep 16, 2025`, `Elias:`, `ok` — became its own embedded,
//! retrievable chunk. A 12-character chunk is nearly pure signal for the few
//! tokens it holds, so it scores at the very top for any query mentioning them.
//! One real store reached 986 chunks under 40 characters, and a recall for a
//! person's birthday returned two bare date fragments at score 1.0 ABOVE the
//! memory that actually contained the birthday.
//!
//! Fixing the write path stops new ones. This fixes the ones already stored:
//! each sub-threshold chunk is merged into a neighbouring chunk of the SAME
//! memory and the result is re-embedded. Nothing is invented and nothing is
//! dropped — a memory's chunk texts are only ever re-partitioned.
//!
//! Idempotent by construction: after a run no memory has a sub-threshold chunk
//! unless that chunk is the memory's only one, which is exactly the condition
//! for "nothing to do".

use crate::commands::usage::{estimated_tokens_for_texts, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::stub::StubEmbedder;
use engram_embed::{Embedder, TaskMode};
use engram_ingest::chunker::{enforce_max_chunk_chars, enforce_min_chunk_chars, PendingChunk};
use engram_ingest::memory::ChunkPolicy;
use engram_storage::ChunkRecord;
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

/// Config keys + env overrides for the chunk-size knobs, in the same
/// env-beats-file order as every other retrieval knob (see
/// `config::resolve_setting`).
pub const MIN_CHARS_ENV: &str = "ENGRAM_CHUNK_MIN_CHARS";
pub const MIN_CHARS_CONFIG_KEY: &str = "chunking.min_chars";
pub const TARGET_CHARS_ENV: &str = "ENGRAM_CHUNK_TARGET_CHARS";
pub const TARGET_CHARS_CONFIG_KEY: &str = "chunking.target_chars";
pub const MAX_CHARS_ENV: &str = "ENGRAM_CHUNK_MAX_CHARS";
pub const MAX_CHARS_CONFIG_KEY: &str = "chunking.max_chars";

/// The chunk policy every write path uses. A malformed value falls back to the
/// default rather than failing the write: a typo in a config file must not make
/// `remember` unusable.
pub fn resolve_policy() -> ChunkPolicy {
    let defaults = ChunkPolicy::default();
    ChunkPolicy {
        min_chars: resolve_usize(MIN_CHARS_ENV, MIN_CHARS_CONFIG_KEY, defaults.min_chars),
        target_chars: resolve_usize(
            TARGET_CHARS_ENV,
            TARGET_CHARS_CONFIG_KEY,
            defaults.target_chars,
        ),
        max_chars: resolve_usize(MAX_CHARS_ENV, MAX_CHARS_CONFIG_KEY, defaults.max_chars),
    }
    .sanitized()
}

fn resolve_usize(env: &str, config_key: &str, default: usize) -> usize {
    let raw = crate::commands::config::resolve_setting(env, config_key, "");
    if raw.trim().is_empty() {
        return default;
    }
    match raw.trim().parse::<usize>() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("{env}/{config_key} is not a number ({raw:?}); using {default}");
            default
        }
    }
}

/// A memory whose chunk sequence needs re-partitioning.
struct RepairPlan {
    memory_id: Uuid,
    document_id: Option<Uuid>,
    after: Vec<PendingChunk>,
}

pub async fn repair(
    ctx: &AppContext,
    kb: Option<String>,
    all_kbs: bool,
    dry_run: bool,
) -> Result<(), CliError> {
    if kb.is_none() && !all_kbs {
        return Err(CliError::BadInput(
            "chunks repair requires --kb <name> or --all-kbs".into(),
        ));
    }
    if kb.is_some() && all_kbs {
        return Err(CliError::BadInput(
            "pass either --kb <name> or --all-kbs, not both".into(),
        ));
    }
    let start = Instant::now();
    let policy = resolve_policy();
    let scope = if all_kbs { None } else { kb.as_deref() };

    let records = ctx.store.list_chunks_by_memory(scope)?;
    let groups = group_by_memory(records);

    let mut memories_scanned = 0usize;
    let mut degenerate_before = 0usize;
    let mut degenerate_after = 0usize;
    let mut chunks_before = 0usize;
    let mut chunks_after = 0usize;
    let mut plans: Vec<RepairPlan> = Vec::new();

    for group in groups {
        memories_scanned += 1;
        chunks_before += group.len();
        let before: Vec<PendingChunk> = group
            .iter()
            .map(|c| PendingChunk {
                text: c.content.clone(),
                position: c.position,
                section: c.section.clone(),
            })
            .collect();
        degenerate_before += degenerate_count(&before, policy.min_chars);
        let after = repartition(before.clone(), policy);
        degenerate_after += degenerate_count(&after, policy.min_chars);
        chunks_after += after.len();
        if same_texts(&before, &after) {
            continue;
        }
        plans.push(RepairPlan {
            memory_id: group[0].memory_id,
            // Chunks of one memory share a document; take the first recorded
            // one so the repaired chunks stay attached to their source doc.
            document_id: group.iter().find_map(|c| c.document_id),
            after,
        });
    }

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("dry_run", dry_run);
    meta.add("memories_repaired", plans.len());
    meta.add("degenerate_chunks_before", degenerate_before);
    meta.add("chunks_before", chunks_before);
    meta.add("chunks_after", chunks_after);

    // --dry-run writes NOTHING: no chunk rows, and no embedding calls either,
    // so inspecting a store is always free.
    let mut reembedded = 0usize;
    let mut model = String::from("none");
    if !dry_run && !plans.is_empty() {
        let (m, n) = apply(ctx, &plans, kb.as_deref()).await?;
        model = m;
        reembedded = n;
    }
    meta.add("chunks_reembedded", reembedded);

    let label = if all_kbs {
        "*".to_string()
    } else {
        kb.clone().unwrap_or_default()
    };
    print_success(
        ctx.format,
        json!({
            "kb": label,
            "dry_run": dry_run,
            "min_chars": policy.min_chars,
            "max_chars": policy.max_chars,
            "memories_scanned": memories_scanned,
            "memories_repaired": plans.len(),
            "degenerate_chunks_before": degenerate_before,
            "degenerate_chunks_after": degenerate_after,
            "chunks_before": chunks_before,
            "chunks_after": chunks_after,
            "chunks_reembedded": reembedded,
            "embedder": model,
            "next_step": if dry_run && !plans.is_empty() {
                Some("re-run without --dry-run to apply")
            } else {
                None
            },
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

/// Re-partition one memory's existing chunk texts. Deliberately does NOT
/// re-merge to the target size: repair is a floor/ceiling fix, not a re-chunk,
/// so a store that is already fine is never re-embedded (and never re-billed).
fn repartition(chunks: Vec<PendingChunk>, policy: ChunkPolicy) -> Vec<PendingChunk> {
    let capped = enforce_max_chunk_chars(chunks, policy.max_chars);
    enforce_min_chunk_chars(capped, policy.min_chars)
}

fn same_texts(a: &[PendingChunk], b: &[PendingChunk]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.text == y.text)
}

/// Degenerate chunks in one memory's chunk list. A memory with a SINGLE chunk
/// never counts, however short it is — "Lira's birthday is 12 March" is 27
/// characters and a perfectly good memory. Only a fragment sitting next to
/// siblings is a defect.
fn degenerate_count(chunks: &[PendingChunk], min_chars: usize) -> usize {
    if chunks.len() < 2 {
        return 0;
    }
    chunks.iter().filter(|c| c.text.len() < min_chars).count()
}

/// Embed the repaired chunks and swap them in, one transaction per memory so a
/// failure halfway through leaves earlier memories correctly repaired rather
/// than rolling the whole store back.
async fn apply(
    ctx: &AppContext,
    plans: &[RepairPlan],
    kb: Option<&str>,
) -> Result<(String, usize), CliError> {
    let force_stub = crate::commands::config::stub_forced();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let have_gemini = !force_stub && gemini_key.is_some();

    let texts: Vec<&str> = plans
        .iter()
        .flat_map(|p| p.after.iter().map(|c| c.text.as_str()))
        .collect();

    let embedder: Box<dyn Embedder> = if have_gemini {
        Box::new(crate::retrieval::cloud_gemini(gemini_key.unwrap()))
    } else {
        Box::new(StubEmbedder::default())
    };
    // Embed BEFORE tearing anything down: a transient embed failure must never
    // strand a memory with no chunks (invisible to recall, unrecoverable).
    let embeddings = embedder
        .embed_batch(&texts, TaskMode::RetrievalDocument)
        .await?;
    if embeddings.len() != texts.len() {
        return Err(CliError::Transient(format!(
            "embedder returned {} embeddings for {} chunks; aborting repair so no chunk is \
             left without a vector",
            embeddings.len(),
            texts.len()
        )));
    }
    let model = embedder.model();
    let dims = embedder.dimensions();
    let prompt_format = embedder.prompt_format().to_string();
    crate::commands::chunks::warn_on_truncation(ctx, embedder.as_ref(), &texts, "chunks repair");

    let mut cursor = 0usize;
    let mut written = 0usize;
    for plan in plans {
        let slice = &embeddings[cursor..cursor + plan.after.len()];
        cursor += plan.after.len();
        ctx.store.with_tx(|store| {
            // Claims/spans point at chunk ids that are about to disappear; drop
            // them so `compile` rebuilds against the repaired chunks instead of
            // leaving dangling evidence. Same teardown `edit` performs.
            store.delete_derived_for_memory_chunks(plan.memory_id)?;
            store.delete_chunks_for_memory(plan.memory_id)?;
            for (chunk, emb) in plan.after.iter().zip(slice.iter()) {
                store.insert_chunk_with_embedding_meta(
                    Uuid::new_v4(),
                    plan.memory_id,
                    &chunk.text,
                    chunk.position,
                    chunk.section.as_deref(),
                    emb,
                    &model,
                    dims,
                    &prompt_format,
                    plan.document_id,
                )?;
            }
            Ok(())
        })?;
        written += plan.after.len();
    }

    if have_gemini && !texts.is_empty() {
        let tokens = estimated_tokens_for_texts(&texts);
        ctx.store.record_usage_event(
            "gemini",
            "chunks_repair_embed",
            Some(&model),
            kb,
            None,
            1,
            texts.len() as i64,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({
                "dimensions": dims,
                "prompt_format": prompt_format,
                "estimated": true,
            }),
        )?;
    }
    Ok((model, written))
}

fn group_by_memory(records: Vec<ChunkRecord>) -> Vec<Vec<ChunkRecord>> {
    let mut out: Vec<Vec<ChunkRecord>> = Vec::new();
    for record in records {
        match out.last_mut() {
            Some(group) if group[0].memory_id == record.memory_id => group.push(record),
            _ => out.push(vec![record]),
        }
    }
    out
}

/// Report — never swallow — text the embedder is about to truncate.
///
/// The Gemini client truncates any input past its ~7,800-character window and
/// used to do it silently: the tail of a long note simply never reached the
/// index. Chunking now caps far below that, so a hit here means somebody
/// widened `chunking.max_chars` past the provider's window and needs to know
/// before the data is quietly gone.
pub fn warn_on_truncation(
    ctx: &AppContext,
    embedder: &dyn Embedder,
    texts: &[&str],
    command: &str,
) -> Option<String> {
    let note = truncation_note(embedder.max_input_chars(), embedder.name(), texts)?;
    tracing::warn!("{command}: {note}");
    if !ctx.quiet {
        eprintln!("warning: {note}");
    }
    Some(note)
}

/// The sentence a caller needs when text is about to lose its tail. `None` when
/// the provider has no cap or nothing exceeds it. Split out from
/// `warn_on_truncation` so the wording is testable without a store.
fn truncation_note(limit: Option<usize>, provider: &str, texts: &[&str]) -> Option<String> {
    let limit = limit?;
    let over: Vec<usize> = texts
        .iter()
        .map(|t| t.len())
        .filter(|len| *len > limit)
        .collect();
    if over.is_empty() {
        return None;
    }
    let longest = over.iter().copied().max().unwrap_or(limit);
    Some(format!(
        "{} of {} chunks exceed the {provider} embedder's {limit}-character input window \
         (longest {longest}) and were TRUNCATED — the dropped tail is not retrievable. Lower \
         `{}` (config `{}`) and re-run `engram chunks repair`.",
        over.len(),
        texts.len(),
        MAX_CHARS_ENV,
        MAX_CHARS_CONFIG_KEY,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_ingest::memory::DEFAULT_MIN_CHUNK_CHARS;

    fn pc(text: &str) -> PendingChunk {
        PendingChunk {
            text: text.to_string(),
            position: 0,
            section: None,
        }
    }

    #[test]
    fn repartition_merges_a_date_fragment_into_its_neighbour() {
        let policy = ChunkPolicy::default();
        let before = vec![pc("Sep 16, 2025"), pc(&"a".repeat(300))];
        let after = repartition(before.clone(), policy);
        assert_eq!(after.len(), 1);
        assert!(after[0].text.starts_with("Sep 16, 2025"));
        assert!(!same_texts(&before, &after));
    }

    #[test]
    fn repartition_is_idempotent() {
        let policy = ChunkPolicy::default();
        let once = repartition(
            vec![pc("Sep 16, 2025"), pc(&"a".repeat(300)), pc("ok")],
            policy,
        );
        let twice = repartition(once.clone(), policy);
        assert!(same_texts(&once, &twice), "second pass changed the chunks");
        assert!(once.iter().all(|c| c.text.len() >= DEFAULT_MIN_CHUNK_CHARS));
    }

    #[test]
    fn repartition_leaves_a_healthy_memory_untouched() {
        let policy = ChunkPolicy::default();
        let before = vec![pc(&"a".repeat(300)), pc(&"b".repeat(300))];
        let after = repartition(before.clone(), policy);
        assert!(same_texts(&before, &after));
    }

    #[test]
    fn repartition_keeps_a_lone_short_chunk() {
        let policy = ChunkPolicy::default();
        let before = vec![pc("Lira's birthday is 12 March")];
        let after = repartition(before.clone(), policy);
        assert!(same_texts(&before, &after));
    }

    #[test]
    fn policy_reads_env_override() {
        // Serialized against other env-touching tests by the mutex below.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(MIN_CHARS_ENV, "123");
        let policy = resolve_policy();
        std::env::remove_var(MIN_CHARS_ENV);
        assert_eq!(policy.min_chars, 123);
    }

    #[test]
    fn policy_ignores_a_garbage_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(MIN_CHARS_ENV, "sixty");
        let policy = resolve_policy();
        std::env::remove_var(MIN_CHARS_ENV);
        assert_eq!(policy.min_chars, DEFAULT_MIN_CHUNK_CHARS);
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn truncation_is_reported_not_swallowed() {
        let long = "x".repeat(9_000);
        let texts = vec!["short", long.as_str()];
        let note = truncation_note(
            Some(engram_embed::gemini::MAX_INPUT_CHARS),
            "gemini",
            &texts,
        )
        .expect("an over-window chunk must produce a note");
        assert!(note.contains("1 of 2"));
        assert!(note.contains("TRUNCATED"));
        assert!(note.contains("9000"));
        assert!(note.contains(MAX_CHARS_ENV));
    }

    #[test]
    fn truncation_note_is_silent_when_nothing_is_lost() {
        let texts = vec!["short", "also short"];
        assert!(truncation_note(Some(7_800), "gemini", &texts).is_none());
        // A provider with no client-side cap never warns.
        let long = "x".repeat(9_000);
        assert!(truncation_note(None, "stub", &[long.as_str()]).is_none());
    }

    #[test]
    fn default_chunk_ceiling_stays_under_the_embedder_window() {
        // The whole point of the cap: a chunk this pipeline produces can never
        // be truncated, even after the min-floor reattaches a tail.
        let policy = ChunkPolicy::default();
        assert!(policy.max_chars + policy.min_chars < engram_embed::gemini::MAX_INPUT_CHARS);
    }
}
