//! `engram export` — dump the source-of-truth store as JSON to stdout.
//!
//! Format version 3 is a SUPERSET of v2: the journal slice (every event, in
//! order) plus the v2 sections it does not carry — KBs, documents, memories,
//! chunks with their base64 embeddings, and facts. The journal is what makes a
//! restored store rebuildable and auditable; the chunk vectors ride along
//! because replay cannot re-embed without paying a provider, and a backup that
//! silently costs money to restore is not a backup.
//!
//! A v2 consumer still finds `memories`/`chunks`/`facts` exactly where it left
//! them; only the `version` discriminator moved. Derived tables — claims,
//! entities, relations, wiki pages, source spans — remain compiler caches; run
//! `engram compile --all` after import to regenerate them.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};

pub fn run(ctx: &AppContext, format: String, kb: Option<String>) -> Result<(), CliError> {
    if format != "json" {
        return Err(CliError::BadInput(format!(
            "unknown format: {format} (only json supported)"
        )));
    }
    // ONE call. The five projection reads this command used to make live inside
    // `export_v3` now, so the slice and the sections cannot disagree about
    // scope: a per-KB export deliberately also carries the un-scoped events
    // whose subject lives in that KB, and a hand-rolled projection read has no
    // equivalent rule.
    let doc = ctx.store.export_v3(kb.as_deref())?;
    let payload = serde_json::to_value(&doc)?;

    let mut meta = Metadata::default();
    meta.add("version", doc.version);
    meta.add("schema_version", doc.schema_version);
    meta.add("events", doc.counts.events);
    meta.add("memories", doc.counts.memories);
    meta.add("chunks", doc.counts.chunks);
    meta.add("documents", doc.counts.documents);
    meta.add("facts", doc.counts.facts);
    print_success(ctx.format, payload, meta, |data| {
        println!("{}", serde_json::to_string(data).unwrap())
    });
    Ok(())
}
