//! The shared harness for every contract test.
//!
//! Isolation is non-negotiable. `ENGRAM_STORE` roots db, vectors, config and
//! cache inside a TempDir, so a contract test can neither read nor migrate the
//! developer's real store — the failure mode that once bricked an installed
//! binary. No test in this lane sets `HOME` or any `XDG_*` variable, and a
//! test that omits `ENGRAM_STORE` is a reject.
//!
//! A subdirectory rather than a bare file, so cargo does not compile it as its
//! own (empty) test target.

#![allow(dead_code)]

use assert_cmd::Command;
use std::process::Output;
use tempfile::TempDir;

pub struct Sandbox {
    pub tmp: TempDir,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        Sandbox {
            tmp: TempDir::new().expect("tempdir"),
        }
    }

    /// `engram`, isolated, with every provider key CLEARED — so no contract
    /// test can make a paid call or pass only on the machine of whoever has
    /// the right keys exported.
    pub fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("engram").expect("engram binary");
        c.env("ENGRAM_STORE", self.tmp.path())
            .env_remove("GEMINI_API_KEY")
            .env_remove("GEMINI_API_KEY_FALLBACKS")
            .env_remove("COHERE_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("ENGRAM_JUDGE_LOG")
            .env_remove("TYPESAFE_API_KEY")
            .env_remove("ENGRAM_JUDGE_STUB")
            .env_remove("ENGRAM_JUDGE_GROUNDING")
            .env_remove("ENGRAM_JUDGE_ADJUDICATE")
            .env_remove("ENGRAM_JUDGE_DEDUPE")
            .env_remove("ENGRAM_JUDGE_RELEVANCE")
            .env_remove("ENGRAM_FORCE_TTY");
        c
    }

    pub fn store_path(&self) -> &std::path::Path {
        self.tmp.path()
    }

    /// Panics with the raw bytes on failure: a contract test that says
    /// "expected object" and hides the output is useless.
    pub fn json(out: &Output) -> serde_json::Value {
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}):\n{}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    pub fn err_json(out: &Output) -> serde_json::Value {
        serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
            panic!(
                "stderr is not JSON ({e}):\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}

/// The manifest, parsed from the built binary.
pub fn manifest(sb: &Sandbox) -> serde_json::Value {
    let out = sb.cmd().arg("agent-info").output().expect("agent-info");
    assert!(out.status.success(), "agent-info failed");
    Sandbox::json(&out)
}
