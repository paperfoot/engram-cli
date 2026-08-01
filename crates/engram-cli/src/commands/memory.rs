//! `engram memory list` — simple durable-memory UX helpers, and the one place
//! a mutator turns an id argument into the memory it will write.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_core::handle::{Handle, HandleKind, HandleParseError};
use serde_json::json;
use uuid::Uuid;

/// Parse a mutator's id argument into the memory uuid it names.
///
/// This is the PARSE half of the handle resolution `edit`, `forget`,
/// `memory update` and `memory delete` all share. v0.3 called
/// `Uuid::parse_str` at each of those four call sites, so `mem:<uuid>` — the
/// form every new surface prints — was rejected as a malformed uuid, and a
/// `chunk:<uuid>` copied out of a recall result died with "invalid UUID"
/// instead of a sentence naming what the caller actually held. Bare uuids are
/// unchanged: they are still the legacy input and still resolve exactly as
/// before.
///
/// The STORE half is not here yet. Following a `chunk:` handle to its
/// `memory_id`, and the two-step probe that tells `not_found` from
/// `wrong_id_kind` for a prefixed id, both need `SqliteStore::resolve_handle`,
/// which lands with the journal. Until then a non-memory handle is refused
/// with the wrong-kind message rather than guessed at — the error names the
/// kind the caller gave and the kind this command needs, which is the whole
/// point of typing the id in the first place.
pub(crate) fn parse_memory_id(raw: &str, command: &'static str) -> Result<Uuid, CliError> {
    let handle = Handle::parse(raw).map_err(|e| match &e {
        // The v0.3 wording ("invalid UUID <id>: …") is what scripts and the
        // existing bad-uuid test look for, so the prefix survives; the detail
        // now comes from the handle parser, which can also say WHICH half of
        // `kind:uuid` was unreadable.
        HandleParseError::BadUuid(_) => CliError::BadInput(format!("invalid UUID {raw}: {e}")),
        HandleParseError::UnknownKind(p) => CliError::BadInput(format!(
            "unknown id kind `{p}`: expected one of mem, chunk, fact, conflict, entity, doc, \
             job, event"
        )),
    })?;
    match handle.kind {
        None | Some(HandleKind::Memory) => Ok(handle.id),
        Some(actual) => Err(wrong_id_kind(raw, actual, command)),
    }
}

/// The wrong-kind refusal: what you gave, what this command needs, and the
/// handle that would work.
///
/// Shaped to match the typed `CliError::WrongIdKind` that replaces it once
/// contract-codegen lands, so the message an agent reads does not move under
/// it twice. The one field we cannot fill yet is the CORRECT handle's uuid —
/// naming the owning memory of a chunk or a fact requires the store probe — so
/// this says what shape the right handle has instead of inventing one.
fn wrong_id_kind(raw: &str, actual: HandleKind, command: &'static str) -> CliError {
    let tail = if actual == HandleKind::Chunk {
        "; following a chunk handle to its own memory arrives with the handle resolver"
    } else {
        ""
    };
    CliError::BadInput(format!(
        "wrong id kind: {raw} is a {actual}, this command needs a mem — `engram {command}` \
         writes a memory, so pass that memory's handle `mem:<uuid>`{tail}"
    ))
}

pub fn list(ctx: &AppContext, diary: String, limit: usize) -> Result<(), CliError> {
    let memories = ctx.store.list_memories(Some(&diary), limit)?;
    let mut meta = Metadata::default();
    meta.add("diary", diary.clone());
    meta.add("count", memories.len());
    print_success(
        ctx.format,
        json!({
            "diary": diary,
            "memories": memories,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_uuid_and_mem_handle_name_the_same_memory() {
        let id = Uuid::new_v4();
        assert_eq!(parse_memory_id(&id.to_string(), "edit").unwrap(), id);
        assert_eq!(parse_memory_id(&format!("mem:{id}"), "edit").unwrap(), id);
        // `memory:` is the long alias `Handle` already accepts.
        assert_eq!(
            parse_memory_id(&format!("memory:{id}"), "edit").unwrap(),
            id
        );
    }

    #[test]
    fn a_wrong_kind_names_both_halves() {
        let id = Uuid::new_v4();
        let err = parse_memory_id(&format!("fact:{id}"), "edit").unwrap_err();
        assert_eq!(err.exit_code(), 3);
        let msg = err.to_string();
        assert!(msg.contains("wrong id kind"), "{msg}");
        assert!(msg.contains(&format!("fact:{id}")), "{msg}");
        assert!(msg.contains("mem:<uuid>"), "{msg}");
    }

    #[test]
    fn a_chunk_handle_says_what_is_coming() {
        let id = Uuid::new_v4();
        let msg = parse_memory_id(&format!("chunk:{id}"), "edit")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("is a chunk"), "{msg}");
        assert!(msg.contains("handle resolver"), "{msg}");
    }

    #[test]
    fn a_malformed_id_keeps_the_legacy_wording() {
        let msg = parse_memory_id("not-a-uuid", "forget")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("invalid UUID"), "{msg}");
        let msg = parse_memory_id("mem:not-a-uuid", "forget")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("invalid UUID"), "{msg}");
    }

    #[test]
    fn an_unknown_prefix_is_not_a_bad_uuid() {
        let id = Uuid::new_v4();
        let msg = parse_memory_id(&format!("blob:{id}"), "edit")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("unknown id kind `blob`"), "{msg}");
        assert!(!msg.contains("invalid UUID"), "{msg}");
    }
}
