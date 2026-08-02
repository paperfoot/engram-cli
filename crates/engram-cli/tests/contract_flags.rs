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
///
/// The check is the PARSER, not the help text. clap distinguishes visible
/// aliases from hidden ones, and `--no-facts` is hidden on both `remember` and
/// `memory add` — a help-text grep passed on `remember` only because its doc
/// comment happens to name the older spelling in prose, which is a coincidence
/// and not a contract. Feeding the alias to the real binary and demanding that
/// it not come back as an unknown argument proves the thing agents care about.
#[test]
fn every_documented_alias_is_accepted_by_the_binary() {
    let sb = Sandbox::new();
    for c in engram_contract::contract().commands {
        for f in c.flags {
            for alias in f.aliases {
                let out = sb
                    .cmd()
                    .args(c.path.split_whitespace())
                    .arg(format!("--{alias}"))
                    .output()
                    .expect("spawn");
                // Nothing here supplies the command's required arguments, so a
                // zero exit would mean the invocation RAN — which no alias
                // probe should ever do.
                assert_ne!(
                    out.status.code(),
                    Some(0),
                    "{} --{alias} ran instead of failing on its missing arguments",
                    c.path
                );
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    !stderr.contains("unexpected argument") && !stderr.contains("unknown argument"),
                    "{} documents --{alias} for --{}, and the binary rejects it: {stderr}",
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
