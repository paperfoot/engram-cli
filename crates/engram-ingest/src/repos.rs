//! Repository ingestion -- split source and docs around symbols/headings.

use crate::chunker::{section_aware_split, PendingChunk};

const MAX_LINES: usize = 90;

pub fn chunk_repo_text(text: &str) -> Vec<PendingChunk> {
    if looks_like_markdown(text) {
        return section_aware_split(text);
    }

    let mut out = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut current_section: Option<String> = None;
    let mut position = 0u32;

    for (line_no, line) in text.lines().enumerate() {
        if let Some(section) = parse_symbol_heading(line) {
            flush(
                &mut out,
                &mut buf,
                &mut position,
                current_section.as_deref(),
            );
            current_section = Some(format!("{}:{}", section, line_no + 1));
        } else if buf.len() >= MAX_LINES && line.trim().is_empty() {
            flush(
                &mut out,
                &mut buf,
                &mut position,
                current_section.as_deref(),
            );
        }
        buf.push(line);
    }
    flush(
        &mut out,
        &mut buf,
        &mut position,
        current_section.as_deref(),
    );

    if out.is_empty() {
        section_aware_split(text)
    } else {
        out
    }
}

fn looks_like_markdown(text: &str) -> bool {
    text.lines()
        .take(80)
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("```")
        })
        .count()
        >= 2
}

fn parse_symbol_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let candidates = [
        ("pub async fn ", "fn"),
        ("pub fn ", "fn"),
        ("async fn ", "fn"),
        ("fn ", "fn"),
        ("pub struct ", "struct"),
        ("struct ", "struct"),
        ("pub enum ", "enum"),
        ("enum ", "enum"),
        ("pub trait ", "trait"),
        ("trait ", "trait"),
        ("impl ", "impl"),
        ("class ", "class"),
        ("def ", "def"),
        ("async def ", "def"),
        ("function ", "function"),
        ("export function ", "function"),
        ("export class ", "class"),
        ("interface ", "interface"),
        ("export interface ", "interface"),
        ("const ", "const"),
        ("export const ", "const"),
    ];
    for (prefix, kind) in candidates {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name = rest
                .split(|c: char| {
                    c == '(' || c == '<' || c == '{' || c == ':' || c == '=' || c.is_whitespace()
                })
                .find(|s| !s.is_empty())?;
            return Some(format!("{kind} {name}"));
        }
    }
    None
}

fn flush(
    out: &mut Vec<PendingChunk>,
    buf: &mut Vec<&str>,
    position: &mut u32,
    section: Option<&str>,
) {
    let text = buf.join("\n");
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        out.push(PendingChunk {
            text: trimmed.to_string(),
            position: *position,
            section: section.map(str::to_string),
        });
        *position += 1;
    }
    buf.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_chunks_rust_symbols() {
        let src = "pub struct Store {\n    id: u64,\n}\n\nimpl Store {\n    pub fn open() {}\n}\n\nfn helper() {}\n";
        let chunks = chunk_repo_text(src);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().any(|chunk| chunk
            .section
            .as_deref()
            .unwrap_or("")
            .starts_with("struct Store")));
        assert!(chunks.iter().any(|chunk| chunk
            .section
            .as_deref()
            .unwrap_or("")
            .starts_with("fn helper")));
    }

    #[test]
    fn repo_uses_markdown_sections_for_readmes() {
        let md = "# CLI\n\n## Install\n\nRun cargo install.\n\n## Usage\n\nRun engram.";
        let chunks = chunk_repo_text(md);
        assert!(chunks
            .iter()
            .any(|chunk| chunk.section.as_deref() == Some("CLI > Install")));
    }
}
