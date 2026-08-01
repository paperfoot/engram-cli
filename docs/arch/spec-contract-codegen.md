# spec-contract-codegen.md — the contract half of move 5

Lane: **contract-codegen**. Wave: **2** (skeleton + everything in this spec except the wave-3
call-site migrations, which are listed as cross-lane patches).

Reads that bind this spec, in precedence order: `docs/arch/interfaces.md` v2 (rulings R1–R15
override everything below them), `docs/arch/coherence-report.md` (CM-8, CM-9, CM-10, CM-12,
CM-13, CD-4, CD-5, CD-6, CD-10, CW-5, CW-6, CW-7, CC-3, CC-5, IN-4, IN-5, IN-23, IN-24, IN-25,
IN-29, IN-30), `docs/arch/redesign-brief.md` (move 5, hard constraints 1–8),
`docs/arch/spec-fact-semantics.md` (§1.4, §2.3.1, §6.1, §7 risks 8–10 — aligned where we touch).

Owned files (exclusive):

| path | state |
|---|---|
| `crates/engram-contract/**` | NEW crate |
| `crates/engram-cli/src/agent_info.rs` | 453 lines today; becomes a ~40-line shim |
| `crates/engram-cli/src/cli.rs` | 980 lines |
| `crates/engram-cli/src/error.rs` | 118 lines |
| `crates/engram-cli/src/output.rs` | 104 lines |
| `crates/engram-cli/src/respond.rs` | NEW (ruling R12) |
| `crates/engram-cli/assets/skill.md` | 131 lines; becomes a generated artefact (path per interfaces §7) |
| `crates/engram-core/src/error.rs` | 12 lines; gains `ErrorCode` (R11 says "`error.rs` (contract-codegen) implements"; interfaces §0 puts `ErrorCode` in `engram_core::error`) |
| `crates/engram-cli/tests/contract_*.rs` + `tests/contract_support/mod.rs` | NEW |

---

## 1. Scope & non-goals

### 1.1 In scope

1. **`engram-contract`, a schema-as-data crate.** One `Contract` value describes every command,
   flag, default, output field, status value, exit code and enumerated vocabulary. Three
   generators read it: `agent_info` JSON, `assets/skill.md`, and JSON Schema per command.
2. **`agent_info.rs` stops being the source of truth** and becomes a renderer over the contract
   plus a small injected `RuntimeContext` (paths, model slugs, `ANSWER_CONTEXT_FORMAT`).
3. **skill.md becomes generated** from `crates/engram-contract/templates/skill.md.tmpl`, with
   hand-written prose preserved verbatim and only marked blocks regenerated (§3.4). Its
   front-matter carries `schema_hash`.
4. **Skill staleness detection.** `engram_contract::schema_hash()` is a stable hash of the
   contract data; `doctor` compares it against the hash in each installed `SKILL.md`.
5. **Contract tests** (`tests/contract_*.rs`) that assert every documented default, status value,
   exit code, error code, flag name and output field **against the built binary**, in a
   `ENGRAM_STORE` TempDir.
6. **Clap errors through the JSON envelope with exit 3**, via `Cli::try_parse` in a function this
   lane owns (`cli::parse_or_exit`), keeping `--help`/`--version` on stdout at exit 0 and keeping
   the landed "clap errors never open a store" property.
7. **`ErrorCode` in `engram_core::error`** plus the `StorageError → ErrorCode` mapping table
   (R11), so `StorageError::NotFound` stops exiting 1 and starts exiting 3.
8. **Real suggestion strings everywhere** — every `ErrorCode` yields a suggestion naming a
   runnable command, not `None` and not "Run `engram --help`".
9. **Flag vocabulary reconciliation** — `--top-k` vs `--limit` given one rule and one alias
   direction (§4.6).
10. **`crates/engram-cli/src/respond.rs`** (R12): the shared
    `resolve { program, args, allowed_actions }` + legacy `resolve_command` builder, landed in
    wave 2, called by all four producers.

### 1.2 Non-goals

- **Not** rewriting skill.md's prose. The template keeps the existing voice byte-for-byte; only
  machine facts move into generated blocks. (Brief constraint 6.)
- **Not** adding a JSON-Schema *validator* dependency. Contract tests walk the emitted JSON with a
  hand-rolled `assert_shape` helper. Adopting `jsonschema` is an open question for the lead (§7.1).
- **Not** generating clap itself. `cli.rs` stays hand-written clap derive; the contract *asserts*
  agreement with it rather than emitting it. Deriving clap from data would need a proc-macro crate
  and would fight `clap::Parser`'s ergonomics for no agent-visible gain. The agent-visible property
  ("documentation and behaviour cannot drift") is delivered by the contract tests.
- **Not** owning the `--store` flag. Deferred to wave 4 by R14/CD-5; the contract documents
  `ENGRAM_STORE` only, and `ENGRAM_HOME` must appear nowhere (CM-13).
- **Not** implementing `allowed_actions()` semantics. That is fact-semantics'
  (`engram_graph::semantics::allowed_actions`, spec-fact-semantics §2.3.1); respond.rs takes the
  list as a parameter (R12) and depends on no classifier.
- **Not** touching `doctor.rs` (frozen, interfaces §7). The staleness check lands there as a
  one-line cross-lane patch over a function this lane ships.
- **Not** adding `engram state` / `engram admin` clap subcommands in wave 2. Their `cli.rs` arms
  are pre-agreed here (§4.7) and land when their lanes do (CD-4).

---

## 2. Data model / API changes

### 2.0 SQL DDL

**None.** This lane adds no table, column, index or migration step. It reads
`SqliteStore::schema_version()` only through `doctor` (which is another lane's file) and through
the contract's documented value `schema.version`, asserted against the binary by
`contract_schema_version` (§6.5). Store version bumps are storage-journal's; the contract test is
the tripwire that the *documentation* moved with them.

**Sequencing (constraint 4).** The live store is still at **9** —
`crates/engram-storage/src/sqlite.rs:792`, `const CURRENT_VERSION: i32 = 9` — and `skill.md:125`
still says "Schema is v9". So `contract().schema_version` is **not** hard-coded to `10`. It reads
`engram_storage::CURRENT_VERSION` (cross-lane patch 2f: make that constant `pub`), which means:

- while storage-journal's v10 migration is unmerged, the generated skill and manifest say `9`,
  which is the truth;
- the wave the migration lands, the constant moves and the generated artefacts move with it —
  `skill_asset_is_freshly_generated` fails until someone reruns the generator, which is exactly
  the tripwire this lane exists to provide;
- `contract_schema_version` never encodes a literal, so it cannot fail the wave-2 workspace gate
  on another lane's merge order.

If the lead prefers `engram-contract` to stay free of an `engram-storage` dependency (§8.1 risk 6
argues it should), the constant is threaded through `RuntimeContext` instead — the rule that
matters is *derive it, never retype it*.

### 2.1 The new crate: `crates/engram-contract`

```toml
# crates/engram-contract/Cargo.toml
[package]
name = "engram-contract"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
uuid.workspace = true          # schema_hash via Uuid::new_v5, per the R9 precedent

[[bin]]
name = "engram-contract-gen"   # dev-time generator; writes assets/skill.md
path = "src/bin/gen.rs"
```

Dependency direction is one-way and deliberate: `engram-contract` depends on **nothing in the
workspace**. `engram-cli` depends on `engram-contract`. Everything runtime-varying
(`config_path_peek()`, model slugs, `ANSWER_CONTEXT_FORMAT`, `CARGO_PKG_VERSION`) is injected as
`RuntimeContext`, so the contract crate stays pure, fast to test, and impossible to make circular.

Workspace registration (`Cargo.toml` `[workspace] members` + `[workspace.dependencies]`) is
a **cross-lane patch** — the workspace manifest belongs to the lead (CC-5/IN-29). See §6 (patch 1).

#### 2.1.1 Schema-as-data types (`crates/engram-contract/src/model.rs`)

```rust
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Ty {
    Str, Bool, Int, Uuid, Rfc3339, Float,
    Enum(&'static [&'static str]),
    Array(&'static Ty),
    Object,
    Nullable(&'static Ty),
}

/// One flag as the agent sees it. `name` is the canonical long form WITHOUT
/// dashes and in snake_case — the JSON/daemon spelling. `cli_long` is what
/// clap registers; they differ only in `-` vs `_`, and a contract test proves it.
#[derive(Debug, Clone, Serialize)]
pub struct FlagSpec {
    pub name: &'static str,
    pub cli_long: &'static str,
    pub aliases: &'static [&'static str],
    pub ty: Ty,
    pub default: Option<&'static str>,
    pub required: bool,
    pub doc: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArgSpec { pub name: &'static str, pub ty: Ty, pub required: bool, pub doc: &'static str }

#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec { pub name: &'static str, pub ty: Ty, pub doc: &'static str,
                       /// false when the field is present only in some responses
                       /// (embedder_degraded, temporal_scope, absence).
                       pub always: bool }

#[derive(Debug, Clone, Serialize)]
pub struct CommandSpec {
    pub path: &'static str,                 // "facts resolve", "memory add", "remember"
    pub aliases: &'static [&'static str],
    pub summary: &'static str,              // one line, agent-facing
    pub destructive: bool,
    pub requires: &'static [&'static str],  // ["--confirm"]
    pub opens_store: bool,                  // false for agent-info (LANDED behaviour)
    pub args: &'static [ArgSpec],
    pub flags: &'static [FlagSpec],
    pub data: &'static [FieldSpec],         // envelope data.*
    pub metadata: &'static [FieldSpec],     // envelope metadata.*
    pub statuses: &'static [&'static str],
    pub exit_codes: &'static [u8],          // the codes this command can actually produce
    pub rules: &'static [&'static str],     // agent_rule / temporal_rule prose
    pub examples: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvVarSpec { pub name: &'static str, pub doc: &'static str, pub implemented: bool }

#[derive(Debug, Clone, Serialize)]
pub struct Contract {
    pub contract_version: &'static str,     // "contract@1"
    pub schema_version: u32,                // store schema the binary expects: 10
    pub envelope: EnvelopeSpec,
    pub commands: &'static [CommandSpec],
    pub global_flags: &'static [FlagSpec],
    pub exit_codes: &'static [(u8, &'static str)],
    pub error_codes: &'static [ErrorCodeSpec],
    pub vocabularies: &'static [(&'static str, &'static [&'static str])],
    pub env_vars: &'static [EnvVarSpec],
    pub config_keys: &'static [(&'static str, &'static str)],
    pub decision_rules: &'static [(&'static str, &'static [&'static str])],
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorCodeSpec { pub code: &'static str, pub exit: u8, pub retryable: bool,
                           pub suggestion: &'static str, pub doc: &'static str }

#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeSpec {
    pub version: &'static str,              // "1"
    pub success_shape: &'static str,
    pub error_shape: &'static str,
    pub listing_fields: &'static [FieldSpec],   // returned / total / cursor / count (IN-23)
    pub scope_fields: &'static [FieldSpec],     // metadata.scope { kbs, diary, chunks_considered }
    pub verdict_values: &'static [&'static str],// ["results", "no_match"]
    pub absence_fields: &'static [FieldSpec],   // top_score / threshold / searched_kbs / searched_chunks
}
```

#### 2.1.2 The registry and its accessors (`crates/engram-contract/src/lib.rs`)

```rust
pub mod model;
pub mod registry;      // the const data — one `pub static` per command family
pub mod agent_info;    // generator 1
pub mod skill;         // generator 2 + front-matter parsing + staleness
pub mod jsonschema;    // generator 3
pub mod runtime;       // RuntimeContext

pub use model::*;

/// The one contract value. Everything else in this crate is a view over it.
pub fn contract() -> &'static Contract;

/// `Some` when the path names a real command; `None` is a caller bug, not an error.
pub fn command(path: &str) -> Option<&'static CommandSpec>;

/// Every enumerated vocabulary the contract pins, by name:
/// "suggested_action", "resolve_action", "status", "verdict", "fact_extraction",
/// "kb_scope", "reextraction", "close_reason", "close_authority", "handle_kind",
/// "error_code", "autonomy", "profile", "mode", "layer".
pub fn vocabulary(name: &str) -> Option<&'static [&'static str]>;

/// Canonical JSON of `contract()` with keys sorted — the hashing pre-image and
/// the thing a diff should be read against.
pub fn canonical_json() -> String;

/// Stable content hash of the contract. NO new dependency: `Uuid::new_v5` over
/// `canonical_json()`, the same primitive R9 pinned for `object_pair_hash`
/// after rejecting blake3. Returned simple-hex (32 chars, no dashes).
pub const CONTRACT_NAMESPACE: uuid::Uuid =
    uuid::uuid!("6f3d2b1e-9a44-4f6d-8e21-1c7b5a0d93f2");
pub fn schema_hash() -> String;
```

`registry.rs` is the file a future agent edits to change agent-visible behaviour. It is data, so a
diff on it reads as a changelog. Ordering inside `commands` is the order agent-info prints, which
is the order skill.md prints, which is deterministic.

#### 2.1.3 `RuntimeContext` (`crates/engram-contract/src/runtime.rs`)

```rust
/// Values the contract cannot know because they depend on this machine or on
/// crates engram-contract deliberately does not depend on. All borrowed —
/// building one is allocation-free at the call site except for the paths.
pub struct RuntimeContext<'a> {
    pub version: &'a str,                    // env!("CARGO_PKG_VERSION") from engram-cli
    pub config_path: &'a str,
    pub data_path: &'a str,
    pub vector_path: &'a str,
    pub cache_path: &'a str,
    pub answer_context_format: &'a str,      // crate::retrieval::ANSWER_CONTEXT_FORMAT
    pub extraction_model: &'a str,           // engram_llm::openrouter::DEFAULT_EXTRACTION_MODEL
    pub synthesis_model: &'a str,            // engram_llm::openrouter::DEFAULT_SYNTHESIS_MODEL
}

/// Deterministic stand-in used by generator + tests: every path is a fixed
/// sentinel, so skill.md and the golden manifest are machine-independent.
impl<'a> RuntimeContext<'a> { pub fn fixture() -> RuntimeContext<'static>; }
```

#### 2.1.4 Generators

```rust
// crates/engram-contract/src/agent_info.rs
/// Renders the FULL agent-info manifest. Field names and nesting are
/// byte-compatible with 0.3.1's manifest (constraint 3: existing JSON fields
/// keep their names and types); new keys are additive.
pub fn render(rt: &RuntimeContext<'_>) -> serde_json::Value;

// crates/engram-contract/src/skill.rs
/// Substitutes generated blocks into the template. Returns the exact bytes
/// that must appear at crates/engram-cli/assets/skill.md.
pub fn render(rt: &RuntimeContext<'_>) -> String;
/// Reads `schema_hash:` out of a SKILL.md YAML front-matter. Pure; no I/O.
pub fn parse_schema_hash(body: &str) -> Option<String>;
#[derive(Serialize)] pub enum SkillFreshness { Fresh, Stale { installed: String }, Missing, Unreadable }
/// Compares an installed SKILL.md against this binary's contract.
pub fn freshness(installed_body: Option<&str>) -> SkillFreshness;

// crates/engram-contract/src/jsonschema.rs
/// JSON Schema 2020-12 for one command: `input` (args+flags) and `output`
/// (the success envelope for that command). `None` for an unknown path.
pub fn schema_for(path: &str) -> Option<serde_json::Value>;
/// { "<command path>": <schema>, ... } for every command.
pub fn all_schemas() -> serde_json::Value;
```

### 2.2 `ErrorCode` in `engram_core::error` — PARTLY LANDED

Storage-journal landed the enum, `exit_code()` and `as_str()` in
`crates/engram-core/src/error.rs` during wave 2 (verified on disk: nine variants, the R11 exit
mapping exactly as interfaces §4 pins it, `Serialize + Deserialize`, one unit test). **Do not
re-declare it.** Two deltas remain, and both are one-liners in that file:

1. **Two wire strings must change back to their 0.3.1 spellings — in `as_str()` *and* in serde.**
   The landed `as_str()` returns `"config"` and `"transient"`; the shipped binary emits
   `"config_error"` and `"transient_error"` (`crates/engram-cli/src/error.rs:70-71`), and hard
   constraint 3 freezes existing JSON *values* that agents already branch on. Every other landed
   spelling is new and correct.

   The enum also derives `Serialize + Deserialize` with `#[serde(rename_all = "snake_case")]`
   (`crates/engram-core/src/error.rs:22`), so **serde emits `"config"` / `"transient"` too**, and
   any path that serializes the enum rather than calling `as_str()` re-breaks constraint 3 by a
   second route. The patch must fix both:

   ```rust
   #[serde(rename = "config_error")]    Config,
   #[serde(rename = "transient_error")] Transient,
   ```

   `contract_errors.rs` pins the equivalence directly:
   `serde_json::to_value(c).unwrap() == json!(c.as_str())` for every `ErrorCode::ALL` member, so
   the two spellings can never drift again. → cross-lane patch 2a (§7).
2. **Two additive members** this lane's generator and tests need:

```rust
impl ErrorCode {
    /// True only for Transient, SqliteBusy, RateLimited (exits 1 and 4).
    /// The manifest publishes it so an agent can branch without a code table.
    pub fn retryable(self) -> bool;
    /// Exhaustive, so `contract_errors.rs` can prove the contract documents
    /// every variant instead of the nine it happened to think of.
    pub const ALL: [ErrorCode; 9];
}
```
→ cross-lane patch 2b (§7). If the lead prefers, this lane lands both patches itself under R11's
"`error.rs` (contract-codegen) implements" clause; the patch channel is the safer default because
storage-journal is actively editing the file.

`engram-core` must not learn about `engram-storage`, so the `StorageError → ErrorCode` mapping
lives in `engram-cli` (§2.3). The IN-5 re-export is **already landed**:
`crates/engram-core/src/lib.rs:17` reads `pub use error::{CoreError, ErrorCode};` today, so
former patch 2c is a no-op — verify on disk and delete it rather than re-applying it (§7).

### 2.3 `crates/engram-cli/src/error.rs` — mapping, exits, suggestions

```rust
use engram_core::ErrorCode;

#[derive(Debug, Error)]
pub enum CliError {
    // --- existing variants, unchanged spelling and payloads ---
    Transient(String), Config(String), BadInput(String), RateLimited(String),
    Storage(#[from] engram_storage::StorageError),
    Embed(#[from] engram_embed::EmbedError),
    Rerank(#[from] engram_rerank::RerankError),
    Ingest(#[from] engram_ingest::IngestError),
    Bench(#[from] engram_bench::BenchError),
    Io(#[from] std::io::Error),
    Serde(#[from] serde_json::Error),
    Zip(#[from] zip::result::ZipError),

    // --- NEW, all additive ---
    /// A handle or id that resolves to nothing. Exit 3 on EVERY mutator,
    /// `forget` included (interfaces §1).
    #[error("not found: {what} {handle}")]
    NotFound { what: &'static str, handle: String },

    /// The handle exists but names the wrong kind of thing. Names the actual
    /// kind AND the handle the caller should have passed.
    #[error("wrong id kind: {handle} is a {actual}, this command needs a {expected}")]
    WrongIdKind { handle: String, actual: String, expected: String, correct: Option<String> },

    /// `facts resolve` lost the claim race: someone else already adjudicated.
    #[error("conflict {conflict_id} was already resolved ({verdict})")]
    ConflictAlreadyResolved { conflict_id: String, verdict: String },

    /// Caller-supplied JSON that did not parse or did not match the shape
    /// (--facts-json, an HTTP body). Distinct from `Serde`, which is ours.
    #[error("malformed json in {field}: {detail}")]
    MalformedJson { field: &'static str, detail: String },

    /// SQLITE_BUSY / "database is locked" — retryable, unlike other storage errors.
    #[error("store busy: {0}")]
    SqliteBusy(String),
}

impl CliError {
    /// The single source of truth for EXITS. `exit_code()` derives from this,
    /// so a new variant cannot get an exit status by accident.
    /// `code()` does NOT derive from it — see the two-map rule below.
    pub fn error_code(&self) -> ErrorCode;
    pub fn exit_code(&self) -> i32 { self.error_code().exit_code() }
    pub fn code(&self) -> &'static str;      // legacy-preserving; see the two-map rule and §5.2
    /// Owned, because a useful suggestion names the caller's own id/handle:
    /// "run `engram facts show <subject> --kb '*'`", not "see --help".
    pub fn suggestion(&self) -> Option<String>;
    pub fn emit_to_stderr(&self);            // unchanged shape; `code` now from error_code()
}

/// R11's table, implemented once. Borrowed, so `From<&StorageError>` cannot
/// consume an error the caller still needs to report.
impl From<&engram_storage::StorageError> for ErrorCode {
    fn from(e: &engram_storage::StorageError) -> ErrorCode {
        use engram_storage::StorageError as S;
        match e {
            S::NotFound(_)  => ErrorCode::NotFound,
            S::Migration(_) => ErrorCode::Config,
            S::Serde(_)     => ErrorCode::MalformedJson,
            S::Io(_)        => ErrorCode::Transient,
            S::Sqlite(e) if is_busy(e) => ErrorCode::SqliteBusy,
            S::Sqlite(_)    => ErrorCode::Transient,
        }
    }
}

/// `rusqlite::Error::SqliteFailure(ffi::Error { code: DatabaseBusy | DatabaseLocked, .. }, _)`,
/// plus the message fallback `"database is locked"` for the wrapped-string cases
/// R11 names explicitly.
fn is_busy(e: &rusqlite::Error) -> bool;
```

**The two-map rule (do not collapse these).** `error_code()` and `code()` answer different
questions and must be written as two separate `match`es over `CliError`:

- `error_code() -> ErrorCode` is the **taxonomy**: what an agent should DO. It is total over
  `CliError` and drives `exit_code()` and the `retryable` flag. It is what `interfaces.md` §4 and
  R11 pin.
- `code() -> &'static str` is the **wire value**: what 0.3.1 already emits in `error.code`, frozen
  by hard constraint 3 (§5.2). Seven of those strings — `embed_error`, `rerank_error`,
  `ingest_error`, `bench_error`, `io_error`, `serde_error`, `zip_error` — have no `ErrorCode`
  variant and never will, because they name a *subsystem*, not an action. Deriving `code()` from
  `error_code()` would silently rename all seven and break constraint 3.

So: for the five NEW variants and for `Storage`, `code()` returns `self.error_code().as_str()`.
For the seven legacy subsystem variants it returns the frozen literal. `contract_errors.rs` pins
both directions (`legacy_wire_strings_are_unchanged` over the eleven 0.3.1 strings;
`new_variants_use_error_code_as_str` over the rest).

**`CliError → ErrorCode`, total.** §2.3 previously specified only `StorageError`; the other
variants carry logic that exists in the shipped binary today (`error.rs:54-63`) and would be lost
if an implementer only wrote R11's storage table:

| `CliError` variant | `ErrorCode` | note |
|---|---|---|
| `Transient` | `Transient` | |
| `Config` | `Config` | |
| `BadInput` | `BadInput` | |
| `RateLimited` | `RateLimited` | |
| `Storage(e)` | `ErrorCode::from(e)` | R11's table above |
| `Embed(EmbedError::MissingKey{..})` | `Config` | **preserves today's exit 2**; the highest-traffic setup failure ("no Gemini key") must keep sending the caller to `config check`, not to a retry loop |
| `Embed(EmbedError::RateLimited{..})` | `RateLimited` | preserves today's exit 4 |
| `Embed(_)` | `Transient` | |
| `Rerank(RerankError::MissingKey{..})` | `Config` | same, preserves exit 2 |
| `Rerank(RerankError::RateLimited{..})` | `RateLimited` | preserves exit 4 |
| `Rerank(_)` | `Transient` | |
| `Ingest` / `Bench` | `BadInput` | preserves today's exit 3 |
| `Io` / `Serde` / `Zip` | `Transient` | preserves today's exit 1. `Serde` here is *our* serialization, not caller JSON — caller JSON is the new `MalformedJson` variant |
| `NotFound` / `WrongIdKind` / `ConflictAlreadyResolved` / `MalformedJson` / `SqliteBusy` | the same-named `ErrorCode` | |

Every exit code in that table equals the one the 0.3.1 binary produces, so the only observable
exit changes are the two listed in §5.1 (clap 2→3, `StorageError::NotFound` 1→3).
`exit_codes_are_unchanged_except_the_two_documented` asserts exactly that, variant by variant.

`is_busy` needs `rusqlite` visible to `engram-cli`. It is not a direct dependency today. Rather
than add one, storage-journal exposes the predicate: cross-lane patch 3 (§7) adds
`impl StorageError { pub fn is_busy(&self) -> bool }` to `crates/engram-storage/src/error.rs`, and
this lane calls it. That is one method in their file and keeps rusqlite out of the CLI's manifest.

### 2.4 `crates/engram-cli/src/respond.rs` (NEW — ruling R12)

```rust
//! The one place a "here is how you fix this" command is built.
//!
//! Four surfaces used to format the same string four ways, and every one of
//! them shipped `--action supersede-old|keep-both|reject-new` — a string with a
//! shell PIPE in it, so the "ready-to-run command" agent-info promises was
//! never runnable. One builder, one runnable command, and the alternatives move
//! to `allowed_actions` where a machine can read them.

use serde_json::{Map, Value};
use uuid::Uuid;

pub const PROGRAM: &str = "engram";

/// What a caller may do with an open conflict. `RequiresContext` is a
/// SUGGESTION, never an allowed action — you cannot pass it to `--action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveAction { KeepBoth, SupersedeOld, RejectNew }

impl ResolveAction {
    pub fn as_str(self) -> &'static str;       // "keep-both" | "supersede-old" | "reject-new"
    pub fn parse(s: &str) -> Option<Self>;
    /// Order matters: element 0 is what the legacy `resolve_command` string runs.
    /// `keep-both` first because it is the only universally SAFE default —
    /// it never closes an interval (move 2), so an agent that runs the command
    /// blind cannot destroy a medication record.
    pub const ALL: [ResolveAction; 3] = [Self::KeepBoth, Self::SupersedeOld, Self::RejectNew];
}

#[derive(Debug, Clone)]
pub struct ResolvePayload {
    pub program: &'static str,
    pub args: Vec<String>,
    pub allowed_actions: Vec<&'static str>,
}

/// The conflict-adjudication payload. `allowed_actions` comes FROM THE CALLER
/// (fact-semantics' `allowed_actions(v, new, old)`), so this module depends on
/// no classifier — R12, verbatim.
pub fn resolve_conflict(conflict_id: Uuid, allowed_actions: &[&'static str]) -> ResolvePayload;

/// A mutator that was handed the wrong handle kind: the command the caller
/// should run instead. Used by CliError::WrongIdKind's suggestion and by the
/// read surfaces that return chunk ids.
pub fn retry_with_handle(command_path: &str, correct_handle: &str, extra: &[&str]) -> ResolvePayload;

/// `{ "program": "engram", "args": [...], "allowed_actions": [...] }`
pub fn resolve_json(p: &ResolvePayload) -> Value;

/// ONE runnable command, no pipes, shell-quoted where a value could contain a
/// space. Uses `allowed_actions[0]`; empty `allowed_actions` is a caller bug and
/// yields the args verbatim with no `--action`.
pub fn resolve_command_string(p: &ResolvePayload) -> String;

/// Inserts BOTH `resolve` (new) and `resolve_command` (legacy string) into an
/// existing object. The single call site producers use, so the two can never
/// disagree.
pub fn attach(obj: &mut Map<String, Value>, p: &ResolvePayload);
```

Worked example — what all four producers emit after this lane:

```json
"suggested_action": "requires_context",
"resolve": {
  "program": "engram",
  "args": ["facts", "resolve", "3f1c…", "--action", "keep-both"],
  "allowed_actions": ["keep-both", "supersede-old", "reject-new"]
},
"resolve_command": "engram facts resolve 3f1c… --action keep-both"
```

### 2.5 `cli.rs` — parse-or-envelope

```rust
// crates/engram-cli/src/cli.rs, appended below the derives.

/// Parse argv, or print a JSON error envelope and exit 3.
///
/// `Cli::parse()` prints clap's own prose to stderr and exits 2 — a code that
/// means "config error, do not retry" to every agent reading our own
/// exit-code table, for what is always a bad ARGUMENT. `--help` and
/// `--version` still go to stdout at exit 0, because those are successes.
/// Runs before any store is opened, preserving the landed guarantee.
pub fn parse_or_exit() -> Cli { parse_from_or_exit(std::env::args_os()) }

/// Testable half: same behaviour, injectable argv. `Err(code)` is the exit the
/// caller must take; unit tests assert on it without a process boundary.
pub fn parse_from<I, T>(argv: I) -> Result<Cli, ParseOutcome>
where I: IntoIterator<Item = T>, T: Into<std::ffi::OsString> + Clone;

pub enum ParseOutcome {
    /// clap rendered help or version; print `text` to STDOUT, exit 0.
    Rendered { text: String },
    /// Anything else: emit the envelope, exit 3.
    Invalid { message: String, suggestion: String },
}

/// Classify one `clap::Error` without owning a `Cli`. The seam every OTHER
/// clap parser in the workspace uses — `engramd`'s `Args` (cross-lane patch 6)
/// most of all, so the daemon and the CLI cannot drift on the same typo.
pub fn classify(e: &clap::Error) -> ParseOutcome;

/// Terminal half of `parse_or_exit`, exposed so patch 6 is one line and not a
/// re-derivation: render `Rendered` to stdout and exit 0, or emit the error
/// envelope to stderr and exit 3. Diverges.
pub fn emit_parse_failure(e: clap::Error) -> !;
```

`parse_from` is `classify` plus `Cli::try_parse_from`; `parse_or_exit` is `parse_from` plus
`emit_parse_failure`. Nothing else in the workspace may call `clap::Parser::parse()` directly —
`contract_errors::no_bare_clap_parse` greps `crates/*/src/**` for `::parse()` on a `Parser` and
fails on a new one, because a second bare parser is how the exit-2 bug comes back.

Rendering rule: `clap::error::ErrorKind::DisplayHelp | DisplayVersion |
DisplayHelpOnMissingArgumentOrSubcommand` → `Rendered`; every other kind → `Invalid` with
`ErrorCode::BadInput`, exit 3. The suggestion is built from the failing context, not boilerplate:
`UnknownArgument` → ``"`engram <command> --help` lists the flags this command accepts"``;
`InvalidSubcommand` → ``"run `engram agent-info` for the full command list"`` plus the `strsim`
nearest match already vendored in `engram-cli`'s dependency list;
`MissingRequiredArgument` / `InvalidValue` → the command's own help line.

Emission uses the same writer as `CliError::emit_to_stderr` so the envelope is identical in shape
(`{version, status:"error", error:{code, message, suggestion, exit_code}}`) and, exactly as today,
is JSON only when stderr is not a terminal.

**The TTY decision is one function, and it is injectable.** `emit_to_stderr` calls
`std::io::stderr().is_terminal()` inline today, which makes the prose path untestable — CI stderr
is a pipe, so `clap_error_on_tty_is_prose` can never observe the human branch from a child
process. Extract it:

```rust
// error.rs
pub(crate) fn stderr_is_tty() -> bool {
    match std::env::var("ENGRAM_FORCE_TTY").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => std::io::stderr().is_terminal(),
    }
}
```

`ENGRAM_FORCE_TTY` is a test seam, documented in the manifest's `env_overrides` as
`"testing only; forces the human/JSON stderr choice"`. `clap_error_on_tty_is_prose` sets it to
`1` and asserts stderr does not parse as JSON; `clap_error_is_json_and_exit_3` sets it to `0` so
the JSON branch is proven rather than inherited from however CI happens to wire stderr.

### 2.6 `agent-info --schema`

```rust
// cli.rs
/// Print this manifest (raw JSON, not enveloped).
#[command(visible_alias = "info")]
AgentInfo {
    /// Emit the JSON Schema (2020-12) for ONE command instead of the manifest,
    /// e.g. `engram agent-info --schema 'facts resolve'`. `--schema all`
    /// emits every schema keyed by command path.
    #[arg(long)]
    schema: Option<String>,
},
```

This changes the shape of a variant `lib.rs` matches on twice (`lib.rs:24`, `:31`) — cross-lane
cross-lane patch 5 (§7). The store-free guarantee is unaffected: both arms still return before
`AppContext::new`.

### 2.7 `output.rs` — envelope additions (IN-23)

`output.rs` gains helpers so listing commands cannot invent their own spelling of the same idea.
No existing signature changes; `print_success` / `print_envelope` / `Metadata` are untouched.

```rust
/// Listing counters, IN-23. `count` is written too and always equals
/// `returned`, because agents in the wild read it. `total` is the TRUE total
/// after scope filtering, not the page length — the field whose absence made
/// agents report 3 conflicts when 953 were open.
pub struct Listing { pub returned: usize, pub total: usize, pub cursor: Option<String> }
impl Listing {
    pub fn full(items: usize) -> Self;                       // total == returned, no cursor
    pub fn page(returned: usize, total: usize, cursor: Option<String>) -> Self;
    /// Writes returned/total/count and cursor-when-some into a data object.
    pub fn attach(&self, obj: &mut serde_json::Map<String, serde_json::Value>);
}

/// metadata.scope { kbs, diary, chunks_considered } — IN-23.
pub struct Scope { pub kbs: Vec<String>, pub diary: String, pub chunks_considered: usize }
impl Scope { pub fn attach(&self, meta: &mut Metadata); }

/// data.verdict + data.absence — the absence certificate.
pub enum Verdict { Results, NoMatch(Absence) }
pub struct Absence { pub top_score: Option<f32>, pub threshold: f32,
                     pub searched_kbs: Vec<String>, pub searched_chunks: usize }
impl Verdict { pub fn attach(&self, obj: &mut serde_json::Map<String, serde_json::Value>); }
```

Read-surfaces and write-surfaces *call* these; this lane ships them in wave 2 so wave 3 has
something to call (the same reasoning R12 applied to respond.rs).

---

## 3. File-by-file change plan

Line numbers are against the tree at commit `1d63d74` (0.3.1).

### 3.1 `crates/engram-contract/` (NEW, ~2 300 lines, mostly data)

| file | contents |
|---|---|
| `Cargo.toml` | §2.1 |
| `src/lib.rs` | `contract()`, `command()`, `vocabulary()`, `canonical_json()`, `schema_hash()`, `CONTRACT_NAMESPACE`, module decls (~120 lines) |
| `src/model.rs` | §2.1.1 types + `Serialize` impls (~180 lines) |
| `src/registry.rs` | the data: one `static` per command family, ~1 400 lines. Transcribed 1:1 from today's `agent_info.rs:16-447` — every summary string, `agent_rule`, `temporal_rule`, example and env-var doc moves here **verbatim**, then gains the new fields |
| `src/runtime.rs` | `RuntimeContext` + `fixture()` (~60 lines) |
| `src/agent_info.rs` | `render()` — walks the registry and rebuilds today's manifest key-for-key (~260 lines) |
| `src/skill.rs` | block substitution, `parse_schema_hash`, `freshness` (~200 lines) |
| `src/jsonschema.rs` | `schema_for`, `all_schemas`, `Ty` → schema mapping (~180 lines) |
| `templates/skill.md.tmpl` | today's `assets/skill.md` with six `<!-- BEGIN GENERATED: … -->` regions (§3.4) |
| `src/bin/gen.rs` | writes `crates/engram-cli/assets/skill.md`; `--check` mode exits 3 on drift (~70 lines) |

Transcription discipline: the registry is *not* a rewrite. Any prose change to an existing string
belongs in a separate commit from the move, so the diff that proves "no agent-visible regression"
stays readable.

### 3.2 `crates/engram-cli/src/agent_info.rs` — 453 lines → ~45

Delete the whole `json!({...})` literal (`:16-447`). `run()` keeps its doc comment (the store-free
guarantee is the point of the file) and becomes:

```rust
pub fn run(schema: Option<String>) -> Result<(), CliError> {
    let value = match schema.as_deref() {
        None            => engram_contract::agent_info::render(&runtime()),
        Some("all")     => engram_contract::jsonschema::all_schemas(),
        Some(path)      => engram_contract::jsonschema::schema_for(path)
            .ok_or_else(|| CliError::BadInput(format!(
                "no command named `{path}`; run `engram agent-info` for the list")))?,
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&value).unwrap());
    Ok(())
}

/// The only place engram-contract meets this machine. Uses the NON-creating
/// peek getters, exactly as the inline manifest did.
fn runtime() -> engram_contract::RuntimeContext<'static> { … }
```

The peek-getter calls at `:333-336` return `Cow`/`PathBuf`; `runtime()` leaks them into
`&'static str` via a `OnceLock<String>` per path — four allocations, once per process, and no
lifetime gymnastics at the call site.

New manifest keys added by `render()` (all additive, constraint 3):
`schema_hash`, `contract_version`, `schema_version`, `error_codes` (the R11 table with exits,
retryability and suggestions), `vocabularies`, `json_schema_command`,
`envelope.listing_fields`/`scope`/`verdict`/`absence`, and `resolve` beside every existing
`resolve_command` mention.

Corrections made while transcribing (each is a coherence-report finding, not a whim):

- `:41` "Safety slots (allergic_to, takes_medication, diagnosed_with) are never auto-superseded" →
  "Safety relations (`allergic_to`, `takes_medication`, `diagnosed_with`) hold MULTIPLE open
  values at once; a second medication is a second fact, not a conflict. Their intervals are only
  closed on your explicit instruction, never by a heuristic." (CW-6 — true in substance, false in
  vocabulary once those relations stop being slots.)
- `:41`, `:43`, `:104`, `:169` "the included resolve_command" → "the included `resolve` object
  (`program` + `args`), or the `resolve_command` string if you shell out" (CM-9/IN-24).
- `:43` recall's `conflicts` description gains the `suggested_action` vocabulary
  `keep-both | supersede-old | requires_context` and the R7 rule that safety pairs are ALWAYS
  `requires_context` (CM-8/IN-25).
- `:161` `facts list` `output_data.count` gains `returned` and `total`, with `count` documented as
  "legacy alias for `returned`" (IN-23).
- `:202-206` `forget` gains `output_data { id, deleted, handle, memory_id }`. It does **not** yet
  gain the rule "an unknown id is `not_found`, exit 3": `forget.rs:17-21` still exits 0 with
  `deleted: false`, and a manifest that promises otherwise is a documented lie an agent will
  branch on — the opposite of what this lane is for. The registry carries the rule behind a
  `since_wave: 3` marker that `render()` omits until write-surfaces lands, and the un-ignoring of
  `unknown_id_exits_3_on_every_mutator` (§6.10) and the un-marking are the **same commit**. Same
  discipline for the `returned`/`total` listing fields below: documented as `always: false` while
  read-surfaces is unmerged, flipped to `always: true` with the lane that populates them.

  This is the general rule for the whole registry: **the contract describes the binary that
  exists, never the binary that is coming.** A generated manifest that runs ahead of the code is
  worse than a hand-written one that lags it, because agents trust it more.
- `:240-246` `exit_codes` gains no new numbers (constraint 3 preserves 0/1/2/3/4) but each entry
  gains the `error_codes` that produce it.
- `:278` `ENGRAM_EMBEDDER` keeps its "NOT IMPLEMENTED" entry; `env_vars` gains `ENGRAM_STORE`
  ("the store root; overrides ProjectDirs; roots db, vectors, config and cache") and explicitly
  **never** `ENGRAM_HOME` (CM-13). A contract test greps the whole manifest for `ENGRAM_HOME`.

### 3.3 `crates/engram-cli/src/cli.rs`

| site | change |
|---|---|
| `:29-30` `AgentInfo` | gains `{ schema: Option<String> }` (§2.6) |
| `:112` `Recall.top_k` (enum arm at `:106`) | `#[arg(long, default_value_t = 10, visible_alias = "limit")]` (§4.6) |
| `:270` `Research.top_k` (enum arm at `:256`, `default_value_t = 12`) | same alias |
| `:457` `Bench.top_k` (enum arm at `:418`, `default_value_t = 0` = per-suite default) | same alias |
| `:644` `MemoryCommand::Search.top_k` (enum at `:598`) | same alias |
| `:439`, `:585`, `:690`, `:707`, `:755`, `:772`, `:799`, `:840`, `:866`, `:909`, `:940` (`limit`) | unchanged — listings do **not** gain a `--top-k` alias (§4.6) |
| end of file | `parse_or_exit` / `parse_from` / `ParseOutcome` (§2.5), plus `#[cfg(test)] mod tests` covering the three `ParseOutcome` branches |

### 3.4 `crates/engram-cli/assets/skill.md` → generated

The template is today's file with six regions replaced. Everything outside the markers — the
memory loop, the quarantine-envelope explanation, "What to save (and not)", the networking view —
is prose, stays hand-written, and the generator copies it byte-for-byte.

| marker | replaces | source |
|---|---|---|
| `<!-- BEGIN GENERATED: frontmatter-metadata -->` | `:5-8` | `schema_hash`, `contract_version`, `schema_version`, `package` |
| `<!-- BEGIN GENERATED: exit-codes -->` | `:122` first sentence | `contract().exit_codes` + `error_codes` |
| `<!-- BEGIN GENERATED: envelope -->` | `:123` | `EnvelopeSpec` incl. `returned`/`total`/`cursor`, `verdict`, `absence` |
| `<!-- BEGIN GENERATED: conflicts -->` | `:90-92` | `suggested_action` + `resolve_action` vocabularies, the `resolve` object shape |
| `<!-- BEGIN GENERATED: schema-version -->` | `:125` "Schema is v9" | `contract().schema_version` — the line that goes stale every migration |
| `<!-- BEGIN GENERATED: flag-vocabulary -->` | new section | the `--top-k` vs `--limit` rule (§4.6) |

Generation is **not** a `build.rs`. A build script that writes into `src/` breaks `cargo package`,
fights incremental rebuilds, and would make `include_str!` race its own generator. Instead the
asset is checked in, `engram-contract-gen` regenerates it, and `contract_skill.rs` fails the build
if the checked-in bytes differ from `engram_contract::skill::render(&fixture())` — the standard
"generated file is committed, a test asserts freshness" arrangement, which also keeps
`skill.rs:12`'s `include_str!("../../assets/skill.md")` working untouched.

**Where the freshness check actually runs** (otherwise "a test asserts it" is a promise nobody
keeps at release time):

1. `cargo test --workspace` — `contract_skill::skill_asset_is_freshly_generated`. Covers every
   developer and every CI run.
2. `engram-contract-gen --check` — same comparison, exit 3 on drift, no test harness. Added to
   `scripts/` next to the existing release checks and run by the release gate, because
   `cargo package`/`cargo publish` does not run tests and a stale checked-in `assets/skill.md`
   would otherwise ship compiled into the binary via `include_str!`.
3. The failure message on both paths is the same sentence: ``assets/skill.md is stale — run
   `cargo run -p engram-contract --bin engram-contract-gen``.

The failure mode this closes: someone edits `registry.rs`, `cargo build` succeeds (the generator
is a separate binary, nothing forces it to run), they publish, and every user installs a skill
describing the previous release.

### 3.5 `crates/engram-cli/src/error.rs`

Rewrite per §2.3. `emit_to_stderr()` (`:96-117`) keeps its exact envelope shape and its
`IsTerminal` rule; only `code()`/`suggestion()` feeding it change. `serve.rs:112` calls
`e.suggestion()` inside a `json!` — `Option<String>` serializes identically to
`Option<&'static str>` there, so the signature change is source-compatible at that call site
(verified: it is the only external caller).

### 3.6 `crates/engram-cli/src/respond.rs` (NEW, ~180 lines)

Per §2.4, plus `#[cfg(test)] mod tests` asserting: no `|` in any `resolve_command_string` output;
`args[0..2] == ["facts","resolve"]`; `allowed_actions` empty yields no `--action`; a value
containing a space is single-quoted.

Wave-3 migrations of the four producers are cross-lane patches (§7, patches 7–10), because
`fact_write.rs`, `facts.rs`, `recall.rs` and `serve.rs` belong to three other lanes:

| site | today | after |
|---|---|---|
| `fact_write.rs:1232` | `format!("engram facts resolve {qid} --action supersede-old\|keep-both\|reject-new")` | `respond::attach(&mut obj, &respond::resolve_conflict(qid, allowed))` |
| `commands/facts.rs:371` | same string, `row.id` | same call |
| `commands/recall.rs:345` | same string, `row.id` | same call |
| `commands/serve.rs:577-610` | bespoke JSON with no `conflicts` array at all | the array, built by the same helper |

### 3.7 `crates/engram-core/src/error.rs`

**Do not append `ErrorCode` — storage-journal already landed it** (nine variants, R11 exits,
`Serialize + Deserialize`, one unit test; verified on disk). What is left is three small deltas,
all via the patch channel because that file has an active owner: patch 2a (the two serde renames
plus the two `as_str()` spellings) and patch 2b (`retryable()`, `ALL`). `CoreError` untouched.
The `pub use error::{CoreError, ErrorCode};` re-export at `lib.rs:17` is already there — former
patch 2c is a no-op (§2.2).

### 3.8 `crates/engram-cli/src/output.rs`

Append `Listing`, `Scope`, `Verdict`, `Absence` per §2.7 (~110 lines). Nothing existing changes,
so no other lane's file needs recompiling against a new signature.

---

## 4. Vocabularies, suggestions, staleness

### 4.1 The pinned vocabularies

`engram_contract::vocabulary(name)` is the only place these live. Any lane emitting one of these
strings imports it from here; a contract test asserts the binary never emits a member outside the
set.

| vocabulary | members | authority |
|---|---|---|
| `suggested_action` | `keep-both`, `supersede-old`, `requires_context` | **R7** — safety pairs ALWAYS `requires_context`; `supersede-old` survives only for non-safety single-valued pairs that queued under `--autonomy strict`. spec-fact-semantics §1.4's blanket removal is overruled |
| `resolve_action` | `keep-both`, `supersede-old`, `reject-new` | `facts resolve --action`; `requires_context` is deliberately absent (§2.4) |
| `status` | `success`, `partial_success`, `all_failed`, `no_results`, `error` | unchanged from 0.3.1 |
| `verdict` | `results`, `no_match` | IN-23 |
| `reextraction` | `replayed`, `pending`, `none` | interfaces §4 |
| `close_reason` | `superseded`, `stopped`, `corrected`, `retracted_source` | `engram_core::interval::CloseReason` (landed) |
| `close_authority` | `explicit_end`, `caller`, `replayed_caller`, `successor_interval`, `heuristic` | `engram_core::interval::CloseAuthority` (landed) |
| `handle_kind` | `mem`, `chunk`, `fact`, `conflict`, `entity`, `doc`, `job`, `event` | `engram_core::handle::HandleKind` (landed) |
| `error_code` | the nine `ErrorCode::ALL` strings | §2.2 |
| `kb_scope` | `flag`, `config`, `all_kbs` | unchanged |
| `fact_extraction` | `ran`, `ran_stub`, `disabled`, `skipped_facts_json`, `skipped_no_openrouter_key`, `failed`, `failed_persist` | unchanged |
| `autonomy` / `profile` / `mode` / `layer` | `full\|assisted\|strict` · `cloud_quality\|fast\|offline` · `evidence\|raw\|wiki\|explore\|agent` · `identity\|critical\|topic\|deep` | clap defaults, asserted equal |

For the four clap-backed ones the contract test does not just compare lists: it *runs the binary*
with a value outside the set and asserts exit 3, so "documented set" and "accepted set" are proven
equal in both directions.

### 4.2 Real suggestion strings

Today three of twelve `CliError` variants have a suggestion, and the one for `BadInput` is
"Run `engram --help` for argument syntax" — advice an agent already had. Every `ErrorCode` gets a
suggestion that names a command the caller can actually run next.

| ErrorCode | suggestion |
|---|---|
| `BadInput` | ``run `engram agent-info --schema '<command>'` for this command's exact argument shape`` |
| `NotFound` | ``run `engram recall '<what you were looking for>'` to find the right id — ids from `recall` are chunk handles (`chunk:…`); mutators take either, and the response tells you which is which`` |
| `WrongIdKind` | built per-error by `respond::retry_with_handle`: ``that is a chunk; run `engram edit mem:<uuid> …``` with the real handle substituted |
| `Config` | ``run `engram config check` — it live-probes every configured key and names the one that failed`` |
| `Transient` | ``retry with backoff; if it repeats, `engram doctor --json` reports store and provider health`` |
| `SqliteBusy` | ``another engram process holds the write lock; retry in a moment, or stop `engram serve` if a daemon is running`` |
| `RateLimited` | ``wait and retry; `engram usage --json` shows what you have spent and `engram budget show` the caps`` |
| `ConflictAlreadyResolved` | ``someone resolved it first; run `engram facts conflicts --open --json` for what is still queued`` |
| `MalformedJson` **from caller input** (`CliError::MalformedJson`, i.e. `--facts-json`, an HTTP body) | ``check the JSON you passed in `<field>`; `engram agent-info --schema '<command>'` gives the exact shape`` |
| `MalformedJson` **from a stored row** (`StorageError::Serde`) | ``a stored row failed to deserialize — this is store corruption or a half-applied migration, not your arguments. Run `engram doctor --json`; do not retry`` |

`MalformedJson` carries **two** suggestions because R11 maps two unrelated failures onto it: a
caller who sent bad JSON, and a projection row that no longer parses. Telling the second one to
"check the JSON you passed" sends an agent to re-edit arguments that were fine, and it will never
run `doctor` — the one command that would have shown it a corrupt store. The `suggestion()` match
therefore branches on the `CliError` variant, not on the `ErrorCode`, and
`contract_errors::malformed_json_suggestions_differ` asserts the two strings are not equal.

Variant-specific overrides beat the table where the error knows more (a `NotFound` on a handle
substitutes the handle; a `Config` from `EmbedError::MissingKey` names `GEMINI_API_KEY`).
`contract_errors.rs` asserts every `ErrorCode::ALL` member produces a non-empty suggestion, and
that no suggestion is the string `Run \`engram --help\` for argument syntax.`

### 4.3 Skill staleness

`schema_hash()` covers the contract data only — not the template prose, not the version number.
That is deliberate: a typo fix in skill.md prose should not tell every user their skill is stale,
but adding a flag or changing a default should.

```
crates/engram-cli/assets/skill.md front-matter:
metadata:
  schema_hash: "a41f…"        # = engram_contract::schema_hash()
  contract_version: "contract@1"
  schema_version: 9           # = contract().schema_version, DERIVED (§2.0); 10 once storage-journal's migration lands
```

`skill install` (`commands/skill.rs`, frozen file, no change needed) writes `SKILL_BODY` verbatim,
so every installed copy carries the hash of the build that installed it.

`doctor` gains one key. `doctor.rs` is frozen (interfaces §7), so this is cross-lane patch 11: one
line inserting `"skill": engram_contract::skill::doctor_json(&installed_skill_paths())` into the
existing `json!` at `doctor.rs:50-78`. The helper ships in this lane and returns:

```json
"skill": {
  "status": "stale",
  "expected_schema_hash": "a41f…",
  "installed": [
    { "path": "~/.claude/skills/engram/SKILL.md", "status": "stale", "schema_hash": "77b2…" },
    { "path": "~/.codex/skills/engram/SKILL.md",  "status": "fresh", "schema_hash": "a41f…" }
  ],
  "suggestion": "run `engram skill install` to refresh 1 stale skill file"
}
```

**The fifth status: `modified`.** `skill install` refuses to overwrite a hand-edited `SKILL.md`
(`commands/skill.rs`), so "installed, edited by the user, hash no longer matches" is a state real
users reach on day two — and reporting it as `stale` tells them to run a command that will refuse.
The per-file statuses are therefore `fresh` (hash matches), `stale` (hash present, differs),
`modified` (front-matter intact but the body's hash-covered content was edited, or the file is
newer than its recorded install), `unreadable` (present, no parseable front-matter — the usual
cause is a user stripping the metadata block), `missing`. `modified` and `unreadable` carry the
suggestion ``that copy was edited locally; `engram skill install --force` replaces it, or move it
aside first`` — never a bare "run `skill install`", because `skill.rs:113-114` **refuses the whole
install and exits 2** when any target is locally modified, so the plain advice is advice the
binary rejects. The wording mirrors that error message on purpose.

`status` is the worst of the per-file statuses
(`missing` < `unreadable` < `modified` < `stale` < `fresh`), and
`missing` everywhere is reported as `not_installed`, which is not a fault. Doctor's overall
`healthy` boolean (`doctor.rs:43`) is **not** gated on it: a stale skill is a documentation drift,
not a broken store, and making `doctor` exit non-zero for it would break CI for every user who
has not re-run `skill install`.

### 4.4 What `schema_hash` hashes

`canonical_json()` serializes `contract()` with map keys sorted and no whitespace, then
`Uuid::new_v5(CONTRACT_NAMESPACE, bytes).simple()`. It covers command paths, flag names and
aliases, defaults, types, output field names, vocabularies, exit codes, error codes and the
schema version. It does **not** cover: `summary`/`doc` prose, `examples`, `rules`,
`decision_rules`. A `contract_hash_stability` test pins the hash of a fixed miniature `Contract`
value so a refactor of the serializer cannot silently change every user's staleness verdict.

### 4.5 JSON Schema shape

`schema_for("remember")` returns:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://engram.tools/schema/remember",
  "title": "engram remember",
  "type": "object",
  "properties": {
    "input": { "type": "object",
      "properties": { "content": {"type":"string"}, "importance": {"type":"integer","minimum":0,"maximum":10,"default":5}, … },
      "required": ["content"], "additionalProperties": false },
    "output": { "type": "object",
      "properties": { "version": {"const":"1"}, "status": {"enum":["success","partial_success","all_failed","no_results","error"]},
                      "data": { "type":"object", "properties": { … }, "required": [ …fields with always=true… ] },
                      "metadata": { "type":"object", "properties": { "elapsed_ms": {"type":"integer"} }, "required": ["elapsed_ms"] } },
      "required": ["version","status","data","metadata"] }
  }
}
```

`Ty::Enum` → `enum`; `Ty::Nullable(t)` → `{"type": [t, "null"]}`; `Ty::Uuid`/`Ty::Rfc3339` →
`{"type":"string","format":…}`. `additionalProperties:false` on `input` only — output is additive
by constraint 3, so pinning it closed would fail every time another lane adds a field.

### 4.6 `--top-k` vs `--limit` — one rule

The rule, published in agent-info, skill.md and the JSON Schemas:

> **`--top-k` means "how many ranked results"; `--limit` means "how many rows".** A command that
> scores and orders by relevance takes `--top-k`. A command that enumerates stored rows takes
> `--limit`.

Ranked (`--top-k`): `recall`, `memory search`, `research`, `bench`.
Enumerating (`--limit`): `facts list`, `facts conflicts`, `people list`, `entities`, `documents
list`, `graph neighbors`, `jobs list`, `facts backfill`, `wiki`.

The alias is **one-directional**: `--limit` is a `visible_alias` for `--top-k` on the four ranked
commands, because that is the mistake agents actually make (they reach for the enumerating word).
`--top-k` is *not* accepted on listings: a ranked flag on an unranked listing would promise an
ordering that does not exist. `contract_flags.rs` asserts both halves — `recall --limit 3` returns
three results, and `facts list --top-k 3` exits 3 with `bad_input`.

### 4.7 Pre-agreed `cli.rs` arms for other lanes (CD-4)

`cli.rs` is this lane's file, and two other lanes need subcommands in it. Their shapes are agreed
here so the lanes do not each invent one; they land as cross-lane patches when those lanes are
ready (patches 12 and 13), and the registry entries ship with them.

```rust
/// Compiled current-state view of one subject: open intervals, provenance,
/// open questions. (reasoning-layer, wave 5)
State { subject: String,
        #[arg(long)] kb: Option<String>,
        #[arg(long)] diary: Option<String>,
        #[arg(long)] relation: Option<String>,
        #[arg(long)] include_closed: bool,
        #[arg(long)] as_of: Option<String> },

/// Journal maintenance. (storage-journal, wave 2)
Admin { #[command(subcommand)] cmd: AdminCommand },

#[derive(Subcommand, Debug)]
pub enum AdminCommand {
    /// Drop and replay every projection from the event journal.
    Rebuild { #[arg(long)] kb: Option<String>, #[arg(long)] dry_run: bool },
}
```

---

## 5. Migration & compatibility

This lane touches no store, so it has no data migration. Its migration surface is the **agent
contract** — what an already-deployed agent script observes changing.

### 5.1 Behaviour changes an agent can observe

| # | change | who breaks | why it is worth it |
|---|---|---|---|
| 1 | `StorageError::NotFound` now exits **3**, was **1** | a script retrying exit 1 with backoff on a bad id | R11/CM-10. Today a wrong id is retried forever, and on `edit` each retry could spend a paid embedding call |
| 2 | clap errors exit **3**, were **2**, and print a JSON envelope on piped stderr, were prose | a script treating exit 2 as "fix config" | a typo'd flag is bad input, not a broken setup; the current mapping sends agents to `config check` for a typo |
| 3 | `resolve_command` no longer contains `\|` | a script splitting the string on `\|` to enumerate actions | the old string was not runnable — a shell would pipe it. The enumeration moves to `resolve.allowed_actions`, where it is parseable |
| 4 | `suggested_action` may now be `requires_context` on safety pairs, where it was `supersede-old` | a script matching `== "supersede-old"` | R7/CC-3. That value on a medication pair told agents to delete a drug record |
| 5 | `code` value `"config"`/`"transient"` → kept as `"config_error"`/`"transient_error"` | nobody — this *preserves* 0.3.1 | see §2.2 patch 2a |
| 6 | new `error.code` values `not_found`, `wrong_id_kind`, `sqlite_busy`, `conflict_already_resolved`, `malformed_json` | a script with an exhaustive `match` on `code` | additive; the exit code still tells them what to do |
| 7 | `--limit` accepted on `recall`/`memory search`/`research`/`bench` | nobody — purely additive | §4.6 |
| 8 | `agent-info` gains `schema_hash`, `error_codes`, `vocabularies`, `contract_version`, `schema_version`, `json_schema_command` | nobody — additive keys | move 5 |

Changes 1–4 go in `CHANGELOG.md` under a **BREAKING FOR AGENTS** heading (the CHANGELOG entry
CW-7 asks for and leaves unowned; this lane writes the contract half, fact-semantics the
`fact_type`/`arity` half). Change 4 needs a one-line note in the `remember` and `recall` manifest
entries telling scripts to read `resolve.allowed_actions` rather than `suggested_action` when
deciding what they *may* do — `suggested_action` is advice, `allowed_actions` is permission.

### 5.2 Wire-string table (`CliError::code()`)

Frozen from 0.3.1 (must not change): `transient_error`, `config_error`, `bad_input`,
`rate_limited`, `embed_error`, `rerank_error`, `ingest_error`, `bench_error`, `io_error`,
`serde_error`, `zip_error`. Seven of those have no `ErrorCode` variant, which is why `code()` is
a separate `match` from `error_code()` — see the two-map rule in §2.3, and do not "simplify" one
into the other.

`storage_error` is the one legacy string that **narrows**: a `CliError::Storage` now emits
`not_found`, `sqlite_busy`, `malformed_json`, `config_error` or `transient_error` depending on the
R11 mapping, and `storage_error` is emitted for no case at all. That is the point of CM-10 — the
old string carried no actionable information. It stays documented in the manifest's
`error_codes` list marked `"deprecated": true, "replaced_by": [...]` for one release, so a script
that greps for it finds out why it vanished.

New strings: `not_found`, `wrong_id_kind`, `sqlite_busy`, `conflict_already_resolved`,
`malformed_json`.

### 5.3 Backward compatibility rules this lane enforces on itself

- Every field present in the 0.3.1 manifest is present in the generated one, with the same key
  path and the same JSON type. `contract_manifest_compat.rs` (§6.2) checks this against a
  **committed golden copy** of the 0.3.1 manifest, `tests/fixtures/agent_info_0.3.1.json`.
  A deliberate removal is a two-line diff in the golden file *and* a CHANGELOG entry — cheap when
  intended, impossible by accident.
- Bare UUIDs stay accepted everywhere a handle is accepted, and the manifest says so once, in the
  `handles` section, rather than in twelve command descriptions.
- No `ENGRAM_HOME` anywhere in any generated artefact (CM-13).
- `agent-info`, `--help`, `--version` and every clap error still complete without opening,
  creating or migrating a store. Asserted by pointing `ENGRAM_STORE` at an empty TempDir and
  proving no `db.sqlite` appears (§6.1).

---

## 6. Test plan

**Isolation is non-negotiable (brief constraint 1, R14).** Every test in this lane creates a
`TempDir` and sets `ENGRAM_STORE` to it. No test in this lane sets `HOME` or any `XDG_*` variable;
`point_at_home` is legacy and is not used here. A test that omits `ENGRAM_STORE` is a reject.

### 6.0 The shared helper — `crates/engram-cli/tests/contract_support/mod.rs`

A subdirectory, not a bare file, so cargo does not compile it as its own (empty) test target.

```rust
use assert_cmd::Command;
use tempfile::TempDir;

/// Every contract test starts here. `ENGRAM_STORE` roots db, vectors, config
/// and cache inside `tmp` (paths.rs), so a test can neither read nor migrate
/// the user's real store — the bug that once bricked an installed binary.
pub struct Sandbox { pub tmp: TempDir }

impl Sandbox {
    pub fn new() -> Sandbox;
    /// `engram` with ENGRAM_STORE set, every provider key CLEARED
    /// (GEMINI_API_KEY, COHERE_API_KEY, OPENROUTER_API_KEY, and the
    /// GEMINI_API_KEY_FALLBACKS chain), so no contract test can make a paid
    /// call or depend on the developer's environment.
    pub fn cmd(&self) -> Command;
    /// Same, plus --offline / --profile offline where the command takes it.
    pub fn cmd_offline(&self) -> Command;
    pub fn store_path(&self) -> &std::path::Path;
    /// Parsed stdout as JSON; panics with the raw bytes on failure, because a
    /// contract test that says "expected object" and hides the output is useless.
    pub fn json(out: &std::process::Output) -> serde_json::Value;
    /// Parsed stderr envelope.
    pub fn err_json(out: &std::process::Output) -> serde_json::Value;
}

/// The manifest, parsed once per test binary.
pub fn manifest(sb: &Sandbox) -> serde_json::Value;
```

### 6.1 `tests/contract_discovery.rs` — the store-free surface

| test | asserts |
|---|---|
| `agent_info_opens_no_store` | run `agent-info` with `ENGRAM_STORE` = empty TempDir; the directory is still empty afterwards (no `db.sqlite`, no `vectors/`, no `config.toml`) |
| `help_and_version_open_no_store` | same for `--help`, `-h`, `--version`, `engram remember --help` |
| `clap_error_opens_no_store` | same for `engram recall --nonsense` |
| `agent_info_is_raw_json_not_enveloped` | top level has `name`/`commands`, and has **no** `version`+`status`+`data` triple (framework principle 1) |
| `agent_info_mentions_no_engram_home` | the whole manifest, stringified, does not contain `ENGRAM_HOME` (CM-13) |
| `agent_info_documents_engram_store` | `env_overrides.ENGRAM_STORE` exists and its doc mentions db, vectors, config and cache |

### 6.2 `tests/contract_manifest.rs` — documentation ↔ binary

| test | asserts |
|---|---|
| `every_documented_command_exists` | for each `commands[path]`, `engram <path> --help` exits 0 |
| `every_clap_command_is_documented` | walk `Cli::command()` (clap's own introspection, in-process via the library) and require a registry entry for every subcommand path; failure message lists the missing paths |
| `every_documented_flag_exists` | for each `FlagSpec`, `engram <path> --help` output contains `--<cli_long>`; every alias too |
| `every_clap_flag_is_documented` | the reverse, from `clap::Command::get_arguments()`; global flags exempted |
| `flag_names_match_json_spelling` | `cli_long` is `name` with `_`→`-`, for every flag — the invariant that keeps the daemon's request fields and the CLI's flags one vocabulary |
| `manifest_is_0_3_1_superset` | every leaf key path in `tests/fixtures/agent_info_0.3.1.json` exists in the live manifest with the same JSON type (§5.3) |
| `schema_hash_is_stable_within_a_build` | two invocations return the same hash |
| `json_schema_for_every_command` | `agent-info --schema '<path>'` exits 0 and returns an object with `$schema`, `properties.input`, `properties.output`, for every documented command; `--schema nope` exits 3 with `bad_input` |

### 6.3 `tests/contract_defaults.rs` — every documented default

One table-driven test. Each row is `(command path, flag, documented default)` read from
`contract()`, verified two ways: the string appears in `--help` output as clap's
`[default: …]`, **and** where the value is observable in a response it is checked there.

Pinned rows (the ones that have drifted before): `recall.top_k=10`, `recall.layer=topic`,
`recall.mode=evidence`, `recall.profile=cloud_quality`, `research.top_k=12`,
`remember.importance=5`, `remember.diary=default`, `remember.kb=default`,
`remember.autonomy=assisted`, `serve.host=127.0.0.1`, `serve.port=8768`,
`people show.kb=*`, `graph neighbors.hops=1`, `facts list` unscoped = all KBs.

**`people show.kb` is `"*"`, not `"default"`** — verified at `cli.rs:783-784`
(`#[arg(long, default_value = "*")]`, "Defaults to all KBs"). Today's `agent_info.rs:154,316`
still say `default`; that is the stale doc the 2026-07-25 all-KB change left behind, and
`skill.md:106` already says every KB. The registry transcription (§3.2) must take the value from
**`cli.rs`, never from `agent_info.rs`** — transcribing the manifest verbatim would freeze the
bug into the generated contract, and `every_documented_default` would then either fail or, worse,
pass while documenting a lie. Same rule for every other row: `cli.rs` is the authority for
defaults, `agent_info.rs` only for prose.

`recall_top_k_default_is_observable`: write 15 memories with `--offline`, run
`recall '…' --profile offline`, assert `data.results.len() == 10`. This is the test that would
have caught a default changing in clap but not in the manifest.

### 6.4 `tests/contract_errors.rs` — the taxonomy against the binary

| test | asserts |
|---|---|
| `every_error_code_has_an_exit_and_a_suggestion` | in-process over `ErrorCode::ALL`: exit per R11, non-empty suggestion, and the suggestion is not the old `--help` boilerplate |
| `manifest_documents_every_error_code` | `ErrorCode::ALL` ⊆ manifest `error_codes`, and vice versa |
| `storage_error_mapping_is_total` | in-process: one `StorageError` of each of the five variants (plus a synthetic busy `rusqlite::Error`) → the R11 code; a `#[deny]`-style exhaustive `match` so a new `StorageError` variant fails to compile rather than silently becoming `Transient` |
| `unknown_id_exits_3_on_every_mutator` | **the CM-10 regression test.** For `forget`, `edit`, `memory update`, `memory delete`, `facts resolve`, `documents delete`: a random UUID → exit **3**, `code == "not_found"`, and stderr is a parseable envelope. **`#[ignore]` until write-surfaces lands** — verified today: `forget.rs:17-21` exits **0** with `deleted: false`, and `edit.rs:61-62` exits 3 as `bad_input`, not `not_found`. Neither is this lane's file, so an un-ignored test here fails the wave-2 workspace gate on someone else's schedule (CC-4) |
| `clap_error_is_json_and_exit_3` | `engram recall --nonsense` piped: exit 3, stderr parses, `error.code == "bad_input"`, `error.exit_code == 3`, suggestion names `--help` or `agent-info` for that command |
| `clap_error_on_tty_is_prose` | with stderr a terminal (or the `IsTerminal` shim), output is not JSON — the existing human path is preserved |
| `help_exits_0_on_stdout` | `--help` and `--version` write to stdout, exit 0, and produce nothing on stderr |
| `unknown_subcommand_suggests_nearest` | `engram recal 'x'` → exit 3, suggestion contains `recall` (strsim) |
| `bad_enum_value_exits_3` | `--autonomy yolo`, `--profile turbo`, `--mode sideways`, `--layer sub` each exit 3 |

### 6.5 `tests/contract_envelope.rs` — shape, per command

| test | asserts |
|---|---|
| `success_envelope_shape` | for every non-destructive, non-network command runnable in an empty store (`kb list`, `facts list`, `entities list`, `documents list`, `config show`, `jobs list`, `usage`, `budget show`): `{version:"1", status, data, metadata.elapsed_ms}` and nothing else at top level |
| `listing_carries_returned_total_count` | every listing has `returned`, `total`, and `count == returned` (IN-23); with more rows than the page size, `total > returned` and a `cursor` is present. **`#[ignore]` until read-surfaces adopts `output::Listing`** — this lane ships the type, another lane populates it |
| `documented_data_fields_are_present` | every `FieldSpec` with `always: true` appears in the response for that command |
| `no_undocumented_data_fields` | every key in `data` has a `FieldSpec`. This is the test CD-10 predicts will fire when `Fact` gains `close_reason`/`close_authority`/`closed_by` one wave before anything populates them — so those three `FieldSpec`s ship in wave 2 with `always: false` |
| `contract_schema_version` | `doctor --json` `data.db.schema_version` equals `contract().schema_version`; the tripwire for a store migration landing without the docs. Asserts **equality with the binary, never a literal** — see the sequencing rule in §2.0 |
| `status_values_are_from_the_vocabulary` | across every command exercised in this file, `status` ∈ the pinned set |

### 6.6 `tests/contract_skill.rs` — generated-file freshness

| test | asserts |
|---|---|
| `skill_asset_is_freshly_generated` | `engram_contract::skill::render(&RuntimeContext::fixture())` equals the bytes of `crates/engram-cli/assets/skill.md`, byte-for-byte. Failure message says "run `cargo run -p engram-contract --bin engram-contract-gen`" |
| `skill_front_matter_hash_matches` | the asset's `schema_hash` equals `engram_contract::schema_hash()` |
| `skill_schema_version_matches` | the generated schema-version line equals `contract().schema_version` — the sentence that says "Schema is v9" today and would otherwise still say it after v10 |
| `skill_install_then_doctor_reports_fresh` | with `HOME` **and** `ENGRAM_STORE` in the TempDir, run `skill install`, then `doctor --json`, assert `data.skill.status == "fresh"`. (This is the one test in the lane that also sets `HOME` — it must, because `skill.rs:24-36` writes under the home directory, and writing four skill directories into the developer's real home would be a worse violation than the extra env var. `ENGRAM_STORE` is set as well, so store isolation is never load-bearing on `HOME`.) |
| `doctor_reports_stale_skill` | after `skill install`, rewrite the installed `SKILL.md` front-matter hash to `deadbeef…`; `doctor --json` reports `status: "stale"` and names that path; `doctor` still exits 0 |
| `doctor_reports_not_installed` | a home with no skill files → `status: "not_installed"`, exit 0 |

### 6.7 `tests/contract_resolve.rs` — the one builder (R12)

| test | asserts |
|---|---|
| `resolve_command_has_no_pipe` | in-process over every `ResolvePayload` the builder can produce: `!s.contains('\|')` and `!s.contains('&')` |
| `resolve_command_is_runnable` | take the string, split it, run it against a store with a real open conflict, assert exit 0 — the strongest form of "ready-to-run", and the assertion the current string would fail |
| `resolve_object_and_string_agree` | `resolve.args` joined equals `resolve_command` minus the program name |
| `allowed_actions_are_from_the_vocabulary` | ⊆ `vocabulary("resolve_action")` |
| `all_four_producers_agree` (wave 3 gate) | drive `remember` into a queued conflict, then read the same conflict through `facts conflicts --open`, `recall`, and `POST /v1/recall`; assert the four `resolve` objects are structurally identical for the same conflict id. **`#[ignore]` until the wave-3 producer patches land** (patches 7–10), listed in this lane's report per CC-4 |
| `suggested_action_vocabulary` | every `suggested_action` the binary emits ∈ `{keep-both, supersede-old, requires_context}`; safety relations emit only `requires_context`. `#[ignore]` until fact-semantics lands (R7) |

### 6.8 `tests/contract_flags.rs` — vocabulary reconciliation

| test | asserts |
|---|---|
| `limit_is_an_alias_for_top_k` | `recall '…' --limit 3 --profile offline` returns 3 results, same as `--top-k 3` |
| `top_k_is_rejected_on_listings` | `facts list --top-k 3` exits 3 with `bad_input` |
| `every_ranked_command_accepts_limit` | table over `recall`, `memory search`, `research`, `bench` |
| `every_listing_accepts_limit` | table over the nine enumerating commands |
| `manifest_publishes_the_rule` | the flag-vocabulary rule text is present in both the manifest and skill.md |

### 6.9 Unit tests inside the crates

- `engram-contract`: `canonical_json` key ordering; `schema_hash` stability against a pinned
  miniature contract; `parse_schema_hash` on well-formed, missing and malformed front-matter;
  `Ty` → JSON Schema for every variant; `freshness` for all four outcomes.
- `cli.rs`: `parse_from` over `["engram","--help"]` → `Rendered`; `["engram","recall"]` →
  `Invalid`; `["engram","recall","q"]` → `Ok`.
- `respond.rs`: as §3.6.
- `error.rs`: `From<&StorageError>` exhaustiveness; `code()` legacy-string table.

### 6.10 Gates

`cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`,
`cargo test --workspace`, and `engram bench personal --json` unchanged (this lane cannot move
retrieval numbers; the bench run is the proof that it did not).

`#[ignore]`d tests at commit time, each with the lane it waits on named in the `#[ignore = "…"]`
reason string, all listed in the lane report and all un-ignored at the wave-3 integration gate
(CC-4):

| test | waits on |
|---|---|
| `all_four_producers_agree` (§6.7) | producer patches 7–10 |
| `suggested_action_vocabulary` (§6.7) | fact-semantics (R7) |
| `unknown_id_exits_3_on_every_mutator` (§6.4) | write-surfaces — `forget` exits 0 today |
| `listing_carries_returned_total_count` (§6.5) | read-surfaces adopting `output::Listing` |
| `documented_data_fields_are_present` (§6.5), for the `always: true` fields on listings only | read-surfaces; the rest of the test runs |

**The rule this table encodes:** a test in this lane may assert against another lane's file only
if it is ignored until that lane lands. Anything else makes contract-codegen's merge conditional
on merge order, which is how a wave gate becomes a queue. Nothing else in the lane is ignored —
the store-free surface, the error taxonomy, the resolve builder and the generator freshness are
all fully this lane's, and all run from commit one.

---

## 7. cross_lane_patches

Each is a one-to-few-line change in a file this lane does not own, applied by the integration lead
at the wave gate (R15/IN-30). Ordered by the wave that needs them.

**Wave 2 — without these the lane does not compile.**

| # | file | change | for |
|---|---|---|---|
| 1 | workspace `Cargo.toml` | add `"crates/engram-contract"` to `[workspace] members` and `engram-contract = { path = "crates/engram-contract", version.workspace = true }` to `[workspace.dependencies]`; add `engram-contract.workspace = true` to `crates/engram-cli/Cargo.toml` `[dependencies]` | §2.1 |
| 2a | `crates/engram-core/src/error.rs:58,59` | `ErrorCode::Config.as_str()` → `"config_error"`, `ErrorCode::Transient.as_str()` → `"transient_error"` | constraint 3: those two strings are what 0.3.1 emits (§5.2) |
| 2b | `crates/engram-core/src/error.rs` | add `pub fn retryable(self) -> bool` and `pub const ALL: [ErrorCode; 9]` | §2.2 |
| ~~2c~~ | `crates/engram-core/src/lib.rs:17` | **already landed** — the line reads `pub use error::{CoreError, ErrorCode};` on disk today. No patch; kept in the table so nobody re-derives it as missing | IN-5 |
| 2d | `crates/engram-cli/src/lib.rs` | `pub mod respond;` beside the existing module declarations. Without it `respond.rs` is a file the compiler never sees, and every producer patch (7–10) fails to resolve `respond::` | §2.4 |
| 2f | `crates/engram-storage/src/sqlite.rs:792` | `pub const CURRENT_VERSION: i32 = 9;` (visibility only, value untouched) so the contract derives the schema version instead of retyping it | §2.0 |
| 2e | `crates/engram-cli/src/lib.rs` | `pub fn skill_paths() -> Vec<PathBuf>` — make `commands/skill.rs:23-37`'s private `target_dirs()` reachable by re-exporting it (`pub use commands::skill::target_dirs as skill_paths;` after changing `fn target_dirs` to `pub(crate) fn`). `skill.rs` is frozen (interfaces §7), so this is a visibility change and nothing else: no logic edit, no duplicated path list. Patch 11 and `contract_skill.rs` both call it, and a second hand-rolled copy of the four skill directories is exactly the drift `doctor` is meant to detect | §4.3, patch 11 |
| 3 | `crates/engram-storage/src/error.rs` | add `impl StorageError { /// SQLITE_BUSY / "database is locked" — the one storage failure worth retrying. pub fn is_busy(&self) -> bool }` | §2.3, keeps `rusqlite` out of engram-cli's dependency list |
| 4 | `crates/engram-cli/src/main.rs:15` | `let cli = engram_cli::cli::parse_or_exit();` (and drop the now-unused `use clap::Parser`) | §2.5 |
| 5 | `crates/engram-cli/src/lib.rs:24` and `:31` | `matches!(cli.command, Command::AgentInfo { .. })` and `Command::AgentInfo { schema } => agent_info::run(schema),` | §2.6 |
| 6 | `crates/engram-cli/src/bin/engramd.rs:29` | route `Args::parse()` through the same envelope: `let args = Args::try_parse().unwrap_or_else(\|e\| engram_cli::cli::emit_parse_failure(e));` | §2.5 — the daemon binary has the same exit-2-for-a-typo bug. `emit_parse_failure` is specified in §2.5 precisely so this is one line and the two parsers cannot drift |

**Wave 3 — the producer migrations (R12). Each replaces one `format!` with one builder call.**

| # | file:line | change |
|---|---|---|
| 7 | `crates/engram-cli/src/fact_write.rs:1232` | replace the `resolve_command` literal with `respond::attach(&mut entry, &respond::resolve_conflict(qid, allowed_actions))`; `allowed_actions` from `engram_graph::semantics::allowed_actions` |
| 8 | `crates/engram-cli/src/commands/facts.rs:371` | same, `row.id` |
| 9 | `crates/engram-cli/src/commands/recall.rs:345` | same, `row.id` |
| 10 | `crates/engram-cli/src/commands/serve.rs:577-610` | **insert**, do not rewrite: that range is the `/v1/recall` response builder and it has no `conflicts` key at all today. The patch adds one, built with the same helper, so the HTTP surface stops being the one producer that hides open conflicts |
| 11 | `crates/engram-cli/src/commands/doctor.rs:50-78` | one key: `"skill": engram_contract::skill::doctor_json(&engram_cli::skill_paths())` — `doctor.rs` is frozen, so this is the only change to it | §4.3 |

**Waves 4–5 — pre-agreed, land with their lane.**

| # | file | change |
|---|---|---|
| 12 | `crates/engram-cli/src/cli.rs` | this lane applies §4.7's `Admin` arm when storage-journal's `commands/admin.rs` lands (CD-4) |
| 13 | `crates/engram-cli/src/cli.rs` | this lane applies §4.7's `State` arm when reasoning-layer lands (CD-4) |
| 14 | `crates/engram-cli/src/commands/mod.rs` | `pub mod admin;` / `pub mod state;` — lead's file per interfaces §7 |

**Requests to other lanes that are not patches** (they belong in those lanes' own work, but this
lane's contract tests will fail without them):

- **fact-semantics**: emit `suggested_action` values from `engram_contract::vocabulary("suggested_action")`
  rather than string literals, and expose `allowed_actions()` returning `&'static [&'static str]`
  whose members are `vocabulary("resolve_action")` (R7, R12).
- **write-surfaces**: `forget` on an unknown id must return `CliError::NotFound`, not exit 0 with
  `deleted: false` (`forget.rs:17-21`); `contract_errors::unknown_id_exits_3_on_every_mutator`
  pins it.
- **read-surfaces**: use `output::Listing` / `output::Scope` / `output::Verdict` (§2.7) rather
  than hand-rolled `count` fields, so `contract_envelope` passes.
- **storage-journal**: `Fact`'s three new fields need `FieldSpec`s in the registry the same wave
  they appear in `fact_to_json` (CD-10); tell this lane, do not add them to the registry directly.
- **lead**: `tests/cli.rs`'s `point_at_home` also setting `ENGRAM_STORE` (R14, wave 2). This
  lane's own tests do not depend on it, but `contract_*` and `cli.rs` running in the same
  `cargo test` share a process environment only by accident, and the brief's constraint 1 is
  satisfied for the whole suite only once that patch lands.

---

## 8. Risks & open questions

### 8.1 Risks

1. **The registry transcription loses a sentence.** 453 lines of hand-tuned agent-facing prose
   move into `registry.rs`. Mitigation: `manifest_is_0_3_1_superset` against a committed golden
   copy (§5.3) catches a missing *key*; a missing *sentence inside* a description it does not.
   Discipline: the move is one commit with no prose edits, and the prose corrections listed in
   §3.2 are a second commit, so `git show` on the first commit should be reviewable as a pure
   restructure.
2. **`schema_hash` churn makes staleness meaningless.** If the hash covers too much, every patch
   release tells users to reinstall the skill and they stop believing it. §4.4 excludes prose,
   examples and rules for exactly this reason; if the hash still moves on most releases, narrow
   it further rather than removing the check.
3. **`clap` error-kind coverage.** `ErrorKind` has ~20 variants and clap adds them between minor
   versions. A kind we forget falls into `Invalid` (exit 3), which is the safe default, but
   `DisplayHelpOnMissingArgumentOrSubcommand` in particular must stay on the `Rendered` path or
   bare `engram` starts exiting 3 with an envelope instead of printing help. Explicitly tested.
4. **Exit-code change 1 (`NotFound` 1→3) reaches a retry loop in the wild.** An agent retrying
   exit 1 on a bad id currently loops forever; after the change it fails fast, which is the
   intent, but a script that treats exit 3 as fatal-and-alert will now alert where it used to
   spin silently. Documented under BREAKING FOR AGENTS (§5.1).
5. **Golden-file tests are brittle across platforms.** `RuntimeContext::fixture()` exists so
   skill.md and the golden manifest contain no machine-specific paths; the live-manifest test
   compares key paths and types, never the whole document. Do not add a golden test over
   `agent-info`'s full output.
6. **Circular dependency by accretion.** The first person who wants `engram-contract` to know
   about a `Fact` will add `engram-core` to its manifest, and the crate stops being pure data.
   The rule to defend: `engram-contract` describes shapes as `Ty`, it never imports a shape.
7. **The `resolve_command` / `suggested_action` transition window.** After the producer patches
   (7–10) land but before fact-semantics lands R7, a single response can carry
   `"suggested_action": "supersede-old"` (still hard-coded at `fact_write.rs:1209-1213` for
   `detector == "slot"`) beside `"resolve_command": "engram facts resolve … --action keep-both"`
   (§2.4's safe-first ordering). An agent that reads the advice and an agent that runs the command
   take different actions on the same conflict. It is not a correctness bug — both are permitted
   values, and `keep-both` is the conservative one — but it is confusing, and on a safety pair it
   is the *advice* that is wrong, not the command. Mitigation: the two changes land in the same
   wave-3 gate, and `suggested_action_vocabulary` stays `#[ignore]`d until they do. If the gate
   slips, the window is real and this is the first thing to explain in the lane report.
8. **The daemon and the CLI drifting on the same typo.** Patch 6 routes `engramd`'s `Args`
   through `emit_parse_failure`, which fixes argv parsing — but `engramd`'s HTTP surface does not
   yet map `CliError` into the R11 taxonomy at all (`serve.rs:112` builds its own JSON). So
   `engram recall --nonsense` and `POST /v1/recall` with a bad body still disagree about what an
   error looks like. In scope to *document* (the manifest says the HTTP surface's error shape is
   the daemon's, wave 4), out of scope to fix; daemon-jobs owns it. Flagged so it is not
   discovered as a surprise at the wave-4 gate.
9. **`ENGRAM_STORE` is new to the manifest, not new to the binary.** `agent_info.rs`'s
   `env_overrides` contains no `ENGRAM_STORE` and no `ENGRAM_HOME` today (verified). The golden
   0.3.1 fixture (§5.3) must therefore be a byte copy of what 0.3.1 actually emits — if someone
   "helpfully" adds `ENGRAM_STORE` to the fixture, `manifest_is_0_3_1_superset` starts asserting
   against a manifest that never shipped, and the superset check silently stops meaning anything.

### 8.2 Open questions for the lead

1. **Who applies patches 2a/2b?** R11 says "`error.rs` (contract-codegen) implements", but
   storage-journal has already landed `ErrorCode` in `engram-core/src/error.rs` (and the `lib.rs`
   re-export, which is why 2c is struck from §7). Either this lane edits that file (violating the
   ownership table as written) or the lead applies four one-liners: two serde renames, two
   `as_str()` spellings, plus `retryable()`/`ALL`. Requesting the latter; needs a ruling either
   way before wave-2 integration. Whoever applies them, 2a is the constraint-3 fix and cannot slip
   past the wave-2 gate — until it lands, the binary emits `"config"` where 0.3.1 emitted
   `"config_error"`.
2. **Is `storage_error` allowed to disappear as an emitted value?** §5.2 keeps it documented and
   deprecated but emits it for no case. The alternative is emitting it for the `Transient` storage
   bucket, which keeps a script working at the cost of two codes meaning one thing. Recommending
   removal; it is a constraint-3 judgement call and the lead owns constraint 3.
3. **May this lane add a JSON-Schema validator dependency?** Validating real command output
   against the generated schema is a much stronger contract test than `assert_shape`. `jsonschema`
   is a heavy tree; `valico` is lighter. Non-goal in this spec (§1.2) pending a ruling. Note the
   precedent: R9 rejected `blake3` for one hash, so the default answer is probably no.
4. **Does `engram agent-info --schema` belong on `agent-info`, or on a new `engram schema`
   command?** The flag costs two cross-lane patches to `lib.rs`; a new subcommand costs a
   `commands/mod.rs` line (no owner, CD-4) and a dispatch arm (daemon-jobs). Recommending the
   flag, as specified — but the lead may prefer to keep `AgentInfo` a unit variant.
5. **Should the daemon serve the contract?** `GET /v1/agent-info` and `GET /v1/schema/{command}`
   would let a remote agent discover the surface without a binary. Cheap once the generator
   exists, but it is daemon-jobs' file and wave 4. Flagging, not claiming.
6. **`--top-k` on listings: reject, or accept-and-warn?** §4.6 rejects (exit 3). An agent that
   guesses wrong gets a clean error and the suggestion names `--limit`, which is better teaching
   than silent acceptance — but it is a hard failure on a request that had an obvious intent.
   Confirm before wave 3, because `contract_flags::top_k_is_rejected_on_listings` encodes it.
7. **Does move 5's "generates clap definitions" stand?** The brief lists clap codegen among move
   5's deliverables; §1.2 declares it a non-goal and keeps the clap enums hand-written. The
   argument for the narrowing: clap derive is already declarative, generating it buys a second
   source of truth and a build-time dependency while `contract_defaults`/`contract_flags` already
   prove the enums and the manifest agree — which is the property the brief actually wants. But an
   implementer reading the brief will find the spec refusing a named deliverable, so this needs a
   ruling in words rather than a non-goal bullet. Recommending the narrowing.
8. **`status: "no_results"` vs `data.verdict: "no_match"` — one absence signal or two?** Recall
   already sets the envelope status (`recall.rs:249-256`); IN-23 adds the verdict and the absence
   certificate. §4.1 pins both vocabularies, and the spec never says what an agent should read.
   Three options: (a) keep both, define `no_results` as the envelope-level shorthand and
   `verdict` as the one carrying evidence — additive, but two things to check; (b) `status` stays
   `success` once `verdict` exists, which is a constraint-3 value change on a field agents branch
   on today; (c) drop `verdict` and put `absence` under the existing status. Recommending (a),
   with the manifest saying in one sentence that `verdict` is authoritative and `no_results` is
   the legacy shorthand — but this is a wire-contract call and the lead owns constraint 3. It
   blocks `status_values_are_from_the_vocabulary`.
9. **How does `resolve_command_is_runnable` (§6.7) manufacture an open conflict offline?** The
   test needs a genuinely contested pair, and the sandbox clears provider keys (correctly), so
   extraction cannot run. Two candidate fixtures: `remember --facts-json` twice with conflicting
   single-valued facts (no network, but depends on fact-semantics' `--facts-json` path staying
   stable), or a direct `store.queue_conflict(...)` in an in-process test that then shells out for
   the resolve. Recommending the second — it is this lane's own fixture, it cannot leak a paid
   call, and it does not couple a contract test to another lane's extraction behaviour. Needs
   confirming that the storage API for it is public by wave 3.
10. **Who tells this lane when `Fact` gains a field?** §7's request to storage-journal ("tell this
    lane, do not add them to the registry directly") names no channel, which is the same
    gap CC-5/IN-30 exists to close. Proposal: a `FieldSpec` addition is a one-line entry in
    `interfaces.md`'s patch table, the same mechanism as every other cross-lane change, so it goes
    through the lead rather than through a message someone has to remember. `no_undocumented_data_fields`
    is the backstop — it fails loudly rather than silently — but a failing gate is a worse
    notification than a patch row.
