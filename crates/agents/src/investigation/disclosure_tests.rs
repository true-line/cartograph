use super::*;
use crate::TaskRangeRole;

pub(super) fn basis_with_source() -> InvestigationInputLedger {
    let mut basis = ledger();
    let selected = &basis.selected_facts[0];
    basis.citations.push(InvestigationCitation {
        citation_id: "evidence-1".into(),
        fact: selected.fact.clone(),
        fact_digest: selected.fact_digest.clone(),
        source: Some(core_prov::EvidenceRef {
            repo: "fixture/repo".into(),
            path: "guard.ts".into(),
            byte_start: 0,
            byte_end: 12,
            commit_sha: "pinned-revision".into(),
        }),
        role: Some(TaskRangeRole::Provenance),
        index: Some(0),
        text_hash: Some(content_hash(b"private text")),
        origin: InvestigationEvidenceOrigin::WorkingTreeUnverified,
    });
    basis.validate().unwrap();
    basis
}

fn supplied(text: &[u8]) -> DisclosureRepresentation {
    DisclosureRepresentation::Supplied {
        representation_hash: content_hash(text),
    }
}

fn manifest(basis: &InvestigationInputLedger) -> InvestigationDisclosure {
    InvestigationDisclosure {
        coverage_details: vec![],
        coverage: None,
        inspection_requirements_hash: None,
        schema_version: 1,
        policy: COMPACT_DISCLOSURE_POLICY.into(),
        input_ledger_hash: basis.fingerprint().unwrap(),
        graph_snapshot_id: basis.graph_snapshot_id.clone(),
        scope_snapshot_id: basis.scope_snapshot_id.clone(),
        payload_hash: content_hash(b"actual-redacted-payload"),
        facts: basis
            .selected_facts
            .iter()
            .map(|selected| DisclosedInvestigationFact {
                fact: selected.fact.clone(),
                representation: supplied(b"fact-representation"),
            })
            .collect(),
        queries: basis
            .queries
            .iter()
            .enumerate()
            .map(|(query_index, _)| DisclosedInvestigationQuery {
                query_index,
                representation: supplied(b"query-summary"),
            })
            .collect(),
        sources: vec![DisclosedInvestigationSource {
            citation_id: "evidence-1".into(),
            representation: supplied(b"complete-source-span-representation"),
            disclosed_text_hash: Some(content_hash(b"private text")),
            redacted: false,
        }],
        history: None,
    }
}

#[test]
fn disclosure_metadata_roundtrip_preserves_acquisition_identity_without_source_text() {
    // AC-0234: a separate disclosure records presentation without rewriting the
    // acquisition basis or persisting its raw text.
    let basis = basis_with_source();
    let original_bytes = serde_json::to_string(&basis).unwrap();
    let original_id = basis.fingerprint().unwrap();
    let disclosure = manifest(&basis);
    let body = serde_json::to_string(&disclosure).unwrap();
    let reopened: InvestigationDisclosure = decode_record(&body).unwrap();
    assert_eq!(
        reopened.fingerprint(&basis).unwrap(),
        disclosure.fingerprint(&basis).unwrap()
    );
    assert!(!body.contains("private text"));
    assert_eq!(serde_json::to_string(&basis).unwrap(), original_bytes);
    assert_eq!(basis.fingerprint().unwrap(), original_id);
    let mut changed = basis.clone();
    changed.revision += 1;
    assert_eq!(
        reopened.validate(&changed),
        Err(InvestigationError::InvalidInput)
    );
}

#[test]
fn disclosure_source_eligibility_requires_supplied_bytes_not_host_membership() {
    // AC-0235: a known source citation is not eligible merely because it is in
    // the host ledger. Graph metadata remains independently available.
    let basis = basis_with_source();
    let mut disclosure = manifest(&basis);
    assert!(
        disclosure
            .eligible_citations(&basis)
            .unwrap()
            .contains("evidence-1")
    );
    disclosure.sources[0].representation = DisclosureRepresentation::Omitted {
        reason: DisclosureOmissionReason::NotSelected,
    };
    disclosure.sources[0].disclosed_text_hash = None;
    assert_eq!(
        disclosure.eligible_citations(&basis).unwrap(),
        BTreeSet::from(["fact-1".into()])
    );
    disclosure.facts[0].representation = DisclosureRepresentation::Omitted {
        reason: DisclosureOmissionReason::WorkingSetBudget,
    };
    assert!(disclosure.eligible_citations(&basis).unwrap().is_empty());
    disclosure.sources[0].disclosed_text_hash = Some(content_hash(b"private text"));
    assert_eq!(
        disclosure.validate(&basis),
        Err(InvestigationError::InvalidInput)
    );
}

#[test]
fn disclosure_rejects_incomplete_accounting_invented_handles_and_unknown_shapes() {
    // AC-0234/0235: every acquired item must have a supplied or omitted entry;
    // missing fields and unknown versions cannot become a permissive legacy path.
    let basis = basis_with_source();
    let valid = manifest(&basis);
    let mut changed = valid.clone();
    changed.sources.clear();
    assert!(changed.validate(&basis).is_err());
    changed = valid.clone();
    changed.facts.clear();
    assert!(changed.validate(&basis).is_err());
    changed = valid.clone();
    changed.sources[0].citation_id = "invented".into();
    assert!(changed.validate(&basis).is_err());
    changed = valid.clone();
    changed.queries[0].query_index = 1;
    assert!(changed.validate(&basis).is_err());
    changed = valid.clone();
    changed.schema_version = 2;
    assert!(changed.validate(&basis).is_err());
    let mut raw = serde_json::to_value(valid).unwrap();
    raw["hidden_source"] = json!("must not be accepted");
    assert!(decode_record::<InvestigationDisclosure>(&raw.to_string()).is_err());
}

#[test]
fn disclosure_binds_redacted_text_and_actual_payload_separately_from_original_source() {
    // AC-0234: redaction changes the representation actually seen. It must not
    // inherit the original text hash or silently become captured verification.
    let basis = basis_with_source();
    let mut disclosure = manifest(&basis);
    let original = disclosure.fingerprint(&basis).unwrap();
    disclosure.sources[0].disclosed_text_hash = Some(content_hash(b"[REDACTED]"));
    assert!(disclosure.validate(&basis).is_err());
    disclosure.sources[0].redacted = true;
    assert_ne!(disclosure.fingerprint(&basis).unwrap(), original);
    let redacted = disclosure.fingerprint(&basis).unwrap();
    disclosure.payload_hash = content_hash(b"different-final-payload");
    assert_ne!(disclosure.fingerprint(&basis).unwrap(), redacted);
    assert!(matches!(
        basis.citations[1].origin,
        InvestigationEvidenceOrigin::WorkingTreeUnverified
    ));
}

#[test]
fn disclosure_rejects_reserved_duplicate_omission_in_every_representation() {
    // AC-0234: shared compact entries remain supplied, never omitted duplicates.
    let mut basis = basis_with_source();
    basis.supplied_history_hash = Some(content_hash(b"saved-history"));
    basis.supplied_history_bytes = b"saved-history".len();
    let mut valid = manifest(&basis);
    valid.sources[0].representation = DisclosureRepresentation::Omitted {
        reason: DisclosureOmissionReason::NotSelected,
    };
    valid.sources[0].disclosed_text_hash = None;
    valid.history = Some(supplied(b"history-representation"));
    valid.validate(&basis).unwrap();
    for location in 0..4 {
        let mut changed = valid.clone();
        let representation = match location {
            0 => &mut changed.facts[0].representation,
            1 => &mut changed.queries[0].representation,
            2 => &mut changed.sources[0].representation,
            _ => changed.history.as_mut().unwrap(),
        };
        *representation = DisclosureRepresentation::Omitted {
            reason: DisclosureOmissionReason::DuplicateRepresentation,
        };
        assert_eq!(
            changed.validate(&basis),
            Err(InvestigationError::InvalidInput)
        );
    }
}
