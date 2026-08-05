//! Shared application context — config + storage + the single-writer core.

use crate::engine::{CompanionTxGuard, Core};
use crate::error::CliError;
use crate::output::OutputFormat;
use engram_storage::{paths, SqliteStore};
use std::path::PathBuf;
use uuid::Uuid;

pub struct AppContext {
    pub format: OutputFormat,
    pub quiet: bool,
    /// Every existing read path still goes through this connection. Writes
    /// move to `core` per the adoption schedule; until a call site moves it
    /// writes here, which is safe under WAL and — because
    /// `/health.writer.writes_total` counts only what went through `Core` —
    /// measurable rather than assumed.
    pub store: SqliteStore,
    pub core: Core,
}

impl AppContext {
    pub fn new(format: OutputFormat, quiet: bool) -> Result<Self, CliError> {
        // Resolved ONCE, so the process cannot straddle two databases when
        // `--store` or `ENGRAM_STORE` is in play.
        let db = paths::db_path();
        Self::open_at(format, quiet, db)
    }

    /// An explicit path, no `paths::` lookup. For in-process contexts over a
    /// TempDir. Not for `engramd --store`: that overrides the db path only,
    /// while vectors, config and cache keep resolving through `paths::`, which
    /// would split the daemon across two stores.
    pub fn with_db(format: OutputFormat, quiet: bool, db: PathBuf) -> Result<Self, CliError> {
        Self::open_at(format, quiet, db)
    }

    fn open_at(format: OutputFormat, quiet: bool, db: PathBuf) -> Result<Self, CliError> {
        let store = SqliteStore::open(&db)?;
        // The wave-4 jobs columns are ensured inside `SqliteStore::open`
        // itself — an invariant every reader depends on (the diary-isolation
        // gate reads `compile_jobs.kind`) cannot live in call-site discipline.
        let core = Core::new(db, crate::engine::default_actor());
        Ok(Self {
            format,
            quiet,
            store,
            core,
        })
    }

    /// A context over a store that has no file behind it — the in-memory
    /// benchmark fixture. Its `core` refuses to write, by name: a second
    /// `:memory:` connection is a second, empty database, and a "successful"
    /// write into one nothing can read is worse than a refusal.
    pub fn fixture(format: OutputFormat, store: SqliteStore) -> Self {
        Self {
            format,
            quiet: true,
            store,
            core: Core::detached("it is an in-memory fixture with no file behind it"),
        }
    }

    /// A context for a job runner: a fresh reader connection to the same
    /// database, JSON output, quiet. Runners READ through this and write only
    /// through `jc.core.write(...)`.
    pub fn for_job(core: &Core, job_id: Uuid) -> Result<Self, CliError> {
        let store = SqliteStore::open(core.db_path())?;
        let job_core = core.clone();
        job_core.set_actor(format!("job:{job_id}"));
        Ok(Self {
            format: OutputFormat::Json,
            quiet: true,
            store,
            core: job_core,
        })
    }

    /// Take a transaction on THIS context's connection, with the nested-write
    /// guard armed.
    ///
    /// `Core::write` from inside one of these is two connections both wanting
    /// `RESERVED` — a lock wait and then `SQLITE_BUSY`, which surfaces as a
    /// slow request rather than as the bug it is. A raw `ctx.store.with_tx`
    /// call bypasses the guard; that is why this wrapper exists and why new
    /// code uses it.
    pub fn with_store_tx<T>(
        &self,
        f: impl FnOnce(&SqliteStore) -> Result<T, CliError>,
    ) -> Result<T, CliError> {
        let _guard = CompanionTxGuard::enter();
        self.store.with_tx_res(f)
    }
}
