//! LLM-based (subject, predicate, object) extraction for contradiction detection.
//!
//! Lean MVP. One LLM call per `engram remember`. Produces a list of atomic
//! triples that downstream code stores in the `facts` table and checks against
//! existing facts on the same `(subject_norm, predicate)` for conflicts.
//!
//! Write-path v2:
//! - Predicates are canonicalized through a small curated synonym map so
//!   `employed_by` and `works_at` land on the same slot. Full semantic
//!   conflict detection does NOT depend on this map — the embedding novelty
//!   gate (see `gate`) catches paraphrases; the map exists so slot policies
//!   can be keyed by predicate.
//! - Known single-valued predicates carry a `SlotPolicy`: last-writer-wins
//!   for routine profile slots, await-confirmation for safety-critical ones
//!   (allergies, medication) which are never auto-superseded.
//! - Extraction also emits `fact_type` and an optional `valid_from` event
//!   date, feeding the bi-temporal fact model.
//! - `adjudicate` resolves uncertain conflicts with one LLM call when the
//!   caller opts into full autonomy; otherwise conflicts queue for the
//!   calling agent, which has conversation context this system lacks.
//! - JSON parsing stays forgiving: tolerates code fences and stray prose. A
//!   parse FAILURE reports where the response broke and how big it was, and
//!   nothing from inside it: the extractor's answer restates the memory it was
//!   given, so an error message is not a place to put it.

use engram_llm::{ChatLlm, ChatMessage, LlmError};
use serde::{Deserialize, Serialize};

const FACT_EXTRACTION_PROMPT: &str = r#"You extract atomic factual claims from text as (subject, predicate, object) triples for a memory system.

RULES:
- subject: the entity the claim is ABOUT (a person, place, organization, project, gene, drug, etc). Use the proper name when available.
- predicate: a normalized verb phrase in snake_case. PREFER these canonical predicates when they fit: works_at, lives_in, has_age, has_role, married_to, partner_of, prefers, prefers_language, allergic_to, takes_medication, diagnosed_with, born_on, graduated_from, founded, owns, is_member_of, located_in, treats, inhibits, uses. Otherwise invent a sensible snake_case predicate.
- object: the value of the claim. Keep it concise (a name, place, number, date, or short noun phrase). For date-valued predicates (born_on, died_on, married_on, founded_on, graduated_on, joined_on, started_on, ended_on, hired_on) the object MUST be an ISO date (YYYY-MM-DD) whenever the text gives a full date — it is stored as the event time.
- confidence: 0.0..=1.0. 1.0 = explicit declarative statement; 0.7 = paraphrase or implicit; 0.4 = inferred from indirect mention.
- fact_type: one of profile_slot (single-valued attribute: employer, city, age), preference, relationship, event (something that happened at a time), procedural (how to do something), assertion (anything else).
- valid_from: if the text explicitly states WHEN the fact became true ("since March 2026", "starting next year"), an ISO date (YYYY-MM-DD). Otherwise null.

SKIP:
- Greetings, opinions, hedged statements ("might", "I think", "probably").
- Statements about anonymous "I" / "me" / "the user" with no identifiable subject.
- Pure questions or commands.
- Generic facts that aren't about a specific entity ("the sky is blue").

OUTPUT:
A JSON array of objects with keys: subject, predicate, object, confidence, fact_type, valid_from.
If there are no extractable facts, output exactly: []
Output ONLY the JSON. No prose, no code fences, no commentary.

EXAMPLE INPUT:
"Ada Example founded Example Labs in 2024, moved to Lisbon in March 2026, and prefers Rust over Go for CLI tools."

EXAMPLE OUTPUT:
[
  {"subject":"Ada Example","predicate":"founded","object":"Example Labs","confidence":1.0,"fact_type":"event","valid_from":"2024-01-01"},
  {"subject":"Ada Example","predicate":"lives_in","object":"Lisbon","confidence":1.0,"fact_type":"profile_slot","valid_from":"2026-03-01"},
  {"subject":"Ada Example","predicate":"prefers_language","object":"Rust","confidence":1.0,"fact_type":"preference","valid_from":null}
]"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// Semantic category emitted by the extractor (see `FactType::parse`).
    #[serde(default)]
    pub fact_type: Option<String>,
    /// ISO date (or RFC3339) when the fact became true, if stated.
    #[serde(default)]
    pub valid_from: Option<String>,
}

fn default_confidence() -> f32 {
    1.0
}

#[derive(Debug, thiserror::Error)]
pub enum FactExtractionError {
    #[error("llm error: {0}")]
    Llm(#[from] LlmError),
    /// A parse failure names WHERE the model's JSON broke and HOW BIG the
    /// response was — never what it said. The extractor's answer restates the
    /// memory it was extracted from, so echoing it copies private content into
    /// stderr, the tracing log, and the `fact_extraction_note` an agent reads
    /// back. Carrying a `&'static str` and three numbers instead of a free-form
    /// String makes the leak unconstructible rather than merely absent, and
    /// leaves nothing an untrusted response can inject into. The REMEDY is the
    /// caller's to name: it knows the KB (see `ExtractionStatus::note`).
    #[error(
        "could not parse fact JSON: {kind} error at line {line} column {column} of the \
         extracted JSON ({response_bytes} bytes of model output withheld)"
    )]
    Parse {
        /// serde_json's own classification, as a word that is stable across
        /// releases so an operator can grep a log for it.
        kind: &'static str,
        line: usize,
        column: usize,
        response_bytes: usize,
    },
}

/// Extract `(subject, predicate, object)` triples from text via one LLM call.
/// Returns an empty Vec if the LLM finds no extractable facts.
pub async fn extract_facts<L: ChatLlm + ?Sized>(
    llm: &L,
    text: &str,
) -> Result<Vec<ExtractedFact>, FactExtractionError> {
    let resp = llm
        .chat(&[
            ChatMessage::system(FACT_EXTRACTION_PROMPT),
            ChatMessage::user(text.to_string()),
        ])
        .await?;
    parse_extraction_output(&resp.content)
}

/// Describe a `serde_json` failure without quoting the data that caused it.
///
/// Forwarding serde_json's own Display is NOT safe here: a `Category::Data`
/// error renders as `invalid type: string "<the value>", expected f32`, so
/// merely dropping the raw suffix would still copy a slice of the model's
/// answer — and therefore of the memory — into the message. Only the category,
/// the position and the size survive.
fn describe_parse_failure(e: &serde_json::Error, response_bytes: usize) -> FactExtractionError {
    use serde_json::error::Category;
    let kind = match e.classify() {
        Category::Syntax => "syntax",
        Category::Data => "schema",
        Category::Eof => "truncated",
        Category::Io => "io",
    };
    FactExtractionError::Parse {
        kind,
        line: e.line(),
        column: e.column(),
        response_bytes,
    }
}

/// Parse the LLM's response, tolerating code fences and surrounding whitespace.
/// Exposed for unit testing without an LLM round-trip.
pub fn parse_extraction_output(content: &str) -> Result<Vec<ExtractedFact>, FactExtractionError> {
    let trimmed = content.trim();
    // Strip markdown code fences if the model wrapped the JSON.
    let json = if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.trim_end_matches("```").trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.trim_end_matches("```").trim()
    } else {
        trimmed
    };
    // Some models prefix the array with explanatory prose; find the first '['.
    let start = json.find('[').unwrap_or(0);
    let json = &json[start..];
    let end = json.rfind(']').map(|i| i + 1).unwrap_or(json.len());
    let json = &json[..end];

    // Not echoed anywhere: `content` is the model restating the memory. The
    // length is of the WHOLE response; the line/column are into the JSON
    // candidate sliced out of it above.
    serde_json::from_str::<Vec<ExtractedFact>>(json)
        .map_err(|e| describe_parse_failure(&e, content.len()))
}

/// Lowercase + trim normalization used for both subject and object lookups.
/// Intentionally minimal — no entity resolution, no alias merging.
pub fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Map predicate synonyms onto one canonical form so slot policies key
/// reliably. Deliberately small: semantic conflict detection does not depend
/// on this map (the embedding gate handles paraphrase); this only routes
/// known single-valued slots onto their policy.
pub fn canonical_predicate(p: &str) -> String {
    let p = p.trim().to_lowercase().replace([' ', '-'], "_");
    match p.as_str() {
        "employed_by" | "employer" | "job_at" | "works_for" | "employed_at" => "works_at",
        "resides_in" | "home_city" | "based_in" | "living_in" | "residence" => "lives_in",
        "age" | "aged" | "is_age" => "has_age",
        "role" | "position" | "job_title" | "title" => "has_role",
        "spouse" | "spouse_of" | "wife_of" | "husband_of" => "married_to",
        "dating" | "girlfriend_of" | "boyfriend_of" | "in_relationship_with" => "partner_of",
        "allergy" | "has_allergy" | "allergic" => "allergic_to",
        "medication" | "on_medication" | "takes_drug" | "prescribed" => "takes_medication",
        "favorite_language" | "preferred_language" | "codes_in" => "prefers_language",
        _ => return p,
    }
    .to_string()
}

/// Conflict policy for single-valued ("slot") predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotPolicy {
    /// Routine profile slot: the newest value wins automatically.
    LastWriterWins,
    /// Safety-critical slot: never auto-supersede; queue for confirmation.
    AwaitConfirmation,
}

/// Policy for a canonical predicate, or None when the predicate is not a
/// known single-valued slot (multi-valued predicates like `prefers` or
/// `founded` can coexist and are handled by the semantic gate instead).
pub fn slot_policy(canonical: &str) -> Option<SlotPolicy> {
    match canonical {
        "works_at" | "lives_in" | "has_age" | "has_role" | "married_to" | "partner_of"
        | "prefers_language" | "born_on" => Some(SlotPolicy::LastWriterWins),
        "allergic_to" | "takes_medication" | "diagnosed_with" | "blood_type" => {
            Some(SlotPolicy::AwaitConfirmation)
        }
        _ => None,
    }
}

/// Predicates whose OBJECT *is* the date the claim refers to. For these the
/// object date is the fact's event time: `Ana born_on 1990-04-12` became true
/// on 1990-04-12, not on the day someone typed it into the store. Callers feed
/// this into the bi-temporal `valid_from` column so birthdays, anniversaries,
/// and start dates answer "when" questions instead of reporting write time.
pub fn date_valued_predicate(canonical: &str) -> bool {
    matches!(
        canonical,
        "born_on"
            | "died_on"
            | "married_on"
            | "founded_on"
            | "graduated_on"
            | "joined_on"
            | "started_on"
            | "ended_on"
            | "hired_on"
    )
}

// ----- Conflict adjudication (full-autonomy mode) -----

const ADJUDICATION_PROMPT: &str = r#"You adjudicate whether a NEW fact contradicts, updates, duplicates, or coexists with EXISTING facts in a memory system.

Decide ONE action:
- "supersede": the new fact is an update/correction — the listed existing fact ids are now outdated.
- "coexist": the new fact and the existing facts are all true simultaneously (different aspects, not contradictory).
- "reject_new": the new fact is redundant with or less reliable than what is stored; do not store it.

Judge by meaning, not wording. An update to a single-valued attribute (city, employer, dosage) supersedes. Additional values of a multi-valued attribute (another preference, another project) coexist.

OUTPUT: one JSON object, nothing else:
{"action":"supersede","ids":["<id>", ...]} or {"action":"coexist"} or {"action":"reject_new"}"#;

/// Adjudicator verdict for an uncertain conflict.
#[derive(Debug, Clone, PartialEq)]
pub enum AdjudicationAction {
    /// The new fact supersedes these existing fact ids.
    Supersede(Vec<String>),
    Coexist,
    RejectNew,
}

/// Resolve an uncertain conflict with one LLM call. `new_fact` and each
/// candidate are rendered as "id | subject predicate object (context)".
pub async fn adjudicate<L: ChatLlm + ?Sized>(
    llm: &L,
    new_fact: &str,
    candidates: &[String],
) -> Result<AdjudicationAction, FactExtractionError> {
    let user = format!(
        "NEW fact:
{new_fact}

EXISTING facts:
{}",
        candidates.join(
            "
"
        )
    );
    let resp = llm
        .chat(&[
            ChatMessage::system(ADJUDICATION_PROMPT),
            ChatMessage::user(user),
        ])
        .await?;
    parse_adjudication_output(&resp.content)
}

/// Parse the adjudicator's JSON, tolerating fences and prose. Unparseable or
/// unknown responses fail closed to `Coexist` — the safe outcome is keeping
/// both facts (contested) rather than destroying one on a garbled verdict.
pub fn parse_adjudication_output(content: &str) -> Result<AdjudicationAction, FactExtractionError> {
    let trimmed = content.trim();
    let start = trimmed.find('{').unwrap_or(0);
    let end = trimmed.rfind('}').map(|i| i + 1).unwrap_or(trimmed.len());
    let json = trimmed.get(start..end).unwrap_or(trimmed);

    #[derive(Deserialize)]
    struct Raw {
        action: String,
        #[serde(default)]
        ids: Vec<String>,
    }
    let Ok(raw) = serde_json::from_str::<Raw>(json) else {
        return Ok(AdjudicationAction::Coexist);
    };
    Ok(match raw.action.as_str() {
        "supersede" if !raw.ids.is_empty() => AdjudicationAction::Supersede(raw.ids),
        "reject_new" => AdjudicationAction::RejectNew,
        _ => AdjudicationAction::Coexist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_array() {
        let raw =
            r#"[{"subject":"Alice","predicate":"works_at","object":"Acme","confidence":1.0}]"#;
        let f = parse_extraction_output(raw).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].subject, "Alice");
        assert_eq!(f[0].predicate, "works_at");
        assert_eq!(f[0].confidence, 1.0);
    }

    #[test]
    fn parse_code_fenced_array() {
        let raw = "```json\n[{\"subject\":\"Bob\",\"predicate\":\"lives_in\",\"object\":\"Berlin\",\"confidence\":0.9}]\n```";
        let f = parse_extraction_output(raw).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].object, "Berlin");
    }

    #[test]
    fn parse_with_leading_prose_is_tolerant() {
        let raw = "Here are the extracted facts:\n[{\"subject\":\"X\",\"predicate\":\"is_a\",\"object\":\"Y\",\"confidence\":1.0}]";
        let f = parse_extraction_output(raw).unwrap();
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn parse_empty_array() {
        let f = parse_extraction_output("[]").unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn missing_confidence_defaults_to_1() {
        let raw = r#"[{"subject":"X","predicate":"is_a","object":"Y"}]"#;
        let f = parse_extraction_output(raw).unwrap();
        assert_eq!(f[0].confidence, 1.0);
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        assert_eq!(normalize("  Ada Example  "), "ada example");
        assert_eq!(normalize("Example Labs"), "example labs");
    }

    #[test]
    fn parse_v2_fields() {
        let raw = r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0,"fact_type":"profile_slot","valid_from":"2026-03-01"}]"#;
        let f = parse_extraction_output(raw).unwrap();
        assert_eq!(f[0].fact_type.as_deref(), Some("profile_slot"));
        assert_eq!(f[0].valid_from.as_deref(), Some("2026-03-01"));
    }

    #[test]
    fn parse_without_v2_fields_still_works() {
        let raw = r#"[{"subject":"X","predicate":"is_a","object":"Y","confidence":1.0}]"#;
        let f = parse_extraction_output(raw).unwrap();
        assert!(f[0].fact_type.is_none());
        assert!(f[0].valid_from.is_none());
    }

    #[test]
    fn canonical_predicate_maps_synonyms() {
        assert_eq!(canonical_predicate("employed_by"), "works_at");
        assert_eq!(canonical_predicate("Employer"), "works_at");
        assert_eq!(canonical_predicate("resides_in"), "lives_in");
        assert_eq!(canonical_predicate("girlfriend_of"), "partner_of");
        // Unknown predicates pass through normalized, not remapped.
        assert_eq!(canonical_predicate("inhibits"), "inhibits");
        assert_eq!(canonical_predicate("Founded By"), "founded_by");
    }

    #[test]
    fn slot_policies_route_correctly() {
        assert_eq!(slot_policy("lives_in"), Some(SlotPolicy::LastWriterWins));
        assert_eq!(
            slot_policy("allergic_to"),
            Some(SlotPolicy::AwaitConfirmation)
        );
        assert_eq!(slot_policy("prefers"), None);
        assert_eq!(slot_policy("inhibits"), None);
    }

    #[test]
    fn date_valued_predicates_are_recognized() {
        assert!(date_valued_predicate("born_on"));
        assert!(date_valued_predicate(&canonical_predicate("Born On")));
        assert!(!date_valued_predicate("lives_in"));
        assert!(!date_valued_predicate("works_at"));
    }

    #[test]
    fn adjudication_parses_supersede() {
        let a = parse_adjudication_output(r#"{"action":"supersede","ids":["abc"]}"#).unwrap();
        assert_eq!(a, AdjudicationAction::Supersede(vec!["abc".into()]));
    }

    #[test]
    fn adjudication_supersede_without_ids_fails_closed_to_coexist() {
        let a = parse_adjudication_output(r#"{"action":"supersede","ids":[]}"#).unwrap();
        assert_eq!(a, AdjudicationAction::Coexist);
    }

    #[test]
    fn adjudication_garbage_fails_closed_to_coexist() {
        assert_eq!(
            parse_adjudication_output("I think they conflict").unwrap(),
            AdjudicationAction::Coexist
        );
    }

    #[test]
    fn adjudication_tolerates_fenced_json() {
        let raw = "```json\n{\"action\":\"reject_new\"}\n```";
        let a = parse_adjudication_output(raw).unwrap();
        assert_eq!(a, AdjudicationAction::RejectNew);
    }

    /// A token that exists nowhere else in engram, so finding it in an error
    /// can only mean the model's answer was echoed.
    const LEAK_SENTINEL: &str = "QUOKKA-7F3A-DO-NOT-ECHO";

    #[test]
    fn a_parse_failure_never_echoes_the_model_response() {
        let cases = [
            // Prose wrapped around a truncated array: the whole response used
            // to be appended after "raw:".
            format!("Here are the facts about {LEAK_SENTINEL}: [{{\"subject\":\"Ana\","),
            // A mistyped field — serde_json's OWN message quotes the value, so
            // this one fails against a fix that merely drops the raw suffix.
            format!(
                "[{{\"subject\":\"Ana\",\"predicate\":\"lives_in\",\"object\":\"Lisbon\",\
                 \"confidence\":\"{LEAK_SENTINEL}\"}}]"
            ),
            // A fenced response the model cut off mid-string.
            format!("```json\n[{{\"subject\":\"{LEAK_SENTINEL}\"\n"),
        ];
        for raw in cases {
            let rendered = parse_extraction_output(&raw).unwrap_err().to_string();
            assert!(
                !rendered.contains(LEAK_SENTINEL),
                "the model response leaked into the error: {rendered}"
            );
        }
    }

    #[test]
    fn a_parse_failure_still_says_where_it_broke_and_how_big_it_was() {
        let raw =
            r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":"high"}]"#;
        let rendered = parse_extraction_output(raw).unwrap_err().to_string();
        assert!(rendered.contains("could not parse fact JSON"), "{rendered}");
        assert!(
            rendered.contains("schema"),
            "a wrong type is a schema error: {rendered}"
        );
        assert!(rendered.contains("line 1"), "{rendered}");
        assert!(
            rendered.contains(&format!("{} bytes", raw.len())),
            "the size is the one number worth reporting: {rendered}"
        );
    }

    #[test]
    fn broken_json_and_wrong_types_are_told_apart() {
        // Distinguishing these is the whole diagnostic value left: a syntax
        // error means the model wrote junk, a schema error means the contract
        // moved.
        assert!(parse_extraction_output(r#"[{"subject": }]"#)
            .unwrap_err()
            .to_string()
            .contains("syntax"));
        assert!(
            parse_extraction_output(r#"[{"subject":"Ana","confidence":"high"}]"#)
                .unwrap_err()
                .to_string()
                .contains("schema")
        );
    }
}
