//! Reduction of host observations, separate from packet review and identity.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Required families of checks. Each family can have multiple source/file checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessInput {
    /// The recovered graph snapshot, including changes outside selected facts.
    Graph,
    /// Exact fact-to-source receipt associations.
    Associations,
    /// Registered source identity and bound revision.
    RegistrationRevision,
    /// Availability and integrity of retained historical material.
    RetainedIntegrity,
    /// Complete selected file bytes, including uncited ranges.
    LiveFileBytes,
    /// Relevant source membership and ignore inputs.
    Membership,
    /// All relevant configuration and dependency inputs are known and checked.
    ConfigurationClosure,
}

/// Result reported by a host check; equal means equality within its stated scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessResult {
    /// Bound and observed inputs agree.
    Equal,
    /// A relevant input differs, even if the recovered graph is unchanged.
    Changed,
    /// Required evidence could not be acquired.
    Unavailable,
    /// No producer can establish this check.
    Unsupported,
    /// Acquisition raced or observations were contradictory.
    Raced,
}

/// Scoped observation retained in the assessment, never in canonical identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessObservation {
    /// Kind of input examined.
    pub input: FreshnessInput,
    /// Host-defined stable reference identifying the checked source/input.
    pub subject: String,
    /// Check outcome.
    pub result: FreshnessResult,
}

/// Independent source freshness; this never conveys review or execution approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    /// All required input families and relevant closure were verified equal.
    Current,
    /// A relevant input changed.
    Stale,
    /// At least one acquisition raced or yielded contradictory observations.
    Conflicted,
    /// At least one required check is missing, unavailable or unsupported.
    Unknown,
}

/// Reduce a complete host assessment without changing or dropping its checks.
///
/// The host must enumerate every relevant subject; the configuration-closure
/// check may be equal only when that enumeration is established by producers.
/// Current producers with incomplete closure must report unsupported/unavailable.
/// Review state is deliberately not an input. A known change outranks missing
/// evidence, while a race outranks both. An empty assessment is never current.
pub fn assess_freshness(checks: &[FreshnessObservation]) -> FreshnessState {
    if checks
        .iter()
        .any(|check| check.result == FreshnessResult::Raced)
    {
        return FreshnessState::Conflicted;
    }
    let mut observed = BTreeMap::new();
    for check in checks {
        let bit = match check.result {
            FreshnessResult::Equal => 1,
            FreshnessResult::Changed => 2,
            _ => 0,
        };
        let results = observed
            .entry((check.input, check.subject.as_str()))
            .or_insert(0);
        *results |= bit;
        if *results == 3 {
            return FreshnessState::Conflicted;
        }
    }
    if checks
        .iter()
        .any(|check| check.result == FreshnessResult::Changed)
    {
        return FreshnessState::Stale;
    }
    let required = [
        FreshnessInput::Graph,
        FreshnessInput::Associations,
        FreshnessInput::RegistrationRevision,
        FreshnessInput::RetainedIntegrity,
        FreshnessInput::LiveFileBytes,
        FreshnessInput::Membership,
        FreshnessInput::ConfigurationClosure,
    ];
    if checks
        .iter()
        .any(|check| check.subject.trim().is_empty() || check.result != FreshnessResult::Equal)
        || required
            .iter()
            .any(|input| !checks.iter().any(|check| check.input == *input))
    {
        return FreshnessState::Unknown;
    }
    FreshnessState::Current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> Vec<FreshnessObservation> {
        [
            FreshnessInput::Graph,
            FreshnessInput::Associations,
            FreshnessInput::RegistrationRevision,
            FreshnessInput::RetainedIntegrity,
            FreshnessInput::LiveFileBytes,
            FreshnessInput::Membership,
            FreshnessInput::ConfigurationClosure,
        ]
        .into_iter()
        .map(|input| FreshnessObservation {
            input,
            subject: "registered-input".into(),
            result: FreshnessResult::Equal,
        })
        .collect()
    }

    #[test]
    fn current_requires_every_family_and_complete_closure() {
        let checks = complete();
        assert_eq!(assess_freshness(&checks), FreshnessState::Current);
        assert_eq!(assess_freshness(&[]), FreshnessState::Unknown);
        for index in 0..checks.len() {
            let mut missing = checks.clone();
            missing.remove(index);
            assert_eq!(assess_freshness(&missing), FreshnessState::Unknown);
            for result in [FreshnessResult::Unsupported, FreshnessResult::Unavailable] {
                let mut incomplete = checks.clone();
                incomplete[index].result = result;
                assert_eq!(assess_freshness(&incomplete), FreshnessState::Unknown);
            }
        }
    }

    #[test]
    fn changed_live_bytes_override_equal_graph_and_unknown_closure() {
        // AC-0249: freshness must not be inferred from graph identity alone.
        let mut checks = complete();
        checks[4].result = FreshnessResult::Changed;
        checks[6].result = FreshnessResult::Unsupported;
        assert_eq!(assess_freshness(&checks), FreshnessState::Stale);
        checks.reverse();
        assert_eq!(assess_freshness(&checks), FreshnessState::Stale);
        checks.push(FreshnessObservation {
            input: FreshnessInput::Associations,
            subject: "second-source".into(),
            result: FreshnessResult::Raced,
        });
        assert_eq!(assess_freshness(&checks), FreshnessState::Conflicted);
    }

    #[test]
    fn additional_unavailable_subject_prevents_current() {
        let mut checks = complete();
        checks.push(FreshnessObservation {
            input: FreshnessInput::LiveFileBytes,
            subject: "deleted-file".into(),
            result: FreshnessResult::Unavailable,
        });
        assert_eq!(assess_freshness(&checks), FreshnessState::Unknown);
        checks.pop();
        checks[0].subject.clear();
        assert_eq!(assess_freshness(&checks), FreshnessState::Unknown);
    }

    #[test]
    fn contradictory_results_for_one_subject_are_conflicted_in_any_order() {
        let mut checks = complete();
        let mut changed = checks[4].clone();
        changed.result = FreshnessResult::Changed;
        checks.push(changed);
        assert_eq!(assess_freshness(&checks), FreshnessState::Conflicted);
        checks.reverse();
        assert_eq!(assess_freshness(&checks), FreshnessState::Conflicted);
        checks[0].subject = "different-file".into();
        assert_eq!(assess_freshness(&checks), FreshnessState::Stale);
    }
}
