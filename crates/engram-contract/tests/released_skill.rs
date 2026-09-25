//! The upgrade the body hash exists for: a SKILL.md exactly as a previous
//! release installed it. The fixture is 0.4.1's asset byte for byte, as
//! published on crates.io.

use engram_contract::skill::{freshness, is_pristine, Freshness};

const V0_4_1: &str = include_str!("fixtures/skill-0.4.1.md");

#[test]
fn the_published_0_4_1_skill_is_an_untouched_copy_and_reads_stale() {
    assert!(is_pristine(V0_4_1));
    assert_eq!(
        freshness(Some(V0_4_1)),
        Freshness::Stale {
            installed: Some("6358c34071a05c5da2d47ad0576c2bc2".into())
        }
    );
}

#[test]
fn one_edited_line_in_it_is_somebodys_file() {
    let edited = V0_4_1.replacen("# engram", "# engram (my notes)", 1);
    assert!(!is_pristine(&edited));
    assert_eq!(freshness(Some(&edited)), Freshness::Modified);
}
