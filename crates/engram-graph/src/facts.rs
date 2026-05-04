//! LLM-based (subject, predicate, object) extraction for contradiction detection.
//!
//! Lean MVP. One LLM call per `engram remember`. Produces a list of atomic
//! triples that downstream code stores in the `facts` table and checks against
//! existing facts on the same `(subject_norm, predicate)` for conflicts.
//!
//! Design choices kept intentionally simple:
//! - One prompt, one model, one shot. No multi-turn refinement.
//! - Predicate vocabulary is open (snake_case suggested) — no fixed schema.
//! - Subject normalization is `trim().to_lowercase()`. No alias resolution.
//!   This means "Ada Example" and "ada example" merge, but "Ada E"
//!   does not. Good enough for the MVP; entity normalization is a follow-up.
//! - Confidence is reported by the LLM and trusted as-is. No reranking.
//! - JSON parsing is forgiving: tolerates code fences and stray whitespace.

use engram_llm::{ChatLlm, ChatMessage, LlmError};
use serde::{Deserialize, Serialize};

const FACT_EXTRACTION_PROMPT: &str = r#"You extract atomic factual claims from text as (subject, predicate, object) triples for a memory system.

RULES:
- subject: the entity the claim is ABOUT (a person, place, organization, project, gene, drug, etc). Use the proper name when available.
- predicate: a normalized verb phrase in snake_case. Examples: works_at, lives_in, prefers, owns, graduated_from, born_on, has_role, is_member_of, married_to, founded_by, located_in, treats, inhibits, uses, prefers_language, has_age, has_height.
- object: the value of the claim. Keep it concise (a name, place, number, date, or short noun phrase).
- confidence: 0.0..=1.0. 1.0 = explicit declarative statement; 0.7 = paraphrase or implicit; 0.4 = inferred from indirect mention.

SKIP:
- Greetings, opinions, hedged statements ("might", "I think", "probably").
- Statements about anonymous "I" / "me" / "the user" with no identifiable subject.
- Pure questions or commands.
- Generic facts that aren't about a specific entity ("the sky is blue").

OUTPUT:
A JSON array of objects with keys: subject, predicate, object, confidence.
If there are no extractable facts, output exactly: []
Output ONLY the JSON. No prose, no code fences, no commentary.

EXAMPLE INPUT:
"Ada Example founded Example Labs in 2024 and prefers Rust over Go for CLI tools because of single-binary deployment."

EXAMPLE OUTPUT:
[
  {"subject":"Ada Example","predicate":"founded","object":"Example Labs","confidence":1.0},
  {"subject":"Example Labs","predicate":"founded_in","object":"2024","confidence":1.0},
  {"subject":"Ada Example","predicate":"prefers_language","object":"Rust","confidence":1.0}
]"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    1.0
}

#[derive(Debug, thiserror::Error)]
pub enum FactExtractionError {
    #[error("llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("could not parse fact JSON: {0}")]
    Parse(String),
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

    serde_json::from_str::<Vec<ExtractedFact>>(json)
        .map_err(|e| FactExtractionError::Parse(format!("{e}; raw: {}", content)))
}

/// Lowercase + trim normalization used for both subject and object lookups.
/// Intentionally minimal — no entity resolution, no alias merging.
pub fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
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
}
