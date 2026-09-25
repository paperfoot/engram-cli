//! Bytes returned to an agent are a budget, and these are its ceilings.
//!
//! Every surface here is read by agents on every session or every call, and
//! each has grown before without anyone deciding it should: the skill reached
//! 23 KB, the manifest 146 KB of which 15% was indentation. A ceiling turns
//! growth into a decision — raise the number in the same commit and say why.
//!
//! Isolation: every run goes through `Sandbox` (store in a TempDir, provider
//! keys cleared); the recall fixture uses the offline stub embedder.

mod contract_support;

use contract_support::Sandbox;

const KIB: usize = 1024;

fn bytes(sb: &Sandbox, args: &[&str]) -> usize {
    let out = sb.cmd().args(args).output().expect("engram");
    assert!(out.status.success(), "{args:?} failed");
    out.stdout.len()
}

fn within(surface: &str, measured: usize, ceiling: usize) {
    assert!(
        measured <= ceiling,
        "{surface} answers {measured} bytes, over its {ceiling}-byte ceiling"
    );
}

#[test]
fn the_command_index_fits_on_a_terminal() {
    within(
        "engram --help",
        bytes(&Sandbox::new(), &["--help"]),
        3 * KIB,
    );
}

/// The skill loads whenever the agent touches memory; the description in its
/// front matter loads in every session. 0.4.1's was 23,086 bytes.
#[test]
fn the_skill_is_a_signpost() {
    let skill = include_str!("../assets/skill.md");
    within("assets/skill.md", skill.len(), 3 * KIB);
}

/// The plan estimated 7 KB; summaries of about 70 commands measured 10.2 KB
/// with the defaults left out. Shrinking it means shorter summaries.
#[test]
fn the_index_stays_an_index() {
    within(
        "agent-info --index",
        bytes(&Sandbox::new(), &["agent-info", "--index"]),
        11 * KIB,
    );
}

/// recall is the most-pulled command and the one error recovery points at:
/// `every_parser_suggestion_runs_and_stays_small` allows the same 12 KiB.
#[test]
fn one_command_costs_a_fraction_of_the_manifest() {
    within(
        "agent-info --command recall",
        bytes(&Sandbox::new(), &["agent-info", "--command", "recall"]),
        12 * KIB,
    );
}

/// 146,595 bytes at 0.4.1; the transcriptions that made every command
/// pullable brought it to about 168 KB. It shrinks when the hand-written
/// prose moves into the registry (plan B1/C2), not before.
#[test]
fn the_full_manifest_does_not_grow_back() {
    within(
        "agent-info",
        bytes(&Sandbox::new(), &["agent-info"]),
        168 * KIB,
    );
}

/// The baseline response v2 has to beat: twelve short memories, ten hits,
/// offline. 10,002 bytes when recorded, about 3.8k tokens — most of it the
/// same text twice (answer_context and results[].content) plus provenance.
#[test]
fn a_ten_hit_recall_stays_under_its_recorded_size() {
    let sb = Sandbox::new();
    for memory in [
        "Sarah prefers async standups; sync only for incidents.",
        "Chose Postgres over DynamoDB because relational reporting mattered more.",
        "Mum's birthday is 14 March.",
        "The pricing model is annual only, decided in March.",
        "Bruno joined Acme as head of sales.",
        "Ana moved to Lisbon last spring.",
        "Weekly standup moved to Tuesday mornings.",
        "Competitor Foo raised prices by 20 percent.",
        "The team decided to drop the free tier.",
        "Sarah's kids are called Tom and Lily.",
        "Board meeting scheduled for the first Monday of each quarter.",
        "Pricing page redesign ships next sprint.",
    ] {
        let args = [
            "remember",
            memory,
            "--kb",
            "personal",
            "--offline",
            "--no-extract-facts",
        ];
        bytes(&sb, &args);
    }
    let out = sb
        .cmd()
        .args([
            "recall",
            "what did we decide about pricing and standups",
            "--profile",
            "offline",
            "--top-k",
            "10",
        ])
        .output()
        .expect("recall");
    assert!(out.status.success());
    let v = Sandbox::json(&out);
    assert_eq!(v["data"]["results"].as_array().map(Vec::len), Some(10));
    within("recall (12 memories, 10 hits)", out.stdout.len(), 10 * KIB);
}
