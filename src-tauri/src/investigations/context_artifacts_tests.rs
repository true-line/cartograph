use super::super::coverage_tests::{Fixture, Sink, inventory_request, request};
use super::*;

const ARTIFACT: &str = "topology";

fn read(context: &FrozenContext, offset: usize, max_bytes: usize) -> InvestigationArtifactRead {
    InvestigationArtifactRead {
        context_id: context.ledger.coverage.as_ref().unwrap().context_id.clone(),
        artifact_id: ARTIFACT.into(),
        expected_content_id: None,
        offset,
        max_bytes,
    }
}

// AC-1814: an in-app artifact read compiles the frozen projection, never the
// live store; pages are canonical, fit their bound, continue by content id,
// and land after the coverage bodies without becoming citations.
#[test]
fn artifact_pages_compile_the_frozen_basis_and_page_within_their_bound() {
    let fixture = Fixture::new(40);
    let mut prepared = fixture.prepare(&request()).unwrap();
    // A whole-system task holds no graph; the artifact is compiled from the
    // complete graph at its pinned revision.
    assert!(prepared.graph.0.is_empty());
    let complete = fixture.state.graph.lock().unwrap().read_snapshot().unwrap();
    let whole = compile_frozen(&complete, prepared.graph_limits, ARTIFACT).unwrap();
    assert!(whole.content.len() > 120, "fixture must need several pages");
    // Bound each page to its envelope plus 40 content bytes.
    let mut envelope = page(&whole, &read(&prepared, 0, 1 << 20), true).unwrap();
    envelope.content.clear();
    envelope.next_offset = Some(whole.content.len());
    let bound = serde_json::to_string(&envelope).unwrap().len() + 40;
    // A store write that leaves the complete graph identical is read through.
    fixture.publish(0, "job:replacement", 2);
    let mut sink = Sink::default();
    let first = read(&prepared, 0, bound);
    prepared
        .read_artifact(&fixture.state, first.clone(), &mut sink)
        .unwrap();
    let raw = prepared.inventory_details.last().unwrap().clone();
    assert!(raw.len() <= bound);
    assert!(page_next(&raw).is_some());
    let page: InvestigationArtifactPage = serde_json::from_str(&raw).unwrap();
    assert_eq!(serde_json::to_string(&page).unwrap(), raw);
    assert_eq!(page.content_id, whole.content_id);
    assert!(page.complete_system);
    assert_eq!(page.total_content_bytes, whole.content.len());
    let manifest = &prepared.ledger.artifact_reads[0];
    assert_eq!(manifest.response_hash, content_hash(raw.as_bytes()));
    assert_eq!(manifest.response_bytes, raw.len());
    assert!(prepared.ledger.citations.is_empty());
    assert!(prepared.ledger.selected_facts.is_empty());
    assert_eq!(sink.publications, 1);

    // Continuations reassemble the whole artifact.
    let mut content = page.content;
    let mut next = page.next_offset;
    while let Some(offset) = next {
        let mut request = read(&prepared, offset, bound);
        request.expected_content_id = Some(whole.content_id.clone());
        prepared
            .read_artifact(&fixture.state, request, &mut sink)
            .unwrap();
        let page: InvestigationArtifactPage =
            serde_json::from_str(prepared.inventory_details.last().unwrap()).unwrap();
        assert_eq!(page.offset, offset);
        assert!(prepared.inventory_details.last().unwrap().len() <= bound);
        content.push_str(&page.content);
        next = page.next_offset;
    }
    assert_eq!(content, whole.content);
    // A later coverage read is retained before every artifact page, the
    // order prompt preparation reads.
    let pages = prepared.inventory_details.clone();
    let coverage = inventory_request(&prepared, &fixture.scope[0].repo_key);
    prepared.read_inventory(coverage, &mut sink).unwrap();
    assert_eq!(prepared.inventory_details[1..], pages[..]);
    assert!(
        serde_json::from_str::<InvestigationCoverageDetail>(&prepared.inventory_details[0]).is_ok()
    );
    // A changed graph is never compiled into this task.
    fixture.publish(0, "job:changed", 41);
    let before = prepared.ledger.clone();
    assert_eq!(
        prepared.read_artifact(&fixture.state, read(&prepared, 0, bound), &mut sink),
        Err(HostError::InputChanged)
    );
    assert_eq!(prepared.ledger, before);
    assert_eq!(
        prepared.inventory_details.len(),
        prepared.ledger.coverage_reads.len() + prepared.ledger.artifact_reads.len()
    );
}

// AC-1814: stale continuations, excluded or unknown ids, foreign contexts and
// bounds too small for any content are refused without touching the ledger.
#[test]
fn artifact_reads_refuse_stale_excluded_and_unfittable_requests() {
    let fixture = Fixture::new(40);
    let mut prepared = fixture.prepare(&request()).unwrap();
    let before = prepared.ledger.clone();
    let mut sink = Sink::default();
    let mut stale = read(&prepared, 1, 1024);
    stale.expected_content_id = Some(format!("spec-artifact-v1:{}", "0".repeat(64)));
    assert_eq!(
        prepared.read_artifact(&fixture.state, stale, &mut sink),
        Err(HostError::InputChanged)
    );
    for id in [
        "user-stories",
        "coverage",
        "d1-product-specification",
        "fact-index",
        "nope",
    ] {
        let mut excluded = read(&prepared, 0, 1024);
        excluded.artifact_id = id.into();
        assert!(!investigation_readable(id));
        assert_eq!(
            prepared.read_artifact(&fixture.state, excluded, &mut sink),
            Err(HostError::InvalidInput)
        );
    }
    let mut foreign = read(&prepared, 0, 1024);
    foreign.context_id.push('0');
    assert_eq!(
        prepared.read_artifact(&fixture.state, foreign, &mut sink),
        Err(HostError::InvalidInput)
    );
    assert_eq!(
        prepared.read_artifact(&fixture.state, read(&prepared, 0, 64), &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(prepared.ledger, before);
    assert!(prepared.inventory_details.is_empty());
    assert_eq!(sink.publications, 0);
    assert!(investigation_readable(ARTIFACT));
}

fn page_next(raw: &str) -> Option<usize> {
    serde_json::from_str::<InvestigationArtifactPage>(raw)
        .unwrap()
        .next_offset
}

// AC-1815: the agent core's artifact list is exactly the MCP-readable
// catalogue without the artifacts an investigation cannot compile from its
// frozen graph, and every listed artifact compiles from a small graph.
#[test]
fn investigation_artifacts_are_the_graph_only_readable_catalogue() {
    let excluded = [
        crate::mcp_reads::curated_read::ARTIFACT_ID,
        crate::mcp_reads::curated_read::JSON_ARTIFACT_ID,
        "d1-product-specification",
        "bundle-manifest",
        "coverage",
        "coverage-index",
        "user-stories",
    ];
    let expected: Vec<_> = crate::mcp_reads::artifacts::readable_catalog()
        .into_iter()
        .map(|artifact| artifact.id)
        .filter(|id| !excluded.contains(id))
        .collect();
    assert_eq!(
        expected,
        agents::investigation::INVESTIGATION_ARTIFACT_IDS.to_vec()
    );
    let fixture = Fixture::new(4);
    let complete = fixture.state.graph.lock().unwrap().read_snapshot().unwrap();
    let limits = fixture.prepare(&request()).unwrap().graph_limits;
    for id in agents::investigation::INVESTIGATION_ARTIFACT_IDS {
        assert!(investigation_readable(id));
        assert_eq!(compile_frozen(&complete, limits, id).unwrap().id, id);
    }
}
