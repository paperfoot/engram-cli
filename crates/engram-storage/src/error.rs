use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("migration failed: {0}")]
    Migration(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// SQLITE_BUSY / SQLITE_LOCKED, hoisted out of the rusqlite wrapping so a
    /// caller can retry without string-matching an error message.
    #[error("database is busy: {0}")]
    Busy(String),

    /// A stored row (or event payload) does not parse. Distinct from `Sqlite`
    /// on purpose: the CLI maps it to `malformed_json`, and a caller that sees
    /// it knows the store needs repairing rather than retrying.
    #[error("malformed row: {0}")]
    MalformedRow(String),
}

impl StorageError {
    /// True for a busy/locked database under ANY wrapping, so the CLI's
    /// `ErrorCode` mapping is a total function over what it can observe
    /// rather than a `to_string().contains(...)` guess.
    pub fn is_busy(&self) -> bool {
        match self {
            StorageError::Busy(_) => true,
            StorageError::Sqlite(e) => is_busy_sqlite(e),
            _ => false,
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, StorageError::NotFound(_))
    }
}

fn is_busy_sqlite(e: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    match e {
        rusqlite::Error::SqliteFailure(f, _) => {
            matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        }
        _ => false,
    }
}

/// Translate a rusqlite failure at a transaction boundary. Everything else
/// keeps its original variant: only the boundary knows the whole statement
/// failed to start, which is what "busy" actually means to a caller.
pub(crate) fn tx_error(e: rusqlite::Error) -> StorageError {
    if is_busy_sqlite(&e) {
        return StorageError::Busy(e.to_string());
    }
    StorageError::Sqlite(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_is_recognised_under_either_wrapping() {
        assert!(StorageError::Busy("x".into()).is_busy());
        let ffi = rusqlite::ffi::Error::new(5); // SQLITE_BUSY
        assert!(StorageError::Sqlite(rusqlite::Error::SqliteFailure(ffi, None)).is_busy());
        assert!(!StorageError::NotFound("x".into()).is_busy());
        assert!(StorageError::NotFound("x".into()).is_not_found());
    }
}
