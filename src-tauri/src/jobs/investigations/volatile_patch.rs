//! @27 retains only patch identities here. Candidate prose has no storage type.
use super::*;
use agents::investigation::finish_patch::PatchBasis;

/// Host-derived, text-free correction authority. A whole-answer fallback owns
/// no volatile candidate and uses the existing full-finish response schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CorrectionMode {
    Patch {
        basis: PatchBasis,
    },
    WholeAnswer {
        finish_hash: String,
        refusal_hash: String,
        finding_count: usize,
    },
}
impl CorrectionMode {
    pub(super) fn from_refusal(
        record: &agents::investigation::staged_refusal::RefusalRecord,
        finding_count: usize,
        role: SpecialistId,
    ) -> Result<Self, InvestigationStoreError> {
        let limitations = role.patches_whole_finish();
        if record.refusal_count != 1
            || finding_count > 12
            || record
                .content_id()
                .map_err(|_| InvestigationStoreError::Invalid)?
                != record.refusal_id
        {
            return Err(InvestigationStoreError::Invalid);
        }
        // Eligibility must not depend on a caller-supplied count: a forged
        // smaller count cannot downgrade a finding-local refusal to fallback.
        Ok(
            match PatchBasis::from_refusal_for(record, 12, limitations) {
                Ok(_) => Self::Patch {
                    basis: PatchBasis::from_refusal_for(record, finding_count, limitations)
                        .map_err(|_| InvestigationStoreError::Invalid)?,
                },
                Err(_) => Self::WholeAnswer {
                    finish_hash: record.finish_hash.clone(),
                    refusal_hash: record.refusal_id.clone(),
                    finding_count,
                },
            },
        )
    }
    fn finding_count(&self) -> usize {
        match self {
            Self::Patch { basis } => basis.finding_count,
            Self::WholeAnswer { finding_count, .. } => *finding_count,
        }
    }
    fn validate(&self) -> Result<(), InvestigationStoreError> {
        match self {
            Self::Patch { basis } => basis
                .validate()
                .map_err(|_| InvestigationStoreError::Invalid),
            Self::WholeAnswer {
                finish_hash,
                refusal_hash,
                finding_count,
            } => {
                if *finding_count > 12 || !hash_valid(finish_hash) || !hash_valid(refusal_hash) {
                    return Err(InvestigationStoreError::Invalid);
                }
                Ok(())
            }
        }
    }
}
impl StoredTask {
    pub(super) fn pending_patch(&self) -> Option<&PatchBasis> {
        match &self.pending_correction {
            Some(CorrectionMode::Patch { basis }) => Some(basis),
            _ => None,
        }
    }
}

/// Observation only, never admission authority. Unknown counts stay null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishTelemetry {
    pub finding_count: Option<usize>,
    pub flagged_count: Option<usize>,
    pub patched_count: Option<usize>,
    pub withdrawn_count: Option<usize>,
    pub kept_count: Option<usize>,
    pub projection_rejection: Option<agents::investigation::finish_patch::PatchRejection>,
}
impl FinishTelemetry {
    pub(super) fn finish(
        count: usize,
        patch: Option<&agents::investigation::finish_patch::PatchCounts>,
    ) -> Self {
        Self {
            finding_count: Some(count),
            flagged_count: Some(0),
            patched_count: Some(patch.map_or(0, |p| p.patched_count)),
            withdrawn_count: Some(patch.map_or(0, |p| p.withdrawn_count)),
            kept_count: Some(patch.map_or(0, |p| p.kept_count)),
            projection_rejection: None,
        }
    }
    pub(super) fn rejected(code: agents::investigation::finish_patch::PatchRejection) -> Self {
        Self {
            finding_count: None,
            flagged_count: None,
            patched_count: None,
            withdrawn_count: None,
            kept_count: None,
            projection_rejection: Some(code),
        }
    }
    pub(super) fn validate(&self) -> Result<(), InvestigationStoreError> {
        if [
            self.finding_count,
            self.flagged_count,
            self.patched_count,
            self.withdrawn_count,
            self.kept_count,
        ]
        .into_iter()
        .flatten()
        .any(|count| count > 12)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    pub(super) fn refusal(
        &mut self,
        feedback: &agents::investigation::staged_finish::RefusalFeedback,
    ) {
        use agents::investigation::ProseField;
        use agents::investigation::staged_finish::{CombinedCorrection, ProseCorrection};
        let Some(ProseCorrection::Combined(combined)) = &feedback.prose_correction else {
            self.flagged_count = None;
            return;
        };
        if combined.omitted != 0 {
            self.flagged_count = None;
            return;
        }
        let mut indices = std::collections::BTreeSet::new();
        for entry in &combined.entries {
            let location = match entry {
                CombinedCorrection::ReplayAt(l)
                | CombinedCorrection::EnumerationAt(l)
                | CombinedCorrection::ConfigurationAt(l) => l,
                _ => {
                    self.flagged_count = None;
                    return;
                }
            };
            let index = |f: &ProseField| match f {
                ProseField::FindingTitle { finding }
                | ProseField::FindingStatement { finding }
                | ProseField::FindingLimitation { finding, .. } => Some(*finding as usize),
                _ => None,
            };
            match (index(&location.start.field), index(&location.end.field)) {
                (Some(first), Some(last)) if first <= last && last < 12 => {
                    indices.extend(first..=last)
                }
                _ => {
                    self.flagged_count = None;
                    return;
                }
            }
        }
        self.flagged_count = Some(indices.len());
    }
}

pub(super) const LOST_CANDIDATE: &str =
    "The one-turn volatile finish candidate was lost; patch correction cannot resume.";

/// Shape validation is not refusal authentication. This runs on base reads so
/// foreign roles cannot smuggle patch state into a legacy task.
pub(super) fn validate_shape(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    if let Some(marker) = &task.pending_correction {
        if !task.detail.summary.specialist_id.uses_finish_patch()
            || !matches!(
                task.phase,
                Phase::Ready | Phase::Consent | Phase::Invocation
            )
            || task.completion_intent.is_none()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        marker
            .validate()
            .map_err(|_| InvestigationStoreError::Invalid)?;
    }
    Ok(())
}

/// Re-derive index authority from the authenticated immutable first refusal,
/// never from the model patch, caller metadata, or a stored candidate body.
/// The live worker must additionally compare this entire marker to its holder
/// before dispatch and again when consuming the holder for settlement.
pub(super) fn authenticate(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<PatchBasis>, InvestigationStoreError> {
    validate_shape(task)?;
    let Some(marker) = &task.pending_correction else {
        if task.detail.summary.specialist_id.uses_finish_patch()
            && matches!(
                task.phase,
                Phase::Ready | Phase::Consent | Phase::Invocation
            )
            && task
                .detail
                .completion
                .as_ref()
                .is_some_and(|c| c.refusal_count == 1)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        return Ok(None);
    };
    let entries = staged::load_for_task(connection, task)?;
    if entries.len() != 1 {
        return Err(InvestigationStoreError::Invalid);
    }
    let entry = &entries[0];
    admission_journal::AuthenticatedRefusalBasis::load(connection, task, 1, entry)?
        .ok_or(InvestigationStoreError::Invalid)?;
    let derived = CorrectionMode::from_refusal(
        &entry.record,
        marker.finding_count(),
        task.detail.summary.specialist_id,
    )?;
    if derived != *marker {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(task.pending_patch().cloned())
}

/// Recovery owns no volatile holder. No model invocation can recover it.
/// Ordinary Drop releases allocations in the worker, not secure erasure.
pub(super) fn abandon(task: &mut StoredTask) -> bool {
    if task.pending_patch().is_none() {
        return false;
    }
    task.pending_correction = None;
    task.phase = Phase::Terminal;
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    if let Some(state) = &mut task.admission_state {
        state.current_call = None;
    }
    task.detail.summary.status = if task.detail.summary.cancel_requested {
        InvestigationStatus::Cancelled
    } else {
        InvestigationStatus::Failed
    };
    task.detail.summary.completion_outcome = None;
    task.detail.error = Some(LOST_CANDIDATE.into());
    true
}

/// Constructible only after saved-call authentication and consuming the live
/// candidate. A host-merged fingerprint never replaces provider action_hash.
/// Neither this witness nor its constructor retains candidate prose.
pub(super) struct MergeAuthority {
    first: PatchBasis,
    step_id: String,
    provider_action_hash: String,
    merged_finish_hash: String,
}
impl MergeAuthority {
    pub(super) fn validate(
        &self,
        task: &StoredTask,
        finish: &StagedFinish,
        response: &InvestigationResponseMeta,
    ) -> Result<(), InvestigationStoreError> {
        if !task.detail.summary.specialist_id.uses_finish_patch()
            || task.detail.summary.cancel_requested
            || task.pending_patch() != Some(&self.first)
            || response.step_id != self.step_id
            || response.action_hash != self.provider_action_hash
            || finish.fingerprint()? != self.merged_finish_hash
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

pub(super) fn merge(
    connection: &Connection,
    task: &StoredTask,
    authenticated: &metadata_admission::AuthenticatedResponse,
    response: &InvestigationResponseMeta,
    raw_patch: &str,
    held: agents::investigation::finish_patch::VolatileFinish,
) -> Result<
    Result<
        (
            agents::investigation::finish_patch::MergedFinish,
            MergeAuthority,
        ),
        agents::investigation::finish_patch::PatchRejection,
    >,
    InvestigationStoreError,
> {
    let first = authenticate(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
    if task.detail.summary.cancel_requested
        || authenticated.call.mode() != CompletionCallMode::FinishOnly
        || authenticated
            .call
            .admission
            .acquisition
            .as_ref()
            .and_then(|s| s.finish_patch.as_ref())
            != Some(&first)
        || response.step_id != authenticated.call.step_id
        || core_prov::content_hash(raw_patch.as_bytes()) != response.response_hash
        || raw_patch.len() > MAX_ACTION_BYTES
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let original: serde_json::Value =
        serde_json::from_str(raw_patch).map_err(|_| InvestigationStoreError::Invalid)?;
    if encode(&original, MAX_ACTION_BYTES)? != authenticated.original {
        return Err(InvestigationStoreError::Invalid);
    }
    let whole = task.detail.summary.specialist_id.patches_whole_finish();
    let merged = match held.merge_with(raw_patch, &first, whole) {
        Ok(merged) => merged,
        Err(code) => return Ok(Err(code)),
    };
    let authority = MergeAuthority {
        first,
        step_id: response.step_id.clone(),
        provider_action_hash: response.action_hash.clone(),
        merged_finish_hash: merged.finish.fingerprint()?,
    };
    authority.validate(task, &merged.finish, response)?;
    Ok(Ok((merged, authority)))
}

impl JobStore {
    pub fn investigation_pending_patch(
        &self,
        execution: &JobExecution,
        id: &str,
    ) -> Result<Option<PatchBasis>, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self.conn.unchecked_transaction()?;
        let task = load_next_prompt_task(&tx, id)?;
        transitions::check_identity(&task, execution)?;
        let marker = authenticate(&tx, &task)?;
        tx.commit()?;
        Ok(marker)
    }
}

/// Host-only commitment to an admitted merged result. Provider hashes remain
/// unchanged, and observation telemetry is deliberately excluded.
pub(super) fn result_witness(
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    result: &InvestigationResult,
    basis: &admission_journal::AuthenticatedCompletionBasis,
) -> Result<String, InvestigationStoreError> {
    let patch = basis.patch().ok_or(InvestigationStoreError::Invalid)?;
    if !task.detail.summary.specialist_id.uses_finish_patch()
        || result.investigation_id != task.detail.summary.investigation_id
        || result.invocation_step_id.as_ref() != Some(&response.step_id)
        || result
            .completion
            .as_ref()
            .is_none_or(|c| c.withholding.is_some())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let result_hash = core_prov::content_hash(&encode(result, MAX_RESULT_BYTES)?);
    let binding = encode(
        &(
            &task.detail.summary.investigation_id,
            &response.step_id,
            &response.response_hash,
            &response.action_hash,
            patch,
            result_hash,
        ),
        MAX_RECORD_BYTES,
    )?;
    let mut bytes = b"cartograph:volatile-patch-admitted-result:v1\0".to_vec();
    bytes.extend_from_slice(&binding);
    Ok(core_prov::content_hash(&bytes))
}

pub(super) fn validate_result_witness(
    connection: &Connection,
    task: &StoredTask,
    result: &InvestigationResult,
    basis: Option<&admission_journal::AuthenticatedCompletionBasis>,
) -> Result<(), InvestigationStoreError> {
    let Some(basis) = basis.filter(|b| b.patch().is_some()) else {
        return Ok(());
    };
    // Partial projections have their own independently authenticated witness.
    if result
        .completion
        .as_ref()
        .is_some_and(|c| c.withholding.is_some())
    {
        return Ok(());
    }
    let step = result
        .invocation_step_id
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let response: InvestigationResponseMeta = read_body(
        connection,
        "investigation_responses",
        "step_id",
        &task.detail.summary.investigation_id,
        step,
    )?
    .ok_or(InvestigationStoreError::Missing)?;
    if response.finish_feedback_witness.as_deref()
        != Some(result_witness(task, &response, result, basis)?.as_str())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    fn marker() -> PatchBasis {
        PatchBasis {
            finish_hash: core_prov::content_hash(b"fixture finish"),
            refusal_hash: core_prov::content_hash(b"fixture refusal"),
            finding_count: 3,
            flagged_indices: vec![0, 2],
            flagged_limitations: vec![],
            capacity_gaps: vec![],
        }
    }

    #[test]
    fn volatile_patch_marker_is_text_free_and_absent_from_legacy_serialization() {
        let fixture = completion::internal_next_prompt_fixture();
        let mut task = fixture.task.clone();
        let historical = serde_json::to_value(&task).unwrap();
        assert!(historical.get("pending_correction").is_none());
        task.detail.summary.specialist_id = SpecialistId::DomainAnalystV27;
        task.phase = Phase::Ready;
        task.pending_correction = Some(CorrectionMode::Patch { basis: marker() });
        validate_shape(&task).unwrap();
        let value = serde_json::to_value(&task).unwrap();
        let object = value["pending_correction"]["basis"].as_object().unwrap();
        assert_eq!(object.len(), 4);
        for key in [
            "finish_hash",
            "refusal_hash",
            "finding_count",
            "flagged_indices",
        ] {
            assert!(object.contains_key(key));
        }
        task.pending_correction = None;
        task.detail.summary.specialist_id = fixture.task.detail.summary.specialist_id;
        task.phase = fixture.task.phase;
        assert_eq!(serde_json::to_value(&task).unwrap(), historical);
    }

    #[test]
    fn volatile_patch_marker_rejects_foreign_roles_and_lost_holder_fails_closed() {
        let fixture = completion::internal_next_prompt_fixture();
        let mut task = fixture.task.clone();
        task.pending_correction = Some(CorrectionMode::Patch { basis: marker() });
        task.phase = Phase::Ready;
        assert!(validate_shape(&task).is_err());
        task.detail.summary.specialist_id = SpecialistId::DomainAnalystV27;
        validate_shape(&task).unwrap();
        for phase in [Phase::Ready, Phase::Consent, Phase::Invocation] {
            for cancelled in [false, true] {
                let mut interrupted = task.clone();
                interrupted.phase = phase;
                interrupted.detail.summary.cancel_requested = cancelled;
                let usage = interrupted.detail.usage.clone();
                assert!(abandon(&mut interrupted));
                assert!(interrupted.pending_correction.is_none());
                assert!(interrupted.pending.is_none());
                assert_eq!(interrupted.phase, Phase::Terminal);
                assert_eq!(interrupted.detail.error.as_deref(), Some(LOST_CANDIDATE));
                assert_eq!(interrupted.detail.usage, usage);
                assert_eq!(
                    interrupted.detail.summary.status,
                    if cancelled {
                        InvestigationStatus::Cancelled
                    } else {
                        InvestigationStatus::Failed
                    }
                );
                assert!(!abandon(&mut interrupted));
                validate_shape(&interrupted).unwrap();
            }
        }
        if let Some(CorrectionMode::Patch { basis }) = &mut task.pending_correction {
            basis.flagged_indices = vec![2, 0];
        }
        assert!(validate_shape(&task).is_err());
    }

    // AC-0832 (#1335): @28 carries @27's volatile patch marker with the same
    // phase and intent rules; the neighbouring @26 still cannot.
    #[test]
    fn volatile_patch_marker_admits_analyst28_like_analyst27() {
        let fixture = completion::internal_next_prompt_fixture();
        let mut task = fixture.task.clone();
        task.pending_correction = Some(CorrectionMode::Patch { basis: marker() });
        for role in [
            SpecialistId::DomainAnalystV26,
            SpecialistId::DomainAnalystV27,
            SpecialistId::DomainAnalystV28,
            SpecialistId::DomainAnalystV29,
            SpecialistId::DomainAnalystV30,
        ] {
            task.detail.summary.specialist_id = role;
            for phase in [
                Phase::Ready,
                Phase::Consent,
                Phase::Invocation,
                Phase::Terminal,
            ] {
                task.phase = phase;
                assert_eq!(
                    validate_shape(&task).is_ok(),
                    role != SpecialistId::DomainAnalystV26 && phase != Phase::Terminal,
                    "{role:?} {phase:?}"
                );
            }
        }
        task.phase = Phase::Ready;
        task.completion_intent = None;
        assert!(validate_shape(&task).is_err());
    }
}
