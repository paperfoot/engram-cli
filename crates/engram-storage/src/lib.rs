//! Storage layer: SQLite is the source of truth, FTS5 provides the lexical index. No external vector DB.
//! vector index, FTS5 lives inside SQLite.

pub mod backup;
pub mod decisions;
pub mod error;
pub mod export_v3;
pub mod fts;
pub mod handles;
pub mod jobs;
pub mod journal;
pub mod lessons;
pub mod migrate_v10;
pub mod migrate_v14;
pub mod paths;
pub mod people;
pub mod rebuild;
pub mod sqlite;
#[cfg(test)]
pub(crate) mod test_support;
pub mod write_path;

pub use decisions::{DecisionRow, InsertOutcome};
pub use error::StorageError;
pub use export_v3::{ExportV3, ImportV3Report};
pub use handles::Resolved;
pub use jobs::{
    CompilePressure, JobFilter, JobHeartbeat, JobRow, NewJob, SubmitOutcome, STATUS_ABANDONED,
    STATUS_CANCELLED, STATUS_COMPLETED, STATUS_FAILED, STATUS_PAUSED, STATUS_QUEUED,
    STATUS_RUNNING,
};
pub use journal::{EventDraft, EventRow, Origin};
pub use lessons::{LessonRow, LessonUpsert};
pub use migrate_v10::MigrationV10Report;
pub use rebuild::RebuildReport;
pub use sqlite::{
    ChunkExportRow, ChunkRecord, ClaimRecord, DocumentExportRow, DocumentRecord, EntityRecord,
    FactExportRow, KbExportRow, KnowledgeBase, MemoryExportRow, RelationRecord, SourceCitation,
    SqliteStore, StoredChunkEmbedding, WikiPageRecord,
};
pub use people::PersonRow;
pub use write_path::{DerivationRow, RetractOutcome};
