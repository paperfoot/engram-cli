//! `engram export` — dump all memories as JSON to stdout.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, format: String, kb: Option<String>) -> Result<(), CliError> {
    if format != "json" {
        return Err(CliError::BadInput(format!(
            "unknown format: {format} (only json supported)"
        )));
    }
    let memories = ctx
        .store
        .list_memories_scoped(None, kb.as_deref(), 1_000_000)?;
    let payload = json!({
        "version": 1,
        "kb": kb,
        "count": memories.len(),
        "memories": memories,
    });
    let mut meta = Metadata::default();
    meta.add("count", memories.len());
    print_success(ctx.format, payload, meta, |data| {
        println!("{}", serde_json::to_string(data).unwrap())
    });
    Ok(())
}
