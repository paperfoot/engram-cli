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
    let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

/// An opened file store inside a fresh guard. A FILE store, not `:memory:`:
/// `VACUUM INTO` needs a directory and the "never back up an in-memory store"
/// rule is only testable against both.
pub fn temp_store() -> (TempDir, StoreGuard, SqliteStore) {
    let (tmp, guard) = store_guard();
    let store = SqliteStore::open(tmp.path().join("db.sqlite")).expect("open temp store");
    (tmp, guard, store)
}
