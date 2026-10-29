//! `engram facts {list,show,conflicts,triage,resolve,backfill}` — browse the
//! fact store, the contradiction history and backlog, and retro-extract facts
//! from memories that were written before the fact layer was switched on.
//!
//! The fact store is populated at `engram remember` time via LLM extraction
//! (see `crate::commands::remember`). Conflicts are non-destructive — when a
//! new fact contradicts an existing one, the old fact is marked
//! `superseded_by` the new one rather than deleted, so history is queryable.
//!
//! `conflicts` and `triage` exist because a queue is only useful while a
//! human would actually read it. A real import (1,332 memories → 2,953 facts)
//! queued 9,510 "conflicts", almost all of them second values of multi-valued
//! predicates or similarity-band candidates about other people. `triage`
//! re-runs the current classifier (`crate::fact_write::classify_pair`) over
//! the backlog and closes everything today's write path would never have
//! queued; `conflicts` shows what is left, grouped by subject and predicate.

use crate::context::AppContext;
use crate::error::CliError;
use crate::fact_write::{
    classify_pair, write_fact_batch, Autonomy, ClassifyMode, ConflictClass, FactBatch,
    FactExtractionMarker, FactWriteCtx, FactWriteOutcome, GroundingJudge, MemoryTime, Verdict,
};
use crate::output::{print_success, Listing, Metadata, Scope};
use crate::read_surface::{self, CursorKey};
use crate::respond;
use engram_core::handle::HandleKind;
use engram_core::interval::is_open_fact;
// R3/CM-4: `Arity` in this file is the registry's two-shape arity and nothing
// else. The legacy single/multi projection keeps a legacy NAME at every call
// site, so a reader never has to ask which `Arity` they are looking at.
use engram_core::relations::{
    canonical_relation, is_safety_slot, predicate_arity, relation_spec, Arity, LegacyArity,
};
use engram_core::types::{Fact, SourceKind};
use engram_embed::stub::StubEmbedder;
use engram_embed::Embedder;
use engram_graph::facts::{canonical_predicate, extract_facts, normalize};
use engram_graph::semantics::{
    triage_decision, DecisionVerdict, FactView, SemanticKey, SEMANTIC_KEY_VERSION,
};
use engram_storage::sqlite::FactConflictRow;
use serde_json::json;
use std::collections::HashMap;
use std::time::Instant;
use uuid::Uuid;

/// `engram facts list` — active facts, across EVERY KB by default.
///
/// Defaulting to the KB literally named "default" made this command lie: an
/// agent that wrote to `--kb personal` and then ran `facts list` got
/// `count: 0` and exit 0, which reads as "extraction produced nothing" rather
/// than "you are looking in the wrong namespace". `recall` already defaults to
/// every KB for the same reason; `--kb <name>` narrows, and each fact is
/// labelled with the KB it came from so a cross-KB listing stays legible.
pub fn list(
    ctx: &AppContext,
    subject: Option<String>,
    diary: String,
    kb: Option<String>,
    all: bool,
    limit: usize,
    cursor: Option<String>,
) -> Result<(), CliError> {
    let kb_label = kb
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or("*")
        .to_string();
    let kb_filter = if kb_label == "*" {
        None
    } else {
        Some(kb_label.as_str())
    };
    let diary_filter = if diary == "*" {
        None
    } else {
        Some(diary.as_str())
    };
    let subject_norm = subject.as_deref().map(normalize);
    // The cursor is bound to the filters that produced it: paging a different
    // scope at the same offset is a wrong answer that looks like a right one.
    let key = CursorKey::new(
        "facts.list",
        &[
            ("kb", kb_filter),
            ("diary", diary_filter),
            ("subject", subject_norm.as_deref()),
            ("all", Some(if all { "1" } else { "0" })),
        ],
    );
    let offset = match &cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    let (facts, total, cursor_out) = match &subject_norm {
        // The subject query is UNBOUNDED in storage today — no limit, no
        // offset — so it returns everything and says so rather than
        // pretending to have paged. `total == returned` here is the honest
        // answer, not a coincidence.
        Some(s_norm) => {
            let rows =
                ctx.store
                    .list_facts_by_subject_scoped(s_norm, diary_filter, kb_filter, all)?;
            let total = rows.len() as i64;
            (rows, total, None)
        }
        None => {
            let rows = ctx
                .store
                .list_facts_scoped(diary_filter, kb_filter, all, limit, offset)?;
            let total = ctx.store.count_facts_scoped(diary_filter, kb_filter, all)?;
            let next = read_surface::next_cursor(&key, offset, rows.len(), total);
            (rows, total, next)
        }
    };
    let listing = Listing::page(facts.len(), total, cursor_out);

    let fact_kbs = ctx
        .store
        .kbs_for_facts(&facts.iter().map(|f| f.id).collect::<Vec<_>>())?;
    let labelled = |f: &Fact| {
        let mut v = fact_to_json(f);
        v["kb"] = json!(fact_kbs.get(&f.id).cloned().unwrap_or_default());
        v
    };

    let diary_scope = if diary == "*" { "all_diaries" } else { "flag" };
    let kb_scope = if kb_filter.is_some() {
        "flag"
    } else {
        "all_kbs"
    };
    let serde_json::Value::Object(mut payload) = json!({
        "kb": kb_label,
        "kb_scope": kb_scope,
        "diary": diary,
        "diary_scope": diary_scope,
        "facts": facts.iter().map(labelled).collect::<Vec<_>>(),
    }) else {
        unreachable!("json! object literal");
    };
    // `count` stays and still equals the page length; `total` is the number
    // of facts that match in scope. A caller that saw `count: 50` had no way
    // to know whether there were 50 facts or 5,000.
    listing.attach(&mut payload);
    let empty = facts.is_empty();
    if empty {
        crate::output::Verdict::NoMatch(read_surface::absence_for_listing(
            vec![kb_label.clone()],
            "no_chunks_in_scope",
        ))
        .attach(&mut payload);
    } else {
        crate::output::Verdict::Results.attach(&mut payload);
    }

    let mut meta = Metadata::default();
    meta.add("kb", kb_label.clone());
    meta.add("kb_scope", kb_scope);
    meta.add("diary", diary.clone());
    meta.add("diary_scope", diary_scope);
    Scope::listing(vec![kb_label.clone()], diary.clone(), kb_scope).attach(&mut meta);
    print_success(
        ctx.format,
        serde_json::Value::Object(payload),
        meta,
        |data| {
            let rows = data["facts"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("(no facts)");
                return;
            }
            for f in &rows {
                // A stopped medication must not print as an active one: the
                // label follows `open`, and names the reason it closed.
                let status = if f["open"] == json!(true) {
                    String::new()
                } else {
                    format!(
                        " [closed: {}]",
                        f["close_reason"].as_str().unwrap_or("unknown")
                    )
                };
                println!(
                    "  [{}] {} -- {} -> {}  (conf={:.2}){}",
                    f["kb"].as_str().unwrap_or("?"),
                    f["subject"].as_str().unwrap_or(""),
                    f["predicate"].as_str().unwrap_or(""),
                    f["object"].as_str().unwrap_or(""),
                    f["confidence"].as_f64().unwrap_or(0.0),
                    status
                );
            }
            println!();
            println!(
                "{} of {} facts",
                rows.len(),
                data["total"].as_i64().unwrap_or(rows.len() as i64)
            );
            if let Some(c) = data["cursor"].as_str() {
                println!("more: engram facts list --cursor {c}");
            }
        },
    );
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

    // THE SWEEP. `superseded_by.is_none()` called a stopped medication
    // active: closing an interval with reason `stopped` leaves no successor,
    // so the old test read the row as current and the store answered from it.
    let active: Vec<&Fact> = facts.iter().filter(|f| is_open_fact(f)).collect();
    let historic: Vec<&Fact> = facts.iter().filter(|f| !is_open_fact(f)).collect();

    // `show` is unlimited by construction, so `total == returned` on both
    // arrays — said out loud rather than left for a caller to assume.
    let payload = json!({
        "subject": subject,
        "subject_norm": s_norm,
        "kb": kb,
        "active_count": active.len(),
        "historic_count": historic.len(),
        "returned": active.len() + historic.len(),
        "total": (active.len() + historic.len()) as i64,
        "count": active.len() + historic.len(),
        "active":  active.iter().map(|f| fact_to_json(f)).collect::<Vec<_>>(),
        "historic": historic.iter().map(|f| fact_to_json(f)).collect::<Vec<_>>(),
    });

    let mut meta = Metadata::default();
    Scope::listing(
        vec![kb.clone()],
        diary.clone(),
        if kb_filter.is_some() {
            "flag"
        } else {
            "all_kbs"
        },
    )
    .attach(&mut meta);
    print_success(ctx.format, payload, meta, |_| {
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
            println!("History (closed intervals):");
            for f in &historic {
                // `valid_until` first: it is when the state ENDED.
                // `superseded_at` is a transaction timestamp and only ever a
                // fallback for rows written before the interval columns.
                let when = f
                    .valid_until
                    .or(f.superseded_at)
                    .map(|t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "?".into());
                println!(
                    "  {} -> {}  (was true until {}; {})",
                    f.predicate,
                    f.object,
                    when,
                    f.close_reason.as_deref().unwrap_or("closed")
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
    subject: Option<String>,
    cursor: Option<String>,
) -> Result<(), CliError> {
    // A subject filter only means something for the queue — the supersession
    // history is a chronological ledger, not a per-person view.
    if open || subject.is_some() {
        return open_conflicts(ctx, limit, diary, kb, subject, cursor);
    }
    let key = CursorKey::new(
        "facts.conflicts",
        &[("kb", kb.as_deref()), ("diary", diary.as_deref())],
    );
    let offset = match &cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    // The history view accepted --kb and --diary and ignored both. It reads
    // them now, which NARROWS what a scoped caller sees; the narrowing is the
    // fix, and `metadata.scope` says which scope produced the answer.
    let pairs = ctx
        .store
        .list_recent_conflicts(diary.as_deref(), kb.as_deref(), limit, offset)?;
    let total = ctx
        .store
        .count_recent_conflicts(diary.as_deref(), kb.as_deref())?;
    let listing = Listing::page(
        pairs.len(),
        total,
        read_surface::next_cursor(&key, offset, pairs.len(), total),
    );
    let open_count = ctx
        .store
        .count_open_fact_conflicts(diary.as_deref(), kb.as_deref())?;
    let serde_json::Value::Object(mut payload) = json!({
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
        // The backlog is invisible from the history view, and a caller who
        // never learns it exists never triages it.
        "open_count": open_count,
        "next_action": if open_count > 0 {
            Some("engram facts conflicts --open  (review) / engram facts triage --dry-run  (auto-resolve the obvious)")
        } else {
            None
        },
    }) else {
        unreachable!("json! object literal");
    };
    listing.attach(&mut payload);
    let mut meta = Metadata::default();
    Scope::listing(
        vec![kb.clone().unwrap_or_else(|| "*".into())],
        diary.clone().unwrap_or_else(|| "*".into()),
        if kb.is_some() { "flag" } else { "all_kbs" },
    )
    .attach(&mut meta);
    print_success(ctx.format, serde_json::Value::Object(payload), meta, |_| {
        if pairs.is_empty() {
            println!("(no conflicts recorded)");
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
        if !pairs.is_empty() {
            println!();
            println!("{} of {total} conflicts", pairs.len());
        }
        if open_count > 0 {
            println!(
                "{open_count} unresolved — `engram facts conflicts --open` to review, \
                 `engram facts triage --dry-run` to auto-resolve the obvious ones"
            );
        }
    });
    Ok(())
}

/// Unresolved (contested) conflicts awaiting a decision, grouped by subject
/// and predicate so the SHAPE of a backlog is visible: 400 rows on one
/// subject's `knows` is a different problem from 400 scattered slot
/// contradictions. Each entry carries both values with the sentence each came
/// from, the classifier's current verdict, and the exact resolve command.
fn open_conflicts(
    ctx: &AppContext,
    limit: usize,
    diary: Option<String>,
    kb: Option<String>,
    subject: Option<String>,
    cursor: Option<String>,
) -> Result<(), CliError> {
    let key = CursorKey::new(
        "facts.conflicts.open",
        &[
            ("kb", kb.as_deref()),
            ("diary", diary.as_deref()),
            ("subject", subject.as_deref()),
        ],
    );
    let offset = match &cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    let rows = ctx.store.list_open_fact_conflicts_with_facts(
        diary.as_deref(),
        kb.as_deref(),
        subject.as_deref(),
        limit,
        offset,
    )?;
    let total_open = ctx
        .store
        .count_open_fact_conflicts(diary.as_deref(), kb.as_deref())?;
    let fact_ids: Vec<Uuid> = rows
        .iter()
        .flat_map(|(_, n, o)| [n.id, o.id])
        .collect::<Vec<_>>();
    let sources = ctx.store.fact_source_previews(&fact_ids)?;

    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|(row, new_f, old_f)| conflict_entry(row, new_f, old_f, &sources))
        .collect();
    let groups = group_conflicts(&rows, &entries);
    let auto_resolvable = entries
        .iter()
        .filter(|e| e["triage"]["auto_resolvable"] == json!(true))
        .count();

    let overridden = entries
        .iter()
        .filter(|e| e["suggested_action_overridden"] == json!(true))
        .count();
    let serde_json::Value::Object(mut payload) = json!({
        // `total_open` was already the honest backlog here; `total` is the
        // canonical spelling of the same number, and `count` stays equal to
        // the page.
        "total_open": total_open,
        "kb": kb.clone().unwrap_or_else(|| "*".into()),
        "subject": subject,
        "auto_resolvable": auto_resolvable,
        "group_count": groups.len(),
        "groups": groups,
        // Flat list kept for callers written against the original shape.
        "open_conflicts": entries.clone(),
    }) else {
        unreachable!("json! object literal");
    };
    Listing::page(
        entries.len(),
        total_open,
        read_surface::next_cursor(&key, offset, entries.len(), total_open),
    )
    .attach(&mut payload);
    let mut meta = Metadata::default();
    meta.suggested_action_overridden(overridden);
    Scope::listing(
        vec![kb.clone().unwrap_or_else(|| "*".into())],
        diary.clone().unwrap_or_else(|| "*".into()),
        if kb.is_some() { "flag" } else { "all_kbs" },
    )
    .attach(&mut meta);
    print_success(
        ctx.format,
        serde_json::Value::Object(payload),
        meta,
        |data| {
            if entries.is_empty() {
                println!("(no open conflicts — nothing awaiting resolution)");
                return;
            }
            for g in data["groups"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{} — {} [{}] ({} open{})",
                    g["subject"].as_str().unwrap_or("?"),
                    g["predicate"].as_str().unwrap_or("?"),
                    g["kb"].as_str().unwrap_or("?"),
                    g["conflict_count"].as_u64().unwrap_or(0),
                    if g["safety_slot"] == json!(true) {
                        ", safety slot"
                    } else {
                        ""
                    },
                );
                for e in g["conflicts"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  {}  '{}' vs '{}'  ({})",
                        e["conflict_id"].as_str().unwrap_or("?"),
                        e["old"]["object"].as_str().unwrap_or("?"),
                        e["new"]["object"].as_str().unwrap_or("?"),
                        e["triage"]["rule"].as_str().unwrap_or("?"),
                    );
                }
            }
            println!();
            println!(
            "{} of {total_open} open conflicts shown; {auto_resolvable} would be auto-resolved by \
             `engram facts triage`",
            entries.len()
        );
        },
    );
    Ok(())
}

/// One queued conflict rendered for a human: both values, where each came
/// from, and what `facts triage` would do with it today.
fn conflict_entry(
    row: &FactConflictRow,
    new_f: &Fact,
    old_f: &Fact,
    sources: &HashMap<Uuid, (Uuid, String)>,
) -> serde_json::Value {
    let verdict = classify_pair(new_f, old_f, row.score, ClassifyMode::Triage);
    let safety = is_safety_slot(&canonical_relation(&new_f.predicate))
        || is_safety_slot(&canonical_relation(&old_f.predicate));
    // R7's read-side guard. The stored value on a pre-wave-3 row may say
    // `supersede-old` on a medication slot; an agent that executes advice
    // retires a live prescription, so the live rule wins on read too.
    let suggested = read_surface::suggested_action_for(row.suggested.as_deref(), safety);
    let serde_json::Value::Object(mut entry) = json!({
        "conflict_id": row.id.to_string(),
        "handle": read_surface::handle(HandleKind::Conflict, row.id),
        "kb": row.kb,
        "diary": row.diary,
        "detector": row.detector,
        "score": row.score,
        "suggested_action": suggested,
        "suggested_action_overridden":
            read_surface::suggested_action_overridden(row.suggested.as_deref(), suggested),
        "safety": safety,
        // NULL on any queue row written before the v10 migration. Null means
        // "pre-v10 row", not "unknown".
        "decision_key": row.decision_key,
        "member_key": row.member_key,
        "registry_version": row.registry_version,
        "created_at": row.created_at,
        "new": conflict_side(new_f, sources),
        "old": conflict_side(old_f, sources),
        "triage": verdict_json(&verdict),
    }) else {
        unreachable!("json! object literal");
    };
    // ONE runnable command, no pipes. The old template shipped
    // `--action supersede-old|keep-both|reject-new`, and a paste of that runs
    // the destructive branch. Permission (`allowed_actions`) is separate from
    // advice (`suggested_action`) and is never narrowed by safety: a caller
    // decision is authority, R7 only constrains what we recommend.
    respond::attach(
        &mut entry,
        &respond::resolve_conflict(row.id, crate::commands::recall::ALLOWED_ACTIONS),
    );
    serde_json::Value::Object(entry)
}

fn conflict_side(f: &Fact, sources: &HashMap<Uuid, (Uuid, String)>) -> serde_json::Value {
    let mut v = fact_to_json(f);
    if let Some((memory_id, preview)) = sources.get(&f.id) {
        v["source_memory_id"] = json!(memory_id.to_string());
        v["source_preview"] = json!(preview);
    }
    v
}

fn verdict_json(v: &Verdict) -> serde_json::Value {
    json!({
        "class": class_name(v.class),
        "rule": v.rule,
        "auto_resolvable": v.is_auto_resolvable(),
    })
}

fn class_name(c: ConflictClass) -> &'static str {
    match c {
        ConflictClass::Coexist => "coexist",
        ConflictClass::Duplicate => "duplicate",
        ConflictClass::SupersedeOld => "supersede_old",
        ConflictClass::SupersedeNew => "supersede_new",
        ConflictClass::Settled => "settled",
        ConflictClass::Queue => "queue",
    }
}

/// Group conflict entries by (kb, subject, predicate), newest group first.
fn group_conflicts(
    rows: &[(FactConflictRow, Fact, Fact)],
    entries: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut order: Vec<(String, String, String)> = Vec::new();
    let mut buckets: HashMap<(String, String, String), Vec<serde_json::Value>> = HashMap::new();
    for ((row, new_f, old_f), entry) in rows.iter().zip(entries.iter()) {
        let predicate = if new_f.predicate == old_f.predicate {
            new_f.predicate.clone()
        } else {
            format!("{} / {}", new_f.predicate, old_f.predicate)
        };
        let key = (row.kb.clone(), new_f.subject.clone(), predicate);
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(entry.clone());
    }
    order
        .into_iter()
        .map(|key| {
            let members = buckets.remove(&key).unwrap_or_default();
            let (kb, subject, predicate) = key;
            // One canonicalization before the registry is consulted: the
            // extractor's spelling map and the relation registry are different
            // tables, and looking a raw predicate up in the second is how a
            // safety slot reads as an ordinary one.
            let canonical = canonical_relation(&canonical_predicate(
                predicate.split(" / ").next().unwrap_or(&predicate),
            ));
            json!({
                "kb": kb,
                "subject": subject,
                "predicate": predicate,
                // Unchanged JSON, unambiguous type: the legacy projection
                // keeps a legacy NAME so nobody has to ask which `Arity` a
                // call site means (R3).
                "arity": match predicate_arity(&canonical) {
                    LegacyArity::Single => "single",
                    LegacyArity::Multi => "multi",
                },
                "arity_class": match relation_spec(&canonical).arity {
                    Arity::SetValued { .. } => "set_valued",
                    Arity::SingleValued { .. } => "single_valued",
                },
                // `safety_slot` keeps its name and value: agent-info
                // documents it and a rename breaks a documented field.
                "safety_slot": is_safety_slot(&canonical),
                "safety": is_safety_slot(&canonical),
                "conflict_count": members.len(),
                "newest_at": members.first().and_then(|m| m["created_at"].as_str()).unwrap_or(""),
                "conflicts": members,
            })
        })
        .collect()
}

/// Resolve a queued conflict. The caller decides; engram records the
/// decision and applies the supersession (never a deletion).
///
/// The whole sequence — claim, decision, event, supersession — runs inside one
/// `Core::write` transaction. It used to be five unguarded statements whose
/// booleans were discarded, so two concurrent resolves both passed the
/// `status == "open"` check, both superseded a fact, and both printed
/// `"resolved": true`; with different actions the pair could end up with each
/// fact superseded by the other. The claim's boolean is now load-bearing and
/// the loser gets `conflict_already_resolved` (exit 3), which an agent can
/// tell apart from a typo.
pub async fn resolve(
    ctx: &AppContext,
    id: String,
    action: String,
    note: Option<String>,
) -> Result<(), CliError> {
    let qid = uuid::Uuid::parse_str(id.strip_prefix("conflict:").unwrap_or(&id))
        .map_err(|_| CliError::BadInput(format!("'{id}' is not a valid conflict id")))?;
    let parsed = respond::ResolveAction::parse(&action).ok_or_else(|| {
        CliError::BadInput(format!(
            "unknown action '{action}' (expected {})",
            respond::ResolveAction::ALL
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join("|")
        ))
    })?;
    let applied = ctx
        .core
        .resolve_conflict(crate::engine::ResolveRequest {
            conflict_id: qid,
            action: parsed,
            note,
            actor: ctx.core.actor(),
        })
        .await?;

    let result = match &applied.closed {
        Some(closed) => json!({
            "kept": fact_to_json(&applied.kept[0]),
            "superseded": fact_to_json(closed),
        }),
        None => json!({
            "kept": applied.kept.iter().map(fact_to_json).collect::<Vec<_>>(),
            "note": "both facts remain active; recorded as non-contradictory",
        }),
    };
    let payload = json!({
        "conflict_id": applied.conflict_id.to_string(),
        "handle": read_surface::handle(HandleKind::Conflict, applied.conflict_id),
        "action": applied.action,
        "resolved": true,
        // The decision is the artefact, `keep-both` included: without it the
        // caller's answer is forgotten and the same question is asked again on
        // every re-derivation.
        "decision_id": applied.decision_id.to_string(),
        "result": result,
    });
    print_success(ctx.format, payload, Metadata::default(), |_| {
        println!("Resolved conflict {qid} with action {action}");
    });
    Ok(())
}

/// `engram facts triage [--kb K] [--dry-run]` — re-apply today's conflict
/// classifier to the conflicts that are ALREADY queued and close everything
/// the current write path would never have queued.
///
/// This is the command that turns an unusable backlog back into a review
/// list. Three properties it must have, and that its tests pin:
/// - **safe**: nothing touching a safety slot (allergies, medication,
///   diagnoses, blood type) is ever auto-resolved, whatever the evidence;
/// - **idempotent**: a second run resolves nothing, because the rules are the
///   same pure function the first run used;
/// - **honest**: `--dry-run` writes nothing at all — not even the orphan
///   sweep — and reports the identical numbers a real run would produce.
///
/// Auto-resolution is never destructive: coexisting values are simply
/// un-queued (both stay active), and the few classes that do resolve a slot
/// supersede one fact rather than deleting it, so `facts show` still has the
/// full history.
pub fn triage(
    ctx: &AppContext,
    kb: Option<String>,
    diary: Option<String>,
    subject: Option<String>,
    limit: Option<usize>,
    dry_run: bool,
) -> Result<(), CliError> {
    let start = Instant::now();
    let kb_filter = kb
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty() && *k != "*");
    let diary_filter = diary
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != "*");
    let open_before = ctx
        .store
        .count_open_fact_conflicts(diary_filter, kb_filter)?;
    let rows = ctx.store.list_open_fact_conflicts_with_facts(
        diary_filter,
        kb_filter,
        subject.as_deref(),
        limit.unwrap_or(usize::MAX),
        0,
    )?;

    let mut resolved_by_rule: HashMap<&'static str, usize> = HashMap::new();
    let mut kept_by_reason: HashMap<&'static str, usize> = HashMap::new();
    let mut resolved_samples: Vec<serde_json::Value> = Vec::new();
    let mut kept_samples: Vec<serde_json::Value> = Vec::new();
    let mut facts_superseded = 0usize;
    let mut resolved = 0usize;
    // Facts this run has already superseded: a later row that references one
    // of them is settled, not a second decision to make.
    let mut settled_facts: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    let mut apply =
        |store: &engram_storage::SqliteStore| -> Result<(), engram_storage::StorageError> {
            for (row, new_f, old_f) in &rows {
                let mut verdict = classify_pair(new_f, old_f, row.score, ClassifyMode::Triage);
                // Defence in depth: a bulk sweep may never supersede a safety
                // fact, whatever a future classifier rule decides. The classifier
                // already queues same-subject safety pairs; this makes the
                // guarantee structural rather than a property of one function.
                if matches!(
                    verdict.class,
                    ConflictClass::Duplicate
                        | ConflictClass::SupersedeOld
                        | ConflictClass::SupersedeNew
                ) && (is_safety_slot(&canonical_predicate(&new_f.predicate))
                    || is_safety_slot(&canonical_predicate(&old_f.predicate)))
                {
                    verdict = Verdict::new(ConflictClass::Queue, "safety_slot");
                }
                // An earlier row in this run already superseded one of the two
                // facts, so this pair has nothing left to decide — the snapshot
                // we classified from predates that write. Without this, those
                // rows would sit in the queue until a SECOND triage run noticed
                // them, and "idempotent" would mean "converges eventually".
                // A coexistence verdict keeps its (more informative) rule name:
                // it is resolved either way and touches no fact.
                if !matches!(verdict.class, ConflictClass::Coexist)
                    && (settled_facts.contains(&new_f.id) || settled_facts.contains(&old_f.id))
                {
                    verdict = Verdict::new(ConflictClass::Settled, "already_settled");
                }
                if verdict.class == ConflictClass::Queue {
                    *kept_by_reason.entry(verdict.rule).or_insert(0) += 1;
                    if kept_samples.len() < 5 {
                        kept_samples.push(triage_sample(row, new_f, old_f, &verdict));
                    }
                    continue;
                }
                // Claim BEFORE any mutation (final-review finding 1): the
                // queue row is the lock, and `engine/resolve.rs` already
                // learned this order the hard way. A caller's `facts resolve`
                // that lands between our snapshot and this write WINS — the
                // sweep must never touch the facts of a question somebody just
                // answered. `Ok(None)` is a lost claim: skip the pair, count
                // it, and let the next listing show whatever the winner did.
                if !dry_run {
                    let key = match row.decision_key.clone() {
                        Some(k) if !k.is_empty() => k,
                        _ => SemanticKey::for_pair(&FactView::of(new_f), &FactView::of(old_f))
                            .encode(),
                    };
                    let decided = triage_decision(&verdict);
                    // The value this sweep retires [G3]. `close_old` retires
                    // the row's old fact, `reject_new` (and the duplicate that
                    // maps onto it) the new one; a `keep_both` or a `void`
                    // retires nothing. Recorded on the decision so it survives
                    // the queue row that a `kb delete` can cascade away.
                    let loser = match decided {
                        DecisionVerdict::CloseOld => Some(old_f.object_norm.as_str()),
                        DecisionVerdict::RejectNew => Some(new_f.object_norm.as_str()),
                        DecisionVerdict::KeepBoth | DecisionVerdict::Void => None,
                    };
                    let claimed = store.resolve_conflict_with_decision(
                        row.id,
                        &row.kb,
                        &row.diary,
                        &key,
                        decided.as_str(),
                        loser,
                        "machine",
                        "triage",
                        Some(&format!(
                            "auto: {} ({})",
                            verdict.rule,
                            class_name(verdict.class)
                        )),
                        SEMANTIC_KEY_VERSION,
                    )?;
                    if claimed.is_none() {
                        *kept_by_reason.entry("lost_claim").or_insert(0) += 1;
                        continue;
                    }
                }
                if resolved_samples.len() < 5 {
                    resolved_samples.push(triage_sample(row, new_f, old_f, &verdict));
                }
                *resolved_by_rule.entry(verdict.rule).or_insert(0) += 1;
                resolved += 1;
                // The audit tag lands in `facts.resolution`, so `facts show`
                // says WHY a value became history months later. A dry run walks
                // the same branches and records the same settlements — it just
                // never writes — so its report is the real run's report.
                let tag = triage_resolution(&verdict);
                let supersede = |loser: Uuid, winner: Uuid, until| {
                    if dry_run {
                        return Ok(true);
                    }
                    store.supersede_fact_with(loser, winner, tag, until)
                };
                match verdict.class {
                    // Both values are true: un-queue, touch neither fact.
                    ConflictClass::Coexist | ConflictClass::Settled | ConflictClass::Queue => {}
                    ConflictClass::Duplicate => {
                        if supersede(new_f.id, old_f.id, None)? {
                            settled_facts.insert(new_f.id);
                            if !dry_run {
                                store.increment_fact_confirmations(old_f.id)?;
                                facts_superseded += 1;
                            }
                        }
                    }
                    ConflictClass::SupersedeOld => {
                        if supersede(old_f.id, new_f.id, new_f.valid_from)? {
                            settled_facts.insert(old_f.id);
                            if !dry_run {
                                facts_superseded += 1;
                            }
                        }
                    }
                    ConflictClass::SupersedeNew => {
                        if supersede(new_f.id, old_f.id, old_f.valid_from)? {
                            settled_facts.insert(new_f.id);
                            if !dry_run {
                                facts_superseded += 1;
                            }
                        }
                    }
                }
            }
            Ok(())
        };

    let orphans_closed = if dry_run {
        // The classifier still runs — the numbers a dry run reports are the
        // numbers the real run produces — but every write is skipped inside
        // the loop, and the orphan sweep never happens.
        apply(&ctx.store)?;
        0
    } else {
        // One transaction: a half-triaged queue is worse than an untriaged one.
        ctx.store.with_tx(&mut apply)?;
        ctx.store
            .resolve_orphan_fact_conflicts(diary_filter, kb_filter, "triage")?
    };

    let scanned = rows.len();
    let remaining = if dry_run {
        open_before - resolved as i64
    } else {
        ctx.store
            .count_open_fact_conflicts(diary_filter, kb_filter)?
    };
    let ratio = if scanned == 0 {
        0.0
    } else {
        resolved as f64 / scanned as f64
    };
    // Both rules keep a pair BECAUSE it is safety-critical: `safety_slot` is
    // the gate's downgrade, `safety_member_change` is a dose change the
    // classifier queued outright. Counting only the first reported zero for
    // exactly the sweep this number exists to describe.
    let safety_kept = kept_by_reason.get("safety_slot").copied().unwrap_or(0)
        + kept_by_reason
            .get("safety_member_change")
            .copied()
            .unwrap_or(0);

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("kb", kb_filter.unwrap_or("*").to_string());
    meta.add("dry_run", dry_run);
    meta.add("resolved", resolved);
    meta.add("remaining", remaining);
    let payload = json!({
        "kb": kb_filter.unwrap_or("*"),
        "diary": diary_filter,
        "subject": subject,
        "dry_run": dry_run,
        "open_before": open_before,
        "scanned": scanned,
        "resolved": resolved,
        "orphans_closed": orphans_closed,
        "facts_superseded": facts_superseded,
        "remaining": remaining.max(0),
        "reduction_ratio": (ratio * 1000.0).round() / 1000.0,
        "resolved_by_rule": map_json(&resolved_by_rule),
        "remaining_by_reason": map_json(&kept_by_reason),
        "safety_slots_kept": safety_kept,
        "resolved_samples": resolved_samples,
        "remaining_samples": kept_samples,
        "note": if dry_run {
            "dry run — nothing was written; re-run without --dry-run to apply"
        } else {
            "auto-resolutions are non-destructive: coexisting values stay active, \
             superseded values remain queryable via `engram facts show <subject>`"
        },
    });
    print_success(ctx.format, payload, meta, |_| {
        if scanned == 0 {
            println!("(no open conflicts to triage)");
            return;
        }
        println!(
            "{}{resolved} of {scanned} open conflicts auto-resolved ({:.1}%), {} left for review",
            if dry_run { "[dry run] " } else { "" },
            ratio * 100.0,
            remaining.max(0),
        );
        // Both breakdowns, because the interesting question after a bulk
        // sweep is not only what went but what is still waiting.
        for (heading, counts) in [
            ("resolved", &resolved_by_rule),
            ("left for review", &kept_by_reason),
        ] {
            if counts.is_empty() {
                continue;
            }
            println!("  {heading}:");
            let mut rules: Vec<_> = counts.iter().collect();
            rules.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (rule, n) in rules {
                println!("  {n:>6}  {rule}");
            }
        }
    });
    Ok(())
}

/// Audit tag written into `facts.resolution` when triage settles a slot.
/// Distinct per rule so a supersession is traceable to the reasoning that
/// caused it, and distinguishable from a write-time or caller decision.
fn triage_resolution(v: &Verdict) -> &'static str {
    match (v.class, v.rule) {
        (ConflictClass::Duplicate, _) => "triage_duplicate",
        (_, "confidence_gap") => "triage_confidence",
        (ConflictClass::SupersedeNew, _) => "triage_stale",
        _ => "triage_recency",
    }
}

fn map_json(m: &HashMap<&'static str, usize>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    let mut pairs: Vec<_> = m.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (k, v) in pairs {
        out.insert((*k).to_string(), json!(v));
    }
    serde_json::Value::Object(out)
}

fn triage_sample(
    row: &FactConflictRow,
    new_f: &Fact,
    old_f: &Fact,
    verdict: &Verdict,
) -> serde_json::Value {
    json!({
        "conflict_id": row.id.to_string(),
        "kb": row.kb,
        "subject": new_f.subject,
        "predicate": new_f.predicate,
        "old": { "predicate": old_f.predicate, "object": old_f.object, "subject": old_f.subject },
        "new": { "predicate": new_f.predicate, "object": new_f.object, "subject": new_f.subject },
        "triage": verdict_json(verdict),
    })
}

/// `engram facts backfill [--kb <kb>]` — run fact extraction over memories
/// that were stored before extraction was on (or while no key was configured).
///
/// Idempotent by provenance: a memory is skipped once it has facts or an
/// extraction marker, so a re-run costs nothing and processes only what is
/// genuinely new. Oldest first, so a `--limit`ed run always advances and a
/// later run resumes where the previous one stopped.
///
/// `--kb` is OPTIONAL: omitted, the run sweeps every KB, oldest-first across
/// the union. Hard-requiring it meant an agent had to already know every
/// namespace it had ever written to before it could catch up the fact layer —
/// and a KB it forgot stayed permanently un-extracted with no signal. Each
/// memory's facts are written back into that memory's own KB.
pub async fn backfill(
    ctx: &AppContext,
    kb: Option<String>,
    diary: Option<String>,
    limit: Option<usize>,
    dry_run: bool,
) -> Result<(), CliError> {
    let start = Instant::now();
    let diary_filter = diary.as_deref().filter(|d| *d != "*");
    let kb_label = kb
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or("*")
        .to_string();
    let kb_filter = if kb_label == "*" {
        None
    } else {
        Some(kb_label.as_str())
    };
    let llm = crate::fact_llm::resolve()?;
    // Before the sweep, not per memory: a bad threshold would otherwise fail
    // every batch and read as N transient failures.
    crate::fact_write::gate_thresholds()?;
    let (total, pending_total) = ctx
        .store
        .fact_extraction_coverage(kb_filter, diary_filter)?;
    let queue = ctx
        .store
        .list_memories_pending_fact_extraction(kb_filter, diary_filter, limit)?;

    if dry_run {
        let preview: Vec<serde_json::Value> = queue
            .iter()
            .map(|(mkb, m)| {
                json!({
                    "memory_id": m.id.to_string(),
                    "kb": mkb,
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
        meta.add("kb", kb_label.clone());
        meta.add("dry_run", true);
        meta.add("openrouter_available", llm.is_some());
        let would_process = preview.len();
        print_success(
            ctx.format,
            json!({
                "kb": kb_label,
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
    // One LLM call per memory, so the spend guardrail applies. A cross-KB
    // sweep is checked against the global budget.
    crate::commands::budget::enforce_budget(ctx, kb_filter)?;
    let model = llm.model();
    // Same grounding as `remember`: backfill runs the same extractor.
    let grounding = GroundingJudge::resolve(false)?;

    // The novelty gate must use the same embedder family the fact
    // embeddings were written with — same rule as `remember`.
    let force_stub = crate::commands::config::stub_forced();
    let gemini_key = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini");
    let gate_embedder: Box<dyn Embedder> = match gemini_key {
        Some(k) if !force_stub => Box::new(crate::retrieval::cloud_gemini(k)),
        _ => Box::new(StubEmbedder::default()),
    };

    let mut outcome = FactWriteOutcome::default();
    let mut processed = 0usize;
    let mut failed = 0usize;
    let mut without_facts = 0usize;
    let queued = queue.len();
    for (i, (memory_kb, memory)) in queue.iter().enumerate() {
        if llm.is_live() {
            ctx.store.record_usage_event(
                "openrouter",
                "facts_backfill_extract",
                Some(&model),
                Some(memory_kb),
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
        let extracted = match extract_facts(
            llm.as_chat(),
            &memory.content,
            memory.event_time.unwrap_or(memory.created_at),
        )
        .await
        {
            Ok(facts) => facts,
            Err(e) => {
                failed += 1;
                // No marker: a failed memory stays pending for the next run.
                progress(ctx, i + 1, queued, &format!("failed ({e})"), memory);
                continue;
            }
        };
        let before = outcome.added;
        let write_ctx = FactWriteCtx {
            memory_id: memory.id,
            diary: &memory.diary,
            // Each memory's facts belong to that memory's OWN KB — a sweep
            // must not relabel them with the flag's (possibly absent) scope.
            kb: memory_kb,
            // A backfilled memory is usually OLD: its facts inherit its own
            // date, not the date of the sweep that finally read it.
            memory_time: MemoryTime {
                event_time: memory.event_time,
                created_at: memory.created_at,
            },
            embedder: Some(gate_embedder.as_ref()),
            llm: Some(llm.as_chat()),
            autonomy: Autonomy::default(),
            source_kind: SourceKind::AgentExtracted,
        };
        let marker_hash = engram_core::keys::content_hash(&memory.content);
        let batch = FactBatch {
            facts: &extracted,
            marker: Some(FactExtractionMarker {
                model: Some(&model),
                source: "backfill",
                content_hash: &marker_hash,
            }),
            source_text: &memory.content,
            grounding: grounding.as_ref(),
        };
        // Same rule as `remember`: the facts and the marker that retires this
        // memory from the queue commit together, so a failed memory keeps
        // neither and the next sweep retries it whole. A marker written on the
        // strength of a half-written batch is what made the rest of the batch
        // invisible to every future backfill.
        if let Err(failure) = write_fact_batch(ctx, &write_ctx, &batch, &mut outcome).await {
            failed += 1;
            progress(
                ctx,
                i + 1,
                queued,
                &format!(
                    "{} ({}) — nothing stored, still pending",
                    failure.what_failed(),
                    failure.error
                ),
                memory,
            );
            continue;
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
    meta.add("kb", kb_label.clone());
    meta.add("processed", processed);
    meta.add("facts_added", outcome.added);
    meta.add("facts_deduped", outcome.deduped);
    meta.add("conflicts_queued", outcome.queued.len());
    meta.add("date_parse_warnings", outcome.date_warnings.len());
    meta.add("extraction_model", model.clone());
    for (name, counts) in outcome.counters() {
        meta.add(name, counts);
    }
    if let Some(note) = gate_embedder.degradation_note() {
        meta.add("embedder_degraded", note.clone());
        eprintln!("warning: {note}");
    }
    let mut data = json!({
            "kb": kb_label,
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
            "date_parse_warnings": outcome.date_warnings,
            "plan_notes": outcome.plan_notes,
            "extraction_model": model,
    });
    crate::commands::remember::with_grounding(&mut data, std::mem::take(&mut outcome.grounding));
    print_success(ctx.format, data, meta, |_| {
        println!(
            "Backfilled {processed} memories in kb {kb_label}: {} facts added, {} deduped, \
                 {} queued for resolution ({} still pending)",
            outcome.added,
            outcome.deduped,
            outcome.queued.len(),
            remaining.max(0),
        );
    });
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
    let canonical = canonical_relation(&f.predicate);
    json!({
        "id": f.id.to_string(),
        "source_memory_id": f.source_memory_id.to_string(),
        // Same name as every other surface uses for the same thing, plus the
        // handle a mutator accepts. `id` is unchanged.
        "memory_id": f.source_memory_id.to_string(),
        "handle": read_surface::handle(HandleKind::Fact, f.id),
        "provenance": read_surface::Provenance {
            memory_id: Some(f.source_memory_id),
            kb: Some(f.kb.clone()),
            diary: Some(f.diary.clone()),
            derived_from: Some(read_surface::DERIVED_FACT),
            ..Default::default()
        }.to_json(),
        // The single definition of "still asserted". A fact with a closed
        // interval and no successor — a medication the user STOPPED — reads
        // as false here, where `superseded_by` alone read it as current.
        "open": is_open_fact(f),
        "closed_by": f.closed_by.map(|u| u.to_string()),
        "close_reason": f.close_reason,
        "close_authority": f.close_authority,
        // Computed from the registry at read time, so they are right on a
        // pre-v10 store too.
        "arity_class": match relation_spec(&canonical).arity {
            Arity::SetValued { .. } => "set_valued",
            Arity::SingleValued { .. } => "single_valued",
        },
        "safety": is_safety_slot(&canonical),
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
