//! An agent's attested proposal from an investigation finding (ADR-0090
//! decision 9, AC-1079).
//!
//! A completed investigation, started in the app, in a chat or over MCP,
//! has findings. One of them may answer a Gap or a section the evidence
//! leaves open. [`proposal`] turns that finding into an attested record
//! stated by the run's agent, citing the ADR-0058 id of each fact the
//! finding's admitted citations rest on. The record is `agent-attested,
//! unconfirmed` until the engineer states or confirms it.
//!
//! When the same content is already stored, [`follow_up`] gives the
//! attestation to append: another agent `agreed`, which is how agents fill a
//! gap together without an engineer. An agent that already attested the
//! record appends nothing.
//!
//! Everything here is pure. The host reads the run, checks its graph, and
//! stores the record through [`crate::attested_store`].

use crate::attested::{AgentVerb, Attestation, AttestedRecord, AttestedTarget, ContributorId};
use crate::attested_store::StoredAttested;
use crate::investigation::{InvestigationCitation, InvestigationClaimKind, InvestigationFinding};
use crate::source_basis::TaskFactKey;
use std::collections::{BTreeMap, BTreeSet};

/// The run a proposal comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRun {
    /// The investigation id.
    pub run: String,
    /// The role, such as `domain-analyst@29`.
    pub role: String,
    /// The role definition's fingerprint.
    pub definition_fingerprint: String,
    /// The provider id.
    pub provider: String,
    /// The model.
    pub model: String,
}

/// Who proposes: the run's agent, registered for its role, provider and
/// model, and the engineer who directed it, when one did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposer {
    /// The agent's contributor id.
    pub agent: ContributorId,
    /// The directing engineer.
    pub directed_by: Option<ContributorId>,
}

/// Why a finding cannot be proposed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProposalRefusal {
    /// A proposed design states no fact about the system.
    #[error("a proposed design cannot be attested")]
    ProposedDesign,
    /// A citation of the finding is not in the run's ledger.
    #[error("the finding cites an id outside the run's ledger")]
    UnknownCitation,
}

impl ProposalRefusal {
    /// The closed reason code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProposedDesign => "finding_is_proposed_design",
            Self::UnknownCitation => "finding_citation_unknown",
        }
    }
}

/// The agent attestation of `run` with `verb`.
pub fn agent_attestation(
    run: &ProposalRun,
    agent: ContributorId,
    verb: AgentVerb,
    directed_by: Option<ContributorId>,
) -> Attestation {
    Attestation::Agent {
        contributor: agent,
        verb,
        role: run.role.clone(),
        definition_fingerprint: run.definition_fingerprint.clone(),
        provider: run.provider.clone(),
        model: run.model.clone(),
        run: run.run.clone(),
        directed_by,
    }
}

/// The record `finding` of `run` proposes for `target`, stated by
/// `proposer`. `fact_id` gives the
/// ADR-0058 id of a ledger fact; `hashes` gives the current hash of each
/// supporting id the graph holds.
pub fn proposal(
    run: &ProposalRun,
    finding: &InvestigationFinding,
    citations: &[InvestigationCitation],
    target: AttestedTarget,
    proposer: &Proposer,
    fact_id: impl Fn(&TaskFactKey) -> String,
    hashes: impl FnOnce(&BTreeSet<String>) -> BTreeMap<String, String>,
) -> Result<AttestedRecord, ProposalRefusal> {
    if finding.claim_kind == InvestigationClaimKind::ProposedDesign {
        return Err(ProposalRefusal::ProposedDesign);
    }
    let ledger: BTreeMap<&str, &TaskFactKey> = citations
        .iter()
        .map(|citation| (citation.citation_id.as_str(), &citation.fact))
        .collect();
    let mut supporting: Vec<String> = Vec::with_capacity(finding.citation_ids.len());
    for id in &finding.citation_ids {
        let fact = ledger
            .get(id.as_str())
            .ok_or(ProposalRefusal::UnknownCitation)?;
        let id = fact_id(fact);
        if !supporting.contains(&id) {
            supporting.push(id);
        }
    }
    let cited: BTreeSet<String> = supporting.iter().cloned().collect();
    Ok(AttestedRecord {
        target,
        statement: finding.statement.clone(),
        supporting,
        recorded_hashes: hashes(&cited),
        attestations: vec![agent_attestation(
            run,
            proposer.agent.clone(),
            AgentVerb::Stated,
            proposer.directed_by.clone(),
        )],
    })
}

/// What `proposer` of `run` appends to `stored`, a record with the content
/// it proposed: nothing when its agent already attested the record,
/// otherwise `agreed`.
pub fn follow_up(
    stored: &StoredAttested,
    run: &ProposalRun,
    proposer: &Proposer,
) -> Option<Attestation> {
    let attested = stored
        .record
        .attestations
        .iter()
        .any(|attestation| *attestation.contributor() == proposer.agent);
    (!attested).then(|| {
        agent_attestation(
            run,
            proposer.agent.clone(),
            AgentVerb::Agreed,
            proposer.directed_by.clone(),
        )
    })
}

#[cfg(test)]
#[path = "attested_proposal_tests.rs"]
mod tests;
