//! `engram admin rebuild` — drop the fact and adjudication projections and
//! replay them from the journal.
//!
//! Contains no SQL. The whole algorithm is `store.rebuild_projections`, which
//! is what lets the byte-identity property be tested without spawning a binary.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn rebuild(
    ctx: &AppContext,
    kb: Option<String>,
    dry_run: bool,
    confirm: bool,
) -> Result<(), CliError> {
    if !dry_run && !confirm {
        // The repo's precedent for a destructive command is `--confirm`, and
        // the refusal names the exact command to re-run rather than the flag
        // in the abstract.
        let scope = kb
            .as_deref()
            .map(|k| format!(" --kb {k}"))
            .unwrap_or_default();
        return Err(CliError::BadInput(format!(
            "rebuild drops and replays the fact and adjudication projections. \
             Rehearse it with `engram admin rebuild{scope} --dry-run`, then run \
             `engram admin rebuild{scope} --confirm`"
        )));
    }
    let report = ctx.store.rebuild_projections(kb.as_deref(), dry_run)?;
    let mut meta = Metadata::default();
    meta.add("events_replayed", report.events_replayed);
    meta.add("dry_run", report.dry_run);

    let skipped: Vec<serde_json::Value> = report
        .skipped
        .iter()
        .map(|s| json!({ "table": s.table, "reason": s.reason }))
        .collect();
    let data = json!({
        "dry_run": report.dry_run,
        "kb": report.kb,
        "rebuilt": {
            "facts": report.facts,
            "fact_conflict_queue": report.conflicts,
            "fact_decisions": report.decisions,
            "derivations": report.derivations,
            "fact_extraction_log": report.extraction_markers,
        },
        "events_replayed": report.events_replayed,
        "events_skipped_deleted_kb": report.events_skipped_deleted_kb,
        "conflicts_skipped_orphan": report.conflicts_skipped_orphan,
        "embeddings_restored": report.embeddings_restored,
        "skipped": skipped,
    });

    print_success(ctx.format, data, meta, |data| {
        let verb = if data["dry_run"].as_bool().unwrap_or(false) {
            "would rebuild"
        } else {
            "rebuilt"
        };
        let scope = data["kb"].as_str().unwrap_or("every KB");
        println!(
            "{verb} {scope}: {} facts, {} queued conflicts, {} decisions, {} derivations \
             ({} events replayed)",
            data["rebuilt"]["facts"],
            data["rebuilt"]["fact_conflict_queue"],
            data["rebuilt"]["fact_decisions"],
            data["rebuilt"]["derivations"],
            data["events_replayed"],
        );
        let orphans = data["conflicts_skipped_orphan"].as_u64().unwrap_or(0);
        if orphans > 0 {
            println!(
                "  {orphans} queued conflict(s) skipped: their facts no longer exist, so the \
                 rows could not be reinserted"
            );
        }
        // Printed on EVERY run. A rebuild that implies it restored things it
        // did not is the class of dishonest health check 0.3.1 removed.
        println!("  not rebuilt (regenerate these another way):");
        for s in data["skipped"].as_array().into_iter().flatten() {
            println!(
                "    {} — {}",
                s["table"].as_str().unwrap_or(""),
                s["reason"].as_str().unwrap_or("")
            );
        }
    });
    Ok(())
}
