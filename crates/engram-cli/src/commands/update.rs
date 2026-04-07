//! `engram update [--check]` — v0 stub. Self-update wires up in Phase 2.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, _check: bool) -> Result<(), CliError> {
    print_success(
        ctx.format,
        json!({
            "current_version": env!("CARGO_PKG_VERSION"),
            "update_available": false,
            "note": "self-update lands once we publish to crates.io / GitHub releases"
        }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
