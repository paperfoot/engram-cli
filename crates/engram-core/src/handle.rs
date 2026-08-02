//! Typed id handles: `mem:<uuid>`, `chunk:<uuid>`, `fact:<uuid>`, …
//!
//! v0.3 shipped five id namespaces (chunk, memory, fact, conflict, entity)
//! through one untyped `id` field, and every mutator guessed. The observed
//! failure was not hypothetical: an agent fed a recall chunk id to `edit`,
//! got "no memory found (already deleted?)", concluded a race, and rebuilt
//! the memory by hand. The grammar here makes the kind part of the value, so
//! a wrong-kind error can name what it was actually given — while a bare
//! uuid stays accepted everywhere for compatibility with every script
//! written against v0.3.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandleKind {
    Memory,
    Chunk,
    Fact,
    Conflict,
    Entity,
    Document,
    Job,
    Event,
    /// D24. A lesson is an addressable row that `lessons confirm|retract`
    /// mutates by id, so it gets the same typed handle every other mutable
    /// row has. Without it the reasoning layer would print a bare uuid on a
    /// surface whose whole point is that ids carry their kind.
    Lesson,
    /// D24, read-surfaces' half. A wiki page is addressable by id in the
    /// compiler cache; it resolves to no memory, which is why `hit_handle`
    /// still declines to mint one for a retrieval hit.
    Wiki,
}

impl HandleKind {
    /// The wire prefix. Short forms on purpose: agents type these.
    pub fn as_str(self) -> &'static str {
        match self {
            HandleKind::Memory => "mem",
            HandleKind::Chunk => "chunk",
            HandleKind::Fact => "fact",
            HandleKind::Conflict => "conflict",
            HandleKind::Entity => "entity",
            HandleKind::Document => "doc",
            HandleKind::Job => "job",
            HandleKind::Event => "event",
            HandleKind::Lesson => "lesson",
            HandleKind::Wiki => "wiki",
        }
    }

    pub fn parse(s: &str) -> Option<HandleKind> {
        Some(match s {
            "mem" | "memory" => HandleKind::Memory,
            "chunk" => HandleKind::Chunk,
            "fact" => HandleKind::Fact,
            "conflict" => HandleKind::Conflict,
            "entity" => HandleKind::Entity,
            "doc" | "document" => HandleKind::Document,
            "job" => HandleKind::Job,
            "event" => HandleKind::Event,
            "lesson" => HandleKind::Lesson,
            "wiki" => HandleKind::Wiki,
            _ => return None,
        })
    }
}

impl fmt::Display for HandleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed id argument: an optional declared kind plus the uuid.
///
/// `kind: None` is the legacy bare-uuid form. It is not an error — the
/// resolver looks the uuid up across kinds — but every NEW surface renders
/// the prefixed form so the kind survives copy-paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    pub kind: Option<HandleKind>,
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleParseError {
    /// A prefix was given but is not a known kind. Carries the prefix so the
    /// error can echo it instead of a generic "invalid id".
    UnknownKind(String),
    /// The uuid part (or the whole bare string) failed to parse.
    BadUuid(String),
}

impl fmt::Display for HandleParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandleParseError::UnknownKind(k) => write!(
                f,
                "unknown id kind '{k}' (expected mem|chunk|fact|conflict|entity|doc|job|event)"
            ),
            HandleParseError::BadUuid(s) => write!(f, "'{s}' is not a valid UUID"),
        }
    }
}

impl std::error::Error for HandleParseError {}

impl Handle {
    pub fn parse(s: &str) -> Result<Handle, HandleParseError> {
        let s = s.trim();
        if let Some((prefix, rest)) = s.split_once(':') {
            let kind = HandleKind::parse(prefix)
                .ok_or_else(|| HandleParseError::UnknownKind(prefix.to_string()))?;
            let id =
                Uuid::parse_str(rest).map_err(|_| HandleParseError::BadUuid(rest.to_string()))?;
            return Ok(Handle {
                kind: Some(kind),
                id,
            });
        }
        let id = Uuid::parse_str(s).map_err(|_| HandleParseError::BadUuid(s.to_string()))?;
        Ok(Handle { kind: None, id })
    }

    pub fn render(kind: HandleKind, id: Uuid) -> String {
        format!("{}:{}", kind.as_str(), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_roundtrip() {
        let id = Uuid::new_v4();
        let s = Handle::render(HandleKind::Memory, id);
        let h = Handle::parse(&s).unwrap();
        assert_eq!(h.kind, Some(HandleKind::Memory));
        assert_eq!(h.id, id);
    }

    #[test]
    fn bare_uuid_is_legacy_not_error() {
        let id = Uuid::new_v4();
        let h = Handle::parse(&id.to_string()).unwrap();
        assert_eq!(h.kind, None);
        assert_eq!(h.id, id);
    }

    #[test]
    fn long_kind_aliases_parse() {
        let id = Uuid::new_v4();
        assert_eq!(
            Handle::parse(&format!("memory:{id}")).unwrap().kind,
            Some(HandleKind::Memory)
        );
        assert_eq!(
            Handle::parse(&format!("document:{id}")).unwrap().kind,
            Some(HandleKind::Document)
        );
    }

    #[test]
    fn unknown_kind_names_the_prefix() {
        let id = Uuid::new_v4();
        match Handle::parse(&format!("blob:{id}")) {
            Err(HandleParseError::UnknownKind(k)) => assert_eq!(k, "blob"),
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[test]
    fn bad_uuid_in_either_form() {
        assert!(matches!(
            Handle::parse("mem:not-a-uuid"),
            Err(HandleParseError::BadUuid(_))
        ));
        assert!(matches!(
            Handle::parse("not-a-uuid"),
            Err(HandleParseError::BadUuid(_))
        ));
    }
}
