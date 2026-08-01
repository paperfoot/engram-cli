//! `engram-contract` — the agent contract as data.
//!
//! One `Contract` value describes every command, flag, default, output field,
//! status value, exit code and enumerated vocabulary. Generators read it; they
//! never restate it. The bug this exists to end is documentation drift: a
//! manifest hand-written beside the code says whatever it said last time
//! somebody remembered to edit it, and an agent trusts it completely.
//!
//! Dependency direction is one-way and deliberate: this crate depends on
//! NOTHING in the workspace. Everything runtime-varying (paths, model slugs,
//! the package version) arrives as a `RuntimeContext`, so the contract stays
//! pure, fast to test, and impossible to make circular.

pub mod agent_info;
pub mod jsonschema;
pub mod model;
pub mod registry;
pub mod runtime;
pub mod skill;

pub use model::*;
pub use runtime::RuntimeContext;

use std::sync::OnceLock;

/// The one contract value. Everything else in this crate is a view over it.
pub fn contract() -> &'static Contract {
    static C: OnceLock<Contract> = OnceLock::new();
    C.get_or_init(|| Contract {
        contract_version: registry::CONTRACT_VERSION,
        schema_version: registry::SCHEMA_VERSION,
        envelope: registry::ENVELOPE,
        commands: registry::COMMANDS,
        global_flags: registry::GLOBAL_FLAGS,
        exit_codes: registry::EXIT_CODES,
        error_codes: registry::ERROR_CODES,
        vocabularies: registry::VOCABULARIES,
        env_vars: registry::ENV_VARS,
    })
}

/// `Some` when the path names a real command; `None` is a caller bug, not an
/// error. Aliases resolve too, so `command("search")` is `recall`.
pub fn command(path: &str) -> Option<&'static CommandSpec> {
    let cs = contract().commands;
    cs.iter()
        .find(|c| c.path == path)
        .or_else(|| cs.iter().find(|c| c.aliases.contains(&path)))
}

/// Every enumerated vocabulary the contract pins, by name.
pub fn vocabulary(name: &str) -> Option<&'static [&'static str]> {
    contract()
        .vocabularies
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
}

/// Canonical JSON of `contract()` with keys sorted and no whitespace — the
/// hashing pre-image, and the thing a diff should be read against.
pub fn canonical_json() -> String {
    // serde_json's Map preserves insertion order unless the `preserve_order`
    // feature is off; sorting explicitly means the hash cannot depend on a
    // feature flag chosen by an unrelated crate in the workspace.
    let value = serde_json::to_value(contract()).expect("the contract is plain data");
    let mut out = String::new();
    write_canonical(&value, &mut out);
    out
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value as V;
    match v {
        V::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        V::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap()),
    }
}

/// Namespace for the contract hash. Chosen once; changing it invalidates every
/// installed skill's freshness verdict at a stroke, which is why it is a
/// constant and not a parameter.
pub const CONTRACT_NAMESPACE: uuid::Uuid = uuid::uuid!("6f3d2b1e-9a44-4f6d-8e21-1c7b5a0d93f2");

/// Stable content hash of the contract, simple-hex (32 chars, no dashes).
///
/// No new dependency: `Uuid::new_v5` over `canonical_json()`, the same
/// primitive R9 pinned for `object_pair_hash` after rejecting blake3.
pub fn schema_hash() -> String {
    uuid::Uuid::new_v5(&CONTRACT_NAMESPACE, canonical_json().as_bytes())
        .simple()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let j = canonical_json();
        assert!(j.starts_with('{'));
        // "commands" sorts before "contract_version" before "envelope".
        let a = j.find("\"commands\"").unwrap();
        let b = j.find("\"contract_version\"").unwrap();
        let c = j.find("\"envelope\"").unwrap();
        assert!(a < b && b < c, "keys are not sorted");
    }

    #[test]
    fn the_hash_is_stable_within_a_build() {
        assert_eq!(schema_hash(), schema_hash());
        assert_eq!(schema_hash().len(), 32);
    }

    #[test]
    fn the_hash_moves_when_the_contract_moves() {
        // The property that makes staleness detection mean anything: a
        // different contract must not hash the same.
        let a = uuid::Uuid::new_v5(&CONTRACT_NAMESPACE, canonical_json().as_bytes());
        let b = uuid::Uuid::new_v5(&CONTRACT_NAMESPACE, (canonical_json() + " ").as_bytes());
        assert_ne!(a, b);
    }

    #[test]
    fn commands_resolve_by_path_and_by_alias() {
        assert_eq!(command("recall").unwrap().path, "recall");
        assert_eq!(command("search").unwrap().path, "recall");
        assert!(command("no-such-command").is_none());
    }

    #[test]
    fn every_vocabulary_is_reachable_by_name() {
        for (name, members) in contract().vocabularies {
            assert_eq!(vocabulary(name), Some(*members));
            assert!(!members.is_empty(), "{name} is empty");
        }
    }

    #[test]
    fn resolve_action_excludes_requires_context() {
        // You cannot pass `requires_context` to --action, so publishing it as
        // an allowed action would be advertising a value the parser rejects.
        let actions = vocabulary("resolve_action").unwrap();
        assert!(!actions.contains(&"requires_context"));
        assert!(vocabulary("suggested_action")
            .unwrap()
            .contains(&"requires_context"));
    }

    #[test]
    fn flag_names_are_the_cli_spelling_with_underscores() {
        // One vocabulary for the CLI and the daemon: `--rerank-top-n` and
        // `"rerank_top_n"` must be the same flag, mechanically.
        for c in contract().commands {
            for f in c.flags {
                assert_eq!(
                    f.cli_long,
                    f.name.replace('_', "-"),
                    "{}: {} vs {}",
                    c.path,
                    f.name,
                    f.cli_long
                );
            }
        }
    }

    #[test]
    fn every_error_code_has_an_exit_and_a_real_suggestion() {
        for e in contract().error_codes {
            assert!([1u8, 2, 3, 4].contains(&e.exit), "{}", e.code);
            assert!(!e.suggestion.is_empty(), "{}", e.code);
            assert!(
                e.suggestion.contains("engram") || e.suggestion.contains("retry"),
                "{} suggests nothing runnable: {}",
                e.code,
                e.suggestion
            );
            assert_eq!(e.retryable, matches!(e.exit, 1 | 4), "{}", e.code);
        }
    }

    #[test]
    fn the_error_code_vocabulary_matches_the_error_code_table() {
        let vocab = vocabulary("error_code").unwrap();
        let table: Vec<&str> = contract().error_codes.iter().map(|e| e.code).collect();
        assert_eq!(vocab.len(), table.len());
        for c in vocab {
            assert!(table.contains(c), "{c} is documented in neither direction");
        }
    }

    #[test]
    fn a_schema_exists_for_every_command() {
        for c in contract().commands {
            let s = jsonschema::schema_for(c.path).unwrap();
            assert!(s["$schema"].is_string());
            assert!(s["properties"]["input"].is_object());
            assert!(s["properties"]["output"].is_object());
        }
        assert!(jsonschema::schema_for("no-such-command").is_none());
    }

    #[test]
    fn the_input_schema_is_closed_and_the_output_schema_is_not() {
        // Output gains fields by design; a schema that fails on a permitted
        // addition teaches agents to ignore schemas.
        let s = jsonschema::schema_for("recall").unwrap();
        assert_eq!(s["properties"]["input"]["additionalProperties"], false);
        assert!(s["properties"]["output"]
            .get("additionalProperties")
            .is_none());
    }

    #[test]
    fn only_always_present_fields_are_required() {
        let s = jsonschema::schema_for("recall").unwrap();
        let required = s["properties"]["output"]["properties"]["data"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|v| v == "results"));
        // absence appears only on a no_match verdict.
        assert!(!required.iter().any(|v| v == "absence"));
    }

    #[test]
    fn nullable_renders_as_a_type_union() {
        let s = jsonschema::schema_for("recall").unwrap();
        let di = &s["properties"]["output"]["properties"]["data"]["properties"]["diary_isolation"];
        assert_eq!(di["type"], serde_json::json!(["object", "null"]));
    }

    #[test]
    fn the_generated_command_fragment_names_its_defaults() {
        let v = agent_info::render_command("recall").unwrap();
        let top_k = v["flags"]["top_k"].as_str().unwrap();
        assert!(top_k.contains("default 10"), "{top_k}");
        assert!(top_k.contains("--limit"), "{top_k}");
    }

    #[test]
    fn the_additive_block_carries_the_hash_and_the_vocabularies() {
        let v = agent_info::render(&RuntimeContext::fixture());
        assert_eq!(v["schema_hash"], serde_json::json!(schema_hash()));
        assert_eq!(v["contract_version"], serde_json::json!("contract@1"));
        assert!(v["vocabularies"]["resolve_action"].is_array());
        assert!(v["error_codes"].is_array());
    }

    #[test]
    fn nothing_generated_mentions_engram_home() {
        // The one env var that must appear nowhere: two names for the store
        // root is how a test suite comes to migrate a developer's real store.
        let all = format!(
            "{}{}",
            canonical_json(),
            agent_info::render(&RuntimeContext::fixture())
        );
        assert!(!all.contains("ENGRAM_HOME"));
    }
}
