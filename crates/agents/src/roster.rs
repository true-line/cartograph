//! The specialist agent roster (#256, AC-1408).
//!
//! The roster names each specialist role as an agent: a stable agent id, a
//! display name, a role line, a glyph and a tier ceiling. The definitions are
//! checked-in data (`data/roster-v1.json`), not code. An agent's version is
//! its role's current version in the capability table (ADR-0071), so the
//! `specialist_id` every saved investigation already records is the agent
//! version that produced it, for example `agt-analyst@28` for
//! `domain-analyst@28`.
//!
//! The tier ceiling is the highest confidence an agent's output may carry
//! (R-INT-1): agent output enters the graph only as proposals at or below
//! it. The roster refuses a definition whose bound specialist could emit
//! output above its agent's ceiling.

use crate::investigation::{
    InvestigationProviderMode, InvestigationStatus, SpecialistId, SpecialistRole,
    current_specialist,
};
use core_prov::{ConfidenceTier, Tier};
use serde::{Deserialize, Serialize};

const ROSTER_V1: &str = include_str!("../data/roster-v1.json");

/// One agent of the roster, resolved against the capability table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterAgent {
    /// Versioned agent id, for example `agt-analyst@28`.
    pub id: String,
    /// The unversioned agent id, for example `agt-analyst`.
    pub agent: String,
    /// The agent version: its role's current version.
    pub version: u32,
    /// Display name.
    pub name: String,
    /// One-line description of what the agent does.
    pub role_line: String,
    /// A single display glyph.
    pub glyph: String,
    /// The specialist role the agent runs as.
    pub role: SpecialistRole,
    /// The specialist version a new dispatch to this agent starts.
    pub specialist_id: SpecialistId,
    /// The highest producing tier the agent's output may claim.
    pub tier_ceiling: Tier,
    /// The highest confidence the agent's output may carry.
    pub confidence_ceiling: ConfidenceTier,
}

impl RosterAgent {
    /// Whether output at `confidence` stays at or below this agent's ceiling.
    /// A Gap is always admissible: stating that something is unresolved never
    /// claims more than the evidence.
    pub fn admits(&self, confidence: ConfidenceTier) -> bool {
        confidence_rank(confidence) <= confidence_rank(self.confidence_ceiling)
    }
}

/// Why the checked-in roster could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RosterError {
    /// The data file does not parse or has the wrong schema version.
    #[error("roster data is invalid: {0}")]
    Invalid(String),
    /// An agent names a role the capability table does not have.
    #[error("agent {0} names an unknown role")]
    UnknownRole(String),
    /// Two agents share an id or a role, or a role has no agent.
    #[error("roster must name each role exactly once under a unique agent id: {0}")]
    Coverage(String),
    /// The role has no current version on offer.
    #[error("role of agent {0} has no current version")]
    NoCurrentVersion(String),
    /// The bound specialist's output confidence is above the agent's ceiling.
    #[error("agent {0} binds a specialist whose output exceeds its tier ceiling")]
    CeilingExceeded(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterFile {
    schema_version: u32,
    agents: Vec<AgentEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentEntry {
    agent: String,
    name: String,
    role: String,
    glyph: String,
    role_line: String,
    tier_ceiling: TierName,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum TierName {
    Deterministic,
    Dynamic,
    Semantic,
    Agentic,
}

impl From<TierName> for Tier {
    fn from(name: TierName) -> Self {
        match name {
            TierName::Deterministic => Tier::Deterministic,
            TierName::Dynamic => Tier::Dynamic,
            TierName::Semantic => Tier::Semantic,
            TierName::Agentic => Tier::Agentic,
        }
    }
}

/// Lower is less certain. Gap claims nothing, so it ranks lowest.
fn confidence_rank(confidence: ConfidenceTier) -> u8 {
    match confidence {
        ConfidenceTier::Gap => 0,
        ConfidenceTier::InferredWeak => 1,
        ConfidenceTier::InferredStrong => 2,
        ConfidenceTier::Confirmed => 3,
    }
}

fn valid_agent_id(agent: &str) -> bool {
    agent
        .strip_prefix("agt-")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase()))
}

/// The roster v1, resolved against the capability table, in data order.
pub fn roster() -> Result<Vec<RosterAgent>, RosterError> {
    parse_roster(ROSTER_V1)
}

/// The roster agent that `specialist` runs as, when its role has one. A
/// retired or older version still names its role's agent, so a saved run
/// shows which agent produced it.
pub fn roster_agent_for(specialist: SpecialistId) -> Option<RosterAgent> {
    roster()
        .ok()?
        .into_iter()
        .find(|agent| agent.role == specialist.role())
}

fn parse_roster(text: &str) -> Result<Vec<RosterAgent>, RosterError> {
    let file: RosterFile =
        serde_json::from_str(text).map_err(|error| RosterError::Invalid(error.to_string()))?;
    if file.schema_version != 1 {
        return Err(RosterError::Invalid(format!(
            "schema_version {} is not 1",
            file.schema_version
        )));
    }
    let mut agents = Vec::with_capacity(file.agents.len());
    for entry in file.agents {
        if !valid_agent_id(&entry.agent)
            || entry.name.trim().is_empty()
            || entry.role_line.trim().is_empty()
            || entry.glyph.chars().count() != 1
        {
            return Err(RosterError::Invalid(format!(
                "agent {} has an invalid id, name, role line or glyph",
                entry.agent
            )));
        }
        if agents
            .iter()
            .any(|known: &RosterAgent| known.agent == entry.agent)
        {
            return Err(RosterError::Coverage(entry.agent));
        }
        let role = SpecialistRole::ALL
            .into_iter()
            .find(|role| role.slug() == entry.role)
            .ok_or_else(|| RosterError::UnknownRole(entry.agent.clone()))?;
        if agents.iter().any(|known: &RosterAgent| known.role == role) {
            return Err(RosterError::Coverage(entry.agent));
        }
        let specialist = current_specialist(role)
            .ok_or_else(|| RosterError::NoCurrentVersion(entry.agent.clone()))?;
        let tier_ceiling = Tier::from(entry.tier_ceiling);
        let confidence_ceiling = tier_ceiling.confidence_ceiling();
        let produced = specialist
            .definition()
            .map_err(|error| RosterError::Invalid(error.to_string()))?
            .confidence_tier;
        if confidence_rank(produced) > confidence_rank(confidence_ceiling) {
            return Err(RosterError::CeilingExceeded(entry.agent));
        }
        let version = specialist.row().version;
        agents.push(RosterAgent {
            id: format!("{}@{version}", entry.agent),
            agent: entry.agent,
            version,
            name: entry.name,
            role_line: entry.role_line,
            glyph: entry.glyph,
            role,
            specialist_id: specialist,
            tier_ceiling,
            confidence_ceiling,
        });
    }
    if let Some(missing) = SpecialistRole::ALL
        .into_iter()
        .find(|role| !agents.iter().any(|agent| agent.role == *role))
    {
        return Err(RosterError::Coverage(missing.slug().into()));
    }
    Ok(agents)
}

/// Whether an agent has dispatched work in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// At least one of its investigations is queued, preparing, running or
    /// waiting for consent.
    Running,
    /// None of its investigations is in flight.
    Idle,
}

/// One roster agent with the work it has in flight, as the host records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterAgentStatus {
    /// The roster definition.
    #[serde(flatten)]
    pub agent: RosterAgent,
    /// Running when any of its investigations is in flight.
    pub state: AgentState,
    /// In-flight investigations on the local provider.
    pub local_active: usize,
    /// In-flight investigations on the cloud provider.
    pub cloud_active: usize,
}

/// One saved investigation as the roster reads it: its specialist, provider
/// mode and status.
pub type AgentWork = (SpecialistId, InvestigationProviderMode, InvestigationStatus);

/// Whether a saved investigation is still in flight.
pub fn investigation_in_flight(status: InvestigationStatus) -> bool {
    matches!(
        status,
        InvestigationStatus::Queued
            | InvestigationStatus::Preparing
            | InvestigationStatus::Running
            | InvestigationStatus::AwaitingConsent
    )
}

/// The roster with each agent's in-flight work. An investigation counts for
/// its role's agent whatever version it runs, so an older offered version in
/// flight still shows its agent running. Finished runs are not counted. Each
/// investigation is its saved specialist, provider mode and status.
pub fn roster_status(
    roster: Vec<RosterAgent>,
    investigations: &[AgentWork],
) -> Vec<RosterAgentStatus> {
    roster
        .into_iter()
        .map(|agent| {
            let in_flight = || {
                investigations.iter().filter(|(specialist, _, status)| {
                    specialist.role() == agent.role && investigation_in_flight(*status)
                })
            };
            let local_active = in_flight()
                .filter(|(_, mode, _)| *mode == InvestigationProviderMode::Local)
                .count();
            let cloud_active = in_flight()
                .filter(|(_, mode, _)| *mode == InvestigationProviderMode::Cloud)
                .count();
            RosterAgentStatus {
                state: if local_active + cloud_active > 0 {
                    AgentState::Running
                } else {
                    AgentState::Idle
                },
                agent,
                local_active,
                cloud_active,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
