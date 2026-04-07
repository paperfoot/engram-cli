//! `engram agent-info` — self-describing manifest for AI agents.
//!
//! This is the entry point any agent uses to discover what engram can do
//! without reading docs. Per agent-cli-framework principle #1.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata, OutputFormat};
use serde_json::json;

pub fn run(ctx: &AppContext) -> Result<(), CliError> {
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
            "agent-info": "Print this manifest. Alias: info",
            "remember <content>": "Store a new memory. Flags: --importance 0-10, --tag <tag> (repeatable), --diary <name>",
            "recall <query>": "Retrieve relevant memories. Flags: --top-k <n>, --layer identity|critical|topic|deep, --diary <name>",
            "ingest <path>": "Mine a file or directory. Flags: --mode papers|conversations|repos|general|auto, --diary <name>",
            "forget <id>": "Soft-delete a memory by UUID.",
            "export": "Export the memory store as JSON.",
            "bench [suite]": "Run a benchmark (default: longmemeval). Flags: --download",
            "config show|set|check": "Manage configuration.",
            "skill install|uninstall": "Install or remove the agent skill signpost.",
            "update": "Self-update from GitHub releases. Flags: --check"
        },
        "global_flags": {
            "--json": "Force JSON output (auto when piped)",
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
            "success": {
                "status": "success",
                "data": "<command-specific>",
                "metadata": { "elapsed_ms": "u64", "...": "extra fields" }
            },
            "error": {
                "status": "error",
                "error": {
                    "code": "<machine code>",
                    "message": "<human message>",
                    "suggestion": "<what to do next>",
                    "exit_code": "<int>"
                }
            }
        },
        "auto_json_when_piped": true,
        "env_prefix": "ENGRAM_",
        "config_path": engram_storage::paths::config_path().to_string_lossy(),
        "data_path": engram_storage::paths::db_path().to_string_lossy(),
        "vector_path": engram_storage::paths::vector_dir().to_string_lossy(),
        "providers": {
            "embeddings": ["gemini-embedding-001 (cloud)", "stub (offline)"],
            "rerankers": ["cohere-rerank-3.0 (cloud)", "passthrough (none)"]
        },
        "benchmarks": {
            "longmemeval": "https://github.com/xiaowu0162/LongMemEval",
            "target_R@5": 0.984,
            "target_R@10": 0.998
        }
    });

    print_success(
        ctx.format,
        manifest.clone(),
        Metadata::default(),
        |data| {
            if ctx.format == OutputFormat::Human {
                println!("{}", serde_json::to_string_pretty(data).unwrap());
            }
        },
    );
    Ok(())
}
