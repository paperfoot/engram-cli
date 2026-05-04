//! `engram entities list|show` — browse persisted KB entities.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn list(ctx: &AppContext, limit: usize, min_mentions: u32, kb: String) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let entities = ctx.store.list_entities(kb_filter, limit, min_mentions)?;
    let mut meta = Metadata::default();
    meta.add("entities_returned", entities.len());
    meta.add("kb", kb.clone());
    print_success(
        ctx.format,
        json!({ "kb": kb, "entities": entities }),
        meta,
        |data| {
            if let Some(arr) = data.get("entities").and_then(|v| v.as_array()) {
                for e in arr {
                    let name = e
                        .get("canonical_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let n = e.get("mention_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{n:6}  {kind:10}  {name}");
                }
            }
        },
    );
    Ok(())
}

pub fn show(ctx: &AppContext, name: String, kb: String) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let entity = ctx.store.find_entity(kb_filter, &name)?;
    let neighbors = if let Some(e) = &entity {
        ctx.store.graph_neighbors(&e.kb, &e.canonical_name, 1)?
    } else {
        Vec::new()
    };

    let mut meta = Metadata::default();
    meta.add("kb", kb.clone());
    meta.add("neighbors", neighbors.len());
    print_success(
        ctx.format,
        json!({
            "kb": kb,
            "name": name,
            "entity": entity,
            "neighbors": neighbors,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
