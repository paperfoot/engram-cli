//! `engram facts {list,show,conflicts}` — browse the fact store and the
//! contradiction history.
//!
//! The fact store is populated at `engram remember` time via LLM extraction
//! (see `crate::commands::remember`). Conflicts are non-destructive — when a
//! new fact contradicts an existing one, the old fact is marked
//! `superseded_by` the new one rather than deleted, so history is queryable.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_core::types::Fact;
use engram_graph::facts::normalize;
use serde_json::json;

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

pub fn conflicts(ctx: &AppContext, limit: usize) -> Result<(), CliError> {
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
    })
}
