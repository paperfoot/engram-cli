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
        "description": "Agent-native memory engine for personal, business, and domain memory.",
        "homepage": "https://github.com/paperfoot/engram-cli",
        "summary_for_agents": [
            "engram is a local-first memory engine. Use `remember` to store, `recall` to retrieve.",
            "A plain `recall <query>` searches EVERY KB. Do not add --kb to 'find' something — you cannot know which namespace a fact was filed under. Narrow with --kb only to deliberately exclude the rest.",
            "Pipe stdout to jq — output is always JSON when piped or with --json.",
            "Errors include `code`, `message`, `suggestion`, and `exit_code` for programmatic handling.",
            "Use `--diary <name>` to keep separate memory namespaces per specialist agent.",
            "Use `--kb <name>` on WRITES to file a memory under a domain (personal, business, songs). Writing to a new KB registers it automatically.",
            "`remember` extracts (subject, predicate, object) facts by default when an OpenRouter key resolves; the envelope always says which via `fact_extraction`. Opt out with --no-extract-facts.",
            "Use `--event-time` whenever the content has a real date (birthday, anniversary, a decision made last March) — `recall --since/--until` filters on it.",
            "For directory ingest, run `engram ingest <dir> --dry-run` first, then ingest with the same --include/--exclude/--max-files scope. Use --all only for an intentional full recursive ingest.",
            "Default recall profile is cloud_quality: Gemini Embedding 2 + FTS5 BM25 + claims/entities/graph + Cohere rerank when configured."
        ],
        "commands": {
            "agent-info | info": "Print this manifest (raw JSON, not enveloped)",
            "kb create <name>": "Create/update a knowledge base. Flags: --description <text>. Rarely needed: writing with --kb <name> registers the KB.",
            "kb list | kb show <name> | kb delete <name> --confirm": "Manage domain KBs.",
            "kb repair [--dry-run]": "Register KBs that hold memories but are missing from `kb list` — the residue of stores written by builds that predated write-time registration. Idempotent and additive; never touches memories. `doctor --integrity` reports the same fault as `unregistered_kbs`.",
            "documents list|show|delete": "Inspect or remove ingested source documents. Flags: --kb <name>, --all-kbs, --limit <n>, --confirm",
            "remember <content>": "Store a new memory. Flags: --importance 0-10, --tag <tag> (repeatable), --diary <name>, --kb <name>, --event-time <RFC3339|YYYY-MM-DD>, --no-extract-facts, --facts-json '<json array>', --autonomy full|assisted|strict, --allow-duplicate. Identical content in the same kb/diary is deduplicated (no second embedding) unless --allow-duplicate. Fact extraction runs BY DEFAULT whenever an OpenRouter key resolves — pass --no-extract-facts for bulk/low-value writes, or --facts-json when you already know the (subject, predicate, object) triple (same contradiction detection, zero LLM cost). Set --event-time for anything with a real date: `recall --since/--until` filters on it and the extracted facts inherit it. Under the default --autonomy assisted, uncertain contradictions come back in `adjudication_needed` for YOU to resolve (you have the conversation context): run the included resolve_command. Safety slots (allergic_to, takes_medication, diagnosed_with) are never auto-superseded.",
            "memory add|search|update|delete|list": "Simple aliases for agent memory workflows. Prefer these when an agent wants CRUD semantics. `memory add` takes the same flags as `remember` (including --event-time, --no-extract-facts, --facts-json, --autonomy).",
            "recall <query>": "Retrieve via cloud-quality hybrid search. Searches EVERY KB unless narrowed. Flags: --kb <name> (narrow to one; '*' means all), --all-kbs (force all, ignoring --kb and retrieval.default_kb), --diary <name>|'*', --mode evidence|raw|wiki|explore|agent, --profile cloud_quality|fast|offline, --top-k <n>, --layer identity|critical|topic|deep, --rerank-top-n <n>, --graph-hops 1|2, --allow-mixed-embeddings, --since <rfc3339>, --until <rfc3339>. `metadata.kb_scope` reports how the scope was chosen (flag|config|all_kbs) and `metadata.kb` the effective KB ('*' for all) — check it before concluding a memory does not exist. A diary-scoped recall reports anything the isolation guard withheld in `metadata.diary_isolation`. Output includes a `conflicts` array with two statuses: `superseded` (a fact was updated — cite the current value or surface the change) and `contested` (an UNRESOLVED contradiction: both values are listed — do not pick one silently; resolve it via the included resolve_command or surface both to the user).",
            "facts list|show <subject>|conflicts|resolve|backfill": "Browse the fact store and contradiction record. `facts conflicts` lists resolved supersessions; `facts conflicts --open` lists CONTESTED pairs awaiting your decision; `facts resolve <id> --action supersede-old|keep-both|reject-new [--note]` records your verdict and applies the supersession (never a deletion). Facts come from `remember` (on by default), `--facts-json`, and `compile --llm`, KB-scoped.",
            "facts backfill --kb <name>": "Extract facts from memories stored before extraction was on. Flags: --diary <name>, --limit <n>, --dry-run. Idempotent — memories that already have facts or an extraction marker are skipped, oldest first, so a --limit'ed run resumes where the last one stopped. One LLM call per memory: --kb is required so the spend is never implicit, and it exits 2 without an OpenRouter key.",
            "people list | people show <name>": "The networking view over person-typed entities. `people list` gives aliases, mention counts, and top relations (flags: --kb <name>|'*', --limit <n>, --min-mentions <n>); `people show <name>` resolves any known alias case-insensitively and returns relations WITH provenance plus the memories that mention them (flags: --kb, --snippets <n>).",
            "research <query>": "Evidence-first research retrieval pass. Returns query_plan, answer_context, evidence leads, and citation readiness.",
            "ingest <path>": "Mine a file or scoped directory. Flags: --mode papers|takeaways|conversations|repos|general|auto, --diary <name>, --kb <name>, --compile evidence, --dry-run, --include <pattern>, --exclude <pattern>, --max-files <n>, --all. Re-ingesting the same path is idempotent when content is unchanged, and REPLACES the prior version when the file changed (edited PDFs no longer go stale).",
            "compile --kb <name> --all": "Derive cited claims, entities, relations, takeaways, and wiki pages. Add --llm for Gemini 3.1 Flash-Lite extraction + Gemini 3.1 Pro synthesis via OpenRouter.",
            "jobs list|show": "Inspect compile job history.",
            "reindex --kb <name> | --all": "Re-embed chunks with Gemini Embedding 2 metadata guards.",
            "wiki --kb <name> [path]": "Inspect generated domain wiki pages.",
            "graph neighbors <entity> --kb <name>": "Inspect SQLite graph neighbors, closest first. Flags: --hops 1|2, --min-weight <n>, --limit <n>, --typed-only (drop weak `co_occurs_with` edges and keep only typed relations — use it when you want the real relationship graph, not co-mention noise).",
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
            "bench <suite>": "Run a benchmark. Suites: mini | mini-fts | personal | scientific-mini | longmemeval | longmemeval-qa | locomo-plus | mab | litqa2 | beir | biogen. Flags: --limit <n>, --download, --profile offline|cloud_quality|fast (personal only), --mode closed-book|fulltext, --dataset <scifact|nfcorpus|trec-covid|scidocs>, --rerank on|off, --year 2024|2025, --top-k <n>, --kb <name>, --corpus <path>, --answerer <slug>, --judge <slug>, --save <path>",
            "bench personal": "The personal-memory quality gate. Offline and deterministic by default (hash embedder, no network, no keys, no writes to your store) — run it before and after any retrieval change. Headline metrics are production behaviour; `legacy_default_comparison` is what the old kb=default floor would have scored, kept as a regression tripwire.",
            "config show": "Display effective configuration",
            "config set <key> <value>": "Persist a configuration value to the on-disk config file (exact path in `config_path` below).",
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
                "flags": {
                    "importance": "0..10",
                    "tag": "string[]",
                    "diary": "string",
                    "kb": "string — writing to a new KB registers it",
                    "event_time": "RFC3339 or YYYY-MM-DD (midnight UTC); when the content actually happened",
                    "no_extract_facts": "boolean; opt out of the default LLM fact extraction",
                    "extract_facts": "boolean; deprecated — extraction is the default. Still forces extraction alongside --facts-json",
                    "facts_json": "json string; supply the triples yourself and skip the extraction LLM call",
                    "autonomy": "full|assisted|strict",
                    "allow_duplicate": "boolean"
                },
                "output_data": {
                    "id": "uuid",
                    "stored": "boolean",
                    "chunks": "integer",
                    "kb": "string",
                    "event_time": "rfc3339|null",
                    "fact_extraction": "ran|ran_stub|disabled|skipped_facts_json|skipped_no_openrouter_key|failed — ALWAYS check this before assuming facts were captured",
                    "fact_extraction_note": "string|null — present whenever facts were NOT captured, with the fix",
                    "facts_added": "integer",
                    "facts_deduped": "integer",
                    "conflicts": "array",
                    "adjudication_needed": "array — unresolved conflicts with a ready-to-run resolve_command; handle them"
                }
            },
            "recall": {
                "destructive": false,
                "args": { "query": "string" },
                "flags": {
                    "top_k": "integer",
                    "kb": "string; omit to search EVERY KB. '*' is an explicit all-KB search",
                    "all_kbs": "boolean; force all KBs, overriding --kb and retrieval.default_kb",
                    "diary": "string|*",
                    "mode": "evidence|raw|wiki|explore|agent",
                    "profile": "cloud_quality|fast|offline",
                    "layer": "identity|critical|topic|deep (default topic)",
                    "rerank_top_n": "integer; candidates sent to rerank (default from profile)",
                    "graph_hops": "1|2; graph expansion depth",
                    "allow_mixed_embeddings": "boolean",
                    "since": "rfc3339; earliest event_time",
                    "until": "rfc3339; latest event_time"
                },
                "output_data": { "status": "success|no_results", "answer_context": "string", "results": "array", "kb": "string — effective scope, '*' when every KB was searched", "kb_scope": "flag|config|all_kbs", "diary_isolation": "object|null — sources withheld by diary isolation" },
                "metadata": { "kb": "string", "kb_scope": "flag|config|all_kbs", "diary": "string", "diary_isolation": "object (only when something was withheld)", "candidates_considered": "integer" },
                "agent_rule": "Never add --kb just to find something: reads default to every KB precisely because the write side chose the namespace. If a recall comes back empty, check metadata.kb_scope before concluding the memory is absent."
            },
            "people show": {
                "destructive": false,
                "args": { "name": "person name or any known alias (case-insensitive)" },
                "flags": { "kb": "string|*", "snippets": "integer" },
                "output_data": { "person": "object with canonical_name + aliases", "relations": "array — typed edges with provenance", "memories": "array of snippets that mention them" }
            },
            "facts backfill": {
                "destructive": false,
                "requires": ["--kb"],
                "flags": { "kb": "string (required)", "diary": "string|*", "limit": "integer", "dry_run": "boolean" },
                "cost": "one LLM call per unprocessed memory; exits 2 without an OpenRouter key and respects `engram budget`",
                "idempotency": "memories with existing facts or an extraction marker are skipped; oldest first, so a --limit'ed run resumes where the last stopped",
                "output_data": { "processed": "integer", "facts_added": "integer", "skipped": "integer" }
            },
            "kb repair": {
                "destructive": false,
                "safe_mode": "--dry-run",
                "repairs": ["KBs that hold live memories but have no `knowledge_bases` row, so `kb list` cannot see them"],
                "idempotency": "additive only; a healed store reports an empty list. Will not resurrect a KB removed by `kb delete`.",
                "output_data": { "unregistered_kbs": "string[]", "registered": "integer", "dry_run": "boolean" }
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
            "ENGRAM_DEFAULT_KB": "Pin reads to one KB when no --kb is given. Unset (the default) means recall spans every KB. Same as config `retrieval.default_kb`.",
            "ENGRAM_EMBEDDER": "Force embedder: gemini|stub (default: gemini if key set)",
            "ENGRAM_COHERE_RERANK_MODEL": "Override the Cohere rerank model slug (default rerank-v4.0-pro; e.g. rerank-v4.0-fast).",
            "ENGRAM_RERANK_PROVIDER": "Reranker selector: cohere|zerank2|none. recall/research/serve use Cohere when COHERE_API_KEY is set; set `none` to disable reranking explicitly (e.g. Cohere billing failure) — retrieval then returns hybrid-fused order.",
            "ENGRAM_LME_SPLIT": "LongMemEval split: s|oracle (default: s)"
        },
        "config_keys": {
            "retrieval.default_kb": "Unset by default, which means reads span EVERY KB. Set it to pin `recall` / `memory search` to one KB when no --kb is given; `--kb <name>` and `--all-kbs` still override it. Env: ENGRAM_DEFAULT_KB.",
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
            "recall": [
                "Default to a bare `engram recall '<query>'`. It spans every KB, which is what you want when looking something up — the write side chose the namespace, not you.",
                "Add --kb only to deliberately EXCLUDE other domains (e.g. business-only competitor intel), never as a guess at where a fact lives.",
                "On an empty result, read metadata.kb_scope and metadata.kb before telling the user nothing is stored. `kb_scope: flag` or `config` means the search was narrowed.",
                "For 'when did X happen' questions, pass --since/--until: they filter on event_time, which is only populated when the write used --event-time."
            ],
            "remember": [
                "File personal facts under --kb personal, work/company facts under --kb business, project context under the project's own KB. Writing to a KB registers it.",
                "Set --event-time for anything dated — birthdays, anniversaries, when a decision was made. Without it the memory is only findable by content, never by time.",
                "Check `fact_extraction` in the response. `skipped_no_openrouter_key` means no facts were captured: either configure the key or run `engram facts backfill --kb <kb>` later.",
                "Handle `adjudication_needed` rather than ignoring it — you have the conversation context the resolver lacks."
            ],
            "people": [
                "Use `people list` / `people show` for who-do-I-know questions; they read the person-typed entity graph, not raw text.",
                "`graph neighbors <name> --typed-only` gives the real relationship graph; without it you also get weak `co_occurs_with` co-mention edges."
            ],
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
            "rerankers": ["cohere/rerank-v4.0-pro (cloud)", "passthrough (none)"],
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
            "cloud_quality": { "embedder": "gemini-embedding-2", "dims": 1536, "lexical": "FTS5 BM25", "entity_claim_search": true, "graph_expansion": true, "reranker": "cohere/rerank-v4.0-pro", "rerank_top_n": 200 },
            "fast": { "embedder": "gemini-embedding-2", "dims": 1536, "rerank_top_n": 20 },
            "offline": { "embedder": "stub", "lexical": "FTS5 BM25", "reranker": "none", "rerank_top_n": 50 }
        },
        "examples": [
            "engram recall 'what did I decide about the pricing model?' --json",
            "engram recall \"Sarah's kids\" --json",
            "engram remember 'Sarah prefers async standups; sync only for incidents.' --kb personal --importance 8",
            "engram remember \"Mum's birthday is 14 March.\" --kb personal --event-time 1958-03-14 --importance 9",
            "engram remember 'Chose Postgres over DynamoDB: relational reporting mattered more than write throughput.' --kb business --importance 9 --tag decision",
            "engram recall 'what happened last March?' --since 2026-03-01 --until 2026-03-31 --json",
            "engram recall 'competitor pricing' --kb business --json",
            "engram people list --kb '*' --min-mentions 2 --json",
            "engram people show 'Sarah Chen' --kb '*' --json",
            "engram graph neighbors 'Sarah Chen' --kb personal --typed-only --limit 20 --json",
            "engram facts show 'Sarah Chen' --kb '*' --json",
            "engram facts backfill --kb personal --dry-run --json",
            "engram facts conflicts --open --json",
            "engram kb list --json",
            "engram kb repair --dry-run --json",
            "engram bench personal --json",
            "engram doctor --integrity --json",
            "engram skill install",
            "engram skill package --out engram-skill.zip",
            "engram serve --host 127.0.0.1 --port 8768",
            "engram ingest ./paper.pdf --kb ageing-biology --mode papers --compile evidence",
            "engram ingest ./papers --kb ageing-biology --mode papers --dry-run --include '*.pdf' --json",
            "engram compile --kb ageing-biology --all --llm --max-llm-chunks 25",
            "engram research 'what human evidence exists for rapamycin dosing?' --kb ageing-biology --json"
        ],
        "benchmarks": {
            "longmemeval_s": {
                "url": "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned",
                "description": "500 questions × ~48 sessions/q, 96% distractors",
                "engram_R@1": 0.91,
                "engram_R@5": 0.99,
                "engram_R@10": 0.998
            }
        },
        "framework": "https://github.com/paperfoot/agent-cli-framework"
    });

    // Raw JSON — NOT wrapped in envelope. Framework principle #1.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&manifest).unwrap());
    Ok(())
}
