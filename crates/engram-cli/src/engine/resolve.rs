//! `facts resolve`, made transactional.
//!
//! The defect: five statements with no transaction, and two of them threw
//! their booleans away. Two `engram facts resolve <same-id>` processes both
//! passed the `status == "open"` check, both superseded a fact, and both
//! printed `"resolved": true` — and because the two actions can differ, the
//! pair could end up with each fact superseded by the other. A two-node
//! supersession cycle with no open interval, reported twice as success.
//!
//! The fix is one claim inside one transaction, and the claim's boolean is
//! load-bearing. Per D8 this is a WRAPPER: `resolve_conflict_with_decision` is
//! the storage primitive that claims, records the decision and appends
//! `decision.recorded`; `supersede_fact_with` appends `fact.interval_closed`.
//! `Core::resolve_conflict` appends nothing itself — it adds the two refusals
//! that belong above storage (the supersession-cycle guard and the interval
//! safety gate) and runs the whole sequence in one write.

use engram_core::interval::{may_close, CloseAuthority, CloseVerdict, Interval};
use engram_core::relations::{canonical_relation, relation_spec};
use engram_core::types::Fact;
use engram_graph::semantics::FactView;
use engram_graph::semantics::{DecisionVerdict, SemanticKey, SEMANTIC_KEY_VERSION};
use engram_storage::SqliteStore;
use uuid::Uuid;

use super::{Core, WriteCtx};
use crate::error::CliError;
use crate::respond::ResolveAction;

/// Longest supersession chain the guard will walk. A pre-existing cycle in the
/// data is itself a refusal, not an infinite loop.
const MAX_CHAIN: usize = 64;

pub struct ResolveRequest {
    pub conflict_id: Uuid,
    pub action: ResolveAction,
    pub note: Option<String>,
    /// `cli:<diary>` | `daemon` | `job:<uuid>`.
    pub actor: String,
}

pub struct ResolveApplied {
    pub conflict_id: Uuid,
    pub action: &'static str,
    pub kept: Vec<Fact>,
    pub closed: Option<Fact>,
    pub decision_id: Uuid,
}

impl Core {
    /// Claim, gate, apply — in one transaction, exactly once.
    pub async fn resolve_conflict(&self, req: ResolveRequest) -> Result<ResolveApplied, CliError> {
        self.write("facts.resolve", move |w| resolve_in_tx(w, req))
            .await
    }

    pub fn resolve_conflict_blocking(
        &self,
        req: ResolveRequest,
    ) -> Result<ResolveApplied, CliError> {
        self.write_blocking("facts.resolve", move |w| resolve_in_tx(w, req))
    }
}

fn resolve_in_tx(w: &WriteCtx<'_>, req: ResolveRequest) -> Result<ResolveApplied, CliError> {
    let store = w.store;
    let id = req.conflict_id;
    let Some(row) = store.get_fact_conflict(id)? else {
        return Err(CliError::NotFound {
            what: "conflict",
            handle: format!("conflict:{id}"),
        });
    };
    let (Some(new_f), Some(old_f)) = (
        store.get_fact(row.new_fact_id)?,
        store.get_fact(row.old_fact_id)?,
    ) else {
        return Err(CliError::NotFound {
            what: "the facts this conflict names",
            handle: format!("conflict:{id}"),
        });
    };

    // Which fact closes, and which survives it.
    let (closing, survivor) = match req.action {
        ResolveAction::SupersedeOld => (Some(&old_f), &new_f),
        ResolveAction::RejectNew => (Some(&new_f), &old_f),
        ResolveAction::KeepBoth => (None, &new_f),
    };

    // The key the DECISION is filed under. Prefer the one the queue row
    // recorded — the write path and the migration both computed it — and
    // recompute only for a pre-wave-3 row that has none.
    let key = match row.decision_key.clone() {
        Some(k) if !k.is_empty() => k,
        _ => SemanticKey::for_pair(&FactView::of(&new_f), &FactView::of(&old_f)).encode(),
    };
    let verdict = match req.action {
        ResolveAction::SupersedeOld => DecisionVerdict::CloseOld,
        ResolveAction::RejectNew => DecisionVerdict::RejectNew,
        ResolveAction::KeepBoth => DecisionVerdict::KeepBoth,
    };
    // WHICH value this ruling retires [G3], recorded on the decision itself.
    // The queue row knows it too, but the queue row is destructible — a `kb
    // delete` cascades it through the fact foreign keys and leaves the decision
    // behind — and a decision that has outlived its orientation replays on pair
    // order, which is how the caller's kept dose became the closed one.
    let loser = closing.map(|f| f.object_norm.clone());

    // THE CLAIM. A conditional `UPDATE … WHERE status='open'` inside this
    // transaction: `None` means somebody else adjudicated first, and the loser
    // never reaches the mutation below.
    let decision_id = store
        .resolve_conflict_with_decision(
            id,
            &row.kb,
            &row.diary,
            &key,
            verdict.as_str(),
            loser.as_deref(),
            "caller",
            &req.actor,
            req.note.as_deref(),
            SEMANTIC_KEY_VERSION,
        )?
        .ok_or_else(|| {
            let reread = store.get_fact_conflict(id).ok().flatten();
            let by = reread
                .as_ref()
                .and_then(|r| r.resolved_by.clone())
                .unwrap_or_else(|| "another caller".to_string());
            let when = reread
                .as_ref()
                .and_then(|r| r.resolved_at.clone())
                .unwrap_or_default();
            let status = reread
                .as_ref()
                .map(|r| r.status.clone())
                .unwrap_or_else(|| "resolved".to_string());
            CliError::ConflictAlreadyResolved {
                conflict_id: id.to_string(),
                verdict: if when.is_empty() {
                    format!("{status} by {by}")
                } else {
                    format!("{status} by {by} at {when}")
                },
            }
        })?;

    // The refusals that belong ABOVE storage, after the claim so that a lost
    // race is reported as a lost race and not as whatever the winner's write
    // happened to make impossible. Either refusal rolls the claim back with
    // the rest of the transaction.
    if let Some(closing) = closing {
        // The degenerate `new_fact_id == old_fact_id` row lands here too: a
        // fact cannot supersede itself.
        if would_cycle(store, closing.id, survivor.id)? {
            return Err(CliError::BadInput(format!(
                "resolving conflict {id} this way would create a supersession cycle: fact \
                 {} already leads back to {} — run `engram doctor --integrity` if the chain \
                 itself looks wrong",
                survivor.id, closing.id
            )));
        }
        // The interval safety gate, unmodified. A caller decision IS
        // `CloseAuthority::Caller`, so this permits far more than the write
        // path does; what it still refuses is a relation whose registry entry
        // says no authority may close it this way.
        let spec = relation_spec(&canonical_relation(&closing.predicate));
        let authority = CloseAuthority::Caller {
            actor: req.actor.clone(),
        };
        if let CloseVerdict::Refuse { reason } = may_close(&spec, &authority) {
            return Err(CliError::BadInput(reason.to_string()));
        }
    }

    // Apply. `supersede_fact_with` closes the interval and appends
    // `fact.interval_closed` — this lane appends nothing on top of it (D8), so
    // `admin rebuild` sees one event per close, not two.
    let closed = match closing {
        Some(closing) => {
            let until = if matches!(req.action, ResolveAction::SupersedeOld) {
                survivor.valid_from
            } else {
                None
            };
            let applied = store.supersede_fact_with(closing.id, survivor.id, "caller", until)?;
            if !applied {
                // The fact was closed between the read and the write: refuse
                // rather than report a supersession that did not happen. The
                // claim rolls back with everything else.
                return Err(CliError::ConflictAlreadyResolved {
                    conflict_id: id.to_string(),
                    verdict: format!("fact {} was already closed by another writer", closing.id),
                });
            }
            store.get_fact(closing.id)?
        }
        None => None,
    };

    let kept = match req.action {
        ResolveAction::KeepBoth => vec![new_f.clone(), old_f.clone()],
        _ => vec![survivor.clone()],
    };
    Ok(ResolveApplied {
        conflict_id: id,
        action: req.action.as_str(),
        kept,
        closed,
        decision_id,
    })
}

/// Would closing `closing` in favour of `survivor` produce a supersession
/// cycle?
///
/// True when they are the same fact, when following `survivor`'s successor
/// chain reaches `closing`, or when the chain runs longer than [`MAX_CHAIN`] —
/// a pre-existing cycle in the data is a refusal, not a hang.
pub fn would_cycle(store: &SqliteStore, closing: Uuid, survivor: Uuid) -> Result<bool, CliError> {
    if closing == survivor {
        return Ok(true);
    }
    let mut at = survivor;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_CHAIN {
        if !seen.insert(at) {
            return Ok(true);
        }
        let Some(f) = store.get_fact(at)? else {
            return Ok(false);
        };
        let Some(next) = f.closed_by.or(f.superseded_by) else {
            return Ok(false);
        };
        if next == closing {
            return Ok(true);
        }
        at = next;
    }
    Ok(true)
}

/// A fact still asserts something. Re-exported here only so the resolve path
/// and its tests read one predicate.
pub fn is_open(f: &Fact) -> bool {
    Interval::of(f).is_open()
}
