//! Wave 3, write surfaces — the id a mutator accepts, what an edit does to the
//! facts underneath it, what a retraction leaves behind, and what a backup
//! carries.
//!
//! Four deliverables under test. W1: every mutator resolves ANY handle kind, so
//! a `chunk:` id copied out of a recall result edits the memory it belongs to
//! and a wrong kind names the handle that would have worked. W2/W5: `forget`
//! answers `not_found` for an id that names nothing, and closes the memory's
//! facts instead of deleting them. W3/W4: `edit --content` closes the old
//! content's claims as `corrected` intervals and reports whether the memory is
//! pending re-extraction. W6/W7: `--facts-json` provenance, and export/import
//! v3 with the journal.
//!
//! Isolation is `ENGRAM_STORE` pointed at a TempDir and nothing else — no HOME
//! rewriting, and therefore no way for a leaked path to reach the developer's
//! real store. Facts, events and markers are asserted by opening that store
//! directly, always AFTER the spawned binary has exited, so the two processes
//! never contend for the writer lock.

use assert_cmd::Command;
use engram_storage::SqliteStore;
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

fn store() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp store")
}

/// An offline `engram` rooted at `store`: stub embedder, no keys, no scripted
/// extractor unless a test asks for one.
fn engram(store: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store);
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("ENGRAM_FACT_LLM_STUB");
    cmd.env_remove("ENGRAM_FACT_WRITE_FAIL");
    cmd.env_remove("ENGRAM_EMBED_FAIL");
    cmd
}

/// The store the spawned binary just wrote, opened for assertions.
fn open(dir: &Path) -> SqliteStore {
    SqliteStore::open(dir.join("db.sqlite")).expect("the temp store opens")
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// The JSON error envelope is the LAST `{`-line on stderr — the write commands
/// also print plain `note:` / `warning:` lines there.
fn parse_error(err: &[u8]) -> Value {
    let line = String::from_utf8_lossy(err)
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("JSON error on stderr")
        .to_string();
    serde_json::from_str(&line).expect("parseable error envelope")
}

fn run_json(cmd: &mut Command, args: &[&str]) -> Value {
    let out = cmd
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

/// Run a command expected to fail, and return its error envelope.
fn run_err(cmd: &mut Command, args: &[&str], code: i32) -> Value {
    let out = cmd
        .args(args)
        .arg("--json")
        .assert()
        .failure()
        .code(code)
        .get_output()
        .clone();
    parse_error(&out.stderr)
}

/// Store one memory and return its id.
fn seed(dir: &Path, content: &str) -> String {
    let v = run_json(
        &mut engram(dir),
        &["remember", content, "--offline", "--no-extract-facts"],
    );
    v["data"]["id"].as_str().expect("memory id").to_string()
}

/// Store one memory with caller-asserted triples, and return its id.
fn seed_with_facts(dir: &Path, content: &str, facts_json: &str) -> String {
    let v = run_json(
        &mut engram(dir),
        &["remember", content, "--offline", "--facts-json", facts_json],
    );
    v["data"]["id"].as_str().expect("memory id").to_string()
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).expect("a uuid")
}

/// The first chunk of a memory, as a `chunk:` handle.
fn first_chunk_handle(dir: &Path, memory_id: &str) -> String {
    let store = open(dir);
    let chunks = store
        .chunks_for_memory(uuid(memory_id))
        .expect("chunks read");
    format!("chunk:{}", chunks.first().expect("one chunk").chunk_id)
}

const ANA_LISBON: &str = r#"[
  {"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0}
]"#;

const TWO_FACTS: &str = r#"[
  {"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0},
  {"subject":"Ana","predicate":"works_at","object":"Acme","confidence":1.0}
]"#;

// ---------------------------------------------------------------------------
// W1 / W8 — every mutator resolves ANY handle kind
// ---------------------------------------------------------------------------

#[test]
fn edit_accepts_a_bare_uuid() {
    let dir = store();
    let id = seed(dir.path(), "Ana moved to Lisbon.");
    let v = run_json(
        &mut engram(dir.path()),
        &["edit", &id, "--content", "Ana moved to Porto.", "--offline"],
    );
    assert_eq!(v["data"]["updated"], true);
    assert_eq!(v["data"]["id"], id);
}

#[test]
fn edit_accepts_a_mem_prefixed_handle() {
    let dir = store();
    let id = seed(dir.path(), "Bruno joined Acme.");
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "edit",
            &format!("mem:{id}"),
            "--content",
            "Bruno joined Globex.",
            "--offline",
        ],
    );
    assert_eq!(v["data"]["updated"], true);
    // The echoed id stays a bare uuid whatever spelling came in — scripts feed
    // `data.id` straight back into the next command — and the typed handle
    // rides beside it.
    assert_eq!(v["data"]["id"], id);
    assert_eq!(v["data"]["handle"], format!("mem:{id}"));
}

#[test]
fn edit_accepts_a_chunk_handle_and_edits_its_memory() {
    // THE HEADLINE TRAP. A recall result hands back chunk ids; feeding one to
    // `edit` used to die with "no memory found … (already deleted?)", and the
    // agent concluded a race and rebuilt the memory by hand.
    let dir = store();
    let id = seed(dir.path(), "Ana moved to Lisbon in March.");
    let handle = first_chunk_handle(dir.path(), &id);
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "edit",
            &handle,
            "--content",
            "Ana moved to Porto in March.",
            "--offline",
        ],
    );
    assert_eq!(v["data"]["updated"], true);
    assert_eq!(v["data"]["id"], id);
    // The mapping is reported, so the agent learns it instead of re-deriving it.
    assert_eq!(v["data"]["resolved_from"], handle);

    let listing = run_json(&mut engram(dir.path()), &["memory", "list"]);
    let text = serde_json::to_string(&listing).unwrap();
    assert!(text.contains("Ana moved to Porto in March."), "{text}");
}

#[test]
fn edit_on_a_fact_handle_offers_the_owning_memory() {
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let fact_id = open(dir.path()).list_facts(None, true, 10).unwrap()[0].id;

    let err = run_err(
        &mut engram(dir.path()),
        &["edit", &format!("fact:{fact_id}"), "--content", "x"],
        3,
    );
    assert_eq!(err["error"]["code"], "wrong_id_kind");
    let msg = err["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains(&format!("fact:{fact_id}")), "{msg}");
    // A fact id names a claim, not the memory a mutator rewrites — but the
    // handle that WOULD work is in the suggestion, so the retry costs no round
    // trip.
    let hint = err["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(hint.contains(&format!("mem:{id}")), "{hint}");
}

#[test]
fn edit_on_a_mem_prefixed_chunk_uuid_is_wrong_id_kind() {
    // The two-step probe. Storage's first probe looks only in `memories`, so a
    // chunk uuid spelled `mem:` misses; without the second probe this is
    // reported as `not_found` and the caller hunts for a memory that exists.
    let dir = store();
    let id = seed(dir.path(), "Ana moved to Lisbon.");
    let chunk = first_chunk_handle(dir.path(), &id);
    let chunk_uuid = chunk.trim_start_matches("chunk:");

    let err = run_err(
        &mut engram(dir.path()),
        &["edit", &format!("mem:{chunk_uuid}"), "--importance", "4"],
        3,
    );
    assert_eq!(err["error"]["code"], "wrong_id_kind");
    let hint = err["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(hint.contains("chunk"), "{hint}");
    assert!(hint.contains(&format!("mem:{id}")), "{hint}");
}

#[test]
fn an_unknown_uuid_is_not_found_whichever_way_it_is_spelled() {
    let dir = store();
    seed(dir.path(), "Something else entirely.");
    let absent = Uuid::new_v4().to_string();

    for spelling in [absent.clone(), format!("mem:{absent}")] {
        let err = run_err(
            &mut engram(dir.path()),
            &["edit", &spelling, "--importance", "3"],
            3,
        );
        // Both probes miss, so this is an absence and not a wrong kind.
        assert_eq!(err["error"]["code"], "not_found", "{spelling}");
    }
}

#[test]
fn edit_on_a_malformed_id_keeps_the_legacy_message() {
    let dir = store();
    let err = run_err(
        &mut engram(dir.path()),
        &["edit", "not-a-uuid", "--content", "x", "--offline"],
        3,
    );
    assert_eq!(err["error"]["code"], "bad_input");
    assert!(
        err["error"]["message"]
            .as_str()
            .expect("message")
            .contains("invalid UUID"),
        "{err}"
    );
}

#[test]
fn an_unknown_prefix_is_reported_as_an_unknown_kind() {
    let dir = store();
    let id = seed(dir.path(), "Prefix taxonomy.");
    let err = run_err(
        &mut engram(dir.path()),
        &["edit", &format!("blob:{id}"), "--importance", "3"],
        3,
    );
    let msg = err["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("unknown id kind `blob`"), "{msg}");
    assert!(msg.contains("mem, chunk, fact"), "{msg}");
}

#[test]
fn the_memory_aliases_resolve_handles_identically() {
    let dir = store();
    let id = seed(dir.path(), "Reachable through the memory alias.");
    let chunk = first_chunk_handle(dir.path(), &id);

    let v = run_json(
        &mut engram(dir.path()),
        &["memory", "update", &chunk, "--importance", "7"],
    );
    assert_eq!(v["data"]["updated"], true);
    assert_eq!(v["data"]["resolved_from"], chunk);

    let v = run_json(
        &mut engram(dir.path()),
        &["memory", "delete", &chunk, "--confirm"],
    );
    assert_eq!(v["data"]["deleted"], true);
    assert_eq!(v["data"]["id"], id);
}

#[test]
fn every_write_response_carries_a_mem_handle() {
    let dir = store();
    let stored = run_json(
        &mut engram(dir.path()),
        &[
            "remember",
            "Handles everywhere.",
            "--offline",
            "--no-extract-facts",
        ],
    );
    let id = stored["data"]["id"].as_str().unwrap().to_string();
    let edited = run_json(&mut engram(dir.path()), &["edit", &id, "--importance", "2"]);
    let forgotten = run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    for v in [&stored, &edited, &forgotten] {
        let handle = v["data"]["handle"].as_str().expect("a handle");
        assert_eq!(handle, format!("mem:{id}"), "{v}");
    }
}

#[test]
fn a_memory_listing_carries_handles_too() {
    let dir = store();
    let id = seed(dir.path(), "Listed with its handle.");
    let v = run_json(&mut engram(dir.path()), &["memory", "list"]);
    let rows = v["data"]["memories"].as_array().expect("memories");
    assert_eq!(rows[0]["handle"], format!("mem:{id}"));
    // The legacy field is untouched beside it.
    assert_eq!(rows[0]["id"], id);
}

// ---------------------------------------------------------------------------
// W2 / W5 — forget answers honestly, and retracts without erasing
// ---------------------------------------------------------------------------

#[test]
fn forget_on_an_unknown_id_is_exit_3_not_found() {
    // THE HEADLINE REGRESSION. v0.3 answered exit 0 `{"deleted": false}`, so a
    // typo was indistinguishable from a successful retraction.
    let dir = store();
    seed(dir.path(), "A memory that is not the one being forgotten.");
    let err = run_err(
        &mut engram(dir.path()),
        &["forget", &Uuid::new_v4().to_string(), "--confirm"],
        3,
    );
    assert_eq!(err["error"]["code"], "not_found");
    assert_eq!(err["status"], "error");
}

#[test]
fn forget_twice_is_idempotent_and_still_exit_0() {
    let dir = store();
    let id = seed(dir.path(), "Retract me twice.");
    let first = run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    assert_eq!(first["data"]["deleted"], true);
    assert_eq!(first["data"]["already_retracted"], false);

    let second = run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    assert_eq!(second["data"]["deleted"], false);
    assert_eq!(second["data"]["already_retracted"], true);
}

#[test]
fn forget_without_confirm_refuses_before_it_resolves_anything() {
    let dir = store();
    let err = run_err(
        &mut engram(dir.path()),
        &["forget", &Uuid::new_v4().to_string()],
        3,
    );
    // The destructive guard precedes resolution, so an unknown id with no
    // `--confirm` is still the missing flag and never `not_found`.
    assert_eq!(err["error"]["code"], "bad_input");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("--confirm"));
}

#[test]
fn forget_accepts_a_chunk_handle_and_retracts_its_memory() {
    let dir = store();
    let id = seed(dir.path(), "Retracted through one of its chunks.");
    let chunk = first_chunk_handle(dir.path(), &id);
    let v = run_json(&mut engram(dir.path()), &["forget", &chunk, "--confirm"]);
    assert_eq!(v["data"]["deleted"], true);
    assert_eq!(v["data"]["id"], id);
    assert_eq!(v["data"]["resolved_from"], chunk);
}

#[test]
fn forget_keeps_facts_as_closed_intervals() {
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana moved to Lisbon.", TWO_FACTS);
    let before: Vec<Uuid> = open(dir.path())
        .list_facts(None, true, 10)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(before.len(), 2);

    let v = run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    assert_eq!(v["data"]["facts_closed"], 2);
    let reported: Vec<Uuid> = v["data"]["facts_closed_ids"]
        .as_array()
        .expect("ids")
        .iter()
        .map(|s| uuid(s.as_str().unwrap()))
        .collect();
    assert_eq!(reported.len(), 2);

    // Not deleted: closed, with the reason D5 pins for a retraction, and
    // `superseded_by` left NULL because nothing succeeded them.
    let store = open(dir.path());
    for fact_id in before {
        let f = store.get_fact(fact_id).unwrap().expect("fact survives");
        assert!(f.valid_until.is_some(), "{f:?}");
        assert_eq!(f.close_reason.as_deref(), Some("retracted_source"));
        assert_eq!(f.close_authority.as_deref(), Some("caller"));
        assert!(f.superseded_by.is_none(), "{f:?}");
    }
}

#[test]
fn forget_appends_memory_retracted_beside_each_closed_interval() {
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana moved to Lisbon.", ANA_LISBON);
    run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);

    let store = open(dir.path());
    let kinds: Vec<String> = store
        .events_for_subject(uuid(&id))
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str().to_string())
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| *k == "memory.retracted").count(),
        1,
        "exactly one retraction event, appended by storage: {kinds:?}"
    );
    let fact_id = store.list_facts(None, true, 10).unwrap()[0].id;
    let closes: Vec<String> = store
        .events_for_subject(fact_id)
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str().to_string())
        .filter(|k| k == "fact.interval_closed")
        .collect();
    assert_eq!(closes.len(), 1, "one close event per closed fact");
}

#[test]
fn a_retracted_memory_is_invisible_to_the_listing() {
    let dir = store();
    let id = seed(dir.path(), "Soft delete still hides content.");
    run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    let v = run_json(&mut engram(dir.path()), &["memory", "list"]);
    let text = serde_json::to_string(&v).unwrap();
    assert!(!text.contains("Soft delete still hides content."), "{text}");
}

#[test]
fn editing_a_retracted_memory_is_not_found() {
    let dir = store();
    let id = seed(dir.path(), "Retracted, then edited.");
    run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    let err = run_err(
        &mut engram(dir.path()),
        &["edit", &id, "--importance", "9"],
        3,
    );
    assert_eq!(err["error"]["code"], "not_found");
}

// ---------------------------------------------------------------------------
// W3 / W4 — an edit closes facts, it does not delete them
// ---------------------------------------------------------------------------

#[test]
fn edit_no_longer_deletes_facts_and_closes_them_as_corrected() {
    // THE MOVE-1 HEADLINE. v0.3 called `delete_facts_for_memory`, so a typo fix
    // erased every claim derived from the memory — and the FK cascade took the
    // adjudication queue with it.
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana moved to Lisbon.", TWO_FACTS);
    let mut before: Vec<Uuid> = open(dir.path())
        .list_facts(None, true, 10)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    before.sort();

    let v = run_json(
        &mut engram(dir.path()),
        &[
            "edit",
            &id,
            "--content",
            "Ana moved to Porto, and left Acme.",
            "--offline",
        ],
    );
    assert_eq!(v["data"]["facts_invalidated"], 2);
    let mut reported: Vec<Uuid> = v["data"]["facts_invalidated_ids"]
        .as_array()
        .expect("ids")
        .iter()
        .map(|s| uuid(s.as_str().unwrap()))
        .collect();
    reported.sort();
    assert_eq!(reported, before);

    let store = open(dir.path());
    for fact_id in before {
        let f = store.get_fact(fact_id).unwrap().expect("fact survives");
        assert!(f.valid_until.is_some(), "closed, not deleted: {f:?}");
        assert_eq!(f.close_reason.as_deref(), Some("corrected"));
        assert_eq!(f.close_authority.as_deref(), Some("caller"));
        // A corrected fact was not superseded by a newer value, so the compat
        // columns stay NULL and `is_open_fact` is the only correct test.
        assert!(f.superseded_by.is_none(), "{f:?}");
    }
}

#[test]
fn edit_preserves_the_adjudication_queue() {
    // The FK cascade `fact_conflict_queue → facts` is no longer reachable from
    // a write surface, so the questions a person was asked survive the edit.
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana lives in Lisbon.", ANA_LISBON);
    // A second, contradicting single-valued claim queues a question rather than
    // picking a side — `--autonomy strict` is the setting that always asks.
    run_json(
        &mut engram(dir.path()),
        &[
            "remember",
            "Ana lives in Porto now.",
            "--offline",
            "--autonomy",
            "strict",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"lives_in","object":"Porto","confidence":1.0}]"#,
        ],
    );
    let queued_before = open(dir.path())
        .count_open_fact_conflicts(None, None)
        .unwrap();
    assert!(queued_before > 0, "the seed must actually queue something");

    run_json(
        &mut engram(dir.path()),
        &[
            "edit",
            &id,
            "--content",
            "Ana lived in Lisbon.",
            "--offline",
        ],
    );
    assert_eq!(
        open(dir.path())
            .count_open_fact_conflicts(None, None)
            .unwrap(),
        queued_before,
        "an edit must not erase the adjudication queue"
    );
}

#[test]
fn edit_appends_memory_revised_with_both_hashes() {
    let dir = store();
    let id = seed(dir.path(), "The original wording.");
    run_json(
        &mut engram(dir.path()),
        &[
            "edit",
            &id,
            "--content",
            "The corrected wording.",
            "--offline",
        ],
    );

    let store = open(dir.path());
    let events = store.events_for_subject(uuid(&id)).unwrap();
    let written = events
        .iter()
        .find(|e| e.kind.as_str() == "memory.written")
        .expect("genesis");
    let revised = events
        .iter()
        .filter(|e| e.kind.as_str() == "memory.revised")
        .collect::<Vec<_>>();
    assert_eq!(
        revised.len(),
        1,
        "exactly one revision, appended by storage"
    );
    let prev = revised[0].payload["prev_content_hash"].as_str().unwrap();
    let now = revised[0].payload["content_hash"].as_str().unwrap();
    assert_eq!(prev, written.payload["content_hash"].as_str().unwrap());
    assert_ne!(prev, now);
    assert_eq!(
        now,
        engram_core::keys::content_hash("The corrected wording.")
    );
}

#[test]
fn edit_importance_only_changes_no_fact_and_needs_no_reextraction() {
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let v = run_json(&mut engram(dir.path()), &["edit", &id, "--importance", "9"]);
    assert_eq!(v["data"]["facts_invalidated"], 0);
    assert_eq!(v["data"]["reextraction"], "none");

    let store = open(dir.path());
    let f = &store.list_facts(None, true, 10).unwrap()[0];
    assert!(f.valid_until.is_none(), "an importance edit closes nothing");
    let kinds: Vec<String> = store
        .events_for_subject(uuid(&id))
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str().to_string())
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| *k == "memory.meta_changed").count(),
        1
    );
    assert!(
        !kinds.iter().any(|k| k == "fact.interval_closed"),
        "{kinds:?}"
    );
}

#[test]
fn edit_marks_the_memory_pending_reextraction() {
    // The marker really is cleared, so `facts backfill` finds the memory again
    // — the re-extraction time bomb was that it did not.
    let dir = store();
    let content = "Ana moved to Lisbon.";
    let id = seed(dir.path(), content);
    // A remember that ran no extraction leaves no marker, so plant one exactly
    // as a completed extraction would.
    {
        let store = open(dir.path());
        // Through the WELDED path (G6): a marker planted without its
        // `derivation.ran` event is a state `admin rebuild` cannot reproduce,
        // and this test's whole subject is what an edit does to that marker.
        store
            .record_derivation(
                uuid(&id),
                &engram_core::keys::content_hash(content),
                engram_storage::journal::EXTRACTOR_FACT_LLM,
                Some("test-model"),
                &[],
                "cli:default",
            )
            .unwrap();
        assert!(store
            .list_memories_pending_fact_extraction(None, None, None)
            .unwrap()
            .is_empty());
    }

    let v = run_json(
        &mut engram(dir.path()),
        &["edit", &id, "--content", "Ana moved to Porto.", "--offline"],
    );
    assert_eq!(v["data"]["reextraction"], "pending");
    let pending = open(dir.path())
        .list_memories_pending_fact_extraction(None, None, None)
        .unwrap();
    assert_eq!(pending.len(), 1, "the edited memory is pending again");
    assert_eq!(pending[0].1.id, uuid(&id));
}

#[test]
fn edit_reports_replayed_when_the_ledger_already_holds_this_content() {
    // `"replayed"` is only honest when the derivation for this exact content
    // belongs to this memory AND its outputs are still open: nothing in wave 3
    // re-observes closed facts, so a ledger hit alone would be a claim that the
    // facts are current when they are not.
    let dir = store();
    let content = "Ana moved to Lisbon.";
    let id = seed_with_facts(dir.path(), content, ANA_LISBON);
    {
        let store = open(dir.path());
        let fact_ids: Vec<Uuid> = store
            .list_facts(None, true, 10)
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        store
            .record_derivation(
                uuid(&id),
                &engram_core::keys::content_hash(content),
                engram_storage::journal::EXTRACTOR_FACT_LLM,
                Some("test-model"),
                &fact_ids,
                "cli:default",
            )
            .unwrap();
    }

    // The same content back in: nothing to invalidate, nothing to re-derive.
    let v = run_json(
        &mut engram(dir.path()),
        &["edit", &id, "--content", content, "--offline"],
    );
    assert_eq!(v["data"]["reextraction"], "replayed");
    assert_eq!(v["data"]["facts_invalidated"], 0);

    let store = open(dir.path());
    let f = &store.list_facts(None, true, 10).unwrap()[0];
    assert!(f.valid_until.is_none(), "a replay closes nothing");
    // …and the memory is NOT pending, because this content is already derived.
    assert!(store
        .list_memories_pending_fact_extraction(None, None, None)
        .unwrap()
        .is_empty());
}

#[test]
fn edit_is_atomic_under_an_injected_embed_failure() {
    let dir = store();
    let id = seed_with_facts(dir.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let events_before = open(dir.path()).count_events().unwrap();

    engram(dir.path())
        .env("ENGRAM_EMBED_FAIL", "simulated 429")
        .args(["edit", &id, "--content", "Ana moved to Porto.", "--offline"])
        .arg("--json")
        .assert()
        .failure();

    let store = open(dir.path());
    // Content, facts and journal all as they were: the failure lands before any
    // write, so there is no orphan `memory.revised` and no closed interval.
    let f = &store.list_facts(None, true, 10).unwrap()[0];
    assert!(f.valid_until.is_none(), "{f:?}");
    assert_eq!(store.count_events().unwrap(), events_before);
    let listing = run_json(&mut engram(dir.path()), &["memory", "list"]);
    assert!(serde_json::to_string(&listing)
        .unwrap()
        .contains("Ana moved to Lisbon."));
}

// ---------------------------------------------------------------------------
// W6 — a caller-asserted triple is not an extractor's guess
// ---------------------------------------------------------------------------

/// A scripted extractor that always answers with a DIFFERENT subject, so a
/// mixed run has one triple from each door and no adjudication between them.
const EXTRACTOR_SCRIPT: &str = r#"{
  "*": [
    {"subject":"Bruno","predicate":"works_at","object":"Acme","confidence":1.0,"fact_type":"profile_slot"}
  ]
}"#;

/// Every fact in the store, as `(subject, source_kind)`.
fn facts(dir: &Path) -> Vec<(String, String)> {
    let v = run_json(
        &mut engram(dir),
        &["facts", "list", "--kb", "*", "--diary", "*"],
    );
    v["data"]["facts"]
        .as_array()
        .expect("facts array")
        .iter()
        .map(|f| {
            (
                f["subject"].as_str().unwrap_or_default().to_string(),
                f["source_kind"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn facts_json_is_stamped_user_stated() {
    let dir = store();
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "remember",
            "Ana moved to Lisbon.",
            "--offline",
            "--facts-json",
            ANA_LISBON,
        ],
    );
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["facts_source_kind"], "user_stated");
    // The caller did the extraction, so no LLM ran — unchanged from v0.3.
    assert_eq!(v["data"]["fact_extraction"], "skipped_facts_json");

    let rows = facts(dir.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0], ("Ana".to_string(), "user_stated".to_string()));
}

#[test]
fn extraction_stays_agent_extracted() {
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", EXTRACTOR_SCRIPT);
    let v = run_json(&mut cmd, &["remember", "Bruno joined Acme.", "--offline"]);
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["facts_source_kind"], "agent_extracted");

    let rows = facts(dir.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0],
        ("Bruno".to_string(), "agent_extracted".to_string())
    );
}

#[test]
fn a_mixed_run_keeps_both_provenances() {
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", EXTRACTOR_SCRIPT);
    // `--extract-facts` alongside `--facts-json` is the ONLY way both doors
    // open on one write; without it the caller's triples suppress extraction.
    let v = run_json(
        &mut cmd,
        &[
            "remember",
            "Ana moved to Lisbon and Bruno joined Acme.",
            "--offline",
            "--facts-json",
            ANA_LISBON,
            "--extract-facts",
        ],
    );
    assert_eq!(v["data"]["facts_added"], 2);
    assert_eq!(v["data"]["facts_source_kind"], "mixed");

    let mut rows = facts(dir.path());
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("Ana".to_string(), "user_stated".to_string()),
            ("Bruno".to_string(), "agent_extracted".to_string()),
        ]
    );
}

#[test]
fn a_failing_extraction_batch_leaves_the_caller_batch_committed() {
    // The landmine of the two-batch split: the old guard keyed on the FLAG, so
    // an extraction failure on a `--facts-json` run hard-errored and reported
    // the caller's triples as lost when they were already in the store.
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", EXTRACTOR_SCRIPT);
    cmd.env("ENGRAM_FACT_WRITE_FAIL", "Bruno");
    let v = run_json(
        &mut cmd,
        &[
            "remember",
            "Ana moved to Lisbon and Bruno joined Acme.",
            "--offline",
            "--facts-json",
            ANA_LISBON,
            "--extract-facts",
        ],
    );
    assert_eq!(v["data"]["fact_extraction"], "failed_persist");
    assert_eq!(v["data"]["facts_source_kind"], "user_stated");

    let rows = facts(dir.path());
    assert_eq!(
        rows,
        vec![("Ana".to_string(), "user_stated".to_string())],
        "the caller's committed batch survives an extraction failure"
    );
}

#[test]
fn a_failing_caller_batch_still_hard_errors() {
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_WRITE_FAIL", "Ana");
    let out = cmd
        .args([
            "remember",
            "Ana moved to Lisbon.",
            "--offline",
            "--facts-json",
            ANA_LISBON,
        ])
        .arg("--json")
        .assert()
        .failure()
        .get_output()
        .clone();
    let msg = parse_error(&out.stderr)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(msg.contains("--facts-json:"), "{msg}");
    assert!(
        facts(dir.path()).is_empty(),
        "a failed batch stores nothing"
    );
}

#[test]
fn the_duplicate_content_path_stamps_the_same_source_kind() {
    let dir = store();
    let content = "Ana moved to Lisbon.";
    run_json(
        &mut engram(dir.path()),
        &["remember", content, "--offline", "--facts-json", ANA_LISBON],
    );
    // The identical command again: the content dedups, the caller's triples are
    // re-offered against the existing memory, and provenance does not drift.
    let v = run_json(
        &mut engram(dir.path()),
        &["remember", content, "--offline", "--facts-json", ANA_LISBON],
    );
    assert_eq!(v["data"]["deduplicated"], true);
    assert_eq!(v["data"]["facts_source_kind"], "user_stated");
    // The dedup answer names another memory's id, which is exactly where a
    // bare uuid gets copied into the next command — so it carries a handle.
    assert_eq!(
        v["data"]["handle"],
        format!("mem:{}", v["data"]["id"].as_str().unwrap())
    );

    let rows = facts(dir.path());
    assert_eq!(rows, vec![("Ana".to_string(), "user_stated".to_string())]);
}

// ---------------------------------------------------------------------------
// W7 — export v3 carries the journal, and import replays it
// ---------------------------------------------------------------------------

fn export_to_file(dir: &Path, file: &Path) -> Value {
    let v = run_json(&mut engram(dir), &["export"]);
    std::fs::write(file, serde_json::to_vec(&v["data"]).unwrap()).expect("export written");
    v
}

#[test]
fn export_emits_version_3_with_a_journal_slice_and_every_v2_section() {
    let dir = store();
    seed_with_facts(dir.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let v = run_json(&mut engram(dir.path()), &["export"]);
    let data = &v["data"];
    assert_eq!(data["version"], 3);
    assert!(data["schema_version"].as_i64().unwrap() >= 10);
    let events = data["events"].as_array().expect("a journal slice");
    assert!(!events.is_empty());
    assert_eq!(data["counts"]["events"], events.len());
    // A v2 consumer still finds everything where it left it.
    for section in ["kbs", "documents", "memories", "chunks", "facts"] {
        assert!(data[section].is_array(), "missing {section}");
    }
    assert!(data["chunks"][0]["embedding_b64"].is_string());
}

#[test]
fn export_then_import_round_trips_into_an_empty_store() {
    let source = store();
    let id = seed_with_facts(source.path(), "Ana moved to Lisbon.", TWO_FACTS);
    let file = source.path().join("backup.json");
    export_to_file(source.path(), &file);

    let target = store();
    let v = run_json(
        &mut engram(target.path()),
        &["import", file.to_str().unwrap()],
    );
    assert_eq!(v["data"]["format_version"], 3);
    assert!(v["data"]["replayed"]["events"].as_u64().unwrap() > 0);
    assert!(v["data"]["imported"]["chunks"].as_u64().unwrap() >= 1);

    let before = open(source.path());
    let after = open(target.path());
    let mut a: Vec<Uuid> = before
        .list_facts(None, true, 50)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    let mut b: Vec<Uuid> = after
        .list_facts(None, true, 50)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same facts, with the same ids");
    // The vectors came across, so recall works without a reindex.
    assert_eq!(after.chunks_for_memory(uuid(&id)).unwrap().len(), 1);
    let recalled = run_json(
        &mut engram(target.path()),
        &["recall", "Lisbon", "--kb", "*"],
    );
    assert_eq!(recalled["status"], "success");
}

#[test]
fn a_round_trip_preserves_closed_intervals() {
    let source = store();
    let id = seed_with_facts(source.path(), "Ana moved to Lisbon.", ANA_LISBON);
    run_json(&mut engram(source.path()), &["forget", &id, "--confirm"]);
    let file = source.path().join("backup.json");
    export_to_file(source.path(), &file);

    let target = store();
    run_json(
        &mut engram(target.path()),
        &["import", file.to_str().unwrap()],
    );
    let f = &open(target.path()).list_facts(None, true, 50).unwrap()[0];
    assert!(f.valid_until.is_some(), "a closed interval crossed intact");
    assert_eq!(f.close_reason.as_deref(), Some("retracted_source"));
}

#[test]
fn importing_the_same_v3_file_twice_adds_nothing() {
    let source = store();
    seed_with_facts(source.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let file = source.path().join("backup.json");
    export_to_file(source.path(), &file);

    let target = store();
    run_json(
        &mut engram(target.path()),
        &["import", file.to_str().unwrap()],
    );
    let events = open(target.path()).count_events().unwrap();
    let facts_before = open(target.path())
        .list_facts(None, true, 50)
        .unwrap()
        .len();

    let second = run_json(
        &mut engram(target.path()),
        &["import", file.to_str().unwrap()],
    );
    assert_eq!(second["data"]["replayed"]["events"], 0);
    assert!(
        second["data"]["skipped_existing"]["events"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(open(target.path()).count_events().unwrap(), events);
    assert_eq!(
        open(target.path())
            .list_facts(None, true, 50)
            .unwrap()
            .len(),
        facts_before
    );
}

#[test]
fn importing_a_v2_file_synthesizes_a_journal() {
    // A v2 file has no events. A store restored without a journal cannot be
    // rebuilt or audited, so one is synthesized from the rows it does carry.
    let source = store();
    seed_with_facts(source.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let v = run_json(&mut engram(source.path()), &["export"]);
    let mut body = v["data"].clone();
    body["version"] = serde_json::json!(2);
    body.as_object_mut().unwrap().remove("events");
    let file = source.path().join("v2.json");
    std::fs::write(&file, serde_json::to_vec(&body).unwrap()).unwrap();

    let target = store();
    let imported = run_json(
        &mut engram(target.path()),
        &["import", file.to_str().unwrap()],
    );
    assert_eq!(imported["data"]["format_version"], 2);
    assert!(imported["data"]["genesis_events"].as_u64().unwrap() > 0);
    assert!(open(target.path()).count_events().unwrap() > 0);
}

#[test]
fn importing_a_v1_file_still_works() {
    let dir = store();
    let legacy = serde_json::json!({
        "version": 1,
        "memories": [{
            "id": Uuid::new_v4().to_string(),
            "content": "A legacy memory, long enough to chunk into something real.",
            "created_at": "2026-01-01T00:00:00Z",
            "event_time": null,
            "importance": 5,
            "emotional_weight": 0,
            "access_count": 0,
            "last_accessed": null,
            "stability": 1.0,
            "source": "manual",
            "diary": "default",
            "valid_from": null,
            "valid_until": null,
            "tags": []
        }]
    });
    let file = dir.path().join("v1.json");
    std::fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let v = run_json(&mut engram(dir.path()), &["import", file.to_str().unwrap()]);
    assert_eq!(v["data"]["format_version"], 1);
    assert_eq!(v["data"]["imported"], 1);
    // `insert_memory` journals its own `memory.written`, so even the legacy
    // path lands with a history.
    assert!(open(dir.path()).count_events().unwrap() > 0);
}

#[test]
fn a_malformed_slice_is_rejected_and_changes_nothing() {
    let source = store();
    seed_with_facts(source.path(), "Ana moved to Lisbon.", ANA_LISBON);
    let v = run_json(&mut engram(source.path()), &["export"]);
    let mut body = v["data"].clone();
    body["events"][0]["ts"] = serde_json::json!("the day before yesterday");
    let file = source.path().join("broken.json");
    std::fs::write(&file, serde_json::to_vec(&body).unwrap()).unwrap();

    let target = store();
    // Storage rejects it — uniformly, so a daemon import refuses the same file.
    let err = run_err(
        &mut engram(target.path()),
        &["import", file.to_str().unwrap()],
        3,
    );
    assert!(
        ["bad_input", "malformed_json"]
            .contains(&err["error"]["code"].as_str().unwrap_or_default()),
        "{err}"
    );
    assert_eq!(open(target.path()).count_events().unwrap(), 0);
    assert!(open(target.path())
        .list_facts(None, true, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn the_store_under_test_is_the_temp_dir() {
    // The guard that catches a leaked path before it reaches a real store.
    let dir = store();
    seed(dir.path(), "Isolation check.");
    assert!(
        dir.path().join("db.sqlite").exists(),
        "store is the TempDir"
    );
}
