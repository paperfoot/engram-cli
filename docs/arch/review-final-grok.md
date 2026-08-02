## Review: `arch/v0.4` vs `main`

No critical remote/exploit findings. Residual issues are integrity, safety-authority forgery, migration ops gaps, and one undeclared compat break. `facts resolve` claim race is fixed; write-path `may_close` / triage safety downgrade look solid for live closes.

| # | Sev | Location | Finding |
|---|-----|----------|---------|
| 1 | **High** | `facts.rs:752-758`, `817-877` | **Triage mutates before claim; lost claim is ignored.** Open conflicts are loaded outside the tx. Inside `with_tx`, `supersede_fact_with` runs first; `resolve_conflict_with_decision` returning `Ok(None)` is discarded. Concurrent `facts resolve --action keep-both` (via `Core::write`) can finish first; triage still closes a fact the caller kept. Safety pairs are mostly shielded by the `is_safety_slot` downgrade (`778-787`), but non-safety adjudication is not. Contrast the fixed order in `engine/resolve.rs:104-118` (claim → gate → mutate). |
| 2 | **High** | `sqlite.rs:3181-3188` | **`engram repair` closes open intervals with `ExplicitEnd`, including safety.** `close_reason` is correctly `retracted_source`, but authority is `ExplicitEnd` — which `may_close` allows for `takes_medication` / `allergic_to` / `diagnosed_with`. Soft-delete uses `Caller` (`2065-2072`); repair forges explicit-end authority and can close leftover open safety intervals with no person/text evidence. |
| 3 | **Medium** | `sqlite.rs:1456-1467` | **`delete_kb` still hard-deletes facts.** `DELETE FROM facts WHERE kb = ?1` (FK cascades conflict rows). Journal events remain; projections for that KB are gone. Outside admin rebuild / `hard_delete_memory`, this is a surviving hard-delete path under the append-only model. |
| 4 | **Medium** | `migrate_v10.rs:228-236`; `rebuild.rs:144-166`; `export_v3.rs:190-208` | **v10 backfill stamps superseded rows `close_authority='heuristic'` (unless `resolution='caller'`), including safety.** Live storage does not call `may_close` (`write_path.rs:78-81`). Rebuild/import replay `FactIntervalClosed` with that authority unchecked — so migrated/imported safety closes with heuristic/`successor_interval` persist and reapply. New live closes are gated; history and replay are not. |
| 5 | **Medium** | `backup.rs:23-43`; `migrate_v10.rs:88-90`; `sqlite.rs:929-936` | **Backup gap: path never recorded.** `VACUUM INTO` runs before the migration tx (good; failure aborts). `MigrationV10Report.backup_path` is never set, so `migration_log` always has `backup_path: null` — operators cannot discover the rollback file from the log. |
| 6 | **Medium** | `edit.rs:232-302`, `facts.rs:891`, `remember.rs:449`, `lessons.rs:347+`, `import.rs:249`; `context.rs:13-17` | **Companion `store.with_tx` still owns major mutations** while daemon/jobs use `Core::write`. Two connections + WAL can interleave with #1 despite `busy_timeout`. Documented as adoption debt; still a single-writer hole. |
| 7 | **Medium** | `CHANGELOG.md:13-19` vs `forget.rs:7-10` / `soft_delete_memory` | **Undeclared compat break: `forget` no longer erases facts.** Changelog only lists forget unknown-id → exit 3. Edit’s “closes, doesn’t delete” is declared; forget’s equivalent value change (facts remain as closed intervals, `facts_closed*`) is not in the Breaking list. Scripts that asserted facts vanished after `deleted: true` break silently. |
| 8 | **Low** | `sqlite.rs:4467-4477` | **`set_fact_embedding` mutates the fact projection with no journal event.** Rebuild snapshots embeddings before drop (`rebuild.rs:468-552`), so local rebuild is OK; journal-only rebuild / v3 event replay cannot restore embeddings. |
| 9 | **Low** | `write_path.rs:219-228`; `sqlite.rs:2369-2376` | **`purge_facts_for_memory` / `hard_delete_memory` remain as hard deletes.** Documented for ingest rollback; no production caller found in this branch. Dead but still a hard-delete API outside rebuild. |

### Checked — no medium+ issue in hunt scope

| Area | Result |
|------|--------|
| **(1) forget/edit data loss** | Soft-delete / edit close intervals + journal; no production `DELETE FROM facts` on those paths. |
| **(3) Live safety gate** | `may_close` + `push_close` + LLM supersede filter + triage safety downgrade; `facts resolve` uses `Caller` after claim. |
| **(4) resolve claim race** | Fixed: conditional claim, then mutate, in `Core::write` (`decisions.rs:235-248`, `resolve.rs:104-190`); covered by `daemon_resolve_race.rs`. |
| **(2) Migration idempotency** | Deterministic genesis ids, `INSERT OR IGNORE`, collapse before unique index, orphans counted/skipped; single migration tx. |
| **(5) Declared breaks** | forget exit 3, edit closes facts, export v3, `requires_context`, arity multi, `resolve_command`, clap exit 3, `fact_type: assertion` match the tree. |

**Highest priority:** fix triage to claim-before-mutate (and abort on `Ok(None)`), and stop stamping `ExplicitEnd` in `repair_integrity` (use `Caller` like soft-delete).
