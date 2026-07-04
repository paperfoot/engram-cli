//! Deterministic entity extraction — no LLM, no network, no randomness.
//!
//! Extracts, in first-appearance order (deduplicated):
//! - Multi-word Title-Case / mixed phrases as ONE entity. A run of adjacent
//!   capitalized tokens — optionally bridged by a lowercase connective
//!   ("of"/"for"/"the"/…) or an interior number — collapses into a single
//!   surface form: "Tumor Necrosis Factor", "Mount Everest", "New York",
//!   "Cell Culture", "Type 2 Diabetes", "Centers for Disease Control".
//! - Standalone acronyms / biomedical terms: mTORC1, BRCA1, IL-6, p53, IC50,
//!   mRNA, siRNA (kept whether Title-Case, ALL-CAPS, or mixed-case).
//! - Any token carrying a digit next to a letter (p53, GPT-5, COVID-19).
//!
//! Filtering is sentence-aware. A lone Title-Case word that is capitalized
//! only because it opens a sentence AND reads as a common English word — a
//! stop-word ("The", "However"), a `-ly` opener ("Interestingly", "Notably"),
//! or a curated lab/structural noun ("Mice", "Patients", "Table") — is
//! dropped. Multi-word phrases, acronyms, and mid-sentence proper nouns are
//! kept. Adjacency is decided on the raw separators, so punctuation breaks a
//! phrase: "New York, New Jersey" yields two entities, never one.
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
    // Sentence-opening subjects / lab nouns (capitalized only at sentence head).
    // These never legitimately head a multi-word entity, so stop-listing them
    // is safe and prevents "Mice", "Patients", … leaking in as entities.
    "Here",
    "There",
    "Together",
    "Altogether",
    "Mice",
    "Mouse",
    "Rats",
    "Rat",
    "Patients",
    "Subjects",
    "Participants",
    "Controls",
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
    // Generic ALL-CAPS roman-numeral noise ("Section III", "Type II")
    "II",
    "III",
    "IV",
    "VI",
    "VII",
    "VIII",
    "IX",
];

/// Lowercase connective particles that may sit *inside* a proper-name phrase
/// ("Centers for Disease Control", "Bank of America", "Ludwig van Beethoven").
/// Deliberately excludes "and" and directional prepositions ("in", "on", "to"),
/// which more often join two *separate* entities than bind one.
const CONNECTORS: &[&str] = &[
    "of", "for", "the", "de", "la", "le", "von", "van", "der", "den", "di", "del", "della", "du",
    "des", "und", "dos", "das", "da", "el",
];

/// A character that can appear inside a single token (letters, digits, and the
/// intra-word joiners hyphen / underscore / apostrophe). Everything else is a
/// separator. `.`/`!`/`?` are separators *and* mark sentence boundaries.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == '\''
}

#[derive(Clone)]
struct Token {
    text: String,
    /// The gap since the previous token was pure whitespace (no punctuation).
    /// Phrases may only span tokens that are whitespace-adjacent.
    space_before: bool,
    /// This token begins a new sentence (start of text, or preceded by `.!?`).
    sentence_start: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Capitalized, non-stop token: can head or join a phrase, or stand alone.
    PhraseCap,
    /// Non-capitalized standalone entity (mTORC1, mRNA, p53) — never a phrase member.
    Solo,
    /// Lowercase connective particle; joins only *between* two PhraseCaps.
    Connector,
    /// Pure number; joins only *between* two PhraseCaps ("Type 2 Diabetes").
    Number,
    /// Dropped, and breaks any phrase run.
    Skip,
}

/// Split `text` into tokens, tracking whitespace-adjacency and sentence starts.
fn tokenize(text: &str) -> Vec<Token> {
    let mut toks: Vec<Token> = Vec::new();
    let mut cur = String::new();
    let mut gap_pure_ws = true;
    let mut pending_sentence = true;

    for c in text.chars() {
        if is_word_char(c) {
            cur.push(c);
            continue;
        }
        // Non-word char: close the current token, if any.
        if !cur.is_empty() {
            let trimmed = cur.trim_matches(|c: char| matches!(c, '-' | '_' | '\''));
            if !trimmed.is_empty() {
                toks.push(Token {
                    text: trimmed.to_string(),
                    space_before: gap_pure_ws,
                    sentence_start: pending_sentence,
                });
            }
            cur.clear();
            // Open a fresh gap for the next token.
            gap_pure_ws = true;
            pending_sentence = false;
        }
        // Fold this separator into the pending gap.
        if !c.is_whitespace() {
            gap_pure_ws = false;
        }
        if matches!(c, '.' | '!' | '?') {
            pending_sentence = true;
        }
    }
    if !cur.is_empty() {
        let trimmed = cur.trim_matches(|c: char| matches!(c, '-' | '_' | '\''));
        if !trimmed.is_empty() {
            toks.push(Token {
                text: trimmed.to_string(),
                space_before: gap_pure_ws,
                sentence_start: pending_sentence,
            });
        }
    }
    toks
}

/// Classify a token for phrase assembly and standalone emission.
fn classify(t: &Token) -> Kind {
    let text = t.text.as_str();
    let len = text.chars().count();

    let mut first_upper = false;
    let mut first_letter = false;
    let mut upper_count = 0usize;
    let mut internal_upper = false;
    let mut has_digit = false;
    let mut has_alpha = false;
    for (idx, c) in text.chars().enumerate() {
        if c.is_uppercase() {
            upper_count += 1;
            if idx > 0 {
                internal_upper = true;
            }
        }
        if c.is_alphabetic() {
            has_alpha = true;
        }
        if c.is_ascii_digit() {
            has_digit = true;
        }
        if idx == 0 {
            first_upper = c.is_uppercase();
            first_letter = c.is_alphabetic();
        }
    }

    // Pure numbers can still bind a phrase interior ("Type 2 Diabetes"); keep
    // them as Number regardless of length so single-digit tokens survive.
    if has_digit && !has_alpha {
        return Kind::Number;
    }

    // Lowercase connective particle.
    if !first_upper {
        let lower = text.to_ascii_lowercase();
        if CONNECTORS.contains(&lower.as_str()) {
            return Kind::Connector;
        }
    }

    // A lone Title-Case word (only its first letter capitalized) that opens a
    // sentence and reads like an adverb ("Interestingly", "Notably") is noise.
    // `Italy`/`July` (len < 6) are spared.
    let simple_title = first_upper && !has_digit && !internal_upper;
    let adverb_opener = t.sentence_start && simple_title && len >= 6 && text.ends_with("ly");

    if COMMON_WORDS.contains(&text) || adverb_opener {
        return Kind::Skip;
    }
    if len < 2 {
        return Kind::Skip;
    }
    if first_upper {
        return Kind::PhraseCap;
    }
    // Non-capitalized standalone entity: an internal/all-caps acronym (mRNA,
    // siRNA) or a letter-then-digit code (p53, p16). A single internal capital
    // (units like "nM", "mL") is intentionally NOT enough.
    if upper_count >= 2 || (first_letter && has_digit) {
        return Kind::Solo;
    }
    Kind::Skip
}

/// Return a deduplicated, first-appearance-ordered list of entity surface
/// forms found in `text`.
pub fn extract_entities(text: &str) -> Vec<String> {
    let toks = tokenize(text);
    let kinds: Vec<Kind> = toks.iter().map(classify).collect();
    let n = toks.len();

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let push = |seen: &mut HashSet<String>, out: &mut Vec<String>, s: String| {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    };

    let mut i = 0;
    while i < n {
        match kinds[i] {
            Kind::PhraseCap => {
                // Greedily extend a phrase from this capitalized head.
                let mut members: Vec<usize> = vec![i];
                let mut j = i + 1;
                while j < n {
                    if !toks[j].space_before {
                        break; // punctuation gap ends the phrase
                    }
                    match kinds[j] {
                        Kind::PhraseCap => {
                            members.push(j);
                            j += 1;
                        }
                        // Connector or interior number only bind when immediately
                        // followed by another whitespace-adjacent PhraseCap.
                        Kind::Connector | Kind::Number
                            if j + 1 < n
                                && toks[j + 1].space_before
                                && kinds[j + 1] == Kind::PhraseCap =>
                        {
                            members.push(j);
                            members.push(j + 1);
                            j += 2;
                        }
                        _ => break,
                    }
                }

                if members.len() >= 2 {
                    let phrase = members
                        .iter()
                        .map(|&m| toks[m].text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    push(&mut seen, &mut out, phrase);
                } else {
                    // Lone capitalized token. Stop-words / sentence-opening
                    // adverbs were already filtered to Skip during classify,
                    // so any surviving PhraseCap is a genuine surface form.
                    push(&mut seen, &mut out, toks[i].text.clone());
                }
                i = j;
            }
            Kind::Solo => {
                push(&mut seen, &mut out, toks[i].text.clone());
                i += 1;
            }
            // Skip / stray Connector / stray Number: dropped.
            _ => i += 1,
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
        // Units like "nM" carry only a single internal capital and are noise.
        assert!(!ents.contains(&"nM".to_string()));
    }

    #[test]
    fn drops_sentence_case_common_words() {
        let ents = extract_entities("The patient had a follow-up visit.");
        assert!(!ents.contains(&"The".to_string()));
    }

    #[test]
    fn multiword_phrase_is_one_entity() {
        let ents = extract_entities("Tumor Necrosis Factor is elevated in sepsis.");
        assert!(ents.contains(&"Tumor Necrosis Factor".to_string()));
        // The phrase must NOT fragment into its constituent words.
        assert!(!ents.contains(&"Tumor".to_string()));
        assert!(!ents.contains(&"Necrosis".to_string()));
        assert!(!ents.contains(&"Factor".to_string()));
    }

    #[test]
    fn acronyms_and_digit_terms_survive() {
        let ents = extract_entities("The mTORC1 axis and IL-6 both raised the IC50 sharply.");
        assert!(ents.contains(&"mTORC1".to_string()));
        assert!(ents.contains(&"IL-6".to_string()));
        assert!(ents.contains(&"IC50".to_string()));
    }

    #[test]
    fn drops_sentence_opening_adverb() {
        let ents = extract_entities("Interestingly, we found no effect.");
        assert!(!ents.contains(&"Interestingly".to_string()));
        // Nothing else in the sentence is an entity.
        assert!(ents.is_empty());
    }

    #[test]
    fn phrase_with_lowercase_connector() {
        let ents = extract_entities("The Centers for Disease Control issued guidance.");
        assert!(ents.contains(&"Centers for Disease Control".to_string()));
    }

    #[test]
    fn phrase_with_interior_number() {
        let ents = extract_entities("Patients with Type 2 Diabetes were enrolled.");
        assert!(ents.contains(&"Type 2 Diabetes".to_string()));
    }

    #[test]
    fn punctuation_breaks_phrase_runs() {
        let ents = extract_entities("We compared New York, New Jersey, and Ohio.");
        assert!(ents.contains(&"New York".to_string()));
        assert!(ents.contains(&"New Jersey".to_string()));
        assert!(ents.contains(&"Ohio".to_string()));
        // The comma-separated names must never merge across the punctuation.
        assert!(!ents.contains(&"New York New Jersey".to_string()));
    }

    #[test]
    fn deterministic_and_first_appearance_order() {
        let text = "Rapamycin blocks mTORC1. mTORC1 needs Rapamycin.";
        let a = extract_entities(text);
        let b = extract_entities(text);
        assert_eq!(a, b, "extraction must be deterministic");
        assert_eq!(
            a,
            vec!["Rapamycin".to_string(), "mTORC1".to_string()],
            "order must follow first appearance, deduplicated"
        );
    }
}
