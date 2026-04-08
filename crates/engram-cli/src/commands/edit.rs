//! `engram edit <id>` — update a memory's content or importance.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use uuid::Uuid;

pub fn run(
    ctx: &AppContext,
    id: String,
    content: Option<String>,
    importance: Option<u8>,
) -> Result<(), CliError> {
    if content.is_none() && importance.is_none() {
        return Err(CliError::BadInput(
            "provide --content and/or --importance to update".into(),
        ));
    }
    if let Some(i) = importance {
        if i > 10 {
            return Err(CliError::BadInput("importance must be 0..=10".into()));
        }
    }
    let uuid = Uuid::parse_str(&id)
        .map_err(|e| CliError::BadInput(format!("invalid UUID {id}: {e}")))?;
    let updated = ctx.store.update_memory(uuid, content.as_deref(), importance)?;
    print_success(
        ctx.format,
        json!({ "id": id, "updated": updated }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
