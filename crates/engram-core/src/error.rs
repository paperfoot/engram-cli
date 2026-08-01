//! Errors for the core crate. I/O errors live in their respective crates.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// The machine-readable error taxonomy every surface maps into (interfaces.md
/// v2 §4, ruling R11). One closed set, because the audit found the opposite:
/// storage/serde/IO all collapsed to "transient, retry" — a wrong id retried a
/// paid call, and `StorageError::NotFound` exited 1 while the manifest
/// promised 3. The `wrong_id_kind` variant exists so a chunk id fed to `edit`
/// gets named for what it is instead of "already deleted?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadInput,
    NotFound,
    WrongIdKind,
    Config,
    Transient,
    RateLimited,
    SqliteBusy,
    ConflictAlreadyResolved,
    MalformedJson,
}

impl ErrorCode {
    /// The process exit code the envelope contract pins to each class.
    /// 3 = fix your arguments (don't retry identically); 2 = fix the setup;
    /// 1 = transient (retry with backoff); 4 = rate limited (wait).
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::BadInput
            | ErrorCode::NotFound
            | ErrorCode::WrongIdKind
            | ErrorCode::MalformedJson
            | ErrorCode::ConflictAlreadyResolved => 3,
            ErrorCode::Config => 2,
            ErrorCode::Transient | ErrorCode::SqliteBusy => 1,
            ErrorCode::RateLimited => 4,
        }
    }

    /// The `error.code` string in the JSON envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadInput => "bad_input",
            ErrorCode::NotFound => "not_found",
            ErrorCode::WrongIdKind => "wrong_id_kind",
            ErrorCode::Config => "config",
            ErrorCode::Transient => "transient",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::SqliteBusy => "sqlite_busy",
            ErrorCode::ConflictAlreadyResolved => "conflict_already_resolved",
            ErrorCode::MalformedJson => "malformed_json",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    #[test]
    fn retryable_classes_are_exactly_transient_and_busy_and_rate_limited() {
        // Exit 1 and 4 are the only "try again later" signals; everything
        // else must tell the caller to change something first.
        for c in [
            ErrorCode::BadInput,
            ErrorCode::NotFound,
            ErrorCode::WrongIdKind,
            ErrorCode::MalformedJson,
            ErrorCode::ConflictAlreadyResolved,
        ] {
            assert_eq!(c.exit_code(), 3, "{}", c.as_str());
        }
        assert_eq!(ErrorCode::Config.exit_code(), 2);
        assert_eq!(ErrorCode::Transient.exit_code(), 1);
        assert_eq!(ErrorCode::SqliteBusy.exit_code(), 1);
        assert_eq!(ErrorCode::RateLimited.exit_code(), 4);
    }
}
