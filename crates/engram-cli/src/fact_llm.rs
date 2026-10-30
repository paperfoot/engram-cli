//! Resolution of the LLM that powers the fact layer — plus the offline seam.
//!
//! Fact extraction is now on by default, so every `remember` has to answer one
//! question before it writes: is there an LLM to extract with? This module is
//! the single place that answers it, so `remember`, `memory add`, and
//! `facts backfill` cannot drift apart on key handling, model selection, or
//! offline behavior.
//!
//! Precedence:
//! 1. `ENGRAM_FACT_LLM_STUB` — a scripted, deterministic, offline LLM (see
//!    [`engram_llm::stub::ScriptedChatLlm`]). Integration tests and dry runs
//!    use it to exercise the whole extraction pipeline without a network call.
//! 2. `ENGRAM_BENCH_FORCE_STUB` — the workspace-wide "touch no paid API" flag.
//!    With it set and no script, the fact layer reports "no LLM" rather than
//!    silently calling a provider.
//! 3. A model override (`ENGRAM_FACT_MODEL`, else `compiler.extraction_model`)
//!    picks its provider by form: `vendor/model` is an OpenRouter slug, a bare
//!    id such as `claude-haiku-5-5` is an Anthropic model. Only that
//!    provider's key is used.
//! 4. No override: `ANTHROPIC_API_KEY` (or `keys.anthropic`) extracts with
//!    Claude Haiku 5.5 direct; else `OPENROUTER_API_KEY` (or
//!    `keys.openrouter`) with the OpenRouter default.
//! 5. Nothing — callers must then skip extraction loudly.
//!
//! A scripted triple may carry the two interval fields the real extractor now
//! emits, so the seam covers a cessation without a network call:
//!
//! ```text
//! {"subject":"Ana","predicate":"takes_medication","object":"metformin",
//!  "valid_from":"2024-01-01","valid_until":"2026-06-01","state":"ended"}
//! ```
//!
//! `state: "ended"` (or a cessation predicate such as `stopped_taking`) CLOSES
//! the open interval instead of opening a second one — the difference between
//! a stopped medication and a second medication, which is the whole point.

use crate::error::CliError;
use engram_llm::anthropic::{self, AnthropicClient};
use engram_llm::openrouter::{self, OpenRouterClient};
use engram_llm::stub::ScriptedChatLlm;
use engram_llm::ChatLlm;

/// Env var holding a scripted offline extraction response (test seam).
pub const STUB_ENV: &str = "ENGRAM_FACT_LLM_STUB";

/// The chat client the fact layer will use.
pub enum FactLlm {
    /// Real Anthropic call — costs money, records usage.
    Anthropic(AnthropicClient),
    /// Real OpenRouter call — costs money, records usage.
    OpenRouter(OpenRouterClient),
    /// Deterministic offline script — free, records no usage.
    Stub(ScriptedChatLlm),
}

impl FactLlm {
    pub fn as_chat(&self) -> &dyn ChatLlm {
        match self {
            FactLlm::Anthropic(c) => c,
            FactLlm::OpenRouter(c) => c,
            FactLlm::Stub(c) => c,
        }
    }

    /// True when calls hit a paid provider (drives usage recording).
    pub fn is_live(&self) -> bool {
        !matches!(self, FactLlm::Stub(_))
    }

    pub fn model(&self) -> String {
        self.as_chat().model_id().to_string()
    }

    /// The provider a usage event is recorded under.
    pub fn provider(&self) -> &'static str {
        self.as_chat().name()
    }
}

/// A provider fact extraction can call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenRouter,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// The env var and config key that hold this provider's key.
    pub fn key_source(self) -> (&'static str, &'static str) {
        match self {
            Provider::Anthropic => ("ANTHROPIC_API_KEY", "keys.anthropic"),
            Provider::OpenRouter => ("OPENROUTER_API_KEY", "keys.openrouter"),
        }
    }

    fn key(self) -> Option<String> {
        let (env, path) = self.key_source();
        crate::commands::config::resolve_secret(env, path)
    }
}

/// The configured extraction model, if one overrides the default.
fn model_override() -> Option<String> {
    std::env::var("ENGRAM_FACT_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            std::env::var("ENGRAM_COMPILER_EXTRACTION_MODEL")
                .ok()
                .filter(|m| !m.trim().is_empty())
        })
        .or_else(|| crate::commands::config::resolve_config_string("compiler.extraction_model"))
}

/// Where extraction goes and with which model: an override picks its
/// provider by form; otherwise Anthropic when it is keyed, else OpenRouter.
fn route_for(model_override: Option<String>, anthropic_keyed: bool) -> (Provider, String) {
    match model_override {
        Some(m) if m.contains('/') => (Provider::OpenRouter, m),
        Some(m) => (Provider::Anthropic, m),
        None if anthropic_keyed => (
            Provider::Anthropic,
            anthropic::DEFAULT_EXTRACTION_MODEL.to_string(),
        ),
        None => (
            Provider::OpenRouter,
            openrouter::DEFAULT_EXTRACTION_MODEL.to_string(),
        ),
    }
}

/// The provider and model extraction would use, from settings alone — no
/// call is made, so `doctor` can report it offline.
pub fn route() -> (Provider, String) {
    route_for(model_override(), Provider::Anthropic.key().is_some())
}

/// True when the routed provider has a key, i.e. extraction may spend.
pub fn key_configured() -> bool {
    route().0.key().is_some()
}

/// Resolve the fact-layer LLM. `Ok(None)` means "no LLM available" — the
/// caller decides whether that is a loud skip (`remember`) or a refusal
/// (`facts backfill`). Errors only on a malformed stub script.
pub fn resolve() -> Result<Option<FactLlm>, CliError> {
    resolve_from(
        std::env::var(STUB_ENV).ok().as_deref(),
        crate::commands::config::stub_forced(),
        model_override(),
        Provider::key,
    )
}

/// Pure core of [`resolve`] — the precedence rules without the environment,
/// so they can be unit-tested without mutating process-global state.
fn resolve_from(
    script: Option<&str>,
    force_stub: bool,
    model_override: Option<String>,
    key: impl Fn(Provider) -> Option<String>,
) -> Result<Option<FactLlm>, CliError> {
    if let Some(script) = script.filter(|s| !s.trim().is_empty()) {
        let stub = ScriptedChatLlm::parse(script)
            .map_err(|e| CliError::Config(format!("{STUB_ENV} is not a valid script: {e}")))?;
        return Ok(Some(FactLlm::Stub(stub)));
    }
    if force_stub {
        return Ok(None);
    }
    let anthropic_key = key(Provider::Anthropic);
    let (provider, model) = route_for(model_override, anthropic_key.is_some());
    Ok(match provider {
        Provider::Anthropic => {
            anthropic_key.map(|k| FactLlm::Anthropic(AnthropicClient::new(k).with_model(model)))
        }
        Provider::OpenRouter => key(Provider::OpenRouter)
            .map(|k| FactLlm::OpenRouter(OpenRouterClient::new(k).with_model(model))),
    })
}

/// One sentence an agent can act on when there is no fact-layer LLM.
pub fn no_llm_hint() -> String {
    let backfill =
        "then run `engram facts backfill --kb <kb>` to extract facts from what is already stored";
    match model_override() {
        Some(model) => {
            let (env, path) = route_for(Some(model.clone()), false).0.key_source();
            format!(
                "no key for the extraction model `{model}` — set {env} or run \
                 `engram config set {path} <KEY>`, {backfill}"
            )
        }
        None => format!(
            "no extraction key configured — set ANTHROPIC_API_KEY or run \
             `engram config set keys.anthropic <KEY>` (Claude Haiku 5.5; an OpenRouter \
             key in OPENROUTER_API_KEY or keys.openrouter also works), {backfill}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon"}]"#;

    fn both_keys(_: Provider) -> Option<String> {
        Some("key".into())
    }

    fn only(p: Provider) -> impl Fn(Provider) -> Option<String> {
        move |q| (q == p).then(|| "key".to_string())
    }

    fn live(
        override_model: Option<&str>,
        key: impl Fn(Provider) -> Option<String>,
    ) -> Option<FactLlm> {
        resolve_from(None, false, override_model.map(str::to_string), key).unwrap()
    }

    #[test]
    fn script_beats_every_other_source() {
        let llm = resolve_from(Some(SCRIPT), true, None, both_keys)
            .unwrap()
            .expect("script resolves");
        assert!(!llm.is_live());
    }

    #[test]
    fn force_stub_never_goes_live() {
        assert!(resolve_from(None, true, None, both_keys).unwrap().is_none());
    }

    #[test]
    fn blank_script_falls_through_to_the_key() {
        let llm = resolve_from(Some("   "), false, None, both_keys)
            .unwrap()
            .expect("key resolves");
        assert!(llm.is_live());
    }

    #[test]
    fn no_key_and_no_script_is_no_llm() {
        assert!(live(None, |_| None).is_none());
    }

    #[test]
    fn an_anthropic_key_extracts_with_haiku_ahead_of_openrouter() {
        let llm = live(None, both_keys).unwrap();
        assert_eq!(llm.provider(), "anthropic");
        assert_eq!(llm.model(), anthropic::DEFAULT_EXTRACTION_MODEL);
        let llm = live(None, only(Provider::OpenRouter)).unwrap();
        assert_eq!(llm.provider(), "openrouter");
        assert_eq!(llm.model(), openrouter::DEFAULT_EXTRACTION_MODEL);
    }

    #[test]
    fn an_override_picks_its_provider_by_form_and_never_borrows_the_other_key() {
        let slug = live(Some("openai/gpt-6.1-terra"), both_keys).unwrap();
        assert_eq!(
            (slug.provider(), slug.model().as_str()),
            ("openrouter", "openai/gpt-6.1-terra")
        );
        let bare = live(Some("claude-sonnet-5-5"), both_keys).unwrap();
        assert_eq!(
            (bare.provider(), bare.model().as_str()),
            ("anthropic", "claude-sonnet-5-5")
        );
        // A slug with only an Anthropic key is no LLM, not a silent switch
        // to a model nobody configured.
        assert!(live(Some("openai/gpt-6.1-terra"), only(Provider::Anthropic)).is_none());
        assert!(live(Some("claude-sonnet-5-5"), only(Provider::OpenRouter)).is_none());
    }
}
