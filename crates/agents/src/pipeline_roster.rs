//! The pipeline agents of the roster (#256, AC-1808).
//!
//! The design's roster also names the passes that recover and link facts
//! without a chat: Flow Tracer, Contract Matcher and IaC Grapher. They are
//! listed as read-only agents next to the specialists. Nobody dispatches
//! them; each runs as part of a recovery or a semantic preview, and its
//! state is whether such work is in flight. The definitions are checked-in
//! data (`data/pipeline-v1.json`); the version is bumped there when the pass
//! changes what it produces.

use core_prov::{ConfidenceTier, Tier};
use serde::{Deserialize, Serialize};

use crate::roster::AgentState;

const PIPELINE_V1: &str = include_str!("../data/pipeline-v1.json");

/// The pass a pipeline agent names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelinePass {
    /// Flow tracing over the graph and the semantic preview's overlay.
    FlowTrace,
    /// The semantic preview's embedding-similarity link proposals.
    ContractMatch,
    /// Infrastructure-as-code recovery inside a repository recovery.
    IacGraph,
}

impl PipelinePass {
    /// Every pass, each named by exactly one agent.
    pub const ALL: [Self; 3] = [Self::FlowTrace, Self::ContractMatch, Self::IacGraph];

    /// The tier of what the pass adds: semantic links and flows across them
    /// are T2, infrastructure recovered from source is T0.
    pub fn produced_tier(self) -> Tier {
        match self {
            Self::FlowTrace | Self::ContractMatch => Tier::Semantic,
            Self::IacGraph => Tier::Deterministic,
        }
    }
}

/// One read-only pipeline agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineAgent {
    /// Versioned agent id, for example `agt-flow@1`.
    pub id: String,
    /// The unversioned agent id.
    pub agent: String,
    /// The definition version.
    pub version: u32,
    /// Display name.
    pub name: String,
    /// One-line description of what the agent does.
    pub role_line: String,
    /// A single display glyph.
    pub glyph: String,
    /// The pass the agent is.
    pub pass: PipelinePass,
    /// The highest producing tier the agent's output may claim.
    pub tier_ceiling: Tier,
    /// The highest confidence the agent's output may carry.
    pub confidence_ceiling: ConfidenceTier,
    /// Always false: a pipeline agent runs inside other work.
    pub dispatchable: bool,
}

/// Why the checked-in pipeline roster could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineRosterError {
    /// The data file does not parse, has the wrong schema version or holds
    /// an invalid entry.
    #[error("pipeline roster data is invalid: {0}")]
    Invalid(String),
    /// Two agents share an id or a pass, or a pass has no agent.
    #[error("pipeline roster must name each pass exactly once under a unique agent id: {0}")]
    Coverage(String),
    /// The ceiling differs from the tier of what the pass produces.
    #[error("agent {0} claims a tier ceiling other than its pass produces")]
    CeilingMismatch(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineFile {
    schema_version: u32,
    agents: Vec<PipelineEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineEntry {
    agent: String,
    version: u32,
    name: String,
    pass: PipelinePass,
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

fn valid_agent_id(agent: &str) -> bool {
    agent
        .strip_prefix("agt-")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase()))
}

/// The pipeline roster v1, in data order.
pub fn pipeline_roster() -> Result<Vec<PipelineAgent>, PipelineRosterError> {
    parse_pipeline(PIPELINE_V1)
}

fn parse_pipeline(text: &str) -> Result<Vec<PipelineAgent>, PipelineRosterError> {
    let file: PipelineFile = serde_json::from_str(text)
        .map_err(|error| PipelineRosterError::Invalid(error.to_string()))?;
    if file.schema_version != 1 {
        return Err(PipelineRosterError::Invalid(format!(
            "schema_version {} is not 1",
            file.schema_version
        )));
    }
    let specialists =
        crate::roster::roster().map_err(|error| PipelineRosterError::Invalid(error.to_string()))?;
    let mut agents: Vec<PipelineAgent> = Vec::with_capacity(file.agents.len());
    for entry in file.agents {
        if !valid_agent_id(&entry.agent)
            || entry.version == 0
            || entry.name.trim().is_empty()
            || entry.role_line.trim().is_empty()
            || entry.glyph.chars().count() != 1
        {
            return Err(PipelineRosterError::Invalid(format!(
                "agent {} has an invalid id, version, name, role line or glyph",
                entry.agent
            )));
        }
        if agents.iter().any(|known| known.agent == entry.agent)
            || specialists.iter().any(|known| known.agent == entry.agent)
            || agents.iter().any(|known| known.pass == entry.pass)
        {
            return Err(PipelineRosterError::Coverage(entry.agent));
        }
        let tier_ceiling = Tier::from(entry.tier_ceiling);
        if tier_ceiling != entry.pass.produced_tier() {
            return Err(PipelineRosterError::CeilingMismatch(entry.agent));
        }
        agents.push(PipelineAgent {
            id: format!("{}@{}", entry.agent, entry.version),
            agent: entry.agent,
            version: entry.version,
            name: entry.name,
            role_line: entry.role_line,
            glyph: entry.glyph,
            pass: entry.pass,
            tier_ceiling,
            confidence_ceiling: tier_ceiling.confidence_ceiling(),
            dispatchable: false,
        });
    }
    if let Some(missing) = PipelinePass::ALL
        .into_iter()
        .find(|pass| !agents.iter().any(|agent| agent.pass == *pass))
    {
        return Err(PipelineRosterError::Coverage(format!("{missing:?}")));
    }
    Ok(agents)
}

/// One pipeline agent with the work it is part of in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineAgentStatus {
    /// The definition.
    #[serde(flatten)]
    pub agent: PipelineAgent,
    /// Running while any work that runs the pass is in flight.
    pub state: AgentState,
    /// Work in flight that runs the pass: recoveries for the IaC Grapher,
    /// semantic previews for the Flow Tracer and the Contract Matcher.
    pub active: usize,
}

/// The pipeline roster with each agent's in-flight work, `active(pass)`
/// being how many runs of work that includes the pass are in flight.
pub fn pipeline_status(
    agents: Vec<PipelineAgent>,
    active: impl Fn(PipelinePass) -> usize,
) -> Vec<PipelineAgentStatus> {
    agents
        .into_iter()
        .map(|agent| {
            let active = active(agent.pass);
            PipelineAgentStatus {
                state: if active > 0 {
                    AgentState::Running
                } else {
                    AgentState::Idle
                },
                agent,
                active,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "pipeline_roster_tests.rs"]
mod tests;
