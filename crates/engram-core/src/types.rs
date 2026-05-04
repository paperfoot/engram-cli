//! Core domain types — shared across the workspace.

use chrono::{DateTime, Utc};
use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type MemoryId = Uuid;
pub type ChunkId = Uuid;
pub type EntityId = Uuid;

/// A score, kept as an `OrderedFloat` so memories can be sorted deterministically.
pub type Score = OrderedFloat<f32>;

/// A single memory record. Verbatim content is always preserved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    pub id: MemoryId,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub event_time: Option<DateTime<Utc>>,
    pub importance: u8,       // 0..=10
    pub emotional_weight: i8, // -5..=5
    pub access_count: u32,
    pub last_accessed: Option<DateTime<Utc>>,
    pub stability: f32, // Ebbinghaus stability parameter
    pub source: MemorySource,
    pub diary: String, // Specialist agent namespace; "default" if none
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    Manual,
    Paper {
        doi: Option<String>,
        title: String,
        section: Option<String>,
    },
    Conversation {
        thread: String,
        turn: u32,
    },
    Repo {
        repo: String,
        path: String,
        line_start: Option<u32>,
    },
    General,
}

/// A retrievable text fragment with its embedding pointer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: ChunkId,
    pub memory_id: MemoryId,
    pub content: String,
    pub position: u32,           // ordinal within the parent memory
    pub section: Option<String>, // for papers: "Methods > Cell Culture"
    pub token_count: Option<u32>,
    pub embedding_id: Option<String>, // reserved for external index
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub kind: EntityKind,
    pub mention_count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Organization,
    Place,
    Gene,
    Protein,
    Compound,
    Pathway,
    Disease,
    Concept,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: EntityId,
    pub to: EntityId,
    pub kind: EdgeKind,
    pub weight: f32,
    pub provenance: Vec<MemoryId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Deterministic: appeared together in the same chunk.
    CoOccurrence,
    /// Deterministic: A cites B (papers).
    Cites,
    /// Deterministic: B is contained in A (section -> chunk).
    Contains,
    /// Deterministic: alias / synonym mapping.
    Synonym,
    /// LLM-extracted relation (optional, lower trust).
    LlmRelation { label: String },
}

/// Memory layers (L0–L3) — tiered context loading.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// L0: identity / system prompt (~50 tokens).
    Identity,
    /// L1: critical facts, condensed (~120 tokens).
    Critical,
    /// L2: topic-specific context loaded on demand.
    Topic,
    /// L3: deep semantic search across everything.
    Deep,
}

impl Layer {
    pub fn default_token_budget(self) -> usize {
        match self {
            Layer::Identity => 50,
            Layer::Critical => 120,
            Layer::Topic => 800,
            Layer::Deep => 4096,
        }
    }
}

/// An atomic factual claim extracted from a memory's content for
/// contradiction detection. Stored independently from chunks so we can answer
/// "what does engram currently believe about subject X?" without re-retrieving
/// raw text.
///
/// Lean MVP design: no bi-temporal windows, no entity normalization beyond
/// lowercase, no quality gates. The minimum that lets us catch contradictions
/// at write time and supersede the old value when a new one disagrees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fact {
    pub id: Uuid,
    pub source_memory_id: MemoryId,
    /// Display form of the subject ("Ada Example").
    pub subject: String,
    /// Lowercased subject for lookup ("ada example").
    pub subject_norm: String,
    /// Snake_case predicate ("works_at", "lives_in", "prefers", "graduated_from").
    pub predicate: String,
    pub object: String,
    pub object_norm: String,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    /// If non-None, this fact has been superseded by another fact (by id).
    /// Old facts are NOT deleted — they remain queryable for history.
    pub superseded_by: Option<Uuid>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub diary: String,
}

/// A scored result returned by retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: Score,
    pub source: RetrievalSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSource {
    Dense,
    Lexical,
    Entity,
    Graph,
    Reranker,
}
