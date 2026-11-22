//! Synthetic trusted-anchor simulation over actual production graph/capture
//! inputs. These scripted assertions establish mechanism, never model quality.
use super::*;
use llm::{Embedding, EmbeddingIdentity, LlmProvider, Locality, ProviderCaps, ProviderError};
use semantic::evaluation::{
    fixtures::{Corpus, InputsManifest, LabelsManifest, LeakageManifest},
    policy::FamilyPolicy,
    reviewed::ReviewEnvelope,
};
use serde::Serialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Scripted(AtomicUsize);
impl LlmProvider for Scripted {
    fn id(&self) -> &str {
        "host-positive-scripted"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: false,
            tool_use: false,
        }
    }
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        Ok(Some(EmbeddingIdentity {
            endpoint: "scripted://host-positive".into(),
            model: "mechanism-only".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
            recipe: "host-positive-v1".into(),
        }))
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|t| {
                let t = t.to_ascii_lowercase();
                if t.contains("alpha") {
                    vec![1.0, 0.0, 0.0]
                } else if t.contains("beta") {
                    vec![0.0, 1.0, 0.0]
                } else {
                    vec![0.0, 0.0, 1.0]
                }
            })
            .collect())
    }
}
fn write(root: &Path, name: &str, value: &impl Serialize) {
    std::fs::write(root.join(name), canonical_json(value).unwrap()).unwrap();
}
fn digest(value: &impl Serialize) -> String {
    sha256(&canonical_json(value).unwrap())
}
fn params() -> Vec<FamilyPolicy> {
    vec![FamilyPolicy {
        family: semantic::FamilyKey::new(semantic::HopKind::Call, None),
        threshold: 0.9,
        ambiguity_margin: 0.01,
        neighbors: 3,
        min_evaluation_cases: 1,
        min_answerable_cases: 1,
        min_no_match_cases: 1,
        precision_floor: 0.95,
    }]
}
fn key(e: &core_prov::EvidenceRef) -> (String, String, u64, u64, String) {
    (
        e.repo.clone(),
        e.path.clone(),
        e.byte_start,
        e.byte_end,
        e.commit_sha.clone(),
    )
}

/// A reviewed mechanism dataset over an actual captured graph: three hops,
/// one calibration case and two evaluation cases with scripted truth.
pub(super) struct Built {
    pub dir: tempfile::TempDir,
    pub state: AppState,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub selector: CorpusSelector,
    pub observation: source::Observation,
    pub hops: Vec<UnresolvedHop>,
    pub candidates: Vec<Candidate>,
    pub root: PathBuf,
    pub dataset: PathBuf,
    pub inputs: InputsManifest,
    pub leakage: LeakageManifest,
    pub labels: LabelsManifest,
}

pub(super) fn build_dataset() -> Built {
    let text = "import { alpha, beta, noDestination } from './runtime';\nexport function firstCaller() { return alpha(); }\nexport function secondCaller() { return beta(); }\nexport function thirdCaller() { return noDestination(); }\nexport function alphaImplementation() { return 'alpha-target'; }\nexport function betaImplementation() { return 'beta-target'; }\n";
    let (dir, state, nodes, edges, selector) = captured_graph_with_source(text);
    let (hops, candidates) = semantic::graph_inputs(&nodes, &edges);
    assert_eq!(hops.len(), 3);
    let observation = {
        let graph = state.graph.lock().unwrap();
        source::observe(
            &state,
            &graph,
            &nodes,
            &edges,
            std::slice::from_ref(&selector),
        )
        .unwrap()
    };
    let guard = state
        .primary_sources
        .task_guard(
            &state
                .sources
                .lock()
                .unwrap()
                .get_by_repo(&selector.repo_id)
                .unwrap()
                .unwrap()
                .source_id,
        )
        .unwrap();
    let capture = state
        .primary_sources
        .evaluation_capture(&guard, &selector.capture_id, 32 * 1024 * 1024)
        .unwrap();
    let root = dir.path().join("semantic-evaluation");
    let dataset = root.join("dataset");
    std::fs::create_dir_all(dataset.join("sources")).unwrap();
    let mut refs = BTreeMap::new();
    for e in hops
        .iter()
        .flat_map(|h| &h.evidence)
        .chain(candidates.iter().flat_map(|c| &c.evidence))
    {
        refs.entry(key(e)).or_insert_with(|| e.clone());
    }
    let mut source_ids = BTreeMap::new();
    let mut sources = Vec::new();
    for (index, (k, e)) in refs.into_iter().enumerate() {
        let source_id = format!("source-{index:03}");
        let path = format!("sources/{source_id}.txt");
        let file = capture.file(&e.path).unwrap();
        let span = file.span(e.byte_start, e.byte_end).unwrap();
        let bytes = capture.read_span(&span).unwrap();
        std::fs::write(dataset.join(&path), bytes).unwrap();
        source_ids.insert(k, source_id.clone());
        sources.push(json!({"source_id":source_id,"corpus_id":selector.corpus_id,"evidence":e,"bytes_path":path,"byte_len":bytes.len(),"content_hash":sha256(bytes)}));
    }
    let ids_for = |ev: &[core_prov::EvidenceRef]| {
        let mut ids: Vec<_> = ev.iter().map(|e| source_ids[&key(e)].clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let mut catalog:Vec<_>=candidates.iter().map(|c|json!({"node_id":c.node_id,"family":c.family(),"text":c.text,"text_hash":sha256(c.text.as_bytes()),"evidence":c.evidence,"source_ids":ids_for(&c.evidence)})).collect();
    catalog.sort_by_key(|c| c["node_id"].as_str().unwrap().to_owned());
    let mut cases = Vec::new();
    let mut labels = Vec::new();
    let mut groups = Vec::new();
    // Partition fixed before policy/model work. Literal bucket vectors for
    // salt host-positive-v1: alpha86/calibration; beta1754/eval; none7121/eval.
    for (query, case_id, group_id, split, target_text) in [
        (
            "alpha",
            "case-alpha",
            "g-alpha",
            "calibration",
            Some("alphaImplementation"),
        ),
        (
            "beta",
            "case-beta",
            "g-beta",
            "evaluation",
            Some("betaImplementation"),
        ),
        ("noDestination", "case-none", "g-none", "evaluation", None),
    ] {
        let hop = hops.iter().find(|h| h.query == query).unwrap();
        let ids = ids_for(&hop.evidence);
        cases.push(json!({"case_id":case_id,"corpus_id":selector.corpus_id,"family":hop.family(),"hop":{"gap_id":hop.gap_id,"source_id":hop.source_id,"edge_label":hop.edge_label,"query":hop.query,"query_hash":sha256(hop.query.as_bytes()),"evidence":hop.evidence},"source_ids":ids,"candidates":catalog,"candidate_set_hash":digest(&catalog)}));
        let mut supervised = ids.clone();
        let truth = if let Some(name) = target_text {
            let target = candidates.iter().find(|c| c.text == name).unwrap();
            supervised.extend(ids_for(&target.evidence));
            json!({"kind":"acceptable_targets","target_ids":[target.node_id]})
        } else {
            json!({"kind":"no_match"})
        };
        supervised.sort();
        supervised.dedup();
        labels.push(json!({"case_id":case_id,"truth":truth,"hard_negative_target_ids":[],"origin":{"kind":"independent_adjudication","author":"synthetic-label-author","reviewer":"separate-reviewer","procedure_version":"scripted-host-mechanism-v1","source_ids":supervised,"rationale":"Synthetic independently reviewed anchor simulation; not real quality evidence.","exclusivity_basis":"Explicit scripted test truth over the complete production candidate catalogue."}}));
        groups.push(json!({"group_id":group_id,"case_ids":[case_id],"split":split,"related_source_ids":supervised,"grouping_basis":["independent"],"review_note":"Distinct supervised query/target spans; unlabeled shared catalogue is not supervision."}));
    }
    cases.sort_by_key(|c| c["case_id"].as_str().unwrap().to_owned());
    labels.sort_by_key(|c| c["case_id"].as_str().unwrap().to_owned());
    let corpus = Corpus {
        corpus_id: selector.corpus_id.clone(),
        repo_id: selector.repo_id.clone(),
        revision: selector.revision.clone(),
        graph_snapshot_hash: observation.observed[0].graph_snapshot_hash.clone(),
        capture_manifest_hash: observation.observed[0].capture_manifest_hash.clone(),
    };
    let inputs:InputsManifest=serde_json::from_value(json!({"schema_version":1,"kind":"inputs","purpose":"independent_evaluation","dataset_id":"host-positive-mechanism","corpora":[corpus],"sources":sources,"cases":cases})).unwrap();
    let leakage:LeakageManifest=serde_json::from_value(json!({"schema_version":1,"kind":"leakage","purpose":"independent_evaluation","dataset_id":inputs.dataset_id,"inputs_hash":digest(&inputs),"partition_policy":{"version":"catalogue-leakage@1","salt":"host-positive-v1","calibration_buckets":1000},"groups":groups})).unwrap();
    let labels:LabelsManifest=serde_json::from_value(json!({"schema_version":1,"kind":"labels","purpose":"independent_evaluation","dataset_id":inputs.dataset_id,"inputs_hash":digest(&inputs),"leakage_hash":digest(&leakage),"labels":labels})).unwrap();
    write(&dataset, "inputs.json", &inputs);
    write(&dataset, "leakage.json", &leakage);
    write(&dataset, "labels.json", &labels);
    drop(capture);
    drop(guard);
    Built {
        dir,
        state,
        nodes,
        edges,
        selector,
        observation,
        hops,
        candidates,
        root,
        dataset,
        inputs,
        leakage,
        labels,
    }
}

// AC-0271..0278: actual captured-directory extraction, fixed host config,
// reviewed scoring, live admission and durable record/reopen form one path.
// No legacy caller labels or per-fact receipt fabrication is involved.
#[test]
fn host_reviewed_positive_path_records_and_reopens_without_caller_labels() {
    let Built {
        dir,
        state,
        nodes,
        edges,
        selector,
        observation,
        hops,
        candidates,
        root,
        dataset,
        inputs,
        leakage,
        labels,
    } = build_dataset();
    // Bootstrap the observed embedding namespace with an explicit SCRIPTED
    // provider, then freeze the real policy digest into the mock owner anchor.
    let provider = Scripted(AtomicUsize::new(0));
    let cache = Arc::new(Mutex::new(
        semantic::embedding_cache::EmbeddingCache::open(dir.path().join("embedding-test.db"))
            .unwrap(),
    ));
    let lease = cache
        .lock()
        .unwrap()
        .bind_instance(&state.graph.lock().unwrap().instance_id().unwrap())
        .unwrap();
    let cached = CachedProvider::new(&provider, cache.clone(), lease.clone()).unwrap();
    let plan = semantic::embedding_inputs(&hops, &candidates, &[]).unwrap();
    cached.embed(&plan).unwrap();
    cached.seal().unwrap();
    let binding = cached.finish_with_binding().unwrap().1.unwrap();
    let mut envelope = ReviewEnvelope {
        schema_version: 1,
        purpose: "reviewed_independent_dataset".into(),
        dataset_id: inputs.dataset_id.clone(),
        inputs_hash: digest(&inputs),
        leakage_hash: digest(&leakage),
        labels_hash: digest(&labels),
        corpora_hash: digest(&inputs.corpora),
        embedding_namespace: binding.namespace().into(),
        embedding_dimensions: binding.dimensions() as u64,
        policy_digest: "0".repeat(64),
        reviewer: "separate-reviewer".into(),
        policy_implementer: "host-test-implementer".into(),
        reviewed_at: 1,
        policy_committed_at: 2,
        calibration_started_at: 3,
    };
    write(&dataset, "review-envelope.json", &envelope);
    let anchor = ReviewAnchor::new(digest(&envelope), envelope.policy_implementer.clone()).unwrap();
    let reviewed = ReviewedDataset::load(&dataset, &anchor, &observation.observed).unwrap();
    let policy = FrozenPolicy::freeze(&reviewed.calibration(), &binding, params()).unwrap();
    envelope.policy_digest = policy.digest().into();
    write(&dataset, "review-envelope.json", &envelope);
    write(
        &root,
        "review-anchor.json",
        &json!({"schema_version":1,"envelope_sha256":digest(&envelope),"policy_implementer":envelope.policy_implementer,"corpora":[{"corpus_id":selector.corpus_id,"repo_id":selector.repo_id,"revision":selector.revision,"capture_id":selector.capture_id}]}),
    );
    // Policy JSON floats are ordinary closed host configuration, not fixture JCS.
    std::fs::write(root.join("policy.json"),serde_json::to_vec(&json!({"schema_version":1,"families":[{"family":{"kind":"Call","subtype":null},"threshold":0.9,"ambiguity_margin":0.01,"neighbors":3,"min_evaluation_cases":1,"min_answerable_cases":1,"min_no_match_cases":1,"precision_floor":0.95}]})).unwrap()).unwrap();
    let prepared = prepare(dir.path(), &state, &nodes, &edges)
        .unwrap()
        .unwrap();
    let before = provider.0.load(Ordering::SeqCst);
    let preview = crate::build_reviewed_semantic_preview(
        &provider,
        cache.clone(),
        lease.clone(),
        &nodes,
        &edges,
        &[],
        0.95,
        Some(&prepared),
    )
    .unwrap();
    assert!(!preview.calibration_available);
    assert!(!preview.approved.is_empty());
    assert!(preview.gaps_filled > 0);
    assert_eq!(provider.0.load(Ordering::SeqCst), before);
    assert_eq!(
        preview.evaluation.mode,
        semantic::evaluation::report::SummaryMode::IndependentEvaluation
    );
    assert!(preview.evaluation.families.iter().any(|f| f.admitted > 0));
    let expected = preview.evaluation.clone();
    crate::record_reviewed_semantic_preview(&state, &cache, &lease, preview, Some(&prepared))
        .unwrap();
    let reopened = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
    let rows = reopened.list_evals().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].evaluation.as_ref(), Some(&expected));
    let mut updated = std::fs::read(root.join("review-anchor.json")).unwrap();
    updated.push(b' ');
    std::fs::write(root.join("review-anchor.json"), updated).unwrap();
    let graph = state.graph.lock().unwrap();
    assert!(prepared.revalidate(&state, &graph).is_err());
    assert_eq!(provider.0.load(Ordering::SeqCst), before);
}

mod locked;
