//! Wave 0.5 — the daemon answers the same request the CLI does.
//!
//! Three defects, one file. `/v1/recall` accepted a blank query and paid a
//! provider to embed it; it accepted `since`, `until` and `layer` and silently
//! dropped them, so a caller who believed they had scoped a window was
//! answered from the whole store; and it must report the embed model and its
//! dimensions, so a 64-dim stub downgrade is distinguishable from 1536-dim
//! cloud retrieval.
//!
//! Deviation from the `wave0_provider_parity` idiom, stated once: the daemon
//! still starts with `GEMINI_API_KEY`, `COHERE_API_KEY` and
//! `ENGRAM_BENCH_FORCE_STUB` removed, and every request that needs results asks
//! for `"profile":"offline"` — the documented offline opt-in rather than the
//! test hook. Seeding uses the CLI with `ENGRAM_BENCH_FORCE_STUB=1`, whose
//! `stub-64` embeddings are what the offline daemon read then matches
//! (otherwise `guard_embedding_compatibility` fires).

use assert_cmd::Command;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

/// Point a spawned binary's data/config/cache at `home` (same idiom as
/// tests/cli.rs: `directories` honors XDG_* on Linux and HOME on macOS).
fn point_at_home(cmd: &mut Command, home: &Path) {
    // A developer's exported ENGRAM_STORE outranks HOME/XDG (cli.rs documents
    // the precedence), so without this line the temp-HOME isolation below is
    // bypassed and mutator tests run against the REAL store.
    cmd.env_remove("ENGRAM_STORE");
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

/// Seed the store through the CLI with the bench stub forced on.
fn seed(home: &Path, args: &[&str]) {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home);
    cmd.args(args).arg("--json").assert().success();
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
///
/// A port is probed and released before the daemon binds it, so under `cargo
/// test`'s parallelism another test can take it in between — twenty daemons
/// start across this wave's two files. Losing that race is not a product
/// failure, so take a fresh port and try again instead of reporting a flake as
/// a daemon that would not start.
fn start_daemon(home: &Path, extra_env: &[(&str, &str)]) -> Daemon {
    for _ in 0..5 {
        if let Some(daemon) = try_start_daemon(home, extra_env) {
            return daemon;
        }
    }
    panic!("engram serve never became healthy on any of 5 ports");
}

fn try_start_daemon(home: &Path, extra_env: &[(&str, &str)]) -> Option<Daemon> {
    let port = free_port();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_engram"));
    cmd.env_remove("ENGRAM_STORE");
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
    let mut daemon = Daemon {
        child: cmd.spawn().expect("engram serve starts"),
        port,
    };
    for _ in 0..200 {
        if let Ok((200, _)) = request(daemon.port, "GET", "/health", None) {
            return Some(daemon);
        }
        // A bind failure exits the process; there is nothing to wait for.
        if matches!(daemon.child.try_wait(), Ok(Some(_))) {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// One HTTP/1.1 round trip against the daemon: (status, parsed body).
fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, Value)> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    // A refusal is immediate. A regression that reaches the provider would
    // otherwise hang here for minutes on retry backoff instead of failing.
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let body = body.unwrap_or("");
    // Head and body in TWO writes, with a flush between.
    //
    // This used to be one write with a comment saying the daemon reads the
    // whole request in a single `read()` — which was true, and was the defect:
    // a body that arrived in a second segment was truncated and blamed on the
    // caller's JSON. The parity suite is not allowed to be the reason that
    // never showed up, so it now sends the way a real client does.
    let head = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes())?;
        stream.flush()?;
    }
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

/// POST a JSON body and return (status, envelope).
fn post(port: u16, path: &str, body: &str) -> (u16, Value) {
    request(port, "POST", path, Some(body)).expect("daemon answers")
}

/// The text of every returned passage, read from `answer_context` — the one
/// copy both surfaces print (results[] repeats it only with `full`).
fn contents(body: &Value) -> Vec<String> {
    let ctx = body["data"]["answer_context"]
        .as_str()
        .expect("answer_context");
    ctx.split("<engram-result ")
        .skip(1)
        .filter_map(|el| {
            let body = el.split_once(">\n")?.1;
            Some(body.split("\n</engram-result>").next()?.to_string())
        })
        .collect()
}

/// Every blank-query refusal asserts the same four things.
fn assert_blank_query_refused(status: u16, body: &Value) {
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["status"], "error", "{body}");
    assert_eq!(body["error"]["code"], "bad_input", "{body}");
    assert_eq!(body["error"]["exit_code"], 3, "{body}");
    assert!(body["data"].is_null(), "no results were invented: {body}");
}

/// A key that is never spent: every test using it expects the refusal to fire
/// before the first network call.
const UNUSED_KEY: &str = "fake-key-not-used-because-the-guard-blocks-first";

#[test]
fn daemon_recall_runs_the_relevance_judge_as_the_cli_does() {
    // `/v1/recall` used to skip the relevance judge entirely: an agent over
    // HTTP got the unjudged top ten that `engram recall` filters. The stub
    // judge beats every key, so nothing leaves the machine.
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed(
        tmp.path(),
        &[
            "remember",
            "The harbour office opens at nine on weekdays.",
            "--kb",
            "judge",
            "--no-facts",
        ],
    );
    let query = "{\"query\":\"harbour office hours\",\"profile\":\"offline\"}";

    // A judge that rejects everything: the daemon abstains, as the CLI does.
    let stub = [
        ("ENGRAM_JUDGE_STUB", "0.01"),
        ("TYPESAFE_API_KEY", "ts-test-not-a-key"),
    ];
    let daemon = start_daemon(tmp.path(), &stub);
    let (status, body) = post(daemon.port, "/v1/recall", query);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["status"], "no_results", "{body}");
    assert!(contents(&body).is_empty(), "{body}");
    let relevance = &body["data"]["metadata"]["relevance"];
    assert_eq!(relevance["mode"], "on", "{body}");
    assert_eq!(relevance["abstained"], true, "{body}");
    drop(daemon);

    // A judge that accepts: every result carries its probability.
    let stub = [
        ("ENGRAM_JUDGE_STUB", "0.9"),
        ("TYPESAFE_API_KEY", "ts-test-not-a-key"),
    ];
    let daemon = start_daemon(tmp.path(), &stub);
    let (status, body) = post(daemon.port, "/v1/recall", query);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["status"], "success", "{body}");
    let results = body["data"]["results"].as_array().expect("results");
    assert!(!results.is_empty(), "{body}");
    assert!(
        results.iter().all(|r| r["relevance"].as_f64() == Some(0.9)),
        "{body}"
    );
}

#[test]
fn daemon_recall_refuses_an_empty_query() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = post(daemon.port, "/v1/recall", "{\"query\":\"\"}");
    assert_blank_query_refused(status, &body);
}

#[test]
fn daemon_recall_refuses_a_whitespace_query() {
    // A query of spaces is not a query: `engram recall "  "` has always been
    // exit 3, and the daemon trims before deciding for the same reason.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = post(daemon.port, "/v1/recall", "{\"query\":\"  \\t\\n \"}");
    assert_blank_query_refused(status, &body);
}

#[test]
fn an_empty_query_is_refused_before_the_provider_and_before_the_budget() {
    // The ORDERING is the contract. A typo must not be reported as a key
    // problem (which sends the caller to fix their config) and must not be
    // reported as a spend problem (which sends them to raise their cap): both
    // gates sit behind a guard that costs nothing.
    let tmp = tempfile::tempdir().expect("tmp dir");

    // (a) No key, default cloud profile: bad_input wins over config_error.
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = post(daemon.port, "/v1/recall", "{\"query\":\"\"}");
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["error"]["code"], "bad_input",
        "a blank query must not be diagnosed as a missing key: {body}"
    );
    drop(daemon);

    // (b) A key and a $0 global cap: bad_input wins over rate_limited too, so
    // the guard provably precedes the paid call rather than merely racing it.
    let capped = tempfile::tempdir().expect("tmp dir");
    seed(capped.path(), &["budget", "set", "--daily-usd", "0"]);
    let daemon = start_daemon(capped.path(), &[("GEMINI_API_KEY", UNUSED_KEY)]);
    let (status, body) = post(daemon.port, "/v1/recall", "{\"query\":\"   \"}");
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["error"]["code"], "bad_input",
        "a blank query must not be diagnosed as a budget refusal: {body}"
    );
}

#[test]
fn daemon_recall_honours_since_and_until() {
    // The core defect: `since` and `until` were parsed off the wire into
    // fields nobody read, so a windowed request was answered from the whole
    // store with no signal that the window had been ignored.
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed(
        tmp.path(),
        &[
            "remember",
            "Sailing regatta in Cascais with the whole family.",
            "--kb",
            "window",
            "--event-time",
            "2019-06-01T10:00:00Z",
            "--no-facts",
        ],
    );
    seed(
        tmp.path(),
        &[
            "remember",
            "Sailing regatta in Cascais with the new crew.",
            "--kb",
            "window",
            "--event-time",
            "2026-06-01",
            "--no-facts",
        ],
    );

    let daemon = start_daemon(tmp.path(), &[]);

    let (status, body) = post(
        daemon.port,
        "/v1/recall",
        "{\"query\":\"sailing regatta Cascais\",\"profile\":\"offline\",\"since\":\"2025-01-01\"}",
    );
    assert_eq!(status, 200, "{body}");
    let found = contents(&body);
    assert!(
        found.iter().any(|c| c.contains("new crew")),
        "the 2026 event must answer a since=2025 recall: {found:?}"
    );
    assert!(
        !found.iter().any(|c| c.contains("whole family")),
        "a 2019 event must not answer a since=2025 recall: {found:?}"
    );

    let (status, body) = post(
        daemon.port,
        "/v1/recall",
        "{\"query\":\"sailing regatta Cascais\",\"profile\":\"offline\",\"until\":\"2020-01-01\"}",
    );
    assert_eq!(status, 200, "{body}");
    let found = contents(&body);
    assert!(
        found.iter().any(|c| c.contains("whole family")),
        "{found:?}"
    );
    assert!(!found.iter().any(|c| c.contains("new crew")), "{found:?}");
}

#[test]
fn daemon_recall_rejects_an_unparseable_since_or_until() {
    // Silently ignoring a bound is the worst outcome: the caller is answered
    // from the whole store and believes they were answered from a window. A
    // bound the reader cannot parse is a hard error naming both spellings.
    let tmp = tempfile::tempdir().expect("tmp dir");
    let daemon = start_daemon(tmp.path(), &[]);
    for field in ["since", "until"] {
        for raw in ["last tuesday", "2026-13-45", ""] {
            let body_json = format!("{{\"query\":\"anything\",\"{field}\":\"{raw}\"}}");
            let (status, body) = post(daemon.port, "/v1/recall", &body_json);
            assert_eq!(status, 400, "{field}={raw:?}: {body}");
            assert_eq!(body["error"]["code"], "bad_input", "{body}");
            assert_eq!(body["error"]["exit_code"], 3, "{body}");
            let msg = body["error"]["message"].as_str().unwrap_or_default();
            assert!(msg.contains("RFC3339"), "the remedy must be named: {msg}");
            assert!(msg.contains("YYYY-MM-DD"), "{msg}");
        }
    }
}

#[test]
fn daemon_recall_accepts_a_bare_date_as_a_whole_utc_day() {
    // Same rule as the CLI (`recall_since_and_until_accept_a_bare_date`): a
    // bare date is a whole day, opening at 00:00:00Z for `since` and closing at
    // 23:59:59.999999999Z for `until`, so `since == until == D` selects day D.
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed(
        tmp.path(),
        &[
            "remember",
            "Turmeric harvest logged at the Sintra allotment.",
            "--kb",
            "garden",
            "--event-time",
            "2026-06-15T18:00:00Z",
            "--no-facts",
        ],
    );
    let daemon = start_daemon(tmp.path(), &[]);

    let found = |window: &str| -> bool {
        let body_json =
            format!("{{\"query\":\"turmeric harvest Sintra\",\"profile\":\"offline\",{window}}}");
        let (status, body) = post(daemon.port, "/v1/recall", &body_json);
        assert_eq!(status, 200, "{body}");
        contents(&body)
            .iter()
            .any(|c| c.contains("Turmeric harvest"))
    };

    assert!(found("\"since\":\"2026-06-15\",\"until\":\"2026-06-15\""));
    // An 18:00 event is inside the END of its own day and outside the day before.
    assert!(found("\"until\":\"2026-06-15\""));
    assert!(!found("\"until\":\"2026-06-14\""));
    assert!(!found("\"since\":\"2026-06-16\""));
}

#[test]
fn daemon_recall_accepts_a_layer_and_reports_it() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed(
        tmp.path(),
        &[
            "remember",
            "Rapamycin extends mouse lifespan in the ITP cohorts.",
            "--kb",
            "layers",
            "--no-facts",
        ],
    );
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = post(
        daemon.port,
        "/v1/recall",
        "{\"query\":\"rapamycin lifespan\",\"profile\":\"offline\",\"layer\":\"critical\"}",
    );
    assert_eq!(status, 200, "{body}");
    let meta = &body["data"]["metadata"];
    assert_eq!(meta["layer"], "critical", "{body}");
    assert_eq!(meta["token_budget"], 120, "{body}");
    assert!(
        meta["tokens_used"].is_number(),
        "the caller must be able to see what the budget was spent on: {body}"
    );
}

#[test]
fn a_layer_never_returns_fewer_results_than_top_k() {
    // The layer budget is a REPORTING knob, not a silent filter: it only trims
    // what exceeds the caller's own top_k, so `identity` (50 tokens) and `deep`
    // (4096) return the same set for a request that asked for ten. A "fix" that
    // makes a layer truncate the answer fails here.
    let tmp = tempfile::tempdir().expect("tmp dir");
    for note in [
        "Rapamycin extends mouse lifespan in the ITP cohorts, replicated at three sites.",
        "Rapamycin dosing intermittently reduced immunosuppression in the ITP follow-up.",
        "Rapamycin analogues were trialled for lifespan in marmosets with mixed results.",
        "Rapamycin lifespan effects were larger in female mice across the ITP replicates.",
    ] {
        seed(
            tmp.path(),
            &["remember", note, "--kb", "layers", "--no-facts"],
        );
    }
    let daemon = start_daemon(tmp.path(), &[]);

    let count = |layer: &str| -> usize {
        let body_json = format!(
            "{{\"query\":\"rapamycin lifespan\",\"profile\":\"offline\",\"layer\":\"{layer}\"}}"
        );
        let (status, body) = post(daemon.port, "/v1/recall", &body_json);
        assert_eq!(status, 200, "{body}");
        body["data"]["results"].as_array().expect("results[]").len()
    };

    let identity = count("identity");
    let deep = count("deep");
    assert!(identity > 0, "the store is not empty");
    assert_eq!(
        identity, deep,
        "a 50-token layer returned fewer results than a 4096-token one"
    );
}

#[test]
fn daemon_recall_rejects_an_unknown_layer() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = post(
        daemon.port,
        "/v1/recall",
        "{\"query\":\"anything\",\"layer\":\"l9\"}",
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "bad_input", "{body}");
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    for name in ["identity", "critical", "topic", "deep"] {
        assert!(msg.contains(name), "the error must name {name}: {msg}");
    }
}

#[test]
fn daemon_recall_still_reports_the_embed_model_and_dimensions() {
    let tmp = tempfile::tempdir().expect("tmp dir");
    seed(
        tmp.path(),
        &[
            "remember",
            "Rapamycin extends mouse lifespan in the ITP cohorts.",
            "--kb",
            "downgrade",
            "--no-facts",
        ],
    );
    let daemon = start_daemon(tmp.path(), &[]);
    let (status, body) = post(
        daemon.port,
        "/v1/recall",
        "{\"query\":\"rapamycin lifespan\",\"profile\":\"offline\"}",
    );
    assert_eq!(status, 200, "{body}");
    let meta = &body["data"]["metadata"];
    assert_eq!(meta["embed_model"], "stub-64", "{body}");
    assert_eq!(meta["embed_dims"], 64, "{body}");
}
