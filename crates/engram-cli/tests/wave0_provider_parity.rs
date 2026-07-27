//! Wave 0.5 — provider and budget parity across every retrieval surface.
//!
//! A cloud profile with no Gemini key must be a loud config error on `research`
//! and on both daemon endpoints, exactly as it already is on `recall`; and a
//! configured spend cap must stop a paid call on every one of them. Each test
//! below fails on `main` before the fix.

use assert_cmd::Command;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

/// Point a spawned binary's data/config/cache at `home` (same idiom as
/// tests/cli.rs: `directories` honors XDG_* on Linux and HOME on macOS).
fn point_at_home(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

/// A command with the bench stub forced on — the ordinary CI shape.
fn stub_cmd_in(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home);
    cmd
}

/// A command in the shape a real user has: no bench stub, and no keys, however
/// the developer's own shell is configured.
fn cloud_cmd_in(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    point_at_home(&mut cmd, home);
    cmd
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// The JSON error envelope engram writes to stderr (last `{`-line, so a
/// tracing line in front of it cannot break the parse).
fn stderr_json(err: &[u8]) -> Value {
    let line = String::from_utf8_lossy(err)
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("JSON error on stderr")
        .to_string();
    serde_json::from_str(&line).expect("parseable error envelope")
}

struct Daemon {
    child: std::process::Child,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ask the OS for an unused port and let it go: a fixed 8768 would collide with
/// the developer's own engramd and with the other tests in this file.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
    let port = l.local_addr().expect("local addr").port();
    drop(l);
    port
}

/// Start `engram serve` against `home` with no bench stub and no keys except
/// `extra_env`. Returns once /health answers.
fn start_daemon(home: &Path, extra_env: &[(&str, &str)]) -> Daemon {
    let port = free_port();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_engram"));
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env_remove("ENGRAM_BENCH_FORCE_STUB")
        .env_remove("GEMINI_API_KEY")
        .env_remove("COHERE_API_KEY")
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let daemon = Daemon {
        child: cmd.spawn().expect("engram serve starts"),
        port,
    };
    for _ in 0..200 {
        if let Ok((200, _)) = request(daemon.port, "GET", "/health", None) {
            return daemon;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("daemon never became healthy on 127.0.0.1:{}", daemon.port);
}

/// One HTTP/1.1 round trip against the daemon: (status, parsed body).
fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, Value)> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    // A config refusal or a budget refusal is immediate. A regression that
    // reaches the provider would otherwise hang here for minutes on retry
    // backoff instead of failing.
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let body = body.unwrap_or("");
    // The daemon reads the whole request with a single read(), so headers and
    // body go out in one write.
    let req = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let json = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    Ok((status, serde_json::from_str(json).unwrap_or(Value::Null)))
}

/// A key that is never spent: every test using it expects the refusal to fire
/// before the first network call.
const UNUSED_KEY: &str = "fake-key-not-used-because-the-guard-blocks-first";

#[test]
fn research_under_a_cloud_profile_without_a_key_is_a_config_error() {
    // The core defect: `research` used to exit 0 with 64-dim stub results and
    // no hint that retrieval quality was fiction.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let assert = cloud_cmd_in(tmp.path())
        .args([
            "research",
            "anything",
            "--profile",
            "cloud_quality",
            "--json",
        ])
        .assert()
        .failure()
        .code(2);
    let v = stderr_json(&assert.get_output().stderr);
    assert_eq!(v["error"]["code"], "config_error", "{v}");
    let msg = v["error"]["message"].as_str().expect("message");
    assert!(msg.contains("Gemini API key"), "{msg}");
    assert!(
        msg.contains("offline"),
        "the error must name the remedy: {msg}"
    );
}

#[test]
fn research_offline_profile_without_a_key_still_works() {
    // The explicit stub opt-in survives (mirror of the recall test).
    let tmp = tempfile::tempdir().expect("tmp dir");
    cloud_cmd_in(tmp.path())
        .args(["research", "anything", "--profile", "offline", "--json"])
        .assert()
        .success();
}

#[test]
fn a_falsy_stub_flag_does_not_force_the_stub() {
    // Review finding (Codex, post-merge): the flag was read with is_ok(), so
    // ENGRAM_BENCH_FORCE_STUB=0 — the natural way to switch the hook OFF in a
    // shell — silently forced the stub under a cloud profile, suppressing the
    // very missing-key error this wave exists to raise. The VALUE decides now.
    let tmp = tempfile::tempdir().expect("tmp dir");
    for falsy in ["0", "false", ""] {
        let out = cloud_cmd_in(tmp.path())
            .env("ENGRAM_BENCH_FORCE_STUB", falsy)
            .args(["research", "anything", "--json"])
            .assert()
            .failure()
            .code(2)
            .get_output()
            .stderr
            .clone();
        let err = stderr_json(&out);
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("Gemini API key"),
            "falsy {falsy:?} must keep the loud missing-key error: {err}"
        );
    }
}

#[test]
fn research_under_the_bench_stub_flag_still_uses_the_stub() {
    // ENGRAM_BENCH_FORCE_STUB stays a silent opt-in — the invariant the whole
    // existing CLI suite runs on.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let out = stub_cmd_in(tmp.path())
        .args(["research", "anything", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_stdout(&out);
    assert_eq!(v["data"]["metadata"]["embed_model"], "stub-64", "{v}");
}

#[test]
fn research_stops_at_a_zero_budget_before_paying() {
    // Budget parity on the CLI research surface: the guard fires before the
    // first paid call, so the fake key is never used.
    let tmp = tempfile::tempdir().expect("tmp dir");
    stub_cmd_in(tmp.path())
        .args([
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

    let assert = cloud_cmd_in(tmp.path())
        .env("GEMINI_API_KEY", UNUSED_KEY)
        .args(["research", "anything", "--kb", "capped", "--json"])
        .assert()
        .failure()
        .code(4);
    let v = stderr_json(&assert.get_output().stderr);
    assert_eq!(v["error"]["code"], "rate_limited", "{v}");
}

#[test]
fn daemon_recall_without_a_key_returns_a_config_error_not_stub_results() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = request(
        daemon.port,
        "POST",
        "/v1/recall",
        Some("{\"query\":\"anything\"}"),
    )
    .expect("daemon answers");
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["status"], "error", "{body}");
    assert_eq!(body["error"]["code"], "config_error", "{body}");
    assert_eq!(body["error"]["exit_code"], 2, "{body}");
    assert!(body["data"].is_null(), "no results were invented: {body}");
}

#[test]
fn daemon_research_without_a_key_returns_the_same_config_error() {
    // Both endpoints, one rule.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = request(
        daemon.port,
        "POST",
        "/v1/research",
        Some("{\"query\":\"anything\"}"),
    )
    .expect("daemon answers");
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "config_error", "{body}");
    assert_eq!(body["error"]["exit_code"], 2, "{body}");
    assert!(body["data"].is_null(), "{body}");
}

#[test]
fn daemon_recall_offline_profile_still_answers_from_the_stub() {
    // The fix must not amputate the daemon's offline path.
    let tmp = tempfile::tempdir().expect("tmp dir");
    stub_cmd_in(tmp.path())
        .args([
            "remember",
            "Rapamycin extends mouse lifespan.",
            "--kb",
            "parity",
            "--no-facts",
            "--json",
        ])
        .assert()
        .success();

    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = request(
        daemon.port,
        "POST",
        "/v1/recall",
        Some("{\"query\":\"rapamycin lifespan\",\"profile\":\"offline\"}"),
    )
    .expect("daemon answers");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["status"], "success", "{body}");
    assert!(
        !body["data"]["results"]
            .as_array()
            .expect("results[]")
            .is_empty(),
        "{body}"
    );
    assert_eq!(body["data"]["metadata"]["embed_model"], "stub-64", "{body}");
}

#[test]
fn daemon_recall_stops_at_a_zero_budget_before_paying() {
    // A kb-less request resolves to the global budget, so cap that one.
    let tmp = tempfile::tempdir().expect("tmp dir");
    stub_cmd_in(tmp.path())
        .args(["budget", "set", "--daily-usd", "0", "--json"])
        .assert()
        .success();

    let daemon = start_daemon(tmp.path(), &[("GEMINI_API_KEY", UNUSED_KEY)]);
    let (status, body) = request(
        daemon.port,
        "POST",
        "/v1/recall",
        Some("{\"query\":\"anything\"}"),
    )
    .expect("daemon answers");
    assert_eq!(status, 429, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited", "{body}");
    assert_eq!(body["error"]["exit_code"], 4, "{body}");
}
