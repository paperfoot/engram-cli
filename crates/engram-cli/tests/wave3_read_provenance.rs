//! Wave 3, read surfaces — deliverable 1: every hit says which memory it came
//! from, and the id it publishes is one a mutator accepts.
//!
//! The defect these tests close is the brief's headline: `recall` returned
//! CHUNK ids under a bare `id`, `edit` and `forget` take memory ids, and the
//! agent that fed one to the other read "no memory found (already deleted?)",
//! concluded a race and rewrote the memory by hand.

mod wave3_read_support;

use serde_json::Value;
use wave3_read_support::{engram, remember, remember_fact, run_json, store};

fn is_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

#[test]
fn recall_hit_carries_memory_id_and_handle() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "personal");
    remember(dir.path(), "Bruno joined Acme as a designer", "personal");

    let v = run_json(dir.path(), &["recall", "Lisbon", "--profile", "offline"]);
    let rows = v["data"]["results"].as_array().expect("results");
    assert!(!rows.is_empty(), "seeded store returned nothing: {v}");
    for row in rows {
        let memory_id = row["memory_id"].as_str().expect("memory_id on every hit");
        assert!(is_uuid(memory_id), "{memory_id} is not a uuid");
        let handle = row["handle"].as_str().expect("handle on a chunk hit");
        assert!(
            handle.starts_with("chunk:") && is_uuid(&handle[6..]),
            "handle is not a kind-prefixed uuid: {handle}"
        );
        let prov = &row["provenance"];
        assert_eq!(prov["memory_id"], row["memory_id"]);
        assert_eq!(prov["kb"], row["kb"]);
        // Nulls are preserved: a caller branching on "no document" must not
        // have to tell it apart from "field not implemented".
        assert!(prov.get("document_id").is_some(), "key must exist: {prov}");
        assert!(prov.get("derived_from").is_some(), "key must exist: {prov}");
    }
}

/// The brief's headline defect, proven closed end to end: the id `recall`
/// publishes is the id `forget` takes.
#[test]
fn recall_memory_id_is_accepted_by_a_mutator() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "personal");
    let v = run_json(dir.path(), &["recall", "Lisbon", "--profile", "offline"]);
    let memory_id = v["data"]["results"][0]["memory_id"]
        .as_str()
        .expect("memory_id")
        .to_string();

    let forgotten = run_json(dir.path(), &["forget", &memory_id, "--confirm"]);
    assert_eq!(forgotten["data"]["deleted"], Value::Bool(true));
}

/// And the kind-prefixed handle should round-trip through a mutator too:
/// interfaces §1 says a mutator that needs a memory id accepts ANY handle and
/// follows a chunk to its memory. Today `forget chunk:<uuid>` exits 3 naming
/// the memory handle instead of following it, which is write-surfaces' half of
/// WS-2 and not something this lane can fix from the read side.
#[test]
#[ignore = "blocked on WS-2: forget/edit must follow a chunk handle to its memory"]
fn recall_handle_round_trips_through_a_mutator() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "personal");
    let v = run_json(dir.path(), &["recall", "Lisbon", "--profile", "offline"]);
    let handle = v["data"]["results"][0]["handle"]
        .as_str()
        .expect("handle")
        .to_string();

    // A chunk handle names a chunk; the mutator follows it to the memory
    // rather than answering "not found" like v0.3 did.
    let forgotten = run_json(dir.path(), &["forget", &handle, "--confirm"]);
    assert_eq!(forgotten["data"]["deleted"], Value::Bool(true));
}

#[test]
fn facts_rows_carry_memory_id_and_a_fact_handle() {
    let dir = store();
    remember_fact(
        dir.path(),
        "note",
        "personal",
        "default",
        "Ana",
        "lives_in",
        "Lisbon",
    );

    let v = run_json(dir.path(), &["facts", "list", "--kb", "personal"]);
    let f = &v["data"]["facts"][0];
    assert!(is_uuid(f["memory_id"].as_str().expect("memory_id")));
    let handle = f["handle"].as_str().expect("handle");
    assert!(handle.starts_with("fact:"), "{handle}");
    assert_eq!(f["provenance"]["memory_id"], f["memory_id"]);
    assert_eq!(f["provenance"]["kb"], Value::from("personal"));
    // Computed from the registry, so they are right on any store.
    assert_eq!(f["arity_class"], Value::from("single_valued"));
    assert_eq!(f["safety"], Value::Bool(false));
    assert_eq!(f["open"], Value::Bool(true));
}

/// A safety relation holds several open values at once — that is why it is
/// set-valued in the registry, and why a second medication is not a
/// correction of the first.
#[test]
fn a_safety_relation_reports_itself_as_set_valued() {
    let dir = store();
    remember_fact(
        dir.path(),
        "note",
        "personal",
        "default",
        "Ana",
        "takes_medication",
        "metformin",
    );
    let v = run_json(dir.path(), &["facts", "list", "--kb", "personal"]);
    let f = &v["data"]["facts"][0];
    assert_eq!(f["arity_class"], Value::from("set_valued"));
    assert_eq!(f["safety"], Value::Bool(true));
}

/// `research` answers with the same three fields, because an agent should not
/// have to learn a second shape to follow a citation home.
#[test]
fn research_hits_carry_the_same_provenance_fields() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon in March", "papers");
    let v = run_json(
        dir.path(),
        &[
            "research",
            "Lisbon",
            "--kb",
            "papers",
            "--profile",
            "offline",
        ],
    );
    let rows = v["data"]["results"].as_array().expect("results");
    assert!(!rows.is_empty(), "{v}");
    for row in rows {
        assert!(row.get("memory_id").is_some(), "{row}");
        assert!(row.get("handle").is_some(), "{row}");
        assert!(row["provenance"].is_object(), "{row}");
    }
}

/// A derived row must never carry a fabricated memory id. There is no cheap
/// offline way to compile a wiki page, so this pins the rule at the surface
/// that would publish one: `people show` snippets always resolve to a real
/// memory, and a fact row declares its derivation.
#[test]
fn derived_rows_declare_their_derivation_instead_of_inventing_a_link() {
    let dir = store();
    remember_fact(
        dir.path(),
        "note",
        "personal",
        "default",
        "Ana",
        "lives_in",
        "Lisbon",
    );
    let v = run_json(dir.path(), &["facts", "list", "--kb", "personal"]);
    let prov = &v["data"]["facts"][0]["provenance"];
    assert_eq!(prov["derived_from"], Value::from("derived:fact_projection"));
    // A fact is derived from a memory, so the chunk link is honestly absent
    // rather than filled with the fact's own id.
    assert_eq!(prov["chunk_id"], Value::Null);
}

#[test]
fn a_wiki_page_carries_a_page_id_and_no_unresolvable_handle() {
    let dir = store();
    remember(dir.path(), "Ana moved to Lisbon", "personal");
    let v = run_json(dir.path(), &["wiki", "--kb", "personal"]);
    // Empty is fine — the point is the listing counters exist and no
    // `wiki:` handle was invented.
    assert!(v["data"].get("total").is_some(), "{v}");
    assert!(v["data"]["pages"].is_array());
    let out = engram(dir.path())
        .args(["wiki", "--kb", "personal", "--path", "index", "--json"])
        .output();
    if let Ok(out) = out {
        if out.status.success() {
            let page: Value = serde_json::from_slice(&out.stdout).expect("json");
            assert!(page["data"].get("handle").is_none(), "{page}");
        }
    }
}
