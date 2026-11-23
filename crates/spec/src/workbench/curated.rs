//! Curated items (#1746, SPEC-12 slice 2, ADR-0061, AC-0653): `curated.md`.
//!
//! The document lists a [`CuratedProjection`] the host computed and passed
//! in. Nothing is decided here: the states, the reasons, the counts and the
//! revision are the projection's, so the export, the Workbench and an MCP
//! read report the same thing for the same inputs. No model is called, no
//! store is read and no fact is written or changed (R-INT-1, R-INT-3).
//!
//! **Items and listed records.** An eligible accepted proposal is a curated
//! item. It is shown with the tier and confidence it was produced with
//! (R-INT-2) and is not a fact: it has no row in the fact index. Every other
//! accepted proposal is listed with its state and the reason.
//!
//! **Mode.** Every curated item is InferredWeak (ADR-0061 decision 7), so a
//! `verified-only` export shows none and states how many it left out, and a
//! `best-effort` export shows them, each marked (R-INT-5). The listed
//! records and the assertion decisions assert nothing and are the same in
//! both modes.
//!
//! **Citations.** In `best-effort`, each item cites its Gap node and the
//! source and target nodes of its proposed edge as assertions of this
//! artifact, so the fact index gives each a row and the printed citation ids
//! resolve (ADR-0058). A node is cited once. A node the export does not
//! admit, because a Workbench rejection withholds it or it is not among the
//! facts this compilation was given, is not cited: the row says so and the
//! document counts them. No citation id is printed for a node without a row.
//!
//! **Findings.** An investigation finding accepted as curated (ADR-0074,
//! AC-0845) is one statement with the facts it cites. A `best-effort`
//! export prints each eligible finding, marked InferredWeak, and cites the
//! facts it names as assertions of this artifact, each once. A finding that
//! cites a fact this export does not admit is not printed and is named. A
//! `verified-only` export prints none and states how many it left out.
//! Stale and unresolvable findings are listed with their state in both
//! modes and their statements are not repeated. The section is written only
//! when a finding is accepted.
//!
//! **No projection.** A bare compile and the work-packet host supply none.
//! The document then says that none was supplied and lists nothing; an empty
//! list would not mean that no proposal was accepted. An MCP read session
//! supplies the projection it computed (SPEC-12 slice 4, AC-0654).
//!
//! **Cost.** One pass over the items and one over the listed records. When
//! a `best-effort` export has items, one index of the admitted nodes by id;
//! otherwise no index. When it has an eligible finding, one pass over the
//! admitted nodes and edges that keeps only the cited ones. Nothing is
//! walked.

use super::document::{Fact, facts_named};
use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text};
use super::{ExportMode, citation_id};
use context_hub::curated::{
    AssertionReview, BasisComparison, BasisReport, CuratedCounts, CuratedFinding,
    CuratedProjection, CuratedRecord, CuratedView, FindingState, FindingStateReason, RecordState,
    ReviewDecision, StateReason,
};
use core_graph::{Edge, FactKey, Node};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

/// Artifact id of `curated.md`.
pub(super) const ARTIFACT_ID: &str = "curated";

/// The curated projection a bundle's `curated` artifact lists: its revision
/// and its record counts (SPEC-12 "Surfaces", AC-0653).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedBasis {
    /// The curated revision.
    pub revision: String,
    /// Accepted, eligible and each other state.
    pub counts: CuratedCounts,
}

impl CuratedBasis {
    /// The basis of `projection`.
    pub fn of(projection: &CuratedProjection) -> Self {
        Self {
            revision: projection.revision.clone(),
            counts: projection.counts,
        }
    }
}

/// What the caller supplies for the `curated` artifact (SPEC-12, AC-0653).
#[derive(Debug, Clone, Copy, Default)]
pub enum Curated<'a> {
    /// The caller computes no curated projection. The document says so.
    #[default]
    NotSupplied,
    /// The caller tried and could not. The document says so with the
    /// reason, shows no accepted proposal, and no other artifact changes.
    NotComputed(CuratedFailure),
    /// The projection the document lists.
    Computed(&'a CuratedProjection),
}

impl<'a> Curated<'a> {
    /// The projection, when one was computed.
    pub fn projection(self) -> Option<&'a CuratedProjection> {
        match self {
            Self::Computed(projection) => Some(projection),
            Self::NotSupplied | Self::NotComputed(_) => None,
        }
    }
}

/// Why a curated projection could not be computed. The recovered documents
/// do not depend on it, so an export states the reason and still succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CuratedFailure {
    /// The proposal history could not be read.
    ReviewsUnavailable,
    /// The proposal history holds more records than one read takes.
    TooManyRecords {
        /// The most records one read takes.
        limit: usize,
    },
    /// More proposals are accepted than one read takes.
    TooManyAccepted {
        /// The most accepted records one read takes.
        limit: usize,
    },
    /// The accepted section drafts, the accepted curated findings or the
    /// attested records hold more bytes than one read takes (#2040).
    AcceptedTooLarge {
        /// The most bytes of accepted drafts, of accepted findings or of
        /// attested records one read takes.
        limit_bytes: usize,
    },
    /// Two reviewed proposals share one record identity.
    DuplicateRecord,
    /// Two Workbench decisions share one content hash.
    DuplicateAssertionDecision,
    /// The current statements could not be compiled to match Workbench
    /// decisions against.
    AssertionsNotCompiled,
    /// The curated revision could not be encoded.
    RevisionNotEncoded,
    /// The Workbench decisions could not be read.
    DecisionsUnavailable,
    /// A Workbench decision matches no stored fact, so it was made on a
    /// derived statement or matches nothing. Telling which needs a compile
    /// of every document, which this read does not make. An export makes it.
    DerivedDecisionsNeedCompile,
}

impl CuratedFailure {
    /// The plain sentence `curated.md` states for this reason. The Workbench
    /// shows the same sentence (SPEC-12 slice 3, AC-0655).
    pub fn sentence(self) -> String {
        self.text().1
    }

    /// The reason code, as serialized.
    pub fn code(self) -> &'static str {
        self.text().0
    }

    /// The reason code, as serialized, and a plain sentence.
    fn text(self) -> (&'static str, String) {
        match self {
            Self::ReviewsUnavailable => (
                "reviews_unavailable",
                "The proposal history could not be read.".into(),
            ),
            Self::TooManyRecords { limit } => (
                "too_many_records",
                format!(
                    "The proposal history holds more than {limit} records, which is the most \
                     one export reads. A part of them is never shown as if it were all."
                ),
            ),
            Self::TooManyAccepted { limit } => (
                "too_many_accepted",
                format!(
                    "More than {limit} proposals are accepted, which is the most one export \
                     reads. A part of them is never shown as if it were all."
                ),
            ),
            Self::AcceptedTooLarge { limit_bytes } => (
                "accepted_too_large",
                format!(
                    "The accepted section drafts, accepted curated findings or attested \
                     records hold more than {limit_bytes} bytes, which is the most one export reads. A part of them \
                     is never shown as if it were all."
                ),
            ),
            Self::DuplicateRecord => (
                "duplicate_record",
                "Two reviewed proposals have the same record identity, so the set of reviews \
                 is ambiguous."
                    .into(),
            ),
            Self::DuplicateAssertionDecision => (
                "duplicate_assertion_decision",
                "Two Workbench decisions were made on the same statement, so the set of \
                 decisions is ambiguous."
                    .into(),
            ),
            Self::AssertionsNotCompiled => (
                "assertions_not_compiled",
                "The current statements could not be compiled to compare the Workbench \
                 decisions with."
                    .into(),
            ),
            Self::RevisionNotEncoded => (
                "revision_not_encoded",
                "The curated revision could not be encoded, and none is invented.".into(),
            ),
            Self::DecisionsUnavailable => (
                "decisions_unavailable",
                "The Workbench decisions could not be read.".into(),
            ),
            Self::DerivedDecisionsNeedCompile => (
                "derived_decisions_need_compile",
                "A Workbench decision was made on a derived statement, or no longer matches \
                 any statement. It can only be checked by compiling every document, which an \
                 agent read does not do. An export does, and shows the curated items."
                    .into(),
            ),
        }
    }
}

const INTRO: &str = "# Curated items\n\n\
Accepted agent proposals, as the curated projection reports them. A proposal is an edge that an \
agent suggested for a Gap and a person accepted. An accepted proposal whose evidence still holds \
is a curated item: it is shown beside the recovered facts and is not one of them. Nothing here \
is written to the graph, no Gap is resolved by it, and no fact's tier or confidence changes. \
Every other accepted proposal is listed with the reason it is not an item. Pending and rejected \
proposals are not listed.\n\n\
## How to read this document\n\n\
- **Revision** identifies everything this document lists: the base graph, each reviewed proposal \
with its state and reason, and the Workbench assertion decisions. The same inputs give the same \
revision. A review, a re-ingest that changes the graph or a change of policy gives a new one.\n\
- **Graph basis** is the content hash of the base graph the projection was computed over.\n\
- **A curated item** keeps the tier and confidence it was produced with. Every curated item is \
InferredWeak, so a `verified-only` export leaves all of them out and states how many, and a \
`best-effort` export includes them, each marked.\n\
- **Citations.** Each item cites its Gap node and the source and target nodes of its proposed \
edge by citation id. The fact index lists each id with its evidence. An item is not itself a \
fact and has no row there.\n\
- **A listed record** asserts nothing. The listed records are the same in both export modes.\n\
- **State** is computed when the export is compiled and is not stored. A record has the first \
state that applies, in this order:\n\
  - `unverified`: its evidence is not all captured primary-source evidence, it has no evidence, \
or it predates captured evidence.\n\
  - `already-covered`: the base graph now holds the proposed edge as a recovered fact.\n\
  - `unresolvable`: the Gap or an end of the proposed edge is no longer in the graph, or the \
node it names as its Gap is no longer a Gap.\n\
  - `stale`: the graph or the source associations changed after the proposal was made, stored \
evidence bytes are not available, or no assessment could be made.\n\
  - `conflicted`: another accepted record proposes a different edge for the same Gap.\n\
  - `eligible`: none of the above. Only these records are curated items.\n\
- **Source support** counts the evidence items of the record: the captured primary-source \
items, and the items that nothing verified.\n\
- **Basis** gives the comparisons the state was read beside: the current graph and the current \
source associations against the ones the proposal was made with, and how many evidence items \
were available. It is not a freshness flag.\n\n";

const NO_PROJECTION: &str = "No curated projection was supplied to this compilation, so nothing \
is stated here about accepted proposals. An empty list below would not mean that there are \
none.\n";

const NOT_COMPUTED: &str = "The curated projection was not computed for this export, so no \
accepted proposal is shown here, neither as a curated item nor in a list. Nothing below would \
mean that there are none. Every other document of this export is unaffected.\n";

const NO_ACCEPTED: &str = "No accepted proposal is recorded. There is no curated item and \
nothing is listed.\n";

/// The R-INT-5 mark of a curated item in a `best-effort` export.
const ITEM_NOTE: &str = "InferredWeak curated item, not a recovered fact; a verified-only export \
                         omits it";

const NOT_CITED: &str = "not cited: the node is not among the facts this export admits";

fn mode_name(mode: ExportMode) -> &'static str {
    match mode {
        ExportMode::VerifiedOnly => "verified-only",
        ExportMode::BestEffort => "best-effort",
    }
}

fn view_name(view: CuratedView) -> &'static str {
    match view {
        CuratedView::Recovered => "recovered",
        CuratedView::Curated => "curated",
    }
}

/// The state as the projection serializes it.
fn state_name(state: RecordState) -> &'static str {
    match state {
        RecordState::Unverified => "unverified",
        RecordState::AlreadyCovered => "already-covered",
        RecordState::Unresolvable => "unresolvable",
        RecordState::Stale => "stale",
        RecordState::Conflicted => "conflicted",
        RecordState::Eligible => "eligible",
    }
}

/// The reason code as the projection serializes it, and what it means.
fn reason_text(reason: StateReason) -> (&'static str, &'static str) {
    use StateReason::*;
    match reason {
        LegacyRecord => (
            "legacy_record",
            "The record predates captured evidence, so nothing ties its evidence to stored \
             source bytes.",
        ),
        UnsupportedRecordVersion => (
            "unsupported_record_version",
            "The record has a version that this policy does not know.",
        ),
        NoEvidence => ("no_evidence", "The record names no evidence item."),
        EvidenceNotCaptured => (
            "evidence_not_captured",
            "At least one evidence item was read from the working tree and was not captured as \
             primary-source evidence, so nothing verified it.",
        ),
        BaseFactFillsSlot => (
            "base_fact_fills_slot",
            "The base graph now holds the proposed edge as a recovered fact, so the proposal \
             adds nothing.",
        ),
        GapMissing => (
            "gap_missing",
            "The Gap the record names is no longer in the graph.",
        ),
        GapNoLongerGap => (
            "gap_no_longer_gap",
            "The node the record names as its Gap is still in the graph and is no longer a Gap.",
        ),
        SourceMissing => (
            "source_missing",
            "The source node of the proposed edge is no longer in the graph.",
        ),
        TargetMissing => (
            "target_missing",
            "The target node of the proposed edge is no longer in the graph.",
        ),
        AssessmentMissing => (
            "assessment_missing",
            "No basis assessment was supplied for the record, so its evidence was not checked \
             against the current graph.",
        ),
        AssessmentFailed => (
            "assessment_failed",
            "The basis assessment could not be completed.",
        ),
        GraphComparisonFailed => (
            "graph_comparison_failed",
            "The current graph could not be compared with the graph the proposal was made \
             against.",
        ),
        GraphChanged => (
            "graph_changed",
            "The graph has changed since the proposal was made.",
        ),
        AssociationComparisonFailed => (
            "association_comparison_failed",
            "The source associations of the facts the proposal used could not be compared with \
             the ones it was made with.",
        ),
        AssociationChanged => (
            "association_changed",
            "The source associations of the facts the proposal used have changed since it was \
             made.",
        ),
        CitedFilesUnchanged => (
            "cited_files_unchanged",
            "The graph or its source associations have changed since the proposal was made, \
             but every file its evidence cites is unchanged, so it can be re-confirmed as it is.",
        ),
        EvidenceNotAssessed => (
            "evidence_not_assessed",
            "A captured evidence item has no availability result.",
        ),
        EvidenceReadFailed => (
            "evidence_read_failed",
            "The stored bytes of a captured evidence item could not be read.",
        ),
        EvidenceUnavailable => (
            "evidence_unavailable",
            "The stored bytes of a captured evidence item are missing, were forgotten or no \
             longer match the record.",
        ),
        AssessmentMismatch => (
            "assessment_mismatch",
            "The basis assessment and the record disagree about the record's evidence items.",
        ),
        DifferentTargetAccepted => (
            "different_target_accepted",
            "Another accepted record proposes a different edge for the same Gap. Neither is a \
             curated item while both are accepted.",
        ),
        AllConditionsHold => ("all_conditions_hold", "Every condition holds."),
    }
}

/// The plain sentence `curated.md` prints for a record's reason code. The
/// Workbench shows the same sentence, so no surface writes a second set
/// (SPEC-12 slice 3, AC-0655).
pub fn curated_reason_sentence(reason: StateReason) -> &'static str {
    reason_text(reason).1
}

/// The SPEC-09 dimensions a record's state was read beside, as `curated.md`
/// prints them in its Basis column.
pub fn curated_basis_summary(basis: &BasisReport) -> String {
    Basis(basis).to_string()
}

fn review_name(review: ReviewDecision) -> &'static str {
    match review {
        ReviewDecision::Accepted => "accepted",
        ReviewDecision::Rejected => "rejected",
    }
}

fn assertion_review_name(review: AssertionReview) -> &'static str {
    match review {
        AssertionReview::Accepted => "accepted",
        AssertionReview::Rejected => "rejected",
        AssertionReview::Annotated => "annotated",
    }
}

fn comparison_name(comparison: BasisComparison) -> &'static str {
    match comparison {
        BasisComparison::Unchanged => "unchanged",
        BasisComparison::Changed => "changed",
        BasisComparison::LegacyUnverified => "not comparable (legacy record)",
        BasisComparison::OperationalFailure => "could not be compared",
    }
}

/// The SPEC-09 dimensions a record's state was read beside.
struct Basis<'a>(&'a BasisReport);
impl fmt::Display for Basis<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            BasisReport::NotSupplied => f.write_str("no assessment supplied"),
            BasisReport::Failed => f.write_str("assessment could not be completed"),
            BasisReport::Assessed {
                graph,
                association,
                available,
                not_available,
                unverified,
            } => write!(
                f,
                "graph {}; associations {}; evidence available {available}, not available \
                 {not_available}, unverified {unverified}",
                comparison_name(*graph),
                comparison_name(*association),
            ),
        }
    }
}

/// The DR-4 cells every record row starts with: record id, Gap, proposed
/// edge, tier and confidence as produced, review status, source support,
/// basis and state.
struct Fields<'a>(&'a CuratedRecord);
impl fmt::Display for Fields<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let record = self.0;
        write!(
            f,
            "{} | {} | {} {} {} | {:?} | {:?} | {} | captured {}, unverified {} | {} | {}",
            Literal(&record.record_id),
            Literal(&record.gap_id),
            Literal(&record.source_id),
            Literal(&record.edge_label),
            Literal(&record.target_id),
            record.tier,
            record.confidence,
            review_name(record.review_status),
            record.source_support.captured,
            record.source_support.unverified,
            Basis(&record.basis),
            state_name(record.state),
        )
    }
}

/// The three nodes an item cites, each `None` when the export does not admit
/// it.
struct Cited<'a> {
    gap: Option<&'a Node>,
    source: Option<&'a Node>,
    target: Option<&'a Node>,
}

impl Cited<'_> {
    fn roles(&self) -> [(&'static str, Option<&Node>); 3] {
        [
            ("Gap", self.gap),
            ("source", self.source),
            ("target", self.target),
        ]
    }

    fn missing(&self) -> usize {
        self.roles()
            .iter()
            .filter(|(_, node)| node.is_none())
            .count()
    }
}

impl fmt::Display for Cited<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (role, node)) in self.roles().into_iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            match node {
                Some(node) => write!(
                    f,
                    "{role} {}",
                    citation_id(&FactKey::Node {
                        id: node.id.clone()
                    })
                )?,
                None => write!(f, "{role} {NOT_CITED}")?,
            }
        }
        Ok(())
    }
}

fn counts(counts: &CuratedCounts, out: &mut Text) -> fmt::Result {
    out.push_str("\n## Counts\n\n| Records | Count |\n|---|---|\n")?;
    for (name, count) in [
        ("Accepted", counts.accepted),
        ("`eligible` (curated items)", counts.eligible),
        ("`unverified`", counts.unverified),
        ("`already-covered`", counts.already_covered),
        ("`unresolvable`", counts.unresolvable),
        ("`stale`", counts.stale),
        ("`conflicted`", counts.conflicted),
    ] {
        writeln!(out, "| {name} | {count} |")?;
    }
    Ok(())
}

fn decisions(projection: &CuratedProjection, out: &mut Text) -> fmt::Result {
    let summary = &projection.assertion_decisions;
    write!(
        out,
        "\n## Workbench assertion decisions\n\n\
         Decisions recorded in the Spec Workbench on inferred statements, each by the content \
         hash of the statement. They are the same in both export modes.\n\n\
         - Rejections applied: {}. Each withholds the statement with that content hash from \
         every artifact of this export.\n\
         - Decisions that match no current assertion: {}. No statement is produced with that \
         content hash now, so the decision has no effect. Each is listed as `stale`.\n\
         - Decision digest: {}\n",
        summary.rejections_applied,
        summary.stale.len(),
        Literal(&summary.digest),
    )?;
    if summary.stale.is_empty() {
        return Ok(());
    }
    out.push_str("\n| Content hash | Decision | State |\n|---|---|---|\n")?;
    for entry in &summary.stale {
        writeln!(
            out,
            "| {} | {} | {} |",
            Literal(&entry.content_hash),
            assertion_review_name(entry.decision),
            state_name(entry.state),
        )?;
    }
    Ok(())
}

/// The accepted section drafts with their state and the cited facts that
/// changed or are gone (ADR-0045 decisions 2 and 3, AC-0671). No claim is
/// repeated here: a document prints the claims. The section is written only
/// when a draft is accepted, so a project without one keeps its bytes.
fn drafts(projection: &CuratedProjection, out: &mut Text) -> fmt::Result {
    if projection.drafts.is_empty() {
        return Ok(());
    }
    out.push_str(
        "\n## Accepted section drafts\n\n\
         Each accepted draft of a document section, with its state. `Changed` lists the cited \
         facts whose content differs from what the draft recorded; `Missing` lists the cited ids \
         that resolve to no fact in this graph. The lists are the same in both export modes, \
         and no drafted claim is repeated here.\n\n\
         | Draft | Document | Section | Scope | State | Reason | Changed | Missing |\n\
         |---|---|---|---|---|---|---|---|\n",
    )?;
    let ids = |ids: &[String]| {
        if ids.is_empty() {
            "—".to_owned()
        } else {
            ids.join(", ")
        }
    };
    for draft in &projection.drafts {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | `{}` | {} | {} |",
            Literal(&draft.draft_id),
            Literal(&draft.address.document),
            Literal(&draft.address.section),
            Literal(&draft.address.scope),
            super::document::state_name(draft.state),
            super::document::reason_name(draft.reason),
            Literal(ids(&draft.changed)),
            Literal(ids(&draft.missing)),
        )?;
    }
    Ok(())
}

/// The state of a finding as the projection serializes it.
fn finding_state_name(state: FindingState) -> &'static str {
    match state {
        FindingState::Unresolvable => "unresolvable",
        FindingState::Stale => "stale",
        FindingState::Eligible => "eligible",
    }
}

/// The reason code of a finding's state as the projection serializes it,
/// and what it means.
fn finding_reason_text(reason: FindingStateReason) -> (&'static str, &'static str) {
    match reason {
        FindingStateReason::NoCitedFact => ("no_cited_fact", "The finding cites no fact."),
        FindingStateReason::CitedFactMissing => (
            "cited_fact_missing",
            "A fact the finding cites is not in the current graph.",
        ),
        FindingStateReason::CitedFactChanged => (
            "cited_fact_changed",
            "A fact the finding cites changed after the investigation read it.",
        ),
        FindingStateReason::AllConditionsHold => (
            "all_conditions_hold",
            "Every fact the finding cites is as the investigation read it.",
        ),
    }
}

/// What a finding state reason means, in the sentence `curated.md` prints.
/// The Workbench shows the same sentence (AC-0845).
pub fn curated_finding_reason_sentence(reason: FindingStateReason) -> &'static str {
    finding_reason_text(reason).1
}

/// Whether an export in `mode` shows `finding` as curated content: only an
/// eligible finding, and only in `best-effort`, since every finding is
/// InferredWeak (R-INT-5, ADR-0074 decision 11). Every curated read uses
/// this one rule.
pub fn curated_finding_shown(finding: &CuratedFinding, mode: ExportMode) -> bool {
    mode == ExportMode::BestEffort && finding.state == FindingState::Eligible
}

const FINDINGS_INTRO: &str = "\n## Findings accepted as curated\n\n\
Each investigation finding that a person accepted as curated. A finding is one statement an \
agent produced, with the graph facts it cites. Accepting it wrote nothing to the graph and \
changed no tier: it is T3 / InferredWeak and is not a recovered fact. A finding has the first \
state that applies: `unresolvable` when it cites no fact or a cited fact is not in the graph, \
`stale` when a cited fact changed after the investigation read it, and `eligible` otherwise. \
Source support counts the cited facts that rest on captured primary-source evidence and the \
ones that nothing verified; it does not decide the state.\n\n";

/// The findings accepted as curated (ADR-0074 decision 11, AC-0845). A
/// `best-effort` export prints the eligible ones with their citation ids
/// and asserts each cited fact once; `asserted` holds the node ids this
/// artifact already cites. The other accepted findings are listed with
/// their state in both modes and their statements are not repeated. The
/// section is written only when a finding is accepted, so a project without
/// one keeps its bytes.
fn findings<'a>(
    projection: &CuratedProjection,
    (nodes, edges): (&[&'a Node], &[&'a Edge]),
    mode: ExportMode,
    asserted: &mut BTreeSet<&'a str>,
    assertions: &mut Assertions,
    out: &mut Text,
) -> Result<(), RenderLimit> {
    let counts = projection.finding_counts;
    if counts.accepted == 0 {
        return Ok(());
    }
    out.push_str(FINDINGS_INTRO)?;
    writeln!(
        out,
        "- Accepted findings: {}. Eligible: {}. Stale: {}. Unresolvable: {}.\n",
        counts.accepted, counts.eligible, counts.stale, counts.unresolvable
    )?;
    let shown: Vec<&CuratedFinding> = projection
        .findings
        .iter()
        .filter(|finding| curated_finding_shown(finding, mode))
        .collect();
    let wanted: BTreeSet<&str> = shown
        .iter()
        .flat_map(|finding| &finding.citations)
        .map(String::as_str)
        .collect();
    let facts = facts_named(&wanted, nodes, edges);
    let mut edges_asserted: BTreeSet<&str> = BTreeSet::new();
    let mut printed = 0usize;
    let mut not_printed: Vec<&CuratedFinding> = Vec::new();
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            out,
            "No finding is shown: this is a `verified-only` export. Eligible findings left \
             out: {}.",
            counts.eligible
        )?,
        ExportMode::BestEffort => {
            for finding in shown {
                // Every id this document prints resolves in the fact index.
                if !finding
                    .citations
                    .iter()
                    .all(|id| facts.contains_key(id.as_str()))
                {
                    not_printed.push(finding);
                    continue;
                }
                printed += 1;
                write!(
                    out,
                    "{printed}. **{}** {} ({}; T3 / InferredWeak; role `{}`; human-reviewed: \
                     accepted as curated; source support: {} captured, {} unverified; finding \
                     `{}`; cites ",
                    Literal(&finding.title),
                    Literal(&finding.statement),
                    Literal(&finding.kind),
                    Literal(&finding.role),
                    finding.source_support.captured,
                    finding.source_support.unverified,
                    Literal(&finding.finding_ref),
                )?;
                for (position, id) in finding.citations.iter().enumerate() {
                    if position > 0 {
                        out.push_str(", ")?;
                    }
                    out.push_str(id)?;
                    match facts[id.as_str()] {
                        Fact::Node(node) => {
                            if asserted.insert(node.id.as_str()) {
                                assertions.node(node, None)?;
                            }
                        }
                        Fact::Edge(edge) => {
                            if edges_asserted.insert(id.as_str()) {
                                assertions.edge(edge)?;
                            }
                        }
                    }
                }
                out.push_str(")\n")?;
                for limitation in &finding.limitations {
                    writeln!(out, "   - Limitation: {}", Literal(limitation))?;
                }
            }
            if printed == 0 {
                out.push_str("No eligible finding is printed in this export.\n")?;
            }
            for finding in &not_printed {
                writeln!(
                    out,
                    "- Finding `{}` is eligible and is not printed: it cites a fact this export \
                     does not list.",
                    Literal(&finding.finding_ref)
                )?;
            }
        }
    }

    if counts.accepted == counts.eligible {
        return Ok(());
    }
    out.push_str(
        "\n### Accepted findings that are not eligible\n\n\
         These findings assert nothing in this export and their statements are not repeated. \
         `Changed` lists the cited facts that differ from what the investigation read; \
         `Missing` lists the cited ids that resolve to no fact in this graph. The list is the \
         same in both export modes.\n\n\
         | Finding | Investigation | Result | State | Reason | Changed | Missing |\n\
         |---|---|---|---|---|---|---|\n",
    )?;
    let ids = |ids: &[String]| {
        if ids.is_empty() {
            "—".to_owned()
        } else {
            ids.join(", ")
        }
    };
    for finding in &projection.findings {
        if finding.state == FindingState::Eligible {
            continue;
        }
        let (code, sentence) = finding_reason_text(finding.reason);
        writeln!(
            out,
            "| {} | {} | {} | {} | {sentence} (`{code}`) | {} | {} |",
            Literal(&finding.finding_ref),
            Literal(&finding.investigation_id),
            Literal(&finding.result_id),
            finding_state_name(finding.state),
            Literal(ids(&finding.changed)),
            Literal(ids(&finding.missing)),
        )?;
    }
    Ok(())
}

/// `curated.md` over the projection the host supplied and the nodes and
/// edges the export mode admitted.
pub(super) fn output(
    (nodes, edges): (&[&Node], &[&Edge]),
    curated: Curated<'_>,
    mode: ExportMode,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    let mut content = Text::new(&budget, INTRO)?;
    let projection = match curated {
        Curated::Computed(projection) => projection,
        Curated::NotSupplied => {
            writeln!(content, "Export mode: `{}`.\n", mode_name(mode))?;
            content.push_str(NO_PROJECTION)?;
            return finish(content, assertions);
        }
        // Nothing of a projection that failed is rendered, in either mode.
        Curated::NotComputed(failure) => {
            writeln!(content, "Export mode: `{}`.\n", mode_name(mode))?;
            content.push_str(NOT_COMPUTED)?;
            let (code, sentence) = failure.text();
            writeln!(content, "\nReason: {sentence} (`{code}`)")?;
            return finish(content, assertions);
        }
    };

    // Only a best-effort export shows items, so only it cites.
    let shown: &[CuratedRecord] = match mode {
        ExportMode::BestEffort => &projection.items,
        ExportMode::VerifiedOnly => &[],
    };
    let by_id: BTreeMap<&str, &Node> = if shown.is_empty() {
        BTreeMap::new()
    } else {
        nodes.iter().map(|node| (node.id.as_str(), *node)).collect()
    };
    let cited: Vec<Cited<'_>> = shown
        .iter()
        .map(|item| Cited {
            gap: by_id.get(item.gap_id.as_str()).copied(),
            source: by_id.get(item.source_id.as_str()).copied(),
            target: by_id.get(item.target_id.as_str()).copied(),
        })
        .collect();
    let mut once: BTreeSet<&str> = BTreeSet::new();
    for item in &cited {
        for (_, node) in item.roles() {
            if let Some(node) = node
                && once.insert(node.id.as_str())
            {
                assertions.node(node, None)?;
            }
        }
    }
    let not_cited: usize = cited.iter().map(Cited::missing).sum();

    let totals = &projection.counts;
    writeln!(
        content,
        "- View: `{}`\n\
         - Curated revision: {}\n\
         - Graph basis: {}\n\
         - Export mode: `{}`\n\
         - Accepted records: {}. Curated items: {}. Listed with a reason: {}.\n",
        view_name(projection.view),
        Literal(&projection.revision),
        Literal(&projection.graph_basis),
        mode_name(mode),
        totals.accepted,
        projection.items.len(),
        projection.listed.len(),
    )?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "Curated items left out of this export because every curated item is InferredWeak: \
             {}. A `best-effort` export includes them, each marked.",
            projection.items.len()
        )?,
        ExportMode::BestEffort => writeln!(
            content,
            "Curated items are included, each marked InferredWeak in the Note column. A \
             `verified-only` export leaves all {} out.",
            projection.items.len()
        )?,
    }
    if not_cited > 0 {
        writeln!(
            content,
            "\n{not_cited} Gap, source or target nodes of the curated items are not cited: this \
             export does not admit the node, so the fact index has no row for it. The item is \
             still shown, and its row names the node that is not cited."
        )?;
    }
    if totals.accepted == 0 {
        content.push_str("\n")?;
        content.push_str(NO_ACCEPTED)?;
    }

    content.push_str("\n## Curated items\n\n")?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "None is shown: this is a `verified-only` export. Curated items left out: {}.",
            projection.items.len()
        )?,
        ExportMode::BestEffort => {
            content.push_str(
                "| Record | Gap | Proposed edge | Tier | Confidence | Review | Source support | \
                 Basis | State | Citations | Note |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
            )?;
            for (item, cited) in shown.iter().zip(&cited) {
                writeln!(content, "| {} | {cited} | {ITEM_NOTE} |", Fields(item))?;
            }
            if shown.is_empty() {
                content.push_str(
                    "| — | — | None in this export | — | — | — | — | — | — | — | — |\n",
                )?;
            }
        }
    }

    content.push_str(
        "\n## Accepted records that are not curated items\n\n\
         These records assert nothing in this export. Each has one state and the reason for it.\n\n\
         | Record | Gap | Proposed edge | Tier | Confidence | Review | Source support | Basis | \
         State | Reason |\n|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for record in &projection.listed {
        let (code, sentence) = reason_text(record.reason);
        writeln!(content, "| {} | {sentence} (`{code}`) |", Fields(record))?;
    }
    if projection.listed.is_empty() {
        content.push_str("| — | — | None | — | — | — | — | — | — | — |\n")?;
    }

    drafts(projection, &mut content)?;
    findings(
        projection,
        (nodes, edges),
        mode,
        &mut once,
        &mut assertions,
        &mut content,
    )?;
    decisions(projection, &mut content)?;
    counts(totals, &mut content)?;
    finish(content, assertions)
}

fn finish(mut content: Text, assertions: Assertions) -> Result<Output, RenderLimit> {
    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
        attested: Vec::new(),
    })
}

/// Borrowed index roles of this producer that follow the graph, for the
/// compile workspace model. The projection is the caller's input and is not
/// copied; the citation cells, one per shown item, follow the projection and
/// not the graph, and a host that reserves this workspace supplies no
/// projection and so holds none.
pub(super) fn logical_index_bytes(rows: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?, // admitted nodes by id
        set_slots::<&str>(rows)?,        // nodes cited once
    ])
}

#[cfg(test)]
#[path = "curated_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "curated_finding_tests.rs"]
mod finding_tests;
