//! End-to-end CLI integration tests using `assert_cmd`.
//!
//! These tests exercise the binary exactly as an agent would: spawn a fresh
//! process, set ENGRAM_BENCH_FORCE_STUB so no API keys are touched, point
//! XDG paths at a temp dir so nothing leaks between tests, and assert on the
//! JSON envelope shape.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::PathBuf;

fn engram() -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd
}

fn with_isolated_home() -> (tempfile::TempDir, Command) {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let mut cmd = engram();
    // directories crate honors XDG_* on Linux and HOME on macOS.
    cmd.env("HOME", tmp.path());
    cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    (tmp, cmd)
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

#[test]
fn agent_info_returns_raw_manifest_not_enveloped() {
    let mut cmd = engram();
    let output = cmd.arg("agent-info").assert().success();
    let out = output.get_output().stdout.clone();
    let v = parse_stdout(&out);
    // Raw manifest has top-level `name`, not the envelope's `status`.
    assert_eq!(v.get("name").and_then(|v| v.as_str()), Some("engram"));
    assert!(v.get("version").is_some());
    assert!(v.get("commands").is_some());
    assert!(v.get("exit_codes").is_some());
    // MUST NOT be wrapped in envelope.
    assert!(v.get("status").is_none() || v.get("status").and_then(|v| v.as_str()) != Some("success"));
}

#[test]
fn agent_info_alias_info_works() {
    engram().arg("info").assert().success();
}

#[test]
fn version_flag_exits_zero() {
    engram().arg("--version").assert().success();
}

#[test]
fn help_flag_exits_zero() {
    engram().arg("--help").assert().success();
}

#[test]
fn remember_then_recall_roundtrip() {
    let (tmp, mut cmd1) = with_isolated_home();
    cmd1.args(["remember", "Rapamycin extends mouse lifespan via mTORC1 inhibition."])
        .arg("--json")
        .assert()
        .success();

    let mut cmd2 = engram();
    cmd2.env("HOME", tmp.path());
    cmd2.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd2.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd2.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out = cmd2
        .args(["recall", "rapamycin lifespan"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    let results = v["data"]["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    assert!(results[0]["content"]
        .as_str()
        .unwrap()
        .contains("Rapamycin"));
}

#[test]
fn recall_empty_query_is_bad_input() {
    let out = engram()
        .args(["recall", ""])
        .arg("--json")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    // Error goes to stderr as JSON.
    let line = String::from_utf8_lossy(&out)
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .map(|s| s.to_string())
        .expect("JSON error on stderr");
    let v: Value = serde_json::from_str(&line).expect("parseable");
    assert_eq!(v["status"], "error");
    assert_eq!(v["error"]["exit_code"], 3);
    assert_eq!(v["error"]["code"], "bad_input");
}

#[test]
fn exit_code_3_on_bad_input() {
    engram()
        .args(["remember", ""])
        .arg("--json")
        .assert()
        .code(3);
}

#[test]
fn forget_without_confirm_is_bad_input() {
    engram()
        .args(["forget", "00000000-0000-0000-0000-000000000000"])
        .assert()
        .code(3);
}

#[test]
fn forget_with_bad_uuid_is_bad_input() {
    engram()
        .args(["forget", "not-a-uuid", "--confirm"])
        .assert()
        .code(3);
}

#[test]
fn bench_mini_fts_deterministic() {
    let out = engram()
        .args(["bench", "mini-fts", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    let r1 = v["data"]["recall_at_1"].as_f64().unwrap();
    assert!(r1 >= 0.8, "FTS baseline should be ≥ 0.8, got {r1}");
    assert!(r1 <= 1.0, "R@1 cannot exceed 1.0, got {r1}");
}

#[test]
fn bench_mini_stub_is_stable_across_runs() {
    let mut r1s: Vec<f64> = Vec::new();
    for _ in 0..3 {
        let out = engram()
            .args(["bench", "mini", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v: Value = serde_json::from_slice(&out).unwrap();
        r1s.push(v["data"]["recall_at_1"].as_f64().unwrap());
    }
    // f64 does not implement Hash, compare pairwise instead.
    let first = r1s[0];
    for r in &r1s[1..] {
        assert!(
            (first - r).abs() < 1e-9,
            "stub bench should be deterministic, got {r1s:?}"
        );
    }
}

#[test]
fn export_returns_envelope_with_count() {
    let (_, mut cmd) = with_isolated_home();
    let out = cmd
        .args(["export"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    assert!(v["data"]["count"].is_number());
}

#[test]
fn import_from_export_roundtrip() {
    let (tmp, mut cmd1) = with_isolated_home();
    cmd1.args(["remember", "Metformin reduces mortality in type 2 diabetics."])
        .arg("--json")
        .assert()
        .success();

    let mut cmd2 = engram();
    cmd2.env("HOME", tmp.path());
    cmd2.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd2.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd2.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let export_out = cmd2
        .args(["export"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // Write export data to a tempfile then import into a fresh tmp home.
    let v = parse_stdout(&export_out);
    let data = &v["data"];
    let export_file = tmp.path().join("export.json");
    std::fs::write(&export_file, serde_json::to_vec(data).unwrap()).unwrap();

    let fresh = tempfile::tempdir().unwrap();
    let mut cmd3 = engram();
    cmd3.env("HOME", fresh.path());
    cmd3.env("XDG_CONFIG_HOME", fresh.path().join(".config"));
    cmd3.env("XDG_DATA_HOME", fresh.path().join(".local/share"));
    cmd3.env("XDG_CACHE_HOME", fresh.path().join(".cache"));
    let import_out = cmd3
        .args(["import", export_file.to_str().unwrap()])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let iv = parse_stdout(&import_out);
    assert_eq!(iv["status"], "success");
    assert!(iv["data"]["imported"].as_u64().unwrap() >= 1);
}

#[test]
fn config_set_writes_toml_file() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args(["config", "set", "keys.cohere", "cohere-test-key-12345"])
        .arg("--json")
        .assert()
        .success();

    // Verify file was written.
    let cfg_path = PathBuf::from(tmp.path())
        .join(".config")
        .join("engram")
        .join("config.toml");
    let path_exists_anywhere = cfg_path.exists()
        || tmp.path().join("Library/Application Support/bio.199-biotechnologies.engram/config.toml").exists();
    assert!(path_exists_anywhere, "config.toml should exist after set");
}

#[test]
fn config_show_masks_secrets() {
    let (_, mut cmd) = with_isolated_home();
    // Inject a fake key via env to exercise the masking path.
    cmd.env("GEMINI_API_KEY", "TEST_FAKE_KEY_1234567890_FAKEKEY");
    let out = cmd
        .args(["config", "show"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let gemini = &v["data"]["keys"]["gemini"];
    let value = gemini["value"].as_str().unwrap_or("");
    // Should be masked (contains "...")
    assert!(value.contains("..."), "key should be masked, got {value}");
    // And NOT contain the full original key's middle section.
    assert!(
        !value.contains("567890"),
        "middle of key should be masked, got {value}"
    );
}

#[test]
fn json_envelope_has_version_and_status() {
    let out = engram()
        .args(["bench", "mini-fts"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["version"], "1");
    assert!(v["status"].as_str().is_some());
    assert!(v["data"].is_object());
    assert!(v["metadata"].is_object());
}

#[test]
fn auto_json_when_piped() {
    // Not passing --json but piping stdout should still produce JSON envelope.
    let out = engram()
        .args(["bench", "mini-fts"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // When piped (which assert_cmd captures), stdout should be JSON.
    let s = String::from_utf8_lossy(&out);
    assert!(
        predicate::str::contains("\"version\"").eval(&s),
        "expected JSON envelope, got:\n{s}"
    );
}
