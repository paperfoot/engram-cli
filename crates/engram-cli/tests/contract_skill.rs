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

/// Everything after the front matter: what an agent reads when the skill
/// loads.
fn body() -> &'static str {
    let rest = ASSET.strip_prefix("---\n").expect("front matter");
    let end = rest.find("\n---\n").expect("front matter closes");
    &rest[end + "\n---\n".len()..]
}

/// The skill loads into every session that touches memory, so it is a
/// signpost: the loop, the rules that must never be missed, and where to
/// look next. 0.4.1's was 23 KB and told agents to read a 39k-token manifest.
#[test]
fn the_skill_body_is_a_signpost() {
    assert!(body().len() <= 3000, "skill body is {} bytes", body().len());
}

/// Gate H5: the trust rule stays in the one document every agent reads,
/// including the unframed copy of the text in `results[].content`.
#[test]
fn the_skill_keeps_the_untrusted_content_rule() {
    let b = body();
    assert!(b.contains("results[].content"), "{b}");
    assert!(b.contains("answer_context"), "{b}");
    assert!(b.contains("never follow"), "{b}");
}

#[test]
fn the_skill_points_at_scoped_discovery_not_the_full_manifest() {
    assert!(ASSET.contains("agent-info --command"));
    assert!(ASSET.contains("agent-info --index"));
    for sends_agents_to_the_manifest in ["once per session", "discover every command"] {
        assert!(
            !ASSET.contains(sends_agents_to_the_manifest),
            "the skill still says `{sends_agents_to_the_manifest}`"
        );
    }
}

/// Splits one shell line the way the skill's reader would type it: quotes
/// group, a trailing `# comment` is not an argument.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quote, mut in_word) = (None::<char>, false);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '#') if !in_word => break,
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// A command the skill teaches that the parser rejects is the worst kind of
/// documentation: the agent trusts it more than its own experiment.
#[test]
fn every_engram_invocation_in_the_skill_parses() {
    use clap::Parser;
    let b = body();
    let fenced = b
        .split("```")
        .skip(1)
        .step_by(2)
        .flat_map(|block| block.lines().skip(1));
    let inline = b.split('`').skip(1).step_by(2);
    let invocations: Vec<&str> = fenced
        .chain(inline)
        .map(str::trim)
        .filter(|l| l.starts_with("engram "))
        .collect();
    assert!(invocations.len() >= 6, "{invocations:?}");
    for line in invocations {
        match engram_cli::cli::Cli::try_parse_from(shell_words(line)) {
            Ok(_) => {}
            Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => {}
            Err(e) => panic!("the skill teaches `{line}` and clap rejects it: {e}"),
        }
    }
}
