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
    assert!(v["data"]["count"].is_number());
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
fn memory_aliases_roundtrip_and_default_no_fact_extraction() {
    let (tmp, mut add) = with_isolated_home();
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
