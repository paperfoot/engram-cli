//! Scientific-text sentence segmentation.
//!
//! The naive `split_terminator('.')` used by the old compiler shattered every
//! decimal and abbreviation: "IC50 of 0.1 nM" became "IC50 of 0" + "1 nM",
//! "p < 0.05" became "p < 0" + "05", "et al." / "Fig. 2" / "37.5 °C" all split
//! mid-token. For a tool whose whole promise is *precise* numeric facts that is
//! fatal — it manufactures numerically-wrong claims from any sentence with a
//! decimal.
//!
//! This segmenter only treats a `.`/`!`/`?` as a sentence boundary when it is
//! not part of a number, not part of a known abbreviation, and is followed by
//! whitespace and a likely sentence start. It returns each sentence together
//! with its char offset into the input so callers can record exact source
//! spans.

/// A sentence with its char-offset range `[start, end)` into the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Abbreviations that end in `.` but do NOT end a sentence. Lowercased; the
/// match is case-insensitive on the token immediately before the period.
const ABBREVIATIONS: &[&str] = &[
    // Latin / scholarly
    "e.g", "i.e", "et al", "etc", "cf", "vs", "viz", "al", "ibid", "op", // Titles
    "dr", "prof", "mr", "mrs", "ms", "st", // Scientific structure
    "fig", "figs", "eq", "eqs", "ref", "refs", "no", "nos", "vol", "pp", "ch", "sec", "tab",
    "approx", "ca", "min", "max", "avg", "std",
    // Units that are sometimes written with a trailing dot
    "mol", "conc", "temp", "wt", "vol",
];

/// Segment `text` into sentences, preserving char offsets. Newlines are treated
/// as soft boundaries (a run of blank lines ends a sentence) so that list items
/// and headings don't glue onto prose.
pub fn segment(text: &str) -> Vec<Sentence> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize; // char index where the current sentence began
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        let is_terminator = c == '.' || c == '!' || c == '?';
        let is_double_newline = c == '\n' && chars.get(i + 1).copied() == Some('\n');

        if is_terminator && is_boundary(&chars, i) {
            let end = i + 1; // include the terminator
            push_sentence(&chars, start, end, &mut out);
            // Skip following whitespace to the next sentence start.
            let mut j = end;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            start = j;
            i = j;
            continue;
        }
        if is_double_newline {
            push_sentence(&chars, start, i, &mut out);
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            start = j;
            i = j;
            continue;
        }
        i += 1;
    }
    if start < chars.len() {
        push_sentence(&chars, start, chars.len(), &mut out);
    }
    out
}

/// Convenience: sentence texts only (trimmed, non-empty).
pub fn split(text: &str) -> Vec<String> {
    segment(text).into_iter().map(|s| s.text).collect()
}

fn push_sentence(chars: &[char], start: usize, end: usize, out: &mut Vec<Sentence>) {
    if start >= end || start >= chars.len() {
        return;
    }
    let end = end.min(chars.len());
    let raw: String = chars[start..end].iter().collect();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    // Recover exact offsets after trimming leading/trailing whitespace.
    let leading = raw.chars().take_while(|c| c.is_whitespace()).count();
    let trailing = raw.chars().rev().take_while(|c| c.is_whitespace()).count();
    out.push(Sentence {
        text: trimmed.to_string(),
        start: start + leading,
        end: end - trailing,
    });
}

/// Decide whether the terminator at `chars[i]` truly ends a sentence.
fn is_boundary(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    // '!' and '?' are almost always real boundaries; only require a following
    // whitespace/EOF so "Wnt?" mid-token doesn't split URLs etc.
    if c == '!' || c == '?' {
        return followed_by_break(chars, i);
    }
    // From here c == '.'.
    // 1. Decimal point: digit '.' digit  → not a boundary.
    let prev = if i > 0 {
        chars.get(i - 1).copied()
    } else {
        None
    };
    let next = chars.get(i + 1).copied();
    if matches!(prev, Some(p) if p.is_ascii_digit())
        && matches!(next, Some(n) if n.is_ascii_digit())
    {
        return false;
    }
    // 2. Must be followed by a break (whitespace/EOF) and a plausible start.
    if !followed_by_break(chars, i) {
        return false;
    }
    // 3. Abbreviation immediately before the dot → not a boundary.
    if preceding_token_is_abbrev(chars, i) {
        return false;
    }
    // 4. Single-letter initial ("A. Smith", "S. cerevisiae"): a lone capital
    //    letter before the dot is usually an initial, not a sentence end.
    if matches!(prev, Some(p) if p.is_ascii_uppercase()) && (i < 2 || chars[i - 2].is_whitespace())
    {
        return false;
    }
    true
}

/// True if the terminator is followed by whitespace (or end of text), and the
/// next non-space char looks like a sentence start (uppercase, digit, quote,
/// or opening bracket) — or it's the end of the text.
fn followed_by_break(chars: &[char], i: usize) -> bool {
    let mut j = i + 1;
    // Allow trailing closing quotes/brackets right after the terminator.
    while let Some(&c) = chars.get(j) {
        if c == '"' || c == '\'' || c == ')' || c == ']' || c == '»' || c == '”' {
            j += 1;
        } else {
            break;
        }
    }
    match chars.get(j) {
        None => true,
        Some(c) if c.is_whitespace() => {
            // Peek the next non-whitespace char.
            let mut k = j;
            while let Some(&w) = chars.get(k) {
                if w.is_whitespace() {
                    k += 1;
                } else {
                    break;
                }
            }
            match chars.get(k) {
                None => true,
                Some(n) => {
                    n.is_ascii_uppercase()
                        || n.is_ascii_digit()
                        || *n == '"'
                        || *n == '\''
                        || *n == '('
                        || *n == '['
                        || *n == '“'
                        || *n == '·'
                        || *n == '-'
                        // Scientific sentences often START with a lowercase
                        // symbol (mTOR, p53, mRNA, siRNA). Accept a lowercase
                        // start only when the leading token is symbol-like:
                        // it carries an uppercase letter or digit within its
                        // first few chars, unlike ordinary prose words.
                        || (n.is_ascii_lowercase() && token_is_symbolish(chars, k))
                }
            }
        }
        _ => false,
    }
}

/// Does the token starting at `k` look like a scientific symbol rather than an
/// ordinary lowercase word? True when an uppercase letter or digit appears
/// within the token's first 6 chars (mTOR, p53, mRNA, siRNA, iPSC).
fn token_is_symbolish(chars: &[char], k: usize) -> bool {
    let mut j = k;
    let mut count = 0;
    while j < chars.len() && count < 6 {
        let c = chars[j];
        if c.is_whitespace() {
            break;
        }
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return true;
        }
        j += 1;
        count += 1;
    }
    false
}

fn preceding_token_is_abbrev(chars: &[char], dot: usize) -> bool {
    // Walk back to the start of the alphabetic token before the dot.
    let mut start = dot;
    while start > 0 {
        let p = chars[start - 1];
        if p.is_ascii_alphabetic() || p == '.' {
            start -= 1;
        } else {
            break;
        }
    }
    let token: String = chars[start..dot].iter().collect();
    let token = token.trim_matches('.').to_ascii_lowercase();
    if token.is_empty() {
        return false;
    }
    ABBREVIATIONS.contains(&token.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        split(text)
    }

    #[test]
    fn does_not_split_decimals() {
        let s = texts("The IC50 of rapamycin is 0.1 nM in this assay. It is potent.");
        assert_eq!(s.len(), 2);
        assert!(s[0].contains("0.1 nM"), "got {s:?}");
        assert_eq!(s[1], "It is potent.");
    }

    #[test]
    fn does_not_split_pvalues() {
        let s = texts("Survival improved (p < 0.05) in treated mice. No harm was seen.");
        assert_eq!(s.len(), 2, "got {s:?}");
        assert!(s[0].contains("0.05"));
    }

    #[test]
    fn does_not_split_abbreviations() {
        let s = texts("Rapamycin, mTOR inhibitors, etc. were tested by Smith et al. in 2006.");
        assert_eq!(s.len(), 1, "got {s:?}");
    }

    #[test]
    fn does_not_split_figure_refs() {
        let s = texts("Lifespan rose 24% (Fig. 2). This was significant.");
        assert_eq!(s.len(), 2, "got {s:?}");
    }

    #[test]
    fn splits_normal_sentences() {
        let s = texts("Aging is complex. Rapamycin extends lifespan. mTOR is central.");
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn offsets_are_exact() {
        let text = "First one. Second two.";
        let sents = segment(text);
        assert_eq!(sents.len(), 2);
        assert_eq!(&text[sents[0].start..sents[0].end], "First one.");
        assert_eq!(&text[sents[1].start..sents[1].end], "Second two.");
    }

    #[test]
    fn double_newline_breaks() {
        let s = texts("Heading line\n\nBody sentence with data 3.5 mg.");
        assert_eq!(s.len(), 2, "got {s:?}");
    }

    #[test]
    fn species_initials_not_split() {
        let s = texts("We used S. cerevisiae in the assay. Growth was normal.");
        assert_eq!(s.len(), 2, "got {s:?}");
    }
}
