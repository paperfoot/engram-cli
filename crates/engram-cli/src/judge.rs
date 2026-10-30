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
//! - [`site_judge`]: the ONE way a site gets a judge. `None` when the site
//!   is off, which is also the consent gate below; otherwise the route.
//!   Route precedence: `ENGRAM_JUDGE_STUB` (a scripted offline judge, see
//!   [`StubJudge`]) beats everything. Otherwise `--offline` and
//!   `ENGRAM_BENCH_FORCE_STUB` mean no judge, because a judge call is a
//!   network call. Then TypeSafe direct (`TYPESAFE_API_KEY` /
//!   `keys.typesafe`), then TypeSafe via OpenRouter (`OPENROUTER_API_KEY` /
//!   `keys.openrouter`), else none.
//!
//! Consent, one rule for every site: a judge request carries a memory to a
//! third party, and engram stores personal and medical memories, so a site
//! may send content only if a TypeSafe key is configured (`TYPESAFE_API_KEY`
//! / `keys.typesafe`: configuring TypeSafe is opting into TypeSafe) or its
//! own setting is explicitly `shadow` or `on`. An OpenRouter key alone was
//! set for extraction and is not consent. `relevance` (recall abstention)
//! and `adjudicate` (`remember --autonomy full`) default to `on` with a
//! TypeSafe key; grounding and dedupe default to `off` regardless. See
//! [`mode_source`].

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
    /// Does a recalled passage answer the query?
    Relevance,
}

impl JudgeSite {
    pub const ALL: [JudgeSite; 4] = [
        JudgeSite::Grounding,
        JudgeSite::Adjudicate,
        JudgeSite::Dedupe,
        JudgeSite::Relevance,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "grounding",
            JudgeSite::Adjudicate => "adjudicate",
            JudgeSite::Dedupe => "dedupe",
            JudgeSite::Relevance => "relevance",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "ENGRAM_JUDGE_GROUNDING",
            JudgeSite::Adjudicate => "ENGRAM_JUDGE_ADJUDICATE",
            JudgeSite::Dedupe => "ENGRAM_JUDGE_DEDUPE",
            JudgeSite::Relevance => "ENGRAM_JUDGE_RELEVANCE",
        }
    }

    pub fn config_key(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "judge.grounding",
            JudgeSite::Adjudicate => "judge.adjudicate",
            JudgeSite::Dedupe => "judge.dedupe",
            JudgeSite::Relevance => "judge.relevance",
        }
    }

    /// Whether any command in this build asks the judge for this site.
    /// Dedupe is read and reported only: setting it to shadow or on changes
    /// nothing, and status says so. Adjudicate is asked by `remember
    /// --autonomy full`.
    pub fn consulted(self) -> bool {
        !matches!(self, JudgeSite::Dedupe)
    }

    /// The mode when neither env nor config sets one. `consented`: the user
    /// configured TypeSafe itself (see [`typesafe_consented`]). Relevance and
    /// adjudicate are then on: their evals cleared the bar (see
    /// `questions::RELEVANCE_FLOOR` and `fact_write::conflict_rule`). Every
    /// other site, and every site without consent, is off.
    pub fn default_mode(self, consented: bool) -> JudgeMode {
        match self {
            JudgeSite::Relevance | JudgeSite::Adjudicate if consented => JudgeMode::On,
            _ => JudgeMode::Off,
        }
    }

    /// The usage-ledger operation for this site's judge calls.
    pub fn operation(self) -> &'static str {
        match self {
            JudgeSite::Grounding => "judge_grounding",
            JudgeSite::Adjudicate => "judge_adjudicate",
            JudgeSite::Dedupe => "judge_dedupe",
            JudgeSite::Relevance => "judge_relevance",
        }
    }
}

/// The mode for `site`: env, then config.toml, then the site's default.
pub fn judge_mode(site: JudgeSite) -> Result<JudgeMode, CliError> {
    let env = std::env::var(site.env_var()).ok();
    mode_from(
        site,
        env.as_deref(),
        || config::config_value(site.config_key()),
        typesafe_consented,
    )
}

/// Whether the user opted into TypeSafe judging by configuring TypeSafe: a
/// TypeSafe key (`TYPESAFE_API_KEY` / `keys.typesafe`) resolves. An
/// OpenRouter key is not consent: it was set for extraction. The test stub
/// is a route, not consent: tests opt in like users do.
pub fn typesafe_consented() -> bool {
    config::resolve_secret("TYPESAFE_API_KEY", "keys.typesafe").is_some()
}

/// Why `site` has the mode it has, for `config show`, `config check` and
/// `doctor`: set by env, set in config, or which default applies.
pub fn mode_source(site: JudgeSite) -> String {
    if std::env::var(site.env_var()).is_ok_and(|v| !v.trim().is_empty()) {
        return format!("set by {}", site.env_var());
    }
    if matches!(config::config_value(site.config_key()), Ok(Some(toml::Value::String(s))) if !s.trim().is_empty())
    {
        return format!("set by {} in config.toml", site.config_key());
    }
    default_why(site, typesafe_consented())
}

fn default_why(site: JudgeSite, consented: bool) -> String {
    let key = site.config_key();
    match site.default_mode(consented) {
        JudgeMode::On => format!(
            "default on: a TypeSafe key is configured, which opts into TypeSafe judging; \
             `engram config set {key} off` stops it"
        ),
        _ if site.default_mode(true) == JudgeMode::On => format!(
            "default off: no TypeSafe key is configured, and an OpenRouter key alone is not \
             consent to send memory text to TypeSafe; `engram config set {key} on` opts in \
             through either key"
        ),
        _ => format!("default off: `engram config set {key} on` opts in"),
    }
}

/// Pure core of [`judge_mode`], testable without process-global env.
/// `consented` is asked only when the site's default is needed.
fn mode_from(
    site: JudgeSite,
    env: Option<&str>,
    file: impl FnOnce() -> Result<Option<toml::Value>, CliError>,
    consented: impl FnOnce() -> bool,
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
        None => Ok(site.default_mode(consented())),
        Some(toml::Value::String(s)) if s.trim().is_empty() => Ok(site.default_mode(consented())),
        Some(toml::Value::String(s)) => {
            JudgeMode::parse(&s).ok_or_else(|| invalid(format!("{} = {s:?}", site.config_key())))
        }
        Some(other) => Err(invalid(format!("{} = {other}", site.config_key()))),
    }
}

/// The fix for a config error this module raised, or `None` for any other
/// message. A bad mode names its env var or config key first (see
/// [`mode_from`]); a bad stub names [`STUB_ENV`] first. Neither is fixed by
/// probing keys, so neither gets `config check` advice.
pub fn config_error_suggestion(message: &str) -> Option<String> {
    for site in JudgeSite::ALL {
        let (env, key) = (site.env_var(), site.config_key());
        if message.starts_with(&format!("{env}=")) {
            return Some(format!(
                "set {env} to off|shadow|on, or unset it (it overrides config), then rerun the \
                 engram command; to turn the judge off, unset it and run `engram config set {key} off`"
            ));
        }
        if message.starts_with(&format!("{key} =")) {
            return Some(format!(
                "{key} takes off|shadow|on: run `engram config set {key} off` (or shadow or on), \
                 then rerun the engram command"
            ));
        }
    }
    message.starts_with(STUB_ENV).then(|| {
        format!(
            "{STUB_ENV} is a test seam: unset it outside tests, or give it a JSON number or \
             object of rules, then rerun the engram command"
        )
    })
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

/// The judge `site` may ask, with its mode, or `None` when the site is off
/// (unset with no TypeSafe key is off: the consent rule) or no judge is
/// reachable (no key, `--offline`, `ENGRAM_BENCH_FORCE_STUB`). Every judged
/// site resolves through here, so no site can send content without consent.
/// A bad setting is exit 2. Build it once per command: the TypeSafe client
/// pools its connections.
pub fn site_judge(
    site: JudgeSite,
    offline: bool,
) -> Result<Option<(JudgeMode, Box<dyn Judge>)>, CliError> {
    let mode = judge_mode(site)?;
    if mode == JudgeMode::Off {
        return Ok(None);
    }
    Ok(resolve_judge(offline)?.map(|judge| (mode, judge)))
}

/// True when `site` is off but a judge is reachable: the consent rule, not
/// a missing key, is what keeps its content on this machine.
pub fn withheld(site: JudgeSite, offline: bool) -> Result<bool, CliError> {
    Ok(judge_mode(site)? == JudgeMode::Off && route_name(offline)? != "none")
}

/// The judge to ask, or `None` when there is none (no key, `--offline`, or
/// `ENGRAM_BENCH_FORCE_STUB`). Errors only on a malformed `ENGRAM_JUDGE_STUB`.
/// Private: sites go through [`site_judge`], which applies consent.
fn resolve_judge(offline: bool) -> Result<Option<Box<dyn Judge>>, CliError> {
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

/// [`record_judge_usage`] for a surface that holds a store, not an
/// `AppContext` (the daemon).
pub fn record_store_judge_usage(
    store: &engram_storage::SqliteStore,
    operation: &str,
    response: &JudgeResponse,
    kb: Option<&str>,
    diary: Option<&str>,
) {
    record_usage(store, operation, response, kb, diary);
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
    pub relevance: Option<JudgeMode>,
    /// Why `adjudicate` and `relevance` have their modes: set by env, set
    /// in config, or which default applies (on only with a TypeSafe key).
    pub adjudicate_source: String,
    pub relevance_source: String,
    /// The route a networked command would take (see [`route_name`]);
    /// `null` when `ENGRAM_JUDGE_STUB` is malformed (see `errors`).
    pub route: Option<&'static str>,
    /// Settings that make judged commands exit 2.
    pub errors: Vec<String>,
    /// A site in shadow or on with no route (it runs without the judge),
    /// answered by the test stub, or not consulted by any command.
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
    let route = route_name(false)
        .map_err(|e| errors.push(e.to_string()))
        .ok();
    // A broken config.toml fails every site the same way: report it once.
    errors.dedup();
    JudgeStatus {
        grounding: modes[0],
        adjudicate: modes[1],
        dedupe: modes[2],
        relevance: modes[3],
        adjudicate_source: mode_source(JudgeSite::Adjudicate),
        relevance_source: mode_source(JudgeSite::Relevance),
        route,
        warnings: warnings_for(&modes, route),
        errors,
    }
}

/// `modes` in [`JudgeSite::ALL`] order. A site no command consults warns
/// whenever it is shadow or on, whatever the route: a key would not make it
/// do anything. A consulted site in shadow or on warns when no judge answers
/// it, or when the test stub does. An unresolvable route is an error,
/// already reported, not a reason to warn.
fn warnings_for(modes: &[Option<JudgeMode>], route: Option<&str>) -> Vec<String> {
    let why = match route {
        Some("none") => Some(
            "no judge is reachable, so the site runs without it: set TYPESAFE_API_KEY (or \
             `engram config set keys.typesafe <KEY>`) or OPENROUTER_API_KEY; --offline and \
             ENGRAM_BENCH_FORCE_STUB also disable the judge",
        ),
        Some("stub") => Some(
            "ENGRAM_JUDGE_STUB is set, so scripted answers stand in for the judge: unset it \
             outside tests",
        ),
        _ => None,
    };
    JudgeSite::ALL
        .iter()
        .zip(modes)
        .filter_map(|(site, mode)| {
            let m = match mode {
                Some(m @ (JudgeMode::Shadow | JudgeMode::On)) => m.as_str(),
                _ => return None,
            };
            let key = site.config_key();
            if !site.consulted() {
                return Some(format!(
                    "{key} is {m} but no command consults it in this build; it has no effect"
                ));
            }
            why.map(|why| format!("{key} is {m} but {why}"))
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

    /// [`mode_from`] with a TypeSafe key configured.
    fn mode_ts(
        site: JudgeSite,
        env: Option<&str>,
        file: impl FnOnce() -> Result<Option<toml::Value>, CliError>,
    ) -> Result<JudgeMode, CliError> {
        mode_from(site, env, file, || true)
    }

    #[test]
    fn relevance_defaults_on_only_when_typesafe_itself_is_configured() {
        let site = JudgeSite::Relevance;
        // Only an OpenRouter key (set for extraction): no consent, off.
        assert_eq!(
            mode_from(site, None, no_file, || false).unwrap(),
            JudgeMode::Off
        );
        assert_eq!(
            mode_from(site, Some(" "), no_file, || false).unwrap(),
            JudgeMode::Off
        );
        // A TypeSafe key is consent: on.
        assert_eq!(
            mode_from(site, None, no_file, || true).unwrap(),
            JudgeMode::On
        );
        // An explicit setting works through either route.
        for m in ["on", "shadow"] {
            let want = JudgeMode::parse(m).unwrap();
            assert_eq!(mode_from(site, Some(m), no_file, || false).unwrap(), want);
            assert_eq!(
                mode_from(site, None, file(toml::Value::String(m.into())), || false).unwrap(),
                want
            );
        }
        // Consent is only asked when the default is needed.
        assert_eq!(
            mode_from(site, Some("on"), no_file, || panic!("asked")).unwrap(),
            JudgeMode::On
        );
        let site = JudgeSite::Relevance;
        assert!(default_why(site, false).starts_with("default off: no TypeSafe key"));
        assert!(default_why(site, true).starts_with("default on"));
        assert!(default_why(JudgeSite::Grounding, true).starts_with("default off"));
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
        assert_eq!(mode_ts(site, Some("on"), shadow()).unwrap(), JudgeMode::On);
        assert_eq!(mode_ts(site, None, shadow()).unwrap(), JudgeMode::Shadow);
        // Blank env means unset, as for every engram setting.
        assert_eq!(
            mode_ts(site, Some("  "), shadow()).unwrap(),
            JudgeMode::Shadow
        );
        assert_eq!(mode_ts(site, None, no_file).unwrap(), JudgeMode::Off);
        assert_eq!(
            mode_ts(site, None, file(toml::Value::String(String::new()))).unwrap(),
            JudgeMode::Off
        );
    }

    #[test]
    fn relevance_alone_defaults_on_and_off_still_turns_it_off() {
        assert_eq!(
            mode_ts(JudgeSite::Relevance, None, no_file).unwrap(),
            JudgeMode::On
        );
        assert_eq!(
            mode_ts(JudgeSite::Relevance, Some(" "), no_file).unwrap(),
            JudgeMode::On
        );
        assert_eq!(
            mode_ts(JudgeSite::Relevance, Some("off"), no_file).unwrap(),
            JudgeMode::Off
        );
        assert_eq!(
            mode_ts(
                JudgeSite::Relevance,
                None,
                file(toml::Value::String("off".into()))
            )
            .unwrap(),
            JudgeMode::Off
        );
        for site in [JudgeSite::Grounding, JudgeSite::Dedupe] {
            assert_eq!(mode_ts(site, None, no_file).unwrap(), JudgeMode::Off);
        }
        // The adjudicator follows the same consent rule as relevance.
        let adj = JudgeSite::Adjudicate;
        assert_eq!(mode_ts(adj, None, no_file).unwrap(), JudgeMode::On);
        assert_eq!(
            mode_from(adj, None, no_file, || false).unwrap(),
            JudgeMode::Off
        );
    }

    #[test]
    fn an_unknown_value_is_a_config_error_naming_the_setting() {
        let err = mode_ts(JudgeSite::Dedupe, Some("true"), no_file).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("ENGRAM_JUDGE_DEDUPE"), "{err}");

        let err = mode_ts(
            JudgeSite::Adjudicate,
            None,
            file(toml::Value::String("maybe".into())),
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
        assert!(err.to_string().contains("judge.adjudicate"), "{err}");

        // A TOML boolean is not a mode either.
        let err =
            mode_ts(JudgeSite::Grounding, None, file(toml::Value::Boolean(true))).unwrap_err();
        assert!(matches!(err, CliError::Config(_)));

        // An unreadable config file is not "off".
        let err = mode_ts(JudgeSite::Grounding, None, || {
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
    fn warnings_only_for_a_live_mode_with_no_route_or_the_stub() {
        let modes = [Some(JudgeMode::Shadow), Some(JudgeMode::Off), None];
        let w = warnings_for(&modes, Some("none"));
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("judge.grounding is shadow"), "{}", w[0]);
        let w = warnings_for(&modes, Some("stub"));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains(STUB_ENV), "{}", w[0]);
        assert!(warnings_for(&modes, Some("typesafe")).is_empty());
        assert!(warnings_for(&modes, Some("openrouter")).is_empty());
        assert!(warnings_for(&modes, None).is_empty());
        assert!(warnings_for(&[Some(JudgeMode::Off); 3], Some("none")).is_empty());
        assert!(warnings_for(&[Some(JudgeMode::Off); 3], Some("stub")).is_empty());
    }

    #[test]
    fn a_bad_mode_or_stub_gets_its_own_fix_not_a_key_probe() {
        // CliError::suggestion sees the message without "config error: ".
        let inner = |e: CliError| match e {
            CliError::Config(m) => m,
            other => panic!("{other:?}"),
        };
        let site = JudgeSite::Grounding;
        let env = mode_ts(site, Some("true"), no_file).unwrap_err();
        let env = inner(env);
        let s = config_error_suggestion(&env).unwrap();
        assert!(
            s.contains("ENGRAM_JUDGE_GROUNDING") && s.contains("off|shadow|on"),
            "{s}"
        );
        assert!(s.contains("`engram config set judge.grounding off`"), "{s}");
        let file = mode_ts(
            JudgeSite::Dedupe,
            None,
            file(toml::Value::String("yes".into())),
        )
        .unwrap_err();
        let file = inner(file);
        let s = config_error_suggestion(&file).unwrap();
        assert!(s.contains("off|shadow|on"), "{s}");
        assert!(s.contains("`engram config set judge.dedupe off`"), "{s}");
        let stub = route_from(Some("nope"), false, false, none, none)
            .err()
            .unwrap();
        let stub = inner(stub);
        assert!(config_error_suggestion(&stub).unwrap().contains(STUB_ENV));
        assert!(config_error_suggestion("config root is not a table").is_none());
    }

    #[test]
    fn an_unconsulted_site_in_shadow_or_on_always_warns_and_never_asks_for_a_key() {
        let modes = [None, Some(JudgeMode::Off), Some(JudgeMode::Shadow)];
        for route in [
            Some("typesafe"),
            Some("openrouter"),
            Some("none"),
            Some("stub"),
            None,
        ] {
            let w = warnings_for(&modes, route);
            assert_eq!(w.len(), 1, "{route:?}: {w:?}");
            assert_eq!(
                w[0],
                "judge.dedupe is shadow but no command consults it in this build; it has no effect"
            );
            assert!(w.iter().all(|w| !w.contains("KEY")), "{w:?}");
        }
        assert!(warnings_for(&[Some(JudgeMode::Off); 3], Some("typesafe")).is_empty());
        // `remember --autonomy full` consults the adjudicator: no "no effect".
        let adjudicate_on = [None, Some(JudgeMode::On), None];
        assert!(warnings_for(&adjudicate_on, Some("typesafe")).is_empty());
        assert!(JudgeSite::Adjudicate.consulted());
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
