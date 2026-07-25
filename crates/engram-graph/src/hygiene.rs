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
//! 4. **Sentence-opening chat filler is dropped.** `Oh`, `Good`, `Morning`,
//!    `Congrats` are capitalized only by position. Like dates they are
//!    rejected only when they stand ALONE, so `Good Health Clinic` survives.
//! 5. **Greetings fold into the person they address.** `Heyy Elias`,
//!    `Good morning Noemi`, `Happy Birthday Avi` are one capitalized run to a
//!    capitalization-driven extractor; the greeting prefix is stripped so the
//!    mention lands on the human, not on a new node per salutation.
//! 6. **Messaging-app contact labels keep only their name.**
//!    `Maria Microsoft London Tech` is a phone contact nickname, not a human
//!    called Maria Microsoft: the leading personal-name run becomes the
//!    canonical form and the whole label is kept as an ALIAS, so the label
//!    still resolves to her without standing up a rival node.

use crate::gazetteer;
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

/// Interjections, greetings, and the acknowledgement noise that opens a chat
/// message. Rejected only when a candidate is EXACTLY one of them, exactly
/// like [`TEMPORAL_WORDS`]: "Good" alone is junk, "Good Health Clinic" is not.
///
/// This is the fix for the third junk class the real store exposed — the
/// extractor is capitalization-driven, so every "Oh", "Good", "Thanks", "Ok"
/// and "Morning" that opened a WhatsApp line became a `person` node.
const FILLER_WORDS: &[&str] = &[
    // Interjections
    "oh",
    "ah",
    "aha",
    "oops",
    "wow",
    "woah",
    "whoa",
    "haha",
    "hahaha",
    "hehe",
    "lol",
    "lmao",
    "omg",
    "hmm",
    "hm",
    "huh",
    "eh",
    "erm",
    "um",
    "uh",
    "yay",
    "woohoo",
    "aww",
    "aw",
    "ugh",
    "meh",
    "nah",
    "yup",
    "yeah",
    "yep",
    "nope",
    "yes",
    "no",
    // Acknowledgements and sign-offs
    "ok",
    "okay",
    "kk",
    "sure",
    "fine",
    "good",
    "great",
    "cool",
    "awesome",
    "amazing",
    "perfect",
    "excellent",
    "wonderful",
    "lovely",
    "brilliant",
    "congrats",
    "congratulations",
    "welcome",
    "cheers",
    "thanks",
    "thank",
    "thx",
    "ty",
    "please",
    "sorry",
    "bye",
    "goodbye",
    "goodnight",
    "hi",
    "hii",
    "hiii",
    "hey",
    "hello",
    "hallo",
    "heya",
    "hiya",
    "yo",
    "hola",
    "ciao",
    "salut",
    "dear",
    // Time-of-day greetings (capitalized only by position in "Morning Elias")
    "morning",
    "afternoon",
    "evening",
    "night",
    // Discourse filler
    "alright",
    "right",
    "true",
    "exactly",
    "absolutely",
    "definitely",
    "totally",
    "obviously",
    "honestly",
    "basically",
    "literally",
    "seriously",
    "anyway",
    "anyways",
    "btw",
    "fyi",
    "asap",
    "noted",
    "agreed",
    "understood",
    "done",
    "happy",
    "birthday",
    "sup",
];

/// Tokens that may LEAD a greeting run and are stripped from the front of a
/// candidate so the mention lands on the person addressed ("Heyy Elias" →
/// "Elias"). Deliberately narrower than [`FILLER_WORDS`]: "Good", "Great" and
/// "Nice" open real names and are only stripped as part of "Good morning …"
/// (see [`GOOD_TIMES`]).
const GREETING_PREFIXES: &[&str] = &[
    "hi",
    "hii",
    "hiii",
    "hey",
    "heya",
    "hiya",
    "hello",
    "hallo",
    "yo",
    "hola",
    "ciao",
    "salut",
    "dear",
    "oh",
    "ah",
    "yes",
    "no",
    "ok",
    "okay",
    "thanks",
    "thank",
    "thx",
    "congrats",
    "congratulations",
    "welcome",
    "bye",
    "goodbye",
    "goodnight",
    "morning",
    "afternoon",
    "evening",
    "happy",
    "birthday",
    "sorry",
    "please",
    "sup",
];

/// Second word of a "Good <time>" greeting. "Good" only leads a greeting in
/// this company, so "Good Health Clinic" is left intact.
const GOOD_TIMES: &[&str] = &["morning", "afternoon", "evening", "night", "day", "luck"];

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

fn filler_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| FILLER_WORDS.iter().copied().collect())
}

fn greeting_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| GREETING_PREFIXES.iter().copied().collect())
}

/// Chat spellings stretch a word's last letter ("Heyy", "Hiii", "Byeee").
/// Collapse a trailing run back to one character, but only for words of four
/// characters or more so a real short name ("Yoo", "Ann") is never rewritten.
fn collapse_trailing_repeat(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 4 {
        return word.to_string();
    }
    let mut end = chars.len();
    while end >= 2 && chars[end - 1] == chars[end - 2] {
        end -= 1;
    }
    chars[..end].iter().collect()
}

/// Is `text` chat filler — an interjection, greeting, or acknowledgement that
/// is capitalized only because it opened a message ("Oh", "Good", "Heyy")?
pub fn is_filler(text: &str) -> bool {
    let lower = text.trim_end_matches(['!', '?', ',']).to_ascii_lowercase();
    filler_set().contains(lower.as_str())
        || filler_set().contains(collapse_trailing_repeat(&lower).as_str())
}

/// Can `word` lead a greeting run that should be stripped from a mention?
fn is_greeting_prefix(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    greeting_set().contains(lower.as_str())
        || greeting_set().contains(collapse_trailing_repeat(&lower).as_str())
}

/// Strip a leading greeting run in place ("Heyy Elias" → "Elias", "Good
/// morning Noemi" → "Noemi"), leaving at least one token behind.
///
/// The greeting form is NOT kept as an alias: "Thanks Elias" is not a spelling
/// of anyone's name, and hoarding one alias per salutation would bury the real
/// ones ("Elias's", "Elias D.") that `people show` prints.
fn strip_greeting_prefix(parts: &mut Vec<String>) -> bool {
    let mut stripped = false;
    loop {
        if parts.len() < 2 {
            return stripped;
        }
        // "Good morning Elias" — "Good" only leads a greeting in this company.
        if parts.len() >= 3
            && parts[0].eq_ignore_ascii_case("good")
            && GOOD_TIMES.contains(&parts[1].to_ascii_lowercase().as_str())
        {
            parts.drain(..2);
            stripped = true;
            continue;
        }
        if is_greeting_prefix(&parts[0]) {
            parts.remove(0);
            stripped = true;
            continue;
        }
        return stripped;
    }
}

/// Does this token look like one component of a personal name — Title-Case (or
/// a single initial), and not a word that only ever appears in a contact label?
fn plausible_name_token(token: &str) -> bool {
    if gazetteer::is_label_marker(token) || is_stopword(token) || is_temporal(token) {
        return false;
    }
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    chars.all(|c| !c.is_ascii_digit())
}

/// Where the personal-name prefix of a messaging-app contact label ends.
///
/// "Maria Microsoft London Tech" is a phone contact nickname: a real given
/// name followed by the tag its owner used to tell two Marias apart. Returns
/// the number of leading tokens that are the actual name, or `None` when the
/// candidate is not a label (an ordinary name, a place like "New York", or a
/// label with no name in front of it like "Microsoft London").
fn contact_label_name_len(parts: &[String]) -> Option<usize> {
    if parts.len() < 2 || parts.len() > 6 {
        return None;
    }
    let whole = parts.join(" ");
    // A compound place is not a contact label — "New York Tech" must never be
    // shortened to a person called "New".
    if gazetteer::is_never_person(&whole) || gazetteer::is_place_modifier(&parts[0]) {
        return None;
    }
    if !plausible_name_token(&parts[0]) {
        return None;
    }
    // The name prefix runs until the first obvious non-name token, and a
    // personal name is at most three tokens long.
    let marker = parts
        .iter()
        .position(|p| gazetteer::is_label_marker(p))
        .filter(|idx| (1..=3).contains(idx))?;
    if !parts[..marker].iter().all(|p| plausible_name_token(p)) {
        return None;
    }
    Some(marker)
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Canonical {
    /// The canonical surface form stored as `entities.canonical_name`.
    pub name: String,
    /// Alternate spellings seen in the source text (possessives, contracted
    /// forms, the full contact label). Never contains `name` itself.
    pub aliases: Vec<String>,
    /// The surface form itself proved this is a human: it was a greeting
    /// ("Heyy Elias") or a messaging-app contact label ("Maria Microsoft
    /// London Tech"). Both address a person by construction, so the caller can
    /// type one bare token as a person without any other cue in the sentence.
    pub person_hint: bool,
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

    // "Heyy Elias", "Good morning Noemi", "Happy Birthday Avi" — one
    // capitalized run to the extractor, one greeting plus one human to a
    // reader. Strip the salutation so the mention counts for the human.
    let greeted = strip_greeting_prefix(&mut parts);

    // "Maria Microsoft London Tech" is a contact label: keep "Maria" as the
    // node and the whole label as an alias so the label still resolves to her.
    let mut labelled = false;
    if let Some(name_len) = contact_label_name_len(&parts) {
        let label = parts.join(" ");
        parts.truncate(name_len);
        if !aliases.contains(&label) {
            aliases.push(label);
        }
        labelled = true;
    }

    let name = parts.join(" ");
    if name.chars().count() < 2 {
        return None;
    }
    // Nothing but filler ("Good Morning", "Thanks Again") names no one, at any
    // length — otherwise a two-word salutation walks straight past the
    // single-token guard below and is typed as a person.
    if parts.iter().all(|p| is_filler(p)) {
        return None;
    }
    if parts.len() == 1 {
        let single = &parts[0];
        if is_stopword(single) || is_temporal(single) || is_honorific(single) || is_filler(single) {
            return None;
        }
    }
    aliases.retain(|a| a != &name);
    aliases.dedup();
    Some(Canonical {
        name,
        aliases,
        person_hint: greeted || labelled,
    })
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

    #[test]
    fn sentence_opening_filler_is_never_an_entity() {
        for junk in [
            "Oh", "Good", "Thanks", "Ok", "Sure", "Morning", "Congrats", "Heyy", "Wow", "Lol",
            "Perfect", "Amazing", "Happy",
        ] {
            assert!(
                canonicalize_surface(junk).is_none(),
                "{junk} must be rejected as chat filler"
            );
        }
        // A two-word salutation is filler too — it must not slip past the
        // single-token guard and become a person.
        assert!(canonicalize_surface("Good Morning").is_none());
        assert!(canonicalize_surface("Happy Birthday").is_none());
    }

    #[test]
    fn filler_rejection_leaves_real_phrases_alone() {
        // Only STANDALONE filler is dropped, exactly like month names.
        assert_eq!(
            canonicalize_surface("Good Health Clinic").unwrap().name,
            "Good Health Clinic"
        );
        assert_eq!(
            canonicalize_surface("Great Ormond Street").unwrap().name,
            "Great Ormond Street"
        );
    }

    #[test]
    fn greetings_fold_into_the_person_addressed() {
        for (surface, expected) in [
            ("Heyy Elias", "Elias"),
            ("Heyyy Elias", "Elias"),
            ("Hi Elias", "Elias"),
            ("Hey Noemi", "Noemi"),
            ("Good morning Noemi", "Noemi"),
            ("Good Morning Noemi", "Noemi"),
            ("Happy Birthday Avi Roy", "Avi Roy"),
            ("Thanks Elias", "Elias"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must fold onto {expected}");
            assert!(c.person_hint, "{surface} addresses a human");
            // The salutation is noise, not a spelling of anyone's name.
            assert!(
                c.aliases.is_empty(),
                "{surface} must not hoard a greeting alias: {:?}",
                c.aliases
            );
        }
        // A short name is never rewritten by the repeated-letter rule.
        assert_eq!(canonicalize_surface("Yoo Jin").unwrap().name, "Yoo Jin");
    }

    #[test]
    fn contact_labels_keep_the_name_and_alias_the_label() {
        let c = canonicalize_surface("Maria Microsoft London Tech").expect("kept");
        assert_eq!(c.name, "Maria");
        assert_eq!(
            c.aliases,
            vec!["Maria Microsoft London Tech".to_string()],
            "the label must stay resolvable as an alias"
        );
        assert!(c.person_hint, "a contact label names a human");

        let c = canonicalize_surface("Ana Lopez Gym").expect("kept");
        assert_eq!(c.name, "Ana Lopez");
        assert_eq!(c.aliases, vec!["Ana Lopez Gym".to_string()]);
    }

    #[test]
    fn label_repair_never_truncates_a_place_or_a_bare_company() {
        // "New York Tech" must not become a person called "New".
        assert_eq!(
            canonicalize_surface("New York Tech").unwrap().name,
            "New York Tech"
        );
        assert_eq!(canonicalize_surface("New York").unwrap().name, "New York");
        // A label with no personal name in front of it keeps its full form.
        assert_eq!(
            canonicalize_surface("Microsoft London").unwrap().name,
            "Microsoft London"
        );
        // An ordinary two-token human name is untouched.
        let c = canonicalize_surface("Maria Marenova").unwrap();
        assert_eq!(c.name, "Maria Marenova");
        assert!(!c.person_hint);
    }
}
