//! `engram memory list` — the durable-memory listing, and the one place a
//! mutator turns an id argument into the memory it will write.
//!
//! `resolve_memory_target` is the whole of W1: every mutator (`edit`,
//! `forget`, `memory update`, `memory delete`) reads its id through it, so a
//! `chunk:` handle copied out of a recall result edits the memory that chunk
//! belongs to instead of dying with "no memory found … (already deleted?)" —
//! the sentence that sent agents hunting for a race that never happened.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_core::handle::{Handle, HandleKind, HandleParseError};
use engram_storage::Resolved;
use serde_json::json;
use uuid::Uuid;

/// A mutator's resolved target: the memory it will write, and how we got there.
#[derive(Debug, Clone)]
pub(crate) struct MemoryTarget {
    /// The memory id every write below uses.
    pub memory_id: Uuid,
    /// `mem:<uuid>` — echoed in every response, so a copied id keeps its kind.
    pub handle: String,
    /// Set when the caller passed something that had to be followed to get
    /// here (today: a `chunk:` handle, or a bare uuid that turned out to be a
    /// chunk). Rendered as `resolved_from`, so the agent learns the mapping
    /// rather than re-deriving it on the next call.
    pub resolved_from: Option<String>,
    pub kb: String,
    pub diary: String,
    /// The row exists but `deleted = 1`. `forget` answers "already retracted"
    /// (exit 0) with it; `edit` turns it into `NotFound` (exit 3).
    pub retracted: bool,
}

impl MemoryTarget {
    /// `cli:<diary>` — the actor string every event this mutator causes is
    /// stamped with (interfaces §2 actor grammar).
    pub fn actor(&self) -> String {
        format!("cli:{}", self.diary)
    }
}

/// `mem:<uuid>`, built in exactly one place so no surface hand-rolls the
/// prefix and no two surfaces disagree about it.
pub(crate) fn render_handle(id: Uuid) -> String {
    Handle::render(HandleKind::Memory, id)
}

/// Parse a mutator's id argument into the handle it names.
///
/// Bare uuids are unchanged: they are still the legacy input and still resolve
/// across every kind. A prefixed id keeps its kind all the way to the store.
fn parse_handle(raw: &str) -> Result<Handle, CliError> {
    Handle::parse(raw).map_err(|e| match &e {
        // The v0.3 wording ("invalid UUID <id>: …") is what scripts and the
        // existing bad-uuid test look for, so the prefix survives; the detail
        // now comes from the handle parser, which can also say WHICH half of
        // `kind:uuid` was unreadable.
        HandleParseError::BadUuid(_) => CliError::BadInput(format!("invalid UUID {raw}: {e}")),
        HandleParseError::UnknownKind(p) => CliError::BadInput(format!(
            "unknown id kind `{p}`: expected one of mem, chunk, fact, conflict, entity, doc, \
             job, event"
        )),
    })
}

/// Resolve ANY handle kind to the memory a mutator should write.
///
/// `mem:` → itself. `chunk:` → its `memory_id`. Every other kind →
/// `wrong_id_kind` naming the kind we actually found and, where the store can
/// supply one, the handle that WOULD work. An unknown uuid → `not_found`. Both
/// exit 3.
///
/// The two-step probe is load-bearing rather than defensive. Storage's
/// `resolve_handle` probes only the named kind's table, so `mem:<chunk-uuid>`
/// misses on the first call and is indistinguishable from a typo; asking a
/// second time with the kind stripped is what turns "not found" into "that is
/// a chunk, here is its memory". Reporting `not_found` there would reintroduce
/// exactly the guess W1 exists to kill.
pub(crate) fn resolve_memory_target(
    ctx: &AppContext,
    raw: &str,
    command: &'static str,
) -> Result<MemoryTarget, CliError> {
    let handle = parse_handle(raw)?;
    let prefixed = handle.kind.is_some();
    let resolved = ctx.store.resolve_handle(&handle)?;

    let (memory_id, resolved_from) = match resolved {
        Some(Resolved {
            kind: HandleKind::Memory,
            id,
            ..
        }) => (id, None),
        Some(Resolved {
            kind: HandleKind::Chunk,
            id,
            memory_id: Some(m),
        }) => (m, Some(Handle::render(HandleKind::Chunk, id))),
        Some(Resolved {
            kind: HandleKind::Chunk,
            id,
            memory_id: None,
        }) => {
            // A chunk with no memory is store corruption, not a caller
            // mistake: exit 1, and point at the command that can prove it.
            return Err(CliError::Transient(format!(
                "chunk {id} has no memory — the store is inconsistent; run \
                 `engram doctor --integrity --json`"
            )));
        }
        // A fact id names a claim, not the memory a mutator should rewrite.
        // Following it silently would let `edit fact:<id>` rewrite content the
        // caller never named, so it is refused — with the memory's own handle
        // in `correct`, so the retry needs no round trip.
        Some(Resolved {
            kind, memory_id, ..
        }) => {
            return Err(wrong_id_kind(raw, kind, memory_id));
        }
        None if prefixed => {
            // Second probe, prefixed ids only: the uuid may live under another
            // kind entirely.
            match ctx.store.resolve_handle(&Handle {
                kind: None,
                id: handle.id,
            })? {
                Some(Resolved {
                    kind, memory_id, ..
                }) => return Err(wrong_id_kind(raw, kind, memory_id)),
                None => return Err(not_found(command, raw)),
            }
        }
        // A bare uuid has already been probed across every table.
        None => return Err(not_found(command, raw)),
    };

    // `memory_scope` filters `deleted = 0`, which is why every v0.3 surface
    // could only guess "(already deleted?)". This one answers.
    let Some((kb, diary, retracted)) = ctx.store.memory_scope_including_deleted(memory_id)? else {
        return Err(not_found(command, raw));
    };
    Ok(MemoryTarget {
        memory_id,
        handle: render_handle(memory_id),
        resolved_from,
        kb,
        diary,
        retracted,
    })
}

fn not_found(command: &'static str, raw: &str) -> CliError {
    CliError::NotFound {
        what: command,
        handle: raw.to_string(),
    }
}

/// The wrong-kind refusal: what you gave, what this command needs, and — when
/// the store can name it — the handle that would have worked. The suggestion
/// prose is `respond::retry_with_handle`'s job, built from `correct`; this
/// function supplies the facts and never the sentence.
fn wrong_id_kind(raw: &str, actual: HandleKind, memory_id: Option<Uuid>) -> CliError {
    CliError::WrongIdKind {
        handle: raw.to_string(),
        actual: actual.as_str().to_string(),
        expected: HandleKind::Memory.as_str().to_string(),
        correct: memory_id.map(render_handle),
    }
}

pub fn list(ctx: &AppContext, diary: String, limit: usize) -> Result<(), CliError> {
    let memories = ctx.store.list_memories(Some(&diary), limit)?;
    let mut meta = Metadata::default();
    meta.add("diary", diary.clone());
    meta.add("count", memories.len());
    // `handle` is inserted per row rather than added to `engram_core::Memory`:
    // the type belongs to another lane, and a serialized field there would
    // travel into exports and events that have no use for it.
    let rows: Vec<serde_json::Value> = memories
        .iter()
        .map(|m| {
            let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("handle".to_string(), json!(render_handle(m.id)));
            }
            v
        })
        .collect();
    print_success(
        ctx.format,
        json!({
            "diary": diary,
            "memories": rows,
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
    fn a_memory_handle_is_rendered_in_one_place() {
        let id = Uuid::nil();
        assert_eq!(render_handle(id), format!("mem:{id}"));
    }

    #[test]
    fn bare_uuid_and_mem_handle_parse_to_the_same_id() {
        let id = Uuid::new_v4();
        assert_eq!(parse_handle(&id.to_string()).unwrap().id, id);
        assert_eq!(parse_handle(&format!("mem:{id}")).unwrap().id, id);
        // `memory:` is the long alias `Handle` already accepts.
        assert_eq!(parse_handle(&format!("memory:{id}")).unwrap().id, id);
    }

    #[test]
    fn a_malformed_id_keeps_the_legacy_wording() {
        let msg = parse_handle("not-a-uuid").unwrap_err().to_string();
        assert!(msg.contains("invalid UUID"), "{msg}");
        let msg = parse_handle("mem:not-a-uuid").unwrap_err().to_string();
        assert!(msg.contains("invalid UUID"), "{msg}");
    }

    #[test]
    fn an_unknown_prefix_is_not_a_bad_uuid() {
        let id = Uuid::new_v4();
        let msg = parse_handle(&format!("blob:{id}")).unwrap_err().to_string();
        assert!(msg.contains("unknown id kind `blob`"), "{msg}");
        assert!(!msg.contains("invalid UUID"), "{msg}");
    }

    #[test]
    fn a_wrong_kind_carries_the_correct_handle_when_there_is_one() {
        let fact = Uuid::new_v4();
        let memory = Uuid::new_v4();
        let err = wrong_id_kind(&format!("fact:{fact}"), HandleKind::Fact, Some(memory));
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.code(), "wrong_id_kind");
        // The suggestion is built from `correct`, so the agent's next command
        // is in the error rather than in a follow-up round trip.
        let hint = err.suggestion().expect("a suggestion");
        assert!(hint.contains(&format!("mem:{memory}")), "{hint}");
    }

    #[test]
    fn a_wrong_kind_with_no_owning_memory_suggests_no_handle() {
        let id = Uuid::new_v4();
        let err = wrong_id_kind(&format!("entity:{id}"), HandleKind::Entity, None);
        match &err {
            CliError::WrongIdKind { correct, .. } => assert!(correct.is_none()),
            other => panic!("expected wrong_id_kind, got {other}"),
        }
    }
}
