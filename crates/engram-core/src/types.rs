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
/// Facts are bi-temporal: `created_at`/`superseded_at` record when the system
/// learned things (transaction time), while `valid_from`/`valid_until` record
/// when the fact was true in the world (event time). Old facts are never
/// deleted — superseding closes their validity window and the history stays
/// queryable.
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
    /// Semantic category. Profile slots get zero-LLM conflict policies.
    #[serde(default)]
    pub fact_type: FactType,
    /// Event time: when this fact became true in the world (if known).
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    /// Event time: when this fact stopped being true (set on supersession).
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    /// How a supersession/conflict involving this fact was decided, namespaced
    /// by WHO decided: exact_match | slot_lww | semantic_llm | duplicate (the
    /// write path), caller (an explicit `facts resolve`), triage_* (the
    /// backlog sweep), compile_* (a compiled fact — `compile_recency`,
    /// `compile_stale`, `compile_confidence`).
    #[serde(default)]
    pub resolution: Option<String>,
    /// Provenance class of the fact — feeds trust weighting.
    #[serde(default)]
    pub source_kind: SourceKind,
    /// Times this exact fact was re-asserted (dedup hits). Feeds trust.
    #[serde(default)]
    pub confirmations: u32,
    /// The successor fact (or the event) that closed this interval.
    #[serde(default)]
    pub closed_by: Option<Uuid>,
    /// `CloseReason::as_str()` — superseded | stopped | corrected |
    /// retracted_source. NULL while the interval is open.
    #[serde(default)]
    pub close_reason: Option<String>,
    /// `CloseAuthority::as_str()` — explicit_end | caller | replayed_caller |
    /// successor_interval | heuristic. WHO closed the interval is the whole
    /// point: the safety audit asks "which safety facts did a machine retire?"
    /// and cannot answer it from `resolution`, which mixes rule names and
    /// actors. The landed spelling is `successor_interval`, never `successor`.
    #[serde(default)]
    pub close_authority: Option<String>,
    /// Knowledge base the fact lives in. The column has always existed; the
    /// struct did not carry it, so `insert_fact` re-derived it with a subquery
    /// over `memories` — which silently re-homes every fact whose source
    /// memory later moved between KBs, and makes a journal replay unable to
    /// reproduce the original row. `""` on a row deserialized from a pre-v10
    /// export.
    #[serde(default)]
    pub kb: String,
}

/// Semantic category of a fact. Closed taxonomy so conflict policies can be
/// keyed off it without semantics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactType {
    /// Single-valued attribute of an entity (employer, city, age).
    ProfileSlot,
    Preference,
    Relationship,
    Event,
    #[default]
    Assertion,
    Procedural,
}

impl FactType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FactType::ProfileSlot => "profile_slot",
            FactType::Preference => "preference",
            FactType::Relationship => "relationship",
            FactType::Event => "event",
            FactType::Assertion => "assertion",
            FactType::Procedural => "procedural",
        }
    }

    pub fn parse(s: &str) -> FactType {
        match s {
            "profile_slot" => FactType::ProfileSlot,
            "preference" => FactType::Preference,
            "relationship" => FactType::Relationship,
            "event" => FactType::Event,
            "procedural" => FactType::Procedural,
            _ => FactType::Assertion,
        }
    }
}

/// Where a fact came from. Ordered roughly by trustworthiness; used as a
/// prior when adjudicating conflicts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    UserStated,
    UserConfirmed,
    #[default]
    AgentExtracted,
    CompileLlm,
    Import,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::UserStated => "user_stated",
            SourceKind::UserConfirmed => "user_confirmed",
            SourceKind::AgentExtracted => "agent_extracted",
            SourceKind::CompileLlm => "compile_llm",
            SourceKind::Import => "import",
        }
    }

    pub fn parse(s: &str) -> SourceKind {
        match s {
            "user_stated" => SourceKind::UserStated,
            "user_confirmed" => SourceKind::UserConfirmed,
            "compile_llm" => SourceKind::CompileLlm,
            "import" => SourceKind::Import,
            _ => SourceKind::AgentExtracted,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSource {
    Dense,
    Lexical,
    Claim,
    Wiki,
    Entity,
    Graph,
    Reranker,
}
