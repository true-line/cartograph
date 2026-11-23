//! AC-1079 (T-1079, ADR-0090 decision 9): an agent's proposal from a
//! finding, and agents attesting one another's records.
use super::*;
use crate::attested::Standing;
use crate::draft_store::DraftStore;
use crate::investigation::InvestigationEvidenceOrigin;
use core_prov::{ConfidenceTier, Tier};

const LOGIN: &str = "F-00000000000000c1";
const HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn finding(kind: InvestigationClaimKind, cites: &[&str]) -> InvestigationFinding {
    InvestigationFinding {
        finding_id: "finding-1".into(),
        claim_kind: kind,
        title: "Owner".into(),
        statement: "The identity team owns the login page.".into(),
        citation_ids: cites.iter().map(|id| (*id).to_owned()).collect(),
        limitations: Vec::new(),
        tier: Tier::Agentic,
        confidence_tier: ConfidenceTier::InferredWeak,
    }
}

fn citation(id: &str, node: &str) -> InvestigationCitation {
    InvestigationCitation {
        citation_id: id.into(),
        fact: TaskFactKey::Node { id: node.into() },
        fact_digest: String::new(),
        source: None,
        role: None,
        index: None,
        text_hash: None,
        origin: InvestigationEvidenceOrigin::GraphMetadata,
    }
}

fn fact_id(fact: &TaskFactKey) -> String {
    match fact {
        TaskFactKey::Node { id } => {
            assert_eq!(id, "cap:login");
            LOGIN.to_owned()
        }
        TaskFactKey::Edge { .. } => unreachable!(),
    }
}

fn run(n: u32, model: &str) -> ProposalRun {
    ProposalRun {
        run: format!("investigation-{n}"),
        role: "domain-analyst@29".into(),
        definition_fingerprint: "definition".into(),
        provider: "local".into(),
        model: model.into(),
    }
}

fn gap() -> AttestedTarget {
    AttestedTarget::Gap {
        gap_id: "gap:login-owner".into(),
        slot: "owner".into(),
    }
}

fn hashes(cited: &BTreeSet<String>) -> BTreeMap<String, String> {
    cited
        .iter()
        .map(|id| (id.clone(), HASH.to_owned()))
        .collect()
}

fn by(agent: &ContributorId) -> Proposer {
    Proposer {
        agent: agent.clone(),
        directed_by: None,
    }
}

#[test]
fn a_finding_becomes_an_agents_statement_citing_its_ledger_facts() {
    let agent = ContributorId("C-00000000000000a1".into());
    let citations = [citation("c1", "cap:login"), citation("c2", "cap:login")];
    let record = proposal(
        &run(1, "fixture"),
        &finding(InvestigationClaimKind::ImplementedBehavior, &["c1", "c2"]),
        &citations,
        gap(),
        &by(&agent),
        fact_id,
        hashes,
    )
    .unwrap();
    assert_eq!(record.statement, "The identity team owns the login page.");
    assert_eq!(record.supporting, vec![LOGIN.to_owned()]);
    assert_eq!(
        record.recorded_hashes.get(LOGIN).map(String::as_str),
        Some(HASH)
    );
    assert_eq!(
        record.attestations,
        vec![agent_attestation(
            &run(1, "fixture"),
            agent,
            AgentVerb::Stated,
            None
        )]
    );

    let refuse = |kind, cites: &[&str]| {
        proposal(
            &run(1, "fixture"),
            &finding(kind, cites),
            &citations,
            gap(),
            &by(&ContributorId("C-00000000000000a1".into())),
            fact_id,
            hashes,
        )
        .unwrap_err()
        .code()
    };
    assert_eq!(
        refuse(InvestigationClaimKind::ProposedDesign, &["c1"]),
        "finding_is_proposed_design"
    );
    assert_eq!(
        refuse(InvestigationClaimKind::ImplementedBehavior, &["c9"]),
        "finding_citation_unknown"
    );
}

#[test]
fn another_agent_agrees_with_an_identical_proposal_and_it_stays_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let drafts = DraftStore::open(dir.path().join("section-drafts.sqlite")).unwrap();
    let store = drafts.attested();
    let first = store
        .agent("domain-analyst@29", "local", "fixture")
        .unwrap();
    let second = store.agent("domain-analyst@29", "local", "other").unwrap();
    let citations = [citation("c1", "cap:login")];
    let propose = |run: &ProposalRun, agent: &ContributorId| {
        proposal(
            run,
            &finding(InvestigationClaimKind::ImplementedBehavior, &["c1"]),
            &citations,
            gap(),
            &by(agent),
            fact_id,
            hashes,
        )
        .unwrap()
    };
    let stored = store.stage(propose(&run(1, "fixture"), &first.id)).unwrap();
    assert_eq!(stored.standing, Standing::AgentAttestedUnconfirmed);
    assert_eq!(follow_up(&stored, &run(1, "fixture"), &by(&first.id)), None);

    // The second agent's identical proposal is the same record.
    let again = store.stage(propose(&run(2, "other"), &second.id)).unwrap();
    assert_eq!(again.record_id, stored.record_id);
    let agreed = follow_up(&again, &run(2, "other"), &by(&second.id)).unwrap();
    let both = store
        .attest(&again.record_id, again.attestation_count(), agreed)
        .unwrap();
    assert_eq!(both.attestation_count(), 2);
    assert_eq!(both.standing, Standing::AgentAttestedUnconfirmed);
    assert_eq!(follow_up(&both, &run(2, "other"), &by(&second.id)), None);
}
