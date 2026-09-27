//! Semantic exit codes per the agent-cli-framework.
//!
//! Two maps, deliberately not one (spec §2.3, "the two-map rule"):
//!
//! - `error_code()` is the TAXONOMY — what an agent should do next. It drives
//!   `exit_code()` and the manifest's `retryable` flag, and it is what
//!   interfaces §4 / R11 pin.
//! - `code()` is the WIRE VALUE — the string 0.3.1 already emits in
//!   `error.code`. Seven of those (`embed_error`, `rerank_error`, …) name a
//!   subsystem rather than an action and have no `ErrorCode` variant, so
//!   deriving one map from the other would silently rename all seven and break
//!   the compatibility constraint.

use engram_core::ErrorCode;
use serde_json::json;
use std::io::{IsTerminal, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("transient error: {0}")]
    Transient(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("bad input: {0}")]
    BadInput(String),

    /// Bad input whose recovery depends on where it happened. The generic
    /// `bad_input` advice points at `agent-info --command`, which is a loop
    /// when the call that failed WAS a discovery call. Same code, same exit;
    /// only the suggestion is specific.
    #[error("bad input: {message}")]
    BadInputHint { message: String, suggestion: String },

    /// A config error whose fix is known where it happens. Same code, same
    /// exit as `Config`; the generic `config check` advice would send the
    /// caller to probe API keys for a problem that has nothing to do with them.
    #[error("config error: {message}")]
    ConfigHint { message: String, suggestion: String },

    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),

    #[error("embed error: {0}")]
    Embed(#[from] engram_embed::EmbedError),

    #[error("rerank error: {0}")]
    Rerank(#[from] engram_rerank::RerankError),

    #[error("ingest error: {0}")]
    Ingest(#[from] engram_ingest::IngestError),

    #[error("bench error: {0}")]
    Bench(#[from] engram_bench::BenchError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// A handle or id that resolves to nothing. Exit 3 on EVERY mutator,
    /// `forget` included (interfaces §1) — a wrong id used to exit 1, which
    /// every retry loop reads as "try again", and on `edit` each retry could
    /// spend a paid embedding call.
    #[error("not found: {what} {handle}")]
    NotFound { what: &'static str, handle: String },

    /// The handle exists but names the wrong kind of thing. Names the actual
    /// kind AND the handle the caller should have passed, because "not found"
    /// on a chunk id sent agents hunting for a deleted memory that was never
    /// deleted.
    #[error("wrong id kind: {handle} is a {actual}, this command needs a {expected}")]
    WrongIdKind {
        handle: String,
        actual: String,
        expected: String,
        /// The handle that would have worked, when we can name it.
        correct: Option<String>,
    },

    /// `facts resolve` lost the claim race: someone else already adjudicated.
    #[error("conflict {conflict_id} was already resolved ({verdict})")]
    ConflictAlreadyResolved {
        conflict_id: String,
        verdict: String,
    },

    /// Caller-supplied JSON that did not parse or did not match the shape
    /// (`--facts-json`, an HTTP body). Distinct from `Serde`, which is ours.
    #[error("malformed json in {field}: {detail}")]
    MalformedJson { field: &'static str, detail: String },

    /// SQLITE_BUSY / "database is locked" — retryable, unlike every other
    /// storage failure.
    #[error("store busy: {0}")]
    SqliteBusy(String),
}

/// R11's table, implemented once. A free function rather than the `From` impl
/// the spec sketched: both types are foreign to this crate, so the orphan rule
/// forbids the impl and the choice is a free function here or a dependency on
/// `engram-core` inside `engram-storage`. The match is exhaustive, so a new
/// `StorageError` variant fails to compile rather than silently becoming
/// "transient, retry".
pub fn storage_error_code(e: &engram_storage::StorageError) -> ErrorCode {
    use engram_storage::StorageError as S;
    match e {
        S::NotFound(_) => ErrorCode::NotFound,
        S::Migration(_) => ErrorCode::Config,
        S::Serde(_) => ErrorCode::MalformedJson,
        // Storage's dedicated variants, landed with the v10 work (CLP-4):
        // the string-sniffing `is_busy` fallback below stays for raw Sqlite
        // errors that arrive unclassified.
        S::Busy(_) => ErrorCode::SqliteBusy,
        S::MalformedRow(_) => ErrorCode::MalformedJson,
        S::Io(_) => ErrorCode::Transient,
        S::Sqlite(_) if is_busy(e) => ErrorCode::SqliteBusy,
        S::Sqlite(_) => ErrorCode::Transient,
    }
}

/// A locked store is the one storage failure worth retrying, and it must not
/// be reported as the same class as a corrupt row.
///
/// Matched on the rendered message rather than `rusqlite::ErrorCode` because
/// `rusqlite` is deliberately not a dependency of this crate; the typed
/// predicate belongs in `engram-storage` (cross-lane patch 3) and this reads
/// as its temporary stand-in, not as the intended home.
fn is_busy(e: &engram_storage::StorageError) -> bool {
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("database is locked")
        || msg.contains("database table is locked")
        || msg.contains("sqlite_busy")
}

impl CliError {
    /// The single source of truth for exits: a new variant cannot acquire an
    /// exit status by accident, only by choosing a taxonomy class.
    pub fn error_code(&self) -> ErrorCode {
        use CliError::*;
        match self {
            Transient(_) | Io(_) | Serde(_) | Zip(_) => ErrorCode::Transient,
            Config(_) | ConfigHint { .. } => ErrorCode::Config,
            BadInput(_) | BadInputHint { .. } | Ingest(_) | Bench(_) => ErrorCode::BadInput,
            RateLimited(_) => ErrorCode::RateLimited,
            Storage(e) => storage_error_code(e),
            // Preserves today's exit 2: the highest-traffic setup failure
            // ("no Gemini key") must keep sending the caller to `config
            // check`, never into a retry loop.
            Embed(e) => match e {
                engram_embed::EmbedError::MissingKey { .. } => ErrorCode::Config,
                engram_embed::EmbedError::RateLimited { .. } => ErrorCode::RateLimited,
                _ => ErrorCode::Transient,
            },
            Rerank(e) => match e {
                engram_rerank::RerankError::MissingKey { .. } => ErrorCode::Config,
                engram_rerank::RerankError::RateLimited { .. } => ErrorCode::RateLimited,
                _ => ErrorCode::Transient,
            },
            NotFound { .. } => ErrorCode::NotFound,
            WrongIdKind { .. } => ErrorCode::WrongIdKind,
            ConflictAlreadyResolved { .. } => ErrorCode::ConflictAlreadyResolved,
            MalformedJson { .. } => ErrorCode::MalformedJson,
            SqliteBusy(_) => ErrorCode::SqliteBusy,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.error_code().exit_code()
    }

    /// The `error.code` wire value. The eleven legacy strings are frozen —
    /// scripts branch on them — so they are literals here and not derived.
    /// `storage_error` is the one that narrows: a storage failure now says
    /// which kind it was, which is the whole point of the taxonomy.
    pub fn code(&self) -> &'static str {
        use CliError::*;
        match self {
            Transient(_) => "transient_error",
            Config(_) | ConfigHint { .. } => "config_error",
            BadInput(_) | BadInputHint { .. } => "bad_input",
            RateLimited(_) => "rate_limited",
            Embed(_) => "embed_error",
            Rerank(_) => "rerank_error",
            Ingest(_) => "ingest_error",
            Bench(_) => "bench_error",
            Io(_) => "io_error",
            Serde(_) => "serde_error",
            Zip(_) => "zip_error",
            Storage(_)
            | NotFound { .. }
            | WrongIdKind { .. }
            | ConflictAlreadyResolved { .. }
            | MalformedJson { .. }
            | SqliteBusy(_) => self.error_code().as_str(),
        }
    }

    /// Owned, because a useful suggestion names the caller's own handle.
    /// Every code yields one, and none of them is "run `engram --help`" —
    /// advice the caller already had.
    pub fn suggestion(&self) -> Option<String> {
        use CliError::*;
        Some(match self {
            BadInput(_) | Ingest(_) | Bench(_) => {
                "run `engram agent-info --command '<command>'` for this command's exact argument shape and rules"
                    .to_string()
            }
            BadInputHint { suggestion, .. } | ConfigHint { suggestion, .. } => suggestion.clone(),
            // Both gate-threshold messages (fact_write.rs
            // `parse_gate_thresholds`) begin with the variable's name; a key
            // probe cannot fix a typo in one of them. Match the two names, not
            // the `ENGRAM_FACT_` prefix: the LLM stub's error shares it.
            Config(m)
                if m.starts_with(crate::fact_write::DUP_THRESHOLD_ENV)
                    || m.starts_with(crate::fact_write::NOVEL_THRESHOLD_ENV) =>
            {
                format!(
                "fix or unset the ENGRAM_FACT_*_THRESHOLD variable the message names, then rerun \
                 the engram command; unset, the novelty gate uses dup {} and novel {}",
                engram_graph::gate::DEFAULT_DUP_THRESHOLD,
                engram_graph::gate::DEFAULT_NOVEL_THRESHOLD
                )
            }
            Config(_) => {
                "run `engram config check` — it live-probes every configured key and names the one that failed"
                    .to_string()
            }
            Embed(engram_embed::EmbedError::MissingKey { .. }) => {
                "no embedding key resolved — set GEMINI_API_KEY (or run `engram config check` to see which keys are live)"
                    .to_string()
            }
            Rerank(engram_rerank::RerankError::MissingKey { .. }) => {
                "no rerank key resolved — set COHERE_API_KEY, or run with `--profile fast` to skip reranking"
                    .to_string()
            }
            RateLimited(_) | Embed(_) | Rerank(_) if self.error_code() == ErrorCode::RateLimited => {
                "wait and retry; `engram usage --json` shows what you have spent and `engram budget show` the caps"
                    .to_string()
            }
            NotFound { what, handle } => format!(
                "no {what} with handle {handle}; run `engram recall '<what you were looking for>'` to find the right id — recall returns chunk handles (`chunk:…`) and mutators accept either, and the response says which is which"
            ),
            WrongIdKind {
                handle,
                actual,
                expected,
                correct,
            } => match correct {
                Some(h) => format!(
                    "{handle} is a {actual}; re-run the same command with {h}, which is the {expected} it belongs to"
                ),
                None => format!(
                    "{handle} is a {actual}, not a {expected}; `engram recall --json` reports the handle of each kind beside every row"
                ),
            },
            ConflictAlreadyResolved { .. } => {
                "someone resolved it first; run `engram facts conflicts --open --json` for what is still queued"
                    .to_string()
            }
            // Two unrelated failures map onto malformed_json, and they need
            // opposite advice. Telling a corrupt-row case to "check the JSON
            // you passed" sends an agent to re-edit arguments that were fine,
            // and it will never run the one command that would have shown it
            // a broken store.
            MalformedJson { field, .. } => format!(
                "check the JSON you passed in `{field}`; `engram agent-info --command '<command>'` gives the exact shape"
            ),
            Storage(e) if storage_error_code(e) == ErrorCode::MalformedJson => {
                "a stored row failed to deserialize — this is store corruption or a half-applied migration, not your arguments. Run `engram doctor --json`; do not retry"
                    .to_string()
            }
            Storage(e) if storage_error_code(e) == ErrorCode::NotFound => {
                "run `engram recall '<what you were looking for>'` to find the right id; ids from `recall` are chunk handles (`chunk:…`) and mutators accept either"
                    .to_string()
            }
            Storage(e) if storage_error_code(e) == ErrorCode::Config => {
                "the store could not be migrated — run `engram doctor --json` for the schema version it stopped at"
                    .to_string()
            }
            SqliteBusy(_) => {
                "another engram process holds the write lock; retry in a moment, or stop `engram serve` if a daemon is running"
                    .to_string()
            }
            Storage(e) if storage_error_code(e) == ErrorCode::SqliteBusy => {
                "another engram process holds the write lock; retry in a moment, or stop `engram serve` if a daemon is running"
                    .to_string()
            }
            _ => {
                "retry with backoff; if it repeats, `engram doctor --json` reports store and provider health"
                    .to_string()
            }
        })
    }

    /// Always emits to stderr — JSON when stderr is piped or stdout was JSON.
    pub fn emit_to_stderr(&self) {
        let want_json = !stderr_is_tty();
        let mut err = std::io::stderr().lock();
        if want_json {
            let payload = json!({
                "version": "1",
                "status": "error",
                "error": {
                    "code": self.code(),
                    "message": self.to_string(),
                    "suggestion": self.suggestion(),
                    "exit_code": self.exit_code(),
                }
            });
            let _ = writeln!(err, "{}", payload);
        } else {
            let _ = writeln!(err, "error: {}", self);
            if let Some(s) = self.suggestion() {
                let _ = writeln!(err, "  hint: {}", s);
            }
        }
    }
}

/// One decision, injectable. Inline `is_terminal()` made the human branch
/// untestable: CI stderr is a pipe, so no child process could ever observe it,
/// and "the prose path still works" was an assumption rather than a test.
/// `ENGRAM_FORCE_TTY` is a test seam and is documented as one.
pub(crate) fn stderr_is_tty() -> bool {
    match std::env::var("ENGRAM_FORCE_TTY").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => std::io::stderr().is_terminal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_storage::StorageError;

    #[test]
    fn legacy_wire_strings_are_unchanged() {
        // Scripts in the wild branch on these eleven. Only `storage_error`
        // narrows, and it narrows into codes that say what actually happened.
        assert_eq!(CliError::Transient("x".into()).code(), "transient_error");
        assert_eq!(CliError::Config("x".into()).code(), "config_error");
        assert_eq!(CliError::BadInput("x".into()).code(), "bad_input");
        assert_eq!(CliError::RateLimited("x".into()).code(), "rate_limited");
        assert_eq!(CliError::Io(std::io::Error::other("x")).code(), "io_error");
    }

    #[test]
    fn new_variants_use_error_code_as_str() {
        let e = CliError::NotFound {
            what: "memory",
            handle: "mem:1".into(),
        };
        assert_eq!(e.code(), "not_found");
        assert_eq!(e.exit_code(), 3);
        assert_eq!(CliError::SqliteBusy("x".into()).code(), "sqlite_busy");
        assert_eq!(CliError::SqliteBusy("x".into()).exit_code(), 1);
    }

    #[test]
    fn storage_not_found_exits_3_not_1() {
        // The change that stops a retry loop spending a paid embedding call
        // on an id that will never exist.
        let e = CliError::Storage(StorageError::NotFound("mem:1".into()));
        assert_eq!(e.error_code(), ErrorCode::NotFound);
        assert_eq!(e.exit_code(), 3);
        assert_eq!(e.code(), "not_found");
    }

    #[test]
    fn storage_mapping_covers_every_variant() {
        let cases: Vec<(StorageError, ErrorCode)> = vec![
            (StorageError::NotFound("x".into()), ErrorCode::NotFound),
            (StorageError::Migration("x".into()), ErrorCode::Config),
            (
                StorageError::Serde(serde_json::from_str::<i32>("nope").unwrap_err()),
                ErrorCode::MalformedJson,
            ),
            (
                StorageError::Io(std::io::Error::other("x")),
                ErrorCode::Transient,
            ),
        ];
        for (e, want) in cases {
            assert_eq!(storage_error_code(&e), want, "{e}");
        }
    }

    #[test]
    fn exit_codes_are_unchanged_for_the_legacy_variants() {
        assert_eq!(CliError::Transient("x".into()).exit_code(), 1);
        assert_eq!(CliError::Io(std::io::Error::other("x")).exit_code(), 1);
        assert_eq!(CliError::Config("x".into()).exit_code(), 2);
        assert_eq!(CliError::BadInput("x".into()).exit_code(), 3);
        assert_eq!(CliError::RateLimited("x".into()).exit_code(), 4);
    }

    #[test]
    fn every_suggestion_names_a_runnable_command() {
        let errs = vec![
            CliError::Transient("x".into()),
            CliError::Config("x".into()),
            CliError::BadInput("x".into()),
            CliError::BadInputHint {
                message: "x".into(),
                suggestion: "`engram --help` lists every command".into(),
            },
            CliError::RateLimited("x".into()),
            CliError::Storage(StorageError::NotFound("x".into())),
            CliError::NotFound {
                what: "memory",
                handle: "mem:1".into(),
            },
            CliError::WrongIdKind {
                handle: "chunk:1".into(),
                actual: "chunk".into(),
                expected: "memory".into(),
                correct: Some("mem:2".into()),
            },
            CliError::ConflictAlreadyResolved {
                conflict_id: "c1".into(),
                verdict: "keep_both".into(),
            },
            CliError::MalformedJson {
                field: "--facts-json",
                detail: "expected array".into(),
            },
            CliError::SqliteBusy("x".into()),
        ];
        for e in errs {
            let s = e.suggestion().expect("every error suggests something");
            assert!(!s.is_empty());
            // Either a command to run, or the exact handle to run it with.
            // Anything else is the boilerplate this table replaced.
            assert!(
                s.contains("engram") || s.contains("mem:2"),
                "suggestion for {} names nothing actionable: {s}",
                e.code()
            );
            assert!(
                !s.contains("Run `engram --help` for argument syntax"),
                "the old non-advice came back"
            );
        }
    }

    #[test]
    fn a_bad_gate_threshold_is_not_sent_to_probe_keys() {
        let e = CliError::Config(
            "ENGRAM_FACT_DUP_THRESHOLD='x' is not a cosine similarity in [-1, 1]; fix or unset it"
                .into(),
        );
        let s = e.suggestion().unwrap();
        assert!(s.contains("ENGRAM_FACT_") && s.contains("0.95"), "{s}");
        assert!(!s.contains("config check"), "{s}");
        let other = CliError::Config("x".into()).suggestion().unwrap();
        assert!(other.contains("config check"), "{other}");
        let stub = CliError::Config("ENGRAM_FACT_LLM_STUB is not a valid script: x".into())
            .suggestion()
            .unwrap();
        assert!(!stub.contains("THRESHOLD"), "{stub}");
    }

    #[test]
    fn the_two_malformed_json_suggestions_differ() {
        // One caller re-edits its arguments; the other runs doctor. Giving
        // both the same sentence sends one of them to the wrong place.
        let caller = CliError::MalformedJson {
            field: "--facts-json",
            detail: "x".into(),
        };
        let row = CliError::Storage(StorageError::Serde(
            serde_json::from_str::<i32>("nope").unwrap_err(),
        ));
        assert_ne!(caller.suggestion(), row.suggestion());
        assert!(row.suggestion().unwrap().contains("doctor"));
    }

    #[test]
    fn a_locked_store_is_retryable_and_a_broken_one_is_not() {
        assert!(is_busy(&StorageError::Migration(
            "database is locked".into()
        )));
        assert!(!is_busy(&StorageError::Migration("no such table".into())));
    }
}
