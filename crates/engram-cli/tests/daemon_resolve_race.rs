//! Wave 4 — `facts resolve` can only be won once.
//!
//! The old body ran five statements with no transaction and discarded two
//! booleans. Two processes both passed the `status == "open"` check, both
//! called `supersede_fact_with`, and both printed `"resolved": true`; with
//! different actions the pair could end up with each fact superseded by the
//! other — a two-node cycle with no open interval and two success reports.

mod daemon_support;

use daemon_support::*;
use serde_json::Value;

const CAROL: &str = r#"[{"subject":"Carol","predicate":"works_at","object":"Acme"},
                        {"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]"#;
const ANA: &str = r#"[{"subject":"Ana","predicate":"takes_medication","object":"warfarin"},
                      {"subject":"Ana","predicate":"takes_medication","object":"apixaban 5mg"}]"#;

fn seeded_conflict(store: &Store, facts: &str, content: &str) -> String {
    let out = store.seed(&[
        "remember",
        content,
        "--kb",
        "personal",
        "--facts-json",
        facts,
    ]);
    out["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap_or_else(|| panic!("a conflict was expected: {out}"))
        .to_string()
}

/// Active AND historic, so a closed interval is still visible.
fn facts_of(store: &Store, subject: &str) -> Vec<Value> {
    let out = store.seed(&["facts", "show", subject, "--kb", "personal", "--diary", "*"]);
    let mut all = out["data"]["active"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    all.extend(
        out["data"]["historic"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    all
}

/// A failing command prints its envelope on stderr, so a test that reads only
/// stdout sees `null` and learns nothing.
fn envelope(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .or_else(|_| serde_json::from_slice(&out.stderr))
        .unwrap_or(Value::Null)
}

#[test]
fn concurrent_resolves_have_exactly_one_winner() {
    let store = Store::new();
    let id = seeded_conflict(
        &store,
        CAROL,
        "One memo says Carol is at Acme, the next line says Beta Corp.",
    );

    // Two processes, at the same time, with DIFFERENT actions. This is the
    // crossed pair that could supersede both facts.
    let outcomes: Vec<(i32, Value)> = std::thread::scope(|scope| {
        let handles: Vec<_> = ["supersede-old", "reject-new"]
            .iter()
            .map(|action| {
                let store = &store;
                let id = id.clone();
                scope.spawn(move || {
                    let mut cmd = store.engram();
                    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
                    let out = cmd
                        .args(["facts", "resolve", &id, "--action", action, "--json"])
                        .output()
                        .expect("engram runs");
                    (out.status.code().unwrap_or(-1), envelope(&out))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    });

    let winners = outcomes.iter().filter(|(code, _)| *code == 0).count();
    assert_eq!(winners, 1, "exactly one resolve may succeed: {outcomes:?}");
    let (code, body) = outcomes.iter().find(|(c, _)| *c != 0).expect("a loser");
    assert_eq!(
        *code, 3,
        "a lost claim is exit 3, the same class as a bad handle, never a retryable 1"
    );
    assert_eq!(
        body["error"]["code"], "conflict_already_resolved",
        "the loser must be distinguishable from a typo: {body}"
    );

    // And the loser mutated nothing: exactly one fact is closed, and the two
    // do not point at each other.
    let facts = facts_of(&store, "Carol");
    let closed: Vec<&Value> = facts
        .iter()
        .filter(|f| !f["superseded_by"].is_null() || !f["valid_until"].is_null())
        .collect();
    assert_eq!(closed.len(), 1, "exactly one interval closed: {facts:#?}");
    let cycle = facts.iter().any(|a| {
        facts.iter().any(|b| {
            a["id"] != b["id"] && a["superseded_by"] == b["id"] && b["superseded_by"] == a["id"]
        })
    });
    assert!(!cycle, "no two-node supersession cycle: {facts:#?}");
}

#[test]
fn resolving_a_resolved_conflict_is_not_bad_input() {
    let store = Store::new();
    let id = seeded_conflict(
        &store,
        CAROL,
        "One memo says Carol is at Acme, the next line says Beta Corp.",
    );
    store.seed(&["facts", "resolve", &id, "--action", "keep-both"]);
    let out = store
        .engram()
        .args(["facts", "resolve", &id, "--action", "keep-both", "--json"])
        .output()
        .expect("engram runs");
    assert_eq!(out.status.code(), Some(3));
    let body = envelope(&out);
    assert_eq!(
        body["error"]["code"], "conflict_already_resolved",
        "`bad_input` is what a typo gets; an agent cannot branch on it: {body}"
    );
}

#[test]
fn keep_both_records_a_decision() {
    // Where this used to write NOTHING at all: the caller's answer existed
    // only in the queue row's status, so every re-derivation asked again.
    let store = Store::new();
    let id = seeded_conflict(
        &store,
        CAROL,
        "One memo says Carol is at Acme, the next line says Beta Corp.",
    );
    let out = store.seed(&["facts", "resolve", &id, "--action", "keep-both"]);
    assert_eq!(out["data"]["resolved"], true, "{out}");
    let decision = out["data"]["decision_id"].as_str().unwrap_or_default();
    assert!(
        !decision.is_empty(),
        "keep-both must leave a decision behind: {out}"
    );

    // The verdict alphabet is the JOURNAL's, not the CLI's: writing
    // `supersede-old` into `decision.recorded` would break replay silently.
    let s = store.open();
    let row = s
        .lookup_decision("personal", &decision_key_of(&s, &id))
        .expect("lookup")
        .expect("a decision for this key");
    assert_eq!(row.verdict, "keep_both", "R4's vocabulary, not the flag's");
    assert_eq!(row.authority, "caller");
    // Both facts stay open — keep-both closes nothing.
    let facts = facts_of(&store, "Carol");
    let open = facts
        .iter()
        .filter(|f| f["superseded_by"].is_null() && f["valid_until"].is_null())
        .count();
    assert_eq!(open, 2, "keep-both keeps both: {facts:#?}");
}

#[test]
fn a_supersede_old_verdict_is_recorded_as_close_old() {
    let store = Store::new();
    let id = seeded_conflict(
        &store,
        CAROL,
        "One memo says Carol is at Acme, the next line says Beta Corp.",
    );
    let out = store.seed(&["facts", "resolve", &id, "--action", "supersede-old"]);
    let decision = out["data"]["decision_id"].as_str().unwrap_or_default();
    assert!(!decision.is_empty(), "{out}");
    let s = store.open();
    let row = s
        .lookup_decision("personal", &decision_key_of(&s, &id))
        .expect("lookup")
        .expect("a decision");
    assert_eq!(row.verdict, "close_old");
    // The close is journalled by storage, once — this lane appends nothing on
    // top of it, so `admin rebuild` sees one event per close.
    let closed_id = out["data"]["result"]["superseded"]["id"]
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .expect("a superseded fact");
    let events = s.events_for_subject(closed_id).expect("events");
    let closes = events
        .iter()
        .filter(|e| e.kind == engram_core::events::EventKind::FactIntervalClosed)
        .count();
    assert_eq!(
        closes, 1,
        "exactly one fact.interval_closed per close, or a rebuild replays it twice"
    );
}

#[test]
fn a_self_conflict_is_refused_rather_than_self_superseded() {
    let store = Store::new();
    let id = seeded_conflict(
        &store,
        CAROL,
        "One memo says Carol is at Acme, the next line says Beta Corp.",
    );
    // Point both sides of the queue row at the same fact, which is the shape a
    // 0.3.x store could hold.
    let s = store.open();
    let row = s
        .get_fact_conflict(uuid::Uuid::parse_str(&id).unwrap())
        .expect("row")
        .expect("open row");
    s.point_conflict_at_one_fact_for_test(row.id, row.new_fact_id)
        .expect("rewrite");
    drop(s);

    let out = store
        .engram()
        .args([
            "facts",
            "resolve",
            &id,
            "--action",
            "supersede-old",
            "--json",
        ])
        .output()
        .expect("engram runs");
    assert_eq!(out.status.code(), Some(3));
    let body = envelope(&out);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cycle"),
        "a fact cannot supersede itself, and the refusal says why: {body}"
    );
    // And nothing moved: the conflict is still open.
    let s = store.open();
    let after = s
        .get_fact_conflict(uuid::Uuid::parse_str(&id).unwrap())
        .expect("row")
        .expect("row");
    assert_eq!(
        after.status, "open",
        "a refused resolve rolls its claim back"
    );
}

#[test]
fn a_safety_close_keeps_its_permission_and_its_advice_apart() {
    // R7: a caller decision IS `CloseAuthority::Caller`, so `may_close` allows
    // it on a safety slot — permission is not narrowed, only advice. What the
    // surface must never do is RECOMMEND it.
    let store = Store::new();
    let out = store.seed(&[
        "remember",
        "Ana was on warfarin; the new chart says apixaban 5mg.",
        "--kb",
        "personal",
        "--facts-json",
        ANA,
    ]);
    let Some(entry) = out["data"]["adjudication_needed"]
        .as_array()
        .and_then(|a| a.first())
    else {
        // A set-valued safety relation with different members coexists rather
        // than queueing; nothing to adjudicate is the right answer.
        return;
    };
    assert_eq!(
        entry["suggested_action"], "requires_context",
        "a safety pair is never advised to supersede: {entry}"
    );
    let allowed = entry["resolve"]["allowed_actions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        allowed.iter().any(|a| a == "supersede-old"),
        "permission is not narrowed by safety: {entry}"
    );
}

/// The queue row's own key, read straight from storage — the same string the
/// resolve wrote its decision under.
fn decision_key_of(s: &engram_storage::SqliteStore, id: &str) -> String {
    s.get_fact_conflict(uuid::Uuid::parse_str(id).unwrap())
        .expect("row")
        .expect("row")
        .decision_key
        .expect("a wave-3 queue row carries its key")
}
