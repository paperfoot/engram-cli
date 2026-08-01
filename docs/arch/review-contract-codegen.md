# review-contract-codegen.md — adversarial review of spec-contract-codegen.md

Reviewer: Grok 4.5 (cursor-agent, `cursor-grok-4.5-high`), 2026-08-01.
Inputs: `docs/arch/spec-contract-codegen.md` against `redesign-brief.md`,
`interfaces.md` (v2, rulings R1–R15 binding), `coherence-report.md`, and the
source tree. Raw output, unedited.

---

**Verdict:** The lane is well-aimed at move 5 / R7–R15, but it is not safely implementable as written. Several wave-2 tests assert other lanes' unfinished behaviour, `CliError::code()` contradicts itself, and at least one "pinned default" freezes a doc bug that the CLI already fixed.

---

### Hard-constraint / binding-ruling violations

**1. Wave-2 gate tests require wave-3 behaviour (constraint 5 / CC-4)**
`unknown_id_exits_3_on_every_mutator` (§6.4) expects `forget`/`edit`/… to exit 3 with `code == "not_found"`. Today `forget.rs:17-21` exits 0 with `deleted: false`; `edit.rs:61-62` exits 3 as `bad_input`, not `not_found`. The write-surfaces request at §7 is correct; the test is not `#[ignore]`d. Same class: `listing_carries_returned_total_count` (§6.5) needs read-surfaces to emit IN-23 fields; only two tests are listed as ignorable (§6.10).

**2. Documenting forget/`not_found` in wave-2 agent-info before the binary changes (constraint 3)**
§3.2 / `:202-206` adds the rule "unknown id is `not_found`, exit 3". Until write-surfaces lands, generated agent-info/skill **lie** about live behaviour. Constraint 3 keeps the CLI surface working; this invents a new documented contract ahead of the implementation.

**3. `schema_version = 10` vs live store still at 9 (constraint 4 sequencing)**
§2.0 / §6.5 pin `contract().schema_version = 10` and assert equality with `doctor --json`. `sqlite.rs` still has `CURRENT_VERSION: i32 = 9`; skill.md still says "Schema is v9". If contract-codegen merges before storage-journal's v10 bump, `contract_schema_version` fails the workspace gate. No ignore / wave-gate ordering.

**4. Brief move 5 says "generates clap defs"; this spec refuses (§1.2)**
Hard constraint 6/7 don't ban the narrowing, but move 5's deliverable list is explicit. An implementer following the brief will fight the non-goal. Needs an explicit lead ruling that clap stays hand-written.

---

### Internal contradictions / interface mismatches

**5. `CliError::code()` cannot both "DERIVE from `ErrorCode`" and preserve the §5.2 legacy table**
§2.3: `exit_code()`/`code()` derive from `error_code()`. §5.2 freezes `embed_error`, `rerank_error`, `ingest_error`, `bench_error`, `io_error`, `serde_error`, `zip_error` — none of which exist on `ErrorCode` (interfaces §4 / R11). Implementer must invent an undocumented dual map (`ErrorCode` for exits vs legacy strings for `error.code`).

**6. No `Embed`/`Rerank`/`Ingest`/`Bench`/`Io` → `ErrorCode` table**
Only `StorageError → ErrorCode` is specified (R11). Today `error.rs:54-63` special-cases `EmbedError::MissingKey`→2 and `RateLimited`→4. That logic disappears unless re-derived. Missing failure mode for the highest-traffic config path ("no Gemini key").

**7. `ErrorCode::as_str` vs `#[serde(rename_all = "snake_case")]`**
Landed `engram-core/src/error.rs:22,58-59`: serde emits `"config"` / `"transient"`; §2.2 patch 2a only changes `as_str()`. Any path that serializes the enum (or copies `as_str` vs serde inconsistently) re-breaks constraint 3's wire values. Patch must also fix serde renames (e.g. `#[serde(rename = "config_error")]`).

**8. Patch 2c is already done; §3.7 still says "Append `ErrorCode`"**
`engram-core/src/lib.rs:17` already re-exports `ErrorCode` (IN-5 landed). §2.2 says do not re-declare; §3.7 says append ~70 lines. Ownership fight with storage-journal (brief: they own `engram-core/**`; R11: contract-codegen implements) is acknowledged in §8.2 Q1 but the file-by-file plan still reads as "you write the enum."

**9. `emit_parse_failure` and `skill_paths()` are used but never specified**
Patch 6 calls `engram_cli::cli::emit_parse_failure`; §2.5 only defines `parse_or_exit` / `parse_from` / `ParseOutcome`. Patch 11 calls `engram_cli::skill_paths()` / `installed_skill_paths()`; `commands/skill.rs:23-37` keeps `target_dirs()` private, and skill.rs is frozen (interfaces §7). Implementer must re-derive a public path helper and a parse-failure emitter — likely another frozen-file or `lib.rs` patch.

**10. Missing cross-lane patch: `pub mod respond` in `lib.rs`**
`respond.rs` is owned here; `lib.rs` is daemon-jobs (brief). Without a §7 patch, the new module does not compile into the crate. Patches 4–5 touch `main`/`lib` for AgentInfo but never add the module decl.

**11. `resolve_command` default `keep-both` vs live `suggested_action: "supersede-old"`**
§2.4 puts `keep-both` first "because safe." Today `fact_write.rs:1209-1213` still suggests `supersede-old` for `detector == "slot"`. After patches 7–10 (or any early `attach` call) an agent can see `suggested_action: "supersede-old"` beside `resolve_command: … --action keep-both`. R7 does not pick the legacy string's action; this contradiction is unstated as a transition hazard.

---

### Wrong claims about current code (implementer will encode bugs)

**12. Pinned default `people show.kb=default` is false**
§6.3 pins it. `cli.rs:783-785` defaults to `"*"`. `agent_info.rs:154,316` still document `default` (stale); skill.md:106 already says every KB. `manifest_is_0_3_1_superset` + transcription-from-agent_info will **lock the wrong value** into the generated contract and fail (or worse, "pass" by documenting a lie) against `every_documented_default`.

**13. Line-number / command confusion on `research.top_k`**
§3.3 cites `:457` for Research `--top-k` alias; `:456-457` is a **Bench** suite flag (`default_value_t = 0`). Real Research `top_k` is `:269-270` (default 12). Alias edits will land on the wrong command if followed literally.

**14. `serve.rs:577-610` citation is the response builder, but it has no `conflicts` key to replace**
Correct that `/v1/recall` omits conflicts; patch 10 must *insert* an array, not rewrite an existing one. Easy to under-implement.

**15. `ENGRAM_STORE` is absent from today's agent-info**
§3.2 / §6.1 correctly require documenting it (R14/CM-13). Confirmed: no `ENGRAM_STORE`/`ENGRAM_HOME` in `agent_info.rs` env_overrides. Fine as work — but the golden 0.3.1 fixture must not be treated as already containing it.

---

### Migration / compatibility holes

**16. `storage_error` removal is a constraint-3 value change, half-owned**
§5.2 / §8.2 Q2: stop emitting `storage_error`. That is an existing JSON **value** agents may match. Deprecation in the manifest does not preserve the wire. Lead must rule before wave 2 (spec admits this; still a hole).

**17. Clap exit 2→3 and NotFound 1→3 are justified (R11) but agent migration is incomplete**
§5.1 CHANGELOG is right. Missing: how HTTP/`engramd` surfaces the same taxonomy (`engramd.rs:29` uses a separate `Args` parser; patch 6 is sketch-only). Daemon and CLI can drift on the same typo.

**18. Skill staleness vs locally modified skills**
`skill install` refuses overwrite of hand-edited SKILL.md (`skill.rs`). Doctor stale/missing logic (§4.3) never defines status for "present but locally modified / hash unreadable because front-matter stripped." Failure mode agents hit after first install+edit.

**19. CD-10 / Fact fields: registry ownership is "tell this lane" with no channel**
§7 asks storage-journal to notify for `FieldSpec`s. No artefact (shared issue file, interfaces bump) — same class of gap CC-5/IN-30 was meant to close beyond one-line patches.

---

### Missing failure modes / underspecified behaviour

**20. `StorageError::Serde` → `MalformedJson` + suggestion "check the JSON you passed"**
R11 maps DB/row serde failures to `MalformedJson` (exit 3). Suggestion table (§4.2) blames caller input. Corrupt projection rows become "fix your args," and agents will not run `doctor`. Need a distinct message when the malformed JSON was not caller-supplied.

**21. `ParseOutcome` / TTY prose path underspecified**
§2.5: JSON only when stderr is not a terminal (matches today). §6.4 `clap_error_on_tty_is_prose` mentions an "`IsTerminal` shim" with no API. CI often has non-TTY stderr — test may be unrunnable as written.

**22. `no_results` status vs new `verdict: no_match` (IN-23)**
Recall already uses envelope `status: "no_results"` (`recall.rs:249-256`). Spec adds `data.verdict` / `absence` without saying whether `no_results` remains, becomes redundant, or both fire. Contract vocabulary pins both `status` and `verdict` — dual absence signals without a rule.

**23. `resolve_command_is_runnable` (§6.7) needs a real open conflict under `--offline`**
Creating a contested pair without OpenRouter/`--facts-json` is non-obvious; steps are not specified. Easy flake or paid-call leak if keys aren't cleared (Sandbox clears keys — good — but then how is the conflict created?).

**24. Generator / `include_str!` workflow gap**
Asset is checked in; `engram-contract-gen` regenerates; test asserts freshness. No CI step or `cargo xtask` is named beyond the failure message. Release packaging can ship a stale `assets/skill.md` if someone edits `registry.rs` and skips the gen binary.

---

### What the spec gets right (for calibration)

- R7 vocabulary (`supersede-old` kept for non-safety strict) matches interfaces; overrides fact-semantics §1.4.
- R12 `respond.rs` in wave 2, `allowed_actions` from caller — correct CM-9 fix.
- R14: `ENGRAM_STORE` only, no `ENGRAM_HOME`; `--store` deferred.
- R15 patch channel used extensively; live-store isolation story for new tests is solid.
- Pipe-free `resolve_command` + additive `resolve` object matches IN-24.
- Line citations for `fact_write.rs:1232`, `facts.rs:371`, `recall.rs:345` still match.

---

### Highest-priority fixes before implementation

1. Mark or gate every test that asserts another lane's unfinished surface (`forget`/`not_found`, IN-23 listings, schema v10, producer agreement).
2. Resolve the `code()` dual-map (ErrorCode vs legacy `*_error` strings) in the spec text, including Embed/Rerank.
3. Fix `people show` default and the Research `:457` citation against `cli.rs`.
4. Add patches for `pub mod respond`, `emit_parse_failure`, and a public skill-path helper without editing frozen `skill.rs` logic ad hoc.
5. Do not publish forget/`not_found` or listing `returned`/`total` as always-present until those lanes land — or document them as `always: false` until then.
