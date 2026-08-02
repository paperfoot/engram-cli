Usability report (all commands used the exact prefix `ENGRAM_STORE=/private/tmp/engram-ux/run-terra ENGRAM_BENCH_FORCE_STUB=1 /private/tmp/engram-ux/engram`):

1. Succeeded, 1 attempt: `remember 'Priya Raman prefers written updates over meetings.' --offline`

2. Succeeded, 1 attempt: `remember "Priya Raman's work anniversary is 3 March 2021." --event-time 2021-03-03 --offline`  
   It stored `event_time:"2021-03-03T00:00:00+00:00"`.

3. Succeeded, 1 attempt: `edit 26334c8b-290b-4b30-9249-050756e67b02 --content 'Priya Raman prefers short video updates, not written updates or meetings.' --offline`

4. Succeeded, 1 attempt: `recall 'Priya Raman' --profile offline --top-k 10`  
   It returned exactly the corrected preference and anniversary.

5. Succeeded; each command worked first try:  
   `remember 'Priya Raman takes 5mg of cetirizine.' --offline`  
   `remember 'Priya Raman also takes 10mg of loratadine.' --offline`  
   `recall 'What medications does Priya Raman take?' --profile offline --top-k 10`  
   It found both, but ranked the anniversary above loratadine. Bad retrieval.

6. Succeeded, 1 attempt: `recall "What is Priya Raman's blood type?" --profile offline --top-k 10`  
   Exact behavior: it returned `verdict:"results"` and all four unrelated memories. It did not say “unknown” or indicate zero evidence. That is seriously wrong behavior for a question-answer memory tool.

7. Succeeded, 1 attempt: `forget 246c8389-e235-4950-be5a-8d1c230f455d --confirm`  
   Output included `"deleted":true`. A follow-up `recall cetirizine --profile offline --top-k 10` still returned three unrelated memories instead of no result.

No commands failed. The repeated write output was: `"fact extraction skipped: no OpenRouter key configured"`, so `state`/facts would not reflect normal memory writes in this required offline setup.

Biggest improvement: retrieval must enforce a relevance threshold and return an explicit **unknown / no evidence** result instead of confidently dumping unrelated memories.