//! `engram repair` — clean store lifecycle residue left by older commands.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, dry_run: bool, confirm: bool) -> Result<(), CliError> {
    if !dry_run && !confirm {
        return Err(CliError::BadInput(
            "repair mutates the store; pass --dry-run to inspect or --confirm to apply".into(),
        ));
    }
    let report = ctx.store.repair_integrity(dry_run)?;
    let mut meta = Metadata::default();
    meta.add("dry_run", dry_run);
    meta.add(
        "facts_from_deleted_memories_removed",
        report.facts_from_deleted_memories_removed,
    );
    meta.add("derived_kbs_cleared", report.derived_kbs_cleared.len());
    meta.add(
        "duplicate_source_documents",
        report.duplicate_source_documents.len(),
    );
    print_success(ctx.format, json!({ "repair": report }), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}
