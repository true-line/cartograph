// AC-1508: the locked runner commits one policy per dataset, scores it on the
// held-out labels and returns aggregate scores and digests only.
use super::*;
use crate::evaluation::reviewed::tests::{Scripted, independent_files};
use std::sync::atomic::{AtomicUsize, Ordering};

fn provider() -> Scripted {
    Scripted {
        calls: AtomicUsize::new(0),
        version: AtomicUsize::new(1),
    }
}

fn millis() -> u64 {
    now().unwrap()
}

/// A store holding the converted mechanism dataset and an in-lock review record.
fn make_store(reviewer: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let dataset = independent_files(false);
    let store = tempfile::tempdir().unwrap();
    let copy = std::process::Command::new("cp")
        .arg("-R")
        .arg(dataset.path())
        .arg(store.path().join("dataset"))
        .status()
        .unwrap();
    assert!(copy.success());
    let review = ReviewRecord {
        schema_version: 1,
        reviewer: reviewer.into(),
        policy_implementer: "policy-implementer".into(),
        reviewed_at: millis() - 1000,
    };
    std::fs::write(
        store.path().join("review.json"),
        serde_json::to_vec(&review).unwrap(),
    )
    .unwrap();
    (store, dataset)
}

fn request(dataset: &Path, threshold: f32) -> Vec<u8> {
    let bundle = fixtures::load_independent_dataset(dataset).unwrap();
    let families: BTreeSet<_> = bundle
        .inputs()
        .cases
        .iter()
        .map(|c| c.family.clone())
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "families": families.iter().map(|family| serde_json::json!({
            "family": family, "threshold": threshold, "ambiguity_margin": 0.01,
            "neighbors": 3, "min_evaluation_cases": 1, "min_answerable_cases": 1,
            "min_no_match_cases": 1, "precision_floor": 0.95,
        })).collect::<Vec<_>>(),
    }))
    .unwrap()
}

fn log_lines(store: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(store.join("submissions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn locked_runner_scores_once_and_returns_aggregates_only() {
    let (store, dataset) = make_store("separate-reviewer");
    let provider = provider();
    let bytes = score(store.path(), &request(dataset.path(), 0.9), &provider).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(receipt["purpose"], RECEIPT_PURPOSE);
    assert_eq!(receipt["submission"], 1);
    let call = receipt["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["family"] == serde_json::json!({"kind": "Call", "subtype": null}))
        .unwrap();
    assert_eq!(call["passed"], true);
    assert!(call["metrics"]["precision"].as_f64().unwrap() >= 0.95);

    // Nothing case-level leaves: no case id, query, candidate text or label.
    let text = String::from_utf8(bytes.clone()).unwrap();
    let bundle = fixtures::load_independent_dataset(&store.path().join("dataset")).unwrap();
    for case in &bundle.inputs().cases {
        assert!(!text.contains(&case.case_id), "{}", case.case_id);
        assert!(!text.contains(&case.hop.query), "{}", case.hop.query);
        for candidate in &case.candidates {
            assert!(!text.contains(&candidate.node_id), "{}", candidate.node_id);
        }
    }
    for label in &bundle.labels().labels {
        assert!(!text.contains(&label.origin.author));
    }

    // The same policy replays the stored receipt without embedding again.
    let calls = provider.calls.load(Ordering::SeqCst);
    let again = score(store.path(), &request(dataset.path(), 0.9), &provider).unwrap();
    assert_eq!(again, bytes);
    assert_eq!(provider.calls.load(Ordering::SeqCst), calls);

    // A different policy for the same dataset is refused.
    assert_eq!(
        score(store.path(), &request(dataset.path(), 0.8), &provider),
        Err(Code::PolicyAlreadyCommitted)
    );
    let log = log_lines(store.path());
    assert_eq!(
        log.iter()
            .map(|l| l["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["scored", "scored", "policy_already_committed"]
    );
    assert_eq!(log[0]["policy_digest"], receipt["policy_digest"]);
}

#[test]
fn locked_runner_commits_one_of_two_concurrent_policies() {
    let (store, dataset) = make_store("separate-reviewer");
    let provider = provider();
    let (first, second) = std::thread::scope(|s| {
        let a = s.spawn(|| score(store.path(), &request(dataset.path(), 0.9), &provider));
        let b = s.spawn(|| score(store.path(), &request(dataset.path(), 0.8), &provider));
        (a.join().unwrap(), b.join().unwrap())
    });
    let outcomes = [first.is_ok(), second.is_ok()];
    assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1);
    let refused = if first.is_ok() { second } else { first };
    assert_eq!(refused, Err(Code::PolicyAlreadyCommitted));
    assert_eq!(log_lines(store.path()).len(), 2);
}

#[test]
fn locked_runner_refuses_malformed_requests_with_fixed_codes() {
    let (store, dataset) = make_store("separate-reviewer");
    let provider = provider();
    let good: serde_json::Value = serde_json::from_slice(&request(dataset.path(), 0.9)).unwrap();
    let mut unknown = good.clone();
    unknown["labels"] = serde_json::json!([]);
    let mut version = good.clone();
    version["schema_version"] = serde_json::json!(2);
    let mut duplicate = good.clone();
    let first = duplicate["families"][0].clone();
    duplicate["families"].as_array_mut().unwrap().push(first);
    let mut threshold = good.clone();
    threshold["families"][0]["threshold"] = serde_json::json!(1.5);
    for bad in [unknown, version, duplicate, threshold] {
        assert_eq!(
            score(store.path(), &serde_json::to_vec(&bad).unwrap(), &provider),
            Err(Code::InvalidRequest)
        );
    }
    assert_eq!(
        score(store.path(), &vec![b' '; MAX_REQUEST_BYTES + 1], &provider),
        Err(Code::InvalidRequest)
    );
    // Nothing was embedded or committed.
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(!store.path().join("commitment.json").exists());
    assert_eq!(log_lines(store.path()).len(), 5);
}

#[test]
fn locked_runner_needs_a_review_record_and_a_distinct_reviewer() {
    let (store, dataset) = make_store("separate-reviewer");
    std::fs::remove_file(store.path().join("review.json")).unwrap();
    assert_eq!(
        score(store.path(), &request(dataset.path(), 0.9), &provider()),
        Err(Code::InvalidStore)
    );

    // A reviewer who also authored labels fails the ADR-0038 load checks.
    let bundle = fixtures::load_independent_dataset(dataset.path()).unwrap();
    let author = bundle.labels().labels[0].origin.author.clone();
    let (store, dataset) = make_store(&author);
    assert_eq!(
        score(store.path(), &request(dataset.path(), 0.9), &provider()),
        Err(Code::InvalidDataset)
    );
}
