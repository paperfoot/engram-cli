//! `engram skill install|uninstall` — deploys the skill signpost.

use crate::cli::SkillCommand;
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use std::path::PathBuf;

const SKILL_BODY: &str = include_str!("../../assets/skill.md");

pub fn run(ctx: &AppContext, sub: SkillCommand) -> Result<(), CliError> {
    match sub {
        SkillCommand::Install => install(ctx),
        SkillCommand::Uninstall => uninstall(ctx),
    }
}

fn target_paths() -> Vec<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    vec![
        home.join(".claude/skills/engram/SKILL.md"),
        home.join(".codex/skills/engram/SKILL.md"),
        home.join(".gemini/skills/engram/SKILL.md"),
    ]
}

fn install(ctx: &AppContext) -> Result<(), CliError> {
    let mut installed = Vec::new();
    for p in target_paths() {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, SKILL_BODY)?;
        installed.push(p.to_string_lossy().into_owned());
    }
    print_success(
        ctx.format,
        json!({ "installed": installed }),
        Metadata::default(),
        |data| println!("Installed skill to:\n{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn uninstall(ctx: &AppContext) -> Result<(), CliError> {
    let mut removed = Vec::new();
    for p in target_paths() {
        if p.exists() {
            let _ = std::fs::remove_file(&p);
            removed.push(p.to_string_lossy().into_owned());
        }
    }
    print_success(
        ctx.format,
        json!({ "removed": removed }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}
