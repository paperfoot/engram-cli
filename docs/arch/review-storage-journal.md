## Verdict

The spec is the strongest of the wave-2 lane docs: line anchors mostly match `sqlite.rs`, the v10 collapse/backup story is real, and R2/R5a/R8/R10/R14/R15 are largely followed. It is **not** “ready to implement” as stamped. A blocking R9 home conflict is still open, landed `CloseAuthority::as_str()` disagrees with the column vocabulary the spec itself pins, and several live `DELETE FROM facts` / FK-CASCADE paths (including frozen `kb`/`repair`) are left to undo move 1 after the soft-delete rewrite.

---

### Hard-constraint / binding-ruling violations

**1. Status “ready to implement” while OQ-1 is blocking (R9 / R1 class)**  
§7 OQ-1: `SEMANTIC_KEY_NAMESPACE` / encode primitives must live somewhere `engram-storage` can call; R9 pins them in `engram_graph`. Spec proposes `engram_core::keys` and proceeds to design the whole migration around it. Until the lead rules, two encodings are the failure mode OQ-1 names — silent non-replay of ~14.5k decisions. Shipping migration code against an unresolved R9 home is a hard fail.

**2. `CloseAuthority::as_str()` ≠ pinned `close_authority` vocabulary (R2 / CM-2)**  
Landed `interval.rs:131-140` returns `"successor_interval"`. Spec §2.1 Fact comment and §2.8 mapping use `"successor"`; fact-semantics annex / IN-11 comment list `successor`. Spec also says storage must persist via `as_str()`. An implementer following “use `as_str()`” writes a value the audit/docs don’t recognize; following the §2.8 table invents a second spelling. Same class of silent rebuild/audit divergence CM-2 was about.

**3. Remaining hard-deletes + CASCADE re-erase the adjudication ledger (brief move 1 / constraint 3 value surface)**  
Spec fixes `soft_delete_memory:1862-1865` and rewrites `delete_facts_for_memory:1925`. Still live in this lane’s own file, unmentioned in §6 patches:

| site | behaviour |
|---|---|
| `delete_kb:1320-1321` | `DELETE FROM facts WHERE kb = ?1` |
| `repair_integrity:2879-2882` | deletes facts whose memory is `deleted=1` |
| `hard_delete_memory:2085-2091` | calls `delete_facts_for_memory`, then `DELETE FROM memories` |

`fact_conflict_queue` still has `ON DELETE CASCADE` (`sqlite.rs:1107-1108`). After soft-delete **stops** deleting facts, `repair_integrity` becomes the path that deletes closed intervals and cascades the queue the journal was meant to save. `kb.rs` / `repair.rs` are frozen (interfaces §7); no CLP requests changes. Move 1 is not closed in this lane as written.

**4. `source_memory_id … ON DELETE CASCADE` (`SCHEMA_V1` ~483) defeats “close, don’t delete” on any real memory-row delete**  
Even if `delete_facts_for_memory` only closes, `hard_delete_memory`’s `DELETE FROM memories` still CASCADE-deletes facts (and then queue rows). Spec never drops or replaces that FK. Implementer must re-derive whether ingest-rollback is allowed to hard-delete or must retract+tombstone.

---

### Interface / peer-spec contradictions

**5. Caller supersession: live path drops `superseded_by`, migration keeps it (compat view / R2)**  
§2.8: `close_fact_interval` sets `superseded_by` **only** when `reason == "superseded"`; `supersede_fact_with` with `resolution="caller"` → `close_reason=corrected`. So a live `facts resolve` caller win closes the interval **without** `superseded_by`. Migration §4.2 only backfills rows that already have `superseded_by`, and keeps it. Effects:

- `list_recent_conflicts:4776-4778` joins on `superseded_by` → caller closes vanish from that history API.
- Wave-2→wave-3 gap: read-surfaces sites still on `superseded_by IS NULL` (`facts.rs:123` etc.) treat corrected-but-not-superseded rows as active (OQ-8 only discusses `stopped`).

Migration and live write of the “same shape” are **not** identical, contrary to §2.8’s claim.

**6. Peer collision with write-surfaces on close reason/authority (already in `review-write-surfaces.md`)**  
This spec: edit/forget path via rewritten `delete_facts_for_memory` / soft-delete → `retracted_source` + `explicit_end`. Write-surfaces: edit → `Corrected` + `Caller`; forget → `Caller` not `ExplicitEnd`. Interfaces do not assign CLI vs storage for the append. Two lanes landing as written → wrong `close_reason`/`close_authority` on the headline defects.

**7. R9 text vs §2.1 `keys.rs` (and pair components)**  
Interfaces §3.4: namespace “pinned in `engram_graph`”. Spec implements in `engram-core` and correctly notes object_norm vs member_identity is unpinned in R9. OQ-1 is right; coding before the ruling is not.

**8. Interfaces §2.1 step 2 wording is unimplementable; spec silently follows the annex**  
R5: `resolution → close_reason` and `resolved_by → authority`. `facts` has `resolution`, not `resolved_by` (`types.rs` / DDL). Annex maps both from `resolution`; this spec copies the annex. Fine outcome, but it never records “interfaces step 2’s `resolved_by` does not exist on facts” — an implementer reading R5 literally will search for a missing column.

**9. Payload additions vs interfaces §2 (OQ-2) are load-bearing, not optional garnish**  
Without `fact_type`/`diary` on `fact.observed`, rebuild fails invariant 3 on any `profile_slot` row (spec admits this). Status still “ready”; rebuild tests 42–43 cannot pass against the pinned payload set until interfaces v2.1 accepts OQ-2.

---

### Migration / import holes

**10. `import_v3` dual source of truth for facts is unspecified**  
`ExportV3` carries both `events` and `facts: Vec<FactExportRow>`. §2.13 says import does v2 sections (memories/chunks/documents — “things the journal does not carry”) then event replay. Facts are journaled **and** listed in the struct. Insert FactExportRow **and** replay `fact.observed` → UNIQUE collisions / non-idempotent import. Ignore facts section → why export it / how does `ImportV3Report.facts` count work? Implementer must invent the rule.

**11. `parse_fact_from_offset` joined-query offsets are not specified after column widening**  
Today: conflict 13 cols → facts at 13 and 31 (`:4366-4367`); recent conflicts second fact at 18 (`:4784`). Spec adds 3 conflict cols + 3 fact cols and warns about offset arithmetic, but never states the new constants (e.g. 16 / 37 / 21). Wrong offsets silently mis-parse triage rows — the exact failure mode the spec names.

**12. Genesis richer than annex order; `memory.written` / `derivation.ran` not in annex step 3**  
Interfaces: “annex order”. Annex step 3 is basically fact observed/closed. Spec adds memory + derivation genesis (good for export/rebuild honesty) without a lead note that annex order is being extended. Low risk if rebuild ignores `memory.*`, but “same generator as annex” is oversold.

**13. `PRAGMA synchronous=OFF` escape in §4.6**  
Allowed “for the duration of the migration transaction” if >30s. A crash mid-migration with sync off can tear the only copy after backup… backup exists, but recovery story isn’t written (restore backup how, from which path, while `open()` keeps retrying migrate). Missing failure mode.

---

### Wrong or incomplete claims about the tree

**14. §2.1 `lib.rs` snippet drops landed `ErrorCode` re-export**  
Tree already has `pub use error::{CoreError, ErrorCode}` (`engram-core/src/lib.rs:17`). Spec’s replacement block lists `CoreError` only. Literal application regresses IN-5 / fights contract-codegen.

**15. `insert_memory_with_kb:1457` / `insert_fact:3937` / `update_memory:1897` are not transactional today**  
§2.3 requires “both halves in one `with_tx`”. Current bodies are bare executes. Plan is fine, but any caller that already opened a different transaction pattern (compile’s `begin_immediate`) needs an explicit composability note — `with_tx` joins an open tx (`:835-836`), which is good, but methods that don’t call `with_tx` and are invoked outside one still violate invariant 1 in release (`debug_assert` only, §2.2).

**16. Fifteen-site line table is accurate; “fifteen” count is slightly soft**  
All listed `superseded_by IS NULL` sites match current `sqlite.rs`. `SCHEMA_V1` partial index `:488` correctly left alone. Good. (Not a defect — credit.)

**17. `tempfile` absence in `engram-storage` is real (CLP-8)**  
Confirmed: only `engram-cli` has `tempfile`; storage has no `[dev-dependencies]`. Patch is necessary for the harness as written.

---

### Missing failure modes / implementer must re-derive

**18. Invariant 3 narrowed below brief move 1 without a binding ruling (OQ-3)**  
Brief: “every current table becomes a rebuildable projection.” Spec excludes memories/chunks/embeddings/compiler caches and prints that. Honest, but the released guarantee string in interfaces invariant 3 still says “byte-identical projection state.” Lead must confirm before CHANGELOG/agent-info language ships.

**19. `admin rebuild` is undeliverable in-tree without CLP-1/2/3**  
D3 + tests 42–48 spawn `engram admin rebuild`. Dispatch/`cli.rs`/`mod.rs` are other owners. Without lead patches at integration, the binary surface does not exist; library `rebuild_projections` can still be unit-tested — the spawned-binary plan cannot.

**20. Empty-`decision_key` stopgap (CLP-6) leaves wave-2 queue semantics half-migrated**  
`""` → SQL NULL so the partial unique index doesn’t collapse everything. Until fact-semantics fills real keys, `DuplicateKey` never fires and open-queue storm behaviour is unchanged. Spec is clear this is a stopgap; “wave 2 green” ≠ “storm fixed.” Tests 23/37/38 need fixture-level keys, not the production caller.

**21. Soft-deleted resolve vs forget/`memory_scope` (IN-7 adjacent)**  
§2.7: soft-deleted memories resolve. Callers that still use `deleted = 0` filters must distinguish “not found” vs “already deleted.” Spec correctly refuses a `deleted` flag on `Resolved` but does not name the read API for diary/kb of a deleted row (write-surfaces review already hit this). Storage-journal implementer will get pinged for it.

**22. `actor = "migration:v10"` outside interfaces actor grammar**  
Interfaces §2: `"cli:<diary>" | "daemon" | "job:<uuid>"`. Additive strings are probably fine; anything that validates actor as an enum will reject genesis. Unstated.

---

### What looks solid

- Backup: no mechanism in tree today; `VACUUM INTO` outside `migrate()`’s `with_tx`; skip `:memory:`; abort on failure — matches R5a/CD-7/CC-2.
- Duplicate-key collapse **before** `idx_fcq_open_key` — closes the annex hole that would abort every storm store.
- Decision precedence SQL (`caller` first, `void` excluded, propagate errors) matches R8/IN-9.
- `InsertOutcome` vs `INSERT OR IGNORE` matches R10/CM-7.
- Fifteen `sqlite.rs` open-interval sites + read-surfaces handoff matches R15/CD-2.
- Defect anchors (`:1855-1869`, `:1925`, `:1106-1108`, FK cascade story) match the code.
- Landed `relations.rs` / `interval.rs` / `EventKind` / `ENGRAM_STORE` claims check out against the tree.

---

### Bottom line

Treat as a strong design draft that still needs lead rulings on **OQ-1 (R9 home)**, **OQ-2 payloads**, **OQ-3 rebuild scope**, and an explicit disposition of **remaining DELETE/CASCADE paths** (`delete_kb`, `repair_integrity`, memory FK) plus the **write-surfaces close_reason/authority split** before coding the migration. Highest-risk silent bugs if implemented as written: `successor` vs `successor_interval`, caller closes without `superseded_by`, `import_v3` double-applying facts, and `repair_integrity` destroying the ledger the soft-delete fix just preserved.
