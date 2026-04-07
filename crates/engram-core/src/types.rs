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
    pub importance: u8, // 0..=10
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
    Paper { doi: Option<String>, title: String, section: Option<String> },
    Conversation { thread: String, turn: u32 },
    Repo { repo: String, path: String, line_start: Option<u32> },
    General,
}

/// A retrievable text fragment with its embedding pointer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: ChunkId,
    pub memory_id: MemoryId,
    pub content: String,
    pub position: u32, // ordinal within the parent memory
    pub section: Option<String>, // for papers: "Methods > Cell Culture"
    pub token_count: Option<u32>,
    pub embedding_id: Option<String>, // pointer into LanceDB
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub kind: EntityKind,
    pub mention_count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// L0: identity / system prompt (~50 tokens).
    Identity,
    /// L1: critical facts, AAAK-encoded (~120 tokens).
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

/// A scored result returned by retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: Score,
    pub source: RetrievalSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSource {
    Dense,
    Lexical,
    Entity,
    Graph,
    Reranker,
}
