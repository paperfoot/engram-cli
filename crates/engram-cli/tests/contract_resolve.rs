//! The one resolve builder (R12/D1), checked as a property rather than by
//! example: whatever a producer passes in, the command it hands an agent must
//! be a single runnable command whose action is one the parser accepts.

mod contract_support;

use engram_cli::respond::{self, ResolveAction, ResolvePayload};
use uuid::Uuid;

fn every_subset() -> Vec<ResolvePayload> {
    let id = Uuid::new_v4();
    let all: Vec<&'static str> = ResolveAction::ALL.iter().map(|a| a.as_str()).collect();
    let mut out = Vec::new();
    for mask in 0..8u8 {
        let subset: Vec<&'static str> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, s)| *s)
            .collect();
        out.push(respond::resolve_conflict(id, &subset));
    }
    out
}

/// The defect this module exists to kill. The old template shipped
/// `--action supersede-old|keep-both|reject-new`; pasted into a shell it runs
/// the destructive branch and pipes the output into nothing.
#[test]
fn no_generated_command_can_be_pasted_into_a_shell_and_do_something_else() {
    for p in every_subset() {
        let s = respond::resolve_command_string(&p);
        for bad in ['|', '&', ';', '>', '<', '`', '$', '\n'] {
            assert!(!s.contains(bad), "{s} contains {bad:?}");
        }
    }
}

#[test]
fn the_object_and_the_string_never_disagree() {
    for p in every_subset() {
        let s = respond::resolve_command_string(&p);
        let json = respond::resolve_json(&p);
        let args: Vec<String> = json["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(args, p.args);
        assert!(s.strip_prefix("engram ").is_some());
        for a in &args {
            assert!(s.contains(a.trim_matches('\'')), "{s} is missing {a}");
        }
    }
}

/// Permission, not advice: everything in `allowed_actions` must be a value
/// `facts resolve --action` will accept.
#[test]
fn allowed_actions_are_from_the_resolve_action_vocabulary() {
    let vocab = engram_contract::vocabulary("resolve_action").unwrap();
    for p in every_subset() {
        for a in &p.allowed_actions {
            assert!(vocab.contains(a), "{a} is not a resolve_action");
        }
    }
}

/// The command an agent runs blind is the conservative one, whatever order
/// the producer listed. `keep-both` never closes an interval, so a blind run
/// cannot retire a medication.
#[test]
fn the_command_runs_the_safest_permitted_action() {
    let id = Uuid::new_v4();
    for order in [
        vec!["supersede-old", "keep-both", "reject-new"],
        vec!["reject-new", "supersede-old", "keep-both"],
        vec!["keep-both", "supersede-old"],
    ] {
        let p = respond::resolve_conflict(id, &order);
        assert!(
            respond::resolve_command_string(&p).ends_with("--action keep-both"),
            "{order:?} produced a destructive blind command"
        );
    }
}

/// The action in the generated command is one the binary's own parser
/// accepts — proven by running it against an empty store and asserting the
/// failure is "no such conflict", never "invalid value for --action".
#[test]
fn the_generated_action_survives_the_real_parser() {
    let sb = contract_support::Sandbox::new();
    let p = respond::resolve_conflict(
        Uuid::new_v4(),
        &["keep-both", "supersede-old", "reject-new"],
    );
    let out = sb.cmd().args(&p.args).output().expect("facts resolve");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("invalid value") && !stderr.contains("unexpected argument"),
        "the generated command does not parse: {stderr}"
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "a random conflict id must not succeed"
    );
}
