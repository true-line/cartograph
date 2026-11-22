use super::*;
use crate::investigation::{
    Availability, InvestigationProviderMode, InvestigationStatus, SpecialistRole,
};

fn file(agents: &str) -> String {
    format!(r#"{{"schema_version":1,"agents":[{agents}]}}"#)
}

fn entry(agent: &str, role: &str, ceiling: &str) -> String {
    format!(
        r#"{{"agent":"{agent}","name":"N","role":"{role}","glyph":"x","role_line":"L","tier_ceiling":"{ceiling}"}}"#
    )
}

fn all_roles(ceiling: &str) -> Vec<String> {
    SpecialistRole::ALL
        .iter()
        .map(|role| {
            entry(
                &format!("agt-{}", role.slug().replace('-', "")),
                role.slug(),
                ceiling,
            )
        })
        .collect()
}

// AC-1408: the checked-in roster names every role once, each at its role's
// current version, with the agent version equal to the specialist version.
#[test]
fn roster_v1_names_every_role_at_its_current_version() {
    let roster = roster().unwrap();
    assert_eq!(
        roster
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<Vec<_>>(),
        [
            "agt-analyst@28",
            "agt-auditor@14",
            "agt-spec@2",
            "agt-gap@2",
            "agt-section@1",
        ]
    );
    for agent in &roster {
        assert_eq!(agent.specialist_id.role(), agent.role);
        assert_eq!(agent.specialist_id.availability(), Availability::Current);
        assert_eq!(agent.version, agent.specialist_id.row().version);
        assert_eq!(agent.tier_ceiling, Tier::Agentic);
        assert_eq!(agent.confidence_ceiling, ConfidenceTier::InferredWeak);
        assert_eq!(agent.glyph.chars().count(), 1);
    }
    let analyst = &roster[0];
    assert_eq!(analyst.name, "Domain Analyst");
    assert_eq!(
        serde_json::to_value(analyst).unwrap()["specialist_id"],
        "domain-analyst@28"
    );
}

// AC-1408: a saved run of any version, retired included, names its role's
// agent, so the version that produced a message stays attributable.
#[test]
fn every_specialist_version_names_its_role_agent() {
    for id in SpecialistId::all() {
        let agent = roster_agent_for(id).unwrap();
        assert_eq!(agent.role, id.role());
    }
    assert_eq!(
        roster_agent_for(SpecialistId::DomainAnalystV9)
            .unwrap()
            .agent,
        "agt-analyst"
    );
}

// AC-1408: the ceiling admits Gap and output up to it, never above. A T2
// ceiling cannot yield Confirmed; a T3 ceiling cannot yield InferredStrong.
#[test]
fn ceiling_admits_only_output_at_or_below_it() {
    let agentic = roster().unwrap().remove(0);
    assert!(agentic.admits(ConfidenceTier::Gap));
    assert!(agentic.admits(ConfidenceTier::InferredWeak));
    assert!(!agentic.admits(ConfidenceTier::InferredStrong));
    assert!(!agentic.admits(ConfidenceTier::Confirmed));

    let semantic = RosterAgent {
        tier_ceiling: Tier::Semantic,
        confidence_ceiling: Tier::Semantic.confidence_ceiling(),
        ..agentic
    };
    assert!(semantic.admits(ConfidenceTier::InferredStrong));
    assert!(semantic.admits(ConfidenceTier::InferredWeak));
    assert!(!semantic.admits(ConfidenceTier::Confirmed));
}

// AC-1408: every bound specialist's output confidence is within the ceiling
// the roster gives it, and every ceiling is the tier's own ceiling.
#[test]
fn bound_specialist_output_is_within_each_ceiling() {
    for agent in roster().unwrap() {
        let produced = agent.specialist_id.definition().unwrap().confidence_tier;
        assert!(agent.admits(produced), "{}", agent.id);
        assert_eq!(
            agent.confidence_ceiling,
            agent.tier_ceiling.confidence_ceiling()
        );
    }
}

// AC-1408: a definition that is malformed, duplicated, incomplete, names an
// unknown role or claims less room than its specialist needs is refused.
#[test]
fn invalid_roster_definitions_are_refused() {
    let ok = all_roles("agentic");
    assert!(parse_roster(&file(&ok.join(","))).is_ok());

    assert!(matches!(
        parse_roster(r#"{"schema_version":2,"agents":[]}"#),
        Err(RosterError::Invalid(_))
    ));
    assert!(matches!(
        parse_roster(&file(&ok[1..].join(","))),
        Err(RosterError::Coverage(_))
    ));
    let mut duplicate_role = ok.clone();
    duplicate_role.push(entry("agt-another", "gap-triager", "agentic"));
    assert!(matches!(
        parse_roster(&file(&duplicate_role.join(","))),
        Err(RosterError::Coverage(_))
    ));
    let mut duplicate_id = ok.clone();
    duplicate_id[1] = entry("agt-domainanalyst", "evidence-auditor", "agentic");
    assert!(matches!(
        parse_roster(&file(&duplicate_id.join(","))),
        Err(RosterError::Coverage(_))
    ));
    let mut unknown = ok.clone();
    unknown[0] = entry("agt-flow", "flow-tracer", "semantic");
    assert!(matches!(
        parse_roster(&file(&unknown.join(","))),
        Err(RosterError::UnknownRole(_))
    ));
    let mut bad_id = ok.clone();
    bad_id[0] = entry("analyst", "domain-analyst", "agentic");
    assert!(matches!(
        parse_roster(&file(&bad_id.join(","))),
        Err(RosterError::Invalid(_))
    ));
    let extra_field = ok[0].replace(r#""glyph""#, r#""model":"x","glyph""#);
    assert!(matches!(
        parse_roster(&file(&[extra_field].join(","))),
        Err(RosterError::Invalid(_))
    ));
    // The specialists emit InferredWeak; a Gap-only ceiling does not exist as
    // a tier, so the narrowest is Agentic. Wider ceilings stay admissible.
    assert!(parse_roster(&file(&all_roles("semantic").join(","))).is_ok());
}

// AC-1408: an agent is running while any of its role's investigations is in
// flight, counted per provider mode; finished work leaves it idle.
#[test]
fn roster_status_counts_in_flight_work_per_agent_and_mode() {
    use InvestigationProviderMode::{Cloud, Local};
    use InvestigationStatus as St;
    let work = [
        (SpecialistId::DomainAnalystV28, Local, St::Running),
        (SpecialistId::DomainAnalystV27, Cloud, St::AwaitingConsent),
        (SpecialistId::DomainAnalystV18, Local, St::Queued),
        (SpecialistId::EvidenceAuditorV14, Local, St::Completed),
        (SpecialistId::EvidenceAuditorV13, Cloud, St::Failed),
        (SpecialistId::GapTriagerV2, Cloud, St::Preparing),
        (SpecialistId::SpecWriterV2, Local, St::Cancelled),
        (SpecialistId::SectionWriterV1, Local, St::Interrupted),
        (SpecialistId::SectionWriterV1, Cloud, St::OutcomeUnknown),
    ];
    let status = roster_status(roster().unwrap(), &work);
    let row = |agent: &str| status.iter().find(|row| row.agent.agent == agent).unwrap();
    let analyst = row("agt-analyst");
    assert_eq!(
        (analyst.state, analyst.local_active, analyst.cloud_active),
        (AgentState::Running, 2, 1)
    );
    let gap = row("agt-gap");
    assert_eq!(
        (gap.state, gap.local_active, gap.cloud_active),
        (AgentState::Running, 0, 1)
    );
    for idle in ["agt-auditor", "agt-spec", "agt-section"] {
        let row = row(idle);
        assert_eq!(
            (row.state, row.local_active, row.cloud_active),
            (AgentState::Idle, 0, 0)
        );
    }
    let wire = serde_json::to_value(analyst).unwrap();
    assert_eq!(wire["id"], "agt-analyst@28");
    assert_eq!(wire["state"], "running");
    assert_eq!(wire["tier_ceiling"], "Agentic");
    assert_eq!(wire["confidence_ceiling"], "InferredWeak");
    assert_eq!(wire["role"], "domain-analyst");
}
