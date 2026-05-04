//! `engram graph` — inspect SQLite-backed entity relations.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn neighbors(
    ctx: &AppContext,
    name: String,
    kb: String,
    hops: u8,
    min_weight: f32,
) -> Result<(), CliError> {
    let hops = hops.max(1).min(2);
    let relations = ctx
        .store
        .graph_neighbors(&kb, &name, hops)?
        .into_iter()
        .filter(|r| r.weight >= min_weight)
        .collect::<Vec<_>>();
    let mut meta = Metadata::default();
    meta.add("kb", kb.clone());
    meta.add("relations", relations.len());
    meta.add("hops", hops);
    meta.add("min_weight", min_weight);
    print_success(
        ctx.format,
        json!({
            "kb": kb,
            "entity": name,
            "hops": hops,
            "min_weight": min_weight,
            "neighbors": relations,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
