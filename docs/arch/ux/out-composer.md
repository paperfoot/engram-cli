## Engram Usability Report

**Task 1** — Success, 1 attempt.  
`engram remember --offline "Priya Raman prefers written updates over meetings"`

**Task 2** — Success, 1 attempt.  
`engram remember --offline --event-time 2021-03-03 "Priya Raman's work anniversary is 3 March 2021"`  
Anniversary stored with `event_time: 2021-03-03T00:00:00Z`.

**Task 3** — Success, 1 attempt.  
`engram edit --offline --content "Priya Raman prefers short video updates over meetings" 7a4d3f86-ec36-4302-8936-d7ae050681a9`

**Task 4** — Success after 2 dead ends, 3 attempts total.  
- `people show "Priya Raman"` → exit 3: `no entity named "Priya Raman"` (suggested `agent-info`, which I couldn't use)  
- `state "Priya Raman"` → `verdict: no_match`  
- **Worked:** `recall --profile offline "Priya Raman"` and `memory list`

**Task 5** — Store: 1 attempt each. Check: 3 attempts.  
- `remember --offline` for both meds worked.  
- `state --relation medication` and `facts show` returned empty.  
- **Worked:** `recall --profile offline "Priya Raman medications"` — returned two separate chunks (cetirizine 5mg, loratadine 10mg), not a unified medication list.

**Task 6** — Asked about blood type.  
`recall --profile offline "What is Priya Raman's blood type?"` returned 4 unrelated Priya memories with `verdict: "results"`. **No "I don't know."** It silently hands you wrong-context data. `state` returned `no_match` — closer to honest, but only because the whole facts layer is empty.

**Task 7** — Success, 1 attempt.  
`engram forget --confirm 30890691-ad35-44e3-b436-0eefa36675ab`

### Confusion

The CLI advertises `people`, `state`, and `facts` as first-class retrieval — but without an OpenRouter key, fact extraction is skipped on every write, so those commands are useless. I burned attempts on `people show` and `state` before falling back to `recall`. Two different "offline" flags: `--offline` on writes, `--profile offline` on reads. Not obvious from `--help` alone.

### Surprises / wrong behavior

- `recall` on unknown questions returns tangentially related memories as if they're answers. Dangerous for agents.
- Every write dumps a JSON blob plus a fact-extraction-skipped warning — noisy.
- 25+ top-level commands. Overwhelming for "remember a person."

### Single change that would help most

A **5-command quickstart** in root `--help`: `remember` → `recall` → `edit` → `memory list` → `forget`, with a note that `people`/`state`/`facts` require fact extraction (needs API key). Without that, you discover the real workflow only after the semantic layer fails on you.
