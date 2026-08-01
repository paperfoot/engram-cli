//! XDG-compliant filesystem paths, with an isolation override.
//!
//! `ENGRAM_STORE` roots EVERYTHING (db, vectors, config, cache) under one
//! directory. It exists because the alternative already failed twice: a test
//! suite that spawned the binary without an isolated HOME migrated the live
//! store and bricked the installed binary against its own database, and
//! agents have no way to rehearse a destructive workflow without gambling
//! the user's real memory. Tests set it to a TempDir; agents set it to fork
//! a scratch store. An empty value means unset.

use directories::ProjectDirs;
use std::path::PathBuf;

const QUALIFIER: &str = "bio";
const ORG: &str = "199-biotechnologies";
const APP: &str = "engram";

fn dirs() -> ProjectDirs {
    ProjectDirs::from(QUALIFIER, ORG, APP).expect("could not determine user directories")
}

/// The `ENGRAM_STORE` override, if set and non-empty.
fn store_override() -> Option<PathBuf> {
    std::env::var_os("ENGRAM_STORE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn ensure(p: PathBuf) -> PathBuf {
    let _ = std::fs::create_dir_all(&p);
    p
}

/// `~/.local/share/engram/db.sqlite` (Linux) — equivalents on macOS/Windows.
/// Under `ENGRAM_STORE`: `<store>/db.sqlite`.
pub fn db_path() -> PathBuf {
    ensure(data_root()).join("db.sqlite")
}

/// `~/.local/share/engram/vectors/`, or `<store>/vectors/`.
pub fn vector_dir() -> PathBuf {
    ensure(data_root().join("vectors"))
}

/// `~/.config/engram/config.toml`, or `<store>/config.toml`.
///
/// Config moves under the override too: an isolated store with the user's
/// real config would silently spend the user's real keys from inside what
/// the caller believes is a sandbox.
pub fn config_path() -> PathBuf {
    match store_override() {
        Some(root) => ensure(root).join("config.toml"),
        None => ensure(dirs().config_dir().to_path_buf()).join("config.toml"),
    }
}

/// `~/.cache/engram/`, or `<store>/cache/`.
pub fn cache_dir() -> PathBuf {
    match store_override() {
        Some(root) => ensure(root.join("cache")),
        None => ensure(dirs().cache_dir().to_path_buf()),
    }
}

fn data_root() -> PathBuf {
    store_override().unwrap_or_else(|| dirs().data_dir().to_path_buf())
}

// ---------------------------------------------------------------------------
// Peek variants: report the path WITHOUT creating directories. Discovery
// surfaces (`agent-info`, help, clap errors) must be able to answer on a
// machine engram has never written to — informational commands that mutate
// the filesystem are how "run agent-info to check the setup" grows side
// effects nobody asked for.
// ---------------------------------------------------------------------------

pub fn db_path_peek() -> PathBuf {
    data_root().join("db.sqlite")
}

pub fn vector_dir_peek() -> PathBuf {
    data_root().join("vectors")
}

pub fn config_path_peek() -> PathBuf {
    match store_override() {
        Some(root) => root.join("config.toml"),
        None => dirs().config_dir().join("config.toml"),
    }
}

pub fn cache_dir_peek() -> PathBuf {
    match store_override() {
        Some(root) => root.join("cache"),
        None => dirs().cache_dir().to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env vars are process-global, so these tests serialize on a lock rather
    // than racing each other (cargo runs tests threaded). A panic inside one
    // test must not poison the rest into false failures.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn override_roots_everything_under_one_dir() {
        let _g = env_guard();
        let tmp = std::env::temp_dir().join(format!("engram-store-test-{}", std::process::id()));
        std::env::set_var("ENGRAM_STORE", &tmp);
        let (db, vec, cfg, cache) = (
            db_path_peek(),
            vector_dir_peek(),
            config_path_peek(),
            cache_dir_peek(),
        );
        std::env::remove_var("ENGRAM_STORE");
        assert_eq!(db, tmp.join("db.sqlite"));
        assert_eq!(vec, tmp.join("vectors"));
        assert_eq!(cfg, tmp.join("config.toml"));
        assert_eq!(cache, tmp.join("cache"));
    }

    #[test]
    fn empty_override_means_unset() {
        let _g = env_guard();
        std::env::remove_var("ENGRAM_STORE");
        let default = db_path_peek();
        std::env::set_var("ENGRAM_STORE", "");
        let with_empty = db_path_peek();
        std::env::remove_var("ENGRAM_STORE");
        assert_eq!(
            with_empty, default,
            "empty ENGRAM_STORE must behave as unset"
        );
    }

    #[test]
    fn peek_creates_nothing() {
        let _g = env_guard();
        let tmp = std::env::temp_dir().join(format!("engram-peek-test-{}", std::process::id()));
        std::env::set_var("ENGRAM_STORE", &tmp);
        let _ = db_path_peek();
        let _ = vector_dir_peek();
        let _ = config_path_peek();
        let _ = cache_dir_peek();
        std::env::remove_var("ENGRAM_STORE");
        assert!(!tmp.exists(), "peek variants must not create directories");
    }
}
