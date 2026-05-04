//! `engram update [--check]` -- distribution-aware self update.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::process::Command;
use std::time::{Duration, Instant};

const DEFAULT_GITHUB_REPO: &str = "paperfoot/engram-cli";
const DEFAULT_CRATE: &str = "paperfoot-engram";
const DEFAULT_HOMEBREW_FORMULA: &str = "paperfoot/tap/engram";

#[derive(Debug, Clone)]
struct Latest {
    version: String,
    release_url: String,
    source: String,
}

#[derive(Debug, Clone)]
struct UpgradeCommand {
    program: String,
    args: Vec<String>,
    display: String,
    executable: bool,
}

pub async fn run(ctx: &AppContext, check: bool) -> Result<(), CliError> {
    let start = Instant::now();
    let current_version = env!("CARGO_PKG_VERSION");
    let update_mode = configured("ENGRAM_UPDATE_MODE", "update.mode", "auto");
    let channel = configured("ENGRAM_UPDATE_CHANNEL", "update.channel", "github");
    let install_source = detect_install_source();
    let github_repo = configured(
        "ENGRAM_UPDATE_GITHUB_REPO",
        "update.github_repo",
        DEFAULT_GITHUB_REPO,
    );
    let crate_name = configured("ENGRAM_UPDATE_CRATE", "update.crate", DEFAULT_CRATE);
    let homebrew_formula = configured(
        "ENGRAM_UPDATE_HOMEBREW_FORMULA",
        "update.homebrew_formula",
        DEFAULT_HOMEBREW_FORMULA,
    );

    let latest = if update_mode == "disabled" {
        None
    } else {
        Some(fetch_latest(&channel, &github_repo, &crate_name).await?)
    };

    let update_available = latest
        .as_ref()
        .map(|latest| compare_versions(&latest.version, current_version) == Ordering::Greater)
        .unwrap_or(false);
    let upgrade = latest.as_ref().and_then(|latest| {
        upgrade_command(
            &install_source,
            &latest.version,
            &github_repo,
            &crate_name,
            &homebrew_formula,
        )
    });
    let can_execute_update = upgrade
        .as_ref()
        .map(|upgrade| upgrade.executable)
        .unwrap_or(false);
    let execution_allowed =
        !check && update_mode == "auto" && update_available && can_execute_update;

    let mut executed = false;
    let mut exit_code: Option<i32> = None;
    if !check && update_mode == "auto" && update_available && !can_execute_update {
        return Err(CliError::Config(format!(
            "cannot auto-update install source `{install_source}`; use `engram update --check --json` for the manual upgrade command"
        )));
    }
    if execution_allowed {
        let Some(upgrade) = &upgrade else {
            return Err(CliError::Config(format!(
                "cannot auto-update install source `{install_source}`; use the reported upgrade_command manually"
            )));
        };
        if !ctx.quiet {
            eprintln!("running update: {}", upgrade.display);
        }
        let output = Command::new(&upgrade.program)
            .args(&upgrade.args)
            .output()
            .map_err(|e| {
                CliError::Transient(format!("failed to run `{}`: {e}", upgrade.display))
            })?;
        executed = true;
        exit_code = output.status.code();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = stderr.lines().rev().take(6).collect::<Vec<_>>();
            return Err(CliError::Transient(format!(
                "update command failed: {}; stderr tail: {}",
                upgrade.display,
                tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
            )));
        }
    }

    let latest_version = latest.as_ref().map(|l| l.version.clone());
    let release_url = latest.as_ref().map(|l| l.release_url.clone());
    let latest_source = latest
        .as_ref()
        .map(|l| l.source.clone())
        .unwrap_or_else(|| "disabled".to_string());
    let upgrade_display = upgrade.as_ref().map(|u| u.display.clone());
    let post_update_commands = if update_available {
        vec!["engram skill install".to_string()]
    } else {
        Vec::new()
    };

    let mut meta = Metadata::default();
    meta.elapsed_ms = start.elapsed().as_millis() as u64;
    meta.add("current_version", current_version);
    meta.add("latest_version", latest_version.clone());
    meta.add("update_available", update_available);
    meta.add("install_source", install_source.clone());
    meta.add("update_mode", update_mode.clone());
    meta.add("channel", channel.clone());

    print_success(
        ctx.format,
        json!({
            "current_version": current_version,
            "latest_version": latest_version,
            "update_available": update_available,
            "install_source": install_source,
            "update_mode": update_mode,
            "channel": channel,
            "latest_source": latest_source,
            "release_url": release_url,
            "upgrade_command": upgrade_display,
            "can_execute_update": can_execute_update,
            "executed": executed,
            "execution_allowed": execution_allowed,
            "exit_code": exit_code,
            "requires_restart": executed,
            "requires_skill_reinstall": update_available,
            "post_update_commands": post_update_commands,
            "agent_when_to_run": {
                "check": "Run `engram update --check --json` during agent bootstrap or maintenance to detect stale installs.",
                "execute": "Run `engram update --json` only when the user asks to update this CLI or a maintenance workflow explicitly permits package-manager changes.",
                "after_update": "Restart the agent process and run `engram skill install` if agent skill files are used."
            },
            "config": {
                "update.mode": "auto|check_only|disabled; default auto",
                "update.channel": "github|crates; default github",
                "update.install_source": "optional override: homebrew|cargo|standalone|source_build",
                "update.github_repo": DEFAULT_GITHUB_REPO,
                "update.crate": DEFAULT_CRATE,
                "update.homebrew_formula": DEFAULT_HOMEBREW_FORMULA
            }
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn configured(env: &str, key: &str, default: &str) -> String {
    crate::commands::config::resolve_setting(env, key, default)
        .trim()
        .to_string()
}

async fn fetch_latest(
    channel: &str,
    github_repo: &str,
    crate_name: &str,
) -> Result<Latest, CliError> {
    if let Some(version) = std::env::var("ENGRAM_UPDATE_LATEST_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        let release_url = std::env::var("ENGRAM_UPDATE_RELEASE_URL").unwrap_or_else(|_| {
            format!(
                "https://github.com/{github_repo}/releases/tag/v{}",
                version.trim().trim_start_matches('v')
            )
        });
        return Ok(Latest {
            version: normalize_version(&version),
            release_url,
            source: "env".to_string(),
        });
    }

    match channel {
        "github" => latest_from_github(github_repo).await,
        "crates" | "crates.io" => latest_from_crates(crate_name).await,
        other => Err(CliError::BadInput(format!(
            "unknown update channel `{other}` (expected github|crates)"
        ))),
    }
}

async fn latest_from_github(repo: &str) -> Result<Latest, CliError> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CliError::Transient(format!("failed to build update HTTP client: {e}")))?;
    let resp = client
        .get(&url)
        .header(
            "User-Agent",
            format!(
                "engram/{} ({})",
                env!("CARGO_PKG_VERSION"),
                DEFAULT_GITHUB_REPO
            ),
        )
        .send()
        .await
        .map_err(|e| CliError::Transient(format!("failed to check GitHub release: {e}")))?;
    if resp.status().as_u16() == 403 || resp.status().as_u16() == 429 {
        return Err(CliError::RateLimited(
            "GitHub release API rate limit while checking updates".into(),
        ));
    }
    if !resp.status().is_success() {
        return Err(CliError::Transient(format!(
            "GitHub release check returned HTTP {} for {url}",
            resp.status()
        )));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| CliError::Transient(format!("invalid GitHub release JSON: {e}")))?;
    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Transient("GitHub release JSON missing tag_name".into()))?;
    let release_url = body
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or(&url)
        .to_string();
    Ok(Latest {
        version: normalize_version(tag),
        release_url,
        source: "github".to_string(),
    })
}

async fn latest_from_crates(crate_name: &str) -> Result<Latest, CliError> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CliError::Transient(format!("failed to build update HTTP client: {e}")))?;
    let resp = client
        .get(&url)
        .header(
            "User-Agent",
            format!(
                "engram/{} ({})",
                env!("CARGO_PKG_VERSION"),
                DEFAULT_GITHUB_REPO
            ),
        )
        .send()
        .await
        .map_err(|e| CliError::Transient(format!("failed to check crates.io: {e}")))?;
    if resp.status().as_u16() == 403 || resp.status().as_u16() == 429 {
        return Err(CliError::RateLimited(
            "crates.io API rate limit while checking updates".into(),
        ));
    }
    if !resp.status().is_success() {
        return Err(CliError::Transient(format!(
            "crates.io update check returned HTTP {} for {url}",
            resp.status()
        )));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| CliError::Transient(format!("invalid crates.io JSON: {e}")))?;
    let version = body
        .get("crate")
        .and_then(|v| v.get("max_version"))
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Transient("crates.io JSON missing crate.max_version".into()))?;
    Ok(Latest {
        version: normalize_version(version),
        release_url: format!("https://crates.io/crates/{crate_name}/{version}"),
        source: "crates".to_string(),
    })
}

fn detect_install_source() -> String {
    if let Some(overridden) = std::env::var("ENGRAM_UPDATE_INSTALL_SOURCE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| crate::commands::config::resolve_config_string("update.install_source"))
    {
        return overridden.trim().to_ascii_lowercase();
    }
    let exe = std::env::current_exe().ok();
    let canonical = exe.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
    let combined = [exe, canonical]
        .into_iter()
        .flatten()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>()
        .join("\n");

    if combined.contains("/Cellar/engram/") || combined.contains("/homebrew/Cellar/engram/") {
        "homebrew".to_string()
    } else if combined.contains("/.cargo/bin/") {
        "cargo".to_string()
    } else if combined.contains("/target/debug/") || combined.contains("/target/release/") {
        "source_build".to_string()
    } else {
        "standalone".to_string()
    }
}

fn upgrade_command(
    install_source: &str,
    latest_version: &str,
    github_repo: &str,
    crate_name: &str,
    homebrew_formula: &str,
) -> Option<UpgradeCommand> {
    match install_source {
        "homebrew" | "brew" => Some(UpgradeCommand {
            program: "brew".to_string(),
            args: vec!["upgrade".to_string(), homebrew_formula.to_string()],
            display: format!("brew upgrade {homebrew_formula}"),
            executable: true,
        }),
        "cargo" | "cargo_install" => Some(UpgradeCommand {
            program: "cargo".to_string(),
            args: vec![
                "install".to_string(),
                crate_name.to_string(),
                "--version".to_string(),
                latest_version.to_string(),
                "--locked".to_string(),
                "--force".to_string(),
            ],
            display: format!(
                "cargo install {crate_name} --version {latest_version} --locked --force"
            ),
            executable: true,
        }),
        "standalone" | "github_release" => Some(UpgradeCommand {
            program: "gh".to_string(),
            args: vec![
                "release".to_string(),
                "download".to_string(),
                format!("v{latest_version}"),
                "--repo".to_string(),
                github_repo.to_string(),
                "--pattern".to_string(),
                format!("engram-v{latest_version}-*"),
            ],
            display: format!(
                "gh release download v{latest_version} --repo {github_repo} --pattern 'engram-v{latest_version}-*'"
            ),
            executable: false,
        }),
        _ => None,
    }
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let mut a = version_parts(left);
    let mut b = version_parts(right);
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a.cmp(&b)
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split(|c| c == '.' || c == '-' || c == '+')
        .take_while(|part| part.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_handles_multi_digit_segments() {
        assert_eq!(compare_versions("0.1.10", "0.1.2"), Ordering::Greater);
        assert_eq!(compare_versions("v0.1.1", "0.1.1"), Ordering::Equal);
        assert_eq!(compare_versions("0.2.0", "0.10.0"), Ordering::Less);
    }

    #[test]
    fn cargo_upgrade_command_is_agent_runnable() {
        let cmd = upgrade_command(
            "cargo",
            "1.2.3",
            DEFAULT_GITHUB_REPO,
            DEFAULT_CRATE,
            DEFAULT_HOMEBREW_FORMULA,
        )
        .unwrap();
        assert_eq!(
            cmd.display,
            "cargo install paperfoot-engram --version 1.2.3 --locked --force"
        );
    }
}
