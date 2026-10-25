//! Transient guarded-source preparation/revalidation, not text access or durable
//! settlement. Frozen context and returned source owners are admitted separately.
use crate::mcp_reads::{dispatch::EvidenceWorkLimits, reservation};
use agents::{TaskFactSelection, investigation::*};
use context_hub::logical::{json_bytes, map, slots, sum};
use core_graph::{SnapshotReadLimits, source::FactKey};
use std::mem::size_of;

pub(crate) fn peak_bytes(limits: EvidenceWorkLimits, app_path_bytes: usize) -> Option<usize> {
    let policy = &INVESTIGATION_V2_POLICY;
    let selected = policy
        .limits()
        .selected_facts
        .min(core_graph::source::MAX_SOURCE_SELECTION_KEYS);
    let wire = super::query_workspace::selection_wire(selected)?;
    let observation = super::query_workspace::selection_peak(selected, wire)?;
    // Caller key Vec survives select; its internal set/result/graph are in the
    // shared observation. Returned selections equal the validated base ledger.
    let keys = sum(&[MAX_MANIFEST_BYTES, slots::<FactKey>(selected)?])?;
    let selection = sum(&[
        json_bytes(MAX_MANIFEST_BYTES)?,
        MAX_MANIFEST_BYTES,
        map::<FactKey, TaskFactSelection>(selected)?,
    ])?;
    let (guards, guard_peak) =
        super::query_workspace::guard_owners(selected, MAX_MANIFEST_BYTES, app_path_bytes)?;
    let receipt = reservation::receipt::workspace_bytes()?;
    // descriptor() copies a validated receipt range BEFORE target-size refusal.
    // Compact receipts can expand shared range strings: use both complete
    // receipt payload currencies, plus generated snapshot and target identities.
    let descriptor = sum(&[
        size_of::<InvestigationInspectionTarget>(),
        adapters_lang_ts::captured::MAX_RECEIPT_BYTES.checked_mul(2)?,
        "context-v1:".len(),
        64,
        64,
    ])?;
    // content_id's clone occurs only after its first bounded serialization.
    // Include that accepted clone, wire and domain-prefixed hash input.
    let target_validation = sum(&[
        size_of::<InvestigationInspectionTarget>(),
        MAX_INSPECTION_TARGET_BYTES,
        MAX_INSPECTION_TARGET_BYTES,
        MAX_INSPECTION_TARGET_BYTES,
        b"cartograph:inspection-target:v1\0".len(),
        slots::<Vec<u8>>(2)?,
    ])?;
    let receipt_load = sum(&[
        receipt.peak,
        256,
        256,
        slots::<Option<String>>(3)?,
        MAX_MANIFEST_BYTES,
        slots::<FactKey>(1)?, // task_receipt caller key clone
        reservation::schema::workspace_bytes()?,
    ])?;
    let graph = SnapshotReadLimits::default(); // same complete observation as select
    let fact_match =
        adapters_lang_ts::captured::fact_match_workspace(graph.max_bytes, graph.max_values)?;
    let origin = sum(&[
        size_of::<InvestigationEvidenceOrigin>(),
        MAX_INSPECTION_TARGET_BYTES,
    ])?;
    let manifest =
        reservation::capture::workspace(limits, MAX_INSPECTION_TARGET_BYTES)?.manifest_peak;
    let phases = [
        ledger_validation_workspace(policy, MAX_MANIFEST_BYTES)?,
        target_validation,
        sum(&[keys, observation])?,
        sum(&[selection, guard_peak])?,
        receipt_load,
        sum(&[receipt.retained, descriptor, target_validation])?,
        sum(&[receipt.retained, fact_match])?,
        sum(&[receipt.retained, origin, manifest])?,
    ];
    // prepare_captured_read releases preliminary keys/map/receipt at last use,
    // but guards and accepted target stay live through complete reacquisition.
    // Charge those outside the phase max. Final prepared owners and base ledger
    // are already in source_retained_workspace, outside this whole contribution.
    sum(&[
        guards,
        size_of::<InvestigationInspectionTarget>(),
        MAX_INSPECTION_TARGET_BYTES,
        phases.into_iter().max()?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    #[test]
    fn source_preparation_retains_outer_guards_and_excludes_object_caps() {
        // AC-0327: metadata phases do not read captured objects. Path owners
        // remain charged even when the complete graph observation dominates.
        let mut limits = EvidenceWorkLimits {
            max_file_validation_bytes: NonZeroU64::MIN,
            max_span_bytes: NonZeroU64::MIN,
        };
        let small = peak_bytes(limits, 16).unwrap();
        assert!(peak_bytes(limits, 4096).unwrap() > small);
        assert!(peak_bytes(limits, usize::MAX).is_none());
        limits.max_file_validation_bytes = NonZeroU64::new(u64::MAX).unwrap();
        limits.max_span_bytes = NonZeroU64::new(u64::MAX).unwrap();
        assert_eq!(peak_bytes(limits, 16), Some(small));
        let n = INVESTIGATION_V2_POLICY
            .limits()
            .selected_facts
            .min(core_graph::source::MAX_SOURCE_SELECTION_KEYS);
        assert!(
            small
                > super::super::query_workspace::selection_peak(
                    n,
                    super::super::query_workspace::selection_wire(n).unwrap()
                )
                .unwrap()
        );
    }
}
