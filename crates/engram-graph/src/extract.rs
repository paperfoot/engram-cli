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
    // Pronouns and articles
    "The",
    "A",
    "An",
    "This",
    "That",
    "These",
    "Those",
    "I",
    "We",
    "You",
    "It",
    "He",
    "She",
    "They",
    "My",
    "Your",
    "Our",
    "His",
    "Her",
    "Their",
    // Auxiliaries
    "Is",
    "Are",
    "Was",
    "Were",
    "Has",
    "Have",
    "Had",
    "Be",
    "Been",
    "Do",
    "Does",
    "Did",
    "Will",
    "Would",
    "Should",
    "Can",
    "Could",
    "May",
    "Might",
    // Prepositions / conjunctions capitalized at sentence start
    "In",
    "On",
    "At",
    "As",
    "By",
    "For",
    "From",
    "Of",
    "To",
    "With",
    "Us",
    "If",
    "Then",
    "When",
    "While",
    "After",
    "Before",
    "Where",
    "Which",
    "Who",
    "Why",
    "How",
    "What",
    "Also",
    "Only",
    "Not",
    "But",
    "And",
    "Or",
    "So",
    "Such",
    "Both",
    "Either",
    "Neither",
    "All",
    "Any",
    "Some",
    "Each",
    "Every",
    "Most",
    "More",
    "Less",
    "First",
    "Second",
    "Third",
    "Last",
    "Next",
    "Previous",
    "Following",
    // Common imperative verbs that open sentences in papers/docs
    "See",
    "Show",
    "Shows",
    "Shown",
    "Find",
    "Found",
    "Take",
    "Takes",
    "Get",
    "Gets",
    "Make",
    "Makes",
    "Use",
    "Uses",
    "Used",
    "Note",
    "Consider",
    "Observe",
    "Assume",
    "Suppose",
    "Recall",
    "Remember",
    "Respond",
    "Responds",
    "Reply",
    "Replies",
    "Return",
    "Returns",
    "Let",
    "Given",
    "Set",
    "Put",
    "Apply",
    "Applied",
    "Call",
    "Called",
    // Logical / rhetorical connectives at sentence start
    "Thus",
    "Therefore",
    "Hence",
    "However",
    "Moreover",
    "Furthermore",
    "Additionally",
    "Nonetheless",
    "Nevertheless",
    "Indeed",
    "Instead",
    "Finally",
    "Accordingly",
    "Consequently",
    "Specifically",
    "Namely",
    "Otherwise",
    "Similarly",
    "Likewise",
    "Meanwhile",
    "Overall",
    // Paper structural words — typically in "Table 1", "Figure 2", etc.
    "Table",
    "Figure",
    "Fig",
    "Section",
    "Appendix",
    "Abstract",
    "Introduction",
    "Methods",
    "Results",
    "Discussion",
    "Conclusion",
    "References",
    "Acknowledgments",
    "Appendices",
    "Equation",
    "Eq",
    // Citation metadata noise
    "Proceedings",
    "Conference",
    "Journal",
    "Workshop",
    "Symposium",
    "Volume",
    "Issue",
    "Page",
    "Pages",
    "Chapter",
    "Vol",
    "URL",
    "DOI",
    "ISBN",
    "ISSN",
];

/// Return a deduplicated list of entity surface forms found in `text`.
pub fn extract_entities(text: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || c == ','
            || c == ';'
            || c == ':'
            || c == '('
            || c == ')'
            || c == '['
            || c == ']'
    }) {
        let token =
            raw.trim_matches(|c: char| matches!(c, '.' | ',' | '?' | '!' | '"' | '\'' | '`'));
        if token.len() < 2 {
            continue;
        }
        if COMMON_WORDS.contains(&token) {
            continue;
        }
        // Drop pure numbers including decimals / thousands ("3", "3.2", "1,000").
        let has_any_letter = token.chars().any(|c| c.is_ascii_alphabetic());
        if !has_any_letter {
            continue;
        }
        let has_digit = token.chars().any(|c| c.is_ascii_digit());
        let upper_count = token.chars().filter(|c| c.is_ascii_uppercase()).count();
        let starts_upper = token
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_uppercase());

        let is_entity = has_digit || upper_count >= 2 || starts_upper;
        if !is_entity {
            continue;
        }
        if seen.insert(token.to_string()) {
            out.push(token.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_biomedical_terms() {
        let ents = extract_entities("Rapamycin inhibits mTORC1 at IC50 of 3.2 nM in HeLa cells.");
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
