use super::*;

fn file(agents: &str) -> String {
    format!(r#"{{"schema_version":1,"agents":[{agents}]}}"#)
}

fn entry(agent: &str, pass: &str, ceiling: &str) -> String {
    format!(
        r#"{{"agent":"{agent}","version":1,"name":"N","pass":"{pass}","glyph":"x","role_line":"L","tier_ceiling":"{ceiling}"}}"#
    )
}

fn all_passes() -> Vec<String> {
    vec![
        entry("agt-flow", "flow_trace", "semantic"),
        entry("agt-contract", "contract_match", "semantic"),
        entry("agt-iac", "iac_graph", "deterministic"),
    ]
}

// AC-1808: the checked-in pipeline roster names Flow Tracer, Contract
// Matcher and IaC Grapher once each, read-only, at their pass's tier.
#[test]
fn pipeline_v1_names_every_pass_read_only_at_its_tier() {
    let agents = pipeline_roster().unwrap();
    assert_eq!(
        agents
            .iter()
            .map(|agent| (agent.id.as_str(), agent.name.as_str(), agent.tier_ceiling))
            .collect::<Vec<_>>(),
        [
            ("agt-flow@1", "Flow Tracer", Tier::Semantic),
            ("agt-contract@1", "Contract Matcher", Tier::Semantic),
            ("agt-iac@1", "IaC Grapher", Tier::Deterministic),
        ]
    );
    for agent in &agents {
        assert!(!agent.dispatchable);
        assert_eq!(agent.tier_ceiling, agent.pass.produced_tier());
        assert_eq!(
            agent.confidence_ceiling,
            agent.tier_ceiling.confidence_ceiling()
        );
        assert_eq!(agent.glyph.chars().count(), 1);
    }
    // A T2 pipeline agent never yields Confirmed; T0 recovery may.
    assert_eq!(agents[1].confidence_ceiling, ConfidenceTier::InferredStrong);
    assert_eq!(agents[2].confidence_ceiling, ConfidenceTier::Confirmed);
    let wire = serde_json::to_value(&agents[0]).unwrap();
    assert_eq!(wire["pass"], "flow_trace");
    assert_eq!(wire["dispatchable"], false);
}

// AC-1808: a malformed, duplicated or incomplete definition, an id a
// specialist agent already uses, or a ceiling other than the pass produces
// is refused.
#[test]
fn invalid_pipeline_definitions_are_refused() {
    let ok = all_passes();
    assert!(parse_pipeline(&file(&ok.join(","))).is_ok());
    assert!(matches!(
        parse_pipeline(r#"{"schema_version":2,"agents":[]}"#),
        Err(PipelineRosterError::Invalid(_))
    ));
    assert!(matches!(
        parse_pipeline(&file(&ok[1..].join(","))),
        Err(PipelineRosterError::Coverage(_))
    ));
    let mut duplicate_pass = ok.clone();
    duplicate_pass.push(entry("agt-other", "iac_graph", "deterministic"));
    assert!(matches!(
        parse_pipeline(&file(&duplicate_pass.join(","))),
        Err(PipelineRosterError::Coverage(_))
    ));
    let mut specialist_id = ok.clone();
    specialist_id[0] = entry("agt-analyst", "flow_trace", "semantic");
    assert!(matches!(
        parse_pipeline(&file(&specialist_id.join(","))),
        Err(PipelineRosterError::Coverage(_))
    ));
    let mut overclaim = ok.clone();
    overclaim[1] = entry("agt-contract", "contract_match", "deterministic");
    assert!(matches!(
        parse_pipeline(&file(&overclaim.join(","))),
        Err(PipelineRosterError::CeilingMismatch(_))
    ));
    let zero = ok[0].replace(r#""version":1"#, r#""version":0"#);
    assert!(matches!(
        parse_pipeline(&file(&[zero, ok[1].clone(), ok[2].clone()].join(","))),
        Err(PipelineRosterError::Invalid(_))
    ));
    let unknown_pass = entry("agt-flow", "flow_tracer", "semantic");
    assert!(matches!(
        parse_pipeline(&file(
            &[unknown_pass, ok[1].clone(), ok[2].clone()].join(",")
        )),
        Err(PipelineRosterError::Invalid(_))
    ));
}

// AC-1808: a pipeline agent is running while work that runs its pass is in
// flight, and idle otherwise.
#[test]
fn pipeline_status_follows_the_work_in_flight() {
    let status = pipeline_status(pipeline_roster().unwrap(), |pass| match pass {
        PipelinePass::IacGraph => 2,
        PipelinePass::FlowTrace | PipelinePass::ContractMatch => 0,
    });
    let row = |agent: &str| status.iter().find(|row| row.agent.agent == agent).unwrap();
    assert_eq!(
        (row("agt-iac").state, row("agt-iac").active),
        (AgentState::Running, 2)
    );
    for idle in ["agt-flow", "agt-contract"] {
        assert_eq!((row(idle).state, row(idle).active), (AgentState::Idle, 0));
    }
    let wire = serde_json::to_value(row("agt-iac")).unwrap();
    assert_eq!(wire["id"], "agt-iac@1");
    assert_eq!(wire["state"], "running");
    assert_eq!(wire["confidence_ceiling"], "Confirmed");
}
