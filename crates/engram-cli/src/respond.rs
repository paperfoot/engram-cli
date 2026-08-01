//! The one place a "here is how you fix this" command is built.
//!
//! Four surfaces used to format the same string four ways, and every one of
//! them shipped `--action supersede-old|keep-both|reject-new` — a string with a
//! shell PIPE in it, so the "ready-to-run command" agent-info promises was
//! never runnable. It was worse than unrunnable: pasted into a shell it runs
//! `engram facts resolve <id> --action supersede-old` and pipes the result
//! through two nonexistent programs, so the first alternative in the list —
//! the destructive one — is the one that executes. That happened: a paste
//! retired a live medication record. One builder, one runnable command, and
//! the alternatives move to `allowed_actions` where a machine can read them.
//!
//! Ruled surface: coherence-report-v2 D1. `resolve_payload` and `resolve_block`
//! are not names here; four lanes call the functions below and nothing else.

use engram_core::types::Fact;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::error::CliError;
use crate::output::{Listing, Metadata};

/// The program every generated command starts with. Never a path: an agent
/// that has `engram` on PATH can run it, and one that does not has a bigger
/// problem than this string.
pub const PROGRAM: &str = "engram";

/// What a caller may do with an open conflict. `requires_context` is a
/// SUGGESTION, never an allowed action — you cannot pass it to `--action`,
/// which is why it is absent from this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveAction {
    KeepBoth,
    SupersedeOld,
    RejectNew,
}

impl ResolveAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolveAction::KeepBoth => "keep-both",
            ResolveAction::SupersedeOld => "supersede-old",
            ResolveAction::RejectNew => "reject-new",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "keep-both" => Some(ResolveAction::KeepBoth),
            "supersede-old" => Some(ResolveAction::SupersedeOld),
            "reject-new" => Some(ResolveAction::RejectNew),
            _ => None,
        }
    }

    /// Order matters: element 0 is what the generated command runs.
    /// `keep-both` first because it is the only universally SAFE default — it
    /// never closes an interval, so an agent that runs the command blind
    /// cannot destroy a medication record.
    pub const ALL: [ResolveAction; 3] = [
        ResolveAction::KeepBoth,
        ResolveAction::SupersedeOld,
        ResolveAction::RejectNew,
    ];
}

/// A ready-to-run command plus the full set of actions the caller may take.
/// `args` already carries the `--action` the string will run, so the object
/// and the string cannot disagree about what happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvePayload {
    pub program: &'static str,
    pub args: Vec<String>,
    pub allowed_actions: Vec<&'static str>,
}

/// The conflict-adjudication payload. `allowed_actions` comes FROM THE CALLER
/// (fact-semantics' `allowed_actions(v, new, old)`), so this module depends on
/// no classifier — R12, verbatim.
///
/// The list is normalized two ways before it is published, and both are safety
/// properties rather than tidiness: unknown strings are dropped (an agent must
/// never be told it may pass a value `--action` will reject), and the survivors
/// are ordered by `ResolveAction::ALL`, so the action the blind command runs is
/// the most conservative one the caller permitted — never whichever the caller
/// happened to list first.
pub fn resolve_conflict(conflict_id: Uuid, allowed_actions: &[&'static str]) -> ResolvePayload {
    let allowed = normalize_actions(allowed_actions);
    let mut args = vec![
        "facts".to_string(),
        "resolve".to_string(),
        conflict_id.to_string(),
    ];
    if let Some(first) = allowed.first() {
        args.push("--action".to_string());
        args.push((*first).to_string());
    }
    ResolvePayload {
        program: PROGRAM,
        args,
        allowed_actions: allowed,
    }
}

/// A mutator that was handed the wrong handle kind: the command the caller
/// should run instead. Used by `CliError::WrongIdKind`'s suggestion and by the
/// read surfaces that hand back chunk ids for memories.
pub fn retry_with_handle(
    command_path: &str,
    correct_handle: &str,
    extra: &[&str],
) -> ResolvePayload {
    let mut args: Vec<String> = command_path
        .split_whitespace()
        .map(str::to_string)
        .collect();
    args.push(correct_handle.to_string());
    args.extend(extra.iter().map(|s| (*s).to_string()));
    ResolvePayload {
        program: PROGRAM,
        args,
        allowed_actions: Vec::new(),
    }
}

/// `{ "program": "engram", "args": [...], "allowed_actions": [...] }`
pub fn resolve_json(p: &ResolvePayload) -> Value {
    json!({
        "program": p.program,
        "args": p.args,
        "allowed_actions": p.allowed_actions,
    })
}

/// ONE runnable command, no pipes, shell-quoted where a value could contain a
/// space or anything else a shell would interpret.
pub fn resolve_command_string(p: &ResolvePayload) -> String {
    let mut out = String::from(p.program);
    for a in &p.args {
        out.push(' ');
        out.push_str(&shell_quote(a));
    }
    out
}

/// Inserts BOTH `resolve` (new) and `resolve_command` (legacy string) into an
/// existing object. The single call site producers use, so the two can never
/// disagree.
pub fn attach(obj: &mut Map<String, Value>, p: &ResolvePayload) {
    obj.insert("resolve".to_string(), resolve_json(p));
    obj.insert(
        "resolve_command".to_string(),
        Value::String(resolve_command_string(p)),
    );
}

// --- the envelope half (D1) -------------------------------------------------
//
// One shape for the CLI and the daemon. Before this, `serve.rs` built the same
// four keys by hand at eighteen call sites, which is how the HTTP surface and
// the CLI came to disagree about what an error looks like.

/// A success envelope as a value. `output::print_success` writes the same
/// shape to stdout; the daemon puts this in a response body.
pub fn success(data: Value, meta: Metadata) -> Value {
    envelope("success", data, meta)
}

/// A success envelope with an explicit status from the `status` vocabulary
/// (`success`, `partial_success`, `all_failed`, `no_results`, `error`).
pub fn envelope(status: &str, data: Value, meta: Metadata) -> Value {
    json!({
        "version": "1",
        "status": status,
        "data": data,
        "metadata": meta.into_value(),
    })
}

/// The error envelope, identical in shape to what `CliError::emit_to_stderr`
/// writes — one definition, so an agent parsing HTTP and an agent parsing
/// stderr branch on the same keys.
pub fn error(err: &CliError) -> Value {
    json!({
        "version": "1",
        "status": "error",
        "error": {
            "code": err.code(),
            "message": err.to_string(),
            "suggestion": err.suggestion(),
            "exit_code": err.exit_code(),
        }
    })
}

/// The `data` object of a listing response: the rows under `items`, plus the
/// IN-23 counters. `total` is the true total after scope filtering — the field
/// whose absence made agents report three open conflicts when 953 were queued.
pub fn listing(items: Vec<Value>, l: &Listing) -> Value {
    let mut obj = Map::new();
    obj.insert("items".to_string(), Value::Array(items));
    l.attach(&mut obj);
    Value::Object(obj)
}

/// One row of the `conflicts` array, as every producer must render it.
/// `suggested` is advice (R7's `keep-both | supersede-old | requires_context`);
/// `allowed` is permission (`vocabulary("resolve_action")`). They are separate
/// fields because an agent may only act on the second.
pub struct ConflictInput<'a> {
    pub id: Uuid,
    pub new: &'a Fact,
    pub old: &'a Fact,
    pub detector: &'a str,
    pub suggested: &'static str,
    pub allowed: &'static [&'static str],
    pub status: &'static str,
}

/// Renders conflict rows for `remember`, `facts conflicts`, `recall` and
/// `/v1/recall`. Identical bytes from all four, which is the property
/// `all_four_producers_agree` exists to hold.
pub fn conflicts(rows: &[ConflictInput<'_>]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            let mut obj = Map::new();
            obj.insert("conflict_id".to_string(), json!(r.id.to_string()));
            obj.insert("status".to_string(), json!(r.status));
            obj.insert("subject".to_string(), json!(r.new.subject));
            obj.insert("predicate".to_string(), json!(r.new.predicate));
            obj.insert("values".to_string(), json!([r.old.object, r.new.object]));
            obj.insert("new".to_string(), fact_summary(r.new));
            obj.insert("old".to_string(), fact_summary(r.old));
            obj.insert("detector".to_string(), json!(r.detector));
            obj.insert("suggested_action".to_string(), json!(r.suggested));
            attach(&mut obj, &resolve_conflict(r.id, r.allowed));
            obj.insert(
                "note".to_string(),
                json!(
                    "unresolved contradiction — do not pick a value silently; \
                       resolve it or surface both to the user"
                ),
            );
            Value::Object(obj)
        })
        .collect()
}

fn fact_summary(f: &Fact) -> Value {
    json!({
        "id": f.id.to_string(),
        "subject": f.subject,
        "predicate": f.predicate,
        "object": f.object,
        "confidence": f.confidence,
        "created_at": f.created_at.to_rfc3339(),
    })
}

fn normalize_actions(allowed: &[&'static str]) -> Vec<&'static str> {
    ResolveAction::ALL
        .iter()
        .map(|a| a.as_str())
        .filter(|canonical| allowed.contains(canonical))
        .collect()
}

/// POSIX single-quoting. Anything outside the safe set is wrapped, and an
/// embedded quote is closed, escaped and reopened — the `'\''` idiom. A
/// subject like `O'Brien` is why this exists.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.:/@=,+".contains(c));
    if safe {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[&str] = &["keep-both", "supersede-old", "reject-new"];

    fn payloads() -> Vec<ResolvePayload> {
        let id = Uuid::nil();
        vec![
            resolve_conflict(id, ALL),
            resolve_conflict(id, &["supersede-old", "reject-new"]),
            resolve_conflict(id, &["reject-new"]),
            resolve_conflict(id, &[]),
            retry_with_handle("edit", "mem:11111111-1111-1111-1111-111111111111", &[]),
            retry_with_handle(
                "memory update",
                "mem:11111111-1111-1111-1111-111111111111",
                &["--content", "two words"],
            ),
        ]
    }

    #[test]
    fn no_generated_command_contains_a_shell_operator() {
        // The whole reason this module exists: the old template piped, and a
        // pasted pipe ran the destructive branch.
        for p in payloads() {
            let s = resolve_command_string(&p);
            for bad in ['|', '&', ';', '>', '<', '`', '$'] {
                assert!(!s.contains(bad), "{s} contains {bad}");
            }
        }
    }

    #[test]
    fn resolve_conflict_targets_facts_resolve() {
        let p = resolve_conflict(Uuid::nil(), ALL);
        assert_eq!(&p.args[0..2], &["facts".to_string(), "resolve".to_string()]);
        assert_eq!(p.args[3], "--action");
    }

    #[test]
    fn the_blind_command_runs_the_safest_permitted_action() {
        // A caller that lists supersede-old first must not make the
        // ready-to-run command destructive.
        let p = resolve_conflict(Uuid::nil(), &["supersede-old", "keep-both"]);
        assert_eq!(p.allowed_actions, vec!["keep-both", "supersede-old"]);
        assert!(resolve_command_string(&p).ends_with("--action keep-both"));
    }

    #[test]
    fn empty_allowed_actions_yields_no_action_flag() {
        let p = resolve_conflict(Uuid::nil(), &[]);
        assert!(!p.args.contains(&"--action".to_string()));
        assert!(p.allowed_actions.is_empty());
    }

    #[test]
    fn unknown_actions_are_never_published() {
        let p = resolve_conflict(Uuid::nil(), &["requires_context", "delete-everything"]);
        assert!(p.allowed_actions.is_empty());
    }

    #[test]
    fn object_and_string_agree() {
        for p in payloads() {
            let s = resolve_command_string(&p);
            let rest = s.strip_prefix(PROGRAM).unwrap().trim_start();
            let expected = p
                .args
                .iter()
                .map(|a| shell_quote(a))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(rest, expected);
        }
    }

    #[test]
    fn values_with_spaces_are_quoted() {
        let p = retry_with_handle("edit", "mem:x", &["--content", "two words"]);
        assert!(resolve_command_string(&p).ends_with("'two words'"));
    }

    #[test]
    fn embedded_quotes_survive() {
        let p = retry_with_handle("edit", "mem:x", &["--content", "O'Brien"]);
        assert!(resolve_command_string(&p).ends_with("'O'\\''Brien'"));
    }

    #[test]
    fn action_round_trips() {
        for a in ResolveAction::ALL {
            assert_eq!(ResolveAction::parse(a.as_str()), Some(a));
        }
        assert_eq!(ResolveAction::parse("requires_context"), None);
    }

    #[test]
    fn attach_writes_both_halves() {
        let mut obj = Map::new();
        attach(&mut obj, &resolve_conflict(Uuid::nil(), ALL));
        assert!(obj["resolve"]["allowed_actions"].is_array());
        assert!(obj["resolve_command"]
            .as_str()
            .unwrap()
            .starts_with("engram "));
    }
}
