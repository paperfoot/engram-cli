//! Flag vocabulary, proven against the binary.
//!
//! The rule the contract publishes: **`--top-k` means "how many ranked
//! results"; `--limit` means "how many rows"**. Ranked commands take
//! `--top-k`, enumerating commands take `--limit`, and the alias runs one way
//! only — `--limit` is accepted on ranked commands because that is the mistake
//! agents actually make, while `--top-k` on a listing would promise an
//! ordering that does not exist.

mod contract_support;

use contract_support::Sandbox;

/// Every alias the contract advertises is one the binary accepts. A
/// documented flag that does not exist is worse than an undocumented one: an
/// agent will use it, and get exit 3 from the tool that told it to.
#[test]
fn every_documented_alias_is_accepted_by_the_binary() {
    let sb = Sandbox::new();
    for c in engram_contract::contract().commands {
        let out = sb
            .cmd()
            .args(c.path.split_whitespace())
            .arg("--help")
            .output()
            .expect("help");
        let help = String::from_utf8_lossy(&out.stdout);
        for f in c.flags {
            for alias in f.aliases {
                assert!(
                    help.contains(&format!("--{alias}")),
                    "{} documents --{alias} for --{}, and the binary has no such flag",
                    c.path,
                    f.cli_long
                );
            }
        }
    }
}

/// `--top-k` on an enumerating listing is rejected rather than silently
/// accepted, and the error is bad input (exit 3), not a crash.
#[test]
fn top_k_is_rejected_on_a_listing() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["facts", "list", "--top-k", "3"])
        .output()
        .expect("facts list");
    assert_eq!(out.status.code(), Some(3));
    let err = Sandbox::err_json(&out);
    assert_eq!(err["error"]["code"], "bad_input");
}
