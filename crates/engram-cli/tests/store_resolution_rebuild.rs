//! [V5] Which rule or judge closed an interval survives `admin rebuild`.
//!
//! `facts.resolution` was written to the projection alone, so a rebuild from
//! the journal erased it: "Lisbon was closed by slot_lww" came back as a close
//! nobody could attribute. The close event now carries the tag.

mod wave3_fact_support;
use serde_json::Value;
use wave3_fact_support::*;

fn lives_in(store: &std::path::Path, content: &str, object: &str) {
    remember_facts(
        store,
        content,
        &format!(r#"[{{"subject":"Ana","predicate":"lives_in","object":"{object}"}}]"#),
    );
}

fn lisbon(store: &std::path::Path) -> Value {
    run_json(
        &mut engram(store),
        &["facts", "list", "--all", "--limit", "100"],
    )["data"]["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["object"] == "Lisbon")
        .cloned()
        .expect("the Lisbon row is still listed")
}

#[test]
fn a_slot_lww_close_keeps_its_resolution_through_admin_rebuild() {
    let tmp = store();
    lives_in(tmp.path(), "Ana lives in Lisbon.", "Lisbon");
    lives_in(tmp.path(), "Ana moved to Porto.", "Porto");

    let before = lisbon(tmp.path());
    assert!(
        !before["valid_until"].is_null(),
        "Porto closed it: {before}"
    );
    assert_eq!(before["resolution"], "slot_lww", "{before}");

    run_json(&mut engram(tmp.path()), &["admin", "rebuild", "--confirm"]);

    let after = lisbon(tmp.path());
    assert_eq!(
        after["resolution"], "slot_lww",
        "rebuild erased it: {after}"
    );
    assert_eq!(after["close_authority"], before["close_authority"]);
    assert_eq!(after["valid_until"], before["valid_until"]);
}
