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
