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
        SkillCommand::Install { force } => install(ctx, force),
        SkillCommand::Package { out } => package(ctx, out),
        SkillCommand::Uninstall => uninstall(ctx),
    }
}

/// The four directories install writes and uninstall clears; `doctor` reads
/// the same list to report each copy's freshness.
pub(crate) fn target_dirs() -> Vec<PathBuf> {
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

/// What `install` did to one target directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallStatus {
    /// The file was absent (or --force replaced an edited one) and has been
    /// written.
    Installed,
    /// An untouched copy from another release, replaced.
    Upgraded,
    /// Already byte-identical to what we ship — nothing to do.
    Unchanged,
    /// Locally modified, so left exactly as the user left it.
    SkippedModified,
}

impl InstallStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Upgraded => "upgraded",
            Self::Unchanged => "unchanged",
            Self::SkippedModified => "skipped_modified",
        }
    }
}

/// Whether writing `path` would destroy somebody's edits. A copy engram wrote
/// and nobody touched — this release's or an earlier one's — is not an edit:
/// treating every earlier release as one made `skill install`, the step
/// `update` prescribes, refuse on every upgrade.
fn classify(path: &Path, force: bool) -> InstallStatus {
    if !path.exists() {
        return InstallStatus::Installed;
    }
    match std::fs::read(path) {
        // Unreadable is not "modified": let the write attempt report the real
        // IO error rather than silently claiming the user edited it.
        Err(_) => InstallStatus::Installed,
        Ok(existing) if existing == SKILL_BODY.as_bytes() => InstallStatus::Unchanged,
        Ok(existing)
            if std::str::from_utf8(&existing).is_ok_and(engram_contract::skill::is_pristine) =>
        {
            InstallStatus::Upgraded
        }
        Ok(_) if force => InstallStatus::Installed,
        Ok(_) => InstallStatus::SkippedModified,
    }
}

fn write_skill_dir(dir: &Path) -> Result<Vec<PathBuf>, CliError> {
    std::fs::create_dir_all(dir.join("agents"))?;

    let skill_path = dir.join("SKILL.md");
    std::fs::write(&skill_path, SKILL_BODY)?;

    let openai_path = dir.join("agents/openai.yaml");
    std::fs::write(&openai_path, OPENAI_METADATA)?;

    Ok(vec![skill_path, openai_path])
}

fn install(ctx: &AppContext, force: bool) -> Result<(), CliError> {
    // Two passes on purpose. `skill install` used to fs::write over all four
    // target directories unconditionally, so a locally-tuned SKILL.md was
    // destroyed without a word. Survey first, then refuse the WHOLE install if
    // any target was hand-edited: a partial install that clobbers one client
    // and spares another is worse than doing nothing.
    let dirs = target_dirs();
    let mut plan: Vec<(PathBuf, InstallStatus)> = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let skill_path = dir.join("SKILL.md");
        let status = classify(&skill_path, force);
        plan.push((skill_path, status));
    }

    let modified: Vec<String> = plan
        .iter()
        .filter(|(_, s)| *s == InstallStatus::SkippedModified)
        .map(|(p, _)| p.to_string_lossy().into_owned())
        .collect();
    if !modified.is_empty() {
        return Err(CliError::ConfigHint {
            message: format!(
                "refusing to overwrite locally modified skill file(s): {}. Nothing was written.",
                modified.join(", ")
            ),
            suggestion:
                "run `engram skill install --force` to replace them, or move your edits aside first"
                    .into(),
        });
    }

    let mut installed = Vec::new();
    let mut directories = Vec::with_capacity(dirs.len());
    for (dir, (skill_path, status)) in dirs.iter().zip(plan.iter()) {
        // `unchanged` targets are still rewritten: the write is a no-op on
        // content, and it repairs a missing agents/openai.yaml sidecar.
        for path in write_skill_dir(dir)? {
            installed.push(path.to_string_lossy().into_owned());
        }
        directories.push(json!({
            "directory": dir.to_string_lossy(),
            "skill_path": skill_path.to_string_lossy(),
            "status": status.as_str(),
        }));
    }

    let mut meta = Metadata::default();
    meta.add(
        "installed",
        plan.iter()
            .filter(|(_, s)| *s == InstallStatus::Installed)
            .count(),
    );
    meta.add(
        "upgraded",
        plan.iter()
            .filter(|(_, s)| *s == InstallStatus::Upgraded)
            .count(),
    );
    meta.add(
        "unchanged",
        plan.iter()
            .filter(|(_, s)| *s == InstallStatus::Unchanged)
            .count(),
    );
    meta.add("forced", force);
    print_success(
        ctx.format,
        json!({
            "installed": installed,
            "directories": directories,
            "forced": force,
            "note": "Claude.ai / Claude desktop app use uploadable skill packages; run `engram skill package --out engram-skill.zip`."
        }),
        meta,
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
