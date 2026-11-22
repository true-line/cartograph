//! AC-1512: a locked store on this host holds only the pinned runner key, the
//! runner's signed receipt, the policy and the corpus selectors. The dataset
//! and its labels exist only in the lock. Scripted mechanism, not quality.
use super::*;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn signer() -> Ed25519KeyPair {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap()
}

const POLICY: &str = r#"{"schema_version":1,"families":[{"family":{"kind":"Call","subtype":null},"threshold":0.9,"ambiguity_margin":0.01,"neighbors":3,"min_evaluation_cases":1,"min_answerable_cases":1,"min_no_match_cases":1,"precision_floor":0.95}]}"#;

#[test]
fn host_locked_receipt_path_admits_without_a_host_dataset() {
    let Built {
        dir,
        state,
        nodes,
        edges,
        selector,
        observation,
        root,
        dataset,
        ..
    } = build_dataset();

    // The lock: the dataset moves off this host, beside the host's own
    // corpus documents, and the locked runner scores the policy once.
    let lock = tempfile::tempdir().unwrap();
    let locked_dataset = lock.path().join("dataset");
    std::fs::rename(&dataset, &locked_dataset).unwrap();
    std::fs::create_dir_all(locked_dataset.join("corpora")).unwrap();
    for (id, (graph, capture)) in &observation.documents {
        std::fs::write(
            locked_dataset.join(format!("corpora/{id}.graph.json")),
            graph,
        )
        .unwrap();
        std::fs::write(
            locked_dataset.join(format!("corpora/{id}.capture.json")),
            capture,
        )
        .unwrap();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    std::fs::write(
        lock.path().join("review.json"),
        serde_json::to_vec(&json!({"schema_version":1,"reviewer":"separate-reviewer","policy_implementer":"host-test-implementer","reviewed_at":now - 1000})).unwrap(),
    )
    .unwrap();
    let runner = Scripted(AtomicUsize::new(0));
    let receipt =
        semantic::evaluation::locked::score(lock.path(), POLICY.as_bytes(), &runner).unwrap();
    let key = signer();
    let signed = serde_json::to_vec(&json!({
        "receipt": String::from_utf8(receipt.clone()).unwrap(),
        "signature": hex(key.sign(&receipt).as_ref()),
        "public_key": hex(key.public_key().as_ref()),
    }))
    .unwrap();

    // The host store: no dataset, no labels.
    assert!(!root.join("dataset").exists());
    std::fs::write(
        root.join("runner-key.txt"),
        format!("{}\n", hex(key.public_key().as_ref())),
    )
    .unwrap();
    std::fs::write(root.join("locked-receipt.json"), &signed).unwrap();
    std::fs::write(root.join("policy.json"), POLICY).unwrap();
    std::fs::write(
        root.join("corpora.json"),
        serde_json::to_vec(&json!({"schema_version":1,"corpora":[{"corpus_id":selector.corpus_id,"repo_id":selector.repo_id,"revision":selector.revision,"capture_id":selector.capture_id}]})).unwrap(),
    )
    .unwrap();

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
    let prepared = prepare(dir.path(), &state, &nodes, &edges)
        .unwrap()
        .unwrap();
    assert!(prepared.planned_texts().unwrap().is_empty());
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
    assert!(!preview.approved.is_empty());
    assert!(preview.gaps_filled > 0);
    assert_eq!(
        preview.evaluation.mode,
        semantic::evaluation::report::SummaryMode::IndependentEvaluation
    );
    let receipt_digest =
        serde_json::from_slice::<serde_json::Value>(&receipt).unwrap()["receipt_digest"]
            .as_str()
            .unwrap()
            .to_owned();
    let call = preview
        .evaluation
        .families
        .iter()
        .find(|f| f.admitted > 0)
        .unwrap();
    assert_eq!(
        call.receipt_digest.as_deref(),
        Some(receipt_digest.as_str())
    );
    let expected = preview.evaluation.clone();
    crate::record_reviewed_semantic_preview(&state, &cache, &lease, preview, Some(&prepared))
        .unwrap();
    let reopened = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.list_evals().unwrap()[0].evaluation.as_ref(),
        Some(&expected)
    );

    // A changed policy file invalidates the prepared evaluation.
    std::fs::write(root.join("policy.json"), POLICY.replace("0.9,", "0.8,")).unwrap();
    {
        let graph = state.graph.lock().unwrap();
        assert!(prepared.revalidate(&state, &graph).is_err());
    }
    // With the host's own policy changed, the receipt's digest no longer
    // matches and every family abstains.
    let changed = prepare(dir.path(), &state, &nodes, &edges)
        .unwrap()
        .unwrap();
    let preview = crate::build_reviewed_semantic_preview(
        &provider,
        cache.clone(),
        lease.clone(),
        &nodes,
        &edges,
        &[],
        0.95,
        Some(&changed),
    )
    .unwrap();
    assert!(preview.approved.is_empty());
    assert!(preview.evaluation.families.iter().all(|f| f.admitted == 0
        && f.reason == semantic::evaluation::report::DecisionReason::IncompatibleEvidence));
    std::fs::write(root.join("policy.json"), POLICY).unwrap();

    // Another pinned key, or a malformed one, refuses preparation.
    let other = signer();
    std::fs::write(
        root.join("runner-key.txt"),
        hex(other.public_key().as_ref()),
    )
    .unwrap();
    assert!(prepare(dir.path(), &state, &nodes, &edges).is_err());
    std::fs::write(root.join("runner-key.txt"), "not-a-key\n").unwrap();
    assert!(prepare(dir.path(), &state, &nodes, &edges).is_err());
}
