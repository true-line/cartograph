//! Gated composition of D1 1.5, `draft-rules@1` (#2083, AC-0980).
//!
//! `section-composer@1` drafts a whole section in one call. On a large
//! section that call drafts a third to two-thirds of the rules, and on a
//! local model it runs out of output before it closes its JSON. This module
//! runs the same composer as a gated workflow instead, after the Universal
//! Agentic Workflow contract (the runtime owns the loop; a model only ever
//! sees one bounded step):
//!
//! 1. **partition** (deterministic): the compiled section is cut into
//!    chunks of at most `max_rows` rule rows, by capability.
//! 2. **draft** (model, per chunk): the composer's prompt over one chunk,
//!    at most `max_claims` claims (claims past the bound are left out and
//!    fail the reply). A deterministic gate checks the reply
//!    against the rows the chunk cites ([`gate`]); a failing gate's
//!    prescriptions become the next attempt's feedback, up to
//!    `max_retries`. After the last attempt the claims that pass every
//!    check are kept and the rest are left out and counted.
//! 3. **audit** (model, per chunk, `rules-auditor@1`): each kept claim is
//!    judged against the rows it cites. Unsupported claims are sent back to
//!    the drafter once with the auditor's reason; claims still unsupported
//!    after that are left out. An audit reply outside the schema is asked
//!    for once more; claims still without a verdict are left out.
//! 4. **assemble** (deterministic): kept claims in chunk order; a statement
//!    drafted again word for word is one claim citing the rows of both.
//!
//! Attested records handed to the section (ADR-0090 decision 6) are drafted
//! after the rule rows, as chunks of their own: one row per record, its
//! `A-` id the row's citation, so every check reads the record's statement
//! as it reads a rule row.
//!
//! The stage parameters and gate checks are read from the declarative
//! workflow file `workflows/draft-rules.workflow.json`, so the same file
//! can drive another runtime. Nothing here calls a provider: the host passes
//! a [`StageModel`] and owns egress, consent, limits and cancellation. The
//! gate reads only data the chunk prints and the model's own reply, so a
//! prescription never carries anything the model has not seen.

use crate::section_compose::{self, ComposeAttested, ComposeFact, ComposeInput};
use crate::section_draft::{
    Claim, ClaimKind, MAX_CITATIONS, MAX_CLAIMS, SectionAddress, SectionDraftFinish,
};
use llm::{AnalysisTier, CompletionAction, CompletionPayload};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The role a gated composition records.
pub const DRAFT_RULES_ROLE: &str = "draft-rules@1";
/// Audit replies asked for per round before the round's claims are dropped
/// unjudged.
const AUDIT_ATTEMPTS: usize = 2;
/// The auditor stage's role.
pub const RULES_AUDITOR_ROLE: &str = "rules-auditor@1";
/// The declarative workflow this module runs.
pub const WORKFLOW_FILE: &str = include_str!("../workflows/draft-rules.workflow.json");

/// One gate check, by its workflow-file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    /// The reply is the schema's object and every claim keeps the per-claim
    /// bounds of [`section_compose::decode`].
    ReplyMatchesSchema,
    /// Every cited id is one the chunk prints.
    CitationsResolve,
    /// Every rule row of the chunk is cited by a kept claim.
    RowsCovered,
    /// No statement names a source identifier it cites (host check, AC-0683).
    NamesSource,
    /// No statement carries a code identifier, call or file name.
    NoCodeIdentifiers,
    /// Every quoted text occurs in a row the claim cites.
    QuotesVerbatim,
    /// Every number in a statement occurs in a row the claim cites, and no
    /// statement carries a template placeholder.
    LiteralsSupported,
    /// An ordering word is used only when a cited row uses it too.
    OrderingSupported,
}

impl CheckType {
    /// The check's workflow-file name.
    pub fn code(self) -> &'static str {
        match self {
            Self::ReplyMatchesSchema => "reply_matches_schema",
            Self::CitationsResolve => "citations_resolve",
            Self::RowsCovered => "rows_covered",
            Self::NamesSource => "names_source",
            Self::NoCodeIdentifiers => "no_code_identifiers",
            Self::QuotesVerbatim => "quotes_verbatim",
            Self::LiteralsSupported => "literals_supported",
            Self::OrderingSupported => "ordering_supported",
        }
    }
}

#[derive(Deserialize)]
struct WorkflowFile {
    workflow: String,
    version: u32,
    role: String,
    section: String,
    stages: Vec<StageFile>,
}

#[derive(Deserialize)]
struct StageFile {
    name: String,
    #[serde(default)]
    max_rows: Option<usize>,
    #[serde(default)]
    max_claims: Option<usize>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    gate: Option<GateFile>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    on_verdict: BTreeMap<String, RouteFile>,
}

#[derive(Deserialize)]
struct GateFile {
    checks: Vec<CheckFile>,
}

#[derive(Deserialize)]
struct CheckFile {
    #[serde(rename = "type")]
    kind: CheckType,
}

#[derive(Deserialize)]
struct RouteFile {
    route_to: String,
    max_round_trips: u32,
}

/// The validated workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    /// The section it drafts.
    pub section: String,
    /// Most rule rows in one chunk.
    pub max_rows: usize,
    /// Most claims one draft call is asked for.
    pub max_claims: usize,
    /// Draft attempts after the first.
    pub max_retries: u32,
    /// The draft gate's checks, in order.
    pub checks: Vec<CheckType>,
    /// Times unsupported claims go back to the drafter; `None` runs no audit.
    pub audit_round_trips: Option<u32>,
}

impl Workflow {
    /// The shipped workflow.
    pub fn shipped() -> Result<Self, String> {
        Self::from_json(WORKFLOW_FILE)
    }

    /// Read and validate a workflow file.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: WorkflowFile =
            serde_json::from_str(text).map_err(|error| format!("workflow_file: {error}"))?;
        if file.workflow != "draft-rules" || file.version != 1 || file.role != DRAFT_RULES_ROLE {
            return Err("workflow_file: not draft-rules v1".into());
        }
        let stage = |name: &str| file.stages.iter().find(|stage| stage.name == name);
        let partition = stage("partition").ok_or("workflow_file: no partition stage")?;
        let draft = stage("draft").ok_or("workflow_file: no draft stage")?;
        let max_rows = partition.max_rows.filter(|n| (1..=32).contains(n));
        let max_claims = draft
            .max_claims
            .filter(|n| (1..=section_compose::COMPOSER_CLAIMS).contains(n));
        let max_retries = draft.max_retries.filter(|n| *n <= 4);
        let (Some(max_rows), Some(max_claims), Some(max_retries)) =
            (max_rows, max_claims, max_retries)
        else {
            return Err("workflow_file: a stage bound is missing or out of range".into());
        };
        let checks: Vec<CheckType> = draft
            .gate
            .as_ref()
            .map(|gate| gate.checks.iter().map(|check| check.kind).collect())
            .unwrap_or_default();
        let audit_round_trips = match stage("audit") {
            None => None,
            Some(audit) => {
                if audit.role.as_deref() != Some(RULES_AUDITOR_ROLE) {
                    return Err("workflow_file: unknown auditor role".into());
                }
                let route = audit
                    .on_verdict
                    .get("unsupported")
                    .filter(|route| route.route_to == "draft" && route.max_round_trips <= 2)
                    .ok_or("workflow_file: the audit routes nowhere")?;
                Some(route.max_round_trips)
            }
        };
        Ok(Self {
            section: file.section,
            max_rows,
            max_claims,
            max_retries,
            checks,
            audit_round_trips,
        })
    }
}

/// The fingerprint of this role's definition: the composer's own, the
/// workflow file and the auditor's instructions.
pub fn definition_fingerprint() -> String {
    crate::source_basis::domain_hash(
        b"cartograph:draft-rules-definition:v1\0",
        format!(
            "{DRAFT_RULES_ROLE}\0{}\0{WORKFLOW_FILE}\0{AUDITOR_SYSTEM}\0{ROW_SELECTION}",
            section_compose::definition_fingerprint()
        )
        .as_bytes(),
    )
}

// ------------------------------------------------------------------ partition

/// One rule row: its line and the citable ids it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The table row as compiled.
    pub text: String,
    /// The citable ids it prints, in order.
    pub ids: Vec<String>,
}

/// One chunk of the section, drafted in one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The chunk as the model reads it: headings, table header, rows.
    pub text: String,
    /// Its rule rows.
    pub rows: Vec<Row>,
}

impl Chunk {
    /// The citable ids the chunk prints.
    pub fn ids(&self) -> BTreeSet<String> {
        self.rows.iter().flat_map(|row| row.ids.clone()).collect()
    }
}

/// Every `F-` citation id `text` prints, in order, once each.
fn printed_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut ids: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("F-") {
        let start = at + found;
        let hex = &bytes[start + 2..];
        let whole = hex.len() >= 16
            && hex[..16]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            && !hex.get(16).is_some_and(u8::is_ascii_alphanumeric)
            && !(start > 0 && bytes[start - 1].is_ascii_alphanumeric());
        if whole {
            let id = text[start..start + 18].to_owned();
            if !ids.contains(&id) {
                ids.push(id);
            }
            at = start + 18;
        } else {
            at = start + 2;
        }
    }
    ids
}

struct Block {
    heads: Vec<String>,
    header: Vec<String>,
    rows: Vec<Row>,
}

fn separator(line: &str) -> bool {
    line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The Rule cell the spec compiler prints for an exit whose condition or
/// outcome is outside its rendering rules (`spec`'s `NOT_STATED`): a Gap,
/// with no rule to state.
const NOT_STATED_PREFIX: &str = "Not stated in domain terms (Gap)";

/// Bump when the rows [`partition`] keeps change; it is part of the role's
/// definition fingerprint, so a draft records which selection it read.
const ROW_SELECTION: &str = "rows@3:gap-skipped:repeated-rules-dropped";

/// Rule rows [`partition`] did not hand to the drafter (#1748).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// Rows whose Rule cell is a Gap: nothing to state.
    pub gap_rows: usize,
    /// Rows whose rule an earlier row of the section already holds: under a
    /// rules table, the same Rule cell (a shared helper is printed under
    /// every capability that reaches it, each copy with its own Where);
    /// under any other table, the same row.
    pub repeated_rows: usize,
}

/// Cut compiled section `text` into chunks of at most `max_rows` rule rows.
/// A rule row is a table row printing at least one id in `citable`; a chunk
/// repeats the area and capability headings and the table header its rows
/// sit under, so it reads on its own.
pub fn partition(text: &str, citable: &BTreeSet<String>, max_rows: usize) -> Vec<Chunk> {
    partition_counted(text, citable, max_rows).0
}

/// [`partition`], leaving out the rows nothing can be drafted from, and
/// saying how many: a Gap row, and a row repeating a rule an earlier row
/// holds (#1748). Without this, each copy of a shared rule became its own
/// claim and a Gap row a claim that states nothing.
pub fn partition_counted(
    text: &str,
    citable: &BTreeSet<String>,
    max_rows: usize,
) -> (Vec<Chunk>, Skipped) {
    let max_rows = max_rows.max(1);
    let mut skipped = Skipped::default();
    let mut rules_seen: BTreeSet<String> = BTreeSet::new();
    let mut blocks: Vec<Block> = Vec::new();
    let (mut area, mut capability, mut label): (Option<&str>, Option<&str>, Option<&str>) =
        (None, None, None);
    let mut in_table = false;
    let title = text.lines().find(|line| line.starts_with("## "));
    for line in text.lines() {
        if line.starts_with('|') {
            if !in_table {
                in_table = true;
                blocks.push(Block {
                    heads: [area, capability, label]
                        .into_iter()
                        .flatten()
                        .map(str::to_owned)
                        .collect(),
                    header: vec![line.to_owned()],
                    rows: Vec::new(),
                });
                continue;
            }
            let block = blocks.last_mut().expect("a table starts a block");
            if separator(line) {
                block.header.push(line.to_owned());
                continue;
            }
            let ids: Vec<String> = printed_ids(line)
                .into_iter()
                .filter(|id| citable.contains(id))
                .collect();
            if !ids.is_empty() {
                let rule = rule_cell(line);
                if rule.starts_with(NOT_STATED_PREFIX) {
                    skipped.gap_rows += 1;
                    continue;
                }
                // A state machine's transitions all cite the machine, and two
                // states may move to the same place: only the whole row says
                // which transition it is.
                let key = if rules_table(&block.header[0]) {
                    norm(rule)
                } else {
                    norm(line)
                };
                if !rules_seen.insert(key) {
                    skipped.repeated_rows += 1;
                    continue;
                }
                block.rows.push(Row {
                    text: line.to_owned(),
                    ids,
                });
            }
            continue;
        }
        in_table = false;
        if line.starts_with("### ") {
            (area, capability, label) = (Some(line), None, None);
        } else if line.starts_with("#### ") {
            (capability, label) = (Some(line), None);
        } else if !line.trim().is_empty() && !line.starts_with("## ") {
            label = Some(line);
        }
    }
    let mut chunks = Vec::new();
    let mut parts: Vec<(&Block, &[Row])> = Vec::new();
    let mut count = 0;
    for block in blocks.iter().filter(|block| !block.rows.is_empty()) {
        for rows in block.rows.chunks(max_rows) {
            if count + rows.len() > max_rows && !parts.is_empty() {
                chunks.push(render(title, &parts));
                parts.clear();
                count = 0;
            }
            parts.push((block, rows));
            count += rows.len();
        }
    }
    if !parts.is_empty() {
        chunks.push(render(title, &parts));
    }
    (chunks, skipped)
}

fn render(title: Option<&str>, parts: &[(&Block, &[Row])]) -> Chunk {
    let mut text = String::new();
    if let Some(title) = title {
        text.push_str(title);
        text.push_str("\n\n");
    }
    let mut rows = Vec::new();
    for (block, part) in parts {
        for head in &block.heads {
            text.push_str(head);
            text.push('\n');
        }
        text.push('\n');
        for line in &block.header {
            text.push_str(line);
            text.push('\n');
        }
        for row in *part {
            text.push_str(&row.text);
            text.push('\n');
            rows.push(row.clone());
        }
        text.push('\n');
    }
    Chunk { text, rows }
}

/// The heading of a chunk of attested records.
pub const ATTESTED_HEADING: &str = "### Attested records";

/// Chunks of at most `max_rows` of `attested`, in order, under the
/// section's `##` title of `text`: one table row per record, citing its
/// `A-` id (ADR-0090 decision 6).
pub fn attested_chunks(text: &str, attested: &[ComposeAttested], max_rows: usize) -> Vec<Chunk> {
    let title = text.lines().find(|line| line.starts_with("## "));
    attested
        .chunks(max_rows.max(1))
        .map(|records| {
            let mut text = String::new();
            if let Some(title) = title {
                text.push_str(title);
                text.push_str("\n\n");
            }
            text.push_str(ATTESTED_HEADING);
            text.push_str("\n\n| Label | Statement | Citation |\n|---|---|---|\n");
            let rows: Vec<Row> = records
                .iter()
                .map(|record| Row {
                    text: format!(
                        "| {} | {} | {} |",
                        record.label,
                        record.statement.replace('|', "\\|"),
                        record.citation_id
                    ),
                    ids: vec![record.citation_id.clone()],
                })
                .collect();
            for row in &rows {
                text.push_str(&row.text);
                text.push('\n');
            }
            Chunk { text, rows }
        })
        .collect()
}

// ---------------------------------------------------------------------- gate

/// A gate's verdict on one reply, in the runtime-neutral shape UAW uses:
/// `{stage, passed, failures, prescriptions}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateResult {
    /// The stage gated.
    pub stage: &'static str,
    /// Whether every check passed.
    pub passed: bool,
    /// The failing checks, once each, in check order.
    pub failures: Vec<CheckType>,
    /// What the next attempt must fix, one line per failure.
    pub prescriptions: Vec<String>,
    /// Per claim, whether it passed every per-claim check.
    #[serde(skip)]
    pub claim_ok: Vec<bool>,
    /// Per claim, the per-claim checks it failed.
    #[serde(skip)]
    pub claim_failures: Vec<Vec<CheckType>>,
}

/// Normalize compiled or model text for containment: Markdown escapes and
/// entities undone, curly quotes made straight, case and spacing folded.
fn norm(text: &str) -> String {
    let text = text
        .replace("\\_", "_")
        .replace("\\|", "|")
        .replace("\\*", "*")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace(['\u{201c}', '\u{201d}'], "\"")
        .replace(['\u{2018}', '\u{2019}'], "'");
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The quoted spans of `statement` (3 or more characters), and the
/// statement with them blanked out.
fn quotes(statement: &str) -> (Vec<String>, String) {
    let chars: Vec<char> = statement.chars().collect();
    let mut spans = Vec::new();
    let mut rest = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let close = match c {
            '\u{201c}' => Some('\u{201d}'),
            '"' => Some('"'),
            '\'' if i == 0 || chars[i - 1].is_whitespace() || matches!(chars[i - 1], '(' | '[') => {
                Some('\'')
            }
            _ => None,
        };
        if let Some(close) = close {
            let end = (i + 1..chars.len()).find(|&j| {
                chars[j] == close
                    && (close != '\''
                        || chars.get(j + 1).is_none_or(|next| {
                            next.is_whitespace()
                                || matches!(next, '.' | ',' | ';' | ':' | ')' | '!' | '?')
                        }))
            });
            if let Some(end) = end {
                let span: String = chars[i + 1..end].iter().collect();
                if span.chars().count() >= 3 {
                    spans.push(span);
                    rest.push(' ');
                    i = end + 1;
                    continue;
                }
            }
        }
        rest.push(c);
        i += 1;
    }
    (spans, rest)
}

/// Whether `word` occurs in `text` (both normalized) on word boundaries.
fn has_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(found) = text[at..].find(word) {
        let start = at + found;
        let end = start + word.len();
        let before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before && after {
            return true;
        }
        at = start + 1;
    }
    false
}

const ORDERING_WORDS: &[&str] = &[
    "before",
    "after",
    "then",
    "first",
    "prior to",
    "followed by",
];

/// Ordering words in `rest` (quotes removed) that no cited row uses.
fn unsupported_order(rest: &str, rows: &str) -> Vec<&'static str> {
    let rest = norm(rest);
    ORDERING_WORDS
        .iter()
        .copied()
        .filter(|word| has_word(&rest, word) && !has_word(rows, word))
        .collect()
}

const FILE_EXTENSIONS: &[&str] = &[
    "py", "ts", "tsx", "js", "jsx", "rs", "go", "java", "kt", "rb", "php", "html", "cs", "swift",
];
const DOMAIN_ENDINGS: &[&str] = &["com", "io", "org", "net", "ai", "dev", "app", "co", "eu"];
const MIXED_CASE_WORDS: &[&str] = &[
    "iOS", "iPhone", "iPad", "macOS", "eBay", "iCloud", "watchOS",
];

/// Code-shaped tokens of `rest` (quotes removed): snake case, calls,
/// camel case, dotted access and file names. URL routes are contract, not
/// code, and are skipped.
fn code_identifiers(rest: &str) -> Vec<String> {
    let mut found = Vec::new();
    for raw in rest.split_whitespace() {
        if raw.starts_with('/') || raw.contains("://") {
            continue;
        }
        let call = raw.char_indices().any(|(at, c)| {
            c == '(' && at > 0 && raw[..at].ends_with(|p: char| p.is_alphanumeric())
        });
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if token.is_empty() {
            continue;
        }
        let snake =
            token.split('_').filter(|part| !part.is_empty()).count() > 1 && token.contains('_');
        let camel = token.len() >= 4
            && token.chars().all(|c| c.is_ascii_alphabetic())
            && token.starts_with(|c: char| c.is_ascii_lowercase())
            && token.chars().skip(1).any(|c| c.is_ascii_uppercase())
            && !MIXED_CASE_WORDS.contains(&token);
        let dotted = token.split('.').count() == 2 && {
            let (head, tail) = token.split_once('.').expect("two parts");
            let lower = |part: &str| {
                !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            };
            lower(head)
                && lower(tail)
                && !DOMAIN_ENDINGS.contains(&tail)
                && !FILE_EXTENSIONS.contains(&tail)
        };
        let file = token
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && FILE_EXTENSIONS.contains(&ext));
        if snake || call || camel || dotted || file {
            found.push(token.to_owned());
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Digit runs of `rest` (quotes removed) that no cited row prints.
fn unsupported_numbers(rest: &str, rows: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for c in rest.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    runs.sort();
    runs.dedup();
    runs.into_iter()
        .filter(|run| !has_word(rows, run))
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

/// Whether `header` heads a rules table (`| Where | Rule | ... |`), whose
/// Rule cell alone is the rule and whose Where says only where it applies.
fn rules_table(header: &str) -> bool {
    rule_cell(header) == "Rule"
}

/// The Rule cell of a compiled row, falling back to the row.
fn rule_cell(row: &str) -> &str {
    let cells: Vec<&str> = row.trim().trim_matches('|').split(" | ").collect();
    cells.get(1).copied().unwrap_or(row).trim()
}

/// Gate `claims`, the decoded reply to one chunk, with `left_out` claims
/// already dropped by decoding (or `None` when the reply was not the
/// schema). `names_source` is the host's check that a statement names a
/// source identifier it cites (AC-0683).
pub fn gate(
    checks: &[CheckType],
    claims: &[Claim],
    left_out: Option<usize>,
    chunk: &Chunk,
    names_source: &dyn Fn(&Claim) -> bool,
) -> GateResult {
    let on = |check: CheckType| checks.contains(&check);
    let mut failures: BTreeSet<CheckType> = BTreeSet::new();
    let mut prescriptions: Vec<String> = Vec::new();
    let ids = chunk.ids();
    let mut row_text: BTreeMap<&str, String> = BTreeMap::new();
    for row in &chunk.rows {
        for id in &row.ids {
            let entry = row_text.entry(id.as_str()).or_default();
            entry.push(' ');
            entry.push_str(&norm(&row.text));
        }
    }
    match left_out {
        None => {
            failures.insert(CheckType::ReplyMatchesSchema);
            prescriptions.push(
                "Reply with exactly the JSON object the response schema describes, holding at \
                 least one claim."
                    .into(),
            );
        }
        Some(n) if n > 0 && on(CheckType::ReplyMatchesSchema) => {
            failures.insert(CheckType::ReplyMatchesSchema);
            prescriptions.push(format!(
                "{n} claim(s) were left out: each claim needs a plain statement of at least five \
                 words, 1 to 16 citation ids printed in this chunk, and a claim_kind from the \
                 schema."
            ));
        }
        _ => {}
    }
    let mut claim_ok = Vec::with_capacity(claims.len());
    let mut claim_failures = Vec::with_capacity(claims.len());
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    for (index, claim) in claims.iter().enumerate() {
        let n = index + 1;
        let mut bad: Vec<(CheckType, String)> = Vec::new();
        let unknown: Vec<&String> = claim
            .citations
            .iter()
            .filter(|id| !ids.contains(*id))
            .collect();
        if on(CheckType::CitationsResolve) && (claim.citations.is_empty() || !unknown.is_empty()) {
            bad.push((
                CheckType::CitationsResolve,
                format!(
                    "Claim {n} cites ids this chunk does not print; cite only the chunk's ids."
                ),
            ));
        }
        let rows: String = claim
            .citations
            .iter()
            .filter_map(|id| row_text.get(id.as_str()))
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let (quoted, rest) = quotes(&claim.statement);
        if on(CheckType::NamesSource) && names_source(claim) {
            bad.push((
                CheckType::NamesSource,
                format!(
                    "Claim {n} names a source file or code symbol of a fact it cites; say what it \
                     does in domain terms instead."
                ),
            ));
        }
        if on(CheckType::NoCodeIdentifiers) {
            let found = code_identifiers(&rest);
            if !found.is_empty() {
                bad.push((
                    CheckType::NoCodeIdentifiers,
                    format!(
                        "Claim {n} uses code identifiers ({}); say what they mean in domain terms.",
                        found.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
        if on(CheckType::QuotesVerbatim) {
            for quote in &quoted {
                let wanted = norm(quote);
                let wanted = wanted.trim_end_matches(['.', ' ']);
                if !wanted.is_empty() && !rows.contains(wanted) {
                    bad.push((
                        CheckType::QuotesVerbatim,
                        format!(
                            "Claim {n} quotes \"{}\", which its cited rows do not print; quote \
                             messages exactly as the rows give them, or do not quote.",
                            clip(quote, 80)
                        ),
                    ));
                }
            }
        }
        if on(CheckType::LiteralsSupported) {
            if claim.statement.contains(['{', '}']) {
                bad.push((
                    CheckType::LiteralsSupported,
                    format!(
                        "Claim {n} carries a template placeholder in braces; describe the value in \
                         words (for example \"the partner's name\")."
                    ),
                ));
            }
            let numbers = unsupported_numbers(&rest, &rows);
            if !numbers.is_empty() {
                bad.push((
                    CheckType::LiteralsSupported,
                    format!(
                        "Claim {n} states {} which its cited rows do not; state only statuses, \
                         limits and numbers the rows give.",
                        numbers.join(", ")
                    ),
                ));
            }
        }
        if on(CheckType::OrderingSupported) {
            for word in unsupported_order(&rest, &rows) {
                bad.push((
                    CheckType::OrderingSupported,
                    format!(
                        "Claim {n} says \"{word}\", an order its cited rows do not establish; \
                         remove it."
                    ),
                ));
            }
        }
        let ok = bad.is_empty();
        if ok {
            covered.extend(claim.citations.iter().map(String::as_str));
        }
        let mut kinds: Vec<CheckType> = bad.iter().map(|(kind, _)| *kind).collect();
        kinds.dedup();
        for (kind, text) in bad {
            failures.insert(kind);
            prescriptions.push(text);
        }
        claim_ok.push(ok);
        claim_failures.push(kinds);
    }
    if on(CheckType::RowsCovered) {
        let missing: Vec<&Row> = chunk
            .rows
            .iter()
            .filter(|row| !row.ids.iter().any(|id| covered.contains(id.as_str())))
            .collect();
        if !missing.is_empty() {
            failures.insert(CheckType::RowsCovered);
            let list = missing
                .iter()
                .take(12)
                .map(|row| format!("{}: {}", row.ids[0], clip(rule_cell(&row.text), 160)))
                .collect::<Vec<_>>()
                .join("; ");
            prescriptions.push(format!(
                "{} rule row(s) are not cited by any passing claim. Cover them: {list}",
                missing.len()
            ));
        }
    }
    let failures: Vec<CheckType> = checks
        .iter()
        .copied()
        .chain([CheckType::ReplyMatchesSchema])
        .filter(|check| failures.contains(check))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    GateResult {
        stage: "draft",
        passed: failures.is_empty(),
        failures,
        prescriptions,
        claim_ok,
        claim_failures,
    }
}

// ---------------------------------------------------------------------- audit

const AUDITOR_SYSTEM: &str = "You are Cartograph's rules auditor. You check claims drafted for a \
product specification against the facts they cite. For each numbered claim, read only the rows it \
cites and judge it: supported when every condition, outcome, message, status and number it states \
is in those rows; adds_detail when it states something the rows do not (a status code, a \
consequence, a limit, an actor); misreads_condition when it gets a condition wrong (for example \
reading \"is not A, is not B, is C\" as \"is none of them\"); contradicts when the rows say \
otherwise. Paraphrase, grouping and domain wording are fine. Reply with the JSON object the \
response schema describes and nothing else.";

/// An auditor's verdict on one claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The cited rows state it.
    Supported,
    /// It states something the rows do not.
    AddsDetail,
    /// It gets a condition wrong.
    MisreadsCondition,
    /// The rows say otherwise.
    Contradicts,
}

impl Verdict {
    fn prescription(self, n: usize) -> Option<String> {
        match self {
            Self::Supported => None,
            Self::AddsDetail => Some(format!(
                "Claim {n} states something its cited rows do not (such as a status, a consequence \
                 or a limit); keep only what the rows say."
            )),
            Self::MisreadsCondition => Some(format!(
                "Claim {n} misreads a condition; restate the condition exactly as its rows give it \
                 (\"is not A, is not B, is C\" means \"is C\")."
            )),
            Self::Contradicts => Some(format!(
                "Claim {n} contradicts its cited rows; restate it from them."
            )),
        }
    }
}

fn auditor_schema() -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdicts"],
        "properties": {
            "verdicts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["claim", "verdict"],
                    "properties": {
                        "claim": {"type": "integer"},
                        "verdict": {"enum": [
                            "supported", "adds_detail", "misreads_condition", "contradicts"
                        ]}
                    }
                }
            }
        }
    })
    .to_string()
}

/// The auditor's one action over `claims` of `chunk`.
pub fn audit_action(chunk: &Chunk, claims: &[Claim], action_id: String) -> CompletionAction {
    let mut prompt = String::from("<claims>\n");
    for (index, claim) in claims.iter().enumerate() {
        let _ = writeln!(
            prompt,
            "[{}] {}\n    cites: {}",
            index + 1,
            claim.statement,
            claim.citations.join(", ")
        );
    }
    let _ = write!(
        prompt,
        "</claims>\n\n<rows>\n{}\n</rows>\n\nGive one verdict for each of the {} claims.\n",
        chunk.text.trim_end(),
        claims.len()
    );
    CompletionAction {
        action_id,
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            response_schema: Some(auditor_schema()),
            system: AUDITOR_SYSTEM.to_owned(),
            prompt,
            spans: Vec::new(),
        },
    }
}

#[derive(Deserialize)]
struct AuditReply {
    verdicts: Vec<AuditVerdict>,
}

#[derive(Deserialize)]
struct AuditVerdict {
    claim: i64,
    verdict: Verdict,
}

/// Decode an auditor reply into one verdict per claim. The reply must give
/// exactly one verdict for each claim, numbered 1 to `claims`; a reply that
/// is not the schema, omits a claim, repeats one or numbers one out of range
/// is `None`, and the audit is counted as failed (`audit_failed`); claims
/// no valid reply judged are left out, never taken as judged.
pub fn decode_audit(text: &str, claims: usize) -> Option<Vec<Verdict>> {
    let reply: AuditReply = serde_json::from_str(text.trim()).ok()?;
    let mut verdicts: Vec<Option<Verdict>> = vec![None; claims];
    for verdict in reply.verdicts {
        let index = usize::try_from(verdict.claim.checked_sub(1)?).ok()?;
        let slot = verdicts.get_mut(index)?;
        if slot.replace(verdict.verdict).is_some() {
            return None;
        }
    }
    verdicts.into_iter().collect()
}

// --------------------------------------------------------------------- runner

/// The model boundary the host provides: one bounded call per action. An
/// error stops the run (a cancel, a provider failure).
pub trait StageModel {
    /// The reply text to `action`.
    fn complete(&mut self, action: &CompletionAction) -> Result<String, String>;
}

/// What a gated composition is drafted from.
#[derive(Debug, Clone)]
pub struct GatedInput<'a> {
    /// The address drafted.
    pub address: &'a SectionAddress,
    /// The section's catalogue title.
    pub title: &'a str,
    /// The section as the export compiles it.
    pub section: &'a str,
    /// The citable facts.
    pub facts: &'a [ComposeFact],
    /// The attested records handed to the section, drafted after its rows.
    pub attested: &'a [ComposeAttested],
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GatedStats {
    /// Chunks drafted.
    pub chunks: usize,
    /// Rule rows in the section that were drafted.
    pub rows: usize,
    /// Gap rows left out before drafting: nothing to state (#1748).
    pub gap_rows_skipped: usize,
    /// Rows left out before drafting because an earlier row holds the
    /// same rule (#1748).
    pub repeated_rows_skipped: usize,
    /// Rule rows cited by a kept claim.
    pub rows_covered: usize,
    /// Model calls, drafting and auditing.
    pub calls: usize,
    /// Chunks whose first draft passed the gate.
    pub first_try_passed: usize,
    /// Draft attempts after a failing gate.
    pub retries: usize,
    /// Claims the auditor sent back to the drafter.
    pub sent_back: usize,
    /// Claims left out because the auditor still found them unsupported.
    pub audit_dropped: usize,
    /// Audit replies that were not the schema; a round with no valid reply
    /// drops its claims.
    pub audit_failed: usize,
    /// Per-claim gate failures among the claims left out, by check.
    pub failures: BTreeMap<String, usize>,
}

/// A gated composition's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedOutcome {
    /// The assembled finish, or `None` when no claim was kept.
    pub finish: Option<SectionDraftFinish>,
    /// Claims left out: decoding bounds, failing checks or the audit.
    pub left_out: usize,
    /// What the run did.
    pub stats: GatedStats,
}

struct Attempt {
    claims: Vec<Claim>,
    gate: GateResult,
    reply: String,
    left_out: usize,
}

fn feedback(base: &str, reply: &str, prescriptions: &[String], kept_note: &str) -> String {
    let mut prompt = base.to_owned();
    let _ = write!(
        prompt,
        "\n<previous_reply>\n{}\n</previous_reply>\n\n<checks_failed>\n",
        reply.trim()
    );
    for line in prescriptions.iter().take(24) {
        let _ = writeln!(prompt, "- {line}");
    }
    let _ = write!(
        prompt,
        "</checks_failed>\n\nReply with the full corrected JSON object.{kept_note}\n"
    );
    prompt
}

fn draft_action(base: &CompletionAction, prompt: String, id: String) -> CompletionAction {
    CompletionAction {
        action_id: id,
        tier: base.tier,
        payload: CompletionPayload {
            prompt,
            ..base.payload.clone()
        },
    }
}

/// One draft call and its gate.
fn attempt(
    workflow: &Workflow,
    input: &GatedInput<'_>,
    chunk: &Chunk,
    model: &mut dyn StageModel,
    action: CompletionAction,
    names_source: &dyn Fn(&Claim) -> bool,
    stats: &mut GatedStats,
) -> Result<Attempt, String> {
    stats.calls += 1;
    let reply = model.complete(&action)?;
    let decoded = section_compose::decode(input.address, &reply, &chunk.ids()).ok();
    let (mut claims, left_out) = match decoded {
        Some(composed) => (composed.finish.claims, Some(composed.left_out)),
        None => (Vec::new(), None),
    };
    // The stage's claim bound holds whatever the model returns: claims past
    // it are left out and fail the reply.
    let surplus = claims.len().saturating_sub(workflow.max_claims);
    claims.truncate(workflow.max_claims);
    let mut gate = gate(&workflow.checks, &claims, left_out, chunk, names_source);
    if surplus > 0 {
        gate.passed = false;
        if !gate.failures.contains(&CheckType::ReplyMatchesSchema) {
            gate.failures.insert(0, CheckType::ReplyMatchesSchema);
        }
        gate.prescriptions.insert(
            0,
            format!(
                "Reply with at most {} claims; the {surplus} after them were left out.",
                workflow.max_claims
            ),
        );
    }
    Ok(Attempt {
        claims,
        gate,
        reply,
        left_out: left_out.unwrap_or(0) + surplus,
    })
}

/// The kept claims of an attempt and how many it left out.
fn kept(attempt: &Attempt, stats: &mut GatedStats) -> (Vec<Claim>, usize) {
    let mut claims = Vec::new();
    let mut dropped = attempt.left_out;
    for ((claim, ok), failed) in attempt
        .claims
        .iter()
        .zip(&attempt.gate.claim_ok)
        .zip(&attempt.gate.claim_failures)
    {
        if *ok {
            claims.push(claim.clone());
        } else {
            dropped += 1;
            for check in failed {
                *stats.failures.entry(check.code().to_owned()).or_default() += 1;
            }
        }
    }
    (claims, dropped)
}

fn score(attempt: &Attempt, chunk: &Chunk) -> (usize, usize) {
    let cited: BTreeSet<&String> = attempt
        .claims
        .iter()
        .zip(&attempt.gate.claim_ok)
        .filter(|(_, ok)| **ok)
        .flat_map(|(claim, _)| &claim.citations)
        .collect();
    let rows = chunk
        .rows
        .iter()
        .filter(|row| row.ids.iter().any(|id| cited.contains(id)))
        .count();
    let ok = attempt.gate.claim_ok.iter().filter(|ok| **ok).count();
    (rows, ok)
}

/// Run the workflow over `input`. `names_source` is the host's AC-0683
/// check; `model` makes each call.
pub fn run(
    workflow: &Workflow,
    input: &GatedInput<'_>,
    model: &mut dyn StageModel,
    names_source: &dyn Fn(&Claim) -> bool,
) -> Result<GatedOutcome, String> {
    let citable: BTreeSet<String> = input
        .facts
        .iter()
        .map(|fact| fact.citation_id.clone())
        .collect();
    let (mut chunks, skipped) = partition_counted(input.section, &citable, workflow.max_rows);
    chunks.extend(attested_chunks(
        input.section,
        input.attested,
        workflow.max_rows,
    ));
    let mut stats = GatedStats {
        chunks: chunks.len(),
        rows: chunks.iter().map(|chunk| chunk.rows.len()).sum(),
        gap_rows_skipped: skipped.gap_rows,
        repeated_rows_skipped: skipped.repeated_rows,
        ..GatedStats::default()
    };
    let mut claims: Vec<Claim> = Vec::new();
    let mut left_out = 0;
    for (number, chunk) in chunks.iter().enumerate() {
        if claims.len() >= MAX_CLAIMS {
            // A draft holds at most MAX_CLAIMS claims; the rest of the
            // section stays uncovered and the stats say so.
            break;
        }
        let ids = chunk.ids();
        let facts: Vec<ComposeFact> = input
            .facts
            .iter()
            .filter(|fact| ids.contains(&fact.citation_id))
            .cloned()
            .collect();
        let attested: Vec<ComposeAttested> = input
            .attested
            .iter()
            .filter(|record| ids.contains(&record.citation_id))
            .cloned()
            .collect();
        let compose = ComposeInput {
            address: input.address,
            title: input.title,
            section: &chunk.text,
            facts: &facts,
            groups: &[],
            attested: &attested,
        };
        let base = section_compose::action(&compose, String::new());
        let base_prompt = section_compose::prompt_with(&compose, workflow.max_claims);
        let id = |step: &str, n: usize| {
            format!(
                "draft-rules:{}:{}:chunk-{number}:{step}-{n}",
                input.address.document, input.address.section
            )
        };
        // Draft, gate, retry with the gate's prescriptions.
        let mut best = attempt(
            workflow,
            input,
            chunk,
            model,
            draft_action(&base, base_prompt.clone(), id("draft", 0)),
            names_source,
            &mut stats,
        )?;
        if best.gate.passed {
            stats.first_try_passed += 1;
        }
        let mut last_reply = best.reply.clone();
        let mut last_prescriptions = best.gate.prescriptions.clone();
        let mut passed = best.gate.passed;
        for retry in 1..=workflow.max_retries {
            if passed {
                break;
            }
            stats.retries += 1;
            let next = attempt(
                workflow,
                input,
                chunk,
                model,
                draft_action(
                    &base,
                    feedback(&base_prompt, &last_reply, &last_prescriptions, ""),
                    id("draft", retry as usize),
                ),
                names_source,
                &mut stats,
            )?;
            passed = next.gate.passed;
            last_reply = next.reply.clone();
            last_prescriptions = next.gate.prescriptions.clone();
            if passed || score(&next, chunk) >= score(&best, chunk) {
                best = next;
            }
        }
        let (mut chunk_claims, dropped) = kept(&best, &mut stats);
        left_out += dropped;
        // Audit, and send unsupported claims back to the drafter.
        if let Some(round_trips) = workflow.audit_round_trips {
            let mut round = 0;
            while !chunk_claims.is_empty() {
                // An audit reply outside the schema is asked for once more;
                // claims no verdict judged are never kept.
                let mut verdicts = None;
                for again in 0..AUDIT_ATTEMPTS {
                    stats.calls += 1;
                    let reply = model.complete(&audit_action(
                        chunk,
                        &chunk_claims,
                        id(
                            if again == 0 { "audit" } else { "audit-again" },
                            round as usize,
                        ),
                    ))?;
                    verdicts = decode_audit(&reply, chunk_claims.len());
                    if verdicts.is_some() {
                        break;
                    }
                    stats.audit_failed += 1;
                }
                let Some(verdicts) = verdicts else {
                    stats.audit_dropped += chunk_claims.len();
                    left_out += chunk_claims.len();
                    chunk_claims.clear();
                    break;
                };
                let lines: Vec<String> = verdicts
                    .iter()
                    .enumerate()
                    .filter_map(|(index, verdict)| verdict.prescription(index + 1))
                    .collect();
                if lines.is_empty() {
                    break;
                }
                if round >= round_trips {
                    let before = chunk_claims.len();
                    chunk_claims = chunk_claims
                        .into_iter()
                        .zip(&verdicts)
                        .filter(|(_, verdict)| **verdict == Verdict::Supported)
                        .map(|(claim, _)| claim)
                        .collect();
                    stats.audit_dropped += before - chunk_claims.len();
                    left_out += before - chunk_claims.len();
                    break;
                }
                stats.sent_back += lines.len();
                round += 1;
                let current = serde_json::json!({
                    "claims": chunk_claims.iter().map(|claim| serde_json::json!({
                        "statement": claim.statement,
                        "claim_kind": match claim.kind {
                            ClaimKind::Implemented => "implemented_behavior",
                            ClaimKind::DocumentedIntent => "documented_intent",
                            ClaimKind::Interpretation => "inferred_interpretation",
                        },
                        "citation_ids": claim.citations,
                    })).collect::<Vec<_>>()
                })
                .to_string();
                let revised = attempt(
                    workflow,
                    input,
                    chunk,
                    model,
                    draft_action(
                        &base,
                        feedback(
                            &base_prompt,
                            &current,
                            &lines,
                            " Keep the claims the checks did not name unchanged.",
                        ),
                        id("revise", round as usize),
                    ),
                    names_source,
                    &mut stats,
                )?;
                let (revised_claims, _) = kept(&revised, &mut stats);
                if revised_claims.is_empty() {
                    // Nothing usable came back: judge the claims we had.
                    continue;
                }
                left_out += chunk_claims.len().saturating_sub(revised_claims.len());
                chunk_claims = revised_claims;
            }
        }
        claims.extend(chunk_claims);
    }
    // Assemble: chunk order. A statement drafted again word for word (only
    // spacing may differ) is one claim citing the rows of both, while they
    // fit its citation bound.
    let mut assembled: Vec<Claim> = Vec::with_capacity(claims.len());
    let mut slot: BTreeMap<String, usize> = BTreeMap::new();
    for claim in claims {
        let key = claim
            .statement
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        // When the rows of both would not fit one claim's citation bound,
        // the later claim stays its own so no row loses its evidence.
        let merged = slot.get(&key).is_some_and(|&kept| {
            let kept = &mut assembled[kept];
            let new: Vec<&String> = claim
                .citations
                .iter()
                .filter(|id| !kept.citations.contains(id))
                .collect();
            let fits = kept.citations.len() + new.len() <= MAX_CITATIONS;
            if fits {
                kept.citations.extend(new.into_iter().cloned());
            }
            fits
        });
        if !merged {
            slot.insert(key, assembled.len());
            assembled.push(claim);
        }
    }
    let mut claims = assembled;
    let before = claims.len();
    claims.truncate(MAX_CLAIMS);
    left_out += before - claims.len();
    let cited: BTreeSet<&String> = claims.iter().flat_map(|claim| &claim.citations).collect();
    stats.rows_covered = chunks
        .iter()
        .flat_map(|chunk| &chunk.rows)
        .filter(|row| row.ids.iter().any(|id| cited.contains(id)))
        .count();
    let finish = (!claims.is_empty()).then(|| SectionDraftFinish {
        address: input.address.clone(),
        claims,
    });
    Ok(GatedOutcome {
        finish,
        left_out,
        stats,
    })
}

#[cfg(test)]
#[path = "gated_compose_tests.rs"]
mod tests;
