//! `engram documents` — inspect and manage ingested source documents.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use uuid::Uuid;

pub fn list(ctx: &AppContext, kb: String, all_kbs: bool, limit: usize) -> Result<(), CliError> {
    let docs = ctx
        .store
        .list_documents(if all_kbs { None } else { Some(kb.as_str()) }, limit)?;
    let mut meta = Metadata::default();
    meta.add("documents", docs.len());
    meta.add("kb", if all_kbs { "*" } else { kb.as_str() });
    print_success(
        ctx.format,
        json!({
            "kb": if all_kbs { "*" } else { kb.as_str() },
            "documents": docs,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
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
    print_success(ctx.format, json!({ "document": document }), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
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
