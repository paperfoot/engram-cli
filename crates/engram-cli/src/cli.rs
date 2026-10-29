//! Clap argument definitions.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "engram",
    version,
    about = "Agent-native memory engine — scientific knowledge + personal memory",
    long_about = None,
)]
pub struct Cli {
    /// Force JSON output (auto-enabled when stdout is piped).
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress progress and non-essential stderr output.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Root every path (db, vectors, config, cache) at DIR. Overrides ENGRAM_STORE.
    #[arg(long, global = true, value_name = "DIR")]
    pub store: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print machine-readable capability manifest.
    #[command(visible_alias = "info")]
    AgentInfo {
        /// Print one command's JSON Schema instead of the whole manifest, e.g.
        /// `--schema 'facts resolve'`. Aliases resolve; an unknown name lists
        /// the valid ones.
        #[arg(long, value_name = "COMMAND", conflicts_with_all = ["command", "index"])]
        schema: Option<String>,
        /// Describe one command, or every command in a group (`facts`): the
        /// global invariants, its contract entry, its prose and the decision
        /// rules that govern it. Aliases resolve.
        #[arg(long = "command", value_name = "PATH", conflicts_with = "index")]
        command: Option<String>,
        /// List every command path with a one-line summary, plus the global
        /// invariants.
        #[arg(long)]
        index: bool,
    },

    /// Store a new memory.
    Remember {
        /// The content to remember.
        content: String,

        /// Importance 0..=10.
        #[arg(long, default_value_t = 5)]
        importance: u8,

        /// Tag (repeatable).
        #[arg(long = "tag", short = 't')]
        tag: Vec<String>,

        /// Specialist agent diary namespace.
        #[arg(long, default_value = "default")]
        diary: String,

        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,

        /// Skip LLM-based fact extraction + contradiction detection.
        /// Extraction is ON by default whenever an OpenRouter key resolves;
        /// use this for bulk/low-value writes you don't want facts from.
        /// `--no-facts` is accepted as the older spelling.
        #[arg(long, visible_alias = "no-facts")]
        no_extract_facts: bool,

        /// Deprecated: fact extraction is now the default, so this flag is
        /// redundant on its own. It still forces extraction alongside
        /// --facts-json, exactly as it did before. Opt out with
        /// --no-extract-facts.
        #[arg(long)]
        extract_facts: bool,

        /// When the content actually happened: RFC3339 (2026-03-01T09:30:00Z)
        /// or YYYY-MM-DD (midnight UTC). Drives `recall --since/--until` and
        /// seeds the event time of facts extracted from it. Defaults to now.
        #[arg(long)]
        event_time: Option<String>,

        /// When this memory STOPS being true: RFC3339, or YYYY-MM-DD meaning
        /// the END of that UTC day ("--valid-until 2026-08-11" holds through
        /// the 11th). After that instant, default `recall` withholds the
        /// memory and reports it under `metadata.expiry`;
        /// `recall --include-expired` still reaches it. Use for content with
        /// a natural end date — an event window, an offer, a deadline.
        #[arg(long)]
        valid_until: Option<String>,

        /// Store even if identical content already exists in this kb/diary.
        /// By default an exact duplicate is a no-op (no second embedding).
        #[arg(long)]
        allow_duplicate: bool,

        /// Conflict-resolution authority: `assisted` (default) queues
        /// uncertain contradictions for YOU to resolve via `facts resolve`
        /// (you have the conversation context); `full` lets engram settle
        /// routine ones with its JEV judge and a code rule (safety pairs
        /// still queue); `strict` never auto-supersedes.
        #[arg(long, default_value = "assisted")]
        autonomy: String,

        /// Store facts directly from a JSON array (same shape as extraction
        /// output: [{"subject","predicate","object","confidence","fact_type",
        /// "valid_from"}]). Skips the extraction LLM call entirely — use this
        /// when you already know the triple. The supplied facts still run the
        /// full write path: canonicalization, dedup, slot policies, and
        /// contradiction detection.
        #[arg(long)]
        facts_json: Option<String>,

        /// Embed with the deterministic offline stub instead of Gemini.
        /// Without this flag a write with no Gemini key is a config error
        /// (exit 2), never a silent 64-dim stub — a store holding both kinds
        /// of vector is what makes `recall` fail with "embedding metadata
        /// mismatch" later. Affects EMBEDDINGS only: fact extraction reports
        /// its own outcome in `fact_extraction`.
        #[arg(long)]
        offline: bool,
    },

    /// Retrieve relevant memories via hybrid search (dense + lexical + rerank).
    #[command(visible_aliases = ["search"])]
    Recall {
        /// Search query.
        query: String,

        /// Number of results. `--limit` is accepted as an alias: agents reach
        /// for the enumerating word, and a rejected flag on the most-called
        /// command is a worse teacher than an alias.
        #[arg(long, default_value_t = 10, visible_alias = "limit")]
        top_k: usize,

        /// Memory layer to target (identity|critical|topic|deep).
        #[arg(long, default_value = "topic")]
        layer: String,

        /// Retrieval mode: evidence|raw|wiki|explore|agent.
        #[arg(long, default_value = "evidence")]
        mode: String,

        /// Retrieval profile: cloud_quality|fast|offline.
        #[arg(long, default_value = "cloud_quality")]
        profile: String,

        /// Knowledge base/domain namespace to search. Omit to search EVERY KB
        /// (or the KB pinned by `retrieval.default_kb`). Pass "*" for all.
        #[arg(long)]
        kb: Option<String>,

        /// Search all KBs, ignoring --kb and retrieval.default_kb.
        #[arg(long)]
        all_kbs: bool,

        /// Specialist agent diary namespace, or "*" for all.
        #[arg(long, default_value = "default")]
        diary: String,

        /// Candidate count sent to rerank.
        #[arg(long)]
        rerank_top_n: Option<usize>,

        /// Graph expansion hops. Explore mode may use 2 hops.
        #[arg(long, default_value_t = 1)]
        graph_hops: u8,

        /// Permit comparing chunks embedded with different metadata.
        #[arg(long)]
        allow_mixed_embeddings: bool,

        /// Earliest event_time to include: RFC3339 (2026-06-01T09:30:00Z) or
        /// YYYY-MM-DD, which means the START of that UTC day. Same spellings
        /// `remember --event-time` accepts. NOTE: a temporal window restricts
        /// results to raw memory chunks — derived sources (claims, wiki pages,
        /// entities, relations) carry no event time of their own and are
        /// withheld, which `metadata.temporal_scope` reports.
        #[arg(long)]
        since: Option<String>,

        /// Latest event_time to include: RFC3339 or YYYY-MM-DD, which means
        /// the END of that UTC day, so `--since D --until D` selects all of
        /// day D.
        #[arg(long)]
        until: Option<String>,

        /// Search memories whose `valid_until` has already passed. By default
        /// expired memories are withheld from every retrieval path and the
        /// count is reported under `metadata.expiry` — never dropped silently.
        #[arg(long)]
        include_expired: bool,
    },

    /// Ingest a file or directory into the memory store.
    #[command(after_long_help = "\
Tips:
  - Single files are safe: `engram ingest ./paper.pdf --kb papers --mode papers`.
  - Directory ingest is scoped by default. Preview first with `--dry-run`.
  - Use `--include`, `--exclude`, and `--max-files` for curated batches.
  - Use `--all` only when you intentionally want the full recursive directory.
  - Defaults can be configured with ENGRAM_INGEST_REQUIRE_SCOPE, ENGRAM_INGEST_INCLUDE, ENGRAM_INGEST_EXCLUDE, and ENGRAM_INGEST_MAX_FILES.

Examples:
  engram ingest ./paper.pdf --kb ageing-biology --mode papers --compile evidence
  engram ingest ./papers --kb ageing-biology --mode papers --dry-run --include '*.pdf'
  engram ingest ./papers --kb ageing-biology --mode papers --include '*.pdf' --max-files 20 --compile evidence
  engram ingest ./papers --kb ageing-biology --mode papers --all --compile evidence
")]
    Ingest {
        /// Path to file or directory.
        path: PathBuf,

        /// Mining mode: papers|takeaways|conversations|repos|general|auto.
        #[arg(long, default_value = "auto")]
        mode: String,

        /// Specialist agent diary namespace.
        #[arg(long, default_value = "default")]
        diary: String,

        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,

        /// Optional compiler mode after ingest: none|evidence.
        #[arg(long, default_value = "none")]
        compile: String,

        /// Preview matched files without storing anything.
        #[arg(long)]
        dry_run: bool,

        /// Glob-like pattern to include when PATH is a directory. Repeatable.
        /// Matches file name and relative path. Supports '*' and '?'.
        #[arg(long = "include")]
        include: Vec<String>,

        /// Glob-like pattern to exclude when PATH is a directory. Repeatable.
        /// Matches file name and relative path. Supports '*' and '?'.
        #[arg(long = "exclude")]
        exclude: Vec<String>,

        /// Refuse to ingest more than this many matched files.
        #[arg(long)]
        max_files: Option<usize>,

        /// Confirm full recursive directory ingest when no filters are provided.
        #[arg(long)]
        all: bool,

        /// Embed with the deterministic offline stub instead of Gemini.
        /// Without this flag an ingest with no Gemini key is a config error
        /// (exit 2), never a silent 64-dim stub. Same flag as
        /// `remember --offline`.
        #[arg(long)]
        offline: bool,
    },

    /// Inspect and manage ingested source documents.
    #[command(subcommand)]
    Documents(DocumentsCommand),

    /// Store maintenance that is not part of ordinary use.
    #[command(subcommand)]
    Admin(AdminCommand),

    /// Manage knowledge bases/domains.
    #[command(subcommand)]
    Kb(KbCommand),

    /// Compile a KB into claims, entities, relations, takeaways, and wiki pages.
    Compile {
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Compile every document/chunk in the KB.
        #[arg(long)]
        all: bool,
        /// Add LLM-assisted extraction and synthesis on top of deterministic compilation.
        #[arg(long)]
        llm: bool,
        /// OpenRouter model for extraction. Defaults to Gemini 3.5 Flash-Lite.
        #[arg(long)]
        extraction_model: Option<String>,
        /// OpenRouter model for synthesis/wiki overview. Defaults to Gemini 3.6 Flash.
        #[arg(long)]
        synthesis_model: Option<String>,
        /// Limit LLM extraction chunks to control cost. Deterministic compile still scans all chunks.
        #[arg(long)]
        max_llm_chunks: Option<usize>,
        /// Submit the compile as a background job and return its handle immediately.
        #[arg(long = "async")]
        r#async: bool,
    },

    /// Run an evidence-first research retrieval pass over a KB.
    Research {
        /// Research question.
        query: String,
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Search all KBs instead of only --kb.
        #[arg(long)]
        all_kbs: bool,
        /// Specialist agent diary namespace, or "*" for all.
        #[arg(long, default_value = "default")]
        diary: String,
        /// Number of evidence leads. `--limit` is accepted as an alias.
        #[arg(long, default_value_t = 12, visible_alias = "limit")]
        top_k: usize,
        /// Retrieval profile: cloud_quality|fast|offline.
        #[arg(long, default_value = "cloud_quality")]
        profile: String,
        /// Permit comparing chunks embedded with different metadata.
        #[arg(long)]
        allow_mixed_embeddings: bool,
        /// Search memories whose `valid_until` has already passed. Same flag
        /// as `recall --include-expired`.
        #[arg(long)]
        include_expired: bool,
    },

    /// Inspect compile/daemon jobs.
    #[command(subcommand)]
    Jobs(JobsCommand),

    /// What is TRUE NOW about one subject: open intervals only, grouped by
    /// relation, with provenance and the open questions still queued.
    ///
    /// `lessons` and `reflect` are their own top-level commands, not
    /// subcommands here, so no subject name is a reserved word.
    State {
        /// The subject of the facts — a name, not a handle.
        subject: String,
        /// Knowledge base/domain namespace, or "*" for all.
        #[arg(long, default_value = "*")]
        kb: String,
        /// Specialist agent diary namespace, or "*" for all.
        #[arg(long, default_value = "*")]
        diary: String,
        /// Narrow to one relation (raw or canonical).
        #[arg(long)]
        relation: Option<String>,
        /// State as it was at this instant (RFC3339 or YYYY-MM-DD).
        #[arg(long)]
        as_of: Option<String>,
        /// Attach the closed intervals under each value's `history`.
        #[arg(long)]
        include_closed: bool,
        /// Max open values per page.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Opaque page token from a previous response.
        #[arg(long)]
        cursor: Option<String>,
    },

    /// Procedural memory: what the WORK taught us, not what is true.
    #[command(subcommand)]
    Lessons(LessonsCommand),

    /// Propose lessons from the decision journal and your corrections.
    Reflect {
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Scan events at or after this instant (RFC3339 or YYYY-MM-DD).
        /// Without it, resume from the last run's high-water mark.
        #[arg(long)]
        since: Option<String>,
        /// Max journal events to scan in one pass.
        #[arg(long, default_value_t = 2000)]
        window: usize,
        /// Print the candidates and write nothing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Re-embed chunks with the active embedding model/profile.
    Reindex {
        /// Knowledge base/domain namespace.
        #[arg(long)]
        kb: Option<String>,
        /// Reindex all KBs.
        #[arg(long)]
        all: bool,
        /// Re-embed with the deterministic offline stub instead of Gemini.
        /// Without a key this is required: keyless reindex used to silently
        /// replace a cloud index with stub vectors.
        #[arg(long)]
        offline: bool,
    },

    /// Inspect generated wiki pages.
    Wiki {
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Optional page path such as index.md or entities/rapamycin.md.
        path: Option<String>,
    },

    /// Inspect the SQLite graph layer.
    #[command(subcommand)]
    Graph(GraphCommand),

    /// Diagnose configuration, keys, schema, embeddings, and daemon port.
    Doctor {
        /// Also require OpenRouter for compiler checks.
        #[arg(long)]
        compiler: bool,
        /// Include store lifecycle checks for stale derived memory artifacts.
        #[arg(long)]
        integrity: bool,
    },

    /// Check and repair store lifecycle invariants.
    Repair {
        /// Show what would be repaired without mutating the store.
        #[arg(long)]
        dry_run: bool,
        /// Confirm mutating repair actions.
        #[arg(long)]
        confirm: bool,
    },

    /// Simpler memory namespace aliases for agent workflows.
    #[command(subcommand)]
    Memory(MemoryCommand),

    /// Run the local engram daemon/API.
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8768)]
        port: u16,
        /// Auth token required when binding to non-local addresses.
        #[arg(long)]
        token: Option<String>,
    },

    /// Summarize recorded Gemini/Cohere/API usage.
    Usage {
        /// Optional KB/domain filter.
        #[arg(long)]
        kb: Option<String>,
        /// Optional RFC3339 lower-bound timestamp.
        #[arg(long)]
        since: Option<String>,
    },

    /// Configure and inspect local API usage budgets.
    #[command(subcommand)]
    Budget(BudgetCommand),

    /// Soft-delete a memory by id. Destructive — requires --confirm.
    #[command(visible_aliases = ["delete", "rm"])]
    Forget {
        /// Memory id (UUID).
        id: String,
        /// Confirm destructive operation.
        #[arg(long)]
        confirm: bool,
    },

    /// Update a memory's content or importance.
    Edit {
        /// Memory id (UUID).
        id: String,
        /// New content.
        #[arg(long)]
        content: Option<String>,
        /// New importance (0..=10).
        #[arg(long)]
        importance: Option<u8>,
        /// Re-embed the new content with the deterministic offline stub
        /// instead of Gemini. Without it, a `--content` edit with no Gemini key
        /// is a config error (exit 2). An `--importance`-only edit never
        /// embeds and needs no key.
        #[arg(long)]
        offline: bool,
    },

    /// Browse extracted entities.
    #[command(subcommand)]
    Entities(EntitiesCommand),

    /// Explore your network: who you know, and how they are connected.
    #[command(subcommand, visible_aliases = ["person"])]
    People(PeopleCommand),

    /// Browse atomic (subject, predicate, object) facts and contradictions.
    #[command(subcommand)]
    Facts(FactsCommand),

    /// Export memories as JSON.
    Export {
        /// Output format (json).
        #[arg(long, default_value = "json")]
        format: String,

        /// Knowledge base/domain namespace.
        #[arg(long)]
        kb: Option<String>,
    },

    /// Import memories from an export file.
    Import {
        /// Path to export JSON.
        file: std::path::PathBuf,
    },

    /// Run a benchmark suite.
    Bench {
        /// Benchmark name: mini | mini-fts | personal | scientific-mini | longmemeval | longmemeval-qa | locomo-plus | mab | litqa2 | beir | biogen
        #[arg(default_value = "mini")]
        suite: String,

        /// Retrieval profile for the `personal` suite: offline (deterministic
        /// hash embedder + passthrough reranker, no network) | cloud_quality |
        /// fast. Ignored by the other suites.
        #[arg(long, default_value = "offline")]
        profile: String,

        /// MemoryAgentBench split: accurate_retrieval | test_time_learning | long_range_understanding | conflict_resolution.
        #[arg(long, default_value = "accurate_retrieval")]
        mab_split: String,

        /// Download dataset if missing.
        #[arg(long)]
        download: bool,

        /// Limit the number of questions (LongMemEval / LoCoMo / LitQA2).
        #[arg(long)]
        limit: Option<usize>,

        /// Answerer model (OpenRouter slug, default anthropic/claude-opus-5).
        #[arg(long, default_value = "anthropic/claude-opus-5")]
        answerer: String,

        /// Judge model (OpenRouter slug, default google/gemini-3.1-pro-preview).
        /// Kept pinned: changing the GRADER invalidates comparison with recorded runs.
        #[arg(long, default_value = "google/gemini-3.1-pro-preview")]
        judge: String,

        /// Compute RAGAS metrics (4 extra LLM calls per question).
        #[arg(long)]
        ragas: bool,

        /// Top-k chunks to pass to the answerer / retriever. 0 = use the
        /// per-suite default (5 for the QA suites, 30 for litqa2 / beir).
        #[arg(long, default_value_t = 0)]
        top_k: usize,

        /// Save the report JSON to this path (in addition to stdout).
        #[arg(long)]
        save: Option<std::path::PathBuf>,

        /// LitQA2 retrieval mode: closed-book | fulltext. closed-book needs only
        /// an answerer API; fulltext additionally retrieves over --kb/--corpus.
        #[arg(long, default_value = "closed-book")]
        mode: String,

        /// Knowledge base to build the retrieval corpus from (LitQA2 fulltext / BioGen).
        #[arg(long)]
        kb: Option<String>,

        /// Corpus file (JSON/JSONL, doc-id -> text) to retrieve over instead of a KB.
        #[arg(long)]
        corpus: Option<std::path::PathBuf>,

        /// BEIR dataset: scifact | nfcorpus | trec-covid | scidocs.
        #[arg(long, default_value = "scifact")]
        dataset: String,

        /// BEIR reranking: on | off. When on, the hybrid row is reranked with Cohere.
        #[arg(long, default_value = "off")]
        rerank: String,

        /// TREC BioGen edition: 2024 | 2025.
        #[arg(long, default_value = "2024")]
        year: String,
    },

    /// Manage engram configuration.
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Manage the agent skill package.
    #[command(subcommand)]
    Skill(SkillCommand),

    /// Self-update from GitHub releases.
    Update {
        /// Only check for updates.
        #[arg(long)]
        check: bool,
    },

    /// Inspect and repair stored chunks.
    #[command(subcommand)]
    Chunks(ChunksCommand),
}

#[derive(Subcommand, Debug)]
pub enum ChunksCommand {
    /// Merge degenerate sub-threshold chunks into their neighbours and
    /// re-embed the result.
    ///
    /// Stores written before chunking enforced a minimum size hold fragments —
    /// bare date stamps, speaker labels, sign-offs — that each became their own
    /// retrievable chunk and outrank the memories that actually answer a query.
    /// Idempotent: a repaired store reports zero on the next run.
    Repair {
        /// Knowledge base to repair.
        #[arg(long)]
        kb: Option<String>,
        /// Repair every KB instead of one.
        #[arg(long)]
        all_kbs: bool,
        /// Report what would change without writing or embedding anything.
        #[arg(long)]
        dry_run: bool,
        /// Re-embed with the deterministic offline stub instead of Gemini.
        /// Without a key this is required: keyless repair used to write stub
        /// vectors next to cloud ones. Same flag as `reindex --offline`.
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show effective configuration (keys masked).
    Show,
    /// Set a configuration key.
    Set { key: String, value: String },
    /// Validate configured API keys.
    Check,
}

#[derive(Subcommand, Debug)]
pub enum AdminCommand {
    /// Drop and replay the fact and adjudication projections from the event journal.
    Rebuild {
        /// Limit the rebuild to one knowledge base.
        #[arg(long)]
        kb: Option<String>,
        /// Rehearse: replay into a transaction that is rolled back, write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Required for a real rebuild — it drops projections before replaying them.
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum KbCommand {
    /// Create or update a KB.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// List KBs.
    #[command(visible_aliases = ["ls"])]
    List,
    /// Show one KB.
    Show { name: String },
    /// Delete one KB. Requires --confirm.
    Delete {
        name: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Register KBs that hold memories but are missing from `kb list`.
    ///
    /// Stores written by builds that predated write-time KB registration have
    /// content in KBs the registry never learned about. Idempotent and
    /// non-destructive: it only ever adds rows.
    Repair {
        /// Report what would be registered without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum DocumentsCommand {
    /// List ingested source documents.
    #[command(visible_aliases = ["ls"])]
    List {
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Search all KBs instead of only --kb.
        #[arg(long)]
        all_kbs: bool,
        /// Max documents to return.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Opaque continuation token from a previous page's `cursor`. Do not
        /// construct or parse it; pass it back verbatim.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one document.
    Show { id: String },
    /// Delete one document and its recallable source chunks. Requires --confirm.
    Delete {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    /// Add a durable memory. Same engine as `remember`, same flags.
    Add {
        content: String,
        #[arg(long, default_value_t = 5)]
        importance: u8,
        #[arg(long = "tag", short = 't')]
        tag: Vec<String>,
        #[arg(long, default_value = "default")]
        diary: String,
        #[arg(long, default_value = "default")]
        kb: String,
        /// Skip LLM-based fact extraction + contradiction detection.
        /// Extraction is ON by default whenever an OpenRouter key resolves.
        #[arg(long, visible_alias = "no-facts")]
        no_extract_facts: bool,
        /// Deprecated: extraction is the default; still forces extraction
        /// alongside --facts-json.
        #[arg(long)]
        extract_facts: bool,
        /// When the content actually happened: RFC3339 or YYYY-MM-DD
        /// (midnight UTC). Drives `recall --since/--until`.
        #[arg(long)]
        event_time: Option<String>,
        /// When this memory STOPS being true: RFC3339, or YYYY-MM-DD meaning
        /// the END of that UTC day. Same flag as `remember --valid-until`.
        #[arg(long)]
        valid_until: Option<String>,
        /// Conflict-resolution authority: assisted (default) | full | strict.
        /// See `remember --help`.
        #[arg(long, default_value = "assisted")]
        autonomy: String,
        /// Store facts directly from a JSON array, skipping the extraction
        /// LLM call. Same shape as `remember --facts-json`.
        #[arg(long)]
        facts_json: Option<String>,
        /// Store even if identical content already exists in this kb/diary.
        #[arg(long)]
        allow_duplicate: bool,
        /// Embed with the deterministic offline stub instead of Gemini.
        /// Without it, a write with no Gemini key is a config error (exit 2).
        /// Same flag as `remember --offline`.
        #[arg(long)]
        offline: bool,
    },
    /// Search durable memories.
    #[command(visible_alias = "find")]
    Search {
        query: String,
        /// Number of results. `--limit` is accepted as an alias.
        #[arg(long, default_value_t = 10, visible_alias = "limit")]
        top_k: usize,
        #[arg(long, default_value = "agent")]
        mode: String,
        #[arg(long, default_value = "cloud_quality")]
        profile: String,
        /// Knowledge base to search. Omit to search EVERY KB (or the KB pinned
        /// by `retrieval.default_kb`). Pass "*" for all.
        #[arg(long)]
        kb: Option<String>,
        /// Search all KBs, ignoring --kb and retrieval.default_kb.
        #[arg(long)]
        all_kbs: bool,
        #[arg(long, default_value = "default")]
        diary: String,
        /// Earliest event_time to include: RFC3339 or YYYY-MM-DD (start of
        /// that UTC day). Same window semantics as `recall --since`.
        #[arg(long)]
        since: Option<String>,
        /// Latest event_time to include: RFC3339 or YYYY-MM-DD (end of that
        /// UTC day). Same window semantics as `recall --until`.
        #[arg(long)]
        until: Option<String>,
        /// Search memories whose `valid_until` has already passed. Same flag
        /// as `recall --include-expired`.
        #[arg(long)]
        include_expired: bool,
    },
    /// Update a memory by UUID.
    Update {
        id: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        importance: Option<u8>,
        /// Re-embed with the deterministic offline stub instead of Gemini.
        /// Same flag as `edit --offline`.
        #[arg(long)]
        offline: bool,
    },
    /// Delete a memory by UUID.
    Delete {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// List stored memories.
    List {
        #[arg(long, default_value = "default")]
        diary: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum JobsCommand {
    /// List compiler jobs.
    #[command(visible_aliases = ["ls"])]
    List {
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Search all KBs instead of only --kb.
        #[arg(long)]
        all_kbs: bool,
        /// Max jobs to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Only jobs of this kind (compile|backfill).
        #[arg(long)]
        kind: Option<String>,
        /// Only jobs in this status (queued|running|completed|paused|failed|cancelled|abandoned).
        #[arg(long)]
        status: Option<String>,
    },
    /// Show one job.
    Show { id: String },
    /// Submit a background job.
    Submit {
        /// compile|backfill.
        #[arg(long)]
        kind: String,
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Runner parameters as a JSON object.
        #[arg(long)]
        params: Option<String>,
        /// Queue the job and return, without running it here.
        #[arg(long)]
        detach: bool,
        /// Block until the job reaches a terminal state.
        #[arg(long)]
        wait: bool,
    },
    /// Ask a running job to stop at its next checkpoint.
    Cancel { id: String },
    /// Continue a paused or failed job from its cursor.
    Resume {
        id: String,
        /// Accept re-running LLM extraction that was already paid for.
        #[arg(long)]
        force: bool,
    },
    /// Follow one job, one NDJSON line per change.
    Watch { id: String },
}

#[derive(Subcommand, Debug)]
pub enum LessonsCommand {
    /// Record a lesson about the work. Written already confirmed: a human who
    /// typed the sentence has confirmed it.
    Add {
        /// What to do next time.
        lesson: String,
        /// When it applies: a slug, `[a-z0-9_]{1,64}`.
        #[arg(long)]
        trigger: String,
        /// Knowledge base/domain namespace.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Diary the recallable twin lands in. `default`, because `recall`
        /// defaults there too and a lesson nobody recalls is a silo.
        #[arg(long, default_value = "default")]
        diary: String,
        /// Journal event this lesson came from. Repeatable.
        #[arg(long)]
        evidence: Vec<String>,
        /// Override the recorded confidence (0.0..=1.0).
        #[arg(long)]
        confidence: Option<f32>,
        /// Embed the twin with the deterministic offline stub.
        #[arg(long)]
        offline: bool,
    },
    /// List lessons. Retired ones are excluded unless asked for by name.
    #[command(visible_aliases = ["ls"])]
    List {
        /// Knowledge base/domain namespace, or "*" for all.
        #[arg(long, default_value = "*")]
        kb: String,
        /// Only this trigger.
        #[arg(long)]
        trigger: Option<String>,
        /// proposed | confirmed | retired.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Opaque page token from a previous response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Promote a proposed lesson and make it recallable.
    Confirm {
        id: String,
        #[arg(long)]
        note: Option<String>,
        /// Embed the twin with the deterministic offline stub.
        #[arg(long)]
        offline: bool,
    },
    /// Take a lesson out of the default listing. Never deletes it.
    Retract {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum BudgetCommand {
    /// Show configured budget and current recorded usage.
    Show {
        /// Optional KB/domain budget. Omit for global.
        #[arg(long)]
        kb: Option<String>,
    },
    /// Set a global or per-KB budget.
    Set {
        /// Optional KB/domain budget. Omit for global.
        #[arg(long)]
        kb: Option<String>,
        /// Daily spend guardrail in USD.
        #[arg(long)]
        daily_usd: Option<f64>,
        /// Monthly spend guardrail in USD.
        #[arg(long)]
        monthly_usd: Option<f64>,
    },
    /// Clear a global or per-KB budget.
    Clear {
        /// Optional KB/domain budget. Omit for global.
        #[arg(long)]
        kb: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum GraphCommand {
    /// Show graph neighbors for one entity.
    Neighbors {
        name: String,
        #[arg(long, default_value = "default")]
        kb: String,
        #[arg(long, default_value_t = 1)]
        hops: u8,
        /// Minimum relation weight to include.
        #[arg(long, default_value_t = 1.0)]
        min_weight: f32,
        /// Max neighbor nodes to return (closest first).
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Drop weak `co_occurs_with` edges and keep only typed relations.
        #[arg(long)]
        typed_only: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum PeopleCommand {
    /// List people with their aliases, mention counts, and top relations.
    #[command(visible_aliases = ["ls"])]
    List {
        /// Knowledge base/domain namespace. Defaults to all KBs ("*").
        #[arg(long, default_value = "*")]
        kb: String,
        /// Max people to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Minimum mention count filter.
        #[arg(long, default_value_t = 1)]
        min_mentions: u32,
        /// Opaque continuation token from a previous page's `cursor`. Do not
        /// construct or parse it; pass it back verbatim.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one person: aliases, relations with provenance, and the memories
    /// that mention them.
    #[command(visible_aliases = ["get"])]
    Show {
        /// Person name or any known alias (case-insensitive).
        name: String,
        /// Knowledge base/domain namespace. Defaults to all KBs ("*").
        #[arg(long, default_value = "*")]
        kb: String,
        /// How many memory snippets to include.
        #[arg(long, default_value_t = 5)]
        snippets: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum EntitiesCommand {
    /// List entities by mention count.
    #[command(visible_aliases = ["ls"])]
    List {
        /// Max entities to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Minimum mention count filter.
        #[arg(long, default_value_t = 1)]
        min_mentions: u32,
        /// Knowledge base/domain namespace. Pass "*" for all.
        #[arg(long, default_value = "default")]
        kb: String,
        /// Opaque continuation token from a previous page's `cursor`. Do not
        /// construct or parse it; pass it back verbatim.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show details for one entity.
    #[command(visible_aliases = ["get"])]
    Show {
        /// Entity name (case-insensitive).
        name: String,
        /// Knowledge base/domain namespace. Pass "*" for all.
        #[arg(long, default_value = "default")]
        kb: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum FactsCommand {
    /// List currently-active facts (newest first).
    #[command(visible_aliases = ["ls"])]
    List {
        /// Filter to a specific subject (case-insensitive).
        #[arg(long)]
        subject: Option<String>,
        /// Diary scope. Defaults to all diaries ("*").
        #[arg(long, default_value = "*")]
        diary: String,
        /// Knowledge base/domain namespace. Omit (or pass "*") to list facts
        /// from EVERY KB — the same default as `recall`, because facts written
        /// to a named KB are otherwise invisible here and read as "extraction
        /// produced nothing". Every fact is labelled with its own KB.
        #[arg(long)]
        kb: Option<String>,
        /// Include superseded (historic) facts in addition to active ones.
        #[arg(long)]
        all: bool,
        /// Max facts to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Opaque continuation token from a previous page's `cursor`. Do not
        /// construct or parse it; pass it back verbatim. A cursor is valid
        /// only on the surface and scope that issued it.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show every fact (active + historic) about a subject.
    #[command(visible_aliases = ["get"])]
    Show {
        /// Subject name (case-insensitive). Pass "*" to use all diaries.
        subject: String,
        /// Diary scope. Omit (or pass "*") for every diary.
        #[arg(long, default_value = "*")]
        diary: String,
        /// Knowledge base/domain namespace. Omit (or pass "*") for every KB,
        /// the same default as `state` and `facts list`.
        #[arg(long, default_value = "*")]
        kb: String,
    },
    /// Show recent contradictions: facts that were superseded by newer claims.
    /// With --open (or --subject): the unresolved queue, grouped by subject
    /// and predicate, newest first, with both values and their sources.
    #[command(after_long_help = "\
Examples:
  engram facts conflicts                          # supersession history + backlog size
  engram facts conflicts --open --limit 50        # the queue, grouped by subject/predicate
  engram facts conflicts --subject \"Ana\"          # one person's contested slots
  engram facts triage --dry-run                   # what triage would auto-resolve
")]
    Conflicts {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// List OPEN conflicts (contested facts awaiting resolution) instead
        /// of the resolved supersession history.
        #[arg(long)]
        open: bool,
        /// Filter open conflicts by diary.
        #[arg(long)]
        diary: Option<String>,
        /// Filter open conflicts by KB.
        #[arg(long)]
        kb: Option<String>,
        /// Only conflicts about this subject (substring, case-insensitive).
        /// Implies --open: the supersession history is not subject-indexed.
        #[arg(long)]
        subject: Option<String>,
        /// Opaque continuation token from a previous page's `cursor`. Do not
        /// construct or parse it; pass it back verbatim.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Re-classify ALREADY-QUEUED conflicts and auto-resolve the ones today's
    /// write path would never have queued (second values of multi-valued
    /// predicates, candidates about another subject or another attribute,
    /// duplicates, and slots with a clear event-time ordering).
    ///
    /// Never auto-resolves a safety slot (allergies, medication, diagnoses,
    /// blood type). Non-destructive: coexisting values are un-queued and stay
    /// active, resolved slots supersede rather than delete. Idempotent — a
    /// second run resolves nothing.
    #[command(after_long_help = "\
Examples:
  engram facts triage --dry-run                   # report only, writes nothing
  engram facts triage --kb personal               # apply to one KB
  engram facts triage --subject \"Ana\" --dry-run   # inspect one person's backlog
")]
    Triage {
        /// Knowledge base to triage. Omit (or "*") for every KB.
        #[arg(long)]
        kb: Option<String>,
        /// Restrict to one diary. Omit (or "*") for every diary.
        #[arg(long)]
        diary: Option<String>,
        /// Only conflicts about this subject (substring, case-insensitive).
        #[arg(long)]
        subject: Option<String>,
        /// Examine at most this many queued conflicts (newest first).
        #[arg(long)]
        limit: Option<usize>,
        /// Report what would be resolved without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Extract facts from memories that were stored before extraction was on.
    ///
    /// Idempotent: memories that already have facts (or an extraction marker)
    /// are skipped, so re-running costs nothing. Oldest first, so a `--limit`ed
    /// run advances chronologically and the next run resumes after it.
    /// One LLM call per memory — needs an OpenRouter key (exit 2 without one)
    /// and respects `engram budget` guardrails.
    #[command(after_long_help = "\
Examples:
  engram facts backfill --dry-run                     # every KB, no API call
  engram facts backfill --kb personal --dry-run       # what would be processed
  engram facts backfill --kb personal --limit 25      # bounded first pass
  engram facts backfill --kb personal --diary work    # one diary only
")]
    Backfill {
        /// Knowledge base/domain namespace to backfill. Omit (or pass "*") to
        /// sweep EVERY KB, oldest memory first across the union. Pair an
        /// unscoped sweep with --dry-run or --limit: this spends one LLM call
        /// per memory.
        #[arg(long)]
        kb: Option<String>,
        /// Restrict to one diary. Omit (or pass "*") for every diary in the KB.
        #[arg(long)]
        diary: Option<String>,
        /// Process at most this many memories in one run.
        #[arg(long)]
        limit: Option<usize>,
        /// Report what would be processed without making any LLM call.
        #[arg(long)]
        dry_run: bool,
    },

    /// Resolve a queued fact conflict. You (the calling agent) have the
    /// conversation context — decide and engram records it.
    Resolve {
        /// Conflict id from `facts conflicts --open` or a remember response.
        id: String,
        /// supersede-old: the new value replaces the old. keep-both: both are
        /// true (not actually a contradiction). reject-new: the old value
        /// stands; the new fact was wrong.
        #[arg(long)]
        action: String,
        /// Optional note recorded with the resolution (e.g. "user confirmed").
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum SkillCommand {
    /// Install the engram skill into agent skill directories.
    Install {
        /// Overwrite a SKILL.md that was edited locally. Without this, a
        /// modified target is left untouched and the install exits 2 —
        /// hand-tuned skill files are not disposable.
        #[arg(long)]
        force: bool,
    },
    /// Package the engram skill folder as a ZIP for app upload.
    Package {
        /// Output ZIP path.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Remove the installed skill files.
    Uninstall,
}

// --- argv parsing, through the envelope -------------------------------------
//
// `Cli::parse()` prints clap's prose to stderr and exits 2. Two exits collide
// there: our own table says 2 means "config error, fix your setup, do not
// retry", so a typo'd flag sent every agent to `config check`. A bad argument
// is bad INPUT — exit 3 — and it must arrive in the same JSON envelope as
// every other failure, because an agent that cannot parse the error cannot
// act on it. `--help` and `--version` stay on stdout at exit 0: they are
// successes. Nothing here opens a store, which preserves the landed guarantee
// that discovery and misuse never migrate a database.

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::CommandFactory;

/// What argv parsing produced when it did not produce a `Cli`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// clap rendered help or version; print `text` to STDOUT, exit 0.
    Rendered { text: String },
    /// Anything else: emit the error envelope, exit 3.
    Invalid { message: String, suggestion: String },
}

/// Classify one `clap::Error` without owning a `Cli` — the seam every other
/// clap parser in the workspace uses, `engramd`'s `Args` most of all, so the
/// daemon and the CLI cannot drift on the same typo.
pub fn classify(e: &clap::Error) -> ParseOutcome {
    match e.kind() {
        // These three are successes wearing an error type. Losing
        // DisplayHelpOnMissingArgumentOrSubcommand here would make bare
        // `engram` exit 3 with an envelope instead of printing help.
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => ParseOutcome::Rendered {
            text: e.render().to_string(),
        },
        kind => ParseOutcome::Invalid {
            message: first_line(&e.render().to_string()),
            suggestion: suggest(kind, e),
        },
    }
}

/// Testable half of `parse_or_exit`: same behaviour, injectable argv, no
/// process boundary.
pub fn parse_from<I, T>(argv: I) -> Result<Cli, ParseOutcome>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Cli::try_parse_from(argv).map_err(|e| classify(&e))
}

/// Terminal half, exposed so the daemon's parser is one line and not a
/// re-derivation: render help to stdout and exit 0, or emit the error
/// envelope and exit 3.
pub fn emit_parse_failure(e: clap::Error) -> ! {
    exit_with(classify(&e))
}

/// Parse argv, or print a JSON error envelope and exit 3.
pub fn parse_or_exit() -> Cli {
    match parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(outcome) => exit_with(outcome),
    }
}

fn exit_with(outcome: ParseOutcome) -> ! {
    use std::io::Write;
    match outcome {
        ParseOutcome::Rendered { text } => {
            let mut out = std::io::stdout().lock();
            let _ = write!(out, "{text}");
            std::process::exit(0)
        }
        ParseOutcome::Invalid {
            message,
            suggestion,
        } => {
            let code = engram_core::ErrorCode::BadInput;
            let mut err = std::io::stderr().lock();
            if crate::error::stderr_is_tty() {
                let _ = writeln!(err, "error: {message}");
                let _ = writeln!(err, "  hint: {suggestion}");
            } else {
                let payload = serde_json::json!({
                    "version": "1",
                    "status": "error",
                    "error": {
                        "code": code.as_str(),
                        "message": message,
                        "suggestion": suggestion,
                        "exit_code": code.exit_code(),
                    }
                });
                let _ = writeln!(err, "{payload}");
            }
            std::process::exit(code.exit_code())
        }
    }
}

/// A suggestion built from the failing context, not boilerplate. An agent that
/// mistypes a subcommand gets the name it meant; one that guesses a flag gets
/// told where the real list is.
fn suggest(kind: ErrorKind, e: &clap::Error) -> String {
    match kind {
        ErrorKind::InvalidSubcommand => {
            let typed = context_string(e, ContextKind::InvalidSubcommand);
            match typed.as_deref().and_then(nearest_subcommand) {
                Some(near) => {
                    format!("did you mean `engram {near}`? `engram --help` lists every command")
                }
                None => "`engram --help` lists every command".to_string(),
            }
        }
        ErrorKind::UnknownArgument => {
            "`engram <command> --help` lists the flags this command accepts; `engram agent-info --command '<command>'` gives their types and rules"
                .to_string()
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
            match context_string(e, ContextKind::ValidValue) {
                Some(v) => format!("valid values are listed in `engram <command> --help` (one of them is `{v}`)"),
                None => "run `engram <command> --help` for the accepted values".to_string(),
            }
        }
        _ => {
            "run `engram <command> --help`, or `engram agent-info --command '<command>'` for this command's exact argument shape and rules"
                .to_string()
        }
    }
}

fn context_string(e: &clap::Error, kind: ContextKind) -> Option<String> {
    match e.get(kind) {
        Some(ContextValue::String(s)) => Some(s.clone()),
        Some(ContextValue::Strings(v)) => v.first().cloned(),
        _ => None,
    }
}

/// Nearest top-level subcommand by Jaro-Winkler, aliases included. The cutoff
/// is deliberately high: a wrong guess reads as authority and sends an agent
/// down a worse path than no guess at all.
fn nearest_subcommand(typed: &str) -> Option<String> {
    let cmd = Cli::command();
    let mut best: Option<(f64, String)> = None;
    for sub in cmd.get_subcommands() {
        for name in std::iter::once(sub.get_name()).chain(sub.get_all_aliases()) {
            let score = strsim::jaro_winkler(typed, name);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, name.to_string()));
            }
        }
    }
    best.filter(|(s, _)| *s >= 0.8).map(|(_, n)| n)
}

/// clap renders a paragraph; the envelope carries a sentence. Keep the first
/// line and drop the `error: ` prefix — the usage block is help, not message.
fn first_line(rendered: &str) -> String {
    rendered
        .lines()
        .next()
        .unwrap_or(rendered)
        .trim()
        .strip_prefix("error: ")
        .unwrap_or_else(|| rendered.lines().next().unwrap_or(rendered).trim())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_and_version_are_successes() {
        for argv in [
            vec!["engram", "--help"],
            vec!["engram", "--version"],
            vec!["engram", "remember", "--help"],
        ] {
            match parse_from(argv.clone()) {
                Err(ParseOutcome::Rendered { text }) => assert!(!text.is_empty()),
                other => panic!("{argv:?} should render, got {other:?}"),
            }
        }
    }

    #[test]
    fn bare_engram_still_prints_help() {
        // The regression that would turn `engram` with no arguments into an
        // exit-3 envelope.
        assert!(matches!(
            parse_from(vec!["engram"]),
            Err(ParseOutcome::Rendered { .. })
        ));
    }

    #[test]
    fn a_missing_argument_is_invalid_not_rendered() {
        match parse_from(vec!["engram", "recall"]) {
            Err(ParseOutcome::Invalid { suggestion, .. }) => {
                assert!(suggestion.contains("engram"))
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_good_command_parses() {
        assert!(parse_from(vec!["engram", "recall", "q"]).is_ok());
    }

    #[test]
    fn a_mistyped_subcommand_names_the_one_it_meant() {
        match parse_from(vec!["engram", "recal", "q"]) {
            Err(ParseOutcome::Invalid { suggestion, .. }) => {
                assert!(suggestion.contains("recall"), "{suggestion}")
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    /// A typo is answered with the 0.6k-token command list, not the
    /// 30k-token manifest: the recovery an error suggests is a call the agent
    /// then pays for.
    #[test]
    fn a_nonsense_subcommand_does_not_invent_a_neighbour() {
        match parse_from(vec!["engram", "zzzzzzzz"]) {
            Err(ParseOutcome::Invalid { suggestion, .. }) => {
                assert!(suggestion.contains("engram --help"), "{suggestion}");
                assert!(!suggestion.contains("did you mean"), "{suggestion}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_flag_points_at_the_flag_list() {
        match parse_from(vec!["engram", "recall", "q", "--nonsense"]) {
            Err(ParseOutcome::Invalid {
                message,
                suggestion,
            }) => {
                assert!(message.contains("nonsense"), "{message}");
                assert!(suggestion.contains("--help"), "{suggestion}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn the_message_is_one_line_without_the_error_prefix() {
        match parse_from(vec!["engram", "recall", "q", "--nonsense"]) {
            Err(ParseOutcome::Invalid { message, .. }) => {
                assert!(!message.contains('\n'));
                assert!(!message.starts_with("error:"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
