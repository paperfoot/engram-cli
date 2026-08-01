# engram v0.4 architecture redesign — build brief

Audience: implementation and spec agents. This document is the shared ground truth for the `arch/v0.4` build. Read it fully before touching anything.

## Why (30-second version)

A 2026-08-01 agent-experience audit of v0.3.1 (repro'd on the shipped binary, file:line-cited) found ~30 defects that collapse into five root causes. engram's retrieval is already elite (LongMemEval 0.91 R@1); everything broken is in the write/mutate/adjudicate layer. We are rebuilding that layer with five architectural moves rather than patching symptoms.

## The five moves (target architecture)

1. **Append-only journal as source of truth.** New `events` table; every current table becomes a rebuildable projection. Edit = revision event; forget = retraction event; adjudication = decision event. Nothing is ever hard-deleted. Export = journal slice (lossless). Kills: `edit --content` hard-deleting facts + cascading away the whole adjudication ledger (edit.rs:190, sqlite.rs delete_facts_for_memory:1925, FK CASCADE), forget hard-deleting facts under a soft-deleted memory (sqlite.rs:1855-1869), keep-both decisions being write-only, lossy export/import.
2. **Bi-temporal interval facts.** A fact = observation about a state interval: (subject, relation, object, valid_from, valid_until|open, observed_at, source_event). Coexistence is the default; a second medication is a second open interval, NOT a conflict. Contradiction = temporally incompatible intervals on a single-valued relation, or same-identity overlap disagreement. Safety relations (takes_medication, allergic_to, diagnosed_with) become MULTI-VALUED SETS whose interval-CLOSING is safety-gated; blood_type stays single-valued. Kills: the N·(N-1)/2 polypharmacy conflict storm (5 meds → 10 queued pairs, reproduced; cause: safety check fires before multi-valued check, fact_write.rs:245 vs :267; arity from slot_policy, fact_write.rs:136-142), `suggested_action: "supersede-old"` on safety pairs (fact_write.rs:1209-1213), the 951-row undrainable backlog. Precedent: Graphiti bi-temporal edges (valid_at/invalid_at + created_at/expired_at; invalidate, never delete).
3. **Content-addressed derivation + decision replay.** Derivation ledger keyed (content_hash, extractor_id@version): extraction is a memoized pure function of content; edits re-derive only changed spans. Decisions recorded as journal events keyed by SEMANTIC identity (subject_norm, relation, object_pair_hash), replayed automatically on re-derivation — the store never asks the same adjudication question twice. Kills: the re-extraction time bomb (markers hard-deleted on edit; backfill pending predicate is marker-only, contradicting agent-info; test wave05_fact_batch.rs:202-234 pins it), storm recurrence after every edit.
4. **Single-writer core; CLI + daemon as thin clients; jobs.** One core library owns the store behind a single writer + job queue. engramd hosts it; the CLI embeds the same core when no daemon (local-first stays). compile/backfill become async, resumable jobs. One response builder serves CLI and HTTP. Kills: concurrent `facts resolve` race that can supersede both facts in a cycle with two success reports (facts.rs:451-506 non-transactional, ignores claim boolean), write locks held across network calls, compile blowing 1700s timeouts, /v1/recall missing the conflicts array (serve.rs:577-610), single-read() HTTP framing.
5. **Generated contract + typed handles + epistemic reads.** Kind-prefixed handles (mem:/chunk:/fact:/conflict:/entity:/doc:) with legacy bare-UUID acceptance; every read emits full provenance chain (chunk → memory → document); every mutator resolves any handle kind or returns typed `wrong_id_kind` naming the right one. One schema module generates clap defs, agent-info, SKILL.md, JSON Schema, and contract tests. Every read returns scope-searched, true totals + cursors, calibrated confidence, and a `no_match` verdict (absence certificate). Kills: recall returning chunk ids that edit/forget reject ("no memory found … (already deleted?)" + boilerplate suggestion), forget exit-0-success-deleted:false on unknown ids (forget.rs:17-21), count-meaning-page (agents reported 3 conflicts when 953 were open), agent-info/skill/behavior drift, clap errors bypassing the JSON envelope with the wrong exit code, --top-k vs --limit vocabulary drift.

Plus two primitives that fall out of move 1: **store profiles/isolation** (`--store <path>` / `ENGRAM_HOME`, ephemeral test stores; `agent-info` must not open or migrate a store — today dispatch creates AppContext before matching, lib.rs:19-21) and **fork/diff/promote branches** (journal diff).

Stage 5 (reasoning layer): `engram state <subject>` — compiled current-state object (open intervals, provenance, open questions); a procedural/experience memory kind (lessons about the work, fed by the decision journal); reflection as a background job.

## Hard constraints — violating any of these fails the lane

1. **NEVER touch the live store.** The user's real memory lives at `~/Library/Application Support/bio.199-biotechnologies.engram/`. Every test and every manual binary run MUST set `ENGRAM_STORE` to a TempDir (the override is landed in `paths.rs` and roots db/vectors/config/cache). Legacy tests isolate via HOME+XDG (`point_at_home`); do not write new ones that way. A test that opens the real store is an automatic reject — this exact bug once migrated the live store and bricked the installed binary.
2. **Do not install the binary** (`cargo install`, brew) and do not run migrations against any store outside a temp dir.
3. **Backward compatibility:** the existing CLI surface keeps working. Bare UUIDs stay accepted everywhere a handle is accepted. Existing JSON fields keep their names and types; new fields are additive. Exit-code taxonomy (0/1/2/3/4) is preserved and corrected, never reinterpreted.
4. **Migration v9→v10** must: back up FIRST via `VACUUM INTO 'db.sqlite.pre-v10-backup.<YYYYMMDDHHMMSS>'` outside the migration transaction (there is NO existing backup mechanism in the tree — the v8 "precedent" was a manual file, not code), synthesize genesis events for existing rows, be idempotent, and leave `bench personal` results unchanged. Binding detail: interfaces.md §2.1.
5. **Quality gates:** `cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`, `cargo test --workspace` green in your worktree before you commit. `engram bench personal --json` (offline, deterministic, never touches the live store) must not regress.
6. **Style:** match the repo's voice — narrative WHY-comments (read edit.rs or fact_write.rs headers for the register), no restating-the-code comments. Rust 2021, workspace deps.
7. **Commit discipline:** commit in your worktree with messages that say why (bug class, user impact), one commit per coherent change. Never push. Never touch files owned by another lane (ownership table below).
8. Cite this brief's defect anchors in commit messages where relevant so the fix→finding mapping survives.

## Lane ownership (exclusive; do not edit outside your lane)

| Lane | Owns |
|---|---|
| storage-journal | crates/engram-storage/src/** (journal.rs NEW, sqlite.rs, paths.rs, migration), crates/engram-core/src/** (event/interval/handle types) |
| fact-semantics | crates/engram-graph/src/**, crates/engram-cli/src/fact_write.rs, fact_llm.rs |
| write-surfaces | crates/engram-cli/src/commands/{remember,edit,forget,memory,import,export}.rs |
| read-surfaces | crates/engram-cli/src/commands/{recall,research,facts,people,entities,graph,wiki,documents}.rs, crates/engram-cli/src/retrieval.rs |
| contract-codegen | crates/engram-contract NEW (schema defs + generators), crates/engram-cli/src/{agent_info,cli,error,output}.rs, assets/skill.md, tests/contract_*.rs |
| daemon-jobs | crates/engram-cli/src/commands/{serve,jobs,compile}.rs, crates/engram-cli/src/{lib,context,main}.rs, tests/daemon_*.rs |
| reasoning-layer | crates/engram-cli/src/commands/state.rs NEW, reflection job spec, tests/state_*.rs |

Cross-lane needs go through `docs/arch/interfaces.md` (the shared contract — trait signatures, event schema, handle grammar, error codes, envelope fields; now v2 with lead rulings R1–R15 and ownership corrections in its §7, which override this table where they conflict). If the interface is missing something you need, note it in your spec/report; do not invent a divergent one. One-line changes you need in another lane's file go in a `cross_lane_patches` section of your report — the lead applies them at wave integration; never edit outside your lane.

Already landed on arch/v0.4 (do not re-spec): `Handle`/`EventKind` in engram-core; `ENGRAM_STORE` + peek getters in paths.rs; agent-info answering before any store opens; dispatch-level `elapsed_ms`; de-flaked bench tests.

## Sequencing

Wave 2 builds storage-journal foundations + contract-codegen skeleton first (everything else compiles against interfaces.md stubs). Wave 3 builds fact-semantics, read-surfaces, write-surfaces. Wave 4 daemon-jobs. Wave 5 reasoning-layer. Waves overlap where files don't.

## Verification culture

Field benchmark numbers are mostly unreproducible marketing (LoCoMo's answer key is 6.4% wrong); engram's deterministic offline bench is a differentiator. Every wave ends with the full gate suite. The final gate is an external deep review; expect your work to be adversarially re-read.
