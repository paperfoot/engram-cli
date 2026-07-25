//! Chunking strategies.
//!
//! `naive_split` is paragraph-based for the general case. `section_aware_split`
//! preserves markdown / paper section headers in chunk metadata so downstream
//! retrieval can surface context like "Methods > Cell Culture" without re-parsing.

#[derive(Debug, Clone)]
pub struct PendingChunk {
    pub text: String,
    pub position: u32,
    pub section: Option<String>,
}

/// Paragraph-based naive split. Drops empty / whitespace-only chunks.
pub fn naive_split(text: &str) -> Vec<PendingChunk> {
    let mut out = Vec::new();
    let mut pos = 0u32;
    for raw in text.split("\n\n") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(PendingChunk {
            text: trimmed.to_string(),
            position: pos,
            section: None,
        });
        pos += 1;
    }
    out
}

/// Section-aware chunker. Walks the document, tracks the current heading path
/// (`H1 > H2 > H3`), and attaches it to every chunk produced below it.
///
/// Recognizes:
/// - Markdown ATX headings (`# H1`, `## H2`, `### H3`, ...)
/// - Setext headings (`Title\n===` / `Title\n---`)
/// - Numbered scientific section titles (`1 Introduction`, `2.3 Cell culture`)
///   that live on their own line in ALL CAPS or Title Case.
/// - Unnumbered canonical section titles (`Methods`, `Results`, `References`)
///   alone on a short line — common once a PDF loses its section numbers.
pub fn section_aware_split(text: &str) -> Vec<PendingChunk> {
    let mut out = Vec::new();
    let mut pos = 0u32;
    let mut headings: Vec<String> = Vec::new();
    let mut buf = String::new();

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Setext heading: next line is === or ---
        if i + 1 < lines.len() {
            let next = lines[i + 1].trim();
            if !trimmed.is_empty()
                && (next.chars().all(|c| c == '=') && next.len() >= 3
                    || next.chars().all(|c| c == '-') && next.len() >= 3)
            {
                // Flush current buffer as a chunk.
                flush_chunk(&mut buf, &mut out, &mut pos, &headings);
                let level = if next.starts_with('=') { 1 } else { 2 };
                push_heading(&mut headings, level, trimmed);
                i += 2;
                continue;
            }
        }

        // ATX heading: '#' to '######'
        if let Some((level, title)) = parse_atx_heading(line) {
            flush_chunk(&mut buf, &mut out, &mut pos, &headings);
            push_heading(&mut headings, level, &title);
            i += 1;
            continue;
        }

        // Numbered scientific section: `1 Introduction`, `2.3 Cell culture`.
        if let Some((level, title)) = parse_numbered_heading(line) {
            flush_chunk(&mut buf, &mut out, &mut pos, &headings);
            push_heading(&mut headings, level, &title);
            i += 1;
            continue;
        }

        // Unnumbered canonical section heading alone on a short line
        // (`Methods`, `Results`, `References`, `Materials and Methods`, ...).
        // Real extracted PDFs frequently drop the section number, so these must
        // still register as breadcrumbs. Treated as top-level (level 1).
        if let Some(title) = parse_unnumbered_heading(line) {
            flush_chunk(&mut buf, &mut out, &mut pos, &headings);
            push_heading(&mut headings, 1, &title);
            i += 1;
            continue;
        }

        // Paragraph boundary
        if trimmed.is_empty() && !buf.is_empty() {
            flush_chunk(&mut buf, &mut out, &mut pos, &headings);
            i += 1;
            continue;
        }

        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
        i += 1;
    }
    flush_chunk(&mut buf, &mut out, &mut pos, &headings);
    out
}

/// Merge adjacent chunks that belong to the same section until the accumulated
/// text reaches `target_chars`. Paper chunkers emit roughly one paragraph per
/// chunk; a lone sentence embeds and reranks poorly, so we coalesce neighbours
/// into richer, self-contained units. Never merges across a section boundary and
/// never splits an already-large chunk; positions are renumbered sequentially.
pub fn merge_small_chunks(chunks: Vec<PendingChunk>, target_chars: usize) -> Vec<PendingChunk> {
    let mut out: Vec<PendingChunk> = Vec::with_capacity(chunks.len());
    let mut pos = 0u32;
    let mut cur: Option<PendingChunk> = None;
    for ch in chunks {
        match cur.take() {
            None => cur = Some(ch),
            Some(mut acc) => {
                if acc.section == ch.section && acc.text.len() < target_chars {
                    acc.text.push_str("\n\n");
                    acc.text.push_str(&ch.text);
                    cur = Some(acc);
                } else {
                    acc.position = pos;
                    pos += 1;
                    out.push(acc);
                    cur = Some(ch);
                }
            }
        }
    }
    if let Some(mut acc) = cur {
        acc.position = pos;
        out.push(acc);
    }
    out
}

/// Enforce a hard upper bound on chunk size. Any chunk whose text exceeds
/// `max_chars` is broken into several chunks at real sentence boundaries;
/// chunks already within the cap pass through untouched. Positions are
/// renumbered sequentially and the section breadcrumb is preserved on each piece.
///
/// This is the ceiling both paper and general chunking rely on. The downstream
/// embedder silently truncates anything past its token window, so a merged
/// reference list or a badly-extracted full page must be split here or its tail
/// becomes permanently unretrievable.
pub fn split_oversized(chunks: Vec<PendingChunk>, max_chars: usize) -> Vec<PendingChunk> {
    let mut out = Vec::with_capacity(chunks.len());
    let mut position = 0u32;
    for chunk in chunks {
        for text in pack_sentences(&chunk.text, max_chars) {
            out.push(PendingChunk {
                text,
                position,
                section: chunk.section.clone(),
            });
            position += 1;
        }
    }
    out
}

/// Pack `text` into pieces of at most `max_chars` bytes, cutting ONLY at real
/// sentence boundaries (via `crate::sentences::split`) so no numeric fact
/// ("0.1 nM", "p < 0.05") or abbreviation is ever severed. Returns the trimmed
/// input as a single piece when it already fits. A lone sentence longer than
/// `max_chars` is emitted whole rather than sliced mid-token — rare in real
/// prose, and one oversized piece beats a corrupted p-value.
pub fn pack_sentences(text: &str, max_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.len() <= max_chars {
        return vec![trimmed.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    for sentence in crate::sentences::split(trimmed) {
        // Start a new piece before this sentence would overflow the cap; never
        // flush an empty buffer, so an over-cap lone sentence lands whole.
        if !buf.is_empty() && buf.len() + 1 + sentence.len() > max_chars {
            out.push(buf.trim().to_string());
            buf.clear();
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(&sentence);
    }
    let tail = buf.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    // Safety net: if segmentation yielded nothing (e.g. no detectable
    // boundaries) keep the original text rather than dropping it.
    if out.is_empty() {
        out.push(trimmed.to_string());
    }
    out
}

fn flush_chunk(buf: &mut String, out: &mut Vec<PendingChunk>, pos: &mut u32, headings: &[String]) {
    let trimmed = buf.trim();
    if !trimmed.is_empty() {
        out.push(PendingChunk {
            text: trimmed.to_string(),
            position: *pos,
            section: if headings.is_empty() {
                None
            } else {
                Some(headings.join(" > "))
            },
        });
        *pos += 1;
    }
    buf.clear();
}

fn parse_atx_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let count = trimmed.chars().take_while(|c| *c == '#').count();
    if count == 0 || count > 6 {
        return None;
    }
    let rest = &trimmed[count..];
    if !rest.starts_with(' ') {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some((count, title))
    }
}

fn parse_numbered_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    let mut chars = trimmed.chars().peekable();
    // First char must be a digit.
    if !chars.peek().map_or(false, |c| c.is_ascii_digit()) {
        return None;
    }
    let mut depth = 1usize;
    let mut idx = 0usize;
    let bytes = trimmed.as_bytes();
    while idx < bytes.len() {
        let b = bytes[idx];
        if b.is_ascii_digit() {
            idx += 1;
        } else if b == b'.' {
            depth += 1;
            idx += 1;
        } else {
            break;
        }
    }
    // Must have at least one space after the numbering.
    if idx >= bytes.len() || bytes[idx] != b' ' {
        return None;
    }
    let title = trimmed[idx..].trim();
    if title.is_empty() {
        return None;
    }
    // Reject reference / citation lines that merely START with a number, e.g.
    // "1 Smith AB, Jones CD, Brown E. Nature 2020". Left unchecked they parse as
    // headings and corrupt the breadcrumb stack for every chunk that follows.
    // Author lists carry a year, "et al", many commas, or repeated
    // Surname-Initials pairs — genuine section titles carry none of these.
    if looks_like_reference(title) {
        return None;
    }
    // Heuristic: only treat as a heading if the title is <= 8 words and
    // mostly Title Case or ALL CAPS (typical for paper sections).
    let words: Vec<&str> = title.split_whitespace().collect();
    if words.len() > 8 {
        return None;
    }
    let caps_ratio = words
        .iter()
        .filter(|w| w.chars().next().map_or(false, |c| c.is_ascii_uppercase()))
        .count() as f32
        / words.len() as f32;
    if caps_ratio < 0.5 {
        return None;
    }
    Some((depth, title.to_string()))
}

/// Canonical scientific section titles that appear unnumbered on their own line.
/// Lowercased for case-insensitive comparison. Deliberately small: a false
/// heading shatters the breadcrumb stack, so we only accept titles that are
/// almost always real section boundaries.
const UNNUMBERED_SECTIONS: &[&str] = &[
    "abstract",
    "introduction",
    "background",
    "related work",
    "materials and methods",
    "materials and method",
    "methods",
    "method",
    "methodology",
    "experimental procedures",
    "results",
    "results and discussion",
    "discussion",
    "conclusion",
    "conclusions",
    "references",
    "bibliography",
    "acknowledgements",
    "acknowledgments",
    "supplementary material",
    "supplementary materials",
];

/// Recognize a canonical unnumbered section heading (`Methods`, `Results`,
/// `Materials and Methods`, ...) alone on a short line. Case-insensitive, at
/// most 5 words, tolerant of a trailing `:` or `.`. Returns the title with its
/// original casing so the breadcrumb reads naturally.
fn parse_unnumbered_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // A heading is a short label, never a sentence.
    let word_count = trimmed.split_whitespace().count();
    if word_count == 0 || word_count > 5 {
        return None;
    }
    let cleaned = trimmed.trim_end_matches([':', '.']).trim();
    if UNNUMBERED_SECTIONS.contains(&cleaned.to_ascii_lowercase().as_str()) {
        Some(cleaned.to_string())
    } else {
        None
    }
}

/// Does `title` look like a bibliography / citation line rather than a section
/// title? Used to stop numbered reference entries ("1 Smith AB, Jones CD, ...")
/// from being mistaken for numbered headings. Conservative on the side of
/// rejecting: missing a real heading is far cheaper than corrupting the stack.
fn looks_like_reference(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    // "et al" is an unambiguous citation marker.
    if lower.contains("et al") {
        return true;
    }
    // A 4-digit year (1xxx / 2xxx) is a citation signal; section titles have none.
    if title
        .split(|c: char| !c.is_ascii_digit())
        .any(|t| t.len() == 4 && matches!(t.as_bytes().first(), Some(b'1' | b'2')))
    {
        return true;
    }
    // Author lists carry several commas; genuine section titles essentially never do.
    if title.matches(',').count() >= 2 {
        return true;
    }
    // Two or more "Surname AB" pairs (a Capitalized word directly followed by a
    // short all-uppercase initials token) => an author list.
    let tokens: Vec<&str> = title.split_whitespace().collect();
    let mut pairs = 0usize;
    for w in tokens.windows(2) {
        let name = w[0].trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let initials = w[1].trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let name_ok = name.len() >= 2
            && name
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_uppercase())
            && name.chars().skip(1).any(|c| c.is_ascii_lowercase());
        let initials_ok =
            (1..=3).contains(&initials.len()) && initials.chars().all(|c| c.is_ascii_uppercase());
        if name_ok && initials_ok {
            pairs += 1;
        }
    }
    pairs >= 2
}

fn push_heading(headings: &mut Vec<String>, level: usize, title: &str) {
    while headings.len() >= level {
        headings.pop();
    }
    while headings.len() + 1 < level {
        headings.push(String::new());
    }
    headings.push(title.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_split_basic() {
        let chunks = naive_split("First paragraph.\n\nSecond paragraph.\n\n\nThird.");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "First paragraph.");
        assert_eq!(chunks[2].position, 2);
    }

    #[test]
    fn section_aware_tracks_atx_headings() {
        let md = "# Methods\n\n## Cell Culture\n\nHeLa cells were grown in DMEM.\n\n## mTOR Assay\n\nKinase activity measured by ADP-Glo.\n";
        let chunks = section_aware_split(md);
        assert!(!chunks.is_empty());
        let cc = chunks
            .iter()
            .find(|c| c.text.contains("HeLa"))
            .expect("finds HeLa chunk");
        assert_eq!(cc.section.as_deref(), Some("Methods > Cell Culture"));
        let mt = chunks
            .iter()
            .find(|c| c.text.contains("Kinase"))
            .expect("finds kinase chunk");
        assert_eq!(mt.section.as_deref(), Some("Methods > mTOR Assay"));
    }

    #[test]
    fn section_aware_handles_setext_headings() {
        // In CommonMark setext is H1 (===) and H2 (---). H2 nests under H1.
        let md = "Introduction\n============\n\nAging is a complex process.\n\nResults\n============\n\nWe observed lifespan extension.";
        let chunks = section_aware_split(md);
        let intro = chunks
            .iter()
            .find(|c| c.text.contains("complex process"))
            .unwrap();
        assert_eq!(intro.section.as_deref(), Some("Introduction"));
        let results = chunks
            .iter()
            .find(|c| c.text.contains("lifespan extension"))
            .unwrap();
        // Both are H1 so the later one replaces the earlier one in the heading stack.
        assert_eq!(results.section.as_deref(), Some("Results"));
    }

    #[test]
    fn section_aware_handles_numbered_sections() {
        let text = "1 Introduction\n\nThis study examines rapamycin.\n\n2 Methods\n\n2.1 Animals\n\nMice were housed at 22°C.";
        let chunks = section_aware_split(text);
        let mice = chunks.iter().find(|c| c.text.contains("Mice"));
        assert!(mice.is_some());
        // Sections stored in chunk metadata — exact nesting depends on depth heuristic.
        assert!(mice.unwrap().section.is_some());
    }

    #[test]
    fn section_aware_detects_unnumbered_headings() {
        // A lone "Methods"/"Results" with no leading digit must still become a
        // breadcrumb on the prose beneath it.
        let text = "Methods\n\nHeLa cells were grown in DMEM.\n\nResults\n\nWe observed lifespan extension.";
        let chunks = section_aware_split(text);
        let m = chunks
            .iter()
            .find(|c| c.text.contains("HeLa"))
            .expect("finds methods body");
        assert_eq!(m.section.as_deref(), Some("Methods"));
        let r = chunks
            .iter()
            .find(|c| c.text.contains("lifespan"))
            .expect("finds results body");
        assert_eq!(r.section.as_deref(), Some("Results"));
    }

    #[test]
    fn unnumbered_multiword_heading_detected() {
        assert_eq!(
            parse_unnumbered_heading("Materials and Methods"),
            Some("Materials and Methods".to_string())
        );
        assert_eq!(
            parse_unnumbered_heading("REFERENCES"),
            Some("REFERENCES".to_string())
        );
        assert_eq!(
            parse_unnumbered_heading("Methods:"),
            Some("Methods".to_string())
        );
        // A real sentence that merely starts with a section word is not a heading.
        assert_eq!(
            parse_unnumbered_heading("Results were significant here."),
            None
        );
    }

    #[test]
    fn reference_line_rejected_as_numbered_heading() {
        // The bug: this parsed as a heading "Smith AB, Jones CD, ..." and poisoned
        // every following chunk's breadcrumb.
        assert!(parse_numbered_heading("1 Smith AB, Jones CD, Brown E. Nature 2020").is_none());
        assert!(parse_numbered_heading("12 Doe J, Roe R. et al. Cell 2019;5:1-10").is_none());
        // Genuine numbered headings still parse.
        assert!(parse_numbered_heading("2 Methods").is_some());
        assert!(parse_numbered_heading("2.1 Cell Culture").is_some());
        assert!(parse_numbered_heading("3 Results").is_some());
    }

    #[test]
    fn reference_line_does_not_corrupt_breadcrumbs() {
        let text = "Results\n\nWe saw an effect.\n\n1 Smith AB, Jones CD, Brown E. Nature 2020";
        let chunks = section_aware_split(text);
        let refc = chunks
            .iter()
            .find(|c| c.text.contains("Smith AB"))
            .expect("reference kept as body");
        // Stays under the real heading, not promoted into its own section.
        assert_eq!(refc.section.as_deref(), Some("Results"));
    }

    #[test]
    fn merge_combines_small_same_section_chunks() {
        let chunks = vec![
            PendingChunk {
                text: "a".repeat(10),
                position: 0,
                section: Some("Intro".into()),
            },
            PendingChunk {
                text: "b".repeat(10),
                position: 1,
                section: Some("Intro".into()),
            },
            PendingChunk {
                text: "c".repeat(10),
                position: 2,
                section: Some("Intro".into()),
            },
        ];
        let merged = merge_small_chunks(chunks, 1000);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].position, 0);
        assert!(merged[0].text.contains(&"a".repeat(10)));
        assert!(merged[0].text.contains(&"c".repeat(10)));
    }

    #[test]
    fn merge_respects_section_boundaries() {
        let chunks = vec![
            PendingChunk {
                text: "intro".into(),
                position: 0,
                section: Some("Intro".into()),
            },
            PendingChunk {
                text: "methods".into(),
                position: 1,
                section: Some("Methods".into()),
            },
        ];
        let merged = merge_small_chunks(chunks, 1000);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].section.as_deref(), Some("Intro"));
        assert_eq!(merged[1].section.as_deref(), Some("Methods"));
        assert_eq!(merged[1].position, 1);
    }

    #[test]
    fn merge_passes_large_chunks_through_and_renumbers() {
        let chunks = vec![
            PendingChunk {
                text: "x".repeat(50),
                position: 0,
                section: None,
            },
            PendingChunk {
                text: "y".repeat(50),
                position: 1,
                section: None,
            },
        ];
        // target smaller than each chunk -> no merge, positions stay contiguous.
        let merged = merge_small_chunks(chunks, 10);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].position, 0);
        assert_eq!(merged[1].position, 1);
    }
}
