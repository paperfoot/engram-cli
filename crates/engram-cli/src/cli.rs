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

        /// Skip LLM-based fact extraction + contradiction detection.
        /// Use for cheap bulk imports where no contradiction check is needed.
        #[arg(long)]
        no_facts: bool,
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

        /// Specialist agent diary namespace, or "*" for all.
        #[arg(long, default_value = "default")]
        diary: String,

        /// Earliest event_time to include (RFC3339).
        #[arg(long)]
        since: Option<String>,

        /// Latest event_time to include (RFC3339).
        #[arg(long)]
        until: Option<String>,
    },

    /// Ingest a file or directory into the memory store.
    Ingest {
        /// Path to file or directory.
        path: PathBuf,

        /// Mining mode: papers|conversations|repos|general|auto.
        #[arg(long, default_value = "auto")]
        mode: String,

        /// Specialist agent diary namespace.
        #[arg(long, default_value = "default")]
        diary: String,
    },

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

    /// Browse atomic (subject, predicate, object) facts and contradictions.
    #[command(subcommand)]
    Facts(FactsCommand),

    /// Export memories as JSON.
    Export {
        /// Output format (json).
        #[arg(long, default_value = "json")]
        format: String,
    },

    /// Import memories from an export file.
    Import {
        /// Path to export JSON.
        file: std::path::PathBuf,
    },

    /// Run a benchmark suite.
    Bench {
        /// Benchmark name: mini | mini-fts | longmemeval | longmemeval-qa | locomo-qa
        #[arg(default_value = "mini")]
        suite: String,

        /// Download dataset if missing.
        #[arg(long)]
        download: bool,

        /// Limit the number of questions (LongMemEval / LoCoMo).
        #[arg(long)]
        limit: Option<usize>,

        /// Answerer model (OpenRouter slug, default openai/gpt-5.4).
        #[arg(long, default_value = "openai/gpt-5.4")]
        answerer: String,

        /// Judge model (OpenRouter slug, default openai/gpt-5.4).
        #[arg(long, default_value = "openai/gpt-5.4")]
        judge: String,

        /// Compute RAGAS metrics (4 extra LLM calls per question).
        #[arg(long)]
        ragas: bool,

        /// Top-k chunks to pass to the answerer.
        #[arg(long, default_value_t = 5)]
        top_k: usize,

        /// Save the report JSON to this path (in addition to stdout).
        #[arg(long)]
        save: Option<std::path::PathBuf>,
    },

    /// Manage engram configuration.
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Manage the agent skill signpost.
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
    Set {
        key: String,
        value: String,
    },
    /// Validate configured API keys.
    Check,
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
    },
    /// Show details for one entity.
    #[command(visible_aliases = ["get"])]
    Show {
        /// Entity name (case-insensitive).
        name: String,
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
    },
    /// Show recent contradictions: facts that were superseded by newer claims.
    Conflicts {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum SkillCommand {
    /// Install the engram skill signpost into ~/.claude, ~/.codex, ~/.gemini.
    Install,
    /// Remove the installed signposts.
    Uninstall,
}
