//! Host lifecycle lineages for the current analyst identities (#1335).
//!
//! `domain-analyst@28` differs from @27 only in model-visible prompt text (the
//! selected per-call response schema). Every host rule that admits @27 admits
//! @28 through these predicates and `SpecialistId::uses_finish_patch`, so the
//! two identities cannot drift apart site by site. @27 keeps its exact rules.
use agents::investigation::{Capability, SpecialistId};

/// Saved `LocalFinish4096V1` generation for new local desktop starts.
pub(super) fn uses_local_finish_generation(id: SpecialistId) -> bool {
    id.has(Capability::LocalFinishGeneration)
}

/// Host-only finding withholding after one authenticated correction.
pub(super) fn uses_finding_withholding(id: SpecialistId) -> bool {
    id.has(Capability::FindingWithholding)
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    // AC-0831 (#1335): @28 takes every @27 host lifecycle rule, and the
    // historical identities keep exactly the rules they had.
    #[test]
    fn analyst28_shares_every_analyst27_host_lineage() {
        type Rule = fn(SpecialistId) -> bool;
        let rules: [(&str, Rule); 3] = [
            ("local finish", uses_local_finish_generation),
            ("withholding", uses_finding_withholding),
            ("finish patch", SpecialistId::uses_finish_patch),
        ];
        for (name, rule) in rules {
            assert!(rule(SpecialistId::DomainAnalystV27), "{name}");
            assert_eq!(
                rule(SpecialistId::DomainAnalystV28),
                rule(SpecialistId::DomainAnalystV27),
                "{name}"
            );
            // AC-1429: @29 takes every @28 host lifecycle rule as well.
            assert_eq!(
                rule(SpecialistId::DomainAnalystV29),
                rule(SpecialistId::DomainAnalystV28),
                "{name}"
            );
        }
        assert!(uses_local_finish_generation(SpecialistId::DomainAnalystV26));
        assert!(!uses_local_finish_generation(
            SpecialistId::DomainAnalystV25
        ));
        assert!(uses_finding_withholding(SpecialistId::DomainAnalystV25));
        assert!(!uses_finding_withholding(SpecialistId::DomainAnalystV24));
        assert!(!SpecialistId::DomainAnalystV26.uses_finish_patch());
        for role in [
            SpecialistId::DomainAnalystV23,
            SpecialistId::EvidenceAuditorV14,
            SpecialistId::SpecWriterV2,
        ] {
            assert!(!uses_local_finish_generation(role), "{role:?}");
            assert!(!uses_finding_withholding(role), "{role:?}");
            assert!(!role.uses_finish_patch(), "{role:?}");
        }
    }
}
