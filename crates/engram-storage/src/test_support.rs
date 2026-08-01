//! Two belts against a test touching the developer's real store.
//!
//! Belt one is the explicit path: every helper here opens
//! `<TempDir>/db.sqlite`. Belt two is `ENGRAM_STORE`, set to the same
//! TempDir for the test's lifetime, so a helper that reaches for
//! `paths::db_path()` behind our back still lands inside the sandbox.
//! Env vars are process-global and cargo runs tests threaded, so the guard
//! holds a lock — poison-tolerant, for the same reason `paths.rs` is: one
//! panicking test must not turn every later test red for the wrong reason.

use crate::sqlite::SqliteStore;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The ONE process-global env lock for this crate's tests. `paths::tests`
/// mutates `ENGRAM_STORE` too, and two locks over one process-global variable
/// is the same as no lock at all.
pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Holds the env lock and restores `ENGRAM_STORE` on drop.
pub struct StoreGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl Drop for StoreGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(v) => std::env::set_var("ENGRAM_STORE", v),
            None => std::env::remove_var("ENGRAM_STORE"),
        }
    }
}

/// A TempDir with `ENGRAM_STORE` pointed at it. Keep BOTH values alive: the
/// TempDir deletes itself when dropped, the guard un-sets the env var.
pub fn store_guard() -> (TempDir, StoreGuard) {
    let lock = env_lock();
    let tmp = TempDir::new().expect("tempdir");
    let previous = std::env::var_os("ENGRAM_STORE");
    std::env::set_var("ENGRAM_STORE", tmp.path());
    (
        tmp,
        StoreGuard {
            _lock: lock,
            previous,
        },
    )
}

/// A SECOND store inside an existing guard's TempDir. `store_guard` holds a
/// non-reentrant lock, so a test that needs two stores (export/import) must
/// take the guard once and open the second store through this — calling
/// `temp_store` twice on one thread deadlocks.
pub fn extra_store(tmp: &TempDir, name: &str) -> SqliteStore {
    SqliteStore::open(tmp.path().join(format!("{name}.sqlite"))).expect("open extra store")
}

/// An opened file store inside a fresh guard. A FILE store, not `:memory:`:
/// `VACUUM INTO` needs a directory and the "never back up an in-memory store"
/// rule is only testable against both.
pub fn temp_store() -> (TempDir, StoreGuard, SqliteStore) {
    let (tmp, guard) = store_guard();
    let store = SqliteStore::open(tmp.path().join("db.sqlite")).expect("open temp store");
    (tmp, guard, store)
}
