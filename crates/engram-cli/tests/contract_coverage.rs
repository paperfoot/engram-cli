//! Discovery matches execution, checked in both directions (gate-4 H6).
//!
//! The parser is the ground truth for what exists. Every leaf clap will run
//! must either answer `agent-info --schema` or be declared
//! `schema_available: false` in the contract; every path and alias the
//! contract publishes must be one clap parses. Without the walk, a command
//! added to `cli.rs` is simply invisible to discovery, and one advertised in
//! the manifest without a schema is found by an agent's exit 3.
//!
//! Isolation: the walk is in-process; every binary run goes through
//! `Sandbox`, which roots the store in a TempDir and clears provider keys.

mod contract_support;

use clap::CommandFactory;
use contract_support::{manifest, Sandbox};
use engram_cli::cli::Cli;

/// Every non-hidden leaf clap will dispatch, as a space-separated canonical
/// path (`"facts resolve"`).
fn clap_leaves() -> Vec<String> {
    fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            if sub.is_hide_set() || sub.get_name() == "help" {
                continue;
            }
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            if sub.has_subcommands() {
                walk(sub, &path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&Cli::command(), "", &mut out);
    out
}

/// The canonical leaf clap resolves `path` to, following aliases at every
/// level (`memory find` is `memory search`), or `None` if clap would reject
/// it or it names a group rather than a runnable command.
fn clap_resolve(path: &str) -> Option<String> {
    let root = Cli::command();
    let mut cmd = &root;
    let mut canonical = Vec::new();
    for token in path.split_whitespace() {
        cmd = cmd.find_subcommand(token)?;
        canonical.push(cmd.get_name().to_string());
    }
    (!cmd.has_subcommands() && !canonical.is_empty()).then(|| canonical.join(" "))
}

fn described(path: &str) -> bool {
    engram_contract::command(path).is_some()
}

fn declared_unavailable(path: &str) -> bool {
    engram_contract::unavailable(path).is_some()
}

#[test]
fn every_clap_leaf_has_a_contract_entry_or_a_declared_gap() {
    let leaves = clap_leaves();
    assert!(leaves.len() > 50, "the walk found only {leaves:?}");
    let missing: Vec<&String> = leaves
        .iter()
        .filter(|p| !described(p) && !declared_unavailable(p))
        .collect();
    assert!(
        missing.is_empty(),
        "clap runs these and discovery has never heard of them: {missing:?}"
    );
}

/// The long flags clap accepts on `path`, global ones (`--json`, `--store`,
/// ...) and hidden ones excluded, sorted.
fn clap_flags(path: &str) -> Vec<String> {
    let root = Cli::command();
    let globals: Vec<&str> = root.get_arguments().filter_map(|a| a.get_long()).collect();
    let mut cmd = &root;
    for token in path.split_whitespace() {
        cmd = cmd.find_subcommand(token).expect("a clap leaf");
    }
    let mut out: Vec<String> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set())
        .filter_map(|a| a.get_long())
        .filter(|l| !globals.contains(l) && *l != "help")
        .map(str::to_string)
        .collect();
    out.sort();
    out
}

/// Every flag a described command parses is in its contract, and every flag
/// the contract lists parses. The leaf walk above cannot see a flag added to
/// an existing command: `ingest --offline` and `chunks repair --offline`
/// changed what a keyless run does and reached the manifest only by hand.
#[test]
fn every_described_command_lists_exactly_the_flags_clap_parses() {
    let mut drift = Vec::new();
    for spec in engram_contract::contract().commands {
        let mut documented: Vec<String> =
            spec.flags.iter().map(|f| f.cli_long.to_string()).collect();
        documented.sort();
        let parsed = clap_flags(spec.path);
        if documented != parsed {
            drift.push(format!(
                "{}: contract {documented:?}, clap {parsed:?}",
                spec.path
            ));
        }
    }
    assert!(drift.is_empty(), "flag drift:\n{}", drift.join("\n"));
}

#[test]
fn every_contract_path_and_alias_parses_in_clap() {
    let c = engram_contract::contract();
    for spec in c.commands {
        for path in std::iter::once(&spec.path).chain(spec.aliases) {
            assert!(
                clap_resolve(path).is_some(),
                "the contract publishes `{path}` ({}) and clap does not run it",
                spec.path
            );
        }
    }
    for u in c.schema_unavailable {
        assert_eq!(clap_resolve(u.path).as_deref(), Some(u.path), "{}", u.path);
        assert!(!described(u.path), "{} is both described and not", u.path);
    }
}

/// Executed, not sampled: every leaf that claims a schema serves it from the
/// binary, and every declared gap answers with a runnable `--help` instead.
#[test]
fn every_leaf_answers_its_schema_path_or_says_why_not() {
    let sb = Sandbox::new();
    for leaf in clap_leaves() {
        let out = sb
            .cmd()
            .args(["agent-info", "--schema", &leaf])
            .output()
            .expect("schema");
        if declared_unavailable(&leaf) {
            assert_eq!(out.status.code(), Some(3), "{leaf}");
            let err = Sandbox::err_json(&out);
            assert!(
                err["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("schema_available: false"),
                "{leaf}: {err}"
            );
            let suggestion = err["error"]["suggestion"].as_str().unwrap();
            assert!(
                suggestion.contains(&format!("engram {leaf} --help")),
                "{suggestion}"
            );
            let help = sb
                .cmd()
                .args(leaf.split_whitespace())
                .arg("--help")
                .output()
                .unwrap();
            assert!(help.status.success(), "{leaf} --help");
        } else {
            assert!(
                out.status.success(),
                "agent-info --schema '{leaf}' failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    assert!(
        std::fs::read_dir(sb.store_path()).unwrap().next().is_none(),
        "a schema lookup created a store"
    );
}

/// An agent reading `command_schemas` will pull what it sees there. Every key
/// must pull, and the declared gaps must be advertised as gaps.
#[test]
fn the_manifest_advertises_no_command_whose_pull_fails() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    for key in m["command_schemas"].as_object().unwrap().keys() {
        let out = sb
            .cmd()
            .args(["agent-info", "--schema", key])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the manifest advertises `{key}`, whose --schema fails"
        );
    }
    let gaps: Vec<&str> = m["schema_unavailable"]
        .as_array()
        .expect("schema_unavailable is published")
        .iter()
        .map(|u| u["path"].as_str().unwrap())
        .collect();
    for u in engram_contract::contract().schema_unavailable {
        assert!(
            gaps.contains(&u.path),
            "{} is not advertised as a gap",
            u.path
        );
    }
}

/// `agent-info --index` is the map an agent chooses from, so it must be the
/// map clap runs: every leaf is an entry or an entry's alias, and every entry
/// is a leaf.
#[test]
fn the_index_lists_exactly_the_clap_leaves() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["agent-info", "--index"])
        .output()
        .expect("index");
    assert!(out.status.success());
    let index = Sandbox::json(&out)["commands_index"].clone();
    let index = index.as_object().expect("commands_index");
    for leaf in clap_leaves() {
        let listed = index.contains_key(&leaf)
            || index.values().any(|e| {
                e["aliases"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x == leaf.as_str()))
            });
        assert!(listed, "the index does not list `{leaf}`");
    }
    for path in index.keys() {
        assert_eq!(clap_resolve(path).as_deref(), Some(path.as_str()), "{path}");
    }
    assert!(std::fs::read_dir(sb.store_path()).unwrap().next().is_none());
}
