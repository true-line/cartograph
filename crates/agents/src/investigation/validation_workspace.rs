//! Semantic validation/fingerprint temporaries, separate from record decoding.
use super::{InvestigationPolicy, MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES, MAX_MANIFEST_BYTES};
use crate::{TaskFactKey, TaskFactSelection};
use context_hub::logical::{coverage_batch_validation_workspace, map, set, slots, sum};
use std::collections::{BTreeMap, BTreeSet};

/// Additional logical owners for archive.validate(): bounded encoding finishes
/// before proof/batch inventory validation. Borrowed archive ownership, decoding,
/// allocator/native internals and RSS are excluded. No admission policy changes.
pub fn coverage_archive_validation_workspace() -> Option<usize> {
    let bytes = MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES;
    let encoding = sum(&[bytes, slots::<Vec<u8>>(1)?])?;
    let validation = coverage_batch_validation_workspace(
        bytes,
        core_graph::coverage::MAX_INVENTORY_BYTES.min(bytes),
        context_hub::coverage::MAX_COVERAGE_REPOS,
    )?;
    Some(encoding.max(validation))
}

/// Additional logical workspace for validate_with_policy / fingerprint_with_policy.
/// Caller must bound the typed ledger's compact serialization by typed_row_bytes
/// BEFORE calling: coverage proof validation precedes final manifest admission.
/// Borrowed ledger, decoder scratch and returned fingerprint are separate owners.
/// This component does not establish full worker admission or an RSS bound.
pub fn ledger_validation_workspace(
    policy: &InvestigationPolicy,
    typed_row_bytes: usize,
) -> Option<usize> {
    let limits = policy.limits();
    let facts = limits.selected_facts;
    let citations = facts.checked_add(limits.evidence_items)?;
    let indexes = sum(&[
        slots::<BTreeMap<&TaskFactKey, &TaskFactSelection>>(1)?,
        map::<&TaskFactKey, &TaskFactSelection>(facts)?,
        slots::<BTreeSet<&String>>(1)?,
        set::<&String>(citations)?,
        slots::<BTreeSet<(&String, &String)>>(2)?,
        set::<(&String, &String)>(facts)?.checked_mul(2)?,
        slots::<BTreeSet<&TaskFactKey>>(1)?,
        set::<&TaskFactKey>(citations)?,
    ])?;
    // Only proof identity validation runs here, never inventory parsing. The
    // batch helper conservatively includes its empty-body/lookup-map phase.
    let proof = coverage_batch_validation_workspace(
        typed_row_bytes,
        0,
        context_hub::coverage::MAX_COVERAGE_REPOS,
    )?;
    let validation = sum(&[indexes, MAX_MANIFEST_BYTES, slots::<Vec<u8>>(1)?])?;
    // Indexes are dropped before fingerprinting. Canonical bytes and the
    // domain-prefixed hash input overlap; v1/v2 domains have equal length.
    let fingerprint = sum(&[
        MAX_MANIFEST_BYTES,
        MAX_MANIFEST_BYTES,
        b"cartograph:investigation-input-ledger:v2\0".len(),
        slots::<Vec<u8>>(2)?,
    ])?;
    Some(proof.max(validation).max(fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigation::{
        INVESTIGATION_V1_POLICY, INVESTIGATION_V2_POLICY, InvestigationError,
    };

    // AC-0327: use actual validation and fingerprinting, including proof failure
    // before the later manifest ceiling can reject a large typed ledger.
    #[test]
    fn ledger_workspace_covers_fingerprint_and_early_proof_rejection() {
        let mut ledger = super::super::tests::ledger();
        for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
            ledger.validate_with_policy(policy).unwrap();
            let wire = super::super::bounded_bytes(&ledger, MAX_MANIFEST_BYTES).unwrap();
            let fingerprint = ledger.fingerprint_with_policy(policy).unwrap();
            assert_eq!(fingerprint.len(), 64);
            let bound = ledger_validation_workspace(policy, wire.len()).unwrap();
            assert!(bound >= wire.len() * 2 + b"cartograph:investigation-input-ledger:v1\0".len());
            assert!(ledger_validation_workspace(policy, usize::MAX).is_none());
        }
        let mut proof = context_hub::coverage::CoverageQualification::new(&[], &[])
            .unwrap()
            .durable_proof(&ledger.scope_snapshot_id)
            .unwrap();
        proof.qualification.contract = "x".repeat(MAX_MANIFEST_BYTES + 1);
        ledger.schema_version = 2;
        ledger.coverage = Some(proof);
        let row = context_hub::logical::count(&ledger).unwrap();
        assert!(row > MAX_MANIFEST_BYTES);
        assert!(
            ledger_validation_workspace(&INVESTIGATION_V1_POLICY, row).unwrap()
                > ledger_validation_workspace(&INVESTIGATION_V1_POLICY, MAX_MANIFEST_BYTES)
                    .unwrap()
        );
        assert!(matches!(
            ledger.validate(),
            Err(InvestigationError::InvalidInput)
        ));
    }
}
