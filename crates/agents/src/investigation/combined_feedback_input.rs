//! Stream all detector families into schema-six feedback without retaining an
//! unbounded intermediate list. Host eligibility and witness checks remain separate.
use super::*;

impl RefusalFeedback {
    /// Collect complete text-free diagnostics from one exact prepared input and
    /// finish. The host must authenticate citations, reserve/charge workspace,
    /// establish correction eligibility and bind the result to its invocation.
    /// This method neither admits the finish nor spends a correction allowance.
    pub fn collect_input_corrections_with_policy(
        input: &InvestigationInput,
        finish: &StagedFinish,
        model: Option<&str>,
        unmet: Vec<FeedbackEntry>,
        policy: &InvestigationPolicy,
    ) -> Result<Option<Self>, InvestigationError> {
        finish.fingerprint()?;
        match input.validate_correction_prose(finish, model) {
            Ok(()) | Err(InvestigationError::SourceReplay) => (),
            Err(error) => return Err(error),
        }
        let InvestigationAction::Finish {
            findings,
            limitations,
            ..
        } = &finish.action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        Self::collect_combined_visiting_with_policy(unmet, policy, |emit| {
            let mut provider_metadata = false;
            input.visit_correction_replay_locations(finish, model, |location| {
                if matches!(location.start.field, ProseField::Model)
                    || matches!(location.end.field, ProseField::Model)
                {
                    provider_metadata = true;
                } else {
                    // The collector retains its first error even though detector
                    // callbacks cannot return it; traversal remains complete.
                    let _ = emit(CombinedCorrection::ReplayAt(location));
                }
            })?;
            let overall = limitations.iter().map(|text| (text.as_str(), &[][..]));
            let local = findings.iter().flat_map(|finding| {
                finding
                    .limitations
                    .iter()
                    .map(|text| (text.as_str(), finding.citation_ids.as_slice()))
            });
            if let Some(ProseCorrection::LimitationDeniesAcquiredEvidence(conflict)) =
                limitation_check::correction(input.ledger(), overall.chain(local))
            {
                for evidence_id in conflict.evidence_ids {
                    emit(CombinedCorrection::LimitationDeniesEvidence { evidence_id })?;
                }
            }
            input.visit_claim_corrections(finish, |correction| {
                let entry = match correction {
                    ProseCorrection::UnverifiedEnumerationClaimAt(location) => {
                        CombinedCorrection::EnumerationAt(location)
                    }
                    ProseCorrection::UnverifiedConfigurationClaimAt(location) => {
                        CombinedCorrection::ConfigurationAt(location)
                    }
                    // The claim visitor emits only these two classes.
                    _ => unreachable!(),
                };
                let _ = emit(entry);
            })?;
            if prose::has_placeholder(finish)
                || (matches!(
                    input.frozen_specialist()?,
                    SpecialistId::DomainAnalystV26
                        | SpecialistId::DomainAnalystV27
                        | SpecialistId::DomainAnalystV28
                        | SpecialistId::DomainAnalystV29
                        | SpecialistId::DomainAnalystV30
                ) && prose::has_bare_finish_field(finish))
            {
                emit(CombinedCorrection::RewriteSchemaPlaceholders)?;
            }
            if provider_metadata {
                emit(CombinedCorrection::ParaphraseProviderMetadata)?;
            }
            Ok(())
        })
    }
}
