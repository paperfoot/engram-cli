//! `engram wiki` — inspect generated KB wiki pages.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Listing, Metadata, Scope};
use serde_json::{json, Value};

pub fn run(ctx: &AppContext, kb: String, path: Option<String>) -> Result<(), CliError> {
    let mut meta = Metadata::default();
    Scope::undiaried(vec![kb.clone()], "flag").attach(&mut meta);
    if let Some(path) = path {
        let page = ctx.store.get_wiki_page(&kb, &path)?;
        // A wiki page carries `page_id` and NO `handle`: `HandleKind` has no
        // wiki variant, and a hand-built `wiki:` prefix would be a handle
        // every mutator rejects. A decorative id that fails on first use is
        // worse than an honest absence.
        print_success(
            ctx.format,
            json!({
                "kb": kb,
                "page": page,
                "page_id": page.as_ref().map(|p| p.id.to_string()),
            }),
            meta,
            |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
        );
    } else {
        let pages = ctx.store.list_wiki_pages(&kb)?;
        meta.add("pages", pages.len());
        // The query is unbounded, so everything that matched is in this
        // response and `total == returned` says so, rather than leaving a
        // caller to wonder whether a page was cut off.
        let Value::Object(mut payload) = json!({ "kb": kb, "pages": pages }) else {
            unreachable!("json! object literal");
        };
        Listing::full(pages.len()).attach(&mut payload);
        print_success(ctx.format, Value::Object(payload), meta, |data| {
            println!("{}", serde_json::to_string_pretty(data).unwrap())
        });
    }
    Ok(())
}
