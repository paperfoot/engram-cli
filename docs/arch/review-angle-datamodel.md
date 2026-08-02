Reviewed clean `arch/v0.4` HEAD `dc3c758`, schema v12. These are representational findings only; I did not revisit the excluded correctness gates.

## Ranked findings

1. **Critical: engram records provenance of a row, not provenance of a belief.**

   The current `Fact` combines three different things: a proposition, one assertion of it, and engram’s current belief about it. It has one `source_memory_id`, one scalar confidence, one coarse `SourceKind`, and a confirmation counter. The journal preserves the same collapsed shape; a confirmation event contains only the fact ID. [Fact and SourceKind fields](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:151), [fact.observed payload](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:429), [fact.confirmed payload](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:501)

   `SourceKind` is a useful start, but it describes how the structured row arrived—not who originally asserted the claim, whether they witnessed it, whether it was quoted, or whether the source is competent in this domain. A Lancet paper and an anonymous forum post are both `AgentExtracted`; “Alice says Dr Lee suspects Ana may have MS” can become indistinguishable from a diagnosis. The extractor simply drops hedged claims, and there is no polarity or modality field for “denied,” “rumoured,” “possible,” or “ruled out.” [extraction skip rule](/Users/dev/Projects/engram-cli/crates/engram-graph/src/facts.rs:46)

   Six months later, copied or correlated sources will inflate `confirmations`, while the store cannot identify independent evidence or retract one source without treating the whole belief as source-bound.

   **Instead:** separate immutable `propositions` from many `assertions`. Each assertion should carry source span, asserted-by, reported-by, extraction agent/model/version, stance/polarity, modality, directness, confidence, and contextual source authority. Confirmation becomes another evidence row, not an integer. The current `facts` table should become a rebuildable belief/current-state projection over those assertions.

2. **Critical: entity identity is disconnected from facts and cannot represent ambiguity or reversible merges.**

   Entities have stable IDs, but facts still use subject and object strings; there is no `subject_entity_id` or typed entity-valued object. [Entity shape](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:68), [Fact subject/object shape](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:151)

   Meanwhile, an alias is unique within a KB, so one spelling cannot legitimately refer to two entities. Entity merging transfers aliases and graph relations, deletes the losing row, and does not rewrite or redirect facts. [alias schema](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:678), [merge implementation](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:3641)

   Scenario: a KB contains Maria Marenova and Maria Moreno. An earlier mention of “Maria” is genuinely ambiguous; the schema must either invent a third Maria or assign the alias to exactly one woman. If later evidence resolves the mention, there is no mention-level record to relink. Likewise, when Jane Smith becomes Jane Doe, graph aliases may converge while old and new fact rows retain different `subject_norm`s and never become one history.

   **Instead:** facts should reference entity IDs or unresolved mention IDs. Add provenance-bearing `entity_mentions` and many-to-many alias claims with temporal validity and candidate probabilities. Model merge as a reversible `same_as`/canonicalization projection with redirects and lineage; never destroy the losing identity. Names themselves should be temporal assertions. KB membership should scope evidence, not define the underlying person anew.

3. **High: the flat string triple is forcing `member_identity` to solve ontology and relationship modelling with token deletion.**

   Values are only `Date` or `Text`, and `member_identity` generically removes parentheses, clauses, numbers, units, frequencies, routes, stages, and forms from every set-valued relation. [ValueDomain](/Users/dev/Projects/engram-cli/crates/engram-core/src/relations.rs:58), [member_identity](/Users/dev/Projects/engram-cli/crates/engram-core/src/relations.rs:206)

   Its real failure distribution is structural:

   - `Glucophage XR` versus `metformin extended-release` under-merges.
   - `Augmentin` versus `amoxicillin/clavulanate` cannot express product/components.
   - `thyroid cancer (papillary)` and `(follicular)` over-merge after parentheses are removed.
   - `BRCA1: c.68_69delAG` is truncated at the colon.
   - Non-English dose and frequency language mostly survives as identity.
   - A cessation stated using a brand may fail to match the open generic-drug interval.

   Under-merging is not harmless: an agent may double-count a medication or leave a discontinued alias looking current.

   The parallel `relations` table is also binary and unique on endpoints plus predicate, with no interval or relationship attributes. Multiple episodes collapse into one weighted edge. [relation schema](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:690) “Ana takes metformin 500 mg twice daily, prescribed by Dr Lee for diabetes” is one prescription episode, but the model can only bury qualifiers in a string or scatter them into unrelated triples.

   **Instead:** introduce typed values—entity references, quantities with units, coded concepts, dates, booleans, and structured compounds. Medication identity should distinguish ingredient, clinical product, components, formulation, strength, route, and schedule. Identity resolutions should be versioned, evidenced, and reversible. Add reified relationship/event instances with participant roles, qualifiers, and their own temporal extent; retain simple triples as a projection.

4. **High: exact half-open intervals cannot express uncertain time, and unknown start is currently misrepresented as zero duration.**

   `Interval` requires an exact start, while missing `valid_from` falls back to transaction time. `valid_until=None` means open, so it cannot distinguish “known to be ongoing,” “end not stated,” and “end unknown.” [interval representation](/Users/dev/Projects/engram-cli/crates/engram-core/src/interval.rs:20), [unknown-start fallback](/Users/dev/Projects/engram-cli/crates/engram-core/src/interval.rs:27)

   The clearest conceptual error is cessation without a known start: the planner writes `[end,end)` and describes that as knowing the state “held up to” the end. But the interval algebra explicitly defines that point as empty and overlapping nothing. [point semantics](/Users/dev/Projects/engram-cli/crates/engram-core/src/interval.rs:50), [cessation fallback](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:1281)

   “Approximately March” is also coerced into a precise date—the prompt’s own example turns March 2026 into March 1—and recurrence such as “every summer,” “weekdays,” or cyclical symptoms is impossible. [date extraction contract](/Users/dev/Projects/engram-cli/crates/engram-graph/src/facts.rs:38)

   **Instead:** store temporal expressions with independently known/unknown bounds, explicit ongoing status, precision (`day`, `month`, `year`), earliest/latest ranges, timezone, and optional recurrence. Preserve the source phrase and its resolution. A historical state with unknown start and known end is `(-∞?, end]`, not a point.

5. **High: the hardcoded relation registry is configuration masquerading as ontology.**

   The extractor explicitly invites frontier models to invent arbitrary predicates, but every unrecognised relation is silently interpreted as set-valued, mutable, continuous state, and text-valued. [extractor predicate contract](/Users/dev/Projects/engram-cli/crates/engram-graph/src/facts.rs:34), [unknown-relation default](/Users/dev/Projects/engram-cli/crates/engram-core/src/relations.rs:102)

   That bias sensibly avoids destructive closes, but it does not yield correct state. Hundreds of predicates such as `has_primary_email`, `legal_name`, `owns_vehicle`, or `has_ssn` will accumulate mutually incompatible open values without the store even knowing its schema is missing. Several pinned entries are already contextually wrong: people can hold multiple jobs and roles, have multiple nationalities, or have concurrent relationships. The registry also lacks domain/range, inverse and symmetry rules, derived relations, conditional cardinality, and structured value types.

   Worse, facts bind only predicate text, not the relation-definition version that interpreted them. Adding a registry entry later can retroactively change the meaning of years of stored facts without an explicit reinterpretation event.

   **Instead:** make relation definitions first-class, versioned store data. Include namespace, domain/range, cardinality constraints, temporal type, value schema, inverse/symmetry, identity policy, and safety policy. Unknown predicates should be marked `unclassified`: preserved without destructive inference, but visible as schema debt. The static table can remain a bootstrap catalogue, not the authority.

The append-only journal and the separation of transaction time from world time are the right foundation. But I would not freeze the current `Fact` row as engram’s long-term semantic contract. Today it is a useful, lossy current-state index—not yet an epistemic memory model. The highest-leverage change is proposition/assertion/evidence separation; it creates the attachment points needed for every other fix.