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
use crate::gazetteer;
use crate::hygiene::{self, Canonical};
use engram_core::names;
use std::collections::{BTreeMap, HashMap};

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
///
/// A double-barrelled surname and an Irish one are COMPOUNDS: "Brandtke-Wendel"
/// and "O'Brien" each carry an interior capital, which the flat Title-Case test
/// this used to be read as an acronym. So "Rosalie Brandtke-Wendel" was typed
/// `concept`, was never a fold candidate, and "Rosalie" (65 mentions) stayed a
/// bare first name forever. Each hyphen/apostrophe-separated piece is therefore
/// tested on its own.
fn name_token(token: &str) -> bool {
    let lower = token.to_lowercase();
    if NAME_PARTICLES.contains(&lower.as_str()) {
        return true;
    }
    // Employer/industry/service tags ("Microsoft", "Tech", "Gym") only ever
    // appear in a messaging-app contact label, never in a human's name. Place
    // words are deliberately NOT rejected here — "Jordan Peterson" is a person.
    if gazetteer::is_contact_label_word(&lower) {
        return false;
    }
    if is_non_person_word(&lower) || hygiene::is_stopword(token) || hygiene::is_temporal(token) {
        return false;
    }
    // A digit anywhere makes it a symbol, not a name ("SARS-CoV-2", "IL-6").
    if token.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let mut segments = token
        .split(['-', '\'', '\u{2019}'])
        .filter(|s| !s.is_empty())
        .peekable();
    segments.peek().is_some() && segments.all(name_segment)
}

/// One hyphen- or apostrophe-separated piece of a name token.
///
/// Title-Case, a single initial ("J"), or a lowercase particle ("van", "de").
/// An ALL-CAPS piece is an acronym, not a given name — which is what keeps
/// `looks_like_person_name` refusing a shouted spelling, the refusal
/// [`as_written_name`] exists to work around.
fn name_segment(segment: &str) -> bool {
    let lower = segment.to_lowercase();
    if NAME_PARTICLES.contains(&lower.as_str()) {
        return true;
    }
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_uppercase() && chars.all(|c| c.is_lowercase() || c == '.')
}

/// Could `name` be a human name at all (before context is considered)?
///
/// A candidate whose WHOLE name is a place, religion, language, or
/// nationality is refused outright: "we met in Ibiza" and "Islam came up over
/// dinner" put those words in exactly the interpersonal contexts that type a
/// bare token as a person, which is how the real store ended up with London
/// (84 mentions), Ibiza (49), Singapore (48) and Islam (32) in `people list`.
pub fn looks_like_person_name(name: &str) -> bool {
    if gazetteer::is_never_person(name) {
        return false;
    }
    // "Chelsea Flower" (the Chelsea Flower Show, cut short by the stop-word
    // filter) has the exact shape of a two-part human name. A place word plus
    // a venue noun gives it away — see `is_truncated_place_phrase` for why
    // both halves are required.
    if gazetteer::is_truncated_place_phrase(name) {
        return false;
    }
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

/// A SHOUTED spelling read back as a written name ("MARIA" → "Maria", "MARIA
/// MARENOVA" → "Maria Marenova").
///
/// A WhatsApp export shouts the contact name on every line, and to a
/// capitalization-driven reader an ALL-CAPS token is an acronym, not a given
/// name — which is why [`looks_like_person_name`] refuses it and why the fold
/// used to walk straight past "MARIA:" and leave it as its own node. Tokens
/// that already carry a lowercase letter are untouched, so a mixed-case name
/// keeps whatever spelling the writer chose.
fn as_written_name(name: &str) -> String {
    name.split_whitespace()
        .map(|token| {
            let shouted = token.chars().count() >= 2
                && token.chars().any(|c| c.is_uppercase())
                && !token.chars().any(|c| c.is_lowercase());
            if !shouted {
                return token.to_string();
            }
            let mut out = String::with_capacity(token.len());
            for (idx, c) in token.chars().enumerate() {
                if idx == 0 {
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// What a whole KB knows about the people it talks about.
///
/// NAME FRAGMENTATION is invisible from inside one chunk. A message that says
/// only "Noemi sent the deck" cannot know whether the KB elsewhere calls her
/// "Noemi Varkoni", so the per-chunk pass left the real store with Noemi (106
/// mentions) and Noemi Varkoni (66) as two unrelated nodes — and the same for
/// Avi/Avi Roy, Francisco/Francisco Ferró, Patrick/Patrick Ashcomb Pruett.
///
/// The compiler therefore scans every chunk once to build this index, then
/// re-analyzes each chunk with it so a bare first name folds into the full
/// person KB-wide.
///
/// SAFETY — the index exists as much to BLOCK merges as to make them. A short
/// name only folds when the whole KB has exactly ONE full-name person starting
/// with it. Two Lauras (Laura Corvo, a friend, and a different Laura from a
/// dating thread) make "Laura" ambiguous everywhere at once, so neither full
/// name absorbs the bare mentions and the two humans stay two humans. That
/// verdict has to be KB-wide: a per-chunk pass sees only one Laura per chunk
/// and would happily fuse them.
#[derive(Debug, Clone, Default)]
pub struct PersonIndex {
    /// Case-folded key → the spelling the KB should display.
    display: HashMap<String, String>,
    /// A full name that is a shorter or differently-accented writing of
    /// ANOTHER full name → the key of the fullest one. "Patrick Pruett" →
    /// "Patrick Ashcomb Pruett", "Francisco Ferro" → "Francisco Ferró Oliver".
    variants: HashMap<String, String>,
    /// First name → the full-name keys starting with it, sorted. Two entries
    /// under one first name is the ambiguity that blocks every fold.
    by_first_name: HashMap<String, Vec<String>>,
    /// Any name part → the full-name keys containing it, sorted. Used only for
    /// the bare-surname fallback.
    by_part: HashMap<String, Vec<String>>,
}

/// Accumulates the first pass one chunk at a time.
///
/// The compiler must not keep every chunk's [`Extraction`] alive just to learn
/// the KB's names — the edges inside them carry a full source sentence each,
/// which on a large corpus is hundreds of megabytes held for the length of the
/// compile transaction. Only the name/kind aggregate is needed, so feed
/// extractions through here and drop each one immediately.
#[derive(Debug, Clone, Default)]
pub struct PersonIndexBuilder {
    spellings: HashMap<String, HashMap<String, usize>>,
    kinds: HashMap<String, &'static str>,
}

impl PersonIndexBuilder {
    /// Fold one chunk's entities into the aggregate.
    pub fn add(&mut self, extraction: &Extraction) {
        for mention in &extraction.entities {
            let key = hygiene::dedup_key(&mention.name);
            *self
                .spellings
                .entry(key.clone())
                .or_default()
                .entry(mention.name.clone())
                .or_insert(0) += mention.mentions.max(1);
            let kind = self.kinds.entry(key).or_insert(mention.kind);
            *kind = merge_kind(kind, mention.kind);
        }
    }

    /// Freeze the aggregate into the lookup index.
    pub fn finish(self) -> PersonIndex {
        PersonIndex::from_parts(self.spellings, self.kinds)
    }
}

impl PersonIndex {
    /// Build the index from a first pass over every chunk of a KB.
    pub fn build<'a, I>(extractions: I) -> Self
    where
        I: IntoIterator<Item = &'a Extraction>,
    {
        let mut builder = PersonIndexBuilder::default();
        for extraction in extractions {
            builder.add(extraction);
        }
        builder.finish()
    }

    fn from_parts(
        spellings: HashMap<String, HashMap<String, usize>>,
        kinds: HashMap<String, &'static str>,
    ) -> Self {
        let display: HashMap<String, String> = spellings
            .iter()
            .map(|(key, votes)| (key.clone(), best_spelling(votes)))
            .collect();
        let mut full_names: Vec<String> = kinds
            .iter()
            .filter(|(key, kind)| **kind == KIND_PERSON && key.split(' ').count() >= 2)
            .map(|(key, _)| key.clone())
            .collect();
        full_names.sort();

        // ONE PERSON PER FULL NAME, before anything folds into one. "Patrick
        // Pruett" and "Patrick Ashcomb Pruett" are one man, and while both
        // stand they are two rival full names under one first name — so the
        // bare "Patrick" (18 mentions) folded into neither. Collapse them
        // first; the survivors are what the inverted indexes below see.
        let variants = variant_targets(&full_names);
        full_names.retain(|key| !variants.contains_key(key));

        // Inverted indexes, so a chunk pays a hash lookup per bare name rather
        // than a scan of every person in the KB.
        let mut by_first_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_part: HashMap<String, Vec<String>> = HashMap::new();
        for full in &full_names {
            let mut parts = full.split(' ');
            if let Some(first) = parts.next() {
                by_first_name
                    .entry(first.to_string())
                    .or_default()
                    .push(full.clone());
            }
            for part in full.split(' ') {
                let bucket = by_part.entry(part.to_string()).or_default();
                if !bucket.contains(full) {
                    bucket.push(full.clone());
                }
            }
        }
        Self {
            display,
            variants,
            by_first_name,
            by_part,
        }
    }

    /// True when nothing is known yet (a plain single-chunk `analyze`).
    pub fn is_empty(&self) -> bool {
        self.display.is_empty()
    }

    /// The full-name person a bare `short_key` may fold into, given the full
    /// names of the chunk being analyzed as well as the KB's.
    ///
    /// Returns `None` when nothing matches OR when more than one person could
    /// claim the name — the conservative half of the rule, and the reason two
    /// Lauras never become one.
    fn fold_target(&self, short_key: &str, local_full: &[String]) -> Option<String> {
        let gather = |kb: Option<&Vec<String>>, first_only: bool| -> Vec<String> {
            let mut out: Vec<String> = kb.cloned().unwrap_or_default();
            for local in local_full {
                let hit = if first_only {
                    local.split(' ').next() == Some(short_key)
                } else {
                    local.split(' ').any(|part| part == short_key)
                };
                if hit && !out.contains(local) {
                    out.push(local.clone());
                }
            }
            out.sort();
            out.dedup();
            out
        };
        // Preferred rule: the short form is the person's FIRST name.
        let first = gather(self.by_first_name.get(short_key), true);
        match first.len() {
            1 => return first.into_iter().next(),
            0 => {}
            _ => return None,
        }
        // Fallback: a bare surname ("Pruett" → "Patrick Ashcomb Pruett"),
        // under the same exactly-one rule.
        let any = gather(self.by_part.get(short_key), false);
        (any.len() == 1).then(|| any.into_iter().next().unwrap_or_default())
    }

    /// The fullest spelling of a full name that is a shorter or
    /// differently-accented writing of another one, as a case-folded key.
    fn variant_target(&self, key: &str) -> Option<String> {
        self.variants.get(key).cloned()
    }

    /// Resolve one case-folded key to the person it belongs to, following a
    /// full-name variant when there is one.
    fn canonical_key(&self, key: &str) -> String {
        self.variant_target(key).unwrap_or_else(|| key.to_string())
    }

    /// The spelling this KB should display for `name`.
    ///
    /// Folds case variants: a WhatsApp export that shouts "MARIA MARENOVA"
    /// and a note that writes "Maria Marenova" are one person, shown the way a
    /// human writes a name.
    pub fn display_name(&self, name: &str) -> String {
        self.display
            .get(&hygiene::dedup_key(name))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    fn name_for_key(&self, key: &str) -> Option<&str> {
        self.display.get(key).map(|s| s.as_str())
    }
}

/// Which of two equally-folded spellings of a name to keep: the one that still
/// carries its diacritics. "Francisco Ferró" is how he writes it; "Francisco
/// Ferro" is what a keyboard without the accent produced.
fn accent_rank(key: &str) -> u8 {
    u8::from(key == names::accent_fold(key))
}

/// Full names that are two spellings, or two lengths, of ONE person.
///
/// Returns each redundant full-name key mapped to the key of the fullest
/// spelling of that person. Two rules, applied in order:
///
/// 1. **Accent variants are one name.** `francisco ferro` and `francisco
///    ferró` fold to the same accent-stripped key; the accented spelling is
///    kept, because that is how the man writes it.
/// 2. **A fuller spelling absorbs a shorter one.** `patrick pruett` is
///    `patrick ashcomb pruett` with the middle name left out, and `ming su` is
///    `ming su lim` with the surname left out — every token in order, plus
///    more (see [`names::is_fuller_name`]).
///
/// SAFETY — rule 2 keeps the ambiguity guard that keeps two Lauras apart, in
/// the form it takes for full names: a name yields only when exactly one
/// person can claim it. Where several fuller names compete they must form a
/// CHAIN (`A B` ⊂ `A B C` ⊂ `A B C D`, one person written out by degrees);
/// a FORK (`Maria Lopez` under both `Maria Lopez Garcia` and `Maria Lopez
/// Fernandez`) is two people who share two names, and nothing merges.
fn variant_targets(full_names: &[String]) -> HashMap<String, String> {
    let mut merged: HashMap<String, String> = HashMap::new();

    // 1 — accent variants. BTreeMap so the grouping order never depends on
    // hashing and a KB compiles to the same graph on every run.
    let mut by_fold: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in full_names {
        by_fold
            .entry(names::accent_fold(key))
            .or_default()
            .push(key.clone());
    }
    let mut representatives: Vec<String> = Vec::new();
    for (_, mut group) in by_fold {
        group.sort_by(|a, b| accent_rank(a).cmp(&accent_rank(b)).then_with(|| a.cmp(b)));
        let Some((keep, rest)) = group.split_first() else {
            continue;
        };
        for key in rest {
            merged.insert(key.clone(), keep.clone());
        }
        representatives.push(keep.clone());
    }
    representatives.sort();

    // 2 — a fuller spelling of the same name.
    for short in &representatives {
        let mut fuller: Vec<&String> = representatives
            .iter()
            .filter(|long| names::is_fuller_name(short, long))
            .collect();
        fuller.sort_by_key(|k| (k.split(' ').count(), (*k).clone()));
        let Some(fullest) = fuller.last().copied() else {
            continue;
        };
        // A chain collapses onto its longest link; a fork stays two people.
        if fuller
            .iter()
            .all(|c| *c == fullest || names::is_fuller_name(c, fullest))
        {
            merged.insert(short.clone(), fullest.clone());
        }
    }

    // Follow the chains: "francisco ferro" → "francisco ferró" → "francisco
    // ferró oliver". Every hop either drops an accent variant or adds a token,
    // so the walk cannot cycle; the bound is belt-and-braces.
    let keys: Vec<String> = merged.keys().cloned().collect();
    for key in keys {
        let mut target = merged[&key].clone();
        for _ in 0..representatives.len() {
            match merged.get(&target) {
                Some(next) if *next != target => target = next.clone(),
                _ => break,
            }
        }
        if target == key {
            merged.remove(&key);
        } else {
            merged.insert(key, target);
        }
    }
    merged
}

/// How badly a spelling reads as a written name: SHOUTED tokens first ("MARIA
/// MARENOVA"), then tokens that were never capitalized at all. Lower is better.
fn spelling_penalty(name: &str) -> (usize, usize) {
    let shouty = name
        .split_whitespace()
        .filter(|t| t.chars().count() >= 2 && t.chars().all(|c| !c.is_lowercase()))
        .count();
    let muted = name
        .split_whitespace()
        .filter(|t| t.chars().next().is_some_and(|c| c.is_lowercase()))
        .count()
        .saturating_sub(
            // Name particles ("van", "de") are lowercase on purpose.
            name.split_whitespace()
                .filter(|t| NAME_PARTICLES.contains(&t.to_lowercase().as_str()))
                .count(),
        );
    (shouty, muted)
}

/// Is `candidate` a better-written spelling of the same name than `current`?
/// Ties keep `current`, so first appearance wins and the result is stable.
fn prefer_spelling(current: &str, candidate: &str) -> bool {
    spelling_penalty(candidate) < spelling_penalty(current)
}

/// Pick the spelling to show for one entity: best-written first, then the most
/// frequent, then alphabetical so the result never depends on hash order.
fn best_spelling(votes: &HashMap<String, usize>) -> String {
    votes
        .iter()
        .min_by(|(a_name, a_votes), (b_name, b_votes)| {
            spelling_penalty(a_name)
                .cmp(&spelling_penalty(b_name))
                .then_with(|| b_votes.cmp(a_votes))
                .then_with(|| a_name.cmp(b_name))
        })
        .map(|(name, _)| name.clone())
        .unwrap_or_default()
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
///
/// This is the single-chunk view (no KB context). The compiler uses
/// [`analyze_with_index`] so bare first names resolve KB-wide.
pub fn analyze(text: &str) -> Extraction {
    analyze_with_index(text, &PersonIndex::default())
}

/// [`analyze`] with everything the KB already knows about its people.
///
/// The index changes exactly two things: a bare first name folds into the full
/// person even when that full name appears in a DIFFERENT chunk, and a name
/// the KB writes several ways is emitted with one spelling.
pub fn analyze_with_index(text: &str, index: &PersonIndex) -> Extraction {
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
        let Canonical {
            name,
            aliases,
            person_hint,
        } = canonical;
        // A title inside the surface ("Dr Patel") is stripped from the
        // canonical name, so check it before it is gone.
        let titled = phrase
            .surface
            .split_whitespace()
            .next()
            .map(hygiene::is_honorific)
            .unwrap_or(false);
        // A greeting ("Heyy Elias") or a contact label ("Maria Microsoft
        // London Tech") addresses a human by construction — that is evidence
        // in its own right, and without it a repaired one-token name would
        // fall back to `concept` and vanish from the networking surface.
        let evidence = titled
            || *person_hint
            || person_evidence(&toks, phrase.start, phrase.end, bounds_of(phrase.sentence));
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
        // A WhatsApp export shouts the contact name on every line
        // ("BRANISLAVA MARENOVIC:") and the prose around it writes it
        // normally. Same person, so keep the spelling a human would write.
        if prefer_spelling(&entry.name, name) {
            entry.name = name.clone();
        }
        for alias in aliases {
            if !entry.aliases.iter().any(|a| a == alias) {
                entry.aliases.push(alias.clone());
            }
        }
        occurrences.push((phrase.clone(), key));
    }

    // One spelling per person, KB-wide ("MARIA MARENOVA" → "Maria Marenova").
    //
    // This runs BEFORE the fold on purpose. A WhatsApp export shouts the
    // contact name on every line ("MARIA:"), and an ALL-CAPS token is an
    // acronym to `looks_like_person_name`, so the fold below used to skip it —
    // then this very rewrite turned it into a perfectly ordinary bare "Maria"
    // that stood up its own node. That is how the real store ended up with
    // "Maria" (432 mentions) beside "Maria Marenova" (21): not a broken
    // ambiguity rule, a fold that never got to run on the shouted spelling.
    if !index.is_empty() {
        for entry in acc.values_mut() {
            let display = index.display_name(&entry.name);
            if display != entry.name {
                entry.aliases.retain(|a| a != &display);
                entry.name = display;
            }
        }
    }

    // Pass 2 — fold a bare first name into the full person it belongs to
    // ("Avi" → alias of "Avi Roy"), so the graph has one node per human.
    //
    // Candidates are this chunk's full names PLUS every full name the KB
    // knows, which is what makes the fold work across chunks — and what makes
    // the ambiguity test honest: two Lauras anywhere in the KB block the fold
    // in every chunk, including the ones that mention only one of them.
    // Each local full name is named by the person it belongs to, not by the
    // spelling this chunk happened to use: a chunk that writes "Patrick
    // Pruett" must not make "Patrick" ambiguous against the KB's "Patrick
    // Ashcomb Pruett", because those are the same man.
    let local_full: Vec<String> = acc
        .values()
        .filter(|a| a.kind == KIND_PERSON && a.name.split_whitespace().count() >= 2)
        .map(|a| index.canonical_key(&hygiene::dedup_key(&a.name)))
        .collect();

    let mut merge_order: Vec<(String, String)> = Vec::new();
    for (key, entry) in acc.iter() {
        if entry.kind == KIND_ORGANIZATION || entry.kind == KIND_GENE_OR_PROTEIN {
            continue;
        }
        let target = if entry.name.split_whitespace().count() == 1 {
            // A place or a creed never folds into a person, whatever it
            // resembles. The test reads a shouted spelling as the name it is,
            // so "MARIA:" is eligible for the same fold as "Maria".
            if !looks_like_person_name(&as_written_name(&entry.name)) {
                continue;
            }
            index.fold_target(key, &local_full)
        } else {
            // A full name that is a shorter or differently-accented writing of
            // another full name is the same person: "Patrick Pruett" into
            // "Patrick Ashcomb Pruett", "Francisco Ferro" into "Francisco
            // Ferró Oliver". This verdict is KB-wide by construction — one
            // chunk cannot know which other spellings exist.
            index.variant_target(key)
        };
        if let Some(full) = target.filter(|full| full != key) {
            merge_order.push((key.clone(), full));
        }
    }
    // Sorted, so a chunk compiles to the same graph on every run.
    merge_order.sort();
    let redirect: HashMap<String, String> = merge_order.iter().cloned().collect();

    for (short_key, full_key) in merge_order {
        let Some(short) = acc.remove(&short_key) else {
            continue;
        };
        match acc.get_mut(&full_key) {
            Some(full) => {
                full.mentions += short.mentions;
                for alias in std::iter::once(short.name.clone()).chain(short.aliases) {
                    if !full.aliases.iter().any(|a| a == &alias) && alias != full.name {
                        full.aliases.push(alias);
                    }
                }
            }
            None => {
                // The full name lives in another chunk. Re-key this mention
                // onto it — dropping it (the old behaviour, which assumed the
                // target was local) would lose the mention entirely.
                let name = index
                    .name_for_key(&full_key)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| short.name.clone());
                let mut aliases = short.aliases.clone();
                if short.name != name && !aliases.contains(&short.name) {
                    aliases.push(short.name.clone());
                }
                aliases.retain(|a| a != &name);
                acc.insert(
                    full_key,
                    Acc {
                        name,
                        aliases,
                        kind: KIND_PERSON,
                        mentions: short.mentions,
                        order: short.order,
                    },
                );
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
    fn places_and_creeds_are_never_people() {
        // Every one of these was a top-25 `person` in the real store.
        let k = kinds(
            "I met Lira in London last month. We flew to Ibiza with my friend Avi. \
             Singapore was next. Islam came up over dinner with her.",
        );
        for junk in ["London", "Ibiza", "Singapore", "Islam"] {
            assert_ne!(
                k.get(junk).copied(),
                Some(KIND_PERSON),
                "{junk} must not be person-typed: {k:?}"
            );
        }
        // …and the actual humans in the same sentences survive.
        assert_eq!(k.get("Lira").copied(), Some(KIND_PERSON));
        assert_eq!(k.get("Avi").copied(), Some(KIND_PERSON));
    }

    #[test]
    fn chat_filler_never_reaches_the_graph() {
        let ex = analyze("Oh nice. Good morning Elias. Thanks Elias! Ok sure. Heyy Elias.");
        for junk in [
            "Oh",
            "Good",
            "Thanks",
            "Ok",
            "Sure",
            "Morning",
            "Heyy Elias",
        ] {
            assert!(
                !ex.entities.iter().any(|e| e.name == junk),
                "{junk} leaked: {:?}",
                ex.entities
            );
        }
        let elias = ex
            .entities
            .iter()
            .find(|e| e.name == "Elias")
            .expect("Elias survives every salutation");
        assert_eq!(e_kind(&ex, "Elias"), Some(KIND_PERSON));
        assert_eq!(
            elias.mentions, 3,
            "every greeting must count for the person: {elias:?}"
        );
    }

    fn e_kind(ex: &Extraction, name: &str) -> Option<&'static str> {
        ex.kind_of(name)
    }

    #[test]
    fn a_contact_label_resolves_to_the_person_it_names() {
        let ex = analyze("Maria Microsoft London Tech sent the deck over.");
        let maria = ex
            .entities
            .iter()
            .find(|e| e.name == "Maria")
            .expect("the label's personal-name prefix is the node");
        assert_eq!(maria.kind, KIND_PERSON, "a contact label names a human");
        assert!(maria
            .aliases
            .contains(&"Maria Microsoft London Tech".to_string()));
        assert!(
            !ex.entities
                .iter()
                .any(|e| e.name == "Maria Microsoft London Tech"),
            "the label must not stand up a rival node: {:?}",
            ex.entities
        );
    }

    /// Every junk string the real store still produced after the first hygiene
    /// pass, with its measured mention count: "Partner Rosalie" 64, "Wants
    /// Elias" 28, "Said" 11, "Says" 8, "Chelsea Flower" 24.
    #[test]
    fn leading_non_name_words_never_produce_a_person() {
        let ex = analyze(
            "Partner Rosalie called about the flat. Wants Elias to confirm the booking. \
             Said the meeting is at noon. Says the deck is ready. \
             Chelsea Flower tickets arrived for my friend Rosalie.",
        );
        for junk in [
            "Partner Rosalie",
            "Wants Elias",
            "Said",
            "Says",
            "Chelsea Flower",
            "Partner",
        ] {
            assert_ne!(
                ex.kind_of(junk),
                Some(KIND_PERSON),
                "{junk} must not be person-typed: {:?}",
                ex.entities
            );
        }
        for gone in ["Said", "Says", "Partner"] {
            assert!(
                ex.kind_of(gone).is_none(),
                "{gone} must not be an entity at all: {:?}",
                ex.entities
            );
        }
        // The humans behind the junk survive, and the junk is not their alias.
        let rosalie = ex
            .entities
            .iter()
            .find(|e| e.name == "Rosalie")
            .expect("the role word folds onto the person");
        assert_eq!(rosalie.kind, KIND_PERSON);
        assert_eq!(
            rosalie.mentions, 2,
            "the prefixed mention counts: {rosalie:?}"
        );
        assert!(
            rosalie.aliases.is_empty(),
            "a relationship word is nobody's name: {:?}",
            rosalie.aliases
        );
        let elias = ex
            .entities
            .iter()
            .find(|e| e.name == "Elias")
            .expect("the verb folds onto the person");
        assert!(elias.aliases.is_empty(), "{:?}", elias.aliases);
    }

    /// The conservative half of the truncated-place rule.
    #[test]
    fn two_part_surnames_survive_the_truncated_place_rule() {
        let k = kinds(
            "Jordan Peterson wrote back. My friend Jisoo Park called. \
             Laura Corvo and Milan Kundera joined.",
        );
        for person in [
            "Jordan Peterson",
            "Jisoo Park",
            "Laura Corvo",
            "Milan Kundera",
        ] {
            assert_eq!(
                k.get(person).copied(),
                Some(KIND_PERSON),
                "{person} must stay a person: {k:?}"
            );
        }
    }

    /// The KB-wide pass: the same person written two ways in two chunks.
    #[test]
    fn kb_wide_index_folds_a_bare_first_name_across_chunks() {
        let chunks = [
            "Noemi Varkoni introduced me to Avi Roy at the summit.",
            "Noemi sent the deck this morning.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);
        let second = analyze_with_index(chunks[1], &index);

        assert!(
            !second.entities.iter().any(|e| e.name == "Noemi"),
            "the bare first name must not stand up a second node: {:?}",
            second.entities
        );
        let noemi = second
            .entities
            .iter()
            .find(|e| e.name == "Noemi Varkoni")
            .expect("folded onto the full person");
        assert_eq!(noemi.kind, KIND_PERSON);
        assert_eq!(noemi.mentions, 1, "the mention must survive the fold");
        assert!(noemi.aliases.contains(&"Noemi".to_string()));
    }

    /// SAFETY: two different humans share a first name. Merging them would
    /// silently fuse a friend with someone from a dating thread.
    #[test]
    fn two_full_names_sharing_a_first_name_never_fuse() {
        let chunks = [
            "Laura Corvo is a friend from the Barcelona trip.",
            "Laura Quiroz is someone I am dating.",
            "Laura texted me about dinner.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);

        let bare = analyze_with_index(chunks[2], &index);
        assert!(
            bare.entities.iter().any(|e| e.name == "Laura"),
            "the ambiguous bare name must stay its own node: {:?}",
            bare.entities
        );
        for chunk in &chunks {
            let ex = analyze_with_index(chunk, &index);
            for person in &ex.entities {
                assert!(
                    !person.aliases.iter().any(|a| a == "Laura"),
                    "{} absorbed the ambiguous bare name: {:?}",
                    person.name,
                    person.aliases
                );
            }
        }
        // Both full names survive, separately.
        let corvo = analyze_with_index(chunks[0], &index);
        assert!(corvo.entities.iter().any(|e| e.name == "Laura Corvo"));
        let moreno = analyze_with_index(chunks[1], &index);
        assert!(moreno.entities.iter().any(|e| e.name == "Laura Quiroz"));
    }

    /// A WhatsApp export shouts the contact name on every line. The shouted
    /// spelling is an acronym to `looks_like_person_name`, so the fold used to
    /// skip it — and the display rewrite that ran afterwards handed the KB a
    /// perfectly ordinary bare "Maria" with its own node. 432 mentions of one
    /// woman under a name that was not hers.
    #[test]
    fn a_shouted_bare_first_name_still_folds_into_the_full_person() {
        let chunks = [
            "Maria Marenova is my partner and she cooked dinner.",
            "MARIA: call me back when you can.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);
        let shouted = analyze_with_index(chunks[1], &index);

        assert!(
            !shouted
                .entities
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case("maria")),
            "the shouted first name stood up its own node: {:?}",
            shouted.entities
        );
        let maria = shouted
            .entities
            .iter()
            .find(|e| e.name == "Maria Marenova")
            .expect("folded onto the full person");
        assert_eq!(maria.kind, KIND_PERSON);
        assert_eq!(maria.mentions, 1);
    }

    #[test]
    fn case_variants_of_one_name_collapse_to_the_written_spelling() {
        let chunks = [
            "MARIA MARENOVA: call me back.",
            "Maria Marenova is my cousin and she called on Sunday.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);
        assert_eq!(index.display_name("MARIA MARENOVA"), "Maria Marenova");
        let shouted = analyze_with_index(chunks[0], &index);
        assert!(
            shouted.entities.iter().any(|e| e.name == "Maria Marenova"),
            "the shouted spelling must display like a name: {:?}",
            shouted.entities
        );
    }

    #[test]
    fn indexed_analysis_is_deterministic() {
        let chunks = [
            "Avi Roy introduced me to Noemi Varkoni at the summit.",
            "Avi emailed me. Noemi replied to Patrick Ashcomb Pruett.",
            "Patrick asked about the raise.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);
        for chunk in &chunks {
            let a = analyze_with_index(chunk, &index);
            let b = analyze_with_index(chunk, &index);
            assert_eq!(
                a.entities.iter().map(|e| &e.name).collect::<Vec<_>>(),
                b.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
            );
            assert_eq!(a.edges, b.edges);
        }
        // The bare surname folds onto the one person carrying it.
        let last = analyze_with_index(chunks[2], &index);
        assert!(
            last.entities
                .iter()
                .any(|e| e.name == "Patrick Ashcomb Pruett"),
            "{:?}",
            last.entities
        );
    }

    /// THE CONSEQUENCE the junk filters exist for.
    ///
    /// On the real store "Maria" (431 mentions) never joined "Maria Marenova"
    /// (22) and "Noemi" (119) never joined "Noemi Varkoni" (54). The fold rule
    /// was right — it refuses when two full names compete for a first name —
    /// but the KB was full of junk that LOOKED like competing full names:
    /// "Maria Ibiza-period", "Noemi Varkoni's Lipitrex". With the junk gone
    /// there is exactly one candidate again and the fold fires.
    #[test]
    fn junk_full_names_no_longer_block_the_kb_wide_fold() {
        let chunks = [
            "Maria Marenova is my partner and she cooked dinner.",
            "Maria Ibiza-period photos were uploaded to the album.",
            "Maria's Nov 2025 Egypt itinerary arrived last night.",
            "MARIA RE-INITIATES AGAIN about the Egypt plan.",
            "Noemi Varkoni introduced me to Avi at the summit.",
            "Noemi Varkoni's Lipitrex prescription was renewed.",
            "Noemi's Southwark flat viewing is on Tuesday.",
            "Maria sent the invoice this morning.",
            "Noemi sent the deck this morning.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);

        for (bare_chunk, full) in [(7, "Maria Marenova"), (8, "Noemi Varkoni")] {
            let ex = analyze_with_index(chunks[bare_chunk], &index);
            let bare = full.split(' ').next().unwrap();
            assert!(
                !ex.entities.iter().any(|e| e.name == bare),
                "{bare} stood up a second node: {:?}",
                ex.entities
            );
            let person = ex
                .entities
                .iter()
                .find(|e| e.name == full)
                .unwrap_or_else(|| panic!("{bare} did not fold into {full}: {:?}", ex.entities));
            assert_eq!(person.kind, KIND_PERSON);
            assert!(person.aliases.contains(&bare.to_string()));
        }

        // …and no junk survived as a person to compete in the first place.
        for chunk in &chunks {
            let ex = analyze_with_index(chunk, &index);
            for junk in [
                "Maria Ibiza-period",
                "Maria's Nov 2025 Egypt",
                "MARIA RE-INITIATES AGAIN",
                "Noemi Varkoni's Lipitrex",
                "Noemi's Southwark",
            ] {
                assert!(
                    !ex.entities.iter().any(|e| e.name == junk),
                    "{junk} survived: {:?}",
                    ex.entities
                );
            }
        }
    }

    /// The hijack: a junk "full name" that CONTAINS a real human's name steals
    /// the canonical slot through the bare-surname fallback. "Reading Lira"
    /// (449 mentions) owned Lira, with the woman herself demoted to an alias.
    #[test]
    fn a_verb_prefixed_junk_name_no_longer_absorbs_the_human() {
        let chunks = [
            "Reading Lira a bedtime story is the best part of the day.",
            "Lira is my daughter and she turned four in the spring.",
            "Avi's One Blackfriars party was packed with the fund crowd.",
            "Avi is a friend from the longevity circuit.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);
        for (chunk, human) in [(0, "Lira"), (2, "Avi")] {
            let ex = analyze_with_index(chunks[chunk], &index);
            assert_eq!(
                ex.entities
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>(),
                vec![human],
                "the junk surface must resolve to {human} alone"
            );
        }
        // The human owns the canonical slot in the chunk that describes her.
        let ex = analyze_with_index(chunks[1], &index);
        assert_eq!(ex.kind_of("Lira"), Some(KIND_PERSON));
    }

    /// A double-barrelled surname carries an interior capital, which the flat
    /// Title-Case token test read as an acronym. "Rosalie Brandtke-Wendel" was
    /// therefore typed `concept`, was never a fold candidate, and "Rosalie"
    /// (65 mentions in the real store) stayed a bare first name.
    #[test]
    fn a_compound_surname_is_still_a_person() {
        let k = kinds(
            "Rosalie Brandtke-Wendel sent the tenancy paperwork. Jean-Pierre Dupont replied. \
             Andrew Lloyd-Webber called about the score. Sinead O'Brien joined the board.",
        );
        for person in [
            "Rosalie Brandtke-Wendel",
            "Jean-Pierre Dupont",
            "Andrew Lloyd-Webber",
            "Sinead O'Brien",
        ] {
            assert_eq!(
                k.get(person).copied(),
                Some(KIND_PERSON),
                "{person} must be a person: {k:?}"
            );
        }
        // …and the hyphenated things that are NOT names stay out.
        let k = kinds("SARS-CoV-2 and IL-6 rose. New York was cold.");
        for junk in ["SARS-CoV-2", "IL-6", "New York"] {
            assert_ne!(k.get(junk).copied(), Some(KIND_PERSON), "{junk}: {k:?}");
        }
    }

    /// The bare first name only folds when ONE full name can claim it, and
    /// "Patrick Pruett" and "Patrick Ashcomb Pruett" were two — so "Patrick"
    /// (18 mentions) joined neither, and one man held two rows.
    #[test]
    fn a_middle_name_is_not_a_different_man() {
        let chunks = [
            "Patrick Pruett asked about the raise and he sounded positive.",
            "Patrick Ashcomb Pruett signed the term sheet on Tuesday.",
            "Patrick texted me about dinner on Sunday.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);

        for chunk in &chunks {
            let ex = analyze_with_index(chunk, &index);
            assert!(
                ex.entities
                    .iter()
                    .any(|e| e.name == "Patrick Ashcomb Pruett"),
                "every spelling must resolve to the fullest name: {:?}",
                ex.entities
            );
            for rival in ["Patrick", "Patrick Pruett"] {
                assert!(
                    !ex.entities.iter().any(|e| e.name == rival),
                    "{rival} stood up its own node: {:?}",
                    ex.entities
                );
            }
        }
        let short = analyze_with_index(chunks[0], &index);
        let patrick = short
            .entities
            .iter()
            .find(|e| e.name == "Patrick Ashcomb Pruett")
            .expect("the fullest name owns the node");
        assert_eq!(patrick.kind, KIND_PERSON);
        assert!(
            patrick.aliases.contains(&"Patrick Pruett".to_string()),
            "the shorter spelling must stay resolvable: {:?}",
            patrick.aliases
        );
    }

    /// One man, two keyboards. "Francisco Ferro", "Francisco Ferró" and
    /// "Francisco Ferró Oliver" were three person rows, and three rival full
    /// names under one first name — so "Francisco" (42) folded into none.
    #[test]
    fn an_accent_is_not_a_different_man() {
        let chunks = [
            "Francisco Ferro wrote back about the Barcelona flat.",
            "Francisco Ferró is my friend from the Madrid trip.",
            "Francisco Ferró Oliver was copied on the same email thread.",
            "I called Francisco this morning about the paperwork.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);

        for chunk in &chunks {
            let ex = analyze_with_index(chunk, &index);
            assert!(
                ex.entities
                    .iter()
                    .any(|e| e.name == "Francisco Ferró Oliver"),
                "every spelling must resolve to the one man: {:?}",
                ex.entities
            );
        }
        // The unaccented spelling is kept as a spelling, never as a person.
        let ascii = analyze_with_index(chunks[0], &index);
        let francisco = ascii
            .entities
            .iter()
            .find(|e| e.name == "Francisco Ferró Oliver")
            .expect("the accented, fullest name owns the node");
        assert!(
            francisco
                .aliases
                .contains(&"Francisco Ferro".to_string()),
            "the unaccented spelling must stay resolvable: {:?}",
            francisco.aliases
        );
    }

    /// SAFETY, the full-name form of the two-Lauras rule: a shorter name that
    /// TWO fuller names could claim belongs to neither. Spanish double
    /// surnames are the everyday case — Maria Lopez Garcia and Maria Lopez
    /// Fernandez are two women, not one written out twice.
    #[test]
    fn a_name_two_fuller_names_could_claim_merges_into_neither() {
        let chunks = [
            "Maria Lopez Garcia is my colleague and she reviewed the deck.",
            "Maria Lopez Fernandez is my dentist and she called back.",
            "Maria Lopez sent the invoice this morning.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);

        for (chunk, expected) in chunks.iter().zip([
            "Maria Lopez Garcia",
            "Maria Lopez Fernandez",
            "Maria Lopez",
        ]) {
            let ex = analyze_with_index(chunk, &index);
            assert!(
                ex.entities.iter().any(|e| e.name == expected),
                "{expected} was fused away: {:?}",
                ex.entities
            );
            for person in &ex.entities {
                assert!(
                    !person.aliases.iter().any(|a| a == "Maria Lopez")
                        || person.name == "Maria Lopez",
                    "{} absorbed the ambiguous name: {:?}",
                    person.name,
                    person.aliases
                );
            }
        }
    }

    /// A CV section heading makes a second person-typed "full name" under one
    /// first name, which is the ambiguity that blocked the fold: "Avi" (60
    /// mentions) never joined "Avi Roy" (22) while "Avi Roy Professional
    /// Background" stood beside it.
    #[test]
    fn a_heading_full_name_no_longer_blocks_the_fold() {
        let chunks = [
            "Avi Roy Professional Background is the section I pasted out of his CV.",
            "Avi Roy runs the longevity fund and he called about the raise.",
            "Avi is a friend from the longevity circuit and he called today.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);

        for chunk in &chunks {
            let ex = analyze_with_index(chunk, &index);
            assert!(
                !ex.entities
                    .iter()
                    .any(|e| e.name == "Avi Roy Professional Background"),
                "the heading survived: {:?}",
                ex.entities
            );
        }
        let bare = analyze_with_index(chunks[2], &index);
        let avi = bare
            .entities
            .iter()
            .find(|e| e.name == "Avi Roy")
            .unwrap_or_else(|| panic!("Avi did not fold into Avi Roy: {:?}", bare.entities));
        assert_eq!(avi.kind, KIND_PERSON);
        assert!(avi.aliases.contains(&"Avi".to_string()));
    }

    /// The variant merge must not drift between runs: it is built from a
    /// HashMap of kinds, so the grouping is sorted before anything collapses.
    #[test]
    fn variant_merging_is_deterministic() {
        let chunks = [
            "Patrick Pruett asked about the raise.",
            "Patrick Ashcomb Pruett signed the term sheet.",
            "Francisco Ferro wrote back about the flat.",
            "Francisco Ferró Oliver was copied on the thread.",
            "Ming Su Lim is my colleague and she reviewed the deck.",
            "Ming Su sent the revised deck this afternoon.",
        ];
        let first: Vec<Extraction> = chunks.iter().map(|c| analyze(c)).collect();
        let index = PersonIndex::build(&first);
        for chunk in &chunks {
            let a = analyze_with_index(chunk, &index);
            let b = analyze_with_index(chunk, &index);
            assert_eq!(
                a.entities.iter().map(|e| &e.name).collect::<Vec<_>>(),
                b.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
            );
        }
        // A two-token given name plus a surname is the same shape as a middle
        // name, and merges the same way.
        let ex = analyze_with_index(chunks[5], &index);
        assert!(
            ex.entities.iter().any(|e| e.name == "Ming Su Lim"),
            "{:?}",
            ex.entities
        );
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
