//! `engram forget <id> --confirm` — soft-delete a memory by UUID.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn run(ctx: &AppContext, id: String, confirm: bool) -> Result<(), CliError> {
    if !confirm {
        return Err(CliError::BadInput(
            "destructive operation — pass --confirm to actually delete".into(),
        ));
    }
    // Same resolution as `edit`, from the same helper: `forget mem:<uuid>` and
    // `memory delete mem:<uuid>` are what an agent copies out of a write
    // response, and rejecting them as malformed uuids was the sharpest edge of
    // the untyped id.
    let uuid = crate::commands::memory::parse_memory_id(&id, "forget")?;
    let deleted = ctx.store.soft_delete_memory(uuid)?;
    print_success(
        ctx.format,
        // The canonical uuid, never the caller's raw spelling: `data.id` has
        // always been a bare uuid and scripts feed it straight back in.
        json!({ "id": uuid.to_string(), "deleted": deleted }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
