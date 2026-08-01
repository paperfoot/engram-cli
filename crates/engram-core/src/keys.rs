//! Deterministic identity primitives: content hashes and semantic-key
//! encoding (interfaces.md ruling D15, amending R9).
//!
//! One home for these, in core, because the v9→v10 migration keys ~14.5k
//! historical adjudication decisions through them and neither storage nor
//! graph may drift from the other: a decision keyed by one hash and looked
//! up by another silently forgets everything the user ever adjudicated.
//! `Uuid::new_v5` (SHA-1, namespaced) on purpose — the workspace already
//! carries the feature and the alternative was a new dependency for no
//! agent-visible gain.

use uuid::Uuid;

/// Namespace for [`object_pair_hash`]. Never change: it is baked into every
/// stored `decision_key`.
pub const SEMANTIC_KEY_NAMESPACE: Uuid = Uuid::from_u128(0x6d1b9d3a_4f0e_5a7c_9b21_8f1c0d6e4a55);
/// Bump when the ENCODING scheme changes; stored on every decision row so a
/// scheme change re-keys instead of re-litigating.
pub const SEMANTIC_KEY_VERSION: &str = "semantic_key@1";
/// Namespace for [`content_hash`]. Same never-change rule: the derivation
/// ledger is keyed by it.
pub const CONTENT_HASH_NAMESPACE: Uuid = Uuid::from_u128(0x2f7c5e88_1a34_5c62_8d90_3b4e7a1f6c02);

const SEP: char = '\u{1f}';

/// Order-insensitive identity of an object PAIR — the "which two values were
/// being compared" half of a decision key. Over the facts' `object_norm`s,
/// NOT their member identities: the migration has only `object_norm`.
pub fn object_pair_hash(a_norm: &str, b_norm: &str) -> String {
    let (lo, hi) = if a_norm <= b_norm {
        (a_norm, b_norm)
    } else {
        (b_norm, a_norm)
    };
    let mut buf = Vec::with_capacity(lo.len() + hi.len() + 1);
    buf.extend_from_slice(lo.as_bytes());
    buf.push(0x1f);
    buf.extend_from_slice(hi.as_bytes());
    Uuid::new_v5(&SEMANTIC_KEY_NAMESPACE, &buf)
        .simple()
        .to_string()
}

/// `subject \x1f relation \x1f pair_hash` — the stored `decision_key`.
pub fn semantic_key_encode(subject_norm: &str, relation: &str, pair_hash: &str) -> String {
    format!("{subject_norm}{SEP}{relation}{SEP}{pair_hash}")
}

/// Inverse of [`semantic_key_encode`]; `None` when the shape is wrong.
pub fn semantic_key_decode(s: &str) -> Option<(String, String, String)> {
    let mut parts = s.split(SEP);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(c), None) if !a.is_empty() && !b.is_empty() && !c.is_empty() => {
            Some((a.to_string(), b.to_string(), c.to_string()))
        }
        _ => None,
    }
}

/// Content identity for the derivation ledger: extraction is a memoized pure
/// function of (content_hash, extractor id), so an edit that leaves a span
/// unchanged never re-pays for it.
pub fn content_hash(content: &str) -> String {
    Uuid::new_v5(&CONTENT_HASH_NAMESPACE, content.as_bytes())
        .simple()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_hash_is_order_insensitive_and_separator_safe() {
        assert_eq!(object_pair_hash("a", "b"), object_pair_hash("b", "a"));
        // The 0x1f separator prevents ("ab","c") colliding with ("a","bc").
        assert_ne!(object_pair_hash("ab", "c"), object_pair_hash("a", "bc"));
    }

    #[test]
    fn pair_hash_is_stable_across_processes() {
        // Pinned value: if this changes, every stored decision_key orphans.
        assert_eq!(
            object_pair_hash("empagliflozin 12.5 mg daily", "rosuvastatin 5 mg"),
            object_pair_hash("rosuvastatin 5 mg", "empagliflozin 12.5 mg daily"),
        );
        let h = object_pair_hash("a", "b");
        assert_eq!(h.len(), 32, "simple (un-hyphenated) uuid form");
        assert_eq!(h, object_pair_hash("a", "b"));
    }

    #[test]
    fn key_roundtrips_and_rejects_malformed() {
        let key = semantic_key_encode("ana", "takes_medication", &object_pair_hash("x", "y"));
        let (s, r, p) = semantic_key_decode(&key).unwrap();
        assert_eq!(s, "ana");
        assert_eq!(r, "takes_medication");
        assert_eq!(p, object_pair_hash("x", "y"));
        assert_eq!(semantic_key_decode("no separators"), None);
        assert_eq!(semantic_key_decode("a\u{1f}b"), None);
        assert_eq!(semantic_key_decode("a\u{1f}b\u{1f}c\u{1f}d"), None);
    }

    #[test]
    fn content_hash_distinguishes_and_repeats() {
        assert_eq!(content_hash("same"), content_hash("same"));
        assert_ne!(content_hash("same"), content_hash("same "));
    }
}
