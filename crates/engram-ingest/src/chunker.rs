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

/// Hard floor on what may become its own retrievable chunk.
///
/// THE BUG THIS FIXES. A paragraph split over real-world notes and exported
/// documents emits fragments — a bare `Sep 16, 2025` date header, a lone
/// `Elias:` speaker label, a trailing `Thanks!`. Each one became a standalone
/// embedded chunk. A 12-character chunk is nearly pure signal for whatever few
/// tokens it contains, so it scores 1.0 against any query mentioning those
/// tokens and outranks the paragraph that actually answers the question. A live
/// store ended up with 986 sub-40-character chunks doing exactly that.
///
/// So: any chunk shorter than `min_chars` is merged into a neighbour instead of
/// standing alone. Preference order, chosen so a breadcrumb is disturbed only
/// when there is no alternative:
///   1. the previous emitted chunk, when it carries the same section;
///   2. the next chunk (the fragment is carried forward and prepended) — this
///      is the date-header case, where the header belongs to what follows;
///   3. the previous emitted chunk regardless of section (end of input);
///   4. nothing — a memory that is *entirely* this short is legitimate
///      ("Lira's birthday is 12 March" is 27 characters) and is kept as-is.
///
/// Positions are renumbered sequentially. Merging can push a chunk up to
/// `min_chars` past a previously enforced maximum; that slack is deliberate and
/// tiny next to the embedder's own input window.
pub fn enforce_min_chunk_chars(chunks: Vec<PendingChunk>, min_chars: usize) -> Vec<PendingChunk> {
    if chunks.len() <= 1 || min_chars == 0 {
        return renumber(chunks);
    }
    let mut out: Vec<PendingChunk> = Vec::with_capacity(chunks.len());
    // A fragment with no usable predecessor waits here for the next chunk.
    let mut carry: Option<PendingChunk> = None;

    for chunk in chunks {
        let mut chunk = match carry.take() {
            None => chunk,
            Some(mut pending) => {
                pending.text.push_str("\n\n");
                pending.text.push_str(&chunk.text);
                // The receiving chunk's breadcrumb wins: the fragment was a
                // header or stray line belonging to the body below it.
                pending.section = chunk.section;
                pending
            }
        };
        if chunk.text.len() >= min_chars {
            out.push(chunk);
            continue;
        }
        match out.last_mut() {
            Some(prev) if prev.section == chunk.section => {
                prev.text.push_str("\n\n");
                prev.text.push_str(&chunk.text);
            }
            _ => {
                chunk.position = 0;
                carry = Some(chunk);
            }
        }
    }

    if let Some(mut leftover) = carry {
        match out.last_mut() {
            // Trailing fragment with no same-section home: attach it anyway.
            // A blurred breadcrumb beats a standalone date-stamp in recall.
            Some(prev) => {
                prev.text.push_str("\n\n");
                prev.text.push_str(&leftover.text);
            }
            // Everything we were given was sub-threshold: this is a short
            // memory, not a fragment. Keep it.
            None => {
                leftover.position = 0;
                out.push(leftover);
            }
        }
    }
    renumber(out)
}

/// Enforce `max_chars` with no escape hatch.
///
/// [`split_oversized`] cuts at sentence boundaries and deliberately emits an
/// over-cap lone sentence whole rather than severing a numeric fact. That is
/// the right trade for papers, but a personal note is often one unbroken run of
/// text with no sentence boundary at all — and the embedder silently truncates
/// anything past its input window, so "emit whole" there means "lose the tail
/// forever". This adds the fallback: a piece still over the cap after sentence
/// packing is wrapped at whitespace (never mid-word unless a single token is
/// itself over the cap).
pub fn enforce_max_chunk_chars(chunks: Vec<PendingChunk>, max_chars: usize) -> Vec<PendingChunk> {
    if max_chars == 0 {
        return renumber(chunks);
    }
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        for piece in pack_sentences(&chunk.text, max_chars) {
            for part in hard_wrap(&piece, max_chars) {
                out.push(PendingChunk {
                    text: part,
                    position: 0,
                    section: chunk.section.clone(),
                });
            }
        }
    }
    renumber(out)
}

/// Split `text` into pieces of at most `max_chars` bytes at whitespace
/// boundaries. Returns the input untouched when it already fits. A single token
/// longer than `max_chars` (a URL, a base64 blob) is cut at a char boundary —
/// losing it entirely to embedder truncation is strictly worse.
fn hard_wrap(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    for word in text.split_whitespace() {
        if !buf.is_empty() && buf.len() + 1 + word.len() > max_chars {
            out.push(std::mem::take(&mut buf));
        }
        if word.len() > max_chars {
            // Unbreakable token: flush what we have, then slice it.
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
            let mut rest = word;
            while rest.len() > max_chars {
                let mut cut = max_chars;
                while cut > 0 && !rest.is_char_boundary(cut) {
                    cut -= 1;
                }
                if cut == 0 {
                    break;
                }
                out.push(rest[..cut].to_string());
                rest = &rest[cut..];
            }
            buf.push_str(rest);
            continue;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(word);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    if out.is_empty() {
        out.push(text.to_string());
    }
    out
}

/// Renumber positions 0..n so downstream ordering stays contiguous after any
/// merge or split pass.
fn renumber(mut chunks: Vec<PendingChunk>) -> Vec<PendingChunk> {
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.position = i as u32;
    }
    chunks
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
    if !chars.peek().is_some_and(|c| c.is_ascii_digit()) {
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
        .filter(|w| w.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
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
            && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
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

    fn chunk(text: &str, section: Option<&str>) -> PendingChunk {
        PendingChunk {
            text: text.to_string(),
            position: 0,
            section: section.map(|s| s.to_string()),
        }
    }

    #[test]
    fn min_floor_merges_trailing_fragment_backwards() {
        let out = enforce_min_chunk_chars(
            vec![chunk(&"a".repeat(200), None), chunk("Sep 16, 2025", None)],
            60,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].text.ends_with("Sep 16, 2025"));
        assert_eq!(out[0].position, 0);
    }

    #[test]
    fn min_floor_carries_leading_fragment_forward() {
        // A date header at position 0 has no predecessor; it must attach to the
        // entry it introduces rather than becoming its own chunk.
        let out = enforce_min_chunk_chars(
            vec![chunk("Sep 16, 2025", None), chunk(&"b".repeat(200), None)],
            60,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].text.starts_with("Sep 16, 2025"));
    }

    #[test]
    fn min_floor_keeps_a_lone_short_chunk() {
        // A memory that is entirely short is legitimate, not a fragment.
        let out = enforce_min_chunk_chars(vec![chunk("Lira's birthday is 12 March", None)], 60);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Lira's birthday is 12 March");
    }

    #[test]
    fn min_floor_keeps_input_when_everything_is_short() {
        let out = enforce_min_chunk_chars(vec![chunk("hi", None), chunk("there", None)], 60);
        assert_eq!(out.len(), 1, "all-fragment input collapses to one chunk");
        assert!(out[0].text.contains("hi") && out[0].text.contains("there"));
    }

    #[test]
    fn min_floor_prefers_a_same_section_neighbour() {
        let out = enforce_min_chunk_chars(
            vec![
                chunk(&"a".repeat(200), Some("Intro")),
                chunk(&"b".repeat(200), Some("Methods")),
                chunk("tiny", Some("Methods")),
            ],
            60,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].section.as_deref(), Some("Intro"));
        assert!(out[1].text.ends_with("tiny"));
        assert_eq!(out[1].section.as_deref(), Some("Methods"));
    }

    #[test]
    fn min_floor_is_idempotent() {
        let once = enforce_min_chunk_chars(
            vec![
                chunk("Sep 16, 2025", None),
                chunk(&"a".repeat(200), None),
                chunk("ok", None),
            ],
            60,
        );
        let twice = enforce_min_chunk_chars(once.clone(), 60);
        let texts_once: Vec<&str> = once.iter().map(|c| c.text.as_str()).collect();
        let texts_twice: Vec<&str> = twice.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts_once, texts_twice);
    }

    #[test]
    fn max_cap_splits_text_with_no_sentence_boundaries() {
        // `split_oversized` alone emits this whole (no boundary to cut at), so
        // the embedder would truncate it. The hard wrap must kick in.
        let text = "word ".repeat(1000);
        let lenient = split_oversized(vec![chunk(&text, None)], 200);
        assert_eq!(lenient.len(), 1, "baseline: sentence packing cannot split");
        let strict = enforce_max_chunk_chars(vec![chunk(&text, None)], 200);
        assert!(strict.len() > 20);
        assert!(strict.iter().all(|c| c.text.len() <= 200));
        let joined: Vec<&str> = strict
            .iter()
            .flat_map(|c| c.text.split_whitespace())
            .collect();
        assert_eq!(joined.len(), 1000, "words lost while wrapping");
    }

    #[test]
    fn max_cap_slices_an_unbreakable_token() {
        let text = "x".repeat(500);
        let out = enforce_max_chunk_chars(vec![chunk(&text, None)], 100);
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(|c| c.text.len() <= 100));
        assert_eq!(
            out.iter().map(|c| c.text.len()).sum::<usize>(),
            500,
            "characters lost while slicing"
        );
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
