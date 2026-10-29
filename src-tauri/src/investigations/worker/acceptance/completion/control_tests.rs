use super::*;
use records::{CheckStatus, ControlClassification, MechanicalCheck, RunBinding};
use serde_json::json;

fn setup() -> (tempfile::TempDir, PathBuf, FrozenDefinition, String) {
    let directory = tempfile::tempdir().unwrap();
    let run_dir = directory.path().join("run");
    std::fs::create_dir(&run_dir).unwrap();
    let runtime_path = directory.path().join("runtime.json");
    let rubric_path = directory.path().join("rubric.txt");
    std::fs::write(&runtime_path, serde_json::to_vec(&json!({"schema_version":1,
        "endpoint":"http://127.0.0.1:11434", "model":"scripted-model", "model_digest":"a".repeat(64), "runtime_version":"offline-scripted",
        "temperature":null,"seed":null,"context_length":null,"verified_read_only":true})).unwrap()).unwrap();
    std::fs::write(
        &rubric_path,
        "Independent initializer-level scope rubric, outside the capture.",
    )
    .unwrap();
    let provider = super::super::super::super::Scripted::new(
        super::super::super::super::Mode::CompletionSupported,
    );
    let descriptor =
        crate::investigations::runtime::descriptor(&provider, InvestigationProviderMode::Local)
            .unwrap();
    let prepared = prepare::prepare(
        &run_dir,
        &descriptor,
        prepare::PreparationBinding {
            run_id: "frozen-control-test".into(),
            protocol_hash: PROTOCOL_SHA256.into(),
            source_commit: "b".repeat(40),
            source_tree: "c".repeat(40),
        },
    )
    .unwrap();
    let runtime = runtime_identity(&read(&runtime_path).unwrap()).unwrap();
    let definition = freeze(
        &run_dir,
        &prepared,
        external(&runtime_path, &run_dir).unwrap(),
        external(&rubric_path, &run_dir).unwrap(),
        runtime,
    )
    .unwrap();
    let digest = sha(&read(&run_dir.join("frozen-definition.json")).unwrap());
    use std::sync::atomic::Ordering;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    (directory, run_dir, definition, digest)
}
fn receipt(definition: &FrozenDefinition) -> RoleReceipt {
    RoleReceipt {
        schema_version: 1,
        role: Role::Analyst,
        samples_per_role: 1,
        automatic_retry: false,
        binding: RunBinding {
            run_id: definition.run_id.clone(),
            protocol_hash: definition.protocol_sha256.clone(),
            source_commit: definition.source_commit.clone(),
            source_tree: definition.source_tree.clone(),
            context_id: definition.capture_manifest.context_id.clone(),
            request_hash: definition.capture_manifest.analyst_request_hash.clone(),
            obligations_hash: definition.analyst_obligations_hash.clone(),
        },
        investigation_id: definition.capture_manifest.analyst_id.clone(),
        terminal_status: InvestigationStatus::Completed,
        stop_cause: None,
        classification: ControlClassification::SupportedFirstShot,
        durable_refusal_count: 0,
        refusals_before_finish: Some(0),
        detail_hash: "a".repeat(64),
        result_hash: Some("b".repeat(64)),
        events_hash: "c".repeat(64),
        usage: InvestigationUsage::default(),
        mechanical_checks: vec![MechanicalCheck {
            name: "actual_disclosure".into(),
            status: CheckStatus::Passed,
        }],
    }
}
fn attestation(definition: &FrozenDefinition, receipt_sha: &str) -> serde_json::Value {
    let binding = json!({"schema":"cartograph-v7-semantic-review-binding@1", "protocol_sha256":definition.protocol_sha256,
        "run_id":definition.run_id,"role":"analyst","receipt_sha256":receipt_sha,"rubric_sha256":definition.rubric.content.sha256});
    let mut review = binding.clone();
    review["schema"] = json!("cartograph-v7-semantic-review@1");
    review["verdict"] = json!("pass");
    review["reasons"] = json!(["Evidence supports the frozen rubric."]);
    json!({"schema":"cartograph-v7-semantic-review-attestation@1","binding":binding,"review":review,
        "comment":{"database_id":123,"url":"https://github.com/true-line/cartograph/issues/652#issuecomment-123","created_at":"2026-10-20T16:30:00Z","body_sha256":"d".repeat(64)}})
}
// AC-0267: capture and frozen definition preflights use real SHA256, not the
// separately named BLAKE3 ReceiptRef, and no provider is invoked during preparation.
#[test]
fn completion_control_freezes_definition_and_detects_artifact_or_digest_tampering() {
    assert_eq!(sha(PROTOCOL), PROTOCOL_SHA256);
    let (_directory, run_dir, definition, digest) = setup();
    assert!(check_content(&run_dir, &digest).is_ok());
    assert!(!definition.delivery_pass);
    assert!(check_content(&run_dir, &"f".repeat(64)).is_err());
    std::fs::write(&definition.rubric.path, "changed oracle").unwrap();
    assert!(check_content(&run_dir, &digest).is_err());
    assert!(
        freeze(
            &run_dir,
            &prepare::reopen(&run_dir).unwrap(),
            definition.runtime_receipt.clone(),
            definition.rubric.clone(),
            definition.runtime
        )
        .is_err()
    );
}
// AC-0267: malformed or failed review/parent checks cannot authorize the auditor;
// local JSON is tested only for content bindings and never identity authenticity.
#[test]
fn completion_control_semantic_gate_rejects_foreign_binding_failures_and_unknown_mechanics() {
    let (_directory, _run_dir, definition, _digest) = setup();
    let receipt = receipt(&definition);
    let hash = "e".repeat(64);
    let good = attestation(&definition, &hash);
    validate_gate(&definition, &receipt, &hash, &json(&good).unwrap()).unwrap();
    for field in [
        "protocol_sha256",
        "run_id",
        "role",
        "receipt_sha256",
        "rubric_sha256",
    ] {
        let mut changed = good.clone();
        changed["binding"][field] = json!("foreign");
        assert!(validate_gate(&definition, &receipt, &hash, &json(&changed).unwrap()).is_err());
    }
    for verdict in ["fail", "unreviewable"] {
        let mut changed = good.clone();
        changed["review"]["verdict"] = json!(verdict);
        assert!(validate_gate(&definition, &receipt, &hash, &json(&changed).unwrap()).is_err());
    }
    for status in [CheckStatus::Failed, CheckStatus::Unknown] {
        let mut changed = receipt.clone();
        changed.mechanical_checks[0].status = status;
        assert!(validate_gate(&definition, &changed, &hash, &json(&good).unwrap()).is_err());
    }
    let mut changed = good;
    changed["owner_approved"] = json!(true);
    assert!(validate_gate(&definition, &receipt, &hash, &json(&changed).unwrap()).is_err());
}
// AC-0267: an attempted stage is irrevocable even when execution/sealing errors;
// a fresh role remains a separate explicit action, never an automatic retry.
#[test]
fn completion_control_attempt_marker_is_exclusive_after_error_and_role_separate() {
    let directory = tempfile::tempdir().unwrap();
    marker(directory.path(), Role::Analyst, &"a".repeat(64)).unwrap();
    let simulated_failure: Result<(), String> = Err("dispatch or postflight failed".into());
    assert!(simulated_failure.is_err());
    assert!(marker(directory.path(), Role::Analyst, &"a".repeat(64)).is_err());
    marker(directory.path(), Role::Auditor, &"a".repeat(64)).unwrap();
    assert_eq!(
        read(&directory.path().join("analyst.attempt")).unwrap(),
        "a".repeat(64).as_bytes()
    );
}
// AC-0267: semantic material cannot be frozen inside any capture/run root, and
// missing runtime evidence is not replaced by imagined sampling defaults.
#[test]
fn completion_control_external_material_and_runtime_shape_fail_closed() {
    let (_directory, run_dir, definition, _digest) = setup();
    let inside = run_dir.join("oracle.txt");
    std::fs::write(&inside, "must remain out").unwrap();
    assert!(external(&inside, &run_dir).is_err());
    let mut runtime: serde_json::Value =
        serde_json::from_slice(&read(&definition.runtime_receipt.path).unwrap()).unwrap();
    for field in ["temperature", "seed", "context_length"] {
        let mut missing = runtime.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            runtime_identity(&json(&missing).unwrap()).is_err(),
            "{field} must be explicitly null or a recorded value"
        );
    }
    runtime["verified_read_only"] = json!(false);
    assert!(runtime_identity(&json(&runtime).unwrap()).is_err());
    runtime["verified_read_only"] = json!(true);
    runtime["extra_model_fallback"] = json!(true);
    assert!(runtime_identity(&json(&runtime).unwrap()).is_err());
}

// AC-0267: the outer seal and actual parent gate consume one real scripted
// production worker observation; tampering never starts another invocation.
#[test]
fn completion_control_actual_scripted_analyst_seal_and_parent_gate_bind_all_evidence() {
    use std::sync::atomic::Ordering;
    let (_directory, run_dir, definition, digest) = setup();
    let (_, prepared) = check_content(&run_dir, &digest).unwrap();
    let provider = Arc::new(super::super::super::super::Scripted::new(
        super::super::super::super::Mode::CompletionSupported,
    ));
    marker(&run_dir, Role::Analyst, &digest).unwrap();
    let evidence = execution::run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
    let calls = provider.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 4);
    let sealed = seal(&run_dir, Role::Analyst, &digest, &evidence).unwrap();
    assert!(
        !receipt_path(&run_dir, Role::Analyst).exists(),
        "No outer receipt before postflight passes"
    );
    let (_, fresh) = check_content(&run_dir, &digest).unwrap();
    write_new(
        &receipt_path(&run_dir, Role::Analyst),
        &json(&sealed).unwrap(),
    )
    .unwrap();
    let parent_hash = sha(&read(&receipt_path(&run_dir, Role::Analyst)).unwrap());
    let review = json(&attestation(&definition, &parent_hash)).unwrap();
    parent_gate(&run_dir, &digest, &definition, &fresh, &review).unwrap();
    assert!(marker(&run_dir, Role::Analyst, &digest).is_err());
    let evidence_path = run_dir.join("analyst-evidence.json");
    let mut bytes = read(&evidence_path).unwrap();
    bytes.push(b' ');
    std::fs::write(&evidence_path, bytes).unwrap();
    assert!(parent_gate(&run_dir, &digest, &definition, &fresh, &review).is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
}

// AC-0267: a genuine scripted worker can finish dispatch then fail record
// validation. Preserve its immutable hashes and defect record, never a receipt.
#[test]
fn completion_control_post_dispatch_recording_failure_preserves_run_and_observations() {
    for extraction_failure in [true, false] {
        let (_directory, run_dir, _definition, digest) = setup();
        let (_, prepared) = check_content(&run_dir, &digest).unwrap();
        let scripted = Arc::new(super::super::super::super::Scripted::new(
            super::super::super::super::Mode::CompletionSupported,
        ));
        let provider: Arc<dyn LlmProvider> = if extraction_failure {
            execution::recording_failure_provider(&prepared)
        } else {
            scripted.clone()
        };
        marker(&run_dir, Role::Analyst, &digest).unwrap();
        let error = execute_stage(&run_dir, &prepared, Role::Analyst, provider)
            .and_then(|evidence| {
                std::fs::create_dir(run_dir.join("analyst-evidence.json")).unwrap();
                seal_stage(&run_dir, Role::Analyst, &digest, &evidence).map(|_| evidence)
            })
            .unwrap_err();
        if extraction_failure {
            assert_eq!(error, "Invalid acceptance record");
        }
        let invalid: serde_json::Value =
            serde_json::from_slice(&read(&run_dir.join("analyst-invalid-run.json")).unwrap())
                .unwrap();
        assert_eq!(invalid["invalid_run"], true);
        assert_eq!(invalid["stage"], "post_dispatch_recording");
        assert_eq!(invalid["delivery_pass"], false);
        assert_eq!(invalid["observations"].as_array().unwrap().len(), 4);
        let observations: serde_json::Value = serde_json::from_slice(
            &read(&run_dir.join("analyst-recording-observations.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(observations, invalid["observations"]);
        assert!(run_dir.join("analyst.attempt").exists());
        assert!(marker(&run_dir, Role::Analyst, &digest).is_err());
        assert!(!receipt_path(&run_dir, Role::Analyst).exists());
        assert!(!run_dir.join("analyst-runtime-receipt.json").exists());
        if !extraction_failure {
            assert_eq!(scripted.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        }
        assert!(write_new(&run_dir.join("analyst-invalid-run.json"), b"replacement").is_err());
    }
}

// AC-0464: content binding is issue-specific; local bytes are not authentication.
#[test]
fn completion_review_issue_binding_preserves_legacy_gate() {
    let (_directory, _run_dir, definition, _digest) = setup();
    let receipt = receipt(&definition);
    let hash = "e".repeat(64);
    let mut review = attestation(&definition, &hash);
    let expected = [
        definition.protocol_sha256.as_str(),
        definition.run_id.as_str(),
        hash.as_str(),
        definition.rubric.content.sha256.as_str(),
    ];
    validate_gate(&definition, &receipt, &hash, &json(&review).unwrap()).unwrap();
    review["comment"]["url"] =
        json!("https://github.com/true-line/cartograph/issues/598#issuecomment-123");
    validate_review_attestation(expected, 598, &json(&review).unwrap()).unwrap();
    assert!(validate_review_attestation(expected, 652, &json(&review).unwrap()).is_err());
    assert!(validate_gate(&definition, &receipt, &hash, &json(&review).unwrap()).is_err());
    let mut changed = review.clone();
    changed["binding"]["receipt_sha256"] = json!("f".repeat(64));
    assert!(validate_review_attestation(expected, 598, &json(&changed).unwrap()).is_err());
    for verdict in ["fail", "unreviewable"] {
        review["review"]["verdict"] = json!(verdict);
        assert!(validate_review_attestation(expected, 598, &json(&review).unwrap()).is_err());
    }
}
