//! Target documentation as evidence (ADR-0065, SPEC-18, AC-0676, AC-0678).
//!
//! A Markdown or plain-text file of a registered source is one `Document`
//! fact and each of its sections one `DocSection` fact. They state that the
//! file and the section exist at the commit, nothing more: section text is
//! not stored, and a title or heading is redacted before it is.
//!
//! An exact reference in a section (an inline code span or a link target)
//! is linked to the one fact it names by a `MENTIONS` edge. A reference
//! that names no fact, or more than one, is not linked and is no finding.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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
    /// Inline code spans and link targets, in file and offset order. They
    /// are not facts: [`link_references`] links the ones that name a fact.
    pub references: Vec<DocumentReference>,
}

/// One inline code span or link target of a Markdown section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentReference {
    /// The id of the innermost section that holds it.
    pub section: String,
    /// The document's repo-relative path.
    pub path: String,
    /// The text as written: the code span's content or the link's target.
    pub text: String,
    /// True for a link target, false for a code span.
    pub link: bool,
    /// Byte span of `text` in the file.
    pub start: usize,
    /// End of that span.
    pub end: usize,
}

/// The rule by which a reference names exactly one fact, in the order the
/// rules are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRule {
    /// The text is the id of a fact.
    FactId,
    /// The text or link target is the path of a `File` fact of the same
    /// repository.
    FilePath,
    /// The text is a method and a path that one `Endpoint` serves.
    Route,
    /// The text is the `name` of exactly one fact of a named kind.
    Name,
}

impl ReferenceRule {
    /// The closed rule code stored on the edge.
    pub fn code(self) -> &'static str {
        match self {
            Self::FactId => "fact_id",
            Self::FilePath => "file_path",
            Self::Route => "route",
            Self::Name => "name",
        }
    }
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

/// The lines of a Markdown file outside fenced code, each with its byte
/// offset and without its line ending. Fence lines are left out.
fn unfenced(text: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
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
            (None, None) => lines.push((offset, body)),
        }
        offset += line.len();
    }
    lines
}

/// The inline code spans of one line: content, start and end within it.
/// A span opens with a run of backticks and closes with the next run of the
/// same length; surrounding spaces are not part of the content.
fn code_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let run_at = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|byte| **byte == b'`')
            .count()
    };
    let mut spans = Vec::new();
    let mut cursor = 0;
    while let Some(open) = line[cursor..].find('`').map(|found| cursor + found) {
        let run = run_at(open);
        let mut search = open + run;
        let mut close = None;
        while let Some(next) = line[search..].find('`').map(|found| search + found) {
            let length = run_at(next);
            if length == run {
                close = Some(next);
                break;
            }
            search = next + length;
        }
        let Some(close) = close else {
            cursor = open + run;
            continue;
        };
        let inner = &line[open + run..close];
        let start = open + run + (inner.len() - inner.trim_start().len());
        let end = start + inner.trim().len();
        if end > start {
            spans.push((start, end));
        }
        cursor = close + run;
    }
    spans
}

/// The inline link and image targets of one line outside `code`: start and
/// end within it. A title after the target and angle brackets around it are
/// not part of the target.
fn link_targets(line: &str, code: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let in_code = |at: usize| code.iter().any(|(start, end)| (*start..*end).contains(&at));
    let mut targets = Vec::new();
    let mut cursor = 0;
    while let Some(open) = line[cursor..].find("](").map(|found| cursor + found) {
        cursor = open + 2;
        if in_code(open) || !line[..open].contains('[') {
            continue;
        }
        let rest = &line[cursor..];
        let Some(close) = rest.find(')') else {
            break;
        };
        let inner = &rest[..close];
        let lead = inner.len() - inner.trim_start().len();
        let target = inner.trim_start();
        let target = target.split(char::is_whitespace).next().unwrap_or("");
        let (skip, target) = match target.strip_prefix('<') {
            Some(bracketed) => (1, bracketed.trim_end_matches('>')),
            None => (0, target),
        };
        if !target.is_empty() {
            let start = cursor + lead + skip;
            targets.push((start, start + target.len()));
        }
        cursor += close + 1;
    }
    targets
}

/// The inline code spans and link targets of a Markdown file outside fenced
/// code, in offset order: text, whether it is a link target, and byte span.
pub(crate) fn markdown_references(text: &str) -> Vec<(String, bool, usize, usize)> {
    let mut found = Vec::new();
    for (offset, line) in unfenced(text) {
        let code = code_spans(line);
        for (start, end) in &code {
            found.push((
                line[*start..*end].to_string(),
                false,
                offset + start,
                offset + end,
            ));
        }
        for (start, end) in link_targets(line, &code) {
            found.push((
                line[start..end].to_string(),
                true,
                offset + start,
                offset + end,
            ));
        }
    }
    found.sort_by_key(|reference| (reference.2, reference.3));
    found
}

/// The sections of a Markdown file, in file order. A section runs from its
/// heading line to the next heading of the same or a higher level. Headings
/// inside fenced code are not structure.
pub(crate) fn markdown_sections(text: &str) -> Vec<Section> {
    let headings: Vec<(u8, String, usize)> = unfenced(text)
        .into_iter()
        .filter_map(|(offset, line)| atx(line).map(|(level, heading)| (level, heading, offset)))
        .collect();
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
        if format == Format::Markdown {
            for (reference, link, start, end) in markdown_references(&text) {
                // The innermost kept section that holds the reference.
                let Some(ordinal) = found
                    .iter()
                    .rposition(|section| (section.start..section.end).contains(&start))
                else {
                    continue;
                };
                facts.references.push(DocumentReference {
                    section: section_id(repo, path, ordinal),
                    path: path.to_string(),
                    text: reference,
                    link,
                    start,
                    end,
                });
            }
        }
    }
    facts.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    facts.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    Ok(facts)
}

/// Labels a reference is never linked to: findings, and documentation
/// itself.
const UNLINKED_LABELS: [&str; 4] = ["Gap", "Drift", "Document", "DocSection"];
/// Labels whose `name` a reference may equal.
const NAMED_LABELS: [&str; 5] = ["Resource", "Channel", "DataEntity", "Service", "Workload"];
/// Methods a route reference may start with.
const ROUTE_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
/// Reference spans one `MENTIONS` edge cites.
const MAX_MENTION_EVIDENCE: usize = 32;

/// The facts a reference can name, read once from the graph.
#[derive(Debug, Default)]
pub struct ReferenceIndex<'a> {
    /// Label by fact id, without findings and documentation.
    labels: BTreeMap<&'a str, &'a str>,
    /// Endpoint ids by canonical `METHOD /path`.
    routes: BTreeMap<String, Vec<&'a str>>,
    /// Fact ids by `name`, for the named kinds.
    names: BTreeMap<&'a str, Vec<&'a str>>,
}

impl<'a> ReferenceIndex<'a> {
    /// Index the facts of the whole system.
    pub fn new(nodes: &'a [Node]) -> Self {
        let mut index = Self::default();
        for node in nodes {
            let label = node.label.as_str();
            if UNLINKED_LABELS.contains(&label) {
                continue;
            }
            index.labels.insert(&node.id, label);
            if label == "Endpoint"
                && node.props["protocol"] != "graphql"
                && let (Some(method), Some(path)) =
                    (node.props["method"].as_str(), node.props["path"].as_str())
                && !method.trim().is_empty()
                && !path.trim().is_empty()
            {
                index
                    .routes
                    .entry(crate::contract::canonical_endpoint(method, path))
                    .or_default()
                    .push(&node.id);
            }
            if NAMED_LABELS.contains(&label)
                && let Some(name) = node.props["name"].as_str()
            {
                index.names.entry(name).or_default().push(&node.id);
            }
        }
        index
    }

    /// The one fact `reference` names, with the first rule that names it.
    fn resolve(
        &self,
        repo: &str,
        reference: &DocumentReference,
    ) -> Option<(ReferenceRule, &'a str)> {
        let text = reference.text.as_str();
        let file = |path: String| {
            self.labels
                .get_key_value(format!("file:{repo}@{path}").as_str())
                .filter(|(_, label)| **label == "File")
                .map(|(id, _)| (ReferenceRule::FilePath, *id))
        };
        if reference.link {
            return file(link_path(&reference.path, text)?);
        }
        if let Some((id, _)) = self.labels.get_key_value(text) {
            return Some((ReferenceRule::FactId, *id));
        }
        if let Some(found) = code_path(text).and_then(file) {
            return Some(found);
        }
        if let Some(route) = route_key(text) {
            return match self.routes.get(&route).map(Vec::as_slice) {
                Some([id]) => Some((ReferenceRule::Route, *id)),
                _ => None,
            };
        }
        match self.names.get(text).map(Vec::as_slice) {
            Some([id]) => Some((ReferenceRule::Name, *id)),
            _ => None,
        }
    }
}

/// A path without its fragment and line suffix (`#L10`, `:10`, `:10-12`).
fn without_position(path: &str) -> &str {
    let path = path.split(['#', '?']).next().unwrap_or(path);
    match path.rsplit_once(':') {
        Some((before, line))
            if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit() || c == '-') =>
        {
            before
        }
        _ => path,
    }
}

/// The repo-relative path a code span names, when it is shaped like one.
fn code_path(text: &str) -> Option<String> {
    let path = without_position(text);
    let path = path.strip_prefix("./").unwrap_or(path);
    (!path.is_empty()
        && !path.starts_with('/')
        && !path.contains("://")
        && !path.contains(char::is_whitespace))
    .then(|| path.to_string())
}

/// The repo-relative path a link target names, resolved against the
/// directory of the document at `document`. `None` for an absolute URL, a
/// bare fragment, or a target that leaves the repository.
fn link_path(document: &str, target: &str) -> Option<String> {
    if target.contains("://") || target.starts_with("mailto:") || target.starts_with("//") {
        return None;
    }
    let target = without_position(target);
    if target.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if !target.starts_with('/') {
        parts.extend(document.split('/'));
        parts.pop();
    }
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The canonical `METHOD /path` a code span names, when it is a method and
/// a path. A query string is not part of the path.
fn route_key(text: &str) -> Option<String> {
    let (method, path) = text.split_once(char::is_whitespace)?;
    let path = path.trim();
    if !ROUTE_METHODS.contains(&method)
        || !path.starts_with('/')
        || path.contains(char::is_whitespace)
    {
        return None;
    }
    let path = path.split('?').next().unwrap_or(path);
    Some(crate::contract::canonical_endpoint(method, path))
}

/// `MENTIONS` edges from each section to the one fact a reference in it
/// names (AC-0678). References of a section that name the same fact are one
/// edge citing each span, with the first rule and text. A reference that
/// names no fact, or more than one, yields nothing.
pub fn link_references(
    repo: &str,
    commit: &str,
    references: &[DocumentReference],
    index: &ReferenceIndex<'_>,
) -> Vec<Edge> {
    type Mention<'r> = (
        ReferenceRule,
        &'r DocumentReference,
        BTreeSet<(usize, usize)>,
    );
    let mut mentions: BTreeMap<(&str, &str), Mention<'_>> = BTreeMap::new();
    for reference in references {
        let Some((rule, target)) = index.resolve(repo, reference) else {
            continue;
        };
        mentions
            .entry((reference.section.as_str(), target))
            .or_insert_with(|| (rule, reference, BTreeSet::new()))
            .2
            .insert((reference.start, reference.end));
    }
    mentions
        .into_iter()
        .map(|((section, target), (rule, first, spans))| {
            let cited: Vec<EvidenceRef> = spans
                .iter()
                .take(MAX_MENTION_EVIDENCE)
                .map(|(start, end)| evidence(repo, &first.path, commit, *start, *end))
                .collect();
            let prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                cited,
                DOCUMENT_EXTRACTOR_ID,
                &hashed(&(section, "MENTIONS", target, rule.code())),
            )
            .expect("Confirmed is within the deterministic confidence ceiling")
            .with_method(crate::spec_rule(DOCUMENT_EXTRACTOR_ID));
            Edge {
                src: section.to_string(),
                dst: target.to_string(),
                label: "MENTIONS".into(),
                props: serde_json::json!({
                    "reference": core_redact::redact_text(&first.text).0,
                    "rule": rule.code(),
                    "occurrences": spans.len(),
                    "prov": prov,
                }),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod tests;
