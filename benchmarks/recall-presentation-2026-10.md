# What recall prints, measured — 30 Oct 2026

Three changes to what `engram recall` prints, each one commit, measured with
`benchmarks/longmemeval_cli.py`, which drives the real binary as an agent does.

## Method
- LongMemEval: all 78 knowledge-update and 133 temporal-reasoning questions, plus 25 of each
  other type (seed 7), 311 in all. Each question's evidence sessions come from the S split,
  together with 4 distractor sessions from the same record that fall no later than the
  question's day.
- One store per question, built once with the baseline binary. Each session is written with
  `engram remember --event-time <session date> --autonomy full`, with fact extraction on
  (openai/gpt-6.1-terra) and the JEV adjudicator.
- Each arm copies the frozen store and runs `<binary> recall "<question>" --json` with the
  relevance judge off and no reranker. All 311 questions retrieved identical result ids, in
  the same order, in every arm, so the arms differ only in what recall printed.
- Answerer: openai/gpt-6.1-terra, low effort, temperature 0. It gets the question date, the
  question and the recall stdout verbatim.
- Judge: anthropic/claude-haiku-5.5, using LongMemEval's own prompt templates, read from its
  `evaluate_qa.py`. One answer call failed, in the final arm, and was excluded; no judge call failed.
- Noise floor: answering the baseline twice from identical recall output flipped 9 of 311
  verdicts (4 temporal, 2 knowledge-update).

## Results (accuracy %, 95% Wilson interval)
| type | n | A baseline | B one copy | C + dates | D + present tense (final, fb777e2) |
|---|---|---|---|---|---|
| knowledge-update | 78 | 70.5 (60–79) | 71.8 | 87.2 | **88.5 (80–94)** |
| temporal-reasoning | 133 | 43.6 (35–52) | 44.4 | 82.0 | **81.8 (74–87)** |
| multi-session | 25 | 80.0 | 80.0 | 76.0 | 80.0 |
| single-session-assistant | 25 | 100 | 100 | 100 | 100 |
| single-session-preference | 25 | 72.0 | 72.0 | 68.0 | 76.0 |
| single-session-user | 25 | 100 | 100 | 96.0 | 100 |
| **all** | 311 | **64.6** | 65.3 | 84.2 | **85.8 (81–89)** |
| answer prompt tokens (mean) | | 7,211 | 5,479 | 5,719 | **5,291** |

Paired gains and losses, exact McNemar:
- **B vs A:** +9 / −7 (p = 0.80). Accuracy unchanged; 24% fewer prompt tokens.
- **C vs B:** temporal-reasoning +50 / −0 (p < 0.001), knowledge-update +13 / −1 (p = 0.002).
- **D vs C:** +12 / −7 (p = 0.36). Within the noise floor. With the review's guards in place
  (scope checked at every hop, an ending only on the authority the safety gate demands, no
  disputed values), present-tense lines fired in 2 of 311 recalls here, because extraction
  rarely records a clean supersession.
- **D vs A:** +68 / −2 (p < 0.001). Final arm: 310 compared, one answer call failed and was
  excluded.

## On a real store (40 real recall queries, copy of the owner's store, local only)
- **Output size:** mean output fell from 29.3 KB to 21.9 KB (−25%), with retrieval identical
  on 40 of 40 queries.
- **Present-tense lines:** fired on 36 of 40 recalls, flagging 140 of 400 returned passages.
- **Data quality:** 1,309 of 1,517 historical supersessions are on many-valued relations and
  were decided by older adjudicators. Present tense surfaces them under the passage they
  concern; a JEV re-check of those old decisions is the next data-quality step.

## Not shipped
Background fact extraction ("remember never waits") cut `remember` p50 from 3.55 s to 0.40 s
with the same facts. Review found it unsafe as built, so it was reverted: an edit or forget can
race the extractor, and the KB-wide sweep ignored opt-outs and could leave a memory pending.
It needs a per-memory job with a content-hash guard.

## Limits
- Small stores: 5 to 10 sessions per question.
- A single answerer and a single judge.
- The present-tense effect on LongMemEval rests on 14 recalls.
