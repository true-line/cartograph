//! Accepted section drafts in the curated projection (#1748, SPEC-15 slice
//! 4, ADR-0064 decisions 7 and 8, AC-0666).
//!
//! A draft is the second record kind of the projection. This module is as
//! pure as the rest of it: the host reads the stored drafts and the current
//! content hash of every fact they cite, and passes both in. Nothing is
//! fetched, and no fact or tier changes.
//!
//! # States
//!
//! Each accepted draft gets the first state that applies:
//!
//! - `superseded`: another accepted draft for the same address names this
//!   one as the draft it replaces.
//! - `unresolvable`: a cited id has no current content hash, so it resolves
//!   to no fact in the current graph. An `A-` id resolves while its attested
//!   record is `eligible` or `agent-attested, unconfirmed`; in any other
//!   state the draft is unresolvable (ADR-0090 decision 6).
//! - `stale`: a cited fact's current content hash differs from the hash the
//!   draft recorded. This and nothing else makes a draft stale.
//! - `conflicted`: another accepted draft has the same address and no
//!   accepted draft replaces it. Every such draft counts, whatever its own
//!   state; a draft a replacement chain has superseded does not.
//! - `eligible`: none of the above.
//!
//! A pending or returned draft is not listed and does not enter the
//! revision. A rejected draft enters the revision with no state.
//!
//! # Cost
//!
//! With `D` drafts and `C` citations in the accepted ones: one sort of the
//! reviewed drafts, one pass to index the accepted drafts by address and by
//! the draft they replace, and one pass that looks each citation up once.
//! Time is `O(D log D + C log C)`; no draft is compared with every other.

use super::ProjectionError;
use core_prov::{ConfidenceTier, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// One section of one catalogue document, for one subject or the system.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DraftAddress {
    /// The catalogue version of the section table.
    pub catalogue_version: String,
    /// `D1`..`D11`.
    pub document: String,
    /// The catalogue number, such as `1.3`.
    pub section: String,
    /// `system`, or the citation id of the subject.
    pub scope: String,
}

/// A human review of a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftReview {
    /// The reviewer accepted the draft.
    Accepted,
    /// The reviewer rejected the draft.
    Rejected,
    /// The reviewer sent the draft back.
    Returned,
}

/// One statement of a draft with the citation ids it rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftClaim {
    /// The statement, as plain text.
    pub statement: String,
    /// What it asserts about: `implemented`, `documented_intent` or
    /// `interpretation`.
    pub kind: String,
    /// The citation ids it rests on, in the order given.
    pub citations: Vec<String>,
}

/// One stored draft as the host read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedDraft {
    /// The draft's identity in the draft store.
    pub draft_id: String,
    /// The section it was drafted for.
    pub address: DraftAddress,
    /// The author role.
    pub role: String,
    /// The claims, in reading order.
    pub claims: Vec<DraftClaim>,
    /// Citation id to the content hash the draft recorded for it.
    pub cited_hashes: BTreeMap<String, String>,
    /// The returned draft this one replaces.
    pub replaces: Option<String>,
    /// The current review; `None` while pending.
    pub decision: Option<DraftReview>,
}

/// The one state of an accepted draft, in first-match order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DraftState {
    /// An accepted successor replaces it.
    Superseded,
    /// A cited id resolves to no current fact.
    Unresolvable,
    /// A cited fact changed since the draft recorded it.
    Stale,
    /// Another accepted draft is for the same address.
    Conflicted,
    /// None of the above.
    Eligible,
}

/// Why a draft has its state. One reason per draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStateReason {
    /// `superseded`: an accepted draft for the address names this one.
    ReplacedByAcceptedDraft,
    /// `unresolvable`: a cited id has no fact in the current graph.
    CitedFactMissing,
    /// `stale`: a cited fact has another content hash than was recorded.
    CitedFactChanged,
    /// `conflicted`: another accepted draft that nothing replaces has the
    /// same address.
    AnotherDraftAccepted,
    /// `eligible`: every condition holds.
    AllConditionsHold,
}

/// One accepted draft with its DR-4 fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedDraft {
    /// The draft's identity.
    pub draft_id: String,
    /// The section it was drafted for.
    pub address: DraftAddress,
    /// The author role.
    pub role: String,
    /// The claims, in reading order: the content a document prints when
    /// the draft is eligible.
    pub claims: Vec<DraftClaim>,
    /// The returned draft it replaces.
    pub replaces: Option<String>,
    /// Always Agentic: acceptance changes no tier.
    pub tier: Tier,
    /// Always InferredWeak.
    pub confidence: ConfidenceTier,
    /// The review; always accepted for a draft in the projection.
    pub review_status: DraftReview,
    /// How many distinct facts the draft cites (DR-4 `source_support`).
    pub cited_facts: usize,
    /// The assigned state.
    pub state: DraftState,
    /// Why that state applies.
    pub reason: DraftStateReason,
    /// Cited ids whose current content hash differs from the recorded one,
    /// sorted (ADR-0045 decision 2). Reported whatever the state.
    #[serde(default)]
    pub changed: Vec<String>,
    /// Cited ids that resolve to no fact in the current graph, sorted.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// Draft totals. `accepted` is the sum of every state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCounts {
    /// Accepted drafts.
    pub accepted: usize,
    /// Drafts a document may print.
    pub eligible: usize,
    /// Drafts listed as `superseded`.
    pub superseded: usize,
    /// Drafts listed as `unresolvable`.
    pub unresolvable: usize,
    /// Drafts listed as `stale`.
    pub stale: usize,
    /// Drafts listed as `conflicted`.
    pub conflicted: usize,
}

/// `(draft_id, decision, state, reason)` as the revision takes it.
pub(super) type RevisionDraft<'a> = (
    &'a str,
    DraftReview,
    Option<DraftState>,
    Option<DraftStateReason>,
);

pub(super) struct DraftProjection<'a> {
    /// Every accepted draft, ordered by identity.
    pub(super) drafts: Vec<CuratedDraft>,
    pub(super) counts: DraftCounts,
    /// Every accepted or rejected draft, ordered by identity.
    pub(super) revision: Vec<RevisionDraft<'a>>,
}

fn assign(
    draft: &ReviewedDraft,
    current: &BTreeMap<String, String>,
    superseded: &HashSet<&str>,
    conflicted: bool,
) -> (DraftState, DraftStateReason) {
    if superseded.contains(draft.draft_id.as_str()) {
        return (
            DraftState::Superseded,
            DraftStateReason::ReplacedByAcceptedDraft,
        );
    }
    let mut changed = false;
    for (id, recorded) in &draft.cited_hashes {
        match current.get(id) {
            None => return (DraftState::Unresolvable, DraftStateReason::CitedFactMissing),
            Some(hash) => changed |= hash != recorded,
        }
    }
    if changed {
        (DraftState::Stale, DraftStateReason::CitedFactChanged)
    } else if conflicted {
        (
            DraftState::Conflicted,
            DraftStateReason::AnotherDraftAccepted,
        )
    } else {
        (DraftState::Eligible, DraftStateReason::AllConditionsHold)
    }
}

/// The cited ids of `draft` whose fact changed and those whose fact is gone,
/// each in id order (ADR-0045 decision 2). A set comparison of the recorded
/// hashes with `current`: the same inputs always give the same lists.
fn drift(draft: &ReviewedDraft, current: &BTreeMap<String, String>) -> (Vec<String>, Vec<String>) {
    let mut changed = Vec::new();
    let mut missing = Vec::new();
    // `cited_hashes` is ordered by id, so both lists are.
    for (id, recorded) in &draft.cited_hashes {
        match current.get(id) {
            None => missing.push(id.clone()),
            Some(hash) if hash != recorded => changed.push(id.clone()),
            Some(_) => {}
        }
    }
    (changed, missing)
}

/// Assign every accepted draft its state. `current` holds the current
/// content hash of each cited id that resolves to a fact.
pub(super) fn project_drafts<'a>(
    drafts: &'a [ReviewedDraft],
    current: &BTreeMap<String, String>,
) -> Result<DraftProjection<'a>, ProjectionError> {
    // Pending and returned drafts are checked too: one identity, one draft.
    let mut identities = HashSet::with_capacity(drafts.len());
    if !drafts
        .iter()
        .all(|draft| identities.insert(draft.draft_id.as_str()))
    {
        return Err(ProjectionError::DuplicateRecord);
    }
    let mut reviewed: Vec<(&ReviewedDraft, DraftReview)> = drafts
        .iter()
        .filter_map(|draft| match draft.decision? {
            DraftReview::Returned => None,
            decision => Some((draft, decision)),
        })
        .collect();
    reviewed.sort_unstable_by(|left, right| left.0.draft_id.cmp(&right.0.draft_id));

    let accepted: HashMap<&str, &ReviewedDraft> = reviewed
        .iter()
        .filter(|(_, decision)| *decision == DraftReview::Accepted)
        .map(|(draft, _)| (draft.draft_id.as_str(), *draft))
        .collect();
    // Accepted drafts an accepted draft of the same address replaces.
    let superseded: HashSet<&str> = accepted
        .values()
        .filter_map(|draft| {
            let replaced = accepted.get(draft.replaces.as_deref()?)?;
            (replaced.address == draft.address).then_some(replaced.draft_id.as_str())
        })
        .collect();
    // The accepted drafts of each address that no accepted draft replaces.
    // A replacement chain A <- B <- C leaves only C, so C does not conflict.
    let mut heads: HashMap<&DraftAddress, usize> = HashMap::new();
    for draft in accepted.values() {
        if !superseded.contains(draft.draft_id.as_str()) {
            *heads.entry(&draft.address).or_default() += 1;
        }
    }

    let mut listed = Vec::with_capacity(accepted.len());
    let mut counts = DraftCounts::default();
    let mut revision = Vec::with_capacity(reviewed.len());
    for (draft, decision) in reviewed {
        if decision == DraftReview::Rejected {
            revision.push((draft.draft_id.as_str(), decision, None, None));
            continue;
        }
        // Another accepted draft of the address that nothing replaces. A
        // superseded draft takes that state first, so its count is moot.
        let conflicted = heads.get(&draft.address).copied().unwrap_or(0) > 1;
        let (state, reason) = assign(draft, current, &superseded, conflicted);
        revision.push((draft.draft_id.as_str(), decision, Some(state), Some(reason)));
        counts.accepted += 1;
        *match state {
            DraftState::Superseded => &mut counts.superseded,
            DraftState::Unresolvable => &mut counts.unresolvable,
            DraftState::Stale => &mut counts.stale,
            DraftState::Conflicted => &mut counts.conflicted,
            DraftState::Eligible => &mut counts.eligible,
        } += 1;
        let (changed, missing) = drift(draft, current);
        listed.push(CuratedDraft {
            draft_id: draft.draft_id.clone(),
            address: draft.address.clone(),
            role: draft.role.clone(),
            claims: draft.claims.clone(),
            replaces: draft.replaces.clone(),
            tier: Tier::Agentic,
            confidence: ConfidenceTier::InferredWeak,
            review_status: decision,
            cited_facts: draft.cited_hashes.len(),
            state,
            reason,
            changed,
            missing,
        });
    }
    Ok(DraftProjection {
        drafts: listed,
        counts,
        revision,
    })
}
