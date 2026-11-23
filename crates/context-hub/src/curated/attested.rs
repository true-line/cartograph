//! Attested records in the curated projection (#2463, ADR-0090 decision 8,
//! AC-1075).
//!
//! An attested record is the fourth record kind of the projection: a
//! statement an engineer or an agent made to fill a gap the evidence leaves
//! open, never a recovered fact. Its attestations are its review, so every
//! stored record is projected. The host reads the stored records, their
//! standing and the current hash of every fact they cite, and passes them
//! in. Nothing is fetched, no fact or tier changes, the base graph hash is
//! the one passed in, and a Gap a record answers is still a Gap of the
//! graph and still counted wherever Gaps are counted.
//!
//! # States
//!
//! Each record gets the first state that applies:
//!
//! - `withdrawn`: its latest engineer attestation withdrew it;
//! - `disputed`: a contributor's latest attestation disputes it;
//! - `already-covered`: the Gap it answers kept its id and is now a fact
//!   that an ingest recovered deterministically (valid provenance, not
//!   Agentic, not Gap confidence);
//! - `unresolvable`: the Gap it answers is not in the graph, or is now a
//!   node an agent produced, or a supporting citation has no current hash;
//! - `stale`: a supporting citation's current hash differs from the one
//!   recorded when the record was made (ADR-0064 decision 8);
//! - `eligible` when an engineer stated or confirmed it, and
//!   `agent-attested, unconfirmed` when only agents attest it. The second
//!   is projected and never accepted.
//!
//! A section address names no graph node, so a section target is resolved
//! by its supporting citations alone.
//!
//! An attested record carries no tier and no confidence: it is not evidence
//! (ADR-0090 decision 1).
//!
//! # Cost
//!
//! With `T` records, `N` nodes and `C` supporting citations: one sort of
//! the records, one pass over the nodes for the Gap ids they name, and one
//! lookup per citation. Time is `O(T log T + N + C log C)`.

use super::{ProjectionError, is_gap, valid_provenance};
use core_graph::Node;
use core_prov::{ConfidenceTier, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// What an attested record answers, as projection input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttestedAnswer {
    /// A Gap slot.
    Gap {
        /// The Gap id.
        gap_id: String,
        /// The missing relation.
        slot: String,
    },
    /// A section of a catalogue document.
    Section {
        /// Catalogue version.
        catalogue_version: String,
        /// Document id, such as `D1`.
        document: String,
        /// Section number.
        section: String,
        /// Scope.
        scope: String,
    },
}

/// How a record's attestations stand, as the host read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestedStanding {
    /// Its latest engineer attestation withdrew it.
    Withdrawn,
    /// A contributor's latest attestation disputes it.
    Disputed,
    /// An engineer stated or confirmed it.
    EngineerBacked,
    /// Only agents attest it.
    AgentAttestedUnconfirmed,
}

/// One attestation as a read prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedContribution {
    /// The contributor's `C-` id.
    pub contributor_id: String,
    /// An engineer's display name, resolved by the host from the project's
    /// contributor table at read time; the stored record holds none
    /// (ADR-0090 decision 4). `None` for an agent, which is named by role
    /// and run.
    pub display_name: Option<String>,
    /// Whether the contributor is an engineer; otherwise an agent.
    pub engineer: bool,
    /// What it did, such as `stated` or `confirmed`.
    pub verb: String,
    /// An agent's role, such as `section-writer@1`.
    pub role: Option<String>,
    /// An agent's producing run or chat.
    pub run: Option<String>,
}

/// One stored attested record, as the host read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedAttested {
    /// The record's identity.
    pub record_id: String,
    /// Its `A-` citation id.
    pub citation_id: String,
    /// What it answers.
    pub answer: AttestedAnswer,
    /// The statement, as plain text.
    pub statement: String,
    /// Each supporting `F-` id with the hash recorded when it was made.
    pub recorded_hashes: BTreeMap<String, String>,
    /// How its attestations stand.
    pub standing: AttestedStanding,
    /// Its attestations, oldest first.
    pub attestations: Vec<AttestedContribution>,
}

/// The one state of an attested record, in first-match order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttestedState {
    /// Its latest engineer attestation withdrew it.
    Withdrawn,
    /// A contributor disputes it; never shown as settled.
    Disputed,
    /// An ingest recovered its Gap deterministically.
    AlreadyCovered,
    /// Its Gap or a supporting citation resolves to nothing current.
    Unresolvable,
    /// A supporting fact changed since the record was made.
    Stale,
    /// Engineer-backed and none of the above: accepted.
    Eligible,
    /// Only agents attest it and none of the above: projected, never
    /// accepted.
    #[serde(rename = "agent-attested-unconfirmed")]
    AgentAttestedUnconfirmed,
}

impl AttestedState {
    /// The label a read prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::Withdrawn => "withdrawn",
            Self::Disputed => "disputed",
            Self::AlreadyCovered => "already-covered",
            Self::Unresolvable => "unresolvable",
            Self::Stale => "stale",
            Self::Eligible => "eligible",
            Self::AgentAttestedUnconfirmed => "agent-attested, unconfirmed",
        }
    }
}

/// Why a record has its state. One reason per record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestedStateReason {
    /// `withdrawn`: the engineer withdrew it.
    EngineerWithdrew,
    /// `disputed`: a latest attestation disputes it.
    ContributorDisputes,
    /// `already-covered`: the Gap is now a recovered fact.
    GapRecovered,
    /// `unresolvable`: no node has the Gap's id.
    GapMissing,
    /// `unresolvable`: the Gap's id is now a node an agent produced.
    GapNotRecovered,
    /// `unresolvable`: a supporting id has no fact in the current graph.
    CitedFactMissing,
    /// `stale`: a supporting fact has another hash than recorded.
    CitedFactChanged,
    /// `eligible`: an engineer stated or confirmed it.
    EngineerBacked,
    /// `agent-attested, unconfirmed`: only agents attest it.
    AgentsOnly,
}

/// One projected attested record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedAttested {
    /// The record's identity.
    pub record_id: String,
    /// Its `A-` citation id.
    pub citation_id: String,
    /// What it answers.
    pub answer: AttestedAnswer,
    /// The statement.
    pub statement: String,
    /// The supporting ids, sorted.
    pub citations: Vec<String>,
    /// Always `None`: an attested record is not evidence.
    pub tier: Option<Tier>,
    /// Always `None`.
    pub confidence: Option<ConfidenceTier>,
    /// Its attestations, oldest first.
    pub attestations: Vec<AttestedContribution>,
    /// The assigned state.
    pub state: AttestedState,
    /// Why that state applies.
    pub reason: AttestedStateReason,
    /// Supporting ids whose current hash differs from the recorded one,
    /// sorted. Reported whatever the state.
    pub changed: Vec<String>,
    /// Supporting ids with no fact in the current graph, sorted.
    pub missing: Vec<String>,
}

impl CuratedAttested {
    /// The label a document or read prints the record with, when it may be
    /// printed at all: `attested` when an engineer stated or confirmed it,
    /// `agent-attested, unconfirmed` when only agents attest it, and `None`
    /// in every other state (ADR-0090 decision 7).
    pub fn printed_label(&self) -> Option<&'static str> {
        match self.state {
            AttestedState::Eligible => Some("attested"),
            AttestedState::AgentAttestedUnconfirmed => Some(self.state.label()),
            _ => None,
        }
    }

    /// Who stands behind the record, oldest first, as a document prints it:
    /// `stated by section-writer@1 (run run-1), confirmed by <engineer>`.
    /// An engineer is named by display name, an agent by role and run.
    pub fn contributors(&self) -> String {
        let mut text = String::new();
        for (position, contribution) in self.attestations.iter().enumerate() {
            if position > 0 {
                text.push_str(", ");
            }
            text.push_str(&contribution.verb);
            text.push_str(" by ");
            if contribution.engineer {
                text.push_str(
                    contribution
                        .display_name
                        .as_deref()
                        .unwrap_or("the project's engineer"),
                );
            } else {
                text.push_str(contribution.role.as_deref().unwrap_or("an agent"));
                if let Some(run) = &contribution.run {
                    text.push_str(" (run ");
                    text.push_str(run);
                    text.push(')');
                }
            }
        }
        text
    }
}

/// Attested totals. `projected` is the sum of every state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedCounts {
    /// Every projected record.
    pub projected: usize,
    /// Engineer-backed records a read may print as attested.
    pub eligible: usize,
    /// Agent-only records: printed only by `best-effort`, with the label.
    pub agent_attested_unconfirmed: usize,
    /// Records listed as `withdrawn`.
    pub withdrawn: usize,
    /// Records listed as `disputed`.
    pub disputed: usize,
    /// Records listed as `already-covered`.
    pub already_covered: usize,
    /// Records listed as `unresolvable`.
    pub unresolvable: usize,
    /// Records listed as `stale`.
    pub stale: usize,
}

/// `(record_id, state, reason, attestations)` as the revision takes it: a
/// read prints the attestations, so a new one gives a new revision.
pub(super) type RevisionAttested<'a> = (
    &'a str,
    AttestedState,
    AttestedStateReason,
    &'a [AttestedContribution],
);

pub(super) struct AttestedProjection<'a> {
    pub(super) records: Vec<CuratedAttested>,
    pub(super) counts: AttestedCounts,
    pub(super) revision: Vec<RevisionAttested<'a>>,
}

/// What the graph says of a Gap id a record names.
#[derive(Clone, Copy)]
enum GapPresence {
    Open,
    Recovered,
    NotRecovered,
}

fn gap_presence<'a>(nodes: &'a [Node], named: &HashSet<&str>) -> HashMap<&'a str, GapPresence> {
    let mut presence = HashMap::with_capacity(named.len());
    for node in nodes {
        if !named.contains(node.id.as_str()) {
            continue;
        }
        let state = if is_gap(node) {
            GapPresence::Open
        } else if valid_provenance(&node.props).is_some_and(|provenance| {
            provenance.tier != Tier::Agentic && provenance.confidence_tier != ConfidenceTier::Gap
        }) {
            GapPresence::Recovered
        } else {
            GapPresence::NotRecovered
        };
        presence.insert(node.id.as_str(), state);
    }
    presence
}

/// Assign every attested record its state. `current` holds the current hash
/// of each cited id that resolves to a fact.
pub(super) fn project_attested<'a>(
    records: &'a [ReviewedAttested],
    nodes: &[Node],
    current: &BTreeMap<String, String>,
) -> Result<AttestedProjection<'a>, ProjectionError> {
    // One identity, one record; and one `A-` id, one record. The id keeps
    // 64 bits of the identity and is never lengthened, so two records whose
    // ids collide make every citation of that id ambiguous and fail the
    // projection rather than one being chosen (ADR-0058 decision 5).
    let mut identities = HashSet::with_capacity(records.len());
    let mut citations = HashSet::with_capacity(records.len());
    if !records.iter().all(|record| {
        identities.insert(record.record_id.as_str())
            && citations.insert(record.citation_id.as_str())
    }) {
        return Err(ProjectionError::DuplicateRecord);
    }
    let mut sorted: Vec<&ReviewedAttested> = records.iter().collect();
    sorted.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
    let named: HashSet<&str> = sorted
        .iter()
        .filter_map(|record| match &record.answer {
            AttestedAnswer::Gap { gap_id, .. } => Some(gap_id.as_str()),
            AttestedAnswer::Section { .. } => None,
        })
        .collect();
    let gaps = gap_presence(nodes, &named);

    let mut projected = Vec::with_capacity(sorted.len());
    let mut counts = AttestedCounts::default();
    let mut revision = Vec::with_capacity(sorted.len());
    for record in sorted {
        let mut changed = Vec::new();
        let mut missing = Vec::new();
        for (id, recorded) in &record.recorded_hashes {
            match current.get(id) {
                None => missing.push(id.clone()),
                Some(hash) if hash != recorded => changed.push(id.clone()),
                Some(_) => {}
            }
        }
        let gap = match &record.answer {
            AttestedAnswer::Gap { gap_id, .. } => Some(gaps.get(gap_id.as_str()).copied()),
            AttestedAnswer::Section { .. } => None,
        };
        use AttestedState as S;
        use AttestedStateReason as R;
        let (state, reason) = match (record.standing, gap) {
            (AttestedStanding::Withdrawn, _) => (S::Withdrawn, R::EngineerWithdrew),
            (AttestedStanding::Disputed, _) => (S::Disputed, R::ContributorDisputes),
            (_, Some(Some(GapPresence::Recovered))) => (S::AlreadyCovered, R::GapRecovered),
            (_, Some(None)) => (S::Unresolvable, R::GapMissing),
            (_, Some(Some(GapPresence::NotRecovered))) => (S::Unresolvable, R::GapNotRecovered),
            _ if !missing.is_empty() => (S::Unresolvable, R::CitedFactMissing),
            _ if !changed.is_empty() => (S::Stale, R::CitedFactChanged),
            (AttestedStanding::EngineerBacked, _) => (S::Eligible, R::EngineerBacked),
            (AttestedStanding::AgentAttestedUnconfirmed, _) => {
                (S::AgentAttestedUnconfirmed, R::AgentsOnly)
            }
        };
        revision.push((
            record.record_id.as_str(),
            state,
            reason,
            record.attestations.as_slice(),
        ));
        counts.projected += 1;
        *match state {
            S::Withdrawn => &mut counts.withdrawn,
            S::Disputed => &mut counts.disputed,
            S::AlreadyCovered => &mut counts.already_covered,
            S::Unresolvable => &mut counts.unresolvable,
            S::Stale => &mut counts.stale,
            S::Eligible => &mut counts.eligible,
            S::AgentAttestedUnconfirmed => &mut counts.agent_attested_unconfirmed,
        } += 1;
        projected.push(CuratedAttested {
            record_id: record.record_id.clone(),
            citation_id: record.citation_id.clone(),
            answer: record.answer.clone(),
            statement: record.statement.clone(),
            citations: record.recorded_hashes.keys().cloned().collect(),
            tier: None,
            confidence: None,
            attestations: record.attestations.clone(),
            state,
            reason,
            changed,
            missing,
        });
    }
    Ok(AttestedProjection {
        records: projected,
        counts,
        revision,
    })
}
