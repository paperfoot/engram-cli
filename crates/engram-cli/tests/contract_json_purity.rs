//! Contract — `--json` means exactly one JSON object on stdout, and nothing else.
//!
//! Three clauses, pinned against the built binary rather than against a
//! function, because every way stdout gets contaminated (a stray `println!`, a
//! progress bar, a tracing writer pointed at the wrong stream) only exists at
//! the process boundary:
//!
//! 1. SUCCESS — the whole of stdout is one JSON document. Not "a JSON object
//!    somewhere in the output": `serde_json::from_slice` over every byte, so a
//!    banner before it or a second document after it is a failure.
//! 2. FAILURE — stdout is EMPTY and the envelope goes to stderr. This is the
//!    0.3.1 behaviour and it is deliberate: a caller that pipes stdout into a
//!    parser must see nothing at all rather than a half-object, and `set -e`
//!    plus a nonzero exit is the signal. These tests pin that split; they do
//!    not argue for it.
//! 3. EXIT CODES — the process status equals the `error.exit_code` the envelope
//!    itself reports, so a caller may branch on either and get the same answer.
//!
//! Isolated HOME per test and `ENGRAM_BENCH_FORCE_STUB=1` everywhere except the
//! one keyless test that needs a real config failure: no network, no key, and
//! the developer's own store is never opened.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

/// Point a spawned binary's data/config/cache at `home`. The `directories`
/// crate honors XDG_* on Linux and HOME on macOS, so both must be set.
fn point_at_home(cmd: &mut Command, home: &Path) {
    cmd.env_remove("ENGRAM_STORE");
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

/// A keyless shell, whatever the developer's environment happens to hold.
/// `assert_cmd` inherits the parent env, so every provider key is removed by
/// name — otherwise these tests pass on CI and fail on a machine with a key.
///
/// `ENGRAM_STORE` is removed for a harder reason than flakiness: it overrides
/// the path root outright, so a developer who exports it in their shell would
/// have these tests — `forget --confirm` among them — run against their real
/// store instead of the temp HOME.
fn bare_cmd_in(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_STORE");
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env_remove("ENGRAM_EMBED_FAIL");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("ANTHROPIC_API_KEY");
    cmd.env_remove("TYPESAFE_API_KEY");
    cmd.env_remove("GEMINI_API_KEY_FALLBACKS");
    cmd.env_remove("ENGRAM_JUDGE_STUB");
    cmd.env_remove("ENGRAM_JUDGE_GROUNDING");
    cmd.env_remove("ENGRAM_JUDGE_ADJUDICATE");
    cmd.env_remove("ENGRAM_JUDGE_DEDUPE");
    cmd.env_remove("ENGRAM_JUDGE_RELEVANCE");
    cmd.env_remove("ENGRAM_JUDGE_LOG");
    cmd.env_remove("RUST_LOG");
    // stderr is a pipe under a test harness anyway; pinning the seam means the
    // machine-readable branch is what these tests observe, not an accident of
    // how the suite was launched.
    cmd.env("ENGRAM_FORCE_TTY", "0");
    point_at_home(&mut cmd, home);
    cmd
}

/// The ordinary offline shape: deterministic stub embedder, no paid call.
fn engram_in(home: &tempfile::TempDir) -> Command {
    let mut cmd = bare_cmd_in(home.path());
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp dir")
}

/// Clause 1, as one assertion. `from_slice` over the FULL buffer already
/// rejects trailing garbage; the explicit first-byte and object checks are here
/// so a failure names which way purity broke instead of just "expected value".
fn assert_stdout_is_one_json_object(stdout: &[u8], what: &str) -> Value {
    let text = String::from_utf8_lossy(stdout);
    assert!(
        !stdout.is_empty(),
        "{what}: --json produced no stdout at all"
    );
    let first = text
        .chars()
        .find(|c| !c.is_whitespace())
        .expect("non-whitespace byte");
    assert_eq!(
        first, '{',
        "{what}: stdout does not open with a JSON object — something printed first:\n{text}"
    );
    let v: Value = serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!("{what}: stdout is not exactly one JSON document ({e}):\n{text}")
    });
    assert!(
        v.is_object(),
        "{what}: stdout parsed, but not as an object:\n{text}"
    );
    v
}

/// Success-path stderr may carry tracing and `note:` lines — that is its job.
/// What it must NOT carry is an error envelope, which would tell a caller that
/// watches both streams that a successful command failed.
fn assert_no_error_envelope(stderr: &[u8], what: &str) {
    let text = String::from_utf8_lossy(stderr);
    assert!(
        !text.contains("\"status\":\"error\""),
        "{what}: an error envelope appeared on the success path:\n{text}"
    );
}

/// Clause 2 + 3 in one place: empty stdout, one envelope on stderr, and the
/// status the envelope claims is the status the process actually returned.
fn assert_failure_envelope(out: &std::process::Output, what: &str, code: &str, exit: i32) -> Value {
    assert!(
        out.stdout.is_empty(),
        "{what}: a failing --json command wrote to stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stderr);
    let v: Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
        panic!("{what}: stderr is not exactly one JSON document ({e}):\n{text}")
    });
    assert_eq!(v["status"], "error", "{what}: {v}");
    assert_eq!(v["error"]["code"], code, "{what}: {v}");
    assert_eq!(
        v["error"]["exit_code"].as_i64(),
        Some(exit as i64),
        "{what}: {v}"
    );
    assert_eq!(
        out.status.code(),
        Some(exit),
        "{what}: process status disagrees with the envelope's own exit_code: {v}"
    );
    v
}

const SEED: &str = "Marisol keeps the quokkavault inventory in a red ledger by the window.";

/// A home with one memory in it, so the read surfaces have something to return.
fn seeded_home() -> tempfile::TempDir {
    let h = home();
    let out = engram_in(&h)
        .args(["remember", SEED, "--kb", "purity", "--no-facts", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_stdout_is_one_json_object(&out.stdout, "seed remember");
    h
}

// ---------------------------------------------------------------------------
// Clause 1 — success paths
// ---------------------------------------------------------------------------

/// The sweep. Every read surface that works with no key and no network, run in
/// one seeded store. A stray `println!` on any of these paths lands here.
#[test]
fn every_offline_success_surface_puts_exactly_one_json_object_on_stdout() {
    let h = seeded_home();
    let cases: &[&[&str]] = &[
        &["recall", "quokkavault inventory", "--all-kbs"],
        &["state", "Marisol"],
        &["doctor"],
        &["kb", "list"],
        &["usage"],
        &["config", "check"],
        &["agent-info"],
    ];
    for args in cases {
        let label = args.join(" ");
        let out = engram_in(&h)
            .args(*args)
            .arg("--json")
            .assert()
            .success()
            .get_output()
            .clone();
        assert_stdout_is_one_json_object(&out.stdout, &label);
        assert_no_error_envelope(&out.stderr, &label);
    }
}

/// `remember` is the write surface and the one that also writes `note:` lines
/// to stderr. The note must not reach stdout, and the envelope must not reach
/// stderr.
#[test]
fn remember_writes_one_object_to_stdout_and_its_notes_to_stderr() {
    let h = home();
    let out = engram_in(&h)
        .args(["remember", SEED, "--kb", "purity", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let v = assert_stdout_is_one_json_object(&out.stdout, "remember");
    assert_eq!(v["status"], "success", "{v}");
    assert_no_error_envelope(&out.stderr, "remember");
}

/// `agent-info` is the largest payload — compact on a pipe, pretty-printed on
/// a terminal, and over 100 kB either way. Layout is not a purity violation; a
/// banner around it would be. Pinned separately because it is the payload
/// most likely to grow a "generated at …" header.
#[test]
fn the_manifest_is_still_exactly_one_document() {
    let h = home();
    let out = engram_in(&h)
        .args(["agent-info", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let v = assert_stdout_is_one_json_object(&out.stdout, "agent-info");
    assert!(
        out.stdout.len() > 1000,
        "the manifest got suspiciously small — is this still the real thing?"
    );
    assert!(v.is_object(), "{v}");
}

/// An absence report is a SUCCESS, not a failure: exit 0, envelope on stdout,
/// stdout still one object. Pinned because it is the case most likely to be
/// "fixed" into an error, which would move the payload to stderr and break
/// every caller that reads absence reasons.
#[test]
fn an_absence_report_stays_on_stdout_with_exit_zero() {
    let h = seeded_home();
    let out = engram_in(&h)
        .args(["state", "Nobody At All", "--kb", "doesnotexist", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let v = assert_stdout_is_one_json_object(&out.stdout, "state absent");
    assert_ne!(v["status"], "error", "{v}");
    assert_no_error_envelope(&out.stderr, "state absent");
}

// ---------------------------------------------------------------------------
// Clause 1 — tracing must never reach stdout
// ---------------------------------------------------------------------------

/// A fresh HOME runs the migration, which logs at INFO; `RUST_LOG=debug` turns
/// the whole subscriber on. Both go to stderr (main.rs pins `with_writer`), and
/// this test fails the day someone drops that argument.
#[test]
fn rust_log_debug_never_contaminates_json_stdout() {
    let h = home();
    let out = engram_in(&h)
        .env("RUST_LOG", "debug")
        .args(["remember", SEED, "--kb", "purity", "--no-facts", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let v = assert_stdout_is_one_json_object(&out.stdout, "remember RUST_LOG=debug");
    assert_eq!(v["status"], "success", "{v}");
    // Proves the test is testing something: the subscriber did emit.
    assert!(
        !out.stderr.is_empty(),
        "no tracing on stderr — this test no longer exercises the logging path"
    );
}

/// The read path with logging on. Separate from the write path because reads
/// go through a different dispatch arm and only reads are hot enough for
/// someone to add a debug print to.
#[test]
fn rust_log_debug_never_contaminates_a_recall() {
    let h = seeded_home();
    let out = engram_in(&h)
        .env("RUST_LOG", "debug")
        .args(["recall", "quokkavault inventory", "--all-kbs", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_stdout_is_one_json_object(&out.stdout, "recall RUST_LOG=debug");
}

/// The field report from 0.3.1: piping `remember` output produced something a
/// parser choked on. Two sequential writes into one store, each parsed on its
/// own — the second one is where a lazily-initialised banner or a "store
/// upgraded" line would show up.
#[test]
fn two_piped_remembers_each_produce_one_parseable_object() {
    let h = home();
    let mut ids = Vec::new();
    for (n, text) in [
        "First piped note about the quokkavault ledger.",
        "Second piped note about the quokkavault ledger.",
    ]
    .iter()
    .enumerate()
    {
        let label = format!("remember #{}", n + 1);
        let out = engram_in(&h)
            .args(["remember", text, "--kb", "purity", "--no-facts", "--json"])
            .assert()
            .success()
            .get_output()
            .clone();
        let v = assert_stdout_is_one_json_object(&out.stdout, &label);
        assert_eq!(v["status"], "success", "{label}: {v}");
        ids.push(
            v["data"]["handle"]
                .as_str()
                .unwrap_or_else(|| panic!("{label}: no handle: {v}"))
                .to_string(),
        );
    }
    assert_ne!(ids[0], ids[1], "two writes, two handles");
}

// ---------------------------------------------------------------------------
// Clause 2 + 3 — failure paths
// ---------------------------------------------------------------------------

const MISSING: &str = "mem:00000000-0000-0000-0000-000000000000";

/// A mutator against an id that resolves to nothing: `not_found`, exit 3, and
/// NOTHING on stdout. Exit 3 rather than 1 is the interfaces §1 ruling — a
/// retry loop must not spend a paid embedding call on an id that will never
/// exist — so the code and the status are pinned together.
#[test]
fn forget_on_a_missing_handle_leaves_stdout_empty_and_exits_3() {
    let h = seeded_home();
    let out = engram_in(&h)
        .args(["forget", MISSING, "--confirm", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let v = assert_failure_envelope(&out, "forget missing", "not_found", 3);
    assert!(
        v["error"]["suggestion"].is_string(),
        "an error envelope must carry a suggestion: {v}"
    );
}

/// The same class through a different command, because `edit` is the one where
/// a wrong classification costs money.
#[test]
fn edit_on_a_missing_handle_leaves_stdout_empty_and_exits_3() {
    let h = seeded_home();
    let out = engram_in(&h)
        .args(["edit", MISSING, "--content", "replacement text", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_failure_envelope(&out, "edit missing", "not_found", 3);
}

/// A clap-level rejection takes a DIFFERENT road out of the process —
/// `cli::exit_with`, not `CliError::emit_to_stderr` — and it runs before
/// `--json` has even been parsed. It must still produce the same envelope on
/// the same stream with the same exit code, or an agent that mistypes a flag
/// gets an unparseable answer.
#[test]
fn an_unparseable_argv_still_emits_the_envelope_on_stderr() {
    let h = home();
    let out = engram_in(&h)
        .args(["recall", "anything", "--not-a-real-flag", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_failure_envelope(&out, "bad flag", "bad_input", 3);
}

/// Exit 2 is its own class — "your setup is broken, do not retry" — and it must
/// not be reachable only through the taxonomy's default. A keyless shell doing
/// a cloud read is the highest-traffic way to hit it.
///
/// `recall` and not `remember`: the read path emits the envelope and nothing
/// else, so the whole of stderr can be asserted to be one document. Write paths
/// print `note:` lines first.
#[test]
fn a_keyless_cloud_read_exits_2_with_a_config_envelope() {
    let h = home();
    let out = bare_cmd_in(h.path())
        .args(["recall", "anything at all", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let v = assert_failure_envelope(&out, "keyless recall", "config_error", 2);
    let msg = v["error"]["message"].as_str().expect("message");
    assert!(
        msg.contains("GEMINI_API_KEY") || msg.contains("keys.gemini"),
        "exit 2 must name the key to set: {msg}"
    );
}

/// The human branch of the same failure. `ENGRAM_FORCE_TTY=1` makes the binary
/// believe a person is watching, and then it must NOT print JSON — otherwise
/// the seam is decorative and the prose path is untested. stdout stays empty
/// either way, which is the invariant that does not depend on who is watching.
#[test]
fn the_tty_branch_prints_prose_to_stderr_and_still_nothing_to_stdout() {
    let h = seeded_home();
    let out = engram_in(&h)
        .env("ENGRAM_FORCE_TTY", "1")
        .args(["forget", MISSING, "--confirm", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty on failure regardless of the TTY branch:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("error: "), "prose branch expected:\n{text}");
    assert!(text.contains("hint: "), "prose branch expected:\n{text}");
    assert!(
        serde_json::from_str::<Value>(text.trim()).is_err(),
        "the TTY branch must not emit JSON:\n{text}"
    );
    assert_eq!(out.status.code(), Some(3), "exit code is not TTY-dependent");
}
