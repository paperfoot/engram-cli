//! The sleep-time compile trigger.
//!
//! A fixed function of observable state, deliberately. No adaptive schedule,
//! no learned priority, no quiet-hours calendar — so "why did it compile
//! then?" has an answer that fits in a log line, and every gate below is
//! visible on `/health`.
//!
//! Two safety choices are worth naming. LLM extraction is OFF unless someone
//! turns it on per install: an unattended background trigger that can spend
//! money is a bug report waiting to happen. And the idle signal is
//! STORE-derived, not in-process: `Core`'s own metrics only see writes made by
//! this daemon, and every CLI mutation happens in a different process, so a
//! daemon watching itself would consider the store idle while an agent is
//! hammering `engram remember` and compile into the middle of an ingest.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::json;

use super::Core;
use crate::error::CliError;

#[derive(Default)]
pub struct SleepState {
    pub last_tick: Mutex<Option<DateTime<Utc>>>,
    pub pending: Mutex<HashMap<String, i64>>,
    pub next_eligible_at: Mutex<Option<DateTime<Utc>>>,
    pub submitted: std::sync::atomic::AtomicU64,
}

/// Every gate, resolved from the environment once per tick so a running daemon
/// can be reconfigured by restart rather than by recompile.
#[derive(Debug, Clone)]
pub struct SleepConfig {
    pub enabled: bool,
    pub llm: bool,
    pub min_writes: i64,
    pub max_age_s: i64,
    pub idle_s: i64,
    pub max_per_day: i64,
    pub min_headroom_usd: f64,
    pub tick_s: u64,
}

impl Default for SleepConfig {
    fn default() -> Self {
        SleepConfig {
            enabled: true,
            llm: false,
            min_writes: 25,
            max_age_s: 6 * 3600,
            idle_s: 300,
            max_per_day: 4,
            min_headroom_usd: 0.10,
            tick_s: 60,
        }
    }
}

impl SleepConfig {
    pub fn from_env() -> SleepConfig {
        let d = SleepConfig::default();
        SleepConfig {
            enabled: env_bool("ENGRAM_SLEEP_COMPILE", d.enabled),
            llm: env_bool("ENGRAM_SLEEP_COMPILE_LLM", d.llm),
            min_writes: env_i64("ENGRAM_SLEEP_COMPILE_MIN_WRITES", d.min_writes),
            max_age_s: env_i64("ENGRAM_SLEEP_COMPILE_MAX_AGE_S", d.max_age_s),
            idle_s: env_i64("ENGRAM_SLEEP_COMPILE_IDLE_S", d.idle_s),
            max_per_day: env_i64("ENGRAM_SLEEP_COMPILE_MAX_PER_DAY", d.max_per_day),
            min_headroom_usd: env_f64("ENGRAM_SLEEP_COMPILE_MIN_HEADROOM_USD", d.min_headroom_usd),
            tick_s: env_i64("ENGRAM_SLEEP_COMPILE_TICK_S", d.tick_s as i64).max(1) as u64,
        }
    }
}

/// Why a KB was not compiled this tick, or that it was. Returned rather than
/// logged only, so the test suite can assert on the decision instead of on a
/// side effect it has to wait for.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Submitted,
    BelowThreshold,
    NotIdle,
    DailyCapReached,
    AlreadyLive,
    NoBudgetHeadroom,
    Disabled,
}

/// One pass over every KB. Called on a timer by the daemon; called directly by
/// tests, which is why it takes its config rather than reading the process
/// environment inside the loop.
pub async fn tick(core: &Core, cfg: &SleepConfig) -> Result<Vec<(String, Decision)>, CliError> {
    if !cfg.enabled {
        return Ok(Vec::new());
    }
    let kbs = core
        .read(|s| {
            Ok(s.list_kbs()?
                .into_iter()
                .map(|k| k.name)
                .collect::<Vec<_>>())
        })
        .await?;
    let now = Utc::now();
    let mut out = Vec::new();
    let mut pending_by_kb = HashMap::new();
    let mut next_eligible: Option<DateTime<Utc>> = None;

    for kb in kbs {
        let kb_for_read = kb.clone();
        let cutoff = now - ChronoDuration::seconds(86_400);
        let (pressure, newest_write, auto_today, live) = core
            .read(move |s| {
                Ok((
                    s.compile_pressure(&kb_for_read)?,
                    s.newest_write_ts(&kb_for_read)?,
                    s.auto_compiles_since(&kb_for_read, cutoff)?,
                    s.live_job("compile", &kb_for_read)?.is_some(),
                ))
            })
            .await?;
        pending_by_kb.insert(kb.clone(), pressure.pending);

        if live {
            out.push((kb, Decision::AlreadyLive));
            continue;
        }
        let aged = pressure
            .oldest_pending_at
            .map(|t| (now - t).num_seconds() >= cfg.max_age_s)
            .unwrap_or(false);
        if pressure.pending < cfg.min_writes && !(aged && pressure.pending > 0) {
            out.push((kb, Decision::BelowThreshold));
            continue;
        }
        // The store's own clock, plus this process's writes as a cheap upper
        // bound. Either one being recent means "not idle".
        let idle_secs = newest_write
            .map(|t| (now - t).num_seconds())
            .unwrap_or(i64::MAX);
        let in_process_idle = core
            .seconds_since_last_write()
            .map(|s| s as i64)
            .unwrap_or(i64::MAX);
        if idle_secs.min(in_process_idle) < cfg.idle_s {
            let eligible =
                now + ChronoDuration::seconds(cfg.idle_s - idle_secs.min(in_process_idle));
            next_eligible =
                Some(next_eligible.map_or(eligible, |e: DateTime<Utc>| e.min(eligible)));
            out.push((kb, Decision::NotIdle));
            continue;
        }
        if auto_today >= cfg.max_per_day {
            out.push((kb, Decision::DailyCapReached));
            continue;
        }
        let mut params = json!({ "llm": cfg.llm, "trigger": "sleep_time" });
        if cfg.llm {
            let kb_for_budget = kb.clone();
            let headroom = core
                .read(move |s| {
                    Ok(s.get_usage_budget(&format!("kb:{kb_for_budget}"))?
                        .and_then(|b| b.daily_usd)
                        .unwrap_or(f64::INFINITY))
                })
                .await?;
            if headroom <= cfg.min_headroom_usd {
                out.push((kb, Decision::NoBudgetHeadroom));
                continue;
            }
            if headroom.is_finite() {
                params["budget_usd"] = json!(headroom);
            }
        }

        // `daemon:sleep` is what makes `jobs list --kind compile` able to say
        // which compiles the user asked for and which the daemon decided on.
        let previous_actor = core.actor();
        core.set_actor("daemon:sleep");
        let submitted = core
            .submit_job("compile".to_string(), kb.clone(), params)
            .await;
        core.set_actor(previous_actor);
        match submitted {
            Ok((row, already)) => {
                if already {
                    out.push((kb, Decision::AlreadyLive));
                } else {
                    tracing::info!(
                        kb = %kb, job = %row.id, pending = pressure.pending,
                        idle_s = idle_secs, llm = cfg.llm,
                        "sleep-time compile submitted"
                    );
                    core.sleep_state()
                        .submitted
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    out.push((kb, Decision::Submitted));
                }
            }
            Err(e) => {
                tracing::warn!(kb = %kb, "sleep-time compile could not be submitted: {e}");
                out.push((kb, Decision::AlreadyLive));
            }
        }
    }

    let state = core.sleep_state();
    if let Ok(mut p) = state.pending.lock() {
        *p = pending_by_kb;
    }
    if let Ok(mut t) = state.last_tick.lock() {
        *t = Some(now);
    }
    if let Ok(mut n) = state.next_eligible_at.lock() {
        *n = next_eligible;
    }
    Ok(out)
}

/// `/health.sleep_compile`.
pub fn health(core: &Core, cfg: &SleepConfig) -> serde_json::Value {
    let state = core.sleep_state();
    json!({
        "enabled": cfg.enabled,
        "llm": cfg.llm,
        "pending_writes": state.pending.lock().ok().map(|p| p.clone()).unwrap_or_default(),
        "next_eligible_at": state
            .next_eligible_at
            .lock()
            .ok()
            .and_then(|n| *n)
            .map(|t| t.to_rfc3339()),
        "submitted_total": state.submitted.load(std::sync::atomic::Ordering::Relaxed),
    })
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => !matches!(v.trim(), "0" | "false" | "no" | "off"),
        Err(_) => default,
    }
}

fn env_i64(key: &str, default: i64) -> i64 {
    match std::env::var(key) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            tracing::warn!("{key}={v} is not a number; using {default}");
            default
        }),
        Err(_) => default,
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    match std::env::var(key) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            tracing::warn!("{key}={v} is not a number; using {default}");
            default
        }),
        Err(_) => default,
    }
}
