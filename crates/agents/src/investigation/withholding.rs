//! Current-response exclusion planning, not result or correction authorization.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{staged_finish::*, *};
use std::collections::BTreeSet;

/// A complete current-finish detector projection. Only the host may authorize
/// its use after authenticating the prior correction and all admission bindings.
#[derive(Debug, PartialEq, Eq)]
pub struct FindingWithholdingPlan {
    finish_hash: String,
    ledger_hash: String,
    withheld: Vec<usize>,
}
impl FindingWithholdingPlan {
    pub fn finish_hash(&self) -> &str {
        &self.finish_hash
    }
    pub fn withheld_indexes(&self) -> &[usize] {
        &self.withheld
    }
}

/// Transient projection only. It cannot be serialized or published as a result;
/// final admission must add the host partial disposition and durable witness.
pub struct WithheldFinishProjection {
    original_finish_hash: String,
    projected: StagedFinish,
    survivor_indexes: Vec<usize>,
}
impl WithheldFinishProjection {
    pub fn original_finish_hash(&self) -> &str {
        &self.original_finish_hash
    }
    pub fn finish(&self) -> &StagedFinish {
        &self.projected
    }
    pub fn survivor_indexes(&self) -> &[usize] {
        &self.survivor_indexes
    }
}

impl InvestigationInput {
    /// Recompute all detector families from this finish. Stored feedback is never
    /// accepted as exclusion authority. This does not admit citations, establish
    /// correction eligibility, discharge obligations, or construct a result.
    pub fn finding_withholding_plan_with_policy(
        &self,
        finish: &StagedFinish,
        model: Option<&str>,
        policy: &InvestigationPolicy,
    ) -> Result<Option<FindingWithholdingPlan>, InvestigationError> {
        // The host route is analyst@25 only. Never broaden its ordinary finish
        // contract to accept narrative-bearing message-role output.
        if finish.schema_version != 1 || finish.narrative.is_some() {
            return Err(InvestigationError::InvalidAction);
        }
        let finish_hash = finish.fingerprint()?;
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(InvestigationError::InvalidAction);
        };
        let Some(feedback) = RefusalFeedback::collect_input_corrections_with_policy(
            self,
            finish,
            model,
            Vec::new(),
            policy,
        )?
        else {
            return Ok(None);
        };
        let Some(withheld) = eligible_indexes(&feedback, findings.len()) else {
            return Ok(None);
        };
        Ok(Some(FindingWithholdingPlan {
            finish_hash,
            ledger_hash: self.ledger().fingerprint_with_policy(policy)?,
            withheld,
        }))
    }
    /// Consume a plan only against the same current finish and ledger. Surviving
    /// content is unchanged and must undergo normal citation, narrative and
    /// obligation admission; this method supplies no completion authorization.
    pub fn project_withheld_finish_with_policy(
        &self,
        plan: FindingWithholdingPlan,
        finish: &StagedFinish,
        policy: &InvestigationPolicy,
    ) -> Result<WithheldFinishProjection, InvestigationError> {
        if plan.finish_hash != finish.fingerprint()?
            || plan.ledger_hash != self.ledger().fingerprint_with_policy(policy)?
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut projected = finish.clone();
        let InvestigationAction::Finish { findings, .. } = &mut projected.action else {
            return Err(InvestigationError::InvalidAction);
        };
        let survivor_indexes = (0..findings.len())
            .filter(|index| plan.withheld.binary_search(index).is_err())
            .collect::<Vec<_>>();
        let mut index = 0;
        findings.retain(|_| {
            let keep = plan.withheld.binary_search(&index).is_err();
            index += 1;
            keep
        });
        if findings.is_empty() {
            return Err(InvestigationError::InvalidInput);
        }
        projected.fingerprint()?;
        Ok(WithheldFinishProjection {
            original_finish_hash: plan.finish_hash,
            projected,
            survivor_indexes,
        })
    }
}

// This helper sees only freshly derived feedback in production. No deserialization
// or public constructor can turn stored/caller-supplied indexes into a plan.
fn eligible_indexes(feedback: &RefusalFeedback, count: usize) -> Option<Vec<usize>> {
    if feedback.schema_version != 6
        || feedback.citation_correction.is_some()
        || !feedback.unmet.is_empty()
    {
        return None;
    }
    let Some(ProseCorrection::Combined(corrections)) = &feedback.prose_correction else {
        return None;
    };
    if corrections.omitted != 0 || corrections.entries.is_empty() {
        return None;
    }
    let mut withheld = BTreeSet::new();
    for entry in &corrections.entries {
        let location = match entry {
            CombinedCorrection::EnumerationAt(location)
            | CombinedCorrection::ConfigurationAt(location) => location,
            _ => return None,
        };
        location.validate().ok()?;
        let start = finding_index(&location.start.field)?;
        let end = finding_index(&location.end.field)?;
        if start != end || start >= count {
            return None;
        }
        withheld.insert(start);
    }
    if withheld.len() == count {
        return None;
    }
    Some(withheld.into_iter().collect())
}

fn finding_index(field: &ProseField) -> Option<usize> {
    match field {
        ProseField::FindingTitle { finding }
        | ProseField::FindingStatement { finding }
        | ProseField::FindingLimitation { finding, .. } => usize::try_from(*finding).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn location(finding: u32) -> ReplayLocation {
        let position = ProsePosition {
            field: ProseField::FindingStatement { finding },
            sentence: 1,
        };
        ReplayLocation {
            start: position.clone(),
            end: position,
        }
    }
    fn feedback(entries: Vec<CombinedCorrection>) -> RefusalFeedback {
        RefusalFeedback {
            citation_drop: None,
            schema_version: 6,
            citation_correction: None,
            unmet: vec![],
            prose_correction: Some(ProseCorrection::Combined(Box::new(CombinedCorrections {
                entries,
                omitted: 0,
            }))),
        }
    }
    // AC-0287 / #1705: current indexes, both classes and deterministic deduplication.
    #[test]
    fn current_indexes_replace_previous_locations_and_are_sorted() {
        let previous = feedback(vec![CombinedCorrection::EnumerationAt(location(1))]);
        let current = feedback(vec![
            CombinedCorrection::ConfigurationAt(location(2)),
            CombinedCorrection::EnumerationAt(location(0)),
            CombinedCorrection::ConfigurationAt(location(0)),
        ]);
        assert_eq!(eligible_indexes(&previous, 4), Some(vec![1]));
        assert_eq!(eligible_indexes(&current, 4), Some(vec![0, 2]));
    }
    #[test]
    fn all_withheld_and_omitted_and_global_fail_closed() {
        let mut f = feedback(vec![CombinedCorrection::EnumerationAt(location(0))]);
        assert_eq!(eligible_indexes(&f, 1), None);
        let Some(ProseCorrection::Combined(c)) = f.prose_correction.as_mut() else {
            panic!()
        };
        c.omitted = 1;
        assert_eq!(eligible_indexes(&f, 2), None);
        let mut global = location(0);
        global.start.field = ProseField::Narrative;
        global.end.field = ProseField::Narrative;
        assert_eq!(
            eligible_indexes(
                &feedback(vec![CombinedCorrection::EnumerationAt(global)]),
                2
            ),
            None
        );
    }
    #[test]
    fn other_failures_and_cross_finding_locations_stay_fatal() {
        for other in [
            CombinedCorrection::ReplayAt(location(0)),
            CombinedCorrection::RewriteSchemaPlaceholders,
            CombinedCorrection::ParaphraseProviderMetadata,
            CombinedCorrection::LimitationDeniesEvidence {
                evidence_id: "evidence-1".into(),
            },
        ] {
            assert_eq!(
                eligible_indexes(
                    &feedback(vec![CombinedCorrection::EnumerationAt(location(0)), other]),
                    3
                ),
                None
            );
        }
        let mut cross = location(0);
        cross.end = location(1).end;
        assert_eq!(
            eligible_indexes(
                &feedback(vec![CombinedCorrection::ConfigurationAt(cross)]),
                3
            ),
            None
        );
        assert_eq!(
            eligible_indexes(
                &feedback(vec![CombinedCorrection::EnumerationAt(location(9))]),
                3
            ),
            None
        );
    }
}
