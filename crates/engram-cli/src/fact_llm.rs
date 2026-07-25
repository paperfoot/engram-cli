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
//!    silently calling OpenRouter.
//! 3. `OPENROUTER_API_KEY` (env or `keys.openrouter` in config.toml).
//! 4. Nothing — callers must then skip extraction loudly.

use crate::error::CliError;
use engram_llm::openrouter::OpenRouterClient;
use engram_llm::stub::ScriptedChatLlm;
use engram_llm::ChatLlm;

/// Env var holding a scripted offline extraction response (test seam).
pub const STUB_ENV: &str = "ENGRAM_FACT_LLM_STUB";

/// The chat client the fact layer will use.
pub enum FactLlm {
    /// Real OpenRouter call — costs money, records usage.
    Live(OpenRouterClient),
    /// Deterministic offline script — free, records no usage.
    Stub(ScriptedChatLlm),
}

impl FactLlm {
    pub fn as_chat(&self) -> &dyn ChatLlm {
        match self {
            FactLlm::Live(c) => c,
            FactLlm::Stub(c) => c,
        }
    }

    /// True when calls hit a paid provider (drives usage recording).
    pub fn is_live(&self) -> bool {
        matches!(self, FactLlm::Live(_))
    }

    pub fn model(&self) -> String {
        self.as_chat().model_id().to_string()
    }
}

/// OpenRouter model used for fact extraction, overridable per call site.
pub fn extraction_model() -> String {
    std::env::var("ENGRAM_FACT_MODEL").unwrap_or_else(|_| {
        crate::commands::config::resolve_setting(
            "ENGRAM_COMPILER_EXTRACTION_MODEL",
            "compiler.extraction_model",
            engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL,
        )
    })
}

/// Resolve the fact-layer LLM. `Ok(None)` means "no LLM available" — the
/// caller decides whether that is a loud skip (`remember`) or a refusal
/// (`facts backfill`). Errors only on a malformed stub script.
pub fn resolve() -> Result<Option<FactLlm>, CliError> {
    resolve_from(
        std::env::var(STUB_ENV).ok().as_deref(),
        std::env::var("ENGRAM_BENCH_FORCE_STUB").is_ok(),
        || crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter"),
    )
}

/// Pure core of [`resolve`] — the precedence rules without the environment,
/// so they can be unit-tested without mutating process-global state.
fn resolve_from(
    script: Option<&str>,
    force_stub: bool,
    key: impl FnOnce() -> Option<String>,
) -> Result<Option<FactLlm>, CliError> {
    if let Some(script) = script.filter(|s| !s.trim().is_empty()) {
        let stub = ScriptedChatLlm::parse(script)
            .map_err(|e| CliError::Config(format!("{STUB_ENV} is not a valid script: {e}")))?;
        return Ok(Some(FactLlm::Stub(stub)));
    }
    if force_stub {
        return Ok(None);
    }
    Ok(key().map(|k| FactLlm::Live(OpenRouterClient::new(k).with_model(extraction_model()))))
}

/// One sentence an agent can act on when there is no fact-layer LLM.
pub fn no_llm_hint() -> String {
    "no OpenRouter key configured — set OPENROUTER_API_KEY or run \
     `engram config set keys.openrouter <KEY>`, then run \
     `engram facts backfill --kb <kb>` to extract facts from what is already stored"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon"}]"#;

    #[test]
    fn script_beats_every_other_source() {
        let llm = resolve_from(Some(SCRIPT), true, || Some("key".into()))
            .unwrap()
            .expect("script resolves");
        assert!(!llm.is_live());
    }

    #[test]
    fn force_stub_never_goes_live() {
        assert!(resolve_from(None, true, || Some("key".into()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn blank_script_falls_through_to_the_key() {
        let llm = resolve_from(Some("   "), false, || Some("key".into()))
            .unwrap()
            .expect("key resolves");
        assert!(llm.is_live());
    }

    #[test]
    fn no_key_and_no_script_is_no_llm() {
        assert!(resolve_from(None, false, || None).unwrap().is_none());
    }
}
