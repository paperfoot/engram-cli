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
    // Threshold 16, matching GeminiEmbedder::mask: at 12, head-6 + tail-4
    // left only 2 characters hidden.
    if key.len() < 16 {
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
    toml::from_str(&text).map_err(|e| toml_error(&p, &text, &e))
}

/// A config.toml parse error as a line and column, never the source line:
/// `toml`'s Display quotes the offending line, which can hold an API key,
/// and this error reaches `doctor`, `config show|check|set` and every
/// command that reads a strict setting.
fn toml_error(path: &std::path::Path, text: &str, e: &toml::de::Error) -> CliError {
    let at = e
        .span()
        .and_then(|span| text.get(..span.start))
        .map(|before| {
            let line_start = before.rfind('\n').map_or(0, |i| i + 1);
            format!(
                " at line {}, column {}",
                before.matches('\n').count() + 1,
                before[line_start..].chars().count() + 1
            )
        })
        .unwrap_or_default();
    CliError::Config(format!(
        "invalid TOML in {}{at}: {}",
        path.display(),
        e.message().trim().replace('\n', "; ")
    ))
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
    let typesafe_source = describe_key_source("TYPESAFE_API_KEY", &file_cfg, "keys.typesafe");
    let chunking = crate::commands::chunks::resolve_policy();

    print_success(
        ctx.format,
        json!({
            "config_path": cfg_path.to_string_lossy(),
            "config_file_exists": cfg_path.exists(),
            "data_path": paths::db_path().to_string_lossy(),
            "cache_path": paths::cache_dir().to_string_lossy(),
            "keys": {
                "gemini": gemini_source,
                "gemini_fallbacks": gemini_fallback_keys()
                    .iter()
                    .map(|k| mask_key(k))
                    .collect::<Vec<_>>(),
                "cohere": cohere_source,
                "openrouter": openrouter_source,
                "typesafe": typesafe_source,
            },
            // Per-site judge modes (off | shadow | on; default off) and the
            // route a networked command would take.
            "judge": crate::judge::status(),
            "retrieval": {
                "profile": "cloud_quality",
                // Derived, not hardcoded: this line said 50 long after the
                // cloud_quality default became 200, so `config show` was
                // reporting a number the retriever never used.
                "rerank_top_n": crate::retrieval::RetrievalProfile::CloudQuality
                    .default_rerank_top_n(),
                // Unset means reads span EVERY KB. Set it to pin recall and
                // `memory search` to one KB when no --kb flag is given.
                "default_kb": std::env::var(crate::retrieval::DEFAULT_KB_ENV).ok()
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| resolve_config_string(crate::retrieval::DEFAULT_KB_CONFIG_KEY)),
                "default_kb_effective_scope": crate::retrieval::resolve_kb_scope(None, false).label(),
            },
            // Chunk shape. `min_chars` is the hard floor: a fragment shorter
            // than this is merged into a neighbour instead of becoming its own
            // retrievable chunk, because a bare "Sep 16, 2025" outranks real
            // answers in recall. A memory that is entirely that short is still
            // stored as one chunk.
            "chunking": {
                "min_chars": chunking.min_chars,
                "target_chars": chunking.target_chars,
                "max_chars": chunking.max_chars,
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
    if let Some(v) = env_secret(env) {
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
    if let Some(s) = cur.as_str().filter(|s| !s.is_empty()) {
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
    let typesafe = has_key("TYPESAFE_API_KEY", &file_cfg, "keys.typesafe");

    // LIVE per-key probe. "Configured" is not "working": a key can be
    // syntactically present while its AI Studio project sits at zero prepaid
    // credits, and the free models-list endpoint answers 200 for such a key —
    // only a real embed call tells the truth. One 5-character embed per key,
    // a fraction of a thousandth of a cent, so `config check` can say which
    // key in the chain will actually serve.
    let mut gemini_keys: Vec<(String, String)> = Vec::new();
    if let Some(primary) = resolve_secret("GEMINI_API_KEY", "keys.gemini") {
        gemini_keys.push(("primary".to_string(), primary));
    }
    for (i, k) in gemini_fallback_keys().into_iter().enumerate() {
        gemini_keys.push((format!("fallback_{}", i + 1), k));
    }
    // Concurrent probes on ONE shared client: serial fresh-client probes made
    // an offline machine wait up to 20s PER KEY in the chain.
    let probe_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .ok();
    let mut probes = serde_json::Map::new();
    let mut any_key_serves = gemini_keys.is_empty();
    let mut join = tokio::task::JoinSet::new();
    for (i, (_, key)) in gemini_keys.iter().enumerate() {
        let client = probe_client.clone();
        let key = key.clone();
        join.spawn(async move { (i, probe_gemini_key(client.as_ref(), &key).await) });
    }
    let mut probe_results: Vec<&'static str> = vec!["probe_error"; gemini_keys.len()];
    while let Some(res) = join.join_next().await {
        if let Ok((i, status)) = res {
            probe_results[i] = status;
        }
    }
    for ((label, key), status) in gemini_keys.iter().zip(probe_results) {
        if status == "ok" {
            any_key_serves = true;
        }
        probes.insert(format!("{label} ({})", mask_key(key)), json!(status));
    }

    print_success(
        ctx.format,
        json!({
            "gemini": if gemini { "configured" } else { "missing" },
            "gemini_keys_probed": probes,
            "cohere": if cohere { "configured (optional)" } else { "missing (optional)" },
            // Not "compiler optional": `remember` extracts facts through
            // OpenRouter by default, so without it no facts are stored.
            "openrouter": if openrouter {
                "configured (fact extraction, compiler, judge)"
            } else {
                "missing (no fact extraction or compiler; judge only via TypeSafe)"
            },
            "typesafe": if typesafe {
                "configured (judge, direct)"
            } else {
                "missing (optional; the judge can go through OpenRouter)"
            },
            // Presence only, no live probe. `route` is what a judged command
            // would use; `warnings` names a site set to shadow or on with
            // no route or on the test stub.
            "judge": crate::judge::status(),
            // Configured is not working: "ok" is true only when a key in
            // the chain actually SERVED the live probe. An agent branching on
            // this contract must not proceed on a depleted chain.
            "ok": gemini && any_key_serves,
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

/// One cheap embedContent against a single key. Statuses are deliberately a
/// closed vocabulary an agent can branch on.
async fn probe_gemini_key(client: Option<&reqwest::Client>, key: &str) -> &'static str {
    let Some(client) = client else {
        return "probe_error";
    };
    match gemini_probe(client, key).send().await {
        Err(_) => "unreachable",
        Ok(resp) => match resp.status().as_u16() {
            200 => "ok",
            429 => "quota_or_credits_depleted",
            400 | 401 | 403 => "invalid_or_revoked",
            _ => "probe_error",
        },
    }
}

/// The probe request. The key rides in a header, never the URL: reqwest
/// puts the URL in its error text, so a `?key=` URL leaks the key wherever
/// an error is shown.
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

fn has_key(env: &str, file: &toml::Value, path: &str) -> bool {
    if env_secret(env).is_some() {
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

/// Split a fallback-key list: comma- or whitespace-separated, trimmed,
/// empties dropped, order preserved, duplicates removed. Pure so it can be
/// unit-tested without touching env or the config file.
pub fn parse_key_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split([',', ' ', '\n', '\t']) {
        let k = part.trim();
        if !k.is_empty() && !out.iter().any(|s| s == k) {
            out.push(k.to_string());
        }
    }
    out
}

/// Gemini fallback keys, in priority order, with the primary excluded so a
/// key listed in both places is not tried twice. Sources, same precedence as
/// every other secret: GEMINI_API_KEY_FALLBACKS env, else `keys.gemini_fallbacks`
/// in the config file — comma-separated in either place.
pub fn gemini_fallback_keys() -> Vec<String> {
    let raw =
        resolve_secret("GEMINI_API_KEY_FALLBACKS", "keys.gemini_fallbacks").unwrap_or_default();
    let primary = resolve_secret("GEMINI_API_KEY", "keys.gemini");
    parse_key_list(&raw)
        .into_iter()
        .filter(|k| primary.as_deref() != Some(k.as_str()))
        .collect()
}

/// A key from the environment; an empty variable is absent. The one env
/// reader for keys: `config show` and `config check` once treated
/// `OPENROUTER_API_KEY=` as configured while every command using the key
/// treated it as missing.
fn env_secret(env: &str) -> Option<String> {
    std::env::var(env).ok().filter(|v| !v.is_empty())
}

/// Resolve a secret in the canonical order: env var → config file → None.
/// This is the single source of truth for key lookup so users can either
/// set `GEMINI_API_KEY` in the environment or run `engram config set keys.gemini <value>` and
/// both work identically. Framework rule: env overrides file.
pub fn resolve_secret(env: &str, toml_path: &str) -> Option<String> {
    if let Some(v) = env_secret(env) {
        return Some(v);
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

/// The value at a dotted `toml_path` in config.toml, `None` when unset.
/// Unlike [`resolve_config_string`], an unreadable or invalid file is an
/// error rather than "unset", for settings that must never fall back to a
/// default silently.
pub fn config_value(toml_path: &str) -> Result<Option<toml::Value>, CliError> {
    let root = read_toml()?;
    let mut cur = &root;
    for part in toml_path.split('.') {
        match cur.get(part) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur.clone()))
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
    let _ = writeln!(w);
}

/// True when the user explicitly disabled reranking
/// (ENGRAM_RERANK_PROVIDER=none). Lets recall keep working when the
/// Cohere account is broken (e.g. 402 billing) without silently changing
/// behavior for anyone else: reranker API errors still fail loud by default.
pub fn rerank_disabled() -> bool {
    std::env::var("ENGRAM_RERANK_PROVIDER")
        .map(|v| v.eq_ignore_ascii_case("none"))
        .unwrap_or(false)
}

/// True when ENGRAM_BENCH_FORCE_STUB asks for the deterministic stub
/// embedder. The VALUE decides, not the variable's presence: the old
/// `env::var(..).is_ok()` check meant `ENGRAM_BENCH_FORCE_STUB=0` — the
/// natural way to switch the hook OFF in a shell — silently forced the
/// stub under a cloud profile, suppressing the missing-key error the
/// provider-parity fix exists to raise. One reader, shared by every
/// write and read path that can select an embedder.
pub fn stub_forced() -> bool {
    std::env::var("ENGRAM_BENCH_FORCE_STUB")
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toml_error_never_quotes_config_toml() {
        let secret = "AIzaFAKESECRET0123456789abcdef";
        for text in [
            format!("[keys]\ngemini \"{secret}\"\n"),
            format!("[keys]\nopenrouter = \"{secret}\n"),
            format!("[keys]\ngemini = {secret}\n"),
            format!("[keys]\n{secret}\n"),
            format!("[keys]\ngemini = \"{secret}\"\ngemini = \"{secret}\"\n"),
        ] {
            let e = toml::from_str::<toml::Value>(&text).unwrap_err();
            let msg = toml_error(std::path::Path::new("/c/config.toml"), &text, &e).to_string();
            assert!(!msg.contains("AIza"), "{msg}");
            assert!(msg.contains("/c/config.toml at line "), "{msg}");
        }
    }

    #[test]
    fn an_empty_key_is_absent_in_env_and_in_the_file() {
        let file: toml::Value = toml::from_str("[keys]\ntypesafe = \"\"\n").unwrap();
        // Read by these tests only.
        let unset = "ENGRAM_TEST_UNSET_KEY_3F9C1B";
        let empty = "ENGRAM_TEST_EMPTY_KEY_3F9C1B";
        std::env::set_var(empty, "");
        for env in [unset, empty] {
            assert!(!has_key(env, &file, "keys.typesafe"), "{env}");
            assert_eq!(
                describe_key_source(env, &file, "keys.typesafe"),
                json!({ "source": "unset" }),
                "{env}"
            );
        }
    }

    #[test]
    fn the_gemini_probe_sends_its_key_in_a_header_not_the_url() {
        let key = "AIzaFAKEPROBEKEY0123456789";
        let req = gemini_probe(&reqwest::Client::new(), key).build().unwrap();
        assert_eq!(req.url().query(), None);
        assert!(!req.url().as_str().contains(key));
        assert_eq!(req.headers()["x-goog-api-key"], key);
    }

    #[test]
    fn key_lists_split_on_commas_and_whitespace_and_dedup() {
        assert_eq!(
            parse_key_list("a, b,,  c\na  "),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert!(parse_key_list("").is_empty());
        assert!(parse_key_list("  , ,\t").is_empty());
    }
}
