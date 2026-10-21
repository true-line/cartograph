//! Exact proposal costs; aggregate production ceilings remain unchanged.
use super::*;

pub(super) fn measure_role<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    graph: &report::GraphPreparation,
    role: RoleSelection,
    preparation_hash: &str,
    selection_hash: &str,
) -> Result<Measurement, String> {
    use crate::investigations::{completion_choices, inspection};
    let inventory = completion_choices::control_inventory(app, state, &role.scope)?;
    let Obligation::Inventory {
        target: inventory_target,
        ..
    } = &inventory
    else {
        return Err("inventory required".into());
    };
    let detail = InvestigationCoverageDetail {
        schema_version: 1,
        context_id: inventory_target.context_id.clone(),
        repo: inventory_target.repo.clone(),
        inventory_hash: inventory_target.inventory_hash.clone(),
        inventory_json: graph.coverage.record.inventory_json.clone(),
    };
    let inventory_bytes = bytes(&detail)?.len();
    let mut obligations = vec![inventory.clone()];
    let mut guards = Vec::new();
    for selection in role.sources {
        let (target, guard) = inspection::resolve(
            state,
            &selection.fact,
            &selection.graph_snapshot_id,
            &selection.receipt_id,
            selection.ordinal,
        )?;
        if target.registered_source_id != graph.source_id
            || target.captured.file.capture_id != graph.capture_id
        {
            return Err("selection outside prepared capture".into());
        }
        let mut obligation = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        };
        let identity = obligation.content_id().map_err(|error| error.to_string())?;
        if let Obligation::Source { obligation_id, .. } = &mut obligation {
            *obligation_id = identity;
        }
        obligations.push(obligation);
        guards.push(guard);
    }
    let request = StagedObligationRequest {
        schema_version: 4,
        request_nonce: role.request_nonce,
        specialist_id: role.specialist_id.clone(),
        obligations: envelope(obligations),
        inspection_requirements: None,
        question: role.question,
        scope: role.scope,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: Some(graph.graph_snapshot_id.clone()),
        expected_context_id: inventory_target.context_id.clone(),
        conversation_id: role.conversation_id,
        parent_id: role.parent_id,
    };
    // Validate each target and immutable request field through production
    // normalization without applying the full proposal's aggregate ceilings.
    // All source snapshots are resolver-issued against this prepared graph;
    // the sole inventory is production-issued. Enforce the remaining exact
    // read-key conflict rule before measurement-only ordering and deduplication.
    let mut source_keys = BTreeMap::new();
    for item in &request.obligations.obligations {
        let mut single = request.clone();
        single.obligations = envelope(vec![item.clone()]);
        single.normalized().map_err(|error| error.to_string())?;
        if let Obligation::Source { target, .. } = item {
            let key = bytes(&(&target.fact, target.role, target.index))?;
            if source_keys
                .insert(key, &target.target_id)
                .is_some_and(|old| old != &target.target_id)
            {
                return Err("conflicting source read keys".into());
            }
        }
    }
    completion_choices::validate_request(app, state, &request)?;
    let raw_count = request.obligations.obligations.len();
    let raw_bytes = bytes(&request)?.len();
    let raw_envelope_bytes = bytes(&request.obligations)?.len();
    let normalization = request.normalized();
    let normalization_error = normalization.as_ref().err().map(ToString::to_string);
    let mut unique = BTreeMap::new();
    for item in &request.obligations.obligations {
        if unique
            .insert(item.key(), item.clone())
            .is_some_and(|old| old != *item)
        {
            return Err("conflicting obligation identities".into());
        }
    }
    let canonical = StagedObligationRequest {
        obligations: envelope(unique.into_values().collect()),
        ..request
    };
    let canonical_bytes = bytes(&canonical)?;
    if let Ok(admitted) = normalization
        && bytes(&admitted)? != canonical_bytes
    {
        return Err("production normalization mismatch".into());
    }
    let ranges: Vec<_> = canonical
        .obligations
        .obligations
        .iter()
        .filter_map(|item| match item {
            Obligation::Source { target, .. } => Some(RangeCost {
                target: *target.clone(),
                range_bytes: target.captured.byte_end - target.captured.byte_start,
                full_file_validation_bytes: target.captured.file.byte_len,
            }),
            _ => None,
        })
        .collect();
    let source_bytes = ranges.iter().map(|range| range.range_bytes).sum::<u64>();
    let validation_bytes = ranges
        .iter()
        .map(|range| range.full_file_validation_bytes)
        .sum::<u64>();
    let tools = ranges.len() + 1; // each role acquires its own inventory
    let models = tools + 1; // finish has its own response, no tool action
    let limits = InvestigationLimits::default();
    let mut violations = Vec::new();
    for (exceeded, name) in [
        (raw_count > 12, "raw_obligations"),
        (raw_bytes > 8192, "request_bytes"),
        (raw_envelope_bytes > 8192, "raw_envelope_bytes"),
        (
            ranges.iter().any(|range| range.range_bytes > 8192),
            "individual_source_bytes",
        ),
        (ranges.len() > limits.evidence_items, "evidence_items"),
        (
            source_bytes > limits.evidence_bytes as u64,
            "evidence_bytes",
        ),
        (
            validation_bytes > limits.captured_validation_bytes,
            "captured_validation_bytes",
        ),
        (
            inventory_bytes > MAX_QUERY_BYTES,
            "inventory_response_bytes",
        ),
        (tools > limits.tool_actions as usize, "tool_actions"),
        (
            models > limits.model_invocations as usize,
            "model_invocations",
        ),
    ] {
        if exceeded {
            violations.push(name);
        }
    }
    drop(guards);
    Ok(Measurement {
        schema_version: 1,
        status: "measurement_only_no_semantic_sufficiency_claim",
        preparation_sha256: preparation_hash.into(),
        raw_selection_sha256: selection_hash.into(),
        canonical_measurement_request_sha256: hash(&canonical_bytes),
        specialist_id: role.specialist_id,
        raw_obligation_count: raw_count,
        canonical_obligation_count: canonical.obligations.obligations.len(),
        raw_request_bytes: raw_bytes,
        raw_envelope_bytes,
        canonical_envelope_bytes: bytes(&canonical.obligations)?.len(),
        request_binding: "exact_proposal_only_parent_and_conversation_may_change_before_execution",
        canonical_request_bytes: canonical_bytes.len(),
        production_normalization_error: normalization_error,
        source_evidence_items: ranges.len(),
        ranges,
        source_evidence_bytes: source_bytes,
        repeated_full_file_validation_bytes: validation_bytes,
        inventory_response_bytes: inventory_bytes,
        minimum_tool_actions: tools,
        minimum_model_invocations: models,
        current_limit_violations: violations,
        executable_admission_established: false,
        compared_limits: limits,
        admission_caps: serde_json::json!({"profile":"investigation-v1","raw_obligations":12,
            "request_bytes":8192,"envelope_bytes":8192,"individual_source_bytes":8192,
            "inventory_response_bytes":MAX_QUERY_BYTES}),
        provider_calls: 0,
        tasks_created: 0,
    })
}
