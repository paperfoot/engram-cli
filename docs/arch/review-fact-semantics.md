# Adversarial review — spec-fact-semantics.md (Grok 4.5, raw output)

Reviewer: `cursor-agent --model cursor-grok-4.5-high`, 2026-08-01.
Prompt: review `docs/arch/spec-fact-semantics.md` against `docs/arch/redesign-brief.md`,
`docs/arch/interfaces.md`, and the v0.3.1 source. Raw, unedited.

---

Adversarial review of `docs/arch/spec-fact-semantics.md` against `docs/arch/redesign-brief.md`, `docs/arch/interfaces.md`, and the current tree. Verdict: the core arity/interval/replay design matches the brief, but several claims contradict the code, the test plan fights existing regressions, and migration/`is_open` semantics have holes an implementer cannot execute as written.

---

### Findings

1. **Regression gate is self-contradictory (will fail `cargo test`).** §5.F says `wave05_compiled_facts.rs` and `cli.rs:1850/:4474` must stay green unchanged. Those tests encode the *old* safety model: two different allergies → one queued `safety_slot` conflict (`cli.rs` `safety_slot_queues_conflict_and_recall_reports_contested`, `wave05_compiled_facts.rs` `a_compiled_fact_never_retires_a_safety_slot` expecting `llm_facts_queued == 1` and rule `safety_slot`). Under this spec they must become coexist / queue-nothing. The gate and the redesign cannot both be true.

2. **`stopped` / non-supersede closes break "active" readers — test 34 assumes a fix this lane does not own.** §6.2 / risk 5: `close_reason=stopped` sets `valid_until` and leaves `superseded_by` NULL. Today every "active" query is `superseded_by IS NULL` (`sqlite.rs` `get_active_facts_in_kb` ~4015–4016, `list_facts*` ~4563+, `active_fact_embeddings` ~4159, `facts.rs` list/show ~156–157). Test 34 expects `facts list` (active only) to hide a stopped med — that requires read-surfaces/storage changes outside this lane's exclusive files. As written, stopped meds stay "active" and stay in the novelty gate.

3. **Open-queue unique index will fail or need an unstated collapse algorithm.** §2.6 creates `UNIQUE … (kb, decision_key) WHERE status='open' AND decision_key IS NOT NULL`, then §4.1 step 5 backfills keys on all 951 open rows. The polypharmacy storm *is* duplicate semantic keys among open rows. SQLite will reject the second `UPDATE` to the same key. Spec never specifies: which row survives, how others are closed, or that the unique index must be created *after* collapse.

4. **Replay integration tests depend on storage-journal + write-surfaces, not this lane.** Tests 38–39 assume a `keep-both` / `supersede-old` decision survives `edit` + re-derivation. Today `edit` → `delete_facts_for_memory` (`sqlite.rs:1925`) and `fact_conflict_queue` has `ON DELETE CASCADE` (`sqlite.rs:1107–1108`) — the brief's exact defect. Without journalled decisions and non-cascading identity, replay has nothing to replay. Spec owns only graph/`fact_write`/`fact_llm`.

5. **`correction_by_user` is dead on every real write path.** §2.3.1 step 7 requires `SourceKind::UserStated|UserConfirmed`. `remember.rs:632` always sets `SourceKind::AgentExtracted` (including `--facts-json`). Only compile sets `CompileLlm`. No §6 request to write-surfaces to set user source kinds — birthday "user correction" never fires in production.

6. **`blood_type` user-correction is also killed by the safety gate.** `blood_type` is `SingleValued{safety:true}` + `Immutable`. Step 7 emits `SupersedeOld("correction_by_user")`; step 9 then downgrades any safety supersession to `Queue("safety_slot")`. Spec never says the correction path is non-safety-only.

7. **`Arity` re-export claim is false against current imports.** Spec re-exports `engram_core::Arity` as `{SetValued,SingleValued}` and claims `commands/facts.rs:20–24` compiles untouched. That file imports `fact_write::Arity` and matches `Arity::Single`/`Multi` (`facts.rs:437–438`). Renaming/replacing without a dual-type shim (or editing read-surfaces) does not compile. `is_safety_slot` is also imported there and is not given a stable shim in §2.3.

8. **Invented schema/events not listed as §6 divergences.** Spec promises "every divergence request is listed in §6." Missing from §6: `close_authority` column; `fact_decisions` projection table; `authority` on `decision.recorded` (interfaces §2 payload has no authority); `conflict.opened` for *open* queue rows (interfaces migration only mentions resolved rows); `decision.replayed` append on apply (interfaces §3 requires it; `apply_plan` never mentions it).

9. **`Observation` / `PlannedFact` / `PlanNote` / `Void` are undefined.** `plan_write` takes `&Observation` and returns those types; `DecisionVerdict::Void` is never given semantics. Implementers must invent the observation→`Ended` mapping, note envelope shape, and when Void applies.

10. **`Autonomy::Strict` and `ENGRAM_FACT_QUEUE_ALL` vanish from the planner.** Today `apply_write_verdict` escalates supersedes under `Autonomy::Strict` and forces Queue when `queue_everything()` (`fact_write.rs:1124–1133`). `cli.rs` `strict_autonomy_queues_even_plain_slot_updates` depends on this. `plan_write` / `apply_plan` omit both — silent behavior change vs brief constraint 3 (CLI surface keeps working).

11. **`disjoint_intervals` drain estimate is overstated for the real backlog.** Two open `lives_in` rows with `valid_until=NULL` both extend to ∞, so they *overlap* under §2.2 half-open rules. Phase 2 lists `disjoint_intervals` as a dominant 951 drain class; for contested open–open pairs the classifier still takes recency/LWW/queue paths. The London–Lisbon coexist story only holds when an end is already known.

12. **Point intervals vs half-open algebra are inconsistent.** `Temporality::Instant` is `[t,t]`; `overlaps` is half-open `[from,until)`. `[t,t)` is empty. Spec says point intervals never overlap but does not define `overlaps`/`is_point` for `until == from` vs `until == Some(from)`. Implementer must re-derive.

13. **`suggested_action` never `supersede-old` is broader than the brief/interfaces safety rule.** Interfaces §3 only forbids suggesting `supersede-old` for safety member-attribute conflicts. Spec removes it for *all* pairs (§2.3.1). Agents that auto-run suggestions on ordinary slot queues change behavior; only partially justified by constraint 3's "names and types" wording.

14. **Cessation with no open interval invents a fact.** §2.4 step 7 inserts a pre-closed interval when nothing is open. That writes a historical medication/allergy from a lone "stopped X" utterance with no prior observation — failure mode noted only as a `PlanNote`, not as a product decision.

15. **Same-object closed interval with `new.from < old.until` falls through unclassified.** Step 4 handles open duplicate and reinstatement (`from >= until`). Overlapping a closed interval (backdated assertion inside a stopped window) has no dedicated rule — falls into set/single branches and may queue or close incorrectly.

16. **`ProfileSlot` claim is mostly right but undersells JSON/compat impact.** Little code *branches* on `FactType::ProfileSlot`, but new safety facts stop being `profile_slot` (§4.2). Any external consumer of `fact_type` for allergies/meds changes; inside-repo assignment is only `fact_write.rs:885–890`. Not a logic bug, but "untouched" oversells.

17. **Lane ownership tension on storage APIs.** Exclusive ownership: storage-journal owns `engram-storage/**`. Spec §6.2 writes full `SqliteStore` method bodies/signatures and §2.6 DDL as if this lane specifies them — correct as interface requests, but `DecisionLedger` *impl for `SqliteStore` in engram-graph* plus widened `insert_fact_conflict` are easy to "just implement" in the wrong crate. No compile-order stub story if wave 2 has not landed methods yet.

18. **Phase 0 "`ENGRAM_STORE` (or `HOME`)" is a live-store footgun.** Brief hard constraint 1 + `paths.rs`: without `ENGRAM_STORE`, macOS resolves via `ProjectDirs` under `HOME` → `~/Library/Application Support/bio.199-biotechnologies.engram/`. Copying `db.sqlite` into a bare temp and setting only `HOME` does not place the DB where the binary looks unless the full App Support path is recreated. interfaces.md already says never rely on HOME tricks — Phase 0's "or HOME" weakens hard constraint 1.

19. **Backup name disagrees with interfaces.** Spec: `db.sqlite.pre-v10-backup-<rfc3339>`. interfaces.md §2: `db.sqlite.pre-v10-backup`. Brief only requires "timestamped" + v8 precedent. Small, but migration code will diverge across lanes.

20. **`engram admin rebuild` cited for rollback without existing command.** §4.1 rollback / interfaces invariant 3. No `admin rebuild` in the current CLI surface — fine as target architecture, but this lane's rollback story is not operable until storage-journal ships it.

21. **Duplicate path semantics shift without an apply rule.** Today `ConflictClass::Duplicate` via `apply_write_verdict` calls `supersede_fact_with` on the new row (`fact_write.rs:1137–1146`). Spec's planner uses `confirm: Vec<Uuid>` and redefines supersession as interval close — never states that Duplicate must *not* set `superseded_by`. Easy to preserve the old applier and corrupt the interval model.

22. **Member-identity over-merge auto-close on non-safety sets is acknowledged but load-bearing.** Risk 1 chooses auto-close for non-safety member-attribute changes. Combined with a heuristic that strips trailing tokens, `has_role: "engineer"` vs `"senior engineer"` may not share a member under v1 rules (nothing trailing is noise), while true dose-like attributes on non-safety sets can auto-close. Spec leaves the unsafe direction open without a default-queue escape hatch in the decision procedure.

---

### What looks solid

- Root-cause anchors (`fact_write.rs:136–142`, `:245` vs `:267`, `:1209–1213`) match the code.
- Safety-as-set + never suggest `supersede-old` on safety matches brief move 2 and interfaces §3.
- Moving `already_settled` before the safety early-return is a real 951-drain fix.
- Machine `CloseOld` non-replay correctly avoids re-freezing triage's ~14.5k supersessions.
- Test isolation requiring `ENGRAM_STORE` + TempDir is *stricter* than today's HOME-only helpers and aligns with interfaces §5 / hard constraint 1.
- Not opening the live store for measurement is correctly treated as non-negotiable.

---

### Bottom line

Implementable as a *design* for move 2, but not as a self-contained lane ticket: the unique-key migration step is underspecified, open-interval/`superseded_by` compatibility is a cross-lane blocker the tests assume is already fixed, replay tests cannot pass on fact-semantics alone, and the "regressions must stay green" list locks in the bug this wave is supposed to kill. An implementer would have to re-derive Observation/apply_plan/Strict/queue-all, invent open-queue collapse, and negotiate read-surfaces + storage-journal before the integration tests in §5.E are honest.

---

## Reviewer-of-the-review notes (Claude, verification pass)

Findings independently confirmed against the tree beyond Grok's list:

- **Triage's `Duplicate` arm is a supersession.** `commands/facts.rs:635-643` resolves `Duplicate`
  by `supersede_fact_with(new_f.id, old_f.id, …)`. The spec's §6.4.1 replaces the inline
  defence-in-depth at `facts.rs:581-593` (which today downgrades `Duplicate` on a safety pair)
  with `safety_gate`, which per §2.3.1 step 9 passes `Duplicate` through untouched. Net effect
  would be a NEW heuristic close on a safety relation — the exact class the lane exists to
  prevent. Related to Grok #21, but the triage path, not the write path.
- **`is_safety_slot` under the deprecated `slot_policy` shim returns `false` for medications.**
  `is_safety_slot` is `matches!(slot_policy(c), Some(AwaitConfirmation))` (`fact_write.rs:145-147`);
  §2.5's shim maps set-valued relations to `None`, so `takes_medication`/`allergic_to`/
  `diagnosed_with` stop being safety everywhere the shim is consulted, including the
  `"safety_slot"` JSON field at `facts.rs:440`.
- **The shim also widens `slot_policy`.** `birthday`/`died_on`/`nationality` are `SINGLE_VALUED_EXTRA`
  today with `slot_policy == None`; under §2.5 they become `SingleValued{false}` → `LastWriterWins`,
  which flips `write_supersede_rule` from `exact_match` to `slot_lww` and changes
  `facts.resolution` / `resolved_by_rule` histogram keys.
- **Migration step 2 can synthesize a negative-length interval.**
  `valid_until = COALESCE(valid_until, superseded_at, created_at)` uses write time as an event-time
  end; a fact whose `valid_from` is in the future relative to `created_at` gets
  `valid_until < valid_from`. §2.2's `close_at` clamp exists for exactly this and the migration
  bypasses it.
- **`lookup_decision` is declared twice with incompatible signatures.** §2.3.2's
  `DecisionLedger::lookup_decision(&self, kb, &SemanticKey) -> Option<DecisionRecord>` and §6.2's
  inherent `SqliteStore::lookup_decision(&self, kb, key_encoded: &str) -> Option<DecisionRow>`.
  The inherent method shadows the trait method at every call site.
- **No precedence rule between multiple decisions on one key.** `lookup_decision` returns one
  record; if the newest is a machine `close_old` (non-binding) and an older one is a caller
  `keep_both` (binding), "newest wins" silently discards the binding caller decision.
- **`INSERT OR IGNORE` + the new partial unique index loses a queue insert silently.**
  `sqlite.rs:4240` is `INSERT OR IGNORE`; a second distinct pair sharing a semantic key returns
  `Ok(false)` and `queue_conflict` reports nothing — an exit-0 success with the question dropped.
