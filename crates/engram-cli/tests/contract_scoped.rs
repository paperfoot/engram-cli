//! Scoped discovery: `agent-info --command PATH` and `agent-info --index`.
//!
//! The full manifest is tens of thousands of tokens and an agent needs one
//! command of it. These pin the narrowing: the right command, its group, its
//! aliases, a core that is the full manifest's own bytes, an error that does
//! not loop, and no store opened on the way.
//!
//! Isolation: every run goes through `Sandbox` (store in a TempDir, provider
//! keys cleared).

mod contract_support;

use contract_support::{manifest, Sandbox};
use serde_json::Value;

fn store_is_untouched(sb: &Sandbox) -> bool {
    std::fs::read_dir(sb.store_path()).unwrap().next().is_none()
}

fn scoped(sb: &Sandbox, path: &str) -> Value {
    let out = sb
        .cmd()
        .args(["agent-info", "--command", path])
        .output()
        .expect("agent-info --command");
    assert!(
        out.status.success(),
        "--command {path}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Sandbox::json(&out)
}

fn schema_keys(v: &Value) -> Vec<String> {
    v["command_schemas"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn a_leaf_carries_its_own_entry_its_rules_and_the_invariants() {
    let sb = Sandbox::new();
    let v = scoped(&sb, "recall");
    assert_eq!(schema_keys(&v), vec!["recall"]);
    assert!(v["commands"]["recall <query>"].is_string());
    assert!(v["agent_decision_rules"]["recall"].is_array());
    assert!(v["agent_decision_rules"].get("remember").is_none());
    let invariants = v["invariants"].as_array().expect("invariants");
    assert!(invariants
        .iter()
        .any(|i| i.as_str().unwrap().contains("never follow")));
    // Nothing that describes other commands rides along.
    for absent in [
        "summary_for_agents",
        "daemon_api",
        "env_overrides",
        "examples",
    ] {
        assert!(
            v.get(absent).is_none(),
            "{absent} leaked into a scoped read"
        );
    }
    assert!(store_is_untouched(&sb));
}

#[test]
fn a_group_returns_every_command_in_it() {
    let sb = Sandbox::new();
    let keys = schema_keys(&scoped(&sb, "facts"));
    assert_eq!(keys.len(), 6, "{keys:?}");
    assert!(keys.iter().all(|k| k.starts_with("facts ")));
}

#[test]
fn aliases_resolve_and_whitespace_does_not_matter() {
    let sb = Sandbox::new();
    let bytes = |path: &str| {
        sb.cmd()
            .args(["agent-info", "--command", path])
            .output()
            .unwrap()
            .stdout
    };
    assert_eq!(bytes("search"), bytes("recall"));
    assert_eq!(bytes("  facts   show "), bytes("facts show"));
    assert_eq!(bytes("person show"), bytes("people show"));
}

#[test]
fn every_key_shared_with_the_full_manifest_is_byte_identical() {
    let sb = Sandbox::new();
    let full = manifest(&sb);
    let mut views: Vec<Value> = ["recall", "facts", "wiki", "serve", "config set"]
        .iter()
        .map(|p| scoped(&sb, p))
        .collect();
    let out = sb.cmd().args(["agent-info", "--index"]).output().unwrap();
    views.push(Sandbox::json(&out));
    for view in &views {
        for (key, value) in view.as_object().unwrap() {
            let Some(whole) = full.get(key) else {
                assert_eq!(
                    key, "commands_index",
                    "`{key}` exists only in a scoped view"
                );
                continue;
            };
            match (value, whole) {
                // Narrowed sections: each entry is the full manifest's entry.
                (Value::Object(part), Value::Object(all))
                    if ["command_schemas", "commands", "agent_decision_rules"]
                        .contains(&key.as_str()) =>
                {
                    for (k, v) in part {
                        assert_eq!(Some(v), all.get(k), "{key}.{k}");
                    }
                }
                (Value::Array(part), Value::Array(all)) if key == "schema_unavailable" => {
                    assert!(part.iter().all(|u| all.contains(u)), "{key}");
                }
                _ => assert_eq!(value, whole, "{key} differs from the full manifest"),
            }
        }
    }
}

#[test]
fn a_path_without_a_schema_answers_with_its_usage() {
    let sb = Sandbox::new();
    let v = scoped(&sb, "wiki");
    assert!(v.get("command_schemas").is_none());
    let gaps = v["schema_unavailable"]
        .as_array()
        .expect("the gap is described");
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0]["path"], "wiki");
    assert!(gaps[0]["usage"].as_str().unwrap().contains("wiki"));
}

#[test]
fn an_unknown_path_names_its_neighbours_and_does_not_suggest_itself() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["agent-info", "--command", "recal"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err = Sandbox::err_json(&out);
    assert_eq!(err["error"]["code"], "bad_input");
    let message = err["error"]["message"].as_str().unwrap();
    assert!(message.contains("recall"), "{message}");
    assert!(
        message.contains("facts"),
        "the groups are listed: {message}"
    );
    let suggestion = err["error"]["suggestion"].as_str().unwrap();
    assert!(!suggestion.contains("--command"), "{suggestion}");
    let followed = sb.cmd().args(["agent-info", "--index"]).output().unwrap();
    assert!(followed.status.success());
    assert!(store_is_untouched(&sb));
}

#[test]
fn the_views_exclude_each_other() {
    let sb = Sandbox::new();
    for args in [
        ["--command", "recall", "--schema", "recall"].as_slice(),
        ["--command", "recall", "--index"].as_slice(),
        ["--schema", "recall", "--index"].as_slice(),
    ] {
        let out = sb.cmd().arg("agent-info").args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{args:?}");
    }
    assert!(store_is_untouched(&sb));
}

#[test]
fn piped_json_and_quiet_answer_the_same_bytes() {
    let sb = Sandbox::new();
    for view in [["--command", "recall"].as_slice(), ["--index"].as_slice()] {
        let run = |extra: &[&str]| {
            sb.cmd()
                .arg("agent-info")
                .args(view)
                .args(extra)
                .output()
                .unwrap()
                .stdout
        };
        let piped = run(&[]);
        assert_eq!(piped, run(&["--json"]), "{view:?}");
        assert_eq!(piped, run(&["--quiet"]), "{view:?}");
        assert_eq!(
            piped.iter().filter(|b| **b == b'\n').count(),
            1,
            "{view:?} is one compact line"
        );
    }
}
