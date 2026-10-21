use super::*;

// AC-0191: exact clean code identity belongs to the measurement, while an
// earlier reviewed preparation remains explicitly identifiable and measurable.
#[test]
fn vendure_measurement_binds_code_revisions_and_reports_mismatch() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let roles = selections(&prepared, 1);
    let (preparation, proposal) = files(&case, &prepared, &roles);
    let output = case.root.parent().unwrap().join("output");
    for revision in ["a".repeat(40), "b".repeat(40)] {
        let measured = measure_bound(
            &case.config,
            &preparation,
            &proposal,
            &output,
            &revision,
            InputPolicy::OfflineFixture,
        )
        .unwrap();
        for receipt in measured {
            assert_eq!(receipt.code_revision, revision);
            assert_eq!(receipt.preparation_code_revision, "a".repeat(40));
            assert_eq!(
                receipt.code_revision_matches_preparation,
                revision == "a".repeat(40)
            );
        }
    }
    assert!(
        measure_bound(
            &case.config,
            &preparation,
            &proposal,
            &output,
            "unbound-worktree",
            InputPolicy::OfflineFixture
        )
        .err()
        .unwrap()
        .contains("code revision")
    );
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&preparation).unwrap()).unwrap();
    saved["code_revision"] = Value::String("not-a-git-revision".into());
    std::fs::write(&preparation, bytes(&saved).unwrap()).unwrap();
    assert!(
        measure(&case.config, &preparation, &proposal, &output)
            .err()
            .unwrap()
            .contains("code revision")
    );
}

// This tests only the pinned metadata predicate. It is not a Vendure capture,
// synthetic coverage publication, or execution of the manual acceptance path.
#[test]
fn vendure_measurement_shared_frozen_gate_checks_every_pinned_dimension() {
    let case = Case::new();
    let mut input = case.prepare().unwrap().input;
    input.repo = "vendurehq/vendure".into();
    input.commit_sha = COMMIT.into();
    input.manifest_sha256 = MANIFEST_SHA.into();
    input.proof_sha256 = PROOF_SHA.into();
    let mut file = input.files.values().next().unwrap().clone();
    input.files.clear();
    for index in 0..17 {
        file.byte_len = if index == 16 { 270_705 } else { 1 };
        input
            .files
            .insert(format!("fixture-metadata-{index}.ts"), file.clone());
    }
    verify_frozen_input(&input).unwrap();
    for field in ["repo", "commit", "manifest", "proof", "count", "bytes"] {
        let mut changed = input.clone();
        match field {
            "repo" => changed.repo = "example/fixture".into(),
            "commit" => changed.commit_sha = "0".repeat(40),
            "manifest" => changed.manifest_sha256 = "0".repeat(64),
            "proof" => changed.proof_sha256 = "0".repeat(64),
            "count" => {
                changed.files.pop_first();
            }
            "bytes" => changed.files.values_mut().next().unwrap().byte_len += 1,
            _ => unreachable!(),
        }
        assert!(verify_frozen_input(&changed).is_err(), "{field}");
    }
}

// The same fixed wrapper used by the ignored manual entrypoint refuses this
// ordinary fixture even when all live stores/targets and selection are valid.
#[test]
fn vendure_measurement_manual_wrapper_refuses_fixture_and_seals_safe_failure() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let roles = selections(&prepared, 1);
    let (preparation, proposal) = files(&case, &prepared, &roles);
    let output = case.root.parent().unwrap().join("output");
    let error = run_frozen_measurement(
        &case.config,
        &preparation,
        &proposal,
        &output,
        &"b".repeat(40),
    )
    .unwrap_err();
    assert_eq!(error, "frozen Vendure input identity mismatch");
    assert!(!output.join("measurement.json").exists());
    let failed = std::fs::read(output.join("measurement-failed.json")).unwrap();
    let receipt: Value = serde_json::from_slice(&failed).unwrap();
    assert_eq!(receipt["status"], "measurement_failed");
    assert_eq!(receipt["code_revision"], "b".repeat(40));
    assert_eq!(receipt["preparation_code_revision"], "a".repeat(40));
    assert_eq!(receipt["tasks_created"], 0);
    assert_eq!(receipt["provider_calls"], 0);
    assert!(
        !String::from_utf8(failed.clone())
            .unwrap()
            .contains("out of stock")
    );
    assert!(
        run_frozen_measurement(
            &case.config,
            &preparation,
            &proposal,
            &output,
            &"b".repeat(40)
        )
        .unwrap_err()
        .contains("output must be new")
    );
    assert_eq!(
        failed,
        std::fs::read(output.join("measurement-failed.json")).unwrap()
    );
    let mut invalid: Value = serde_json::from_slice(&std::fs::read(&preparation).unwrap()).unwrap();
    invalid["code_revision"] =
        Value::String("untrusted source-like text must not enter receipts".into());
    std::fs::write(&preparation, bytes(&invalid).unwrap()).unwrap();
    let invalid_output = output.with_extension("invalid-code");
    assert!(
        run_frozen_measurement(
            &case.config,
            &preparation,
            &proposal,
            &invalid_output,
            &"b".repeat(40)
        )
        .unwrap_err()
        .contains("code revision")
    );
    let invalid_receipt = std::fs::read(invalid_output.join("measurement-failed.json")).unwrap();
    assert!(
        !String::from_utf8(invalid_receipt.clone())
            .unwrap()
            .contains("source-like text")
    );
    let invalid_receipt: Value = serde_json::from_slice(&invalid_receipt).unwrap();
    assert!(invalid_receipt["preparation_code_revision"].is_null());
    let unsafe_output = case.root.join("must-not-create");
    assert!(
        run_frozen_measurement(
            &case.config,
            &preparation,
            &proposal,
            &unsafe_output,
            &"b".repeat(40)
        )
        .is_err()
    );
    assert!(!unsafe_output.exists());
    let app = prepared.profile.app().unwrap();
    let state = app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap().list().unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].investigation_id.is_none());
}
