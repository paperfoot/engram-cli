//! `engram facts {list,show,conflicts,resolve,backfill}` — browse the fact
//! store, the contradiction history, and retro-extract facts from memories
//! that were written before the fact layer was switched on.
//!
//! The fact store is populated at `engram remember` time via LLM extraction
//! (see `crate::commands::remember`). Conflicts are non-destructive — when a
//! new fact contradicts an existing one, the old fact is marked
//! `superseded_by` the new one rather than deleted, so history is queryable.

use crate::context::AppContext;
use crate::error::CliError;
use crate::fact_write::{write_fact, Autonomy, FactWriteCtx, FactWriteOutcome};
use crate::output::{print_success, Metadata};
use engram_core::types::{Fact, SourceKind};
use engram_embed::gemini::GeminiEmbedder;
use engram_embed::stub::StubEmbedder;
use engram_embed::Embedder;
use engram_graph::facts::{extract_facts, normalize};
use serde_json::json;
use std::time::Instant;

pub fn list(
    ctx: &AppContext,
    subject: Option<String>,
    diary: String,
    kb: String,
    all: bool,
    limit: usize,
) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let facts = match subject {
        Some(s) => {
            let s_norm = normalize(&s);
            let diary_filter = if diary == "*" {
                None
            } else {
                Some(diary.as_str())
            };
            ctx.store
                .list_facts_by_subject_scoped(&s_norm, diary_filter, kb_filter, all)?
        }
        None => {
            let diary_filter = if diary == "*" {
                None
            } else {
                Some(diary.as_str())
            };
            ctx.store
                .list_facts_scoped(diary_filter, kb_filter, all, limit)?
        }
    };

    let payload = json!({
        "kb": kb,
        "count": facts.len(),
        "facts": facts.iter().map(fact_to_json).collect::<Vec<_>>(),
    });

    print_success(ctx.format, payload, Metadata::default(), |_| {
        if facts.is_empty() {
            println!("(no facts)");
            return;
        }
        for f in &facts {
            let status = if f.superseded_by.is_some() {
                " [superseded]"
            } else {
                ""
            };
            println!(
                "  {} -- {} -> {}  (conf={:.2}){}",
                f.subject, f.predicate, f.object, f.confidence, status
            );
        }
        println!();
        println!("{} facts", facts.len());
    });
    Ok(())
}

pub fn show(ctx: &AppContext, subject: String, diary: String, kb: String) -> Result<(), CliError> {
    let s_norm = normalize(&subject);
    let diary_filter = if diary == "*" {
        None
    } else {
        Some(diary.as_str())
    };
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let facts = ctx
        .store
        .list_facts_by_subject_scoped(&s_norm, diary_filter, kb_filter, true)?;

    let active: Vec<&Fact> = facts.iter().filter(|f| f.superseded_by.is_none()).collect();
    let historic: Vec<&Fact> = facts.iter().filter(|f| f.superseded_by.is_some()).collect();

    let payload = json!({
        "subject": subject,
        "subject_norm": s_norm,
        "kb": kb,
        "active_count": active.len(),
        "historic_count": historic.len(),
        "active":  active.iter().map(|f| fact_to_json(*f)).collect::<Vec<_>>(),
        "historic": historic.iter().map(|f| fact_to_json(*f)).collect::<Vec<_>>(),
    });

    print_success(ctx.format, payload, Metadata::default(), |_| {
        if facts.is_empty() {
            println!("(no facts about {})", subject);
            return;
        }
        println!("Active facts about {}:", subject);
        for f in &active {
            println!(
                "  {} -> {}  (conf={:.2})",
                f.predicate, f.object, f.confidence
            );
        }
        if !historic.is_empty() {
            println!();
            println!("History (superseded):");
            for f in &historic {
                let when = f
                    .superseded_at
                    .map(|t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "?".into());
                println!(
                    "  {} -> {}  (was true until {})",
                    f.predicate, f.object, when
                );
            }
        }
    });
    Ok(())
}

pub fn conflicts(
    ctx: &AppContext,
    limit: usize,
    open: bool,
    diary: Option<String>,
    kb: Option<String>,
) -> Result<(), CliError> {
    if open {
        return open_conflicts(ctx, limit, diary, kb);
    }
    let pairs = ctx.store.list_recent_conflicts(limit)?;
    let payload = json!({
        "count": pairs.len(),
        "conflicts": pairs.iter().map(|(old, new)| json!({
            "subject": old.subject,
            "predicate": old.predicate,
            "old_object": old.object,
            "new_object": new.object,
            "old_created_at": old.created_at.to_rfc3339(),
            "superseded_at": old.superseded_at.map(|t| t.to_rfc3339()),
            "old_fact_id": old.id.to_string(),
            "new_fact_id": new.id.to_string(),
        })).collect::<Vec<_>>(),
    });
    print_success(ctx.format, payload, Metadata::default(), |_| {
        if pairs.is_empty() {
            println!("(no conflicts recorded)");
            return;
        }
        for (old, new) in &pairs {
            let when = old
                .superseded_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "{}: {} {} CHANGED from \"{}\" to \"{}\"",
                when, old.subject, old.predicate, old.object, new.object
            );
        }
        println!();
        println!("{} conflicts", pairs.len());
    });
    Ok(())
}

/// Unresolved (contested) conflicts awaiting a decision. Each entry carries
/// the exact resolve command so the calling agent can act in one step.
fn open_conflicts(
    ctx: &AppContext,
    limit: usize,
    diary: Option<String>,
    kb: Option<String>,
) -> Result<(), CliError> {
    let rows = ctx
        .store
        .list_open_fact_conflicts(diary.as_deref(), kb.as_deref(), limit)?;
    let mut entries = Vec::new();
    for row in &rows {
        let (Some(new_f), Some(old_f)) = (
            ctx.store.get_fact(row.new_fact_id)?,
            ctx.store.get_fact(row.old_fact_id)?,
        ) else {
            continue;
        };
        entries.push(json!({
            "conflict_id": row.id.to_string(),
            "detector": row.detector,
            "score": row.score,
            "suggested_action": row.suggested,
            "created_at": row.created_at,
            "new": fact_to_json(&new_f),
            "old": fact_to_json(&old_f),
            "resolve_command": format!(
                "engram facts resolve {} --action supersede-old|keep-both|reject-new",
                row.id
            ),
        }));
    }
    let payload = json!({ "count": entries.len(), "open_conflicts": entries.clone() });
    print_success(ctx.format, payload, Metadata::default(), |_| {
        if rows.is_empty() {
            println!("(no open conflicts — nothing awaiting resolution)");
            return;
        }
        for e in &entries {
            println!(
                "{}: {} {} — '{}' vs '{}' (suggested: {})",
                e["conflict_id"].as_str().unwrap_or("?"),
                e["new"]["subject"].as_str().unwrap_or("?"),
                e["new"]["predicate"].as_str().unwrap_or("?"),
                e["old"]["object"].as_str().unwrap_or("?"),
                e["new"]["object"].as_str().unwrap_or("?"),
                e["suggested_action"].as_str().unwrap_or("?"),
            );
        }
    });
    Ok(())
}

/// Resolve a queued conflict. The caller decides; engram records the
/// decision and applies the supersession (never a deletion).
pub fn resolve(
    ctx: &AppContext,
    id: String,
    action: String,
    note: Option<String>,
) -> Result<(), CliError> {
    let qid = uuid::Uuid::parse_str(&id)
        .map_err(|_| CliError::BadInput(format!("'{id}' is not a valid conflict id")))?;
    let Some(row) = ctx.store.get_fact_conflict(qid)? else {
        return Err(CliError::BadInput(format!("no conflict with id {id}")));
    };
    if row.status != "open" {
        return Err(CliError::BadInput(format!(
            "conflict {id} is already {} — nothing to resolve",
            row.status
        )));
    }
    let (Some(new_f), Some(old_f)) = (
        ctx.store.get_fact(row.new_fact_id)?,
        ctx.store.get_fact(row.old_fact_id)?,
    ) else {
        return Err(CliError::BadInput(format!(
            "conflict {id} references facts that no longer exist"
        )));
    };

    let applied = match action.as_str() {
        "supersede-old" => {
            ctx.store
                .supersede_fact_with(old_f.id, new_f.id, "caller", new_f.valid_from)?;
            json!({
                "kept": fact_to_json(&new_f),
                "superseded": fact_to_json(&old_f),
            })
        }
        "keep-both" => json!({
            "kept": [fact_to_json(&new_f), fact_to_json(&old_f)],
            "note": "both facts remain active; recorded as non-contradictory",
        }),
        "reject-new" => {
            ctx.store
                .supersede_fact_with(new_f.id, old_f.id, "caller", None)?;
            json!({
                "kept": fact_to_json(&old_f),
                "superseded": fact_to_json(&new_f),
            })
        }
        other => {
            return Err(CliError::BadInput(format!(
                "unknown action '{other}' (expected supersede-old|keep-both|reject-new)"
            )))
        }
    };
    ctx.store
        .resolve_fact_conflict(qid, "caller", note.as_deref())?;

    let payload = json!({
        "conflict_id": id,
        "action": action,
        "resolved": true,
        "result": applied,
    });
    print_success(ctx.format, payload, Metadata::default(), |_| {
        println!("Resolved conflict {id} with action {action}");
    });
    Ok(())
}

/// `engram facts backfill --kb <kb>` — run fact extraction over memories that
/// were stored before extraction was on (or while no key was configured).
///
/// Idempotent by provenance: a memory is skipped once it has facts or an
/// extraction marker, so a re-run costs nothing and processes only what is
/// genuinely new. Oldest first, so a `--limit`ed run always advances and a
/// later run resumes where the previous one stopped.
pub async fn backfill(
    ctx: &AppContext,
    kb: String,
    diary: Option<String>,
    limit: Option<usize>,
    dry_run: bool,
) -> Result<(), CliError> {
    let start = Instant::now();
    let diary_filter = diary.as_deref().filter(|d| *d != "*");
    let llm = crate::fact_llm::resolve()?;
    let (total, pending_total) = ctx.store.fact_extraction_coverage(&kb, diary_filter)?;
    let queue = ctx
        .store
        .list_memories_pending_fact_extraction(&kb, diary_filter, limit)?;

    if dry_run {
        let preview: Vec<serde_json::Value> = queue
            .iter()
            .map(|m| {
                json!({
                    "memory_id": m.id.to_string(),
                    "created_at": m.created_at.to_rfc3339(),
                    "event_time": m.event_time.map(|t| t.to_rfc3339()),
                    "diary": m.diary,
                    "preview": preview_of(&m.content),
                })
            })
            .collect();
        let mut meta = Metadata {
            elapsed_ms: start.elapsed().as_millis() as u64,
            ..Default::default()
        };
        meta.add("kb", kb.clone());
        meta.add("dry_run", true);
        meta.add("openrouter_available", llm.is_some());
        let would_process = preview.len();
        print_success(
            ctx.format,
            json!({
                "kb": kb,
                "diary": diary_filter,
                "dry_run": true,
                "memories_total": total,
                "already_processed": total - pending_total,
                "pending": pending_total,
                "would_process": would_process,
                "llm_available": llm.is_some(),
                "memories": preview,
                "note": if llm.is_some() {
                    "re-run without --dry-run to extract; one LLM call per memory"
                } else {
                    "no extraction LLM configured — a real run would exit 2 until a key is set"
                },
            }),
            meta,
            |_| {
                println!(
                    "{would_process} of {pending_total} pending memories would be processed \
                     ({} of {total} already done)",
                    total - pending_total
                );
            },
        );
        return Ok(());
    }

    // Refuse loudly rather than silently doing nothing: an agent that asked
    // for a backfill and got "0 processed" would conclude the store is clean.
    let Some(llm) = llm else {
        return Err(CliError::Config(format!(
            "`facts backfill` needs an extraction LLM — {}. Run with --dry-run to see \
             what would be processed without any API call.",
            crate::fact_llm::no_llm_hint()
        )));
    };
    // One LLM call per memory, so the spend guardrail applies.
    crate::commands::budget::enforce_budget(ctx, Some(&kb))?;
    let model = llm.model();

    // The novelty gate must use the same embedder family the fact
    // embeddings were written with — same rule as `remember`.
    let force_stub = std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let gate_embedder: Box<dyn Embedder> = match gemini_key {
        Some(k) if !force_stub => Box::new(GeminiEmbedder::new(k)),
        _ => Box::new(StubEmbedder::default()),
    };

    let mut outcome = FactWriteOutcome::default();
    let mut processed = 0usize;
    let mut failed = 0usize;
    let mut without_facts = 0usize;
    let queued = queue.len();
    for (i, memory) in queue.iter().enumerate() {
        if llm.is_live() {
            ctx.store.record_usage_event(
                "openrouter",
                "facts_backfill_extract",
                Some(&model),
                Some(&kb),
                Some(&memory.diary),
                1,
                1,
                crate::commands::usage::estimated_tokens(&memory.content),
                0,
                0.0,
                0.0,
                json!({ "estimated": true, "memory_id": memory.id.to_string() }),
            )?;
        }
        let extracted = match extract_facts(llm.as_chat(), &memory.content).await {
            Ok(facts) => facts,
            Err(e) => {
                failed += 1;
                // No marker: a failed memory stays pending for the next run.
                progress(ctx, i + 1, queued, &format!("failed ({e})"), memory);
                continue;
            }
        };
        ctx.store.record_fact_extraction(
            memory.id,
            Some(&model),
            extracted.len() as i64,
            "backfill",
        )?;
        let before = outcome.added;
        let write_ctx = FactWriteCtx {
            memory_id: memory.id,
            diary: &memory.diary,
            kb: &kb,
            event_time: memory.event_time,
            embedder: Some(gate_embedder.as_ref()),
            llm: Some(llm.as_chat()),
            autonomy: Autonomy::default(),
            source_kind: SourceKind::AgentExtracted,
        };
        for ef in &extracted {
            if let Err(e) = write_fact(ctx, &write_ctx, ef, &mut outcome).await {
                tracing::warn!("fact write failed for {:?}: {e}", ef);
            }
        }
        processed += 1;
        if extracted.is_empty() {
            without_facts += 1;
        }
        progress(
            ctx,
            i + 1,
            queued,
            &format!("{} facts", outcome.added - before),
            memory,
        );
    }

    // A failed memory keeps no marker, so it stays in the queue for next time.
    let remaining = pending_total - processed as i64;
    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("kb", kb.clone());
    meta.add("processed", processed);
    meta.add("facts_added", outcome.added);
    meta.add("facts_deduped", outcome.deduped);
    meta.add("conflicts_queued", outcome.queued.len());
    meta.add("extraction_model", model.clone());
    print_success(
        ctx.format,
        json!({
            "kb": kb,
            "diary": diary_filter,
            "memories_total": total,
            "processed": processed,
            "failed": failed,
            "no_facts_found": without_facts,
            "remaining_pending": remaining.max(0),
            "facts_added": outcome.added,
            "facts_deduped": outcome.deduped,
            "conflicts": outcome.conflicts,
            "adjudication_needed": outcome.queued,
            "extraction_model": model,
        }),
        meta,
        |_| {
            println!(
                "Backfilled {processed} memories in kb {kb}: {} facts added, {} deduped, \
                 {} queued for resolution ({} still pending)",
                outcome.added,
                outcome.deduped,
                outcome.queued.len(),
                remaining.max(0),
            );
        },
    );
    Ok(())
}

/// Per-memory progress. Always stderr: stdout carries exactly one JSON
/// envelope, and a long backfill is unwatchable without a heartbeat.
fn progress(
    ctx: &AppContext,
    index: usize,
    total: usize,
    result: &str,
    memory: &engram_core::types::Memory,
) {
    if ctx.quiet {
        return;
    }
    eprintln!(
        "[{index}/{total}] {} {} — {}",
        memory.id,
        result,
        preview_of(&memory.content)
    );
}

fn preview_of(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(72).collect();
    if flat.chars().count() > 72 {
        out.push('…');
    }
    out
}

fn fact_to_json(f: &Fact) -> serde_json::Value {
    json!({
        "id": f.id.to_string(),
        "source_memory_id": f.source_memory_id.to_string(),
        "subject": f.subject,
        "subject_norm": f.subject_norm,
        "predicate": f.predicate,
        "object": f.object,
        "object_norm": f.object_norm,
        "confidence": f.confidence,
        "created_at": f.created_at.to_rfc3339(),
        "superseded_by": f.superseded_by.map(|u| u.to_string()),
        "superseded_at": f.superseded_at.map(|t| t.to_rfc3339()),
        "diary": f.diary,
        "fact_type": f.fact_type.as_str(),
        "valid_from": f.valid_from.map(|t| t.to_rfc3339()),
        "valid_until": f.valid_until.map(|t| t.to_rfc3339()),
        "resolution": f.resolution,
        "source_kind": f.source_kind.as_str(),
        "confirmations": f.confirmations,
    })
}
