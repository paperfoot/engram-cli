//! `engram agent-info` — self-describing manifest for AI agents.
//!
//! Per agent-cli-framework principle #1, this returns the raw JSON manifest
//! directly (NOT wrapped in the standard success envelope). Agents parse
//! this once to discover every command.

use crate::context::AppContext;
use crate::error::CliError;
use serde_json::json;
use std::io::Write;

pub fn run(_ctx: &AppContext) -> Result<(), CliError> {
    let manifest = json!({
        "name": "engram",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Agent-native memory engine for scientific knowledge and personal memory.",
        "homepage": "https://github.com/199-biotechnologies/engram-2",
        "summary_for_agents": [
            "engram is a local-first knowledge engine. Use `remember` to store, `recall` to retrieve.",
            "Pipe stdout to jq — output is always JSON when piped or with --json.",
            "Errors include `code`, `message`, `suggestion`, and `exit_code` for programmatic handling.",
            "Use `--diary <name>` to keep separate memory namespaces per specialist agent."
        ],
        "commands": {
            "agent-info | info": "Print this manifest (raw JSON, not enveloped)",
            "remember <content>": "Store a new memory. Flags: --importance 0-10, --tag <tag> (repeatable), --diary <name>",
            "recall <query>": "Retrieve relevant memories via hybrid search (dense + lexical + rerank). Flags: --top-k <n>, --layer identity|critical|topic|deep, --diary <name>, --since <iso8601>, --until <iso8601>",
            "ingest <path>": "Mine a file or directory. Flags: --mode papers|conversations|repos|general|auto, --diary <name>",
            "forget <id> --confirm": "Soft-delete a memory by UUID. Destructive — requires --confirm.",
            "edit <id>": "Update memory content or metadata. Flags: --content <text>, --importance 0-10",
            "export [--format json]": "Export all memories as JSON",
            "import <file>": "Import memories from a JSON export",
            "entities [list|show <name>]": "Browse the entity graph. Alias: list|ls",
            "bench <suite>": "Run a benchmark. Suites: mini | mini-fts | longmemeval. Flags: --limit <n>, --download",
            "config show": "Display effective configuration",
            "config set <key> <value>": "Persist a configuration value to ~/.config/engram/config.toml",
            "config check": "Validate configured API keys",
            "skill install | uninstall": "Install or remove the agent skill signpost",
            "update [--check]": "Self-update from GitHub releases"
        },
        "flags": {
            "--json": "Force JSON envelope output (auto when stdout is piped)",
            "--quiet": "Suppress non-essential stderr output"
        },
        "exit_codes": {
            "0": "Success",
            "1": "Transient (retry with backoff)",
            "2": "Config error (fix setup, do not retry)",
            "3": "Bad input (fix arguments)",
            "4": "Rate limited (back off and retry)"
        },
        "envelope": {
            "version": "1",
            "success": "{ version: \"1\", status: \"success\", data: <command-specific>, metadata: { elapsed_ms, ... } }",
            "error": "{ version: \"1\", status: \"error\", error: { code, message, suggestion, exit_code } }"
        },
        "status_values": ["success", "partial_success", "all_failed", "no_results", "error"],
        "auto_json_when_piped": true,
        "env_prefix": "ENGRAM_",
        "env_overrides": {
            "GEMINI_API_KEY": "Google Gemini embedding key (required for hybrid retrieval)",
            "COHERE_API_KEY": "Cohere rerank key (optional, improves R@1)",
            "ENGRAM_RRF_K": "RRF smoothing constant (default: 60)",
            "ENGRAM_EMBEDDER": "Force embedder: gemini|stub (default: gemini if key set)",
            "ENGRAM_RERANKER": "Force reranker: cohere|passthrough (default: cohere if key set)",
            "ENGRAM_LME_SPLIT": "LongMemEval split: s|oracle (default: s)"
        },
        "config_path": engram_storage::paths::config_path().to_string_lossy(),
        "data_path": engram_storage::paths::db_path().to_string_lossy(),
        "vector_path": engram_storage::paths::vector_dir().to_string_lossy(),
        "cache_path": engram_storage::paths::cache_dir().to_string_lossy(),
        "providers": {
            "embeddings": ["gemini-embedding-001 (cloud, 768 dims)", "stub (offline, deterministic)"],
            "rerankers": ["rerank-english-v3.0 (cloud)", "passthrough (none)"]
        },
        "benchmarks": {
            "longmemeval_s": {
                "url": "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned",
                "description": "500 questions × ~48 sessions/q, 96% distractors",
                "mempalace_baseline_R@5": 0.984,
                "engram_v2_R@1": 0.91,
                "engram_v2_R@5": 0.99,
                "engram_v2_R@10": 0.998
            }
        },
        "framework": "https://github.com/199-biotechnologies/agent-cli-framework"
    });

    // Raw JSON — NOT wrapped in envelope. Framework principle #1.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&manifest).unwrap());
    Ok(())
}
