//! Provider status: the one model `config check` (live probes) and `doctor`
//! (presence only — doctor stays offline) report for every provider engram
//! calls. Per provider: what engram uses it for, where its key comes from,
//! whether it answers, how much credit is left where the provider says, what
//! stops working without it, and one next step an agent can run or relay.
//!
//! Two incidents shaped it. A primary Gemini key answered 403 "project has
//! been denied access" while engram quietly served every embed from a
//! fallback, and nothing said so. An OpenRouter ACCOUNT held $0.14 while the
//! key's own limit showed $15 left, so fact extraction was about to fail with
//! no warning: the effective balance is the smaller of the two.
//!
//! Probe hygiene: keys ride in headers only (reqwest puts the URL in its
//! error text), transport errors are never formatted, and response bodies
//! are read only to classify or to parse a balance — never echoed.

use super::{env_secret, gemini_fallback_keys, mask_key, resolve_config_string};
use serde_json::{json, Value};
use std::time::Duration;

/// Below this effective OpenRouter balance (USD) the status is a warning:
/// one busy `remember` session can spend it.
pub const LOW_CREDIT_USD: f64 = 1.0;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const OPENROUTER_CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
const COHERE_RERANK_URL: &str = "https://api.cohere.com/v2/rerank";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gemini,
    Anthropic,
    OpenRouter,
    Cohere,
    TypeSafe,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::Cohere => "cohere",
            Self::TypeSafe => "typesafe",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Self::Gemini => "embeddings for every write and cloud recall",
            Self::Anthropic => {
                "fact extraction on remember with Claude Haiku 5.5, ahead of OpenRouter"
            }
            Self::OpenRouter => "fact extraction on remember, the judge's OpenRouter route",
            Self::Cohere => "rerank in cloud_quality recall",
            Self::TypeSafe => "the judge, direct route (with this key recall's relevance site defaults to shadow; the other sites default off)",
        }
    }

    fn degrades(self) -> &'static str {
        match self {
            Self::Gemini => "without a serving key, remember/ingest refuse to write (exit 2) and cloud recall is refused; only --offline / --profile offline stub vectors work",
            Self::Anthropic => "unset: fact extraction goes through OpenRouter, or with no OpenRouter key remember stores memories without facts (fact_extraction: skipped_no_openrouter_key); keyed but broken: remember stores memories without facts (fact_extraction: failed), because a keyed Anthropic route never falls back to OpenRouter",
            Self::OpenRouter => "with no Anthropic key, remember stores memories without facts (fact_extraction: skipped_no_openrouter_key, or failed when the key is broken); a shadow/on judge site has no route unless TypeSafe is keyed",
            Self::Cohere => "unset: recall runs unreranked (reranker passthrough, lower precision); a broken key makes recall fail unless ENGRAM_RERANK_PROVIDER=none",
            Self::TypeSafe => "unset: nothing, and a shadow/on judge site routes through OpenRouter; keyed but broken: every judged call on a shadow/on site fails, because a keyed TypeSafe route never falls back to OpenRouter",
        }
    }

    /// `not_exposed_by_provider` for every provider but OpenRouter: Gemini,
    /// Cohere and TypeSafe publish no balance API (TypeSafe checked
    /// 2026-09-27: nothing in docs.typesafe.ai/llms.txt, and every likely
    /// `/v1/{usage,balance,credits,billing,account}` path is a 404).
    fn exposes_balance(self) -> bool {
        self == Self::OpenRouter
    }

    fn console(self) -> &'static str {
        match self {
            Self::Gemini => "https://aistudio.google.com/apikey",
            Self::Anthropic => "https://platform.claude.com/settings/billing",
            Self::OpenRouter => "https://openrouter.ai/settings/credits",
            Self::Cohere => "https://dashboard.cohere.com/billing",
            Self::TypeSafe => "https://console.typesafe.ai/keys",
        }
    }
}

/// Closed vocabulary an agent can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    Ok,
    Invalid,
    Denied,
    QuotaExhausted,
    NoCredit,
    Unreachable,
    /// An HTTP answer none of the states above describes (5xx, a retired
    /// model, an unsupported region); `http_status` carries the code.
    ProbeError,
    NotProbed,
}

impl Live {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Invalid => "invalid",
            Self::Denied => "denied",
            Self::QuotaExhausted => "quota_exhausted",
            Self::NoCredit => "no_credit",
            Self::Unreachable => "unreachable",
            Self::ProbeError => "probe_error",
            Self::NotProbed => "not_probed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    /// Configured but not probed (doctor): presence is not health. Also an
    /// OpenRouter key whose account balance it may not read.
    Unchecked,
    Warning,
    Error,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unchecked => "unchecked",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Balance {
    NotExposed,
    /// OpenRouter: the smaller of the key's remaining limit and the
    /// account's remaining credit, with both parts shown. `source` is
    /// `account`, `key_limit`, or `key_limit_only` when the account balance
    /// is not visible to this key — then `remaining_usd` is only an upper
    /// bound.
    Usd {
        remaining_usd: f64,
        source: &'static str,
        key_remaining_usd: Option<f64>,
        account_remaining_usd: Option<f64>,
    },
    /// OpenRouter with an unlimited key and an account balance this key may
    /// not read (`/credits` wants a management key).
    Unknown,
}

impl Balance {
    fn to_json(&self) -> Value {
        match self {
            Self::NotExposed => json!("not_exposed_by_provider"),
            Self::Usd {
                remaining_usd,
                source,
                key_remaining_usd,
                account_remaining_usd,
            } => json!({
                "remaining_usd": round_cents(*remaining_usd),
                "source": source,
                "key_remaining_usd": key_remaining_usd.map(round_cents),
                "account_remaining_usd": account_remaining_usd.map(round_cents),
            }),
            Self::Unknown => json!({
                "remaining_usd": null,
                "source": "unknown",
                "note": "unlimited key and the account balance needs a management key; see https://openrouter.ai/settings/credits",
            }),
        }
    }
}

fn round_cents(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[derive(Debug, Clone)]
pub struct ProviderStatus {
    /// `gemini`, `gemini_fallback_1`, …, `anthropic`, `openrouter`, `cohere`,
    /// `typesafe`.
    pub name: String,
    pub provider: Provider,
    pub required: bool,
    pub env: &'static str,
    pub config_key: &'static str,
    /// `env` | `config_file` | `unset` (the `config show` vocabulary).
    pub source: &'static str,
    pub masked: Option<String>,
    pub live: Live,
    pub http_status: Option<u16>,
    pub balance: Option<Balance>,
    pub severity: Severity,
    pub fix: Option<String>,
}

impl ProviderStatus {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "provider": self.provider.name(),
            "role": self.provider.role(),
            "required": self.required,
            "configured": {
                "source": self.source,
                "env": self.env,
                "config_key": self.config_key,
                "value": self.masked,
            },
            "live": self.live.as_str(),
            "http_status": self.http_status,
            "balance": self.balance.as_ref().map(Balance::to_json),
            "severity": self.severity.as_str(),
            "degrades": self.provider.degrades(),
            "fix": self.fix,
        })
    }

    fn is_fallback(&self) -> bool {
        self.name.starts_with("gemini_fallback_")
    }
}

/// A provider key and where it came from.
struct Slot {
    name: String,
    provider: Provider,
    env: &'static str,
    config_key: &'static str,
    source: &'static str,
    key: Option<String>,
}

fn slot(name: &str, provider: Provider, env: &'static str, config_key: &'static str) -> Slot {
    let (source, key) = match env_secret(env) {
        Some(k) => ("env", Some(k)),
        None => match resolve_config_string(config_key) {
            Some(k) => ("config_file", Some(k)),
            None => ("unset", None),
        },
    };
    Slot {
        name: name.to_string(),
        provider,
        env,
        config_key,
        source,
        key,
    }
}

fn slots() -> Vec<Slot> {
    let mut out = vec![slot(
        "gemini",
        Provider::Gemini,
        "GEMINI_API_KEY",
        "keys.gemini",
    )];
    let fallback_source = if env_secret("GEMINI_API_KEY_FALLBACKS").is_some() {
        "env"
    } else {
        "config_file"
    };
    for (i, key) in gemini_fallback_keys().into_iter().enumerate() {
        out.push(Slot {
            name: format!("gemini_fallback_{}", i + 1),
            provider: Provider::Gemini,
            env: "GEMINI_API_KEY_FALLBACKS",
            config_key: "keys.gemini_fallbacks",
            source: fallback_source,
            key: Some(key),
        });
    }
    out.push(slot(
        "anthropic",
        Provider::Anthropic,
        "ANTHROPIC_API_KEY",
        "keys.anthropic",
    ));
    out.push(slot(
        "openrouter",
        Provider::OpenRouter,
        "OPENROUTER_API_KEY",
        "keys.openrouter",
    ));
    out.push(slot(
        "cohere",
        Provider::Cohere,
        "COHERE_API_KEY",
        "keys.cohere",
    ));
    out.push(slot(
        "typesafe",
        Provider::TypeSafe,
        "TYPESAFE_API_KEY",
        "keys.typesafe",
    ));
    out
}

/// What one probe learned.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub live: Live,
    pub http_status: Option<u16>,
    pub balance: Option<Balance>,
}

impl Probe {
    fn not_probed() -> Self {
        Self {
            live: Live::NotProbed,
            http_status: None,
            balance: None,
        }
    }
}

/// Every provider, presence only. `doctor`'s view: no network.
pub fn offline() -> Vec<ProviderStatus> {
    assemble(slots(), |_| Probe::not_probed())
}

/// Every provider, each configured key probed once, concurrently, on one
/// short-timeout client. Unset providers are never dialled. `config check`'s
/// view.
pub async fn probed() -> Vec<ProviderStatus> {
    let slots = slots();
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("engram/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok();
    // A keyed slot whose probe never reports (no client, a panicked task)
    // reads as unreachable, never as "not probed" — that would send the
    // agent straight back to `config check`.
    let mut probes: Vec<Probe> = slots
        .iter()
        .map(|s| match s.key {
            Some(_) => Probe {
                live: Live::Unreachable,
                ..Probe::not_probed()
            },
            None => Probe::not_probed(),
        })
        .collect();
    if let Some(client) = client {
        let mut join = tokio::task::JoinSet::new();
        for (i, s) in slots.iter().enumerate() {
            if let Some(key) = s.key.clone() {
                let client = client.clone();
                let provider = s.provider;
                join.spawn(async move { (i, probe(&client, provider, &key).await) });
            }
        }
        while let Some(res) = join.join_next().await {
            if let Ok((i, p)) = res {
                probes[i] = p;
            }
        }
    }
    let mut it = probes.into_iter();
    assemble(slots, |_| it.next().unwrap_or_else(Probe::not_probed))
}

fn assemble(slots: Vec<Slot>, mut probe_of: impl FnMut(&Slot) -> Probe) -> Vec<ProviderStatus> {
    let mut out: Vec<ProviderStatus> = slots
        .into_iter()
        .map(|s| {
            let p = probe_of(&s);
            let balance = match (&s.key, s.provider.exposes_balance()) {
                (Some(_), false) => Some(Balance::NotExposed),
                (Some(_), true) => p.balance,
                (None, _) => None,
            };
            ProviderStatus {
                required: s.name == "gemini",
                masked: s.key.as_deref().map(mask_key),
                name: s.name,
                provider: s.provider,
                env: s.env,
                config_key: s.config_key,
                source: s.source,
                live: p.live,
                http_status: p.http_status,
                balance,
                severity: Severity::Ok,
                fix: None,
            }
        })
        .collect();
    advise(&mut out, judge_active());
    out
}

/// True when some judge site is in shadow or on (an invalid setting counts
/// as off here; `judge.errors` reports it).
fn judge_active() -> bool {
    use crate::judge::JudgeMode::{On, Shadow};
    let j = crate::judge::status();
    [j.grounding, j.adjudicate, j.dedupe]
        .iter()
        .any(|m| matches!(m, Some(Shadow | On)))
}

/// Fill `severity` and `fix` for every status. The Gemini chain is judged as
/// a chain: a dead primary with a serving fallback is a warning (engram
/// still embeds), a chain with no serving key is an error. `judge_active`:
/// some judge site is shadow or on, so a broken TypeSafe key is an error.
pub fn advise(statuses: &mut [ProviderStatus], judge_active: bool) {
    let serving_fallback = statuses
        .iter()
        .find(|s| s.is_fallback() && s.live == Live::Ok)
        .map(|s| s.name.trim_start_matches("gemini_fallback_").to_string());
    let openrouter_keyed = statuses
        .iter()
        .any(|s| s.provider == Provider::OpenRouter && s.source != "unset");
    for s in statuses.iter_mut() {
        let (severity, fix) = judge_one(
            s,
            serving_fallback.as_deref(),
            judge_active,
            openrouter_keyed,
        );
        s.severity = severity;
        s.fix = fix;
    }
}

fn judge_one(
    s: &ProviderStatus,
    serving_fallback: Option<&str>,
    judge_active: bool,
    openrouter_keyed: bool,
) -> (Severity, Option<String>) {
    let p = s.provider;
    if s.source == "unset" {
        return match p {
            Provider::Gemini => (
                Severity::Error,
                Some(format!(
                    "get a key at {} and run `engram config set keys.gemini <key>` (or export GEMINI_API_KEY)",
                    p.console()
                )),
            ),
            // Extraction needs one of the two keys; the warning sits on the
            // preferred one so a store with neither shows it once.
            Provider::Anthropic if openrouter_keyed => (
                Severity::Ok,
                Some(format!(
                    "optional: get a key at {} and run `engram config set keys.anthropic <key>` to extract facts with Claude Haiku 5.5 instead of OpenRouter",
                    key_page(p)
                )),
            ),
            Provider::Anthropic => (
                Severity::Warning,
                Some(format!(
                    "no fact-extraction key: get one at {} and run `engram config set keys.anthropic <key>` (or use an OpenRouter key: `engram config set keys.openrouter <key>`); then `engram facts backfill --kb <kb>` extracts facts for memories stored without them",
                    key_page(p)
                )),
            ),
            Provider::OpenRouter => (
                Severity::Ok,
                Some("optional with an Anthropic key: get a key at https://openrouter.ai/settings/keys and run `engram config set keys.openrouter <key>` for OpenRouter extraction models and the judge's OpenRouter route".into()),
            ),
            Provider::Cohere => (
                Severity::Warning,
                Some("get a key at https://dashboard.cohere.com/api-keys and run `engram config set keys.cohere <key>` for reranked recall".into()),
            ),
            Provider::TypeSafe => (
                Severity::Ok,
                Some(format!(
                    "optional: get a key at {} and run `engram config set keys.typesafe <key>` to judge direct instead of through OpenRouter",
                    p.console()
                )),
            ),
        };
    }
    if s.live == Live::NotProbed {
        let cost = if p == Provider::Cohere {
            "one Cohere search unit"
        } else {
            "a fraction of a cent"
        };
        return (
            Severity::Unchecked,
            Some(format!(
                "presence is not health: run `engram config check --json` for a live probe ({cost})"
            )),
        );
    }
    if s.live == Live::Ok {
        return low_credit(s);
    }
    let cause = failure_fix(s);
    if p == Provider::TypeSafe {
        // A resolvable TypeSafe key always takes the direct route and never
        // retries through OpenRouter, so a broken key fails every judged
        // call — an error once a site is shadow/on, a warning while all are
        // off (the grounding, adjudicate and dedupe sites default off; the
        // relevance site's shadow default only annotates, so its failures
        // degrade nothing).
        let severity = if judge_active {
            Severity::Error
        } else {
            Severity::Warning
        };
        return (
            severity,
            Some(format!(
                "{cause}; or clear it (`engram config set keys.typesafe \"\"` and unset TYPESAFE_API_KEY) so judged calls route through OpenRouter"
            )),
        );
    }
    if p != Provider::Gemini {
        return (Severity::Error, Some(cause));
    }
    if s.is_fallback() {
        return (
            Severity::Warning,
            Some(format!(
                "this fallback is {}: replace or drop it with `engram config set keys.gemini_fallbacks <k1,k2>`",
                s.live.as_str()
            )),
        );
    }
    match serving_fallback {
        Some(n) => (
            Severity::Warning,
            Some(format!(
                "rotate keys.gemini (`engram config set keys.gemini <new>`) — running on fallback {n}; cause: {cause}"
            )),
        ),
        None => (Severity::Error, Some(cause)),
    }
}

/// A live-ok OpenRouter key with little or no effective credit. Zero is
/// folded into `no_credit` by [`openrouter_live`]; here only "low" remains.
/// An account balance this key may not read is `unchecked`, never `ok`: a
/// $15 key limit on a $0.14 account is exactly the silent failure this
/// model exists to catch.
fn low_credit(s: &ProviderStatus) -> (Severity, Option<String>) {
    let account_hidden = "OpenRouter's account balance is not visible to this key (GET /api/v1/credits needs a management key)";
    match &s.balance {
        Some(Balance::Usd {
            remaining_usd,
            source,
            ..
        }) if *remaining_usd < LOW_CREDIT_USD => (
            Severity::Warning,
            Some(format!(
                "OpenRouter credit is low (${:.2} left, {source}): {}",
                remaining_usd,
                top_up_openrouter(source)
            )),
        ),
        Some(Balance::Usd {
            remaining_usd,
            source: "key_limit_only",
            ..
        }) => (
            Severity::Unchecked,
            Some(format!(
                "{account_hidden}, so ${remaining_usd:.2} is only the key's own limit: check the account at https://openrouter.ai/settings/credits and top up if it is near $0 — when it runs out remember stores memories without facts (fact_extraction: failed); run `engram facts backfill --kb <kb>` afterwards"
            )),
        ),
        Some(Balance::Unknown) => (
            Severity::Unchecked,
            Some(format!(
                "{account_hidden} and the key has no limit: check the account at https://openrouter.ai/settings/credits and top up if it is near $0 — when it runs out remember stores memories without facts (fact_extraction: failed)"
            )),
        ),
        _ => (Severity::Ok, None),
    }
}

fn top_up_openrouter(source: &str) -> String {
    let action = if source.starts_with("key_limit") {
        "raise this key's credit limit at https://openrouter.ai/settings/keys"
    } else {
        "top up at https://openrouter.ai/settings/credits"
    };
    format!(
        "{action}; until then remember stores memories without facts (fact_extraction: failed) — run `engram facts backfill --kb <kb>` afterwards"
    )
}

fn failure_fix(s: &ProviderStatus) -> String {
    let p = s.provider;
    let set = format!("`engram config set {} <key>`", s.config_key);
    match s.live {
        Live::Invalid => format!("the key is invalid or revoked: create one at {} and run {set}", key_page(p)),
        Live::Denied => match p {
            Provider::Gemini => format!("Google denies this key's project (403): check the project (API enabled, billing, not suspended) at {} or rotate the key with {set}", p.console()),
            _ => format!("the provider refuses this key (403): check its permissions at {} or rotate it with {set}", key_page(p)),
        },
        Live::QuotaExhausted => match p {
            Provider::Gemini => format!("quota or prepaid credit exhausted (429): add billing or credit for the project at {}, wait for the quota reset, or add a working fallback with `engram config set keys.gemini_fallbacks <key>`", p.console()),
            Provider::Cohere => "rate or trial limit reached (429): move to a production key at https://dashboard.cohere.com/api-keys, or recall without rerank with ENGRAM_RERANK_PROVIDER=none".into(),
            _ => format!("rate limited (429): wait and re-run `engram config check --json`; if it persists check the account at {}", p.console()),
        },
        Live::NoCredit => match p {
            Provider::OpenRouter => {
                let source = match &s.balance {
                    Some(Balance::Usd { source, .. }) => *source,
                    _ => "account",
                };
                format!("OpenRouter has no credit left: {}", top_up_openrouter(source))
            }
            Provider::Cohere => format!("Cohere billing is blocked (402): fix it at {}; until then set ENGRAM_RERANK_PROVIDER=none so recall runs unreranked", p.console()),
            _ => format!("no credit left (402): add credit at {}", p.console()),
        },
        Live::Unreachable => format!("{} could not be reached (network, DNS, proxy, or no answer within {}s): check connectivity and re-run `engram config check --json`", p.name(), PROBE_TIMEOUT.as_secs()),
        Live::ProbeError => format!(
            "{} answered HTTP {}: re-run `engram config check --json`; if it persists check the provider's status page",
            p.name(),
            s.http_status.map_or("?".to_string(), |c| c.to_string())
        ),
        Live::Ok | Live::NotProbed => String::new(),
    }
}

fn key_page(p: Provider) -> &'static str {
    match p {
        Provider::Gemini => "https://aistudio.google.com/apikey",
        Provider::Anthropic => "https://platform.claude.com/settings/keys",
        Provider::OpenRouter => "https://openrouter.ai/settings/keys",
        Provider::Cohere => "https://dashboard.cohere.com/api-keys",
        Provider::TypeSafe => "https://console.typesafe.ai/keys",
    }
}

/// Every non-ok status's fix, errors first, each prefixed with its name.
pub fn next_steps(statuses: &[ProviderStatus]) -> Vec<String> {
    let mut ranked: Vec<&ProviderStatus> = statuses
        .iter()
        .filter(|s| s.severity != Severity::Ok && s.fix.is_some())
        .collect();
    ranked.sort_by_key(|s| std::cmp::Reverse(s.severity));
    ranked
        .into_iter()
        .map(|s| {
            format!(
                "{} ({}): {}",
                s.name,
                s.severity.as_str(),
                s.fix.as_deref().unwrap_or_default()
            )
        })
        .collect()
}

/// True when some key in the Gemini chain answered the live probe.
pub fn gemini_serves(statuses: &[ProviderStatus]) -> bool {
    statuses
        .iter()
        .any(|s| s.provider == Provider::Gemini && s.live == Live::Ok)
}

// --- mapping (pure, unit-tested) ------------------------------------------

/// An HTTP answer to a provider probe as a live state.
pub fn live_from_http(provider: Provider, status: u16, body: &str) -> Live {
    match status {
        200..=299 => Live::Ok,
        // Gemini answers a bad key with 400 + reason API_KEY_INVALID; any
        // other 400 is not a key problem.
        400 if provider == Provider::Gemini && body.contains("API_KEY_INVALID") => Live::Invalid,
        // Anthropic documents a 402 billing_error; an empty prepaid balance
        // has also come back as a 400 saying so.
        400 if provider == Provider::Anthropic && body.contains("credit balance") => Live::NoCredit,
        401 => Live::Invalid,
        402 => Live::NoCredit,
        403 => Live::Denied,
        429 => Live::QuotaExhausted,
        _ => Live::ProbeError,
    }
}

/// `limit_remaining` from `GET /api/v1/key`; null for an unlimited key.
pub fn openrouter_key_remaining(body: &Value) -> Option<f64> {
    body["data"]["limit_remaining"].as_f64()
}

/// `total_credits - total_usage` from `GET /api/v1/credits`.
pub fn openrouter_account_remaining(body: &Value) -> Option<f64> {
    let d = &body["data"];
    Some(d["total_credits"].as_f64()? - d["total_usage"].as_f64()?)
}

/// The effective OpenRouter balance: the SMALLER of what the key may still
/// spend and what the account still holds — either one running out stops
/// every call.
pub fn openrouter_balance(key: Option<f64>, account: Option<f64>) -> Balance {
    let (remaining_usd, source) = match (key, account) {
        (Some(k), Some(a)) if k < a => (k, "key_limit"),
        (_, Some(a)) => (a, "account"),
        // `/credits` refused (documented as management-key only): the key
        // limit is all this key can see, not the effective balance.
        (Some(k), None) => (k, "key_limit_only"),
        (None, None) => return Balance::Unknown,
    };
    Balance::Usd {
        remaining_usd,
        source,
        key_remaining_usd: key,
        account_remaining_usd: account,
    }
}

/// A live-ok key with no effective credit is `no_credit`: the next paid
/// call will be refused.
pub fn openrouter_live(key_live: Live, balance: &Balance) -> Live {
    match balance {
        Balance::Usd { remaining_usd, .. } if key_live == Live::Ok && *remaining_usd <= 0.0 => {
            Live::NoCredit
        }
        _ => key_live,
    }
}

// --- probes -----------------------------------------------------------------

async fn probe(client: &reqwest::Client, provider: Provider, key: &str) -> Probe {
    match provider {
        Provider::OpenRouter => probe_openrouter(client, key).await,
        _ => {
            let req = match provider {
                Provider::Gemini => gemini_probe(client, key),
                Provider::Anthropic => anthropic_probe(client, key),
                Provider::Cohere => cohere_probe(client, key),
                _ => typesafe_probe(client, key),
            };
            let (live, http_status, _) = send(req, provider).await;
            Probe {
                live,
                http_status,
                balance: None,
            }
        }
    }
}

/// One request, classified. The error is dropped unformatted; the body is
/// returned only for a 2xx (to parse) — never for echoing.
async fn send(
    req: reqwest::RequestBuilder,
    provider: Provider,
) -> (Live, Option<u16>, Option<Value>) {
    let Ok(resp) = req.send().await else {
        return (Live::Unreachable, None, None);
    };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let live = live_from_http(provider, status, &body);
    if live == Live::Ok {
        (live, None, serde_json::from_str(&body).ok())
    } else {
        (live, Some(status), None)
    }
}

async fn probe_openrouter(client: &reqwest::Client, key: &str) -> Probe {
    let (key_probe, credits_probe) = tokio::join!(
        send(
            openrouter_get(client, OPENROUTER_KEY_URL, key),
            Provider::OpenRouter
        ),
        // A 401/403 here means "this key may not read the account", not a
        // broken key: `/credits` is documented as management-key only.
        send(
            openrouter_get(client, OPENROUTER_CREDITS_URL, key),
            Provider::OpenRouter
        ),
    );
    let (key_live, http_status, key_body) = key_probe;
    if key_live != Live::Ok {
        return Probe {
            live: key_live,
            http_status,
            balance: None,
        };
    }
    let balance = openrouter_balance(
        key_body.as_ref().and_then(openrouter_key_remaining),
        credits_probe
            .2
            .as_ref()
            .and_then(openrouter_account_remaining),
    );
    Probe {
        live: openrouter_live(key_live, &balance),
        http_status: None,
        balance: Some(balance),
    }
}

/// The Gemini probe: one 5-character embed. The free models-list endpoint
/// answers 200 for a key whose project sits at zero prepaid credit, so only
/// a real embed tells the truth.
fn gemini_probe(client: &reqwest::Client, key: &str) -> reqwest::RequestBuilder {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:embedContent",
        engram_embed::gemini::DEFAULT_MODEL
    );
    client.post(url).header("x-goog-api-key", key).json(&json!({
        "content": {"parts": [{"text": "probe"}]},
        "outputDimensionality": 8,
    }))
}

/// A one-token message to the extraction model: about $0.000002, and unlike
/// the free models list it fails when the account has no credit.
fn anthropic_probe(client: &reqwest::Client, key: &str) -> reqwest::RequestBuilder {
    client
        .post(ANTHROPIC_MESSAGES_URL)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": engram_llm::anthropic::DEFAULT_EXTRACTION_MODEL,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "probe"}],
        }))
}

fn openrouter_get(client: &reqwest::Client, url: &str, key: &str) -> reqwest::RequestBuilder {
    client.get(url).bearer_auth(key)
}

/// One single-document rerank: costs one Cohere search unit, which is why
/// only `config check` sends it.
fn cohere_probe(client: &reqwest::Client, key: &str) -> reqwest::RequestBuilder {
    client
        .post(COHERE_RERANK_URL)
        .bearer_auth(key)
        .json(&json!({
            "model": engram_rerank::cohere::resolve_model(),
            "query": "probe",
            "documents": ["probe"],
            "top_n": 1,
        }))
}

/// One Noul: about 270 input tokens at $0.042 per million, the cheapest call
/// that exercises the key's credit. `GET /v1/models` authenticates but
/// cannot see credit.
fn typesafe_probe(client: &reqwest::Client, key: &str) -> reqwest::RequestBuilder {
    use engram_llm::judge::typesafe::{DIRECT_ENDPOINT, DIRECT_MODEL};
    client.post(DIRECT_ENDPOINT).bearer_auth(key).json(&json!({
        "model": DIRECT_MODEL,
        "state": "probe",
        "questions": {"probe": {"type": "noul", "instructions": "Is `state` non-empty?"}},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(name: &str, provider: Provider, live: Live) -> ProviderStatus {
        ProviderStatus {
            name: name.to_string(),
            provider,
            required: name == "gemini",
            env: "X",
            config_key: if name.starts_with("gemini_fallback") {
                "keys.gemini_fallbacks"
            } else {
                "keys.x"
            },
            source: "config_file",
            masked: Some("AIzaSy...abcd".into()),
            live,
            http_status: None,
            balance: if provider == Provider::OpenRouter {
                None
            } else {
                Some(Balance::NotExposed)
            },
            severity: Severity::Ok,
            fix: None,
        }
    }

    #[test]
    fn http_answers_map_to_the_closed_live_vocabulary() {
        use Provider::*;
        assert_eq!(live_from_http(Gemini, 200, ""), Live::Ok);
        assert_eq!(
            live_from_http(
                Gemini,
                400,
                r#"{"error":{"status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#
            ),
            Live::Invalid
        );
        // A 400 that is not about the key is not "invalid".
        assert_eq!(
            live_from_http(Gemini, 400, r#"{"error":{"status":"FAILED_PRECONDITION"}}"#),
            Live::ProbeError
        );
        assert_eq!(
            live_from_http(Gemini, 403, r#"{"error":{"status":"PERMISSION_DENIED"}}"#),
            Live::Denied
        );
        assert_eq!(
            live_from_http(Gemini, 429, r#"{"error":{"status":"RESOURCE_EXHAUSTED"}}"#),
            Live::QuotaExhausted
        );
        assert_eq!(
            live_from_http(
                Anthropic,
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#
            ),
            Live::NoCredit
        );
        for p in [Anthropic, OpenRouter, Cohere, TypeSafe] {
            assert_eq!(live_from_http(p, 401, ""), Live::Invalid);
            assert_eq!(live_from_http(p, 402, ""), Live::NoCredit);
            assert_eq!(live_from_http(p, 403, ""), Live::Denied);
            assert_eq!(live_from_http(p, 429, ""), Live::QuotaExhausted);
            assert_eq!(live_from_http(p, 503, ""), Live::ProbeError);
            // Only Gemini and Anthropic read the 400 body.
            assert_eq!(live_from_http(p, 400, "API_KEY_INVALID"), Live::ProbeError);
        }
    }

    #[test]
    fn openrouter_bodies_parse_and_the_effective_balance_is_the_smaller() {
        let key: Value = serde_json::from_str(
            r#"{"data":{"label":"x","limit":20,"limit_remaining":15.0,"usage":5}}"#,
        )
        .unwrap();
        let credits: Value =
            serde_json::from_str(r#"{"data":{"total_credits":100.5,"total_usage":100.36}}"#)
                .unwrap();
        let k = openrouter_key_remaining(&key);
        let a = openrouter_account_remaining(&credits);
        assert_eq!(k, Some(15.0));
        assert!((a.unwrap() - 0.14).abs() < 1e-9);
        // Today's incident: $15 on the key, $0.14 in the account.
        let Balance::Usd {
            remaining_usd,
            source,
            ..
        } = openrouter_balance(k, a)
        else {
            panic!("expected a USD balance")
        };
        assert!((remaining_usd - 0.14).abs() < 1e-9);
        assert_eq!(source, "account");
        // A tighter key limit wins.
        assert!(matches!(
            openrouter_balance(Some(0.5), Some(40.0)),
            Balance::Usd {
                source: "key_limit",
                ..
            }
        ));
        // Unlimited key (null) and an unreadable account balance.
        let unlimited: Value =
            serde_json::from_str(r#"{"data":{"limit":null,"limit_remaining":null}}"#).unwrap();
        assert_eq!(openrouter_key_remaining(&unlimited), None);
        assert_eq!(openrouter_balance(None, None), Balance::Unknown);
        assert!(matches!(
            openrouter_balance(None, Some(3.0)),
            Balance::Usd { source: "account", remaining_usd, .. } if remaining_usd == 3.0
        ));
        assert_eq!(openrouter_account_remaining(&json!({"data": {}})), None);
        // `/credits` refused: the key limit is shown, but labelled as all
        // this key can see.
        assert!(matches!(
            openrouter_balance(Some(15.0), None),
            Balance::Usd {
                source: "key_limit_only",
                account_remaining_usd: None,
                ..
            }
        ));
    }

    #[test]
    fn an_account_balance_the_key_cannot_read_is_unchecked_not_ok() {
        // The $0.14 incident seen through a key refused `/credits`: the $15
        // key limit must not read as a healthy balance.
        let mut limited = status("openrouter", Provider::OpenRouter, Live::Ok);
        limited.balance = Some(openrouter_balance(Some(15.0), None));
        let mut unlimited = status("openrouter", Provider::OpenRouter, Live::Ok);
        unlimited.balance = Some(openrouter_balance(None, None));
        let mut low_limit = status("openrouter", Provider::OpenRouter, Live::Ok);
        low_limit.balance = Some(openrouter_balance(Some(0.4), None));
        let mut all = vec![limited, unlimited, low_limit];
        advise(&mut all, false);
        for s in &all[..2] {
            assert_eq!(s.severity, Severity::Unchecked);
            let fix = s.fix.as_deref().unwrap();
            assert!(
                fix.contains("management key") && fix.contains("openrouter.ai/settings/credits"),
                "{fix}"
            );
        }
        assert_eq!(all[0].to_json()["balance"]["source"], "key_limit_only");
        // A low key limit is still a low-credit warning.
        assert_eq!(all[2].severity, Severity::Warning);
        assert_eq!(next_steps(&all).len(), 3);
    }

    #[test]
    fn a_store_without_an_extraction_key_is_warned_once_on_the_anthropic_row() {
        let unset = |name: &str, p: Provider| {
            let mut s = status(name, p, Live::NotProbed);
            s.source = "unset";
            s
        };
        let mut neither = vec![
            unset("anthropic", Provider::Anthropic),
            unset("openrouter", Provider::OpenRouter),
        ];
        advise(&mut neither, false);
        assert_eq!(neither[0].severity, Severity::Warning);
        assert!(neither[0]
            .fix
            .as_deref()
            .unwrap()
            .contains("keys.openrouter"));
        assert_eq!(neither[1].severity, Severity::Ok);
        assert_eq!(next_steps(&neither).len(), 1);

        // An OpenRouter key extracts, so a missing Anthropic key is optional.
        let mut openrouter_only = vec![
            unset("anthropic", Provider::Anthropic),
            status("openrouter", Provider::OpenRouter, Live::NotProbed),
        ];
        advise(&mut openrouter_only, false);
        assert_eq!(openrouter_only[0].severity, Severity::Ok);
    }

    #[test]
    fn a_broken_typesafe_key_is_an_error_once_a_judge_site_is_live() {
        // A keyed TypeSafe route never falls back to OpenRouter.
        for (active, want) in [(false, Severity::Warning), (true, Severity::Error)] {
            let mut one = vec![status("typesafe", Provider::TypeSafe, Live::Invalid)];
            advise(&mut one, active);
            assert_eq!(one[0].severity, want);
            let fix = one[0].fix.as_deref().unwrap();
            assert!(
                fix.contains("config set keys.typesafe") && fix.contains("through OpenRouter"),
                "{fix}"
            );
        }
    }

    #[test]
    fn no_effective_credit_is_no_credit_and_low_credit_is_a_warning() {
        let zero = openrouter_balance(Some(15.0), Some(0.0));
        assert_eq!(openrouter_live(Live::Ok, &zero), Live::NoCredit);
        let negative = openrouter_balance(None, Some(-0.2));
        assert_eq!(openrouter_live(Live::Ok, &negative), Live::NoCredit);
        // A broken key stays what the HTTP said.
        assert_eq!(openrouter_live(Live::Invalid, &zero), Live::Invalid);

        let mut low = status("openrouter", Provider::OpenRouter, Live::Ok);
        low.balance = Some(openrouter_balance(Some(15.0), Some(0.14)));
        let mut plenty = status("openrouter", Provider::OpenRouter, Live::Ok);
        plenty.balance = Some(openrouter_balance(Some(15.0), Some(40.0)));
        let mut dry = status("openrouter", Provider::OpenRouter, Live::NoCredit);
        dry.balance = Some(zero);
        let mut all = vec![low, plenty, dry];
        advise(&mut all, false);
        assert_eq!(all[0].severity, Severity::Warning);
        let fix = all[0].fix.as_deref().unwrap();
        assert!(
            fix.contains("$0.14") && fix.contains("openrouter.ai/settings/credits"),
            "{fix}"
        );
        assert_eq!(all[1].severity, Severity::Ok);
        assert_eq!(all[1].fix, None);
        assert_eq!(all[2].severity, Severity::Error);
        assert!(all[2].fix.as_deref().unwrap().contains("facts backfill"));
    }

    #[test]
    fn a_dead_primary_with_a_serving_fallback_is_a_warning_naming_the_fallback() {
        let mut chain = vec![
            status("gemini", Provider::Gemini, Live::Denied),
            status("gemini_fallback_1", Provider::Gemini, Live::QuotaExhausted),
            status("gemini_fallback_2", Provider::Gemini, Live::Ok),
        ];
        advise(&mut chain, false);
        assert_eq!(chain[0].severity, Severity::Warning);
        let fix = chain[0].fix.as_deref().unwrap();
        assert!(
            fix.starts_with("rotate keys.gemini (`engram config set keys.gemini <new>`) — running on fallback 2"),
            "{fix}"
        );
        assert_eq!(chain[1].severity, Severity::Warning);
        assert!(chain[1]
            .fix
            .as_deref()
            .unwrap()
            .contains("keys.gemini_fallbacks"));
        assert_eq!(chain[2].severity, Severity::Ok);
        assert!(gemini_serves(&chain));

        // No key serves: the primary is an error.
        let mut dead = vec![
            status("gemini", Provider::Gemini, Live::Denied),
            status("gemini_fallback_1", Provider::Gemini, Live::Unreachable),
        ];
        advise(&mut dead, false);
        assert_eq!(dead[0].severity, Severity::Error);
        assert!(!gemini_serves(&dead));
        assert!(next_steps(&dead)[0].starts_with("gemini (error): "));
    }

    #[test]
    fn every_failure_state_carries_a_next_step() {
        for p in [
            Provider::Gemini,
            Provider::Anthropic,
            Provider::OpenRouter,
            Provider::Cohere,
            Provider::TypeSafe,
        ] {
            for live in [
                Live::Invalid,
                Live::Denied,
                Live::QuotaExhausted,
                Live::NoCredit,
                Live::Unreachable,
                Live::ProbeError,
                Live::NotProbed,
            ] {
                let mut one = vec![status(p.name(), p, live)];
                advise(&mut one, false);
                assert_ne!(one[0].severity, Severity::Ok, "{p:?} {live:?}");
                let fix = one[0].fix.clone().unwrap_or_default();
                assert!(fix.len() > 20, "{p:?} {live:?}: {fix}");
            }
            let mut unset = vec![status(p.name(), p, Live::NotProbed)];
            unset[0].source = "unset";
            advise(&mut unset, false);
            assert!(unset[0].fix.is_some(), "{p:?} unset");
        }
    }

    #[test]
    fn every_probe_sends_its_key_in_a_header_not_the_url() {
        let key = "FAKEPROBEKEY0123456789abcdef";
        let c = reqwest::Client::new();
        for req in [
            gemini_probe(&c, key),
            anthropic_probe(&c, key),
            openrouter_get(&c, OPENROUTER_KEY_URL, key),
            openrouter_get(&c, OPENROUTER_CREDITS_URL, key),
            cohere_probe(&c, key),
            typesafe_probe(&c, key),
        ] {
            let req = req.build().unwrap();
            assert_eq!(req.url().query(), None);
            assert!(!req.url().as_str().contains(key));
            let in_header = req
                .headers()
                .values()
                .any(|v| v.to_str().unwrap_or_default().contains(key));
            assert!(in_header, "{}", req.url());
        }
    }

    #[test]
    fn a_status_serialises_the_documented_fields_and_never_the_key() {
        let mut s = status("gemini", Provider::Gemini, Live::Ok);
        s.masked = Some(mask_key("AIzaFAKEKEY0123456789abcdef"));
        let v = s.to_json();
        for field in [
            "name",
            "provider",
            "role",
            "required",
            "configured",
            "live",
            "http_status",
            "balance",
            "severity",
            "degrades",
            "fix",
        ] {
            assert!(v.get(field).is_some(), "missing {field}");
        }
        assert_eq!(v["balance"], "not_exposed_by_provider");
        assert!(!v.to_string().contains("FAKEKEY0123456789"));
        assert!(s.required);
    }
}
