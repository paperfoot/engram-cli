//! The generated artefact is committed, and a test asserts its freshness.
//!
//! Without this the failure is silent and slow: someone edits `registry.rs`,
//! `cargo build` succeeds (the generator is a separate binary, nothing forces
//! it to run), they publish, and every user installs a skill describing the
//! previous release.

const ASSET: &str = include_str!("../assets/skill.md");

#[test]
fn the_skill_asset_is_freshly_generated() {
    assert_eq!(
        engram_contract::skill::stamp(ASSET),
        ASSET,
        "assets/skill.md is stale — run `cargo run -p paperfoot-engram-contract --bin engram-contract-gen`"
    );
}

#[test]
fn the_skill_asset_reports_this_builds_hash() {
    assert_eq!(
        engram_contract::skill::parse_schema_hash(ASSET),
        Some(engram_contract::schema_hash())
    );
    assert_eq!(
        engram_contract::skill::freshness(Some(ASSET)),
        engram_contract::skill::Freshness::Fresh
    );
}

/// The asset ships inside the binary via `include_str!`, so a stale one is a
/// stale *release*, not just a stale working copy.
#[test]
fn an_installed_copy_of_a_previous_release_reads_as_stale() {
    let previous = ASSET.replace(
        &engram_contract::schema_hash(),
        "00000000000000000000000000000000",
    );
    match engram_contract::skill::freshness(Some(&previous)) {
        engram_contract::skill::Freshness::Stale { installed } => {
            assert_eq!(
                installed.as_deref(),
                Some("00000000000000000000000000000000")
            )
        }
        other => panic!("expected stale, got {other:?}"),
    }
}
