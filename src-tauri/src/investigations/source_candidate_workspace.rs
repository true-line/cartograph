//! Candidate source-ledger validation before final manifest admission. Caller
//! text/source/guards and durable publication are separate; not allocator/RSS.
use agents::{
    InputClosureStatus, PrimarySourceScope, TaskCaptureFileRef, TaskCaptureSpanRef, TaskFactKey,
    investigation::*,
};
use context_hub::logical::{count, json_bytes, slots, string_wire, sum};

pub(crate) struct SourceCandidateWorkspace {
    pub candidate_wire: usize,
}
impl SourceCandidateWorkspace {
    pub fn peak(&self) -> Option<usize> {
        sum(&[
            slots::<InvestigationInputLedger>(1)?,
            json_bytes(self.candidate_wire)?,
            ledger_validation_workspace(&INVESTIGATION_V2_POLICY, self.candidate_wire)?,
        ])
    }
}

/// The base ledger and target must already pass their existing wire admission.
/// Each fact/digest/source/captured field is copied from that target (or its
/// revalidated equal receipt). Target wire bounds those values, including enums
/// and numbers; the real citation envelope covers the added nesting and tags.
pub(crate) fn workspace(base_wire: usize, target_wire: usize) -> Option<SourceCandidateWorkspace> {
    if base_wire > MAX_MANIFEST_BYTES || target_wire > MAX_INSPECTION_TARGET_BYTES {
        return None;
    }
    let envelope = InvestigationCitation {
        citation_id: String::new(),
        fact: TaskFactKey::Node { id: String::new() },
        fact_digest: String::new(),
        source: None,
        role: None,
        index: None,
        text_hash: Some(String::new()),
        origin: InvestigationEvidenceOrigin::CapturedPrimarySource {
            registered_source_id: String::new(),
            receipt_id: String::new(),
            receipt_inventory_index: 0,
            captured: TaskCaptureSpanRef {
                file: TaskCaptureFileRef {
                    source_id: String::new(),
                    capture_id: String::new(),
                    path: String::new(),
                    digest: String::new(),
                    byte_len: 0,
                },
                byte_start: 0,
                byte_end: 0,
            },
            scope: PrimarySourceScope::PrimarySourceOnly,
            input_closure: InputClosureStatus::InputClosureNotEstablished,
        },
    };
    let append = sum(&[
        count(&envelope)?,
        target_wire,
        string_wire(
            "evidence-"
                .len()
                .checked_add(usize::MAX.to_string().len())?,
        )?,
        string_wire(64)?, // new text hash
        u64::MAX.to_string().len(),
        1, // revision growth and array separator
    ])?;
    let candidate_wire = base_wire.checked_add(append)?;
    Some(SourceCandidateWorkspace { candidate_wire })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_candidate_admission_covers_append_beyond_accepted_manifest() {
        // AC-0327: pre-refusal validation must not clamp to the accepted cap.
        let max = workspace(MAX_MANIFEST_BYTES, MAX_INSPECTION_TARGET_BYTES).unwrap();
        assert!(max.candidate_wire > MAX_MANIFEST_BYTES + MAX_INSPECTION_TARGET_BYTES);
        let small = workspace(1, 1).unwrap();
        assert!(max.peak().unwrap() > small.peak().unwrap());
        assert!(workspace(MAX_MANIFEST_BYTES + 1, 1).is_none());
        assert!(workspace(1, MAX_INSPECTION_TARGET_BYTES + 1).is_none());
        assert!(workspace(usize::MAX, usize::MAX).is_none());
    }
}
