//! Whether a call site asks the calibrated judge, and which provider answers.
//!
//! The judge itself lives in [`engram_llm::judge`]. This module is the single
//! place that answers the two questions every judged site asks first:
//!
//! - [`judge_mode`]: per site, `off` (never asked; the default), `shadow`
//!   (asked and logged, today's behaviour kept) or `on` (the verdict is
//!   used). Env `ENGRAM_JUDGE_<SITE>` overrides config `judge.<site>`; blank
//!   means unset, as for every engram setting. Any other value is a config
//!   error (exit 2), never a silent default.
//! - [`resolve_judge`]: `ENGRAM_JUDGE_STUB` (a scripted offline judge, see
//!   [`StubJudge`]) beats everything. Otherwise `--offline` and
//!   `ENGRAM_BENCH_FORCE_STUB` mean no judge, because a judge call is a
//!   network call. Then TypeSafe direct (`TYPESAFE_API_KEY` /
//!   `keys.typesafe`), then TypeSafe via OpenRouter (`OPENROUTER_API_KEY` /
//!   `keys.openrouter`), else none.
//!
//! A judge request carries a memory to a third party, and engram stores
//! personal and medical memories, so every site defaults to `off`.

use crate::commands::config;
use crate::context::AppContext;
use crate::error::CliError;
use engram_llm::judge::stub::StubJudge;
use engram_llm::judge::typesafe::{TypeSafeJudge, TypeSafeRoute};
use engram_llm::judge::{Judge, JudgeResponse};
use serde::Serialize;
use serde_json::json;

/// Env var holding a scripted offline judge spec (test seam).
pub const STUB_ENV: &str = "ENGRAM_JUDGE_STUB";

/// How a site uses the judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeMode {
    /// Never asked.
    Off,
    /// Asked and logged; the site keeps its judge-free behaviour.
    Shadow,
    /// Asked, and the verdict is used.
    On,
}

impl JudgeMode {
    /// `off`, `shadow` or `on`, trimmed, any ASCII case. Nothing else: a
    /// typo must not quietly turn a judge off (or on).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(JudgeMode::Off),
            "shadow" => Some(JudgeMode::Shadow),
            "on" => Some(JudgeMode::On),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JudgeMode::Off => "off",
            JudgeMode::Shadow => "shadow",
            JudgeMode::On => "on",
        }
    }
}

/// A place in engram that can ask the judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeSite {
    /// Is an extracted fact or claim supported by its source text?
    Grounding,
    /// Does a new fact end or contradict an existing one?
    Adjudicate,
    /// Does a new fact restate an existing one?
    Dedupe,
}

impl JudgeSite {
    pub const ALL: [JudgeSite; 3] = [
        JudgeSite::Grounding,
        JudgeSite::Adjudicate,
        JudgeSite::Dedupe,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "grounding",
            JudgeSite::Adjudicate => "adjudicate",
            JudgeSite::Dedupe => "dedupe",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "ENGRAM_JUDGE_GROUNDING",
            JudgeSite::Adjudicate => "ENGRAM_JUDGE_ADJUDICATE",
            JudgeSite::Dedupe => "ENGRAM_JUDGE_DEDUPE",
        }
    }

    pub fn config_key(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "judge.grounding",
            JudgeSite::Adjudicate => "judge.adjudicate",
            JudgeSite::Dedupe => "judge.dedupe",
        }
    }

    /// The usage-ledger operation for this site's judge calls.
    pub fn operation(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "judge_grounding",
            JudgeSite::Adjudicate => "judge_adjudicate",
            JudgeSite::Dedupe => "judge_dedupe",
        }
    }
}

/// The mode for `site`: env, then config.toml, then `off`.
pub fn judge_mode(site: JudgeSite) -> Result<JudgeMode, CliError> {
    let env = std::env::var(site.env_var()).ok();
    mode_from(site, env.as_deref(), || {
        config::config_value(site.config_key())
    })
}

/// Pure core of [`judge_mode`], testable without process-global env.
fn mode_from(
    site: JudgeSite,
    env: Option<&str>,
    file: impl FnOnce() -> Result<Option<toml::Value>, CliError>,
) -> Result<JudgeMode, CliError> {
    let invalid = |setting: String| {
        CliError::Config(format!(
            "{setting} is not a judge mode; expected off, shadow or on"
        ))
    };
    if let Some(v) = env.filter(|v| !v.trim().is_empty()) {
        return JudgeMode::parse(v).ok_or_else(|| invalid(format!("{}={v:?}", site.env_var())));
    }
    match file()? {
        None => Ok(JudgeMode::Off),
        Some(toml::Value::String(s)) if s.trim().is_empty() => Ok(JudgeMode::Off),
        Some(toml::Value::String(s)) => {
            JudgeMode::parse(&s).ok_or_else(|| invalid(format!("{} = {s:?}", site.config_key())))
        }
        Some(other) => Err(invalid(format!("{} = {other}", site.config_key()))),
    }
}

/// Where judge calls would go. Holds the key only until the judge is built.
enum Route {
    Stub(StubJudge),
    Direct(String),
    OpenRouter(String),
}

impl Route {
    fn name(&self) -> &'static str {
        match self {
            Route::Stub(_) => "stub",
            Route::Direct(_) => "typesafe",
            Route::OpenRouter(_) => "openrouter",
        }
    }
}

fn route(offline: bool) -> Result<Option<Route>, CliError> {
    route_from(
        std::env::var(STUB_ENV).ok().as_deref(),
        offline,
        config::stub_forced(),
        || config::resolve_secret("TYPESAFE_API_KEY", "keys.typesafe"),
        || config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter"),
    )
}

/// Pure core of [`resolve_judge`]: the precedence without the environment.
fn route_from(
    stub: Option<&str>,
    offline: bool,
    force_stub: bool,
    typesafe_key: impl FnOnce() -> Option<String>,
    openrouter_key: impl FnOnce() -> Option<String>,
) -> Result<Option<Route>, CliError> {
    if let Some(spec) = stub.filter(|s| !s.trim().is_empty()) {
        let stub = StubJudge::parse(spec)
            .map_err(|e| CliError::Config(format!("{STUB_ENV} is not a valid spec: {e}")))?;
        return Ok(Some(Route::Stub(stub)));
    }
    if offline || force_stub {
        return Ok(None);
    }
    if let Some(key) = typesafe_key() {
        return Ok(Some(Route::Direct(key)));
    }
    Ok(openrouter_key().map(Route::OpenRouter))
}

/// The judge to ask, or `None` when there is none (no key, `--offline`, or
/// `ENGRAM_BENCH_FORCE_STUB`). Errors only on a malformed `ENGRAM_JUDGE_STUB`.
/// Build it once per command: the TypeSafe client pools its connections.
pub fn resolve_judge(offline: bool) -> Result<Option<Box<dyn Judge>>, CliError> {
    Ok(route(offline)?.map(|r| -> Box<dyn Judge> {
        match r {
            Route::Stub(stub) => Box::new(stub),
            Route::Direct(key) => Box::new(TypeSafeJudge::new(TypeSafeRoute::Direct { key })),
            Route::OpenRouter(key) => {
                Box::new(TypeSafeJudge::new(TypeSafeRoute::OpenRouter { key }))
            }
        }
    }))
}

/// `typesafe` | `openrouter` | `stub` | `none`: the route [`resolve_judge`]
/// would take, without building a client.
pub fn route_name(offline: bool) -> Result<&'static str, CliError> {
    Ok(route(offline)?.map_or("none", |r| r.name()))
}

/// One usage-ledger row for a judge response. Stub responses are free and
/// not recorded. Never fatal: the call is paid the moment the provider
/// answers, so a ledger failure is a warning (the `record_embed_spend`
/// convention).
pub fn record_judge_usage(
    ctx: &AppContext,
    operation: &str,
    response: &JudgeResponse,
    kb: Option<&str>,
    diary: Option<&str>,
) {
    record_usage(&ctx.store, operation, response, kb, diary);
}

fn record_usage(
    store: &engram_storage::SqliteStore,
    operation: &str,
    response: &JudgeResponse,
    kb: Option<&str>,
    diary: Option<&str>,
) {
    if response.provider == "stub" {
        return;
    }
    if let Err(e) = store.record_usage_event(
        response.provider,
        operation,
        Some(&response.model),
        kb,
        diary,
        i64::from(response.requests),
        response.answers.len() as i64,
        response.input_tokens as i64,
        0,
        0.0,
        response.cost_usd,
        json!({ "latency_ms": response.latency_ms }),
    ) {
        tracing::warn!("could not record {operation} usage: {e}");
    }
}

/// The judge block `config show`, `config check` and `doctor` print.
#[derive(Debug, Serialize)]
pub struct JudgeStatus {
    /// Each site's mode; `null` when its setting is invalid (see `errors`).
    pub grounding: Option<JudgeMode>,
    pub adjudicate: Option<JudgeMode>,
    pub dedupe: Option<JudgeMode>,
    /// The route a networked command would take (see [`route_name`]).
    pub route: &'static str,
    /// Settings that make judged commands exit 2.
    pub errors: Vec<String>,
    /// A site in shadow or on with no route: it runs without the judge.
    pub warnings: Vec<String>,
}

/// Every site's mode and the route, with problems collected, not raised:
/// the status surfaces exist to show a broken setting, not to die on it.
pub fn status() -> JudgeStatus {
    let mut errors = Vec::new();
    let modes: Vec<Option<JudgeMode>> = JudgeSite::ALL
        .iter()
        .map(|&site| {
            judge_mode(site)
                .map_err(|e| errors.push(e.to_string()))
                .ok()
        })
        .collect();
    let route = route_name(false).unwrap_or_else(|e| {
        errors.push(e.to_string());
        "none"
    });
    JudgeStatus {
        grounding: modes[0],
        adjudicate: modes[1],
        dedupe: modes[2],
        route,
        warnings: warnings_for(&modes, route),
        errors,
    }
}

/// `modes` in [`JudgeSite::ALL`] order.
fn warnings_for(modes: &[Option<JudgeMode>], route: &str) -> Vec<String> {
    if route != "none" {
        return Vec::new();
    }
    JudgeSite::ALL
        .iter()
        .zip(modes)
        .filter_map(|(site, mode)| match mode {
            Some(m @ (JudgeMode::Shadow | JudgeMode::On)) => Some(format!(
                "{} is {} but no judge is reachable, so the site runs without it: set \
                 TYPESAFE_API_KEY (or `engram config set keys.typesafe <KEY>`) or \
                 OPENROUTER_API_KEY; --offline and ENGRAM_BENCH_FORCE_STUB also disable the judge",
                site.config_key(),
                m.as_str()
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_llm::judge::Answer;
    use std::collections::BTreeMap;

    fn no_file() -> Result<Option<toml::Value>, CliError> {
        Ok(None)
    }

    fn file(v: toml::Value) -> impl FnOnce() -> Result<Option<toml::Value>, CliError> {
        move || Ok(Some(v))
    }

    #[test]
    fn modes_parse_strictly() {
        assert_eq!(JudgeMode::parse("off"), Some(JudgeMode::Off));
        assert_eq!(JudgeMode::parse(" Shadow "), Some(JudgeMode::Shadow));
        assert_eq!(JudgeMode::parse("ON"), Some(JudgeMode::On));
        for bad in [
            "",
            "true",
            "false",
            "1",
            "0",
            "yes",
            "enabled",
            "of",
            "shadow,on",
        ] {
            assert_eq!(JudgeMode::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn env_beats_config_and_unset_is_off() {
        let site = JudgeSite::Grounding;
        let shadow = || file(toml::Value::String("shadow".into()));
        assert_eq!(
            mode_from(site, Some("on"), shadow()).unwrap(),
            JudgeMode::On
        );
        assert_eq!(mode_from(site, None, shadow()).unwrap(), JudgeMode::Shadow);
        // Blank env means unset, as for every engram setting.
        assert_eq!(
            mode_from(site, Some("  "), shadow()).unwrap(),
            JudgeMode::Shadow
        );
        assert_eq!(mode_from(site, None, no_file).unwrap(), JudgeMode::Off);
        assert_eq!(
            mode_from(site, None, file(toml::Value::String(String::new()))).unwrap(),
            JudgeMode::Off
        );
    }

    #[test]
    fn an_unknown_value_is_a_config_error_naming_the_setting() {
        let err = mode_from(JudgeSite::Dedupe, Some("true"), no_file).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("ENGRAM_JUDGE_DEDUPE"), "{err}");

        let err = mode_from(
            JudgeSite::Adjudicate,
            None,
            file(toml::Value::String("maybe".into())),
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
        assert!(err.to_string().contains("judge.adjudicate"), "{err}");

        // A TOML boolean is not a mode either.
        let err =
            mode_from(JudgeSite::Grounding, None, file(toml::Value::Boolean(true))).unwrap_err();
        assert!(matches!(err, CliError::Config(_)));

        // An unreadable config file is not "off".
        let err = mode_from(JudgeSite::Grounding, None, || {
            Err(CliError::Config("invalid TOML".into()))
        })
        .unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
    }

    fn key(k: &str) -> impl FnOnce() -> Option<String> + '_ {
        move || Some(k.to_string())
    }

    fn none() -> Option<String> {
        None
    }

    fn name(r: Result<Option<Route>, CliError>) -> &'static str {
        r.unwrap().map_or("none", |r| r.name())
    }

    #[test]
    fn the_stub_beats_offline_force_stub_and_keys() {
        let r = route_from(Some("0.9"), true, true, key("ts"), key("or"));
        assert_eq!(name(r), "stub");
    }

    #[test]
    fn offline_and_force_stub_never_reach_the_network() {
        assert_eq!(
            name(route_from(None, true, false, key("ts"), key("or"))),
            "none"
        );
        assert_eq!(
            name(route_from(None, false, true, key("ts"), key("or"))),
            "none"
        );
    }

    #[test]
    fn typesafe_direct_beats_openrouter() {
        assert_eq!(
            name(route_from(None, false, false, key("ts"), key("or"))),
            "typesafe"
        );
        assert_eq!(
            name(route_from(None, false, false, none, key("or"))),
            "openrouter"
        );
        assert_eq!(name(route_from(None, false, false, none, none)), "none");
        // A blank stub falls through to the keys.
        assert_eq!(
            name(route_from(Some(" "), false, false, key("ts"), none)),
            "typesafe"
        );
    }

    #[test]
    fn a_malformed_stub_is_a_config_error() {
        let err = route_from(Some("not json"), false, false, none, none)
            .err()
            .expect("error");
        assert!(matches!(err, CliError::Config(_)));
        assert!(err.to_string().contains(STUB_ENV));
    }

    #[test]
    fn warnings_only_for_a_live_mode_with_no_route() {
        let modes = [Some(JudgeMode::Shadow), Some(JudgeMode::Off), None];
        let w = warnings_for(&modes, "none");
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("judge.grounding is shadow"), "{}", w[0]);
        assert!(warnings_for(&modes, "typesafe").is_empty());
        assert!(warnings_for(&[Some(JudgeMode::Off); 3], "none").is_empty());
    }

    fn response(provider: &'static str) -> JudgeResponse {
        JudgeResponse {
            provider,
            model: "jev-1.13.0".into(),
            answers: BTreeMap::from([
                ("a".to_string(), Answer::Noul(0.9)),
                ("b".to_string(), Answer::Noul(0.1)),
            ]),
            input_tokens: 1200,
            cost_usd: 0.0000504,
            latency_ms: 250,
            requests: 1,
        }
    }

    #[test]
    fn usage_is_recorded_per_paid_response_and_never_for_the_stub() {
        let store = engram_storage::SqliteStore::open_in_memory().unwrap();
        record_usage(
            &store,
            "judge_grounding",
            &response("stub"),
            Some("kb"),
            None,
        );
        assert!(store.usage_summary(None, None).unwrap().is_empty());

        record_usage(
            &store,
            "judge_grounding",
            &response("typesafe"),
            Some("kb"),
            Some("d"),
        );
        let rows = store.usage_summary(None, None).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.provider, "typesafe");
        assert_eq!(row.operation, "judge_grounding");
        assert_eq!(row.model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(row.request_count, 1);
        assert_eq!(row.item_count, 2);
        assert_eq!(row.input_tokens_estimated, 1200);
        assert!((row.cost_usd_estimated - 0.0000504).abs() < 1e-12);
    }
}
