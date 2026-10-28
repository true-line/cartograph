use super::super::expanded::vendure_prepare::{Definition, Selection, prepare_bound};
use super::tests::Case;
use super::*;
use agents::investigation::*;
use sha2::{Digest, Sha256};
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
// AC-0287: synthetic offline ingest exercises the captured-profile bridge;
// it is not real Vendure/provider/native acceptance evidence.
#[test]
fn expanded_captured_profile_freezes_reopens_and_recovers_lost_ack_without_execution() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let base = case.root.parent().unwrap();
    let receipt = base.join("preparation.json");
    let definition_path = base.join("analyst-definition.json");
    let output = base.join("analyst-output");
    let bytes = serde_json::to_vec(&prepared).unwrap();
    std::fs::write(&receipt, &bytes).unwrap();
    let graph = serde_json::to_value(&prepared.graph).unwrap();
    let candidate = graph["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            !c["receipt"]["content"]["ranges"]
                .as_array()
                .unwrap()
                .is_empty()
        })
        .unwrap();
    let expected = Definition {
        binding: super::super::completion::prepare::PreparationBinding {
            run_id: "captured-test".into(),
            protocol_hash: "a".repeat(64),
            source_commit: "b".repeat(40),
            source_tree: "c".repeat(40),
        },
        preparation_sha256: sha(&bytes),
        provider: InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: "local:unverified-test".into(),
            model: "unverified-test".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            deployment: None,
            protocol_version: None,
            available: true,
            unavailable_reason: None,
        },
        selections: vec![Selection {
            fact: serde_json::from_value(candidate["binding"]["fact"].clone()).unwrap(),
            graph_snapshot_id: prepared.graph.graph_snapshot_id.clone(),
            receipt_id: candidate["binding"]["receipt_id"].as_str().unwrap().into(),
            ordinal: 0,
        }],
        question: "What does this selected source establish?".into(),
        request_nonce: "captured-test-analyst".into(),
        scope: InvestigationScope::All,
    };
    std::fs::write(&definition_path, serde_json::to_vec(&expected).unwrap()).unwrap();
    let run = |definition: &Definition| {
        prepare_bound(
            &case.config,
            &receipt,
            &definition_path,
            &output,
            definition,
            InputPolicy::OfflineFixture,
        )
    };
    let manifest = run(&expected).unwrap();
    let wire = std::fs::read(output.join("analyst-request.json")).unwrap();
    let request: StagedObligationRequest = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        (
            request.schema_version,
            request.specialist_id.as_str(),
            request.limit_profile.as_str()
        ),
        (7, "domain-analyst@10", "investigation-v2")
    );
    assert!(request.obligations.obligations.iter().any(|item| matches!(item,
        Obligation::Inventory { target, .. } if target.inventory_hash == prepared.graph.coverage.inventory_hash)));
    assert_eq!(request.question, expected.question);
    assert_eq!(request.request_nonce, expected.request_nonce);
    assert_eq!(manifest["capture_id"], prepared.graph.capture_id);
    assert_eq!(manifest["source_id"], prepared.graph.source_id);
    assert_eq!(manifest["provider_calls"], 0);
    assert_eq!(run(&expected).unwrap(), manifest);
    std::fs::remove_file(output.join("captured-analyst.json")).unwrap(); // committed start, lost final ack
    assert_eq!(run(&expected).unwrap(), manifest);
    assert_eq!(
        std::fs::read(output.join("analyst-request.json")).unwrap(),
        wire
    );
    for field in [
        "question",
        "selection",
        "provider",
        "digest",
        "nonce",
        "binding",
    ] {
        let mut changed = expected.clone();
        match field {
            "question" => changed.question.push('?'),
            "selection" => changed.selections[0].ordinal += 1,
            "provider" => changed.provider.model.push('x'),
            "digest" => changed.preparation_sha256 = "0".repeat(64),
            "binding" => changed.binding.protocol_hash = "malformed".into(),
            _ => changed.request_nonce.push('x'),
        }
        std::fs::write(&definition_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(run(&changed).is_err(), "{field}");
    }
    std::fs::write(&definition_path, serde_json::to_vec(&expected).unwrap()).unwrap();
    std::fs::write(output.join("analyst-request.json"), b"{}").unwrap();
    assert!(run(&expected).is_err());
    std::fs::write(output.join("analyst-request.json"), wire).unwrap();
    for field in [
        "source_id",
        "capture_id",
        "graph_snapshot_id",
        "source_root",
    ] {
        let mut changed = serde_json::to_value(&prepared).unwrap();
        if field == "source_root" {
            changed[field] = "/foreign".into();
        } else {
            changed["graph"][field] = "foreign".into();
        }
        std::fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(run(&expected).is_err(), "{field}");
    }
    std::fs::write(&receipt, bytes).unwrap();
    let second_output = base.join("preconsumed-output");
    for _ in 0..2 {
        assert!(
            prepare_bound(
                &case.config,
                &receipt,
                &definition_path,
                &second_output,
                &expected,
                InputPolicy::OfflineFixture
            )
            .is_err()
        );
    }
    std::fs::remove_file(output.join("start.json")).unwrap();
    assert!(run(&expected).is_err()); // committed task without sealed start is never adopted

    let app = prepared.profile.app().unwrap();
    let state = app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let task = jobs
        .investigation(manifest["analyst_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(task.summary.status, InvestigationStatus::Queued);
    assert_eq!(task.usage.model_invocations, 0);
    assert_eq!(
        jobs.list()
            .unwrap()
            .iter()
            .filter(|job| job.investigation_id.is_some())
            .count(),
        1
    );
    let db = rusqlite::Connection::open(prepared.profile.data().join("state.db")).unwrap();
    for table in ["investigation_invocations", "completion_attempts"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    std::fs::write(&case.config, b"{}").unwrap();
    assert!(run(&expected).is_err());
}
