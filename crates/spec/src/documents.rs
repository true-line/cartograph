//! Target documentation as evidence (ADR-0065, SPEC-18 slice 1, AC-0676).
//!
//! A Markdown or plain-text file of a registered source is one `Document`
//! fact and each of its sections one `DocSection` fact. They state that the
//! file and the section exist at the commit, nothing more: section text is
//! not stored, and a title or heading is redacted before it is.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Deterministic extractor id for documentation files.
pub const DOCUMENT_EXTRACTOR_ID: &str = "t0.documentation";
/// A file larger than this is counted and not read.
pub const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;
/// Files read per repository, taken in path order.
pub const MAX_DOCUMENTS_PER_REPO: usize = 2_000;
/// Sections kept per file, in file order.
pub const MAX_SECTIONS_PER_DOCUMENT: usize = 512;

/// Why a documentation file was found and not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentSkip {
    /// Larger than [`MAX_DOCUMENT_BYTES`].
    TooLarge,
    /// Not valid UTF-8.
    NotUtf8,
    /// Past [`MAX_DOCUMENTS_PER_REPO`].
    OverFileLimit,
}

impl DocumentSkip {
    /// The closed reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::NotUtf8 => "not_utf8",
            Self::OverFileLimit => "over_file_limit",
        }
    }
}

/// What the reader found in one repository.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentCounts {
    /// Files with a documentation extension.
    pub found: u64,
    /// Files read into a `Document` fact.
    pub read: u64,
    /// Files not read, by reason code.
    pub skipped: BTreeMap<&'static str, u64>,
    /// `DocSection` facts.
    pub sections: u64,
    /// Files read to the section bound, with sections left out.
    pub sections_truncated: u64,
}

/// The documentation facts of one repository.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentFacts {
    /// `Document` and `DocSection` nodes, sorted by id.
    pub nodes: Vec<Node>,
    /// `HAS_SECTION` edges, sorted.
    pub edges: Vec<Edge>,
    /// Files found, read and skipped.
    pub counts: DocumentCounts,
}

/// A section of a document: byte span, heading and level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// 0 for text before the first heading and for a plain-text file.
    pub level: u8,
    pub heading: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Markdown,
    Text,
}

impl Format {
    fn of(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.ends_with(".md") || name.ends_with(".markdown") {
            Some(Self::Markdown)
        } else if name.ends_with(".txt") {
            Some(Self::Text)
        } else {
            None
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
        }
    }
}

/// The fence a line opens: its character and length. A backtick fence's
/// info string cannot contain a backtick, so a line such as
/// ```` ```code``` ```` is inline code, not a fence.
fn fence(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = rest.chars().take_while(|c| *c == mark).count();
    if mark == '`' && rest[run..].contains('`') {
        return None;
    }
    (run >= 3).then_some((mark, run))
}

/// An ATX heading: its level and text.
fn atx(line: &str) -> Option<(u8, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = &rest[level..];
    if !(text.is_empty() || text.starts_with([' ', '\t'])) {
        return None;
    }
    let text = text.trim();
    // A closing run of `#` is not part of the heading.
    let stripped = text.trim_end_matches('#');
    let text = if stripped.is_empty() || stripped.ends_with([' ', '\t']) {
        stripped.trim_end()
    } else {
        text
    };
    Some((level as u8, text.to_string()))
}

/// The sections of a Markdown file, in file order. A section runs from its
/// heading line to the next heading of the same or a higher level. Headings
/// inside fenced code are not structure.
pub(crate) fn markdown_sections(text: &str) -> Vec<Section> {
    let mut headings: Vec<(u8, String, usize)> = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        match (open, fence(body)) {
            (Some((mark, run)), Some((closing, length)))
                if closing == mark
                    && length >= run
                    && body.trim_start_matches(' ')[length..].trim().is_empty() =>
            {
                open = None;
            }
            (Some(_), _) => {}
            (None, Some(opened)) => open = Some(opened),
            (None, None) => {
                if let Some((level, heading)) = atx(body) {
                    headings.push((level, heading, offset));
                }
            }
        }
        offset += line.len();
    }
    let mut sections = Vec::new();
    let first = headings.first().map_or(text.len(), |heading| heading.2);
    if !text[..first].trim().is_empty() {
        sections.push(Section {
            level: 0,
            heading: String::new(),
            start: 0,
            end: first,
        });
    }
    for (index, (level, heading, start)) in headings.iter().enumerate() {
        let end = headings[index + 1..]
            .iter()
            .find(|next| next.0 <= *level)
            .map_or(text.len(), |next| next.2);
        sections.push(Section {
            level: *level,
            heading: heading.clone(),
            start: *start,
            end,
        });
    }
    sections
}

fn sections(format: Format, text: &str) -> Vec<Section> {
    match format {
        Format::Markdown => markdown_sections(text),
        Format::Text if text.trim().is_empty() => Vec::new(),
        Format::Text => vec![Section {
            level: 0,
            heading: String::new(),
            start: 0,
            end: text.len(),
        }],
    }
}

fn evidence(repo: &str, path: &str, commit: &str, start: usize, end: usize) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: start as u64,
        byte_end: end as u64,
        commit_sha: commit.into(),
    }
}

fn provenance(evidence: EvidenceRef, fact: &[u8]) -> Provenance {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![evidence],
        DOCUMENT_EXTRACTOR_ID,
        fact,
    )
    .expect("Confirmed is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(DOCUMENT_EXTRACTOR_ID))
}

/// The id of the document at `path` in `repo`.
pub fn document_id(repo: &str, path: &str) -> String {
    format!("doc:{repo}@{path}")
}

/// The id of section `ordinal` of the document at `path` in `repo`.
pub fn section_id(repo: &str, path: &str, ordinal: usize) -> String {
    format!("docsec:{repo}@{path}#{ordinal}")
}

fn hashed(parts: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(parts).expect("fact identity serializes")
}

/// Read the Markdown and plain-text files under `root` as `Document` and
/// `DocSection` facts with `HAS_SECTION` edges.
///
/// The walk honours ignore files and skips dot directories and build output.
/// Every file with a documentation extension is counted as read or as skipped
/// with one reason. The same tree yields the same facts in the same order.
pub fn extract_documents(root: &Path, repo: &str, commit: &str) -> std::io::Result<DocumentFacts> {
    let skip = |name: &str| {
        let name = name.to_ascii_lowercase();
        name.starts_with('.')
            || matches!(
                name.as_str(),
                "node_modules" | "target" | "dist" | "build" | "vendor"
            )
    };
    let mut facts = DocumentFacts::default();
    let skipped = |facts: &mut DocumentFacts, reason: DocumentSkip| {
        *facts.counts.skipped.entry(reason.code()).or_default() += 1;
    };
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        let Some(format) = Format::of(&file.name) else {
            continue;
        };
        facts.counts.found += 1;
        if facts.counts.read as usize >= MAX_DOCUMENTS_PER_REPO {
            skipped(&mut facts, DocumentSkip::OverFileLimit);
            continue;
        }
        if file.path.metadata()?.len() > MAX_DOCUMENT_BYTES {
            skipped(&mut facts, DocumentSkip::TooLarge);
            continue;
        }
        let Ok(text) = String::from_utf8(std::fs::read(&file.path)?) else {
            skipped(&mut facts, DocumentSkip::NotUtf8);
            continue;
        };
        facts.counts.read += 1;
        let mut found = sections(format, &text);
        if found.len() > MAX_SECTIONS_PER_DOCUMENT {
            found.truncate(MAX_SECTIONS_PER_DOCUMENT);
            facts.counts.sections_truncated += 1;
        }
        facts.counts.sections += found.len() as u64;
        let path = file.rel.as_str();
        let id = document_id(repo, path);
        let title = found
            .iter()
            .find(|section| section.level == 1 && !section.heading.is_empty())
            .map_or(file.name.as_str(), |section| section.heading.as_str());
        facts.nodes.push(Node {
            id: id.clone(),
            label: "Document".into(),
            props: serde_json::json!({
                "path": path,
                "format": format.code(),
                "title": core_redact::redact_text(title).0,
                "sections": found.len(),
                "bytes": text.len(),
                "prov": provenance(
                    evidence(repo, path, commit, 0, text.len()),
                    &hashed(&(&id, &text)),
                ),
            }),
        });
        for (ordinal, section) in found.iter().enumerate() {
            let section_id = section_id(repo, path, ordinal);
            let span = evidence(repo, path, commit, section.start, section.end);
            facts.nodes.push(Node {
                id: section_id.clone(),
                label: "DocSection".into(),
                props: serde_json::json!({
                    "path": path,
                    "ordinal": ordinal,
                    "level": section.level,
                    "heading": core_redact::redact_text(&section.heading).0,
                    "prov": provenance(
                        span.clone(),
                        &hashed(&(&section_id, &text[section.start..section.end])),
                    ),
                }),
            });
            facts.edges.push(Edge {
                src: id.clone(),
                dst: section_id.clone(),
                label: "HAS_SECTION".into(),
                props: serde_json::json!({
                    "prov": provenance(span, format!("{id} HAS_SECTION {section_id}").as_bytes()),
                }),
            });
        }
    }
    facts.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    facts.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    Ok(facts)
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod tests;
