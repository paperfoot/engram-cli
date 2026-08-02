//! Wave 4 — `/v1/recall` stops hiding contradictions.
//!
//! `engram recall` has surfaced contested and closed facts since wave 3. The
//! daemon's `/v1/recall` did not, so an agent driving engram over HTTP was
//! handed one side of a live disagreement with no signal at all. One builder,
//! two surfaces.

mod daemon_support;

use daemon_support::*;
use serde_json::Value;

const CAROL: &str = r#"[{"subject":"Carol","predicate":"works_at","object":"Acme"},
                        {"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]"#;

fn contested_store() -> Store {
    let store = Store::new();
    store.seed(&[
        "remember",
        "One memo says Carol works at Acme, the next line says Beta Corp.",
        "--kb",
        "default",
        "--facts-json",
        CAROL,
    ]);
    store
}

fn recall(daemon: &Daemon, query: &str) -> Value {
    let (status, body) = daemon.post(
        "/v1/recall",
        &serde_json::json!({ "query": query, "profile": "offline", "kb": "default" }).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    body
}

#[test]
fn the_daemon_carries_the_conflicts_array() {
    let store = contested_store();
    let daemon = Daemon::start(&store);
    let body = recall(&daemon, "Carol");
    let conflicts = body["data"]["conflicts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !conflicts.is_empty(),
        "a live contradiction must reach the HTTP caller: {body}"
    );
    let first = &conflicts[0];
    assert_eq!(first["status"], "contested");
    assert_eq!(first["subject"], "Carol");
}

#[test]
fn a_clean_store_carries_an_empty_array_not_a_missing_key() {
    // Absence-as-signal is already spoken for by `embedder_degraded`; reusing
    // it for a second meaning is how presence checks rot.
    let store = Store::new();
    store.seed(&[
        "remember",
        "Nothing here contradicts anything.",
        "--kb",
        "default",
    ]);
    let daemon = Daemon::start(&store);
    let body = recall(&daemon, "nothing");
    assert!(
        body["data"]["conflicts"].is_array(),
        "the key is always present: {body}"
    );
    assert_eq!(body["data"]["conflicts"].as_array().unwrap().len(), 0);
}

#[test]
fn the_cli_and_the_daemon_emit_the_same_conflicts() {
    let store = contested_store();
    let daemon = Daemon::start(&store);
    let http = recall(&daemon, "Carol");
    let cli = store.seed(&["recall", "Carol", "--kb", "default", "--profile", "offline"]);

    let ids = |v: &Value| -> Vec<String> {
        let mut out: Vec<String> = v["data"]["conflicts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c["conflict_id"].as_str().map(str::to_string))
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        ids(&http),
        ids(&cli),
        "one builder, two surfaces:\nhttp={http}\ncli={cli}"
    );
}

#[test]
fn every_suggested_action_is_in_the_ruled_vocabulary() {
    let store = contested_store();
    let daemon = Daemon::start(&store);
    let body = recall(&daemon, "Carol");
    for c in body["data"]["conflicts"].as_array().unwrap() {
        let suggested = c["suggested_action"].as_str().unwrap_or_default();
        assert!(
            matches!(
                suggested,
                "keep-both" | "supersede-old" | "requires_context"
            ),
            "R7's three values and no fourth spelling: {c}"
        );
    }
}

#[test]
fn the_resolve_command_is_one_runnable_command() {
    // The old template shipped `--action supersede-old|keep-both|reject-new`.
    // Pasted into a shell that runs the FIRST alternative — the destructive
    // one — and pipes the result through two nonexistent programs. It has
    // happened, and it retired a live medication record.
    let store = contested_store();
    let daemon = Daemon::start(&store);
    let body = recall(&daemon, "Carol");
    for c in body["data"]["conflicts"].as_array().unwrap() {
        let cmd = c["resolve_command"].as_str().unwrap_or_default();
        assert!(!cmd.is_empty(), "{c}");
        for bad in ['|', '&', ';', '>', '<', '`', '$'] {
            assert!(!cmd.contains(bad), "`{cmd}` contains {bad}");
        }
        assert!(
            c["resolve"]["allowed_actions"].is_array(),
            "permission is a machine-readable list, not a string to split: {c}"
        );
    }
}

#[test]
fn research_keeps_its_shape() {
    // `/v1/research` moved onto the pooled reader and the shared envelope this
    // wave; it must answer byte-for-byte what wave 0.5 pinned.
    let store = contested_store();
    let daemon = Daemon::start(&store);
    let (status, body) = daemon.post(
        "/v1/research",
        r#"{"query":"Carol","profile":"offline","kb":"default"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let meta = &body["data"]["metadata"];
    assert_eq!(meta["embed_model"], "stub-64", "{body}");
    assert_eq!(meta["embed_dims"], 64, "{body}");
    assert!(body["data"]["answer_context"].is_string(), "{body}");
}
