//! `engram config show|set|check` — v0 minimal stub.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub fn show(ctx: &AppContext) -> Result<(), CliError> {
    let cfg_path = engram_storage::paths::config_path();
    let exists = cfg_path.exists();
    print_success(
        ctx.format,
        json!({
            "config_path": cfg_path.to_string_lossy(),
            "exists": exists,
            "gemini_api_key": std::env::var("GEMINI_API_KEY").map(|_| "set").unwrap_or("unset"),
            "cohere_api_key": std::env::var("COHERE_API_KEY").map(|_| "set").unwrap_or("unset"),
        }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn set(_ctx: &AppContext, _key: String, _value: String) -> Result<(), CliError> {
    Err(CliError::Config("config set will be wired in Phase 2 (use env vars for now)".into()))
}

pub async fn check(ctx: &AppContext) -> Result<(), CliError> {
    let gemini = std::env::var("GEMINI_API_KEY").is_ok();
    let cohere = std::env::var("COHERE_API_KEY").is_ok();
    print_success(
        ctx.format,
        json!({
            "gemini": if gemini { "configured" } else { "missing" },
            "cohere": if cohere { "configured" } else { "missing" },
            "ok": gemini, // gemini is required, cohere optional
        }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
