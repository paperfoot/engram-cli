//! `engram memory list` — simple durable-memory UX helpers.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn list(ctx: &AppContext, diary: String, limit: usize) -> Result<(), CliError> {
    let memories = ctx.store.list_memories(Some(&diary), limit)?;
    let mut meta = Metadata::default();
    meta.add("diary", diary.clone());
    meta.add("count", memories.len());
    print_success(
        ctx.format,
        json!({
            "diary": diary,
            "memories": memories,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
