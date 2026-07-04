//! PDF extraction.
//!
//! Uses `pdf-extract` to get raw text from a scientific paper and returns it
//! to the ingest pipeline. Scientific papers have a predictable shape
//! (abstract, intro, methods, results, discussion, references) so the
//! section-aware chunker downstream produces meaningful breadcrumbs.

use crate::error::IngestError;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

/// Extract text from a PDF file. Returns cleaned plain UTF-8 on success.
///
/// `pdf-extract` can panic (e.g. `unwrap()` on malformed font tables in some
/// scientific PDFs). We isolate it with `catch_unwind` so one bad file becomes
/// a recoverable error instead of crashing the entire ingest batch. The raw
/// text is then run through [`clean_extracted_text`] before returning.
pub fn extract_text(path: &Path) -> Result<String, IngestError> {
    let raw = catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text(path)))
        .map_err(|_| IngestError::Invalid("pdf extract: parser panicked".to_string()))?
        .map_err(|e| IngestError::Invalid(format!("pdf extract: {e}")))?;
    Ok(clean_extracted_text(&raw))
}

/// Best-effort extraction for a PDF byte slice. Cleans the raw text like
/// [`extract_text`].
pub fn extract_text_from_bytes(bytes: &[u8]) -> Result<String, IngestError> {
    let raw = catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(bytes)))
        .map_err(|_| IngestError::Invalid("pdf extract: parser panicked".to_string()))?
        .map_err(|e| IngestError::Invalid(format!("pdf extract: {e}")))?;
    Ok(clean_extracted_text(&raw))
}

/// Is this path a PDF?
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

/// Longest line (chars) still eligible to be a repeated running header/footer.
/// Body prose lines are usually longer; page furniture is short.
const RUNNING_HEAD_MAX_CHARS: usize = 80;

/// A short line must repeat at least this many times to count as a running
/// header/footer. Real prose almost never repeats an identical short line this
/// often, so the threshold keeps false positives low.
const RUNNING_HEAD_MIN_REPEATS: usize = 3;

/// Post-process raw `pdf-extract` output into cleaner prose for the chunker.
///
/// Two conservative passes:
/// 1. Strip page furniture — bare page-number lines and running headers/footers
///    that repeat across pages (journal name, running title).
/// 2. De-hyphenate words broken across a line break ("mito-\nchondria").
///
/// KNOWN LIMITATION: two-column PDFs are extracted with the columns interleaved
/// line-by-line; de-interleaving them is out of scope here (it needs glyph
/// coordinates that `pdf-extract`'s plain-text output does not expose).
fn clean_extracted_text(raw: &str) -> String {
    let without_furniture = strip_page_furniture(raw);
    dehyphenate(&without_furniture)
}

/// Remove bare page-number lines and repeated running headers/footers.
fn strip_page_furniture(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();

    // Tally short, non-empty lines. A short line repeating across the document
    // is almost always a running header/footer rather than body prose.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for line in &lines {
        let t = line.trim();
        if !t.is_empty() && t.chars().count() <= RUNNING_HEAD_MAX_CHARS {
            *counts.entry(t).or_insert(0) += 1;
        }
    }

    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    for line in &lines {
        let t = line.trim();
        // Drop bare page-number lines ("12", "- 12 -", "Page 12").
        if is_page_number_line(t) {
            continue;
        }
        // Drop repeated running headers/footers.
        if !t.is_empty()
            && t.chars().count() <= RUNNING_HEAD_MAX_CHARS
            && counts.get(t).copied().unwrap_or(0) >= RUNNING_HEAD_MIN_REPEATS
        {
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// Is this (already-trimmed) line just a page number? Accepts a bare number, a
/// dash-decorated number ("- 12 -"), or a "Page 12" / "p. 12" label. Capped at
/// 4 digits so real data values / identifiers are never mistaken for a page tag.
fn is_page_number_line(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return false;
    }
    // Strip surrounding dash/whitespace decoration.
    let core = trimmed
        .trim_matches(|c: char| c == '-' || c == '—' || c == '–' || c.is_whitespace());
    // Optional leading "page" / "p." label.
    let lower = core.to_ascii_lowercase();
    let digits = lower
        .strip_prefix("page")
        .or_else(|| lower.strip_prefix("p."))
        .map(str::trim)
        .unwrap_or(core);
    !digits.is_empty() && digits.len() <= 4 && digits.chars().all(|c| c.is_ascii_digit())
}

/// Rejoin words split by a hyphen at a line break: "mito-\nchondria" ->
/// "mitochondria". Conservative — only fires when a lowercase letter precedes
/// the end-of-line hyphen AND a lowercase letter starts the next line, so real
/// compounds ("well-being"), enumerations ("- item"), and ranges are untouched.
fn dehyphenate(text: &str) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut i = 0;
    while i + 1 < lines.len() {
        // Decide the merge with only immutable borrows, then mutate.
        let prefix = {
            let cur = lines[i].trim_end();
            match cur.strip_suffix('-') {
                Some(p)
                    if p.chars().last().map_or(false, |c| c.is_ascii_lowercase())
                        && lines[i + 1]
                            .trim_start()
                            .chars()
                            .next()
                            .map_or(false, |c| c.is_ascii_lowercase()) =>
                {
                    Some(p.to_string())
                }
                _ => None,
            }
        };
        if let Some(prefix) = prefix {
            let continuation = lines[i + 1].trim_start().to_string();
            lines[i] = format!("{prefix}{continuation}");
            lines.remove(i + 1);
            continue; // re-examine the merged line for chained hyphenation
        }
        i += 1;
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dehyphenates_across_line_break() {
        let cleaned = clean_extracted_text("The mito-\nchondria produce ATP.");
        assert!(cleaned.contains("mitochondria"), "got {cleaned:?}");
        assert!(!cleaned.contains("mito-"), "hyphen not removed: {cleaned:?}");
    }

    #[test]
    fn dehyphenation_handles_chained_breaks() {
        let cleaned = clean_extracted_text("electro-\nphoresis and immuno-\nblotting were used.");
        assert!(cleaned.contains("electrophoresis"), "got {cleaned:?}");
        assert!(cleaned.contains("immunoblotting"), "got {cleaned:?}");
    }

    #[test]
    fn preserves_intraline_and_uppercase_hyphens() {
        // Real compound on one line is untouched.
        let a = clean_extracted_text("A well-being metric was used.");
        assert!(a.contains("well-being"), "got {a:?}");
        // Uppercase after the break (e.g. "T-\nCell") is NOT joined blindly.
        let b = clean_extracted_text("The anti-\nCD3 antibody bound.");
        assert!(b.contains("anti-"), "should not merge into uppercase: {b:?}");
    }

    #[test]
    fn strips_bare_page_numbers() {
        let cleaned = clean_extracted_text("Body text here.\n12\nMore body text.");
        assert!(!cleaned.lines().any(|l| l.trim() == "12"), "got {cleaned:?}");
        assert!(cleaned.contains("Body text here."));
        assert!(cleaned.contains("More body text."));
    }

    #[test]
    fn keeps_long_numbers_that_are_not_page_tags() {
        // A 6-digit value is data, not a page number.
        let cleaned = clean_extracted_text("Count was\n123456\nin the sample.");
        assert!(cleaned.contains("123456"), "got {cleaned:?}");
    }

    #[test]
    fn strips_repeated_running_headers() {
        let raw = "Nature Aging\nReal content one.\nNature Aging\nReal content two.\nNature Aging\nReal content three.";
        let cleaned = clean_extracted_text(raw);
        assert!(
            !cleaned.lines().any(|l| l.trim() == "Nature Aging"),
            "running header not stripped: {cleaned:?}"
        );
        assert!(cleaned.contains("Real content one."));
        assert!(cleaned.contains("Real content three."));
    }

    #[test]
    fn keeps_unique_short_lines() {
        // A short line that appears once is real content, not furniture.
        let cleaned = clean_extracted_text("Abstract\nWe studied aging.");
        assert!(cleaned.contains("Abstract"), "got {cleaned:?}");
    }
}
