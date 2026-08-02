//! The Breaking list is a contract, not a courtesy.
//!
//! `error.code` is a frozen wire value — scripts branch on it — so a release
//! that changes what a command answers owes its readers a line saying so. The
//! release gate found three such changes shipping undeclared (`edit`, `facts
//! resolve` and `jobs show` now answer `not_found` where 0.3.1 answered
//! `bad_input`), and the reason nobody noticed is that every existing contract
//! test pins the NEW taxonomy against the CURRENT binary. Agreement with
//! itself is not completeness.
//!
//! So this file holds the table the gate wrote, and asserts three things about
//! every row: the binary really answers the new code, the code is published in
//! the manifest's error mapping, and the CHANGELOG's Breaking section carries a
//! bullet naming that command AND that code. Adding a fourth wire change means
//! adding a row here, and a row with no CHANGELOG bullet fails the build.

mod contract_support;

use contract_support::{manifest, Sandbox};

/// A uuid shaped correctly and belonging to nothing — the gate's own probe.
const ABSENT: &str = "11111111-1111-4111-8111-111111111111";

struct WireChange {
    /// How the CHANGELOG and the contract spell the command.
    command: &'static str,
    /// The invocation, minus `--json`, which every case gets.
    argv: &'static [&'static str],
    /// What 0.3.1 answered, kept beside the new value so the row reads as a
    /// diff rather than as a restatement of today's code.
    before: &'static str,
    after: &'static str,
}

const CHANGED_WIRE_ERRORS: &[WireChange] = &[
    WireChange {
        command: "edit",
        argv: &["edit", ABSENT, "--importance", "3"],
        before: "bad_input",
        after: "not_found",
    },
    WireChange {
        command: "facts resolve",
        argv: &["facts", "resolve", ABSENT, "--action", "keep-both"],
        before: "bad_input",
        after: "not_found",
    },
    WireChange {
        command: "jobs show",
        argv: &["jobs", "show", ABSENT],
        before: "bad_input",
        after: "not_found",
    },
    WireChange {
        command: "forget",
        argv: &["forget", ABSENT, "--confirm"],
        before: "exit 0 with deleted:false",
        after: "not_found",
    },
    WireChange {
        command: "clap parse",
        argv: &["recall", "--nonsense"],
        before: "prose on exit 2",
        after: "bad_input",
    },
];

/// The row must describe the binary, or the CHANGELOG it drives is fiction.
#[test]
fn every_declared_wire_change_is_what_the_binary_answers() {
    let sb = Sandbox::new();
    for w in CHANGED_WIRE_ERRORS {
        let out = sb
            .cmd()
            .args(w.argv)
            .arg("--json")
            .output()
            .expect("invocation");
        assert_eq!(out.status.code(), Some(3), "{} exited wrong", w.command);
        let err = Sandbox::err_json(&out);
        assert_eq!(
            err["error"]["code"], w.after,
            "{} answers something other than the declared `{}` (0.3.1 said `{}`)",
            w.command, w.after, w.before
        );
    }
}

/// An agent reading the manifest must find the code it is about to receive.
#[test]
fn every_changed_code_is_published_in_the_error_mapping() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    let mapping = m["daemon_api"]["error_mapping"]
        .as_str()
        .expect("the manifest maps error codes to HTTP statuses");
    let documented: Vec<String> = m["error_codes"]
        .as_array()
        .expect("error_codes")
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect();
    for w in CHANGED_WIRE_ERRORS {
        assert!(
            mapping.contains(w.after),
            "{} now answers `{}` and the error mapping never names it",
            w.command,
            w.after
        );
        assert!(
            documented.iter().any(|d| d == w.after),
            "`{}` is not in the published error_codes table",
            w.after
        );
    }
}

/// The completeness check the gate asked for: a wire change with no line in
/// the Breaking section is an undeclared break, whatever the code does.
#[test]
fn every_changed_command_has_a_breaking_bullet_naming_its_new_code() {
    let bullets = breaking_bullets();
    for w in CHANGED_WIRE_ERRORS {
        let command = w.command.to_ascii_lowercase();
        assert!(
            bullets
                .iter()
                .any(|b| b.contains(&command) && b.contains(w.after)),
            "the Breaking section has no bullet naming both `{}` and `{}` — \
             this release changes that command's wire error and does not say so",
            w.command,
            w.after
        );
    }
}

/// The Breaking bullets of the topmost release, lowercased. Parsed rather than
/// grepped whole so a bullet about `edit --content` cannot satisfy a claim
/// about `edit`'s error code by accident.
fn breaking_bullets() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md");
    let text = std::fs::read_to_string(&path).expect("CHANGELOG.md is beside the workspace root");
    let start = text
        .find("### Breaking")
        .expect("the release documents its breaking changes");
    let section = &text[start..];
    // The section ends at the next heading of any level; searching from byte 1
    // so the section's own heading does not terminate it.
    let end = ["\n### ", "\n## "]
        .iter()
        .filter_map(|h| section[1..].find(h).map(|i| i + 1))
        .min()
        .unwrap_or(section.len());
    section[..end]
        .split("\n- ")
        .skip(1)
        .map(|b| b.to_ascii_lowercase())
        .collect()
}

/// The parser has to be able to fail: a helper that returns one giant blob
/// would make the test above pass for any CHANGELOG that mentions the words.
#[test]
fn the_breaking_section_parses_into_separate_bullets() {
    let bullets = breaking_bullets();
    assert!(bullets.len() > 3, "{bullets:#?}");
    assert!(
        bullets.iter().all(|b| !b.contains("\n### ")),
        "a bullet swallowed the next section"
    );
}
