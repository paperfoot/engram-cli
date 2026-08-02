# What Anthropic's current guidance says about engram

Sources, read 2026-08-02: [Context engineering rules for Claude 5 models](https://claude.com/blog/the-new-rules-of-context-engineering-for-claude-5-generation-models),
[Writing tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents),
[Harness design for long-running apps](https://www.anthropic.com/engineering/harness-design-long-running-apps),
[Effective context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents).

engram's users are frontier agents, so how those agents are built is a design input, not
trivia. Nine findings, ordered by how much they should change what we do. The first one
contradicts something this rebuild just shipped.

## 1. UUIDs measurably hurt agent precision — and we just made them the contract

> "merely resolving arbitrary alphanumeric UUIDs to more semantically meaningful and
> interpretable language … significantly improves Claude's precision."

We spent a wave making ids *typed* (`mem:7e02e127-…`) and that was right as far as it went —
it killed the wrong-kind class. But the id is still a UUID, and Anthropic's own measurement
says that costs accuracy on every surface an agent touches. Typing the handle fixed the
*kind* confusion and left the *legibility* problem untouched.

What to do: give every handle an optional semantic form the store can resolve —
`mem:ana-metformin-2026-08` or `fact:ana/takes_medication/metformin` — emitted beside the
UUID and accepted anywhere a handle is. The UUID stays canonical for storage; the agent
never has to hold one. This composes with what we built rather than replacing it.

## 2. Six read surfaces, and no rule for choosing between them

> "if a human engineer can't definitively say which tool should be used in a given
> situation, an AI agent can't be expected to do better."

engram has `recall`, `memory search`, `research`, `state`, `facts show`, and `people show`.
Ask when to use `research` over `recall`, or `state` over `facts show`, and the honest
answer is a paragraph of nuance. The coherence report already flagged `memory search` ≡
`recall` as near-duplicates and we deferred it as cosmetic. It is not cosmetic; it is the
single most-cited cause of agent tool misuse in Anthropic's guidance.

What to do: collapse to one retrieval verb with a mode, or make the distinction mechanical
and stateable in one line each. This is a bigger win than any individual output field.

## 3. Our biggest response is mostly duplicate bytes

> "the smallest possible set of high-signal tokens that maximize the likelihood of some
> desired outcome."

`recall` returns `answer_context` (a rendered envelope) *and* `results[].content` (the same
text again). Field data from the July mining: 1.01–1.30× duplication, 17 responses too
large for context, 2.5 MB spilled to disk, largest single response 476 KB. Every agent read
pays that twice.

## 4. `response_format` is a named recommendation and we have none

> "Offer a `response_format` enum parameter allowing agents to request `concise` or
> `detailed` responses."

Combined with (3): `--format concise` drops `answer_context`, truncates content to the
matched span plus a window, and returns handles the agent can expand. Sub-agent guidance
puts the target budget in view — a distilled sub-agent return is **1,000–2,000 tokens**.
That is the number `concise` should be designed against, not a guess.

## 5. Progressive disclosure — we built the primitive and left the default wrong

> Move detail into things the agent "could selectively call" … "deferred loading, which
> means the agent must search for their full definitions using ToolSearch before using
> them."

`agent-info` is one large manifest, read once per session by every agent. We shipped
`agent-info --schema <command>` in this very rebuild, which is exactly the deferred-loading
primitive — but the manifest is still the default and still carries everything. Invert it:
a small core (verbs, envelope, exit codes, the id contract) plus per-command detail on
demand. We are one default away from matching the pattern.

## 6. Trust the model more; our skill file is written for 2024 models

> Anthropic removed "over 80% of Claude Code's system prompt" for Opus 5 and Fable 5 "with
> no measurable loss." From prescriptive rules to "Claude can interpret the user's intent."

`assets/skill.md` is a long list of imperatives ("Do not add --kb to find something",
"Always check fact_extraction", …). Much of it is defending against mistakes a Claude 5
model does not make. Every component of a harness "encodes an assumption about what the
model can't do on its own" — and those assumptions expire. The skill should shrink toward
gotchas that are genuinely non-obvious *from the interface*, and the interface should carry
the rest.

## 7. Memory should be automatic, not a command the agent remembers to run

> "From Manual Memory to Auto-Memory: Replace explicit CLAUDE.md-based note-taking with
> automatic saves of memories that are relevant to the work."

engram is entirely opt-in: an agent must decide to call `remember`. The audit's own finding
— agents storing secrets they were told not to — is the same coin: what gets written is
whatever the agent thought of in the moment. Anthropic's direction is the harness deciding
what is durable. For engram that suggests a capture mode fed by the session rather than by
individual `remember` calls, with the store doing the filtering it is already good at.

## 8. Long-horizon handoff is a product we are one step from

> Context reset "combined with a structured handoff that carries the previous agent's state
> and the next steps" … "Communication was handled via files."

Handoff artifacts are files today. engram already stores durable state with provenance,
decisions, and now lessons. `engram handoff` — write/read a session's carry-forward state —
is a small surface over machinery that exists, and it is exactly the gap the harness
article describes agents papering over with markdown files.

## 9. Our own process was right for a reason we did not know

> "When asked to evaluate work they've produced, agents tend to respond by confidently
> praising the work — even when quality is obviously mediocre. Separating the agent doing
> the work from the agent judging it proves to be a strong lever."

The build used implementation lanes plus independent Grok and Codex gates, and the gates
found what the lanes could not see about themselves — three passes of it. Worth writing
down as method, not luck: **no lane grades its own work; the grader must be a different
model with its own reproductions.** The corollary we also hit: graders must be *re-run after
fixes*, because fixes compose into new defects.

## Ordering

(2) and (3)+(4) are the cheapest large wins for how well an agent uses engram tomorrow.
(1) is the one that contradicts shipped work and should be decided deliberately. (5) is
nearly free. (7) and (8) are new product surface and belong after 0.4.0 ships.
