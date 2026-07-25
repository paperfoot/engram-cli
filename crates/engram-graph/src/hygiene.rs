//! Entity-surface hygiene — the filters that keep junk out of the entity layer.
//!
//! The deterministic extractor is capitalization-driven, which is exactly why
//! it used to emit `I'm`, `She's`, `It'll`, `Monday` and bare stop-words as
//! "entities", and why `Elias` and `Elias's` landed as two unrelated rows. A
//! personal-memory graph made of those is worse than no graph at all: every
//! junk node dilutes mention counts, co-occurrence edges and wiki pages.
//!
//! Three rules live here, shared by the offline extractor and the LLM
//! compiler path so both converge on the same canonical surface form:
//!
//! 1. **Contractions are never entities.** `I'm`, `She's`, `It'll`, `Don't` —
//!    a pronoun/auxiliary stem plus a contraction suffix — are dropped.
//! 2. **Possessives fold into their owner.** `Elias's` → canonical `Elias`
//!    with `Elias's` kept as an alias, so both spellings resolve to one node.
//! 3. **Bare function words and standalone dates are dropped.** Stop-words
//!    are matched case-insensitively; month/weekday names are dropped only
//!    when they stand alone (so `Monday Standup` survives as a phrase).

use std::collections::HashSet;
use std::sync::OnceLock;

/// Function words that are never entities on their own. Matched
/// case-insensitively, so `THE`, `The` and `the` are all rejected.
const STOPWORDS: &[&str] = &[
    // Pronouns, articles, determiners
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "those",
    "i",
    "we",
    "you",
    "it",
    "he",
    "she",
    "they",
    "me",
    "him",
    "them",
    "my",
    "mine",
    "your",
    "yours",
    "our",
    "ours",
    "his",
    "her",
    "hers",
    "its",
    "their",
    "theirs",
    "myself",
    "yourself",
    "himself",
    "herself",
    "itself",
    "ourselves",
    "themselves",
    // Auxiliaries / modals
    "is",
    "are",
    "was",
    "were",
    "am",
    "has",
    "have",
    "had",
    "be",
    "being",
    "been",
    "do",
    "does",
    "did",
    "done",
    "will",
    "would",
    "shall",
    "should",
    "can",
    "could",
    "may",
    "might",
    "must",
    // Prepositions / conjunctions
    "in",
    "on",
    "at",
    "as",
    "by",
    "for",
    "from",
    "of",
    "to",
    "with",
    "us",
    "if",
    "then",
    "than",
    "when",
    "while",
    "after",
    "before",
    "where",
    "which",
    "who",
    "whom",
    "whose",
    "why",
    "how",
    "what",
    "also",
    "only",
    "not",
    "no",
    "yes",
    "but",
    "and",
    "or",
    "nor",
    "so",
    "such",
    "both",
    "either",
    "neither",
    "all",
    "any",
    "some",
    "each",
    "every",
    "most",
    "more",
    "less",
    "few",
    "many",
    "much",
    "first",
    "second",
    "third",
    "last",
    "next",
    "previous",
    "following",
    "because",
    "since",
    "though",
    "although",
    "unless",
    "until",
    "whether",
    "during",
    "between",
    "among",
    "without",
    "within",
    "through",
    "across",
    "about",
    "against",
    "under",
    "over",
    "above",
    "below",
    "into",
    "onto",
    "upon",
    "per",
    "via",
    "plus",
    "versus",
    // Common imperative / reporting verbs that open a sentence
    "see",
    "show",
    "shows",
    "shown",
    "find",
    "found",
    "take",
    "takes",
    "get",
    "gets",
    "got",
    "make",
    "makes",
    "use",
    "uses",
    "used",
    "note",
    "consider",
    "observe",
    "assume",
    "suppose",
    "recall",
    "remember",
    "respond",
    "responds",
    "reply",
    "replies",
    "return",
    "returns",
    "let",
    "given",
    "set",
    "put",
    "apply",
    "applied",
    "call",
    "called",
    "went",
    "going",
    "gone",
    // Logical / rhetorical connectives
    "thus",
    "therefore",
    "hence",
    "however",
    "moreover",
    "furthermore",
    "additionally",
    "nonetheless",
    "nevertheless",
    "indeed",
    "instead",
    "finally",
    "accordingly",
    "consequently",
    "specifically",
    "namely",
    "otherwise",
    "similarly",
    "likewise",
    "meanwhile",
    "overall",
    "anyway",
    "besides",
    "maybe",
    "perhaps",
    "actually",
    "really",
    "just",
    "still",
    "even",
    "very",
    "well",
    "okay",
    "ok",
    "sure",
    "please",
    "sorry",
    "thanks",
    "thank",
    "hi",
    "hey",
    "hello",
    "yeah",
    "yep",
    "nope",
    // Sentence-opening subjects / lab nouns
    "here",
    "there",
    "together",
    "altogether",
    "mice",
    "mouse",
    "rats",
    "rat",
    "patients",
    "subjects",
    "participants",
    "controls",
    // Chat/diary filler that is capitalized only by position
    "today",
    "tomorrow",
    "yesterday",
    "tonight",
    "something",
    "someone",
    "somebody",
    "anyone",
    "anybody",
    "everyone",
    "everybody",
    "nobody",
    "nothing",
    "everything",
    "anything",
    // Paper structure
    "table",
    "figure",
    "fig",
    "section",
    "appendix",
    "abstract",
    "introduction",
    "methods",
    "results",
    "discussion",
    "conclusion",
    "references",
    "acknowledgments",
    "appendices",
    "equation",
    "eq",
    // Citation metadata
    "proceedings",
    "conference",
    "journal",
    "workshop",
    "symposium",
    "volume",
    "issue",
    "page",
    "pages",
    "chapter",
    "vol",
    "url",
    "doi",
    "isbn",
    "issn",
    // Roman-numeral noise ("Section III", "Type II")
    "ii",
    "iii",
    "iv",
    "vi",
    "vii",
    "viii",
    "ix",
];

/// Month and weekday names (plus common abbreviations). Dropped only when a
/// candidate entity is exactly one of them — `Monday Standup` still survives
/// as a multi-word phrase.
const TEMPORAL_WORDS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "tues",
    "wed",
    "thu",
    "thur",
    "thurs",
    "fri",
    "sat",
    "sun",
];

/// Suffixes that follow an apostrophe in an English contraction or possessive.
const CONTRACTION_SUFFIXES: &[&str] = &["s", "m", "re", "ve", "ll", "d", "t"];

/// Titles that precede a personal name. They are never entities on their own,
/// and they are stripped from the canonical form of a person ("Dr Jane Doe" →
/// "Jane Doe") so one human is one node.
pub const HONORIFICS: &[&str] = &[
    "dr",
    "mr",
    "mrs",
    "ms",
    "miss",
    "prof",
    "professor",
    "sir",
    "dame",
    "lord",
    "lady",
    "rev",
    "reverend",
    "father",
    "sister",
    "uncle",
    "aunt",
    "auntie",
    "grandma",
    "grandpa",
    "cousin",
    "captain",
    "capt",
    "coach",
    "chef",
];

/// Is `word` a personal title (trailing period optional)?
pub fn is_honorific(word: &str) -> bool {
    let lower = word.trim_end_matches('.').to_ascii_lowercase();
    HONORIFICS.contains(&lower.as_str())
}

fn stopword_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

fn temporal_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| TEMPORAL_WORDS.iter().copied().collect())
}

/// Is `text` a bare function word (case-insensitive)?
pub fn is_stopword(text: &str) -> bool {
    stopword_set().contains(text.to_ascii_lowercase().as_str())
}

/// Is `text` a month or weekday name?
pub fn is_temporal(text: &str) -> bool {
    temporal_set().contains(text.to_ascii_lowercase().as_str())
}

/// Split a token at its last apostrophe (ASCII `'` or typographic `’`).
fn split_apostrophe(token: &str) -> Option<(&str, &str)> {
    let idx = token.rfind(['\'', '\u{2019}'])?;
    let suffix_start = idx + token[idx..].chars().next().map(char::len_utf8).unwrap_or(1);
    Some((&token[..idx], &token[suffix_start..]))
}

/// Is this token a pronoun/auxiliary contraction (`I'm`, `She's`, `It'll`,
/// `Don't`)? Such tokens are never entities, however they are capitalized.
///
/// `n't` forms are recognized structurally (stem ends in `n`), so the real
/// name `Don` is untouched while `Don't` is rejected.
pub fn is_contraction(token: &str) -> bool {
    let Some((stem, suffix)) = split_apostrophe(token) else {
        return false;
    };
    if stem.is_empty() || suffix.is_empty() {
        return false;
    }
    let suffix_lower = suffix.to_ascii_lowercase();
    if !CONTRACTION_SUFFIXES.contains(&suffix_lower.as_str()) {
        return false;
    }
    if suffix_lower == "t" {
        // `n't`: don't, doesn't, isn't, won't, can't …
        return stem.chars().count() >= 2 && stem.to_ascii_lowercase().ends_with('n');
    }
    is_stopword(stem)
}

/// A cleaned-up entity surface form plus the spellings that should resolve to
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canonical {
    /// The canonical surface form stored as `entities.canonical_name`.
    pub name: String,
    /// Alternate spellings seen in the source text (possessives, contracted
    /// forms). Never contains `name` itself.
    pub aliases: Vec<String>,
}

/// Normalize one candidate entity surface form, or reject it as junk.
///
/// Returns `None` for contractions, bare stop-words, standalone month/weekday
/// names, and anything that normalizes to fewer than two characters.
pub fn canonicalize_surface(surface: &str) -> Option<Canonical> {
    let trimmed = surface.trim().trim_matches(|c: char| {
        c == '"' || c == '\u{201c}' || c == '\u{201d}' || c == ',' || c == ';' || c == ':'
    });
    if trimmed.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = trimmed
        .split_whitespace()
        .map(|s| s.trim_end_matches(['\'', '\u{2019}']).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }

    let original = parts.join(" ");
    let mut aliases = Vec::new();
    // Fold a trailing possessive/contraction on the LAST token only:
    // "Avi Roy's" -> "Avi Roy", "I'm" -> rejected.
    let last = parts[parts.len() - 1].clone();
    if let Some((stem, suffix)) = split_apostrophe(&last) {
        let suffix_lower = suffix.to_ascii_lowercase();
        if CONTRACTION_SUFFIXES.contains(&suffix_lower.as_str()) {
            if is_contraction(&last) && parts.len() == 1 {
                return None;
            }
            if stem.chars().count() >= 2 {
                let idx = parts.len() - 1;
                parts[idx] = stem.to_string();
                if suffix_lower == "s" {
                    // Possessive: keep the written form as an alias so a
                    // later lookup of "Elias's" still resolves.
                    aliases.push(original.clone());
                }
            }
        }
    }

    // "Dr Jane Doe" and "Dr. Jane Doe" must land on the same node as "Jane
    // Doe", so the title is folded into an alias.
    if parts.len() >= 2 && is_honorific(&parts[0]) {
        let full = parts.join(" ");
        parts.remove(0);
        if !aliases.contains(&full) {
            aliases.push(full);
        }
    }

    let name = parts.join(" ");
    if name.chars().count() < 2 {
        return None;
    }
    if parts.len() == 1 {
        let single = &parts[0];
        if is_stopword(single) || is_temporal(single) || is_honorific(single) {
            return None;
        }
    }
    aliases.retain(|a| a != &name);
    aliases.dedup();
    Some(Canonical { name, aliases })
}

/// Case-folded key used to deduplicate entity surface forms.
pub fn dedup_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pronoun_contractions_are_never_entities() {
        for junk in [
            "I'm", "She's", "It's", "I'll", "He's", "We're", "You're", "That's", "There's",
            "They've", "We'd", "Don't", "Doesn't", "Isn't", "Won't", "Can't",
        ] {
            assert!(
                canonicalize_surface(junk).is_none(),
                "{junk} must be rejected"
            );
        }
    }

    #[test]
    fn possessive_folds_into_owner_with_alias() {
        let c = canonicalize_surface("Elias's").expect("kept");
        assert_eq!(c.name, "Elias");
        assert_eq!(c.aliases, vec!["Elias's".to_string()]);

        let c = canonicalize_surface("Avi Roy's").expect("kept");
        assert_eq!(c.name, "Avi Roy");
        assert_eq!(c.aliases, vec!["Avi Roy's".to_string()]);
    }

    #[test]
    fn trailing_bare_apostrophe_folds_too() {
        let c = canonicalize_surface("Jones'").expect("kept");
        assert_eq!(c.name, "Jones");
    }

    #[test]
    fn real_names_with_apostrophes_survive() {
        let c = canonicalize_surface("O'Brien").expect("kept");
        assert_eq!(c.name, "O'Brien");
        assert!(c.aliases.is_empty());
        // "Don" must not be collateral damage from the n't rule.
        assert_eq!(canonicalize_surface("Don").unwrap().name, "Don");
    }

    #[test]
    fn bare_stopwords_and_dates_are_dropped() {
        for junk in ["The", "THE", "However", "Monday", "July", "Sept", "Friday"] {
            assert!(
                canonicalize_surface(junk).is_none(),
                "{junk} must be rejected"
            );
        }
        // …but a month inside a phrase is fine.
        assert_eq!(
            canonicalize_surface("Monday Standup").unwrap().name,
            "Monday Standup"
        );
    }

    #[test]
    fn dedup_key_is_case_and_space_folded() {
        assert_eq!(dedup_key("Avi  Roy"), dedup_key("avi roy"));
    }
}
