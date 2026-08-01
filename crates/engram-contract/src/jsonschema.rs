//! Generator 3: JSON Schema 2020-12, one per command.
//!
//! `additionalProperties: false` on `input` only. Output is additive by the
//! compatibility constraint, so pinning it closed would fail the moment
//! another lane adds a field — a schema that breaks on a permitted change
//! teaches agents to ignore schemas.

use serde_json::{json, Map, Value};

use crate::model::{CommandSpec, FieldSpec, Ty};
use crate::{command, contract};

pub fn schema_for(path: &str) -> Option<Value> {
    command(path).map(build)
}

/// `{ "<command path>": <schema>, ... }` for every command the contract knows.
pub fn all_schemas() -> Value {
    let mut obj = Map::new();
    for c in contract().commands {
        obj.insert(c.path.into(), build(c));
    }
    Value::Object(obj)
}

fn build(c: &'static CommandSpec) -> Value {
    let mut input_props = Map::new();
    let mut required: Vec<&'static str> = Vec::new();
    for a in c.args {
        input_props.insert(a.name.into(), ty_schema(&a.ty, a.doc, None));
        if a.required {
            required.push(a.name);
        }
    }
    for f in c.flags {
        input_props.insert(f.name.into(), ty_schema(&f.ty, f.doc, f.default));
        if f.required {
            required.push(f.name);
        }
    }

    let mut data_props = Map::new();
    let mut data_required: Vec<&'static str> = Vec::new();
    for f in c.data {
        data_props.insert(f.name.into(), ty_schema(&f.ty, f.doc, None));
        if f.always {
            data_required.push(f.name);
        }
    }

    let mut meta_props = Map::new();
    for f in c.metadata {
        meta_props.insert(f.name.into(), ty_schema(&f.ty, f.doc, None));
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://engram.tools/schema/{}", c.path.replace(' ', "/")),
        "title": format!("engram {}", c.path),
        "description": c.summary,
        "type": "object",
        "properties": {
            "input": {
                "type": "object",
                "properties": input_props,
                "required": required,
                "additionalProperties": false,
            },
            "output": {
                "type": "object",
                "properties": {
                    "version": { "const": "1" },
                    "status": { "enum": c.statuses },
                    "data": {
                        "type": "object",
                        "properties": data_props,
                        "required": data_required,
                    },
                    "metadata": {
                        "type": "object",
                        "properties": meta_props,
                        "required": ["elapsed_ms"],
                    },
                },
                "required": ["version", "status", "data", "metadata"],
            },
        },
    })
}

fn ty_schema(ty: &Ty, doc: &'static str, default: Option<&'static str>) -> Value {
    let mut obj = match ty {
        Ty::Str => json!({ "type": "string" }),
        Ty::Bool => json!({ "type": "boolean" }),
        Ty::Int => json!({ "type": "integer" }),
        Ty::Float => json!({ "type": "number" }),
        Ty::Uuid => json!({ "type": "string", "format": "uuid" }),
        Ty::Rfc3339 => json!({ "type": "string", "format": "date-time" }),
        Ty::Enum(vs) => json!({ "enum": vs }),
        Ty::Object => json!({ "type": "object" }),
        Ty::Array(inner) => json!({ "type": "array", "items": ty_schema(inner, "", None) }),
        // `["string","null"]` rather than a nullable keyword: 2020-12 has no
        // `nullable`, and an agent that sees one will guess.
        Ty::Nullable(inner) => {
            let inner_schema = ty_schema(inner, "", None);
            match inner_schema.get("type").and_then(Value::as_str) {
                Some(t) => json!({ "type": [t, "null"] }),
                None => json!({ "oneOf": [inner_schema, { "type": "null" }] }),
            }
        }
    };
    if let Value::Object(map) = &mut obj {
        if !doc.is_empty() {
            map.insert("description".into(), json!(doc));
        }
        if let Some(d) = default {
            map.insert("default".into(), json!(d));
        }
    }
    obj
}

/// Every documented output field, flattened — the list
/// `no_undocumented_data_fields` checks a live response against.
pub fn documented_data_fields(path: &str) -> Option<&'static [FieldSpec]> {
    command(path).map(|c| c.data)
}
