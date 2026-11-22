//! The graph/capture association is explicit reviewed host selection, not a
//! retroactive claim that every graph fact has a producer receipt.
use super::*;
use std::io::Write;

pub(super) struct Observation {
    pub observed: Vec<ObservedCorpus>,
    pub guards: Vec<RetentionGuard>,
    /// Per corpus, the canonical graph and capture documents the observed
    /// digests hash. A locked store's corpus files are these bytes.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "production admits on the digests alone")
    )]
    pub documents: BTreeMap<String, (Vec<u8>, Vec<u8>)>,
    captures: BTreeMap<String, source_capture::Capture>,
}
fn evidence(props: &serde_json::Value) -> Result<Vec<core_prov::EvidenceRef>, String> {
    match props.get("prov").and_then(|p| p.get("evidence")) {
        Some(value) => serde_json::from_value(value.clone()).map_err(err),
        None => Ok(vec![]),
    }
}
fn bound_graph(nodes: &[Node], edges: &[Edge]) -> Result<(), String> {
    if nodes
        .len()
        .checked_add(edges.len())
        .is_none_or(|n| n > SnapshotReadLimits::MAX_ROWS)
    {
        return Err(err("graph row capacity"));
    }
    fn visit(value: &serde_json::Value, depth: usize, count: &mut usize) -> Result<(), String> {
        *count = count
            .checked_add(1)
            .ok_or_else(|| err("graph value overflow"))?;
        if depth > SnapshotReadLimits::MAX_DEPTH || *count > SnapshotReadLimits::MAX_VALUES {
            return Err(err("graph JSON capacity"));
        }
        match value {
            serde_json::Value::Array(a) => {
                for v in a {
                    visit(v, depth + 1, count)?
                }
            }
            serde_json::Value::Object(o) => {
                for v in o.values() {
                    visit(v, depth + 1, count)?
                }
            }
            _ => {}
        };
        Ok(())
    }
    let mut count = 3 + nodes.len() * 3 + edges.len() * 4;
    for props in nodes
        .iter()
        .map(|n| &n.props)
        .chain(edges.iter().map(|e| &e.props))
    {
        visit(props, 4, &mut count)?;
    }
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(b.len())
                .ok_or_else(|| std::io::Error::other("graph byte overflow"))?;
            if self.0 > SnapshotReadLimits::MAX_BYTES {
                return Err(std::io::Error::other("graph byte capacity"));
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), &(nodes, edges)).map_err(err)
}
pub(super) fn observe(
    state: &AppState,
    _graph: &SqliteGraphStore,
    nodes: &[Node],
    edges: &[Edge],
    selectors: &[CorpusSelector],
) -> Result<Observation, String> {
    bound_graph(nodes, edges)?;
    let (hops, candidates) = semantic::graph_inputs(nodes, edges);
    let covered: BTreeSet<_> = selectors.iter().map(|s| s.repo_id.as_str()).collect();
    for ev in hops
        .iter()
        .flat_map(|h| &h.evidence)
        .chain(candidates.iter().flat_map(|c| &c.evidence))
    {
        if !covered.contains(ev.repo.as_str()) {
            return Err(err("semantic corpus repo not covered"));
        }
    }
    let mut observed = Vec::new();
    let mut guards = Vec::new();
    let mut captures = BTreeMap::new();
    let mut documents = BTreeMap::new();
    let mut remaining = 32 * 1024 * 1024;
    for selector in selectors {
        let source = state
            .sources
            .lock()
            .map_err(err)?
            .get_by_repo(&selector.repo_id)?
            .ok_or_else(|| err("unregistered corpus source"))?;
        let guard = state
            .primary_sources
            .task_guard(&source.source_id)
            .map_err(err)?;
        let capture = state
            .primary_sources
            .evaluation_capture(&guard, &selector.capture_id, remaining)
            .map_err(err)?;
        let bytes = capture
            .manifest()
            .files
            .iter()
            .try_fold(0usize, |sum, file| {
                usize::try_from(file.byte_len)
                    .ok()
                    .and_then(|len| sum.checked_add(len))
            })
            .ok_or_else(|| err("capture byte overflow"))?;
        remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| err("capture aggregate capacity"))?;
        let mut selected_nodes = Vec::new();
        let mut selected_edges = Vec::new();
        let relevant = |props: &serde_json::Value| -> Result<bool, String> {
            let ev = evidence(props)?;
            let matching: Vec<_> = ev.iter().filter(|e| e.repo == selector.repo_id).collect();
            if matching.iter().any(|e| e.commit_sha != selector.revision) {
                return Err(err("corpus revision mismatch"));
            }
            Ok(!matching.is_empty())
        };
        for node in nodes {
            if relevant(&node.props)? {
                selected_nodes.push(node.clone());
            }
        }
        for edge in edges {
            if relevant(&edge.props)? {
                selected_edges.push(edge.clone());
            }
        }
        if selected_nodes.is_empty() && selected_edges.is_empty() {
            return Err(err("empty corpus graph"));
        }
        let snapshot =
            context_hub::ContextSnapshot::new(selected_nodes, selected_edges).map_err(err)?;
        // A locked store holds these exact documents as its corpus files, so
        // the runner's corpus digests equal this host's (ADR-0082).
        let graph_document = canonical_json(
            &serde_json::json!({"recipe":"cartograph-t2-host-graph@1","corpus_id":selector.corpus_id,"repo_id":selector.repo_id,"revision":selector.revision,"snapshot_id":snapshot.id()}),
        )
        .map_err(err)?;
        let capture_document = canonical_json(
            &serde_json::json!({"recipe":"cartograph-t2-host-capture@1","corpus_id":selector.corpus_id,"repo_id":selector.repo_id,"revision":selector.revision,"capture_id":capture.id()}),
        )
        .map_err(err)?;
        let graph_snapshot_hash = sha256(&graph_document);
        let capture_manifest_hash = sha256(&capture_document);
        documents.insert(
            selector.corpus_id.clone(),
            (graph_document, capture_document),
        );
        observed.push(ObservedCorpus {
            corpus_id: selector.corpus_id.clone(),
            repo_id: selector.repo_id.clone(),
            revision: selector.revision.clone(),
            graph_snapshot_hash,
            capture_manifest_hash,
        });
        captures.insert(selector.corpus_id.clone(), capture);
        guards.push(guard);
    }
    observed.sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
    Ok(Observation {
        observed,
        guards,
        documents,
        captures,
    })
}
fn evidence_key(e: &core_prov::EvidenceRef) -> (&str, &str, u64, u64, &str) {
    (&e.repo, &e.path, e.byte_start, e.byte_end, &e.commit_sha)
}
fn evidence_matches(
    actual: &[core_prov::EvidenceRef],
    declared: &[semantic::evaluation::fixtures::EvidenceRef],
) -> bool {
    let a: BTreeSet<_> = actual.iter().map(evidence_key).collect();
    let b: BTreeSet<_> = declared
        .iter()
        .map(|e| {
            (
                e.repo.as_str(),
                e.path.as_str(),
                e.byte_start,
                e.byte_end,
                e.commit_sha.as_str(),
            )
        })
        .collect();
    a == b
}
pub(super) fn validate_dataset(
    dataset: &ReviewedDataset,
    observation: &Observation,
    nodes: &[Node],
    edges: &[Edge],
    selectors: &[CorpusSelector],
) -> Result<(), String> {
    for source in dataset.retained_sources() {
        let selector = selectors
            .iter()
            .find(|s| s.corpus_id == source.corpus_id)
            .ok_or_else(|| err("dataset source corpus missing"))?;
        if source.evidence.repo != selector.repo_id
            || source.evidence.commit_sha != selector.revision
        {
            return Err(err("dataset source provenance mismatch"));
        }
        let capture = observation
            .captures
            .get(&source.corpus_id)
            .ok_or_else(|| err("dataset capture missing"))?;
        let file = capture.file(&source.evidence.path).map_err(err)?;
        let span = file
            .span(source.evidence.byte_start, source.evidence.byte_end)
            .map_err(err)?;
        if Some(capture.read_span(&span).map_err(err)?) != dataset.source_bytes(&source.source_id) {
            return Err(err("dataset source differs from actual captured bytes"));
        }
    }
    let (hops, candidates) = semantic::graph_inputs(nodes, edges);
    let calibration = dataset.calibration();
    let evaluation = dataset.evaluation_inputs();
    for case in calibration.cases().iter().chain(evaluation.cases()) {
        validate_case(case, &hops, &candidates)?;
    }
    Ok(())
}
pub(super) fn validate_case(
    case: &semantic::evaluation::fixtures::CaseInput,
    hops: &[UnresolvedHop],
    candidates: &[Candidate],
) -> Result<(), String> {
    let hop = hops
        .iter()
        .find(|hop| hop.gap_id == case.hop.gap_id && hop.source_id == case.hop.source_id)
        .ok_or_else(|| err("dataset hop not in actual graph"))?;
    let label = match case.hop.edge_label {
        semantic::evaluation::fixtures::EdgeLabel::CALLS => "CALLS",
        semantic::evaluation::fixtures::EdgeLabel::PUBLISHES => "PUBLISHES",
        semantic::evaluation::fixtures::EdgeLabel::SUBSCRIBES => "SUBSCRIBES",
    };
    if hop.edge_label != label
        || hop.family() != case.family
        || hop.query != case.hop.query
        || !evidence_matches(&hop.evidence, &case.hop.evidence)
    {
        return Err(err("dataset hop differs from actual graph"));
    }
    let eligible: BTreeMap<_, _> = candidates
        .iter()
        .filter(|c| c.family() == case.family)
        .map(|c| (c.node_id.as_str(), c))
        .collect();
    if eligible.len() != case.candidates.len() {
        return Err(err("dataset candidate universe is incomplete"));
    }
    for declared in &case.candidates {
        let actual = eligible
            .get(declared.node_id.as_str())
            .ok_or_else(|| err("dataset candidate not in actual graph"))?;
        if actual.family() != declared.family
            || actual.text != declared.text
            || !evidence_matches(&actual.evidence, &declared.evidence)
        {
            return Err(err("dataset candidate differs from actual graph"));
        }
    }
    Ok(())
}
