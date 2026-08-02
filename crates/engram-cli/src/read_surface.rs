//! The read surfaces' shared machinery: cursors, provenance, and the two
//! guards a read must never skip.
//!
//! WHY THIS IS ONE MODULE. Eight commands hand-rolled the same three things —
//! page counts, scope labels, provenance JSON — in eight slightly different
//! ways, and the drift is what produced the defect this wave exists to close:
//! `count` meant "rows on this page" on one surface and "rows that matched" on
//! another, so an agent read `count: 3` from `facts conflicts` and reported
//! three open conflicts to a user who had 953 queued. One builder, one meaning.
//! The envelope TYPES (`Listing`, `Scope`, `Verdict`, `Absence`) are
//! `output.rs`'s and are imported here, never redeclared; a parallel set would
//! recreate the same drift one layer up.
//!
//! Rulings implemented here: D11 (one cursor codec, `state` and `jobs list`
//! call this one), D12 (one `Provenance` struct), R7's read-side guard.
//! Everything is a pure function over values — no store, no async — so the
//! rules can be tested without a database.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use engram_core::handle::HandleKind;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::CliError;
use crate::output::Absence;

/// The request a cursor is bound to.
///
/// A cursor issued for `--kb work --diary default` must not silently page a
/// different scope: an offset into a different result set is a wrong answer
/// wearing the shape of a right one. The fingerprint is a hash of the
/// effective filters, so a caller that changes a filter and keeps the cursor
/// gets exit 3 naming the mismatch instead of a plausible page of the wrong
/// rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorKey {
    pub surface: &'static str,
    pub fingerprint: String,
}

impl CursorKey {
    /// `parts` are the effective filters, in a FIXED order per surface. `None`
    /// and `Some("")` are deliberately distinct from `Some("*")`: "no filter"
    /// and "every value" reach the same rows today but are different requests,
    /// and a cursor should not survive the difference.
    pub fn new(surface: &'static str, parts: &[(&str, Option<&str>)]) -> CursorKey {
        let mut buf = String::from(surface);
        for (name, value) in parts {
            buf.push('\u{1f}');
            buf.push_str(name);
            buf.push('=');
            match value {
                Some(v) => buf.push_str(v),
                None => buf.push('\u{0}'),
            }
        }
        CursorKey {
            surface,
            fingerprint: engram_core::keys::content_hash(&buf),
        }
    }
}

/// The cursor's wire version. Bump it and old cursors are rejected loudly
/// rather than decoded into a wrong offset.
const CURSOR_VERSION: &str = "1";

/// Opaque, URL-safe, versioned. Opaque BY CONTRACT: the encoding may change
/// between releases, a caller that parses one is doing so at its own risk, and
/// `agent-info` says exactly that. Pass it back verbatim.
pub fn encode_cursor(key: &CursorKey, offset: usize) -> String {
    let body = json!({ "s": key.surface, "f": key.fingerprint, "o": offset });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    format!(
        "{CURSOR_VERSION}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// `BadInput` (exit 3) on a malformed cursor, and `BadInput` naming the
/// mismatch when the cursor belongs to another surface or another scope.
/// Never a silent `0`: paging from the top because a token was unreadable is
/// how a caller re-reads page one forever and calls it a hang.
pub fn decode_cursor(raw: &str, key: &CursorKey) -> Result<usize, CliError> {
    let malformed = || {
        CliError::BadInput(
            "cursor is not a cursor this build issued — pass back the `cursor` field verbatim, \
             or omit it to start from the first page"
                .to_string(),
        )
    };
    let body = raw.strip_prefix("1.").ok_or_else(malformed)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| malformed())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| malformed())?;
    let surface = value
        .get("s")
        .and_then(Value::as_str)
        .ok_or_else(malformed)?;
    let fingerprint = value
        .get("f")
        .and_then(Value::as_str)
        .ok_or_else(malformed)?;
    let offset = value
        .get("o")
        .and_then(Value::as_u64)
        .ok_or_else(malformed)?;
    if surface != key.surface {
        return Err(CliError::BadInput(format!(
            "this cursor was issued by `{surface}` and was passed to `{}` — \
             a cursor is only valid on the surface that issued it",
            key.surface
        )));
    }
    if fingerprint != key.fingerprint {
        return Err(CliError::BadInput(format!(
            "this cursor was issued for a different scope on `{}` — re-run without --cursor \
             to page the scope you asked for",
            key.surface
        )));
    }
    Ok(offset as usize)
}

/// The single cursor-emission rule, so no surface invents its own.
///
/// An ABSENT cursor is the only end-of-listing signal. A caller must not infer
/// the end from `returned < limit`: a filtered page can be short and still have
/// successors, and every consumer that guessed got it wrong on the first
/// sparsely-populated KB it met.
pub fn next_cursor(key: &CursorKey, offset: usize, returned: usize, total: i64) -> Option<String> {
    let seen = offset.saturating_add(returned);
    if (seen as i64) < total {
        Some(encode_cursor(key, seen))
    } else {
        None
    }
}

/// chunk → memory → document, as far as it is knowable for this row.
///
/// Every field is `Option` because a row kind may genuinely lack a link — a
/// wiki page is a synthesis over a whole KB and has no single memory — and a
/// FABRICATED link is worse than a missing one: an agent that follows an
/// invented `memory_id` edits somebody else's memory. `derived_from` says why
/// a link is missing, so "unknown" and "not applicable" stop looking alike.
/// Shape pinned by D12; `observed_at` and `snippet` are reasoning-layer's half
/// of the same struct and stay unset here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    pub chunk_id: Option<Uuid>,
    pub memory_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub kb: Option<String>,
    pub diary: Option<String>,
    pub derived_from: Option<&'static str>,
    pub observed_at: Option<DateTime<Utc>>,
    pub snippet: Option<String>,
}

/// `derived_from` vocabulary — the reasons a row has no memory behind it.
pub const DERIVED_WIKI: &str = "derived:wiki_page";
pub const DERIVED_ENTITY: &str = "derived:entity_aggregate";
pub const DERIVED_RELATION: &str = "derived:relation_edge";
pub const DERIVED_FACT: &str = "derived:fact_projection";

impl Provenance {
    pub fn for_kb(kb: impl Into<String>) -> Provenance {
        Provenance {
            kb: Some(kb.into()),
            ..Default::default()
        }
    }

    pub fn derived(kb: impl Into<String>, from: &'static str) -> Provenance {
        Provenance {
            kb: Some(kb.into()),
            derived_from: Some(from),
            ..Default::default()
        }
    }

    /// Nulls are PRESERVED, not omitted: agents branch on null, and dropping
    /// the key makes "we looked and there is none" indistinguishable from
    /// "this build does not implement the field".
    ///
    /// `observed_at` and `snippet` are struct fields, so serializing six of the
    /// eight was a silent truncation: every caller but `state` reported a hit's
    /// provenance without the two fields that say WHEN it was observed and WHAT
    /// the text was — the citation half of a citation.
    pub fn to_json(&self) -> Value {
        json!({
            "chunk_id": self.chunk_id.map(|i| i.to_string()),
            "memory_id": self.memory_id.map(|i| i.to_string()),
            "document_id": self.document_id.map(|i| i.to_string()),
            "kb": self.kb,
            "diary": self.diary,
            "derived_from": self.derived_from,
            "observed_at": self.observed_at.map(|t| t.to_rfc3339()),
            "snippet": self.snippet,
        })
    }
}

/// The row's own kind-prefixed handle, per interfaces §1.
pub fn handle(kind: HandleKind, id: Uuid) -> String {
    format!("{}:{}", kind.as_str(), id)
}

/// The handle for a retrieval hit, or `None` when the hit kind has no
/// resolvable identity.
///
/// `claim`, `wiki` and `relation` hits get NO handle rather than an invented
/// `claim:`/`wiki:` prefix: `HandleKind` is closed (D24) and `resolve_handle`
/// would reject those strings, so publishing one would hand agents a handle
/// every mutator refuses. Their `provenance.memory_id` is the id that works.
pub fn hit_handle(kind: &str, id: Uuid) -> Option<String> {
    match kind {
        "chunk" => Some(handle(HandleKind::Chunk, id)),
        "entity" => Some(handle(HandleKind::Entity, id)),
        _ => None,
    }
}

/// R7's read-side guard: a safety pair is ALWAYS `requires_context`, whatever
/// the stored row says.
///
/// The queue rows written before wave 3 are still on disk — 951 of them in a
/// real store — and many carry `suggested_action: "supersede-old"` on
/// medication slots. An agent that executes the suggestion retires a live
/// medication record. The write side is fixed, but the read side must not
/// republish what the old write side wrote, so the rule is applied at BOTH
/// ends. `metadata.suggested_action_overridden` counts every firing, so this
/// guard can never quietly mask a write-side regression (spec risk R-5).
pub fn suggested_action_for(row_suggested: Option<&str>, safety: bool) -> &'static str {
    if safety {
        return "requires_context";
    }
    match row_suggested {
        Some("supersede-old") => "supersede-old",
        Some("requires_context") => "requires_context",
        Some("keep-both") => "keep-both",
        // An unknown or absent stored value is not evidence of anything. The
        // conservative reading is the honest one.
        _ => "keep-both",
    }
}

/// Whether the guard changed the advice — the number behind
/// `metadata.suggested_action_overridden`.
pub fn suggested_action_overridden(row_suggested: Option<&str>, emitted: &str) -> bool {
    matches!(row_suggested, Some(s) if s != emitted)
}

/// What a ranked read actually looked at, for the absence certificate.
///
/// Populated even when nothing came back — that is the whole point: without
/// these numbers "no results" and "you searched the wrong namespace" are the
/// same response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecallStats {
    /// Chunks the dense pass actually scored, after diary/kb/window filtering.
    pub chunks_considered: usize,
    /// Candidates that entered fusion, from every source.
    pub candidates_fused: usize,
    /// Best fused score BEFORE any truncation. `None` when the pool was empty.
    pub top_score: Option<f32>,
    /// KBs represented in the chunk pool — the honest `searched_kbs`.
    pub kbs_searched: Vec<String>,
}

/// Which filters the request carried, for `absence.reason`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestShape {
    pub has_window: bool,
    pub has_diary_filter: bool,
}

/// Why a read came back empty, derived from what the read measured.
///
/// `hybrid_recall` does not tag its own emptiness, so the reason is inferred
/// from the stats in a fixed order, first match wins. `no_candidate_above_cutoff`
/// is deliberately ABSENT from this vocabulary: there is no score cutoff
/// anywhere in the retrieval path, so the value could only ever be a lie.
pub fn absence_reason(stats: &RecallStats, req: RequestShape) -> &'static str {
    if stats.chunks_considered == 0 {
        if req.has_window {
            return "filtered_by_window";
        }
        if req.has_diary_filter {
            return "filtered_by_diary";
        }
        return "no_chunks_in_scope";
    }
    if stats.candidates_fused == 0 {
        return "no_candidate_matched";
    }
    "trimmed_by_layer_budget"
}

/// The absence certificate for a ranked read.
///
/// `threshold` stays `None` on purpose: nothing in the retrieval path rejects
/// a candidate by score, so a `0.0` here would be a fabricated bar that agents
/// would then tune against. When a real cutoff lands, this is the one place
/// that changes.
pub fn absence_from(stats: &RecallStats, searched_kbs: Vec<String>, req: RequestShape) -> Absence {
    Absence::new(
        absence_reason(stats, req),
        searched_kbs,
        stats.chunks_considered,
    )
    .with_scores(stats.top_score, None)
}

/// The absence certificate for an ENUMERATING read (`facts list`, `people
/// list`, …), which scores no chunks and can only report the scope it walked.
pub fn absence_for_listing(searched_kbs: Vec<String>, reason: &'static str) -> Absence {
    Absence::new(reason, searched_kbs, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CursorKey {
        CursorKey::new(
            "facts.list",
            &[("kb", Some("work")), ("diary", Some("default"))],
        )
    }

    #[test]
    fn a_cursor_round_trips() {
        let k = key();
        let c = encode_cursor(&k, 40);
        assert!(c.starts_with("1."));
        assert_eq!(decode_cursor(&c, &k).unwrap(), 40);
    }

    #[test]
    fn a_cursor_from_another_scope_is_rejected() {
        // The defect this prevents: paging a DIFFERENT result set at the same
        // offset and presenting it as page two of this one.
        let other = CursorKey::new(
            "facts.list",
            &[("kb", Some("personal")), ("diary", Some("default"))],
        );
        let err = decode_cursor(&encode_cursor(&other, 5), &key()).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("different scope"));
    }

    #[test]
    fn a_cursor_from_another_surface_is_rejected() {
        let other = CursorKey::new("facts.conflicts", &[("kb", Some("work"))]);
        let err = decode_cursor(&encode_cursor(&other, 5), &key()).unwrap_err();
        assert!(err.to_string().contains("only valid on the surface"));
    }

    #[test]
    fn garbage_is_exit_3_never_offset_zero() {
        for raw in ["", "garbage", "2.abc", "1.!!!!", "1."] {
            let err = decode_cursor(raw, &key()).unwrap_err();
            assert_eq!(err.exit_code(), 3, "{raw}");
        }
    }

    #[test]
    fn an_unfiltered_scope_is_not_the_same_key_as_a_wildcard_one() {
        let none = CursorKey::new("facts.list", &[("kb", None)]);
        let star = CursorKey::new("facts.list", &[("kb", Some("*"))]);
        assert_ne!(none.fingerprint, star.fingerprint);
    }

    #[test]
    fn the_last_page_has_no_cursor() {
        let k = key();
        assert!(next_cursor(&k, 0, 5, 25).is_some());
        assert!(next_cursor(&k, 20, 5, 25).is_none());
        assert!(next_cursor(&k, 0, 0, 0).is_none());
        // A short page mid-listing still has a successor: `returned < limit`
        // is not an end-of-listing signal.
        assert!(next_cursor(&k, 0, 3, 25).is_some());
    }

    #[test]
    fn safety_always_requires_context_whatever_the_row_says() {
        // The pre-wave-3 queue row that would otherwise advise retiring a
        // medication.
        assert_eq!(
            suggested_action_for(Some("supersede-old"), true),
            "requires_context"
        );
        assert_eq!(suggested_action_for(None, true), "requires_context");
        // The honest half of R7: a non-safety slot may still suggest it.
        assert_eq!(
            suggested_action_for(Some("supersede-old"), false),
            "supersede-old"
        );
        assert_eq!(suggested_action_for(None, false), "keep-both");
        assert_eq!(suggested_action_for(Some("nonsense"), false), "keep-both");
        assert!(suggested_action_overridden(
            Some("supersede-old"),
            "requires_context"
        ));
        assert!(!suggested_action_overridden(
            Some("supersede-old"),
            "supersede-old"
        ));
    }

    #[test]
    fn absence_reason_covers_every_case_and_invents_none() {
        let empty = RecallStats::default();
        assert_eq!(
            absence_reason(&empty, RequestShape::default()),
            "no_chunks_in_scope"
        );
        assert_eq!(
            absence_reason(
                &empty,
                RequestShape {
                    has_window: true,
                    has_diary_filter: true
                }
            ),
            "filtered_by_window"
        );
        assert_eq!(
            absence_reason(
                &empty,
                RequestShape {
                    has_window: false,
                    has_diary_filter: true
                }
            ),
            "filtered_by_diary"
        );
        let scored = RecallStats {
            chunks_considered: 10,
            ..Default::default()
        };
        assert_eq!(
            absence_reason(&scored, RequestShape::default()),
            "no_candidate_matched"
        );
        let fused = RecallStats {
            chunks_considered: 10,
            candidates_fused: 4,
            top_score: Some(0.7),
            ..Default::default()
        };
        assert_eq!(
            absence_reason(&fused, RequestShape::default()),
            "trimmed_by_layer_budget"
        );
    }

    #[test]
    fn absence_never_fabricates_a_threshold() {
        let stats = RecallStats {
            chunks_considered: 3,
            candidates_fused: 2,
            top_score: Some(0.41),
            kbs_searched: vec!["work".into()],
        };
        let a = absence_from(&stats, vec!["work".into()], RequestShape::default());
        assert_eq!(a.threshold, None);
        assert_eq!(a.top_score, Some(0.41));
        assert_eq!(a.searched_chunks, 3);
    }

    #[test]
    fn provenance_keeps_its_nulls() {
        let p = Provenance::derived("work", DERIVED_WIKI);
        let v = p.to_json();
        assert!(v["memory_id"].is_null());
        assert!(v.get("memory_id").is_some(), "the key must still be there");
        assert_eq!(v["derived_from"], json!(DERIVED_WIKI));
        assert_eq!(v["kb"], json!("work"));
        // Every field of the struct is serialized, nulls included: a caller
        // that reads `provenance.snippet` must not get `undefined` on the
        // surfaces that happen not to fill it in.
        for key in ["observed_at", "snippet"] {
            assert!(v.get(key).is_some(), "{key} was dropped by to_json");
            assert!(v[key].is_null());
        }
    }

    #[test]
    fn provenance_serializes_observed_at_and_snippet_when_it_has_them() {
        let p = Provenance {
            observed_at: Some(
                DateTime::parse_from_rfc3339("2026-03-01T09:30:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            snippet: Some("Ana moved to Lisbon".into()),
            ..Provenance::for_kb("personal")
        };
        let v = p.to_json();
        assert_eq!(v["observed_at"], json!("2026-03-01T09:30:00+00:00"));
        assert_eq!(v["snippet"], json!("Ana moved to Lisbon"));
    }

    #[test]
    fn only_resolvable_kinds_get_a_handle() {
        let id = Uuid::nil();
        assert_eq!(
            hit_handle("chunk", id).as_deref(),
            Some("chunk:00000000-0000-0000-0000-000000000000")
        );
        // No invented prefix that `resolve_handle` would reject.
        assert_eq!(hit_handle("claim", id), None);
        assert_eq!(hit_handle("wiki", id), None);
        assert_eq!(hit_handle("relation", id), None);
    }
}
