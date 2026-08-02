//! The store-free surface. Discovery and misuse must never be the call that
//! creates or migrates a database — asserted by pointing `ENGRAM_STORE` at an
//! empty TempDir and proving it is still empty afterwards.

mod contract_support;

use contract_support::{manifest, Sandbox};

fn store_is_untouched(sb: &Sandbox) -> bool {
    std::fs::read_dir(sb.store_path())
        .expect("store dir")
        .next()
        .is_none()
}

#[test]
fn agent_info_opens_no_store() {
    let sb = Sandbox::new();
    let out = sb.cmd().arg("agent-info").output().unwrap();
    assert!(out.status.success());
    assert!(store_is_untouched(&sb), "agent-info created a store");
}

#[test]
fn help_and_version_open_no_store() {
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["--version"],
        vec!["remember", "--help"],
    ] {
        let sb = Sandbox::new();
        let out = sb.cmd().args(&args).output().unwrap();
        assert!(out.status.success(), "{args:?} failed");
        assert!(out.stderr.is_empty(), "{args:?} wrote to stderr");
        assert!(store_is_untouched(&sb), "{args:?} created a store");
    }
}

/// Three things must agree about which release this is, and until the gate
/// caught it they did not: the binary shipped 0.3.1 while the CHANGELOG's top
/// entry described 0.4.0. "The version command succeeded" is not a version
/// test — an agent asked to report which engram it is talking to needs the
/// answer to be true, and a store migrated by a binary that misreports its own
/// version is a support case nobody can reconstruct.
#[test]
fn the_binary_the_manifest_and_the_changelog_name_the_same_release() {
    let sb = Sandbox::new();
    let pkg = env!("CARGO_PKG_VERSION");

    let out = sb.cmd().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("engram {pkg}"),
        "`engram --version` disagrees with the package version"
    );

    assert_eq!(
        manifest(&sb)["version"],
        pkg,
        "agent-info reports a different version than the binary was built as"
    );

    assert_eq!(
        top_changelog_version(),
        pkg,
        "the CHANGELOG's newest entry is not the version this binary claims"
    );
}

/// The version in the first `## ` heading of the CHANGELOG, e.g. `0.4.0` from
/// `## 0.4.0 — UNRELEASED (branch arch/v0.4)`.
fn top_changelog_version() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md");
    let text = std::fs::read_to_string(&path).expect("CHANGELOG.md");
    let heading = text
        .lines()
        .find(|l| l.starts_with("## "))
        .expect("the CHANGELOG has a release heading");
    heading
        .trim_start_matches("## ")
        .split_whitespace()
        .next()
        .expect("the heading names a version")
        .to_string()
}

/// A typo must not create a database, and must not exit 2 — our own table
/// reads 2 as "your setup is broken", which sent agents to `config check`
/// for a mistyped flag.
#[test]
fn a_clap_error_is_json_exit_3_and_opens_no_store() {
    let sb = Sandbox::new();
    let out = sb.cmd().args(["recall", "--nonsense"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(store_is_untouched(&sb), "a clap error created a store");
    let err = Sandbox::err_json(&out);
    assert_eq!(err["status"], "error");
    assert_eq!(err["error"]["code"], "bad_input");
    assert_eq!(err["error"]["exit_code"], 3);
    let suggestion = err["error"]["suggestion"].as_str().unwrap();
    assert!(
        suggestion.contains("--help") || suggestion.contains("agent-info"),
        "{suggestion}"
    );
}

#[test]
fn a_mistyped_subcommand_names_the_one_it_meant() {
    let sb = Sandbox::new();
    let out = sb.cmd().args(["recal", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err = Sandbox::err_json(&out);
    assert!(
        err["error"]["suggestion"]
            .as_str()
            .unwrap()
            .contains("recall"),
        "{err}"
    );
}

/// The human path is proven, not inherited from however CI wires stderr.
#[test]
fn a_clap_error_on_a_tty_is_prose() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .env("ENGRAM_FORCE_TTY", "1")
        .args(["recall", "--nonsense"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        serde_json::from_slice::<serde_json::Value>(&out.stderr).is_err(),
        "the human branch emitted JSON: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Framework principle 1: the discovery surface is raw JSON, never enveloped.
#[test]
fn agent_info_is_raw_json_not_enveloped() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    assert!(m["name"].is_string());
    assert!(m["commands"].is_object());
    assert!(
        !(m.get("version").is_some() && m.get("status").is_some() && m.get("data").is_some()),
        "agent-info is wrapped in the success envelope"
    );
}
