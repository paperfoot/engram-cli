# Engram CLI v0.4 forward-plan review — gate 4

Review target: the unimplemented forward plan on branch **arch/v0.4**. Required planning and UX documents were read in full, and the claims below were checked against the current source tree, contract crate, migrations, tests, generated skill, and existing binary using only fresh temporary stores.

The review began at commit **aec5ba80e85b16b953f7a76571f5cb89701d6175**. While it was in progress, the shared branch advanced externally to **28cb8856f71df533e8f41ee57bb66629a5edc5af**, which fixes the two reported state defects and adds three tests. I did not reset or modify the repository. Current-source citations use 28cb885; the old state paths are identified explicitly as aec5ba8 evidence.

## 1. Verdicts

| Priority | Verdict | One-line reason |
|---|---|---|
| 1 — invert fact extraction | **FLAWED** | The direction is efficient, but the existing facts-JSON path confuses transport with person authority, remains pending for later LLM backfill, and leaves bare remember, edit, ingest, offline, empty-result, and shell trust semantics undefined. |
| 2 — single-copy wire and compact agent-info | **SOUND-WITH-CHANGES** | The duplicate text is real and the savings are plausible, but the deletion currently removes the only tested untrusted-content boundary and depends on an incomplete, shallow schema registry plus four drifting serializers. |
| 3 — recall abstention | **FLAWED** | There is no common score on which a static floor can operate: offline rank one is fabricated as 1.0, Cohere emits a different quantity, and the receipt reports pre-rerank RRF. |
| 4 — subtraction | **SOUND-WITH-CHANGES** | The architectural simplification and most arithmetic are real, but the proposed migration omits load-bearing fact identity, hot SQL callers, historical journal/rebuild compatibility, orphan lesson twins, and an existing privacy-gate bug. |

Overall: **NO-GO on implementing the plan in its present order; GO on a revised plan after the interface decisions and preconditions below are ratified.** The proposed end state can be substantially leaner and cleaner. The current documents do not yet provide a safe route to it.

## 2. Ranked findings

### High

#### H1 — Priority 1 launders frontier-agent output into person-authoritative evidence

The plan says the caller is already an LLM and proposes making the existing facts-JSON route the normal path ([HANDOFF.md:86](/Users/dev/Projects/engram-cli/docs/arch/HANDOFF.md:86)). That route does not merely mean “structured JSON arrived.” It stamps every supplied fact as **UserStated** ([remember.rs:520](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:520)), while binding ruling G1 reserves UserStated and UserConfirmed for person-supplied evidence and grants those classes explicit interval-closing authority ([interfaces.md:357](/Users/dev/Projects/engram-cli/docs/arch/interfaces.md:357)).

That distinction changes behavior:

- A UserStated immutable-slot disagreement can supersede machine or import evidence as a user correction ([semantics.rs:347](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:347)).
- UserStated and UserConfirmed map to **ExplicitEnd**; other sources receive only heuristic extractor authority ([semantics.rs:970](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:970)).
- The tests deliberately prove both halves: agent-extracted medication cessation is queued rather than closing the prescription, while facts-JSON with state ended closes it immediately ([wave6b_extraction_authority.rs:128](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave6b_extraction_authority.rs:128), [wave6b_extraction_authority.rs:174](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave6b_extraction_authority.rs:174)).

G8–G10 do not protect the source store. Ordinary live events default to own origin ([journal.rs:61](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:61)); imported-origin safety gating is applied later on import/rebuild ([rebuild.rs:173](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:173)). A prompt-injected or mistaken frontier agent could therefore close takes_medication, allergic_to, or diagnosed_with locally, and the local journal would reproduce that close. Export/import can then treat the same event differently, creating source/restored-store divergence.

The required model has separate axes:

1. **Authority:** user_confirmed, agent_extracted, imported_untrusted, or equivalent.
2. **Extraction provenance:** caller agent, OpenRouter, Codex child, Claude child, model/version, schema/prompt version, input content hash.
3. **Processing state and coverage:** complete with facts, complete with none, intentional text-only, deferred, partial, failed.

Default caller-produced facts must be agent-extracted. User authority must require a separate explicit confirmation action that a model cannot self-assert merely by choosing a JSON field.

#### H2 — The proposed default has no complete fact-processing state machine

Caller batches currently carry no extraction marker by design ([remember.rs:504](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:504)). Pending backfill is marker-based, not “has no facts” based ([sqlite.rs:800](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:800)). A memory written with facts-JSON is therefore still eligible for a later paid LLM extraction, and the existing test pins that behavior ([wave05_fact_batch.rs:202](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave05_fact_batch.rs:202)). Making this route the default without changing the ledger re-runs the very extraction the plan claims to eliminate and lets later operator behavior change the fact set.

The bare-call contract is also unresolved. With no key, remember currently chooses SkippedNoKey ([remember.rs:473](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:473)) and still reports success, stored true, facts_added zero ([remember.rs:635](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:635)). Existing help and contract text promise automatic extraction when configuration permits ([cli.rs:63](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:63)). If extraction merely becomes opt-in, bare remember becomes a deterministic version of the same false success rather than a fix.

The response must distinguish:

- facts not supplied;
- an explicit complete assessment yielding no facts;
- facts all deduplicated;
- invalid facts rejected;
- intentional text-only storage;
- deferred extraction;
- provider or validation failure;
- partial success after the memory committed.

Today it cannot. Blank triple fields are silently skipped ([fact_write.rs:655](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:655)); caller confidence and state are permissive strings/defaults ([facts.rs:76](/Users/dev/Projects/engram-cli/crates/engram-graph/src/facts.rs:76)); and an empty JSON array produces no durable “complete and empty” receipt. The fact marker is still described through the legacy fact_llm identity and source assumptions ([journal.rs:23](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:23), [write_path.rs:255](/Users/dev/Projects/engram-cli/crates/engram-storage/src/write_path.rs:255)).

Before changing defaults, add one journalled fact-processing receipt, welded to the accepted fact batch even when the batch is empty, and rebuildable/importable. A safe new write contract is an explicit choice among facts-JSON, extract-facts, text-only, or defer-facts. A bare call should fail before writing with a machine-readable next action. If one compatibility release must accept it, return partial_success plus structured_state_ready false—not ordinary success.

#### H3 — Priority 1 is incomplete unless edit, memory update, failure atomicity, and offline behavior change with it

Content edit is part of the same lifecycle. It closes prior facts, clears the extraction marker, and returns reextraction pending, but accepts no replacement fact input ([edit.rs:227](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/edit.rs:227)). After the inversion, a normal agent correction to memory text would erase current structured state and wait for an opt-in path. Edit and memory update need the same fact modes, authority model, validation, receipt, and replacement tests as remember.

The “no half-store” claim is not currently true. Memory and chunks commit before fact work ([remember.rs:447](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:447)); a later fact failure exits nonzero while explicitly telling the caller that the memory remains stored ([remember.rs:728](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:728)). This can be a valid recoverable design, but it needs partial_success and a retry/idempotency token, or a deliberate atomic refactor. It cannot be marketed as eliminating half-stores.

Offline is also only embeddings-offline today ([cli.rs:104](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:104)). Fact LLM resolution does not receive that flag ([fact_llm.rs:76](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_llm.rs:76)), and full autonomy may still perform paid adjudication from a caller batch ([fact_write.rs:570](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:570)). The accurate present claim is “no extraction-model call by default under assisted/strict autonomy,” not zero cost. Either make offline a global network/process deny—including extraction, adjudication, embeddings, and external children—or rename it to offline-embeddings and stop promising global offline behavior.

Finally, fact-model configuration currently falls back through compiler.extraction_model ([fact_llm.rs:65](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_llm.rs:65)). Priority 4 would delete that configuration home. Move and migrate fact-specific configuration before deleting compiler configuration.

#### H4 — Automatic Codex/Claude fallback over PDFs creates a new execution trust boundary

The optional shell fallback is much more than another extractor. PDF, repository, and recalled text are explicitly untrusted data under the installed agent contract ([skill.md:54](/Users/dev/Projects/engram-cli/crates/engram-cli/assets/skill.md:54)). Feeding a PDF to a general agent CLI that has tools, MCP servers, hooks, project rules, filesystem access, network access, or session state turns document instructions into capabilities.

The inherited environment is dangerous as well. Engram deliberately exports ENGRAM_STORE so child CLI processes inherit the selected store ([lib.rs:555](/Users/dev/Projects/engram-cli/crates/engram-cli/src/lib.rs:555)). A child can also inherit API keys, repository credentials, provider configuration, and the caller's working directory. If its output then enters the current facts-JSON path, this execution risk composes with H1's person-authority escalation.

Do not auto-detect a binary on PATH and invoke it. If this feature survives, require explicit provider selection and all of the following: argv plus stdin rather than a shell string; a fresh empty working directory; tools/MCP/hooks/rules/session persistence disabled; a minimal allow-listed environment with ENGRAM_STORE and secrets removed; strict schema-only output with prose rejected; input/output/fact caps; timeout and process-group cancellation; no raw child stdout/stderr in public errors; version/capability probing; provider/model/version provenance; privacy/spend consent; and agent-extracted authority. If supported CLI versions cannot guarantee those properties, defer the fallback and retain the direct OpenRouter path.

PDF ingest itself also needs a separate design. It currently stores one whole-document Memory and separate bounded chunks ([ingest.rs:156](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:156), [ingest.rs:232](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:232)); it writes no interval facts, and its only semantic derivation is the compile path Priority 4 removes ([ingest.rs:349](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:349)). Extraction must be chunk/section bounded, report per-file coverage and partial failures, and retain page/span provenance. Scientific paper claims must not be poured into a personal takes_medication-style interval model. Decide explicitly whether PDFs become raw-recall-only or receive a distinct cited-claim representation.

#### H5 — Deleting answer_context is not pure deletion because it removes the tested prompt-injection boundary

The duplication exists. Recall renders answer_context from each result and also serializes the same content into results[].content ([recall.rs:172](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:172), [recall.rs:344](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:344)). Research and both daemon endpoints do the same ([research.rs:127](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/research.rs:127), [serve.rs:1762](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/serve.rs:1762), [serve.rs:1912](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/serve.rs:1912)).

The second rendering is differently escaped: the renderer trims boundaries, escapes sequences able to forge or close Engram elements, and prepends the untrusted-content warning ([retrieval.rs:311](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:311)). This behavior was added for a reproduced forged-citation/prompt-injection class and is pinned across CLI and daemon, including forged closing tags, notices, KB attributes, and whitespace ([wave0_untrusted_envelope.rs:123](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave0_untrusted_envelope.rs:123), [wave05_daemon_envelope.rs:245](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave05_daemon_envelope.rs:245)). The changelog and skill teach that contract ([CHANGELOG.md:161](/Users/dev/Projects/engram-cli/CHANGELOG.md:161), [skill.md:54](/Users/dev/Projects/engram-cli/crates/engram-cli/assets/skill.md:54)).

JSON prevents stored text from creating sibling JSON fields, but it does not stop a host from placing content into a prompt and obeying it. Keep content exactly once, but replace the envelope with an explicit, versioned trust marker—at response or typed-content level—and replacement adversarial tests proving malicious text cannot forge trust, receipts, citations, conflicts, or sibling fields on either transport.

#### H6 — Compact agent-info cannot yet rely on per-command schema pulls

The default manifest has 36 command schemas: 30 generated entries plus six hand-maintained ones ([agent_info.rs:95](/Users/dev/Projects/engram-cli/crates/engram-cli/src/agent_info.rs:95)). The schema pull delegates only to the 30-entry contract registry ([agent_info.rs:457](/Users/dev/Projects/engram-cli/crates/engram-cli/src/agent_info.rs:457)); its test explicitly says all thirty ([contract_schema.rs:20](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_schema.rs:20)).

Current pulls fail for retained or potentially retained surfaces including research, ingest, people show, kb create, and config set. The six manifest-only entries are documents delete, ingest, kb repair, people show, repair, and update. Removing the embedded manifest first would make central commands—especially ingest—advertised but not learnable.

The generated schemas are also shallow. The contract type system has only opaque Object and Array<Object> ([model.rs:10](/Users/dev/Projects/engram-cli/crates/engram-contract/src/model.rs:10)), and JSON-schema generation consequently cannot teach exact nested hit, citation, conflict, trust, receipt, or provenance shapes ([jsonschema.rs:91](/Users/dev/Projects/engram-cli/crates/engram-contract/src/jsonschema.rs:91)).

Before compaction:

- mechanically compare every retained clap path with the schema registry;
- require a successful schema pull, or explicitly advertise schema_available false with concise usage;
- add nested contract types and definitions for load-bearing response objects;
- preserve global invariants—trust, error vocabulary, handles, versioning, offline, and risk—in the compact index;
- change the installed skill, which currently tells agents to read the full manifest ([skill.md:12](/Users/dev/Projects/engram-cli/crates/engram-cli/assets/skill.md:12)).

The plan also promises a risk class, while CommandSpec currently has only destructive, requires, and opens_store ([model.rs:95](/Users/dev/Projects/engram-cli/crates/engram-contract/src/model.rs:95)). “Relevant time” is not a current retrieval-hit field either ([retrieval.rs:294](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:294)). Those are useful additions, not subtraction.

#### H7 — Four response serializers already disagree; wire compaction and abstention need one response v2

CLI recall includes handles, provenance, conflict counts, scope, verdict, and conditional absence ([recall.rs:309](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:309)). CLI research has provenance and verification ([research.rs:194](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/research.rs:194)). Daemon research lacks verification, verdict, absence, and scope; daemon recall lacks verdict, receipt, provenance, memory-resolving handle, and retrieval stats ([serve.rs:1757](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/serve.rs:1757), [serve.rs:1907](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/serve.rs:1907)).

Implementing P2 and P3 independently in those JSON literals would guarantee semantic drift. Define one typed hit DTO, one gate result, one trust representation, and one response builder shared by CLI recall and /v1/recall. Either delete research first or make its CLI/daemon pair reuse the same primitives.

The current bindings also favor additive compatibility. R1–R15/D1–D28 include field/handle commitments, and generated output schemas intentionally remain open to additions ([interfaces.md:45](/Users/dev/Projects/engram-cli/docs/arch/interfaces.md:45), [jsonschema.rs:1](/Users/dev/Projects/engram-cli/crates/engram-contract/src/jsonschema.rs:1)). Default output envelope and contract versions are still hard-coded at version 1 ([output.rs:326](/Users/dev/Projects/engram-cli/crates/engram-cli/src/output.rs:326)). This is a declared response-contract v2, not an unversioned cleanup. Ship P2 and P3's wire changes together, with one explicit v1 compatibility mode for a bounded window rather than two breaks or dual-sending both payloads.

#### H8 — A static relevance floor has no valid common input score

The reported bench result—abstention_rate 0.0 and negative controls at top_score 1.0—does not locate a threshold. It proves that the tested offline score is not confidence.

- Cohere emits its provider relevance score ([cohere.rs:192](/Users/dev/Projects/engram-cli/crates/engram-rerank/src/cohere.rs:192)).
- The passthrough reranker fabricates 1.0 minus rank divided by count, making rank one exactly 1.0 regardless of relevance ([passthrough.rs:14](/Users/dev/Projects/engram-cli/crates/engram-rerank/src/passthrough.rs:14)).
- Hybrid recall copies that number into the public score ([retrieval.rs:999](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:999)).
- The personal bench then calls the first result's value top_score, defines abstention only as an empty result list, and excludes negatives from headline recall ([personal.rs:1407](/Users/dev/Projects/engram-cli/crates/engram-bench/src/personal.rs:1407), [personal.rs:1491](/Users/dev/Projects/engram-cli/crates/engram-bench/src/personal.rs:1491)).

No static floor on that value works: a floor at or below 1 admits every weak rank-one result; a floor above 1 rejects every offline result. The proposed receipt makes matters worse because RecallStats.top_score is pre-rerank fused RRF ([retrieval.rs:960](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:960)), while a cloud gate would likely use post-rerank Cohere. RRF itself sums rank-only votes ([fusion.rs:19](/Users/dev/Projects/engram-cli/crates/engram-core/src/fusion.rs:19)); its range changes with query mode and the number of active dense, lexical, claim, wiki, and graph runs.

Required sequence:

1. Carry separately named dense similarity, lexical score/coverage, RRF score, rerank score, and score_kind.
2. Make passthrough preserve order without claiming relevance.
3. Report the exact score used by the gate, with provider/model/profile/mode/result-kind and calibration ID.
4. Calibrate only within a frozen retrieval regime. Offline needs a real local relevance model, a deliberately conservative lexical gate, or abstention_capability unavailable.
5. Recalibrate after Priority 4 removes candidate sources; a threshold chosen before that cut is immediately stale.

#### H9 — Search abstention, state projection negatives, and decision currency are three different claims

The current Absence type is shared by semantic recall, fact and lesson listings, and state ([read_surface.rs:303](/Users/dev/Projects/engram-cli/crates/engram-cli/src/read_surface.rs:303), [facts.rs:145](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/facts.rs:145), [state.rs:782](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:782)). A global rename to search receipt would mislabel exhaustive projections and enumerations.

Use distinct types:

- **search_receipt:** ranked retrieval attempted sources, backend success/failure, candidates, gate, score kind, threshold, calibration;
- **scope:** KB/diary/window traversed;
- **projection_receipt or negative_evidence:** exact fact query, subject resolution, relation, valid-time, open/historic/contested counts, projection watermark, extraction coverage;
- **verdict:** the mechanism-specific outcome.

The backend detail matters because lexical, claim, and wiki SQL failures are swallowed as empty runs with tracing warnings ([retrieval.rs:815](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:815), [retrieval.rs:847](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:847), [retrieval.rs:894](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:894)). Existing RecallStats reports only chunks, candidates, top fused score, and KBs ([read_surface.rs:257](/Users/dev/Projects/engram-cli/crates/engram-cli/src/read_surface.rs:257)). A receipt cannot certify hybrid search when a component silently failed.

The interval model can prove only **no open recorded assertion for canonical subject X and relation R in scope S at valid time T**. It cannot prove a real-world negative, particularly when P1 permits intentional text-only/deferred/partial extraction. State's as-of filtering is valid-time containment ([state.rs:568](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:568)), not reconstruction of what Engram believed at a historical transaction time.

Recall no_match is narrower still: “no passage cleared this calibrated gate.” Facts are not a retrieval source ([types.rs:297](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:297)); superseded or rejected raw chunks can remain highly relevant. The conflict sideband scans at most 20 facts and matches only subject/object terms ([recall.rs:439](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:439), [sqlite.rs:4409](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4409)). Abstention therefore does not make recall a current-truth answer and must not hide conflict/decision evidence. Route current/as-of questions to state or annotate epistemic status.

#### H10 — Removing entities breaks caller facts and state before it simplifies anything

The entity graph is not only compile output. Caller fact writes canonicalize subject and object through entity aliases ([fact_write.rs:293](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:293)); state mirrors that resolution ([state.rs:314](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:314)); and storage lookup joins the entity alias tables ([sqlite.rs:4585](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4585)).

A fresh-schema proof with those tables removed showed asymmetric failure: remember with facts could appear to succeed because alias SQL errors are swallowed/fallback-normalized, while state propagated a missing-table error. That is precisely the composing-fixes failure pattern the prior gates exposed.

Before deleting entities/entity_aliases, define one fact-owned identity policy used identically by writes, state reads, imports, and rebuild. Options include strict normalized caller identity with no aliasing or a narrow provenance-bearing fact alias projection. Do not copy machine/compiler aliases into a trusted identity table without applying the G8–G10 import-trust lens: derived text must not silently rewrite user subject identity.

The people decision must also be made first. HANDOFF leaves a thin fact-backed people view undecided ([HANDOFF.md:125](/Users/dev/Projects/engram-cli/docs/arch/HANDOFF.md:125)), while the line/read-verb savings count the existing surface as removed.

#### H11 — Priority 4 is not executable until every hot SQL and historical protocol dependency is retired

The proposed table list has live callers outside the files counted for deletion:

- soft-delete/forget and edit clean claims, evidence, and spans ([sqlite.rs:2073](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2073));
- document deletion cleans derived rows ([sqlite.rs:2857](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2857));
- KB clear, doctor/integrity, and repair query the same tables ([sqlite.rs:3134](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:3134));
- generic handle resolution probes entities, lessons, and wiki before other handle kinds, so a missing table can break unrelated handles ([handles.rs:26](/Users/dev/Projects/engram-cli/crates/engram-cli/src/handles.rs:26)).

The schema upgrade chain must also remain capable of opening old stores. Migrations run sequentially through v13 ([sqlite.rs:1015](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1015)); early versions create and later transform the tables now slated for removal. Add a v14+ destructive migration after the historical chain. Only a future baseline rewrite may remove the create-then-drop work.

Persisted event kinds are a historical wire protocol. Compile and lesson events remain parseable enums ([events.rs:15](/Users/dev/Projects/engram-cli/crates/engram-core/src/events.rs:15)); unknown kinds are fatal ([journal.rs:259](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:259)); rebuild currently executes lesson SQL and clears/recounts the projection ([rebuild.rs:510](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:510), [rebuild.rs:706](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:706)). Retain old parsing indefinitely and replay retired projection events as explicit counted no-ops. Export v3 intentionally omits derived artifacts ([export_v3.rs:1](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:1)); provide an optional human-readable artifact dump before irreversible deletion.

Use a two-step rollout: first a quiescence binary that removes all SQL callers, provides legacy-handle retirement behavior, inventories residue, and can rehearse safely; then the destructive migration with backup, child/virtual/parent drop order, foreign-key/integrity/object/row checks, and old-store fixtures.

#### H12 — Priority 1 makes an existing cross-diary privacy bug more reachable

MEMORY_CONSTRAINS_DERIVED treats any completed job newer than a deletion as evidence that deleted memory has been compiled out; the subquery has no job-kind predicate ([sqlite.rs:830](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:830)). But compile_jobs evolved into a generic jobs table with a kind column ([jobs.rs:11](/Users/dev/Projects/engram-cli/crates/engram-storage/src/jobs.rs:11)).

A completed fact-backfill job can therefore falsely certify that stale compiler output no longer contains forgotten diary content and reopen cross-diary derived results. P1 increases backfill activity; P4 eventually deletes the affected derived layer. Fix the bug **before either** by requiring COALESCE(kind, compile) = compile and add a non-compile-job regression. Delete the certificate only after no derived exposure path remains.

This defect is missing from the forward plan and is a release-order blocker independent of the larger architecture.

#### H13 — The lesson deletion recipe misses reproducible orphan recall twins

The plan closes memories referenced by lessons.memory_id ([subtraction-plan.md:121](/Users/dev/Projects/engram-cli/docs/arch/subtraction-plan.md:121)). Lessons add, however, commits a new recallable twin before upserting the lesson ([lessons.rs:300](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/lessons.rs:300)). On duplicate upsert, storage preserves the prior non-null memory_id ([lessons.rs:153](/Users/dev/Projects/engram-cli/crates/engram-storage/src/lessons.rs:153)), leaving the just-created twin active and unreferenced.

A fresh-store reproduction of two identical adds produced one lesson row, two active procedural/lesson-tagged memories, and only one referenced twin. The migration needs a dry-run inventory of referenced and provable orphan twins, with ambiguous rows reported rather than guessed. Close them through normal journalled soft delete **before** dropping claims/source spans, because the current delete path still cleans derived rows.

#### H14 — Both state root causes are confirmed, but bug (a)'s stated response shape was impossible and the fixes are not purely local

At aec5ba8, state sorted facts, recorded total, and sliced them by offset/limit before constructing relation groups (aec5ba8: crates/engram-cli/src/commands/state.rs:596–637, then 659–704). The root cause is confirmed: pagination over facts can hide an entire relation.

The exact claim “two facts, returned one, groups empty” cannot be produced by that loop. If one fact is returned, the loop necessarily appends it to a group or creates one. A valid reproduction needs at least two relations—for example a safety-prioritized allergy on page one and medication facts after it. The response then shows the allergy group and silently omits medication. Correct the plan's fixture so the regression test proves the actual defect.

Current 28cb885 fixes this by building group skeletons and true open_count values from the full current set before applying pagination to values ([state.rs:638](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:638), [state.rs:699](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:699)). Empty values with open_count greater than zero now means “present beyond this page”; P2 must retain open_count or compaction recreates ambiguity.

Bug (b)'s diagnosis is exact. At aec5ba8 subject resolution occurred before relation filtering, but an empty filtered result unconditionally emitted subject_not_found and nearest subjects, including the exact subject at 1.0 (aec5ba8: state.rs:308–356, 545–571, 742–762). Current HEAD carries subject_known through the branch and emits no_open_values without the self-nearest list ([state.rs:765](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:765)).

The implementation is locally understandable, but the release change was not “just two lines”: it touched state behavior, response vocabulary, contract/skill material, and tests. no_open_values also combines “none for this relation,” “none in scope,” “none at as-of,” and “only closed,” distinguished only by request context. P3 must not promote it to an unscoped negative.

### Medium

#### M1 — Caller and LLM facts share the contradiction engine, but not parity of authority, processing, or lifecycle

The positive premise is real. Both paths ultimately call the same FactWriteCtx/write_fact_batch flow ([remember.rs:705](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:705), [fact_write.rs:581](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:581)). They therefore share canonicalization, interval planning, exact/semantic deduplication, contradiction classification, decision replay, queueing, and persistence. The fact batch and derivation marker can be welded into one transaction, and rebuild tests cover productive and zero-fact extraction ([wave6b_derivation_weld.rs:73](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave6b_derivation_weld.rs:73)).

The plan overstates this as behavioral parity. Source authority differs (H1), caller processing remains pending (H2), edits have no replacement route (H3), and full-autonomy adjudication may still invoke a model. Preserve the shared writer; repair provenance and lifecycle around it rather than rewriting the contradiction core.

#### M2 — The wire token reductions are plausible, but the causal claim is overstated

The existing binary's pretty agent-info was approximately 144.2 KB in an isolated probe, close to the plan's 143,723-byte/37,422-token baseline. Top-level help plus one recall schema was approximately 2,315 reference tokens. That validates the order of magnitude, not the proposed capability-index design: the latter is a separate reported 412-token prototype, and the installed skill itself is about 5.7k tokens. State exactly which bundle a cold agent loads.

For recall, answer_context is genuinely duplicate content, but the independent economics fixture attributes only about 1,565 of 5,531 tokens—roughly 28%—to that copy ([review-angle-economics.md:21](/Users/dev/Projects/engram-cli/docs/arch/review-angle-economics.md:21)). The claimed 5,774 to 1,192 reduction is plausible only because the prototype also deletes redundant IDs, full provenance, null fields, provider diagnostics, scores, sources, and metadata ([subtraction-plan.md:13](/Users/dev/Projects/engram-cli/docs/arch/subtraction-plan.md:13)). HANDOFF should not attribute the full 79.4% saving merely to “send each result once.”

Commit reproducible fixtures and measure the **final serialized response**, not content only. Recall's current budget counts result content and deliberately guarantees top_k before enforcing it ([recall.rs:130](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:130)); it excludes JSON, trust, conflicts, receipts, citations, and metadata. Without a serialized-byte/token budget plus deterministic truncation/continuation, P3 fields will gradually consume the saving again.

#### M3 — Complete in-tree consumer inventory for the proposed wire deletions

External clients cannot be enumerated, which is why response v2 and a bounded compatibility window are required. In-tree consumers/producers are:

| Field family | Runtime, contract, test, benchmark, and documentation consumers | Load-bearing disposition |
|---|---|---|
| answer_context | Producers: recall.rs, research.rs, and both serve.rs handlers; renderer/security logic: retrieval.rs; contract/codegen: registry.rs, engram-contract agent_info.rs/runtime.rs, CLI agent_info.rs; generated skill; tests wave0_untrusted_envelope, wave05_daemon_envelope, daemon_recall_conflicts; CHANGELOG and architecture/spec/refinement docs. | Delete duplicate text; replace trust semantics and adversarial tests. No benchmark reads this wire field. |
| results content | Four recall/research CLI/daemon emitters; human recall renderer; CLI/daemon parity and envelope tests; contract and skill. | Keep exactly once as canonical untrusted content. |
| id, memory_id, handle | Recall/research emitters; typed-handle, provenance, and contract-surface tests; skill/docs teach round-tripping. | Keep one typed hit handle; remove row/memory IDs only where handle and citation retain required addressing. |
| provenance | Recall/research producers; read_surface type; manifest/skill; contract_surface, state_view, and wave3_read_provenance tests; other read surfaces have their own provenance. | Compact default is reasonable, but retain time/citation/KB/trust/handle and provide an exact audit/expand path. |
| score and rerank_score | Four emitters; human recall; HybridResult; personal bench mapping and reports. | Remove misleading default score only after P3 adds typed internal score provenance; benchmark still needs truthful scores. |
| sources and citations | Four emitters; research readiness reads internal citations; daemon tests; scientific benchmark/docs. | Keep compact citation plus source/trust class; move full arrays to audit. |
| inner and outer metadata | Four serializers; provider-parity, daemon-parity, offline-write, and paid-accounting tests. | Ordinary output may drop most; retain profile/score kind/degradation/scope when semantically material. |
| verdict and absence | output/read_surface; recall, research, facts, lessons, state; contract vocabularies; manifest/skill; absence, state, and paging tests. | Replace coherently with verdict plus typed search/scope/projection receipts; do not blind-rename. |
| embedded command_schemas | hand manifest/generated merge; contract registry/model/jsonschema/agent-info; manifest/schema/discovery/skill tests; generated skill. | Delete default embedding only after complete retained-command pull coverage and a compact global-invariants index. |

Citation grounding and ready_for_synthesis do **not** read answer_context. Research counts results with non-empty citations and sets ready_for_synthesis when cited is greater than zero ([research.rs:122](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/research.rs:122), [research.rs:212](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/research.rs:212)). So duplicate deletion does not break that code—but the code currently proves addressability, not relevance or support. If research survives, readiness must count post-gate relevant cited evidence. If P4 deletes it, do not build a new durable research subsystem immediately beforehand.

#### M4 — Priority 3 needs a calibration program, not a guessed constant

A static threshold can be valid inside one frozen regime—for example one Cohere model, cloud-quality profile, evidence mode, chunk result kind, and final source vocabulary. It is not valid across Cohere/passthrough, evidence/raw/wiki/explore/agent modes, raw and derived result kinds, different RRF source sets, or pre/post-P4 builds.

Before default-on abstention, preserve a small benchmark independent of the large bench crate and add:

- positive retention and false-abstention by query class;
- negative precision/recall and AUROC/AUPRC;
- calibration error if a confidence is exposed;
- exact, lexical, paraphrase, indirect-reference, correction, superseded/rejected, current/as-of, and adversarial cases;
- 1k/10k/100k distractor growth and multiple KB/diary scopes;
- every supported provider/model/profile/mode;
- final post-subtraction source vocabulary.

The six small personal negative controls are a smoke test, not calibration. Their 1.0 scores diagnose fabricated ranking, not threshold placement.

#### M5 — The subtraction arithmetic is mostly reproducible, but its headline mixes phases and definitions

At the reviewed pre-state-fix snapshot, physical Rust lines under crates/*/src totaled 72,409; the current tree totals 72,452. Whole-file counts reproduce:

| Cut | Verified physical lines | Assessment |
|---|---:|---|
| compile/research/wiki/graph/entities/people plus graph/extract/people/hygiene/gazetteer engine files | 8,935 | Exact. |
| lessons/reflect plus lesson storage | 1,877 | Exact. |
| engram-bench source plus CLI bench dispatcher | 11,662 | Exact. |
| Total | 22,474 | Exact, 31.0% of 72,409—but only by including the benchmark phase. |

Thus the advertised approximately 22,500-line cut is real as a whole-program program, but derived plus lessons alone are 10,812 lines. HANDOFF Priority 4 does not mention bench deletion. These are physical lines including comments, blanks, and embedded unit tests, not 22,474 lines of runtime production logic.

A fresh v13 schema has 28 logical tables and 294 logical columns when sqlite_sequence and FTS implementation shadows are excluded. The four dead tables contain 25 columns; six memory columns make the 31-column claim correct. But eight compiler tables plus four dead tables plus lessons equals **13** logical tables removed, not 12, taking 28 to 15. Current knowledge-facing read surfaces are at least recall, state, research, wiki, graph, entities, and people—seven, not six. If thin people survives, the end state is three read verbs, not two.

These are documentation/accounting corrections, not arguments against subtraction.

#### M6 — The six memory columns are a domain and export compatibility change

The fields remain on the public serialized Memory type ([types.rs:15](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:15)), in MemoryExportRow without defaults ([sqlite.rs:284](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:284)), and in export/import SQL ([sqlite.rs:5388](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5388), [sqlite.rs:5527](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5527)). A new reader may ignore old extra fields; an old v0.4 reader cannot deserialize a new export that omits required fields. The plan states compatibility in the wrong direction. Retain deprecated zero-valued wire fields for one window or bump/negotiate the export format and document old-binary incompatibility.

The journal rationale is also false. memory.written stores content_hash, importance, tags, event time, and source—not raw content ([journal.rs:444](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:444)). Rebuild treats memory events as provenance and does not reconstruct memories from them ([rebuild.rs:74](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:74)). Removing access telemetry does not make raw memories journal-rebuildable. Correct HANDOFF's claim that memory content survives in memory.written ([HANDOFF.md:134](/Users/dev/Projects/engram-cli/docs/arch/HANDOFF.md:134)); facts may still place values in other events, so deletion/privacy language also needs precision.

#### M7 — The entity-index diagnosis is real but overstated

Migration v1 creates idx_entities_kind; v2 renames entities to entities_v1_global and then runs CREATE INDEX IF NOT EXISTS with the same name on the new live table ([sqlite.rs:521](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:521), [sqlite.rs:1323](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1323)). SQLite carries the named index to the renamed table, so the second create is skipped. The live entities table consequently lacks its kind index, and kind-only listing scans.

It is not wholly unindexed: KB/name and automatic uniqueness/primary indexes remain. Say “missing the kind index.” If P4 is delayed, repair it immediately rather than waiting for deletion.

#### M8 — The four UX runs reveal work not covered by the four priorities

The plan directly addresses the bare-remember false success, command-surface weight, weak recall matches, and much of derived-surface confusion. It does not close:

- correction/preference/current-state editing as a single agent workflow;
- canonical relation discovery and aliases—agents still guess medication versus takes_medication;
- automatic capture/handoff from agent-era findings;
- a decided fact-backed people/relationship surface;
- forget responses that avoid presenting merely related recall hits as proof of deletion;
- PDF/page-level grounding after compile/claims removal;
- current-truth routing when raw recalled text is stale;
- output budgeting on state/facts and not only recall;
- large-store lifecycle, compaction/sync, or the near-linear dense scan retained after subtraction.

These need not all block v0.5, but the plan must explicitly defer them rather than implying that P1–P4 exhaust the usability findings.

### Low

#### L1 — Compact provenance needs a concrete recovery route

Full provenance is expensive, but contract tests currently require it ([contract_surface.rs:444](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_surface.rs:444)). Before deleting it from default output, specify recall --detail audit, recall --expand with a typed hit handle, or a stable result-detail command. “Move it to debug” is not an agent-usable contract.

#### L2 — Contract-break detection is curated rather than a complete compatibility diff

The breaking-contract test validates a curated list rather than mechanically comparing the whole released response/command surface ([contract_breaking.rs:35](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_breaking.rs:35)). P1 changes command behavior, P2 removes fields, and P3 adds verdict/receipt semantics. Add a release-fixture comparison against v0.4/main so omissions are visible rather than dependent on remembering to update a list.

#### L3 — Release documentation already has a schema-version inconsistency

The changelog header still describes schema v12 while later entries introduce v13 ([CHANGELOG.md:8](/Users/dev/Projects/engram-cli/CHANGELOG.md:8), [CHANGELOG.md:41](/Users/dev/Projects/engram-cli/CHANGELOG.md:41)). Fix this before documenting a v14 destructive migration.

## 3. Missed details — what the plan does not know it does not know

1. **Transport is not authority.** The plan assumes facts-JSON is neutral plumbing; it is currently an explicit person-authority escalation.
2. **Fact completeness is durable state.** Zero facts, no facts supplied, partial extraction, deferred extraction, and text-only intent need journalled, rebuildable distinctions before any negative can be proved.
3. **Content edit is a structured-state replacement.** Inversion limited to remember silently strands corrected memories.
4. **External-agent execution is an import-like trust boundary plus a process sandbox.** G8–G10 cover journal import, not a live child with inherited store/secrets/tools.
5. **PDF claims and personal interval facts are different products.** Removing compile does not make a generic triple extractor a cited research replacement.
6. **Ingest provenance currently cannot support trustworthy fact attribution.** Whole-document memories plus chunked retrieval need per-page/span extraction and deterministic merge; selecting a first chunk is not enough.
7. **The trust warning lives inside the duplicate slated for deletion.** There is no independent response-level quarantine marker today.
8. **Lazy schema coverage and depth are incomplete.** The proposed compact index cannot yet teach every retained command or nested response.
9. **There is no common response builder.** CLI and daemon will disagree on no_match unless unified before compaction.
10. **The current public score is sometimes a rank ramp.** The bench's 1.0 negatives are not poor calibration; they are proof that calibration input is invalid.
11. **Search backend failure is invisible.** A receipt needs attempted/succeeded/failed per source and freshness/watermark information.
12. **Search absence and projection absence are different epistemic objects.** A single renamed Absence type will overclaim one or misdescribe the other.
13. **Projection negatives depend on P1 coverage.** Intentional text-only or partial extraction must weaken the claim to “no open recorded fact.”
14. **Valid-time as-of is not transaction-time belief replay.**
15. **Fact identity currently depends on a supposedly removable compiler entity graph.**
16. **Generic jobs can prematurely clear the cross-diary derived privacy gate.**
17. **Duplicate lesson adds leave active orphan recall twins outside the proposed migration set.**
18. **Hot mutators, admin/repair, and generic handle routing query removed tables.**
19. **Old migrations and journal event names cannot be deleted with their current projections.**
20. **Export v3 cannot preserve derived artifacts, and old v0.4 readers cannot read exports that omit required memory fields.**
21. **The final output needs a serialized budget.** Content-only budgeting will let receipts and metadata recreate the bloat.
22. **Calibration evidence is scheduled for deletion.** Extract the small negative/growth/safety harness before deleting engram-bench.
23. **The exact state paging fixture in HANDOFF is impossible even though the mechanism is real.**
24. **People is still undecided.** Removal counts and the “two read verbs” claim cannot be final until that product choice is made.
25. **Raw-store restore trust remains a declared boundary assumption.** Table deletion must not imply that G8–G10 make an attacker-controlled SQLite restore trusted or safe.

## 4. Composition risks

| Change × change | Composed failure | Required control |
|---|---|---|
| P1 caller facts × G1 | Frontier model output becomes person-authoritative and closes safety intervals. | Separate transport/provider from authority; caller defaults to agent-extracted; explicit human confirmation only. |
| P1 caller facts × G8–G10 | Own-origin local close survives rebuild but may be rejected after export/import, diverging stores. | Persist authority/origin semantics consistently and add caller-close round-trip tests. |
| P1 facts × current marker/backfill | Default caller work remains pending and is later re-extracted at cost, adding facts/contradictions. | Journal a complete fact-processing receipt, including complete-empty. |
| P1 remember × edit | New writes are structured by default, but corrections close facts and leave replacement pending. | One fact-mode/authority/receipt contract across remember and edit. |
| P1 shell child × untrusted PDF | Document prompt injection gains child tools, network, filesystem, sessions, secrets, or store access. | Explicit sandboxed provider with scrubbed environment and strict output, or defer. |
| P1 text-only/partial × P3 provable negative | Incomplete projection is reported as absence. | Coverage-aware wording and projection receipt; never claim real-world absence. |
| P1 configuration × P4 compiler deletion | Fact model fallback references a removed compiler setting. | Move/migrate fact extraction configuration first. |
| P1 facts/state × P4 entity deletion | Writes silently raw-normalize while reads fail or resolve differently. | One fact-owned identity API and policy before table removal. |
| P1 backfill × generic jobs privacy gate | Non-compile completion exposes stale cross-diary derived data. | Kind-filter completed jobs before P1. |
| P2 answer_context deletion × G10-style content trust | Only tested untrusted-text boundary disappears. | Versioned structured trust marker and replacement adversarial tests. |
| P2 wire deletion × P3 new fields | Scores/absence are deleted then re-added under new names; two breaks and drift. | Design and ship one response v2. |
| P2 compact index × incomplete schemas | Agent discovers a command whose schema pull fails or hides nested semantics. | Full retained-command registry coverage and nested definitions first. |
| P2 compact state × landed paging fix | values empty becomes indistinguishable from relation absent if open_count is pruned. | Retain open_count and page semantics. |
| P3 abstention × decision replay/current state | Relevant retired or rejected raw text clears the gate and is mistaken for current truth. | Narrow no_match to passage relevance; route/annotate current state. |
| P3 calibration × P4 source deletion | Claims/wiki/entity/graph removal changes RRF votes and score distribution. | Calibrate default only on final candidate vocabulary. |
| P3 calibration × bench deletion | Evidence harness disappears before threshold selection. | Extract versioned integration fixture first. |
| P3 ready_for_synthesis × P4 research deletion | Work is spent hardening a surface scheduled for removal. | Decide deletion first; otherwise implement only shared primitives. |
| P3 no_open_values × state scopes | Relation/scope/as-of/closed-only miss becomes an unscoped negative. | Mechanism-specific projection receipt with exact query context. |
| P4 lesson deletion × orphan twins | Referenced twins close but duplicate-created active memories survive. | Pre-drop inventory and journalled closure of provable orphans. |
| P4 table drop × forget/edit/docs/admin/handles | Missing-table failures appear in unrelated live commands. | Quiescence release removing all SQL callers before DDL. |
| P4 event/table deletion × journal/rebuild/import | Old event parsing fails or replay runs SQL against missing projections. | Parse legacy kinds forever; counted no-op retired projections; old fixtures. |
| P4 memory-column deletion × P2 response/export changes | Overlapping wire breaks obscure old-agent/import failures. | Separate response and export versions with explicit compatibility tests. |
| P4 table drop × G8–G10 replay | Broad rebuild cleanup accidentally bypasses surviving fact/adjudication trust gates. | Retire only compile/lesson projection arms; preserve and rerun all forged-import/decision tests. |

The synergies are therefore conditional:

- **P1 + P4 is positive only after fact identity and processing receipts are independent of compiler/entity infrastructure.**
- **P2 + P3 is strongly positive if shipped as one typed response v2; it is negative if implemented as sequential JSON edits.**
- **P3 + P4 is negative in the proposed order because subtraction invalidates calibration.**
- **P1 + P3 is unsafe unless extraction coverage constrains negative claims.**

## 5. Overall recommendation

### Revised proceed order

1. **Freeze and document current baseline.** Adopt 28cb885's state fix, correct the impossible paging example, and pin multi-relation paging, open_count, known-subject/no-relation, scope, as-of, and compact-contract fixtures.
2. **Fix the immediate independent defects.** Add the job-kind predicate to the derived privacy gate. Recreate the missing live entity kind index if P4 will not land immediately.
3. **Ratify interfaces before implementation.** Add binding rulings for authority versus extraction provider; fact-processing state/coverage; bare remember and explicit empty semantics; edit replacement; offline; fact identity after entities; response v2; typed search/projection receipts; retained capability/people decision; and export compatibility.
4. **Build the fact-processing ledger first.** Journal and rebuild/import complete-with-facts, complete-empty, text-only, deferred, partial, and failed states. Validate strict FactInput values and report supplied/accepted/rejected/deduplicated counts. Include provider/model/schema/content hash without granting person authority.
5. **Implement P1 on that substrate.** Default caller facts to agent-extracted; make fact mode explicit; add edit parity; expose honest partial success; define global offline behavior. Keep OpenRouter opt-in. Defer Codex/Claude shell fallback unless its sandbox acceptance criteria are demonstrably supported. Decide PDF raw-versus-cited-claim semantics before promising ingest facts.
6. **Complete discovery and response plumbing.** Register every retained command, add nested schemas, introduce one shared CLI/daemon response DTO, and add truthful score/backend provenance with abstention default-off.
7. **Design and ship P2 plus P3's wire contract together.** Response v2 should contain one untrusted content copy, one typed handle, compact citation/KB/time if implemented, trust marker, conflicts, scope, verdict, typed receipt, score kind, and audit expansion. Apply a final serialized-output budget. Keep explicit v1 output for at most one release.
8. **Preserve calibration evidence before subtraction.** Extract a small versioned positive/negative/correction/growth/safety harness from engram-bench.
9. **Land a P4 quiescence release.** Remove all production SQL references to retired tables; replace entity-dependent fact identity; preserve legacy event/handle vocabulary; add derived-artifact export; inventory and close lesson twins; migrate fact configuration; rehearse upgrades and real-store copies without destructive DDL.
10. **Then run v14+ destructive migration.** Take a VACUUM INTO-style backup; close provable twins through journalled paths; drop FTS virtual/shadow, child, and parent objects in order; apply the memory/export compatibility decision; run foreign-key, integrity, object, row-count, reopen, import, and rebuild checks for v1/v4/v9/v13 fixtures.
11. **Calibrate and enable P3 last.** Use the final retained source set and regime-specific scores. If offline lacks a meaningful relevance signal, say abstention unavailable rather than fabricate confidence. Projection negatives remain separate and coverage-aware.
12. **Delete or finish research deliberately.** If removed, do not polish ready_for_synthesis first. If retained, make it require post-gate relevant cited evidence with CLI/daemon parity.

### Preconditions before coding begins

- A binding authority/provider/processing/coverage model exists.
- Bare remember, explicit empty facts, edit, offline, and partial failure have exact response semantics.
- Shell fallback is either removed from the phase or has a written sandbox/threat model.
- The retained command list and people decision are final.
- Response v2 and its trust/receipt/audit/version strategy are ratified.
- P3 calibration inputs and post-P4 timing are agreed.
- Fact identity no longer depends implicitly on removable entity tables.
- The generic-job privacy bug has a regression and fix.
- P4 has a complete SQL caller/event/export/handle/lesson-twin retirement inventory.
- Migration proof includes old stores, old journals, old exports, and all G8–G10 trust tests—not only a fresh schema.

### Final gate

Proceed with the **architectural intent**, not the current document sequence.

Priority 2's default-wire reduction and Priority 4's removal of low-value derived machinery can produce a genuinely leaner system. Priority 1 can remove a redundant model pass. Priority 3 can stop weak matches from masquerading as evidence. But those gains are only real after the plan stops treating caller transport as human trust, stops treating rank as confidence, stops treating projection absence as world absence, and stops treating table deletion as isolated from identity, mutators, replay, and export.

The highest-value revised sequence is:

**trust and lifecycle contracts → fact ledger and P1 → complete schema/shared DTO/score provenance → one P2/P3 response v2 → P4 quiescence and migration → post-subtraction calibration and default abstention.**

Until those preconditions are written into the forward plan and binding interfaces, this gate remains **NO-GO**.
