//! Wave 3, read surfaces — deliverable 3: an empty answer proves the store
//! looked, says where, and says how close it got.
//!
//! Without the certificate, "no results" and "you searched the wrong
//! namespace" are the same response, and an agent that cannot tell them apart
//! reports a memory as absent when it is one KB away.

mod wave3_read_support;

use serde_json::Value;
use wave3_read_support::{remember, remember_fact, run_json, store};

#[test]
fn an_empty_store_answers_no_match_with_a_certificate() {
    let dir = store();
    let v = run_json(dir.path(), &["recall", "anything", "--profile", "offline"]);

    assert_eq!(v["status"], Value::from("no_results"));
    assert_eq!(v["data"]["verdict"], Value::from("no_match"));
    let absence = &v["data"]["absence"];
    assert_eq!(absence["reason"], Value::from("no_chunks_in_scope"));
    assert_eq!(absence["searched_chunks"], Value::from(0));
    // Nothing scored, so there is no best score to report — and none is
    // invented.
    assert!(absence.get("top_score").is_none(), "{absence}");
}

/// Proves an agent can tell a scoping mistake from an empty store.
#[test]
fn a_wrong_kb_recall_names_the_kb_it_searched() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "work");

    let v = run_json(
        dir.path(),
        &[
            "recall",
            "Lisbon",
            "--kb",
            "personal",
            "--profile",
            "offline",
        ],
    );
    assert_eq!(v["data"]["verdict"], Value::from("no_match"));
    assert_eq!(
        v["data"]["absence"]["searched_kbs"],
        serde_json::json!(["personal"])
    );
    assert_eq!(
        v["metadata"]["scope"]["kbs"],
        serde_json::json!(["personal"])
    );
    assert_eq!(v["metadata"]["scope"]["origin"], Value::from("flag"));
}

/// Nothing in the retrieval path rejects a candidate by score, so there is no
/// threshold to report. A fabricated `0.0` would be tuned against by the first
/// agent that saw it.
#[test]
fn the_certificate_never_fabricates_a_threshold() {
    let dir = store();
    let v = run_json(dir.path(), &["recall", "anything", "--profile", "offline"]);
    let absence = &v["data"]["absence"];
    assert!(
        absence.get("threshold").is_none() || absence["threshold"].is_null(),
        "a threshold nobody applied was reported: {absence}"
    );
}

#[test]
fn results_carry_no_absence_certificate() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "personal");
    let v = run_json(dir.path(), &["recall", "Lisbon", "--profile", "offline"]);
    assert_eq!(v["data"]["verdict"], Value::from("results"));
    assert!(v["data"].get("absence").is_none(), "{v}");
    assert_eq!(v["status"], Value::from("success"));
}

/// `verdict` and `status` are computed from one boolean, so they cannot drift
/// into disagreeing about whether the read found anything.
#[test]
fn verdict_and_status_always_agree() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "personal");
    for (args, status, verdict) in [
        (
            vec!["recall", "Lisbon", "--profile", "offline"],
            "success",
            "results",
        ),
        // The offline stub scores every chunk, so a query that matches
        // nothing still ranks something; an empty answer needs an empty
        // SCOPE, which is exactly the case the certificate exists for.
        (
            vec![
                "recall",
                "Lisbon",
                "--kb",
                "nosuchkb",
                "--profile",
                "offline",
            ],
            "no_results",
            "no_match",
        ),
    ] {
        let v = run_json(dir.path(), &args);
        assert_eq!(v["status"], Value::from(status), "{}", args.join(" "));
        assert_eq!(
            v["data"]["verdict"],
            Value::from(verdict),
            "{}",
            args.join(" ")
        );
    }
}

/// Every read says what it looked at, results or not.
#[test]
fn scope_metadata_is_present_on_every_read() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon", "personal");
    remember_fact(
        dir.path(),
        "note",
        "personal",
        "default",
        "Ana",
        "lives_in",
        "Lisbon",
    );

    for args in [
        vec!["recall", "Lisbon", "--profile", "offline"],
        vec![
            "research",
            "Lisbon",
            "--kb",
            "personal",
            "--profile",
            "offline",
        ],
        vec!["memory", "search", "Lisbon", "--profile", "offline"],
        vec!["facts", "list", "--kb", "personal"],
        vec!["facts", "show", "Ana", "--kb", "personal", "--diary", "*"],
        vec!["facts", "conflicts"],
        vec!["facts", "conflicts", "--open"],
        vec!["people", "list"],
        vec!["entities", "list", "--kb", "personal"],
        vec!["documents", "list", "--kb", "personal"],
        vec!["wiki", "--kb", "personal"],
    ] {
        let v = run_json(dir.path(), &args);
        let scope = &v["metadata"]["scope"];
        let label = args.join(" ");
        assert!(scope.is_object(), "{label} has no metadata.scope");
        assert!(scope["kbs"].is_array(), "{label}: {scope}");
        assert!(scope["diary"].is_string(), "{label}: {scope}");
        assert!(scope["origin"].is_string(), "{label}: {scope}");
    }
}

/// A surface with no diary column says so, rather than reporting `diary: "*"`
/// as though a filter had run and matched everything.
#[test]
fn undiaried_surfaces_say_the_diary_does_not_apply() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon", "personal");
    for args in [
        vec!["entities", "list", "--kb", "personal"],
        vec!["people", "list"],
        vec!["documents", "list", "--kb", "personal"],
    ] {
        let v = run_json(dir.path(), &args);
        let scope = &v["metadata"]["scope"];
        assert_eq!(
            scope["diary_applicable"],
            Value::Bool(false),
            "{}: {scope}",
            args.join(" ")
        );
        // And no fabricated chunk count from a surface that scores none.
        assert!(scope.get("chunks_considered").is_none(), "{scope}");
    }
}

/// `memory search` and `recall` are one code path, so an agent that learns one
/// envelope has learned both.
#[test]
fn memory_search_and_recall_return_the_same_envelope_keys() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "personal");

    let recall = run_json(dir.path(), &["recall", "Lisbon", "--profile", "offline"]);
    let search = run_json(
        dir.path(),
        &["memory", "search", "Lisbon", "--profile", "offline"],
    );

    let keys = |v: &Value, section: &str| -> Vec<String> {
        let mut k: Vec<String> = v[section]
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    };
    assert_eq!(keys(&recall, "data"), keys(&search, "data"));
    assert_eq!(keys(&recall, "metadata"), keys(&search, "metadata"));
}

#[test]
fn memory_search_reports_which_spelling_answered() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon", "personal");
    let search = run_json(
        dir.path(),
        &["memory", "search", "Lisbon", "--profile", "offline"],
    );
    assert_eq!(
        search["metadata"]["command_alias"],
        Value::from("memory search")
    );
    let recall = run_json(dir.path(), &["recall", "Lisbon", "--profile", "offline"]);
    assert_eq!(recall["metadata"]["command_alias"], Value::from("recall"));
    // `search` is a clap alias of `recall`, not a third command.
    let alias = run_json(dir.path(), &["search", "Lisbon", "--profile", "offline"]);
    assert_eq!(alias["metadata"]["command_alias"], Value::from("recall"));
    assert_eq!(alias["data"]["results"], recall["data"]["results"]);
}
