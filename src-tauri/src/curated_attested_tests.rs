//! AC-1077 (T-1077): the host reads the attested records from the section
//! draft store's file, the curated projection states each against the
//! graph the read took, and the MCP read returns the same records and
//! states per export mode.

use super::*;
use crate::curated_workbench::AttestedRead;
use agents::attested::{
    AgentVerb, Attestation, AttestationSource, AttestedRecord, AttestedTarget, EngineerVerb,
};
use agents::attested_store::Contributor;
use agents::draft_store::DraftStore;
use context_hub::curated::{AttestedState, AttestedStateReason};
use core_graph::{FactKey, GraphStore};
use core_prov::EvidenceRef;
use serde_json::json;
use spec::ExportMode;

// Placeholder contributors only (ADR-0090 decision 4).
const ENGINEER: &str = "Placeholder Engineer";

struct Fixture {
    _dir: tempfile::TempDir,
    graph: SqliteGraphStore,
    reviews: StoredReviews,
}

fn rule(id: &str, note: &str) -> Node {
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/app.ts".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "fixture",
        // The hash a draft or an attested record records covers the
        // provenance, so a changed source changes it.
        note.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: "Rule".into(),
        props: json!({ "name": id, "note": note, "prov": prov }),
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    graph.put_node(&rule("rule:refund", "as read")).unwrap();
    graph
        .put_node(&Node {
            id: "gap:refund".into(),
            label: "Gap".into(),
            props: json!({ "name": "who approves a refund" }),
        })
        .unwrap();
    Fixture {
        graph,
        reviews: StoredReviews {
            proposals: Mutex::new(
                agents::ProposalStore::open(dir.path().join("proposals.sqlite")).unwrap(),
            ),
            drafts: Mutex::new(DraftStore::open(dir.path().join("section-drafts.sqlite")).unwrap()),
        },
        _dir: dir,
    }
}

fn citation(node: &str) -> String {
    spec::citation_id(&FactKey::Node { id: node.into() })
}

fn agent_says(agent: &Contributor, verb: AgentVerb) -> Attestation {
    Attestation::Agent {
        contributor: agent.id.clone(),
        verb,
        role: "gap-filler@1".into(),
        definition_fingerprint: "fp-1".into(),
        provider: "local".into(),
        model: "placeholder-model".into(),
        run: "run-7".into(),
        directed_by: None,
    }
}

impl Fixture {
    fn drafts(&self) -> std::sync::MutexGuard<'_, DraftStore> {
        self.reviews.drafts.lock().unwrap()
    }

    /// A record answering the refund Gap, resting on `rule:refund` with the
    /// hash it has in the graph now.
    fn record(&self, attestations: Vec<Attestation>) -> AttestedRecord {
        let (nodes, edges) = self.graph.read_snapshot().unwrap();
        let cited = BTreeSet::from([citation("rule:refund")]);
        AttestedRecord {
            target: AttestedTarget::Gap {
                gap_id: "gap:refund".into(),
                slot: "approved_by".into(),
            },
            statement: "A refund over the limit is approved by a supervisor.".into(),
            supporting: cited.iter().cloned().collect(),
            recorded_hashes: spec::cited_fact_hashes(&nodes, &edges, &cited),
            attestations,
        }
    }

    fn project(&self, review: &CuratedReview) -> CuratedProjection {
        let (revision, ((nodes, edges), coverage)) = read_graph(&self.graph).unwrap();
        let content_hash = spec::graph_content_hash(&nodes, &edges);
        project(
            review,
            &Snapshot {
                nodes: &nodes,
                edges: &edges,
                overlay: &[],
                derived_decisions: DerivedDecisions::Compile(&[]),
                coverage: &coverage,
                content_hash: &content_hash,
                revision: &revision,
            },
            &[],
        )
        .unwrap()
    }
}

#[test]
fn an_attested_record_is_read_from_the_store_and_follows_its_review_and_facts() {
    let mut fixture = fixture();
    let none = fixture.project(&fixture.reviews.gather());
    assert!(none.attested.is_empty());

    let (agent, staged) = {
        let drafts = fixture.drafts();
        let store = drafts.attested();
        let agent = store
            .agent("gap-filler@1", "local", "placeholder-model")
            .unwrap();
        let staged = store
            .stage(fixture.record(vec![agent_says(&agent, AgentVerb::Stated)]))
            .unwrap();
        (agent, staged)
    };
    let gathered = fixture.reviews.gather();
    let projection = fixture.project(&gathered);
    let [record] = projection.attested.as_slice() else {
        panic!("one attested record");
    };
    assert_eq!(record.citation_id, staged.citation_id);
    assert_eq!(
        (record.state, record.reason),
        (
            AttestedState::AgentAttestedUnconfirmed,
            AttestedStateReason::AgentsOnly
        )
    );
    assert_eq!((record.tier, record.confidence), (None, None));
    assert_ne!(projection.revision, none.revision);
    // The graph is not written, and the Gap is still a Gap.
    assert_eq!(projection.graph_basis, none.graph_basis);
    let (nodes, _) = fixture.graph.read_snapshot().unwrap();
    assert!(
        nodes
            .iter()
            .any(|node| node.id == "gap:refund" && node.label == "Gap")
    );
    assert_eq!(fixture.reviews.unchanged(&gathered), Ok(true));

    // The engineer confirms: the read made before is stale, and the
    // record is eligible, the engineer named from the contributor table.
    {
        let drafts = fixture.drafts();
        let store = drafts.attested();
        let engineer = store.set_engineer(ENGINEER, None).unwrap();
        store
            .attest(
                &staged.record_id,
                1,
                Attestation::Engineer {
                    contributor: engineer.id,
                    verb: EngineerVerb::Confirmed,
                    at_ms: 1_700_000_000_000,
                    source: AttestationSource::GapsRegister,
                    reference: None,
                },
            )
            .unwrap();
    }
    assert_eq!(fixture.reviews.unchanged(&gathered), Ok(false));
    let confirmed = fixture.project(&fixture.reviews.gather());
    let record = &confirmed.attested[0];
    assert_eq!(record.state, AttestedState::Eligible);
    let names: Vec<_> = record
        .attestations
        .iter()
        .map(|a| {
            (
                a.display_name.as_deref(),
                a.role.as_deref(),
                a.verb.as_str(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            (None, Some("gap-filler@1"), "stated"),
            (Some(ENGINEER), None, "confirmed")
        ]
    );
    assert_eq!(record.attestations[0].contributor_id, agent.id.0);

    // A supporting fact whose source changes makes it stale.
    fixture
        .graph
        .put_node(&rule("rule:refund", "changed since"))
        .unwrap();
    let stale = fixture.project(&fixture.reviews.gather());
    assert_eq!(
        (stale.attested[0].state, stale.attested[0].reason),
        (AttestedState::Stale, AttestedStateReason::CitedFactChanged)
    );
    assert_eq!(stale.attested[0].changed, [citation("rule:refund")]);
}

#[test]
fn the_mcp_read_shows_attested_records_in_best_effort_only_with_their_label() {
    let fixture = fixture();
    {
        let drafts = fixture.drafts();
        let store = drafts.attested();
        let agent = store
            .agent("gap-filler@1", "local", "placeholder-model")
            .unwrap();
        store
            .stage(fixture.record(vec![agent_says(&agent, AgentVerb::Stated)]))
            .unwrap();
    }
    let projection = fixture.project(&fixture.reviews.gather());

    let best = serde_json::to_value(AttestedRead::of(&projection, ExportMode::BestEffort)).unwrap();
    assert_eq!(best["attested"].as_array().unwrap().len(), 1);
    assert_eq!(best["attested"][0]["label"], "agent-attested, unconfirmed");
    assert_eq!(
        best["attested"][0]["statement"],
        "A refund over the limit is approved by a supervisor."
    );
    assert_eq!(
        best["attested"][0]["attestations"][0]["role"],
        "gap-filler@1"
    );
    assert_eq!(best["attested_omitted"], 0);
    assert_eq!(best["attested_counts"]["agent_attested_unconfirmed"], 1);

    // verified-only prints no attested statement and counts what it left
    // out.
    let verified =
        serde_json::to_value(AttestedRead::of(&projection, ExportMode::VerifiedOnly)).unwrap();
    assert_eq!(verified["attested"], json!([]));
    assert_eq!(verified["attested_omitted"], 1);
    assert_eq!(verified["attested_listed"], json!([]));
    assert!(!verified.to_string().contains("supervisor"));
}

#[test]
fn a_withdrawn_record_is_listed_without_its_statement_in_both_modes() {
    let fixture = fixture();
    {
        let drafts = fixture.drafts();
        let store = drafts.attested();
        let engineer = store.set_engineer(ENGINEER, None).unwrap();
        let engineer_says = |verb| Attestation::Engineer {
            contributor: engineer.id.clone(),
            verb,
            at_ms: 1_700_000_000_000,
            source: AttestationSource::Workbench,
            reference: None,
        };
        let staged = store
            .stage(fixture.record(vec![engineer_says(EngineerVerb::Stated)]))
            .unwrap();
        store
            .attest(&staged.record_id, 1, engineer_says(EngineerVerb::Withdrew))
            .unwrap();
    }
    let projection = fixture.project(&fixture.reviews.gather());
    for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
        let read = serde_json::to_value(AttestedRead::of(&projection, mode)).unwrap();
        assert_eq!(read["attested"], json!([]));
        assert_eq!(read["attested_omitted"], 0);
        assert_eq!(read["attested_listed"][0]["state"], "withdrawn");
        assert_eq!(read["attested_listed"][0]["cited_facts"], 1);
        assert!(!read.to_string().contains("supervisor"), "{mode:?}");
    }
}

#[test]
fn attested_records_are_bounded_by_their_bytes_before_they_are_kept() {
    let fixture = fixture();
    let count = 3;
    let mut one = 0;
    {
        let drafts = fixture.drafts();
        let store = drafts.attested();
        let agent = store
            .agent("gap-filler@1", "local", "placeholder-model")
            .unwrap();
        for index in 0..count {
            let mut record = fixture.record(vec![agent_says(&agent, AgentVerb::Stated)]);
            record.statement = format!("A refund over limit {index} is approved by a supervisor.");
            one = payload_bytes(&record, MAX_CURATED_ATTESTED_BYTES);
            store.stage(record).unwrap();
        }
    }
    let review = fixture.reviews.gather();
    assert_eq!(review.failure, None);
    assert_eq!(review.attested.len(), count);

    let store = fixture.drafts();
    // Every record has the same encoded size here.
    assert!(attested_records_within(&store, one * count).is_ok());
    assert_eq!(
        attested_records_within(&store, one * count - 1).map(|read| read.len()),
        Err(CuratedFailure::AcceptedTooLarge {
            limit_bytes: one * count - 1
        })
    );
}
