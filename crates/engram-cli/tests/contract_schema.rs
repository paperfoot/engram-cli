//! `engram agent-info --schema '<command>'` — the invocation three separate
//! surfaces already told agents to run.
//!
//! The parser's unknown-flag suggestion, `CliError::BadInput` and
//! `MalformedJson` all advertise it, and until the release gate tried it the
//! subcommand took no such flag: the advice exited 3 `bad_input` and repeated
//! itself, which is the worst shape an error can have — a loop with a
//! confident tone. The generator was there the whole time; only the door was
//! missing.
//!
//! Every schema path is executed here, not sampled. A schema an agent cannot
//! fetch is a schema that does not exist, however well it renders in a unit
//! test.

mod contract_support;

use contract_support::{manifest, Sandbox};

fn store_is_untouched(sb: &Sandbox) -> bool {
    std::fs::read_dir(sb.store_path())
        .expect("store dir")
        .next()
        .is_none()
}

/// Every registered command, one process each (`contract_coverage.rs` holds
/// the same line for every clap leaf, including the declared gaps). The generator and the binary must
/// agree byte for byte, or `--schema` is a second, quietly diverging source.
#[test]
fn every_registered_command_answers_its_own_schema_path() {
    let sb = Sandbox::new();
    for c in engram_contract::contract().commands {
        let out = sb
            .cmd()
            .args(["agent-info", "--schema", c.path])
            .output()
            .expect("schema");
        assert!(
            out.status.success(),
            "agent-info --schema '{}' failed: {}",
            c.path,
            String::from_utf8_lossy(&out.stderr)
        );
        let got = Sandbox::json(&out);
        assert_eq!(
            got,
            engram_contract::jsonschema::schema_for(c.path).unwrap(),
            "the binary's schema for `{}` is not the contract's",
            c.path
        );
        assert_eq!(got["title"], format!("engram {}", c.path));
        assert!(got["properties"]["input"].is_object(), "{}", c.path);
        assert!(got["properties"]["output"].is_object(), "{}", c.path);
    }
    assert!(
        store_is_untouched(&sb),
        "a schema lookup created a store — discovery must never migrate"
    );
}

/// Agents pass back whatever name they were given, and `search` is a name we
/// gave them.
#[test]
fn an_alias_answers_with_the_command_it_aliases() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["agent-info", "--schema", "search"])
        .output()
        .expect("schema");
    assert!(out.status.success());
    assert_eq!(Sandbox::json(&out)["title"], "engram recall");
}

/// The dead end this whole feature exists to remove: a wrong command name must
/// hand back the right ones, not repeat the suggestion that just failed.
#[test]
fn an_unknown_command_lists_the_valid_ones() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["agent-info", "--schema", "recal"])
        .output()
        .expect("schema");
    assert_eq!(out.status.code(), Some(3));
    let err = Sandbox::err_json(&out);
    assert_eq!(err["error"]["code"], "bad_input");
    let whole = format!("{} {}", err["error"]["message"], err["error"]["suggestion"]);
    for path in ["recall", "facts resolve", "jobs show"] {
        assert!(
            whole.contains(path),
            "the failure names no valid command ({path} missing): {whole}"
        );
    }
    assert!(store_is_untouched(&sb), "a bad schema name created a store");
}

/// The manifest is where an agent looks first, so the flag has to be there —
/// and `--help` is where a human looks, so it has to be there too.
#[test]
fn the_manifest_and_the_help_both_advertise_the_flag() {
    let sb = Sandbox::new();
    let entry = manifest(&sb)["commands"]["agent-info | info"]
        .as_str()
        .expect("agent-info documents itself")
        .to_string();
    assert!(entry.contains("--schema"), "{entry}");

    let out = sb.cmd().args(["agent-info", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--schema"), "{help}");
}

/// The advice has to be executable, so the test executes it: take the
/// suggestion a real parse failure hands out, and run the thing it advertises.
#[test]
fn the_suggestion_the_parser_hands_out_actually_runs() {
    let sb = Sandbox::new();
    let out = sb.cmd().args(["recall", "--nonsense"]).output().unwrap();
    let err = Sandbox::err_json(&out);
    let suggestion = err["error"]["suggestion"].as_str().unwrap().to_string();
    assert!(suggestion.contains("agent-info --command"), "{suggestion}");
    let followed = sb
        .cmd()
        .args(["agent-info", "--command", "recall"])
        .output()
        .expect("the advertised invocation");
    assert!(
        followed.status.success(),
        "the suggestion sends agents to a command that fails: {}",
        String::from_utf8_lossy(&followed.stderr)
    );
}

/// The loop this file's header describes, closed at its last door: a wrong
/// name passed to `--schema` used to get the generic `bad_input` advice —
/// "run `engram agent-info --schema '<command>'`" — the call that had just
/// failed. The suggestion must name a different call, and that call must run.
#[test]
fn an_unknown_schema_path_does_not_suggest_itself() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["agent-info", "--schema", "ingest-all"])
        .output()
        .expect("schema");
    assert_eq!(out.status.code(), Some(3));
    let err = Sandbox::err_json(&out);
    assert_eq!(err["error"]["code"], "bad_input");
    let suggestion = err["error"]["suggestion"].as_str().unwrap();
    assert!(!suggestion.contains("--schema"), "{suggestion}");
    assert!(suggestion.contains("engram --help"), "{suggestion}");
    let followed = sb.cmd().arg("--help").output().unwrap();
    assert!(followed.status.success());
    assert!(store_is_untouched(&sb));
}

/// Recovery is executable and cheap: every discovery command an error hands
/// out runs, and none of them is the full manifest. A suggestion that sends an
/// agent to a 39k-token read after a typo costs more than the typo.
#[test]
fn every_parser_suggestion_runs_and_stays_small() {
    let sb = Sandbox::new();
    // The followed calls run in their own sandbox: `repair` opens the store
    // before it refuses, but the discovery calls it points at must not.
    let probe = Sandbox::new();
    // (argv that fails, the command an agent would substitute for <command>)
    let cases: &[(&[&str], &str)] = &[
        (&["recal", "x"], "recall"),
        (&["recall", "x", "--nonsense"], "recall"),
        (&["recall", "x", "--layer", "bogus"], "recall"),
        (&["recall"], "recall"),
        (&["repair"], "repair"),
        (&["agent-info", "--schema", "bogus"], "recall"),
        (&["agent-info", "--command", "bogus"], "recall"),
    ];
    for (argv, command) in cases {
        let out = sb.cmd().args(*argv).output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{argv:?}");
        let suggestion = Sandbox::err_json(&out)["error"]["suggestion"]
            .as_str()
            .unwrap()
            .to_string();
        let runnable: Vec<Vec<String>> = suggestion
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|c| {
                c.starts_with("engram ") && (c.contains("--help") || c.contains("agent-info"))
            })
            .map(|c| {
                c.replace("'<command>'", command)
                    .replace("<command>", command)
                    .split_whitespace()
                    .skip(1)
                    .map(String::from)
                    .collect()
            })
            .collect();
        assert!(
            !runnable.is_empty(),
            "{argv:?} suggests nothing runnable: {suggestion}"
        );
        for followed in runnable {
            let out = probe.cmd().args(&followed).output().unwrap();
            assert!(out.status.success(), "{argv:?} -> {followed:?} fails");
            assert!(
                out.stdout.len() < 12 * 1024,
                "{argv:?} -> {followed:?} answers {} bytes",
                out.stdout.len()
            );
        }
    }
    assert!(store_is_untouched(&probe));
}
