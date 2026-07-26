//! Comparison keys for personal names, shared by the graph and the store.
//!
//! A man writes his own name "Francisco Ferró"; a chat export writes it
//! "Francisco Ferro". Compared byte for byte those are two people, which is how
//! the real store ended up with `Francisco Ferro`, `Francisco Ferró` and
//! `Francisco Ferró Oliver` as three rows for one man — and, worse, with three
//! rival "full names" under one first name, which is exactly the ambiguity that
//! stops a bare `Francisco` (42 mentions) from folding into any of them.
//!
//! So names are COMPARED accent-insensitively and DISPLAYED exactly as written:
//! everything here produces a lookup key, never a stored name.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Lowercase, accent-stripped form of `text` — a comparison key only.
///
/// Canonical decomposition first, so a precomposed `ó` and a decomposed
/// `o`+U+0301 (which is what macOS hands you) fold to the same thing.
pub fn accent_fold(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Accent-folded whitespace tokens of a name.
pub fn fold_tokens(name: &str) -> Vec<String> {
    name.split_whitespace().map(accent_fold).collect()
}

/// Is `fuller` the same name as `shorter` with more of it written out?
///
/// True when every token of `shorter` appears in `fuller` IN ORDER and `fuller`
/// has more tokens: "Patrick Pruett" → "Patrick Ashcomb Pruett" (a middle name
/// inserted), "Ming Su" → "Ming Su Lim" (a surname appended), "Noemi" → "Noemi
/// Varkoni".
///
/// ORDER is what keeps it honest. A messaging-app contact label ("Maria
/// Microsoft London Tech") is longer but does not contain "Marenova"; two
/// unrelated names ("Laura Corvo", "Laura Quiroz") share only their first
/// token; and a surname-first spelling ("Pruett Patrick") is a different
/// writing convention, not a fuller name, so none of them can take a canonical
/// slot away from a real one.
pub fn is_fuller_name(shorter: &str, fuller: &str) -> bool {
    let shorter = fold_tokens(shorter);
    let fuller = fold_tokens(fuller);
    !shorter.is_empty() && fuller.len() > shorter.len() && is_subsequence(&shorter, &fuller)
}

/// Do all of `needle`'s tokens appear in `haystack` in order?
pub fn is_subsequence(needle: &[String], haystack: &[String]) -> bool {
    let mut it = haystack.iter();
    needle.iter().all(|token| it.any(|h| h == token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_fold_for_comparison_but_never_for_display() {
        assert_eq!(accent_fold("Ferró"), "ferro");
        assert_eq!(accent_fold("FERRÓ"), "ferro");
        // Decomposed input (macOS) folds to the same key as precomposed.
        assert_eq!(accent_fold("Ferro\u{301}"), "ferro");
        assert_eq!(accent_fold("Brandtke-Wendel"), "brandtke-wendel");
        assert_eq!(accent_fold("Zoë Ångström"), "zoe angstrom");
    }

    #[test]
    fn a_fuller_name_inserts_or_appends_tokens() {
        assert!(is_fuller_name("Patrick Pruett", "Patrick Ashcomb Pruett"));
        assert!(is_fuller_name("Ming Su", "Ming Su Lim"));
        assert!(is_fuller_name("Noemi", "Noemi Varkoni"));
        assert!(is_fuller_name("Francisco Ferro", "Francisco Ferró Oliver"));
        assert!(is_fuller_name("FRANCISCO FERRÓ", "Francisco Ferró Oliver"));
    }

    #[test]
    fn a_label_a_rival_and_a_reversal_are_not_fuller_names() {
        // The junk-label hijack: longer, but not the same name.
        assert!(!is_fuller_name(
            "Maria Marenova",
            "Maria Microsoft London Tech"
        ));
        // Two women who share a first name.
        assert!(!is_fuller_name("Laura Corvo", "Laura Quiroz"));
        // Same tokens, other order — a convention, not more of the name.
        assert!(!is_fuller_name("Patrick Pruett", "Pruett Ashcomb Patrick"));
        // Never shorter, never equal, never from nothing.
        assert!(!is_fuller_name("Noemi Varkoni", "Noemi"));
        assert!(!is_fuller_name("Elias", "Elias"));
        assert!(!is_fuller_name("", "Anything At All"));
    }
}
