//! Deterministic grounding: the rules a judge result is compared against and
//! falls back to.
//!
//! Copied from `compile.rs` with identical behaviour, for the fact write path
//! and the judge eval. `compile.rs` still has its own copy until it calls
//! [`claim_grounded_lexically`]; change both or neither, or the shipped gate
//! drifts from the evaluated one. These rules are cheap and catch an
//! invented number, but they cannot see meaning: on SciFact's human labels the
//! word-overlap rule admits 69% of claims the passage CONTRADICTS, and no
//! lexical rule can see a negation (see the tests). The judge owns polarity;
//! numbers stay here, because the judge is weak at them.

use std::collections::HashSet;

/// A number's canonical digit string (drops separators/sign) for matching.
pub fn numeric_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || (c == '.' && !cur.is_empty()) {
            cur.push(c);
        } else {
            if cur.chars().any(|c| c.is_ascii_digit()) {
                out.push(cur.trim_matches('.').to_string());
            }
            cur.clear();
        }
    }
    if cur.chars().any(|c| c.is_ascii_digit()) {
        out.push(cur.trim_matches('.').to_string());
    }
    out
}

/// Tokenize into lowercase word tokens for word-boundary matching. Avoids the
/// substring bug where "rat" matched "temperature"/"literature" and "cell"
/// matched "excellent", mislabeling most chunks' evidence level.
pub fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

/// Every numeric value in the claim (two or more characters) appears in the
/// chunk, so a model cannot invent a dosage, p-value, or effect size.
pub fn numbers_ok(claim: &str, chunk: &str) -> bool {
    let chunk_nums = numeric_tokens(&chunk.to_ascii_lowercase());
    numeric_tokens(&claim.to_ascii_lowercase())
        .iter()
        .all(|n| n.len() < 2 || chunk_nums.iter().any(|c| c == n))
}

/// Is an LLM-extracted claim actually grounded in its source chunk?
///
/// Two requirements, both cheap and deterministic:
/// 1. Every numeric value in the claim must appear in the chunk (so the model
///    cannot invent a dosage, p-value, or effect size).
/// 2. The claim's content words must overlap the chunk enough that it is
///    plausibly a restatement of it, not a free-floating assertion.
pub fn claim_grounded_lexically(claim: &str, chunk: &str) -> bool {
    if !numbers_ok(claim, chunk) {
        return false;
    }
    let chunk_lower = chunk.to_ascii_lowercase();
    // Lexical overlap on content words (length >= 4 to skip stopwords).
    let chunk_words: HashSet<String> = word_set(&chunk_lower)
        .into_iter()
        .filter(|w| w.len() >= 4)
        .collect();
    let claim_words: Vec<String> = word_set(&claim.to_ascii_lowercase())
        .into_iter()
        .filter(|w| w.len() >= 4)
        .collect();
    if claim_words.is_empty() {
        return true; // nothing substantive to verify (rare)
    }
    let overlap = claim_words
        .iter()
        .filter(|w| chunk_words.contains(*w))
        .count();
    let ratio = overlap as f32 / claim_words.len() as f32;
    ratio >= 0.5
}

/// Are a fact's subject and object present in the chunk?
///
/// Stricter than the claim rule for the part a lexical rule can check: no
/// invented number in "subject object", and EVERY alphanumeric token of two
/// or more characters of the subject and of the object appears in the chunk
/// (case-insensitive). The predicate is not checked — no lexical rule can
/// tell "is allergic to" from "is not allergic to"; the judge does that.
pub fn fact_grounded_lexically(subject: &str, _predicate: &str, object: &str, chunk: &str) -> bool {
    if !numbers_ok(&format!("{subject} {object}"), chunk) {
        return false;
    }
    let chunk_words = word_set(chunk);
    [subject, object]
        .iter()
        .flat_map(|part| word_set(part))
        .filter(|w| w.len() >= 2)
        .all(|w| chunk_words.contains(&w))
}

/// Is a fact's object itself a negation ("no blood thinners", "none",
/// "celiac disease negative", "ruled out appendicitis")?
///
/// A member of a safety SET (a medication, an allergy, a diagnosis) is never
/// a negation: "Sam takes_medication no blood thinners" is an extraction that
/// put "Sam takes no blood thinners" into the wrong shape, and `state` would
/// list "no blood thinners" as a medication. The judge cannot see this — the
/// text does say it (p 0.82 and 0.91 in the remember eval) — so the shape is
/// checked here. Callers apply it to set-valued safety relations only (a
/// blood type of "O negative" is a value, not a negation).
pub fn negated_object(object: &str) -> bool {
    let o = object.trim().to_lowercase();
    const LEADING: [&str; 7] = [
        "no ",
        "not ",
        "never ",
        "without ",
        "negative for ",
        "ruled out ",
        "none",
    ];
    o == "none"
        || o == "nothing"
        || LEADING.iter().any(|p| o.starts_with(p))
        || o.ends_with(" negative")
        || o.ends_with(" ruled out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_negation_is_never_a_safety_set_member() {
        for o in [
            "no blood thinners",
            "None",
            "celiac disease negative",
            "ruled out appendicitis",
            "not allergic",
            "negative for HIV",
        ] {
            assert!(negated_object(o), "{o}");
        }
        for o in [
            "metformin 500 mg",
            "penicillin",
            "type 2 diabetes",
            "nonsteroidal anti-inflammatories",
            "notch inhibitor",
        ] {
            assert!(!negated_object(o), "{o}");
        }
    }

    #[test]
    fn grounding_gate_rejects_invented_numbers() {
        let chunk = "Rapamycin extended median lifespan by 14% in treated mice.";
        // Same numbers + overlap → grounded.
        assert!(claim_grounded_lexically(
            "Rapamycin extended lifespan by 14% in mice.",
            chunk
        ));
        // Invented number 34% not in chunk → rejected.
        assert!(!claim_grounded_lexically(
            "Rapamycin extended lifespan by 34% in mice.",
            chunk
        ));
        // No lexical overlap → rejected even with no numbers.
        assert!(!claim_grounded_lexically(
            "Metformin improves insulin sensitivity substantially.",
            chunk
        ));
    }

    #[test]
    fn numbers_ok_ignores_single_digits_and_trailing_dots() {
        assert!(numbers_ok("took 5 pills", "took five pills"));
        assert!(numbers_ok("dose 2.5.", "a dose of 2.5 mg"));
        assert!(!numbers_ok("dose 25 mg", "a dose of 2.5 mg"));
        assert_eq!(numeric_tokens("1,000 and 3.14."), vec!["1", "000", "3.14"]);
    }

    #[test]
    fn a_fact_about_someone_absent_from_the_chunk_is_not_grounded() {
        let weather = "Rain expected in Lisbon on Tuesday, clearing by the weekend.";
        assert!(!fact_grounded_lexically(
            "Bob",
            "diagnosed_with",
            "HIV",
            weather
        ));
    }

    #[test]
    fn lexical_rules_cannot_see_negation() {
        // Documented limitation, not a wish: the chunk says the OPPOSITE and
        // the lexical rule admits it. The judge's grounding question is what
        // catches this.
        assert!(fact_grounded_lexically(
            "Ana",
            "allergic_to",
            "penicillin",
            "Ana is not allergic to penicillin."
        ));
    }

    #[test]
    fn every_subject_and_object_token_must_appear() {
        let chunk = "Ana Silva moved to Lisbon in 2024.";
        assert!(fact_grounded_lexically(
            "Ana Silva",
            "lives_in",
            "Lisbon",
            chunk
        ));
        assert!(!fact_grounded_lexically(
            "Ana Costa",
            "lives_in",
            "Lisbon",
            chunk
        ));
        // Case-insensitive, single characters skipped.
        assert!(fact_grounded_lexically("ana", "lives_in", "LISBON", chunk));
        // An invented year fails the number rule.
        assert!(!fact_grounded_lexically("Ana", "moved_in", "2023", chunk));
    }
}
