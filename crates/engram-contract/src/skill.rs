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
    front_matter_value(body, "schema_hash:")
}

fn front_matter_value(body: &str, key: &str) -> Option<String> {
    let mut lines = body.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix(key) {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The front-matter lines `stamp` owns. The body hash is computed with all
/// three removed, so restamping never moves it.
const STAMP_KEYS: &[&str] = &["schema_hash:", "contract_version:", "body_hash:"];

/// Namespace for skill body hashes. Like [`crate::CONTRACT_NAMESPACE`], fixed
/// once: changing it would make every installed copy read as hand-edited.
pub const SKILL_BODY_NAMESPACE: uuid::Uuid = uuid::uuid!("5a915b48-bb13-4a42-982e-8a96c4c18e48");

fn v5(bytes: &[u8]) -> String {
    uuid::Uuid::new_v5(&SKILL_BODY_NAMESPACE, bytes)
        .simple()
        .to_string()
}

/// The hash `stamp` records as `body_hash`: the file with its stamp lines
/// removed and its line endings normalized. Same primitive as `schema_hash`
/// (`Uuid::new_v5`), so no hashing dependency arrives for one field.
pub fn body_hash(body: &str) -> String {
    let mut unstamped = String::with_capacity(body.len());
    let (mut seen_open, mut in_front_matter) = (false, false);
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            in_front_matter = !seen_open;
            seen_open = true;
        } else if in_front_matter && STAMP_KEYS.iter().any(|k| trimmed.starts_with(k)) {
            continue;
        }
        unstamped.push_str(line);
        unstamped.push('\n');
    }
    v5(unstamped.as_bytes())
}

/// Every SKILL.md engram released before copies carried their own
/// `body_hash`, hashed whole (`v5` of the file bytes): 0.1.0 (= rc.1), 0.1.1,
/// 0.1.2, 0.1.3, 0.1.4, 0.2.0, 0.3.0, 0.3.1, 0.4.0 and 0.4.1. A copy that
/// matches one byte for byte is a copy nobody edited. The list is closed:
/// every release from here on stamps a `body_hash` that verifies itself.
const RELEASED_BEFORE_BODY_HASH: &[&str] = &[
    "7f310dc9d1d55e6db51996b3770a83ca",
    "9d12e804770d55c2aa9412981197d6d8",
    "b8a70c1a07975aa3a0ce5bd499fe3028",
    "f26c9b331ad45301a026d46ca20c4247",
    "92debb1f32865079810d8cc646efbd66",
    "617b27fd60755513a69892a90bbd2ffc",
    "ce638efae3ed5f6d81092169e9655319",
    "676c91349a845b2aaee09cc9f3abb216",
    "2e2e710102bf5785a07506f7db9bd6a7",
    "c32757d797e4542999546457a449bd2f",
];

/// True when `installed` is a copy engram wrote and nobody edited: its
/// recorded `body_hash` matches its own content, or it is byte for byte a
/// release that predates the hash. `skill install` replaces such a copy
/// without --force — refusing it made every upgrade look like a user edit.
pub fn is_pristine(installed: &str) -> bool {
    match front_matter_value(installed, "body_hash:") {
        Some(recorded) => recorded == body_hash(installed),
        None => RELEASED_BEFORE_BODY_HASH.contains(&v5(installed.as_bytes()).as_str()),
    }
}

/// Writes this build's `schema_hash` and `contract_version` into a SKILL.md
/// front matter, replacing whatever was there, then the `body_hash` of the
/// result. Idempotent, and the only writer of those three lines.
///
/// Generation is not a `build.rs`: a build script that writes into `src/`
/// breaks `cargo package`, fights incremental rebuilds, and would race
/// `include_str!`. The asset is checked in, `engram-contract-gen` regenerates
/// it, and a test fails the build when the checked-in bytes drift.
pub fn stamp(body: &str) -> String {
    let hash = schema_hash();
    let version = crate::registry::CONTRACT_VERSION;
    let stamp_lines = format!("  schema_hash: \"{hash}\"\n  contract_version: \"{version}\"\n");
    let mut out = String::with_capacity(body.len() + 160);
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
                out.push_str(&stamp_lines);
                wrote = true;
            }
            in_front_matter = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_front_matter && STAMP_KEYS.iter().any(|k| trimmed.starts_with(k)) {
            continue; // replaced below, at the metadata block
        }
        out.push_str(line);
        out.push('\n');
        if in_front_matter && trimmed == "metadata:" {
            out.push_str(&stamp_lines);
            wrote = true;
        }
    }
    // The body hash covers everything but the stamp lines, so it can be
    // computed over the output and inserted after the fact.
    let body_hash_line = format!("  body_hash: \"{}\"\n", body_hash(&out));
    match out.find(&stamp_lines) {
        Some(at) => out.insert_str(at + stamp_lines.len(), &body_hash_line),
        None => unreachable!("a front matter always receives the stamp"),
    }
    out
}

/// One installed copy's verdict.
///
/// `Modified` exists because `skill install` REFUSES to overwrite a
/// hand-edited SKILL.md: reporting that state as `stale` tells a user to run a
/// command the binary will reject, which is worse advice than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    /// An untouched copy from another build. `installed` is its schema hash,
    /// `None` for releases that predate the stamp.
    Stale {
        installed: Option<String>,
    },
    /// Not a copy engram wrote: somebody edited it, so `skill install` leaves
    /// it alone without --force.
    Modified,
    Missing,
}

impl Freshness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Freshness::Fresh => "fresh",
            Freshness::Stale { .. } => "stale",
            Freshness::Modified => "modified",
            Freshness::Missing => "missing",
        }
    }

    /// Worst-first ordering for the roll-up: missing < modified < stale <
    /// fresh, so one stale copy is never hidden behind three fresh ones.
    fn rank(&self) -> u8 {
        match self {
            Freshness::Missing => 0,
            Freshness::Modified => 1,
            Freshness::Stale { .. } => 2,
            Freshness::Fresh => 3,
        }
    }
}

/// Compares one installed SKILL.md against this binary's contract.
pub fn freshness(installed_body: Option<&str>) -> Freshness {
    let Some(body) = installed_body else {
        return Freshness::Missing;
    };
    let hash = parse_schema_hash(body);
    if hash.as_deref() == Some(schema_hash().as_str()) {
        Freshness::Fresh
    } else if is_pristine(body) {
        Freshness::Stale { installed: hash }
    } else {
        Freshness::Modified
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
    let modified_count = per_file
        .iter()
        .filter(|(_, f)| *f == Freshness::Modified)
        .count();

    let suggestion = if stale_count > 0 {
        Some(format!(
            "run `engram skill install` to refresh {stale_count} stale skill file(s)"
        ))
    } else if modified_count > 0 {
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
                    Freshness::Stale { installed } => installed.clone(),
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

    /// A copy exactly as `skill install` would write it.
    fn installed(hash: &str) -> String {
        let raw =
            format!("---\nname: engram\nmetadata:\n  schema_hash: \"{hash}\"\n---\n\n# engram\n");
        let stamped = stamp(&raw);
        // Keep the requested schema hash; stamp wrote this build's.
        stamped.replace(&schema_hash(), hash)
    }

    #[test]
    fn a_hash_is_read_out_of_the_front_matter() {
        assert_eq!(
            parse_schema_hash(&installed("abc123")),
            Some("abc123".into())
        );
    }

    #[test]
    fn a_file_without_front_matter_is_modified_not_stale() {
        // The usual cause is a user stripping the metadata block, and telling
        // them to reinstall a file they deliberately edited is wrong advice.
        assert_eq!(parse_schema_hash("# engram\n"), None);
        assert_eq!(freshness(Some("# engram\n")), Freshness::Modified);
    }

    #[test]
    fn a_hash_after_the_front_matter_is_not_read() {
        let body = "---\nname: engram\n---\n\nschema_hash: deadbeef\n";
        assert_eq!(parse_schema_hash(body), None);
    }

    #[test]
    fn this_builds_own_hash_is_fresh() {
        let body = stamp("---\nname: engram\nmetadata:\n---\n\n# engram\n");
        assert_eq!(freshness(Some(&body)), Freshness::Fresh);
    }

    #[test]
    fn an_untouched_copy_from_another_build_is_stale_and_names_its_hash() {
        assert_eq!(
            freshness(Some(&installed("deadbeef"))),
            Freshness::Stale {
                installed: Some("deadbeef".into())
            }
        );
    }

    #[test]
    fn an_edited_copy_from_another_build_is_modified() {
        let edited = installed("deadbeef").replace("# engram", "# my engram notes");
        assert_eq!(freshness(Some(&edited)), Freshness::Modified);
    }

    #[test]
    fn stamp_writes_a_body_hash_that_verifies() {
        let once = stamp("---\nname: engram\nmetadata:\n  homepage: \"x\"\n---\n\n# engram\n");
        let recorded = front_matter_value(&once, "body_hash:").expect("body_hash stamped");
        assert_eq!(recorded, body_hash(&once));
        assert!(is_pristine(&once));
        // Editing the body breaks it; editing only a stamp line does not.
        assert!(!is_pristine(&once.replace("# engram", "# engram, mine")));
        assert!(is_pristine(
            &once.replace(&schema_hash(), "00000000000000000000000000000000")
        ));
    }

    #[test]
    fn a_released_copy_without_a_body_hash_is_pristine_only_byte_for_byte() {
        // The allowlist holds whole-file hashes of released bodies; anything
        // else without a recorded body hash is somebody's file.
        assert!(!is_pristine("# some other skill\n"));
        assert_eq!(RELEASED_BEFORE_BODY_HASH.len(), 10);
        // The Rust hash is the one the list was computed with (uuid5 over the
        // file bytes in SKILL_BODY_NAMESPACE, as Python's uuid.uuid5 does).
        assert_eq!(
            v5(b"---\nname: engram\n---\n\n# engram\n"),
            "51ecedca1cdf5251a514e7ae4985ee4f"
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
        let fresh = installed(&schema_hash());
        let v = doctor_json(&[
            ("a".into(), Some(fresh.clone())),
            ("b".into(), Some(fresh.clone())),
            ("c".into(), Some(installed("deadbeef"))),
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
        let stale = "---\nname: engram\nmetadata:\n  schema_hash: \"deadbeef\"\n  body_hash: \"0\"\n---\n\n# engram\n";
        let fresh = stamp(stale);
        assert_eq!(fresh.matches("schema_hash:").count(), 1);
        assert_eq!(fresh.matches("body_hash:").count(), 1);
        assert!(!fresh.contains("deadbeef"));
    }

    #[test]
    fn a_front_matter_with_no_metadata_block_still_gets_stamped() {
        let stamped = stamp("---\nname: engram\n---\n\n# engram\n");
        assert_eq!(freshness(Some(&stamped)), Freshness::Fresh);
        assert!(is_pristine(&stamped));
    }

    #[test]
    fn an_edited_copy_is_told_about_force() {
        let v = doctor_json(&[("a".into(), Some("# engram\n".into()))]);
        assert_eq!(v["status"], "modified");
        assert!(v["suggestion"].as_str().unwrap().contains("--force"));
    }
}
