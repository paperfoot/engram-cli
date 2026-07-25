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
fn remember_with_facts(
    tmp: &tempfile::TempDir,
    content: &str,
    facts: &str,
    autonomy: &str,
) -> Value {
    let out = cmd_in(tmp)
        .args([
            "remember",
            content,
            "--facts-json",
            facts,
            "--autonomy",
            autonomy,
        ])
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

/// The manifest is the only thing most agents ever read, so a stale example is
/// a bug that ships straight into someone's tool call. Every example must at
/// least parse against the real clap definition.
#[test]
fn agent_info_examples_are_all_real_commands() {
    let mut cmd = engram();
    let out = cmd
        .arg("agent-info")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let examples = v["examples"].as_array().expect("examples[]");
    assert!(examples.len() >= 10, "manifest lost its examples");

    for example in examples {
        let line = example.as_str().unwrap();
        let args = shell_split(line);
        assert_eq!(
            args[0], "engram",
            "example is not an engram command: {line}"
        );
        // `--help` makes clap validate the subcommand path and flag names
        // without executing anything or touching the network.
        let mut probe = engram();
        probe.args(&args[1..]).arg("--help");
        let status = probe.assert();
        assert!(
            status.get_output().status.success(),
            "manifest example does not parse: {line}\n{}",
            String::from_utf8_lossy(&status.get_output().stderr)
        );
    }
}

/// Minimal POSIX-ish splitter: handles the single and double quotes the
/// manifest examples use. Not a general shell parser.
fn shell_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
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

    // A fresh install reports every target as newly installed.
    let dirs = v["data"]["directories"].as_array().expect("directories[]");
    assert_eq!(dirs.len(), 4);
    assert!(dirs.iter().all(|d| d["status"] == "installed"), "{dirs:?}");
}

#[test]
fn skill_install_is_idempotent_and_reports_unchanged_targets() {
    let (tmp, mut first) = with_isolated_home();
    first
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success();

    let out = cmd_in(&tmp)
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let dirs = v["data"]["directories"].as_array().expect("directories[]");
    assert!(dirs.iter().all(|d| d["status"] == "unchanged"), "{dirs:?}");
    assert_eq!(v["metadata"]["unchanged"], 4);
    assert_eq!(v["metadata"]["installed"], 0);
}

/// The footgun this guards: `skill install` used to fs::write over all four
/// targets unconditionally, silently destroying a hand-tuned SKILL.md.
#[test]
fn skill_install_refuses_to_clobber_a_locally_modified_skill() {
    let (tmp, mut first) = with_isolated_home();
    first
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success();

    let edited = tmp.path().join(".claude/skills/engram/SKILL.md");
    std::fs::write(&edited, "# my hand-written skill\n").unwrap();

    let assert = cmd_in(&tmp)
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .failure()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let err: Value = serde_json::from_str(stderr.trim()).expect("JSON error on stderr");
    assert_eq!(err["error"]["exit_code"], 2);
    // The message must name the exact path so the user can go rescue it.
    let message = err["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(edited.to_str().unwrap()),
        "error did not name the path: {message}"
    );
    assert!(message.contains("--force"), "no recovery hint: {message}");

    // Nothing was written — not even to the untouched targets.
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "# my hand-written skill\n"
    );
}

#[test]
fn skill_install_force_overwrites_a_modified_skill() {
    let (tmp, mut first) = with_isolated_home();
    first
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success();

    let edited = tmp.path().join(".claude/skills/engram/SKILL.md");
    let pristine = std::fs::read_to_string(&edited).unwrap();
    std::fs::write(&edited, "# my hand-written skill\n").unwrap();

    let out = cmd_in(&tmp)
        .arg("--json")
        .args(["skill", "install", "--force"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["forced"], true);
    let dirs = v["data"]["directories"].as_array().unwrap();
    let claude = dirs
        .iter()
        .find(|d| d["skill_path"] == edited.to_str().unwrap())
        .expect("claude target present");
    assert_eq!(claude["status"], "installed");
    // The other three were already correct and stay reported as unchanged.
    assert_eq!(
        dirs.iter().filter(|d| d["status"] == "unchanged").count(),
        3
    );
    assert_eq!(std::fs::read_to_string(&edited).unwrap(), pristine);
}

/// A target that never had the skill installs normally even while a sibling
/// is modified — but only once the modified one is resolved.
#[test]
fn skill_install_reports_absent_targets_as_installed() {
    let (tmp, mut first) = with_isolated_home();
    first
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success();
    std::fs::remove_file(tmp.path().join(".gemini/skills/engram/SKILL.md")).unwrap();

    let out = cmd_in(&tmp)
        .arg("--json")
        .args(["skill", "install"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["metadata"]["installed"], 1);
    assert_eq!(v["metadata"]["unchanged"], 3);
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

/// The live-store bug this closes: memories written with `--kb <name>` were
/// invisible to `kb list` because nothing registered the KB.
#[test]
fn remembering_into_a_new_kb_registers_it_for_kb_list() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args([
        "remember",
        "Practised the bridge of Bohemian Rhapsody.",
        "--kb",
        "songs",
        "--no-facts",
        "--json",
    ])
    .assert()
    .success();

    let out = cmd_in(&tmp)
        .args(["kb", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    let kbs = v["data"]["knowledge_bases"].as_array().unwrap();
    let songs = kbs
        .iter()
        .find(|k| k["name"] == "songs")
        .expect("songs KB registered by the write");
    assert_eq!(songs["memory_count"], 1);
}

#[test]
fn kb_repair_is_a_no_op_on_a_healthy_store() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args([
        "remember",
        "A decision worth keeping.",
        "--kb",
        "business",
        "--no-facts",
        "--json",
    ])
    .assert()
    .success();

    for args in [
        vec!["kb", "repair", "--dry-run", "--json"],
        vec!["kb", "repair", "--json"],
    ] {
        let out = cmd_in(&tmp)
            .args(&args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v = parse_stdout(&out);
        assert_eq!(v["status"], "success");
        assert_eq!(v["data"]["unregistered_kbs"].as_array().unwrap().len(), 0);
        assert_eq!(v["data"]["registered"], 0);
    }

    // A healthy store also passes the integrity gate that surfaces the fault.
    let out = cmd_in(&tmp)
        .args(["doctor", "--integrity", "--json"])
        .assert()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(
        v["data"]["integrity"]["unregistered_kbs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

/// `kb repair` must not undo `kb delete` — deletion removes the registry row
/// deliberately, and repair is only for KBs that never had one.
#[test]
fn kb_repair_leaves_a_deleted_kb_deleted() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args(["kb", "create", "throwaway", "--json"])
        .assert()
        .success();
    cmd_in(&tmp)
        .args([
            "remember",
            "Temporary note in a doomed KB.",
            "--kb",
            "throwaway",
            "--no-facts",
            "--json",
        ])
        .assert()
        .success();
    cmd_in(&tmp)
        .args(["kb", "delete", "throwaway", "--confirm", "--json"])
        .assert()
        .success();

    let out = cmd_in(&tmp)
        .args(["kb", "repair", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["registered"], 0);

    let out = cmd_in(&tmp)
        .args(["kb", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert!(
        !v["data"]["knowledge_bases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["name"] == "throwaway"),
        "kb repair resurrected a deleted KB"
    );
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
            "recall",
            "rapamycin mTORC1",
            "--diary",
            "*",
            "--until",
            "2020-01-01T00:00:00Z",
            "--json",
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
            "recall",
            "rapamycin mTORC1",
            "--diary",
            "*",
            "--since",
            "2020-01-01T00:00:00Z",
            "--json",
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
        .args([
            "remember",
            "Rapamycin extends mouse lifespan.",
            "--allow-duplicate",
            "--json",
        ])
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
        c.env(
            "GEMINI_API_KEY",
            "fake-key-not-used-because-budget-blocks-first",
        );
        c.env("HOME", tmp.path());
        c.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
        c.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
        c.env("XDG_CACHE_HOME", tmp.path().join(".cache"));
    };
    // First, record some usage so spent >= 0 and set a $0 cap → immediately over.
    let mut setb = Command::cargo_bin("engram").unwrap();
    env(&mut setb);
    setb.args([
        "budget",
        "set",
        "--kb",
        "capped",
        "--daily-usd",
        "0",
        "--json",
    ])
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
    assert!(facts[0]["valid_from"]
        .as_str()
        .unwrap()
        .starts_with("2026-03-01"));
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
    assert_eq!(
        conflicts.len(),
        1,
        "LWW slot update must report the supersession"
    );
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
    let facts = parse_stdout(&out)["data"]["facts"]
        .as_array()
        .unwrap()
        .clone();
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
    let facts = parse_stdout(&out)["data"]["facts"]
        .as_array()
        .unwrap()
        .clone();
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
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        1
    );
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
        &[
            "remember",
            "Ana moved to Lisbon last March.",
            "--no-extract-facts",
        ],
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
        &[
            "remember",
            "Ana moved to Lisbon last March.",
            "--extract-facts",
        ],
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
    assert!(
        found.iter().any(|c| c.contains("whole family")),
        "{found:?}"
    );
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
        &[
            "facts",
            "backfill",
            "--kb",
            "personal",
            "--dry-run",
            "--limit",
            "1",
        ],
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
        &[
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
        ],
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
        .args([
            "remember",
            "Work note about Acme.",
            "--kb",
            "personal",
            "--diary",
            "work",
            "--json",
        ])
        .assert()
        .success();
    cmd_in(&tmp)
        .args([
            "remember",
            "Home note about Lisbon.",
            "--kb",
            "personal",
            "--json",
        ])
        .assert()
        .success();
    cmd_in(&tmp)
        .args(["remember", "Other kb note.", "--kb", "elsewhere", "--json"])
        .assert()
        .success();

    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "facts",
            "backfill",
            "--kb",
            "personal",
            "--diary",
            "work",
            "--dry-run",
        ],
    );
    assert_eq!(v["data"]["memories_total"], 1);
    assert_eq!(v["data"]["would_process"], 1);
}

// ---------------------------------------------------------------------------
// People graph — the networking surface. All of it must work offline with no
// API key: the deterministic extractor is the floor, not a fallback.
// ---------------------------------------------------------------------------

/// Ingest one networking note and compile it, returning the isolated home.
fn networking_kb() -> tempfile::TempDir {
    let (tmp, mut ingest) = with_isolated_home();
    let fixture = tmp.path().join("network.md");
    std::fs::write(
        &fixture,
        "Avi Roy introduced me to Noemi Kostova at the Longevity Summit. \
         Noemi Kostova is married to Sam Lee. \
         I'm seeing Noemi on Monday. She's bringing Avi Roy. \
         Noemi's deck was excellent. \
         Sam Lee works with Avi Roy on the fund.",
    )
    .unwrap();
    ingest
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--kb",
            "network",
            "--mode",
            "general",
            "--compile",
            "evidence",
            "--json",
        ])
        .assert()
        .success();
    tmp
}

fn json_cmd(tmp: &tempfile::TempDir, args: &[&str]) -> Value {
    let out = cmd_in(tmp)
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

#[test]
fn compile_types_people_and_rejects_contraction_junk() {
    let tmp = networking_kb();
    let v = json_cmd(
        &tmp,
        &["entities", "list", "--kb", "network", "--limit", "100"],
    );
    let entities = v["data"]["entities"].as_array().unwrap();
    let names: Vec<&str> = entities
        .iter()
        .filter_map(|e| e["canonical_name"].as_str())
        .collect();

    for junk in ["I'm", "She's", "It's", "Monday", "The"] {
        assert!(!names.contains(&junk), "junk entity {junk} in {names:?}");
    }
    // "Noemi's" folds into its owner; no rival possessive node.
    assert!(
        !names.iter().any(|n| n.ends_with("'s")),
        "possessive entity survived: {names:?}"
    );

    let people: Vec<&str> = entities
        .iter()
        .filter(|e| e["kind"] == "person")
        .filter_map(|e| e["canonical_name"].as_str())
        .collect();
    for expected in ["Avi Roy", "Noemi Kostova", "Sam Lee"] {
        assert!(
            people.contains(&expected),
            "{expected} not typed person: {people:?}"
        );
    }
}

#[test]
fn people_list_returns_person_entities_with_relations() {
    let tmp = networking_kb();
    let v = json_cmd(&tmp, &["people", "list", "--kb", "network"]);
    assert_eq!(v["status"], "success");
    let people = v["data"]["people"].as_array().unwrap();
    assert!(!people.is_empty(), "expected people in {v}");
    assert!(people
        .iter()
        .all(|p| p["mentions"].as_u64().unwrap_or(0) >= 1));

    let noemi = people
        .iter()
        .find(|p| p["name"] == "Noemi Kostova")
        .expect("Noemi Kostova listed");
    let predicates: Vec<&str> = noemi["top_relations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["predicate"].as_str())
        .collect();
    assert!(
        predicates.contains(&"introduced_by") || predicates.contains(&"married_to"),
        "expected typed relations, got {predicates:?}"
    );
    // Alias spellings fold onto the person instead of splitting the node.
    let aliases: Vec<&str> = noemi["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a.as_str())
        .collect();
    assert!(
        aliases.iter().any(|a| a.starts_with("Noemi")),
        "{aliases:?}"
    );
}

#[test]
fn people_show_reports_predicate_provenance_and_snippets() {
    let tmp = networking_kb();
    let v = json_cmd(
        &tmp,
        &["people", "show", "Noemi Kostova", "--kb", "network"],
    );
    assert_eq!(v["status"], "success");
    assert_eq!(v["data"]["person"]["kind"], "person");

    let relations = v["data"]["relations"].as_array().unwrap();
    let introduced = relations
        .iter()
        .find(|r| r["predicate"] == "introduced_by")
        .expect("introduced_by edge");
    assert_eq!(introduced["subject"], "Noemi Kostova");
    assert_eq!(introduced["object"], "Avi Roy");

    // Every typed edge must be checkable: a memory id and the source sentence.
    let source = introduced["sources"]
        .as_array()
        .and_then(|s| s.first())
        .expect("edge provenance");
    assert!(source["memory_id"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(source["chunk_id"].as_str().is_some_and(|c| !c.is_empty()));
    assert!(source["sentence"]
        .as_str()
        .unwrap_or_default()
        .contains("introduced"));

    let memories = v["data"]["memories"].as_array().unwrap();
    assert!(!memories.is_empty(), "expected memory snippets");
    assert!(memories
        .iter()
        .all(|m| m["memory_id"].as_str().is_some_and(|s| !s.is_empty())));
}

#[test]
fn people_show_resolves_an_alias() {
    let tmp = networking_kb();
    // "Noemi" was written bare in one sentence; it must resolve to the person.
    let v = json_cmd(&tmp, &["people", "show", "noemi", "--kb", "network"]);
    assert_eq!(v["data"]["person"]["name"], "Noemi Kostova");
}

#[test]
fn people_show_unknown_name_is_bad_input() {
    let tmp = networking_kb();
    cmd_in(&tmp)
        .args([
            "people",
            "show",
            "Nobody At All",
            "--kb",
            "network",
            "--json",
        ])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn graph_neighbors_is_person_centric() {
    let tmp = networking_kb();
    let v = json_cmd(
        &tmp,
        &["graph", "neighbors", "Noemi Kostova", "--kb", "network"],
    );
    assert_eq!(v["data"]["resolved"]["kind"], "person");
    let neighbors = v["data"]["neighbors"].as_array().unwrap();
    let avi = neighbors
        .iter()
        .find(|n| n["name"] == "Avi Roy")
        .expect("Avi Roy is a neighbor");
    assert_eq!(avi["kind"], "person");
    assert!(avi["relations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["predicate"] == "introduced_by"));

    // --typed-only drops the weak co-occurrence edges.
    let typed = json_cmd(
        &tmp,
        &[
            "graph",
            "neighbors",
            "Noemi Kostova",
            "--kb",
            "network",
            "--typed-only",
        ],
    );
    assert!(typed["data"]["relations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["predicate"] != "co_occurs_with"));
}

#[test]
fn recompiling_converges_instead_of_accumulating() {
    let tmp = networking_kb();
    let before = json_cmd(
        &tmp,
        &["entities", "list", "--kb", "network", "--limit", "500"],
    );
    let people_before = json_cmd(&tmp, &["people", "list", "--kb", "network"]);

    json_cmd(&tmp, &["compile", "--kb", "network", "--all"]);
    json_cmd(&tmp, &["compile", "--kb", "network", "--all"]);

    let after = json_cmd(
        &tmp,
        &["entities", "list", "--kb", "network", "--limit", "500"],
    );
    let people_after = json_cmd(&tmp, &["people", "list", "--kb", "network"]);

    let fingerprint = |v: &Value, key: &str, field: &str| -> Vec<String> {
        let mut out: Vec<String> = v["data"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                format!(
                    "{}:{}",
                    e[field].as_str().unwrap_or(""),
                    e["mention_count"]
                        .as_u64()
                        .or_else(|| e["mentions"].as_u64())
                        .unwrap_or(0)
                )
            })
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        fingerprint(&before, "entities", "canonical_name"),
        fingerprint(&after, "entities", "canonical_name"),
        "recompile must replace derived entities, not accumulate them"
    );
    assert_eq!(
        fingerprint(&people_before, "people", "name"),
        fingerprint(&people_after, "people", "name")
    );

    // Edge weight and provenance must not creep up across recompiles either.
    let edges = json_cmd(
        &tmp,
        &["people", "show", "Noemi Kostova", "--kb", "network"],
    );
    let introduced = edges["data"]["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["predicate"] == "introduced_by")
        .expect("introduced_by edge");
    assert_eq!(introduced["sources"].as_array().unwrap().len(), 1);
    assert!(introduced["weight"].as_f64().unwrap() <= 1.0);
}

/// The personal-memory suite must run end to end with no API keys and produce
/// a well-formed report. Deliberately asserts NOTHING about score levels: the
/// retrieval paths it measures (cross-KB, event-time temporal) are being fixed
/// in parallel, and the point of the suite is to record where they stand.
#[test]
fn bench_personal_runs_offline_and_reports_sane_structure() {
    let (_tmp, mut cmd) = with_isolated_home();
    let out = cmd
        .args(["bench", "personal", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["status"], "success");
    let d = &v["data"];
    assert_eq!(d["suite"], "personal");
    assert_eq!(d["profile"], "offline");
    assert_eq!(d["reranker"], "passthrough");

    // The dataset actually reached the store through the write path.
    let seeded = d["memories_seeded"].as_u64().unwrap();
    assert!((40..=60).contains(&seeded), "seeded {seeded} memories");
    assert!(d["chunks_seeded"].as_u64().unwrap() >= seeded);
    assert_eq!(d["kbs"].as_array().unwrap().len(), 4);

    let queries = d["queries_evaluated"].as_u64().unwrap() as usize;
    assert!((30..=50).contains(&queries), "evaluated {queries} queries");
    let per_query = d["per_query"].as_array().unwrap();
    assert_eq!(per_query.len(), queries);

    // Every category appears exactly once in the breakdown.
    let categories: Vec<&str> = d["per_category"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["category"].as_str().unwrap())
        .collect();
    for expected in [
        "person_fact",
        "temporal",
        "cross_kb",
        "paraphrase_dedup",
        "multi_fact",
        "negative_control",
    ] {
        assert_eq!(
            categories.iter().filter(|c| **c == expected).count(),
            1,
            "category {expected} missing from breakdown"
        );
    }

    // Metrics are well-formed rates, not NaN or out-of-range garbage.
    let m = &d["metrics"];
    for key in [
        "recall_at_1",
        "recall_at_3",
        "recall_at_5",
        "recall_at_10",
        "mrr_at_10",
        "gold_coverage_at_10",
        "abstention_rate",
    ] {
        let value = m[key].as_f64().unwrap_or_else(|| panic!("{key} missing"));
        assert!((0.0..=1.0).contains(&value), "{key} out of range: {value}");
    }
    assert_eq!(
        m["queries_scored"].as_u64().unwrap() + m["negative_controls"].as_u64().unwrap(),
        queries as u64
    );
    assert!(m["negative_controls"].as_u64().unwrap() >= 1);

    // Negative controls carry no gold; every other query does.
    for q in per_query {
        let gold = q["gold"].as_array().unwrap();
        if q["category"] == "negative_control" {
            assert!(
                gold.is_empty(),
                "negative control {} has gold",
                q["query_id"]
            );
        } else {
            assert!(!gold.is_empty(), "query {} has no gold", q["query_id"]);
        }
        assert!(q["latency_ms"].is_number());
    }

    // Headline metrics are production behaviour (recall spans every KB).
    // Hint-less queries were additionally rerun pinned to the legacy
    // `kb=default` floor — the regression tripwire, not the ceiling.
    let cmp = &d["legacy_default_comparison"];
    assert!(cmp["queries"].as_u64().unwrap() >= 1);
    let hintless = per_query
        .iter()
        .filter(|q| q["legacy_default"].is_object())
        .count();
    assert_eq!(cmp["queries"].as_u64().unwrap() as usize, hintless);
    // The legacy floor must not beat production on the queries it was run for
    // — if it does, the all-KB default has stopped paying for itself.
    assert!(
        cmp["recall_at_10"].as_f64().unwrap() <= m["recall_at_10"].as_f64().unwrap() + 1e-6,
        "legacy kb=default floor outscored the production default: {cmp}"
    );
    // The comparison run is a genuinely different scope, so it must not be a
    // verbatim copy of the headline row for every hint-less query.
    let differs = per_query.iter().any(|q| {
        q["legacy_default"].is_object() && q["legacy_default"]["recall_at_10"] != q["recall_at_10"]
    });
    assert!(
        differs,
        "legacy_default mirrored the production run on every query — the comparison is redundant"
    );
}

#[test]
fn bench_personal_is_deterministic_and_saves_a_report() {
    let (tmp, mut cmd) = with_isolated_home();
    let report_path = tmp.path().join("reports/personal.json");
    let out = cmd
        .args([
            "bench",
            "personal",
            "--save",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let first = parse_stdout(&out);
    assert!(report_path.exists(), "--save wrote no report");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&report_path).unwrap()).expect("valid report JSON");
    assert_eq!(saved["suite"], "personal");
    // The saved file holds the same report as stdout. Compared field by field
    // rather than as raw JSON: the envelope round-trips f32 scores through
    // f64, so 0.14285715 on disk prints as 0.1428571492433548 on stdout.
    let saved_rows = saved["per_query"].as_array().unwrap();
    let stdout_rows = first["data"]["per_query"].as_array().unwrap();
    assert_eq!(saved_rows.len(), stdout_rows.len());
    for (a, b) in saved_rows.iter().zip(stdout_rows) {
        assert_eq!(a["query_id"], b["query_id"]);
        assert_eq!(a["gold"], b["gold"]);
        assert_eq!(a["first_gold_rank"], b["first_gold_rank"]);
        assert_eq!(a["top_retrieved"], b["top_retrieved"]);
    }

    let out2 = cmd_in(&tmp)
        .args(["bench", "personal", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second = parse_stdout(&out2);
    // The offline profile is the deterministic hash embedder: identical scores
    // and identical rankings on every rerun.
    assert_eq!(
        first["data"]["metrics"]["mrr_at_10"],
        second["data"]["metrics"]["mrr_at_10"]
    );
    assert_eq!(
        first["data"]["per_category"],
        second["data"]["per_category"]
    );
    for (a, b) in stdout_rows
        .iter()
        .zip(second["data"]["per_query"].as_array().unwrap())
    {
        assert_eq!(a["query_id"], b["query_id"]);
        assert_eq!(a["top_retrieved"], b["top_retrieved"]);
        assert_eq!(a["mrr_at_10"], b["mrr_at_10"]);
    }
}

#[test]
fn bench_personal_cloud_profile_without_keys_is_config_error() {
    let (_tmp, mut cmd) = with_isolated_home();
    cmd.args(["bench", "personal", "--profile", "cloud_quality", "--json"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn bench_personal_rejects_an_unknown_profile() {
    let (_tmp, mut cmd) = with_isolated_home();
    cmd.args(["bench", "personal", "--profile", "nonsense", "--json"])
        .assert()
        .failure()
        .code(3);
}

// ---------------------------------------------------------------------------
// Verification-pass regressions. Each test below reproduces a defect an
// adversarial review found in a live sandbox; each one fails without its fix.
// ---------------------------------------------------------------------------

/// Store a memory and return its id.
fn remember_returning_id(tmp: &tempfile::TempDir, args: &[&str]) -> String {
    let out = cmd_in(tmp)
        .arg("remember")
        .args(args)
        .args(["--no-facts", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)["data"]["id"]
        .as_str()
        .expect("memory id")
        .to_string()
}

/// FINDING 1 (critical): `forget` must not widen what another diary can see.
///
/// `forget` is a soft delete that leaves the KB's wiki pages and entity
/// aggregates standing until the next compile. The isolation gate counted only
/// live memories, so deleting the last memory of the private diary made the
/// shared KB look like it belonged wholly to `work` — and a `--diary work`
/// recall started returning wiki text paraphrasing the just-deleted private
/// memory. Deleting content must never publish it.
#[test]
fn forget_does_not_unseal_another_diarys_derived_content() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let private_id = remember_returning_id(
        &tmp,
        &[
            "Nadia consulted a psychiatrist about 3 ketamine infusions last winter.",
            "--kb",
            "shared",
            "--diary",
            "personal",
        ],
    );
    remember_returning_id(
        &tmp,
        &[
            "Standup moved to Tuesday mornings at 9 am for the whole team.",
            "--kb",
            "shared",
            "--diary",
            "work",
        ],
    );

    // Compile builds the derived layer: wiki pages that quote chunk text
    // verbatim, plus entity/relation aggregates.
    cmd_in(&tmp)
        .args(["compile", "--kb", "shared", "--all", "--json"])
        .assert()
        .success();

    let leaked = |v: &Value| -> Vec<String> {
        contents(v)
            .into_iter()
            .filter(|c| {
                let c = c.to_lowercase();
                c.contains("ketamine") || c.contains("psychiatrist") || c.contains("nadia")
            })
            .collect()
    };

    // BEFORE forget: the mixed KB is gated, and the gate says so.
    let before = recall_json(
        &tmp,
        &[
            "ketamine psychiatrist Nadia",
            "--diary",
            "work",
            "--mode",
            "agent",
        ],
    );
    assert!(
        leaked(&before).is_empty(),
        "pre-forget leak into diary work: {:?}",
        leaked(&before)
    );
    assert!(
        before["metadata"]["diary_isolation"]["excluded_candidates"].is_object(),
        "the gate must report what it withheld: {}",
        before["metadata"]
    );

    // The owning diary can still read its own memory.
    let owner = recall_json(
        &tmp,
        &[
            "ketamine psychiatrist Nadia",
            "--diary",
            "personal",
            "--mode",
            "agent",
        ],
    );
    assert!(
        !leaked(&owner).is_empty(),
        "the personal diary must still recall its own content"
    );

    cmd_in(&tmp)
        .args(["forget", &private_id, "--confirm", "--json"])
        .assert()
        .success();

    // AFTER forget: still gated. The wiki page and entity rows that paraphrase
    // the deleted memory have not been purged, so the KB is not pure `work`.
    let after = recall_json(
        &tmp,
        &[
            "ketamine psychiatrist Nadia",
            "--diary",
            "work",
            "--mode",
            "agent",
        ],
    );
    assert!(
        leaked(&after).is_empty(),
        "deleting the private memory PUBLISHED it to diary work: {:?}",
        leaked(&after)
    );
    assert!(
        after["metadata"]["diary_isolation"]["excluded_candidates"].is_object(),
        "the withheld sources must still be reported after forget: {}",
        after["metadata"]
    );
    assert_eq!(after["metadata"]["diary_isolation"]["diary"], "work");

    // The gate over-blocks on purpose, so it needs a real exit: a full compile
    // rebuilds the derived layer from live chunks only, genuinely purging the
    // deleted memory — after which the KB is honestly pure `work` and stops
    // being gated. Without this the KB would be blocked forever.
    cmd_in(&tmp)
        .args(["compile", "--kb", "shared", "--all", "--json"])
        .assert()
        .success();
    let recompiled = recall_json(
        &tmp,
        &[
            "ketamine psychiatrist Nadia",
            "--diary",
            "work",
            "--mode",
            "agent",
        ],
    );
    assert!(
        leaked(&recompiled).is_empty(),
        "the recompile must PURGE the deleted memory, not expose it: {:?}",
        leaked(&recompiled)
    );
    assert!(
        recompiled["metadata"]["diary_isolation"].is_null(),
        "once the KB really is single-diary, nothing should be withheld: {}",
        recompiled["metadata"]
    );
}

/// FINDING 2 (major): the "already extracted" marker is a promise that the
/// facts are in the store, so it may only be written after they are.
///
/// Recording it straight off the LLM response meant a persistence failure was
/// merely warned about while the memory was still marked done — and
/// `facts backfill` treats any marker row as processed, so those facts were
/// lost permanently and invisibly.
#[test]
fn a_failed_fact_write_leaves_the_memory_pending_for_backfill() {
    let tmp = tempfile::tempdir().expect("tmp dir");

    let out = cmd_with_extractor(&tmp, LISBON_SCRIPT)
        .env("ENGRAM_FACT_WRITE_FAIL", "Ana")
        .args([
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    // The memory is stored, the failure is loud, and nothing was persisted.
    assert_eq!(v["data"]["stored"], true);
    assert_eq!(v["data"]["fact_extraction"], "failed_partial_persist");
    assert_eq!(v["data"]["facts_added"], 0);
    assert!(v["data"]["fact_extraction_note"]
        .as_str()
        .unwrap_or_default()
        .contains("facts backfill"));

    // The memory must still be in the backfill queue.
    let pending = run_json(
        &mut cmd_in(&tmp),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(
        pending["data"]["pending"], 1,
        "a memory whose facts never persisted must stay pending: {}",
        pending["data"]
    );

    // And a later backfill actually recovers the facts.
    let done = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(done["data"]["processed"], 1);
    assert_eq!(done["data"]["facts_added"], 1);

    let facts = run_json(&mut cmd_in(&tmp), &["facts", "list", "--kb", "personal"]);
    assert_eq!(facts["data"]["count"], 1);
    assert_eq!(facts["data"]["facts"][0]["predicate"], "lives_in");
}

/// FINDING 3 (major): `--since/--until` bound a memory's event time, and only
/// raw chunks have one. Wiki pages, claims and entity aggregates were never
/// filtered, so a window could return a wiki page compiled from a 2010 memory.
#[test]
fn a_temporal_window_excludes_derived_sources_and_says_so() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    cmd_in(&tmp)
        .args([
            "remember",
            "Marguerite presented the vermilion protocol at the Kyoto workshop.",
            "--kb",
            "archive",
            "--event-time",
            "2010-05-04",
            "--no-facts",
            "--json",
        ])
        .assert()
        .success();
    cmd_in(&tmp)
        .args(["compile", "--kb", "archive", "--all", "--json"])
        .assert()
        .success();

    let kinds = |v: &Value| -> Vec<String> {
        v["data"]["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| r["kind"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    // Control: the wiki page exists and matches the query.
    let control = recall_json(&tmp, &["Marguerite vermilion protocol", "--mode", "wiki"]);
    assert!(
        kinds(&control).iter().any(|k| k == "wiki_page"),
        "expected a wiki page without a window: {:?}",
        kinds(&control)
    );
    assert!(
        control["metadata"]["temporal_scope"].is_null(),
        "no window means no temporal note"
    );

    // Windowed: the 2010 page is not admissible under a 2025+ window.
    let windowed = recall_json(
        &tmp,
        &[
            "Marguerite vermilion protocol",
            "--mode",
            "wiki",
            "--since",
            "2025-01-01",
        ],
    );
    assert!(
        !kinds(&windowed).iter().any(|k| k == "wiki_page"),
        "a wiki page compiled from a 2010 memory leaked through --since: {:?}",
        kinds(&windowed)
    );
    let excluded = windowed["metadata"]["temporal_scope"]["excluded_sources"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        excluded.iter().any(|s| s == "wiki"),
        "the exclusion must be reported, not silent: {}",
        windowed["metadata"]["temporal_scope"]
    );

    // Agent mode withholds every derived source, all reported.
    let agent = recall_json(
        &tmp,
        &[
            "Marguerite vermilion protocol",
            "--mode",
            "agent",
            "--since",
            "2025-01-01",
        ],
    );
    let excluded = agent["metadata"]["temporal_scope"]["excluded_sources"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for source in ["claim", "entity", "wiki"] {
        assert!(
            excluded.iter().any(|s| s == source),
            "{source} exclusion missing from {}",
            agent["metadata"]["temporal_scope"]
        );
    }
}

/// FINDING 4 (major): `people show --kb '*'` used a LIMIT-1 lookup with no
/// ORDER BY, so SQLite returned one arbitrary KB's row and every other KB's
/// relations, aliases and mentions vanished from an explicitly all-KB query.
#[test]
fn people_show_across_every_kb_merges_instead_of_dropping_kbs() {
    let (tmp, _cmd) = with_isolated_home();
    for (kb, text) in [
        (
            "network-a",
            "Avi Roy introduced me to Noemi Kostova at the Longevity Summit.",
        ),
        (
            "network-b",
            "Priya Nair introduced me to Noemi Kostova at the Basel workshop.",
        ),
    ] {
        let fixture = tmp.path().join(format!("{kb}.md"));
        std::fs::write(&fixture, text).unwrap();
        cmd_in(&tmp)
            .args([
                "ingest",
                fixture.to_str().unwrap(),
                "--kb",
                kb,
                "--mode",
                "general",
                "--compile",
                "evidence",
                "--json",
            ])
            .assert()
            .success();
    }

    let v = json_cmd(&tmp, &["people", "show", "Noemi Kostova", "--kb", "*"]);
    let kbs: Vec<&str> = v["data"]["kbs"]
        .as_array()
        .expect("kbs array")
        .iter()
        .filter_map(|k| k.as_str())
        .collect();
    assert!(
        kbs.contains(&"network-a") && kbs.contains(&"network-b"),
        "an all-KB lookup dropped a KB: {kbs:?}"
    );

    // Relations from BOTH KBs, each still carrying its own provenance.
    let relations = v["data"]["relations"].as_array().expect("relations");
    let relation_kbs: Vec<&str> = relations.iter().filter_map(|r| r["kb"].as_str()).collect();
    assert!(
        relation_kbs.contains(&"network-a") && relation_kbs.contains(&"network-b"),
        "relations came from only one KB: {relation_kbs:?}"
    );
    let others: Vec<&str> = relations
        .iter()
        .filter_map(|r| r["other"].as_str())
        .collect();
    assert!(
        others.contains(&"Avi Roy") && others.contains(&"Priya Nair"),
        "both KBs' counterparties must survive the merge: {others:?}"
    );

    // Per-KB grouping keeps the namespaces separable.
    let groups = v["data"]["relations_by_kb"].as_array().expect("groups");
    assert_eq!(groups.len(), 2, "one group per KB: {groups:?}");

    // Mentions are summed, not taken from one arbitrary row.
    let merged = v["data"]["person"]["mentions"].as_u64().unwrap_or(0);
    let single = json_cmd(
        &tmp,
        &["people", "show", "Noemi Kostova", "--kb", "network-a"],
    )["data"]["person"]["mentions"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        merged > single,
        "merged mentions ({merged}) must exceed a single KB's ({single})"
    );
}

/// FINDING 5 (major): `remember --event-time` accepts `YYYY-MM-DD` and its help
/// says it drives `recall --since/--until`, but the reader was RFC3339-only, so
/// the documented round-trip exited 3. A date-only bound now brackets the whole
/// UTC day: `--since` at its start, `--until` at its end.
#[test]
fn recall_since_and_until_accept_a_bare_date() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    cmd_in(&tmp)
        .args([
            "remember",
            "Turmeric harvest logged at the Sintra allotment.",
            "--kb",
            "garden",
            "--event-time",
            "2026-06-15T18:00:00Z",
            "--no-facts",
            "--json",
        ])
        .assert()
        .success();

    let found = |args: &[&str]| -> bool {
        contents(&recall_json(&tmp, args))
            .iter()
            .any(|c| c.contains("Turmeric harvest"))
    };

    // Date-only --since is the start of the day, so the same day matches.
    assert!(found(&["Turmeric harvest Sintra", "--since", "2026-06-15"]));
    // Date-only --until is the END of the day: an 18:00 event is inside it.
    assert!(found(&["Turmeric harvest Sintra", "--until", "2026-06-15"]));
    // Both bounds on the same date select exactly that day.
    assert!(found(&[
        "Turmeric harvest Sintra",
        "--since",
        "2026-06-15",
        "--until",
        "2026-06-15"
    ]));
    // And the window really is a filter, not a no-op.
    assert!(!found(&[
        "Turmeric harvest Sintra",
        "--since",
        "2026-06-16"
    ]));
    assert!(!found(&[
        "Turmeric harvest Sintra",
        "--until",
        "2026-06-14"
    ]));
    // RFC3339 keeps working.
    assert!(found(&[
        "Turmeric harvest Sintra",
        "--since",
        "2026-06-15T00:00:00Z"
    ]));

    // `memory search` is the same engine and takes the same window.
    let out = cmd_in(&tmp)
        .args([
            "memory",
            "search",
            "Turmeric harvest Sintra",
            "--since",
            "2026-06-15",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(contents(&parse_stdout(&out))
        .iter()
        .any(|c| c.contains("Turmeric harvest")));

    // Garbage is still a hard error, never a silent "now".
    for bad in ["last tuesday", "2026-13-45"] {
        cmd_in(&tmp)
            .args(["recall", "anything", "--since", bad, "--json"])
            .assert()
            .failure()
            .code(3);
        cmd_in(&tmp)
            .args(["recall", "anything", "--until", bad, "--json"])
            .assert()
            .failure()
            .code(3);
    }
}

/// FINDING 6a (major): `facts list` defaulted to the KB literally named
/// "default", so facts written to any other KB came back as `count: 0` with
/// exit 0 — which an agent reads as "extraction produced nothing".
#[test]
fn facts_list_spans_every_kb_by_default_and_labels_each_fact() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    cmd_in(&tmp)
        .args([
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0}]"#,
            "--json",
        ])
        .assert()
        .success();

    // No --kb: every KB, and each fact says where it came from.
    let v = run_json(&mut cmd_in(&tmp), &["facts", "list"]);
    assert_eq!(
        v["data"]["count"], 1,
        "an unscoped facts list must find facts in named KBs: {}",
        v["data"]
    );
    assert_eq!(v["data"]["kb"], "*");
    assert_eq!(v["data"]["kb_scope"], "all_kbs");
    assert_eq!(v["data"]["facts"][0]["kb"], "personal");

    // --kb still narrows.
    let scoped = run_json(&mut cmd_in(&tmp), &["facts", "list", "--kb", "personal"]);
    assert_eq!(scoped["data"]["count"], 1);
    assert_eq!(scoped["data"]["kb_scope"], "flag");
    let elsewhere = run_json(&mut cmd_in(&tmp), &["facts", "list", "--kb", "default"]);
    assert_eq!(elsewhere["data"]["count"], 0);
}

/// FINDING 6b (major): `facts backfill` hard-required `--kb`, so catching up
/// the fact layer meant already knowing every namespace ever written to.
/// Omitting it now sweeps every KB, oldest first, writing each memory's facts
/// back into that memory's own KB.
#[test]
fn facts_backfill_without_a_kb_sweeps_every_kb() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    for (kb, content) in [
        ("personal", "Ana moved to Lisbon last March."),
        ("elsewhere", "Ana works at Acme Corp."),
    ] {
        cmd_in(&tmp)
            .args(["remember", content, "--kb", kb, "--json"])
            .assert()
            .success();
    }

    // Dry run with no --kb covers both namespaces and labels each memory.
    let dry = run_json(&mut cmd_in(&tmp), &["facts", "backfill", "--dry-run"]);
    assert_eq!(dry["data"]["kb"], "*");
    assert_eq!(dry["data"]["memories_total"], 2);
    assert_eq!(dry["data"]["would_process"], 2);
    let kbs: Vec<&str> = dry["data"]["memories"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["kb"].as_str())
        .collect();
    assert!(
        kbs.contains(&"personal") && kbs.contains(&"elsewhere"),
        "the sweep must span both KBs: {kbs:?}"
    );

    // The real sweep routes each memory's facts back into its own KB.
    let done = run_json(
        &mut cmd_with_extractor(&tmp, LISBON_SCRIPT),
        &["facts", "backfill"],
    );
    assert_eq!(done["data"]["processed"], 2);
    assert_eq!(done["data"]["facts_added"], 2);

    let personal = run_json(&mut cmd_in(&tmp), &["facts", "list", "--kb", "personal"]);
    assert_eq!(personal["data"]["count"], 1);
    assert_eq!(personal["data"]["facts"][0]["predicate"], "lives_in");
    let elsewhere = run_json(&mut cmd_in(&tmp), &["facts", "list", "--kb", "elsewhere"]);
    assert_eq!(elsewhere["data"]["count"], 1);
    assert_eq!(elsewhere["data"]["facts"][0]["predicate"], "works_at");
}

/// FINDING 7 (small): `--diary '*'` is a read-side wildcard. Writing to it
/// stored a diary literally named `*` — unreachable by its own scoped recall,
/// and enough to mark the KB permanently mixed for diary isolation.
#[test]
fn writing_to_the_diary_wildcard_is_rejected() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    for bad in ["*", "   ", ""] {
        cmd_in(&tmp)
            .args(["remember", "Something durable.", "--diary", bad, "--json"])
            .assert()
            .failure()
            .code(3);
        cmd_in(&tmp)
            .args([
                "memory",
                "add",
                "Something durable.",
                "--diary",
                bad,
                "--json",
            ])
            .assert()
            .failure()
            .code(3);
    }

    let fixture = tmp.path().join("note.md");
    std::fs::write(&fixture, "A note worth ingesting.").unwrap();
    cmd_in(&tmp)
        .args([
            "ingest",
            fixture.to_str().unwrap(),
            "--diary",
            "*",
            "--mode",
            "general",
            "--json",
        ])
        .assert()
        .failure()
        .code(3);

    // Nothing was written under the wildcard name.
    let v = run_json(&mut cmd_in(&tmp), &["memory", "list", "--diary", "*"]);
    assert_eq!(v["data"]["memories"].as_array().map(|a| a.len()), Some(0));
}

/// The diary axis of `facts list` must not resurrect the trap the KB axis
/// escaped: a fact written under a non-default diary has to be visible to an
/// unscoped listing, with the scope spelled out in the envelope.
#[test]
fn facts_list_spans_every_diary_by_default_and_says_so() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args([
        "remember",
        "Priya Raman is Northwind's co-founder.",
        "--kb",
        "relationships",
        "--diary",
        "work",
        "--facts-json",
        r#"[{"subject":"Priya Raman","predicate":"has_role","object":"co-founder at Northwind","confidence":1.0,"fact_type":"profile_slot"}]"#,
        "--json",
    ])
    .assert()
    .success();

    // Unscoped: the work-diary fact must appear, labelled with its scope.
    let v = run_json(&mut cmd_in(&tmp), &["facts", "list"]);
    assert_eq!(
        v["data"]["count"], 1,
        "unscoped facts list must span diaries"
    );
    assert_eq!(v["data"]["diary"], "*");
    assert_eq!(v["data"]["diary_scope"], "all_diaries");
    assert_eq!(v["data"]["facts"][0]["subject"], "Priya Raman");

    // Narrowing still works, and the envelope says it was a flag.
    let v = run_json(&mut cmd_in(&tmp), &["facts", "list", "--diary", "work"]);
    assert_eq!(v["data"]["count"], 1);
    assert_eq!(v["data"]["diary_scope"], "flag");
    let v = run_json(&mut cmd_in(&tmp), &["facts", "list", "--diary", "personal"]);
    assert_eq!(v["data"]["count"], 0);
}

// ---------------------------------------------------------------------------
// Entity hygiene v2 — the person graph after a real WhatsApp/notes import.
//
// Compiling the owner's real store produced a `people list` whose top entries
// included [Maria Microsoft London Tech 340], [Heyy Elias 183], [London 84],
// [Ibiza 49], [Singapore 48], [Islam 32], [Oh 31] and [Good 20], while the
// humans it did find were split in half (Noemi 106 / Noemi Varkoni 66, Avi 52
// / Avi Roy 25, Patrick 57 / Patrick Ashcomb Pruett 57). This fixture
// reproduces every one of those failure classes end to end.
// ---------------------------------------------------------------------------

/// Ingest a small WhatsApp-shaped corpus — one file per conversation, so the
/// name unification has to work ACROSS chunks — and compile it.
fn whatsapp_kb() -> tempfile::TempDir {
    let (tmp, _) = with_isolated_home();
    let files: [(&str, &str); 6] = [
        (
            "chat-maria.md",
            "# WhatsApp export — Maria\n\n\
             Maria Microsoft London Tech: Heyy Elias, the deck is ready for the review.\n\
             Elias: Thanks Maria. Ok great, I will send notes tonight.\n\
             Maria Microsoft London Tech: Good morning Elias. Oh and happy birthday!\n\
             Elias: Morning Maria, dinner on Friday works for me.\n",
        ),
        (
            "chat-noemi.md",
            "# Notes — Noemi and Avi\n\n\
             Noemi Varkoni introduced me to Avi Roy at the Longevity Summit.\n\
             Noemi is speaking in Singapore next month and asked me to join her.\n\
             Avi emailed me about the fund. I told Avi we should meet in London.\n\
             I met Lira in London last week and we flew to Ibiza with my friend Avi Roy.\n",
        ),
        // The two Lauras live in DIFFERENT conversations on purpose: chunk by
        // chunk each looks unambiguous, so a per-chunk fold would give both of
        // them the alias "Laura" and the alias table (unique per KB) would
        // quietly award one woman's mentions to the other.
        (
            "chat-laura-corvo.md",
            "# Notes — Laura Corvo\n\n\
             Laura Corvo is a friend from the Barcelona trip; we had dinner in London.\n\
             Laura texted me about the trip photos.\n",
        ),
        (
            "chat-laura-dating.md",
            "# Notes — dating\n\n\
             Laura Quiroz is someone I met on a dating app while in Ibiza.\n\
             Laura asked if I am free on Friday.\n",
        ),
        (
            "chat-branislava.md",
            "# WhatsApp export — family\n\n\
             BRANISLAVA MARENOVIC: call me when you land in Belgrade.\n\
             Branislava Marenovic is my cousin and she called on Sunday.\n\
             Branislava sent photos from the wedding.\n\
             Islam came up over dinner with Lira. My friend Lira is Serbian.\n",
        ),
        (
            "chat-elias.md",
            "# Notes — Elias\n\n\
             Elias Danilovski runs the biotech fund from London.\n\
             Elias met Patrick Ashcomb Pruett at the summit in Ibiza.\n\
             Patrick asked about the raise and Pruett wants an intro to Avi Roy.\n\
             Elias's calendar is full. Heyy Elias, are we still on for Friday?\n",
        ),
    ];
    for (name, body) in files {
        let path = tmp.path().join(name);
        std::fs::write(&path, body).unwrap();
        cmd_in(&tmp)
            .args([
                "ingest",
                path.to_str().unwrap(),
                "--kb",
                "whatsapp",
                "--mode",
                "general",
                "--json",
            ])
            .assert()
            .success();
    }
    json_cmd(&tmp, &["compile", "--kb", "whatsapp", "--all"]);
    tmp
}

fn people_named(v: &Value) -> Vec<String> {
    v["data"]["people"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .map(|s| s.to_string())
        .collect()
}

fn person<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["data"]["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("{name} missing from {:?}", people_named(v)))
}

fn aliases_of(person: &Value) -> Vec<String> {
    person["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a.as_str())
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn places_creeds_and_chat_filler_are_never_people() {
    let tmp = whatsapp_kb();
    let v = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );
    let names = people_named(&v);
    for junk in [
        // Places and a creed, every one of them in an interpersonal context.
        "London",
        "Ibiza",
        "Singapore",
        "Barcelona",
        "Belgrade",
        "Islam",
        "Serbian",
        // Sentence-initial interjections and acknowledgements.
        "Oh",
        "Good",
        "Ok",
        "Thanks",
        "Morning",
        // Greeting-prefixed and contact-label forms.
        "Heyy Elias",
        "Morning Maria",
        "Thanks Maria",
        "Good Morning",
        "Maria Microsoft London Tech",
    ] {
        assert!(
            !names.contains(&junk.to_string()),
            "{junk} must not be a person: {names:?}"
        );
    }
    // Places survive as ordinary entities — only the TYPE was ever wrong.
    let entities = json_cmd(
        &tmp,
        &["entities", "list", "--kb", "whatsapp", "--limit", "200"],
    );
    let london = entities["data"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["canonical_name"] == "London")
        .expect("London is still an entity");
    assert_eq!(london["kind"], "concept");
}

#[test]
fn a_contact_label_resolves_to_the_person_with_the_label_as_an_alias() {
    let tmp = whatsapp_kb();
    let v = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );
    let maria = person(&v, "Maria");
    assert!(
        aliases_of(maria).contains(&"Maria Microsoft London Tech".to_string()),
        "the WhatsApp label must stay resolvable: {:?}",
        aliases_of(maria)
    );
    // …and the label itself still resolves back to her.
    let show = json_cmd(
        &tmp,
        &[
            "people",
            "show",
            "Maria Microsoft London Tech",
            "--kb",
            "whatsapp",
        ],
    );
    assert_eq!(show["data"]["person"]["name"], "Maria");
    assert_eq!(show["data"]["person"]["kind"], "person");
}

#[test]
fn greetings_and_case_variants_fold_onto_the_person() {
    let tmp = whatsapp_kb();
    let v = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );

    // "Heyy Elias" was the real store's second-biggest person, and it had
    // swallowed the actual Elias's mentions along with the greeting.
    let elias = person(&v, "Elias Danilovski");
    let aliases = aliases_of(elias);
    assert!(aliases.contains(&"Elias".to_string()), "{aliases:?}");
    assert!(aliases.contains(&"Elias's".to_string()), "{aliases:?}");
    assert!(
        elias["mentions"].as_u64().unwrap() >= 6,
        "every greeting must count for the human: {elias}"
    );

    // A shouted WhatsApp contact line and the prose spelling are one person,
    // displayed the way a human writes a name.
    let branislava = person(&v, "Branislava Marenovic");
    assert!(aliases_of(branislava).contains(&"Branislava".to_string()));
    assert!(!people_named(&v).contains(&"BRANISLAVA MARENOVIC".to_string()));
}

#[test]
fn bare_first_names_unify_kb_wide_when_unambiguous() {
    let tmp = whatsapp_kb();
    let v = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );
    let names = people_named(&v);

    for (short, full) in [
        ("Noemi", "Noemi Varkoni"),
        ("Avi", "Avi Roy"),
        ("Patrick", "Patrick Ashcomb Pruett"),
        ("Branislava", "Branislava Marenovic"),
    ] {
        assert!(
            !names.contains(&short.to_string()),
            "{short} must fold into {full}, not stand alone: {names:?}"
        );
        let merged = person(&v, full);
        assert!(
            aliases_of(merged).contains(&short.to_string()),
            "{full} must carry {short} as an alias: {:?}",
            aliases_of(merged)
        );
    }
    // The mention counts merged rather than splitting across two nodes.
    assert!(person(&v, "Avi Roy")["mentions"].as_u64().unwrap() >= 4);
    // The bare form still resolves in `people show`.
    let show = json_cmd(&tmp, &["people", "show", "Noemi", "--kb", "whatsapp"]);
    assert_eq!(show["data"]["person"]["name"], "Noemi Varkoni");
}

/// SAFETY: the owner knows two different Lauras (a friend, Laura Corvo, and
/// someone from a dating thread), and they appear in different conversations.
/// Same-first-name merging must never fuse two humans — which is why the
/// unifier folds only on an EXACTLY-ONE match, decided KB-wide rather than
/// chunk by chunk.
#[test]
fn two_people_sharing_a_first_name_are_never_fused() {
    let tmp = whatsapp_kb();
    let v = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );
    let names = people_named(&v);
    for expected in ["Laura Corvo", "Laura Quiroz", "Laura"] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} must stay its own node: {names:?}"
        );
    }
    for full in ["Laura Corvo", "Laura Quiroz"] {
        assert!(
            !aliases_of(person(&v, full)).contains(&"Laura".to_string()),
            "{full} absorbed the ambiguous bare name: {:?}",
            aliases_of(person(&v, full))
        );
    }
    // Neither full name may collect the other's bare mentions, so the two
    // ambiguous ones stay on the ambiguous node.
    assert_eq!(person(&v, "Laura Corvo")["mentions"], 1);
    assert_eq!(person(&v, "Laura Quiroz")["mentions"], 1);
    assert_eq!(person(&v, "Laura")["mentions"], 2);
    // An ambiguous lookup resolves to the ambiguous node, not to a guess.
    let show = json_cmd(&tmp, &["people", "show", "Laura", "--kb", "whatsapp"]);
    assert_eq!(show["data"]["person"]["name"], "Laura");
}

#[test]
fn the_hygienic_person_graph_is_stable_across_recompiles() {
    let tmp = whatsapp_kb();
    let fingerprint = |v: &Value| -> Vec<String> {
        let mut out: Vec<String> = v["data"]["people"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                format!(
                    "{}:{}:{}",
                    p["name"].as_str().unwrap_or(""),
                    p["mentions"].as_u64().unwrap_or(0),
                    aliases_of(p).join("|")
                )
            })
            .collect();
        out.sort();
        out
    };
    let before = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );
    json_cmd(&tmp, &["compile", "--kb", "whatsapp", "--all"]);
    json_cmd(&tmp, &["compile", "--kb", "whatsapp", "--all"]);
    let after = json_cmd(
        &tmp,
        &["people", "list", "--kb", "whatsapp", "--limit", "99"],
    );
    assert_eq!(
        fingerprint(&before),
        fingerprint(&after),
        "recompiling must converge, not drift"
    );
}
// ===========================================================================
// Conflict triage: the fact layer has to survive real volume.
//
// Importing 1,332 memories produced 2,953 facts and 9,510 queued conflicts —
// a backlog nobody adjudicates, which buries the handful of contradictions
// that matter. These tests pin the classification that keeps the queue small
// and the `facts triage` sweep that shrinks a backlog already on disk.
// ===========================================================================

/// Store facts through the CURRENT write path with the novelty gate forced
/// into its "everything is uncertain" band, which is what a store with
/// thousands of facts looks like: every write finds similar-enough neighbours.
fn remember_dense(tmp: &tempfile::TempDir, content: &str, facts: &str) -> Value {
    let out = cmd_in(tmp)
        .env("ENGRAM_FACT_NOVEL_THRESHOLD", "0.0")
        .env("ENGRAM_FACT_DUP_THRESHOLD", "0.999")
        .args([
            "remember",
            content,
            "--kb",
            "personal",
            "--facts-json",
            facts,
        ])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

/// Store facts through the PRE-TRIAGE write path: every conflict candidate is
/// queued, the way the import that produced 9,510 rows did. This is how a
/// legacy backlog is built for `facts triage` to clean up.
fn remember_legacy(tmp: &tempfile::TempDir, content: &str, facts: &str) -> Value {
    let out = cmd_in(tmp)
        .env("ENGRAM_FACT_QUEUE_ALL", "1")
        .env("ENGRAM_FACT_NOVEL_THRESHOLD", "0.0")
        .env("ENGRAM_FACT_DUP_THRESHOLD", "0.999")
        .args([
            "remember",
            content,
            "--kb",
            "personal",
            "--facts-json",
            facts,
        ])
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

/// The address book: a person's second acquaintance is an addition, not a
/// contradiction. This one rule is most of the 9,510.
#[test]
fn multi_valued_predicates_coexist_instead_of_queueing() {
    let (tmp, _cmd) = with_isolated_home();
    for (content, facts) in [
        (
            "Elias met Alice at the mixer.",
            r#"[{"subject":"Elias","predicate":"knows","object":"Alice"}]"#,
        ),
        (
            "Elias knows Bob from the fund.",
            r#"[{"subject":"Elias","predicate":"knows","object":"Bob"}]"#,
        ),
        (
            "Elias knows Carol via Alice.",
            r#"[{"subject":"Elias","predicate":"knows","object":"Carol"}]"#,
        ),
        (
            "Elias is friends with Dan.",
            r#"[{"subject":"Elias","predicate":"friend_of","object":"Dan"}]"#,
        ),
        (
            "Elias met Erin at the conference.",
            r#"[{"subject":"Elias","predicate":"met_at","object":"Erin"}]"#,
        ),
    ] {
        let v = remember_dense(&tmp, content, facts);
        assert_eq!(
            v["data"]["adjudication_needed"].as_array().unwrap().len(),
            0,
            "a second value of a multi-valued predicate must not queue: {content}"
        );
        assert_eq!(v["data"]["facts_added"], 1, "{content}");
    }

    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(
        v["data"]["active_count"], 5,
        "every relationship must stay active: {:?}",
        v["data"]["active"]
    );
    assert_eq!(v["data"]["historic_count"], 0, "nothing may be superseded");
    let v = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    assert_eq!(v["data"]["total_open"], 0);
}

/// A single-valued slot still behaves: newest value wins, old value kept.
/// And an OLDER value arriving late (a backfill of a 2015 memory) must not
/// overwrite the current one.
#[test]
fn single_valued_slots_supersede_by_event_time_in_both_directions() {
    let (tmp, _cmd) = with_isolated_home();
    remember_dense(
        &tmp,
        "Elias moved to Lisbon in 2024.",
        r#"[{"subject":"Elias","predicate":"lives_in","object":"Lisbon","valid_from":"2024-02-01"}]"#,
    );
    let v = remember_dense(
        &tmp,
        "Elias lived in London back in 2015.",
        r#"[{"subject":"Elias","predicate":"lives_in","object":"London","valid_from":"2015-06-01"}]"#,
    );
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "an out-of-order backfill is not a question for a human"
    );
    let conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["resolution"], "recency_stale");

    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(v["data"]["active_count"], 1);
    assert_eq!(
        v["data"]["active"][0]["object"], "Lisbon",
        "the stale 2015 value must not become the current one"
    );
    assert_eq!(v["data"]["historic"][0]["object"], "London");
}

/// Seed a legacy backlog of mixed conflicts: multi-valued additions,
/// candidates about other people, other attributes of one person, one slot
/// with a clear event-time ordering, one genuine same-memo contradiction,
/// and one safety slot.
fn seed_legacy_backlog(tmp: &tempfile::TempDir) {
    for (content, facts) in [
        (
            "Elias met Alice at the London biotech mixer.",
            r#"[{"subject":"Elias","predicate":"knows","object":"Alice"}]"#,
        ),
        (
            "Elias knows Bob from the fund.",
            r#"[{"subject":"Elias","predicate":"knows","object":"Bob"}]"#,
        ),
        (
            "Elias knows Carol via Alice.",
            r#"[{"subject":"Elias","predicate":"knows","object":"Carol"}]"#,
        ),
        (
            "Alice lives in Berlin.",
            r#"[{"subject":"Alice","predicate":"lives_in","object":"Berlin"}]"#,
        ),
        (
            "Bob lives in Porto.",
            r#"[{"subject":"Bob","predicate":"lives_in","object":"Porto"}]"#,
        ),
        (
            "Elias lived in London back in 2015.",
            r#"[{"subject":"Elias","predicate":"lives_in","object":"London","valid_from":"2015-06-01"}]"#,
        ),
        (
            "Elias moved to Lisbon in 2024.",
            r#"[{"subject":"Elias","predicate":"lives_in","object":"Lisbon","valid_from":"2024-02-01"}]"#,
        ),
        (
            "Ana is allergic to penicillin.",
            r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]"#,
        ),
        (
            "Ana is allergic to amoxicillin, per her chart.",
            r#"[{"subject":"Ana","predicate":"allergic_to","object":"amoxicillin"}]"#,
        ),
        (
            "One memo says Carol is at Acme, the next line says Beta Corp.",
            r#"[{"subject":"Carol","predicate":"works_at","object":"Acme"},
                {"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]"#,
        ),
        (
            "Elias has the role of CEO.",
            r#"[{"subject":"Elias","predicate":"has_role","object":"CEO"}]"#,
        ),
    ] {
        remember_legacy(tmp, content, facts);
    }
}

#[test]
fn triage_resolves_the_obvious_and_keeps_the_real_contradictions() {
    let (tmp, _cmd) = with_isolated_home();
    seed_legacy_backlog(&tmp);

    let before = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    let queued = before["data"]["total_open"].as_i64().unwrap();
    assert!(
        queued >= 20,
        "the legacy write path should bury the user in conflicts, got {queued}"
    );

    let v = run_json(&mut cmd_in(&tmp), &["facts", "triage"]);
    let d = &v["data"];
    assert_eq!(d["scanned"], queued);
    assert_eq!(d["remaining"], 2, "{:?}", d["remaining_samples"]);
    assert_eq!(d["resolved"].as_i64().unwrap(), queued - 2);
    assert!(
        d["reduction_ratio"].as_f64().unwrap() > 0.9,
        "reduction ratio was {}",
        d["reduction_ratio"]
    );

    // Every rule that fired is named, and the ones that fired are the ones
    // this fixture was built to exercise.
    let by_rule = d["resolved_by_rule"].as_object().unwrap();
    for rule in ["multi_valued", "different_subject", "different_predicate"] {
        assert!(
            by_rule.get(rule).and_then(|v| v.as_u64()).unwrap_or(0) > 0,
            "rule {rule} never fired: {by_rule:?}"
        );
    }
    assert_eq!(
        by_rule.get("recency_supersede").and_then(|v| v.as_u64()),
        Some(1),
        "the 2015 → 2024 move is the one slot with a clear ordering"
    );
    assert_eq!(d["facts_superseded"], 1);

    // What is left is exactly the safety slot and the one-memo contradiction.
    let reasons = d["remaining_by_reason"].as_object().unwrap();
    assert_eq!(reasons.get("safety_slot").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(
        reasons
            .get("same_source_contradiction")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    assert_eq!(d["safety_slots_kept"], 1);

    // Coexisting values are untouched: nothing was superseded to empty a queue.
    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    let active: Vec<&str> = v["data"]["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["object"].as_str().unwrap())
        .collect();
    for who in ["Alice", "Bob", "Carol"] {
        assert!(active.contains(&who), "triage dropped {who}: {active:?}");
    }
    assert!(active.contains(&"Lisbon"));
    assert!(!active.contains(&"London"), "the 2015 value stays historic");
    assert_eq!(
        v["data"]["historic"][0]["resolution"], "triage_recency",
        "a triage supersession must be auditable"
    );

    // Ana keeps BOTH allergy values active — a safety slot is never decided
    // by a sweep.
    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Ana", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(v["data"]["active_count"], 2);

    // The remainder is still resolvable by hand.
    let open = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    assert_eq!(open["data"]["total_open"], 2);
    let id = open["data"]["open_conflicts"][0]["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "resolve", &id, "--action", "keep-both"],
    );
    assert_eq!(v["data"]["resolved"], true);
    let open = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    assert_eq!(open["data"]["total_open"], 1);
}

#[test]
fn triage_dry_run_writes_nothing_and_reports_the_real_numbers() {
    let (tmp, _cmd) = with_isolated_home();
    seed_legacy_backlog(&tmp);
    let before = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    let queued = before["data"]["total_open"].as_i64().unwrap();

    let dry = run_json(&mut cmd_in(&tmp), &["facts", "triage", "--dry-run"]);
    assert_eq!(dry["data"]["dry_run"], true);
    assert_eq!(dry["data"]["facts_superseded"], 0);
    assert!(dry["data"]["resolved"].as_i64().unwrap() > 0);

    // Nothing moved: same queue, same facts.
    let after = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    assert_eq!(after["data"]["total_open"], queued);
    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(
        v["data"]["historic_count"], 0,
        "a dry run must not supersede a fact"
    );

    // And the numbers it printed are the numbers the real run produces.
    let real = run_json(&mut cmd_in(&tmp), &["facts", "triage"]);
    assert_eq!(real["data"]["resolved"], dry["data"]["resolved"]);
    assert_eq!(real["data"]["remaining"], dry["data"]["remaining"]);
    assert_eq!(
        real["data"]["resolved_by_rule"],
        dry["data"]["resolved_by_rule"]
    );
}

#[test]
fn triage_is_idempotent() {
    let (tmp, _cmd) = with_isolated_home();
    seed_legacy_backlog(&tmp);
    let first = run_json(&mut cmd_in(&tmp), &["facts", "triage"]);
    let remaining = first["data"]["remaining"].as_i64().unwrap();
    assert!(first["data"]["resolved"].as_i64().unwrap() > 0);

    let second = run_json(&mut cmd_in(&tmp), &["facts", "triage"]);
    assert_eq!(
        second["data"]["resolved"], 0,
        "a re-run must resolve nothing"
    );
    assert_eq!(second["data"]["scanned"], remaining);
    assert_eq!(second["data"]["remaining"], remaining);
    assert_eq!(second["data"]["facts_superseded"], 0);

    let third = run_json(&mut cmd_in(&tmp), &["facts", "triage", "--dry-run"]);
    assert_eq!(third["data"]["resolved"], 0);
}

/// A queue is only reviewable if its shape is visible: grouped by subject and
/// predicate, with both values and the sentence each came from.
#[test]
fn open_conflicts_are_grouped_by_subject_and_predicate_with_sources() {
    let (tmp, _cmd) = with_isolated_home();
    seed_legacy_backlog(&tmp);
    cmd_in(&tmp)
        .args(["facts", "triage", "--json"])
        .assert()
        .success();

    let v = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"]);
    let groups = v["data"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{groups:?}");
    let ana = groups
        .iter()
        .find(|g| g["subject"] == "Ana")
        .expect("Ana group");
    assert_eq!(ana["predicate"], "allergic_to");
    assert_eq!(ana["safety_slot"], true);
    assert_eq!(ana["arity"], "single");
    assert_eq!(ana["conflict_count"], 1);
    let entry = &ana["conflicts"][0];
    assert_eq!(entry["triage"]["auto_resolvable"], false);
    assert_eq!(entry["triage"]["rule"], "safety_slot");
    assert!(entry["resolve_command"]
        .as_str()
        .unwrap()
        .starts_with("engram facts resolve "));
    // Both values, and where each came from.
    let objects = [
        entry["new"]["object"].as_str().unwrap(),
        entry["old"]["object"].as_str().unwrap(),
    ];
    assert!(objects.contains(&"penicillin") && objects.contains(&"amoxicillin"));
    assert!(entry["new"]["source_preview"]
        .as_str()
        .unwrap()
        .contains("allergic"));
    assert!(entry["new"]["source_memory_id"].is_string());

    // A subject filter narrows to one person's backlog, without --open.
    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "conflicts", "--subject", "ana"],
    );
    assert_eq!(v["data"]["count"], 1);
    assert_eq!(v["data"]["groups"][0]["subject"], "Ana");

    // The history view advertises the backlog rather than hiding it.
    let v = run_json(&mut cmd_in(&tmp), &["facts", "conflicts"]);
    assert_eq!(v["data"]["open_count"], 2);
    assert!(v["data"]["next_action"].is_string());
}

/// Triage must respect the scope it was given: another KB's backlog is not
/// its business.
#[test]
fn triage_scopes_to_the_requested_kb() {
    let (tmp, _cmd) = with_isolated_home();
    seed_legacy_backlog(&tmp);
    let before = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"])["data"]
        ["total_open"]
        .as_i64()
        .unwrap();

    let v = run_json(
        &mut cmd_in(&tmp),
        &["facts", "triage", "--kb", "unrelated-kb"],
    );
    assert_eq!(v["data"]["scanned"], 0);
    assert_eq!(v["data"]["resolved"], 0);
    let after = run_json(&mut cmd_in(&tmp), &["facts", "conflicts", "--open"])["data"]
        ["total_open"]
        .as_i64()
        .unwrap();
    assert_eq!(after, before, "another KB's queue must be untouched");

    let v = run_json(&mut cmd_in(&tmp), &["facts", "triage", "--kb", "personal"]);
    assert!(v["data"]["resolved"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// Chunking: degenerate chunks must never reach the index.
//
// `remember` chunked with a bare paragraph split — no merge, no size cap, no
// minimum. Real content (a pasted thread, a doc with date headers) turned every
// stray line into its own embedded, retrievable chunk. A live store reached 986
// chunks under 40 characters, and a recall for a person's birthday returned two
// bare date fragments at score 1.0 ABOVE the memory that held the birthday.
// ---------------------------------------------------------------------------

/// Chunk texts for one KB, in storage order.
fn stored_chunks(tmp: &tempfile::TempDir, kb: &str) -> Vec<String> {
    let v = run_json(
        &mut cmd_in(tmp),
        &["export", "--format", "json", "--kb", kb],
    );
    v["data"]["chunks"]
        .as_array()
        .expect("export carries chunks")
        .iter()
        .map(|c| c["content"].as_str().unwrap().to_string())
        .collect()
}

/// A document whose paragraphs alternate date header / entry — the exact shape
/// that produced the 986 fragments.
fn dated_document() -> String {
    let mut doc = String::new();
    for day in 16..24 {
        doc.push_str(&format!("Sep {day}, 2025\n\n"));
        doc.push_str(&format!(
            "Met Noemi at the Belgrade summit on day {day} and she mentioned her birthday is \
             on the 3rd of April, which is worth remembering because she throws a party every \
             year at the family house outside the city.\n\n"
        ));
    }
    doc.push_str("Oct 2, 2025");
    doc
}

#[test]
fn remember_never_stores_a_bare_date_as_its_own_chunk() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args([
        "remember",
        &dated_document(),
        "--kb",
        "chunkfix",
        "--no-extract-facts",
        "--json",
    ])
    .assert()
    .success();

    let chunks = stored_chunks(&tmp, "chunkfix");
    assert!(chunks.len() > 1, "a 2k-character doc should split");
    for chunk in &chunks {
        assert!(
            chunk.len() >= 60,
            "degenerate chunk reached the index: {chunk:?}"
        );
    }
    assert!(
        !chunks.iter().any(|c| c.trim() == "Sep 16, 2025"),
        "a bare date became its own retrievable chunk"
    );
    // The header rides along with the entry it introduces rather than vanishing.
    assert!(chunks.iter().any(|c| c.contains("Sep 16, 2025")));
}

#[test]
fn remember_keeps_a_legitimately_short_memory_as_one_chunk() {
    let (tmp, mut cmd) = with_isolated_home();
    // 27 characters: under the floor, but it is the WHOLE memory, so it stands.
    let out = cmd
        .args([
            "remember",
            "Lira's birthday is 12 March",
            "--kb",
            "chunkfix",
            "--no-extract-facts",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(parse_stdout(&out)["data"]["chunks"], 1);

    let chunks = stored_chunks(&tmp, "chunkfix");
    assert_eq!(chunks, vec!["Lira's birthday is 12 March".to_string()]);
}

#[test]
fn remember_chunks_a_long_unbroken_note_instead_of_truncating_it() {
    let (tmp, mut cmd) = with_isolated_home();
    // No blank lines and no sentence boundaries: previously this went to the
    // embedder in one piece and everything past ~7,800 characters was dropped.
    let note = "reminder ".repeat(2_000);
    assert!(note.len() > 7_800);
    cmd.args([
        "remember",
        &note,
        "--kb",
        "chunkfix",
        "--no-extract-facts",
        "--json",
    ])
    .assert()
    .success();

    let chunks = stored_chunks(&tmp, "chunkfix");
    assert!(
        chunks.len() > 5,
        "long note must be split, got {}",
        chunks.len()
    );
    for chunk in &chunks {
        assert!(chunk.len() <= 1800 + 60, "chunk over cap: {}", chunk.len());
    }
    let words: usize = chunks.iter().map(|c| c.split_whitespace().count()).sum();
    assert_eq!(words, 2_000, "content was lost while chunking");
}

/// Write a memory with the floor disabled, reproducing a pre-fix store.
fn remember_unchunked(tmp: &tempfile::TempDir, content: &str, kb: &str) {
    cmd_in(tmp)
        .env("ENGRAM_CHUNK_MIN_CHARS", "1")
        .env("ENGRAM_CHUNK_TARGET_CHARS", "1")
        .args([
            "remember",
            content,
            "--kb",
            kb,
            "--no-extract-facts",
            "--json",
        ])
        .assert()
        .success();
}

#[test]
fn chunk_policy_env_overrides_are_honored_and_reported() {
    let (tmp, _cmd) = with_isolated_home();
    remember_unchunked(&tmp, &dated_document(), "legacy");
    let chunks = stored_chunks(&tmp, "legacy");
    assert!(
        chunks.iter().any(|c| c.trim() == "Sep 16, 2025"),
        "ENGRAM_CHUNK_MIN_CHARS=1 must disable the floor so the fixture is degenerate"
    );

    let mut cmd = cmd_in(&tmp);
    cmd.env("ENGRAM_CHUNK_MIN_CHARS", "123");
    let v = run_json(&mut cmd, &["config", "show"]);
    assert_eq!(v["data"]["chunking"]["min_chars"], 123);
    let v = run_json(&mut cmd_in(&tmp), &["config", "show"]);
    assert_eq!(v["data"]["chunking"]["min_chars"], 60);
}

#[test]
fn chunks_repair_merges_degenerate_chunks_and_is_idempotent() {
    let (tmp, _cmd) = with_isolated_home();
    remember_unchunked(&tmp, &dated_document(), "legacy");
    let before = stored_chunks(&tmp, "legacy");
    let degenerate_before = before.iter().filter(|c| c.len() < 60).count();
    assert!(degenerate_before > 0, "fixture must start degenerate");

    let v = run_json(&mut cmd_in(&tmp), &["chunks", "repair", "--kb", "legacy"]);
    assert_eq!(v["data"]["dry_run"], false);
    assert_eq!(v["data"]["degenerate_chunks_before"], degenerate_before);
    assert_eq!(v["data"]["degenerate_chunks_after"], 0);
    assert_eq!(v["data"]["memories_repaired"], 1);
    assert!(v["data"]["chunks_reembedded"].as_u64().unwrap() > 0);

    let after = stored_chunks(&tmp, "legacy");
    assert!(after.iter().all(|c| c.len() >= 60), "{after:?}");
    assert!(after.len() < before.len());
    // Nothing invented, nothing dropped: the same words in the same order.
    let words = |v: &[String]| -> Vec<String> {
        v.iter()
            .flat_map(|c| {
                c.split_whitespace()
                    .map(|w| w.to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    assert_eq!(words(&before), words(&after));

    // Idempotent: a repaired store has nothing left to do.
    let v = run_json(&mut cmd_in(&tmp), &["chunks", "repair", "--kb", "legacy"]);
    assert_eq!(v["data"]["memories_repaired"], 0);
    assert_eq!(v["data"]["degenerate_chunks_before"], 0);
    assert_eq!(v["data"]["chunks_reembedded"], 0);
    assert_eq!(stored_chunks(&tmp, "legacy"), after);
}

#[test]
fn chunks_repair_dry_run_writes_nothing() {
    let (tmp, _cmd) = with_isolated_home();
    remember_unchunked(&tmp, &dated_document(), "legacy");
    let before = stored_chunks(&tmp, "legacy");

    let v = run_json(
        &mut cmd_in(&tmp),
        &["chunks", "repair", "--kb", "legacy", "--dry-run"],
    );
    assert_eq!(v["data"]["dry_run"], true);
    assert!(v["data"]["memories_repaired"].as_u64().unwrap() > 0);
    assert_eq!(v["data"]["chunks_reembedded"], 0);
    assert!(v["data"]["next_step"]
        .as_str()
        .unwrap()
        .contains("--dry-run"));
    assert_eq!(
        stored_chunks(&tmp, "legacy"),
        before,
        "dry-run mutated store"
    );

    // Reported counts are stable, and the real run still has work to do.
    let again = run_json(
        &mut cmd_in(&tmp),
        &["chunks", "repair", "--kb", "legacy", "--dry-run"],
    );
    assert_eq!(again["data"], v["data"]);
    let applied = run_json(&mut cmd_in(&tmp), &["chunks", "repair", "--all-kbs"]);
    assert_eq!(
        applied["data"]["memories_repaired"],
        v["data"]["memories_repaired"]
    );
    assert_ne!(stored_chunks(&tmp, "legacy"), before);
}

#[test]
fn chunks_repair_requires_an_explicit_scope() {
    let (_tmp, mut cmd) = with_isolated_home();
    cmd.args(["chunks", "repair", "--json"])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn chunks_repair_leaves_a_healthy_store_untouched() {
    let (tmp, mut cmd) = with_isolated_home();
    cmd.args([
        "remember",
        &dated_document(),
        "--kb",
        "chunkfix",
        "--no-extract-facts",
        "--json",
    ])
    .assert()
    .success();
    let before = stored_chunks(&tmp, "chunkfix");

    let v = run_json(&mut cmd_in(&tmp), &["chunks", "repair", "--kb", "chunkfix"]);
    assert_eq!(v["data"]["memories_repaired"], 0);
    assert_eq!(v["data"]["chunks_reembedded"], 0);
    assert_eq!(stored_chunks(&tmp, "chunkfix"), before);
}
