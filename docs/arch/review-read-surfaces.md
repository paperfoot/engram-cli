# Adversarial review — spec-read-surfaces.md (Grok 4.5 high)

Raw output, unedited.

---

## Verdict

The lane correctly owns the real read defects (chunk-id provenance, count-means-page, diary-leaking conflicts, `is_open_fact` sweep sites, pipe-laden `resolve_command`) and mostly follows R7/R12/R14/R15. It is **not implementable as written**: it **duplicates contract-codegen’s envelope types**, **invents storage APIs that `spec-storage-journal.md` never ships** (and diverges from the race API storage *does* ship), **claims `memory.rs` against the brief and write-surfaces**, and leaves several wire mappings / `absence` algorithms for the implementer to invent.

Line cites below are against the tree at `a239e0e` (spec’s `1d63d74` base is an ancestor; core `relations`/`interval`/`ErrorCode` have already landed).

---

### Critical — hard interface / peer-spec collisions

**1. Duplicate envelope builders vs contract-codegen (IN-23 / CM-9-class)**
`spec-contract-codegen.md` §2.7 / §7 explicitly requires read-surfaces to call `output::{Listing, Scope, Verdict, Absence}` — “so `contract_envelope` passes.”
This spec §2.1–§2.4 invents parallel `Page` / `ReadScope` / `ReadVerdict` / `Absence` in a new `read_surface.rs` and tells the lane not to edit `output.rs`.
Two homes for the same wire fields → contract tests fail whichever spelling loses, and IN-23’s “one meaning” goal repeats the count-drift failure mode.

**2. `respond::resolve_payload` does not exist (R12)**
§3.3.6 / CC-R1 call `respond::resolve_payload(program, args, allowed_actions)`.
Contract-codegen §2.4 ships `resolve_conflict` / `attach` / `resolve_json` / `resolve_command_string` — no `resolve_payload`.
Implementer must re-derive the call shape against a wave-2 module this lane is forbidden to edit.

**3. `CliError::ConflictAlreadyResolved` shape fights contract-codegen (R11 / CLP-7)**
This CLP-7: `ConflictAlreadyResolved(String)`.
Contract-codegen §2.3: `ConflictAlreadyResolved { conflict_id, verdict }`.
Same file, same patch channel, incompatible constructors. Call sites written here will not compile against the owner’s type.

**4. Invented `claim_fact_conflict` vs storage’s `resolve_conflict_with_decision` (brief move 4)**
Storage §2.11 already pins the race fix as one atomic API: claim + `decision.recorded` → `Result<Option<Uuid>, _>`.
This §3.4.8 / SR-5 invents `claim_fact_conflict` then separately `record_decision` + `supersede_fact_with`.
If both land: two claim paths. If only storage lands: this resolve plan is fiction. The race the brief names is closed only if supersession also sits in that same tx — this plan does not require that of storage’s method.

**5. SR-1…SR-9 are almost entirely absent from storage-journal’s committed surface**
Storage’s “new methods” list (`spec-storage-journal.md` ~§3.2) has `record_decision`, `close_fact_interval`, `resolve_conflict_with_decision`, etc. It does **not** commit to:
`memory_ids_for_chunks`, diary-scoped `facts_matching_terms` / `open_conflicts_matching_terms`, `count_open_conflicts_matching_terms`, scoped `list_recent_conflicts` + count, `count_facts_scoped`, entity/document/memory counts, or list `offset` params.
Deliverables 1, 2, and 5 **cannot compile** until storage accepts these as interface additions — they are not “already pinned in interfaces.md §2/§3” (§2.7). Interfaces never names them.

**6. `memory.rs` ownership collision (brief table / R15 / write-surfaces)**
Brief: write-surfaces owns `commands/memory.rs`. Write-surfaces §3.1 rewrites that file as the shared handle-resolution home and adds `handle` on `list`.
This lane claims `memory list` for paging (§1.3 / §3.9) and lists the file in its exclusive ownership block.
OQ-5 notes the gap but still schedules edits. Two exclusive owners of one 23-line file.

**7. `absence.threshold` contradicts interfaces + contract-codegen (IN-23)**
Interfaces §4 and contract-codegen §2.7: `threshold: f32` (required field on `Absence`).
This §2.3 / §2.3.1 / OQ-7: `Option<f32>`, permanently null, honesty rule against fabricating `0.0`.
Honesty is right against today’s `hybrid_recall` (`retrieval.rs:950-955` — empty pool, no cutoff). The **binding envelope type is still non-optional**. Contract tests that require a number will fail; fabricating `0.0` reintroduces the dishonest-health class. Lead ruling required before implement.

**8. `close-old` breaks the pinned `resolve_action` vocabulary**
§3.3.6 / §3.4.8 add `close-old` to `allowed_actions` and `--action`.
Contract-codegen vocabulary table: `resolve_action` = `keep-both | supersede-old | reject-new` only; `allowed_actions_are_from_the_vocabulary` will fail. Fact-semantics §6.4.5 marks close-old optional; this lane treats it as in-scope without a CC vocabulary patch.

---

### Hard-constraint / compatibility tension

**9. Ranked-read `total` redefines IN-23’s meaning**
Interfaces: `total` = matching rows in scope.
§2.8: on recall/research, `total = candidates_fused`, documented as “candidates considered, not matches available,” `cursor` always null.
Agents taught that `total` means backlog size will misread recall. Additive fields are fine; **overloading `total` is a value-semantics change** of the same field name constraint 3 protects. Prefer `candidates_fused` (already on recall metadata as `candidates_considered` today, `recall.rs:239`) beside a listing-shaped `total` or omit `total` on ranked surfaces.

**10. Conflict `status: "ended"|"corrected"|…` changes an existing field’s value set**
Today recall conflicts emit `"superseded"` | `"contested"` (`recall.rs:321-338`).
§3.3.6 widens `status` from `close_reason`. Constraint 3 keeps names/types; value-set expansion is the same class as CC-3’s `suggested_action` fight. Needs CHANGELOG + agent-info, not only a silent widen — and flooding `conflicts` with non-adjudicable “ended” rows (stopped meds with no successor) changes what agents treat as actionable.

**11. `KbScopeOrigin` vocabulary invented then contradicted**
§2.2 claims origin is `"flag" | "all_kbs" | "config_default"` and also “reuse `KbScopeOrigin::as_str()`”.
Code (`retrieval.rs:461-466`): `"flag" | "config" | "all_kbs"`. There is no `config_default`. Internal bug; implementer must pick.

**12. Test isolation is solid; live-store risk is low if followed**
§5 preamble + R14 match constraint 1. No finding that the lane plans to open the live store. Residual: §5.1.2/`forget` and §5.1.3/`edit` require write-surfaces handle acceptance — unmarked cross-lane gate (see **22**).

---

### Migration / cross-wave holes

**13. `decision.recorded` verdict mapping never stated (R4)**
§3.4.8 records a decision with CLI actions (`keep-both`, `supersede-old`, …).
Interfaces R4 / storage DDL: `keep_both | close_old | reject_new | void`.
No mapping table. Wrong string → replay never binds (CD-3 half-fixed). Implementer must invent `supersede-old → close_old`, etc.

**14. Pre-migration queue columns vs conflict JSON**
§4.2 correctly nulls `decision_key` / `member_key` / `registry_version` on v9-shaped rows.
§2.6 still presents them as always present in the conflict shape. Consumers branching on non-null keys need the null case in agent-info the same wave (CLP-5 lists fields but not “null means pre-v10”).

**15. `Fact` still lacks `closed_by` / `close_reason` / `close_authority` in tree**
`types.rs:151-191` has no those fields yet; `is_open_fact` is already in `interval.rs:86-88`.
§2.6 / `fact_to_json` assume the columns. Fine as a storage dependency — but §4.2’s “serde default on v9 rows” only holds after storage lands the fields; until then this code does not compile. Risk R-1 understates that **deliverable 4/7 JSON fields block on storage Fact DDL**, not only on graph types.

**16. History view still cannot see stopped intervals**
Storage deliberately leaves `list_recent_conflicts` on `superseded_by IS NOT NULL` (`spec-storage-journal.md` §2.9).
SR-3 only adds diary/kb filters. Stopped-without-successor facts remain invisible on `facts conflicts` (non-open) after the redesign — only `facts show` historic. Spec never says whether that is intended.

**17. Duplicate / divergent index request**
SR-6: `idx_facts_scope_open ON facts(kb, diary, valid_until, superseded_by)`.
Storage already plans `idx_facts_open_interval … WHERE valid_until IS NULL AND superseded_by IS NULL` for open lookups.
Two indexes, different shapes; lead must merge or this lane’s COUNT plan stays O(table) after all.

---

### Missing / wrong failure modes & control flow

**18. Fourth `RecallOutcome` early return omitted**
§3.2 names sites `:951-956` and final `Ok`. Code also returns empty at `retrieval.rs:920-924` (`runs.is_empty()`). Miss that site → empty `stats` → dishonest absence certificate on a real path.

**19. `absence.reason` has no decision procedure**
§2.3 lists `no_chunks_in_scope | no_candidate_above_cutoff | filtered_by_window | filtered_by_diary`.
`hybrid_recall` does not tag why the pool emptied; there is no score cutoff (so `no_candidate_above_cutoff` is a lie if used). Layer-budget trim in `recall.rs:99-111` can empty the *emitted* set while `outcome.results` is non-empty — verdict/`absence` computed from `results_json` vs `stats` can disagree. Algorithm missing.

**20. Resolve transaction boundaries under-specified**
“Whole body runs inside `with_tx`” (§3.4.8) while calling inherent methods that each use `self.conn` is OK *if* nested under an open tx (`sqlite.rs:835-836`). Spec never says claim + close + decision must go through storage’s single method (finding **4**), nor what happens if `supersede_fact_with` returns false after a successful claim (orphan resolved queue row, no interval close).

**21. Concurrent resolve test is likely invalid as written**
§5.5.3: “two threads, one conflict.” `SqliteStore`’s `Connection` is not a multi-thread share model; this needs two processes or two stores. As written, the test teaches the wrong concurrency story.

**22. Provenance↔mutator tests depend on write-surfaces without a gate**
§5.1.2–5.1.3 require `forget`/`edit` to accept `memory_id` / kind-prefixed handles. That is write-surfaces W1, not this lane. Unlike §5.6, these are not `#[ignore]`’d pending a named gate → false reds or skipped proof of the brief’s headline defect.

**23. Wiki / entity / relation `handle` prefix undefined**
Handle grammar (`interfaces.md` §1 / landed `HandleKind`) has no `wiki`. §3.8 “page branch gains `handle`” does not say which kind. CC-R2’s prefix list also omits wiki. Implementer must invent a non-resolvable prefix or skip — contradicting “every row carries handle.”

**24. `facts list --subject` paging undefined**
Subject path uses `list_facts_by_subject_scoped` with **no limit** (`facts.rs:63-72`). §3.4.2 adds cursor/Page globally; SR-7’s offset on subject list is incomplete without a limit on that API. Unbounded “page” with `total == returned` is honest only if stated; cursor on an unbounded query is nonsense.

**25. R7 read-side guard vs fact-semantics’ older blanket removal**
This lane correctly implements R7 (safety-only → `requires_context`; non-safety may still suggest `supersede-old`). Fact-semantics §1.4 still describes global withdrawal (overruled by R7). Not this lane’s bug, but implementers reading both will thrash; this spec should cite R7 as overriding fact-semantics §1.4 explicitly at the `suggested_action` site (it cites R7, but not the overruled text).

**26. `parse_bound` promotion is unnecessary / wrong home**
§3.4.8 moves date parsing to `read_surface::parse_bound_arg`. `recall.rs` already uses `engram_core::temporal::{parse_bound, DayBound}`. Re-homing into a CLI module duplicates core and fights “one home.”

**27. Brief move 5 “calibrated confidence” not owned, not declined**
Brief move 5 requires calibrated confidence on every read. Interfaces §4 dropped it from the envelope pin; reasoning-layer redefines it for `engram state`. This non-goals list never says confidence is out of scope → an implementer can waste a cycle inventing a third formula.

---

### What is solid (so the lead does not re-litigate)

- Diary leak on `surface_conflicts` / `facts conflicts` history is real (`recall.rs:296-336`, `facts.rs:212`).
- Sweep sites and R3 `LegacyArity` rename match R15 / CM-4.
- R7 defence-in-depth on stale queue rows is the right read-side posture.
- Ranked surfaces refusing a fake cursor (§2.8) is sound.
- Additive `conflicts_returned` / `conflicts_total` (OQ-3) is the constraint-3-safe choice.
- Test preamble’s `ENGRAM_STORE`-only rule matches R14 / constraint 1.

---

### Implementer must re-derive (summary)

| Gap | Why |
|---|---|
| Call `output::Listing` vs `read_surface::Page` | Peer specs disagree; interfaces name fields not modules |
| `respond` API names | CC-R1 vs contract-codegen §2.4 |
| Resolve race API | SR-5 vs storage §2.11 |
| Entire SR count/diary/offset surface | Not in storage spec or interfaces |
| CLI action → `decision.recorded` verdict | R4 vocabulary ≠ CLI strings |
| `absence.reason` + required `threshold` | Honesty vs IN-23 type |
| Wiki handle kind | Not in `HandleKind` |
| Who edits `memory.rs` | Brief + write-surfaces vs this §1.3 |

**Bottom line:** Do not implement from this document alone. Lead needs rulings on (1) envelope module home, (2) resolve/claim API, (3) SR-* acceptance into storage, (4) `threshold` nullability, (5) `memory.rs` owner, (6) `close-old` vocabulary — then a short delta pass on this spec.
