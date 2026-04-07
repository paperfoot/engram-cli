//! Storage layer: SQLite is the source of truth, LanceDB is a rebuildable
//! vector index, FTS5 lives inside SQLite.

pub mod error;
pub mod fts;
pub mod paths;
pub mod sqlite;

pub use error::StorageError;
pub use sqlite::SqliteStore;
