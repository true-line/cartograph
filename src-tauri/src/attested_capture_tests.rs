//! AC-1079 (T-1079, ADR-0090 decision 9): the engineer's capture paths.
use super::*;
use agents::attested::{AgentVerb, Standing};
use core_graph::{FactKey, GraphStore, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;

fn capability(id: &str) -> Node {
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/login.ts".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: "Capability".into(),
        props: json!({ "name": "Login", "prov": prov }),
    }
}

fn id(node: &str) -> String {
    spec::citation_id(&FactKey::Node { id: node.into() })
}

/// A state with `cap:login` and the open Gap `gap:login-owner`.
fn state() -> (tempfile::TempDir, crate::AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(dir.path());
    {
        let mut graph = state.graph.lock().unwrap();
        graph.put_node(&capability("cap:login")).unwrap();
        graph
            .put_node(&Node {
                id: "gap:login-owner".into(),
                label: "Gap".into(),
                props: json!({"missing": "owner"}),
            })
            .unwrap();
    }
    (dir, state)
}

// Placeholder contributor only (ADR-0090 decision 4).
const ENGINEER: &str = "Placeholder Engineer";

#[test]
fn the_engineer_answers_a_gap_from_the_register_engineer_backed() {
    let (_dir, state) = state();
    assert_eq!(
        state_gap_answer(
            &state,
            "gap:login-owner",
            "owner",
            "The login page is owned by the identity team.",
            vec![id("cap:login")],
            1,
        )
        .unwrap_err(),
        "no_engineer"
    );
    let engineer = set_engineer(&state, ENGINEER, None).unwrap();
    assert!(set_engineer(&state, "Another Placeholder", None).is_err());

    let stored = state_gap_answer(
        &state,
        "gap:login-owner",
        "owner",
        "The login page is owned by the identity team.",
        vec![id("cap:login")],
        1_700_000_000_000,
    )
    .unwrap();
    assert_eq!(stored.standing, Standing::EngineerBacked);
    assert!(stored.record.recorded_hashes.contains_key(&id("cap:login")));
    assert_eq!(
        stored.record.attestations,
        vec![Attestation::Engineer {
            contributor: engineer.id,
            verb: EngineerVerb::Stated,
            at_ms: 1_700_000_000_000,
            source: AttestationSource::GapsRegister,
            reference: None,
        }]
    );
    // The name is in the contributor table only, never in the record.
    let record = serde_json::to_string(&stored.record).unwrap();
    assert!(!record.contains(ENGINEER));
    // The Gap stays a Gap.
    let graph = state.graph.lock().unwrap();
    let (nodes, _) = graph.read_snapshot().unwrap();
    let gap = nodes.iter().find(|n| n.id == "gap:login-owner").unwrap();
    assert!(context_hub::curated::is_gap(gap));
}

#[test]
fn a_gap_answer_needs_an_open_gap_and_supporting_facts_the_graph_holds() {
    let (_dir, state) = state();
    set_engineer(&state, ENGINEER, None).unwrap();
    let answer = |gap: &str, supporting: Vec<String>| {
        state_gap_answer(
            &state,
            gap,
            "owner",
            "Owned by the identity team.",
            supporting,
            1,
        )
    };
    assert_eq!(answer("gap:unknown", vec![]).unwrap_err(), "gap_not_found");
    assert_eq!(answer("cap:login", vec![]).unwrap_err(), "gap_recovered");
    assert_eq!(
        answer("gap:login-owner", vec![id("cap:missing")]).unwrap_err(),
        "supporting_fact_not_found"
    );
    assert!(answer("gap:login-owner", vec![]).is_ok());
}

#[test]
fn the_engineer_confirms_an_agents_record_and_withdraws_it_at_the_count_read() {
    let (_dir, state) = state();
    set_engineer(&state, ENGINEER, None).unwrap();
    let staged = {
        let drafts = state.drafts.lock().unwrap();
        let store = drafts.attested();
        let agent = store.agent("gap-filler@1", "local", "fixture").unwrap();
        store
            .stage(AttestedRecord {
                target: AttestedTarget::Gap {
                    gap_id: "gap:login-owner".into(),
                    slot: "owner".into(),
                },
                statement: "The identity team owns login.".into(),
                supporting: Vec::new(),
                recorded_hashes: Default::default(),
                attestations: vec![Attestation::Agent {
                    contributor: agent.id,
                    verb: AgentVerb::Stated,
                    role: "gap-filler@1".into(),
                    definition_fingerprint: "definition".into(),
                    provider: "local".into(),
                    model: "fixture".into(),
                    run: "run:1".into(),
                    directed_by: None,
                }],
            })
            .unwrap()
    };
    assert_eq!(staged.standing, Standing::AgentAttestedUnconfirmed);
    let review = |count, verb| {
        engineer_attest(
            &state,
            &staged.record_id,
            count,
            verb,
            AttestationSource::Workbench,
            None,
            2,
        )
    };
    assert_eq!(
        review(1, EngineerVerb::Stated).unwrap_err(),
        "stated_is_not_a_review"
    );
    let confirmed = review(1, EngineerVerb::Confirmed).unwrap();
    assert_eq!(confirmed.standing, Standing::EngineerBacked);
    assert!(review(1, EngineerVerb::Withdrew).is_err(), "stale count");
    let withdrawn = review(2, EngineerVerb::Withdrew).unwrap();
    assert_eq!(withdrawn.standing, Standing::Withdrawn);

    // AC-1081: the surfaces read the label and each contributor by name.
    let page = attested_views(&state, 10, None).unwrap();
    assert_eq!(page.items.len(), 1);
    let view = &page.items[0];
    assert_eq!(view.attestation_count, 3);
    assert_eq!(view.label, "withdrawn");
    assert_eq!(
        view.contributors
            .iter()
            .map(|c| (c.display_name.as_str(), c.verb.as_str(), c.run.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("gap-filler@1 (local/fixture)", "stated", Some("run:1")),
            (ENGINEER, "confirmed", None),
            (ENGINEER, "withdrew", None),
        ]
    );
}

// The engineer's answer with the content an agent already stated is
// appended to that record, once (Codex review of #2528).
#[test]
fn the_engineers_answer_to_an_agents_identical_record_is_appended_once() {
    let (_dir, state) = state();
    set_engineer(&state, ENGINEER, None).unwrap();
    let statement = "The identity team owns login.";
    let recorded_hashes = {
        let graph = state.graph.lock().unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        spec::cited_fact_hashes(&nodes, &edges, &BTreeSet::from([id("cap:login")]))
    };
    let agents = {
        let drafts = state.drafts.lock().unwrap();
        let store = drafts.attested();
        let agent = store.agent("gap-filler@1", "local", "fixture").unwrap();
        store
            .stage(AttestedRecord {
                target: AttestedTarget::Gap {
                    gap_id: "gap:login-owner".into(),
                    slot: "owner".into(),
                },
                statement: statement.into(),
                supporting: vec![id("cap:login")],
                recorded_hashes,
                attestations: vec![Attestation::Agent {
                    contributor: agent.id,
                    verb: AgentVerb::Stated,
                    role: "gap-filler@1".into(),
                    definition_fingerprint: "definition".into(),
                    provider: "local".into(),
                    model: "fixture".into(),
                    run: "run:1".into(),
                    directed_by: None,
                }],
            })
            .unwrap()
    };
    let answer = || {
        state_gap_answer(
            &state,
            "gap:login-owner",
            "owner",
            statement,
            vec![id("cap:login")],
            3,
        )
        .unwrap()
    };
    let stated = answer();
    assert_eq!(stated.record_id, agents.record_id);
    assert_eq!(stated.standing, Standing::EngineerBacked);
    assert_eq!(stated.attestation_count(), 2);
    assert_eq!(answer().attestation_count(), 2);
}
