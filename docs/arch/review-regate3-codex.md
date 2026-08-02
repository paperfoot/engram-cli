# NO-GO

Three release blockers remain at clean HEAD `501583d11deb61b11523cedd43bb81eeaa203cc3` (`arch/v0.4`, 82 commits over `main`, schema v12). Both pass-two proofs now pass in isolation; the failures appear when the fixes compose.

## Blocking findings

1. **Imported events are laundered into `OwnJournal` by `admin rebuild`.**

   Events persist no origin field. [`import_v3`](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:264) appends an attacker-controlled event, then applies it as `Origin::Imported`. A rejected close is deliberately retained in the journal at [rebuild.rs:324](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:324). Later, `admin rebuild` calls `apply`, which unconditionally reclassifies it as `OwnJournal` at [rebuild.rs:165](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:165) and [rebuild.rs:735](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:735). The event schema cannot remember otherwise: [journal.rs:43](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:43).

   Reproduction: [repro_imported_event_rebuild_launder.sh](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate3/repro_imported_event_rebuild_launder.sh:24)

   ```text
   open_after_import=1
   forged_event_committed=1
   closed_after_rebuild=1
   allergic_to | penicillin | until=from | superseded | heuristic
   ```

   Route assessment:

   - V3 event import: exploitable as above.
   - V2 import: attacker rows generate trusted genesis events; see blocker 2.
   - V1 import: cannot inject facts or close events.
   - Daemon: no import or rebuild HTTP endpoint in its route table at [serve.rs:1080](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/serve.rs:1080).
   - Raw backup/copied SQLite store: trusted wholesale. An attacker-supplied DB therefore reaches `OwnJournal`; that is defensible only if store-file integrity is an explicit trust assumption.

2. **`reason: "stopped"` is attacker assertion, not provenance.**

   `cessation_close_is_exempt` checks only the caller-controlled reason and equal timestamps: [export_v3.rs:138](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:138).

   Two V3 attacks succeed:

   - A forged fact row lands first and is exempted at [sqlite.rs:5606](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5606). Its authentic open `fact.observed` event then loses to `INSERT OR IGNORE` at [rebuild.rs:223](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:223).  
     Reproduction: [repro_stopped_row_shadow.sh](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate3/repro_stopped_row_shadow.sh:22)

     ```text
     authentic_open_events=1
     allergic_to | penicillin | until=from | stopped | heuristic
     ```

     `INSERT OR IGNORE` protects an already-present same-ID target row, but not a fresh restore: the forged redundant row shadows the legitimate event from that backup.

   - Removing the rows and adding `payload.close_reason:"stopped"` directly to `fact.observed` also works. The guard reads this supposedly nonexistent field at [rebuild.rs:195](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:195), exempts the event, then inserts a closed safety fact with both close reason and authority `NULL`: [rebuild.rs:223](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:223).  
     Reproduction: [repro_forged_observed_cessation.sh](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate3/repro_forged_observed_cessation.sh:21)

     ```text
     closed_without_authority=1
     allergic_to | penicillin | until=from | NULL | NULL
     ```

   V2 has the same problem: an exempt forged row is accepted, then `synthesize_genesis` converts it into trusted journal history at [export_v3.rs:318](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:318). [Runnable proof](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate3/repro_v2_stopped_genesis.sh:21) produced `synthesized_close=1 closed_before=1 closed_after=1`.

3. **A genuine v11 ordinary decision still reverses after its conflict row is purged.**

   The v11→v12 migration adds the column but intentionally leaves existing decisions `NULL`: [decisions.rs:93](/Users/dev/Projects/engram-cli/crates/engram-storage/src/decisions.rs:93). Recovery depends on the conflict row and losing fact at [semantics.rs:693](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:693). `kb delete` removes facts, cascading the queue while the decision survives: [sqlite.rs:1474](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1474), [sqlite.rs:1251](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1251).

   Safety relations now queue correctly, but ordinary relations deliberately retain positional replay at [semantics.rs:850](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:850), explicitly pinned by [semantics/tests.rs:1096](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics/tests.rs:1096).

   A real schema-v11 store created through the previous binary reproduces the reversal using only documented commands: [repro_v11_ordinary_direction_reversal.sh](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate3/repro_v11_ordinary_direction_reversal.sh:24).

   ```text
   Beta Corp | closed | replayed_caller | replayed_close_old
   Acme      | open
   null_after_v12_upgrade=1 wrong_close=1 wrong_winner=1
   ```

   The caller had rejected Acme and kept Beta. The supported upgrade/purge/recreate path silently applies the opposite decision. Under the unchanged data-corruption blocker definition, the safety/ordinary split is not defensible.

## Checks that held

- Both supplied pass-two proofs exit `0`.
- `would_move` itself has no comparison gap: it covers every column written by the close update, including conditional supersession fields. [rebuild.rs:150](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:150), [rebuild.rs:330](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:330)
- Realistic v9 migration: `14,526` decisions, zero verdict/loser/event mismatches, zero missing or spurious orientation, queue `15476/15476`, confirmations `300/300`, identical decision digest after rebuild, `integrity_check=ok`. The directional fixture also passed all four decision shapes. [Proof script](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate3/proof_v9_decision_migration.sh:45)
- The six quick regressions held: extractor provenance, inverted/non-point import rejection, orphan preservation, confirmation restoration, extraction-marker weld, and known wire-break declarations.
- `131` focused storage/CLI tests passed. The current workspace lists `1,172` test entries, not `1,168`.
- The whole-workspace run passed `334` tests before this sandbox rejected `TcpListener::bind` in [daemon_support/mod.rs:108](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/daemon_support/mod.rs:108). That is an environment limitation, not a product failure.
- All eight audit evidence obligations reproduced their expected results. The repository remained clean.

## Advisories

- **Medium:** `close_authority_rejected` is accumulated but omitted from import output, while `conflicts_preserved_orphan` is similarly omitted from rebuild output. [export_v3.rs:78](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:78), [import.rs:84](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/import.rs:84), [rebuild.rs:678](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:678), [admin.rs:42](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/admin.rs:42)
- **Low:** fact-row rejection is calculated before `INSERT OR IGNORE`; an already-existing ID can therefore increment the refusal counter even though no row moved. [sqlite.rs:5606](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5606)
- **Low:** raw SQLite restoration has no authenticity or origin model. Document that copied database files are trusted local state if that is the intended boundary.