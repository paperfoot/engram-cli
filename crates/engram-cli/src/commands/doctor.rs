//! `engram doctor` — configuration and store diagnostics.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_embed::gemini::{DEFAULT_DIMS, DEFAULT_MODEL, PROMPT_FORMAT};
use serde_json::json;
use std::net::TcpListener;

pub fn run(ctx: &AppContext, compiler: bool, integrity: bool) -> Result<(), CliError> {
    let gemini = crate::commands::config::resolve_secret("GEMINI_API_KEY", "keys.gemini").is_some();
    let cohere = crate::commands::config::resolve_secret("COHERE_API_KEY", "keys.cohere").is_some();
    let openrouter =
        crate::commands::config::resolve_secret("OPENROUTER_API_KEY", "keys.openrouter").is_some();
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

    let extraction_model = crate::commands::config::resolve_setting(
        "ENGRAM_COMPILER_EXTRACTION_MODEL",
        "compiler.extraction_model",
        engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL,
    );
    let synthesis_model = crate::commands::config::resolve_setting(
        "ENGRAM_COMPILER_SYNTHESIS_MODEL",
        "compiler.synthesis_model",
        engram_llm::openrouter::DEFAULT_SYNTHESIS_MODEL,
    );

    let ok = gemini
        && cohere
        && (!compiler || openrouter)
        && schema_version >= 4
        && embedding_consistent
        && daemon_port_available
        && integrity_report.as_ref().map(|r| r.ok).unwrap_or(true);

    print_success(
        ctx.format,
        json!({
            "ok": ok,
            "keys": {
                "gemini": if gemini { "present" } else { "missing" },
                "cohere": if cohere { "present" } else { "missing" },
                "openrouter": if openrouter { "present" } else { "missing" },
            },
            "db": {
                "path": ctx.store.path().to_string_lossy(),
                "schema_version": schema_version,
            },
            "compiler": {
                "llm_requested": compiler,
                "openrouter_required": compiler,
                "extraction_model": extraction_model,
                "synthesis_model": synthesis_model,
            },
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
