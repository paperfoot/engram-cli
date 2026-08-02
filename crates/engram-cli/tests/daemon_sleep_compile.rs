//! Wave 4 — the sleep-time compile trigger.
//!
//! Every gate is a fixed function of observable state, so "why did it compile
//! then?" has an answer that fits in a log line. The two that matter most are
//! that LLM extraction is OFF unless somebody turns it on per install — an
//! unattended trigger that can spend money is a bug report waiting to happen —
//! and that the idle signal is STORE-derived, because a daemon watching only
//! its own writer thinks the store is idle while an agent is hammering
//! `engram remember` from another process.

mod daemon_support;

use daemon_support::*;
use engram_cli::context::AppContext;
use engram_cli::engine::sleep::{self, Decision, SleepConfig};
use engram_cli::output::OutputFormat;

fn ctx(store: &Store) -> AppContext {
    AppContext::with_db(OutputFormat::Json, true, store.db()).expect("context")
}

fn seeded(n: usize) -> Store {
    let store = Store::new();
    for i in 0..n {
        store.seed(&[
            "remember",
            &format!("Pressure memory number {i}."),
            "--kb",
            "default",
        ]);
    }
    store
}

fn decision(decisions: &[(String, Decision)], kb: &str) -> &'static str {
    match decisions.iter().find(|(k, _)| k == kb).map(|(_, d)| d) {
        Some(Decision::Submitted) => "submitted",
        Some(Decision::BelowThreshold) => "below_threshold",
        Some(Decision::NotIdle) => "not_idle",
        Some(Decision::DailyCapReached) => "daily_cap",
        Some(Decision::AlreadyLive) => "already_live",
        Some(Decision::NoBudgetHeadroom) => "no_headroom",
        Some(Decision::Disabled) | None => "none",
    }
}

#[tokio::test]
async fn pressure_below_the_threshold_compiles_nothing() {
    let store = seeded(3);
    let ctx = ctx(&store);
    let cfg = SleepConfig {
        min_writes: 25,
        idle_s: 0,
        ..SleepConfig::default()
    };
    let out = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert_eq!(decision(&out, "default"), "below_threshold", "{out:?}");
}

#[tokio::test]
async fn pressure_over_the_threshold_on_an_idle_store_compiles() {
    let store = seeded(3);
    let ctx = ctx(&store);
    let cfg = SleepConfig {
        min_writes: 2,
        idle_s: 0,
        ..SleepConfig::default()
    };
    let out = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert_eq!(decision(&out, "default"), "submitted", "{out:?}");

    let s = store.open();
    let live = s
        .live_job("compile", "default")
        .expect("live")
        .expect("a job");
    assert_eq!(
        live.actor, "daemon:sleep",
        "`jobs list` must be able to say which compiles the user asked for"
    );
    assert_eq!(live.params["trigger"], "sleep_time");
    assert_eq!(
        live.params["llm"], false,
        "an unattended trigger does not spend money by default"
    );
}

#[tokio::test]
async fn a_busy_store_is_not_compiled() {
    // The writes just landed, so the store is not idle — whichever process
    // made them.
    let store = seeded(5);
    let ctx = ctx(&store);
    let cfg = SleepConfig {
        min_writes: 2,
        idle_s: 3600,
        ..SleepConfig::default()
    };
    let out = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert_eq!(
        decision(&out, "default"),
        "not_idle",
        "a store written to seconds ago is not asleep: {out:?}"
    );
}

#[tokio::test]
async fn the_daily_cap_holds() {
    let store = seeded(3);
    let ctx = ctx(&store);
    let cfg = SleepConfig {
        min_writes: 2,
        idle_s: 0,
        max_per_day: 1,
        ..SleepConfig::default()
    };
    let first = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert_eq!(decision(&first, "default"), "submitted");
    // Clear the live job so the cap, not the index, is what refuses.
    store
        .open()
        .abandon_live_jobs("compile", "default", "test")
        .expect("abandon");
    let second = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert_eq!(
        decision(&second, "default"),
        "daily_cap",
        "a misconfigured trigger must not compile in a loop: {second:?}"
    );
}

#[tokio::test]
async fn a_live_job_is_never_duplicated() {
    let store = seeded(3);
    let ctx = ctx(&store);
    let cfg = SleepConfig {
        min_writes: 2,
        idle_s: 0,
        ..SleepConfig::default()
    };
    sleep::tick(&ctx.core, &cfg).await.expect("tick");
    let again = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert_eq!(decision(&again, "default"), "already_live", "{again:?}");
}

#[tokio::test]
async fn disabling_it_makes_the_tick_a_no_op() {
    let store = seeded(30);
    let ctx = ctx(&store);
    let cfg = SleepConfig {
        enabled: false,
        ..SleepConfig::default()
    };
    let out = sleep::tick(&ctx.core, &cfg).await.expect("tick");
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn the_cli_never_runs_the_watcher() {
    // The watcher is spawned by `serve` and by nothing else, so a CLI process
    // cannot compile behind a user's back.
    let store = seeded(30);
    let before = store.open();
    assert!(
        before
            .live_job("compile", "default")
            .expect("live")
            .is_none(),
        "seeding must not have submitted anything"
    );
    drop(before);
    store.seed(&[
        "recall",
        "pressure",
        "--kb",
        "default",
        "--profile",
        "offline",
    ]);
    let after = store.open();
    assert!(
        after
            .live_job("compile", "default")
            .expect("live")
            .is_none(),
        "a CLI process spawns no sleep watcher"
    );
}

#[test]
fn health_reports_the_gates_it_is_deciding_on() {
    let store = seeded(2);
    let daemon = Daemon::start_with(
        &store,
        &[
            ("ENGRAM_SLEEP_COMPILE", "1"),
            ("ENGRAM_SLEEP_COMPILE_MIN_WRITES", "9999"),
            ("ENGRAM_SLEEP_COMPILE_TICK_S", "1"),
        ],
    );
    // One tick's worth of grace, then the numbers must be there.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let (status, body) = daemon.get("/health");
    assert_eq!(status, 200, "{body}");
    let s = &body["data"]["sleep_compile"];
    assert_eq!(s["enabled"], true, "{body}");
    assert_eq!(s["llm"], false, "off by default, everywhere: {body}");
    assert!(
        s["pending_writes"].is_object(),
        "per-KB pressure is reported so a caller can see the gate: {body}"
    );
}
