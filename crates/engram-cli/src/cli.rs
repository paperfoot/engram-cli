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

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print machine-readable capability manifest.
    #[command(visible_alias = "info")]
    AgentInfo,

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
        #[arg(long, alias = "no-facts")]
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

        /// Store even if identical content already exists in this kb/diary.
        /// By default an exact duplicate is a no-op (no second embedding).
        #[arg(long)]
        allow_duplicate: bool,

        /// Conflict-resolution authority: `assisted` (default) queues
        /// uncertain contradictions for YOU to resolve via `facts resolve`
        /// (you have the conversation context); `full` lets engram
        /// adjudicate with its own LLM call; `strict` never auto-supersedes.
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
    },

    /// Retrieve relevant memories via hybrid search (dense + lexical + rerank).
    #[command(visible_aliases = ["search"])]
    Recall {
        /// Search query.
        query: String,

        /// Number of results.
        #[arg(long, default_value_t = 10)]
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
    },

    /// Inspect and manage ingested source documents.
    #[command(subcommand)]
    Documents(DocumentsCommand),

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
        /// OpenRouter model for extraction. Defaults to Gemini 3.1 Flash-Lite Preview.
        #[arg(long)]
        extraction_model: Option<String>,
        /// OpenRouter model for synthesis/wiki overview. Defaults to Gemini 3.1 Pro Preview.
        #[arg(long)]
        synthesis_model: Option<String>,
        /// Limit LLM extraction chunks to control cost. Deterministic compile still scans all chunks.
        #[arg(long)]
        max_llm_chunks: Option<usize>,
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
        /// Number of evidence leads.
        #[arg(long, default_value_t = 12)]
        top_k: usize,
        /// Retrieval profile: cloud_quality|fast|offline.
        #[arg(long, default_value = "cloud_quality")]
        profile: String,
        /// Permit comparing chunks embedded with different metadata.
        #[arg(long)]
        allow_mixed_embeddings: bool,
    },

    /// Inspect compile/daemon jobs.
    #[command(subcommand)]
    Jobs(JobsCommand),

    /// Re-embed chunks with the active embedding model/profile.
    Reindex {
        /// Knowledge base/domain namespace.
        #[arg(long)]
        kb: Option<String>,
        /// Reindex all KBs.
        #[arg(long)]
        all: bool,
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

        /// Answerer model (OpenRouter slug, default anthropic/claude-opus-4.8).
        #[arg(long, default_value = "anthropic/claude-opus-4.8")]
        answerer: String,

        /// Judge model (OpenRouter slug, default google/gemini-3.1-pro-preview).
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
        #[arg(long, alias = "no-facts")]
        no_extract_facts: bool,
        /// Deprecated: extraction is the default; still forces extraction
        /// alongside --facts-json.
        #[arg(long)]
        extract_facts: bool,
        /// When the content actually happened: RFC3339 or YYYY-MM-DD
        /// (midnight UTC). Drives `recall --since/--until`.
        #[arg(long)]
        event_time: Option<String>,
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
    },
    /// Search durable memories.
    #[command(visible_alias = "find")]
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
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
    },
    /// Update a memory by UUID.
    Update {
        id: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        importance: Option<u8>,
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
    },
    /// Show one job.
    Show { id: String },
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
        /// Diary scope. Default is "default".
        #[arg(long, default_value = "default")]
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
    },
    /// Show every fact (active + historic) about a subject.
    #[command(visible_aliases = ["get"])]
    Show {
        /// Subject name (case-insensitive). Pass "*" to use all diaries.
        subject: String,
        /// Diary scope. Default is "default". Pass "*" for all.
        #[arg(long, default_value = "default")]
        diary: String,
        /// Knowledge base/domain namespace. Pass "*" for all.
        #[arg(long, default_value = "default")]
        kb: String,
    },
    /// Show recent contradictions: facts that were superseded by newer claims.
    /// With --open: unresolved (contested) conflicts awaiting your decision.
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
