//! `engram documents` — inspect and manage ingested source documents.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Listing, Metadata, Scope};
use crate::read_surface::{self, CursorKey};
use engram_core::handle::HandleKind;
use serde_json::{json, Value};
use uuid::Uuid;

pub fn list(
    ctx: &AppContext,
    kb: String,
    all_kbs: bool,
    limit: usize,
    cursor: Option<String>,
) -> Result<(), CliError> {
    let kb_filter = if all_kbs { None } else { Some(kb.as_str()) };
    let key = CursorKey::new("documents.list", &[("kb", kb_filter)]);
    let offset = match &cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    // Over-fetch and skip, as in `entities list`: storage has no OFFSET on
    // this query yet and a cursor that silently restarted at zero would be
    // worse than none.
    let docs: Vec<_> = ctx
        .store
        .list_documents(kb_filter, limit.saturating_add(offset))?
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();
    let total = ctx.store.count_documents(kb_filter)?;
    let rows: Vec<Value> = docs
        .iter()
        .map(|d| {
            let mut v = serde_json::to_value(d).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut v {
                map.insert(
                    "handle".to_string(),
                    json!(read_surface::handle(HandleKind::Document, d.id)),
                );
            }
            v
        })
        .collect();
    let mut meta = Metadata::default();
    meta.add("documents", rows.len());
    meta.add("kb", if all_kbs { "*" } else { kb.as_str() });
    Scope::undiaried(
        vec![if all_kbs { "*".into() } else { kb.clone() }],
        if all_kbs { "all_kbs" } else { "flag" },
    )
    .attach(&mut meta);
    let Value::Object(mut payload) = json!({
        "kb": if all_kbs { "*" } else { kb.as_str() },
        "documents": rows,
    }) else {
        unreachable!("json! object literal");
    };
    Listing::page(
        docs.len(),
        total,
        read_surface::next_cursor(&key, offset, docs.len(), total),
    )
    .attach(&mut payload);
    print_success(ctx.format, Value::Object(payload), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

pub fn show(ctx: &AppContext, id: String) -> Result<(), CliError> {
    let id = parse_uuid(&id)?;
    let Some(document) = ctx.store.get_document(id)? else {
        return Err(CliError::BadInput(format!("document not found: {id}")));
    };
    let mut meta = Metadata::default();
    meta.add("document_id", id.to_string());
    meta.add("kb", document.kb.clone());
    // Which memories this document produced — the provenance chain read from
    // the other end, capped so a 4,000-chunk PDF does not print a novel.
    const MEMORY_PREVIEW: usize = 20;
    let memory_ids = ctx.store.memory_ids_for_document(id)?;
    let shown: Vec<String> = memory_ids
        .iter()
        .take(MEMORY_PREVIEW)
        .map(|m| m.to_string())
        .collect();
    print_success(
        ctx.format,
        json!({
            "document": document,
            "handle": read_surface::handle(HandleKind::Document, id),
            "provenance": {
                "memory_ids": shown,
                "memory_count": memory_ids.len(),
                "truncated": memory_ids.len() > MEMORY_PREVIEW,
            },
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn delete(ctx: &AppContext, id: String, confirm: bool) -> Result<(), CliError> {
    if !confirm {
        return Err(CliError::BadInput(
            "document delete requires --confirm because it removes recallable source chunks".into(),
        ));
    }
    let id = parse_uuid(&id)?;
    let deleted = ctx.store.delete_document(id)?;
    let mut meta = Metadata::default();
    meta.add("document_id", id.to_string());
    meta.add("deleted", deleted);
    print_success(
        ctx.format,
        json!({ "document_id": id.to_string(), "deleted": deleted }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn parse_uuid(s: &str) -> Result<Uuid, CliError> {
    Uuid::parse_str(s).map_err(|_| CliError::BadInput(format!("invalid UUID: {s}")))
}
