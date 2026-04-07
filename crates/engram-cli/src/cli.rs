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
    },

    /// Retrieve relevant memories.
    Recall {
        /// Search query.
        query: String,

        /// Number of results.
        #[arg(long, default_value_t = 10)]
        top_k: usize,

        /// Memory layer to target (identity|critical|topic|deep).
        #[arg(long, default_value = "topic")]
        layer: String,

        /// Specialist agent diary namespace.
        #[arg(long, default_value = "default")]
        diary: String,
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

    /// Soft-delete a memory by id.
    Forget {
        /// Memory id (UUID).
        id: String,
    },

    /// Export memories.
    Export {
        /// Output format (json).
        #[arg(long, default_value = "json")]
        format: String,
    },

    /// Run a benchmark suite.
    Bench {
        /// Benchmark name: mini | mini-fts | longmemeval
        #[arg(default_value = "mini")]
        suite: String,

        /// Download dataset if missing.
        #[arg(long)]
        download: bool,

        /// Limit the number of questions (LongMemEval only).
        #[arg(long)]
        limit: Option<usize>,
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
pub enum SkillCommand {
    /// Install the engram skill signpost into ~/.claude, ~/.codex, ~/.gemini.
    Install,
    /// Remove the installed signposts.
    Uninstall,
}
