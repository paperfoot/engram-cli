# Adversarial review — spec-reasoning-layer.md (Grok 4.5 high, 2026-08-01)

Raw output, unedited. Reviewed against interfaces.md v2 (R1-R15), redesign-brief.md,
coherence-report.md, spec-contract-codegen.md, spec-daemon-jobs.md, and the source tree at 0.3.1.

---

**Verdict:** The lane correctly owns CD-4/R15 patching and CD-6 isolation, and the open-interval read story is right. It is not implementable as written without re-deriving several APIs, inventing a handle kind interfaces.md forbids, and resolving a clap contract that already contradicts contract-codegen’s pre-agreed `State` shape.

---

### Hard-constraint / binding-contract violations

1. **`lesson:` handle invents a kind outside the closed grammar (interfaces.md §1, landed `handle.rs`).** Test 16 and §6.6 require a `lesson:` handle. `HandleKind` is only `mem|chunk|fact|conflict|entity|doc|job|event`. `Handle::parse("lesson:…")` is `UnknownKind`. Unlike X-1 (absence), this is not filed as an interfaces addition — it invents a divergent shape.

2. **§4.2 downgrade claim is self-contradictory and wrong against current migrate().** It says a v11 store “opened by a v10 binary works”, then that the v10 binary “will refuse on the version check”. Code at `sqlite.rs:878-882` refuses `version > CURRENT_VERSION`. Constraint 3’s “keeps working” does not cover opening a newer schema; the first sentence must be deleted or the claim narrowed to “v11 writes do not corrupt v10-readable tables.”

3. **`remember::run` as the memory-twin path breaks the JSON envelope contract.** `remember.rs:171` always `print_success`s. Calling it from `learn`/`confirm` emits a remember envelope (or two) on stdout before the lesson response — agents that parse one envelope get lying output (constraint 3 / move 5). There is no non-printing remember helper today; the spec must request one from write-surfaces or write the twin via storage/`Core::write` itself.

4. **`RememberArgs` literal contradicts current code (`remember.rs:40-59`).** Spec uses `diary: Some(…)`, `kb: Some(…)`, `autonomy: None`. Actual fields are `diary: String`, `kb: String`, `autonomy: String`. Also references bare `offline` with no `LearnArgs.offline` / CLI flag — won’t compile without re-derivation.

5. **Journal invariant 1 (interfaces.md §2) is unspecified for learn/confirm/reflect writes.** Projection mutate + event append must be one transaction. Spec sequences `upsert_lesson` → `remember::run` (own tx + prints) → `lesson.recorded` with no atomicity, no failure story if step 2 succeeds and step 3 fails (orphan twin / lesson without event / rebuild hole).

---

### Interface assumptions vs code / v2 rulings / sibling specs

6. **Clap surface fights contract-codegen’s pre-agreed shape.** `spec-contract-codegen.md` §4.7 freezes  
   `State { subject, kb, diary, relation, include_closed, as_of }` — no lessons/learn/reflect, no `--limit`/`--cursor`. This spec’s C-1/C-2 replace that with a subcommand tree. CD-4 was supposed to be pre-agreed; an implementer cannot apply both.

7. **C-1 and C-2 contradict each other.** C-1 adds `State(StateCommand)`; C-2 needs `Command::State { subject: Option<String>, sub: Option<StateCommand> }` for bare `state ada`. One of them is wrong.

8. **Subject resolution under default `--kb '*'` / `--diary '*'` is undefined against real APIs.** `canonicalize_subject` (`fact_write.rs:515-556`) takes `&str` diary/kb and calls `distinct_fact_subjects(diary, kb)` / `resolve_entity_alias(kb, …)` with equality filters (`sqlite.rs:4186-4213`). Passing literal `"*"` searches a KB named `*`. Spec never says how `resolve_subject` expands `*` (loop? new storage API?). Default invocation is the broken path.

9. **Provenance cites the wrong storage API.** §3.1 points at `list_chunks_by_memory` (`sqlite.rs:1805`) for a fact’s chunk/snippet. That method lists **every** live chunk in a KB (or all KBs), not chunks for one `memory_id`. No `chunks_for_memory(id)` exists. Per-value provenance either O(n) scans the KB or needs an S-* request the spec doesn’t make.

10. **`Interval::contains` does not exist** (`interval.rs`). §3.2 step 4 says keep facts whose `Interval::of(f)` “contains” `T`. Implementer must invent the predicate. The written rule (`until > T` half-open) makes **point/Instant** intervals (`until == Some(from)`) never contain their own instant — birthday/`--as-of` is silently empty (test 13 only covers medications).

11. **Reflect job API mismatches daemon-jobs.** Spec: `Core::submit_job("reflect", params)` and a free `reflect_job(ctx, …)`. Daemon-jobs: `JobRunner` trait (`kind`/`validate`/`run` + checkpoints), `registry.register(ReflectRunner)`, `submit_job(&NewJob)`. D-3 is not the one-liner daemon-jobs §4.3 describes.

12. **Daemon scheduler will not run `reflect` even after registration.** `spec-daemon-jobs.md` §4.6: `next_runnable_job(&["compile","backfill"])`. Spec never patches that allowlist. Test 32 / default non-`--inline` path queues work nothing claims (or only the no-daemon inline claim path works — unspecified).

13. **Lesson `EventKind`s are not in interfaces.md §2.** S-2 patches `events.rs` only. Payload schemas and rebuild obligations live in the binding contract; unknown kinds are not listed. Export/import/rebuild consumers have no wire contract.

14. **Schema v11 is outside the migration contract.** interfaces.md §2.1 binds only v9→v10. v10→v11 + `lessons` DDL is a lead/interfaces change, not only an S-1 sqlite patch. Storage-journal’s own spec stops at `CURRENT_VERSION → 10`.

15. **S-5 fights storage-journal’s frozen signatures.** Both storage-journal and fact-semantics pin `open_intervals(kb: &str, diary: &str, …)`. S-5 asks `Option<&str>`. Risk 2’s N×M fallback is real; the “one-line change” understates a cross-spec conflict.

16. **`contested` type lies.** §2.3: `contested: f32`. §7.8: omit as `contested: null` when the conflict cap is exceeded. JSON null vs f32 / “we did not look” vs 0.0 is exactly the count-meaning-page class the lane claims to avoid — and is not in the payload schema.

17. **`arity_class` / `safety` flattening is unspecified.** `RelationSpec` is `{ arity: Arity::SetValued { safety }, mutability, … }` (`relations.rs:65-71`). Payload wants separate `arity_class` + `safety`. No `to_json` mapping; same work read-surfaces does elsewhere, not written here.

---

### Migration / surface holes

18. **`confirm` / `retire` missing from the public CLI in §2.1** but present in §3.1, tests 23–24, and D-2. Agent-info draft (§6.6) omits them. Collision risk §7.1 says “four reserved words”; with confirm/retire it is six.

19. **`--since <rfc3339>` vs `ReflectParams.since_seq: Option<i64>`.** No mapping from timestamp → `events.seq`. High-water resume vs `--since` override is hand-waved (“`--since` overrides”).

20. **`--inline` high-water has no durable home.** High-water is stored in the job’s `metadata_json`. `--inline` bypasses the job row; second `--inline` resume (tests 27–28) has nowhere specified to read/write the mark (config? side table? only job metadata?).

21. **`upsert_lesson` before KB ensure → FK failure.** DDL: `FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE`. Order in §3.1: upsert lesson, then `remember` (which `ensure_kb`s via `insert_memory_with_kb`). First `learn` into a new KB fails the FK unless `ensure_kb` is called first — unstated.

22. **§4.4 degradation is incompatible with the test plan.** Memory-only fallback drops `status`/`occurrences`/dedup, but tests 16–24 assert `proposed`/`confirmed`/`retired`, occurrences, and `lesson.*` events. “Degrade or block” is not wired to which tests `#[ignore]`.

23. **Proposer window-boundary misses.** `correction_within_window` / `retraction_after_extraction` need `memory.written` / `derivation.ran` that may sit **below** the high-water mark while the revise/retract is inside the window. Pure `fn(&[EventRow])` over the window silently drops those lessons. No “look back by subject_id” rule.

24. **KB CASCADE deletes lessons without a retraction event.** Move 1: nothing hard-deleted from the journal; projections rebuild from events. Cascade wipe of `lessons` on `kb` delete (same pattern as other projections) leaves `lesson.*` events pointing at a projection that vanished mid-flight with no `lesson.retracted` — rebuild can restore, but live reads after delete are empty with no certificate. Not called out.

---

### Missing failure modes / implementer must re-derive

25. **Fuzzy subject swap has no product rule.** `resolved_from: Fuzzy` still yields `verdict: "results"` about another person. Test 12 only covers total miss. Agents that don’t read `resolved_from` will answer about Adam when asked Ada — the absence path’s near-miss data is only on `no_match`.

26. **`distinct_fact_subjects` still uses `superseded_by IS NULL`** (`sqlite.rs:4212`) — the exact risk-5 predicate. Nearest-subject / fuzzy resolution can miss subjects who only have closed intervals, or treat stopped-med rows as “active subjects.” Spec reuses it “verbatim” without noting the bias.

27. **History payload fields `close_reason` / `valid_until` assume Fact columns that are additive in v10** (interfaces §3.3) — fine if wave 2 landed; current `Fact` in tree lacks them. Spec should pin “requires fact projection columns from storage-journal/fact-semantics” as a hard gate, not imply they exist on main@0.3.1 anchors.

28. **`metadata.scope` (interfaces §4) never wired.** `StateView` has `scanned_kbs`; envelope needs `metadata.scope { kbs, diary, chunks_considered }`. `print_success` / respond path unstated — implementer invents it or ships non-compliant reads.

29. **Exit 2 on reflect without key (§4.3) vs offline proposers (§2.5).** Pipeline can produce lessons with no model; exit 2 when “asked for an LLM pass” is right only if “LLM pass” is precisely defined when candidates exist vs when the model is only a rewriter. Easy to implement as “no key ⇒ exit 2” and break test 25’s spirit if someone runs without `--offline` and expects deterministic proposers-only.

30. **Anchors drift / incomplete patches.** Line refs for `cli.rs` / `lib.rs` / `JobsCommand` are “main@0.3.1”; contract-codegen will rewrite `cli.rs`. D-4 HTTP parity deferred is fine; missing daemon allowlist patch (finding 12) is not. S-4 rebuild replay is named but has no event→row algorithm (status transitions, evidence union, memory_id).

---

### What the spec gets right (for balance)

- Correct refusal to re-derive interval/registry rules; consumes `engram_core::{relations,interval}` per R1/CM-3.
- Open-interval-only current state + test 1 as the risk-5 regression is the right headline.
- CD-4/R15 ownership of `mod.rs` / clap / dispatch via `cross_lane_patches` matches the coherence report.
- Test isolation via `ENGRAM_STORE` + ban on `point_at_home` matches R14/CD-6 and constraint 1.
- X-1 (`nearest_subjects`) is properly filed as an additive interface request; calibrated-confidence honesty in risk 5 is good.

---

### Highest-priority fixes before implementation

1. File or drop `lesson:` (handle kind + `HandleParseError` string) in interfaces.md.  
2. Align clap with contract-codegen (or explicitly supersede §4.7 via lead ruling).  
3. Replace `remember::run` with a non-printing write path; one transaction with `lesson.*` events.  
4. Specify `*` scoping for subject resolution (and likely storage helpers).  
5. Specify ReflectRunner + scheduler kind allowlist + inline high-water storage.  
6. Fix provenance API citation; add `contains` (incl. Instant) or ban `--as-of` on Instant until defined.
