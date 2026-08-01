//! Documentation against the binary. Every assertion here is the kind that
//! used to be caught by a human reading two files side by side, which is to
//! say: not caught.

mod contract_support;

use contract_support::{manifest, Sandbox};

/// The round trip the whole crate exists for: the contract is the source, the
/// generator renders it, and the SHIPPED BINARY emits exactly that. A default
/// changed in `cli.rs` but not in the registry fails here rather than in an
/// agent's retry loop.
#[test]
fn the_manifest_entry_for_recall_is_the_generated_one() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    let generated = engram_contract::agent_info::render_command("recall").unwrap();
    assert_eq!(
        m["command_schemas"]["recall"], generated,
        "the binary's recall entry is not the contract's"
    );
}

#[test]
fn the_manifest_carries_the_contract_block() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    assert_eq!(m["contract_version"], "contract@1");
    assert_eq!(m["schema_hash"], engram_contract::schema_hash());
    assert_eq!(
        m["schema_version"],
        engram_contract::contract().schema_version
    );
    assert!(m["vocabularies"]["resolve_action"].is_array());
    assert!(m["error_codes"].is_array());
}

/// The manifest documents the taxonomy the binary actually implements, in
/// both directions — the check that would have caught `error.code` values
/// existing in code and nowhere in the docs.
#[test]
fn every_error_code_is_documented_and_every_documented_code_exists() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    let documented: Vec<String> = m["error_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect();
    for c in engram_core::ErrorCode::ALL {
        assert!(
            documented.iter().any(|d| d == c.as_str()),
            "{} is emitted but undocumented",
            c.as_str()
        );
    }
    for d in &documented {
        assert!(
            engram_core::ErrorCode::ALL.iter().any(|c| c.as_str() == d),
            "{d} is documented but no code emits it"
        );
    }
}

/// Exits are published per code, and they match what the binary will do.
#[test]
fn documented_exits_match_the_taxonomy() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    for e in m["error_codes"].as_array().unwrap() {
        let code = e["code"].as_str().unwrap();
        let want = engram_core::ErrorCode::ALL
            .iter()
            .find(|c| c.as_str() == code)
            .unwrap();
        assert_eq!(e["exit"], want.exit_code(), "{code}");
        assert_eq!(e["retryable"], want.retryable(), "{code}");
    }
}

/// `ENGRAM_STORE` is the one name for the store root. Two names is how a test
/// suite comes to migrate somebody's real store.
#[test]
fn the_manifest_documents_engram_store_and_never_engram_home() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    let whole = serde_json::to_string(&m).unwrap();
    assert!(!whole.contains("ENGRAM_HOME"));
    let store = m["env_vars"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "ENGRAM_STORE")
        .expect("ENGRAM_STORE is documented");
    let doc = store["doc"].as_str().unwrap();
    for word in ["db", "vectors", "config", "cache"] {
        assert!(doc.contains(word), "ENGRAM_STORE doc omits {word}: {doc}");
    }
}

#[test]
fn the_hash_is_stable_across_invocations() {
    let sb = Sandbox::new();
    assert_eq!(manifest(&sb)["schema_hash"], manifest(&sb)["schema_hash"]);
}

/// Documented flags exist, aliases included. `--limit` on `recall` is the
/// mistake agents actually make, so it is documented AND accepted.
#[test]
fn every_documented_recall_flag_exists_in_the_binary() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["recall", "--help"])
        .output()
        .expect("recall --help");
    let help = String::from_utf8_lossy(&out.stdout);
    for f in engram_contract::command("recall").unwrap().flags {
        assert!(
            help.contains(&format!("--{}", f.cli_long)),
            "--{} is documented but absent from `recall --help`",
            f.cli_long
        );
    }
}

/// The tripwire for a store migration landing without the documentation
/// moving with it. Asserts equality with the binary, never against a literal,
/// so it cannot fail the workspace gate on another lane's merge order.
#[test]
fn the_documented_schema_version_matches_the_store() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["doctor", "--json"])
        .output()
        .expect("doctor");
    let v = Sandbox::json(&out);
    let reported = v["data"]["db"]["schema_version"].as_u64();
    if let Some(reported) = reported {
        assert_eq!(
            reported,
            engram_contract::contract().schema_version as u64,
            "the store migrated and the contract did not"
        );
    }
}
