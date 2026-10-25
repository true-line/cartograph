//! Source owners retained across nested execution phases. Acquisition, rejected
//! candidate construction, settlement scratch and allocator/RSS are separate.
use super::evidence::prepared_source::{PreparedSourceCandidate, PreparedSourceFailure};
use crate::jobs::investigations::{CommittedSourceReservation, CommittedSourceValidation};
use crate::mcp_reads::dispatch::EvidenceWorkLimits;
use agents::{AgentEvidence, TaskFactSelection, investigation::*};
use context_hub::logical::{json_bytes, map, slots, sum};
use core_graph::source::FactKey;
use std::mem::size_of;

/// Covers the successful prepared/candidate state and the failure retaining its
/// source leases. Callers add this outside the execution-phase maximum. It is
/// not a bound on preparation or the source continuation snapshot itself.
pub(crate) fn retained_bytes(limits: EvidenceWorkLimits, app_path_bytes: usize) -> Option<usize> {
    let policy = &INVESTIGATION_V2_POLICY;
    let selected = policy
        .limits()
        .selected_facts
        .min(core_graph::source::MAX_SOURCE_SELECTION_KEYS);
    let (ledger, _) = InvestigationInputLedger::decode_workspace_bytes()?;
    // A returned selection equals the validated ledger's selected_facts. Its
    // map also owns a second copy of each fact key; rejected observations are
    // acquisition work and cannot use this successful-state bound.
    let selection = sum(&[
        json_bytes(MAX_MANIFEST_BYTES)?,
        MAX_MANIFEST_BYTES,
        map::<FactKey, TaskFactSelection>(selected)?,
    ])?;
    let (guards, _) =
        super::query_workspace::guard_owners(selected, MAX_MANIFEST_BYTES, app_path_bytes)?;
    let receipt = crate::mcp_reads::reservation::receipt::workspace_bytes()?.retained;
    // Use the enforcement ceiling for the validated target and, conservatively,
    // its source DTO copied into AgentEvidence. Headers are counted separately.
    let target = json_bytes(MAX_INSPECTION_TARGET_BYTES)?;
    let text = usize::try_from(
        limits
            .max_span_bytes
            .get()
            .min(source_capture::MAX_SPAN_BYTES),
    )
    .ok()?
    .min(policy.limits().evidence_bytes);
    // Reservation and charge receipts each retain a saved detail; Worker also
    // keeps a cloned detail. Use complete bounded task owners for all three,
    // including pre-existing completion observations and citations.
    let (detail, _) = crate::jobs::investigations::next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    sum(&[
        size_of::<PreparedSourceCandidate>().max(size_of::<PreparedSourceFailure>()),
        ledger.checked_mul(2)?, // prepared base and accepted candidate ledger
        selection,
        guards,
        receipt,
        target.checked_mul(2)?,
        text,
        size_of::<AgentEvidence>(),
        "evidence-"
            .len()
            .checked_add(usize::MAX.to_string().len())?,
        64,             // prepared base-ledger hash
        128 + 128 + 64, // attempted task/step/reservation identity
        slots::<CommittedSourceReservation>(1)?,
        slots::<CommittedSourceValidation>(1)?,
        (128 + 128 + 64 * 3) * 2, // receipt identities, excluding saved detail
        detail.checked_mul(3)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    #[test]
    fn source_retained_owners_track_span_and_path_without_file_payload() {
        // AC-0327: object validation is transient; exact evidence survives.
        let mut limits = EvidenceWorkLimits {
            max_file_validation_bytes: NonZeroU64::new(1).unwrap(),
            max_span_bytes: NonZeroU64::new(1).unwrap(),
        };
        let small = retained_bytes(limits, 16).unwrap();
        limits.max_span_bytes = NonZeroU64::new(128).unwrap();
        assert_eq!(retained_bytes(limits, 16).unwrap() - small, 127);
        let span = retained_bytes(limits, 16).unwrap();
        limits.max_file_validation_bytes = NonZeroU64::new(u64::MAX).unwrap();
        assert_eq!(retained_bytes(limits, 16), Some(span));
        assert!(retained_bytes(limits, 4096).unwrap() > span);
        assert!(retained_bytes(limits, usize::MAX).is_none());
    }
}
