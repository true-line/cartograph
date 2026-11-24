//! Host-only current-finish projection after an authenticated single correction.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn project(
    tx: &Transaction<'_>,
    task: &StoredTask,
    current: &InvestigationInput,
    request: &StagedObligationRequest,
    finish: &StagedFinish,
    evidence: &[AttemptEvidence<'_>],
    response: &InvestigationResponseMeta,
    basis: &admission_journal::AuthenticatedCompletionBasis,
    policy: &InvestigationPolicy,
    patch_eligible: Option<&[usize]>,
) -> Result<Option<InvestigationResult>, InvestigationStoreError> {
    if !uses_finding_withholding(task.detail.summary.specialist_id)
        || !matches!(
            (
                task.detail.summary.specialist_id,
                request.specialist_id.as_str()
            ),
            (SpecialistId::DomainAnalystV25, "domain-analyst@25")
                | (SpecialistId::DomainAnalystV26, "domain-analyst@26")
                | (SpecialistId::DomainAnalystV27, "domain-analyst@27")
                | (SpecialistId::DomainAnalystV28, "domain-analyst@28")
                | (SpecialistId::DomainAnalystV29, "domain-analyst@29")
                | (SpecialistId::DomainAnalystV30, "domain-analyst@30")
                | (SpecialistId::DomainAnalystV31, "domain-analyst@31")
                | (SpecialistId::DomainAnalystV32, "domain-analyst@32")
        )
        || task.detail.summary.cancel_requested
    {
        return Ok(None);
    }
    let refusals = completion::raw_refusals(tx, task)?;
    let [prior] = refusals.as_slice() else {
        return Ok(None);
    };
    if prior.record.refusal_count != 1 || prior.record.previous_refusal.is_some() {
        return Ok(None);
    }
    admission_journal::AuthenticatedRefusalBasis::load(tx, task, 1, prior)?
        .ok_or(InvestigationStoreError::Invalid)?;
    let Some(plan) = current.finding_withholding_plan_with_policy(
        finish,
        response.observed_model.as_deref(),
        policy,
    )?
    else {
        return Ok(None);
    };
    if patch_eligible.is_some_and(|eligible| {
        plan.withheld_indexes()
            .iter()
            .any(|index| eligible.binary_search(index).is_err())
    }) {
        // An originally unflagged finding does not gain a withholding escape.
        return Ok(None);
    }
    let withheld_indexes = plan
        .withheld_indexes()
        .iter()
        .map(|index| *index as u32)
        .collect();
    let original_finding_count = match &finish.action {
        InvestigationAction::Finish { findings, .. } => findings.len() as u32,
        _ => return Err(InvestigationStoreError::Invalid),
    };
    let projection = current.project_withheld_finish_with_policy(plan, finish, policy)?;
    let mut projected = projection.finish().clone();
    let InvestigationAction::Finish {
        knowledge_completeness,
        ..
    } = &mut projected.action
    else {
        return Err(InvestigationStoreError::Invalid);
    };
    *knowledge_completeness = KnowledgeCompleteness::Partial;
    // This repeats full admission with unchanged request, evidence and disclosure.
    // It cannot discharge an unmet obligation or admit a surviving invalid citation.
    let decision = basis.binding()?.admit_finish(
        current,
        request,
        &projected,
        evidence,
        response.observed_model.clone(),
        timestamp(tx)?,
    );
    let Some(result) = admitted_projection(decision, || {
        Ok(current
            .finding_withholding_plan_with_policy(
                &projected,
                response.observed_model.as_deref(),
                policy,
            )?
            .is_some())
    })?
    else {
        return Ok(None);
    };
    let completion = result
        .completion
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let sidecar = refusal_diagnostics::collect(
        current,
        finish,
        prior.record.refusal_id.clone(),
        projection.original_finish_hash().to_owned(),
    );
    let witness = FindingWithholdingWitness {
        schema_version: 1,
        detector_version: 1,
        disposition: WithholdingDisposition::FindingsWithheldAfterCorrection,
        original_finish_hash: projection.original_finish_hash().to_owned(),
        original_response_hash: response.response_hash.clone(),
        request_hash: completion.request_hash.clone(),
        obligations_hash: completion.obligations_hash.clone(),
        input_ledger_hash: result.input_ledger_hash.clone(),
        disclosure_hash: result
            .disclosure_hash
            .clone()
            .ok_or(InvestigationStoreError::Invalid)?,
        prior_correction_witness_hash: prior.record.feedback_witness_hash()?,
        projected_result_hash: result.projected_result_hash()?,
        original_finding_count,
        withheld_indexes,
        survivor_indexes: projection
            .survivor_indexes()
            .iter()
            .map(|index| *index as u32)
            .collect(),
    };
    let result = result.with_withholding_witness(witness, policy)?;
    // Sequence two is the current post-correction finish. A projected finish
    // never also appends a second refusal; both writes share the caller tx.
    refusal_diagnostics::insert(tx, &task.detail.summary.investigation_id, 2, &sidecar)?;
    Ok(Some(result))
}

// A refusal is not an error transport. Recompute eligibility only for a genuine
// detector refusal; authority, validation and budget failures stop the writer.
fn admitted_projection(
    decision: Result<Result<InvestigationResult, FinishDecision>, InvestigationError>,
    eligible_claim: impl FnOnce() -> Result<bool, InvestigationStoreError>,
) -> Result<Option<InvestigationResult>, InvestigationStoreError> {
    let decision = decision.map_err(|error| match error {
        InvestigationError::LimitExceeded => InvestigationStoreError::Capacity,
        _ => InvestigationStoreError::Invalid,
    })?;
    match decision {
        Ok(result) => Ok(Some(result)),
        Err(FinishDecision::Refused {
            assessment,
            feedback,
        }) if feedback.unmet.is_empty()
            && assessment.iter().all(|item| item.unmet.is_none())
            && eligible_claim()? =>
        {
            Ok(None)
        }
        Err(_) => Err(InvestigationStoreError::Invalid),
    }
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn projection_non_detector_errors_never_become_refusals() {
        // AC-0287: inject errors at the actual projection admission boundary.
        for error in [
            InvestigationError::InvalidInput,
            InvestigationError::InvalidCitation,
            InvestigationError::SourceReplay,
            InvestigationError::SensitiveOutput,
        ] {
            assert!(matches!(
                admitted_projection(Err(error), || panic!("error must not invoke fallback")),
                Err(InvestigationStoreError::Invalid)
            ));
        }
        assert!(matches!(
            admitted_projection(Err(InvestigationError::LimitExceeded), || {
                panic!("budget failure must not invoke fallback")
            }),
            Err(InvestigationStoreError::Capacity)
        ));
    }
    #[test]
    fn projection_fallback_requires_fresh_claim_eligibility() {
        let refusal = || {
            Ok(Err(FinishDecision::Refused {
                assessment: vec![],
                feedback: agents::investigation::staged_finish::RefusalFeedback {
                    citation_drop: None,
                    schema_version: 6,
                    citation_correction: None,
                    unmet: vec![],
                    prose_correction: None,
                },
            }))
        };
        // The production closure recomputes all detectors on the projection.
        assert!(
            admitted_projection(refusal(), || Ok(true))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            admitted_projection(refusal(), || Ok(false)),
            Err(InvestigationStoreError::Invalid)
        ));
        assert!(matches!(
            admitted_projection(refusal(), || Err(InvestigationStoreError::Storage)),
            Err(InvestigationStoreError::Storage)
        ));
        assert!(matches!(
            admitted_projection(refusal(), || Err(InvestigationStoreError::Ownership)),
            Err(InvestigationStoreError::Ownership)
        ));
    }
}
