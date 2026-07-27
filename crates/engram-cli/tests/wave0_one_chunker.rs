//! Wave 0.4 — one chunker for every write path.
//!
//! `remember` chunks through `chunk_memory` + the configured `ChunkPolicy`:
//! paragraphs merged to target, capped at `max_chars`, floored at `min_chars`.
//! `edit --content` and legacy v1 `import` used a bare `naive_split` instead,
//! so a memory rewritten or restored through them came back with exactly the
//! fragments the floor exists to stop — a live store reached 986 chunks under
//! 40 characters, and a birthday recall returned two bare date stamps above the
//! memory that held the birthday.
//!
//! These tests drive the built binary against an isolated HOME, so the
//! developer's real store is never read or written.

use assert_cmd::Command;
use serde_json::Value;

/// The shipped contract, spelled out rather than imported: a test that reads
/// the same constant as the code cannot notice the constant changing.
const MIN_CHARS: usize = 60;
const MAX_CHARS: usize = 1800;

/// Point a spawned binary's data/config/cache at `home`.
/// The `directories` crate honors XDG_* on Linux and HOME on macOS.
fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

/// A command against `home`, with the stub embedder forced on: no API key, no
/// network, deterministic vectors.
fn engram(home: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home.path());
    cmd
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
    serde_json::from_slice(&out).expect("valid JSON on stdout")
}

/// Stored chunk rows for one KB, in storage order. `export` is the only read
/// surface that returns chunk text AND its embedding column.
fn stored_chunk_rows(home: &tempfile::TempDir, kb: &str) -> Vec<Value> {
    let v = run_json(
        &mut engram(home),
        &["export", "--format", "json", "--kb", kb],
    );
    v["data"]["chunks"]
        .as_array()
        .expect("export carries chunks")
        .clone()
}

fn chunk_texts(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["content"].as_str().expect("chunk content").to_string())
        .collect()
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(|w| w.to_string()).collect()
}

/// Every chunk is inside the policy's bounds, and the chunks together still
/// hold every word of `source`, in order.
fn assert_policy_respected(rows: &[Value], source: &str) {
    let texts = chunk_texts(rows);
    assert!(
        texts.len() > 1,
        "a multi-KB document should split: {texts:?}"
    );
    for text in &texts {
        assert!(
            text.len() >= MIN_CHARS,
            "degenerate chunk reached the index: {text:?}"
        );
        // The floor may reattach a small tail to a full chunk — bounded slack,
        // far below the embedder's ~7,800-character window.
        assert!(
            text.len() <= MAX_CHARS + MIN_CHARS,
            "chunk over the ceiling: {}",
            text.len()
        );
    }
    assert!(
        !texts.iter().any(|t| t.trim() == "Sep 16, 2025"),
        "a bare date became its own retrievable chunk"
    );
    let got: Vec<String> = texts.iter().flat_map(|t| words(t)).collect();
    assert_eq!(words(source), got, "chunking lost or reordered content");
}

/// Several KB of the two shapes that break a naive splitter at once: paragraphs
/// alternating date header / entry (fragments the floor must absorb) and one
/// unbroken run with no sentence boundary (which the ceiling must wrap).
fn degenerate_prone_document() -> String {
    let mut doc = String::new();
    for day in 16..24 {
        doc.push_str(&format!("Sep {day}, 2025\n\n"));
        doc.push_str(&format!(
            "Met Noemi at the Belgrade summit on day {day} and she mentioned her birthday is \
             on the 3rd of April, which is worth remembering because she throws a party every \
             year at the family house outside the city.\n\n"
        ));
    }
    doc.push_str(&"reminder ".repeat(500));
    doc.push_str("\n\nOct 2, 2025");
    doc
}

/// One memory in the LEGACY v1 export shape: a bare `{ "memories": [...] }`
/// document with no `version` key. Every field of `engram_core::types::Memory`
/// is present because the importer silently SKIPS an entry it cannot
/// deserialize — a fixture missing one field would make the test vacuous.
fn v1_export_file(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
    let payload = serde_json::json!({
        "memories": [{
            "id": "3f2a1c64-0000-4000-8000-000000000001",
            "content": content,
            "created_at": "2026-01-05T10:00:00Z",
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
    let path = dir.join("legacy-v1-export.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&payload).expect("fixture serializes"),
    )
    .unwrap();
    path
}

/// Store a seed memory in `kb` and return its id.
fn seed_memory(home: &tempfile::TempDir, kb: &str) -> String {
    let v = run_json(
        &mut engram(home),
        &[
            "remember",
            "Seed note, replaced by the edit under test.",
            "--kb",
            kb,
            "--no-extract-facts",
        ],
    );
    v["data"]["id"].as_str().expect("memory id").to_string()
}

#[test]
fn edit_content_is_chunked_by_the_shared_policy() {
    let home = tempfile::tempdir().expect("tmp dir");
    let id = seed_memory(&home, "wave0-edit");

    let doc = degenerate_prone_document();
    assert!(doc.len() > 5_000, "fixture must be several KB");
    let edited = run_json(
        &mut engram(&home),
        &["edit", id.as_str(), "--content", doc.as_str()],
    );
    assert_eq!(edited["data"]["updated"], true);
    let replaced = edited["data"]["chunks_replaced"].as_u64().unwrap();

    let rows = stored_chunk_rows(&home, "wave0-edit");
    assert_eq!(
        rows.len() as u64,
        replaced,
        "the reported count must be the count actually stored"
    );
    assert_policy_respected(&rows, &doc);
    // The header rides along with the entry it introduces rather than vanishing.
    assert!(chunk_texts(&rows)
        .iter()
        .any(|t| t.contains("Sep 16, 2025")));
    // `edit` re-embeds: a chunk without a vector is invisible to dense recall.
    assert!(
        rows.iter().all(|r| r["embedding_b64"].is_string()),
        "an edited chunk reached the index without an embedding"
    );
}

#[test]
fn edit_keeps_a_legitimately_short_memory_as_one_chunk() {
    let home = tempfile::tempdir().expect("tmp dir");
    let id = seed_memory(&home, "wave0-edit-short");
    // 27 characters: under the floor, but it is the WHOLE memory, so it stands.
    let edited = run_json(
        &mut engram(&home),
        &[
            "edit",
            id.as_str(),
            "--content",
            "Lira's birthday is 12 March",
        ],
    );
    assert_eq!(edited["data"]["chunks_replaced"], 1);
    assert_eq!(
        chunk_texts(&stored_chunk_rows(&home, "wave0-edit-short")),
        vec!["Lira's birthday is 12 March".to_string()]
    );
}

#[test]
fn edit_honors_the_chunk_policy_override() {
    // ENGRAM_CHUNK_MIN_CHARS=1 disables the floor, reproducing a pre-fix store.
    // Before the fix this knob had NO effect on `edit` at all; that it bites now
    // is the proof the policy is read there — and that the fixture really is
    // degenerate-prone, so the test above is not passing by accident.
    let home = tempfile::tempdir().expect("tmp dir");
    let id = seed_memory(&home, "wave0-edit-legacy");
    let doc = degenerate_prone_document();

    let mut cmd = engram(&home);
    cmd.env("ENGRAM_CHUNK_MIN_CHARS", "1")
        .env("ENGRAM_CHUNK_TARGET_CHARS", "1");
    run_json(&mut cmd, &["edit", id.as_str(), "--content", doc.as_str()]);

    let texts = chunk_texts(&stored_chunk_rows(&home, "wave0-edit-legacy"));
    assert!(
        texts.iter().any(|t| t.trim() == "Sep 16, 2025"),
        "the floor override must reach `edit`: {texts:?}"
    );
}

#[test]
fn v1_import_chunks_with_the_shared_policy() {
    let home = tempfile::tempdir().expect("tmp dir");
    let doc = degenerate_prone_document();
    let file = v1_export_file(home.path(), &doc);

    let v = run_json(&mut engram(&home), &["import", file.to_str().unwrap()]);
    assert_eq!(v["data"]["format_version"], 1);
    assert_eq!(
        v["data"]["imported"], 1,
        "the fixture must actually import — a skipped entry would make this \
         test vacuous: {}",
        v["data"]
    );
    let created = v["data"]["chunks_created"].as_u64().unwrap();
    // v1 memories land in the KB literally named `default`.
    let rows = stored_chunk_rows(&home, "default");
    assert_eq!(rows.len() as u64, created);
    assert_policy_respected(&rows, &doc);

    // Embeddings stay absent BY DESIGN, and the response says what to run.
    assert!(
        rows.iter().all(|r| r["embedding_b64"].is_null()),
        "v1 import must not invent embeddings"
    );
    assert!(v["data"]["next_step"]
        .as_str()
        .unwrap_or_default()
        .contains("reindex"));
}

#[test]
fn v1_import_honors_the_chunk_policy_override() {
    let home = tempfile::tempdir().expect("tmp dir");
    let file = v1_export_file(home.path(), &degenerate_prone_document());

    let mut cmd = engram(&home);
    cmd.env("ENGRAM_CHUNK_MIN_CHARS", "1")
        .env("ENGRAM_CHUNK_TARGET_CHARS", "1");
    run_json(&mut cmd, &["import", file.to_str().unwrap()]);

    let texts = chunk_texts(&stored_chunk_rows(&home, "default"));
    assert!(
        texts.iter().any(|t| t.trim() == "Sep 16, 2025"),
        "the floor override must reach v1 import: {texts:?}"
    );
}
