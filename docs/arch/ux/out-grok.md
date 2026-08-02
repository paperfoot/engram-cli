## Engram usability report

### Per task
1. **Preference (written)** — Succeeded on attempt 2. Plain `remember --offline "..."` stored a chunk but `facts_added:0`. Worked: `remember --offline --facts-json '[{"subject":"Priya Raman","predicate":"prefers","object":"written updates over meetings","confidence":1.0,"fact_type":"preference"}]' "..."`.
2. **Anniversary as date** — Succeeded on attempt 1: `remember --offline --event-time 2021-03-03 --facts-json '[{"subject":"Priya Raman","predicate":"work_anniversary","object":"2021-03-03","confidence":1.0,"fact_type":"event","valid_from":"2021-03-03"}]' "..."`.
3. **Correct to video** — Succeeded on attempt 3. New `remember --facts-json` kept *both* prefs open (`set_valued`, no conflict). `facts conflicts --open` was empty. Worked: `edit --offline --content "SUPERSEDED: ..." d049fc44-...` → `facts_invalidated:1`.
4. **Everything about Priya** — Succeeded via `state "Priya Raman"`. `people show "Priya Raman"` failed: `no entity named "Priya Raman"`. `people list` was empty.
5. **Medications** — Succeeded: two `remember --offline --facts-json` calls for cetirizine/loratadine; `state "Priya Raman" --relation takes_medication` showed both doses.
6. **Unknown (blood type)** — Mixed. `recall --profile offline "What is Priya Raman's blood type?"` returned 6 unrelated Priya chunks as `verdict:results` (cetirizine first). `state "Priya Raman" --relation blood_type` correctly returned `verdict:no_match` / `absence.reason:subject_not_found`.
7. **Delete** — Succeeded on attempt 2: `forget` without `--confirm` error; `forget --confirm e458a113-...` deleted and closed the cetirizine fact.

### Confusion / wrong guesses
- `remember --offline "..."` → `fact_extraction:skipped_no_openrouter_key` — looked “successful” but created no facts.
- Contradictory `prefers` facts silently coexisted; I expected auto-supersede or a conflict queue.
- Errors suggest `engram agent-info --schema` (forbidden in this experiment).
- `people` is a dead end after storing person facts.

### Surprises / wrong behaviour
- Offline `remember` without `--facts-json` is a silent half-store.
- `prefers` is multi-valued, so “correction” is not a correction.
- `recall` on unknown questions never says “unknown”; it ranks irrelevant hits.
- `state` is the honest read path; `recall` is not.

### Single most helpful change
If `--offline` / no API key: **refuse or loudly warn** that plain `remember` won’t create queryable facts, and print the `--facts-json` shape to use — or auto-parse simple subject/predicate/object from the text.
