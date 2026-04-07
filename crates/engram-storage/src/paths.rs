//! XDG-compliant filesystem paths.

use directories::ProjectDirs;
use std::path::PathBuf;

const QUALIFIER: &str = "bio";
const ORG: &str = "199-biotechnologies";
const APP: &str = "engram";

fn dirs() -> ProjectDirs {
    ProjectDirs::from(QUALIFIER, ORG, APP)
        .expect("could not determine user directories")
}

/// `~/.local/share/engram/db.sqlite` (Linux) — equivalents on macOS/Windows.
pub fn db_path() -> PathBuf {
    let d = dirs();
    let p = d.data_dir().to_path_buf();
    let _ = std::fs::create_dir_all(&p);
    p.join("db.sqlite")
}

/// `~/.local/share/engram/vectors/`
pub fn vector_dir() -> PathBuf {
    let d = dirs();
    let p = d.data_dir().to_path_buf().join("vectors");
    let _ = std::fs::create_dir_all(&p);
    p
}

/// `~/.config/engram/config.toml`
pub fn config_path() -> PathBuf {
    let d = dirs();
    let p = d.config_dir().to_path_buf();
    let _ = std::fs::create_dir_all(&p);
    p.join("config.toml")
}

/// `~/.cache/engram/`
pub fn cache_dir() -> PathBuf {
    let d = dirs();
    let p = d.cache_dir().to_path_buf();
    let _ = std::fs::create_dir_all(&p);
    p
}
