//! `engram lessons` — procedural memory: what the WORK taught us.
//!
//! The store remembered what is true and had nowhere to put what it had
//! learned about answering: that a caller overrules the machine on this slot,
//! that a memory written in a hurry gets corrected within the minute, that a
//! particular source produces facts nobody keeps. Every agent rediscovered
//! those from scratch, one correction at a time.
//!
//! A lesson has a trigger (when it applies), a sentence (what to do), evidence
//! (journal event ids), and a status. It is stored twice on purpose:
//!
//! 1. in the `lessons` projection, where it has structure, a dedup key and a
//!    counter, and
//! 2. as an ordinary memory tagged `kind:procedural` — the TWIN — so `recall`
//!    finds it. A lesson layer an agent has to already know about is a silo,
//!    and the whole point is that the next agent does not have to know.
//!
//! Two rules that are cheap to state and expensive to rediscover:
//!
//! - **The twin is written HERE, not by calling `remember::run`.** That
//!   function always prints (`print_success` at its end), so calling it would
//!   emit a `remember` envelope on stdout followed by this command's — and a
//!   caller that parses one JSON document off stdout gets a lie. Write-surfaces
//!   was asked for a non-printing `write_memory` (request W-1) and it has not
//!   landed; until it does, this file writes the twin through the same storage
//!   calls `remember` uses, in one transaction.
//! - **`no_extract_facts` in spirit: a lesson never becomes a world fact.** The
//!   twin is written with no extraction at all. "When a caller resolves a
//!   takes_medication conflict, keep both" must not extract a
//!   `takes_medication` fact, or the reflection layer poisons the fact layer.

use chrono::{DateTime, Utc};
use engram_core::events::EventKind;
use engram_core::handle::{Handle, HandleKind, HandleParseError};
use engram_core::types::{Memory, MemorySource};
use engram_embed::TaskMode;
use engram_ingest::memory::chunk_memory;
use engram_storage::journal::{cli_actor, EventDraft};
use engram_storage::lessons::{
    normalize_lesson, payload, LessonRow, LessonUpsert, STATUS_CONFIRMED, STATUS_PROPOSED,
    STATUS_RETIRED,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Absence, Listing, Metadata, Scope, Verdict};
use crate::read_surface::{self, CursorKey};

/// The diary a lesson's twin lands in unless the caller says otherwise.
///
/// `default`, NOT a `lessons` namespace of its own, and the reason is the
/// whole point of the twin: `recall` defaults to `--diary default`, so a
/// lesson filed in its own diary is invisible to every recall an agent
/// actually runs. A silo with better hygiene is still a silo. Callers who do
/// want the separate namespace pass `--diary lessons` and read it back with
/// `recall --diary lessons`.
pub const DEFAULT_LESSON_DIARY: &str = "default";
const TRIGGER_MAX: usize = 64;

pub struct AddArgs {
    pub lesson: String,
    pub trigger: String,
    pub kb: String,
    pub diary: String,
    pub evidence: Vec<String>,
    pub confidence: Option<f32>,
    pub offline: bool,
}

pub struct ListArgs {
    pub kb: String,
    pub trigger: Option<String>,
    pub status: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

/// `lesson:<uuid>`, built in one place.
pub fn render_handle(id: Uuid) -> String {
    read_surface::handle(HandleKind::Lesson, id)
}

/// `[a-z0-9_]{1,64}`. A trigger is a KEY — it is what makes two agents'
/// lessons about the same situation land in the same bucket — so free text
/// would quietly turn the dedup index into a list.
fn validate_trigger(raw: &str) -> Result<String, CliError> {
    let t = raw.trim();
    let ok = !t.is_empty()
        && t.len() <= TRIGGER_MAX
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(CliError::BadInput(format!(
            "--trigger `{raw}` is not a slug: use lowercase letters, digits and underscores, \
             1..{TRIGGER_MAX} characters (e.g. `fresh_write_correction`)"
        )));
    }
    Ok(t.to_string())
}

fn parse_id(raw: &str, expected: HandleKind) -> Result<Uuid, CliError> {
    let handle = Handle::parse(raw).map_err(|e| match &e {
        HandleParseError::BadUuid(_) => CliError::BadInput(format!("invalid UUID {raw}: {e}")),
        HandleParseError::UnknownKind(p) => CliError::BadInput(format!(
            "unknown id kind `{p}`: expected `{}` or a bare uuid",
            expected.as_str()
        )),
    })?;
    match handle.kind {
        None => Ok(handle.id),
        Some(k) if k == expected => Ok(handle.id),
        Some(k) => Err(CliError::WrongIdKind {
            handle: raw.to_string(),
            actual: k.as_str().to_string(),
            expected: expected.as_str().to_string(),
            correct: None,
        }),
    }
}

/// Every `--evidence` id must name an EVENT that exists. A lesson whose
/// evidence points at nothing is an opinion wearing a citation.
fn resolve_evidence(ctx: &AppContext, raw: &[String]) -> Result<Vec<Uuid>, CliError> {
    let mut out = Vec::new();
    for item in raw {
        let id = parse_id(item, HandleKind::Event)?;
        let found = ctx.store.resolve_handle(&Handle {
            kind: Some(HandleKind::Event),
            id,
        })?;
        if found.is_none() {
            return Err(CliError::NotFound {
                what: "event",
                handle: item.clone(),
            });
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

fn lesson_json(row: &LessonRow) -> Value {
    json!({
        "handle": render_handle(row.id),
        "id": row.id.to_string(),
        "kb": row.kb,
        "diary": row.diary,
        "trigger": row.trigger,
        "lesson": row.lesson,
        "status": row.status,
        "source_kind": row.source_kind,
        "confidence": row.confidence,
        "occurrences": row.occurrences,
        "rule": row.rule,
        "evidence": row.evidence.iter().map(|e| read_surface::handle(HandleKind::Event, *e)).collect::<Vec<_>>(),
        "memory_id": row.memory_id.map(|m| m.to_string()),
        "memory": row.memory_id.map(|m| read_surface::handle(HandleKind::Memory, m)),
        "created_at": row.created_at.to_rfc3339(),
        "updated_at": row.updated_at.to_rfc3339(),
    })
}

/// The text of the recallable twin. Prefixed with the trigger because recall
/// is over content: a lesson that does not mention its own situation is a
/// lesson recall cannot surface at the moment it applies.
fn twin_content(trigger: &str, lesson: &str) -> String {
    format!("Lesson ({trigger}): {lesson}")
}

/// One chunk of a twin with its vector: id, text, embedding, position,
/// section.
type EmbeddedChunk = (Uuid, String, Vec<f32>, u32, Option<String>);

/// A twin with its vectors in hand, ready to commit.
struct TwinDraft {
    memory: Memory,
    chunks: Vec<EmbeddedChunk>,
    model: String,
    dims: usize,
    prompt_format: String,
}

/// The memory twin, written through the same storage calls `remember` uses —
/// chunked, embedded, and committed inside the caller's transaction.
///
/// Embedding happens BEFORE the transaction opens because it is a network
/// call: a memory row committed without its chunks can never be recalled, and
/// the exact-content dedup would then make every retry a successful-looking
/// no-op.
async fn write_twin(
    ctx: &AppContext,
    kb: &str,
    diary: &str,
    content: &str,
    trigger: &str,
    offline: bool,
) -> Result<TwinDraft, CliError> {
    let embedder = crate::retrieval::resolve_write_embedder("lessons add", offline)?;
    let policy = crate::commands::chunks::resolve_policy();
    let chunks = chunk_memory(content, policy);
    let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    let embeddings = embedder
        .embedder
        .embed_batch_accounted(&texts, TaskMode::RetrievalDocument)
        .await
        .map_err(|f| CliError::from(f.error))?;
    if embeddings.len() != chunks.len() {
        return Err(CliError::Transient(format!(
            "embedder returned {} embeddings for {} chunks; nothing was stored — retry \
             `engram lessons add`",
            embeddings.len(),
            chunks.len()
        )));
    }
    if embedder.cloud {
        crate::commands::usage::record_embed_spend(
            &ctx.store,
            "lessons_embed",
            &embedder.model,
            Some(kb),
            Some(diary),
            &texts,
            1,
            json!({ "dimensions": embedder.dims, "estimated": true }),
        );
    }
    let now = Utc::now();
    let memory = Memory {
        id: Uuid::new_v4(),
        content: content.to_string(),
        created_at: now,
        event_time: None,
        importance: 7,
        emotional_weight: 0,
        access_count: 0,
        last_accessed: None,
        stability: 1.0,
        source: MemorySource::Manual,
        diary: diary.to_string(),
        valid_from: Some(now),
        valid_until: None,
        tags: vec![
            "kind:procedural".to_string(),
            "lesson".to_string(),
            format!("trigger:{trigger}"),
        ],
    };
    let rows = chunks
        .iter()
        .zip(embeddings)
        .map(|(c, e)| {
            (
                Uuid::new_v4(),
                c.text.clone(),
                e,
                c.position,
                c.section.clone(),
            )
        })
        .collect();
    Ok(TwinDraft {
        memory,
        chunks: rows,
        model: embedder.model,
        dims: embedder.dims,
        prompt_format: embedder.prompt_format,
    })
}

/// Commits inside the caller's open transaction — `StorageError`, not
/// `CliError`, so it can be called from a `with_tx` closure without a lossy
/// re-wrap.
fn commit_twin(
    store: &engram_storage::SqliteStore,
    kb: &str,
    twin: &TwinDraft,
) -> Result<(), engram_storage::StorageError> {
    store.insert_memory_with_kb(&twin.memory, kb)?;
    for (chunk_id, text, embedding, position, section) in &twin.chunks {
        store.insert_chunk_with_embedding_meta(
            *chunk_id,
            twin.memory.id,
            text,
            *position,
            section.as_deref(),
            embedding,
            &twin.model,
            twin.dims,
            &twin.prompt_format,
            None,
        )?;
    }
    Ok(())
}

/// `engram lessons add "<lesson>" --trigger <slug>`.
///
/// Written at `confirmed`: a human who typed the sentence has already
/// confirmed it. `engram reflect` writes at `proposed`, and only a
/// confirmation gives a machine-proposed sentence a recallable twin — so
/// `recall` never surfaces an unreviewed machine opinion as if a person had
/// said it.
pub async fn add(ctx: &AppContext, args: AddArgs) -> Result<(), CliError> {
    let trigger = validate_trigger(&args.trigger)?;
    let lesson = args.lesson.trim().to_string();
    if lesson.is_empty() {
        return Err(CliError::BadInput(
            "a lesson cannot be empty — say what to do next time".into(),
        ));
    }
    crate::commands::validate_diary_for_write(&args.diary)?;
    if args.kb.trim() == "*" || args.kb.trim().is_empty() {
        return Err(CliError::BadInput(
            "--kb '*' is a read-side wildcard; a lesson is written to ONE knowledge base".into(),
        ));
    }
    let evidence = resolve_evidence(ctx, &args.evidence)?;
    let content = twin_content(&trigger, &lesson);
    let twin = write_twin(ctx, &args.kb, &args.diary, &content, &trigger, args.offline).await?;

    let now = Utc::now();
    let row = LessonRow {
        id: Uuid::new_v4(),
        kb: args.kb.trim().to_string(),
        diary: args.diary.clone(),
        trigger,
        lesson_norm: normalize_lesson(&lesson),
        lesson,
        status: STATUS_CONFIRMED.to_string(),
        source_kind: "user_stated".to_string(),
        confidence: args.confidence.unwrap_or(0.9),
        occurrences: 1,
        evidence,
        memory_id: Some(twin.memory.id),
        rule: None,
        created_at: now,
        updated_at: now,
    };

    // ONE unit of work: the KB registration the FK needs, the twin, the
    // projection row and its event commit together or not at all. There is no
    // state in which a lessons row exists without the event that rebuilds it.
    let (stored, outcome) = ctx.store.with_tx(|store| {
        store.ensure_kb(row.kb.trim(), None)?;
        commit_twin(store, &row.kb, &twin)?;
        let (stored, outcome) = store.upsert_lesson(&row)?;
        store.append_event(
            &EventDraft::new(
                EventKind::LessonRecorded,
                cli_actor(&stored.diary),
                payload::lesson_recorded(&stored),
            )
            .scope(Some(&stored.kb), Some(&stored.diary))
            .subject(stored.id),
        )?;
        Ok((stored, outcome))
    })?;

    let merged = outcome == LessonUpsert::Merged;
    let mut meta = Metadata::default();
    meta.add("handle", render_handle(stored.id));
    meta.add("merged", merged);
    meta.add("occurrences", stored.occurrences);
    let payload_json = lesson_json(&stored);
    print_success(ctx.format, payload_json, meta, |data| {
        println!(
            "{} lesson {} ({})",
            if merged { "Merged into" } else { "Recorded" },
            data["handle"].as_str().unwrap_or(""),
            data["trigger"].as_str().unwrap_or("")
        );
        println!("  {}", data["lesson"].as_str().unwrap_or(""));
        if merged {
            println!(
                "  learned {} times so far",
                data["occurrences"].as_i64().unwrap_or(1)
            );
        }
    });
    Ok(())
}

/// `engram lessons list`. `--status` defaults to everything except `retired`:
/// retiring a lesson takes it out of the way without deleting it, and a
/// listing that still shows it has not retired anything.
pub fn list(ctx: &AppContext, args: ListArgs) -> Result<(), CliError> {
    let kb_filter = match args.kb.trim() {
        "*" | "" => None,
        other => Some(other),
    };
    let trigger = match &args.trigger {
        Some(t) => Some(validate_trigger(t)?),
        None => None,
    };
    if let Some(s) = &args.status {
        if ![STATUS_PROPOSED, STATUS_CONFIRMED, STATUS_RETIRED].contains(&s.as_str()) {
            return Err(CliError::BadInput(format!(
                "--status `{s}` is not one of proposed|confirmed|retired"
            )));
        }
    }
    let key = CursorKey::new(
        "lessons.list",
        &[
            ("kb", kb_filter),
            ("trigger", trigger.as_deref()),
            ("status", args.status.as_deref()),
        ],
    );
    let offset = match &args.cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    let mut rows = ctx.store.list_lessons(
        kb_filter,
        trigger.as_deref(),
        args.status.as_deref(),
        args.limit.max(1),
        offset,
    )?;
    let mut total =
        ctx.store
            .count_lessons(kb_filter, trigger.as_deref(), args.status.as_deref())?;
    if args.status.is_none() {
        let retired =
            ctx.store
                .count_lessons(kb_filter, trigger.as_deref(), Some(STATUS_RETIRED))?;
        rows.retain(|r| r.status != STATUS_RETIRED);
        total -= retired;
    }
    let cursor = read_surface::next_cursor(&key, offset, rows.len(), total);

    let mut payload = serde_json::Map::new();
    payload.insert(
        "lessons".to_string(),
        Value::Array(rows.iter().map(lesson_json).collect()),
    );
    Listing::page(rows.len(), total, cursor).attach(&mut payload);
    if rows.is_empty() {
        // An enumerating surface scores no chunks, so the certificate says
        // what scope it walked and nothing it did not measure.
        Verdict::NoMatch(Absence::new("no_chunks_in_scope", vec![args.kb.clone()], 0))
            .attach(&mut payload);
    } else {
        Verdict::Results.attach(&mut payload);
    }
    let mut meta = Metadata::default();
    Scope::listing(
        vec![args.kb.clone()],
        DEFAULT_LESSON_DIARY,
        if kb_filter.is_some() {
            "flag"
        } else {
            "all_kbs"
        },
    )
    .attach(&mut meta);
    print_success(ctx.format, Value::Object(payload), meta, |data| {
        let rows = data["lessons"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            println!("(no lessons)");
            return;
        }
        for l in &rows {
            println!(
                "  [{}] {} ({}x)  {}",
                l["status"].as_str().unwrap_or(""),
                l["trigger"].as_str().unwrap_or(""),
                l["occurrences"].as_i64().unwrap_or(1),
                l["lesson"].as_str().unwrap_or("")
            );
            println!("      {}", l["handle"].as_str().unwrap_or(""));
        }
        println!();
        println!(
            "{} of {} lessons",
            rows.len(),
            data["total"].as_i64().unwrap_or(0)
        );
        if let Some(c) = data["cursor"].as_str() {
            println!("more: engram lessons list --cursor {c}");
        }
    });
    Ok(())
}

fn load(ctx: &AppContext, raw: &str) -> Result<LessonRow, CliError> {
    let id = parse_id(raw, HandleKind::Lesson)?;
    ctx.store.get_lesson(id)?.ok_or(CliError::NotFound {
        what: "lesson",
        handle: raw.to_string(),
    })
}

/// `engram lessons confirm <id>` — promote a proposed lesson and give it the
/// recallable twin it did not get while it was only a machine's suggestion.
pub async fn confirm(
    ctx: &AppContext,
    raw_id: String,
    note: Option<String>,
    offline: bool,
) -> Result<(), CliError> {
    let row = load(ctx, &raw_id)?;
    let at = Utc::now();
    let twin = match row.memory_id {
        Some(_) => None,
        None => {
            let content = twin_content(&row.trigger, &row.lesson);
            Some(write_twin(ctx, &row.kb, &row.diary, &content, &row.trigger, offline).await?)
        }
    };
    let memory_id = twin.as_ref().map(|t| t.memory.id).or(row.memory_id);

    ctx.store.with_tx(|store| {
        store.ensure_kb(&row.kb, None)?;
        if let Some(twin) = &twin {
            commit_twin(store, &row.kb, twin)?;
            store.set_lesson_memory(row.id, twin.memory.id, at)?;
        }
        store.set_lesson_status(row.id, STATUS_CONFIRMED, at)?;
        store.append_event(
            &EventDraft::new(
                EventKind::LessonConfirmed,
                cli_actor(&row.diary),
                payload::lesson_confirmed(row.id, "caller", note.as_deref(), memory_id, at),
            )
            .scope(Some(&row.kb), Some(&row.diary))
            .subject(row.id),
        )?;
        Ok(())
    })?;

    let updated = load(ctx, &raw_id)?;
    let mut meta = Metadata::default();
    meta.add("handle", render_handle(updated.id));
    print_success(ctx.format, lesson_json(&updated), meta, |data| {
        println!(
            "Confirmed {} — recall can now find it",
            data["handle"].as_str().unwrap_or("")
        );
    });
    Ok(())
}

/// `engram lessons retract <id>` — out of the default listing, never out of
/// the store. Move 1: nothing is ever hard-deleted, and the `lesson.retracted`
/// event is what makes the disappearance explicable rather than mysterious.
pub fn retract(ctx: &AppContext, raw_id: String, reason: Option<String>) -> Result<(), CliError> {
    let row = load(ctx, &raw_id)?;
    let at = Utc::now();
    ctx.store.with_tx(|store| {
        store.set_lesson_status(row.id, STATUS_RETIRED, at)?;
        store.append_event(
            &EventDraft::new(
                EventKind::LessonRetracted,
                cli_actor(&row.diary),
                payload::lesson_retracted(row.id, "caller", reason.as_deref(), at),
            )
            .scope(Some(&row.kb), Some(&row.diary))
            .subject(row.id),
        )?;
        Ok(())
    })?;
    let updated = load(ctx, &raw_id)?;
    let mut meta = Metadata::default();
    meta.add("handle", render_handle(updated.id));
    print_success(ctx.format, lesson_json(&updated), meta, |data| {
        println!(
            "Retired {} — still in the store, out of the default listing",
            data["handle"].as_str().unwrap_or("")
        );
    });
    Ok(())
}

/// What a proposer proposes. A struct rather than eight positional arguments:
/// a lesson, its rule and its evidence are one thing, and splitting them into
/// a parameter list is how a caller eventually passes the trigger as the rule.
pub struct Proposal<'a> {
    pub trigger: &'a str,
    pub lesson: &'a str,
    pub rule: &'a str,
    pub evidence: Vec<Uuid>,
    pub confidence: f32,
}

/// Record a machine-proposed lesson: `proposed` status, NO memory twin, and
/// the `lesson.recorded` event, in one transaction. Shared with `engram
/// reflect`, which is the only caller that proposes.
pub fn record_proposed(
    ctx: &AppContext,
    kb: &str,
    proposal: &Proposal<'_>,
    at: DateTime<Utc>,
) -> Result<(LessonRow, LessonUpsert), CliError> {
    let Proposal {
        trigger,
        lesson,
        rule,
        evidence,
        confidence,
    } = proposal;
    let row = LessonRow {
        id: Uuid::new_v4(),
        kb: kb.to_string(),
        diary: DEFAULT_LESSON_DIARY.to_string(),
        trigger: trigger.to_string(),
        lesson_norm: normalize_lesson(lesson),
        lesson: lesson.to_string(),
        status: STATUS_PROPOSED.to_string(),
        source_kind: "agent_extracted".to_string(),
        confidence: *confidence,
        occurrences: 1,
        evidence: evidence.clone(),
        memory_id: None,
        rule: Some(rule.to_string()),
        created_at: at,
        updated_at: at,
    };
    let out = ctx.store.with_tx(|store| {
        store.ensure_kb(kb, None)?;
        let (stored, outcome) = store.upsert_lesson(&row)?;
        store.append_event(
            &EventDraft::new(
                EventKind::LessonRecorded,
                "job:reflect",
                payload::lesson_recorded(&stored),
            )
            .scope(Some(&stored.kb), Some(&stored.diary))
            .subject(stored.id),
        )?;
        Ok((stored, outcome))
    })?;
    Ok(out)
}

pub fn to_json(row: &LessonRow) -> Value {
    lesson_json(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_is_a_slug_not_a_sentence() {
        assert_eq!(
            validate_trigger("fresh_write_correction").unwrap(),
            "fresh_write_correction"
        );
        assert_eq!(
            validate_trigger(" conflict_lives_in ").unwrap(),
            "conflict_lives_in"
        );
        for bad in ["Not A Slug!", "", "  ", "UPPER", "has-dash", "emoji🙂"] {
            let err = validate_trigger(bad).unwrap_err();
            assert_eq!(err.exit_code(), 3, "{bad:?}");
            assert!(
                err.to_string().contains("slug"),
                "{bad:?} message must say the shape"
            );
        }
        assert!(validate_trigger(&"a".repeat(TRIGGER_MAX + 1)).is_err());
    }

    #[test]
    fn an_id_of_the_wrong_kind_names_the_kind_it_got() {
        let id = Uuid::new_v4();
        assert_eq!(parse_id(&id.to_string(), HandleKind::Lesson).unwrap(), id);
        assert_eq!(
            parse_id(&format!("lesson:{id}"), HandleKind::Lesson).unwrap(),
            id
        );
        let err = parse_id(&format!("mem:{id}"), HandleKind::Lesson).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("mem"), "{err}");
    }

    #[test]
    fn the_twin_says_which_situation_it_is_about() {
        // Recall is over content: a lesson that never names its trigger is a
        // lesson recall cannot surface at the moment it applies.
        let c = twin_content("fresh_write_correction", "re-read before superseding");
        assert!(c.contains("fresh_write_correction"));
        assert!(c.contains("re-read before superseding"));
    }
}
