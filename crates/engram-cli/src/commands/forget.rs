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
    let deleted = ctx.store.soft_delete_memory(uuid)?;
    print_success(
        ctx.format,
        json!({ "id": id, "deleted": deleted }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
