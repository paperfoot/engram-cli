//! JSON envelope and TTY-aware output routing.

use serde::Serialize;
use serde_json::{json, Value};
use std::io::{IsTerminal, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Human,
}

impl OutputFormat {
    pub fn detect(json_flag: bool) -> Self {
        if json_flag || !std::io::stdout().is_terminal() {
            OutputFormat::Json
        } else {
            OutputFormat::Human
        }
    }
}

/// Set once by `dispatch` when the command starts. `Metadata::default()`
/// reads it, so every command reports real elapsed time without each call
/// site carrying its own timer. Before this, only the commands that happened
/// to time themselves did — `edit` re-embedded over the network and reported
/// `elapsed_ms: 0`, which agents read as "nothing ran". Process-global (not
/// thread-local) because one process runs one command, and tokio's runtime
/// may finish the command on a different thread than the one that started it.
static COMMAND_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Called by `dispatch` at command entry. First call wins; later calls
/// (nested dispatch in tests) keep the original start, which is the honest
/// outermost measurement.
pub fn mark_command_start() {
    let _ = COMMAND_START.set(std::time::Instant::now());
}

pub struct Metadata {
    pub elapsed_ms: u64,
    pub fields: serde_json::Map<String, Value>,
}

impl Default for Metadata {
    fn default() -> Self {
        let elapsed_ms = COMMAND_START
            .get()
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        Metadata {
            elapsed_ms,
            fields: serde_json::Map::new(),
        }
    }
}

impl Metadata {
    pub fn add(&mut self, key: &str, value: impl Serialize) {
        if let Ok(v) = serde_json::to_value(value) {
            self.fields.insert(key.to_string(), v);
        }
    }
}

/// Print a success envelope to stdout. JSON or human, decided by `format`.
pub fn print_success<T: Serialize, F: FnOnce(&T)>(
    format: OutputFormat,
    data: T,
    metadata: Metadata,
    human: F,
) {
    print_envelope(format, "success", data, metadata, human)
}

/// Print a JSON envelope with an explicit status.
pub fn print_envelope<T: Serialize, F: FnOnce(&T)>(
    format: OutputFormat,
    status: &str,
    data: T,
    metadata: Metadata,
    human: F,
) {
    match format {
        OutputFormat::Json => {
            let mut meta = json!({ "elapsed_ms": metadata.elapsed_ms });
            if let Value::Object(ref mut map) = meta {
                for (k, v) in metadata.fields {
                    map.insert(k, v);
                }
            }
            let envelope = json!({
                "version": "1",
                "status": status,
                "data": data,
                "metadata": meta,
            });
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", serde_json::to_string(&envelope).unwrap());
        }
        OutputFormat::Human => {
            human(&data);
        }
    }
}
