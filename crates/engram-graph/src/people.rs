//! People-typed entities and the person↔person relationship graph.
//!
//! The inherited extractor could emit `gene_or_protein` but never `person`,
//! which made the entity layer useless for the thing a personal memory store
//! is actually for: knowing who you know and how they connect. This module
//! adds, deterministically and offline (no API key, no network):
//!
//! - **Person typing.** Honorific patterns ("Dr Jane Doe"), multi-token
//!   capitalized names that survive a non-person lexicon, and single-token
//!   names that appear in an interpersonal context ("Avi introduced me to
//!   Noemi", "my friend Noemi", "Noemi's birthday").
//! - **Typed relationship edges** with the cue that produced them, so each
//!   edge can be re-read against its source sentence: `introduced_by`,
//!   `friend_of`, `married_to`, `dating`, `family_of`, `works_with`,
//!   `colleague_of`, `investor_in`, `client_of`, `met_at`, `knows`.
//! - **Alias folding.** A bare first name ("Avi") merges into the full person
//!   ("Avi Roy") instead of standing up a second, half-connected node.
//!
//! Precision beats recall here. An edge only exists when two person-typed
//! entities sit within a short window of a relationship cue in the SAME
//! sentence, so "Noemi — introduced_by → Avi Roy" is always traceable back to
//! a sentence a human can read.

use crate::extract::{candidates, Phrase, Token};
use crate::hygiene::{self, Canonical};
use std::collections::HashMap;

/// Entity kind stored in `entities.kind`.
pub const KIND_PERSON: &str = "person";
/// Organizations: companies, universities, funds, labs.
pub const KIND_ORGANIZATION: &str = "organization";
/// Gene/protein symbols (BRCA1, IL-6, p53) — inherited from the paper corpus.
pub const KIND_GENE_OR_PROTEIN: &str = "gene_or_protein";
/// Everything else.
pub const KIND_CONCEPT: &str = "concept";

/// Rank of an entity kind, used when two chunks disagree: a chunk that proves
/// someone is a person wins over a chunk that could only guess "concept".
pub fn kind_rank(kind: &str) -> u8 {
    match kind {
        KIND_PERSON => 3,
        KIND_ORGANIZATION => 2,
        KIND_GENE_OR_PROTEIN => 1,
        _ => 0,
    }
}

/// Merge two kinds, keeping the more specific one.
pub fn merge_kind<'a>(existing: &'a str, incoming: &'a str) -> &'a str {
    if kind_rank(incoming) > kind_rank(existing) {
        incoming
    } else {
        existing
    }
}

/// Words that describe a human role or a family tie. Their presence next to a
/// capitalized token is strong evidence that the token names a person.
const ROLE_WORDS: &[&str] = &[
    "friend",
    "friends",
    "bestie",
    "colleague",
    "colleagues",
    "coworker",
    "co-worker",
    "cofounder",
    "co-founder",
    "founder",
    "wife",
    "husband",
    "spouse",
    "girlfriend",
    "boyfriend",
    "fiance",
    "fiancee",
    "partner",
    "brother",
    "sister",
    "mother",
    "father",
    "mom",
    "mum",
    "dad",
    "son",
    "daughter",
    "cousin",
    "uncle",
    "aunt",
    "nephew",
    "niece",
    "grandmother",
    "grandfather",
    "parents",
    "neighbor",
    "neighbour",
    "boss",
    "mentor",
    "mentee",
    "investor",
    "angel",
    "client",
    "customer",
    "assistant",
    "advisor",
    "adviser",
    "doctor",
    "lawyer",
    "accountant",
    "recruiter",
    "teacher",
    "student",
    "roommate",
    "flatmate",
    "classmate",
    "teammate",
    "ceo",
    "cto",
    "cfo",
    "coo",
    "chairman",
    "chairwoman",
    "president",
    "director",
    "manager",
    "guy",
    "woman",
    "man",
    "person",
    "people",
];

/// Verbs and nouns that only make sense with a human subject or object.
/// Deliberately narrow — "works", "said" and "called" are too ambiguous
/// ("the protein called X", "the assay works") and are excluded.
const INTERPERSONAL_CUES: &[&str] = &[
    "met",
    "meets",
    "meeting",
    "introduced",
    "introduce",
    "introduces",
    "introduction",
    "intro",
    "married",
    "marry",
    "marries",
    "wedding",
    "divorced",
    "dating",
    "engaged",
    "texted",
    "emailed",
    "phoned",
    "messaged",
    "pinged",
    "dm'd",
    "invited",
    "birthday",
    "knows",
    "knew",
    "told",
    "asked",
    "thanked",
    "congratulated",
    "hired",
    "mentored",
    "befriended",
    "referred",
    "chatted",
    "lunch",
    "dinner",
    "coffee",
    "drinks",
    "his",
    "her",
    "hers",
    "him",
    "she",
    "he",
    "himself",
    "herself",
];

/// Head/modifier nouns that make a Title-Case phrase something other than a
/// human name. Any one of these anywhere in a candidate blocks person typing:
/// "Tumor Necrosis Factor", "New York", "Acme Capital", "Golgi Apparatus".
const NON_PERSON_WORDS: &[&str] = &[
    // Organizations
    "university",
    "college",
    "school",
    "academy",
    "institute",
    "institutes",
    "inc",
    "ltd",
    "llc",
    "plc",
    "gmbh",
    "corp",
    "corporation",
    "company",
    "co",
    "labs",
    "lab",
    "laboratory",
    "foundation",
    "trust",
    "fund",
    "funds",
    "capital",
    "ventures",
    "venture",
    "partners",
    "holdings",
    "group",
    "society",
    "association",
    "council",
    "committee",
    "department",
    "ministry",
    "agency",
    "bureau",
    "office",
    "bank",
    "museum",
    "library",
    "church",
    "temple",
    "hospital",
    "clinic",
    "center",
    "centre",
    "centers",
    "centres",
    "team",
    "club",
    "league",
    "network",
    "platform",
    "systems",
    "technologies",
    "biotechnologies",
    "solutions",
    "media",
    "studio",
    "studios",
    "press",
    // Places
    "street",
    "road",
    "avenue",
    "boulevard",
    "lane",
    "drive",
    "square",
    "city",
    "town",
    "village",
    "county",
    "state",
    "province",
    "country",
    "island",
    "lake",
    "mount",
    "mountain",
    "river",
    "valley",
    "beach",
    "bay",
    "airport",
    "station",
    "hotel",
    "north",
    "south",
    "east",
    "west",
    "new",
    "san",
    "los",
    "las",
    "saint",
    "st",
    "fort",
    "port",
    "cape",
    "upper",
    "lower",
    "great",
    "little",
    "old",
    "long",
    // Science / domain vocabulary
    "disease",
    "syndrome",
    "cancer",
    "therapy",
    "treatment",
    "protein",
    "proteins",
    "gene",
    "genes",
    "cell",
    "cells",
    "receptor",
    "factor",
    "factors",
    "pathway",
    "acid",
    "virus",
    "vaccine",
    "dose",
    "assay",
    "apparatus",
    "complex",
    "membrane",
    "matrix",
    "reticulum",
    "index",
    "ratio",
    "rate",
    "score",
    "test",
    "analysis",
    "method",
    "theory",
    "law",
    "effect",
    "model",
    "study",
    "trial",
    "cohort",
    "research",
    "project",
    "program",
    "programme",
    "summit",
    "conference",
    "workshop",
    "meetup",
    "journal",
    "review",
    "report",
    "award",
    "prize",
    "series",
    "season",
    "episode",
    "engine",
    "api",
    "app",
    "version",
    "release",
    "roadmap",
    "budget",
    "invoice",
    "contract",
];

/// Lowercase particles that legitimately sit inside a personal name.
const NAME_PARTICLES: &[&str] = &[
    "de", "del", "della", "der", "den", "di", "da", "das", "dos", "du", "la", "le", "van", "von",
    "bin", "ibn", "al", "ter", "ten", "af", "mac", "mc",
];

fn is_non_person_word(lower: &str) -> bool {
    NON_PERSON_WORDS.contains(&lower)
}

/// Does this token look like one component of a human name?
fn name_token(token: &str) -> bool {
    let lower = token.to_lowercase();
    if NAME_PARTICLES.contains(&lower.as_str()) {
        return true;
    }
    if is_non_person_word(&lower) || hygiene::is_stopword(token) || hygiene::is_temporal(token) {
        return false;
    }
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    // A single initial ("J") is fine; anything longer must be Title-Case
    // (an ALL-CAPS token is an acronym, not a given name).
    let rest: Vec<char> = chars.collect();
    if rest.is_empty() {
        return true;
    }
    if rest.iter().any(|c| c.is_ascii_digit()) {
        return false;
    }
    rest.iter()
        .all(|c| c.is_lowercase() || *c == '\'' || *c == '-' || *c == '.')
}

/// Could `name` be a human name at all (before context is considered)?
pub fn looks_like_person_name(name: &str) -> bool {
    let tokens: Vec<&str> = name.split_whitespace().collect();
    if tokens.is_empty() || tokens.len() > 4 {
        return false;
    }
    if !tokens.iter().all(|t| name_token(t)) {
        return false;
    }
    // A name made only of particles ("Van Der") is not a name.
    tokens
        .iter()
        .any(|t| t.chars().count() >= 2 && !NAME_PARTICLES.contains(&t.to_lowercase().as_str()))
}

/// Organization suffixes kept from the original compiler heuristic.
fn looks_like_organization(name: &str) -> bool {
    const ORG_SUFFIXES: &[&str] = &[
        " Inc",
        " Ltd",
        " LLC",
        " Labs",
        " Lab",
        " Corp",
        " Company",
        " University",
        " Institute",
        " Foundation",
        " Capital",
        " Ventures",
        " Partners",
        " Holdings",
        " Group",
        " Hospital",
        " Bank",
    ];
    ORG_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix) || name.ends_with(&format!("{suffix}.")))
}

/// A short alphanumeric token carrying a digit looks like a gene/protein
/// symbol (BRCA1, IL-6, p53, mTORC1). Bare all-caps words (DNA, FDA) are not.
fn looks_like_symbol(name: &str) -> bool {
    let letters: String = name.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    let has_digit = name.chars().any(|c| c.is_ascii_digit());
    name.len() <= 12
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && has_digit
        && letters.chars().any(|c| c.is_ascii_uppercase())
}

/// Infer the stored entity kind for `name`.
///
/// `person_evidence` is `true` when the surrounding text showed an
/// interpersonal cue (an honorific, a role word, a gendered pronoun, or a
/// relationship verb) next to this mention. A single capitalized token is only
/// typed as a person with that evidence; a multi-token capitalized name that
/// survives the non-person lexicon is typed as a person on its own.
pub fn infer_kind(name: &str, person_evidence: bool) -> &'static str {
    if looks_like_organization(name) {
        return KIND_ORGANIZATION;
    }
    if looks_like_person_name(name) {
        let multi_token = name.split_whitespace().count() >= 2;
        if person_evidence || multi_token {
            return KIND_PERSON;
        }
    }
    if looks_like_symbol(name) {
        return KIND_GENE_OR_PROTEIN;
    }
    KIND_CONCEPT
}

/// A distinct entity found in one block of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// Canonical surface form.
    pub name: String,
    /// Alternate spellings (possessives, bare first names) that resolve here.
    pub aliases: Vec<String>,
    /// `person` | `organization` | `gene_or_protein` | `concept`.
    pub kind: &'static str,
    /// How many times the entity occurred in this text.
    pub mentions: usize,
}

/// A typed person↔person edge with the evidence that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonEdge {
    /// Subject entity (canonical name).
    pub from: String,
    /// Object entity (canonical name).
    pub to: String,
    /// Typed predicate, e.g. `introduced_by`.
    pub predicate: &'static str,
    /// The word in the source text that triggered the predicate.
    pub cue: String,
    /// The sentence the edge was read from — the human-checkable receipt.
    pub sentence: String,
    /// Venue for `met_at`, when the sentence named one.
    pub context: Option<String>,
}

/// Result of analyzing one chunk of text.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Entities in first-appearance order.
    pub entities: Vec<Mention>,
    /// Person↔person edges, deduplicated on (from, predicate, to).
    pub edges: Vec<PersonEdge>,
}

impl Extraction {
    /// Look up an entity kind by canonical name (case-insensitive).
    pub fn kind_of(&self, name: &str) -> Option<&'static str> {
        let key = hygiene::dedup_key(name);
        self.entities
            .iter()
            .find(|e| hygiene::dedup_key(&e.name) == key)
            .map(|e| e.kind)
    }
}

/// Window (in tokens) scanned on each side of a mention for person evidence.
const CUE_WINDOW: usize = 4;
/// Maximum tokens allowed between two people for a relation to be believable.
const RELATION_GAP: usize = 6;

fn lower_token(toks: &[Token], idx: usize) -> String {
    toks[idx].text.to_lowercase()
}

/// Is there interpersonal evidence around the mention at `[start, end)`?
///
/// The window never leaves the mention's own sentence: "Rapamycin worked.
/// She's fine." must not make Rapamycin a person because a pronoun happens to
/// follow the full stop.
fn person_evidence(toks: &[Token], start: usize, end: usize, sentence: (usize, usize)) -> bool {
    // Honorific immediately before the name is decisive.
    if start > sentence.0 && hygiene::is_honorific(&toks[start - 1].text) {
        return true;
    }
    let lo = start.saturating_sub(CUE_WINDOW).max(sentence.0);
    let hi = (end + CUE_WINDOW).min(sentence.1).min(toks.len());
    for idx in lo..hi {
        if idx >= start && idx < end {
            continue;
        }
        let word = lower_token(toks, idx);
        if ROLE_WORDS.contains(&word.as_str()) || INTERPERSONAL_CUES.contains(&word.as_str()) {
            return true;
        }
    }
    false
}

/// Predicate decided from the words between two person mentions.
struct Predicate {
    name: &'static str,
    cue: String,
    /// The edge points from the SECOND mention to the first ("B introduced_by A").
    reverse: bool,
    /// Direction is meaningful (investor_in, client_of, introduced_by).
    directional: bool,
}

fn any_of(mid: &[String], words: &[&str]) -> Option<String> {
    mid.iter()
        .find(|w| words.contains(&w.as_str()))
        .map(|w| w.to_string())
}

/// Read the tokens between two people and decide what ties them together.
fn predicate_between(mid: &[String]) -> Option<Predicate> {
    let has = |w: &str| mid.iter().any(|t| t == w);
    let pred = |name: &'static str, cue: &str, reverse: bool, directional: bool| {
        Some(Predicate {
            name,
            cue: cue.to_string(),
            reverse,
            directional,
        })
    };

    if let Some(pos) = mid
        .iter()
        .position(|w| w.starts_with("introduc") || w == "intro")
    {
        // "A introduced me to B" → B was introduced BY A.
        // "B was introduced by A" → B was introduced BY A.
        let by_after = mid.iter().skip(pos + 1).any(|w| w == "by");
        return pred("introduced_by", &mid[pos], !by_after, true);
    }
    if let Some(cue) = any_of(mid, &["married", "wife", "husband", "spouse", "marrying"]) {
        return pred("married_to", &cue, false, false);
    }
    if let Some(cue) = any_of(
        mid,
        &[
            "dating",
            "girlfriend",
            "boyfriend",
            "fiance",
            "fiancee",
            "engaged",
        ],
    ) {
        return pred("dating", &cue, false, false);
    }
    if let Some(cue) = any_of(
        mid,
        &[
            "brother",
            "sister",
            "mother",
            "father",
            "mom",
            "mum",
            "dad",
            "son",
            "daughter",
            "cousin",
            "uncle",
            "aunt",
            "nephew",
            "niece",
            "grandmother",
            "grandfather",
            "parents",
            "sibling",
            "siblings",
            "family",
        ],
    ) {
        return pred("family_of", &cue, false, false);
    }
    if let Some(cue) = any_of(mid, &["friend", "friends", "befriended"]) {
        return pred("friend_of", &cue, false, false);
    }
    if let Some(cue) = any_of(
        mid,
        &[
            "colleague",
            "colleagues",
            "coworker",
            "co-worker",
            "teammate",
        ],
    ) {
        return pred("colleague_of", &cue, false, false);
    }
    if let Some(cue) = any_of(
        mid,
        &["invested", "investor", "backed", "funded", "invests"],
    ) {
        // "A's investor B" flips to "B invests in A" at the call site.
        return pred("investor_in", &cue, false, true);
    }
    if let Some(cue) = any_of(mid, &["client", "customer", "clients"]) {
        return pred("client_of", &cue, false, true);
    }
    if let Some(cue) = any_of(
        mid,
        &[
            "cofounded",
            "co-founded",
            "cofounder",
            "co-founder",
            "founded",
        ],
    ) {
        return pred("works_with", &cue, false, false);
    }
    if (has("works") || has("working") || has("work") || has("worked"))
        && (has("with") || has("together") || has("alongside"))
    {
        return pred("works_with", "works with", false, false);
    }
    if let Some(cue) = any_of(mid, &["met", "meets", "meeting", "know", "knows", "knew"]) {
        return pred("knows", &cue, false, false);
    }
    None
}

/// Venue named by an "at <Place>" following a mention.
///
/// Only a NAMED place counts — the first candidate phrase after the "at".
/// "met Noemi at the summit" names nothing checkable, so it yields no venue
/// (and the edge stays `knows`) rather than recording the determiner.
fn venue_after(
    toks: &[Token],
    found: &[(Phrase, Canonical)],
    from_token: usize,
    sentence_end: usize,
) -> Option<String> {
    let at_idx = (from_token..sentence_end).find(|idx| lower_token(toks, *idx) == "at")?;
    found
        .iter()
        .find(|(phrase, _)| phrase.start > at_idx && phrase.end <= sentence_end)
        .filter(|(phrase, _)| phrase.start <= at_idx + 3)
        .map(|(_, canonical)| canonical.name.clone())
}

/// Analyze one block of text: hygienic entities, their kinds, and the typed
/// person↔person edges the text supports.
pub fn analyze(text: &str) -> Extraction {
    let (toks, found) = candidates(text);
    if found.is_empty() {
        return Extraction::default();
    }

    let sentence_bounds = sentence_spans(&toks);
    let bounds_of = |sentence: usize| -> (usize, usize) {
        sentence_bounds
            .get(&sentence)
            .copied()
            .unwrap_or((0, toks.len()))
    };

    // Pass 1 — collect occurrences, aliases, and per-occurrence kinds.
    struct Acc {
        name: String,
        aliases: Vec<String>,
        kind: &'static str,
        mentions: usize,
        order: usize,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    let mut occurrences: Vec<(Phrase, String)> = Vec::new();
    for (phrase, canonical) in &found {
        let Canonical { name, aliases } = canonical;
        // A title inside the surface ("Dr Patel") is stripped from the
        // canonical name, so check it before it is gone.
        let titled = phrase
            .surface
            .split_whitespace()
            .next()
            .map(hygiene::is_honorific)
            .unwrap_or(false);
        let evidence =
            titled || person_evidence(&toks, phrase.start, phrase.end, bounds_of(phrase.sentence));
        let kind = infer_kind(name, evidence);
        let key = hygiene::dedup_key(name);
        let next_order = acc.len();
        let entry = acc.entry(key.clone()).or_insert_with(|| Acc {
            name: name.clone(),
            aliases: Vec::new(),
            kind,
            mentions: 0,
            order: next_order,
        });
        entry.mentions += 1;
        entry.kind = merge_kind(entry.kind, kind);
        for alias in aliases {
            if !entry.aliases.iter().any(|a| a == alias) {
                entry.aliases.push(alias.clone());
            }
        }
        occurrences.push((phrase.clone(), key));
    }

    // Pass 2 — fold a bare first/last name into the full person it belongs to
    // ("Avi" → alias of "Avi Roy"), so the graph has one node per human.
    let people_keys: Vec<String> = acc
        .values()
        .filter(|a| a.kind == KIND_PERSON && a.name.split_whitespace().count() >= 2)
        .map(|a| hygiene::dedup_key(&a.name))
        .collect();
    let mut redirect: HashMap<String, String> = HashMap::new();
    for (key, entry) in acc.iter() {
        if entry.name.split_whitespace().count() != 1 {
            continue;
        }
        if entry.kind == KIND_ORGANIZATION || entry.kind == KIND_GENE_OR_PROTEIN {
            continue;
        }
        let mut matches = people_keys
            .iter()
            .filter(|full| full.split(' ').any(|part| part == key));
        if let Some(full) = matches.next() {
            // Ambiguous ("Avi" inside both "Avi Roy" and "Avi Chen") → leave
            // the standalone node alone rather than guess wrong.
            if matches.next().is_none() {
                redirect.insert(key.clone(), full.clone());
            }
        }
    }
    for (short_key, full_key) in &redirect {
        let Some(short) = acc.remove(short_key) else {
            continue;
        };
        if let Some(full) = acc.get_mut(full_key) {
            full.mentions += short.mentions;
            for alias in std::iter::once(short.name.clone()).chain(short.aliases) {
                if !full.aliases.iter().any(|a| a == &alias) && alias != full.name {
                    full.aliases.push(alias);
                }
            }
        }
    }

    let resolve = |key: &String| -> String { redirect.get(key).cloned().unwrap_or(key.clone()) };

    let mut entities: Vec<Acc> = acc.into_values().collect();
    entities.sort_by_key(|a| a.order);
    let kind_by_key: HashMap<String, &'static str> = entities
        .iter()
        .map(|a| (hygiene::dedup_key(&a.name), a.kind))
        .collect();
    let name_by_key: HashMap<String, String> = entities
        .iter()
        .map(|a| (hygiene::dedup_key(&a.name), a.name.clone()))
        .collect();

    // Pass 3 — relationship edges inside each sentence.
    let mut edges: Vec<PersonEdge> = Vec::new();
    let mut i = 0;
    while i + 1 < occurrences.len() {
        let (a_phrase, a_key_raw) = &occurrences[i];
        let (b_phrase, b_key_raw) = &occurrences[i + 1];
        i += 1;
        if a_phrase.sentence != b_phrase.sentence {
            continue;
        }
        let a_key = resolve(a_key_raw);
        let b_key = resolve(b_key_raw);
        if a_key == b_key {
            continue;
        }
        if kind_by_key.get(&a_key).copied() != Some(KIND_PERSON)
            || kind_by_key.get(&b_key).copied() != Some(KIND_PERSON)
        {
            continue;
        }
        if b_phrase.start < a_phrase.end || b_phrase.start - a_phrase.end > RELATION_GAP {
            continue;
        }
        let mid: Vec<String> = (a_phrase.end..b_phrase.start)
            .map(|idx| lower_token(&toks, idx))
            .collect();
        let Some(predicate) = predicate_between(&mid) else {
            continue;
        };
        let (sentence_start_tok, sentence_end_tok) = bounds_of(a_phrase.sentence);
        let sentence = slice_text(text, &toks, sentence_start_tok, sentence_end_tok);

        // "Avi's client Noemi" reads the other way round from "Avi is a client
        // of Noemi": a possessive owner is the OBJECT of a directional edge.
        let a_possessive = a_phrase.surface.ends_with("'s") || a_phrase.surface.ends_with('\'');
        let (mut from_key, mut to_key) = (a_key.clone(), b_key.clone());
        if predicate.reverse || (predicate.directional && a_possessive) {
            std::mem::swap(&mut from_key, &mut to_key);
        }

        let (predicate_name, context) = if predicate.name == "knows" {
            match venue_after(&toks, &found, b_phrase.end, sentence_end_tok) {
                Some(venue) => ("met_at", Some(venue)),
                None => ("knows", None),
            }
        } else {
            (predicate.name, None)
        };

        let edge = PersonEdge {
            from: name_by_key.get(&from_key).cloned().unwrap_or(from_key),
            to: name_by_key.get(&to_key).cloned().unwrap_or(to_key),
            predicate: predicate_name,
            cue: predicate.cue,
            sentence,
            context,
        };
        if !edges
            .iter()
            .any(|e| e.from == edge.from && e.to == edge.to && e.predicate == edge.predicate)
        {
            edges.push(edge);
        }
    }

    Extraction {
        entities: entities
            .into_iter()
            .map(|a| Mention {
                name: a.name,
                aliases: a.aliases,
                kind: a.kind,
                mentions: a.mentions,
            })
            .collect(),
        edges,
    }
}

/// First and last token index of every sentence in the text.
fn sentence_spans(toks: &[Token]) -> HashMap<usize, (usize, usize)> {
    let mut sentences: HashMap<usize, (usize, usize)> = HashMap::new();
    if toks.is_empty() {
        return sentences;
    }
    let mut sentence = 0usize;
    let mut start = 0usize;
    for (idx, tok) in toks.iter().enumerate() {
        if tok.sentence_start && idx > 0 {
            sentences.insert(sentence, (start, idx));
            sentence += 1;
            start = idx;
        }
    }
    sentences.insert(sentence, (start, toks.len()));
    sentences
}

/// Source text covered by tokens `[start_tok, end_tok)`, trimmed.
fn slice_text(text: &str, toks: &[Token], start_tok: usize, end_tok: usize) -> String {
    if toks.is_empty() || start_tok >= toks.len() {
        return String::new();
    }
    let end_tok = end_tok.min(toks.len());
    let start = toks[start_tok].start;
    let end = toks[end_tok - 1].end.min(text.len());
    // Include a trailing sentence terminator when it is right there.
    let mut slice_end = end;
    if let Some(next) = text[end..].chars().next() {
        if matches!(next, '.' | '!' | '?') {
            slice_end = end + next.len_utf8();
        }
    }
    text[start..slice_end].trim().replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> HashMap<String, &'static str> {
        analyze(text)
            .entities
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect()
    }

    #[test]
    fn multi_token_names_are_people() {
        let k = kinds("Avi Roy runs the fund. Jane Doe joined last year.");
        assert_eq!(k.get("Avi Roy").copied(), Some(KIND_PERSON));
        assert_eq!(k.get("Jane Doe").copied(), Some(KIND_PERSON));
    }

    #[test]
    fn honorifics_type_a_person() {
        let k = kinds("Dr Patel reviewed the scan.");
        assert_eq!(k.get("Patel").copied(), Some(KIND_PERSON));
    }

    #[test]
    fn single_name_needs_interpersonal_context() {
        // Bare mention, no cue: stays a concept rather than inventing a person.
        let k = kinds("Rapamycin lowered the readout.");
        assert_eq!(k.get("Rapamycin").copied(), Some(KIND_CONCEPT));
        // With a cue, the same shape of token becomes a person.
        let k = kinds("I met Noemi at the summit and she was great.");
        assert_eq!(k.get("Noemi").copied(), Some(KIND_PERSON));
    }

    #[test]
    fn domain_phrases_are_not_people() {
        let k = kinds(
            "Tumor Necrosis Factor rose. The Golgi Apparatus was intact. New York was cold. Acme Capital led the round.",
        );
        assert_eq!(k.get("Tumor Necrosis Factor").copied(), Some(KIND_CONCEPT));
        assert_eq!(k.get("Golgi Apparatus").copied(), Some(KIND_CONCEPT));
        assert_eq!(k.get("New York").copied(), Some(KIND_CONCEPT));
        assert_eq!(k.get("Acme Capital").copied(), Some(KIND_ORGANIZATION));
    }

    #[test]
    fn gene_symbols_still_type_as_gene_or_protein() {
        let k = kinds("BRCA1 and IL-6 were measured.");
        assert_eq!(k.get("BRCA1").copied(), Some(KIND_GENE_OR_PROTEIN));
        assert_eq!(k.get("IL-6").copied(), Some(KIND_GENE_OR_PROTEIN));
    }

    #[test]
    fn introduction_edge_points_at_the_introducer() {
        let ex = analyze("Avi Roy introduced me to Noemi Kostova at the longevity summit.");
        let edge = ex
            .edges
            .iter()
            .find(|e| e.predicate == "introduced_by")
            .expect("introduced_by edge");
        assert_eq!(edge.from, "Noemi Kostova");
        assert_eq!(edge.to, "Avi Roy");
        assert!(edge.sentence.contains("introduced me to"));
    }

    #[test]
    fn passive_introduction_keeps_the_same_direction() {
        let ex = analyze("Noemi Kostova was introduced by Avi Roy.");
        let edge = &ex.edges[0];
        assert_eq!(edge.predicate, "introduced_by");
        assert_eq!(edge.from, "Noemi Kostova");
        assert_eq!(edge.to, "Avi Roy");
    }

    #[test]
    fn relationship_predicates_are_typed() {
        let cases = [
            ("Avi Roy is married to Noemi Kostova.", "married_to"),
            ("Avi Roy is dating Noemi Kostova.", "dating"),
            ("Avi Roy's sister Noemi Kostova called.", "family_of"),
            ("Avi Roy's friend Noemi Kostova called.", "friend_of"),
            ("Avi Roy is a colleague of Noemi Kostova.", "colleague_of"),
            ("Avi Roy works with Noemi Kostova.", "works_with"),
            ("Avi Roy invested in Noemi Kostova.", "investor_in"),
            ("Avi Roy is a client of Noemi Kostova.", "client_of"),
        ];
        for (text, expected) in cases {
            let ex = analyze(text);
            assert_eq!(
                ex.edges.first().map(|e| e.predicate),
                Some(expected),
                "wrong predicate for {text:?}: {:?}",
                ex.edges
            );
        }
    }

    #[test]
    fn possessive_flips_directional_edges() {
        let ex = analyze("Avi Roy's client Noemi Kostova signed.");
        let edge = &ex.edges[0];
        assert_eq!(edge.predicate, "client_of");
        assert_eq!(edge.from, "Noemi Kostova");
        assert_eq!(edge.to, "Avi Roy");
    }

    #[test]
    fn met_at_records_the_venue() {
        let ex = analyze("Avi Roy met Noemi Kostova at Longevity Summit.");
        let edge = &ex.edges[0];
        assert_eq!(edge.predicate, "met_at");
        assert_eq!(edge.context.as_deref(), Some("Longevity Summit"));
    }

    #[test]
    fn an_unnamed_venue_is_not_invented() {
        // "the summit" names nothing checkable, so no venue is recorded and
        // the edge stays a plain acquaintance link.
        let ex = analyze("Avi Roy met Noemi Kostova at the summit.");
        let edge = &ex.edges[0];
        assert_eq!(edge.predicate, "knows");
        assert_eq!(edge.context, None);
    }

    #[test]
    fn cues_do_not_leak_across_a_sentence_boundary() {
        // The pronoun belongs to the NEXT sentence; it must not make the drug
        // a person.
        let k = kinds("Rapamycin worked. She's fine.");
        assert_eq!(k.get("Rapamycin").copied(), Some(KIND_CONCEPT));
    }

    #[test]
    fn edges_do_not_span_sentences() {
        let ex = analyze("Avi Roy called. My friend Noemi Kostova did too.");
        assert!(
            !ex.edges
                .iter()
                .any(|e| e.from == "Avi Roy" || e.to == "Avi Roy"),
            "cross-sentence edge invented: {:?}",
            ex.edges
        );
    }

    #[test]
    fn edges_require_two_people() {
        // Rapamycin is not a person, so no edge is invented.
        let ex = analyze("Rapamycin met the endpoint with mTORC1.");
        assert!(ex.edges.is_empty(), "{:?}", ex.edges);
    }

    #[test]
    fn bare_first_name_folds_into_the_full_person() {
        let ex = analyze("Avi Roy introduced me to Noemi. Avi later emailed me.");
        let avi = ex
            .entities
            .iter()
            .find(|e| e.name == "Avi Roy")
            .expect("Avi Roy");
        assert!(avi.aliases.contains(&"Avi".to_string()));
        assert_eq!(avi.mentions, 2);
        assert!(
            !ex.entities.iter().any(|e| e.name == "Avi"),
            "the bare first name must not stand up a second node"
        );
    }

    #[test]
    fn contractions_and_dates_never_reach_the_graph() {
        let ex = analyze("I'm seeing Noemi on Monday. She's bringing Avi Roy. It's set.");
        for junk in ["I'm", "She's", "It's", "Monday"] {
            assert!(
                !ex.entities.iter().any(|e| e.name == junk),
                "{junk} leaked: {:?}",
                ex.entities
            );
        }
    }

    #[test]
    fn realistic_personal_note_types_the_right_things() {
        let text = "Had coffee with Dr. Amara Osei on Tuesday. She's now at Calico Labs and \
                    she's dating Marcus. Elias's birthday is in July; Elias introduced me to \
                    Amara last year at the Longevity Summit in New York. I'm going to email \
                    her about the NAD trial. Her colleague Wei Zhang works on IL-6.";
        let ex = analyze(text);
        let kind_of = |name: &str| ex.kind_of(name);

        // People — including a titled name, a bare first name with a cue, and
        // a full name that only appears once.
        assert_eq!(kind_of("Amara Osei"), Some(KIND_PERSON));
        assert_eq!(kind_of("Elias"), Some(KIND_PERSON));
        assert_eq!(kind_of("Marcus"), Some(KIND_PERSON));
        assert_eq!(kind_of("Wei Zhang"), Some(KIND_PERSON));
        // Not people.
        assert_eq!(kind_of("Calico Labs"), Some(KIND_ORGANIZATION));
        assert_eq!(kind_of("Longevity Summit"), Some(KIND_CONCEPT));
        assert_eq!(kind_of("New York"), Some(KIND_CONCEPT));
        assert_eq!(kind_of("IL-6"), Some(KIND_GENE_OR_PROTEIN));
        // Junk that used to become entities.
        for junk in ["She's", "I'm", "Tuesday", "July", "Dr", "Her"] {
            assert!(kind_of(junk).is_none(), "{junk} leaked: {:?}", ex.entities);
        }

        // Aliases fold onto their owner rather than splitting the node.
        let amara = ex
            .entities
            .iter()
            .find(|e| e.name == "Amara Osei")
            .expect("Amara Osei");
        assert!(amara.aliases.contains(&"Amara".to_string()));
        assert_eq!(amara.mentions, 2);
        let elias = ex
            .entities
            .iter()
            .find(|e| e.name == "Elias")
            .expect("Elias");
        assert!(elias.aliases.contains(&"Elias's".to_string()));

        assert!(ex
            .edges
            .iter()
            .any(|e| e.predicate == "introduced_by" && e.from == "Amara Osei" && e.to == "Elias"));
    }

    #[test]
    fn analysis_is_deterministic() {
        let text =
            "Avi Roy introduced me to Noemi Kostova at the summit. Noemi's partner is Sam Lee.";
        let a = analyze(text);
        let b = analyze(text);
        assert_eq!(
            a.entities.iter().map(|e| &e.name).collect::<Vec<_>>(),
            b.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        assert_eq!(a.edges, b.edges);
    }
}
