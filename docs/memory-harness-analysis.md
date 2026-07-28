# Memory & context management in production agent harnesses

**What this is:** a detailed analysis of the memory, dreaming, and
context-management subsystems found in a collection of production coding-agent
harness codebases we were provided. The goal: enough precise detail — prompts,
constants, data formats, algorithms — that a developer can recreate the designs
without reading the original code.

Naming: **Harness J** = a large JS terminal harness, examined at two points:
**J-old** (we have its original source — every J-old claim is verbatim from it)
and **J-new** (a later release, examined as a faithful but machine-reconstructed
bundle — constants and strings are exact, control flow is high-confidence but not
guaranteed). **Harness R** = an open-source Rust harness (original source; claims
are verbatim).

**Epistemic status.** This is a *descriptive* analysis of shipped systems, not a
formal specification. Where a harness's own design has a known weakness, we
document it faithfully **and** flag it (look for ⚠). Where we recommend adopting
a pattern, we recommend the corrected version, not the flawed original. Claims
marked *(inferred)* are reconstructions of intent from usage, not direct
observation.

**Provenance quick map:**

| Content | Source | Confidence |
|---|---|---|
| §2.1–2.4 threshold constants, compact prompt structure | J-old original source | verbatim |
| §2.5–2.6 micro-compact, precompute | J-new reconstruction | strings exact, flow high-confidence |
| §3 auto-memory (layout, formats, extraction, injection) | J-old original source | verbatim |
| §4 dream gates, lock, prompt | J-old original source | verbatim |
| §5 session memory, team sync, away-summary | J-old original source | verbatim |
| §6 R storage/search/dream/flush | R original source | verbatim |

---

## 1. The layered architecture (Harness J)

Five cooperating layers, each with its own timescale and writer:

| Layer | Timescale | Written by | Read by |
|---|---|---|---|
| Context window (compaction stack) | minutes–hours | harness (mechanical) + LLM summarizer | the model, every turn |
| Session memory | one session | background fork, every ~5k tokens | compaction (as summary substitute) |
| Auto-memory (index + topic files) | across sessions, per project | LLM extraction fork at turn end | index always injected; topics pulled |
| Dream consolidation | daily batch | forked dream agent | rewrites auto-memory files |
| Team memory | across people | all of the above + ETag sync | same as auto-memory |

**The core design decision:** the harness enforces budgets, triggers, and
sandboxing; the model does *all* semantic work (extraction, merge, dedup,
contradiction resolution). There is no embedding/BM25/dedup math in Harness J's
memory at all. Harness R made the opposite choice (SQLite FTS5 + vectors,
temporal decay, MMR, cosine dedup gates). Both are documented below.

---

## 2. Context management — the compaction stack

### 2.1 Symbols and thresholds (J-old constants, verbatim)

```ts
const AUTOCOMPACT_BUFFER_TOKENS       = 13_000
const WARNING_THRESHOLD_BUFFER_TOKENS = 20_000
const MANUAL_COMPACT_BUFFER_TOKENS    =  3_000
const MAX_OUTPUT_TOKENS_FOR_SUMMARY   = 20_000  // p99.99 observed summary = 17,387
const MAX_CONSECUTIVE_FAILURES        = 3       // circuit breaker
```

Symbol definitions (distinct — earlier drafts of this doc conflated them):

```
model_context_window   = capability of the model (e.g. 200_000; 1M behind a beta flag)
output_reserve         = min(max_output_tokens, 20_000)
effective_window       = model_context_window − output_reserve
compact_threshold      = effective_window − 13_000        (auto-compact fires here)
warn_threshold         = compact_threshold − 20_000
blocked_threshold      = effective_window − 3_000         (hard stop before API call)
```

**Worked example** (200k model, default output): `output_reserve = 20_000`,
`effective_window = 180_000`, `compact_threshold = 167_000`,
`warn_threshold = 147_000`, `blocked_threshold = 177_000`.

J-new adds a user-configurable `autoCompactWindow` that *replaces*
`effective_window` in the threshold formulas (precedence: env var → settings →
remote-config → experiment → per-model default table; defaults: newest model →
967_000, older models → 200_000; clamped to [100_000, 1_000_000]; `"auto"` = the
formula above). Note: `blocked_threshold` still uses the *model's real* window,
not the configured one.

State returned per turn (J-new shape):

```ts
type TokenState = {
  level: "ok" | "warn" | "compact" | "blocked",
  tokens_left: int,        // compact_threshold − current_tokens
  percent_left: float,     // tokens_left / compact_threshold, clamped [0,1]
}
```

J-new safety nets worth copying:
- **Prefix-overflow detection**: if system+tools prefix alone exceeds
  `compact_threshold`, report "compaction cannot help" instead of looping.
- **Rapid-refill breaker**: context refilling to threshold within 3 turns of a
  compact, 3 times consecutively → stop with a user-facing "thrashing" message.

### 2.2 The compact summary prompt (structure)

Text-only, no tools: a `NO_TOOLS` preamble ("Tool calls will be REJECTED and will
waste your only turn"), an `<analysis>` scratchpad section (stripped from the
stored result), then nine numbered sections: Primary Request and Intent; Key
Technical Concepts; Files and Code Sections; Errors and fixes; Problem Solving;
**All user messages**; Pending Tasks; Current Work; Optional Next Step (with
direct quotes, verbatim). Custom instructions appended as
`Additional Instructions:`.

J-new additions (verbatim intent):
- *"Note any security-relevant instructions or constraints the user stated. These
  MUST be preserved verbatim in the summary so they continue to apply after
  compaction."*
- Anti-attribution: text inside assistant messages merely *formatted like* a user
  turn is model-generated — never attribute it to the user.

### 2.3 The pipeline

```python
def compact(messages, trigger, custom_instructions):
    hook_out = run_hooks("PreCompact", trigger, custom_instructions)
    if hook_out.blocked:
        raise CompactionBlocked(hook_out.blockedBy)       # J-new: hooks may block

    try:
        result = summarize_with_retry_ladder(messages, hook_out.instructions)
    except ModelError:
        result = fallback_model_chain(messages, hook_out.instructions)   # J-new

    # result = {summary, summarized_through_uuid, preserved_groups, stripped_media}
    # The ladder OWNS the preserved tail (§2.4) — compact() must use exactly
    # result.preserved_groups, deduplicated by message UUID.
    return [boundary_marker(trigger, pre_tokens, result.summarized_through_uuid),
            summary_message(result.summary),               # isCompactSummary=true
            *result.preserved_groups,
            *restore_attachments(excluding=uuids(result.preserved_groups))]
```

`restore_attachments()` budgets (J-old constants): re-read up to **5** recently
read files at ≤5,000 tokens/file (aggregate file budget 50,000 — *in practice
capped at 25,000 by the count×per-file limit*; the 50k figure is a shared ceiling
that also covers other restoration kinds), invoked skills ≤5,000 tokens each
(25,000 total), plan file, SessionStart hooks re-run (re-injects project
instructions + memory index), attachments diffed against the preserved tail.

**Post-condition worth adding** (⚠ neither harness documents it): after assembly,
verify `tokens(result) < compact_threshold`. On overflow, shrink in a coverage-safe
order — first drop restoration attachments (they are redundant with the preserved
tail by construction), then re-run the ladder with a *smaller* `s` so the excess
groups get summarized rather than discarded. ⚠ Never silently drop preserved
groups: they were deliberately excluded from the summary, so dropping them creates
a conversation-coverage gap. If the ladder is exhausted, fail with a user-facing
error instead.

### 2.4 Retry ladders

**Head-truncation (J-old):** group messages by API round (boundary = each new
assistant `message.id`); on summarizer prompt-too-long, drop oldest groups guided
by the 413's token gap (else drop 20%); prepend
`"[earlier conversation truncated for compaction retry]"`; max 3 retries, then a
user-facing "conversation too long" error.

**Tail-peeling reactive (J-new default):** attempt = summarize `groups[0..n−s]`,
preserve last `s` groups verbatim. On `prompt_too_long`, increase `s` by the
smallest step covering the reported token gap. Bail reasons:
`too_few_groups | exhausted | error | media_unstrippable`. On
`media_too_large`, retry the *same* split with images/documents replaced by
`[image]`/`[document]` text; a second media error → user-facing failure.

### 2.5 Micro-compact with persisted-output (J-new)

Instead of summarizing, clear *old tool results* in place when ≥20,000 tokens are
clearable (keep last 5). Cleared content is written to disk and replaced with a
pointer — the model can re-Read it:

```xml
<persisted-output>
Output too large (47.3K). Full output saved to: /path/to/session/<tool_use_id>.txt

Preview (first 2.0K):
...
</persisted-output>
```

Guards: already-cleared blocks are never re-cleared; oversized results are also
persisted *eagerly* at tool-execution time above a per-tool size ceiling.

### 2.6 Background precomputation (J-new)

Arm threshold, with `window` defined as the same value used for
`compact_threshold` — i.e. `min(autoCompactWindow, effective_window)` when
configured, else `effective_window`: `ARM = min(window × 0.8, window − 13_000)`.
(The 967_000 default for the newest model is itself slightly *below* that model's
1M context, deliberately reserving headroom; thresholds then subtract from it as
usual, e.g. compact at 967_000 − 13_000 = 954_000.)

Store entry state machine (completed from the observed fields; some transitions
*(inferred)*):

```
absent --[tokens ≥ ARM, gates pass]--> pending --[fork succeeds]--> ready
pending --[fork fails]--> failed --[up to 3 consecutive failures]--> (no re-arm)
ready   --[threshold fires]--> consumed      (swap-in; messages since
                                              precomputedAtUuid appended to preserved)
ready   --[invalidated: model change, compaction, custom instructions]--> absent
```

Swap-in is also used by manual `/compact` when there are no custom or hook
instructions (reuse telemetry: `hit | miss_custom_instructions | miss_hook |
miss_not_ready`).

**Disk sidecar** rehydration on session resume validates: same session AND model,
age ≤ 7 days, boundary UUID still present, growth since precompute ≤ 150,000
tokens, all preserved UUIDs resolvable. ⚠ It does *not* hash the exact summarized
prefix or hook output — a stronger implementation should (invalidation on any
prefix/hook/config change).

---

## 3. Auto-memory: the file-based store

### 3.1 Layout

```
<config-home>/projects/<sanitized-git-root>/memory/
├── MEMORY.md                  # THE INDEX — the only always-loaded artifact
├── <topic>.md                 # one file per memory
├── team/                      # team scope (synced)
└── logs/YYYY/MM/YYYY-MM-DD.md # append-only daily logs (assistant mode)
```

- All worktrees of one repo share a memory dir (canonical git root).
- Path overrides accepted only from *trusted* settings scopes — project-local
  settings are excluded so a malicious repo cannot redirect memory writes to
  `~/.ssh`.

### 3.2 Write confinement (strengthened description)

Every Edit/Write by memory agents is gated by `is_within_memory_dir(path)`. ⚠ A
plain string-prefix check is insufficient (it confuses `/memory` with
`/memory-evil`, and symlinks inside the memory dir escape it). The harness's team
sync uses the correct pattern; adopt it for all memory writes:

```python
def is_within_memory_dir(path, memdir):
    canon_memdir = realpath(memdir)                      # resolve symlinks once
    canon_target = realpath_deepest_existing(path)       # resolve existing prefix
    return canon_target == canon_memdir or \
           canon_target.startswith(canon_memdir + os.sep)  # separator-aware
```

⚠ Even this is a check-then-write path: a symlink can be swapped between the
check and the write. For a hardened implementation, re-run the containment check
on the *final parent* at write time, or open through a directory file descriptor
with no-follow semantics (`openat(dirfd, name, O_NOFOLLOW)`).

### 3.3 File formats

Topic file (frontmatter + body; filenames are semantic, e.g.
`feedback_retry_tests.md`):

```markdown
---
name: retry-flaky-tests
description: User prefers re-running flaky integration tests once before debugging
type: feedback
---

Re-run a flaky integration test once before investigating.

**Why:** Their CI has known flakes; investigating a flake wastes the session.

**How to apply:** On first test failure, retry once. Only dig in if it fails twice.
```

Index file (`MEMORY.md`) — an **index, not content**:

```markdown
- [Retry flaky tests](feedback_retry_tests.md) — re-run once before debugging
- [Deploy freeze](project_deploy_freeze.md) — no merges until 2026-08-02
- [Sarah's role](user_sarah.md) — backend lead, owns payments service
```

Caps: **200 lines AND 25,000 bytes**, enforced at *read time*: drop lines past
200, then cut at the last newline before byte 25,000 (⚠ edge case the harness
leaves unspecified: a first line longer than 25KB — a robust implementation
should hard-cut at a UTF-8-safe boundary). A warning is appended: *"Only part of
it was loaded. Keep index entries to one line under ~200 chars; move detail into
topic files."* (Rationale for the byte cap, from code comments: p100 observed
index was 197KB across <200 lines.)

### 3.4 The content policy — what to save and what not

Four types: **user** (role, goals, knowledge — avoid negative judgement),
**feedback** (corrections AND confirmations; body = rule → `**Why:**` → `**How to
apply:**`; verbatim rationale: *"Record from failure AND success: if you only
save corrections, you will avoid past mistakes but drift away from approaches the
user has already validated, and may grow overly cautious."*), **project**
(non-derivable ongoing work; relative dates converted to absolute at save time),
**reference** (pointers to external systems).

The exclusion list (applies **even when the user explicitly asks to save**):

```
- Code patterns, conventions, architecture, file paths — derivable from the project
- Git history, who-changed-what — git log/blame are authoritative
- Debugging solutions or fix recipes — the fix is in the code
- Anything already documented in project instruction files
- Ephemeral task details: in-progress work, temporary state, current conversation
```

### 3.5 The extraction pipeline

```python
# at the end of every main-thread turn (fire-and-forget):
def on_turn_end(conversation, cursor):
    if not (feature_on and is_main_agent and auto_memory_enabled): return
    if turn_index % EVERY_N_TURNS != 0: return                       # throttle (default 1)
    if main_agent_wrote_memory_since(cursor):                        # mutual exclusion
        cursor.advance(); return
    fork(system=EXTRACT_PROMPT(N=new_messages, manifest=memory_manifest()),
         prefix=conversation,        # cache-shared prefix (§3.7)
         maxTurns=5,                 # "well-behaved extractions complete in 2-4 turns"
         canUseTool=memory_sandbox)  # §3.6
```

The extraction prompt's scope discipline, verbatim intent: *"You MUST only use
content from the last ~N messages. Do not waste any turns attempting to
investigate or verify that content further — no grepping source files, no reading
code to confirm a pattern exists, no git commands."* The existing-files manifest
(`- [type] filename (ISO date): description` per file) is pre-injected so no
listing turn is wasted.

⚠ Two races exist in the shipped design; fix them in yours:
1. The mutual-exclusion check is a *preflight* check — a direct write can land
   after it and race the fork. Fix: serialize all memory-dir writes (direct and
   fork) through one per-directory queue or a transactional claim.
2. Scope discipline is prompt-only; the fork *receives* the whole conversation.
   Fix: pass only the allowed message slice, or require each proposed memory to
   cite a message UUID within the allowed range and reject others.

### 3.6 The write sandbox (same for extraction and dream)

```python
def can_use_tool(tool, input):
    if tool in (Read, Grep, Glob): return ALLOW
    if tool == Bash:  return ALLOW if bash_is_read_only(input) else DENY
    if tool in (Edit, Write):
        return ALLOW if is_within_memory_dir(input.file_path) else DENY
    return DENY
```

Tools are denied at execution, not hidden — hiding them would change the fork's
cache key (§3.7). ⚠ `bash_is_read_only` on raw shell strings is *not* a safe
contract (`ls > /etc/x`, `find -delete`, command substitution). In your
implementation: expose structured read/list/search tools instead of a shell, or
parse a tiny allowlisted argv grammar with no shell evaluation.

### 3.7 Cache-sharing forks (how the model "sees everything for free")

The summarizer/extractor/dream forks keep the parent's **system prompt, tool
list, model, message prefix, and thinking config byte-identical**, so the fork's
first API request hits the parent's prompt cache; the fork appends only its
instructions as a final user message. Restrictions must therefore be enforced by
`canUseTool` at execution time — any change to the visible tool list or system
prompt busts the cache key. When we say a fork "sees the entire conversation,"
this is the mechanism.

### 3.8 Read path — two regimes

**Regime A (default):** MEMORY.md index injected as user context at conversation
start; topic files pulled on demand.

**Regime B (relevance selection):** no index. Per user turn, a cheap small-model
side-query picks ≤5 files from the header manifest:

```
System: You are selecting memories relevant to the user's message.
        Only include memories that you are CERTAIN will be helpful. Be selective.
Input:  [manifest] + [user message]
Output: {"selected_memories": ["feedback_retry_tests.md", ...]}   # JSON schema, ≤256 tokens
```

Budgets: **≤4KB and ≤200 lines per file, ≤60KB cumulative per session**
(compaction resets the byte counter). Already-surfaced paths tracked to prevent
re-injection. ⚠ Two hardening notes: validate selector output as manifest *IDs*
(not raw paths — reject unknown/duplicate IDs, resolve through §3.2's
containment), and reset the surfaced-path tracking on compaction (their
content may have been compacted away).

### 3.9 Staleness — surfaced, not enforced

- Injection header renders age in human terms: `Memory (saved 47 days ago)`. The
  code comments state the rationale: models are poor at date arithmetic, and a
  relative phrase triggers staleness reasoning better than an ISO timestamp
  *(their stated rationale, not an independent evaluation)*.
- Files >1 day old get: *"This memory is N days old. Memories are point-in-time
  observations, not live state — verify against current code before asserting as
  fact."*
- Drift rule (prompt-level): *"If a recalled memory conflicts with current
  information, trust what you observe now — and update or remove the stale
  memory rather than acting on it."*

---

## 4. Dream — scheduled batch consolidation

### 4.1 Trigger gates (cost-ordered, checked at end of every turn)

```python
def dream_gates(memdir):
    if assistant_mode or remote_mode or not auto_memory or not dream_enabled:
        return SKIP
    last = lock_file.mtime if lock_file.exists() else EPOCH   # first run: time gate passes
    if hours_since(last) < MIN_HOURS:      return SKIP        # default 24
    if throttled(10 * 60_000):             return SKIP        # scan throttle
    if count_transcripts_touched_since(last) < MIN_SESSIONS:  # default 5
        return SKIP
    if not try_acquire_lock(lock_file):    return SKIP
    return FIRE
```

Per-turn cost when gated off: one remote-config cache read + one `stat`.

### 4.2 The lock protocol — as shipped, with weaknesses flagged

```python
def try_acquire_lock(path, stale_ms=3_600_000):
    if path.exists():
        body = path.read()
        try: pid = int(body)
        except ValueError: pid = None          # corrupt body -> reclaimable
        if pid and age(path.mtime) < stale_ms and process_alive(pid):
            return None                        # genuinely held
    prior = path.mtime if path.exists() else None
    path.write(str(my_pid))                    # write bumps mtime to now
    try:                                       # a racer may have written garbage
        won = int(path.read()) == my_pid
    except ValueError:
        won = False
    if not won: return None                    # lost the race
    return prior

def rollback(path, prior, path_existed):
    if not path_existed:
        path.delete()                          # ⚠ shipped version just clears the body,
    else:                                      #   leaving a fresh-mtime file that
        path.clear_body(); path.set_mtime(prior)  # suppresses the next run — fixed here
```

⚠ Honest assessment: write-then-read-back is **not** mutual exclusion (two
processes can both win), and the design knowingly tolerates it — the code comment
says consolidation is idempotent and callers must tolerate duplicates. **For
adoption, use the stronger primitive** (§7): an atomic create-exclusive file, a
filesystem lock, or a DB compare-and-swap with fencing tokens, and separate
`last_success_at` (written only after successful completion) from the active
lease record.

### 4.3 The consolidation prompt (verbatim structure)

```
# Dream: Memory Consolidation

You are performing a dream — a reflective pass over your memory files. Synthesize
what you've learned recently into durable, well-organized memories so that future
sessions can orient quickly.

Memory directory: <memdir>   (already exists — write directly, no mkdir)
Session transcripts: <dir>   (large JSONL files — grep narrowly, don't read whole)

## Phase 1 — Orient
- ls the memory directory; read MEMORY.md; skim topic files so you improve them
  rather than creating duplicates
- If logs/ or sessions/ subdirectories exist, review recent entries

## Phase 2 — Gather recent signal   (priority order)
1. Daily logs (logs/YYYY/MM/YYYY-MM-DD.md) — the append-only stream
2. Existing memories that DRIFTED — facts that contradict something you see in
   the codebase now
3. Transcript search — narrow greps only:
     grep -rn "<narrow term>" <dir>/ --include="*.jsonl" | tail -50
   Don't exhaustively read transcripts. Look only for things you already suspect
   matter.

## Phase 3 — Consolidate
- Merge new signal into existing topic files rather than creating near-duplicates
- Convert relative dates ("yesterday", "last week") to absolute dates
- Delete contradicted facts — if today's investigation disproves an old memory,
  fix it at the source
- The file format and type conventions from the system prompt are the source of
  truth for what to save and what NOT to save

## Phase 4 — Prune and index
- Keep MEMORY.md under 200 lines AND ~25KB. It's an index, not a dump:
  "- [Title](file.md) — one-line hook" under ~150 chars
- Demote verbose entries (over ~200 chars) — move detail into the topic file
- Remove pointers to stale/wrong/superseded memories
- Resolve contradictions — if two files disagree, fix the wrong one

Return a brief summary of what you consolidated, updated, or pruned.
If nothing changed, say so.
```

The dream agent runs with the §3.6 sandbox and the §3.7 cache-shared prefix;
progress surfaces as a background task (phase, files touched); completion shows
an inline "Improved N memories" message.

### 4.4 The contradiction model (all three tiers)

1. **Incremental** — extraction prompt: "update an existing file rather than
   creating a duplicate."
2. **Recall-time** — drift caveat: trust current observation; update the stale
   memory.
3. **Batch** — dream prompt: "if two files disagree, fix the wrong one."

⚠ All three are prompt instructions with no provenance or authority ordering —
"fix the wrong one" delegates the judgement entirely. A principled implementation
should attach evidence metadata (observation time, source) per fact, define
deterministic precedence by fact class, and preserve contested alternatives when
authority is insufficient.

---

## 5. Supporting patterns

### 5.1 Session memory (continuous incremental summarization)

A structured notes file updated by a background fork every **5,000 context tokens
of growth AND ≥3 tool calls** (or the token threshold at a natural break — a turn
with no tool calls). Fixed 10-section template (Session Title, Current State,
Task specification, Files and Functions, Workflow, Errors & Corrections, Codebase
Docs, Learnings, Key results, Worklog). Per-section cap 2,000 tokens, total
12,000, with dynamically generated "MUST condense" reminders naming oversized
sections. At compact time it can **substitute for the LLM summary entirely**,
keeping a verbatim tail that is ≥10,000 tokens AND ≥5 text messages (both
required), capped at 40,000. The fork's only tool: Edit on that one file.
⚠ Add a coverage contract to your version: store `summarized_through_uuid` and
require a fresh checkpoint before substitution.

### 5.2 Team memory sync (deliberately lossy, simple)

- Layout: `<memdir>/team/`, scoped per repo via git remote.
- Protocol: `GET /team_memory?repo=o/r` (ETag/If-None-Match),
  `GET ?view=hashes` (per-key checksums), `PUT {entries}` with `If-Match`.
- Semantics: **pull = server wins per-key; push = delta upload of changed keys;
  deletions never propagate** — documented inline as "the lesser evil."
  ⚠ Note the acknowledged hazard: server-wins pull can overwrite an *unsynced*
  local edit. Track a last-synced base hash and only apply server-wins to keys
  the local side hasn't dirtied.
- Limits: **180,000 bytes/entry** (encoded) against a **200,000 bytes/PUT body**
  cap, leaving 20KB for JSON/encoding overhead; greedy bin-packing of entries into
  PUT batches. (⚠ The shipped constants — 250KB/entry vs 200KB body — contradict
  each other: a legal 200–250KB entry could never be pushed. The 180KB figure is
  the corrected value to adopt.)
- Retry taxonomy *(corrected)*: 412 → refresh hashes, recompute delta, retry ≤2;
  transient statuses (408/409/412/425/429) are exempt from permanent suppression;
  other 4xx and auth failures suppress retries until restart (born of an incident
  where one device emitted 167K push events).
- Secret hygiene at two layers: write-time block + push-time skip (gitleaks-style
  regexes; only the rule ID is logged, never the value).

### 5.3 Assistant mode (append-only paradigm)

The always-on variant flips the write side to **append-only daily logs**
(`logs/YYYY/MM/YYYY-MM-DD.md` — same template as §3.1): "Do not rewrite or
reorganize the log — a separate nightly process distills these logs into
MEMORY.md and topic files." Date-rollover flushes the day's transcript segment so
the nightly distiller always has material. The distiller itself is a cron
(night-window) skill; it shares the §4.3 prompt. *(Its independent gates are
internal and not observable; treat §4.1 with a cron trigger as the model.)*

### 5.4 away-summary

After 5 minutes of terminal blur (focus events), a 1–3 sentence "while you were
away" recap: *"Start by stating the high-level task. Next: the concrete next
step. Skip status reports and commit recaps."* Small fast model, no tools, no
cache write.

---

## 6. Harness R — the algorithmic counter-design

### 6.1 Storage + chunking

```
<config>/memory/
├── MEMORY.md                      # global curated knowledge
└── {slug}-{hash8(identity)}/      # per-workspace (git origin → org/repo, else path)
    ├── MEMORY.md                  # workspace curated knowledge (dream output)
    ├── sessions/YYYY-MM-DD-{slug}-{sid8}.md
    ├── index.sqlite
    └── .dream-lock
```

Chunking (markdown-aware; `max_chunk_chars=1600` applies to the final serialized
chunk text including the prefix; `overlap=320`):
1. whole file ≤ max → single chunk
2. split on ATX headers; oversized sections split on paragraph boundaries;
   oversized lines split at the previous newline, **with a guaranteed-progress
   fallback** to a UTF-8-safe word boundary (⚠ the naive rule loops forever on a
   single 10KB line)
3. continuation chunks are prefixed: `[Context: ## Parent > ### Child]\n\n<tail>`

Index (SQLite): `chunks(id, path, start_line, end_line, text, hash, source,
created_at, updated_at, access_count, last_accessed)`; contentless FTS5 over
`text`; vec0 embedding table (1024 dims) created only if the extension loads —
else FTS-only. Reindex is hash-diff based and transactional.

Multi-process reindex claim (⚠ the shipped meta-row trick has a bootstrap hole;
corrected form — the row is never deleted, so `fence` only grows, and writers must
present the fence they acquired so a stale owner cannot commit):

```sql
CREATE TABLE IF NOT EXISTS claims(
  name TEXT PRIMARY KEY, owner TEXT, expires_at INTEGER, fence INTEGER);
-- acquire (one transaction; success iff affected_rows == 1; remember :fence_out):
INSERT INTO claims(name, owner, expires_at, fence)
  VALUES ('reindex', :owner, :now + 60_000, 1)
ON CONFLICT(name) DO UPDATE SET owner=:owner, expires_at=:now+60_000, fence=fence+1
  WHERE claims.expires_at < :now
RETURNING fence;                       -- :fence_out
-- every write performed under the claim carries :fence_out and is verified
-- against the current row before commit;
-- release (do NOT delete the row — that would reset fence to 1):
UPDATE claims SET expires_at = :now WHERE name='reindex' AND owner=:owner;
```

`source` ∈ {global, workspace, session, dream} — drives decay exemption (§6.2,
§6.4).

### 6.2 Search math (corrected, exact)

```python
candidates = union(fts_search(query, k=18), vec_search(query, k=18))  # max_results=6 → k=3×

# signals — presence is explicit; numeric zero is a VALID score
fts  = 1.0 - (rank - min_rank) / max(rank_range, EPS)   # per FTS result set
vec  = clamp(1.0 - L2_distance / 2.0, 0, 1)             # unit-normalized embeddings

# merge — mutually exclusive branches (vector_weight=0.7, text_weight=0.3)
if has_fts and has_vec: base = max(0.3*fts + 0.7*vec, fts)  # hybrid never undercuts FTS-only
elif has_fts:           base = fts                           # full weight, NOT 0.3*fts
elif has_vec:           base = 0.7 * vec
else:                   reject

# modifiers
decay        = exp(-ln(2)/7.0 * age_days)     # session chunks; EVERGREEN sources -> 1.0
EVERGREEN    = {"global", "workspace"}        # ⚠ NOT "dream" — see §6.4
source_w     = weights.get(source, 1.0)
access_boost = 1.0 + log1p(access_count) * 0.05    # 1 access≈1.035, 100≈1.23

raw     = base * decay * source_w * access_boost
display = clamp(raw, 0, 1)          # ranking uses raw (unclamped); gating uses display
include iff display >= 0.35         # min_score
```

Query side: keyword extraction drops ~150 stop words; an **evergreen supplement
query** (FTS restricted to global+workspace) is union-merged so session volume
can't crowd out curated memory; structurally empty chunks filtered. Staleness
notes on session results: >1 day → "Verify this is still current"; >7 days →
"Verify current state before relying on this."

**MMR** (opt-in, λ=0.7), completed pseudocode:

```python
selected = []
while candidates and len(selected) < max_results:
    best = argmax over d in candidates of (
        0.7 * rel(d)                                # rel = min-max normalized raw
        - 0.3 * max((jaccard(tokens(d), tokens(s)) for s in selected), default=0)
    )
    selected.append(best); candidates.remove(best)
# jaccard over lowercase alphanumeric token sets; J(∅,∅) = 1; ties → lower chunk id
```

### 6.3 Dream + flush (the write contract)

Dream gates: enabled → time (≥4h) → sessions (≥3 since last) → lock — then one
model call, input ≤32K chars (existing MEMORY.md prepended unless it's a
scaffold template, capped at half the budget; session logs appended until full),
strict output contract:

```
System: You are performing a dream — a reflective pass over memory files...
1. Merge related information into coherent topic summaries
2. Resolve contradictions — if a recent session disproves an older fact,
   keep only the current truth
3. Convert relative dates to absolute dates
4. Discard ephemeral details (greetings, tool noise, message counts,
   'Current state'/'Next steps' sections, session metadata)
5. Preserve decisions, rationale, architecture, preferences, problem/solution pairs
Respond with a single markdown document using ## headers.
If the logs contain nothing worth persisting, respond with NO_REPLY.
```

Outcome handling (⚠ corrected against three shipped weaknesses):
- empty / NO_REPLY → treated as *failure* and retried forever on the same input.
  **Fix: treat NO_REPLY as a successful no-content outcome — record the examined
  session versions and advance the watermark without deleting anything.**
- Output validation: must contain a `##` header; then truncated at 16K chars.
  ⚠ Blind truncation can cut mid-heading/mid-UTF-8 — **fix: reject-and-condense
  over-limit output, or truncate at a structure-safe boundary and re-validate.**
- Write-back: **full overwrite** of workspace MEMORY.md. ⚠ Data-loss risk: if
  existing memory + sessions exceed the 32K input cap, content the model never
  saw is deleted. **Fix: full-overwrite only when the complete prior file was in
  the input; otherwise patch/merge sections hierarchically.**
- On success, processed session logs are deleted (300s recency guard against
  concurrent appends); on failure, lock rollback.

Flush (pre-compaction extraction): fires at `compact_threshold − 4_000` tokens,
last ~20 messages, delta-mode after the first flush (extract only NEW
information). Dedup gate: embed content, kNN(k=3), duplicate if cosine > **0.92**
— fail-open on any embedding error. Cap 8K chars, mandatory `##` header.

### 6.4 The wart to learn from

Dream reindexes consolidated MEMORY.md with `source="dream"` — **not** in the
evergreen set, so consolidated knowledge loses its decay exemption (7-day
half-life!) until a watcher reindex reclassifies it. ⚠ Note this only heals if
the reindex diff key includes source classification (a pure content-hash diff
would skip the unchanged text forever). Lesson: classification fields that gates
depend on must (a) be set identically on every write path, and (b) participate in
the diff/upsert key.

### 6.5 Injection (first turn + post-compaction)

First turn: search with the user's first real query (greeting fallback → literal
`"project conventions preferences architecture"`), min_score = 0.0, top 6, each
snippet ≤500 chars, wrapped in `<memory-context>` tags. Idempotency: if the
leading system message already contains the tag, **reuse it verbatim** —
re-searching would mutate the prefix and bust the KV cache. Post-compaction: same
search with the last user query, top 3, appended to the post-compaction system
reminder. ⚠ Memory text is *untrusted content*: chunks containing closing tags or
instruction-like text can break the wrapper — use a structured field or robust
escaping, not naive string interpolation.

---

## 7. Applying this to a memory CLI (engram)

### Reported strengths of engram today
The project reports semantic supersession with contested-slot hand-off, cited
precision retrieval, and typed entities/relationships — all more principled than
Harness J's prompt-only approach. *(Verify these against your current commit and
benchmarks before leaning on them in design decisions.)*

### Adopt (priority order; corrected forms, not shipped warts)
1. **The exclusion list (§3.4)** as a write-time lint — reject code-derivable,
   git-derivable, ephemeral content *even when the user asks*.
2. **Evergreen vs decaying classes (§6.2)** — curated facts exempt from decay;
   session/ephemeral entries decay. Set the class on every write path and include
   it in the diff key (§6.4).
3. **Human-term age at recall (§3.9)** — "saved 47 days ago" + point-in-time
   caveat on code-referencing facts.
4. **Two-budget injection (§3.8)** — per-item cap + session cap + surfaced
   tracking; reset both on compaction.
5. **Consolidation gates (§4.1) with an atomic fenced claim (§6.1 SQL)** —
   *not* the §4.2 shipped lock (its race is documented above). Separate
   `last_success_at` from the lease record; rollback = delete-if-created.
6. **Gather-phase priority (§4.3 Phase 2)** — recent entries → drifted/
   contradicted facts → targeted search only.
7. **Write contract (§6.3)** — mandatory `##` structure, hard caps, and
   NO_REPLY-as-success with watermark advance (not infinite retry).
8. **Confirmations as first-class feedback (§3.4)** — rule → Why → How-to-apply;
   prevents over-cautious drift.
9. **Serialized memory writes (§3.5)** — one per-directory queue covering both
   interactive and background writers; snapshot explicit `[start_uuid, end_uuid]`
   extraction ranges and commit the cursor only after success.
10. **Persisted-output pointers (§2.5)** — full content to disk, preview + path
    in context; never a tombstone.
11. **Absolute-ize dates at write time**, not recall time.
12. **Cache-stable injection (§6.5)** — never mutate the system prefix; reuse the
    prior injection verbatim if present; treat memory text as untrusted quoted
    data.
13. **No full-overwrite consolidation (§6.3)** unless the complete prior state
    was in the model's input; otherwise hierarchical section merges.

### Avoid
- Pure prompt-driven dedup at multi-KB scale (works only with one strong model
  and one project scope)
- Write-then-read-back "locks"; classification fields that drift between write
  paths; blind truncation of model output; unbudgeted recall injection

**Synthesis:** these harnesses *illustrate* (we have no independent effectiveness
measurements) two complementary envelopes — an operational one (when to write,
how much to inject, when to consolidate, how to be crash-safe) and a
retrieval-math one (hybrid + class-aware decay + MMR). A strong memory CLI keeps
its semantic core (supersession, entities, citations) and wraps it in both.
