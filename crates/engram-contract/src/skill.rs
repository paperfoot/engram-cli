//! Generator 2's first half: skill staleness.
//!
//! The failure this closes: someone changes a default, publishes, and every
//! user keeps a SKILL.md describing the previous release — a document agents
//! trust more than their own experiment. `schema_hash` is stamped into the
//! asset's front matter, so an installed copy carries the hash of the build
//! that installed it, and `doctor` can say plainly whether it still matches.
//!
//! The hash covers the contract DATA only — not prose, examples or rules
//! (§4.4). A typo fix in the skill's prose must not tell every user their
//! skill is stale, or they stop believing the check; adding a flag or changing
//! a default must.

use serde::Serialize;

use crate::schema_hash;

/// Reads `schema_hash:` out of a SKILL.md YAML front matter. Pure; no I/O, and
/// no YAML dependency — the front matter this parses is one we generate.
pub fn parse_schema_hash(body: &str) -> Option<String> {
    let mut lines = body.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix("schema_hash:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Writes this build's `schema_hash` and `contract_version` into a SKILL.md
/// front matter, replacing whatever was there. Idempotent, and the only
/// writer of those two lines.
///
/// Generation is not a `build.rs`: a build script that writes into `src/`
/// breaks `cargo package`, fights incremental rebuilds, and would race
/// `include_str!`. The asset is checked in, `engram-contract-gen` regenerates
/// it, and a test fails the build when the checked-in bytes drift.
pub fn stamp(body: &str) -> String {
    let hash = schema_hash();
    let version = crate::registry::CONTRACT_VERSION;
    let mut out = String::with_capacity(body.len() + 96);
    let mut in_front_matter = false;
    let mut seen_open = false;
    let mut wrote = false;

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "---" && !seen_open {
            seen_open = true;
            in_front_matter = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_front_matter && trimmed == "---" {
            if !wrote {
                // No metadata block at all: give it one rather than dropping
                // the stamp silently.
                out.push_str("metadata:\n");
                out.push_str(&format!("  schema_hash: \"{hash}\"\n"));
                out.push_str(&format!("  contract_version: \"{version}\"\n"));
                wrote = true;
            }
            in_front_matter = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_front_matter
            && (trimmed.starts_with("schema_hash:") || trimmed.starts_with("contract_version:"))
        {
            continue; // replaced below, at the metadata block
        }
        out.push_str(line);
        out.push('\n');
        if in_front_matter && trimmed == "metadata:" {
            out.push_str(&format!("  schema_hash: \"{hash}\"\n"));
            out.push_str(&format!("  contract_version: \"{version}\"\n"));
            wrote = true;
        }
    }
    out
}

/// One installed copy's verdict.
///
/// `modified` exists because `skill install` REFUSES to overwrite a
/// hand-edited SKILL.md: reporting that state as `stale` tells a user to run a
/// command the binary will reject, which is worse advice than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale { installed: String },
    Unreadable,
    Missing,
}

impl Freshness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Freshness::Fresh => "fresh",
            Freshness::Stale { .. } => "stale",
            Freshness::Unreadable => "unreadable",
            Freshness::Missing => "missing",
        }
    }

    /// Worst-first ordering for the roll-up: missing < unreadable < stale <
    /// fresh, so one stale copy is never hidden behind three fresh ones.
    fn rank(&self) -> u8 {
        match self {
            Freshness::Missing => 0,
            Freshness::Unreadable => 1,
            Freshness::Stale { .. } => 2,
            Freshness::Fresh => 3,
        }
    }
}

/// Compares one installed SKILL.md against this binary's contract.
pub fn freshness(installed_body: Option<&str>) -> Freshness {
    match installed_body {
        None => Freshness::Missing,
        Some(body) => match parse_schema_hash(body) {
            None => Freshness::Unreadable,
            Some(h) if h == schema_hash() => Freshness::Fresh,
            Some(h) => Freshness::Stale { installed: h },
        },
    }
}

/// `doctor`'s `skill` key. Takes the bodies rather than the paths, so this
/// crate stays free of I/O and the caller keeps the one list of skill
/// directories it already has.
///
/// The overall status is the WORST per-file status; every copy missing is
/// reported as `not_installed`, which is not a fault. Deliberately not folded
/// into doctor's `healthy` boolean: a stale skill is documentation drift, not
/// a broken store, and failing CI for every user who has not re-run
/// `skill install` would teach them to ignore doctor.
pub fn doctor_json(installed: &[(String, Option<String>)]) -> serde_json::Value {
    let per_file: Vec<(String, Freshness)> = installed
        .iter()
        .map(|(path, body)| (path.clone(), freshness(body.as_deref())))
        .collect();

    let all_missing = per_file.iter().all(|(_, f)| *f == Freshness::Missing);
    let worst = per_file.iter().map(|(_, f)| f.rank()).min().unwrap_or(0);
    let status = if per_file.is_empty() || all_missing {
        "not_installed".to_string()
    } else {
        per_file
            .iter()
            .find(|(_, f)| f.rank() == worst)
            .map(|(_, f)| f.as_str().to_string())
            .unwrap_or_else(|| "fresh".to_string())
    };

    let stale_count = per_file
        .iter()
        .filter(|(_, f)| matches!(f, Freshness::Stale { .. }))
        .count();
    let unreadable_count = per_file
        .iter()
        .filter(|(_, f)| *f == Freshness::Unreadable)
        .count();

    let suggestion = if stale_count > 0 {
        Some(format!(
            "run `engram skill install` to refresh {stale_count} stale skill file(s)"
        ))
    } else if unreadable_count > 0 {
        Some(
            "that copy was edited locally; `engram skill install --force` replaces it, or move it aside first"
                .to_string(),
        )
    } else {
        None
    };

    serde_json::json!({
        "status": status,
        "expected_schema_hash": schema_hash(),
        "installed": per_file.iter().map(|(path, f)| {
            serde_json::json!({
                "path": path,
                "status": f.as_str(),
                "schema_hash": match f {
                    Freshness::Stale { installed } => Some(installed.clone()),
                    Freshness::Fresh => Some(schema_hash()),
                    _ => None,
                },
            })
        }).collect::<Vec<_>>(),
        "suggestion": suggestion,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front_matter(hash: &str) -> String {
        format!("---\nname: engram\nmetadata:\n  schema_hash: \"{hash}\"\n---\n\n# engram\n")
    }

    #[test]
    fn a_hash_is_read_out_of_the_front_matter() {
        assert_eq!(
            parse_schema_hash(&front_matter("abc123")),
            Some("abc123".into())
        );
    }

    #[test]
    fn a_file_without_front_matter_is_unreadable_not_stale() {
        // The usual cause is a user stripping the metadata block, and telling
        // them to reinstall a file they deliberately edited is wrong advice.
        assert_eq!(parse_schema_hash("# engram\n"), None);
        assert_eq!(freshness(Some("# engram\n")), Freshness::Unreadable);
    }

    #[test]
    fn a_hash_after_the_front_matter_is_not_read() {
        let body = "---\nname: engram\n---\n\nschema_hash: deadbeef\n";
        assert_eq!(parse_schema_hash(body), None);
    }

    #[test]
    fn this_builds_own_hash_is_fresh() {
        let body = front_matter(&schema_hash());
        assert_eq!(freshness(Some(&body)), Freshness::Fresh);
    }

    #[test]
    fn a_foreign_hash_is_stale_and_names_what_is_installed() {
        let body = front_matter("deadbeef");
        assert_eq!(
            freshness(Some(&body)),
            Freshness::Stale {
                installed: "deadbeef".into()
            }
        );
    }

    #[test]
    fn no_files_at_all_is_not_a_fault() {
        let v = doctor_json(&[("~/.claude/skills/engram/SKILL.md".into(), None)]);
        assert_eq!(v["status"], "not_installed");
        assert!(v["suggestion"].is_null());
    }

    #[test]
    fn one_stale_copy_is_not_hidden_behind_three_fresh_ones() {
        let fresh = front_matter(&schema_hash());
        let v = doctor_json(&[
            ("a".into(), Some(fresh.clone())),
            ("b".into(), Some(fresh.clone())),
            ("c".into(), Some(front_matter("deadbeef"))),
        ]);
        assert_eq!(v["status"], "stale");
        assert!(v["suggestion"]
            .as_str()
            .unwrap()
            .contains("1 stale skill file"));
        assert_eq!(v["installed"][2]["schema_hash"], "deadbeef");
    }

    #[test]
    fn stamping_is_idempotent_and_makes_a_file_fresh() {
        let raw = "---\nname: engram\nmetadata:\n  homepage: \"x\"\n---\n\n# engram\n";
        let once = stamp(raw);
        assert_eq!(freshness(Some(&once)), Freshness::Fresh);
        assert_eq!(stamp(&once), once);
        assert!(once.contains("  homepage: \"x\""), "{once}");
    }

    #[test]
    fn stamping_replaces_a_stale_hash_rather_than_adding_a_second() {
        let stale = "---\nname: engram\nmetadata:\n  schema_hash: \"deadbeef\"\n---\n\n# engram\n";
        let fresh = stamp(stale);
        assert_eq!(fresh.matches("schema_hash:").count(), 1);
        assert!(!fresh.contains("deadbeef"));
    }

    #[test]
    fn a_front_matter_with_no_metadata_block_still_gets_stamped() {
        let raw = "---\nname: engram\n---\n\n# engram\n";
        assert_eq!(freshness(Some(&stamp(raw))), Freshness::Fresh);
    }

    #[test]
    fn an_edited_copy_is_told_about_force() {
        let v = doctor_json(&[("a".into(), Some("# engram\n".into()))]);
        assert_eq!(v["status"], "unreadable");
        assert!(v["suggestion"].as_str().unwrap().contains("--force"));
    }
}
