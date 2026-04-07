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

#[derive(Default)]
pub struct Metadata {
    pub elapsed_ms: u64,
    pub fields: serde_json::Map<String, Value>,
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
                "status": "success",
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
