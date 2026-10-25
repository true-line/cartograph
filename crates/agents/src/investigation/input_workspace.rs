//! Retained completed input, separate from construction and sealing peaks.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

impl InvestigationInput {
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
            json_bytes(MAX_MANIFEST_BYTES)?, // validated owned ledger
            json_bytes(MAX_INPUT_BYTES)?,    // complete accepted payload wire
            json_bytes(policy.envelope_bytes())?, // normalized private targets
            privacy_bytes,
            slots::<String>(privacy_slots)?,
        ])
    }
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
