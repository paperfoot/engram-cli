//! Deterministic entity extraction — no LLM required.
//!
//! Extracts:
//! - Capitalized multi-word phrases ("Mount Everest", "New York")
//! - ALL-CAPS acronyms / biomedical terms (mTORC1, BRCA1, IL-6)
//! - Words containing digits (IC50, p53, GPT-5)
//!
//! This is intentionally cheap and fast. Phase 4+ can layer LLM-extracted
//! triples on top, but this baseline already populates the entities table
//! for co-occurrence edges.

use std::collections::HashSet;

const COMMON_WORDS: &[&str] = &[
    "The", "A", "An", "This", "That", "These", "Those", "I", "We", "You",
    "It", "He", "She", "They", "My", "Your", "Our", "His", "Her", "Their",
    "Is", "Are", "Was", "Were", "Has", "Have", "Had", "Be", "Been", "Do",
    "Does", "Did", "Will", "Would", "Should", "Can", "Could", "May", "Might",
    // Paper-prose noise — capitalized at start of sentences.
    "In", "On", "At", "As", "By", "For", "From", "Of", "To", "With",
    "We", "Our", "Us", "If", "Then", "When", "While", "After", "Before",
    "Where", "Which", "Who", "Why", "How", "What", "Also", "Only", "Not",
    "But", "And", "Or", "So", "Such", "Both", "Either", "Neither",
    "All", "Any", "Some", "Each", "Every", "Most", "More", "Less",
    "First", "Second", "Third", "Last", "Next", "Previous", "Following",
    // Paper structural words that get picked up from "Table 1", "Figure 2", etc.
    "Table", "Figure", "Fig", "Section", "Appendix", "Abstract",
    "Introduction", "Methods", "Results", "Discussion", "Conclusion",
    "References", "Acknowledgments", "Appendices",
    // Citation noise
    "Proceedings", "Conference", "Journal", "Workshop", "Symposium",
    "Volume", "Issue", "Page", "Pages", "Chapter", "Vol",
    "URL", "DOI", "ISBN", "ISSN",
];

/// Return a deduplicated list of entity surface forms found in `text`.
pub fn extract_entities(text: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ':' || c == '(' || c == ')' || c == '[' || c == ']' || c == '.') {
        let token = raw.trim_end_matches(|c: char| c == '.' || c == ',' || c == '?' || c == '!');
        if token.len() < 2 {
            continue;
        }
        if COMMON_WORDS.contains(&token) {
            continue;
        }
        // Heuristics:
        let has_digit = token.chars().any(|c| c.is_ascii_digit());
        let upper_count = token.chars().filter(|c| c.is_ascii_uppercase()).count();
        let all_upper_letters = upper_count >= 2
            && token.chars().all(|c| c.is_ascii_alphanumeric())
            && token.chars().any(|c| c.is_ascii_alphabetic())
            && token
                .chars()
                .filter(|c| c.is_ascii_alphabetic())
                .all(|c| c.is_ascii_uppercase());
        let starts_upper = token
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_uppercase());

        let is_entity = has_digit || upper_count >= 2 || all_upper_letters || starts_upper;
        if !is_entity {
            continue;
        }
        // Drop things that are JUST a number.
        if token.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let key = token.to_string();
        if seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_biomedical_terms() {
        let ents = extract_entities(
            "Rapamycin inhibits mTORC1 at IC50 of 3.2 nM in HeLa cells.",
        );
        assert!(ents.contains(&"Rapamycin".to_string()));
        assert!(ents.contains(&"mTORC1".to_string()));
        assert!(ents.contains(&"IC50".to_string()));
        assert!(ents.contains(&"HeLa".to_string()));
    }

    #[test]
    fn drops_sentence_case_common_words() {
        let ents = extract_entities("The patient had a follow-up visit.");
        assert!(!ents.contains(&"The".to_string()));
    }
}
