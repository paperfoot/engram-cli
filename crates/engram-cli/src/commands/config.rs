//! `engram config show|set|check` — manage configuration.
//!
//! Config lives at `~/.config/engram/config.toml` (per agent-cli-framework).
//! Secrets are resolved in order: explicit flag → env var → config file.
//! API keys are masked on display.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_storage::paths;
use serde_json::json;
use std::io::Write;

fn mask_key(key: &str) -> String {
    if key.len() < 12 {
        "***".to_string()
    } else {
        let head: String = key.chars().take(6).collect();
        let tail: String = key
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("{head}...{tail}")
    }
}

fn read_toml() -> Result<toml::Value, CliError> {
    let p = paths::config_path();
    if !p.exists() {
        return Ok(toml::Value::Table(toml::value::Table::new()));
    }
    let text = std::fs::read_to_string(&p)?;
    toml::from_str(&text)
        .map_err(|e| CliError::Config(format!("invalid TOML at {}: {e}", p.display())))
}

fn write_toml(value: &toml::Value) -> Result<(), CliError> {
    let p = paths::config_path();
    let text = toml::to_string_pretty(value)
        .map_err(|e| CliError::Config(format!("failed to serialize TOML: {e}")))?;
    std::fs::write(&p, text)?;
    // 0600 perms on Unix so secrets are user-readable only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn show(ctx: &AppContext) -> Result<(), CliError> {
    let cfg_path = paths::config_path();
    let file_cfg = read_toml().unwrap_or(toml::Value::Table(toml::value::Table::new()));

    let gemini_source = describe_key_source("GEMINI_API_KEY", &file_cfg, "keys.gemini");
    let cohere_source = describe_key_source("COHERE_API_KEY", &file_cfg, "keys.cohere");
    let openrouter_source = describe_key_source("OPENROUTER_API_KEY", &file_cfg, "keys.openrouter");

    print_success(
        ctx.format,
        json!({
            "config_path": cfg_path.to_string_lossy(),
            "config_file_exists": cfg_path.exists(),
            "data_path": paths::db_path().to_string_lossy(),
            "cache_path": paths::cache_dir().to_string_lossy(),
            "keys": {
                "gemini": gemini_source,
                "cohere": cohere_source,
                "openrouter": openrouter_source,
            },
            "retrieval": {
                "profile": "cloud_quality",
                "rerank_top_n": 50,
            },
            "embedding": {
                "provider": "gemini",
                "model": engram_embed::gemini::DEFAULT_MODEL,
                "dimensions": engram_embed::gemini::DEFAULT_DIMS,
                "prompt_format": engram_embed::gemini::PROMPT_FORMAT,
            },
            "compiler": {
                "extraction_model": resolve_setting(
                    "ENGRAM_COMPILER_EXTRACTION_MODEL",
                    "compiler.extraction_model",
                    engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL,
                ),
                "synthesis_model": resolve_setting(
                    "ENGRAM_COMPILER_SYNTHESIS_MODEL",
                    "compiler.synthesis_model",
                    engram_llm::openrouter::DEFAULT_SYNTHESIS_MODEL,
                ),
                "llm_default": resolve_setting("ENGRAM_COMPILER_LLM", "compiler.llm", "false"),
            },
            "ingest": {
                "require_scope": resolve_setting("ENGRAM_INGEST_REQUIRE_SCOPE", "ingest.require_scope", "true"),
                "max_files": std::env::var("ENGRAM_INGEST_MAX_FILES").ok()
                    .or_else(|| resolve_config_string("ingest.max_files")),
                "include": std::env::var("ENGRAM_INGEST_INCLUDE").ok()
                    .or_else(|| resolve_config_string("ingest.include")),
                "exclude": std::env::var("ENGRAM_INGEST_EXCLUDE").ok()
                    .or_else(|| resolve_config_string("ingest.exclude")),
            },
            "update": {
                "mode": resolve_setting("ENGRAM_UPDATE_MODE", "update.mode", "auto"),
                "channel": resolve_setting("ENGRAM_UPDATE_CHANNEL", "update.channel", "github"),
                "install_source": std::env::var("ENGRAM_UPDATE_INSTALL_SOURCE").ok()
                    .or_else(|| resolve_config_string("update.install_source")),
                "github_repo": resolve_setting("ENGRAM_UPDATE_GITHUB_REPO", "update.github_repo", "paperfoot/engram-cli"),
                "crate": resolve_setting("ENGRAM_UPDATE_CRATE", "update.crate", "paperfoot-engram"),
                "homebrew_formula": resolve_setting("ENGRAM_UPDATE_HOMEBREW_FORMULA", "update.homebrew_formula", "paperfoot/tap/engram"),
            }
        }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn describe_key_source(env: &str, file: &toml::Value, path: &str) -> serde_json::Value {
    if let Ok(v) = std::env::var(env) {
        return json!({ "source": "env", "env": env, "value": mask_key(&v) });
    }
    // Walk path dot-separated
    let mut cur = file;
    for part in path.split('.') {
        match cur.get(part) {
            Some(next) => cur = next,
            None => {
                return json!({ "source": "unset" });
            }
        }
    }
    if let Some(s) = cur.as_str() {
        json!({ "source": "config_file", "value": mask_key(s) })
    } else {
        json!({ "source": "unset" })
    }
}

pub fn set(ctx: &AppContext, key: String, value: String) -> Result<(), CliError> {
    let mut root = read_toml()?;
    let root_table = root
        .as_table_mut()
        .ok_or_else(|| CliError::Config("config root is not a table".into()))?;
    // Walk the dotted key, creating intermediate tables.
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() {
        return Err(CliError::BadInput("empty key".into()));
    }
    let (last, front) = parts.split_last().unwrap();
    let mut cur: &mut toml::value::Table = root_table;
    for p in front {
        let entry = cur
            .entry((*p).to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        cur = entry
            .as_table_mut()
            .ok_or_else(|| CliError::Config(format!("{p} exists but is not a table")))?;
    }
    cur.insert((*last).to_string(), toml::Value::String(value));

    write_toml(&root)?;
    print_success(
        ctx.format,
        json!({ "key": key, "updated": true, "path": paths::config_path().to_string_lossy() }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    // ensure the file is written before we drop ctx
    let _ = ctx;
    Ok(())
}

pub async fn check(ctx: &AppContext) -> Result<(), CliError> {
    let file_cfg = read_toml().unwrap_or(toml::Value::Table(toml::value::Table::new()));
    let gemini = has_key("GEMINI_API_KEY", &file_cfg, "keys.gemini");
    let cohere = has_key("COHERE_API_KEY", &file_cfg, "keys.cohere");
    let openrouter = has_key("OPENROUTER_API_KEY", &file_cfg, "keys.openrouter");

    print_success(
        ctx.format,
        json!({
            "gemini": if gemini { "configured" } else { "missing" },
            "cohere": if cohere { "configured (optional)" } else { "missing (optional)" },
            "openrouter": if openrouter { "configured (compiler)" } else { "missing (compiler optional)" },
            "ok": gemini,
            "summary": if gemini {
                if cohere { "hybrid_gemini + cohere" } else { "hybrid_gemini (no rerank)" }
            } else {
                "stub embedder only — retrieval quality will be low"
            }
        }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn has_key(env: &str, file: &toml::Value, path: &str) -> bool {
    if std::env::var(env).is_ok() {
        return true;
    }
    let mut cur = file;
    for part in path.split('.') {
        match cur.get(part) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    cur.as_str().map(|s| !s.is_empty()).unwrap_or(false)
}

/// Resolve a secret in the canonical order: env var → config file → None.
/// This is the single source of truth for key lookup so users can either
/// set `GEMINI_API_KEY` in the environment or run `engram config set keys.gemini <value>` and
/// both work identically. Framework rule: env overrides file.
pub fn resolve_secret(env: &str, toml_path: &str) -> Option<String> {
    if let Ok(v) = std::env::var(env) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    let root = read_toml().ok()?;
    let mut cur = &root;
    for part in toml_path.split('.') {
        cur = cur.get(part)?;
    }
    cur.as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

pub fn resolve_config_string(toml_path: &str) -> Option<String> {
    let root = read_toml().ok()?;
    let mut cur = &root;
    for part in toml_path.split('.') {
        cur = cur.get(part)?;
    }
    cur.as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

pub fn resolve_setting(env: &str, toml_path: &str, default: &str) -> String {
    std::env::var(env)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| resolve_config_string(toml_path))
        .unwrap_or_else(|| default.to_string())
}

// Silence unused-imports warning for `Write` on non-unix.
fn _keep_write_import(mut w: impl Write) {
    let _ = writeln!(w, "");
}
