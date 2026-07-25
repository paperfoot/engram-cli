//! Personal-memory benchmark — the suite for what engram is actually used for.
//!
//! Every other harness in this crate (LitQA2, BEIR, LongMemEval, LoCoMo,
//! BioGen) evaluates scientific-corpus or conversational-log QA. None of them
//! measures the production use case: *answering questions about people from
//! stored personal facts*. This module is that missing quality gate.
//!
//! The dataset is synthetic, fully fictional, committed in-tree, and needs no
//! network: 53 memories spread over four knowledge bases (`personal`,
//! `relationships`, `business`, `projects`) plus 43 queries with gold memory
//! ids. It deliberately encodes the hazards a real personal store contains:
//!
//! - **paraphrase near-duplicates** — "Lira Marchetti's birthday is 12 March"
//!   next to "Lira Marchetti was born on March 12"; retrieval must find the
//!   fact without letting the pair crowd out everything else.
//! - **multi-fact paragraph blobs** — one memory holding four facts, queried
//!   for the *third* one, so chunk-level scoring cannot coast on the headline.
//! - **cross-KB placement** — person facts filed under `business`, landlord
//!   facts inside a `personal` moving blob; queries carry no KB hint, exactly
//!   like an agent that does not know where the user filed something.
//! - **event-time temporal facts** — memories whose `event_time` (when the
//!   thing happened) is months away from their `created_at` (when it was
//!   written down), including facts whose text never names the month, so a
//!   "what happened in February" query can only be answered from event time.
//!
//! Several categories are *expected to score badly* on today's defaults — that
//! is the point. The suite exists to make the regressions measurable before
//! and after the retrieval work that fixes them.
//!
//! Scoring lives here (pure functions, unit-tested). Seeding goes through the
//! same storage/chunker/embedder calls as `engram remember`. The retrieval
//! driver lives in the CLI (`engram bench personal`) because the hybrid recall
//! pipeline is defined there.

use crate::error::BenchError;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use engram_embed::{Embedder, TaskMode};
use engram_ingest::chunker::naive_split;
use engram_storage::SqliteStore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// When every memory was *written down*. Identical for all rows so that a
/// temporal query can only be answered from `event_time`, never from insertion
/// order or creation date.
pub const INGESTED_AT: &str = "2026-06-01T09:00:00Z";

/// Knowledge bases the dataset is spread across, mirroring a real store
/// (personal / relationships / business / project namespaces).
pub const KBS: &[(&str, &str)] = &[
    ("personal", "Personal life: home, health, admin, routines"),
    (
        "relationships",
        "People: birthdays, preferences, introductions",
    ),
    (
        "business",
        "Business decisions, customers, competitors, money",
    ),
    ("projects", "Project and product context"),
];

/// The KB an agent searches when the caller gives no hint. Matches the
/// `--kb` default on `engram recall`; queries with `kb_hint: None` are scored
/// through this filter precisely because that is what fails today.
pub const DEFAULT_KB: &str = "default";

/// Diary namespace used for every seeded memory (the `recall --diary` default).
pub const DIARY: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryCategory {
    /// Look up a single fact about a person, with the right KB.
    PersonFact,
    /// "What happened in February" — answerable only from event time.
    Temporal,
    /// No KB hint: the fact lives somewhere other than the default KB.
    CrossKb,
    /// Two memories say the same thing in different words.
    ParaphraseDedup,
    /// The answer is buried mid-paragraph in a multi-fact blob.
    MultiFact,
    /// The fact is NOT in the store; the right answer is "I don't know".
    NegativeControl,
}

impl QueryCategory {
    pub const ALL: &'static [QueryCategory] = &[
        QueryCategory::PersonFact,
        QueryCategory::Temporal,
        QueryCategory::CrossKb,
        QueryCategory::ParaphraseDedup,
        QueryCategory::MultiFact,
        QueryCategory::NegativeControl,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            QueryCategory::PersonFact => "person_fact",
            QueryCategory::Temporal => "temporal",
            QueryCategory::CrossKb => "cross_kb",
            QueryCategory::ParaphraseDedup => "paraphrase_dedup",
            QueryCategory::MultiFact => "multi_fact",
            QueryCategory::NegativeControl => "negative_control",
        }
    }
}

/// One seeded memory. `content` is stored verbatim through the same write path
/// `engram remember` uses (chunk -> embed -> insert with embedding metadata).
#[derive(Debug, Clone)]
pub struct PersonalMemory {
    /// Stable dataset id (also the gold id used by queries).
    pub id: &'static str,
    pub kb: &'static str,
    pub content: &'static str,
    pub tags: &'static [&'static str],
    pub importance: u8,
    /// When the described event happened (`YYYY-MM-DD`), not when it was
    /// written down. `None` means the fact is timeless (a birthday rule, a
    /// preference).
    pub event_date: Option<&'static str>,
    /// Set on both halves of a paraphrase pair, pointing at the other half.
    pub near_duplicate_of: Option<&'static str>,
}

/// One benchmark query.
#[derive(Debug, Clone)]
pub struct PersonalQuery {
    pub id: &'static str,
    pub text: &'static str,
    pub category: QueryCategory,
    /// `None` = the caller gave no KB hint. Scored against the default-KB
    /// filter (today's behaviour) and, as a ceiling, against all KBs.
    pub kb_hint: Option<&'static str>,
    /// Dataset memory ids that answer the query. Empty for negative controls.
    pub gold: &'static [&'static str],
    /// Inclusive event-date window (`YYYY-MM-DD`) to pass as `--since/--until`.
    /// `None` on a temporal query means the run gets no filter help at all and
    /// must recover the date from retrieval alone.
    pub window: Option<(&'static str, &'static str)>,
}

/// 53 fictional memories. Names, companies and dates are invented; any
/// resemblance to a real store is the point, not the content.
pub const MEMORIES: &[PersonalMemory] = &[
    // ---------------- relationships ----------------
    PersonalMemory {
        id: "r-lira-bday-1",
        kb: "relationships",
        content: "Lira Marchetti's birthday is 12 March.",
        tags: &["birthday", "lira"],
        importance: 7,
        event_date: None,
        near_duplicate_of: Some("r-lira-bday-2"),
    },
    PersonalMemory {
        id: "r-lira-bday-2",
        kb: "relationships",
        content: "Lira Marchetti was born on March 12, so plan the dinner for the weekend before.",
        tags: &["birthday", "lira"],
        importance: 6,
        event_date: None,
        near_duplicate_of: Some("r-lira-bday-1"),
    },
    PersonalMemory {
        id: "r-ondine-bday-1",
        kb: "relationships",
        content: "Ondine Kessler's birthday is 30 September.",
        tags: &["birthday", "ondine"],
        importance: 6,
        event_date: None,
        near_duplicate_of: Some("r-ondine-bday-2"),
    },
    PersonalMemory {
        id: "r-ondine-bday-2",
        kb: "relationships",
        content: "Ondine was born on the thirtieth of September, right at the end of the quarter.",
        tags: &["birthday", "ondine"],
        importance: 5,
        event_date: None,
        near_duplicate_of: Some("r-ondine-bday-1"),
    },
    PersonalMemory {
        id: "r-ondine-intro",
        kb: "relationships",
        content: "Ondine Kessler introduced me to Rafferty Nwosu at the Trellis conference in Lisbon.",
        tags: &["introduction", "ondine", "rafferty"],
        importance: 6,
        event_date: Some("2025-02-14"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-ondine-loc",
        kb: "relationships",
        content: "Ondine Kessler relocated to Ghent and now works remotely.",
        tags: &["location", "ondine"],
        importance: 5,
        event_date: Some("2025-02-03"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-rafferty-role",
        kb: "relationships",
        content: "Rafferty Nwosu runs infrastructure at Halden Freight and prefers to be reached on Signal, never by phone.",
        tags: &["rafferty", "contact"],
        importance: 6,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-milena-blob",
        kb: "relationships",
        content: "Milena Toft moved from Oslo to Porto, she is allergic to shellfish, her daughter started school in September, and she only drinks decaf after two in the afternoon.",
        tags: &["milena", "blob"],
        importance: 6,
        event_date: Some("2025-04-08"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-milena-gift",
        kb: "relationships",
        content: "Milena Toft's favourite wine is a dry Vinho Verde; she brought one to every dinner last year.",
        tags: &["milena", "gift"],
        importance: 4,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-cass-anniv",
        kb: "relationships",
        content: "Cass Aberdeen and Petra Lindqvist got married on 21 June; they celebrate with a hike, not a party.",
        tags: &["cass", "petra", "anniversary"],
        importance: 6,
        event_date: Some("2025-06-21"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-yusuf-bday",
        kb: "relationships",
        content: "Yusuf Halloran's birthday is 3 November and he hates surprise parties.",
        tags: &["birthday", "yusuf"],
        importance: 6,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-petra-pref",
        kb: "relationships",
        content: "Petra Lindqvist reads only paper books and refuses e-readers as gifts.",
        tags: &["petra", "preference"],
        importance: 5,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-bram-kids",
        kb: "relationships",
        content: "Bram Oyelaran has twin sons who were born in 2019.",
        tags: &["bram", "family"],
        importance: 4,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-delphine-intro",
        kb: "relationships",
        content: "Delphine Achebe was introduced to me by Milena Toft during the Porto move.",
        tags: &["introduction", "delphine"],
        importance: 6,
        event_date: Some("2025-04-19"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-ines-loc",
        kb: "relationships",
        content: "Ines Vaquero lives in Valencia and hosts the Thursday supper club.",
        tags: &["ines", "location"],
        importance: 5,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-kiro-allergy",
        kb: "relationships",
        content: "Kiro Vantanen carries an EpiPen for a peanut allergy; always check catering before he visits.",
        tags: &["kiro", "allergy"],
        importance: 8,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-tomas-contact",
        kb: "relationships",
        content: "Tomas Brekke changed his number; the old one still forwards to his partner.",
        tags: &["tomas", "contact"],
        importance: 5,
        event_date: Some("2025-03-19"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "r-lira-pref",
        kb: "relationships",
        content: "Lira Marchetti dislikes cut flowers and would rather have a plant that lasts.",
        tags: &["lira", "preference"],
        importance: 5,
        event_date: None,
        near_duplicate_of: None,
    },
    // ---------------- personal ----------------
    PersonalMemory {
        id: "p-passport",
        kb: "personal",
        content: "My passport expires on 14 August 2027; renew it at least six months before any travel.",
        tags: &["admin", "passport"],
        importance: 8,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-coffee",
        kb: "personal",
        content: "I stop drinking coffee after three in the afternoon or I sleep badly.",
        tags: &["habit"],
        importance: 4,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-dentist",
        kb: "personal",
        content: "Dentist appointment with Dr Halloran; the cleaning takes ninety minutes.",
        tags: &["health", "appointment"],
        importance: 5,
        event_date: Some("2025-02-07"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-gym",
        kb: "personal",
        content: "I train at Fenwick Gym on Tuesday and Friday mornings before eight.",
        tags: &["habit", "fitness"],
        importance: 4,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-move-blob",
        kb: "personal",
        content: "We moved into the flat on Alder Row, the boiler was replaced a month later, the landlord is Ines Vaquero, and the lease runs until 2028.",
        tags: &["home", "blob"],
        importance: 7,
        event_date: Some("2025-01-22"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-anniversary",
        kb: "personal",
        content: "Our wedding anniversary is 9 September.",
        tags: &["anniversary"],
        importance: 9,
        event_date: None,
        near_duplicate_of: Some("p-anniversary-2"),
    },
    PersonalMemory {
        id: "p-anniversary-2",
        kb: "personal",
        content: "The anniversary dinner is always at the same restaurant in Trieste on the ninth of September.",
        tags: &["anniversary"],
        importance: 8,
        event_date: None,
        near_duplicate_of: Some("p-anniversary"),
    },
    PersonalMemory {
        id: "p-car",
        kb: "personal",
        content: "The car is due for its service at Brekke Motors.",
        tags: &["admin", "car"],
        importance: 4,
        event_date: Some("2025-05-16"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-allergy",
        kb: "personal",
        content: "I am mildly allergic to penicillin — the reaction is a rash, not anaphylaxis.",
        tags: &["health", "allergy"],
        importance: 9,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-book-club",
        kb: "personal",
        content: "Book club meets the first Monday of the month at Ines Vaquero's place.",
        tags: &["social"],
        importance: 4,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-visa-trip",
        kb: "personal",
        content: "Flew to Lisbon for the Trellis conference and stayed four nights.",
        tags: &["travel"],
        importance: 5,
        event_date: Some("2025-02-13"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "p-heating",
        kb: "personal",
        content: "The flat's heating runs on a timer: six to nine in the morning and five to eleven at night on weekdays.",
        tags: &["home"],
        importance: 3,
        event_date: None,
        near_duplicate_of: None,
    },
    // ---------------- business ----------------
    PersonalMemory {
        id: "b-halden-deal",
        kb: "business",
        content: "Decided to sign the Halden Freight pilot for six months rather than a full year, so we can exit if their volumes slip.",
        tags: &["decision", "halden"],
        importance: 8,
        event_date: Some("2025-03-05"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-pricing",
        kb: "business",
        content: "We moved from per-seat pricing to usage-based pricing after three customers churned on seat counts.",
        tags: &["decision", "pricing"],
        importance: 9,
        event_date: Some("2025-02-20"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-competitor",
        kb: "business",
        content: "Aurelis Systems undercuts us by roughly thirty percent but has no audit logging, which is why regulated buyers still choose us.",
        tags: &["competitor"],
        importance: 8,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-hire",
        kb: "business",
        content: "Hired Delphine Achebe as head of revenue; she starts on the second.",
        tags: &["hiring", "delphine"],
        importance: 8,
        event_date: Some("2025-05-12"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-investor-blob",
        kb: "business",
        content: "Kessler Ventures led the seed round at a twelve million post-money valuation, Ondine Kessler took the board seat, the round closed without a bridge, and they require monthly rather than quarterly reporting.",
        tags: &["investors", "blob"],
        importance: 9,
        event_date: Some("2025-01-28"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-churn",
        kb: "business",
        content: "Norvik Logistics churned over missing SSO; they said they would return once SAML shipped.",
        tags: &["churn", "norvik"],
        importance: 7,
        event_date: Some("2025-04-11"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-legal",
        kb: "business",
        content: "Our counsel is Vantanen & Fell; Kiro Vantanen is the partner who handles our contracts.",
        tags: &["legal", "kiro"],
        importance: 7,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-refund",
        kb: "business",
        content: "Policy decision: refunds above two thousand euros are approved by me personally, no exceptions.",
        tags: &["policy"],
        importance: 7,
        event_date: None,
        near_duplicate_of: Some("b-refund-2"),
    },
    PersonalMemory {
        id: "b-refund-2",
        kb: "business",
        content: "Anything over two thousand euros needs my personal sign-off before we refund it.",
        tags: &["policy"],
        importance: 6,
        event_date: None,
        near_duplicate_of: Some("b-refund"),
    },
    PersonalMemory {
        id: "b-conference",
        kb: "business",
        content: "The Trellis conference in Lisbon is where most of our regulated-industry leads come from; go every year.",
        tags: &["marketing"],
        importance: 6,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-lira-vendor",
        kb: "business",
        content: "Lira Marchetti is our design contractor and invoices on the last working day of each month.",
        tags: &["vendor", "lira"],
        importance: 6,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "b-bank",
        kb: "business",
        content: "We bank with Fenwick Union; the account manager changed twice and is now Delphine's contact.",
        tags: &["finance"],
        importance: 5,
        event_date: None,
        near_duplicate_of: None,
    },
    // ---------------- projects ----------------
    PersonalMemory {
        id: "j-petra-owner",
        kb: "projects",
        content: "Petra Lindqvist owns the Meridian rewrite and reports progress every second Friday.",
        tags: &["meridian", "petra"],
        importance: 7,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-meridian-decision",
        kb: "projects",
        content: "Decided to drop the offline sync feature from Meridian v2 because it doubled the QA surface.",
        tags: &["meridian", "decision"],
        importance: 8,
        event_date: Some("2025-04-24"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-saml",
        kb: "projects",
        content: "SAML SSO shipped, which unblocks the Norvik Logistics re-sell.",
        tags: &["meridian", "sso"],
        importance: 7,
        event_date: Some("2025-06-09"),
        near_duplicate_of: Some("j-saml-2"),
    },
    PersonalMemory {
        id: "j-saml-2",
        kb: "projects",
        content: "Single sign-on with SAML went live; Norvik can be re-approached now.",
        tags: &["meridian", "sso"],
        importance: 6,
        event_date: Some("2025-06-09"),
        near_duplicate_of: Some("j-saml"),
    },
    PersonalMemory {
        id: "j-stack",
        kb: "projects",
        content: "Meridian runs on Postgres 17 with a Rust ingestion worker; we rejected Kafka as too heavy for our volume.",
        tags: &["meridian", "architecture"],
        importance: 6,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-blob",
        kb: "projects",
        content: "The Meridian v2 launch is planned for October, Rafferty Nwosu is the design partner, the beta has eleven customers, and pricing stays usage-based.",
        tags: &["meridian", "blob"],
        importance: 8,
        event_date: Some("2025-06-30"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-incident",
        kb: "projects",
        content: "The outage was caused by an expired TLS certificate on the ingestion worker; we added expiry alerting afterwards.",
        tags: &["meridian", "incident"],
        importance: 8,
        event_date: Some("2025-03-27"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-naming",
        kb: "projects",
        content: "The internal codename for the mobile client is Tessera; never use it in customer-facing documents.",
        tags: &["naming"],
        importance: 5,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-onboarding",
        kb: "projects",
        content: "New engineers pair with Tomas Brekke for their first week on Meridian.",
        tags: &["meridian", "onboarding"],
        importance: 6,
        event_date: None,
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-backup",
        kb: "projects",
        content: "Backups run nightly to Frankfurt; the restore drill was run and passed.",
        tags: &["meridian", "ops"],
        importance: 6,
        event_date: Some("2025-02-25"),
        near_duplicate_of: None,
    },
    PersonalMemory {
        id: "j-docs",
        kb: "projects",
        content: "Meridian's runbook lives in the team wiki under Operations; it is reviewed each quarter.",
        tags: &["meridian", "docs"],
        importance: 4,
        event_date: None,
        near_duplicate_of: None,
    },
];

/// 43 queries with gold memory ids.
pub const QUERIES: &[PersonalQuery] = &[
    // ---------------- person fact lookup ----------------
    PersonalQuery {
        id: "pf-01",
        text: "How should I contact Rafferty Nwosu?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-rafferty-role"],
        window: None,
    },
    PersonalQuery {
        id: "pf-02",
        text: "Who introduced me to Rafferty Nwosu?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-ondine-intro"],
        window: None,
    },
    PersonalQuery {
        id: "pf-03",
        text: "What is Yusuf Halloran's birthday?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-yusuf-bday"],
        window: None,
    },
    PersonalQuery {
        id: "pf-04",
        text: "What gift should I avoid giving Lira Marchetti?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-lira-pref"],
        window: None,
    },
    PersonalQuery {
        id: "pf-05",
        text: "Which friend needs an EpiPen kept nearby?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-kiro-allergy"],
        window: None,
    },
    PersonalQuery {
        id: "pf-06",
        text: "Where does Ines Vaquero live?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-ines-loc"],
        window: None,
    },
    PersonalQuery {
        id: "pf-07",
        text: "What kind of books does Petra Lindqvist like?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-petra-pref"],
        window: None,
    },
    PersonalQuery {
        id: "pf-08",
        text: "Who is our legal counsel?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("business"),
        gold: &["b-legal"],
        window: None,
    },
    PersonalQuery {
        id: "pf-09",
        text: "Who did we hire to run revenue?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("business"),
        gold: &["b-hire"],
        window: None,
    },
    PersonalQuery {
        id: "pf-10",
        text: "Who owns the Meridian rewrite?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("projects"),
        gold: &["j-petra-owner"],
        window: None,
    },
    PersonalQuery {
        id: "pf-11",
        text: "When does my passport expire?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("personal"),
        gold: &["p-passport"],
        window: None,
    },
    PersonalQuery {
        id: "pf-12",
        text: "Who is Cass Aberdeen married to?",
        category: QueryCategory::PersonFact,
        kb_hint: Some("relationships"),
        gold: &["r-cass-anniv"],
        window: None,
    },
    // ---------------- temporal ----------------
    // With an explicit window: exercises the event_time filter path.
    PersonalQuery {
        id: "tm-01",
        text: "What did I have scheduled in February?",
        category: QueryCategory::Temporal,
        kb_hint: Some("personal"),
        gold: &["p-dentist", "p-visa-trip"],
        window: Some(("2025-02-01", "2025-02-28")),
    },
    PersonalQuery {
        id: "tm-02",
        text: "What did we change about the business in February?",
        category: QueryCategory::Temporal,
        kb_hint: Some("business"),
        gold: &["b-pricing"],
        window: Some(("2025-02-01", "2025-02-28")),
    },
    PersonalQuery {
        id: "tm-03",
        text: "What went wrong on the project in March?",
        category: QueryCategory::Temporal,
        kb_hint: Some("projects"),
        gold: &["j-incident"],
        window: Some(("2025-03-01", "2025-03-31")),
    },
    PersonalQuery {
        id: "tm-04",
        text: "Which customer did we lose in April?",
        category: QueryCategory::Temporal,
        kb_hint: Some("business"),
        gold: &["b-churn"],
        window: Some(("2025-04-01", "2025-04-30")),
    },
    // No window: the month is never written in the text, so only event-time
    // aware retrieval can answer these. Expected to fail on today's defaults.
    PersonalQuery {
        id: "tm-05",
        text: "What happened with people in February?",
        category: QueryCategory::Temporal,
        kb_hint: Some("relationships"),
        gold: &["r-ondine-intro", "r-ondine-loc"],
        window: None,
    },
    PersonalQuery {
        id: "tm-06",
        text: "What personnel changes happened in May?",
        category: QueryCategory::Temporal,
        kb_hint: Some("business"),
        gold: &["b-hire"],
        window: None,
    },
    PersonalQuery {
        id: "tm-07",
        text: "What did we decide about Meridian last spring?",
        category: QueryCategory::Temporal,
        kb_hint: Some("projects"),
        gold: &["j-meridian-decision"],
        window: None,
    },
    PersonalQuery {
        id: "tm-08",
        text: "What happened with the company in January?",
        category: QueryCategory::Temporal,
        kb_hint: Some("business"),
        gold: &["b-investor-blob"],
        window: None,
    },
    // ---------------- cross-KB (no hint) ----------------
    PersonalQuery {
        id: "ck-01",
        text: "When does Lira Marchetti invoice us?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["b-lira-vendor"],
        window: None,
    },
    PersonalQuery {
        id: "ck-02",
        text: "Who is my landlord?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["p-move-blob"],
        window: None,
    },
    PersonalQuery {
        id: "ck-03",
        text: "What does Kiro Vantanen do for us?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["b-legal"],
        window: None,
    },
    PersonalQuery {
        id: "ck-04",
        text: "Where is the Trellis conference held?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["b-conference", "r-ondine-intro", "p-visa-trip"],
        window: None,
    },
    PersonalQuery {
        id: "ck-05",
        text: "What do I know about Delphine Achebe?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["b-hire", "r-delphine-intro"],
        window: None,
    },
    PersonalQuery {
        id: "ck-06",
        text: "Who should new engineers pair with?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["j-onboarding"],
        window: None,
    },
    PersonalQuery {
        id: "ck-07",
        text: "What is Milena Toft allergic to?",
        category: QueryCategory::CrossKb,
        kb_hint: None,
        gold: &["r-milena-blob"],
        window: None,
    },
    // ---------------- paraphrase near-duplicates ----------------
    PersonalQuery {
        id: "pd-01",
        text: "When is Lira's birthday?",
        category: QueryCategory::ParaphraseDedup,
        kb_hint: Some("relationships"),
        gold: &["r-lira-bday-1", "r-lira-bday-2"],
        window: None,
    },
    PersonalQuery {
        id: "pd-02",
        text: "When was Ondine Kessler born?",
        category: QueryCategory::ParaphraseDedup,
        kb_hint: Some("relationships"),
        gold: &["r-ondine-bday-1", "r-ondine-bday-2"],
        window: None,
    },
    PersonalQuery {
        id: "pd-03",
        text: "Who approves large refunds?",
        category: QueryCategory::ParaphraseDedup,
        kb_hint: Some("business"),
        gold: &["b-refund", "b-refund-2"],
        window: None,
    },
    PersonalQuery {
        id: "pd-04",
        text: "Did SAML single sign-on ship?",
        category: QueryCategory::ParaphraseDedup,
        kb_hint: Some("projects"),
        gold: &["j-saml", "j-saml-2"],
        window: None,
    },
    PersonalQuery {
        id: "pd-05",
        text: "When is our wedding anniversary?",
        category: QueryCategory::ParaphraseDedup,
        kb_hint: Some("personal"),
        gold: &["p-anniversary", "p-anniversary-2"],
        window: None,
    },
    // ---------------- multi-fact blobs ----------------
    PersonalQuery {
        id: "mf-01",
        text: "How often do our investors want reporting?",
        category: QueryCategory::MultiFact,
        kb_hint: Some("business"),
        gold: &["b-investor-blob"],
        window: None,
    },
    PersonalQuery {
        id: "mf-02",
        text: "How many customers are in the beta?",
        category: QueryCategory::MultiFact,
        kb_hint: Some("projects"),
        gold: &["j-blob"],
        window: None,
    },
    PersonalQuery {
        id: "mf-03",
        text: "Was the boiler ever replaced?",
        category: QueryCategory::MultiFact,
        kb_hint: Some("personal"),
        gold: &["p-move-blob"],
        window: None,
    },
    PersonalQuery {
        id: "mf-04",
        text: "Does Milena drink caffeine in the afternoon?",
        category: QueryCategory::MultiFact,
        kb_hint: Some("relationships"),
        gold: &["r-milena-blob"],
        window: None,
    },
    PersonalQuery {
        id: "mf-05",
        text: "Who is the design partner for Meridian v2?",
        category: QueryCategory::MultiFact,
        kb_hint: Some("projects"),
        gold: &["j-blob"],
        window: None,
    },
    // ---------------- negative controls ----------------
    PersonalQuery {
        id: "nc-01",
        text: "When is Marisol Ferrand's birthday?",
        category: QueryCategory::NegativeControl,
        kb_hint: Some("relationships"),
        gold: &[],
        window: None,
    },
    PersonalQuery {
        id: "nc-02",
        text: "What is Rafferty Nwosu's home address?",
        category: QueryCategory::NegativeControl,
        kb_hint: Some("relationships"),
        gold: &[],
        window: None,
    },
    PersonalQuery {
        id: "nc-03",
        text: "How much do we pay for the Zurich office lease?",
        category: QueryCategory::NegativeControl,
        kb_hint: Some("business"),
        gold: &[],
        window: None,
    },
    PersonalQuery {
        id: "nc-04",
        text: "What is my blood type?",
        category: QueryCategory::NegativeControl,
        kb_hint: Some("personal"),
        gold: &[],
        window: None,
    },
    PersonalQuery {
        id: "nc-05",
        text: "Which cloud provider hosts the Tessera mobile backend?",
        category: QueryCategory::NegativeControl,
        kb_hint: Some("projects"),
        gold: &[],
        window: None,
    },
    PersonalQuery {
        id: "nc-06",
        text: "When does the Halden Freight contract auto-renew?",
        category: QueryCategory::NegativeControl,
        kb_hint: Some("business"),
        gold: &[],
        window: None,
    },
];

// ---------------------------------------------------------------------------
// Dataset integrity
// ---------------------------------------------------------------------------

/// Verify the committed dataset is self-consistent. Called by the runner
/// before seeding so a typo in a gold id fails loudly instead of silently
/// scoring zero.
pub fn validate_dataset() -> Result<(), BenchError> {
    let bad = |msg: String| BenchError::InvalidDataset(msg);

    let kb_names: HashSet<&str> = KBS.iter().map(|(n, _)| *n).collect();
    let mut ids: HashSet<&str> = HashSet::new();
    for m in MEMORIES {
        if !ids.insert(m.id) {
            return Err(bad(format!("duplicate memory id {}", m.id)));
        }
        if !kb_names.contains(m.kb) {
            return Err(bad(format!("memory {} uses unknown kb {}", m.id, m.kb)));
        }
        if m.content.trim().is_empty() {
            return Err(bad(format!("memory {} has empty content", m.id)));
        }
        if m.importance > 10 {
            return Err(bad(format!("memory {} importance out of range", m.id)));
        }
        if let Some(d) = m.event_date {
            parse_date(d)?;
        }
    }
    if !(40..=60).contains(&MEMORIES.len()) {
        return Err(bad(format!(
            "dataset should hold 40-60 memories, has {}",
            MEMORIES.len()
        )));
    }
    // Near-duplicate declarations must be symmetric and same-KB, or the
    // paraphrase hazard is not actually testing what it claims to.
    for m in MEMORIES {
        let Some(other_id) = m.near_duplicate_of else {
            continue;
        };
        let Some(other) = MEMORIES.iter().find(|o| o.id == other_id) else {
            return Err(bad(format!(
                "memory {} points at unknown near-duplicate {other_id}",
                m.id
            )));
        };
        if other.near_duplicate_of != Some(m.id) {
            return Err(bad(format!(
                "near-duplicate link {} -> {other_id} is not symmetric",
                m.id
            )));
        }
        if other.kb != m.kb {
            return Err(bad(format!(
                "near-duplicate pair {} / {other_id} spans KBs",
                m.id
            )));
        }
    }

    let mut query_ids: HashSet<&str> = HashSet::new();
    let mut seen_categories: HashSet<&str> = HashSet::new();
    for q in QUERIES {
        if !query_ids.insert(q.id) {
            return Err(bad(format!("duplicate query id {}", q.id)));
        }
        if q.text.trim().is_empty() {
            return Err(bad(format!("query {} has empty text", q.id)));
        }
        seen_categories.insert(q.category.as_str());
        if let Some(kb) = q.kb_hint {
            if !kb_names.contains(kb) {
                return Err(bad(format!("query {} hints unknown kb {kb}", q.id)));
            }
        }
        for g in q.gold {
            if !ids.contains(g) {
                return Err(bad(format!("query {} references unknown memory {g}", q.id)));
            }
        }
        match q.category {
            QueryCategory::NegativeControl => {
                if !q.gold.is_empty() {
                    return Err(bad(format!(
                        "negative control {} must have no gold memories",
                        q.id
                    )));
                }
            }
            _ => {
                if q.gold.is_empty() {
                    return Err(bad(format!("query {} has no gold memories", q.id)));
                }
            }
        }
        if let Some((from, to)) = q.window {
            if q.category != QueryCategory::Temporal {
                return Err(bad(format!(
                    "query {} has an event-time window but is not temporal",
                    q.id
                )));
            }
            let (from, to) = (parse_date(from)?, parse_date(to)?);
            if from > to {
                return Err(bad(format!("query {} has an inverted window", q.id)));
            }
            // A windowed query is only meaningful if its gold memories carry
            // event dates inside the window.
            for g in q.gold {
                let m = MEMORIES.iter().find(|m| &m.id == g).expect("checked above");
                let Some(d) = m.event_date else {
                    return Err(bad(format!(
                        "query {} windows on {g}, which has no event date",
                        q.id
                    )));
                };
                let d = parse_date(d)?;
                if d < from || d > to {
                    return Err(bad(format!(
                        "query {} gold {g} (event {d}) falls outside its window",
                        q.id
                    )));
                }
            }
        }
        if q.category == QueryCategory::CrossKb && q.kb_hint.is_some() {
            return Err(bad(format!(
                "cross-KB query {} must not carry a kb hint",
                q.id
            )));
        }
    }
    for c in QueryCategory::ALL {
        if !seen_categories.contains(c.as_str()) {
            return Err(bad(format!("no queries in category {}", c.as_str())));
        }
    }
    if !(30..=50).contains(&QUERIES.len()) {
        return Err(bad(format!(
            "query set should hold 30-50 queries, has {}",
            QUERIES.len()
        )));
    }
    Ok(())
}

fn parse_date(s: &str) -> Result<DateTime<Utc>, BenchError> {
    let naive = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| BenchError::InvalidDataset(format!("bad date {s}: {e}")))?;
    // Midday UTC so a date never lands on the wrong side of a window boundary
    // because of a timezone offset.
    let dt = naive
        .and_hms_opt(12, 0, 0)
        .ok_or_else(|| BenchError::InvalidDataset(format!("bad date {s}")))?;
    Ok(Utc.from_utc_datetime(&dt))
}

/// Inclusive event-time bounds for a query window, as RFC3339-ready datetimes.
/// The lower bound is the start of the first day, the upper the end of the last.
pub fn window_bounds(window: (&str, &str)) -> Result<(DateTime<Utc>, DateTime<Utc>), BenchError> {
    // Dataset event times sit at midday, so widen to the day boundaries.
    let from = parse_date(window.0)? - chrono::Duration::hours(12);
    let to = parse_date(window.1)? + chrono::Duration::hours(12) - chrono::Duration::seconds(1);
    Ok((from, to))
}

// ---------------------------------------------------------------------------
// Seeding (real write path)
// ---------------------------------------------------------------------------

/// What `seed_store` hands back: the id maps a retrieval hit needs to be
/// scored against the dataset.
#[derive(Debug, Clone, Default)]
pub struct SeededDataset {
    /// dataset memory id -> stored memory uuid.
    pub memory_ids: HashMap<String, Uuid>,
    /// stored chunk uuid -> dataset memory id.
    pub chunk_to_memory: HashMap<Uuid, String>,
    pub chunks_written: usize,
}

impl SeededDataset {
    /// Map a retrieval result id back to a dataset memory id. Returns `None`
    /// for anything that is not a seeded chunk (entity/claim/wiki candidates),
    /// which then counts as a non-gold slot in the ranking.
    pub fn dataset_id_for(&self, retrieved_id: &Uuid) -> Option<&str> {
        self.chunk_to_memory.get(retrieved_id).map(|s| s.as_str())
    }
}

/// Deterministic uuid so repeated runs produce identical ids (no random tie
/// breaks between runs, same trick the mini bench uses).
fn stable_id(prefix: &str, key: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("{prefix}:{key}").as_bytes())
}

/// Seed the dataset into `store` through the same steps `engram remember`
/// performs: ensure the KB exists, insert the memory row, split with the
/// production chunker, embed each chunk, and write chunks with full embedding
/// metadata so `hybrid_recall`'s compatibility guard is satisfied.
///
/// Unlike `remember`, event times are written from the dataset — that is the
/// ground truth the temporal queries are scored against.
pub async fn seed_store<E: Embedder + ?Sized>(
    store: &SqliteStore,
    embedder: &E,
) -> Result<SeededDataset, BenchError> {
    use engram_core::types::{Memory, MemorySource};

    validate_dataset()?;

    let created_at = DateTime::parse_from_rfc3339(INGESTED_AT)
        .map_err(|e| BenchError::InvalidDataset(format!("bad INGESTED_AT: {e}")))?
        .with_timezone(&Utc);

    for (name, description) in KBS {
        store.ensure_kb(name, Some(description))?;
    }

    let mut seeded = SeededDataset::default();
    let model = embedder.model();
    let dims = embedder.dimensions();
    let prompt_format = embedder.prompt_format().to_string();

    for m in MEMORIES {
        let event_time = match m.event_date {
            Some(d) => Some(parse_date(d)?),
            None => None,
        };
        let memory_id = stable_id("personal-mem", m.id);
        let memory = Memory {
            id: memory_id,
            content: m.content.to_string(),
            created_at,
            event_time,
            importance: m.importance,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Manual,
            diary: DIARY.to_string(),
            valid_from: Some(created_at),
            valid_until: None,
            tags: m.tags.iter().map(|t| (*t).to_string()).collect(),
        };
        store.insert_memory_with_kb(&memory, m.kb)?;
        seeded.memory_ids.insert(m.id.to_string(), memory_id);

        let chunks = naive_split(m.content);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let embeddings = embedder
            .embed_batch(&texts, TaskMode::RetrievalDocument)
            .await?;
        for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
            let chunk_id = stable_id("personal-chunk", &format!("{}#{}", m.id, chunk.position));
            store.insert_chunk_with_embedding_meta(
                chunk_id,
                memory_id,
                &chunk.text,
                chunk.position,
                chunk.section.as_deref(),
                embedding,
                &model,
                dims,
                &prompt_format,
                None,
            )?;
            seeded.chunk_to_memory.insert(chunk_id, m.id.to_string());
            seeded.chunks_written += 1;
        }
    }

    Ok(seeded)
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Cut-offs the suite reports recall at.
pub const K_VALUES: [usize; 4] = [1, 3, 5, 10];

/// One retrieved item, already mapped back to the dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedItem {
    /// Dataset memory id, or `None` when the hit is not a seeded chunk.
    pub dataset_id: Option<String>,
    pub kind: String,
    pub score: f32,
}

/// Collapse a chunk-level ranking to a memory-level one: a memory that
/// contributed several chunks occupies only its best rank, but non-dataset
/// hits keep their slot because they really do push answers down the list.
pub fn collapse_to_memories(retrieved: &[RetrievedItem]) -> Vec<Option<String>> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::with_capacity(retrieved.len());
    for item in retrieved {
        match &item.dataset_id {
            Some(id) => {
                if seen.insert(id.as_str()) {
                    out.push(Some(id.clone()));
                }
            }
            None => out.push(None),
        }
    }
    out
}

/// Score of a single ranking against a gold set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreRow {
    pub recall_at_1: f32,
    pub recall_at_3: f32,
    pub recall_at_5: f32,
    pub recall_at_10: f32,
    /// Reciprocal rank of the first gold memory within the top 10.
    pub mrr_at_10: f32,
    /// Fraction of the gold set present in the top 10 — the metric that
    /// matters when a query has several right answers (paraphrase pairs,
    /// facts scattered across KBs).
    pub gold_coverage_at_10: f32,
    /// 1-based rank of the first gold memory, if any.
    pub first_gold_rank: Option<usize>,
}

/// Hit-rate recall@k: 1.0 when any gold id is inside the top k.
pub fn hit_at_k(ranking: &[Option<String>], gold: &[&str], k: usize) -> f32 {
    let hit = ranking
        .iter()
        .take(k)
        .any(|slot| slot.as_deref().is_some_and(|id| gold.contains(&id)));
    if hit {
        1.0
    } else {
        0.0
    }
}

/// Fraction of the gold set found inside the top k.
pub fn coverage_at_k(ranking: &[Option<String>], gold: &[&str], k: usize) -> f32 {
    if gold.is_empty() {
        return 0.0;
    }
    let top: HashSet<&str> = ranking
        .iter()
        .take(k)
        .filter_map(|slot| slot.as_deref())
        .collect();
    let found = gold.iter().filter(|g| top.contains(*g)).count();
    found as f32 / gold.len() as f32
}

/// Reciprocal rank of the first gold memory within the top k.
pub fn reciprocal_rank_at_k(ranking: &[Option<String>], gold: &[&str], k: usize) -> f32 {
    match first_gold_rank(ranking, gold, k) {
        Some(rank) => 1.0 / rank as f32,
        None => 0.0,
    }
}

/// 1-based rank of the first gold memory within the top k.
pub fn first_gold_rank(ranking: &[Option<String>], gold: &[&str], k: usize) -> Option<usize> {
    ranking
        .iter()
        .take(k)
        .position(|slot| slot.as_deref().is_some_and(|id| gold.contains(&id)))
        .map(|i| i + 1)
}

/// Score one ranking. `gold` empty (negative control) yields an all-zero row —
/// negative controls are scored by abstention, not recall.
pub fn score_ranking(ranking: &[Option<String>], gold: &[&str]) -> ScoreRow {
    if gold.is_empty() {
        return ScoreRow::default();
    }
    ScoreRow {
        recall_at_1: hit_at_k(ranking, gold, 1),
        recall_at_3: hit_at_k(ranking, gold, 3),
        recall_at_5: hit_at_k(ranking, gold, 5),
        recall_at_10: hit_at_k(ranking, gold, 10),
        mrr_at_10: reciprocal_rank_at_k(ranking, gold, 10),
        gold_coverage_at_10: coverage_at_k(ranking, gold, 10),
        first_gold_rank: first_gold_rank(ranking, gold, 10),
    }
}

/// A single executed query, produced by the runner and fed to [`aggregate`].
#[derive(Debug, Clone)]
pub struct QueryRun {
    pub query_id: &'static str,
    pub category: QueryCategory,
    pub text: &'static str,
    pub kb_filter: Option<String>,
    pub gold: &'static [&'static str],
    pub retrieved: Vec<RetrievedItem>,
    /// Second run of the same query pinned to the legacy `kb=default` floor.
    /// Only set for hint-less queries, where the headline run takes the
    /// production all-KB default; this row is the regression tripwire showing
    /// what the old single-KB default would have scored.
    pub retrieved_legacy_default: Option<Vec<RetrievedItem>>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub query_id: String,
    pub category: String,
    pub query: String,
    pub kb_filter: Option<String>,
    pub gold: Vec<String>,
    #[serde(flatten)]
    pub score: ScoreRow,
    /// Same query pinned to the legacy `kb=default` floor, when it was run.
    pub legacy_default: Option<ScoreRow>,
    pub results_returned: usize,
    /// True for a negative control that returned nothing at all.
    pub abstained: bool,
    pub top_score: Option<f32>,
    pub top_retrieved: Vec<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CategoryScore {
    pub category: String,
    pub queries: usize,
    pub recall_at_1: f32,
    pub recall_at_3: f32,
    pub recall_at_5: f32,
    pub recall_at_10: f32,
    pub mrr_at_10: f32,
    pub gold_coverage_at_10: f32,
    /// Negative controls only: fraction that returned nothing.
    pub abstention_rate: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonalMetrics {
    /// Averages over every scored (non negative-control) query.
    pub recall_at_1: f32,
    pub recall_at_3: f32,
    pub recall_at_5: f32,
    pub recall_at_10: f32,
    pub mrr_at_10: f32,
    pub gold_coverage_at_10: f32,
    pub queries_scored: usize,
    pub negative_controls: usize,
    /// Fraction of negative controls that returned no results at all.
    pub abstention_rate: f32,
    /// Mean top score on negative controls — a calibration signal: a store that
    /// cannot abstain should at least score its wrong answers lower.
    pub negative_mean_top_score: f32,
    pub mean_latency_ms: f32,
    pub p50_latency_ms: f32,
    pub p95_latency_ms: f32,
}

/// Recall of the hint-less queries when pinned to the legacy `kb=default`
/// floor. The headline metrics measure production behaviour (recall spans
/// every KB); this is the regression tripwire — if it ever catches up with the
/// headline, the all-KB default has stopped paying for itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LegacyDefaultComparison {
    pub queries: usize,
    pub recall_at_1: f32,
    pub recall_at_5: f32,
    pub recall_at_10: f32,
    pub mrr_at_10: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalReport {
    pub suite: String,
    pub profile: String,
    pub embedder: String,
    pub reranker: String,
    pub top_k: usize,
    pub memories_seeded: usize,
    pub chunks_seeded: usize,
    pub kbs: Vec<String>,
    pub queries_evaluated: usize,
    pub metrics: PersonalMetrics,
    pub per_category: Vec<CategoryScore>,
    pub legacy_default_comparison: Option<LegacyDefaultComparison>,
    pub per_query: Vec<QueryResult>,
    pub notes: Vec<String>,
}

/// Turn executed runs into the report. Pure function — no IO, no clock.
pub fn aggregate(
    runs: &[QueryRun],
    suite: &str,
    profile: &str,
    embedder: &str,
    reranker: &str,
    top_k: usize,
    seeded: &SeededDataset,
) -> PersonalReport {
    let mut per_query: Vec<QueryResult> = Vec::with_capacity(runs.len());
    for run in runs {
        let ranking = collapse_to_memories(&run.retrieved);
        let score = score_ranking(&ranking, run.gold);
        let legacy_default = run.retrieved_legacy_default.as_ref().map(|items| {
            let r = collapse_to_memories(items);
            score_ranking(&r, run.gold)
        });
        let top_score = run.retrieved.first().map(|r| r.score);
        per_query.push(QueryResult {
            query_id: run.query_id.to_string(),
            category: run.category.as_str().to_string(),
            query: run.text.to_string(),
            kb_filter: run.kb_filter.clone(),
            gold: run.gold.iter().map(|g| (*g).to_string()).collect(),
            score,
            legacy_default,
            results_returned: run.retrieved.len(),
            abstained: run.retrieved.is_empty(),
            top_score,
            top_retrieved: ranking
                .iter()
                .take(5)
                .map(|slot| slot.clone().unwrap_or_else(|| "-".to_string()))
                .collect(),
            latency_ms: run.latency_ms,
        });
    }

    let scored: Vec<&QueryResult> = per_query
        .iter()
        .filter(|q| q.category != QueryCategory::NegativeControl.as_str())
        .collect();
    let negatives: Vec<&QueryResult> = per_query
        .iter()
        .filter(|q| q.category == QueryCategory::NegativeControl.as_str())
        .collect();

    let metrics = PersonalMetrics {
        recall_at_1: mean(scored.iter().map(|q| q.score.recall_at_1)),
        recall_at_3: mean(scored.iter().map(|q| q.score.recall_at_3)),
        recall_at_5: mean(scored.iter().map(|q| q.score.recall_at_5)),
        recall_at_10: mean(scored.iter().map(|q| q.score.recall_at_10)),
        mrr_at_10: mean(scored.iter().map(|q| q.score.mrr_at_10)),
        gold_coverage_at_10: mean(scored.iter().map(|q| q.score.gold_coverage_at_10)),
        queries_scored: scored.len(),
        negative_controls: negatives.len(),
        abstention_rate: mean(
            negatives
                .iter()
                .map(|q| if q.abstained { 1.0 } else { 0.0 }),
        ),
        negative_mean_top_score: mean(negatives.iter().map(|q| q.top_score.unwrap_or(0.0))),
        mean_latency_ms: mean(per_query.iter().map(|q| q.latency_ms as f32)),
        p50_latency_ms: percentile(&per_query, 50),
        p95_latency_ms: percentile(&per_query, 95),
    };

    let mut per_category = Vec::new();
    for c in QueryCategory::ALL {
        let rows: Vec<&QueryResult> = per_query
            .iter()
            .filter(|q| q.category == c.as_str())
            .collect();
        if rows.is_empty() {
            continue;
        }
        let negative = *c == QueryCategory::NegativeControl;
        per_category.push(CategoryScore {
            category: c.as_str().to_string(),
            queries: rows.len(),
            recall_at_1: mean(rows.iter().map(|q| q.score.recall_at_1)),
            recall_at_3: mean(rows.iter().map(|q| q.score.recall_at_3)),
            recall_at_5: mean(rows.iter().map(|q| q.score.recall_at_5)),
            recall_at_10: mean(rows.iter().map(|q| q.score.recall_at_10)),
            mrr_at_10: mean(rows.iter().map(|q| q.score.mrr_at_10)),
            gold_coverage_at_10: mean(rows.iter().map(|q| q.score.gold_coverage_at_10)),
            abstention_rate: negative
                .then(|| mean(rows.iter().map(|q| if q.abstained { 1.0 } else { 0.0 }))),
        });
    }

    let with_legacy_default: Vec<&ScoreRow> = per_query
        .iter()
        .filter_map(|q| q.legacy_default.as_ref())
        .collect();
    let legacy_default_comparison =
        (!with_legacy_default.is_empty()).then(|| LegacyDefaultComparison {
            queries: with_legacy_default.len(),
            recall_at_1: mean(with_legacy_default.iter().map(|s| s.recall_at_1)),
            recall_at_5: mean(with_legacy_default.iter().map(|s| s.recall_at_5)),
            recall_at_10: mean(with_legacy_default.iter().map(|s| s.recall_at_10)),
            mrr_at_10: mean(with_legacy_default.iter().map(|s| s.mrr_at_10)),
        });

    let notes = vec![
        "Synthetic personal-memory suite; all people, companies and dates are fictional."
            .to_string(),
        format!(
            "Headline metrics are production behaviour: hint-less queries take the `recall` \
             default, which spans every KB. `legacy_default_comparison` reruns them pinned to \
             kb={DEFAULT_KB} — the old default, kept as a regression tripwire."
        ),
        "Negative controls are scored by abstention, not recall: they have no gold memories."
            .to_string(),
        "Temporal queries without a window can only be answered from event_time.".to_string(),
    ];

    PersonalReport {
        suite: suite.to_string(),
        profile: profile.to_string(),
        embedder: embedder.to_string(),
        reranker: reranker.to_string(),
        top_k,
        memories_seeded: seeded.memory_ids.len(),
        chunks_seeded: seeded.chunks_written,
        kbs: KBS.iter().map(|(n, _)| (*n).to_string()).collect(),
        queries_evaluated: per_query.len(),
        metrics,
        per_category,
        legacy_default_comparison,
        per_query,
        notes,
    }
}

fn mean(values: impl Iterator<Item = f32>) -> f32 {
    let mut n = 0usize;
    let mut sum = 0f32;
    for v in values {
        sum += v;
        n += 1;
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

fn percentile(rows: &[QueryResult], pct: usize) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    let mut lat: Vec<u64> = rows.iter().map(|q| q.latency_ms).collect();
    lat.sort_unstable();
    let idx = (lat.len() * pct / 100).min(lat.len() - 1);
    lat[idx] as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_embed::stub::StubEmbedder;

    fn ranking(ids: &[&str]) -> Vec<Option<String>> {
        ids.iter()
            .map(|id| {
                if *id == "-" {
                    None
                } else {
                    Some((*id).to_string())
                }
            })
            .collect()
    }

    #[test]
    fn dataset_is_valid() {
        validate_dataset().expect("committed dataset is self-consistent");
    }

    #[test]
    fn every_category_is_populated() {
        for c in QueryCategory::ALL {
            assert!(
                QUERIES.iter().any(|q| q.category == *c),
                "category {} has no queries",
                c.as_str()
            );
        }
    }

    #[test]
    fn dataset_covers_every_kb_and_each_kb_is_queried() {
        for (kb, _) in KBS {
            assert!(
                MEMORIES.iter().any(|m| m.kb == *kb),
                "kb {kb} has no memories"
            );
            let gold_in_kb = QUERIES.iter().any(|q| {
                q.gold
                    .iter()
                    .any(|g| MEMORIES.iter().any(|m| &m.id == g && m.kb == *kb))
            });
            assert!(gold_in_kb, "no query has gold in kb {kb}");
        }
    }

    #[test]
    fn hazards_are_present() {
        // Paraphrase pairs.
        assert!(
            MEMORIES
                .iter()
                .filter(|m| m.near_duplicate_of.is_some())
                .count()
                >= 8,
            "expected at least four paraphrase pairs"
        );
        // Multi-fact blobs: long single-paragraph memories.
        assert!(
            MEMORIES.iter().filter(|m| m.content.len() > 140).count() >= 3,
            "expected multi-fact paragraph blobs"
        );
        // Cross-KB placement: a person fact filed under business.
        assert!(MEMORIES
            .iter()
            .any(|m| m.id == "b-lira-vendor" && m.kb == "business"));
        // Temporal facts that never name their month.
        let m = MEMORIES.iter().find(|m| m.id == "b-hire").unwrap();
        assert!(m.event_date.is_some() && !m.content.contains("May"));
    }

    #[test]
    fn hint_less_queries_exist_and_target_non_default_kbs() {
        let hintless: Vec<&PersonalQuery> =
            QUERIES.iter().filter(|q| q.kb_hint.is_none()).collect();
        assert!(hintless.len() >= 5);
        for q in hintless {
            for g in q.gold {
                let m = MEMORIES.iter().find(|m| &m.id == g).unwrap();
                assert_ne!(m.kb, DEFAULT_KB, "gold {g} sits in the default KB");
            }
        }
    }

    #[test]
    fn window_bounds_bracket_the_whole_day_range() {
        let (from, to) = window_bounds(("2025-02-01", "2025-02-28")).unwrap();
        let inside = parse_date("2025-02-07").unwrap();
        assert!(from < inside && inside < to);
        let before = parse_date("2025-01-31").unwrap();
        let after = parse_date("2025-03-01").unwrap();
        assert!(before < from, "31 Jan must fall outside a February window");
        assert!(after > to, "1 Mar must fall outside a February window");
    }

    #[test]
    fn hit_at_k_respects_the_cutoff() {
        let r = ranking(&["a", "b", "gold"]);
        assert_eq!(hit_at_k(&r, &["gold"], 3), 1.0);
        assert_eq!(hit_at_k(&r, &["gold"], 2), 0.0);
        assert_eq!(hit_at_k(&r, &[], 3), 0.0);
    }

    #[test]
    fn unmapped_hits_occupy_a_rank() {
        // A non-dataset candidate at rank 1 pushes gold to rank 2.
        let r = ranking(&["-", "gold"]);
        assert_eq!(hit_at_k(&r, &["gold"], 1), 0.0);
        assert_eq!(first_gold_rank(&r, &["gold"], 10), Some(2));
        assert!((reciprocal_rank_at_k(&r, &["gold"], 10) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn coverage_counts_the_share_of_gold_found() {
        let r = ranking(&["g1", "x", "y"]);
        assert!((coverage_at_k(&r, &["g1", "g2"], 10) - 0.5).abs() < 1e-6);
        assert_eq!(coverage_at_k(&r, &["g1"], 10), 1.0);
        assert_eq!(coverage_at_k(&r, &[], 10), 0.0);
    }

    #[test]
    fn collapse_keeps_best_rank_per_memory_but_keeps_foreign_slots() {
        let items = vec![
            RetrievedItem {
                dataset_id: Some("m1".into()),
                kind: "chunk".into(),
                score: 1.0,
            },
            RetrievedItem {
                dataset_id: Some("m1".into()),
                kind: "chunk".into(),
                score: 0.9,
            },
            RetrievedItem {
                dataset_id: None,
                kind: "entity".into(),
                score: 0.8,
            },
            RetrievedItem {
                dataset_id: Some("m2".into()),
                kind: "chunk".into(),
                score: 0.7,
            },
        ];
        let collapsed = collapse_to_memories(&items);
        assert_eq!(
            collapsed,
            vec![Some("m1".to_string()), None, Some("m2".to_string())]
        );
    }

    #[test]
    fn score_ranking_is_all_zero_for_negative_controls() {
        let r = ranking(&["a", "b"]);
        let s = score_ranking(&r, &[]);
        assert_eq!(s.recall_at_10, 0.0);
        assert_eq!(s.mrr_at_10, 0.0);
        assert!(s.first_gold_rank.is_none());
    }

    #[test]
    fn score_ranking_hand_worked() {
        // gold at rank 3 of 4; one of two gold memories retrieved.
        let r = ranking(&["x", "y", "g1", "z"]);
        let s = score_ranking(&r, &["g1", "g2"]);
        assert_eq!(s.recall_at_1, 0.0);
        assert_eq!(s.recall_at_3, 1.0);
        assert_eq!(s.recall_at_5, 1.0);
        assert!((s.mrr_at_10 - 1.0 / 3.0).abs() < 1e-6);
        assert!((s.gold_coverage_at_10 - 0.5).abs() < 1e-6);
        assert_eq!(s.first_gold_rank, Some(3));
    }

    fn run(
        id: &'static str,
        category: QueryCategory,
        gold: &'static [&'static str],
        hits: &[&str],
    ) -> QueryRun {
        QueryRun {
            query_id: id,
            category,
            text: "q",
            kb_filter: None,
            gold,
            retrieved: hits
                .iter()
                .enumerate()
                .map(|(i, h)| RetrievedItem {
                    dataset_id: (*h != "-").then(|| (*h).to_string()),
                    kind: "chunk".into(),
                    score: 1.0 - i as f32 / 10.0,
                })
                .collect(),
            retrieved_legacy_default: None,
            latency_ms: 5,
        }
    }

    #[test]
    fn aggregate_splits_scored_queries_from_negative_controls() {
        let runs = vec![
            run("a", QueryCategory::PersonFact, &["g1"], &["g1"]),
            run("b", QueryCategory::PersonFact, &["g2"], &["x", "g2"]),
            run("c", QueryCategory::NegativeControl, &[], &[]),
            run("d", QueryCategory::NegativeControl, &[], &["x"]),
        ];
        let report = aggregate(
            &runs,
            "personal",
            "offline",
            "stub",
            "passthrough",
            10,
            &SeededDataset::default(),
        );
        assert_eq!(report.queries_evaluated, 4);
        assert_eq!(report.metrics.queries_scored, 2);
        assert_eq!(report.metrics.negative_controls, 2);
        // recall@1: 1.0 and 0.0 -> 0.5 (negative controls excluded).
        assert!((report.metrics.recall_at_1 - 0.5).abs() < 1e-6);
        // MRR: 1.0 and 0.5 -> 0.75.
        assert!((report.metrics.mrr_at_10 - 0.75).abs() < 1e-6);
        // One of two negative controls returned nothing.
        assert!((report.metrics.abstention_rate - 0.5).abs() < 1e-6);
        let neg = report
            .per_category
            .iter()
            .find(|c| c.category == "negative_control")
            .unwrap();
        assert_eq!(neg.abstention_rate, Some(0.5));
        let pf = report
            .per_category
            .iter()
            .find(|c| c.category == "person_fact")
            .unwrap();
        assert_eq!(pf.queries, 2);
        assert!(report.legacy_default_comparison.is_none());
    }

    /// The headline row is production behaviour (all KBs); the comparison row
    /// is the legacy `kb=default` floor, which is expected to score WORSE.
    #[test]
    fn aggregate_reports_the_legacy_default_floor() {
        let mut r = run("ck", QueryCategory::CrossKb, &["g1"], &["g1"]);
        // The old single-KB default could not see the gold memory at all.
        r.retrieved_legacy_default = Some(vec![RetrievedItem {
            dataset_id: Some("x".into()),
            kind: "chunk".into(),
            score: 1.0,
        }]);
        let report = aggregate(
            &[r],
            "personal",
            "offline",
            "stub",
            "passthrough",
            10,
            &SeededDataset::default(),
        );
        assert_eq!(report.metrics.recall_at_1, 1.0);
        let cmp = report
            .legacy_default_comparison
            .expect("comparison present");
        assert_eq!(cmp.queries, 1);
        assert_eq!(cmp.recall_at_1, 0.0);
        assert_eq!(cmp.recall_at_10, 0.0);
    }

    #[tokio::test]
    async fn seeding_writes_every_memory_and_maps_chunks_back() {
        let store = SqliteStore::open_in_memory().unwrap();
        let embedder = StubEmbedder::default();
        let seeded = seed_store(&store, &embedder).await.unwrap();
        assert_eq!(seeded.memory_ids.len(), MEMORIES.len());
        assert!(seeded.chunks_written >= MEMORIES.len());
        for (chunk_id, dataset_id) in &seeded.chunk_to_memory {
            assert!(seeded.dataset_id_for(chunk_id) == Some(dataset_id.as_str()));
        }
        // Every gold id resolves to a stored memory.
        for q in QUERIES {
            for g in q.gold {
                assert!(seeded.memory_ids.contains_key(*g), "gold {g} not seeded");
            }
        }
    }

    #[tokio::test]
    async fn seeded_event_times_survive_the_write_path() {
        let store = SqliteStore::open_in_memory().unwrap();
        let embedder = StubEmbedder::default();
        let seeded = seed_store(&store, &embedder).await.unwrap();
        let id = seeded.memory_ids.get("p-dentist").copied().unwrap();
        let m = store
            .list_memories(Some(DIARY), 1000)
            .unwrap()
            .into_iter()
            .find(|m| m.id == id)
            .expect("memory stored");
        let event = m.event_time.expect("event time persisted");
        assert_eq!(event.format("%Y-%m-%d").to_string(), "2025-02-07");
        // Written down long after it happened — a temporal query must not be
        // answerable from created_at.
        assert!(m.created_at > event);
    }
}
