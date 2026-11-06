//! Retained completed input, separate from construction and sealing peaks.
use super::*;
use context_hub::logical::{count, json_bytes, map, set, slots, sum};
use std::mem::size_of;

impl InvestigationInput {
    /// Construction through the final wire check, including rejected growth.
    /// Caller pages/evidence/request and its already bounded ledger acquisition
    /// are separate; the owned ledger passed to the builder is included here.
    pub fn construction_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        let queries = usize::try_from(policy.limits().tool_actions).ok()?;
        let facts = policy.limits().selected_facts;
        let sources = policy.limits().evidence_items;
        // Each returned fact consumes a raw JSON byte at minimum. Compaction
        // adds two index arrays; an index uses at most usize's decimal width
        // plus a separator, before the projected context's own wire check.
        let index_wire = MAX_TOTAL_QUERY_BYTES
            .checked_mul(2)?
            .checked_mul(usize::MAX.to_string().len().checked_add(1)?)?;
        let frame = count(&serde_json::json!({
            "query_index": usize::MAX, "selection": null, "context": null,
            "evidence_option_indexes": [], "qualification": null,
        }))?
        .checked_add(count(&QueryHistoryReference {
            query_index: usize::MAX,
            query_id: "f".repeat(64),
        })?)?;
        let projection_wire = sum(&[
            MAX_TOTAL_QUERY_BYTES,
            MAX_MANIFEST_BYTES,
            index_wire,
            queries.checked_add(1)?.checked_mul(frame)?.checked_mul(2)?,
        ])?;
        let projection = sum(&[
            json_bytes(projection_wire)?.checked_mul(2)?, // DTO and Value conversion
            json_bytes(MAX_QUERY_BYTES)?,                 // current page, while prior views live
            json_bytes(MAX_MANIFEST_BYTES)?.checked_mul(2)?, // owned fact-key sets
            set::<crate::TaskFactKey>(facts)?.checked_mul(2)?,
            set::<&crate::TaskFactKey>(facts)?.checked_mul(2)?,
            slots::<usize>(MAX_TOTAL_QUERY_BYTES)?.checked_mul(2)?,
            slots::<QueryHistoryReference>(queries)?.checked_mul(2)?,
        ])?;
        // Privacy vectors grow before their joint final check. Finding prose
        // and history metadata each copy from the admitted 16 KiB history wire.
        let privacy = sum(&[
            MAX_TOTAL_QUERY_BYTES,
            2 * 16 * 1024,
            policy.limits().evidence_bytes,
            MAX_MANIFEST_BYTES,
        ])?;
        let details = queries.checked_mul(MAX_QUERY_BYTES.checked_add(1)?)?;
        sum(&[
            Self::retained_workspace_bytes(policy)?,
            acquisition_choices::AuthenticatedCitationCorrection::projection_workspace_bytes(
                policy,
            )?,
            response_schema::workspace_bytes(policy)?,
            json_bytes(MAX_RECORD_BYTES)?, // incoming owned ledger, before validation
            ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
            projection,
            privacy,
            slots::<String>(privacy.checked_add(sources)?)?,
            map::<&str, &InvestigationCitation>(facts.checked_add(sources)?)?,
            set::<&String>(sources)?,
            slots::<PayloadSpan>(sources)?,
            MAX_MANIFEST_BYTES,
            policy.limits().evidence_bytes,
            json_bytes(16 * 1024)?.checked_mul(2)?,
            record_decode_scratch_bytes()?,
            // Base prompt may exceed the final cap. Later builders retain the
            // accepted base while adding contract/metadata/progress and details.
            json_bytes(base_prompt_wire_bytes()?)?,
            json_bytes(MAX_INPUT_BYTES)?.checked_mul(3)?,
            json_bytes(policy.envelope_bytes())?.checked_mul(3)?,
            json_bytes(MAX_MANIFEST_BYTES)?, // validated completion metadata
            json_bytes(details)?.checked_mul(2)?, // detail DTOs and JSON conversion
            json_bytes(MAX_QUERY_BYTES)?,
            next_prompt_validation_workspace()?,
            super::super::task_validation_workspace::envelope_workspace(policy)?,
            MAX_INPUT_BYTES.checked_mul(2)?, // prompt/full-payload bounded encodings
        ])
    }

    /// Logical owners retained by a successfully constructed input. This does
    /// not bound rejected construction, action clones, firewall preparation,
    /// disclosure sealing, allocator capacity or RSS.
    pub fn retained_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        let reads = usize::try_from(policy.limits().tool_actions).ok()?;
        // Base construction admits both privacy vectors together. Coverage
        // details are added afterwards: collect_strings copies decoded string
        // leaves/property keys, and supplied_strings keeps inventory_json too.
        // One JSON byte can conservatively fund one additional String slot;
        // the complete raw inventory is one further slot per coverage read.
        let inventory_bytes = reads.checked_mul(MAX_QUERY_BYTES)?;
        let privacy_bytes = MAX_INPUT_BYTES.checked_add(inventory_bytes.checked_mul(2)?)?;
        let privacy_slots =
            MAX_PRIVACY_STRINGS.checked_add(reads.checked_mul(MAX_QUERY_BYTES.checked_add(1)?)?)?;
        sum(&[
            size_of::<Self>(),
            json_bytes(MAX_ACTION_BYTES)?, // cached, final bounded acquisition choices
            acquisition_choices::AcquisitionChoices::private_workspace_bytes(policy)?,
            json_bytes(staged_finish::RefusalFeedback::max_bytes_with_policy(policy).ok()?)?,
            128,                                  // cached feedback action/choices hashes
            json_bytes(MAX_MANIFEST_BYTES)?,      // validated owned ledger
            json_bytes(MAX_INPUT_BYTES)?,         // complete accepted payload wire
            json_bytes(policy.envelope_bytes())?, // normalized private targets
            privacy_bytes,
            slots::<String>(privacy_slots)?,
        ])
    }
}

pub(super) fn base_prompt_wire_bytes() -> Option<usize> {
    // Compact context has already passed its own cap; legacy pages have their
    // aggregate cap. Ledger/citation/public-proof copies are charged separately.
    // An additional full input allowance covers fixed protocol text/frames.
    sum(&[
        MAX_INPUT_BYTES,
        MAX_INPUT_BYTES,
        MAX_TOTAL_QUERY_BYTES,
        MAX_MANIFEST_BYTES.checked_mul(3)?,
        MAX_QUESTION_BYTES.checked_mul(6)?,
        16 * 1024,
    ])
}

#[cfg(test)]
impl InvestigationInput {
    pub(in crate::investigation) fn assert_retained_workspace(&self, policy: &InvestigationPolicy) {
        use context_hub::logical::count;
        let strings = self.supplied_strings.iter().chain(&self.graph_strings);
        let observed = sum(&[
            size_of::<Self>(),
            json_bytes(count(&self.ledger).unwrap()).unwrap(),
            json_bytes(count(&self.payload).unwrap()).unwrap(),
            json_bytes(count(&self.inspection_requirements).unwrap()).unwrap(),
            json_bytes(count(&self.acquisition_choices.as_deref()).unwrap()).unwrap(),
            self.acquisition_choices
                .as_ref()
                .map_or(0, |choices| choices.private_bytes()),
            json_bytes(count(&self.action_feedback).unwrap()).unwrap(),
            json_bytes(count(&self.citation_correction).unwrap()).unwrap(),
            strings.clone().map(String::len).sum(),
            slots::<String>(strings.count()).unwrap(),
        ])
        .unwrap();
        assert!(Self::retained_workspace_bytes(policy).unwrap() >= observed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0327: use real input construction and its private retained owners.
    // This checks the named logical currency, not process allocations.
    #[test]
    fn input_retained_workspace_covers_real_source_and_graph_privacy_owners() {
        for role in [
            SpecialistId::DomainAnalystV3,
            SpecialistId::EvidenceAuditorV3,
        ] {
            let input = crate::investigation::tests::sealing_tests::input(role);
            assert!(!input.supplied_strings.is_empty());
            assert!(!input.graph_strings.is_empty());
            input.assert_retained_workspace(&INVESTIGATION_V1_POLICY);
        }
        // The host uses the opt-in floor even for older registered families.
        assert!(
            InvestigationInput::retained_workspace_bytes(&INVESTIGATION_V2_POLICY).unwrap()
                >= InvestigationInput::retained_workspace_bytes(&INVESTIGATION_V1_POLICY).unwrap()
        );
    }
}
