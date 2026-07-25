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

/// Point a spawned binary's data/config/cache at `home`.
/// The `directories` crate honors XDG_* on Linux and HOME on macOS.
fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

/// Process-wide throwaway home for tests that need no store of their own.
///
/// `engram` opens (and migrates) its SQLite database on every startup, so a
/// spawned binary with no HOME override would read and WRITE the developer's
/// real memory store — which also made the suite fail whenever that store had
/// been migrated by a newer build. Every command in this file is sandboxed.
fn shared_sandbox_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("tmp dir");
        // Create and migrate the sandbox database exactly once, before any
        // parallel test can spawn against it. `OnceLock` serializes this
        // initializer, so the tests never race to create the same fresh
        // SQLite file from several processes at the same time.
        let mut warmup = Command::cargo_bin("engram").expect("binary built");
        warmup.env("ENGRAM_BENCH_FORCE_STUB", "1");
        point_at_home(&mut warmup, tmp.path());
        warmup.arg("agent-info").assert().success();
        tmp
    })
    .path()
}

fn engram() -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, shared_sandbox_home());
    cmd
}

fn with_isolated_home() -> (tempfile::TempDir, Command) {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let mut cmd = engram();
    point_at_home(&mut cmd, tmp.path());
    (tmp, cmd)
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// A second/third command against the same isolated home.
fn cmd_in(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = engram();
    point_at_home(&mut cmd, tmp.path());
    cmd
}

/// Store a memory with caller-supplied facts (no LLM), returning the
/// envelope. `autonomy` is the conflict-resolution mode under test.
fn remember_with_facts(tmp: &tempfile::TempDir, content: &str, facts: &str, autonomy: &str) -> Value {
    let out = cmd_in(tmp)
        .args(["remember", content, "--facts-json", facts, "--autonomy", autonomy])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
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
    assert!(
        v.get("status").is_none() || v.get("status").and_then(|v| v.as_str()) != Some("success")
    );
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
fn skill_install_writes_cli_and_cross_agent_locations() {
    let (tmp, mut cmd) = with_isolated_home();
    let out = cmd
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");

    for path in [
        ".claude/skills/engram/SKILL.md",
        ".codex/skills/engram/SKILL.md",
        ".gemini/skills/engram/SKILL.md",
        ".agents/skills/engram/SKILL.md",
        ".agents/skills/engram/agents/openai.yaml",
    ] {
        assert!(tmp.path().join(path).exists(), "missing {path}");
    }
}

#[test]
fn skill_package_creates_uploadable_skill_zip() {
    let (tmp, mut cmd) = with_isolated_home();
    let zip_path = tmp.path().join("engram-skill.zip");
    cmd.arg("--json")
        .args(["skill", "package", "--out", zip_path.to_str().unwrap()])
        .assert()
        .success();

    let file = std::fs::File::open(&zip_path).expect("zip exists");
    let mut archive = zip::ZipArchive::new(file).expect("valid zip");
    assert!(archive.by_name("engram/SKILL.md").is_ok());
    assert!(archive.by_name("engram/agents/openai.yaml").is_ok());
}

#[test]
fn remember_then_recall_roundtrip() {
    let (tmp, mut cmd1) = with_isolated_home();
    cmd1.args([
        "remember",
        "Rapamycin extends mouse lifespan via mTORC1 inhibition.",
    ])
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
fn recall_refuses_mixed_embedding_metadata() {
    let (tmp, mut cmd1) = with_isolated_home();
    cmd1.args(["remember", "Rapamycin inhibits mTORC1."])
        .arg("--json")
        .assert()
        .success();

    let mut cmd2 = Command::cargo_bin("engram").expect("binary built");
    cmd2.env("HOME", tmp.path());
    cmd2.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd2.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd2.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    cmd2.env("GEMINI_API_KEY", "fake-gemini-key-for-guard-only");
    let out = cmd2
        .args(["recall", "rapamycin"])
        .arg("--json")
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let line = String::from_utf8_lossy(&out)
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("JSON error on stderr")
        .to_string();
    let v: Value = serde_json::from_str(&line).expect("parseable");
    assert_eq!(v["error"]["code"], "config_error");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("embedding metadata mismatch"));
}

#[test]
fn kb_ingest_compile_entities_flow() {
    let (tmp, mut kb_cmd) = with_isolated_home();
    kb_cmd
        .args([
            "kb",
            "create",
            "ageing-biology",
            "--description",
            "Ageing biology",
        ])
        .arg("--json")
        .assert()
        .success();

    let fixture = tmp.path().join("paper.md");
    std::fs::write(
        &fixture,
        "Rapamycin inhibits mTORC1 signaling in mice. Human trials measure vaccine response rather than lifespan.",
    )
    .unwrap();

    let mut ingest_cmd = engram();
    ingest_cmd.env("HOME", tmp.path());
    ingest_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    ingest_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    ingest_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    ingest_cmd
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--kb",
            "ageing-biology",
            "--mode",
            "takeaways",
            "--compile",
            "evidence",
            "--json",
        ])
        .assert()
        .success();

    let mut entities_cmd = engram();
    entities_cmd.env("HOME", tmp.path());
    entities_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    entities_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    entities_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out = entities_cmd
        .args(["entities", "list", "--kb", "ageing-biology", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    assert!(v["data"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["canonical_name"] == "Rapamycin"));
}

#[test]
fn kb_delete_removes_recallable_memories_in_that_kb() {
    let (tmp, mut kb_cmd) = with_isolated_home();
    kb_cmd
        .args(["kb", "create", "throwaway", "--json"])
        .assert()
        .success();

    let mut remember_cmd = engram();
    remember_cmd.env("HOME", tmp.path());
    remember_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    remember_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    remember_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    remember_cmd
        .args([
            "remember",
            "Temporary deletion marker alpha beta.",
            "--kb",
            "throwaway",
            "--no-facts",
            "--json",
        ])
        .assert()
        .success();

    let mut delete_cmd = engram();
    delete_cmd.env("HOME", tmp.path());
    delete_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    delete_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    delete_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    delete_cmd
        .args(["kb", "delete", "throwaway", "--confirm", "--json"])
        .assert()
        .success();

    let mut recall_cmd = engram();
    recall_cmd.env("HOME", tmp.path());
    recall_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    recall_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    recall_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out = recall_cmd
        .args([
            "recall",
            "Temporary deletion marker",
            "--kb",
            "throwaway",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "no_results");
    assert_eq!(v["data"]["results"].as_array().unwrap().len(), 0);
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
fn recall_no_results_sets_top_level_status() {
    let (_tmp, mut cmd) = with_isolated_home();
    let out = cmd
        .args(["recall", "nothing stored here", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "no_results");
    assert_eq!(v["data"]["status"], "no_results");
}

#[test]
fn usage_command_returns_summary_envelope() {
    let (_tmp, mut cmd) = with_isolated_home();
    let out = cmd
        .args(["usage", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    assert!(v["data"]["summary"].is_array());
    assert_eq!(v["data"]["totals"]["events"], 0);
}

#[test]
fn update_check_reports_agent_runnable_command_without_executing() {
    let (_tmp, mut cmd) = with_isolated_home();
    let out = cmd
        .env("ENGRAM_UPDATE_LATEST_VERSION", "9.9.9")
        .env("ENGRAM_UPDATE_INSTALL_SOURCE", "cargo")
        .args(["update", "--check", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    assert_eq!(v["data"]["latest_version"], "9.9.9");
    assert_eq!(v["data"]["update_available"], true);
    assert_eq!(v["data"]["install_source"], "cargo");
    assert_eq!(v["data"]["can_execute_update"], true);
    assert_eq!(v["data"]["executed"], false);
    assert_eq!(
        v["data"]["upgrade_command"],
        "cargo install paperfoot-engram --version 9.9.9 --locked --force"
    );
}

#[test]
fn update_disabled_is_offline_and_non_mutating() {
    let (_tmp, mut cmd) = with_isolated_home();
    let out = cmd
        .env("ENGRAM_UPDATE_MODE", "disabled")
        .args(["update", "--check", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    assert_eq!(v["data"]["update_mode"], "disabled");
    assert_eq!(v["data"]["latest_version"], Value::Null);
    assert_eq!(v["data"]["update_available"], false);
    assert_eq!(v["data"]["executed"], false);
}

#[test]
fn documents_jobs_budget_and_scientific_bench_work() {
    let (tmp, mut ingest_cmd) = with_isolated_home();
    let fixture = tmp.path().join("paper.md");
    std::fs::write(
        &fixture,
        "Rapamycin inhibits mTORC1 in mice. A human trial measured vaccine response.",
    )
    .unwrap();
    ingest_cmd
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--kb",
            "ageing-biology",
            "--mode",
            "papers",
            "--json",
        ])
        .assert()
        .success();

    let mut docs_cmd = engram();
    docs_cmd.env("HOME", tmp.path());
    docs_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    docs_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    docs_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let docs_out = docs_cmd
        .args(["documents", "list", "--kb", "ageing-biology", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let docs = parse_stdout(&docs_out);
    assert_eq!(docs["status"], "success");
    assert_eq!(docs["data"]["documents"].as_array().unwrap().len(), 1);

    let mut compile_cmd = engram();
    compile_cmd.env("HOME", tmp.path());
    compile_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    compile_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    compile_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    compile_cmd
        .args(["compile", "--kb", "ageing-biology", "--all", "--json"])
        .assert()
        .success();

    let mut jobs_cmd = engram();
    jobs_cmd.env("HOME", tmp.path());
    jobs_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    jobs_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    jobs_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let jobs_out = jobs_cmd
        .args(["jobs", "list", "--kb", "ageing-biology", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = parse_stdout(&jobs_out);
    assert!(jobs["data"]["jobs"].as_array().unwrap().len() >= 1);

    let mut budget_cmd = engram();
    budget_cmd.env("HOME", tmp.path());
    budget_cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    budget_cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    budget_cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    budget_cmd
        .args([
            "budget",
            "set",
            "--kb",
            "ageing-biology",
            "--daily-usd",
            "2.50",
            "--json",
        ])
        .assert()
        .success();

    let out = engram()
        .args(["bench", "scientific-mini", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let bench = parse_stdout(&out);
    assert_eq!(bench["data"]["suite"], "scientific-mini");
    assert_eq!(bench["data"]["accuracy"], 1.0);
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
    assert_eq!(v["data"]["version"], 2);
    assert!(v["data"]["counts"]["memories"].is_number());
    assert!(v["data"]["chunks"].is_array());
    assert!(v["data"]["kbs"].is_array());
}

#[test]
fn import_from_export_roundtrip() {
    let (tmp, mut cmd1) = with_isolated_home();
    cmd1.args([
        "remember",
        "Metformin reduces mortality in type 2 diabetics.",
    ])
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
    assert_eq!(iv["data"]["format_version"], 2);
    assert!(iv["data"]["imported"]["memories"].as_u64().unwrap() >= 1);
    // v2 round-trip carries the chunks + embeddings, so dense search works
    // without re-embedding.
    assert!(iv["data"]["imported"]["chunks"].as_u64().unwrap() >= 1);
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
        || tmp
            .path()
            .join("Library/Application Support/bio.199-biotechnologies.engram/config.toml")
            .exists();
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

#[test]
fn ingest_same_source_path_is_idempotent() {
    let (tmp, mut ingest1) = with_isolated_home();
    let fixture = tmp.path().join("note.md");
    std::fs::write(
        &fixture,
        "Rapamycin inhibits mTORC1. Human evidence should stay cited.",
    )
    .unwrap();
    ingest1
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--kb",
            "idempotency",
            "--mode",
            "takeaways",
            "--json",
        ])
        .assert()
        .success();

    let mut ingest2 = engram();
    ingest2.env("HOME", tmp.path());
    ingest2.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    ingest2.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    ingest2.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out = ingest2
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--kb",
            "idempotency",
            "--mode",
            "takeaways",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["memories_created"], 0);
    assert_eq!(v["data"]["chunks_created"], 0);
    assert_eq!(v["data"]["skipped_existing"], 1);

    let mut show = engram();
    show.env("HOME", tmp.path());
    show.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    show.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    show.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let kb_out = show
        .args(["kb", "show", "idempotency", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let kb = parse_stdout(&kb_out);
    assert_eq!(kb["data"]["knowledge_base"]["document_count"], 1);
    assert_eq!(kb["data"]["knowledge_base"]["memory_count"], 1);
}

#[test]
fn document_delete_clears_derived_entities() {
    let (tmp, mut ingest) = with_isolated_home();
    let fixture = tmp.path().join("paper.md");
    std::fs::write(
        &fixture,
        "Rapamycin inhibits mTORC1 in mice. Human trials measure vaccine response.",
    )
    .unwrap();
    ingest
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--kb",
            "delete-clean",
            "--mode",
            "papers",
            "--compile",
            "evidence",
            "--json",
        ])
        .assert()
        .success();

    let mut docs = engram();
    docs.env("HOME", tmp.path());
    docs.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    docs.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    docs.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let docs_out = docs
        .args(["documents", "list", "--kb", "delete-clean", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let docs_json = parse_stdout(&docs_out);
    let document_id = docs_json["data"]["documents"][0]["id"].as_str().unwrap();

    let mut delete = engram();
    delete.env("HOME", tmp.path());
    delete.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    delete.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    delete.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    delete
        .args(["documents", "delete", document_id, "--confirm", "--json"])
        .assert()
        .success();

    // Deleting one document no longer wipes the whole KB's compiled
    // knowledge; entity aggregates go stale until the next compile.
    // A fresh compile over the (now empty) KB must clear them.
    let mut compile = engram();
    compile.env("HOME", tmp.path());
    compile.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    compile.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    compile.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    compile
        .args(["compile", "--kb", "delete-clean", "--all", "--json"])
        .assert()
        .success();

    let mut entities = engram();
    entities.env("HOME", tmp.path());
    entities.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    entities.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    entities.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let entities_out = entities
        .args(["entities", "list", "--kb", "delete-clean", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let entities_json = parse_stdout(&entities_out);
    assert_eq!(
        entities_json["data"]["entities"].as_array().unwrap().len(),
        0
    );
}

#[test]
fn memory_aliases_roundtrip_and_skip_extraction_loudly_without_a_key() {
    let (tmp, mut add) = with_isolated_home();
    // ENGRAM_BENCH_FORCE_STUB (set by engram()) stands in for "no key": the
    // fact layer must refuse to reach the network, and say so.
    add.env("OPENROUTER_API_KEY", "fake-key-that-should-not-be-used");
    let out = add
        .args([
            "memory",
            "add",
            "The release checklist requires integrity repair before publish.",
            "--importance",
            "8",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let added = parse_stdout(&out);
    assert_eq!(added["data"]["facts_added"], 0);
    assert_eq!(
        added["data"]["fact_extraction"],
        "skipped_no_openrouter_key"
    );
    assert_eq!(
        added["metadata"]["fact_extraction"],
        "skipped_no_openrouter_key"
    );
    let note = added["metadata"]["fact_extraction_note"]
        .as_str()
        .expect("a loud skip carries a note");
    assert!(note.contains("facts backfill"), "actionable note: {note}");

    let mut search = engram();
    search.env("HOME", tmp.path());
    search.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    search.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    search.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let search_out = search
        .args([
            "memory",
            "search",
            "integrity repair release",
            "--top-k",
            "1",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let found = parse_stdout(&search_out);
    assert_eq!(found["status"], "success");
    assert!(found["data"]["results"][0]["content"]
        .as_str()
        .unwrap()
        .contains("integrity repair"));
}

#[test]
fn directory_ingest_requires_explicit_scope_or_preview() {
    let (tmp, mut cmd) = with_isolated_home();
    let dir = tmp.path().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("one.md"), "One important note.").unwrap();
    std::fs::write(dir.join("two.md"), "Two important note.").unwrap();

    cmd.args([
        "ingest",
        dir.to_str().unwrap(),
        "--kb",
        "scope-safe",
        "--json",
    ])
    .assert()
    .code(3);
}

#[test]
fn directory_ingest_dry_run_and_include_are_selective() {
    let (tmp, mut dry_run) = with_isolated_home();
    let dir = tmp.path().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.md"), "Keep this selected note.").unwrap();
    std::fs::write(dir.join("skip.md"), "Skip this unselected note.").unwrap();

    let out = dry_run
        .args([
            "ingest",
            dir.to_str().unwrap(),
            "--kb",
            "selective",
            "--include",
            "keep.md",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let preview = parse_stdout(&out);
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(preview["data"]["matched_count"], 1);
    assert!(preview["data"]["matched_files"][0]
        .as_str()
        .unwrap()
        .ends_with("keep.md"));

    let mut ingest = engram();
    ingest.env("HOME", tmp.path());
    ingest.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    ingest.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    ingest.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let ingest_out = ingest
        .args([
            "ingest",
            dir.to_str().unwrap(),
            "--kb",
            "selective",
            "--include",
            "keep.md",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result = parse_stdout(&ingest_out);
    assert_eq!(result["data"]["matched_count"], 1);
    assert_eq!(result["data"]["memories_created"], 1);

    let mut docs = engram();
    docs.env("HOME", tmp.path());
    docs.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    docs.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    docs.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let docs_out = docs
        .args(["documents", "list", "--kb", "selective", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let docs_json = parse_stdout(&docs_out);
    assert_eq!(docs_json["data"]["documents"].as_array().unwrap().len(), 1);
    assert!(docs_json["data"]["documents"][0]["source_path"]
        .as_str()
        .unwrap()
        .ends_with("keep.md"));
}

#[test]
fn directory_ingest_max_files_caps_matches() {
    let (tmp, mut cmd) = with_isolated_home();
    let dir = tmp.path().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("one.md"), "One note.").unwrap();
    std::fs::write(dir.join("two.md"), "Two note.").unwrap();

    cmd.args([
        "ingest",
        dir.to_str().unwrap(),
        "--kb",
        "max-files",
        "--all",
        "--max-files",
        "1",
        "--json",
    ])
    .assert()
    .code(3);
}

#[test]
fn directory_ingest_uses_configured_scope_defaults() {
    let (tmp, mut set_include) = with_isolated_home();
    let dir = tmp.path().join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.md"), "Keep this configured note.").unwrap();
    std::fs::write(dir.join("skip.md"), "Skip this unconfigured note.").unwrap();

    set_include
        .args(["config", "set", "ingest.include", "keep.md", "--json"])
        .assert()
        .success();

    let mut set_max = engram();
    set_max.env("HOME", tmp.path());
    set_max.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    set_max.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    set_max.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    set_max
        .args(["config", "set", "ingest.max_files", "1", "--json"])
        .assert()
        .success();

    let mut ingest = engram();
    ingest.env("HOME", tmp.path());
    ingest.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    ingest.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    ingest.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out = ingest
        .args([
            "ingest",
            dir.to_str().unwrap(),
            "--kb",
            "configured",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result = parse_stdout(&out);
    assert_eq!(result["data"]["matched_count"], 1);
    assert_eq!(result["data"]["include"][0], "keep.md");
    assert_eq!(result["data"]["max_files"], 1);
    assert_eq!(result["data"]["memories_created"], 1);

    let mut docs = engram();
    docs.env("HOME", tmp.path());
    docs.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    docs.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    docs.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let docs_out = docs
        .args(["documents", "list", "--kb", "configured", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let docs_json = parse_stdout(&docs_out);
    assert_eq!(docs_json["data"]["documents"].as_array().unwrap().len(), 1);
    assert!(docs_json["data"]["documents"][0]["source_path"]
        .as_str()
        .unwrap()
        .ends_with("keep.md"));
}

/// Helper: run a recall with extra args in an existing isolated home.
fn recall_in(home: &std::path::Path, args: &[&str]) -> Value {
    let mut cmd = engram();
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    let out = cmd.args(args).assert().get_output().stdout.clone();
    parse_stdout(&out)
}

#[test]
fn recall_since_until_actually_filter_by_time() {
    let (tmp, mut add) = with_isolated_home();
    add.args(["remember", "Rapamycin inhibits mTORC1 signalling."])
        .arg("--json")
        .assert()
        .success();

    // A window entirely in the past must exclude the just-stored memory.
    let past = recall_in(
        tmp.path(),
        &[
            "recall", "rapamycin mTORC1", "--diary", "*", "--until",
            "2020-01-01T00:00:00Z", "--json",
        ],
    );
    assert_eq!(
        past["data"]["results"].as_array().unwrap().len(),
        0,
        "--until in the past should exclude a memory stored now"
    );

    // A window from the past onward must include it (the flag is wired).
    let now_window = recall_in(
        tmp.path(),
        &[
            "recall", "rapamycin mTORC1", "--diary", "*", "--since",
            "2020-01-01T00:00:00Z", "--json",
        ],
    );
    assert!(
        !now_window["data"]["results"].as_array().unwrap().is_empty(),
        "--since in the past should include a memory stored now"
    );
}

#[test]
fn recall_output_carries_conflicts_field() {
    let (tmp, mut add) = with_isolated_home();
    add.args(["remember", "The capital is Springfield."])
        .arg("--json")
        .assert()
        .success();
    let v = recall_in(tmp.path(), &["recall", "capital", "--diary", "*", "--json"]);
    assert!(
        v["data"]["conflicts"].is_array(),
        "recall output must include a conflicts array"
    );
}

#[test]
fn recall_cloud_profile_without_key_errors_not_silent_stub() {
    // No ENGRAM_BENCH_FORCE_STUB, no GEMINI key → a cloud profile must
    // fail with a config error (exit 2), not silently use the 64-dim stub.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env("HOME", tmp.path());
    cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let assert = cmd
        .args(["recall", "anything", "--profile", "cloud_quality", "--json"])
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("Gemini API key") || err.contains("offline"),
        "expected a config error about the missing key, got: {err}"
    );
}

#[test]
fn recall_offline_profile_without_key_works() {
    // The explicit offline profile opts into the stub and must succeed.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env("HOME", tmp.path());
    cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    cmd.args(["recall", "anything", "--profile", "offline", "--json"])
        .assert()
        .success();
}

#[test]
fn remember_dedups_identical_content() {
    let (tmp, mut c1) = with_isolated_home();
    let out1 = c1
        .args(["remember", "Rapamycin extends mouse lifespan.", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v1 = parse_stdout(&out1);
    assert_eq!(v1["data"]["stored"], true);

    // Second identical remember → deduplicated, not stored again.
    let mut c2 = engram();
    c2.env("HOME", tmp.path());
    c2.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    c2.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    c2.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out2 = c2
        .args(["remember", "Rapamycin extends mouse lifespan.", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v2 = parse_stdout(&out2);
    assert_eq!(v2["data"]["stored"], false);
    assert_eq!(v2["data"]["deduplicated"], true);
    assert_eq!(v2["data"]["id"], v1["data"]["id"]);

    // --allow-duplicate forces a second row.
    let mut c3 = engram();
    c3.env("HOME", tmp.path());
    c3.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    c3.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    c3.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    let out3 = c3
        .args(["remember", "Rapamycin extends mouse lifespan.", "--allow-duplicate", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v3 = parse_stdout(&out3);
    assert_eq!(v3["data"]["stored"], true);
    assert_ne!(v3["data"]["id"], v1["data"]["id"]);
}

#[test]
fn budget_gate_blocks_over_cap_spend() {
    // Set a $0 daily budget, then a cloud-embedder ingest must be refused with
    // exit code 4 (rate limited) BEFORE spending — proven with a fake key so no
    // real call is made. Uses a fake GEMINI key so use_cloud_embedder is true;
    // the budget gate fires before the (would-be) network call.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let env = |c: &mut Command| {
        c.env_remove("ENGRAM_BENCH_FORCE_STUB");
        c.env("GEMINI_API_KEY", "fake-key-not-used-because-budget-blocks-first");
        c.env("HOME", tmp.path());
        c.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
        c.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
        c.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    };
    // First, record some usage so spent >= 0 and set a $0 cap → immediately over.
    let mut setb = Command::cargo_bin("engram").unwrap();
    env(&mut setb);
    setb.args(["budget", "set", "--kb", "capped", "--daily-usd", "0", "--json"])
        .assert()
        .success();

    let fixture = tmp.path().join("note.md");
    std::fs::write(&fixture, "Some content to embed.").unwrap();
    let mut ing = Command::cargo_bin("engram").unwrap();
    env(&mut ing);
    ing.args([
        "ingest",
        fixture.to_str().unwrap(),
        "--kb",
        "capped",
        "--all",
        "--json",
    ])
    .assert()
    .failure()
    .code(4);
}

// ----- Write-path v2: facts-json, slot policies, conflict queue, resolve -----

#[test]
fn facts_json_stores_typed_fact_without_llm() {
    let (tmp, _cmd) = with_isolated_home();
    let v = remember_with_facts(
        &tmp,
        "Ana moved to Lisbon in March.",
        r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0,"valid_from":"2026-03-01"}]"#,
        "assisted",
    );
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 0);

    let out = cmd_in(&tmp)
        .args(["facts", "list", "--subject", "Ana"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let facts = v["data"]["facts"].as_array().expect("facts array");
    assert_eq!(facts.len(), 1);
    // lives_in is a known profile slot; valid_from came from the caller.
    assert_eq!(facts[0]["fact_type"], "profile_slot");
    assert!(facts[0]["valid_from"].as_str().unwrap().starts_with("2026-03-01"));
}

#[test]
fn slot_last_writer_wins_supersedes_and_recall_reports_update() {
    let (tmp, _cmd) = with_isolated_home();
    remember_with_facts(
        &tmp,
        "Ana lives in Porto.",
        r#"[{"subject":"Ana","predicate":"lives_in","object":"Porto"}]"#,
        "assisted",
    );
    // Same slot, synonym predicate, new value: canonicalized to lives_in and
    // superseded last-writer-wins with zero LLM involvement.
    let v = remember_with_facts(
        &tmp,
        "Ana now resides in Lisbon.",
        r#"[{"subject":"Ana","predicate":"resides_in","object":"Lisbon"}]"#,
        "assisted",
    );
    let conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "LWW slot update must report the supersession");
    assert_eq!(conflicts[0]["old_object"], "Porto");
    assert_eq!(conflicts[0]["new_object"], "Lisbon");
    assert_eq!(conflicts[0]["resolution"], "slot_lww");

    // recall surfaces the resolved update.
    let out = cmd_in(&tmp)
        .args(["recall", "where does Ana live", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let recall_conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert!(
        recall_conflicts.iter().any(|c| c["status"] == "superseded"
            && c["old_object"] == "Porto"
            && c["new_object"] == "Lisbon"),
        "recall must report the superseded value: {recall_conflicts:?}"
    );
}

#[test]
fn safety_slot_queues_conflict_and_recall_reports_contested() {
    let (tmp, _cmd) = with_isolated_home();
    remember_with_facts(
        &tmp,
        "Ana is allergic to penicillin.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]"#,
        "assisted",
    );
    // A different allergy value must NOT auto-supersede (await-confirmation):
    // both stay active and the conflict queues for the caller.
    let v = remember_with_facts(
        &tmp,
        "Actually Ana is allergic to amoxicillin.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"amoxicillin"}]"#,
        "assisted",
    );
    let queued = v["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "safety slot must queue, not supersede");
    assert_eq!(queued[0]["status"], "contested");
    let conflict_id = queued[0]["conflict_id"].as_str().unwrap().to_string();

    // Both values active.
    let out = cmd_in(&tmp)
        .args(["facts", "list", "--subject", "Ana", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["facts"].as_array().unwrap().len(), 2);

    // recall flags the subject as contested with both values.
    let out = cmd_in(&tmp)
        .args(["recall", "Ana penicillin allergy", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let contested: Vec<&Value> = v["data"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "contested")
        .collect();
    assert_eq!(contested.len(), 1, "recall must mark the allergy contested");
    let values = contested[0]["values"].as_array().unwrap();
    assert!(values.iter().any(|v| v == "penicillin"));
    assert!(values.iter().any(|v| v == "amoxicillin"));

    // conflicts --open lists it; resolve applies the caller's decision.
    let out = cmd_in(&tmp)
        .args(["facts", "conflicts", "--open", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["count"], 1);

    let out = cmd_in(&tmp)
        .args([
            "facts",
            "resolve",
            &conflict_id,
            "--action",
            "supersede-old",
            "--note",
            "user confirmed the correction",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["resolved"], true);

    // Queue is empty; only the new value remains active with resolution=caller.
    let out = cmd_in(&tmp)
        .args(["facts", "conflicts", "--open", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(parse_stdout(&out)["data"]["count"], 0);
    let out = cmd_in(&tmp)
        .args(["facts", "list", "--subject", "Ana", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let facts = parse_stdout(&out)["data"]["facts"].as_array().unwrap().clone();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["object"], "amoxicillin");
}

#[test]
fn reasserted_fact_dedupes_into_confirmation() {
    let (tmp, _cmd) = with_isolated_home();
    remember_with_facts(
        &tmp,
        "Ana works at Acme.",
        r#"[{"subject":"Ana","predicate":"works_at","object":"Acme"}]"#,
        "assisted",
    );
    let v = remember_with_facts(
        &tmp,
        "As mentioned, Ana works at Acme.",
        r#"[{"subject":"Ana","predicate":"works_at","object":"Acme"}]"#,
        "assisted",
    );
    assert_eq!(v["data"]["facts_added"], 0);
    assert_eq!(v["data"]["facts_deduped"], 1);

    let out = cmd_in(&tmp)
        .args(["facts", "list", "--subject", "Ana", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let facts = parse_stdout(&out)["data"]["facts"].as_array().unwrap().clone();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["confirmations"], 1);
}

#[test]
fn strict_autonomy_queues_even_plain_slot_updates() {
    let (tmp, _cmd) = with_isolated_home();
    remember_with_facts(
        &tmp,
        "Ana works at Acme.",
        r#"[{"subject":"Ana","predicate":"works_at","object":"Acme"}]"#,
        "strict",
    );
    let v = remember_with_facts(
        &tmp,
        "Ana works at Beta Corp now.",
        r#"[{"subject":"Ana","predicate":"works_at","object":"Beta Corp"}]"#,
        "strict",
    );
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(v["data"]["adjudication_needed"].as_array().unwrap().len(), 1);
}

#[test]
fn facts_json_invalid_is_bad_input_exit_3() {
    let (_tmp, mut cmd) = with_isolated_home();
    cmd.args(["remember", "x", "--facts-json", "not json", "--json"])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn resolve_unknown_conflict_is_bad_input() {
    let (_tmp, mut cmd) = with_isolated_home();
    cmd.args([
        "facts",
        "resolve",
        "00000000-0000-0000-0000-000000000000",
        "--action",
        "keep-both",
        "--json",
    ])
    .assert()
    .failure()
    .code(3);
}

// ===========================================================================
// Retrieval scope: a plain `recall` must search every KB, `retrieval.default_kb`
// pins it, and a diary-scoped recall must never leak derived content from
// another diary.
// ===========================================================================

/// Store `content` in `kb` (and optionally `diary`) in the isolated home.
fn remember_in(tmp: &tempfile::TempDir, content: &str, kb: &str, diary: &str) {
    cmd_in(tmp)
        .args([
            "remember",
            content,
            "--kb",
            kb,
            "--diary",
            diary,
            "--no-facts",
            "--json",
        ])
        .assert()
        .success();
}

/// Run `recall` with the given extra args and return the envelope.
fn recall_json(tmp: &tempfile::TempDir, args: &[&str]) -> Value {
    let out = cmd_in(tmp)
        .arg("recall")
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

// ----- Fact layer activation: default-on extraction, event time, backfill ---

/// A deterministic offline "extractor". Keys are matched against the memory
/// text, so several memories can yield different facts in one process without
/// a network call.
const LISBON_SCRIPT: &str = r#"{
  "Lisbon": [{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0,"fact_type":"profile_slot"}],
  "Acme":   [{"subject":"Ana","predicate":"works_at","object":"Acme","confidence":1.0,"fact_type":"profile_slot"}],
  "*": []
}"#;

/// A command against `tmp` whose fact extraction is driven by `script`.
fn cmd_with_extractor(tmp: &tempfile::TempDir, script: &str) -> Command {
    let mut cmd = cmd_in(tmp);
    cmd.env("ENGRAM_FACT_LLM_STUB", script);
    cmd
}

fn run_json(cmd: &mut Command, args: &[&str]) -> Value {
    let out = cmd
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

fn contents(v: &Value) -> Vec<String> {
    v["data"]["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn plain_recall_searches_every_kb_by_default() {
    let (tmp, _cmd) = with_isolated_home();
    remember_in(
        &tmp,
        "Noemi birthday is on the fourteenth of March.",
        "relationships",
        "default",
    );

    // No --kb at all: the fact lives in `relationships`, and the caller has no
    // way to know that. Searching only the KB named "default" would miss it.
    let v = recall_json(&tmp, &["Noemi birthday"]);
    assert_eq!(v["status"], "success", "plain recall must find the memory");
    assert!(contents(&v).iter().any(|c| c.contains("Noemi birthday")));
    assert_eq!(v["metadata"]["kb"], "*");
    assert_eq!(v["metadata"]["kb_scope"], "all_kbs");
    assert_eq!(v["data"]["results"][0]["kb"], "relationships");
}

#[test]
fn memory_search_alias_also_defaults_to_every_kb() {
    let (tmp, _cmd) = with_isolated_home();
    remember_in(
        &tmp,
        "Petus raised a seed round in June.",
        "business",
        "default",
    );

    let out = cmd_in(&tmp)
        .args(["memory", "search", "Petus seed round"])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    assert!(contents(&v).iter().any(|c| c.contains("Petus raised")));
}

#[test]
fn explicit_kb_flag_still_scopes_recall_to_that_kb() {
    let (tmp, _cmd) = with_isolated_home();
    remember_in(&tmp, "Alpha marker phrase quokka.", "alpha", "default");
    remember_in(&tmp, "Beta marker phrase wombat.", "beta", "default");

    let v = recall_json(&tmp, &["marker phrase quokka", "--kb", "beta"]);
    assert_eq!(v["metadata"]["kb"], "beta");
    assert_eq!(v["metadata"]["kb_scope"], "flag");
    assert!(
        !contents(&v).iter().any(|c| c.contains("quokka")),
        "an explicit --kb must not return another KB's memory: {:?}",
        contents(&v)
    );

    // ...and the same query unscoped does reach it.
    let v = recall_json(&tmp, &["marker phrase quokka"]);
    assert!(contents(&v).iter().any(|c| c.contains("quokka")));
}

#[test]
fn retrieval_default_kb_config_pins_recall_scope() {
    let (tmp, _cmd) = with_isolated_home();
    remember_in(&tmp, "Alpha marker phrase quokka.", "alpha", "default");
    remember_in(&tmp, "Beta marker phrase wombat.", "beta", "default");

    cmd_in(&tmp)
        .args(["config", "set", "retrieval.default_kb", "beta"])
        .arg("--json")
        .assert()
        .success();

    // The pinned KB is discoverable via `config show`.
    let out = cmd_in(&tmp)
        .args(["config", "show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let cfg = parse_stdout(&out);
    assert_eq!(cfg["data"]["retrieval"]["default_kb"], "beta");
    assert_eq!(
        cfg["data"]["retrieval"]["default_kb_effective_scope"],
        "beta"
    );

    // Config pins the scope when no --kb is passed.
    let v = recall_json(&tmp, &["marker phrase"]);
    assert_eq!(v["metadata"]["kb"], "beta");
    assert_eq!(v["metadata"]["kb_scope"], "config");
    let found = contents(&v);
    assert!(found.iter().any(|c| c.contains("wombat")));
    assert!(!found.iter().any(|c| c.contains("quokka")));

    // An explicit --kb outranks the config.
    let v = recall_json(&tmp, &["marker phrase", "--kb", "alpha"]);
    assert_eq!(v["metadata"]["kb_scope"], "flag");
    assert!(contents(&v).iter().any(|c| c.contains("quokka")));

    // --all-kbs is still the escape hatch back to everything.
    let v = recall_json(&tmp, &["marker phrase", "--all-kbs"]);
    assert_eq!(v["metadata"]["kb"], "*");
    assert_eq!(v["metadata"]["kb_scope"], "all_kbs");
    let found = contents(&v);
    assert!(found.iter().any(|c| c.contains("quokka")));
    assert!(found.iter().any(|c| c.contains("wombat")));
}

#[test]
fn diary_scoped_recall_never_returns_another_diarys_compiled_claims() {
    let (tmp, _cmd) = with_isolated_home();
    // Two diaries share one KB, so the KB alone cannot prove provenance.
    // The digit matters: the deterministic compiler only turns a sentence into
    // a cited claim when it looks like evidence.
    remember_in(
        &tmp,
        "Nadia consulted a psychiatrist about 3 ketamine infusions last winter.",
        "shared",
        "private",
    );
    remember_in(
        &tmp,
        "Standup moved to Tuesday mornings at 9 am for the whole team.",
        "shared",
        "work",
    );

    // Compile turns each chunk into cited claims — the LLM/compiler paraphrase
    // layer that recall searches in agent/evidence mode.
    cmd_in(&tmp)
        .args(["compile", "--kb", "shared", "--all", "--json"])
        .assert()
        .success();

    // The owning diary still sees its own content.
    let v = recall_json(
        &tmp,
        &[
            "ketamine psychiatrist",
            "--diary",
            "private",
            "--mode",
            "agent",
        ],
    );
    assert!(
        contents(&v).iter().any(|c| c.contains("ketamine")),
        "the private diary must still recall its own memory"
    );

    // A recall scoped to another diary must return nothing derived from it —
    // not the chunk, and not a claim restating the chunk.
    let v = recall_json(
        &tmp,
        &[
            "ketamine psychiatrist",
            "--diary",
            "work",
            "--mode",
            "agent",
        ],
    );
    let leaked = contents(&v);
    assert!(
        !leaked
            .iter()
            .any(|c| c.to_lowercase().contains("ketamine")
                || c.to_lowercase().contains("psychiatrist")),
        "diary isolation leak: {leaked:?}"
    );
    // The exclusion is reported, not silent.
    let excluded = &v["metadata"]["diary_isolation"]["excluded_candidates"];
    assert!(
        excluded["claim"].as_u64().unwrap_or(0) >= 1,
        "withheld claims must be reported in metadata: {}",
        v["metadata"]["diary_isolation"]
    );
    assert_eq!(v["metadata"]["diary_isolation"]["diary"], "work");
}

#[test]
fn entity_retrieval_fires_when_recall_spans_every_kb() {
    let (tmp, _cmd) = with_isolated_home();
    remember_in(
        &tmp,
        "Rapamycin extends mouse lifespan through mTORC1 inhibition.",
        "one",
        "default",
    );
    remember_in(
        &tmp,
        "Metformin lowers fasting glucose in older adults.",
        "two",
        "default",
    );
    for kb in ["one", "two"] {
        cmd_in(&tmp)
            .args(["compile", "--kb", kb, "--all", "--json"])
            .assert()
            .success();
    }

    // Both KBs really do carry an entity.
    for (kb, name) in [("one", "Rapamycin"), ("two", "Metformin")] {
        let out = cmd_in(&tmp)
            .args(["entities", "list", "--kb", kb, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v = parse_stdout(&out);
        assert!(
            v["data"]["entities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["canonical_name"] == name),
            "expected entity {name} in kb {kb}"
        );
    }

    // Unscoped recall used to return early before entity/graph retrieval,
    // silently dropping two of the pipeline's sources.
    let v = recall_json(&tmp, &["Rapamycin Metformin", "--top-k", "20"]);
    let results = v["data"]["results"].as_array().unwrap();
    let entity_kbs: Vec<&str> = results
        .iter()
        .filter(|r| {
            r["sources"]
                .as_array()
                .map(|s| s.iter().any(|x| x == "entity"))
                .unwrap_or(false)
        })
        .map(|r| r["kb"].as_str().unwrap_or_default())
        .collect();
    assert!(
        entity_kbs.contains(&"one") && entity_kbs.contains(&"two"),
        "entity retrieval must fire in both KBs, got {entity_kbs:?}"
    );
}

#[test]
fn remember_extracts_facts_by_default() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["remember", "Ana moved to Lisbon last March."],
    );
    // No --extract-facts flag anywhere: extraction is the default now.
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["fact_extraction"], "ran_stub");
    assert!(v["data"]["fact_extraction_note"].is_null());

    let facts = run_json(&mut cmd_in(&tmp), &["facts", "list", "--subject", "Ana"]);
    assert_eq!(facts["data"]["facts"][0]["predicate"], "lives_in");
    assert_eq!(facts["data"]["facts"][0]["object"], "Lisbon");
}

#[test]
fn remember_no_extract_facts_opts_out() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["remember", "Ana moved to Lisbon last March.", "--no-extract-facts"],
    );
    assert_eq!(v["data"]["facts_added"], 0);
    assert_eq!(v["data"]["fact_extraction"], "disabled");

    // The legacy spelling still works.
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["remember", "Ana works at Acme.", "--no-facts"],
    );
    assert_eq!(v["data"]["fact_extraction"], "disabled");
}

#[test]
fn facts_json_still_skips_the_extraction_call() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    // An extractor IS available, but the caller supplied the triple — the
    // documented contract is "no LLM call", so the Lisbon rule must not fire.
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &[
            "remember",
            "Ana moved to Lisbon last March.",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"prefers_language","object":"Rust"}]"#,
        ],
    );
    assert_eq!(v["data"]["fact_extraction"], "skipped_facts_json");
    assert_eq!(v["data"]["facts_added"], 1);

    let facts = run_json(&mut cmd_in(&tmp), &["facts", "list", "--subject", "Ana"]);
    assert_eq!(facts["data"]["count"], 1);
    assert_eq!(facts["data"]["facts"][0]["predicate"], "prefers_language");
}

#[test]
fn remember_still_accepts_the_deprecated_extract_facts_flag() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["remember", "Ana moved to Lisbon last March.", "--extract-facts"],
    );
    assert_eq!(v["data"]["facts_added"], 1);

    // Old behavior preserved: --extract-facts alongside --facts-json still
    // does both, instead of only storing the caller's triple.
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &[
            "remember",
            "Ana works at Acme Corp.",
            "--extract-facts",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"prefers_language","object":"Rust"}]"#,
        ],
    );
    assert_eq!(v["data"]["fact_extraction"], "ran_stub");
    assert_eq!(v["data"]["facts_added"], 2);
}

#[test]
fn remember_without_a_key_skips_extraction_loudly_on_stderr() {
    let (_tmp, mut cmd) = with_isolated_home();
    let assert = cmd
        .args(["remember", "Ana moved to Lisbon last March.", "--json"])
        .assert()
        .success();
    let v = parse_stdout(&assert.get_output().stdout);
    assert_eq!(v["data"]["fact_extraction"], "skipped_no_openrouter_key");
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("fact extraction skipped"),
        "the skip must be audible, got stderr: {err}"
    );
}

#[test]
fn memory_add_has_full_flag_parity_with_remember() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    // --facts-json, --autonomy and --event-time now exist on `memory add`.
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "memory",
            "add",
            "Ana and Bo married in Porto.",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"married_to","object":"Bo"}]"#,
            "--autonomy",
            "strict",
            "--event-time",
            "2019-08-17",
            "--no-extract-facts",
        ],
    );
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["event_time"], "2019-08-17T00:00:00+00:00");

    // The fact inherits the memory's event time as its bi-temporal start.
    let facts = run_json(&mut cmd_in(&tmp), &["facts", "list", "--subject", "Ana"]);
    assert!(facts["data"]["facts"][0]["valid_from"]
        .as_str()
        .unwrap()
        .starts_with("2019-08-17"));
}

#[test]
fn date_valued_predicate_object_becomes_the_fact_event_time() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Ana's birthday.",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"born_on","object":"1990-04-12"}]"#,
        ],
    );
    let facts = run_json(&mut cmd_in(&tmp), &["facts", "list", "--subject", "Ana"]);
    assert!(facts["data"]["facts"][0]["valid_from"]
        .as_str()
        .unwrap()
        .starts_with("1990-04-12"));
}

#[test]
fn event_time_is_persisted_and_scopes_recall_since_until() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let old = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Sailing regatta in Cascais with the whole family.",
            "--event-time",
            "2019-06-01T10:00:00Z",
        ],
    );
    assert_eq!(old["data"]["event_time"], "2019-06-01T10:00:00+00:00");
    let recent = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Sailing regatta in Cascais with the new crew.",
            "--event-time",
            "2026-06-01",
        ],
    );

    assert_eq!(recent["data"]["event_time"], "2026-06-01T00:00:00+00:00");

    let contents = |v: &Value| -> Vec<String> {
        v["data"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["content"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    // --since after the old event: only the recent memory survives, even
    // though both rows were created seconds ago.
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "recall",
            "sailing regatta Cascais",
            "--since",
            "2025-01-01T00:00:00Z",
        ],
    );
    let found = contents(&v);
    assert!(
        found.iter().any(|c| c.contains("new crew")),
        "the 2026 event must answer a --since 2025 recall, got {found:?}"
    );
    assert!(
        !found.iter().any(|c| c.contains("whole family")),
        "a 2019 event must not answer a --since 2025 recall, got {found:?}"
    );

    // --until before the recent event flips the result.
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "recall",
            "sailing regatta Cascais",
            "--until",
            "2020-01-01T00:00:00Z",
        ],
    );
    let found = contents(&v);
    assert!(found.iter().any(|c| c.contains("whole family")), "{found:?}");
    assert!(!found.iter().any(|c| c.contains("new crew")), "{found:?}");
}

#[test]
fn event_time_rejects_unparseable_dates() {
    let (_tmp, mut cmd) = with_isolated_home();
    cmd.args(["remember", "x", "--event-time", "last tuesday", "--json"])
        .assert()
        .failure()
        .code(3);
}

/// Store three memories with no extractor available — the pre-activation
/// state of a real store — then backfill them.
fn seed_unextracted(tmp: &tempfile::TempDir) {
    for content in [
        "Ana moved to Lisbon last March.",
        "Ana works at Acme Corp.",
        "Weather was pleasant, nothing worth filing.",
    ] {
        cmd_in(tmp)
            .args(["remember", content, "--kb", "personal", "--json"])
            .assert()
            .success();
    }
}

#[test]
fn facts_backfill_dry_run_reports_work_without_calling_an_llm() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed_unextracted(&tmp);

    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(v["data"]["dry_run"], true);
    assert_eq!(v["data"]["memories_total"], 3);
    assert_eq!(v["data"]["pending"], 3);
    assert_eq!(v["data"]["would_process"], 3);
    assert_eq!(v["data"]["llm_available"], false);
    assert_eq!(v["data"]["memories"].as_array().unwrap().len(), 3);

    // --limit bounds the preview.
    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "backfill", "--kb", "personal", "--dry-run", "--limit", "1"],
    );
    assert_eq!(v["data"]["would_process"], 1);
    assert_eq!(v["data"]["pending"], 3);

    // Nothing was written: a dry run is still a dry run.
    let facts = run_json(
        &mut cmd_in(&tmp),
        &["facts", "list", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(facts["data"]["count"], 0);
}

#[test]
fn facts_backfill_without_a_key_refuses_with_exit_2() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed_unextracted(&tmp);
    let assert = cmd_in(&tmp)
        .args(["facts", "backfill", "--kb", "personal", "--json"])
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("OPENROUTER_API_KEY"),
        "the refusal must name the missing key, got: {err}"
    );
}

#[test]
fn facts_backfill_is_idempotent_oldest_first_and_resumable() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed_unextracted(&tmp);

    // First pass, bounded: the two oldest memories only.
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["facts", "backfill", "--kb", "personal", "--limit", "2"],
    );
    assert_eq!(v["data"]["processed"], 2);
    assert_eq!(v["data"]["facts_added"], 2);
    assert_eq!(v["data"]["failed"], 0);
    assert_eq!(v["data"]["remaining_pending"], 1);

    // Second pass picks up exactly where the first stopped, including the
    // memory that legitimately yields no facts.
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(v["data"]["processed"], 1);
    assert_eq!(v["data"]["facts_added"], 0);
    assert_eq!(v["data"]["no_facts_found"], 1);
    assert_eq!(v["data"]["remaining_pending"], 0);

    // Third pass is a no-op — no re-extraction, no duplicate facts.
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(v["data"]["processed"], 0);
    assert_eq!(v["data"]["facts_added"], 0);

    let facts = run_json(
        &mut cmd_in(&tmp),
        &["facts", "list", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(facts["data"]["count"], 2);
    let predicates: Vec<&str> = facts["data"]["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["predicate"].as_str().unwrap())
        .collect();
    assert!(predicates.contains(&"lives_in"));
    assert!(predicates.contains(&"works_at"));
}

#[test]
fn facts_backfill_skips_memories_whose_facts_arrived_at_write_time() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["remember", "Ana moved to Lisbon last March.", "--kb", "personal"],
    );
    let v = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(v["data"]["pending"], 0);
    assert_eq!(v["data"]["already_processed"], 1);
}

#[test]
fn facts_backfill_scopes_to_kb_and_diary() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    cmd_in(&tmp)
        .args(["remember", "Work note about Acme.", "--kb", "personal", "--diary", "work", "--json"])
        .assert()
        .success();
    cmd_in(&tmp)
        .args(["remember", "Home note about Lisbon.", "--kb", "personal", "--json"])
        .assert()
        .success();
    cmd_in(&tmp)
        .args(["remember", "Other kb note.", "--kb", "elsewhere", "--json"])
        .assert()
        .success();

    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "backfill", "--kb", "personal", "--diary", "work", "--dry-run"],
    );
    assert_eq!(v["data"]["memories_total"], 1);
    assert_eq!(v["data"]["would_process"], 1);
}
