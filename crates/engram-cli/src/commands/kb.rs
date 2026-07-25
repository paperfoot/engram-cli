//! `engram kb` — first-class knowledge base/domain management.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn create(ctx: &AppContext, name: String, description: Option<String>) -> Result<(), CliError> {
    validate_kb_name(&name)?;
    ctx.store.ensure_kb(&name, description.as_deref())?;
    print_success(
        ctx.format,
        json!({ "name": name, "description": description, "created": true }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn list(ctx: &AppContext) -> Result<(), CliError> {
    let kbs = ctx.store.list_kbs()?;
    let mut meta = Metadata::default();
    meta.add("count", kbs.len());
    print_success(
        ctx.format,
        json!({ "knowledge_bases": kbs }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn show(ctx: &AppContext, name: String) -> Result<(), CliError> {
    let Some(kb) = ctx.store.get_kb(&name)? else {
        return Err(CliError::BadInput(format!("KB not found: {name}")));
    };
    print_success(
        ctx.format,
        json!({ "knowledge_base": kb }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn delete(ctx: &AppContext, name: String, confirm: bool) -> Result<(), CliError> {
    if !confirm {
        return Err(CliError::BadInput(
            "delete requires --confirm because it removes KB documents and derived artifacts"
                .into(),
        ));
    }
    let deleted = ctx.store.delete_kb(&name)?;
    print_success(
        ctx.format,
        json!({ "name": name, "deleted": deleted }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}

/// Heal a store whose KB registry fell out of sync with its content.
///
/// Writing through `remember --kb <name>` registers the KB today, but stores
/// created by earlier builds carry memories in KBs that `kb list` never shows
/// — so those memories are invisible to anyone enumerating KBs. This registers
/// them. Additive only: no memory, chunk or fact is touched.
pub fn repair(ctx: &AppContext, dry_run: bool) -> Result<(), CliError> {
    let missing = if dry_run {
        ctx.store.unregistered_kbs()?
    } else {
        ctx.store.repair_kb_registry()?
    };

    let mut meta = Metadata::default();
    meta.add("unregistered", missing.len());
    meta.add("dry_run", dry_run);
    let registered = if dry_run { 0 } else { missing.len() };
    meta.add("registered", registered);

    print_success(
        ctx.format,
        json!({
            "unregistered_kbs": missing,
            "registered": registered,
            "dry_run": dry_run,
            "note": if missing.is_empty() {
                "KB registry already matches stored content; nothing to repair."
            } else if dry_run {
                "Re-run without --dry-run to register these KBs."
            } else {
                "Registered. These KBs now appear in `engram kb list`."
            },
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn validate_kb_name(name: &str) -> Result<(), CliError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(CliError::BadInput(
            "KB name must use lowercase letters, digits, hyphen, or underscore".into(),
        ));
    }
    Ok(())
}
