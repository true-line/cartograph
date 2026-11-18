//! Investigation findings accepted as curated, in the curated projection
//! (#1746 requirement 2, SPEC-12 "Findings", ADR-0074, AC-0843).
//!
//! A finding is the third record kind of the projection. A finding accepted
//! for follow-up (SPEC-03) is not one: only the separate curation review
//! brings a finding here. This module is as pure as the rest of the
//! projection: the host reads the reviewed findings and the current digest
//! of every fact they cite, and passes both in. Nothing is fetched, and no
//! fact or tier changes.
//!
//! # States
//!
//! Each accepted finding gets the first state that applies:
//!
//! - `unresolvable`: it cites no fact, or a cited id has no current digest,
//!   so it resolves to no fact in the current graph.
//! - `stale`: a cited fact's current digest differs from the digest the
//!   producing run recorded for it. This and nothing else makes a finding
//!   stale.
//! - `eligible`: neither of the above.
//!
//! A finding has no address, so two accepted findings never conflict and
//! none supersedes another. A finding with no curation review is not listed
//! and does not enter the revision. A rejected one enters the revision with
//! no state.
//!
//! # Cost
//!
//! With `F` findings and `C` citations in the accepted ones: one sort of
//! the reviewed findings and one pass that looks each citation up once.
//! Time is `O(F log F + C log C)`.

use super::{ProjectionError, SourceSupport};
use core_prov::{ConfidenceTier, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// A human curation review of a finding. It is not the follow-up review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCuration {
    /// The reviewer accepted the finding as curated.
    Accepted,
    /// The reviewer rejected it, or withdrew an earlier acceptance.
    Rejected,
}

/// One saved finding with its curation review, as the host read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedFinding {
    /// The finding's complete identity: the SPEC-03 review subject id, which
    /// binds the immutable result, its producing configuration and the
    /// result-local finding id.
    pub finding_ref: String,
    /// The saved investigation that owns the finding.
    pub investigation_id: String,
    /// The immutable result the finding is part of.
    pub result_id: String,
    /// The finding's id within that result.
    pub finding_id: String,
    /// The specialist that produced it, as `role@version`.
    pub role: String,
    /// The finding's claim kind as saved, such as `implemented_behavior`.
    pub kind: String,
    /// The finding's title.
    pub title: String,
    /// The statement, as plain text.
    pub statement: String,
    /// The limitations the finding was saved with, in order.
    pub limitations: Vec<String>,
    /// Citation id of each cited fact to the digest the producing run
    /// recorded for that fact.
    pub cited_digests: BTreeMap<String, String>,
    /// How many of its citations rest on captured primary-source evidence
    /// and how many do not, as the run recorded them.
    pub support: SourceSupport,
    /// The current curation review; `None` while there is none.
    pub decision: Option<FindingCuration>,
}

/// The one state of an accepted finding, in first-match order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingState {
    /// It cites no fact, or a cited id resolves to no current fact.
    Unresolvable,
    /// A cited fact changed since the producing run read it.
    Stale,
    /// None of the above.
    Eligible,
}

/// Why a finding has its state. One reason per finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStateReason {
    /// `unresolvable`: the finding cites no fact.
    NoCitedFact,
    /// `unresolvable`: a cited id has no fact in the current graph.
    CitedFactMissing,
    /// `stale`: a cited fact has another digest than the run recorded.
    CitedFactChanged,
    /// `eligible`: every condition holds.
    AllConditionsHold,
}

/// One accepted finding with its DR-4 fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedFinding {
    /// The finding's complete identity.
    pub finding_ref: String,
    /// The saved investigation that owns it.
    pub investigation_id: String,
    /// The immutable result it is part of.
    pub result_id: String,
    /// Its id within that result.
    pub finding_id: String,
    /// The specialist that produced it.
    pub role: String,
    /// What it asserts about.
    pub kind: String,
    /// The finding's title.
    pub title: String,
    /// The statement: the content a read shows when the finding is eligible.
    pub statement: String,
    /// The limitations it was saved with.
    pub limitations: Vec<String>,
    /// The citation ids it rests on, sorted.
    pub citations: Vec<String>,
    /// Always Agentic: acceptance changes no tier.
    pub tier: Tier,
    /// Always InferredWeak.
    pub confidence: ConfidenceTier,
    /// The review; always accepted for a finding in the projection.
    pub review_status: FindingCuration,
    /// Captured and unverified citation counts (DR-4 `source_support`).
    pub source_support: SourceSupport,
    /// The assigned state.
    pub state: FindingState,
    /// Why that state applies.
    pub reason: FindingStateReason,
    /// Cited ids whose current digest differs from the recorded one, sorted.
    /// Reported whatever the state.
    pub changed: Vec<String>,
    /// Cited ids that resolve to no fact in the current graph, sorted.
    pub missing: Vec<String>,
}

/// Finding totals. `accepted` is the sum of every state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingCounts {
    /// Accepted findings.
    pub accepted: usize,
    /// Findings a read may show as curated content.
    pub eligible: usize,
    /// Findings listed as `unresolvable`.
    pub unresolvable: usize,
    /// Findings listed as `stale`.
    pub stale: usize,
}

/// `(finding_ref, decision, state, reason)` as the revision takes it.
pub(super) type RevisionFinding<'a> = (
    &'a str,
    FindingCuration,
    Option<FindingState>,
    Option<FindingStateReason>,
);

pub(super) struct FindingProjection<'a> {
    /// Every accepted finding, ordered by identity.
    pub(super) findings: Vec<CuratedFinding>,
    pub(super) counts: FindingCounts,
    /// Every accepted or rejected finding, ordered by identity.
    pub(super) revision: Vec<RevisionFinding<'a>>,
}

/// Assign every accepted finding its state. `current` holds the current
/// digest of each cited id that resolves to a fact.
pub(super) fn project_findings<'a>(
    findings: &'a [ReviewedFinding],
    current: &BTreeMap<String, String>,
) -> Result<FindingProjection<'a>, ProjectionError> {
    // Unreviewed findings are checked too: one identity, one finding.
    let mut identities = HashSet::with_capacity(findings.len());
    if !findings
        .iter()
        .all(|finding| identities.insert(finding.finding_ref.as_str()))
    {
        return Err(ProjectionError::DuplicateRecord);
    }
    let mut reviewed: Vec<(&ReviewedFinding, FindingCuration)> = findings
        .iter()
        .filter_map(|finding| Some((finding, finding.decision?)))
        .collect();
    reviewed.sort_unstable_by(|left, right| left.0.finding_ref.cmp(&right.0.finding_ref));

    let mut listed = Vec::new();
    let mut counts = FindingCounts::default();
    let mut revision = Vec::with_capacity(reviewed.len());
    for (finding, decision) in reviewed {
        if decision == FindingCuration::Rejected {
            revision.push((finding.finding_ref.as_str(), decision, None, None));
            continue;
        }
        let mut changed = Vec::new();
        let mut missing = Vec::new();
        // `cited_digests` is ordered by id, so both lists are.
        for (id, recorded) in &finding.cited_digests {
            match current.get(id) {
                None => missing.push(id.clone()),
                Some(digest) if digest != recorded => changed.push(id.clone()),
                Some(_) => {}
            }
        }
        let (state, reason) = if finding.cited_digests.is_empty() {
            (FindingState::Unresolvable, FindingStateReason::NoCitedFact)
        } else if !missing.is_empty() {
            (
                FindingState::Unresolvable,
                FindingStateReason::CitedFactMissing,
            )
        } else if !changed.is_empty() {
            (FindingState::Stale, FindingStateReason::CitedFactChanged)
        } else {
            (
                FindingState::Eligible,
                FindingStateReason::AllConditionsHold,
            )
        };
        revision.push((
            finding.finding_ref.as_str(),
            decision,
            Some(state),
            Some(reason),
        ));
        counts.accepted += 1;
        *match state {
            FindingState::Unresolvable => &mut counts.unresolvable,
            FindingState::Stale => &mut counts.stale,
            FindingState::Eligible => &mut counts.eligible,
        } += 1;
        listed.push(CuratedFinding {
            finding_ref: finding.finding_ref.clone(),
            investigation_id: finding.investigation_id.clone(),
            result_id: finding.result_id.clone(),
            finding_id: finding.finding_id.clone(),
            role: finding.role.clone(),
            kind: finding.kind.clone(),
            title: finding.title.clone(),
            statement: finding.statement.clone(),
            limitations: finding.limitations.clone(),
            citations: finding.cited_digests.keys().cloned().collect(),
            tier: Tier::Agentic,
            confidence: ConfidenceTier::InferredWeak,
            review_status: decision,
            source_support: finding.support,
            state,
            reason,
            changed,
            missing,
        });
    }
    Ok(FindingProjection {
        findings: listed,
        counts,
        revision,
    })
}
