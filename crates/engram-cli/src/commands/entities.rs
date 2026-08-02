//! `engram entities list|show` — browse persisted KB entities.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Listing, Metadata, Scope};
use crate::read_surface::{self, CursorKey};
use engram_core::handle::HandleKind;
use serde_json::{json, Value};

pub fn list(
    ctx: &AppContext,
    limit: usize,
    min_mentions: u32,
    kb: String,
    cursor: Option<String>,
) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let key = CursorKey::new(
        "entities.list",
        &[
            ("kb", kb_filter),
            ("min_mentions", Some(&min_mentions.to_string())),
        ],
    );
    let offset = match &cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    // `list_entities` has no OFFSET yet (storage request SR-9's remainder), so
    // the page is taken from an over-fetch. Honest and O(offset+limit); when
    // storage grows the parameter this becomes a one-line change and the wire
    // shape does not move.
    let entities: Vec<_> = ctx
        .store
        .list_entities(kb_filter, limit.saturating_add(offset), min_mentions)?
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();
    let total = ctx.store.count_entities(kb_filter, min_mentions)?;
    let rows: Vec<Value> = entities
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut v {
                map.insert(
                    "handle".to_string(),
                    json!(read_surface::handle(HandleKind::Entity, e.id)),
                );
            }
            v
        })
        .collect();
    let mut meta = Metadata::default();
    meta.add("entities_returned", rows.len());
    meta.add("kb", kb.clone());
    // Entities carry no diary column, so the scope says `diary_applicable:
    // false` rather than claiming a diary filter ran.
    Scope::undiaried(
        vec![kb.clone()],
        if kb_filter.is_some() {
            "flag"
        } else {
            "all_kbs"
        },
    )
    .attach(&mut meta);
    let Value::Object(mut payload) = json!({ "kb": kb, "entities": rows }) else {
        unreachable!("json! object literal");
    };
    Listing::page(
        entities.len(),
        total,
        read_surface::next_cursor(&key, offset, entities.len(), total),
    )
    .attach(&mut payload);
    print_success(ctx.format, Value::Object(payload), meta, |data| {
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
    });
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
    let entity_json = entity.as_ref().map(|e| {
        let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut v {
            map.insert(
                "handle".to_string(),
                json!(read_surface::handle(HandleKind::Entity, e.id)),
            );
        }
        v
    });
    print_success(
        ctx.format,
        json!({
            "kb": kb,
            "name": name,
            "entity": entity_json,
            "neighbors": neighbors,
            // The neighbour list is whatever the walk found: unpaged, so
            // saying `total == returned` is the honest answer rather than a
            // number a caller has to guess the meaning of.
            "returned": neighbors.len(),
            "total": neighbors.len() as i64,
            "count": neighbors.len(),
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
