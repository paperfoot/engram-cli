//! `engram forget <id> --confirm` — soft-delete a memory by UUID.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use uuid::Uuid;

pub fn run(ctx: &AppContext, id: String, confirm: bool) -> Result<(), CliError> {
    if !confirm {
        return Err(CliError::BadInput(
            "destructive operation — pass --confirm to actually delete".into(),
        ));
    }
    let uuid =
        Uuid::parse_str(&id).map_err(|e| CliError::BadInput(format!("invalid UUID {id}: {e}")))?;
    // `soft_delete_memory` now reports which fact intervals it closed as well
    // as whether it retracted anything. The published `deleted` boolean keeps
    // its exact old meaning here; write-surfaces owns surfacing the rest.
    let deleted = ctx
        .store
        .soft_delete_memory(uuid)?
        .map(|o| o.retracted)
        .unwrap_or(false);
    print_success(
        ctx.format,
        json!({ "id": id, "deleted": deleted }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
