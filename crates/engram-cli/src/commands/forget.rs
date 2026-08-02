//! `engram forget <id> --confirm` — retract a memory.
//!
//! Two things changed in v0.4 and both were lies the old command told. An id
//! that names nothing is now `not_found`, exit 3: v0.3 answered exit 0 with
//! `{"deleted": false}`, so a typo looked exactly like a successful retraction
//! and a caller could believe a memory was gone while it sat in the store. And
//! the retraction no longer erases the memory's facts — storage closes them as
//! intervals with `close_reason = "retracted_source"`, so the claims stop
//! asserting anything from this moment while their history, their conflicts and
//! the decisions a person already made about them survive.
//!
//! Retracting the same memory twice is still exit 0: idempotence is what a
//! retry loop needs, and `already_retracted` says which call did the work.

use crate::commands::memory;
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, id: String, confirm: bool) -> Result<(), CliError> {
    // The destructive guard precedes resolution, deliberately: a command that
    // could delete something must refuse before it so much as reads an id.
    if !confirm {
        return Err(CliError::BadInput(
            "destructive operation — pass --confirm to actually delete".into(),
        ));
    }
    // Same resolution as `edit`, from the same helper: `forget mem:<uuid>` and
    // `forget chunk:<uuid>` are what an agent copies out of a write or a recall
    // response, and rejecting them as malformed uuids was the sharpest edge of
    // the untyped id. A chunk handle retracts the memory the chunk belongs to —
    // a chunk is an index entry, never a unit of retraction — and
    // `resolved_from` says so in the response.
    let target = memory::resolve_memory_target(ctx, &id, "forget")?;

    // Already retracted: no event, no closure, no second answer. The store
    // knows the difference between "unknown id" and "you already did this", so
    // the CLI stops guessing.
    let outcome = if target.retracted {
        None
    } else {
        // `soft_delete_memory` closes the memory's open intervals and appends
        // `memory.retracted` itself, in one transaction (D7). This command
        // calls it and reports what it did; appending the event here too would
        // write it twice and make `admin rebuild` replay the retraction twice.
        ctx.store.soft_delete_memory(target.memory_id)?
    };
    // `None` from storage means the row vanished between resolution and the
    // transaction — a racing `forget`. It collapses onto the idempotent answer,
    // which is the true one: it is retracted, and this call did not do it.
    let (deleted, closed) = match outcome {
        Some(o) => (o.retracted, o.closed),
        None => (false, Vec::new()),
    };
    let closed_ids: Vec<String> = closed.iter().map(|f| f.to_string()).collect();

    let mut meta = Metadata::default();
    meta.add("handle", target.handle.clone());
    if let Some(from) = &target.resolved_from {
        meta.add("resolved_from", from.clone());
    }
    meta.add("kb", target.kb.clone());
    meta.add("already_retracted", !deleted);
    meta.add("facts_closed", closed.len());
    let mut data = serde_json::Map::new();
    // The canonical uuid, never the caller's raw spelling: `data.id` has always
    // been a bare uuid and scripts feed it straight back in.
    data.insert("id".into(), json!(target.memory_id.to_string()));
    data.insert("handle".into(), json!(target.handle));
    if let Some(from) = &target.resolved_from {
        data.insert("resolved_from".into(), json!(from));
    }
    // Unchanged meaning: `true` = THIS call retracted it.
    data.insert("deleted".into(), json!(deleted));
    data.insert("already_retracted".into(), json!(!deleted));
    // Closed, not deleted. `retracted_source`/`caller` per D5's pinned table.
    data.insert("facts_closed".into(), json!(closed.len()));
    data.insert("facts_closed_ids".into(), json!(closed_ids));
    print_success(ctx.format, serde_json::Value::Object(data), meta, |data| {
        if data["deleted"].as_bool().unwrap_or(false) {
            println!(
                "Retracted {} ({} facts closed)",
                data["handle"].as_str().unwrap_or_default(),
                data["facts_closed"],
            );
        } else {
            println!(
                "{} was already retracted — nothing changed",
                data["handle"].as_str().unwrap_or_default(),
            );
        }
    });
    Ok(())
}
