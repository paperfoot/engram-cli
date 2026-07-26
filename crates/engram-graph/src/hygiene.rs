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
//! 7. **Relationship words and sentence-opening verbs are stripped the same
//!    way greetings are.** `Partner Rosalie`, `Wants Elias` and `Told Noemi`
//!    are one capitalized run to the extractor and were typed as people with
//!    64, 28 and 11 mentions in the real store. The leading word is dropped so
//!    the mention counts for the human, and — like a greeting — it is NOT kept
//!    as an alias, because "Partner Rosalie" is nobody's name. Standing alone,
//!    `Partner`, `Doctor`, `Said` and `Says` are rejected outright. Gerunds
//!    (`Reading Lira`) lead a clause exactly like the finite forms and are
//!    stripped with them.
//! 8. **A possessive followed by more words names a THING, not a person.**
//!    `Noemi's Southwark`, `Avi's One Blackfriars`, `Noemi Varkoni's Lipitrex`
//!    are a flat, a party and a prescription. Only the owner survives, and the
//!    compound is NOT kept as an alias — it is no spelling of anyone's name.
//!    (A bare possessive is different: `Elias's` still folds onto `Elias` and
//!    keeps the written form as an alias — see rule 2.)
//! 9. **Dates and periods terminate a name.** `Maria Nov-2025`, `Maria
//!    June-2026`, `Maria Ibiza-period` are a woman plus a timestamp; the
//!    leading personal-name run is kept and the rest dropped.
//! 10. **A SHOUTED verb or adverb after a name is emphasis, not a surname.**
//!     `MARIA RE-INITIATES AGAIN`, `Maria Marenova PERSONALLY ORGANISES`.
//!     Truncated only on evidence that the caps token is a verb/adverb, or
//!     that the name in front of it is already complete — real ALL-CAPS
//!     surnames exist, and a shouted WhatsApp header is a whole name.
//! 11. **Profile slugs are never names.** `maria-marenova-9398b6108` is a
//!     LinkedIn URL fragment: hyphenated lowercase words plus an opaque id.
//!
//! Rules 8–11 exist because JUNK ENTITIES BLOCK UNIFICATION. The fold that
//! merges a bare first name into a full name refuses when two full names
//! compete for it (two Lauras must never fuse), and on the real store
//! "Maria Ibiza-period" and "Noemi Varkoni's Lipitrex" were exactly such
//! rival "full names" — which is why "Maria" (431 mentions) never joined
//! "Maria Marenova" (22), and "Noemi" (119) never joined "Noemi Varkoni"
//! (54). Deleting the junk is what repairs the merge.

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

/// Verbs that open a clause and glue themselves to the next capitalized word
/// ("Wants Elias", "Said hi", "Says Maria"). They belong to the same class as
/// [`FILLER_WORDS`] — capitalized only by position — so they are rejected
/// standing alone AND stripped from the front of a candidate.
///
/// These say nothing about whether what follows is human, so stripping one
/// does not set [`Canonical::person_hint`]; see [`HUMAN_OBJECT_VERBS`] for the
/// ones that do.
///
/// The `-ing` forms are here for the same reason the finite forms are:
/// "Reading Lira" opened a WhatsApp line 449 times in the real store and was
/// typed as a person, because v3 only knew the finite verbs. Gerunds that are
/// as often a noun modifier ("Meeting Notes", "Booking Reference", "Training
/// Plan") are deliberately absent — stripping those would invent an entity
/// called "Notes".
const LEADING_VERBS: &[&str] = &[
    "said",
    "says",
    "saying",
    "wants",
    "want",
    "wanted",
    "needs",
    "needed",
    "thinks",
    "think",
    "thought",
    "feels",
    "felt",
    "likes",
    "liked",
    "loves",
    "loved",
    "hates",
    "hated",
    "mentioned",
    "mentions",
    "replied",
    "replies",
    "confirmed",
    "confirms",
    "promised",
    "promises",
    "suggested",
    "suggests",
    "offered",
    "offers",
    "decided",
    "decides",
    "forgot",
    "forgets",
    "sent",
    "sends",
    "wrote",
    "writes",
    "came",
    "comes",
    "saw",
    "sees",
    "brought",
    "brings",
    // Gerunds — same clause-opening role, same treatment.
    "reading",
    "writing",
    "sending",
    "wanting",
    "needing",
    "thinking",
    "feeling",
    "liking",
    "loving",
    "hating",
    "mentioning",
    "replying",
    "confirming",
    "promising",
    "suggesting",
    "offering",
    "deciding",
    "forgetting",
    "seeing",
    "bringing",
    "chasing",
    "collecting",
    "fetching",
    "dropping",
    "picking",
    "watching",
    "helping",
    "reminding",
];

/// Verbs whose object is a human by construction ("Told Noemi", "Met Elias",
/// "Thanked Avi"). Same treatment as [`LEADING_VERBS`], except that stripping
/// one is itself the interpersonal evidence — the cue would otherwise be
/// invisible, because it sits INSIDE the candidate's own token span and the
/// evidence scan skips those.
const HUMAN_OBJECT_VERBS: &[&str] = &[
    "told",
    "tells",
    "asked",
    "asks",
    "met",
    "meets",
    "texted",
    "texts",
    "emailed",
    "emails",
    "messaged",
    "messages",
    "phoned",
    "phones",
    "pinged",
    "pings",
    "thanked",
    "thanks",
    "invited",
    "invites",
    "introduced",
    "introduces",
    "hired",
    "hires",
    "married",
    "marries",
    "visited",
    "visits",
    "knows",
    "knew",
    // Gerunds whose object is still a human ("Telling Noemi", "Calling Avi").
    "telling",
    "asking",
    "texting",
    "emailing",
    "messaging",
    "phoning",
    "pinging",
    "thanking",
    "inviting",
    "introducing",
    "visiting",
    "calling",
    "congratulating",
];

/// Words for a human role or family tie. "Partner Rosalie" is a relationship
/// plus a name, not a name — the run is one capitalized phrase only because
/// the sentence started there.
///
/// Given names that read like roles are deliberately absent: Angel, Faith,
/// Grace, Hope, Prince, King, Earl, Duke, Major, Mason. Losing "Angel the
/// friend" costs a person; keeping "angel the investor label" costs nothing.
const RELATIONSHIP_WORDS: &[&str] = &[
    "partner",
    "friend",
    "bestie",
    "colleague",
    "coworker",
    "co-worker",
    "boss",
    "wife",
    "husband",
    "spouse",
    "ex",
    "girlfriend",
    "boyfriend",
    "fiance",
    "fiancee",
    "brother",
    "sister",
    "stepbrother",
    "stepsister",
    "mother",
    "father",
    "stepmother",
    "stepfather",
    "mom",
    "mum",
    "mommy",
    "mummy",
    "dad",
    "daddy",
    "son",
    "daughter",
    "cousin",
    "uncle",
    "aunt",
    "auntie",
    "nephew",
    "niece",
    "grandma",
    "grandpa",
    "granny",
    "grandmother",
    "grandfather",
    "neighbour",
    "neighbor",
    "roommate",
    "flatmate",
    "housemate",
    "classmate",
    "teammate",
    "landlord",
    "landlady",
    "tenant",
    "doctor",
    "dentist",
    "therapist",
    "physio",
    "lawyer",
    "solicitor",
    "accountant",
    "nanny",
    "babysitter",
    "cleaner",
    "gardener",
    "plumber",
    "electrician",
    "builder",
    "handyman",
    "barber",
    "hairdresser",
    "trainer",
    "driver",
    "mentor",
    "mentee",
    "assistant",
    "secretary",
    "recruiter",
    "teacher",
    "student",
    "client",
    "customer",
    "investor",
    "supplier",
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

/// Nouns that turn the token they end into a span of TIME rather than a name
/// ("Ibiza-period", "lockdown-era"). Only ever consulted as the tail of a
/// hyphenated token, so an ordinary word is unaffected.
const PERIOD_WORDS: &[&str] = &[
    "period",
    "periods",
    "era",
    "eras",
    "phase",
    "phases",
    "window",
    "stint",
    "season",
    "seasons",
    "timeframe",
    "days",
    "weeks",
    "months",
    "years",
    "decade",
    "decades",
];

/// Verbs and adverbs that a writer SHOUTS after a name for emphasis. None of
/// them is anybody's surname, which is what makes truncating at one safe;
/// see [`shouted_stop_index`] for the two other, structural triggers.
const SHOUTED_STOP_WORDS: &[&str] = &[
    // Adverbs of emphasis
    "once",
    "again",
    "always",
    "never",
    "already",
    "finally",
    "personally",
    "officially",
    "apparently",
    "urgently",
    "immediately",
    "repeatedly",
    "constantly",
    "definitely",
    "absolutely",
    "seriously",
    "literally",
    "actually",
    "really",
    // Verbs a chat line shouts about a person
    "initiate",
    "initiates",
    "initiated",
    "organise",
    "organises",
    "organised",
    "organize",
    "organizes",
    "organized",
    "arrange",
    "arranges",
    "arranged",
    "cancel",
    "cancels",
    "cancelled",
    "canceled",
    "insists",
    "insisted",
    "refuses",
    "refused",
    "agrees",
    "agreed",
    "demands",
    "demanded",
    "promises",
    "promised",
    "confirms",
    "confirmed",
    "decides",
    "decided",
    "wants",
    "needs",
    "says",
    "said",
];

/// Prefixes that make a hyphenated ALL-CAPS token a VERB ("RE-INITIATES",
/// "OVER-PROMISES"). Name prefixes — Al-, Ben-, Mc-, O'- — are deliberately
/// absent, so "AL-FARSI" is never truncated.
const VERBAL_HYPHEN_PREFIXES: &[&str] = &["re", "un", "over", "under", "out", "mis", "pre"];

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
    SET.get_or_init(|| {
        FILLER_WORDS
            .iter()
            .chain(LEADING_VERBS)
            .chain(HUMAN_OBJECT_VERBS)
            .copied()
            .collect()
    })
}

fn greeting_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| GREETING_PREFIXES.iter().copied().collect())
}

fn relationship_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| RELATIONSHIP_WORDS.iter().copied().collect())
}

fn verb_prefix_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| LEADING_VERBS.iter().copied().collect())
}

fn human_object_verb_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| HUMAN_OBJECT_VERBS.iter().copied().collect())
}

/// Is `text` a word for a human role or family tie ("partner", "doctor")?
/// Such a word names a relationship, never a person, so it is never an entity
/// on its own.
pub fn is_role_word(text: &str) -> bool {
    relationship_set().contains(
        text.trim_end_matches([':', ','])
            .to_ascii_lowercase()
            .as_str(),
    )
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

/// Strip the leading words that are not part of anybody's name — a greeting
/// ("Heyy Elias" → "Elias", "Good morning Noemi" → "Noemi"), a relationship
/// word ("Partner Rosalie" → "Rosalie") or a clause-opening verb ("Wants
/// Elias" → "Elias") — leaving at least one token behind.
///
/// None of them is kept as an alias: "Thanks Elias" and "Partner Rosalie" are
/// not spellings of anyone's name, and hoarding one alias per salutation would
/// bury the real ones ("Elias's", "Elias D.") that `people show` prints.
///
/// Returns `true` when what was stripped PROVES a human follows — a greeting
/// addresses someone, "my partner X" describes someone, "told X" has a human
/// object. A plain verb ("Wants X") proves nothing and returns `false`.
fn strip_leading_noise(parts: &mut Vec<String>) -> bool {
    let mut human = false;
    loop {
        if parts.len() < 2 {
            return human;
        }
        // "Good morning Elias" — "Good" only leads a greeting in this company.
        if parts.len() >= 3
            && parts[0].eq_ignore_ascii_case("good")
            && GOOD_TIMES.contains(&parts[1].to_ascii_lowercase().as_str())
        {
            parts.drain(..2);
            human = true;
            continue;
        }
        let head = parts[0].to_ascii_lowercase();
        if is_greeting_prefix(&parts[0])
            || relationship_set().contains(head.as_str())
            || human_object_verb_set().contains(head.as_str())
        {
            parts.remove(0);
            human = true;
            continue;
        }
        if verb_prefix_set().contains(head.as_str()) {
            parts.remove(0);
            continue;
        }
        return human;
    }
}

/// Split a token into its hyphen-separated components, lowercased.
fn hyphen_parts(token: &str) -> Vec<String> {
    token
        .to_ascii_lowercase()
        .split('-')
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

/// A four-digit year (1000–2999). Shorter numbers are left alone so the
/// phrase interior of "Type 2 Diabetes" is not mistaken for a date.
fn is_year(token: &str) -> bool {
    token.len() == 4 && token.chars().all(|c| c.is_ascii_digit()) && token.starts_with(['1', '2'])
}

/// Does this token stamp a DATE or a PERIOD onto the name in front of it?
///
/// "Nov-2025", "June-2026", "Ibiza-period", a bare month, a bare year. Real
/// hyphenated names ("Jean-Pierre", "Lloyd-Webber") carry neither a year nor a
/// period noun and are untouched.
fn is_date_or_period_token(token: &str) -> bool {
    if is_temporal(token) || is_year(token) {
        return true;
    }
    let parts = hyphen_parts(token);
    if parts.len() < 2 {
        return false;
    }
    if parts.iter().any(|p| is_temporal(p))
        && parts
            .iter()
            .any(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() >= 2)
    {
        return true;
    }
    parts
        .last()
        .is_some_and(|last| PERIOD_WORDS.contains(&last.as_str()))
}

/// Is this token SHOUTED — two or more characters, at least one uppercase
/// letter, and no lowercase one ("MARIA", "RE-INITIATES")?
fn is_shouted(token: &str) -> bool {
    token.chars().count() >= 2
        && token.chars().any(|c| c.is_uppercase())
        && !token.chars().any(|c| c.is_lowercase())
}

/// A LinkedIn/profile URL fragment: hyphenated lowercase words plus an opaque
/// alphanumeric id ("maria-marenova-9398b6108"). Never a person, never worth a
/// node.
///
/// Three components are required, and one of them must mix letters and digits
/// over at least five characters — which is what keeps "SARS-CoV-2" and
/// "HIF-1alpha" out of it.
fn is_identifier_slug(token: &str) -> bool {
    let parts = hyphen_parts(token);
    if parts.len() < 3 {
        return false;
    }
    let word_run = parts
        .iter()
        .any(|p| p.chars().count() >= 3 && p.chars().all(|c| c.is_ascii_alphabetic()));
    let opaque_id = parts.iter().any(|p| {
        p.chars().count() >= 5
            && p.chars().any(|c| c.is_ascii_digit())
            && p.chars().any(|c| c.is_ascii_alphabetic())
    });
    word_run && opaque_id
}

/// Index of the first token that carries a possessive `'s` while MORE tokens
/// follow it — the shape of a thing that belongs to somebody ("Noemi's
/// Southwark", "Noemi Varkoni's Lipitrex", "Avi's One Blackfriars").
///
/// A possessive on the LAST token is a different animal: "Elias's" is a
/// spelling of Elias and is handled (with an alias) by the caller.
fn possessive_owner_end(parts: &[String]) -> Option<usize> {
    if parts.len() < 2 {
        return None;
    }
    for (idx, token) in parts.iter().enumerate().take(parts.len() - 1) {
        let Some((stem, suffix)) = split_apostrophe(token) else {
            continue;
        };
        if !suffix.eq_ignore_ascii_case("s") || stem.chars().count() < 2 {
            continue;
        }
        // "It's Friday" is a contraction, not an owner.
        if is_stopword(stem) || is_filler(stem) {
            continue;
        }
        return Some(idx);
    }
    None
}

/// Index at which a date or period token terminates the name ("Maria
/// Nov-2025" → 1). Never zero: a candidate that STARTS with a date was
/// already handled by the standalone-temporal rule.
fn date_stop_index(parts: &[String]) -> Option<usize> {
    (1..parts.len()).find(|&idx| is_date_or_period_token(&parts[idx]))
}

/// Index at which a SHOUTED verb/adverb terminates the name.
///
/// Three triggers, in ascending order of how much they assume:
/// 1. the shouted word is a known verb or adverb ("MARIA ONCE");
/// 2. it is hyphenated behind a verbal prefix ("MARIA RE-INITIATES") — no
///    surname is spelled that way;
/// 3. the name in front of it is a complete, NOT-shouted first+last name
///    ("Maria Marenova PERSONALLY"). Requiring the prefix to be mixed-case is
///    what protects a wholly shouted WhatsApp header: "MARIA MARENOVA" stays
///    one name, and so does "PATRICK ASHCOMB PRUETT".
fn shouted_stop_index(parts: &[String]) -> Option<usize> {
    (1..parts.len()).find(|&idx| {
        let token = &parts[idx];
        if !is_shouted(token) {
            return false;
        }
        let lower = token.to_ascii_lowercase();
        if SHOUTED_STOP_WORDS.contains(&lower.as_str()) || is_filler(token) {
            return true;
        }
        let hyphenated = hyphen_parts(token);
        if hyphenated.len() >= 2
            && VERBAL_HYPHEN_PREFIXES.contains(&hyphenated[0].as_str())
            && hyphenated[1].chars().count() >= 3
        {
            return true;
        }
        idx >= 2 && !parts[..idx].iter().any(|p| is_shouted(p))
    })
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
    // A profile slug is not a name in any position.
    if parts.iter().any(|p| is_identifier_slug(p)) {
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

    // "Noemi's Southwark", "Avi's One Blackfriars", "Noemi Varkoni's Lipitrex"
    // — a possessive with more words behind it names a FLAT, a PARTY, a
    // PRESCRIPTION. Keep the owner, drop the rest, and keep no alias: the
    // compound is not a spelling of her name, and as a rival "full name" it
    // was blocking the very fold that should have merged her two rows.
    if let Some(owner_end) = possessive_owner_end(&parts) {
        parts.truncate(owner_end + 1);
        if let Some((stem, _)) = split_apostrophe(&parts[owner_end]) {
            parts[owner_end] = stem.to_string();
        }
        // Any alias collected so far spells the compound, not the owner.
        aliases.clear();
    }

    // "Heyy Elias", "Good morning Noemi", "Partner Rosalie", "Wants Elias" —
    // one capitalized run to the extractor, one piece of noise plus one human
    // to a reader. Strip the prefix so the mention counts for the human. Runs
    // BEFORE the honorific fold: a title ("Dr Patel") is a name spelling worth
    // keeping as an alias, a relationship word ("Partner Rosalie") never is.
    let greeted = strip_leading_noise(&mut parts);

    // "Dr Jane Doe" and "Dr. Jane Doe" must land on the same node as "Jane
    // Doe", so the title is folded into an alias.
    if parts.len() >= 2 && is_honorific(&parts[0]) {
        let full = parts.join(" ");
        parts.remove(0);
        if !aliases.contains(&full) {
            aliases.push(full);
        }
    }

    // "Maria Nov-2025", "Maria Ibiza-period", "MARIA RE-INITIATES AGAIN",
    // "Maria Marenova PERSONALLY ORGANISES" — a name plus a timestamp or a
    // shouted clause. Cut at whichever comes first and keep the name; the
    // tail is nobody's name, so it is not kept as an alias either.
    if let Some(stop) = [date_stop_index(&parts), shouted_stop_index(&parts)]
        .into_iter()
        .flatten()
        .min()
    {
        parts.truncate(stop);
    }

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
        // A relationship word standing alone ("Partner", "Doctor") names a
        // role, never a human — the same verdict filler and titles get.
        if is_stopword(single)
            || is_temporal(single)
            || is_honorific(single)
            || is_filler(single)
            || is_role_word(single)
        {
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
    fn a_relationship_word_folds_into_the_person_it_describes() {
        for (surface, expected) in [
            ("Partner Rosalie", "Rosalie"),
            ("Friend Rosalie", "Rosalie"),
            ("Brother Milan", "Milan"),
            ("Sister Maria Marenova", "Maria Marenova"),
            ("Boss Elias", "Elias"),
            ("Doctor Patel", "Patel"),
            ("Neighbour Noemi", "Noemi"),
            ("Colleague Wei Zhang", "Wei Zhang"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must fold onto {expected}");
            assert!(c.person_hint, "{surface} describes a human");
            // "Partner Rosalie" is nobody's name — it must not be hoarded.
            assert!(
                c.aliases.is_empty(),
                "{surface} left a junk alias: {:?}",
                c.aliases
            );
        }
    }

    #[test]
    fn a_relationship_word_alone_is_never_an_entity() {
        for junk in [
            "Partner",
            "Doctor",
            "Friend",
            "Boss",
            "Colleague",
            "Wife",
            "Husband",
            "Brother",
            "Neighbour",
            "Landlord",
        ] {
            assert!(
                canonicalize_surface(junk).is_none(),
                "{junk} names a role, not a person"
            );
        }
    }

    #[test]
    fn given_names_that_read_like_roles_still_survive() {
        // The relationship list deliberately omits these — see its docs.
        for name in ["Angel", "Grace", "Faith", "Hope", "Prince", "Mason", "Duke"] {
            assert_eq!(
                canonicalize_surface(name).map(|c| c.name),
                Some(name.to_string()),
                "{name} is a given name, not a role"
            );
        }
    }

    #[test]
    fn a_clause_opening_verb_folds_into_the_name_behind_it() {
        for (surface, expected) in [
            ("Wants Elias", "Elias"),
            ("Said Maria", "Maria"),
            ("Says Noemi", "Noemi"),
            ("Told Noemi", "Noemi"),
            ("Asked Avi Roy", "Avi Roy"),
            ("Sent Elias", "Elias"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must fold onto {expected}");
            assert!(
                c.aliases.is_empty(),
                "{surface} left a junk alias: {:?}",
                c.aliases
            );
        }
        // A verb whose object is a human by construction is itself evidence;
        // a plain verb proves nothing about what follows it.
        assert!(canonicalize_surface("Told Noemi").unwrap().person_hint);
        assert!(!canonicalize_surface("Wants Rapamycin").unwrap().person_hint);
    }

    #[test]
    fn a_clause_opening_verb_alone_is_never_an_entity() {
        for junk in [
            "Said", "Says", "Told", "Asked", "Wants", "Needs", "Thinks", "Sent", "Met",
        ] {
            assert!(
                canonicalize_surface(junk).is_none(),
                "{junk} is a verb, not a person"
            );
        }
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

    /// A possessive with more words behind it names a THING. Every string
    /// here was a `person` row in the real store, competing with the woman it
    /// belongs to.
    #[test]
    fn a_possessive_compound_resolves_to_its_owner() {
        for (surface, expected) in [
            ("Noemi's Southwark", "Noemi"),
            ("Noemi's June", "Noemi"),
            ("Noemi Varkoni's Lipitrex", "Noemi Varkoni"),
            ("Avi's One Blackfriars", "Avi"),
            ("Avi's Thanksgiving", "Avi"),
            ("Maria's Nov 2025 Egypt", "Maria"),
            ("Maria's Egypt-invite PDF", "Maria"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must resolve to its owner");
            // The compound is not a spelling of her name.
            assert!(
                c.aliases.is_empty(),
                "{surface} left a junk alias: {:?}",
                c.aliases
            );
        }
    }

    /// The conservative half: a BARE possessive is still a spelling of its
    /// owner and still keeps the written form as an alias.
    #[test]
    fn a_bare_possessive_still_keeps_its_written_spelling() {
        let c = canonicalize_surface("Elias's").expect("kept");
        assert_eq!(c.name, "Elias");
        assert_eq!(c.aliases, vec!["Elias's".to_string()]);
        let c = canonicalize_surface("Avi Roy's").expect("kept");
        assert_eq!(c.name, "Avi Roy");
        assert_eq!(c.aliases, vec!["Avi Roy's".to_string()]);
    }

    #[test]
    fn a_date_or_period_terminates_a_name() {
        for (surface, expected) in [
            ("Maria Nov-2025", "Maria"),
            ("Maria June-2026", "Maria"),
            ("Maria Ibiza-period", "Maria"),
            ("Noemi 2025", "Noemi"),
            ("Avi Roy 2026", "Avi Roy"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must lose its timestamp");
            assert!(c.aliases.is_empty(), "{surface}: {:?}", c.aliases);
        }
        // A hyphenated name carries neither a year nor a period noun.
        assert_eq!(
            canonicalize_surface("Jean-Pierre Dupont").unwrap().name,
            "Jean-Pierre Dupont"
        );
        assert_eq!(
            canonicalize_surface("Andrew Lloyd-Webber").unwrap().name,
            "Andrew Lloyd-Webber"
        );
        // An interior number that binds a phrase is not a date.
        assert_eq!(
            canonicalize_surface("Type 2 Diabetes").unwrap().name,
            "Type 2 Diabetes"
        );
    }

    #[test]
    fn a_shouted_verb_after_a_name_is_not_a_surname() {
        for (surface, expected) in [
            ("MARIA RE-INITIATES AGAIN", "MARIA"),
            ("MARIA RE-INITIATED", "MARIA"),
            ("Maria Marenova PERSONALLY ORGANISES", "Maria Marenova"),
            ("Maria ONCE", "Maria"),
            ("Noemi CONFIRMED", "Noemi"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must lose the shouted clause");
            assert!(c.aliases.is_empty(), "{surface}: {:?}", c.aliases);
        }
    }

    /// SAFETY: a WhatsApp export shouts the whole contact name, and ALL-CAPS
    /// surnames are real. Truncating those would split a person in two.
    #[test]
    fn a_wholly_shouted_name_survives_intact() {
        for shouted in [
            "MARIA MARENOVA",
            "PATRICK ASHCOMB PRUETT",
            "NOEMI VARKONI",
            "AHMED AL-FARSI",
            "JISOO PARK",
        ] {
            assert_eq!(
                canonicalize_surface(shouted).map(|c| c.name),
                Some(shouted.to_string()),
                "{shouted} is a name, not a name plus a verb"
            );
        }
    }

    #[test]
    fn a_profile_slug_is_never_an_entity() {
        for slug in [
            "maria-marenova-9398b6108",
            "noemi-varkoni-4b21ff903",
            "avi-roy-7ab29c114",
        ] {
            assert!(
                canonicalize_surface(slug).is_none(),
                "{slug} is a URL fragment, not a person"
            );
        }
        // …and the hyphenated domain terms that look a little like one live.
        for term in ["SARS-CoV-2", "HIF-1alpha", "IL-6", "COVID-19"] {
            assert_eq!(
                canonicalize_surface(term).map(|c| c.name),
                Some(term.to_string()),
                "{term} must survive the slug filter"
            );
        }
    }

    /// v3 stripped the finite verbs; the gerunds walked straight past it, and
    /// "Reading Lira" was a 449-mention `person` in the real store.
    #[test]
    fn a_gerund_folds_into_the_name_behind_it() {
        for (surface, expected) in [
            ("Reading Lira", "Lira"),
            ("Calling Noemi", "Noemi"),
            ("Telling Avi Roy", "Avi Roy"),
            ("Chasing Elias", "Elias"),
            ("Thanking Maria Marenova", "Maria Marenova"),
        ] {
            let c = canonicalize_surface(surface).expect("kept");
            assert_eq!(c.name, expected, "{surface} must fold onto {expected}");
            assert!(
                c.aliases.is_empty(),
                "{surface} left a junk alias: {:?}",
                c.aliases
            );
        }
        // A gerund with a human object is itself the evidence; a plain one
        // says nothing about what follows.
        assert!(canonicalize_surface("Calling Noemi").unwrap().person_hint);
        assert!(!canonicalize_surface("Reading Lira").unwrap().person_hint);
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
