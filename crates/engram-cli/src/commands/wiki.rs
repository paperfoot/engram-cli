//! `engram wiki` — inspect generated KB wiki pages.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, kb: String, path: Option<String>) -> Result<(), CliError> {
    if let Some(path) = path {
        let page = ctx.store.get_wiki_page(&kb, &path)?;
        print_success(
            ctx.format,
            json!({ "kb": kb, "page": page }),
            Metadata::default(),
            |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
        );
    } else {
        let pages = ctx.store.list_wiki_pages(&kb)?;
        let mut meta = Metadata::default();
        meta.add("pages", pages.len());
        print_success(
            ctx.format,
            json!({ "kb": kb, "pages": pages }),
            meta,
            |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
        );
    }
    Ok(())
}
