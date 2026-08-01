use crate::error::CliError;

pub mod admin;
pub mod bench;
pub mod budget;
pub mod chunks;
pub mod compile;
pub mod config;
pub mod doctor;
pub mod documents;
pub mod edit;
pub mod entities;
pub mod export;
pub mod facts;
pub mod forget;
pub mod graph;
pub mod import;
pub mod ingest;
pub mod jobs;
pub mod kb;
pub mod memory;
pub mod people;
pub mod recall;
pub mod reindex;
pub mod remember;
pub mod repair;
pub mod research;
pub mod serve;
pub mod skill;
pub mod update;
pub mod usage;
pub mod wiki;

/// Reject a diary name that is legal to READ but never legal to WRITE.
///
/// On the read side `--diary '*'` is the wildcard for "every diary". Passing
/// it to a write stored a diary whose name is literally `*`, which is a trap
/// with no exit: its own scoped recall (`--diary '*'`) means "all diaries", so
/// nothing can ever address it, and because the KB now holds a second diary
/// name the diary-isolation gate marks that KB permanently mixed — every other
/// diary's wiki pages and entity aggregates get withheld forever on account of
/// a diary no one can read. Empty or whitespace-only names are rejected for
/// the same reason: `--diary ''` is not a namespace, it is a typo.
///
/// Exit 3 (bad input), because the caller can fix it by picking a real name.
pub fn validate_diary_for_write(diary: &str) -> Result<(), CliError> {
    if diary.trim().is_empty() {
        return Err(CliError::BadInput(
            "--diary cannot be empty or whitespace — pass a real diary name (e.g. `default`)"
                .into(),
        ));
    }
    if diary.trim() == "*" {
        return Err(CliError::BadInput(
            "--diary '*' is a read-side wildcard meaning EVERY diary; it cannot be written to. \
             Pick a real diary name (e.g. `--diary personal`)."
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_diary_rejects_the_read_wildcard_and_blanks() {
        for bad in ["*", " * ", "", "   ", "\t"] {
            let err = validate_diary_for_write(bad).unwrap_err();
            assert_eq!(err.exit_code(), 3, "{bad:?} must be exit 3");
        }
    }

    #[test]
    fn write_diary_accepts_real_names() {
        for good in ["default", "work", "personal", "a*b"] {
            assert!(validate_diary_for_write(good).is_ok(), "{good:?}");
        }
    }
}
