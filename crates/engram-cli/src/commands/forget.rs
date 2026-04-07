//! `engram forget <id>` — v0 stub.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, id: String) -> Result<(), CliError> {
    print_success(
        ctx.format,
        json!({ "id": id, "deleted": false, "note": "forget will be wired in Phase 2" }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
