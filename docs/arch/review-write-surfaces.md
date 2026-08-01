# review-write-surfaces.md — adversarial review of `spec-write-surfaces.md`

Reviewer: Grok 4.5 (`cursor-agent`, `cursor-grok-4.5-high`), 2026-08-01.
Reviewed against: `redesign-brief.md`, `interfaces.md` (v2, R1–R15), `coherence-report.md`, the
peer specs already on disk, and the 0.3.1 source tree. Raw output below, unedited.

Disposition of each finding is recorded in `spec-write-surfaces.md` — applied fixes in §2–§5,
contested points as open questions 9–14 in §7.

---

## Verdict

The lane correctly targets the real W1–W6 defects (`edit.rs:190`, `forget.rs:17–21`, `remember.rs:632`, export v2), and it follows R13/R14/R15 in spirit. It is not implementable as written against the peer specs already on disk: it **re-owns journal appends and export APIs that `spec-storage-journal.md` already claimed**, its handle algorithm **misses the two-step resolve storage documented**, and the **edit/`reextraction` control flow cannot produce a correct `"replayed"`**.

---

### Critical — hard interface / peer-spec collisions

**1. Duplicate journal ownership with storage-journal (invariant 1 / CD-9 adjacent)**  
`spec-storage-journal.md` §2.3 / §3.2 already appends `memory.revised`, `memory.meta_changed`, and `memory.retracted` inside `update_memory` / `soft_delete_memory`, and closes facts inside those methods. This spec’s E4/E5/§3.3 also append those events and call `close_facts_for_memory` after `soft_delete_memory`.  
If both land as written → **duplicate events** and/or **double-close**. If only one lands, the other lane’s plan is fiction. An implementer cannot know which is binding without a lead ruling. `interfaces.md` §2 only says *someone* appends in the same tx — it does not assign CLI vs storage.

**2. Edit close-reason conflict with storage’s `delete_facts_for_memory` rewrite**  
Storage §3.2 `:1925-1935`: deprecate and rewrite to close with **`retracted_source`**, alias `close_facts_for_memory`.  
This spec W3/E3/WR-1: close with **`CloseReason::Corrected`** on edit, `RetractedSource` on forget.  
Following storage’s alias → every edit stamps the wrong `close_reason`. Following this WR-1 → storage’s “rename not redesign” claim is false.

**3. Forget close-authority conflict**  
Storage soft-delete closes with `authority="explicit_end"`. This spec uses `CloseAuthority::Caller { actor }` (and payload `authority: "caller:<actor>"`).  
`ExplicitEnd` vs `Caller` matters for `may_close` audits and rebuild of `close_authority` (R2/CM-2). User `forget` is not an explicit end-date.

**4. Export/import API invented; CD-9 seam is the wrong shape**  
This WR-5: `export_journal_slice` / `import_journal_slice` / `synthesize_genesis_events` / opaque `JournalSlice`.  
Storage §2.13 (the CD-9 owner): `export_v3` / `import_v3` / `import_v2_with_genesis` / concrete `ExportV3` (events **plus** kbs/documents/memories/chunks/facts).  
I2 only wires events + chunk rows → **drops documents/KBs** that storage says the journal does not carry. I3’s `synthesize_genesis_events(None)` is not the storage API and ignores KB scope of a filtered export.

**5. Handle resolution misses storage’s two-step `wrong_id_kind` (IN-7)**  
`spec-storage-journal.md` §2.7: `kind == Some(k)` probes **only** that table; `None` → not found; caller must re-resolve bare to name the real kind.  
This §2.1 does one `resolve_handle` call. So `edit fact:<memory-uuid>` or `edit mem:<chunk-uuid>` → **`not_found`**, not `wrong_id_kind`. That reintroduces the “already deleted?” class of agent failure for prefixed wrong kinds — the move-5 defect, half-fixed.

**6. Soft-deleted resolve vs `memory_scope` is internally inconsistent (WR-8 / W2)**  
WR-8 / storage: soft-deleted memories **resolve**.  
`memory_scope` today filters `deleted = 0` (`sqlite.rs:2763-2768`).  
§2.1 step 3: `memory_scope` None → forget treats as already-retracted.  
§3.3: already-retracted comes from `soft_delete_memory → false`, and needs a `MemoryTarget` with `diary` for `actor`.  
`resolve_memory_target` cannot both (a) return `NotFound` via `memory_scope` and (b) return `Ok(MemoryTarget)` for double-forget. No API to read kb/diary of a deleted row is requested. **Implementer must invent one.**

**7. `CliError` shapes contradict contract-codegen (R11 / XC-1)**  
This XC-1: `NotFound(String)`, `WrongIdKind { message, got: HandleKind, expected: HandleKind, handle }`.  
`spec-contract-codegen.md` §2.3: `NotFound { what, handle }`, `WrongIdKind { handle, actual: String, expected: String, correct: Option<String> }`.  
Both claim the same file via the patch channel. One must lose; this lane’s call sites are written against the wrong shape.

**8. E9 race `NotFound` → exit 1 fights R11**  
E9: in-tx `StorageError::NotFound` stays **transient exit 1**.  
R11 / contract-codegen: `StorageError::NotFound` → `ErrorCode::NotFound` → **exit 3**.  
After R11 lands, `?` on that storage error flips the race from exit 1 → 3 unless mapped to `CliError::Transient` explicitly. Spec assumes today’s `CliError::Storage → 1` mapping (`error.rs:50`), which R11 deletes.

---

### Hard-constraint / compatibility tension

**9. `export` `version: 2` → `3` changes an existing field’s value**  
Constraint 3: existing JSON fields keep names and types; new fields additive. Value change of `version` breaks any consumer that pins `== 2` (spec §4.2 admits this). Storage’s v3 is a **superset** file; bumping the discriminator without `--compat-v2` (XC-4 deferred) is a real agent break beyond the documented forget exit-code break.

**10. Chunk hard-delete on edit vs move 1**  
Brief move 1: “Nothing is ever hard-deleted.” Spec §1.3/§7.6 exempts chunks. Defensible as index, but **`admin rebuild` cannot restore vectors** from the journal (spec’s own open Q6). That is a silent hole in invariant 3’s “byte-identical projection state” for edited memories unless storage preserves chunks — not contracted here beyond an assumption.

**11. §2.5 vs §3.4 on `memory.written`**  
§2.5 table: `remember` / `memory add` append `memory.written`.  
§3.4: “W6 only… nothing else.”  
Storage already appends on `insert_memory_with_kb`. §2.5 will cause double-append if followed; §3.4 is the only coherent reading. **Table is misleading.**

---

### Migration / import holes

**12. v3 import control flow under-specified vs storage’s rules**  
Storage: v2 projection sections **before** events; preserve event ids; refuse `schema_version` ahead of binary; per-KB export includes `kb IS NULL` subject events.  
This I2/I6: none of that. Test 40 “idempotent” depends on event-id `INSERT OR IGNORE` that this lane never requires of WR-5.

**13. v2/v1 genesis bolted on after row import**  
I3–I4 call a different genesis API than `import_v2_with_genesis`. Risk: genesis that **replays into** projections vs genesis that **only appends events from rows**. Spec never says which; wrong choice duplicates facts or leaves an unreplayable store (invariant 4).

**14. Pre-v10 deleted facts are gone — OK, but CHANGELOG-only**  
§4.1.2 is correct and forward-only. No doctor/integrity signal for “facts were erased by old edit/forget” — agents will see closed-interval history only after the fix.

**15. Test 20 asserts `fact_decisions` survival**  
No `fact_decisions` table in the tree today. Depends on storage migration. Fine if gated; stated as if the cascade to `fact_conflict_queue` is the only FK risk (`sqlite.rs:1106-1108`). Decisions ledger FK behaviour is unstated.

---

### Missing / wrong failure modes & control flow

**16. `reextraction: "replayed"` algorithm is self-contradictory (not just deferred)**  
E3 **always** `clear_extraction_marker` inside the tx.  
E7 **after** tx: if `derivation_for_content` hits → `"replayed"` and “no marker cleared”.  
Marker is already cleared. Also facts were already closed; nothing in this lane restores derivation outputs. Open Q1 understates this: the value is not merely unreachable — **the prescribed order cannot implement it**.

**17. WR-3 / derivation API mismatch**  
This: `derivation_for_content(content_hash)`.  
Storage §2.4: `derivation_for(content_hash, extractor_id)` — content-addressed **per extractor**. Without `extractor_id`, `"replayed"` cannot mean what move 3 means.

**18. Mixed `--facts-json --extract-facts` failure guard**  
Today `remember.rs:508-510`: any failure with `facts_json.is_some()` → hard `facts_json_failure`.  
R5 requires that guard to apply only to the caller batch. Easy to miss; if missed, extraction failure after a committed caller batch incorrectly hard-errors (or the inverse). Risk 2’s adjudication-order change is flagged; this guard change is not called out as a landmine with the same force.

**19. Prefixed BadUuid / UnknownKind message claims**  
§2.1 pins BadUuid wording to `invalid UUID {raw}: {e}` and cites `tests/cli.rs`.  
`forget_with_bad_uuid_is_bad_input` (`cli.rs:896-900`) only asserts exit 3 — **no message pin**.  
`HandleParseError::BadUuid` Display is `'{s}' is not a valid UUID` (`handle.rs:92`), not Uuid’s parse error. Implementer must hand-build the legacy string; the “pinned test” does not exist.

**20. `authority: "caller:<actor>"` wire form unspecified in interfaces**  
Interfaces give `CloseAuthority::Caller { actor }` and an `authority` payload field; storage examples use bare `"explicit_end"` / `"caller"`. Spec invents `"caller:<actor>"`. Rebuild of `close_authority` (R2) needs one canonical form.

**21. Concurrent forget during edit still embeds then fails**  
Existence check moves to handle resolve (good for unknown ids). Soft-delete race after embed still pays then hits E9. Unchanged from today, but R11 exit-code flip (finding 8) changes agent retry behaviour on that path.

**22. `forget` on `chunk:` retracts whole memory**  
Acknowledged in §7.7; no `wrong_id_kind` alternative, no confirm escalation. Surprising mutator behaviour with only `resolved_from` as mitigation.

---

### What the implementer must re-derive (not specified)

**23.** Who appends memory/fact events: CLI or storage methods (finding 1).  
**24.** Soft-deleted kb/diary lookup API (finding 6).  
**25.** Two-step resolve for prefixed wrong kinds (finding 5).  
**26.** Conditional close/marker/restore path for `"replayed"` (finding 16).  
**27.** Whether `import_v3` is “pass whole `ExportV3` to storage” (correct per CD-9) or the custom I2 pipeline.  
**28.** `content_hash` home: WR-6 says `engram_storage` or `engram_core`; storage pins `engram_core::keys::content_hash`.

---

### What holds up

- W2 (`forget` unknown → exit 3), W3/W5 (stop `DELETE FROM facts`), W6 (`UserStated` / R13 / IN-26), and W1’s **bare/chunk** acceptance match the brief and current code anchors.  
- R14 test isolation (`ENGRAM_STORE` only; no `--store`) is correct.  
- `sets_superseded_by: false` on corrected/retracted matches R2/CM-2.  
- Ordering note that `close_facts_for_memory` must not filter `memories.deleted = 0` is necessary if forget closes after soft-delete.  
- Refusing to stub `close_facts_for_memory` as `Ok([])` (§4.4) is the right discipline.  
- Line anchors for today’s defects (`edit.rs:190`, `sqlite.rs:1855-1869`, `:1925-1935`, `forget.rs:17-21`, `remember.rs:632`) match the tree.

---

### Lead decisions needed before this lane codes

1. **Single owner of projection+event writes** for memory revise/retract/fact-close (storage methods vs CLI).  
2. **Canonical export/import API** = storage’s `ExportV3` (wire commands only).  
3. **Reconcile `CliError::NotFound` / `WrongIdKind`** with contract-codegen.  
4. **Edit close reason** = `corrected` (this spec), not storage’s `retracted_source` alias.  
5. **Specify two-step handle resolve** in this lane’s algorithm to match storage §2.7.  
6. **Either drop `"replayed"` from wave 3 wire** or rewrite E3/E7 so marker clear + fact restore are conditional on derivation hit **before** close/clear.

No files were edited.
