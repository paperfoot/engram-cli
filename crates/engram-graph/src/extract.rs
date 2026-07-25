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
//! Every surviving surface form is then put through [`crate::hygiene`], which
//! rejects pronoun contractions ("I'm", "She's"), folds possessives onto their
//! owner ("Elias's" → "Elias"), drops standalone month/weekday names, and
//! case-folds the dedup key.
//!
//! This is intentionally cheap and fast. Phase 4+ can layer LLM-extracted
//! triples on top, but this baseline already populates the entities table
//! for co-occurrence edges.

use crate::hygiene::{self, Canonical};
use std::collections::HashSet;

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
pub(crate) struct Token {
    pub(crate) text: String,
    /// The gap since the previous token was pure whitespace (no punctuation).
    /// Phrases may only span tokens that are whitespace-adjacent.
    pub(crate) space_before: bool,
    /// This token begins a new sentence (start of text, or preceded by `.!?`).
    pub(crate) sentence_start: bool,
    /// Byte offset of this token's first char in the source text.
    pub(crate) start: usize,
    /// Byte offset one past this token's last char in the source text.
    pub(crate) end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
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

/// Split `text` into tokens, tracking whitespace-adjacency, sentence starts,
/// and byte offsets back into `text` (used to quote the exact source sentence
/// when a relation is extracted).
pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let mut toks: Vec<Token> = Vec::new();
    let mut cur = String::new();
    let mut cur_start = 0usize;
    let mut gap_pure_ws = true;
    let mut pending_sentence = true;

    let push_token = |toks: &mut Vec<Token>,
                      cur: &str,
                      cur_start: usize,
                      cur_end: usize,
                      gap_pure_ws: bool,
                      pending_sentence: bool| {
        let trimmed = cur.trim_matches(|c: char| matches!(c, '-' | '_' | '\''));
        if trimmed.is_empty() {
            return;
        }
        // Re-anchor the offsets onto the trimmed slice.
        let lead = cur.find(trimmed).unwrap_or(0);
        toks.push(Token {
            text: trimmed.to_string(),
            space_before: gap_pure_ws,
            sentence_start: pending_sentence,
            start: cur_start + lead,
            end: (cur_start + lead + trimmed.len()).min(cur_end),
        });
    };

    for (idx, c) in text.char_indices() {
        if is_word_char(c) {
            if cur.is_empty() {
                cur_start = idx;
            }
            cur.push(c);
            continue;
        }
        // Non-word char: close the current token, if any.
        if !cur.is_empty() {
            push_token(
                &mut toks,
                &cur,
                cur_start,
                idx,
                gap_pure_ws,
                pending_sentence,
            );
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
        push_token(
            &mut toks,
            &cur,
            cur_start,
            text.len(),
            gap_pure_ws,
            pending_sentence,
        );
    }
    toks
}

/// Classify a token for phrase assembly and standalone emission.
pub(crate) fn classify(t: &Token) -> Kind {
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

    // Stop-words are matched case-insensitively (so `THE` is dropped like
    // `The`), and pronoun contractions ("I'm", "She's", "It'll") never head or
    // join a phrase — they used to leak straight into the entities table.
    if hygiene::is_stopword(text) || hygiene::is_contraction(text) || adverb_opener {
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

/// One candidate entity occurrence, with the token span it covers so callers
/// (the person/relationship layer) can look at the surrounding words.
#[derive(Debug, Clone)]
pub(crate) struct Phrase {
    /// Raw surface form exactly as written ("Avi Roy's").
    pub(crate) surface: String,
    /// Index of the first token of the phrase.
    pub(crate) start: usize,
    /// Index one past the last token of the phrase.
    pub(crate) end: usize,
    /// Zero-based sentence index within the analyzed text.
    pub(crate) sentence: usize,
}

/// Assemble candidate entity occurrences in reading order, without dedup.
pub(crate) fn phrases(toks: &[Token], kinds: &[Kind]) -> Vec<Phrase> {
    let n = toks.len();
    let mut sentence_of = vec![0usize; n];
    let mut sentence = 0usize;
    for (idx, tok) in toks.iter().enumerate() {
        if tok.sentence_start && idx > 0 {
            sentence += 1;
        }
        sentence_of[idx] = sentence;
    }

    let mut out: Vec<Phrase> = Vec::new();
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
                let surface = members
                    .iter()
                    .map(|&m| toks[m].text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let end = members[members.len() - 1] + 1;
                out.push(Phrase {
                    surface,
                    start: i,
                    end,
                    sentence: sentence_of[i],
                });
                i = j;
            }
            Kind::Solo => {
                out.push(Phrase {
                    surface: toks[i].text.clone(),
                    start: i,
                    end: i + 1,
                    sentence: sentence_of[i],
                });
                i += 1;
            }
            // Skip / stray Connector / stray Number: dropped.
            _ => i += 1,
        }
    }
    out
}

/// Canonicalized candidate entities in reading order (duplicates included).
pub(crate) fn candidates(text: &str) -> (Vec<Token>, Vec<(Phrase, Canonical)>) {
    let toks = tokenize(text);
    let kinds: Vec<Kind> = toks.iter().map(classify).collect();
    let found = phrases(&toks, &kinds)
        .into_iter()
        .filter_map(|p| hygiene::canonicalize_surface(&p.surface).map(|c| (p, c)))
        .collect();
    (toks, found)
}

/// Return a deduplicated, first-appearance-ordered list of entity surface
/// forms found in `text`.
///
/// Surfaces are hygienic: contractions are gone, possessives are folded onto
/// their owner ("Elias's" → "Elias"), standalone month/weekday names are
/// dropped, and dedup is case-folded so "Rapamycin"/"rapamycin" is one entity.
pub fn extract_entities(text: &str) -> Vec<String> {
    let (_, found) = candidates(text);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for (_, canonical) in found {
        if seen.insert(hygiene::dedup_key(&canonical.name)) {
            out.push(canonical.name);
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
    fn pronoun_contractions_never_become_entities() {
        let ents = extract_entities(
            "I'm meeting Noemi tomorrow. She's bringing Avi. It's going to be useful. I'll confirm.",
        );
        for junk in ["I'm", "She's", "It's", "I'll", "I", "She", "It"] {
            assert!(
                !ents.contains(&junk.to_string()),
                "{junk} leaked in: {ents:?}"
            );
        }
        assert!(ents.contains(&"Noemi".to_string()));
        assert!(ents.contains(&"Avi".to_string()));
    }

    #[test]
    fn possessive_merges_with_owner() {
        let ents = extract_entities("Elias's calendar. Elias confirmed the date.");
        assert_eq!(
            ents.iter().filter(|e| e.starts_with("Elias")).count(),
            1,
            "Elias and Elias's must be one entity: {ents:?}"
        );
        assert!(ents.contains(&"Elias".to_string()));
    }

    #[test]
    fn standalone_months_and_weekdays_are_dropped() {
        let ents = extract_entities("We spoke on Monday. July was quiet.");
        assert!(!ents.contains(&"Monday".to_string()));
        assert!(!ents.contains(&"July".to_string()));
    }

    #[test]
    fn dedup_is_case_folded() {
        let ents = extract_entities("HeLa cells. hela cells again.");
        assert_eq!(
            ents.iter()
                .filter(|e| e.eq_ignore_ascii_case("hela"))
                .count(),
            1,
            "case variants must collapse: {ents:?}"
        );
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
