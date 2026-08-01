//! Storage layer: SQLite is the source of truth, FTS5 provides the lexical index. No external vector DB.
//! vector index, FTS5 lives inside SQLite.

pub mod backup;
pub mod decisions;
pub mod error;
pub mod fts;
pub mod journal;
pub mod migrate_v10;
pub mod paths;
pub mod sqlite;
#[cfg(test)]
pub(crate) mod test_support;
pub mod write_path;

pub use decisions::{DecisionRow, InsertOutcome};
pub use error::StorageError;
pub use journal::{EventDraft, EventRow};
pub use migrate_v10::MigrationV10Report;
pub use sqlite::{
    ChunkExportRow, ChunkRecord, ClaimRecord, DocumentExportRow, DocumentRecord, EntityRecord,
    FactExportRow, KbExportRow, KnowledgeBase, MemoryExportRow, RelationRecord, SourceCitation,
    SqliteStore, StoredChunkEmbedding, WikiPageRecord,
};
pub use write_path::{DerivationRow, RetractOutcome};
