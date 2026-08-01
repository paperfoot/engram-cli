//! `engram-contract-gen` — regenerates the checked-in agent artefacts.
//!
//! Run it after editing `registry.rs`:
//!
//! ```text
//! cargo run -p paperfoot-engram-contract --bin engram-contract-gen
//! cargo run -p paperfoot-engram-contract --bin engram-contract-gen -- --check
//! ```
//!
//! `--check` exits 3 on drift with no test harness, because `cargo package`
//! and `cargo publish` do not run tests — and `assets/skill.md` is compiled
//! into the binary by `include_str!`, so a stale asset ships to every user who
//! then installs a skill describing the previous release.

use std::path::PathBuf;

fn skill_asset() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../engram-cli/assets/skill.md")
        .canonicalize()
        .expect("assets/skill.md is where the crate layout says it is")
}

fn main() -> std::process::ExitCode {
    let check = std::env::args().any(|a| a == "--check");
    let path = skill_asset();
    let current = std::fs::read_to_string(&path).expect("read assets/skill.md");
    let wanted = engram_contract::skill::stamp(&current);

    if current == wanted {
        if !check {
            println!("assets/skill.md is already current");
        }
        return std::process::ExitCode::SUCCESS;
    }

    if check {
        eprintln!(
            "assets/skill.md is stale — run `cargo run -p paperfoot-engram-contract --bin engram-contract-gen`"
        );
        return std::process::ExitCode::from(3);
    }

    std::fs::write(&path, &wanted).expect("write assets/skill.md");
    println!(
        "assets/skill.md regenerated (schema_hash {})",
        engram_contract::schema_hash()
    );
    std::process::ExitCode::SUCCESS
}
