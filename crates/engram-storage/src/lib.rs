//! Storage layer: SQLite is the source of truth, FTS5 provides the lexical index. No external vector DB.
//! vector index, FTS5 lives inside SQLite.

pub mod error;
pub mod fts;
pub mod paths;
pub mod sqlite;

pub use error::StorageError;
pub use sqlite::{
    ChunkRecord, ClaimRecord, DocumentRecord, EntityRecord, KnowledgeBase, RelationRecord,
    SourceCitation, SqliteStore, StoredChunkEmbedding, WikiPageRecord,
};
