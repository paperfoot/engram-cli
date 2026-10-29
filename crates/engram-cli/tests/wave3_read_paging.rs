//! Wave 3, read surfaces — deliverable 2: `total` means the rows that match,
//! `returned` means the rows in this response, and `count` keeps meaning what
//! it always did (the page) so nothing written against v0.3 breaks.
//!
//! The defect: agents reported three open conflicts to a user with 953 queued,
//! because `count` was the only number a listing published and it was the page
//! length.

mod wave3_read_support;

use serde_json::Value;
use wave3_read_support::{remember, remember_fact, run_err, run_json, store};

/// `n` open facts about one subject. A SET-valued relation on purpose:
/// several values coexist, so the seed measures paging rather than the
/// classifier. Distinct subjects would be collapsed by entity hygiene, which
/// reads `Person01` and `Person02` as spellings of one person.
fn seed_facts(dir: &std::path::Path, n: usize) {
    for i in 0..n {
        remember_fact(
            dir,
            &format!("note {i}"),
            "personal",
            "default",
            "Ana",
            "knows",
            &format!("Contact {i:02}"),
        );
    }
}

#[test]
fn facts_list_total_exceeds_the_page() {
    let dir = store();
    seed_facts(dir.path(), 25);

    let v = run_json(
        dir.path(),
        &["facts", "list", "--kb", "personal", "--limit", "5"],
    );
    assert_eq!(v["data"]["returned"], Value::from(5));
    assert_eq!(v["data"]["total"], Value::from(25));
    // The legacy field stays, and stays equal to the page.
    assert_eq!(v["data"]["count"], v["data"]["returned"]);
    assert!(v["data"]["cursor"].is_string(), "a page has a successor");
}

#[test]
fn a_cursor_walks_the_whole_set_exactly_once() {
    let dir = store();
    seed_facts(dir.path(), 25);

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let mut args = vec![
            "facts".to_string(),
            "list".to_string(),
            "--kb".to_string(),
            "personal".to_string(),
            "--limit".to_string(),
            "5".to_string(),
        ];
        if let Some(c) = &cursor {
            args.push("--cursor".to_string());
            args.push(c.clone());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = run_json(dir.path(), &borrowed);
        for f in v["data"]["facts"].as_array().expect("facts") {
            seen.push(f["id"].as_str().expect("id").to_string());
        }
        assert_eq!(v["data"]["total"], Value::from(25));
        match v["data"]["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            // An ABSENT cursor is the only end-of-listing signal.
            None => {
                cursor = None;
                break;
            }
        }
    }
    assert!(cursor.is_none(), "the walk never reached the last page");
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(seen.len(), 25, "every row exactly once: {}", seen.len());
    assert_eq!(unique.len(), 25, "a row was repeated across pages");
}

/// A cursor issued for one scope must not page another at the same offset:
/// that is a wrong answer wearing the shape of a right one.
#[test]
fn a_cursor_is_bound_to_the_scope_that_issued_it() {
    let dir = store();
    seed_facts(dir.path(), 25);
    remember_fact(
        dir.path(),
        "work note",
        "business",
        "default",
        "Cara",
        "works_at",
        "Acme",
    );

    let v = run_json(
        dir.path(),
        &["facts", "list", "--kb", "personal", "--limit", "5"],
    );
    let cursor = v["data"]["cursor"].as_str().expect("cursor").to_string();

    let (code, err) = run_err(
        dir.path(),
        &[
            "facts", "list", "--kb", "business", "--limit", "5", "--cursor", &cursor,
        ],
    );
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], Value::from("bad_input"));
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("different scope"),
        "{err}"
    );
}

#[test]
fn a_malformed_cursor_is_exit_3_not_page_one() {
    let dir = store();
    seed_facts(dir.path(), 3);
    let (code, err) = run_err(
        dir.path(),
        &["facts", "list", "--kb", "personal", "--cursor", "garbage"],
    );
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], Value::from("bad_input"));
}

/// The 953-conflicts defect, in miniature: the page is three, the backlog is
/// twelve, and both numbers are in the response.
#[test]
fn conflicts_total_is_the_backlog_not_the_page() {
    let dir = store();
    // Twelve unmistakably different people: entity hygiene merges
    // near-identical spellings, so `Person01`/`Person02` would seed one
    // subject and one conflict.
    for subject in [
        "Ana", "Bruno", "Cara", "Dmitri", "Elena", "Farid", "Gita", "Hugo", "Iris", "Jonas",
        "Kira", "Liam",
    ] {
        remember_fact(
            dir.path(),
            "first",
            "personal",
            "default",
            subject,
            "lives_in",
            "Lisbon",
        );
        remember_fact(
            dir.path(),
            "second",
            "personal",
            "default",
            subject,
            "lives_in",
            "Porto",
        );
    }

    let v = run_json(
        dir.path(),
        &["facts", "conflicts", "--open", "--limit", "3"],
    );
    assert_eq!(v["data"]["returned"], Value::from(3));
    assert_eq!(v["data"]["total"], Value::from(12));
    assert_eq!(v["data"]["total_open"], Value::from(12));
    assert_eq!(v["data"]["count"], Value::from(3));
    assert!(v["data"]["cursor"].is_string());
}

/// Table-driven over every listing surface this lane owns: three keys, one
/// meaning each, everywhere.
#[test]
fn every_listing_surface_reports_returned_and_total() {
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
        vec!["facts", "list", "--kb", "personal"],
        vec!["facts", "conflicts"],
        vec!["facts", "conflicts", "--open"],
        vec!["people", "list"],
        vec!["documents", "list", "--kb", "personal"],
    ] {
        let v = run_json(dir.path(), &args);
        let data = &v["data"];
        let label = args.join(" ");
        assert!(data.get("returned").is_some(), "{label} has no returned");
        assert!(data.get("total").is_some(), "{label} has no total");
        assert_eq!(
            data["count"], data["returned"],
            "{label}: count != returned"
        );
    }
}

/// `memory list` is write-surfaces' file (spec §1.3 / WS-1) and is the one
/// listing still reporting only a page length.
#[test]
#[ignore = "blocked on WS-1: memory list adopts output::Listing"]
fn memory_list_reports_returned_and_total() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon", "personal");
    let v = run_json(dir.path(), &["memory", "list"]);
    assert!(v["data"].get("total").is_some(), "{v}");
    assert_eq!(v["data"]["count"], v["data"]["returned"]);
}

/// A ranked read is a truncation, not a page. Emitting a cursor there would be
/// a lie with a stable-looking shape, because the order depends on a reranker
/// whose output is not guaranteed stable between calls.
#[test]
fn ranked_reads_publish_no_cursor() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon", "personal");

    for args in [
        vec!["recall", "Lisbon", "--profile", "offline"],
        vec!["memory", "search", "Lisbon", "--profile", "offline"],
    ] {
        let v = run_json(dir.path(), &args);
        let label = args.join(" ");
        assert!(
            v["data"].get("cursor").is_none(),
            "{label} published a cursor"
        );
        assert!(v["data"].get("returned").is_some(), "{label}");
        // The pool the ranking chose from, named for what it is rather than
        // borrowing `total`, which means "matching rows in scope" elsewhere.
        assert!(v["data"].get("candidates_fused").is_some(), "{label}");
        assert!(v["data"].get("total").is_none(), "{label} reused `total`");
    }
}
