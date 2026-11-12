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
//! **No projection.** A bare compile, the work-packet host and an MCP read
//! supply none. The document then says that none was supplied and lists
//! nothing; an empty list would not mean that no proposal was accepted.
//!
//! **Cost.** One pass over the items and one over the listed records. When
//! a `best-effort` export has items, one index of the admitted nodes by id;
//! otherwise no index. Nothing is walked.

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text};
use super::{ExportMode, citation_id};
use context_hub::curated::{
    AssertionReview, BasisComparison, BasisReport, CuratedCounts, CuratedProjection, CuratedRecord,
    CuratedView, RecordState, ReviewDecision, StateReason,
};
use core_graph::{FactKey, Node};
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

/// `curated.md` over the projection the host supplied and the nodes the
/// export mode admitted.
pub(super) fn output(
    nodes: &[&Node],
    curated: Option<&CuratedProjection>,
    mode: ExportMode,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    let mut content = Text::new(&budget, INTRO)?;
    let Some(projection) = curated else {
        writeln!(content, "Export mode: `{}`.\n", mode_name(mode))?;
        content.push_str(NO_PROJECTION)?;
        return finish(content, assertions);
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
