//! The curated read of the Spec Workbench (SPEC-12 slice 3, ADR-0061,
//! AC-0655).
//!
//! It is not a second read. `export_spec_with_views` returns it beside the
//! bundle, made from the one projection that compile computed, so its
//! revision and counts are the bundle's `curated_basis` by construction and
//! the Workbench cannot show a projection of another revision.
//!
//! Each record carries its reason code and the sentence `curated.md` prints
//! for that code, so the UI maps no code to prose. Every curated item is
//! InferredWeak (ADR-0061 decision 7): a `verified-only` read returns no
//! item, only the number left out (R-INT-5), as the export and the MCP read
//! do. Listed records and assertion decisions assert nothing and are the
//! same in both modes.
//!
//! A finding accepted as curated (ADR-0074, AC-0845) is InferredWeak too:
//! a `best-effort` read returns each eligible finding with its statement
//! and citation ids, a `verified-only` read returns none and the number
//! left out, and both list every other accepted finding with its state and
//! reason and without its statement. The MCP `curated-json` read is built
//! from the same split.
//!
//! Nothing is written and no fact, tier or review changes (R-INT-1,
//! R-INT-3).

use context_hub::curated::{
    AssertionDecisionSummary, AttestedAnswer, AttestedCounts, AttestedState, AttestedStateReason,
    CuratedAttested, CuratedCounts, CuratedDraft, CuratedFinding, CuratedProjection, CuratedRecord,
    CuratedView, DraftAddress, DraftCounts, DraftState, DraftStateReason, FindingCounts,
    FindingState, FindingStateReason,
};
use serde::Serialize;
use spec::{CuratedFailure, ExportMode};

use crate::CuratedInput;

/// One accepted record with its DR-4 fields and the sentences for them.
#[derive(Debug, Serialize)]
pub(crate) struct CuratedRow {
    #[serde(flatten)]
    record: CuratedRecord,
    /// What `reason` means, as `curated.md` states it.
    reason_sentence: &'static str,
    /// The SPEC-09 dimensions of `basis`, as `curated.md` prints them.
    basis_summary: String,
}

impl CuratedRow {
    fn of(record: &CuratedRecord) -> Self {
        Self {
            reason_sentence: spec::curated_reason_sentence(record.reason),
            basis_summary: spec::curated_basis_summary(&record.basis),
            record: record.clone(),
        }
    }
}

/// One accepted section draft with its state and the sentence for its
/// reason (SPEC-15 slice 7, AC-0669). It carries no claim: the document
/// view shows the claims from the draft store, and no InferredWeak claim
/// is repeated in a `verified-only` read.
#[derive(Debug, Serialize)]
pub(crate) struct DraftRow {
    draft_id: String,
    address: DraftAddress,
    role: String,
    replaces: Option<String>,
    cited_facts: usize,
    state: DraftState,
    reason: DraftStateReason,
    /// What `reason` means, in one sentence.
    reason_sentence: &'static str,
    /// Cited ids whose fact changed since the draft recorded it, sorted
    /// (ADR-0045, AC-0671).
    changed: Vec<String>,
    /// Cited ids that resolve to no fact now, sorted.
    missing: Vec<String>,
}

/// What a draft state reason means.
pub(crate) fn draft_reason_sentence(reason: DraftStateReason) -> &'static str {
    match reason {
        DraftStateReason::ReplacedByAcceptedDraft => {
            "An accepted draft for the same section replaces this one."
        }
        DraftStateReason::CitedFactMissing => {
            "A fact this draft cites is not in the current graph."
        }
        DraftStateReason::CitedFactChanged => {
            "A fact this draft cites changed after the draft was written; a best-effort export prints it marked stale."
        }
        DraftStateReason::AnotherDraftAccepted => {
            "Another accepted draft is for the same section and scope."
        }
        DraftStateReason::AllConditionsHold => {
            "Every fact this draft cites is unchanged; a best-effort export prints it."
        }
    }
}

impl DraftRow {
    fn of(draft: &CuratedDraft) -> Self {
        Self {
            draft_id: draft.draft_id.clone(),
            address: draft.address.clone(),
            role: draft.role.clone(),
            replaces: draft.replaces.clone(),
            cited_facts: draft.cited_facts,
            state: draft.state,
            reason: draft.reason,
            reason_sentence: draft_reason_sentence(draft.reason),
            changed: draft.changed.clone(),
            missing: draft.missing.clone(),
        }
    }
}

/// One finding a read shows as curated content, with the sentence for its
/// reason. It is always eligible.
#[derive(Debug, Serialize)]
pub(crate) struct FindingRow {
    #[serde(flatten)]
    finding: CuratedFinding,
    /// What `reason` means, as `curated.md` states it.
    reason_sentence: &'static str,
}

/// One accepted finding that is not eligible: its identity, state and the
/// cited ids behind the state. It carries no statement and no title, so a
/// read repeats no InferredWeak content it does not show.
#[derive(Debug, Serialize)]
pub(crate) struct FindingListing {
    finding_ref: String,
    investigation_id: String,
    result_id: String,
    finding_id: String,
    role: String,
    cited_facts: usize,
    state: FindingState,
    reason: FindingStateReason,
    /// What `reason` means, as `curated.md` states it.
    reason_sentence: &'static str,
    /// Cited ids whose fact changed since the investigation read it.
    changed: Vec<String>,
    /// Cited ids that resolve to no fact now.
    missing: Vec<String>,
}

/// The findings of a projection as one export mode reads them (ADR-0074
/// decision 11). `shown` plus `omitted` plus `listed` is every accepted
/// finding.
#[derive(Debug, Serialize)]
pub(crate) struct FindingsRead {
    /// The eligible findings this export mode shows.
    #[serde(rename = "findings")]
    shown: Vec<FindingRow>,
    /// Eligible findings the export mode leaves out.
    #[serde(rename = "findings_omitted")]
    omitted: usize,
    /// Every accepted finding that is not eligible; the same in both modes.
    #[serde(rename = "findings_listed")]
    listed: Vec<FindingListing>,
    finding_counts: FindingCounts,
}

impl FindingsRead {
    pub(crate) fn of(projection: &CuratedProjection, mode: ExportMode) -> Self {
        let mut shown = Vec::new();
        let mut listed = Vec::new();
        let mut omitted = 0;
        for finding in &projection.findings {
            let reason_sentence = spec::curated_finding_reason_sentence(finding.reason);
            if spec::curated_finding_shown(finding, mode) {
                shown.push(FindingRow {
                    finding: finding.clone(),
                    reason_sentence,
                });
            } else if finding.state == FindingState::Eligible {
                // R-INT-5: no InferredWeak finding, and the number left out.
                omitted += 1;
            } else {
                listed.push(FindingListing {
                    finding_ref: finding.finding_ref.clone(),
                    investigation_id: finding.investigation_id.clone(),
                    result_id: finding.result_id.clone(),
                    finding_id: finding.finding_id.clone(),
                    role: finding.role.clone(),
                    cited_facts: finding.citations.len(),
                    state: finding.state,
                    reason: finding.reason,
                    reason_sentence,
                    changed: finding.changed.clone(),
                    missing: finding.missing.clone(),
                });
            }
        }
        Self {
            shown,
            omitted,
            listed,
            finding_counts: projection.finding_counts,
        }
    }
}

/// One attested record a read shows, with the label it prints: `attested`
/// for an engineer-backed one, `agent-attested, unconfirmed` when only
/// agents attest it (ADR-0090 decision 7).
#[derive(Debug, Serialize)]
pub(crate) struct AttestedRow {
    #[serde(flatten)]
    record: CuratedAttested,
    label: &'static str,
}

/// One attested record a read does not show: its identity, state and the
/// cited ids behind the state, with no statement.
#[derive(Debug, Serialize)]
pub(crate) struct AttestedListing {
    record_id: String,
    citation_id: String,
    answer: AttestedAnswer,
    cited_facts: usize,
    state: AttestedState,
    label: &'static str,
    reason: AttestedStateReason,
    changed: Vec<String>,
    missing: Vec<String>,
}

/// The attested records of a projection as one export mode reads them
/// (ADR-0090 decisions 7 and 8). `best-effort` shows the eligible and the
/// agent-attested, unconfirmed ones; `verified-only` shows neither and
/// counts them. Every other record is listed without its statement in both
/// modes. `shown` plus `omitted` plus `listed` is every projected record.
#[derive(Debug, Serialize)]
pub(crate) struct AttestedRead {
    #[serde(rename = "attested")]
    shown: Vec<AttestedRow>,
    #[serde(rename = "attested_omitted")]
    omitted: usize,
    #[serde(rename = "attested_listed")]
    listed: Vec<AttestedListing>,
    attested_counts: AttestedCounts,
}

impl AttestedRead {
    pub(crate) fn of(projection: &CuratedProjection, mode: ExportMode) -> Self {
        let mut shown = Vec::new();
        let mut listed = Vec::new();
        let mut omitted = 0;
        for record in &projection.attested {
            if let Some(label) = record.printed_label() {
                if mode == ExportMode::BestEffort {
                    shown.push(AttestedRow {
                        record: record.clone(),
                        label,
                    });
                } else {
                    omitted += 1;
                }
            } else {
                let label = record.state.label();
                listed.push(AttestedListing {
                    record_id: record.record_id.clone(),
                    citation_id: record.citation_id.clone(),
                    answer: record.answer.clone(),
                    cited_facts: record.citations.len(),
                    state: record.state,
                    label,
                    reason: record.reason,
                    changed: record.changed.clone(),
                    missing: record.missing.clone(),
                });
            }
        }
        Self {
            shown,
            omitted,
            listed,
            attested_counts: projection.attested_counts,
        }
    }
}

/// What the Workbench reads of the curated projection.
// One value is built per read and serialized at once; boxing a field would
// only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CuratedWorkbenchRead {
    /// The projection, with the items this export mode shows.
    Computed {
        export_mode: ExportMode,
        view: CuratedView,
        revision: String,
        graph_basis: String,
        items: Vec<CuratedRow>,
        /// Curated items the export mode leaves out.
        items_omitted: usize,
        listed: Vec<CuratedRow>,
        /// Every accepted section draft with its state; the same in both
        /// modes, and without claims.
        drafts: Vec<DraftRow>,
        draft_counts: DraftCounts,
        /// `findings`, `findings_omitted`, `findings_listed` and
        /// `finding_counts`.
        #[serde(flatten)]
        findings: FindingsRead,
        assertion_decisions: AssertionDecisionSummary,
        counts: CuratedCounts,
    },
    /// It was not computed. `reason` (and `limit`, where the reason has one)
    /// is the failure as the export serializes it; `sentence` is the one
    /// `curated.md` states.
    NotComputed {
        export_mode: ExportMode,
        #[serde(flatten)]
        failure: CuratedFailure,
        sentence: String,
    },
}

impl CuratedWorkbenchRead {
    fn computed(projection: &CuratedProjection, mode: ExportMode) -> Self {
        let shown = match mode {
            ExportMode::BestEffort => projection.items.as_slice(),
            // R-INT-5: no InferredWeak item, and the number left out.
            ExportMode::VerifiedOnly => &[],
        };
        Self::Computed {
            export_mode: mode,
            view: projection.view,
            revision: projection.revision.clone(),
            graph_basis: projection.graph_basis.clone(),
            items: shown.iter().map(CuratedRow::of).collect(),
            items_omitted: projection.items.len() - shown.len(),
            listed: projection.listed.iter().map(CuratedRow::of).collect(),
            drafts: projection.drafts.iter().map(DraftRow::of).collect(),
            draft_counts: projection.draft_counts,
            findings: FindingsRead::of(projection, mode),
            assertion_decisions: projection.assertion_decisions.clone(),
            counts: projection.counts,
        }
    }
}

impl CuratedWorkbenchRead {
    /// The read of the projection one compilation computed. `None` when
    /// that compilation did not read the projection at all.
    pub(crate) fn of(curated: &CuratedInput, mode: ExportMode) -> Option<Self> {
        match curated {
            CuratedInput::Computed(projection) => Some(Self::computed(projection, mode)),
            CuratedInput::NotComputed(failure) => Some(Self::NotComputed {
                export_mode: mode,
                failure: *failure,
                sentence: failure.sentence(),
            }),
            CuratedInput::NotRead => None,
        }
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;
    use context_hub::curated::{
        CURATED_POLICY_VERSION, DraftClaim, DraftReview, ProjectionInput, ReviewedDraft, project,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn accepted(draft_id: &str, hash: &str) -> ReviewedDraft {
        ReviewedDraft {
            draft_id: draft_id.into(),
            address: DraftAddress {
                catalogue_version: "1.0".into(),
                document: "D1".into(),
                section: "1.3".into(),
                scope: draft_id.into(),
            },
            role: "section-writer@1".into(),
            claims: vec![DraftClaim {
                statement: "A drafted claim.".into(),
                kind: "implemented".into(),
                citations: vec!["F-0000000000000001".into()],
            }],
            cited_hashes: BTreeMap::from([("F-0000000000000001".to_owned(), hash.to_owned())]),
            replaces: None,
            decision: Some(DraftReview::Accepted),
        }
    }

    // AC-0669: the Workbench read names every accepted draft with its state
    // and the sentence for its reason, in both modes, and repeats no claim.
    #[test]
    fn the_read_lists_accepted_drafts_with_state_and_reason_and_no_claim() {
        let drafts = [accepted("draft-a", "current"), accepted("draft-b", "old")];
        let projection = project(&ProjectionInput {
            nodes: &[],
            edges: &[],
            graph_content_hash: "graph",
            proposals: &[],
            assessments: &BTreeMap::new(),
            assertion_decisions: &[],
            current_assertion_hashes: &BTreeSet::new(),
            drafts: &drafts,
            cited_fact_hashes: &BTreeMap::from([(
                "F-0000000000000001".to_owned(),
                "current".to_owned(),
            )]),
            findings: &[],
            cited_fact_digests: &std::collections::BTreeMap::new(),
            attested: &[],
            policy_version: CURATED_POLICY_VERSION,
        })
        .unwrap();
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let read =
                serde_json::to_value(CuratedWorkbenchRead::computed(&projection, mode)).unwrap();
            assert_eq!(read["status"], "computed");
            assert_eq!(read["draft_counts"]["accepted"], 2);
            assert_eq!(read["draft_counts"]["eligible"], 1);
            assert_eq!(read["draft_counts"]["stale"], 1);
            let rows = read["drafts"].as_array().unwrap();
            assert_eq!(rows.len(), 2);
            let row = |id: &str| rows.iter().find(|row| row["draft_id"] == id).unwrap();
            assert_eq!(row("draft-a")["state"], "eligible");
            assert_eq!(row("draft-a")["reason"], "all_conditions_hold");
            assert_eq!(row("draft-b")["state"], "stale");
            assert_eq!(row("draft-b")["reason"], "cited_fact_changed");
            assert_eq!(
                row("draft-b")["reason_sentence"],
                draft_reason_sentence(DraftStateReason::CitedFactChanged)
            );
            // AC-0671: the facts behind the state are named.
            assert_eq!(
                row("draft-b")["changed"],
                serde_json::json!(["F-0000000000000001"])
            );
            assert_eq!(row("draft-b")["missing"], serde_json::json!([]));
            assert_eq!(row("draft-a")["changed"], serde_json::json!([]));
            assert_eq!(row("draft-a")["address"]["section"], "1.3");
            assert_eq!(row("draft-a")["cited_facts"], 1);
            for row in rows {
                assert!(row.get("claims").is_none());
            }
            assert!(!read.to_string().contains("A drafted claim."));
        }
    }
}
