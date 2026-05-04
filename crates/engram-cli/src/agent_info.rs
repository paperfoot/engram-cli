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
        "homepage": "https://github.com/paperfoot/engram-cli",
        "summary_for_agents": [
            "engram is a local-first knowledge engine. Use `remember` to store, `recall` to retrieve.",
            "Pipe stdout to jq — output is always JSON when piped or with --json.",
            "Errors include `code`, `message`, `suggestion`, and `exit_code` for programmatic handling.",
            "Use `--diary <name>` to keep separate memory namespaces per specialist agent.",
            "Use `--kb <name>` to select the domain knowledge base.",
            "For directory ingest, run `engram ingest <dir> --dry-run` first, then ingest with the same --include/--exclude/--max-files scope. Use --all only for an intentional full recursive ingest.",
            "Default recall profile is cloud_quality: Gemini Embedding 2 + FTS5 BM25 + claims/entities/graph + Cohere rerank when configured."
        ],
        "commands": {
            "agent-info | info": "Print this manifest (raw JSON, not enveloped)",
            "kb create <name>": "Create/update a knowledge base. Flags: --description <text>",
            "kb list | kb show <name> | kb delete <name> --confirm": "Manage domain KBs.",
            "documents list|show|delete": "Inspect or remove ingested source documents. Flags: --kb <name>, --all-kbs, --limit <n>, --confirm",
            "remember <content>": "Store a new memory. Flags: --importance 0-10, --tag <tag> (repeatable), --diary <name>, --kb <name>, --extract-facts. Fact extraction is opt-in because it may call an LLM.",
            "memory add|search|update|delete|list": "Simple aliases for agent memory workflows. Prefer these when an agent wants CRUD semantics.",
            "recall <query>": "Retrieve via cloud-quality hybrid search. Flags: --kb <name>, --mode evidence|raw|wiki|explore|agent, --profile cloud_quality|fast|offline, --top-k <n>, --allow-mixed-embeddings",
            "research <query>": "Evidence-first research retrieval pass. Returns query_plan, answer_context, evidence leads, and citation readiness.",
            "ingest <path>": "Mine a file or scoped directory. Flags: --mode papers|takeaways|conversations|repos|general|auto, --diary <name>, --kb <name>, --compile evidence, --dry-run, --include <pattern>, --exclude <pattern>, --max-files <n>, --all",
            "compile --kb <name> --all": "Derive cited claims, entities, relations, takeaways, and wiki pages. Add --llm for Gemini 3.1 Flash-Lite extraction + Gemini 3.1 Pro synthesis via OpenRouter.",
            "jobs list|show": "Inspect compile job history.",
            "reindex --kb <name> | --all": "Re-embed chunks with Gemini Embedding 2 metadata guards.",
            "wiki --kb <name> [path]": "Inspect generated domain wiki pages.",
            "graph neighbors <entity> --kb <name>": "Inspect SQLite graph neighbors. Flags: --hops 1|2, --min-weight <n>.",
            "doctor": "Verify keys, schema, embedding metadata, and daemon port. Use --compiler to require OpenRouter and --integrity to check lifecycle residue.",
            "repair --dry-run | --confirm": "Inspect or clean stale facts and derived graph residue left by old delete/edit flows.",
            "serve": "Start local HTTP API. Default: 127.0.0.1:8768. Alias binary: engramd.",
            "usage [--kb <name>] [--since <rfc3339>]": "Summarize recorded Gemini embedding and Cohere rerank usage.",
            "budget show|set|clear": "Configure local usage guardrails. Flags: --kb <name>, --daily-usd <n>, --monthly-usd <n>.",
            "forget <id> --confirm": "Soft-delete a memory by UUID. Destructive — requires --confirm.",
            "edit <id>": "Update memory content or metadata. Flags: --content <text>, --importance 0-10",
            "export [--format json] [--kb <name>]": "Export memories as JSON",
            "import <file>": "Import memories from a JSON export",
            "entities [list|show <name>]": "Browse the entity graph. Alias: list|ls",
            "bench <suite>": "Run a benchmark. Suites: mini | mini-fts | scientific-mini | longmemeval | longmemeval-qa | locomo-qa. Flags: --limit <n>, --download, --answerer, --judge",
            "config show": "Display effective configuration",
            "config set <key> <value>": "Persist a configuration value to ~/.config/engram/config.toml",
            "config check": "Validate configured API keys",
            "skill install | package | uninstall": "Install, package, or remove the agent skill. Installs SKILL.md plus Codex agents/openai.yaml metadata.",
            "update [--check]": "Check or apply distribution-aware updates. Output includes install_source, update_mode, release_url, upgrade_command, and post_update_commands."
        },
        "flags": {
            "--json": "Force JSON envelope output (auto when stdout is piped)",
            "--quiet": "Suppress non-essential stderr output"
        },
        "command_schemas": {
            "remember": {
                "destructive": false,
                "args": { "content": "string" },
                "flags": { "importance": "0..10", "tag": "string[]", "diary": "string", "kb": "string", "extract_facts": "boolean" },
                "output_data": { "id": "uuid", "stored": "boolean", "chunks": "integer", "kb": "string", "facts_added": "integer", "conflicts": "array" }
            },
            "recall": {
                "destructive": false,
                "args": { "query": "string" },
                "flags": { "top_k": "integer", "kb": "string", "all_kbs": "boolean", "diary": "string|*", "mode": "evidence|raw|wiki|explore|agent", "profile": "cloud_quality|fast|offline" },
                "output_data": { "status": "success|no_results", "answer_context": "string", "results": "array" }
            },
            "ingest": {
                "destructive": false,
                "args": { "path": "file_or_directory" },
                "flags": {
                    "kb": "string",
                    "diary": "string",
                    "mode": "papers|takeaways|conversations|repos|general|auto",
                    "compile": "none|evidence",
                    "dry_run": "boolean preview without writes",
                    "include": "string[] glob-like patterns; repeatable; matches file name and relative path",
                    "exclude": "string[] glob-like patterns; repeatable; matches file name and relative path",
                    "max_files": "integer cap; refuses if matched files exceed cap",
                    "all": "boolean explicit full recursive directory confirmation"
                },
                "safety": {
                    "single_file": "ingests exactly that file",
                    "directory_default": "refuses unless --all, --include/--exclude, --max-files, or --dry-run is present when ingest.require_scope is true",
                    "agent_rule": "for directories, run --dry-run first, inspect matched_files, then repeat with the same scope and without --dry-run",
                    "config": ["ingest.require_scope", "ingest.include", "ingest.exclude", "ingest.max_files"]
                },
                "idempotency": "same kb+source_path is skipped when it already has active chunks",
                "output_data": { "matched_count": "integer", "matched_files": "string[]", "skipped_existing": "integer", "memories_created": "integer", "chunks_created": "integer" }
            },
            "forget": {
                "destructive": true,
                "requires": ["--confirm"],
                "args": { "id": "uuid" }
            },
            "documents delete": {
                "destructive": true,
                "requires": ["--confirm"],
                "side_effects": ["soft-deletes document memories", "removes facts for those memories", "clears derived graph/wiki artifacts for the KB"]
            },
            "repair": {
                "destructive": true,
                "safe_mode": "--dry-run",
                "mutating_mode": "--confirm",
                "repairs": ["facts from deleted memories", "derived graph residue in empty KBs"]
            },
            "update": {
                "destructive": false,
                "args": {},
                "flags": { "check": "boolean; only report update status and command" },
                "config": {
                    "update.mode": "auto|check_only|disabled; auto permits `engram update` to run the package-manager command",
                    "update.channel": "github|crates",
                    "update.install_source": "optional override: homebrew|cargo|standalone|source_build"
                },
                "output_data": {
                    "current_version": "string",
                    "latest_version": "string|null",
                    "update_available": "boolean",
                    "install_source": "homebrew|cargo|standalone|source_build|unknown",
                    "update_mode": "auto|check_only|disabled",
                    "release_url": "string|null",
                    "upgrade_command": "string|null",
                    "can_execute_update": "boolean",
                    "post_update_commands": "string[]"
                }
            }
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
            "OPENROUTER_API_KEY": "Optional extraction/synthesis key for compiler and fact extraction",
            "ENGRAM_COMPILER_EXTRACTION_MODEL": "Compiler extraction model slug (default google/gemini-3.1-flash-lite-preview)",
            "ENGRAM_COMPILER_SYNTHESIS_MODEL": "Compiler synthesis model slug (default google/gemini-3.1-pro-preview)",
            "ENGRAM_COMPILER_LLM": "Set true to make `engram compile --all` use the LLM compiler by default",
            "ENGRAM_RRF_K": "RRF smoothing constant (default: 60)",
            "ENGRAM_COHERE_RERANK_USD_PER_SEARCH": "Optional local price for usage cost estimates; Cohere usage is recorded as search units by default",
            "ENGRAM_INGEST_REQUIRE_SCOPE": "Directory safety gate. Default true: unscoped directory ingest requires --all, --dry-run, --include/--exclude, or --max-files.",
            "ENGRAM_INGEST_INCLUDE": "Comma-separated default include patterns for directory ingest, overridden by CLI --include.",
            "ENGRAM_INGEST_EXCLUDE": "Comma-separated default exclude patterns for directory ingest, overridden by CLI --exclude.",
            "ENGRAM_INGEST_MAX_FILES": "Default directory ingest cap, overridden by CLI --max-files.",
            "ENGRAM_UPDATE_MODE": "auto|check_only|disabled. Default auto.",
            "ENGRAM_UPDATE_CHANNEL": "github|crates. Default github.",
            "ENGRAM_UPDATE_INSTALL_SOURCE": "Override package-manager detection: homebrew|cargo|standalone|source_build.",
            "ENGRAM_UPDATE_GITHUB_REPO": "GitHub repo used by update checks. Default paperfoot/engram-cli.",
            "ENGRAM_UPDATE_CRATE": "crates.io package used by cargo updates. Default paperfoot-engram.",
            "ENGRAM_UPDATE_HOMEBREW_FORMULA": "Homebrew formula used by brew updates. Default paperfoot/tap/engram.",
            "ENGRAM_EMBEDDER": "Force embedder: gemini|stub (default: gemini if key set)",
            "ENGRAM_RERANK_PROVIDER": "Force reranker: cohere|zerank2|none (default: cohere if key set)",
            "ENGRAM_LME_SPLIT": "LongMemEval split: s|oracle (default: s)"
        },
        "config_keys": {
            "ingest.require_scope": "true by default. Keep true for agent safety; set false only in tightly controlled batch environments.",
            "ingest.include": "Comma-separated default include patterns, for example `*.pdf,notes/*.md`.",
            "ingest.exclude": "Comma-separated default exclude patterns, for example `private/*,drafts/*`.",
            "ingest.max_files": "Default maximum matched files for directory ingest.",
            "update.mode": "auto|check_only|disabled. Use check_only when agents may detect but not mutate installs.",
            "update.channel": "github|crates. GitHub checks the latest release; crates checks the package registry.",
            "update.install_source": "Optional override when binary path detection is ambiguous.",
            "update.github_repo": "Repository for release checks and standalone release downloads.",
            "update.crate": "crates.io package for cargo install upgrades.",
            "update.homebrew_formula": "Homebrew formula for brew upgrades."
        },
        "agent_decision_rules": {
            "ingest": [
                "If the user gives one exact file path, ingest that file directly.",
                "If the user gives a directory, first run `engram ingest <dir> --dry-run --json` plus any obvious --include/--exclude scope and inspect data.matched_files.",
                "For a curated batch, rerun without --dry-run using the same --include/--exclude and a reasonable --max-files cap.",
                "Use --all only when the user explicitly asks to ingest the entire folder or after the preview clearly matches the intended corpus.",
                "Use config or ENGRAM_INGEST_* defaults for repeated agent workflows, but keep command-line flags in task logs when the current task has a specific scope."
            ],
            "update": [
                "Run `engram update --check --json` during bootstrap or maintenance if knowing install freshness matters.",
                "Do not run `engram update` as a side effect of ordinary recall/remember/ingest workflows.",
                "Run `engram update --json` only when the user asks to update the CLI or an approved maintenance workflow allows package-manager mutation.",
                "After a successful update, restart the agent process and run `engram skill install` if local agent skill files are used."
            ]
        },
        "config_path": engram_storage::paths::config_path().to_string_lossy(),
        "data_path": engram_storage::paths::db_path().to_string_lossy(),
        "vector_path": engram_storage::paths::vector_dir().to_string_lossy(),
        "cache_path": engram_storage::paths::cache_dir().to_string_lossy(),
        "providers": {
            "embeddings": ["gemini-embedding-2 (cloud, 1536 dims)", "stub (offline, deterministic)"],
            "rerankers": ["cohere/rerank-v3.5 (cloud)", "passthrough (none)"],
            "compiler": {
                "extraction_default": engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL,
                "synthesis_default": engram_llm::openrouter::DEFAULT_SYNTHESIS_MODEL
            }
        },
        "daemon_api": {
            "base": "http://127.0.0.1:8768",
            "endpoints": [
                "GET /health",
                "GET|POST /v1/kbs",
                "GET /v1/documents",
                "GET|DELETE /v1/documents/{id}",
                "POST /v1/recall",
                "POST /v1/research",
                "POST /v1/ingest",
                "POST /v1/compile",
                "POST /v1/reindex",
                "GET /v1/entities",
                "GET /v1/entities/{id}",
                "GET /v1/jobs",
                "GET /v1/jobs/{id}",
                "GET /v1/usage",
                "GET|POST /v1/budget"
            ]
            ,
            "ingest_request_fields": {
                "path": "string",
                "kb": "string optional",
                "mode": "papers|takeaways|conversations|repos|general|auto optional",
                "diary": "string optional",
                "compile": "none|evidence optional",
                "dry_run": "boolean optional",
                "include": "string[] optional",
                "exclude": "string[] optional",
                "max_files": "integer optional",
                "all": "boolean optional"
            }
        },
        "retrieval_profiles": {
            "cloud_quality": { "embedder": "gemini-embedding-2", "dims": 1536, "lexical": "FTS5 BM25", "entity_claim_search": true, "graph_expansion": true, "reranker": "cohere/rerank-v3.5", "rerank_top_n": 50 },
            "fast": { "embedder": "gemini-embedding-2", "dims": 1536, "rerank_top_n": 20 },
            "offline": { "embedder": "stub", "lexical": "FTS5 BM25", "reranker": "none" }
        },
        "examples": [
            "engram kb create ageing-biology --description 'Ageing biology and longevity science'",
            "engram ingest ./paper.pdf --kb ageing-biology --mode papers --compile evidence",
            "engram ingest ./papers --kb ageing-biology --mode papers --dry-run --include '*.pdf' --json",
            "engram ingest ./papers --kb ageing-biology --mode papers --include '*.pdf' --max-files 20 --compile evidence",
            "engram documents list --kb ageing-biology --json",
            "engram compile --kb ageing-biology --all --llm --max-llm-chunks 25",
            "engram research 'what human evidence exists for rapamycin dosing?' --kb ageing-biology --json",
            "engram reindex --kb ageing-biology",
            "engram recall 'rapamycin dosing evidence in humans' --kb ageing-biology --mode evidence --profile cloud_quality --json",
            "engram usage --kb ageing-biology --json",
            "engram budget set --kb ageing-biology --daily-usd 5 --monthly-usd 100",
            "engram skill install",
            "engram skill package --out engram-skill.zip",
            "engram serve --host 127.0.0.1 --port 8768",
            "engram bench scientific-mini --json"
        ],
        "benchmarks": {
            "longmemeval_s": {
                "url": "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned",
                "description": "500 questions × ~48 sessions/q, 96% distractors",
                "engram_v2_R@1": 0.91,
                "engram_v2_R@5": 0.99,
                "engram_v2_R@10": 0.998
            }
        },
        "framework": "https://github.com/paperfoot/agent-cli-framework"
    });

    // Raw JSON — NOT wrapped in envelope. Framework principle #1.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&manifest).unwrap());
    Ok(())
}
