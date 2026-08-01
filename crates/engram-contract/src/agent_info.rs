//! Generator 1: the agent-info manifest, rendered from the contract.
//!
//! Wave 2 lands the additive block and one round-tripped command (`recall`).
//! The rest of `agent_info.rs`'s hand-written manifest is unchanged and is
//! transcribed command by command in later commits, because a transcription
//! that silently drops a sentence of agent-facing prose is a regression no
//! key-level test can see.

use serde_json::{json, Map, Value};

use crate::model::{CommandSpec, FieldSpec};
use crate::runtime::RuntimeContext;
use crate::{command, contract, schema_hash};

/// One command's `command_schemas` entry, byte-compatible with the shape the
/// 0.3.1 manifest used: an object of `flag -> description`, so an agent that
/// already reads the manifest keeps reading it the same way. The type moves
/// into the description rather than into a nested object for the same reason.
pub fn render_command(path: &str) -> Option<Value> {
    command(path).map(render_command_spec)
}

fn render_command_spec(c: &'static CommandSpec) -> Value {
    let mut obj = Map::new();
    obj.insert("destructive".into(), json!(c.destructive));
    if !c.args.is_empty() {
        let mut args = Map::new();
        for a in c.args {
            args.insert(
                a.name.into(),
                json!(format!("{} — {}", a.ty.render(), a.doc)),
            );
        }
        obj.insert("args".into(), Value::Object(args));
    }
    if !c.flags.is_empty() {
        let mut flags = Map::new();
        for f in c.flags {
            let mut desc = format!("{} — {}", f.ty.render(), f.doc);
            if let Some(d) = f.default {
                desc.push_str(&format!(" (default {d})"));
            }
            if !f.aliases.is_empty() {
                desc.push_str(&format!(" [also --{}]", f.aliases.join(", --")));
            }
            flags.insert(f.name.into(), json!(desc));
        }
        obj.insert("flags".into(), Value::Object(flags));
    }
    if !c.data.is_empty() {
        obj.insert("output_data".into(), render_fields(c.data));
    }
    if !c.metadata.is_empty() {
        obj.insert("metadata".into(), render_fields(c.metadata));
    }
    if !c.aliases.is_empty() {
        obj.insert("aliases".into(), json!(c.aliases));
    }
    if !c.requires.is_empty() {
        obj.insert("requires".into(), json!(c.requires));
    }
    obj.insert("summary".into(), json!(c.summary));
    obj.insert("statuses".into(), json!(c.statuses));
    obj.insert("exit_codes".into(), json!(c.exit_codes));
    obj.insert("opens_store".into(), json!(c.opens_store));
    if !c.rules.is_empty() {
        obj.insert("rules".into(), json!(c.rules));
    }
    if !c.examples.is_empty() {
        obj.insert("examples".into(), json!(c.examples));
    }
    Value::Object(obj)
}

fn render_fields(fields: &'static [FieldSpec]) -> Value {
    let mut obj = Map::new();
    for f in fields {
        let always = if f.always {
            ""
        } else {
            " (present only in some responses)"
        };
        obj.insert(
            f.name.into(),
            json!(format!("{} — {}{}", f.ty.render(), f.doc, always)),
        );
    }
    Value::Object(obj)
}

/// The additive block every generated manifest carries: what the binary
/// promises about itself, in machine-readable form. Merged into the manifest
/// at the top level, so nothing an agent already reads moves.
pub fn render(rt: &RuntimeContext<'_>) -> Value {
    let c = contract();
    let mut exit_codes = Map::new();
    for (code, doc) in c.exit_codes {
        exit_codes.insert(code.to_string(), json!(doc));
    }
    let mut vocabularies = Map::new();
    for (name, members) in c.vocabularies {
        vocabularies.insert((*name).into(), json!(members));
    }
    json!({
        "contract_version": c.contract_version,
        "schema_version": c.schema_version,
        "schema_hash": schema_hash(),
        "answer_context_format": rt.answer_context_format,
        "exit_codes_detail": exit_codes,
        "error_codes": c.error_codes,
        "vocabularies": vocabularies,
        "envelope_detail": c.envelope,
        "env_vars": c.env_vars,
        "global_flags": c.global_flags,
    })
}
