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
//!
//! Two reference forms assert something checkable: a route, and a path to
//! a file of the same repository. One that no longer matches is a `Drift`
//! fact citing the reference and what it was compared with (AC-0679).

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Deterministic extractor id for documentation files.
pub const DOCUMENT_EXTRACTOR_ID: &str = "t0.documentation";
/// Deterministic rule id for documentation drift.
pub const DOCUMENTATION_DRIFT_EXTRACTOR_ID: &str = "t0.documentation-drift";
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
    /// A declared OpenAPI reference (`.json`): a document with no sections.
    OpenApi,
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
            Self::OpenApi => "openapi",
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
/// end within it. A title after the target is not part of it. As in
/// CommonMark, a target in angle brackets runs to its `>`, and a bare target
/// runs to whitespace or the `)` that balances its parentheses, so
/// `[page](app/(shop)/page.tsx)` names `app/(shop)/page.tsx`.
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
        let lead = rest.len() - rest.trim_start().len();
        let Some((start, end)) = link_destination(&rest[lead..]) else {
            continue;
        };
        let (start, end) = (cursor + lead + start, cursor + lead + end);
        if start < end {
            targets.push((start, end));
        }
        // Past the destination and any title, to the link's closing `)`.
        cursor = line[end..]
            .find(')')
            .map_or(line.len(), |close| end + close + 1);
    }
    targets
}

/// The destination at the start of `rest`: start and end within it, without
/// angle brackets. `None` when it is not a well-formed destination.
fn link_destination(rest: &str) -> Option<(usize, usize)> {
    if let Some(bracketed) = rest.strip_prefix('<') {
        let mut escaped = false;
        for (at, ch) in bracketed.char_indices() {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '<' => return None,
                '>' => return Some((1, 1 + at)),
                _ => {}
            }
        }
        return None;
    }
    let mut depth = 0usize;
    let mut escaped = false;
    for (at, ch) in rest.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' if depth == 0 => return Some((0, at)),
            ')' => depth -= 1,
            ch if ch.is_whitespace() => return (depth == 0).then_some((0, at)),
            _ => {}
        }
    }
    None
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
        Format::OpenApi => Vec::new(),
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

/// The `content_hash` a `DocSection`'s provenance carries for `text`, its
/// section's bytes: a host that reads a section's span from the source
/// checks it against the graph before using the text (AC-1505).
pub fn section_content_hash(section_id: &str, text: &str) -> String {
    core_prov::content_hash(&hashed(&(section_id, text)))
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
    let cases = DocumentationCases {
        documents: true,
        api_references: &[],
        decisions: None,
    };
    read_documentation(root, repo, commit, &cases).map(|read| read.documents)
}

/// The role a document plays besides being documentation (ADR-0065
/// decision 6), stored on the `Document` as `roles`.
pub const ROLE_API_REFERENCE: &str = "api_reference";
/// A found decision record (ADR or RFC).
pub const ROLE_DECISION_RECORD: &str = "decision_record";

/// Which readers one documentation walk serves (ADR-0065 decision 6). The
/// document reader, the found-decision reader and the declared API
/// references are cases of one read: each file is found by one walk and its
/// bytes are read once, under one size bound.
pub struct DocumentationCases<'a> {
    /// Emit `Document` and `DocSection` facts and count them.
    pub documents: bool,
    /// The repository's declared API reference files, validated by their
    /// loader. Each is a document with the `api_reference` role, read from
    /// its validated contents when the walk does not reach it (a dot
    /// directory, or an OpenAPI `.json` file).
    pub api_references: &'a [crate::DeclaredApiDoc],
    /// Read found decision records as `ADR` facts, linking the ids for which
    /// this answers true.
    pub decisions: Option<&'a dyn Fn(&str) -> bool>,
}

/// What one documentation walk read.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Documentation {
    /// `Document` and `DocSection` facts, `HAS_SECTION` and `RECORDS` edges.
    pub documents: DocumentFacts,
    /// Found decision records.
    pub decisions: crate::FoundAdrRead,
}

/// Directories the documentation walk never enters: dot directories and
/// dependency or compiler output.
fn walk_skips(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "dist")
}

/// Whether a file at `rel` sits under build output that holds no documents
/// of the target's own (`build`, `vendor`). Decision records there are
/// still read, as they were before the readers were one walk.
fn under_build_output(rel: &str) -> bool {
    let mut directories = rel.split('/').collect::<Vec<_>>();
    directories.pop();
    directories
        .iter()
        .any(|part| matches!(part.to_ascii_lowercase().as_str(), "build" | "vendor"))
}

/// Read a repository's documentation in one walk and serve each case of
/// `cases` from it (ADR-0065 decision 6, AC-1600).
///
/// A Markdown or plain-text file is a `Document`; a Markdown file under a
/// decision directory, or named like a decision, is also a found decision
/// record, and its `Document` carries the `decision_record` role; when the
/// decision case runs, a `RECORDS` edge joins the two. A declared API
/// reference is a `Document` with the `api_reference` role. Facts, ids and
/// counts of each case equal what its reader yielded on its own.
pub fn read_documentation(
    root: &Path,
    repo: &str,
    commit: &str,
    cases: &DocumentationCases<'_>,
) -> std::io::Result<Documentation> {
    let files: Vec<String> = source_walk::files(root, &walk_skips, source_walk::Gitignores::Honor)?
        .into_iter()
        .map(|file| file.rel)
        .collect();
    let open = |rel: &str| -> std::io::Result<Opened> {
        let path = root.join(rel);
        if path.metadata()?.len() > MAX_DOCUMENT_BYTES {
            return Ok(Opened::TooLarge);
        }
        std::fs::read(path).map(Opened::Bytes)
    };
    read_listed(&files, &open, repo, commit, cases)
}

/// True when the documentation walk may open `rel`: a Markdown or
/// plain-text file outside the directories the walk never enters. A source
/// capture retains these files (AC-1065).
pub fn is_documentation_file(rel: &str) -> bool {
    let mut parts: Vec<&str> = rel.split('/').collect();
    let name = parts.pop().unwrap_or(rel);
    Format::of(name).is_some() && !parts.iter().any(|part| walk_skips(part))
}

/// The files [`read_documentation`] may open under `root`, in path order: a
/// source capture retains them (AC-1065).
pub fn documentation_paths(root: &Path) -> std::io::Result<Vec<String>> {
    Ok(
        source_walk::files(root, &walk_skips, source_walk::Gitignores::Honor)?
            .into_iter()
            .map(|file| file.rel)
            .filter(|rel| is_documentation_file(rel))
            .collect(),
    )
}

/// A capture's documentation files as the walk needs them (AC-1065): each
/// path with its bytes, or `None` for a file the walk never parses, so only
/// its path is kept: a file over [`MAX_DOCUMENT_BYTES`], or a document past
/// the first [`MAX_DOCUMENTS_PER_REPO`] readable ones that is not a decision
/// record.
pub type RetainedDocuments = BTreeMap<String, Option<Vec<u8>>>;

/// What [`read_documentation_from_capture`] needs of a capture's `files`
/// (path and bytes, in any order): its [`is_documentation_file`] paths,
/// with bytes only for the files the walk may parse, so what is kept is
/// bounded by the walk's own file and document bounds.
pub fn retained_documentation<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> RetainedDocuments {
    let sorted: BTreeMap<&str, &[u8]> = files
        .into_iter()
        .filter(|(rel, _)| is_documentation_file(rel))
        .collect();
    let mut documents = 0usize;
    sorted
        .into_iter()
        .map(|(rel, bytes)| {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            let fits = bytes.len() as u64 <= MAX_DOCUMENT_BYTES;
            let document = Format::of(name).is_some() && !under_build_output(rel);
            // Counted like the walk's read documents, never more, so a file
            // the walk may parse always keeps its bytes.
            let parsed = !document || documents < MAX_DOCUMENTS_PER_REPO;
            documents += usize::from(document && fits && std::str::from_utf8(bytes).is_ok());
            let kept = (fits && (parsed || crate::adr::is_decision_record(rel, name)))
                .then(|| bytes.to_vec());
            (rel.to_owned(), kept)
        })
        .collect()
}

/// [`read_documentation`] over a source capture (AC-1065, #385):
/// `captured` is what [`retained_documentation`] kept of it. Only its paths
/// are listed, so a document edited, deleted or created in the checkout
/// after capture changes nothing. Declared API references are still read
/// from the contents their loader validated.
pub fn read_documentation_from_capture(
    captured: &RetainedDocuments,
    repo: &str,
    commit: &str,
    cases: &DocumentationCases<'_>,
) -> std::io::Result<Documentation> {
    let files: Vec<String> = captured.keys().cloned().collect();
    let open = |rel: &str| -> std::io::Result<Opened> {
        match captured.get(rel) {
            Some(Some(bytes)) => Ok(Opened::Bytes(bytes.clone())),
            // Kept without bytes: the walk opens only an oversized one.
            Some(None) => Ok(Opened::TooLarge),
            None => Err(std::io::Error::other("document is not in the capture")),
        }
    };
    read_listed(&files, &open, repo, commit, cases)
}

/// A listed file's bytes, or that it is over [`MAX_DOCUMENT_BYTES`].
enum Opened {
    TooLarge,
    Bytes(Vec<u8>),
}

fn read_listed(
    files: &[String],
    open: &dyn Fn(&str) -> std::io::Result<Opened>,
    repo: &str,
    commit: &str,
    cases: &DocumentationCases<'_>,
) -> std::io::Result<Documentation> {
    let mut read = Documentation::default();
    let declared: BTreeMap<&str, &crate::DeclaredApiDoc> = cases
        .api_references
        .iter()
        .map(|doc| (doc.path.as_str(), doc))
        .collect();
    // Declared references the walk counted, read or not.
    let mut walked = BTreeSet::new();
    // Paths the decision case read, so a declared reference read after the
    // walk links to its `ADR` too.
    let mut decided = BTreeSet::new();
    let skipped = |facts: &mut DocumentFacts, reason: DocumentSkip| {
        *facts.counts.skipped.entry(reason.code()).or_default() += 1;
    };
    for path in files {
        let path = path.as_str();
        let name = path.rsplit('/').next().unwrap_or(path);
        let format = Format::of(name).filter(|_| cases.documents && !under_build_output(path));
        let decision = crate::adr::is_decision_record(path, name);
        let mut decision_case = cases.decisions.filter(|_| decision);
        if format.is_none() && decision_case.is_none() {
            continue;
        }
        let mut document = format;
        if document.is_some() {
            read.documents.counts.found += 1;
            if declared.contains_key(path) {
                walked.insert(path.to_string());
            }
            if read.documents.counts.read as usize >= MAX_DOCUMENTS_PER_REPO {
                skipped(&mut read.documents, DocumentSkip::OverFileLimit);
                document = None;
                if decision_case.is_none() {
                    continue;
                }
            }
        }
        // A declared reference is read from the contents its loader
        // validated, so the contract and every case of this walk see the
        // same bytes (AC-1601).
        let validated = declared.get(path).map(|doc| doc.contents.as_str());
        // One size bound for every case: both readers kept 1 MiB.
        let opened = match validated {
            Some(_) => None,
            None => Some(open(path)?),
        };
        if matches!(opened, Some(Opened::TooLarge)) {
            if document.is_some() {
                skipped(&mut read.documents, DocumentSkip::TooLarge);
            }
            if decision_case.is_some() {
                crate::adr::skip_found_adr(
                    &mut read.decisions,
                    path,
                    crate::UnreadAdrReason::TooLarge,
                );
            }
            continue;
        }
        // A project's raised API reference bound (AC-1804) widens only the
        // contract and document cases: a declared reference that is also a
        // decision record keeps the decision reader's own bound (AC-0685).
        if decision_case.is_some()
            && validated.is_some_and(|text| text.len() as u64 > crate::adr::MAX_FOUND_ADR_BYTES)
        {
            crate::adr::skip_found_adr(&mut read.decisions, path, crate::UnreadAdrReason::TooLarge);
            decision_case = None;
        }
        let owned;
        let text = if let Some(validated) = validated {
            validated
        } else if let Some(Opened::Bytes(bytes)) = opened
            && let Ok(text) = String::from_utf8(bytes)
        {
            owned = text;
            owned.as_str()
        } else {
            if document.is_some() {
                skipped(&mut read.documents, DocumentSkip::NotUtf8);
            }
            if decision_case.is_some() {
                crate::adr::skip_found_adr(
                    &mut read.decisions,
                    path,
                    crate::UnreadAdrReason::NotUtf8,
                );
            }
            continue;
        };
        if decision_case.is_some() {
            decided.insert(path.to_string());
        }
        if let Some(is_candidate) = decision_case {
            crate::adr::read_found_adr(&mut read.decisions, repo, commit, path, text, is_candidate);
        }
        if let Some(format) = document {
            let mut roles = Vec::new();
            if declared.contains_key(path) {
                roles.push(ROLE_API_REFERENCE);
            }
            if decision {
                roles.push(ROLE_DECISION_RECORD);
            }
            push_document(
                &mut read.documents,
                repo,
                commit,
                path,
                name,
                format,
                text,
                &roles,
            );
            if decision_case.is_some() {
                push_records_edge(&mut read.documents, repo, commit, path, text.len());
            }
        }
    }
    if cases.documents {
        // A declared reference the walk did not reach is read from the
        // contents its loader validated, so it is a document too.
        for (path, doc) in &declared {
            if walked.contains(*path) {
                continue;
            }
            let lower = path.to_ascii_lowercase();
            let format = if lower.ends_with(".json") {
                Format::OpenApi
            } else {
                Format::of(path).unwrap_or(Format::Text)
            };
            let name = path.rsplit('/').next().unwrap_or(path);
            read.documents.counts.found += 1;
            // The same file bound as the walk's (AC-1601).
            if read.documents.counts.read as usize >= MAX_DOCUMENTS_PER_REPO {
                skipped(&mut read.documents, DocumentSkip::OverFileLimit);
                continue;
            }
            // A decision record under build output is read by the decision
            // case and not by the walk's document case: its document keeps
            // both roles and its `RECORDS` edge.
            let mut roles = vec![ROLE_API_REFERENCE];
            if crate::adr::is_decision_record(path, name) {
                roles.push(ROLE_DECISION_RECORD);
            }
            push_document(
                &mut read.documents,
                repo,
                commit,
                path,
                name,
                format,
                &doc.contents,
                &roles,
            );
            if decided.contains(*path) {
                push_records_edge(&mut read.documents, repo, commit, path, doc.contents.len());
            }
        }
    }
    let facts = &mut read.documents;
    facts.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    facts.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    // A declared reference read from its contents may sort before a walked
    // file: references stay in file and offset order.
    facts
        .references
        .sort_by(|left, right| (&left.path, left.start).cmp(&(&right.path, right.start)));
    crate::adr::finish_found_adrs(&mut read.decisions);
    Ok(read)
}

/// The `RECORDS` edge from the document of a found decision record to its
/// `ADR` fact.
fn push_records_edge(
    facts: &mut DocumentFacts,
    repo: &str,
    commit: &str,
    path: &str,
    bytes: usize,
) {
    let id = document_id(repo, path);
    let adr = format!("adr:{repo}@{path}");
    facts.edges.push(Edge {
        src: id.clone(),
        dst: adr.clone(),
        label: "RECORDS".into(),
        props: serde_json::json!({
            "prov": provenance(
                evidence(repo, path, commit, 0, bytes),
                format!("{id} RECORDS {adr}").as_bytes(),
            ),
        }),
    });
}

/// Add one read file's `Document`, `DocSection` and `HAS_SECTION` facts and
/// its references.
#[allow(clippy::too_many_arguments)]
fn push_document(
    facts: &mut DocumentFacts,
    repo: &str,
    commit: &str,
    path: &str,
    name: &str,
    format: Format,
    text: &str,
    roles: &[&str],
) {
    facts.counts.read += 1;
    let all = sections(format, text);
    if all.len() > MAX_SECTIONS_PER_DOCUMENT {
        facts.counts.sections_truncated += 1;
    }
    let found = &all[..all.len().min(MAX_SECTIONS_PER_DOCUMENT)];
    facts.counts.sections += found.len() as u64;
    let id = document_id(repo, path);
    let title = found
        .iter()
        .find(|section| section.level == 1 && !section.heading.is_empty())
        .map_or(name, |section| section.heading.as_str());
    let mut props = serde_json::json!({
        "path": path,
        "format": format.code(),
        "title": core_redact::redact_text(title).0,
        "sections": found.len(),
        "bytes": text.len(),
        "prov": provenance(
            evidence(repo, path, commit, 0, text.len()),
            &hashed(&(&id, text)),
        ),
    });
    // Present only for a document with a role, so every other document
    // keeps the fact it had (AC-1600).
    if !roles.is_empty() {
        props["roles"] = serde_json::json!(roles);
    }
    facts.nodes.push(Node {
        id: id.clone(),
        label: "Document".into(),
        props,
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
        for (reference, link, start, end) in markdown_references(text) {
            // The innermost section that holds the reference, decided
            // before the bound: a reference in a section past it is not
            // credited to an enclosing section that was kept.
            let Some(ordinal) = all
                .iter()
                .rposition(|section| (section.start..section.end).contains(&start))
                .filter(|ordinal| *ordinal < found.len())
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
    /// Endpoints by canonical path, whatever their method.
    paths: BTreeMap<String, Vec<(String, &'a Node)>>,
    /// Endpoints a route reference is compared with.
    endpoints: usize,
}

impl<'a> ReferenceIndex<'a> {
    /// Index the facts of the whole system.
    pub fn new(nodes: &'a [Node]) -> Self {
        Self::from_parts(
            nodes
                .iter()
                .map(|node| (node.id.as_str(), node.label.as_str())),
            nodes,
        )
    }

    /// Whether the index reads the props of a node with this label. Every
    /// other node is indexed by its id and label alone.
    pub fn reads_props(label: &str) -> bool {
        label == "Endpoint" || NAMED_LABELS.contains(&label)
    }

    /// [`Self::new`] for a caller that holds the graph packed (#2003): `keys`
    /// is every node's `(id, label)` and `detailed` the decoded nodes whose
    /// label [`Self::reads_props`], in graph order. The index equals the one
    /// [`Self::new`] builds over the same graph.
    pub fn from_parts(
        keys: impl IntoIterator<Item = (&'a str, &'a str)>,
        detailed: &'a [Node],
    ) -> Self {
        let mut index = Self::default();
        for (id, label) in keys {
            if !UNLINKED_LABELS.contains(&label) {
                index.labels.insert(id, label);
            }
        }
        for node in detailed {
            let label = node.label.as_str();
            if label == "Endpoint"
                && node.props["protocol"] != "graphql"
                && let (Some(method), Some(path)) =
                    (node.props["method"].as_str(), node.props["path"].as_str())
                && !method.trim().is_empty()
                && !path.trim().is_empty()
            {
                let route = crate::contract::canonical_endpoint(method, path);
                let served = route.split_once(' ').map_or("", |(_, path)| path);
                index
                    .paths
                    .entry(served.to_string())
                    .or_default()
                    .push((route.clone(), node));
                index.endpoints += 1;
                index.routes.entry(route).or_default().push(&node.id);
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
            return file(local_link_path(&reference.path, text)?);
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
/// The repository path a Markdown link target names: as [`link_path`],
/// after percent-decoding, so `setup%20guide.md` names `setup guide.md`. A
/// target whose escapes do not decode to UTF-8 is taken as written.
fn local_link_path(document: &str, target: &str) -> Option<String> {
    link_path(document, &percent_decoded(target))
}

/// `text` with each `%XX` escape decoded, or `text` itself when the result
/// is not UTF-8.
fn percent_decoded(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('%') {
        return text.into();
    }
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escape = (bytes[at] == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                at += 3;
            }
            None => {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(decoded).map_or(text.into(), Into::into)
}

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

/// Sections and endpoints one drift entry links to.
const MAX_DRIFT_LINKS: usize = 8;
/// Characters no checked file path holds: globs, placeholders, ids, options.
const NOT_A_PATH: [char; 18] = [
    '*', '<', '>', '{', '}', '$', '|', ':', '@', '=', ',', ';', '(', ')', '[', ']', '"', '\'',
];

/// The kind of a documentation drift entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum DocumentationDrift {
    /// The route's path is served, with no endpoint of its method.
    RouteMethodDiffers,
    /// No endpoint of the system serves the route's path.
    RouteNotServed,
    /// The path names no file of the repository at the commit.
    FileMissing,
}

impl DocumentationDrift {
    /// The closed kind code stored as `drift_kind`.
    pub fn code(self) -> &'static str {
        match self {
            Self::RouteMethodDiffers => "doc-route-method-differs",
            Self::RouteNotServed => "doc-route-not-served",
            Self::FileMissing => "doc-file-missing",
        }
    }
}

/// The repo-relative paths a code span may name, when it is shaped like a
/// path to a file: it has a directory and an extension, and that directory
/// is in the tree. A path is read from the repository root and from the
/// document's directory; only those whose directory exists are returned.
fn checked_code_paths(document: &str, text: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let path = without_position(text);
    let name = path.rsplit('/').next().unwrap_or(path);
    let shaped = path.contains('/')
        && !path.contains(NOT_A_PATH)
        && !path.contains(char::is_whitespace)
        && name.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty()
                && (1..=8).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
        });
    if !shaped {
        return Vec::new();
    }
    let mut paths: Vec<String> = [link_path("", path), link_path(document, path)]
        .into_iter()
        .flatten()
        .filter(|path| {
            path.rsplit_once('/')
                .is_some_and(|(directory, _)| exists(directory))
        })
        .collect();
    paths.dedup();
    paths
}

struct Finding<'r> {
    reference: &'r DocumentReference,
    /// The route or path as compared, before redaction.
    compared: String,
    sections: BTreeSet<&'r str>,
    spans: BTreeSet<(usize, usize)>,
}

/// `Drift` facts for the route and file references that no longer match
/// (AC-0679), with a `DOCUMENTED_AS` edge to each section that holds one
/// and, for a route served with other methods, a `SERVED_AS` edge to those
/// endpoints.
///
/// A reference [`link_references`] links yields nothing. A route is checked
/// only when the system has a recovered endpoint. `exists` answers whether a
/// repo-relative path is in the repository's tree. One document, kind and
/// route or path are one entry, citing each occurrence.
pub fn documentation_drift(
    repo: &str,
    commit: &str,
    references: &[DocumentReference],
    index: &ReferenceIndex<'_>,
    exists: &dyn Fn(&str) -> bool,
) -> (Vec<Node>, Vec<Edge>) {
    let mut findings: BTreeMap<(&str, DocumentationDrift, String), Finding<'_>> = BTreeMap::new();
    for reference in references {
        if index.resolve(repo, reference).is_some() {
            continue;
        }
        let text = reference.text.as_str();
        let found = if reference.link {
            local_link_path(&reference.path, text)
                .filter(|path| !exists(path))
                .map(|path| (DocumentationDrift::FileMissing, path))
        } else if let Some(route) = route_key(text) {
            let served = route.split_once(' ').map_or("", |(_, path)| path);
            if index.routes.contains_key(&route) || index.endpoints == 0 {
                None
            } else if index.paths.contains_key(served) {
                Some((DocumentationDrift::RouteMethodDiffers, route))
            } else {
                Some((DocumentationDrift::RouteNotServed, route))
            }
        } else {
            let paths = checked_code_paths(&reference.path, text, exists);
            let missing = !paths.iter().any(|path| exists(path));
            paths
                .into_iter()
                .next()
                .filter(|_| missing)
                .map(|path| (DocumentationDrift::FileMissing, path))
        };
        let Some((kind, key)) = found else {
            continue;
        };
        let finding = findings
            .entry((reference.path.as_str(), kind, key.clone()))
            .or_insert_with(|| Finding {
                reference,
                compared: key,
                sections: BTreeSet::new(),
                spans: BTreeSet::new(),
            });
        finding.sections.insert(reference.section.as_str());
        finding.spans.insert((reference.start, reference.end));
    }

    let rule = || crate::spec_rule(DOCUMENTATION_DRIFT_EXTRACTOR_ID);
    let inferred = |cited: Vec<EvidenceRef>, fact: &[u8]| {
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::InferredStrong,
            cited,
            DOCUMENTATION_DRIFT_EXTRACTOR_ID,
            fact,
        )
        .expect("InferredStrong is within the deterministic confidence ceiling")
        .with_method(rule())
    };
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for ((document, kind, raw), finding) in findings {
        // The route or path is stored and is part of the identity, so it is
        // redacted like a heading (AC-0680). Two keys that differ only in
        // secret text stay two entries: the id carries a digest of the key
        // as compared, never the secret itself.
        let key = core_redact::redact_text(&raw).0;
        let id = if key == raw {
            format!(
                "drift:documentation:{repo}@{document}:{}:{key}",
                kind.code()
            )
        } else {
            let digest = blake3::hash(raw.as_bytes()).to_hex();
            format!(
                "drift:documentation:{repo}@{document}:{}:{}:{key}",
                kind.code(),
                &digest[..16]
            )
        };
        let cited: Vec<EvidenceRef> = finding
            .spans
            .iter()
            .take(MAX_MENTION_EVIDENCE)
            .map(|(start, end)| evidence(repo, document, commit, *start, *end))
            .collect();
        let served = finding
            .compared
            .split_once(' ')
            .map_or("", |(_, path)| path);
        let others: &[(String, &Node)] = match kind {
            DocumentationDrift::RouteMethodDiffers => {
                index.paths.get(served).map_or(&[], Vec::as_slice)
            }
            _ => &[],
        };
        let compared = match kind {
            DocumentationDrift::RouteMethodDiffers => {
                let routes: BTreeSet<&str> =
                    others.iter().map(|(route, _)| route.as_str()).collect();
                format!(
                    "the path is served as {}",
                    routes.into_iter().collect::<Vec<_>>().join(", ")
                )
            }
            DocumentationDrift::RouteNotServed => format!(
                "none of the {} recovered endpoints of the system serves this path",
                index.endpoints
            ),
            DocumentationDrift::FileMissing => {
                format!("no file at this path in {repo}@{commit}")
            }
        };
        nodes.push(Node {
            id: id.clone(),
            label: "Drift".into(),
            props: serde_json::json!({
                "kind": "documentation-drift",
                "drift_kind": kind.code(),
                "key": key,
                "title": format!("{}: {key}", kind.code()),
                "document": document,
                "reference": core_redact::redact_text(&finding.reference.text).0,
                "compared": core_redact::redact_text(&compared).0,
                "occurrences": finding.spans.len(),
                "prov": inferred(cited.clone(), &hashed(&(kind.code(), repo, document, if key == raw { &key } else { &id }))),
            }),
        });
        for section in finding.sections.iter().take(MAX_DRIFT_LINKS) {
            edges.push(Edge {
                src: id.clone(),
                dst: (*section).to_string(),
                label: "DOCUMENTED_AS".into(),
                props: serde_json::json!({
                    "prov": inferred(
                        cited.clone(),
                        format!("{id} DOCUMENTED_AS {section}").as_bytes(),
                    ),
                }),
            });
        }
        let mut endpoints: Vec<&Node> = others.iter().map(|(_, node)| *node).collect();
        endpoints.sort_by(|left, right| left.id.cmp(&right.id));
        for endpoint in endpoints.into_iter().take(MAX_DRIFT_LINKS) {
            // The endpoint's own evidence where it has any, else the
            // reference it was compared with.
            let served_at = Provenance::deserialize(&*crate::stored_prov(&endpoint.props))
                .ok()
                .map(|prov| prov.evidence)
                .filter(|evidence| !evidence.is_empty())
                .unwrap_or_else(|| cited.clone());
            edges.push(Edge {
                src: id.clone(),
                dst: endpoint.id.clone(),
                label: "SERVED_AS".into(),
                props: serde_json::json!({
                    "prov": inferred(
                        served_at,
                        format!("{id} SERVED_AS {}", endpoint.id).as_bytes(),
                    ),
                }),
            });
        }
    }
    (nodes, edges)
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "documents_cases_tests.rs"]
mod cases_tests;
