use super::super::tests::Case;
use super::*;
fn measure(
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
) -> Result<Vec<Measurement>, String> {
    measure_bound(
        config,
        preparation,
        proposal,
        output,
        &"a".repeat(40),
        InputPolicy::OfflineFixture,
    )
}
fn selections(prepared: &Preparation, count: usize) -> Vec<RoleSelection> {
    let graph = serde_json::to_value(&prepared.graph).unwrap();
    let sources: Vec<_> = graph["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|candidate| {
            let receipt = &candidate["receipt"]["content"];
            let fact = serde_json::from_value(candidate["binding"]["fact"].clone());
            // Graph and task fact DTOs share the tagged key representation.
            let fact: TaskFactKey = fact.unwrap();
            receipt["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(move |(ordinal, _)| SourceSelection {
                    fact: fact.clone(),
                    graph_snapshot_id: prepared.graph.graph_snapshot_id.clone(),
                    receipt_id: candidate["binding"]["receipt_id"].as_str().unwrap().into(),
                    ordinal,
                })
        })
        .take(count)
        .collect();
    assert_eq!(sources.len(), count);
    ["domain-analyst@7", "evidence-auditor@7"]
        .into_iter()
        .map(|specialist_id| RoleSelection {
            specialist_id: specialist_id.into(),
            question: "What behavior is established by the selected captured source?".into(),
            request_nonce: format!("measurement-{}", specialist_id.replace('@', "-")),
            scope: InvestigationScope::All,
            sources: sources.clone(),
            parent_id: None,
            conversation_id: None,
        })
        .collect()
}
fn files(
    case: &Case,
    prepared: &Preparation,
    roles: &[RoleSelection],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let parent = case.root.parent().unwrap();
    let preparation = parent.join("preparation.json");
    let proposal = parent.join("selection.json");
    std::fs::write(&preparation, bytes(prepared).unwrap()).unwrap();
    std::fs::write(&proposal, bytes(&roles).unwrap()).unwrap();
    (preparation, proposal)
}
// AC-0191: offline fixtures are real ingest/store measurements, not Vendure evidence.
#[test]
fn vendure_measurement_reopens_resolves_and_accounts_without_tasks() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let roles = selections(&prepared, 2);
    let (preparation, proposal) = files(&case, &prepared, &roles);
    let measured = measure(
        &case.config,
        &preparation,
        &proposal,
        &case.root.parent().unwrap().join("measurement-output"),
    )
    .unwrap();
    for receipt in &measured {
        assert_eq!(receipt.source_evidence_items, 2);
        assert_eq!(receipt.minimum_tool_actions, 3);
        assert_eq!(receipt.minimum_model_invocations, 4);
        assert_eq!(
            receipt.source_evidence_bytes,
            receipt
                .ranges
                .iter()
                .map(|range| range.range_bytes)
                .sum::<u64>()
        );
        assert_eq!(
            receipt.repeated_full_file_validation_bytes,
            receipt
                .ranges
                .iter()
                .map(|range| range.full_file_validation_bytes)
                .sum::<u64>()
        );
        for cost in &receipt.ranges {
            let file = std::fs::read(case.root.join(&cost.target.captured.file.path)).unwrap();
            let start = cost.target.captured.byte_start as usize;
            let end = cost.target.captured.byte_end as usize;
            assert_eq!(file[start..end].len() as u64, cost.range_bytes);
            assert_eq!(file.len() as u64, cost.full_file_validation_bytes);
        }
        assert!(!receipt.executable_admission_established);
        assert!(
            receipt.inventory_response_bytes > prepared.graph.coverage.record.inventory_json.len()
        );
    }
    assert_eq!(
        measured[0].ranges[0].target.target_id,
        measured[1].ranges[0].target.target_id
    );
    let app = prepared.profile.app().unwrap();
    let state = app.state::<AppState>();
    for (receipt, role) in measured.iter().zip(&roles) {
        let inventory = crate::investigations::completion_choices::control_inventory(
            app.handle(),
            &state,
            &role.scope,
        )
        .unwrap();
        let Obligation::Inventory {
            target: inventory_target,
            ..
        } = &inventory
        else {
            panic!("expected inventory");
        };
        let detail = InvestigationCoverageDetail {
            schema_version: 1,
            context_id: inventory_target.context_id.clone(),
            repo: inventory_target.repo.clone(),
            inventory_hash: inventory_target.inventory_hash.clone(),
            inventory_json: prepared.graph.coverage.record.inventory_json.clone(),
        };
        assert_eq!(
            receipt.inventory_response_bytes,
            serde_json::to_string(&detail).unwrap().len()
        );
        let context_id = inventory_target.context_id.clone();
        let mut obligations = vec![inventory];
        for selection in &role.sources {
            let (target, _guard) = crate::investigations::inspection::resolve(
                &state,
                &selection.fact,
                &selection.graph_snapshot_id,
                &selection.receipt_id,
                selection.ordinal,
            )
            .unwrap();
            let mut obligation = Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(target),
            };
            let identity = obligation.content_id().unwrap();
            if let Obligation::Source { obligation_id, .. } = &mut obligation {
                *obligation_id = identity;
            }
            obligations.push(obligation);
        }
        let request = StagedObligationRequest {
            schema_version: 4,
            request_nonce: role.request_nonce.clone(),
            specialist_id: role.specialist_id.clone(),
            question: role.question.clone(),
            scope: role.scope.clone(),
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision: Some(prepared.graph.graph_snapshot_id.clone()),
            expected_context_id: context_id,
            conversation_id: role.conversation_id.clone(),
            parent_id: role.parent_id.clone(),
            inspection_requirements: None,
            obligations: envelope(obligations),
        }
        .normalized()
        .unwrap();
        let exact = serde_json::to_string(&request).unwrap();
        assert_eq!(receipt.canonical_request_bytes, exact.len());
        assert_eq!(
            receipt.canonical_measurement_request_sha256,
            hash(exact.as_bytes())
        );
    }
    let jobs = state.jobs.lock().unwrap().list().unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].investigation_id.is_none());
    let encoded = String::from_utf8(bytes(&measured).unwrap()).unwrap();
    assert!(!encoded.contains("out of stock"));
    std::fs::write(&case.config, b"{}").unwrap();
    assert!(
        measure(
            &case.config,
            &preparation,
            &proposal,
            &case.root.parent().unwrap().join("measurement-output")
        )
        .is_err()
    );
}
#[test]
fn vendure_measurement_reports_over_admission_and_rejects_stale_selection() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let distinct = selections(&prepared, 8);
    let (preparation, proposal) = files(&case, &prepared, &distinct);
    let measured = measure(
        &case.config,
        &preparation,
        &proposal,
        &case.root.parent().unwrap().join("measurement-output"),
    )
    .unwrap();
    assert_eq!(measured[0].source_evidence_items, 8);
    assert!(
        measured[0]
            .current_limit_violations
            .contains(&"tool_actions")
    );
    assert!(
        measured[0]
            .current_limit_violations
            .contains(&"model_invocations")
    );
    let mut roles = selections(&prepared, 1);
    for role in &mut roles {
        role.sources = vec![role.sources[0].clone(); 13];
    }
    let (preparation, proposal) = files(&case, &prepared, &roles);
    let measured = measure(
        &case.config,
        &preparation,
        &proposal,
        &case.root.parent().unwrap().join("measurement-output"),
    )
    .unwrap();
    assert_eq!(measured[0].raw_obligation_count, 14);
    assert_eq!(measured[0].canonical_obligation_count, 2);
    assert!(measured[0].production_normalization_error.is_some());
    assert!(
        measured[0]
            .current_limit_violations
            .contains(&"raw_obligations")
    );
    roles[0].sources[0].receipt_id = "0".repeat(64);
    std::fs::write(&proposal, bytes(&roles).unwrap()).unwrap();
    assert!(
        measure(
            &case.config,
            &preparation,
            &proposal,
            &case.root.parent().unwrap().join("measurement-output")
        )
        .is_err()
    );
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&preparation).unwrap()).unwrap();
    saved["graph"]["capture_id"] = Value::String("0".repeat(64));
    std::fs::write(&preparation, bytes(&saved).unwrap()).unwrap();
    assert!(
        measure(
            &case.config,
            &preparation,
            &proposal,
            &case.root.parent().unwrap().join("measurement-output")
        )
        .err()
        .unwrap()
        .contains("drift")
    );
}

#[test]
fn vendure_measurement_rejects_scope_and_missing_store_without_recreation() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let mut roles = selections(&prepared, 8);
    let catalogue = serde_json::to_value(&prepared.graph).unwrap();
    let anchor = catalogue["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|candidate| {
            if candidate["receipt"]["content"]["file"]["path"] != "stock.ts" {
                return None;
            }
            match serde_json::from_value::<TaskFactKey>(candidate["binding"]["fact"].clone())
                .unwrap()
            {
                TaskFactKey::Node { id } => Some(id),
                _ => None,
            }
        })
        .unwrap();
    // The missing store is checked first: a measurement retains its app (and
    // so its open store) for the rest of the process, and Windows cannot
    // rename a file another handle holds open.
    let (preparation, proposal) = files(&case, &prepared, &selections(&prepared, 1));
    let graph = prepared.profile.data().join("graph.db");
    let retained = graph.with_extension("retained");
    std::fs::rename(&graph, &retained).unwrap();
    assert!(
        measure(
            &case.config,
            &preparation,
            &proposal,
            &case.root.parent().unwrap().join("measurement-output")
        )
        .err()
        .unwrap()
        .contains("store missing")
    );
    assert!(!graph.exists());
    std::fs::rename(&retained, &graph).unwrap();
    roles[0].scope = InvestigationScope::Neighborhood { anchor, hops: 1 };
    std::fs::write(&proposal, bytes(&roles).unwrap()).unwrap();
    let error = measure(
        &case.config,
        &preparation,
        &proposal,
        &case.root.parent().unwrap().join("measurement-output"),
    )
    .err()
    .unwrap();
    assert!(error.contains("outside the requested scope"), "{error}");
    std::fs::write(&proposal, bytes(&selections(&prepared, 1)).unwrap()).unwrap();
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&preparation).unwrap()).unwrap();
    saved["graph"]["coverage"]["inventory_hash"] = Value::String("0".repeat(64));
    std::fs::write(&preparation, bytes(&saved).unwrap()).unwrap();
    assert!(
        measure(
            &case.config,
            &preparation,
            &proposal,
            &case.root.parent().unwrap().join("measurement-output")
        )
        .err()
        .unwrap()
        .contains("drift")
    );
}

#[path = "measurement_binding_tests.rs"]
mod binding_tests;
