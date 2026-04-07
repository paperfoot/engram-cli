//! Shared application context — config + storage + clients.

use crate::error::CliError;
use crate::output::OutputFormat;
use engram_storage::{paths, SqliteStore};

pub struct AppContext {
    pub format: OutputFormat,
    pub quiet: bool,
    pub store: SqliteStore,
}

impl AppContext {
    pub fn new(format: OutputFormat, quiet: bool) -> Result<Self, CliError> {
        let store = SqliteStore::open(paths::db_path())?;
        Ok(Self { format, quiet, store })
    }
}
