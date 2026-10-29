//! The relation registry: arity, mutability, temporality, and safety — the
//! single table every lane consults before deciding what a second value on a
//! relation means.
//!
//! One home, in engram-core, on purpose (interfaces.md v2, ruling R1): the
//! v9→v10 migration keys thousands of decision rows via the member heuristic
//! below, and engram-core cannot depend on engram-graph, so a second copy of
//! this table in the graph crate would drift on the first edit — and the
//! arity table IS the polypharmacy fix. `engram_graph::relations` re-exports;
//! fact-semantics owns the CONTENT of the table, this crate only houses it.
//!
//! The class of bug this retires, observed live on 0.3.1: `takes_medication`
//! was a single-valued slot, so five concurrent medications produced ten
//! pairwise "conflicts" — N·(N−1)/2 — each suggesting the previous drug be
//! superseded. Reality is a SET whose members change over time; asserting a
//! second member is an addition, not a disagreement.

use serde::{Deserialize, Serialize};

/// Bump when the registry TABLE changes meaning (not when entries are added
/// compatibly). Stored on queue rows so a migration can re-key.
pub const REGISTRY_VERSION: &str = "relations@1";
/// Bump when `member_identity`'s heuristic changes; stored beside it.
pub const MEMBER_IDENTITY_VERSION: &str = "member_identity@1";

/// How many values one `(subject, relation)` may hold at once — the new,
/// payload-carrying arity. The legacy two-variant enum is [`LegacyArity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arity {
    /// Members coexist; a second member is an addition. `safety` gates who
    /// may CLOSE a member's interval, never whether members may coexist.
    SetValued { safety: bool },
    /// One value at a time; a different value is a real disagreement.
    SingleValued { safety: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mutability {
    /// The true value can change over time (residence, role, regimen).
    Mutable,
    /// The true value never changes (birthday, blood type). A disagreement is
    /// an error somewhere, not an update.
    Immutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temporality {
    /// True over an interval (takes_medication, lives_in).
    State,
    /// True at a moment (born_on, married_on). Point intervals never overlap
    /// anything — see `interval::Interval::is_point`.
    Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueDomain {
    Date,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationSpec {
    pub arity: Arity,
    pub mutability: Mutability,
    pub temporality: Temporality,
    pub value: ValueDomain,
}

impl RelationSpec {
    pub fn is_safety(&self) -> bool {
        matches!(
            self.arity,
            Arity::SetValued { safety: true } | Arity::SingleValued { safety: true }
        )
    }
    pub fn is_set_valued(&self) -> bool {
        matches!(self.arity, Arity::SetValued { .. })
    }
    pub fn is_single_valued(&self) -> bool {
        matches!(self.arity, Arity::SingleValued { .. })
    }
}

const fn spec(
    arity: Arity,
    mutability: Mutability,
    temporality: Temporality,
    value: ValueDomain,
) -> RelationSpec {
    RelationSpec {
        arity,
        mutability,
        temporality,
        value,
    }
}

/// Registry v1. Anything not listed is `SetValued{safety:false}`, Mutable,
/// State, Text — the safe default: a wrong `Set` costs two coexisting facts,
/// a wrong `Single` silently retires a true one.
pub fn relation_spec(canonical: &str) -> RelationSpec {
    use Arity::*;
    use Mutability::*;
    use Temporality::*;
    use ValueDomain::*;
    match canonical {
        // Safety SETS — changed from single-valued slots; the storm fix.
        "takes_medication" | "allergic_to" | "diagnosed_with" => {
            spec(SetValued { safety: true }, Mutable, State, Text)
        }
        // The one true safety SLOT.
        "blood_type" => spec(SingleValued { safety: true }, Immutable, State, Text),
        // Immutable instants (Date domain).
        "birthday" | "has_birthday" | "date_of_birth" | "born_on" | "died_on" => {
            spec(SingleValued { safety: false }, Immutable, Instant, Date)
        }
        // Immutable states.
        "nationality" | "has_nationality" => {
            spec(SingleValued { safety: false }, Immutable, State, Text)
        }
        // Today's LastWriterWins profile slots: single-valued, mutable.
        "lives_in" | "works_at" | "has_role" | "has_age" | "married_to" | "partner_of"
        | "prefers_language" => spec(SingleValued { safety: false }, Mutable, State, Text),
        // Life events: many may exist, none may change.
        "married_on" | "founded_on" | "graduated_on" | "joined_on" | "started_on" | "ended_on"
        | "hired_on" => spec(SetValued { safety: false }, Immutable, Instant, Date),
        _ => spec(SetValued { safety: false }, Mutable, State, Text),
    }
}

pub fn relation_arity(canonical: &str) -> Arity {
    relation_spec(canonical).arity
}

/// Registry-backed, NOT the deprecated slot_policy shim: under the shim the
/// safety sets map to nothing and this would return false for exactly the
/// relations it exists to flag (spec-fact-semantics §2.1).
pub fn is_safety_slot(canonical: &str) -> bool {
    relation_spec(canonical).is_safety()
}

/// Legacy projection for surfaces that already publish `arity: "single"|"multi"`.
/// Kept under a legacy NAME at every call site (ruling R3): nothing re-exports
/// this as `Arity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyArity {
    Single,
    Multi,
}

pub fn predicate_arity(canonical: &str) -> LegacyArity {
    match relation_spec(canonical).arity {
        Arity::SingleValued { .. } => LegacyArity::Single,
        Arity::SetValued { .. } => LegacyArity::Multi,
    }
}

/// Synonym map + normalization, mirroring `engram_graph::facts::canonical_predicate`
/// exactly; that function will delegate here once the lanes land. Then applies
/// nothing else — cessation phrasings are NOT canonicalized away, because
/// "stopped_taking" names the END of an interval, not a relation (see
/// [`cessation_of`]).
pub fn canonical_relation(raw: &str) -> String {
    let p = raw.trim().to_lowercase().replace([' ', '-'], "_");
    match p.as_str() {
        "employed_by" | "employer" | "job_at" | "works_for" | "employed_at" => "works_at",
        "resides_in" | "home_city" | "based_in" | "living_in" | "residence" => "lives_in",
        "age" | "aged" | "is_age" => "has_age",
        "role" | "position" | "job_title" | "title" => "has_role",
        "spouse" | "spouse_of" | "wife_of" | "husband_of" => "married_to",
        "dating" | "girlfriend_of" | "boyfriend_of" | "in_relationship_with" => "partner_of",
        // Safety aliases. The safety gate, the grounding cut and the import
        // close refusal all key on the CANONICAL relation, so a safety fact
        // spelled any other way skips every one of them and `state --relation
        // allergic_to` reports no allergy. Over-mapping costs a misfiled
        // non-medical fact; under-mapping costs an unguarded allergy.
        "allergy"
        | "has_allergy"
        | "allergic"
        | "is_allergic_to"
        | "allergies"
        | "has_allergies"
        | "allergy_to"
        | "has_allergy_to"
        | "allergic_reaction_to" => "allergic_to",
        "medication"
        | "medications"
        | "on_medication"
        | "is_on_medication"
        | "takes_drug"
        | "prescribed"
        | "is_prescribed"
        | "was_prescribed"
        | "prescribed_medication"
        | "takes_medicine"
        | "takes_medications" => "takes_medication",
        "diagnosis"
        | "has_diagnosis"
        | "diagnosed"
        | "is_diagnosed_with"
        | "was_diagnosed_with"
        | "has_condition"
        | "condition"
        | "medical_condition"
        | "has_medical_condition"
        | "suffers_from"
        | "has_disease" => "diagnosed_with",
        "has_blood_type" | "bloodtype" | "blood_group" | "has_blood_group" => "blood_type",
        "favorite_language" | "preferred_language" | "codes_in" => "prefers_language",
        _ => return p,
    }
    .to_string()
}

/// `stopped_taking` names the END of a `takes_medication` interval, not a
/// relation of its own. Returns the relation whose open interval such a
/// predicate closes. v1 is deliberately small; fact-semantics extends it.
pub fn cessation_of(raw_or_canonical: &str) -> Option<&'static str> {
    let p = raw_or_canonical
        .trim()
        .to_lowercase()
        .replace([' ', '-'], "_");
    Some(match p.as_str() {
        "stopped_taking" | "no_longer_takes" | "discontinued" | "stopped_medication"
        | "off_medication" => "takes_medication",
        "no_longer_allergic" | "outgrew_allergy" => "allergic_to",
        "recovered_from" | "cured_of" | "in_remission_from" => "diagnosed_with",
        "moved_from" | "no_longer_lives_in" => "lives_in",
        "no_longer_works_at" | "left_job_at" | "quit_job_at" => "works_at",
        "divorced_from" | "no_longer_married_to" => "married_to",
        "broke_up_with" | "no_longer_dating" => "partner_of",
        _ => return None,
    })
}

/// Which SET MEMBER an object names. "empagliflozin 12.5 mg daily" and
/// "empagliflozin 25mg" are the same member at different doses; "metformin"
/// is a different member entirely. For single-valued relations the member IS
/// the whole object — there is only one slot to be in.
///
/// Errs toward MORE distinct members: under-merging costs a second coexisting
/// interval; over-merging closes an interval that was true, or manufactures a
/// dose-change conflict out of two different drugs. So `metformin xr` stays
/// distinct from `metformin`.
///
/// Canonicalizes the relation internally (ruling R6): callers cannot get the
/// precondition wrong. Pure; versioned by [`MEMBER_IDENTITY_VERSION`].
pub fn member_identity(relation: &str, object_norm: &str) -> String {
    let canonical = canonical_relation(relation);
    if relation_spec(&canonical).is_single_valued() {
        return object_norm.trim().to_string();
    }
    let s = object_norm.trim();

    // 1. Drop a trailing parenthetical: "penicillin (severe)" → "penicillin".
    let s = match (s.rfind('('), s.ends_with(')')) {
        (Some(open), true) => s[..open].trim_end(),
        _ => s,
    };

    // 2. Cut at the first clause separator and keep the head.
    let mut head = s;
    for sep in [",", ";", " - ", "—", ":"] {
        if let Some(i) = head.find(sep) {
            head = head[..i].trim_end();
        }
    }

    // 3. Pop trailing noise tokens (dose, unit, frequency, route, qualifier).
    let mut tokens: Vec<&str> = head.split_whitespace().collect();
    let mut popped_number = false;
    while tokens.len() > 1 {
        let last = tokens[tokens.len() - 1];
        if is_number_or_dose(last) {
            popped_number = true;
            tokens.pop();
        } else if is_unit(last) || is_frequency(last) || is_route_or_form(last) {
            popped_number = false;
            tokens.pop();
        } else if matches!(last, "stage" | "grade" | "type") && popped_number {
            // "hypertension stage 2": the 2 popped first, then its qualifier.
            popped_number = false;
            tokens.pop();
        } else if matches!(last, "allergy" | "allergies") && canonical == "allergic_to" {
            popped_number = false;
            tokens.pop();
        } else {
            break;
        }
    }

    // 4. Drop a leading article.
    if tokens.len() > 1 && matches!(tokens[0], "the" | "a") {
        tokens.remove(0);
    }

    // 5. Collapse whitespace; empty result falls back to the full object.
    let member = tokens.join(" ");
    if member.is_empty() {
        object_norm.trim().to_string()
    } else {
        member
    }
}

fn is_number_or_dose(t: &str) -> bool {
    // "12.5", "500", "12.5mg", "2x", "1000iu" — digits first, optional unit tail.
    let mut chars = t.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => {}
        _ => return false,
    }
    t.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '/' || c.is_ascii_alphabetic() || c == '%')
}

fn is_unit(t: &str) -> bool {
    matches!(
        t,
        "mg" | "mcg"
            | "µg"
            | "g"
            | "ml"
            | "l"
            | "iu"
            | "unit"
            | "units"
            | "tablet"
            | "tablets"
            | "tab"
            | "caps"
            | "capsule"
            | "capsules"
            | "puff"
            | "puffs"
            | "drop"
            | "drops"
            | "%"
    )
}

fn is_frequency(t: &str) -> bool {
    matches!(
        t,
        "daily"
            | "nightly"
            | "weekly"
            | "monthly"
            | "bid"
            | "tid"
            | "qid"
            | "qd"
            | "hs"
            | "prn"
            | "once"
            | "twice"
            | "three"
            | "four"
            | "times"
            | "per"
            | "day"
            | "week"
            | "night"
            | "morning"
            | "evening"
            | "every"
            | "other"
    )
}

fn is_route_or_form(t: &str) -> bool {
    matches!(
        t,
        "oral"
            | "orally"
            | "po"
            | "iv"
            | "im"
            | "sc"
            | "subq"
            | "subcutaneous"
            | "topical"
            | "sublingual"
            | "inhaled"
            | "inhaler"
            | "injection"
            | "injected"
            | "patch"
            | "cream"
            | "gel"
            | "spray"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_sets_are_set_valued_and_safety() {
        for r in ["takes_medication", "allergic_to", "diagnosed_with"] {
            let s = relation_spec(r);
            assert!(s.is_set_valued(), "{r}");
            assert!(s.is_safety(), "{r}");
        }
    }

    #[test]
    fn blood_type_is_the_one_safety_slot() {
        let s = relation_spec("blood_type");
        assert!(s.is_single_valued());
        assert!(s.is_safety());
        assert_eq!(s.mutability, Mutability::Immutable);
    }

    #[test]
    fn unknown_relations_default_to_open_set() {
        let s = relation_spec("collects_stamps_of");
        assert_eq!(
            s.arity,
            Arity::SetValued { safety: false },
            "unknown must default multi — a wrong Single silently retires truth"
        );
    }

    #[test]
    fn no_instant_relation_is_mutable() {
        // §2.2's pin: Instant pairs never reach the interval branch because
        // every Instant relation is Immutable. Guard the invariant for every
        // registry entry we know about.
        for r in [
            "birthday",
            "has_birthday",
            "date_of_birth",
            "born_on",
            "died_on",
            "married_on",
            "founded_on",
            "graduated_on",
            "joined_on",
            "started_on",
            "ended_on",
            "hired_on",
        ] {
            let s = relation_spec(r);
            assert_eq!(s.temporality, Temporality::Instant, "{r}");
            assert_eq!(s.mutability, Mutability::Immutable, "{r}");
        }
    }

    #[test]
    fn legacy_arity_projection_matches_registry() {
        assert_eq!(predicate_arity("takes_medication"), LegacyArity::Multi);
        assert_eq!(predicate_arity("lives_in"), LegacyArity::Single);
        assert_eq!(predicate_arity("knows"), LegacyArity::Multi);
    }

    #[test]
    fn member_identity_strips_dose_route_and_frequency() {
        let cases = [
            ("empagliflozin 12.5 mg daily", "empagliflozin"),
            ("empagliflozin 25mg", "empagliflozin"),
            ("rapamycin 6 mg weekly", "rapamycin"),
            ("metformin 500 mg twice daily", "metformin"),
            ("lisinopril 10 mg daily", "lisinopril"),
            ("levothyroxine 75 mcg daily", "levothyroxine"),
            ("penicillin (severe)", "penicillin"),
            ("vitamin d 1000iu", "vitamin d"),
        ];
        for (input, want) in cases {
            assert_eq!(member_identity("takes_medication", input), want, "{input}");
        }
    }

    #[test]
    fn member_identity_errs_toward_distinctness() {
        assert_eq!(
            member_identity("takes_medication", "metformin xr"),
            "metformin xr"
        );
        assert_eq!(
            member_identity("diagnosed_with", "type 2 diabetes"),
            "type 2 diabetes",
            "nothing at the END is noise, so nothing is stripped"
        );
        assert_eq!(
            member_identity("diagnosed_with", "hypertension stage 2"),
            "hypertension"
        );
    }

    #[test]
    fn member_identity_single_valued_is_whole_object() {
        assert_eq!(
            member_identity("lives_in", "lisbon 12"),
            "lisbon 12",
            "single-valued members are never token-stripped"
        );
    }

    #[test]
    fn member_identity_canonicalizes_relation_internally() {
        // Ruling R6: raw and canonical spellings agree, so a caller passing
        // the raw predicate cannot compute a different member.
        assert_eq!(
            member_identity("prescribed", "empagliflozin 12.5 mg"),
            member_identity("takes_medication", "empagliflozin 12.5 mg"),
        );
    }

    #[test]
    fn cessation_maps_to_the_relation_it_closes() {
        assert_eq!(cessation_of("stopped_taking"), Some("takes_medication"));
        assert_eq!(cessation_of("stopped taking"), Some("takes_medication"));
        assert_eq!(cessation_of("recovered_from"), Some("diagnosed_with"));
        assert_eq!(cessation_of("takes_medication"), None);
    }

    #[test]
    fn canonical_relation_mirrors_graph_synonyms() {
        assert_eq!(canonical_relation("prescribed"), "takes_medication");
        assert_eq!(canonical_relation("Has Allergy"), "allergic_to");
        assert_eq!(canonical_relation("codes-in"), "prefers_language");
        assert_eq!(canonical_relation("novel_predicate"), "novel_predicate");
    }

    #[test]
    fn every_safety_spelling_lands_on_a_safety_relation() {
        // An unregistered spelling of a safety predicate used to stay its own
        // non-safety relation: `is_allergic_to penicillin` skipped the safety
        // gate, the grounding cut and the import close refusal, and
        // `state --relation allergic_to` answered "no allergy".
        let cases = [
            ("is_allergic_to", "allergic_to"),
            ("Is Allergic To", "allergic_to"),
            ("allergy", "allergic_to"),
            ("allergic", "allergic_to"),
            ("has_allergies", "allergic_to"),
            ("takes_medicine", "takes_medication"),
            ("on_medication", "takes_medication"),
            ("is-on-medication", "takes_medication"),
            ("prescribed", "takes_medication"),
            ("diagnosis", "diagnosed_with"),
            ("has_condition", "diagnosed_with"),
            ("was_diagnosed_with", "diagnosed_with"),
            ("suffers_from", "diagnosed_with"),
            ("has_blood_type", "blood_type"),
            ("blood_group", "blood_type"),
        ];
        for (raw, want) in cases {
            let canonical = canonical_relation(raw);
            assert_eq!(canonical, want, "{raw}");
            assert!(is_safety_slot(&canonical), "{raw} must be gated");
        }
        // Bare verbs stay generic: "takes piano lessons" is not a medication.
        for verb in ["takes", "taking", "is_taking"] {
            assert_eq!(canonical_relation(verb), verb);
            assert!(!is_safety_slot(verb), "{verb} must not be gated");
        }
        // Cessation phrasings still name the END of an interval, not a
        // relation: aliasing must not swallow them.
        assert_eq!(cessation_of("stopped_taking"), Some("takes_medication"));
        assert_eq!(canonical_relation("stopped_taking"), "stopped_taking");
    }
}
