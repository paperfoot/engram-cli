//! Wave 6b — a caller's ruling travels with the VALUE it retired.
//!
//! The semantic key sorts the object pair on purpose: it identifies the
//! QUESTION, so the same contradiction keys the same row however the two
//! values are spelled or ordered. The ledger then recorded only "close_old",
//! which names a side of a pair rather than a value — and a re-derivation that
//! emitted the two employers in the other order mapped that verdict onto the
//! fact the caller had KEPT. The caller retired Acme; the store closed Beta,
//! reported `replayed_caller`, and on a safety pair that authority walks
//! straight through the interval gate [G3].

mod wave3_fact_support;
use wave3_fact_support::*;

const ACME_THEN_BETA: &str = r#"[{"subject":"Carol","predicate":"works_at","object":"Acme"},
                                 {"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]"#;

/// The same two claims, derived in the other order — what `edit --content`,
/// `facts backfill` or a second memory routinely produces.
const BETA_THEN_ACME: &str = r#"[{"subject":"Carol","predicate":"works_at","object":"Beta Corp"},
                                 {"subject":"Carol","predicate":"works_at","object":"Acme"}]"#;

/// Every employer Carol currently has, sorted so the assertion reads as a set.
fn current_employers(store: &std::path::Path) -> Vec<String> {
    let mut objects: Vec<String> = current_facts(store, "Carol")
        .iter()
        .filter_map(|f| f["object"].as_str().map(str::to_string))
        .collect();
    objects.sort();
    objects
}

fn answer_the_question(store: &std::path::Path, envelope: &serde_json::Value, action: &str) {
    let conflict_id = envelope["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap_or_else(|| panic!("a queued question: {envelope}"))
        .to_string();
    run_json(
        &mut engram(store),
        &["facts", "resolve", &conflict_id, "--action", action],
    );
}

/// THE BLOCKER, as the gate reproduced it.
#[test]
fn a_caller_decision_is_not_reversed_by_a_reversed_re_derivation() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "Carol works at Acme and Beta Corp.",
        ACME_THEN_BETA,
    );
    answer_the_question(tmp.path(), &first, "supersede-old");
    assert_eq!(
        current_employers(tmp.path()),
        vec!["Beta Corp"],
        "the caller retired Acme"
    );

    let again = remember_facts(
        tmp.path(),
        "Reversed re-derivation: Beta Corp and Acme.",
        BETA_THEN_ACME,
    );
    assert_eq!(
        current_employers(tmp.path()),
        vec!["Beta Corp"],
        "replaying `close_old` onto whichever row happens to be older reverses \
         the answer the caller gave: {again}"
    );
}

/// The mirror ruling: `reject-new` retires the value that arrived second, and
/// that value stays retired when the pair comes back the other way round.
#[test]
fn a_rejected_value_stays_rejected_when_the_pair_reverses() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "Carol works at Acme and Beta Corp.",
        ACME_THEN_BETA,
    );
    answer_the_question(tmp.path(), &first, "reject-new");
    assert_eq!(
        current_employers(tmp.path()),
        vec!["Acme"],
        "the caller rejected Beta Corp"
    );

    let again = remember_facts(
        tmp.path(),
        "Reversed re-derivation: Beta Corp and Acme.",
        BETA_THEN_ACME,
    );
    assert_eq!(
        current_employers(tmp.path()),
        vec!["Acme"],
        "the rejected value must not walk back in through the other door: {again}"
    );
}
