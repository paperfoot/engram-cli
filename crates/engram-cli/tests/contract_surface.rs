//! The v0.4 surface, pinned against the binary.
//!
//! `contract_manifest.rs` proves the manifest and the registry agree. This
//! file proves the REGISTRY and the BINARY agree about the things that changed
//! in v0.4: typed handles, closed-not-deleted intervals, epistemic reads, the
//! jobs lifecycle, and the exit-code taxonomy. A documented field that no
//! response carries is the same defect as an undocumented one, in the
//! direction agents notice last.
//!
//! Isolation: `ENGRAM_STORE` in a TempDir, provider keys cleared, `--offline`
//! on every write. Nothing here can spend money or touch a real store.

mod contract_support;

use contract_support::{manifest, Sandbox};
use serde_json::Value;

/// The uuid no store will ever hold. Used wherever a test needs a
/// syntactically valid handle that resolves to nothing.
const ABSENT: &str = "00000000-0000-0000-0000-0000000000ff";

fn documented(path: &str) -> &'static engram_contract::CommandSpec {
    engram_contract::command(path).unwrap_or_else(|| panic!("{path} is not in the contract"))
}

fn data_field_names(path: &str) -> Vec<&'static str> {
    documented(path).data.iter().map(|f| f.name).collect()
}

/// Store one memory and hand back its handle. `--offline` because the sandbox
/// clears every provider key, and a write that needs one is a config error.
fn remember(sb: &Sandbox, content: &str) -> String {
    let out = sb
        .cmd()
        .args(["remember", content, "--kb", "personal", "--offline"])
        .args(["--no-extract-facts"])
        .output()
        .expect("remember");
    assert!(
        out.status.success(),
        "remember failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = Sandbox::json(&out);
    format!("mem:{}", v["data"]["id"].as_str().expect("an id"))
}

// --- reachability -----------------------------------------------------------

/// Every path the contract publishes is a path the binary answers to. The
/// cheapest possible defect, and the one a transcription introduces first: a
/// command documented under the name it ALMOST has.
#[test]
fn every_documented_command_path_exists_in_the_binary() {
    let sb = Sandbox::new();
    for c in engram_contract::contract().commands {
        let out = sb
            .cmd()
            .args(c.path.split_whitespace())
            .arg("--help")
            .output()
            .expect("help");
        assert!(
            out.status.success(),
            "`engram {} --help` failed: {}",
            c.path,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Aliases too. `memory update`, `facts get`, `jobs ls` and `memory delete`
/// are how agents that learned one spelling keep working, and a documented
/// alias that does not resolve teaches them the tool is unreliable.
#[test]
fn every_documented_command_alias_exists_in_the_binary() {
    let sb = Sandbox::new();
    for c in engram_contract::contract().commands {
        for alias in c.aliases {
            let out = sb
                .cmd()
                .args(alias.split_whitespace())
                .arg("--help")
                .output()
                .expect("help");
            assert!(
                out.status.success(),
                "`{}` is documented as an alias of `{}` and the binary does not know it",
                alias,
                c.path
            );
        }
    }
}

/// Destructive commands say so, and name the flag that unlocks them. A
/// `destructive: true` entry with no `requires` is an invitation to run it.
#[test]
fn every_destructive_command_names_its_guard_and_enforces_it() {
    let sb = Sandbox::new();
    for c in engram_contract::contract().commands {
        if !c.destructive {
            continue;
        }
        assert!(
            !c.requires.is_empty(),
            "{} is destructive and documents no guard",
            c.path
        );
        // And the guard is real: without it, exit 3 rather than an effect.
        let mut cmd = sb.cmd();
        cmd.args(c.path.split_whitespace());
        for a in c.args {
            let _ = a;
            cmd.arg(ABSENT);
        }
        let out = cmd.output().expect("guarded command");
        assert_eq!(
            out.status.code(),
            Some(3),
            "{} ran without {:?}",
            c.path,
            c.requires
        );
    }
}

// --- typed handles ----------------------------------------------------------

/// The vocabulary and the parser are the same list. `lesson` and `wiki`
/// arrived after the first transcription; nothing but a test stops the next
/// kind from arriving without them.
#[test]
fn the_documented_handle_kinds_are_exactly_the_ones_the_parser_accepts() {
    use engram_core::handle::HandleKind;
    for kind in engram_contract::vocabulary("handle_kind").unwrap() {
        let parsed = HandleKind::parse(kind)
            .unwrap_or_else(|| panic!("{kind} is documented and the parser rejects it"));
        assert_eq!(parsed.as_str(), *kind, "{kind} does not round-trip");
    }
    // And the other direction, by construction: every variant reachable from
    // a documented prefix.
    for k in [
        HandleKind::Memory,
        HandleKind::Chunk,
        HandleKind::Fact,
        HandleKind::Conflict,
        HandleKind::Entity,
        HandleKind::Document,
        HandleKind::Job,
        HandleKind::Event,
        HandleKind::Lesson,
        HandleKind::Wiki,
    ] {
        assert!(
            engram_contract::vocabulary("handle_kind")
                .unwrap()
                .contains(&k.as_str()),
            "{} is a real handle kind and the contract omits it",
            k.as_str()
        );
    }
}

/// The chunk id of the first hit for `query` — the id an agent copies out of a
/// recall response and then feeds back to a mutator.
fn a_chunk_id(sb: &Sandbox, query: &str) -> String {
    let out = sb
        .cmd()
        .args(["recall", query, "--profile", "offline"])
        .output()
        .expect("recall");
    let v = Sandbox::json(&out);
    v["data"]["results"]
        .as_array()
        .and_then(|r| r.first())
        .and_then(|r| r["provenance"]["chunk_id"].as_str())
        .expect("a chunk id in provenance")
        .to_string()
}

/// Both id failures the contract documents, on every mutator, with the codes
/// it documents. v0.3 answered a typo with exit 0.
///
/// The `wrong_id_kind` probe uses a REAL chunk id under a `fact:` prefix,
/// because the code is about a handle that resolves to a different kind of
/// thing. A `fact:` prefix over a uuid nothing holds is `not_found`, and
/// asserting otherwise would document a refusal the binary does not make.
#[test]
fn an_unknown_id_is_not_found_and_a_wrong_kind_is_wrong_id_kind() {
    let sb = Sandbox::new();
    remember(&sb, "Ana moved to Lisbon");
    let chunk = a_chunk_id(&sb, "Lisbon");

    for (path, extra) in [
        (vec!["edit"], vec!["--importance", "3"]),
        (vec!["forget"], vec!["--confirm"]),
        (vec!["memory", "update"], vec!["--importance", "3"]),
        (vec!["memory", "delete"], vec!["--confirm"]),
    ] {
        for absent in [
            format!("mem:{ABSENT}"),
            ABSENT.to_string(),
            format!("fact:{ABSENT}"),
        ] {
            let out = sb
                .cmd()
                .args(&path)
                .arg(&absent)
                .args(&extra)
                .output()
                .expect("mutator");
            assert_eq!(out.status.code(), Some(3), "{path:?} on {absent}");
            assert_eq!(
                Sandbox::err_json(&out)["error"]["code"],
                "not_found",
                "{path:?} on {absent}"
            );
        }

        let out = sb
            .cmd()
            .args(&path)
            .arg(format!("fact:{chunk}"))
            .args(&extra)
            .output()
            .expect("mutator");
        assert_eq!(
            out.status.code(),
            Some(3),
            "{path:?} on a mislabelled handle"
        );
        let err = Sandbox::err_json(&out);
        assert_eq!(
            err["error"]["code"], "wrong_id_kind",
            "{path:?} on a mislabelled handle"
        );
        // The refusal names the handle to retry with, so the fix costs no
        // round trip.
        assert!(
            err["error"]["message"].as_str().unwrap().contains("chunk")
                || err["error"]["suggestion"]
                    .as_str()
                    .unwrap()
                    .contains("chunk"),
            "the refusal does not name the actual kind: {err}"
        );
    }
}

/// A chunk handle is followed to its memory, and the response SAYS it was
/// followed, naming the handle it came from. Silently editing something other
/// than what the caller named is the failure `resolved_from` exists to prevent.
#[test]
fn a_chunk_handle_reaches_the_memory_and_reports_the_hop() {
    let sb = Sandbox::new();
    let handle = remember(&sb, "Bruno joined Acme in March and stayed two years");
    let mem_id = handle.trim_start_matches("mem:").to_string();
    let chunk = a_chunk_id(&sb, "Bruno");

    let out = sb
        .cmd()
        .args(["edit", &format!("chunk:{chunk}"), "--importance", "7"])
        .output()
        .expect("edit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = Sandbox::json(&out);
    assert_eq!(
        v["data"]["id"], mem_id,
        "the chunk did not resolve to its memory"
    );
    assert_eq!(v["data"]["handle"], handle);
    assert_eq!(
        v["data"]["resolved_from"],
        format!("chunk:{chunk}"),
        "resolved_from must carry the handle that was followed"
    );
    assert_eq!(v["metadata"]["resolved_from"], format!("chunk:{chunk}"));
}

// --- closed, not deleted ----------------------------------------------------

/// XC-3, the write half: every field the contract promises on `edit` is on the
/// response, and `facts_invalidated` counts CLOSURES.
#[test]
fn edit_answers_every_documented_field_including_the_closure_counters() {
    let sb = Sandbox::new();
    let handle = remember(&sb, "Ana lives in Lisbon");
    let out = sb
        .cmd()
        .args([
            "edit",
            &handle,
            "--content",
            "Ana lives in Porto",
            "--offline",
        ])
        .output()
        .expect("edit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = Sandbox::json(&out);
    for name in data_field_names("edit") {
        // `resolved_from` is documented as sometimes-absent and this id was a
        // memory handle, so it must NOT be here.
        if name == "resolved_from" {
            assert!(
                v["data"].get(name).is_none(),
                "resolved_from on a mem: handle"
            );
            continue;
        }
        assert!(
            v["data"].get(name).is_some(),
            "edit documents data.{name} and does not emit it: {}",
            v["data"]
        );
    }
    assert_eq!(v["data"]["updated"], true);
    assert!(v["data"]["facts_invalidated_ids"].is_array());
    let reextraction = v["data"]["reextraction"].as_str().unwrap();
    assert!(
        engram_contract::vocabulary("reextraction")
            .unwrap()
            .contains(&reextraction),
        "{reextraction} is outside the documented vocabulary"
    );
}

/// XC-3, the retraction half. Idempotence is the contract: the second call is
/// exit 0 and says `already_retracted`, which is what separates it from the
/// `not_found` an unknown id now raises.
#[test]
fn forget_answers_every_documented_field_and_is_idempotent() {
    let sb = Sandbox::new();
    let handle = remember(&sb, "Bruno works at Acme");

    let out = sb
        .cmd()
        .args(["forget", &handle, "--confirm"])
        .output()
        .expect("forget");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = Sandbox::json(&out);
    for name in data_field_names("forget") {
        if name == "resolved_from" {
            continue;
        }
        assert!(
            v["data"].get(name).is_some(),
            "forget documents data.{name} and does not emit it: {}",
            v["data"]
        );
    }
    assert_eq!(v["data"]["deleted"], true);
    assert_eq!(v["data"]["already_retracted"], false);
    assert!(v["data"]["facts_closed_ids"].is_array());

    let out = sb
        .cmd()
        .args(["forget", &handle, "--confirm"])
        .output()
        .expect("forget again");
    assert!(out.status.success(), "a repeated retraction must be exit 0");
    let v = Sandbox::json(&out);
    assert_eq!(v["data"]["deleted"], false);
    assert_eq!(v["data"]["already_retracted"], true);
}

// --- epistemic reads --------------------------------------------------------

/// `returned` / `total` / `verdict` / `scope` on a listing, and `absence` only
/// where it belongs. A certificate of absence beside results would be a
/// certificate of nothing.
#[test]
fn a_listing_reports_returned_total_verdict_and_scope() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["facts", "list", "--json"])
        .output()
        .expect("facts list");
    assert!(out.status.success());
    let v = Sandbox::json(&out);
    for key in ["returned", "total", "count", "verdict"] {
        assert!(v["data"].get(key).is_some(), "facts list omits {key}");
    }
    assert!(v["metadata"]["scope"]["kbs"].is_array());
    // This surface scores no chunks, so the key is OMITTED rather than lying
    // with a 0 — the same class of honesty as an absent threshold.
    assert!(
        v["metadata"]["scope"].get("chunks_considered").is_none(),
        "facts list reported chunks_considered"
    );
    assert_eq!(
        v["data"]["verdict"], "no_match",
        "an empty store has no facts"
    );
    let reason = v["data"]["absence"]["reason"].as_str().expect("a reason");
    assert!(
        engram_contract::vocabulary("absence_reason")
            .unwrap()
            .contains(&reason),
        "{reason} is outside the documented vocabulary"
    );
}

/// Reads span every KB since 0.3.x, and `facts show` was the one that still
/// defaulted to `default`: a fact filed under `personal` answered "0 facts",
/// with `origin: flag` although no flag was passed. `state` found it.
#[test]
fn facts_show_without_kb_finds_a_fact_filed_under_personal() {
    let sb = Sandbox::new();
    let facts = r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon"}]"#;
    let out = sb
        .cmd()
        .args([
            "remember",
            "Ana lives in Lisbon.",
            "--kb",
            "personal",
            "--offline",
        ])
        .args(["--facts-json", facts])
        .output()
        .expect("remember");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = sb
        .cmd()
        .args(["facts", "show", "Ana"])
        .output()
        .expect("facts show");
    assert!(out.status.success());
    let v = Sandbox::json(&out);
    let active = v["data"]["active"].as_array().expect("active facts");
    assert_eq!(active.len(), 1, "{v}");
    assert_eq!(v["metadata"]["scope"]["origin"], "all_kbs");
}

/// `state` is the command whose whole promise is that an absent answer is an
/// ANSWER: exit 0, `no_match`, and a reason naming the miss.
#[test]
fn an_unknown_subject_is_a_verdict_not_a_failure() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["state", "Nobody At All", "--json"])
        .output()
        .expect("state");
    assert!(
        out.status.success(),
        "an unknown subject must be exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = Sandbox::json(&out);
    assert_eq!(v["data"]["verdict"], "no_match");
    assert_eq!(v["data"]["absence"]["reason"], "subject_not_found");
    assert_eq!(v["data"]["resolved_from"], "raw");
    assert!(engram_contract::vocabulary("subject_resolution")
        .unwrap()
        .contains(&v["data"]["resolved_from"].as_str().unwrap()));
    assert!(v["metadata"]["scope"].is_object());
}

/// The reasoning-lane finding: `Provenance::to_json` dropped two of its own
/// fields, so every surface but `state` published a citation without the
/// `observed_at` and `snippet` that make it one.
#[test]
fn every_provenance_block_carries_observed_at_and_snippet() {
    let sb = Sandbox::new();
    remember(&sb, "Ana moved to Lisbon in March and loves the tram");
    let out = sb
        .cmd()
        .args(["recall", "Lisbon", "--profile", "offline"])
        .output()
        .expect("recall");
    let v = Sandbox::json(&out);
    let hits = v["data"]["results"].as_array().expect("results");
    assert!(!hits.is_empty(), "the fixture must be findable");
    for hit in hits {
        let p = &hit["provenance"];
        for key in [
            "chunk_id",
            "memory_id",
            "document_id",
            "kb",
            "diary",
            "derived_from",
            "observed_at",
            "snippet",
        ] {
            assert!(
                p.get(key).is_some(),
                "provenance omits {key}, so a caller cannot tell 'none' from 'unimplemented': {p}"
            );
        }
    }
}

/// Recall against its own published schema: every `data` field the contract
/// calls always-present is present, on both spellings. `kb` and `kb_scope`
/// were promised at the top of `data` from the day the contract landed and
/// only ever emitted one level down — an agent that trusted the schema read
/// `null`. And the three notes that say nothing unless something happened
/// are absent, not `null`, as the contract says.
#[test]
fn live_recall_output_satisfies_its_own_schema() {
    let sb = Sandbox::new();
    remember(&sb, "Ana moved to Lisbon in March and loves the tram");
    let schema = engram_contract::jsonschema::schema_for("recall").unwrap();
    let required = schema["properties"]["output"]["properties"]["data"]["required"]
        .as_array()
        .expect("required data fields");
    let spellings: [&[&str]; 2] = [
        &["recall", "Lisbon", "--profile", "offline"],
        &["memory", "search", "Lisbon", "--profile", "offline"],
    ];
    for argv in spellings {
        let out = sb.cmd().args(argv).output().expect("recall");
        assert!(out.status.success(), "{argv:?}");
        let v = Sandbox::json(&out);
        for key in required {
            let key = key.as_str().unwrap();
            assert!(
                v["data"].get(key).is_some(),
                "{argv:?} omits `data.{key}`, which its schema requires"
            );
        }
        assert_eq!(v["data"]["kb"], v["metadata"]["kb"], "{argv:?}");
        assert_eq!(v["data"]["kb_scope"], "all_kbs", "{argv:?}");
        for quiet in ["diary_isolation", "temporal_scope", "expiry"] {
            assert!(
                v["data"]["metadata"].get(quiet).is_none(),
                "{argv:?} prints `data.metadata.{quiet}: null` on a call it says nothing about"
            );
        }
    }
}

/// Scores are computed in `f32` and were printed widened to `f64`:
/// `0.9200000166893005` for a stored `0.92`. Every score, confidence and
/// threshold an agent reads is now four decimal places at most — on recall,
/// on state (including a caller-supplied six-decimal confidence), and on the
/// absence block a near-miss subject gets.
#[test]
fn no_serialized_score_carries_more_than_4_decimals() {
    const SCORES: &[&str] = &[
        "score",
        "rerank_score",
        "top_score",
        "threshold",
        "confidence",
        "resolution_score",
        "extraction",
        "source_kind",
        "confirmations",
        "staleness",
        "contested",
    ];
    fn walk(v: &Value, key: &str, seen: &mut usize, context: &str) {
        match v {
            Value::Object(map) => map.iter().for_each(|(k, v)| walk(v, k, seen, context)),
            Value::Array(items) => items.iter().for_each(|v| walk(v, key, seen, context)),
            Value::Number(n) if SCORES.contains(&key) && n.is_f64() => {
                *seen += 1;
                let text = n.to_string();
                let decimals = text.split_once('.').map_or(0, |(_, d)| d.len());
                assert!(
                    decimals <= 4 && !text.contains('e'),
                    "{context}: `{key}` printed as {text}"
                );
            }
            _ => {}
        }
    }

    let sb = Sandbox::new();
    remember(&sb, "Sarah prefers async standups; sync only for incidents");
    remember(&sb, "The pricing model is annual only, decided in March");
    let facts =
        r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":0.123456}]"#;
    let out = sb
        .cmd()
        .args([
            "remember",
            "Ana lives in Lisbon",
            "--kb",
            "personal",
            "--offline",
        ])
        .args(["--facts-json", facts])
        .output()
        .expect("remember");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut seen = 0;
    // `Anna` is a fuzzy match (resolution_score); `Zed` is an absence with a
    // threshold, a top score and scored nearest subjects.
    let probes: [&[&str]; 4] = [
        &["recall", "pricing standups", "--profile", "offline"],
        &["state", "Ana"],
        &["state", "Anna"],
        &["state", "Zed"],
    ];
    for argv in probes {
        let out = sb.cmd().args(argv).output().expect("probe");
        assert!(out.status.success(), "{argv:?}");
        walk(&Sandbox::json(&out), "", &mut seen, &argv.join(" "));
    }
    assert!(seen > 5, "the fixture printed only {seen} scores to check");
}

// --- the taxonomy and the vocabularies --------------------------------------

/// The four codes v0.4 added are documented, with the exits and the
/// retryability the binary implements. `sqlite_busy` is the one that matters
/// most: an agent that treats it as fatal gives up on a lock that clears.
#[test]
fn the_v04_error_codes_are_documented_with_the_right_exits() {
    let m = manifest(&Sandbox::new());
    let by_code: std::collections::HashMap<&str, &Value> = m["error_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["code"].as_str().unwrap(), e))
        .collect();
    for (code, exit, retryable) in [
        ("wrong_id_kind", 3u64, false),
        ("not_found", 3, false),
        ("conflict_already_resolved", 3, false),
        ("sqlite_busy", 1, true),
    ] {
        let e = by_code
            .get(code)
            .unwrap_or_else(|| panic!("{code} is undocumented"));
        assert_eq!(e["exit"], exit, "{code}");
        assert_eq!(e["retryable"], retryable, "{code}");
        assert!(
            e["suggestion"].as_str().unwrap().contains("engram")
                || e["suggestion"].as_str().unwrap().contains("retry"),
            "{code} suggests nothing runnable"
        );
    }
}

/// R7's vocabulary, published whole. `requires_context` must be advertised as
/// advice and absent from what `--action` accepts, or an agent will pass it.
#[test]
fn the_suggested_action_vocabulary_is_published_and_resolve_action_is_narrower() {
    let m = manifest(&Sandbox::new());
    let suggested: Vec<&str> = m["vocabularies"]["suggested_action"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        suggested,
        vec!["keep-both", "supersede-old", "requires_context"]
    );
    let allowed: Vec<&str> = m["vocabularies"]["resolve_action"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!allowed.contains(&"requires_context"));
    let contract = &m["suggested_action_contract"];
    assert!(contract["requires_context"]
        .as_str()
        .unwrap()
        .contains("ADVICE"));
    assert!(contract["authority"]
        .as_str()
        .unwrap()
        .contains("permission"));
}

/// The replacement the lane reports asked for: the manifest must describe the
/// `resolve` OBJECT, and must no longer tell anyone to run a pipe template.
#[test]
fn the_manifest_documents_the_respond_object_and_not_a_pipe_template() {
    let whole = serde_json::to_string(&manifest(&Sandbox::new())).unwrap();
    assert!(
        !whole.contains("run the included resolve_command"),
        "the manifest still points agents at the old template"
    );
    // The template that made a paste destructive. Any occurrence of the
    // alternation in a command position is the bug, whatever wraps it.
    assert!(
        !whole.contains("--action supersede-old|keep-both|reject-new"),
        "the piped action template is back in the manifest"
    );
    for fragment in ["program", "args", "allowed_actions"] {
        assert!(
            whole.contains(fragment),
            "the respond object omits {fragment}"
        );
    }
}

/// The store-root contract, in the one place an agent looks for it.
#[test]
fn the_store_flag_is_documented_globally_and_accepted_everywhere() {
    let sb = Sandbox::new();
    let m = manifest(&sb);
    let store = m["global_flags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "store")
        .expect("--store is a documented global flag");
    assert_eq!(store["cli_long"], "store");
    assert!(store["doc"].as_str().unwrap().contains("ENGRAM_STORE"));

    // And it really is global: a command that is not `serve` accepts it.
    let scratch = tempfile::TempDir::new().unwrap();
    let out = sb
        .cmd()
        .args(["facts", "list", "--store"])
        .arg(scratch.path())
        .output()
        .expect("facts list --store");
    assert!(
        out.status.success(),
        "--store was rejected: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The status commands answer with exactly the fields their contract names.
/// A block added to `config show` / `config check` / `doctor` (the judge's
/// modes and route, the TypeSafe key) is invisible to an agent until the
/// contract lists it; this fails in the commit that adds it. Keyless, so
/// `config check` probes nothing.
#[test]
fn the_status_commands_answer_with_exactly_their_documented_fields() {
    let sb = Sandbox::new();
    for path in ["config show", "config check", "doctor"] {
        let out = sb
            .cmd()
            .args(path.split_whitespace())
            .arg("--json")
            .output()
            .expect(path);
        assert!(
            out.status.success(),
            "{path}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = Sandbox::json(&out);
        let mut live: Vec<&str> = v["data"]
            .as_object()
            .unwrap_or_else(|| panic!("{path}: data is not an object"))
            .keys()
            .map(String::as_str)
            .collect();
        live.sort();
        let mut named = data_field_names(path);
        named.sort();
        assert_eq!(live, named, "{path}: live data keys vs the contract");
        // Every provider entry says where its key came from, and every
        // problem comes with a fix an agent can act on.
        if path == "config check" || path == "doctor" {
            let providers = v["data"]["providers"].as_array().expect("providers");
            for p in ["gemini", "openrouter", "cohere", "typesafe"] {
                let entry = providers
                    .iter()
                    .find(|e| e["provider"] == p)
                    .unwrap_or_else(|| panic!("{path}: no {p} entry"));
                let source = entry["configured"]["source"].as_str().unwrap_or_default();
                assert!(
                    ["env", "config_file", "unset"].contains(&source),
                    "{path}: {p} source {source:?}"
                );
                if entry["severity"] != "ok" {
                    assert!(entry["fix"].is_string(), "{path}: {p} has no fix");
                }
            }
            assert!(v["data"]["next_steps"].is_array(), "{path}: next_steps");
        }
    }
}

/// What the fact write path reports is what `remember`'s contract names: the
/// gate, queue and adjudication counters in metadata, and on the dedup path a
/// `fact_extraction` value the published vocabulary contains. A value outside
/// the enum fails every agent that validates against the schema.
#[test]
fn remember_reports_only_documented_fact_write_fields() {
    let sb = Sandbox::new();
    let spec = documented("remember");
    let meta_names: Vec<&str> = spec.metadata.iter().map(|f| f.name).collect();
    let run = |content: &str| {
        let out = sb
            .cmd()
            .args([
                "remember",
                content,
                "--kb",
                "personal",
                "--offline",
                "--json",
            ])
            .output()
            .expect("remember");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Sandbox::json(&out)
    };
    // No OpenRouter key in the sandbox: extraction is skipped, never called.
    let first = run("Ana plays the cello.");
    for counter in ["gate", "queued_by_rule", "adjudication"] {
        assert!(
            first["metadata"].get(counter).is_some(),
            "{counter} not emitted"
        );
        assert!(
            meta_names.contains(&counter),
            "{counter} emitted, not documented"
        );
    }
    let again = run("Ana plays the cello.");
    assert_eq!(again["data"]["stored"], false, "{again}");
    let status = again["data"]["fact_extraction"].as_str().unwrap();
    assert_eq!(status, "skipped_duplicate");
    assert!(
        engram_contract::registry::FACT_EXTRACTION.contains(&status),
        "fact_extraction {status:?} is not in the published vocabulary"
    );
    for (path, v) in [("stored", &first), ("dedup", &again)] {
        for field in v["data"].as_object().unwrap().keys() {
            assert!(
                data_field_names("remember").contains(&field.as_str()),
                "{path} data.{field} is not documented"
            );
        }
    }
}
