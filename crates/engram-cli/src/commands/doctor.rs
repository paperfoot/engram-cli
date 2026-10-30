//! `engram doctor` — configuration and store diagnostics.

use crate::commands::config::providers::ProviderStatus;
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::gemini::{DEFAULT_DIMS, DEFAULT_MODEL, PROMPT_FORMAT};
use serde_json::json;
use std::net::TcpListener;

pub fn run(ctx: &AppContext, compiler: bool, integrity: bool) -> Result<(), CliError> {
    let gemini = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini").is_some();
    let cohere = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere").is_some();
    let anthropic =
        crate::commands::config::resolve_secret("ANTHROPIC_API_KEY", "keys.anthropic").is_some();
    let openrouter =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter").is_some();
    let typesafe =
        crate::commands::config::resolve_secret("TYPESAFE_API_KEY", "keys.typesafe").is_some();
    // Offline, like the rest of doctor: modes and route from settings only.
    // A shadow/on site with no route, or on the stub, is a warning inside
    // the block; an invalid setting fails `ok`, because every judged
    // command would exit 2.
    let judge = crate::judge::status();
    let providers = crate::commands::config::providers::offline();
    let schema_version = ctx.store.schema_version()?;
    let profiles = ctx.store.embedding_profiles(None)?;
    let embedding_consistent = profiles.iter().all(|(model, dims, prompt, _)| {
        model == DEFAULT_MODEL
            && *dims == Some(DEFAULT_DIMS as i64)
            && prompt.as_deref() == Some(PROMPT_FORMAT)
    }) || profiles.is_empty();
    let daemon_port_available = TcpListener::bind(("127.0.0.1", 8768)).is_ok();
    let integrity_report = if integrity {
        Some(ctx.store.integrity_report()?)
    } else {
        None
    };

    // The route `remember` would take, keyed or not: a missing key is
    // reported by `extraction_key`, not by a different model.
    let (extraction_provider, extraction_model) = crate::fact_llm::route();
    let extraction_keyed = crate::fact_llm::key_configured();
    let synthesis_model = crate::commands::config::resolve_setting(
        "ENGRAM_COMPILER_SYNTHESIS_MODEL",
        "compiler.synthesis_model",
        engram_llm::openrouter::DEFAULT_SYNTHESIS_MODEL,
    );

    let ok = gemini
        && cohere
        && (!compiler || extraction_keyed)
        && schema_version >= 4
        && embedding_consistent
        && daemon_port_available
        && integrity_report.as_ref().map(|r| r.ok).unwrap_or(true)
        && judge.errors.is_empty();

    print_success(
        ctx.format,
        json!({
            "ok": ok,
            "keys": {
                "gemini": if gemini { "present" } else { "missing" },
                "cohere": if cohere { "present" } else { "missing" },
                "anthropic": if anthropic { "present" } else { "missing" },
                "openrouter": if openrouter { "present" } else { "missing" },
                "typesafe": if typesafe { "present" } else { "missing" },
                // Presence is not health: a key can sit at zero prepaid
                // credits and still look "present". The live probes live in
                // `config check`, which doctor points at rather than dialling
                // the network itself — doctor must stay useful offline.
                "note": "presence only; run `engram config check --json` for a live probe of every key, fallbacks included (live: ok | invalid | denied | quota_exhausted | no_credit | unreachable | probe_error)",
            },
            // The `config check` provider model with every key `not_probed`:
            // role, key source (masked), what degrades without it, and a fix
            // for each provider that is unset or unchecked.
            "providers": providers.iter().map(ProviderStatus::to_json).collect::<Vec<_>>(),
            "next_steps": crate::commands::config::providers::next_steps(&providers),
            "db": {
                "path": ctx.store.path().to_string_lossy(),
                "schema_version": schema_version,
            },
            "compiler": {
                "llm_requested": compiler,
                "openrouter_required": compiler && extraction_provider == crate::fact_llm::Provider::OpenRouter,
                "extraction_provider": extraction_provider.name(),
                "extraction_model": extraction_model,
                "extraction_key": if extraction_keyed { "present" } else { "missing" },
                "synthesis_model": synthesis_model,
            },
            "judge": judge,
            "embedding": {
                "expected_model": DEFAULT_MODEL,
                "expected_dimensions": DEFAULT_DIMS,
                "expected_prompt_format": PROMPT_FORMAT,
                "consistent": embedding_consistent,
                "profiles": profiles.iter().map(|(model, dims, prompt, count)| json!({
                    "model": model,
                    "dimensions": dims,
                    "prompt_format": prompt,
                    "count": count
                })).collect::<Vec<_>>()
            },
            "daemon": {
                "host": "127.0.0.1",
                "port": 8768,
                "available": daemon_port_available
            },
            "integrity": integrity_report
        }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
