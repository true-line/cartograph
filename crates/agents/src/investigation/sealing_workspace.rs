//! Sealing logical owners, including construction before disclosure admission.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use llm::bounded::CompletionLimits;
use std::mem::size_of;

impl InvestigationInput {
    /// Unread-source representation certificate only: no source acquisition or
    /// provider execution. Descriptor validation caps its wire before copying.
    pub fn source_preview_workspace_bytes(
        policy: &InvestigationPolicy,
        limits: &CompletionLimits,
    ) -> Option<usize> {
        let descriptor: usize = 8 * 1024; // InvestigationInspectionTarget::content_id
        // Citation origin/handle, completed-action descriptor and span metadata.
        // Four descriptor envelopes also cover their fixed tags and ordinal ID.
        let append = descriptor.checked_mul(4)?;
        let projected = super::admission_input::reserve_projection_bytes(policy)?
            .checked_add(append.checked_mul(20)?)?;
        let (disclosure, sealing) = Self::sealing_workspace_bytes(policy, limits)?;
        sum(&[
            Self::context_preview_workspace_bytes(policy, limits)?,
            acquisition_choices::AcquisitionChoices::candidate_workspace_bytes(policy)?,
            acquisition_choices::AcquisitionChoices::private_workspace_bytes(policy)?,
            sealing,
            disclosure.checked_mul(2)?, // actual receipt and sizing-only clone
            json_bytes(projected)?.checked_mul(2)?, // source reserve/mode clone
            json_bytes(append)?,
            json_bytes(MAX_MANIFEST_BYTES)?, // expected citation handles
            json_bytes(policy.envelope_bytes())?, // expected inspection progress
            slots::<&InvestigationCitation>(policy.limits().evidence_items)?,
            json_bytes(MAX_INPUT_BYTES)?.checked_mul(2)?, // protected-field parses
            MAX_DISCLOSURE_BYTES,                         // rejected sizing encoding
            slots::<UnreadSourcePreviewBound>(2)?,
            128,
        ])
    }

    /// Metadata-preview validation and synthetic continuation measurement.
    /// Current/candidate inputs and the original provider owners are borrowed;
    /// unread-source preview construction is a separate acquisition phase.
    pub fn context_preview_workspace_bytes(
        policy: &InvestigationPolicy,
        limits: &CompletionLimits,
    ) -> Option<usize> {
        let tree = json_bytes(MAX_INPUT_BYTES)?;
        // Two admission bases plus usage/metadata/progress comparisons. The
        // sealing component conservatively covers each nested basis validator.
        let validation = sum(&[
            Self::sealing_workspace_bytes(policy, limits)?
                .1
                .checked_mul(2)?,
            json_bytes(MAX_MANIFEST_BYTES)?, // cloned previous ledger in basis
            tree.checked_mul(5)?,            // delta DTOs and old inventory context
            json_bytes(MAX_QUERY_BYTES)?,    // admitted inventory response
        ])?;
        let reserved = super::admission_input::reserve_projection_bytes(policy)?;
        let measurement = sum(&[
            response_schema::workspace_bytes(policy)?,
            acquisition_choices::AuthenticatedCitationCorrection::projection_workspace_bytes(
                policy,
            )?, // finite typed-feedback sizing candidates
            json_bytes(reserved)?.checked_mul(2)?, // reserve and per-mode clone
            tree.checked_mul(3)?,                  // candidate/current/reserve action payloads
            slots::<CompletionAction>(3)?,
            3 * 128,
            json_bytes(MAX_MANIFEST_BYTES)?.checked_mul(2)?, // contract/metadata
            super::super::task_validation_workspace::envelope_workspace(policy)?,
            limits.firewall_workspace_bytes()?.prepare_peak_bytes,
            MAX_INPUT_BYTES.checked_mul(2)?, // basis canonical/domain wire
            slots::<Vec<u8>>(2)?,
        ])?;
        // Verification can hold the original certificate while recomputing it.
        sum(&[
            validation.max(measurement),
            slots::<ContextPreviewBound>(2)?,
            128,
        ])
    }

    /// Additional sealing workspace for a completed private input. Includes a
    /// caller action retained by metadata continuation and the sealer's action
    /// clone. Borrowed input/prepared payloads, input construction, certificates,
    /// allocator capacity and RSS are separate. Returns (disclosure, peak).
    pub fn sealing_workspace_bytes(
        policy: &InvestigationPolicy,
        limits: &CompletionLimits,
    ) -> Option<(usize, usize)> {
        let tree = json_bytes(MAX_INPUT_BYTES)?;
        let actions = sum(&[size_of::<CompletionAction>(), tree, 128])?.checked_mul(2)?;
        // Wrapper schema inspection; original/prepared trees in the sealer.
        // Protected fields borrow the latter pair, rather than parsing again.
        let prompts = tree.checked_mul(3)?;
        // Admission reserve basis and expected progress; contract, metadata and
        // feedback each own a cloned Value followed by a decoded DTO. Sum these
        // phases conservatively rather than relying on temporary drop timing.
        let admission = sum(&[
            response_schema::workspace_bytes(policy)?,
            acquisition_choices::AuthenticatedCitationCorrection::projection_workspace_bytes(
                policy,
            )?, // finite typed-feedback sizing candidates
            tree.checked_mul(2)?,
            tree.checked_mul(2)?, // contract clone / decoded contract
            tree.checked_mul(2)?, // metadata clone / decoded metadata
            tree.checked_mul(2)?, // feedback clone / decoded feedback
            super::super::task_validation_workspace::envelope_workspace(policy)?,
            next_prompt_validation_workspace()?, // fixed role prompt construction
        ])?;
        // Progressive original/actual contexts and coverage details each decode
        // cloned subtrees. Their source trees were admitted before these copies.
        let progressive = tree.checked_mul(4)?;
        let details = tree.checked_mul(4)?;
        // Selected-fact hashing builds a new representation tree and two
        // borrowed-reference vectors. Key comparisons decode two fact keys.
        let representation = sum(&[
            tree,
            tree.checked_mul(2)?,
            slots::<&Value>(MAX_INPUT_BYTES)?.checked_mul(2)?,
            MAX_INPUT_BYTES.checked_mul(2)?,
            slots::<Vec<u8>>(2)?,
        ])?;
        let disclosure = disclosure_construction(policy)?;
        let peak = sum(&[
            actions,
            prompts,
            admission,
            progressive,
            details,
            representation,
            disclosure,
            json_bytes(MAX_MANIFEST_BYTES)?, // public coverage qualification
            disclosure_validation_workspace(policy, MAX_MANIFEST_BYTES)?,
            // matches_action uses the same original-action hash workspace as
            // firewall preparation; that existing bound also covers rejection.
            limits.firewall_workspace_bytes()?.prepare_peak_bytes,
        ])?;
        Some((disclosure, peak))
    }
}

fn disclosure_construction(policy: &InvestigationPolicy) -> Option<usize> {
    let facts = policy.limits().selected_facts;
    let queries = usize::try_from(policy.limits().tool_actions).ok()?;
    let sources = facts.checked_add(policy.limits().evidence_items)?;
    let hashes = sum(&[
        facts,
        queries.checked_mul(2)?,
        sources.checked_mul(2)?,
        queries.checked_mul(2)?, // coverage-detail representation/body hashes
        queries.checked_mul(2)?, // artifact-page representation/body hashes
        12,
    ])?;
    // Do not use MAX_DISCLOSURE_BYTES here: the final validator enforces it
    // after construction. IDs/keys come from the admitted ledger; admission
    // state and the inspection hash can still occupy their entire prompt.
    sum(&[
        size_of::<InvestigationDisclosure>(),
        slots::<DisclosedInvestigationFact>(facts)?,
        slots::<DisclosedInvestigationQuery>(queries)?,
        slots::<DisclosedInvestigationSource>(sources)?,
        slots::<DisclosedInvestigationCoverageDetail>(queries)?,
        slots::<DisclosedInvestigationArtifactPage>(queries)?,
        slots::<String>(queries)?,          // query-history IDs
        MAX_MANIFEST_BYTES.checked_mul(2)?, // citation/key and basis/coverage IDs
        json_bytes(MAX_INPUT_BYTES)?,       // decoded admission state
        MAX_INPUT_BYTES,                    // inspection_requirements_hash before validation
        hashes.checked_mul(64)?,
        QUERY_HISTORY_DISCLOSURE_POLICY
            .len()
            .max(COMPACT_DISCLOSURE_POLICY.len()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::{EgressFirewall, EgressPolicy, OllamaProvider};

    // AC-0327: real firewall/sealing output, no network invocation. This checks
    // disclosed owners in the logical currency, not allocator measurements.
    #[test]
    fn sealing_workspace_covers_real_redacted_disclosure_and_policy_growth() {
        let provider = OllamaProvider::new(
            "http://127.0.0.1:11434",
            "fixture",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let limits = CompletionLimits::default();
        for role in [
            SpecialistId::DomainAnalystV3,
            SpecialistId::EvidenceAuditorV3,
        ] {
            let input = crate::investigation::tests::sealing_tests::input(role);
            let action = input.action("fixture:sealing".into()).unwrap();
            let prepared = EgressFirewall::new(EgressPolicy::default())
                .prepare_bounded(&provider, &action, &limits)
                .unwrap();
            let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
            assert!(disclosure.sources.iter().any(|source| source.redacted));
            let wire = context_hub::logical::count(&disclosure).unwrap();
            let (retained, peak) =
                InvestigationInput::sealing_workspace_bytes(&INVESTIGATION_V1_POLICY, &limits)
                    .unwrap();
            assert!(retained >= json_bytes(wire).unwrap());
            assert!(peak > retained);
            let larger =
                InvestigationInput::sealing_workspace_bytes(&INVESTIGATION_V2_POLICY, &limits)
                    .unwrap();
            assert!(larger.0 >= retained && larger.1 >= peak);
        }
    }
}
