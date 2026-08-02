1. **Remember preference:** Succeeded, 1 attempt.  
`ENGRAM_STORE=/private/tmp/engram-ux/run-luna ENGRAM_BENCH_FORCE_STUB=1 /private/tmp/engram-ux/engram remember --offline --json "Priya Raman prefers written updates over meetings."`

2. **Remember anniversary as date:** Succeeded, 1 attempt.  
`... engram remember --offline --json --event-time 2021-03-03 "Priya Raman's work anniversary is 3 March 2021."`  
Output stored `"event_time":"2021-03-03T00:00:00+00:00"`.

3. **Correct preference:** Succeeded, 1 edit attempt.  
`... engram edit --offline --json 2ea5016e-45f5-400a-8ee3-81f4e9eca85a --content "Priya Raman prefers short video updates, not written updates or meetings."`

4. **Find everything known:** Succeeded, 2 queries.  
`... engram state --json "Priya Raman"` returned `"verdict":"no_match"`.  
`... engram recall --json --profile offline "Priya Raman"` returned the anniversary and corrected preference.

5. **Store/check medications:** Succeeded, 2 store attempts and 1 check.  
`... engram remember --offline --json "Priya Raman takes 5mg of cetirizine."`  
`... engram remember --offline --json "Priya Raman also takes 10mg of loratadine."`  
`... engram recall --json --profile offline "What medications does Priya Raman take?"` found both medications. However, `state --relation medication` again returned `"subject_not_found"`.

6. **Unknown blood type:** Query succeeded, 1 attempt:  
`... engram recall --json --profile offline "What is Priya Raman's blood type?"`  
It did not say unknown. It returned all four unrelated memories with `"verdict":"results"`.

7. **Delete one memory:** Succeeded, 1 attempt. Deleted loratadine:  
`... engram forget --json --confirm 381e0ac3-f63a-4ca5-93d8-c8f7315747fc`  
A follow-up recall no longer returned loratadine.

Surprises: every write reported `fact extraction skipped: no OpenRouter key configured`; raw recall worked, but structured `state` knew nothing. The unknown query also produced irrelevant results instead of abstaining.

Most helpful change: make offline mode perform local fact extraction and make recall return an explicit “no answer” when relevance is weak.