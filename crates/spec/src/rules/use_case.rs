//! Use-case projection over recovered rule observations, and its content
//! acceptance against an independently specified expected set (US-0035,
//! AC-0243).
//!
//! The projection reads stored `BusinessRule` guarded-exit observations only.
//! Which callables belong to the use case is declared by the caller's
//! [`UseCaseScope`]; membership is never recovered or guessed. Each decision
//! row restates one observation under the named rule
//! `spec.use-case.decision-row` with the observation's own tier and
//! confidence. Reading a row as a guard of the use case is a separate labeled
//! interpretation, `spec.use-case.guard-candidate`, at T2/InferredWeak: it is
//! never promoted to a confirmed business rule.
//!
//! [`check_use_case_acceptance`] compares the projection with an expected set
//! that the projection never reads. Each expectation is labeled implemented,
//! documented intent, inferred or unknown, and every gap is listed. The
//! current rule schema has no established interpretation, so no row can be
//! implemented today; the report says so instead of reporting completeness.

use super::render::{EvidenceText, MethodLabel};
use super::{source, text};
use crate::workbench::{identified_artifact, included};
use crate::{ExportMode, SpecArtifact, SpecAssertion, provenance};
use core_graph::rules::{
    BranchPolarity, ExpressionCapture, GuardedExitEvidence, InterpretationStatus, LocalExit,
};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write};

/// Artifact id of the use-case projection.
pub const USE_CASE_ARTIFACT_ID: &str = "use-case";
/// Rule restating one guarded-exit observation as a decision-table row.
pub const USE_CASE_ROW_RULE: &str = "spec.use-case.decision-row";
/// Rule proposing that a row may block the use case's goal (T2/InferredWeak).
pub const USE_CASE_CANDIDATE_RULE: &str = "spec.use-case.guard-candidate";
const EXTRACTOR: &str = "spec.use-case";
/// The only expected-set schema the acceptance check reads.
pub const EXPECTED_RULES_SCHEMA: &str = "cartograph.eval.expected-rules.v1";

/// Caller-declared feature boundary. Membership is declared, not recovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseCaseScope {
    /// Stable scope id, such as `cart-readiness`.
    pub id: String,
    /// Display title.
    pub title: String,
    /// The goal whose guards the decision table explains.
    pub goal: String,
    /// Callables whose exits decide the use case directly.
    pub entry_owners: Vec<String>,
    /// Callables whose exits supply values the entry decisions consume.
    pub supporting_owners: Vec<String>,
}

/// Whether a row's owner is an entry or a supporting callable of the scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowRole {
    /// Declared in [`UseCaseScope::entry_owners`].
    Entry,
    /// Declared in [`UseCaseScope::supporting_owners`].
    Supporting,
}

/// One captured branch condition, restated from the observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowCondition {
    /// Sanitized display stored by the producer.
    pub display: String,
    /// Branch polarity governing the exit's location.
    pub polarity: BranchPolarity,
    /// Syntactic capture status.
    pub capture: ExpressionCapture,
    /// Original condition span.
    pub source: EvidenceRef,
}

/// The observed local exit of a row, without consumer meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowOutcome {
    /// `return` or `throw`.
    pub kind: String,
    /// Sanitized display of the exit value, if any.
    pub display: Option<String>,
    /// Capture status of the exit value, if any.
    pub capture: Option<ExpressionCapture>,
}

/// One decision-table row: a recovered observation in a declared owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRow {
    /// Stable row id, `row:<rule node id>`.
    pub id: String,
    /// The `BusinessRule` node the row restates.
    pub rule_id: String,
    /// Owning callable.
    pub owner_id: String,
    /// Entry or supporting.
    pub role: RowRole,
    /// Lexical exit order in the owner; not runtime precedence.
    pub source_order: u32,
    /// Original exit span.
    pub exit_source: EvidenceRef,
    /// Captured branch conditions, outermost first.
    pub conditions: Vec<RowCondition>,
    /// Observed local exit.
    pub outcome: RowOutcome,
    /// Whether the full execution predicate is established.
    pub execution_predicate_established: bool,
    /// Whether the caller's interpretation of the exit is established.
    pub consumer_effect_established: bool,
    /// Explicit interpretation gaps recorded by the producer.
    pub gap_ids: Vec<String>,
    /// The restatement's provenance: the observation's tier and confidence.
    pub row: Provenance,
    /// The labeled guard interpretation, absent when the export mode or
    /// curation excludes it.
    pub candidate: Option<Provenance>,
    /// What this row lacks before it could be an implemented rule.
    pub missing: Vec<String>,
}

impl DecisionRow {
    /// Whether the row is complete enough to count as implemented: a
    /// deterministic Confirmed observation with an established predicate and
    /// consumer effect, no recorded gaps, and complete captured syntax.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// A found (documented) ADR that decides or governs a declared owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentedIntent {
    /// The ADR node id.
    pub id: String,
    /// The owner it decides or governs.
    pub owner_id: String,
    /// The ADR's own provenance.
    pub provenance: Provenance,
}

/// Recovered coverage of one declared owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerCoverage {
    /// Declared owner id.
    pub owner_id: String,
    /// Entry or supporting.
    pub role: RowRole,
    /// Whether a visible graph node exists for the owner.
    pub present: bool,
    /// Evidence spans of the owner node, if present.
    pub evidence: Vec<EvidenceRef>,
    /// Number of rows recovered for the owner.
    pub rows: usize,
    /// Documented intent attached to the owner.
    pub documented_by: Vec<String>,
}

/// The projected use case: rows, owner coverage, what is missing, and the
/// identified artifact rendering exactly this content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseCaseProjection {
    /// The declared scope.
    pub scope: UseCaseScope,
    /// Export policy applied.
    pub mode: ExportMode,
    /// Rows ordered by role, owner, lexical order and rule id.
    pub rows: Vec<DecisionRow>,
    /// Declared owners in scope order.
    pub owners: Vec<OwnerCoverage>,
    /// Documented intent found for declared owners.
    pub documented: Vec<DocumentedIntent>,
    /// Everything the projection could not establish.
    pub missing: Vec<String>,
    /// True whenever anything is missing; never reported complete then.
    pub partial: bool,
    /// The rendered artifact (schema and content identity, AC-0242).
    pub artifact: SpecArtifact,
}

fn established(status: InterpretationStatus) -> bool {
    match status {
        InterpretationStatus::NotEstablished => false,
    }
}

fn gap_reason(gap_id: &str) -> &str {
    gap_id.rsplit(':').next().unwrap_or(gap_id)
}

fn capture_name(capture: ExpressionCapture) -> &'static str {
    match capture {
        ExpressionCapture::CompleteSyntax => "complete syntax",
        ExpressionCapture::Redacted => "redacted",
        ExpressionCapture::Unsupported => "unsupported",
    }
}

fn hash_bytes(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("use-case identity serializes")
}

fn row_missing(row: &DecisionRow) -> Vec<String> {
    let mut missing = Vec::new();
    if row.row.tier != Tier::Deterministic && row.row.tier != Tier::Dynamic
        || row.row.confidence_tier != ConfidenceTier::Confirmed
    {
        missing.push(format!(
            "observation is {:?}/{:?}, not a deterministic Confirmed observation",
            row.row.tier, row.row.confidence_tier
        ));
    }
    if !row.execution_predicate_established {
        missing.push("execution predicate not established".into());
    }
    if !row.consumer_effect_established {
        missing.push("consumer effect not established".into());
    }
    for condition in &row.conditions {
        if condition.capture != ExpressionCapture::CompleteSyntax {
            missing.push(format!(
                "condition at bytes {}..{} is {}",
                condition.source.byte_start,
                condition.source.byte_end,
                capture_name(condition.capture)
            ));
        }
    }
    if let Some(capture) = row.outcome.capture
        && capture != ExpressionCapture::CompleteSyntax
    {
        missing.push(format!("exit value is {}", capture_name(capture)));
    }
    let reasons: BTreeSet<&str> = row.gap_ids.iter().map(|id| gap_reason(id)).collect();
    if !reasons.is_empty() {
        missing.push(format!(
            "open gaps: {}",
            reasons.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    missing
}

/// Project the declared use case from stored rule observations. Only
/// `BusinessRule` nodes with a valid guarded-exit payload whose owner is
/// declared in `scope` become rows; `mode` and `rejected_hashes` apply as in
/// [`crate::compile_spec`]. Deterministic for the same input in any order.
pub fn project_use_case(
    nodes: &[Node],
    edges: &[Edge],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    scope: &UseCaseScope,
) -> UseCaseProjection {
    let mut roles: BTreeMap<&str, RowRole> = BTreeMap::new();
    for owner in &scope.supporting_owners {
        roles.insert(owner.as_str(), RowRole::Supporting);
    }
    for owner in &scope.entry_owners {
        roles.insert(owner.as_str(), RowRole::Entry);
    }
    let visible: BTreeMap<&str, &Node> = nodes
        .iter()
        .filter(|node| included(&provenance(&node.props, &node.id), mode, rejected_hashes))
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut rows = Vec::new();
    for node in visible.values() {
        if node.label != "BusinessRule" {
            continue;
        }
        let Ok(rule) = super::rule_payload(node) else {
            continue;
        };
        // A guarded effect (#1998) changes state; it is not an exit that may
        // block the goal, so it is no decision row or guard candidate.
        if !rule.effect.exits() {
            continue;
        }
        let Some(&role) = roles.get(rule.owner_id.as_str()) else {
            continue;
        };
        rows.push(row(node, rule, role, mode, rejected_hashes, &scope.goal));
    }
    rows.sort_by(|left, right| {
        (left.role, &left.owner_id, left.source_order, &left.rule_id).cmp(&(
            right.role,
            &right.owner_id,
            right.source_order,
            &right.rule_id,
        ))
    });

    let mut documented = Vec::new();
    for edge in edges {
        if !matches!(edge.label.as_str(), "DECIDES" | "GOVERNS")
            || !roles.contains_key(edge.dst.as_str())
        {
            continue;
        }
        let Some(adr) = visible.get(edge.src.as_str()) else {
            continue;
        };
        if adr.label != "ADR" || adr.props["origin"].as_str() != Some("found") {
            continue;
        }
        let edge_prov = provenance(
            &edge.props,
            &format!("{} {} {}", edge.src, edge.label, edge.dst),
        );
        if !included(&edge_prov, mode, rejected_hashes) {
            continue;
        }
        documented.push(DocumentedIntent {
            id: adr.id.clone(),
            owner_id: edge.dst.clone(),
            provenance: provenance(&adr.props, &adr.id),
        });
    }
    documented.sort_by(|left, right| (&left.owner_id, &left.id).cmp(&(&right.owner_id, &right.id)));
    documented.dedup_by(|left, right| left.owner_id == right.owner_id && left.id == right.id);

    let mut seen_owners: BTreeSet<&str> = BTreeSet::new();
    let declared = scope
        .entry_owners
        .iter()
        .map(|owner| (owner, RowRole::Entry))
        .chain(
            scope
                .supporting_owners
                .iter()
                .map(|owner| (owner, RowRole::Supporting)),
        )
        .filter(|(owner, _)| seen_owners.insert(owner.as_str()));
    let owners: Vec<OwnerCoverage> = declared
        .map(|(owner, role)| OwnerCoverage {
            owner_id: owner.clone(),
            role,
            present: visible.contains_key(owner.as_str()),
            evidence: visible
                .get(owner.as_str())
                .map(|node| provenance(&node.props, &node.id).evidence)
                .unwrap_or_default(),
            rows: rows.iter().filter(|row| &row.owner_id == owner).count(),
            documented_by: documented
                .iter()
                .filter(|intent| &intent.owner_id == owner)
                .map(|intent| intent.id.clone())
                .collect(),
        })
        .collect();

    let mut missing = Vec::new();
    if owners.is_empty() {
        missing.push("scope declares no owners".into());
    }
    for owner in &owners {
        if owner.rows == 0 {
            missing.push(format!(
                "no recovered rule observation for declared owner {}",
                owner.owner_id
            ));
        }
    }
    for row in &rows {
        for item in &row.missing {
            missing.push(format!("{}: {item}", row.id));
        }
    }
    if !rows.is_empty() {
        missing.push(
            "runtime precedence between rows is not established (rows are in lexical order)".into(),
        );
        missing.push("no decision outcome has been evaluated or executed".into());
    }
    let partial = !missing.is_empty();

    let mut projection = UseCaseProjection {
        scope: scope.clone(),
        mode,
        rows,
        owners,
        documented,
        missing,
        partial,
        artifact: identified_artifact(
            USE_CASE_ARTIFACT_ID,
            "use_case.md",
            "Use case",
            "markdown",
            String::new(),
            Vec::new(),
        ),
    };
    let assertions = assertions(&projection);
    let mut content = String::new();
    render(&mut content, &projection, &assertions).expect("write to string");
    projection.artifact = identified_artifact(
        USE_CASE_ARTIFACT_ID,
        "use_case.md",
        &format!("Use case: {}", projection.scope.title),
        "markdown",
        content,
        assertions,
    );
    projection
}

fn row(
    node: &Node,
    rule: GuardedExitEvidence,
    role: RowRole,
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    goal: &str,
) -> DecisionRow {
    let observation = provenance(&node.props, &node.id);
    let conditions: Vec<RowCondition> = rule
        .conditions
        .iter()
        .map(|condition| RowCondition {
            display: condition.expression.display.as_str().to_owned(),
            polarity: condition.polarity,
            capture: condition.expression.capture,
            source: condition.expression.source.clone(),
        })
        .collect();
    let outcome = match &rule.effect {
        LocalExit::Return { value } => RowOutcome {
            kind: "return".into(),
            display: value.as_ref().map(|v| v.display.as_str().to_owned()),
            capture: value.as_ref().map(|v| v.capture),
        },
        LocalExit::Throw { value } => RowOutcome {
            kind: "throw".into(),
            display: Some(value.display.as_str().to_owned()),
            capture: Some(value.capture),
        },
        LocalExit::Assign { target, value } => RowOutcome {
            kind: "assign".into(),
            display: Some(format!(
                "{} = {}",
                target.display.as_str(),
                value.display.as_str()
            )),
            capture: Some(if target.capture == ExpressionCapture::CompleteSyntax {
                value.capture
            } else {
                target.capture
            }),
        },
        LocalExit::Call { value } => RowOutcome {
            kind: "call".into(),
            display: Some(value.display.as_str().to_owned()),
            capture: Some(value.capture),
        },
    };
    let id = format!("row:{}", node.id);
    let row_prov = Provenance::new(
        observation.tier,
        observation.confidence_tier,
        observation.evidence.clone(),
        EXTRACTOR,
        &hash_bytes(&(
            USE_CASE_ROW_RULE,
            &id,
            &rule.owner_id,
            &rule.exit_source,
            &conditions,
            &outcome,
            &observation.content_hash,
        )),
    )
    .expect("a restatement keeps the observation's own tier and confidence")
    .with_method(crate::spec_rule(USE_CASE_ROW_RULE));
    let mut evidence = vec![rule.exit_source.clone()];
    evidence.extend(conditions.iter().map(|condition| condition.source.clone()));
    let candidate = (observation.confidence_tier != ConfidenceTier::Gap)
        .then(|| {
            Provenance::new(
                Tier::Semantic,
                ConfidenceTier::InferredWeak,
                evidence,
                EXTRACTOR,
                &hash_bytes(&(USE_CASE_CANDIDATE_RULE, &id, goal, &row_prov.content_hash)),
            )
            .expect("InferredWeak is within the semantic ceiling")
            .with_method(crate::spec_rule(USE_CASE_CANDIDATE_RULE))
        })
        .filter(|candidate| included(candidate, mode, rejected_hashes));
    let mut row = DecisionRow {
        id,
        rule_id: node.id.clone(),
        owner_id: rule.owner_id.clone(),
        role,
        source_order: rule.source_order,
        exit_source: rule.exit_source.clone(),
        conditions,
        outcome,
        execution_predicate_established: established(rule.interpretation.execution_predicate),
        consumer_effect_established: established(rule.interpretation.consumer_effect),
        gap_ids: rule.interpretation.gap_ids.clone(),
        row: row_prov,
        candidate,
        missing: Vec::new(),
    };
    row.missing = row_missing(&row);
    row
}

fn assertions(projection: &UseCaseProjection) -> Vec<SpecAssertion> {
    let mut out = Vec::new();
    for row in &projection.rows {
        out.push(SpecAssertion {
            id: format!("use-case:{}", row.id),
            subject_id: row.rule_id.clone(),
            subject_kind: "UseCaseRow".into(),
            summary: format!(
                "{:?} row {} in {}: observed local {} under {} captured condition(s)",
                row.role,
                row.source_order,
                row.owner_id,
                row.outcome.kind,
                row.conditions.len()
            ),
            provenance: row.row.clone(),
            edge_label: None,
            omission: None,
        });
        if let Some(candidate) = &row.candidate {
            out.push(SpecAssertion {
                id: format!("use-case:{}:candidate", row.id),
                subject_id: row.rule_id.clone(),
                subject_kind: "UseCaseGuardCandidate".into(),
                summary: format!(
                    "Labeled interpretation (rule-derived candidate): this exit may block \"{}\"; execution predicate and consumer effect are not established",
                    projection.scope.goal
                ),
                provenance: candidate.clone(),
                edge_label: None,
                omission: None,
            });
        }
    }
    for intent in &projection.documented {
        out.push(SpecAssertion {
            id: format!("use-case:documented:{}:{}", intent.owner_id, intent.id),
            subject_id: intent.id.clone(),
            subject_kind: "DocumentedIntent".into(),
            summary: format!(
                "Documented intent {} decides {}",
                intent.id, intent.owner_id
            ),
            provenance: intent.provenance.clone(),
            edge_label: None,
            omission: None,
        });
    }
    out
}

fn polarity(value: BranchPolarity) -> &'static str {
    match value {
        BranchPolarity::TruthyBranch => "truthy",
        BranchPolarity::FalsyBranch => "falsy",
        BranchPolarity::RaisedBranch => "raised",
        BranchPolarity::UnraisedBranch => "not raised",
    }
}

fn render(
    out: &mut impl Write,
    projection: &UseCaseProjection,
    assertions: &[SpecAssertion],
) -> fmt::Result {
    let scope = &projection.scope;
    writeln!(out, "# Use case: {}\n", text(&scope.title))?;
    if projection.partial {
        writeln!(
            out,
            "**Status: PARTIAL.** {} missing item(s) are listed below; this artifact is not complete.\n",
            projection.missing.len()
        )?;
    } else {
        writeln!(out, "**Status: complete under the declared scope.**\n")?;
    }
    writeln!(
        out,
        "- Scope: `{}` (membership declared by the caller, not recovered)\n- Goal: {}\n- Export mode: {:?}\n- Rows: {} recovered observation(s) in lexical source order per owner; order is not runtime precedence.\n- Interpretation: each guard reading is a labeled rule-derived candidate (T2/InferredWeak, rule `{USE_CASE_CANDIDATE_RULE}`), never a confirmed business rule.\n",
        text(&scope.id),
        text(&scope.goal),
        projection.mode,
        projection.rows.len()
    )?;
    out.write_str("## Declared owners\n\n| Owner | Role | Node | Rows | Documented intent |\n|---|---|---|---|---|\n")?;
    for owner in &projection.owners {
        writeln!(
            out,
            "| {} | {:?} | {} | {} | {} |",
            text(&owner.owner_id),
            owner.role,
            if owner.present {
                "present"
            } else {
                "not in graph"
            },
            owner.rows,
            if owner.documented_by.is_empty() {
                "—".to_owned()
            } else {
                text(owner.documented_by.join(", ")).to_string()
            }
        )?;
    }
    out.write_str("\n## Decision table\n\n")?;
    if projection.rows.is_empty() {
        out.write_str("No recovered rule observation lies in a declared owner.\n")?;
    } else {
        out.write_str("| Row | Role | Order | When (captured conditions) | Then (local exit) | Interpretation | Evidence |\n|---|---|---|---|---|---|---|\n")?;
        for row in &projection.rows {
            let mut when = String::new();
            for (index, condition) in row.conditions.iter().enumerate() {
                if index > 0 {
                    when.push_str(" AND ");
                }
                write!(
                    when,
                    "{} [{}, {}]",
                    condition.display,
                    polarity(condition.polarity),
                    capture_name(condition.capture)
                )?;
            }
            if when.is_empty() {
                when.push_str("(no captured condition)");
            }
            let then = match (&row.outcome.display, row.outcome.capture) {
                (Some(display), Some(capture)) => {
                    format!("{} {display} [{}]", row.outcome.kind, capture_name(capture))
                }
                _ => format!("bare {}", row.outcome.kind),
            };
            let interpretation = match &row.candidate {
                Some(_) => {
                    "guard candidate (T2/InferredWeak); predicate and consumer effect not established"
                }
                None => "excluded by export mode or curation; local observation only",
            };
            writeln!(
                out,
                "| `{}` | {:?} | {} | {} | {} | {} | {} |",
                text(&row.id),
                row.role,
                row.source_order,
                text(&when),
                text(&then),
                interpretation,
                source(&row.exit_source)
            )?;
        }
    }
    out.write_str("\n## What is missing\n\n")?;
    if projection.missing.is_empty() {
        out.write_str("Nothing recorded as missing.\n")?;
    }
    for item in &projection.missing {
        writeln!(out, "- {}", text(item))?;
    }
    out.write_str("\n## Assertions and inline provenance\n\n")?;
    if assertions.is_empty() {
        return out.write_str("No graph-backed assertions were recovered for this artifact.\n");
    }
    out.write_str("| Assertion | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n|---|---|---|---|---|---|---|\n")?;
    for assertion in assertions {
        let p = &assertion.provenance;
        writeln!(
            out,
            "| {} | {:?} | {:?} | {} | {} | `{}` | `{}` |",
            text(&assertion.summary),
            p.tier,
            p.confidence_tier,
            text(MethodLabel(&p.method)),
            text(EvidenceText(p)),
            text(&p.extractor_id),
            text(&p.content_hash)
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Content acceptance (AC-0243)
// ---------------------------------------------------------------------------

/// One cited span of an expected rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedSpan {
    /// Source-reference id in the manifest.
    pub ref_id: String,
    /// Repository path.
    pub path: String,
    /// Pinned commit.
    pub commit_sha: String,
    /// Inclusive start byte.
    pub byte_start: u64,
    /// Exclusive end byte.
    pub byte_end: u64,
}

/// One independently specified expected rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedRule {
    /// Expected rule id.
    pub id: String,
    /// Category, such as `core_entry_guard`.
    pub category: String,
    /// Title.
    pub title: String,
    /// Enforcing evidence spans.
    pub spans: Vec<ExpectedSpan>,
    /// Expected dependency claim ids.
    pub dependencies: Vec<String>,
}

/// One frozen decision case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedCase {
    /// Case id.
    pub id: String,
    /// Case name.
    pub name: String,
    /// Rules the case exercises.
    pub rule_ids: Vec<String>,
}

/// The expected set the acceptance check compares against. Parsed from a
/// frozen manifest; never derived from the projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedSet {
    /// Benchmark id.
    pub benchmark_id: String,
    /// Expected rules in manifest order.
    pub rules: Vec<ExpectedRule>,
    /// Decision cases in manifest order.
    pub cases: Vec<ExpectedCase>,
    /// Negative-claim ids the manifest requires to be reviewed.
    pub negative_claim_ids: Vec<String>,
}

/// Why an expected-set manifest was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedSetError {
    /// The manifest is not `cartograph.eval.expected-rules.v1`.
    UnsupportedSchema,
    /// A required field is missing or has the wrong type.
    Malformed(&'static str),
    /// A rule cites a source reference the manifest does not define.
    UnknownSourceRef(String),
    /// A case names a rule the manifest does not define.
    UnknownRule(String),
}

impl fmt::Display for ExpectedSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema => write!(f, "expected set is not {EXPECTED_RULES_SCHEMA}"),
            Self::Malformed(field) => write!(f, "malformed expected set: {field}"),
            Self::UnknownSourceRef(id) => write!(f, "expected rule cites unknown source ref {id}"),
            Self::UnknownRule(id) => write!(f, "decision case names unknown rule {id}"),
        }
    }
}
impl std::error::Error for ExpectedSetError {}

fn field<'a>(
    value: &'a serde_json::Value,
    key: &'static str,
) -> Result<&'a serde_json::Value, ExpectedSetError> {
    value.get(key).ok_or(ExpectedSetError::Malformed(key))
}
fn string(value: &serde_json::Value, key: &'static str) -> Result<String, ExpectedSetError> {
    field(value, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or(ExpectedSetError::Malformed(key))
}
fn strings(value: &serde_json::Value, key: &'static str) -> Result<Vec<String>, ExpectedSetError> {
    field(value, key)?
        .as_array()
        .ok_or(ExpectedSetError::Malformed(key))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or(ExpectedSetError::Malformed(key))
        })
        .collect()
}
fn array<'a>(
    value: &'a serde_json::Value,
    key: &'static str,
) -> Result<&'a Vec<serde_json::Value>, ExpectedSetError> {
    field(value, key)?
        .as_array()
        .ok_or(ExpectedSetError::Malformed(key))
}

impl ExpectedSet {
    /// Read an `cartograph.eval.expected-rules.v1` manifest.
    pub fn from_manifest(manifest: &serde_json::Value) -> Result<Self, ExpectedSetError> {
        if manifest.get("schema").and_then(|v| v.as_str()) != Some(EXPECTED_RULES_SCHEMA) {
            return Err(ExpectedSetError::UnsupportedSchema);
        }
        let refs = field(manifest, "source_refs")?;
        let mut rules = Vec::new();
        for rule in array(manifest, "rules")? {
            let mut spans = Vec::new();
            for ref_id in strings(rule, "source_refs")? {
                let span = refs
                    .get(&ref_id)
                    .ok_or_else(|| ExpectedSetError::UnknownSourceRef(ref_id.clone()))?;
                let number = |key: &'static str| {
                    field(span, key)?
                        .as_u64()
                        .ok_or(ExpectedSetError::Malformed(key))
                };
                spans.push(ExpectedSpan {
                    path: string(span, "path")?,
                    commit_sha: string(span, "commit_sha")?,
                    byte_start: number("byte_start")?,
                    byte_end: number("byte_end")?,
                    ref_id,
                });
            }
            rules.push(ExpectedRule {
                id: string(rule, "id")?,
                category: string(rule, "category")?,
                title: string(rule, "title")?,
                spans,
                dependencies: strings(rule, "dependencies")?,
            });
        }
        let known: BTreeSet<&str> = rules.iter().map(|rule| rule.id.as_str()).collect();
        let mut cases = Vec::new();
        for case in array(field(manifest, "decision_case_matrix")?, "cases")? {
            let rule_ids = strings(case, "rule_ids")?;
            if let Some(unknown) = rule_ids.iter().find(|id| !known.contains(id.as_str())) {
                return Err(ExpectedSetError::UnknownRule(unknown.clone()));
            }
            cases.push(ExpectedCase {
                id: string(case, "id")?,
                name: string(case, "name")?,
                rule_ids,
            });
        }
        let negative_claim_ids = match manifest.get("negative_claims") {
            None => Vec::new(),
            Some(claims) => claims
                .as_array()
                .ok_or(ExpectedSetError::Malformed("negative_claims"))?
                .iter()
                .map(|claim| string(claim, "id"))
                .collect::<Result<_, _>>()?,
        };
        Ok(Self {
            benchmark_id: string(manifest, "benchmark_id")?,
            rules,
            cases,
            negative_claim_ids,
        })
    }
}

/// How an expectation is supported, ordered weakest to strongest:
/// `Unknown < DocumentedIntent < Inferred < Implemented`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectationLabel {
    /// No recovered or documented support.
    Unknown,
    /// Only a found ADR deciding an owner whose evidence covers the span.
    DocumentedIntent,
    /// A recovered observation at the enforcing span, read through a labeled
    /// interpretation; something the rule needs is not established.
    Inferred,
    /// A complete recovered row: deterministic Confirmed observation with an
    /// established predicate and consumer effect and no open gap.
    Implemented,
}

/// The label and gaps for one expected rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectationResult {
    /// Expected rule id.
    pub id: String,
    /// Category.
    pub category: String,
    /// Title.
    pub title: String,
    /// Label.
    pub label: ExpectationLabel,
    /// Rows assigned to this expectation.
    pub row_ids: Vec<String>,
    /// Documented intent supporting it.
    pub documented_by: Vec<String>,
    /// What prevents a stronger label.
    pub missing: Vec<String>,
}

/// The label for one decision case: the weakest label among its rules,
/// never above `Inferred` because no case is executed here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseResult {
    /// Case id.
    pub id: String,
    /// Label.
    pub label: ExpectationLabel,
    /// Always `not_executed`.
    pub execution_status: String,
    /// Rules below `Inferred` and the execution gap.
    pub missing: Vec<String>,
}

/// Content-acceptance result for one projection against one expected set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceReport {
    /// Expected set benchmark id.
    pub benchmark_id: String,
    /// Content identity of the checked artifact.
    pub artifact_content_id: String,
    /// True only when every expectation and case is implemented and nothing
    /// is unchecked. Never true for a partial projection.
    pub complete: bool,
    /// Expectation counts by label.
    pub expectation_labels: BTreeMap<ExpectationLabel, usize>,
    /// Case counts by label.
    pub case_labels: BTreeMap<ExpectationLabel, usize>,
    /// Per-expectation results in manifest order.
    pub expectations: Vec<ExpectationResult>,
    /// Per-case results in manifest order.
    pub cases: Vec<CaseResult>,
    /// Rows matching no expectation: reviewed extras, not false claims.
    pub extra_row_ids: Vec<String>,
    /// Requirements of the expected set this check does not evaluate.
    pub unchecked: Vec<String>,
}

fn contains(span: &ExpectedSpan, reference: &EvidenceRef) -> bool {
    span.path == reference.path
        && span.commit_sha == reference.commit_sha
        && span.byte_start <= reference.byte_start
        && reference.byte_end <= span.byte_end
}

fn overlaps(span: &ExpectedSpan, reference: &EvidenceRef) -> bool {
    span.path == reference.path
        && span.commit_sha == reference.commit_sha
        && span.byte_start < reference.byte_end
        && reference.byte_start < span.byte_end
}

/// Check a projection against an independently specified expected set.
///
/// A row is assigned to the expected rule whose cited span most narrowly
/// contains its exit (same path and commit); ties assign it to every tied
/// rule. Labels: `Implemented` needs a complete row; `Inferred` any assigned
/// row; `DocumentedIntent` a found ADR deciding a declared owner whose
/// evidence overlaps an expected span; otherwise `Unknown`.
pub fn check_use_case_acceptance(
    projection: &UseCaseProjection,
    expected: &ExpectedSet,
) -> AcceptanceReport {
    let mut assigned: BTreeMap<&str, Vec<&DecisionRow>> = BTreeMap::new();
    let mut extra_row_ids = Vec::new();
    for row in &projection.rows {
        let widths: Vec<(&str, u64)> = expected
            .rules
            .iter()
            .filter_map(|rule| {
                rule.spans
                    .iter()
                    .filter(|span| contains(span, &row.exit_source))
                    .map(|span| span.byte_end - span.byte_start)
                    .min()
                    .map(|width| (rule.id.as_str(), width))
            })
            .collect();
        match widths.iter().map(|(_, width)| *width).min() {
            None => extra_row_ids.push(row.id.clone()),
            Some(narrowest) => {
                for (id, width) in widths {
                    if width == narrowest {
                        assigned.entry(id).or_default().push(row);
                    }
                }
            }
        }
    }

    let mut expectations = Vec::new();
    for rule in &expected.rules {
        let rows = assigned.get(rule.id.as_str()).cloned().unwrap_or_default();
        let documented_by: Vec<String> = projection
            .owners
            .iter()
            .filter(|owner| {
                owner
                    .evidence
                    .iter()
                    .any(|evidence| rule.spans.iter().any(|span| overlaps(span, evidence)))
            })
            .flat_map(|owner| owner.documented_by.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut missing = Vec::new();
        let label = if rows.iter().any(|row| row.is_complete()) {
            ExpectationLabel::Implemented
        } else if !rows.is_empty() {
            for row in &rows {
                for item in &row.missing {
                    missing.push(format!("{}: {item}", row.id));
                }
            }
            ExpectationLabel::Inferred
        } else {
            let spans: Vec<&str> = rule.spans.iter().map(|span| span.ref_id.as_str()).collect();
            missing.push(format!(
                "no recovered rule observation within expected span(s) {}",
                spans.join(", ")
            ));
            if documented_by.is_empty() {
                ExpectationLabel::Unknown
            } else {
                ExpectationLabel::DocumentedIntent
            }
        };
        if !rule.dependencies.is_empty() {
            missing.push(format!(
                "dependency claims not checked: {}",
                rule.dependencies.join(", ")
            ));
        }
        expectations.push(ExpectationResult {
            id: rule.id.clone(),
            category: rule.category.clone(),
            title: rule.title.clone(),
            label,
            row_ids: rows.iter().map(|row| row.id.clone()).collect(),
            documented_by,
            missing,
        });
    }

    let by_id: BTreeMap<&str, ExpectationLabel> = expectations
        .iter()
        .map(|result| (result.id.as_str(), result.label))
        .collect();
    let cases: Vec<CaseResult> = expected
        .cases
        .iter()
        .map(|case| {
            let weakest = case
                .rule_ids
                .iter()
                .map(|id| {
                    by_id
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(ExpectationLabel::Unknown)
                })
                .min()
                .unwrap_or(ExpectationLabel::Unknown);
            let mut missing: Vec<String> = case
                .rule_ids
                .iter()
                .filter(|id| {
                    by_id
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(ExpectationLabel::Unknown)
                        < ExpectationLabel::Inferred
                })
                .map(|id| format!("{id} has no recovered row"))
                .collect();
            missing.push("decision outcome not executed or evaluated".into());
            CaseResult {
                id: case.id.clone(),
                label: weakest.min(ExpectationLabel::Inferred),
                execution_status: "not_executed".into(),
                missing,
            }
        })
        .collect();

    let mut unchecked = vec![
        "rule predicate, scope, configuration gate and consumer effect text are not compared; a row only locates evidence".to_owned(),
    ];
    if !expected.negative_claim_ids.is_empty() {
        unchecked.push(format!(
            "negative claims not reviewed: {}",
            expected.negative_claim_ids.join(", ")
        ));
    }
    let count = |labels: &mut dyn Iterator<Item = ExpectationLabel>| {
        let mut counts = BTreeMap::new();
        for label in labels {
            *counts.entry(label).or_insert(0) += 1;
        }
        counts
    };
    let expectation_labels = count(&mut expectations.iter().map(|result| result.label));
    let case_labels = count(&mut cases.iter().map(|result| result.label));
    let complete = !projection.partial
        && unchecked.is_empty()
        && expectations
            .iter()
            .all(|result| result.label == ExpectationLabel::Implemented)
        && cases
            .iter()
            .all(|result| result.label == ExpectationLabel::Implemented);
    AcceptanceReport {
        benchmark_id: expected.benchmark_id.clone(),
        artifact_content_id: projection.artifact.content_id.clone(),
        complete,
        expectation_labels,
        case_labels,
        expectations,
        cases,
        extra_row_ids,
        unchecked,
    }
}

fn label_name(label: ExpectationLabel) -> &'static str {
    match label {
        ExpectationLabel::Implemented => "implemented",
        ExpectationLabel::DocumentedIntent => "documented intent",
        ExpectationLabel::Inferred => "inferred",
        ExpectationLabel::Unknown => "unknown",
    }
}

impl AcceptanceReport {
    /// Portable Markdown rendering of the report.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        self.write_markdown(&mut out).expect("write to string");
        out
    }

    fn write_markdown(&self, out: &mut impl Write) -> fmt::Result {
        writeln!(out, "# Content acceptance: {}\n", text(&self.benchmark_id))?;
        if self.complete {
            out.write_str("**Status: complete.**\n\n")?;
        } else {
            out.write_str(
                "**Status: PARTIAL — not complete.** Missing items are listed per expectation.\n\n",
            )?;
        }
        writeln!(
            out,
            "- Checked artifact: `{}`",
            text(&self.artifact_content_id)
        )?;
        for (title, counts, total) in [
            (
                "Expectations",
                &self.expectation_labels,
                self.expectations.len(),
            ),
            ("Decision cases", &self.case_labels, self.cases.len()),
        ] {
            let parts: Vec<String> = [
                ExpectationLabel::Implemented,
                ExpectationLabel::DocumentedIntent,
                ExpectationLabel::Inferred,
                ExpectationLabel::Unknown,
            ]
            .into_iter()
            .map(|label| {
                format!(
                    "{} {}",
                    counts.get(&label).copied().unwrap_or(0),
                    label_name(label)
                )
            })
            .collect();
            writeln!(out, "- {title} ({total}): {}", parts.join(", "))?;
        }
        out.write_str("\n## Expectations\n\n| Expectation | Category | Label | Rows | Missing |\n|---|---|---|---|---|\n")?;
        for result in &self.expectations {
            writeln!(
                out,
                "| {} — {} | {} | {} | {} | {} |",
                text(&result.id),
                text(&result.title),
                text(&result.category),
                label_name(result.label),
                if result.row_ids.is_empty() {
                    "—".to_owned()
                } else {
                    text(result.row_ids.join("; ")).to_string()
                },
                text(result.missing.join("; "))
            )?;
        }
        out.write_str(
            "\n## Decision cases\n\n| Case | Label | Execution | Missing |\n|---|---|---|---|\n",
        )?;
        for case in &self.cases {
            writeln!(
                out,
                "| {} | {} | {} | {} |",
                text(&case.id),
                label_name(case.label),
                text(&case.execution_status),
                text(case.missing.join("; "))
            )?;
        }
        out.write_str("\n## Not checked\n\n")?;
        for item in &self.unchecked {
            writeln!(out, "- {}", text(item))?;
        }
        if !self.extra_row_ids.is_empty() {
            out.write_str("\n## Rows matching no expectation\n\n")?;
            for id in &self.extra_row_ids {
                writeln!(out, "- `{}`", text(id))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "use_case_tests.rs"]
mod tests;
