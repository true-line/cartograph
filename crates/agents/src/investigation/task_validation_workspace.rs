//! Pure @10 contract-validation owners; not decoding, store replay or RSS.
use super::*;
use context_hub::logical::{count, json_bytes, map, slots, sum};
use std::{collections::BTreeMap, mem::size_of};

/// Additional logical workspace while validating a borrowed @10 task. Fixed
/// catalog construction is measured from compiled prompts, never caller text.
/// Task ownership, bounded row decoding, saved-call/ledger/result replay and
/// host encodings are separate. Regex/native allocations are not modeled.
pub fn next_prompt_validation_workspace() -> Option<usize> {
    let definition = definition_workspace(SpecialistId::DomainAnalystV16)?
        .max(definition_workspace(SpecialistId::EvidenceAuditorV11)?);
    let text = core_redact::direct_source_json_content_bound(MAX_QUESTION_BYTES.max(2048))?;
    // redact_text retains the prior output during each replacement pass.
    let redaction = sum(&[text, text, slots::<String>(2)?])?;
    // Sum rather than reuse owners across condition temporaries. The envelope
    // component also includes normalization retained by an outer comparison.
    sum(&[
        definition,
        redaction,
        envelope_workspace(&INVESTIGATION_V2_POLICY)?,
    ])
}

pub(super) fn definition_workspace(role: SpecialistId) -> Option<usize> {
    let prompt = role.prompt();
    let definition = role.definition().ok()?;
    let wire = count(&(role, definition.version, &prompt))?;
    if wire > 16 * 1024 {
        return None;
    }
    // Original/policy common, original/policy discovery, two finishing common
    // replacements and final formatted prompt. Each is shorter than the final
    // prompt. Spec Writer @2's extra replacement/append consumes its temporary;
    // retained pre-message common, message common, discovery and final prompt
    // stay within seven owners. @13 and @14 each add one formatted prompt
    // while retaining their preceding version. No resource ceiling is increased.
    // @15 adds two replacement outputs, with @14 shorter than its result.
    // @16 retains @15 while adding its visible-schema instruction.
    // Account all twelve even though some lifetimes do not overlap.
    sum(&[
        prompt.len().checked_mul(12)?,
        slots::<String>(12)?,
        wire,
        wire,
        b"cartograph:specialist-definition:v1\0".len(),
        slots::<Vec<u8>>(2)?,
        json_bytes(count(&definition)?)?,
        size_of::<InvestigationSpecialist>(),
    ])
}

pub(super) fn envelope_workspace(policy: &InvestigationPolicy) -> Option<usize> {
    let n = policy.max_completion_obligations();
    let bytes = policy.envelope_bytes();
    // Normalization admits raw bytes before map construction or cloning. Two
    // normalized envelopes cover a comparison plus nested fingerprinting.
    let retained = sum(&[json_bytes(bytes)?, size_of::<ObligationEnvelope>()])?;
    let indexes = sum(&[
        slots::<BTreeMap<(&str, &str), &Obligation>>(1)?,
        map::<(&str, &str), &Obligation>(n)?,
        slots::<BTreeMap<Vec<u8>, &String>>(1)?,
        map::<Vec<u8>, &String>(n)?,
        n.checked_mul(4096)?,
        slots::<BTreeMap<(&String, &String), &String>>(1)?,
        map::<(&String, &String), &String>(n)?,
    ])?;
    // Source-target authentication first admits 8 KiB, then clones and clears
    // target_id. Per-obligation encoding retains its historical request cap.
    let target = sum(&[
        json_bytes(8 * 1024)?,
        size_of::<InvestigationInspectionTarget>(),
    ])?;
    let wire = bytes.max(COMPLETION_REQUEST_BYTES).max(8 * 1024);
    let domain = [
        b"cartograph:completion-obligations:v2\0".len(),
        b"cartograph:inspection-target:v1\0".len(),
        b"cartograph:source-obligation:v1\0".len(),
        b"cartograph:inventory-obligation:v1\0".len(),
    ]
    .into_iter()
    .max()?;
    sum(&[
        retained.checked_mul(2)?,
        indexes,
        target,
        wire,
        wire,
        domain,
        slots::<Vec<u8>>(2)?,
        128,
        slots::<String>(2)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0327: legacy parent validation constructs historical definitions too.
    // Pin the definition component independently, not the much larger total.
    #[test]
    fn saved_family_definitions_fit_admitted_definition_workspace() {
        let admitted = definition_workspace(SpecialistId::DomainAnalystV16)
            .unwrap()
            .max(definition_workspace(SpecialistId::EvidenceAuditorV11).unwrap());
        for role in [
            SpecialistId::DomainAnalystV1,
            SpecialistId::EvidenceAuditorV1,
            SpecialistId::DomainAnalyst,
            SpecialistId::EvidenceAuditor,
            SpecialistId::DomainAnalystV3,
            SpecialistId::EvidenceAuditorV3,
            SpecialistId::DomainAnalystV4,
            SpecialistId::EvidenceAuditorV4,
            SpecialistId::DomainAnalystV5,
            SpecialistId::EvidenceAuditorV5,
            SpecialistId::DomainAnalystV6,
            SpecialistId::EvidenceAuditorV6,
            SpecialistId::DomainAnalystV7,
            SpecialistId::EvidenceAuditorV7,
            SpecialistId::DomainAnalystV8,
            SpecialistId::EvidenceAuditorV8,
            SpecialistId::DomainAnalystV9,
            SpecialistId::EvidenceAuditorV9,
            SpecialistId::DomainAnalystV10,
            SpecialistId::EvidenceAuditorV10,
            SpecialistId::DomainAnalystV11,
            SpecialistId::DomainAnalystV12,
            SpecialistId::DomainAnalystV13,
            SpecialistId::DomainAnalystV14,
            SpecialistId::DomainAnalystV15,
            SpecialistId::DomainAnalystV16,
            SpecialistId::EvidenceAuditorV11,
            SpecialistId::SpecWriterV1,
            SpecialistId::SpecWriterV2,
            SpecialistId::GapTriagerV1,
            SpecialistId::GapTriagerV2,
        ] {
            assert!(definition_workspace(role).unwrap() <= admitted, "{role:?}");
        }
    }

    // AC-0327: actual compiled definitions, redaction and mixed normalization;
    // this checks component currency, not native allocation or full admission.
    #[test]
    fn next_prompt_validation_workspace_covers_real_primitives() {
        let bound = next_prompt_validation_workspace().unwrap();
        for role in [
            SpecialistId::DomainAnalystV10,
            SpecialistId::EvidenceAuditorV10,
            SpecialistId::DomainAnalystV11,
            SpecialistId::DomainAnalystV12,
            SpecialistId::DomainAnalystV13,
            SpecialistId::DomainAnalystV14,
            SpecialistId::DomainAnalystV15,
            SpecialistId::DomainAnalystV16,
            SpecialistId::EvidenceAuditorV11,
        ] {
            let definition = role.definition().unwrap();
            assert!(matches!(definition.version, 10..=16));
            assert!(
                definition_workspace(role).unwrap()
                    >= role.prompt().len() + count(&definition).unwrap()
            );
        }
        let text = "secret=abcd";
        let redacted = redacted_history_text(text, 2048).unwrap();
        assert_ne!(redacted, text);
        assert!(bound >= text.len() + redacted.len());
        assert!(matches!(
            redacted_history_text(text, text.len()),
            Err(InvestigationError::LimitExceeded)
        ));
        let target = super::super::tests::inspection_tests::fixture().0;
        let mut source = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        };
        let id = source.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut source {
            *obligation_id = id;
        }
        let mut inventory = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: format!("context-v2:{}", "b".repeat(64)),
                repo: "example/app".into(),
                inventory_hash: "a".repeat(64),
            },
        };
        let id = inventory.content_id().unwrap();
        if let Obligation::Inventory { obligation_id, .. } = &mut inventory {
            *obligation_id = id;
        }
        let envelope = ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![source.clone(), inventory, source],
        };
        let policy = &INVESTIGATION_V2_POLICY;
        let normalized = envelope.normalized_with_policy(policy).unwrap();
        assert_eq!(normalized.obligations.len(), 2);
        assert_eq!(
            envelope.fingerprint_with_policy(policy).unwrap(),
            normalized.fingerprint_with_policy(policy).unwrap()
        );
        assert!(
            envelope_workspace(policy).unwrap()
                >= count(&envelope).unwrap() + count(&normalized).unwrap() * 2
        );
        let mut rejected = envelope;
        rejected.obligations =
            vec![rejected.obligations[0].clone(); policy.max_completion_obligations() + 1];
        assert!(rejected.normalized_with_policy(policy).is_err());
    }
}
