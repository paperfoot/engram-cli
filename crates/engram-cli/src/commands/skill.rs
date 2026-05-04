//! `engram skill install|package|uninstall` — deploys the agent skill.

use crate::cli::SkillCommand;
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

const SKILL_BODY: &str = include_str!("../../assets/skill.md");
const OPENAI_METADATA: &str = include_str!("../../assets/agents/openai.yaml");

pub fn run(ctx: &AppContext, sub: SkillCommand) -> Result<(), CliError> {
    match sub {
        SkillCommand::Install => install(ctx),
        SkillCommand::Package { out } => package(ctx, out),
        SkillCommand::Uninstall => uninstall(ctx),
    }
}

fn target_dirs() -> Vec<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    vec![
        // Claude Code personal skills.
        home.join(".claude/skills/engram"),
        // Codex legacy/current local skill directory used by existing installs.
        home.join(".codex/skills/engram"),
        // Gemini CLI personal skills.
        home.join(".gemini/skills/engram"),
        // Cross-client Agent Skills convention used by Codex and other agents.
        home.join(".agents/skills/engram"),
    ]
}

fn write_skill_dir(dir: &Path) -> Result<Vec<PathBuf>, CliError> {
    std::fs::create_dir_all(dir.join("agents"))?;

    let skill_path = dir.join("SKILL.md");
    std::fs::write(&skill_path, SKILL_BODY)?;

    let openai_path = dir.join("agents/openai.yaml");
    std::fs::write(&openai_path, OPENAI_METADATA)?;

    Ok(vec![skill_path, openai_path])
}

fn install(ctx: &AppContext) -> Result<(), CliError> {
    let mut installed = Vec::new();
    for dir in target_dirs() {
        for path in write_skill_dir(&dir)? {
            installed.push(path.to_string_lossy().into_owned());
        }
    }
    print_success(
        ctx.format,
        json!({
            "installed": installed,
            "note": "Claude.ai / Claude desktop app use uploadable skill packages; run `engram skill package --out engram-skill.zip`."
        }),
        Metadata::default(),
        |data| {
            println!(
                "Installed skill to:\n{}",
                serde_json::to_string_pretty(data).unwrap()
            )
        },
    );
    Ok(())
}

fn package(ctx: &AppContext, out: Option<PathBuf>) -> Result<(), CliError> {
    let out = out.unwrap_or_else(|| PathBuf::from("engram-skill.zip"));
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(&out)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.add_directory("engram/", options)?;
    zip.start_file("engram/SKILL.md", options)?;
    zip.write_all(SKILL_BODY.as_bytes())?;

    zip.add_directory("engram/agents/", options)?;
    zip.start_file("engram/agents/openai.yaml", options)?;
    zip.write_all(OPENAI_METADATA.as_bytes())?;

    zip.finish()?;

    print_success(
        ctx.format,
        json!({
            "package": out.to_string_lossy(),
            "contains": ["engram/SKILL.md", "engram/agents/openai.yaml"],
            "for": "Upload to Claude.ai / Claude desktop app Skills, or inspect as a portable Agent Skills package."
        }),
        Metadata::default(),
        |data| println!("{}", data),
    );
    Ok(())
}

fn uninstall(ctx: &AppContext) -> Result<(), CliError> {
    let mut removed = Vec::new();
    for dir in target_dirs() {
        for p in [dir.join("SKILL.md"), dir.join("agents/openai.yaml")] {
            if p.exists() {
                let _ = std::fs::remove_file(&p);
                removed.push(p.to_string_lossy().into_owned());
            }
        }
        let _ = std::fs::remove_dir(dir.join("agents"));
        let _ = std::fs::remove_dir(&dir);
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
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
