//! AC-0287: actual retained capture, real reserve/charge, actual sealed prompt,
//! authenticated settlement and installation. No provider dispatch occurs.
use super::*;
use crate::investigations::evidence::prepared_source::publication::{
    SourcePublicationError, SourcePublicationInput,
};
use crate::jobs::investigations::CapturedSourceInvocationFixture;

fn publication<'a>(
    saved: &'a CapturedSourceInvocationFixture,
    firewall: &'a llm::EgressFirewall,
    provider: &'a dyn llm::LlmProvider,
) -> SourcePublicationInput<'a> {
    SourcePublicationInput {
        request: saved.request(),
        current: saved.current(),
        current_prepared: saved.prepared(),
        proof: saved.proof(),
        supplied_history: &[],
        firewall,
        provider,
    }
}
pub(super) fn provider() -> llm::OllamaProvider {
    llm::OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        std::time::Duration::from_secs(1),
    )
    .unwrap()
}
fn read(
    saved: &CapturedSourceInvocationFixture,
    state: &AppState,
    context: &FrozenContext,
    target: &InvestigationInspectionTarget,
) -> Result<PreparedSourceCandidate, PreparedSourceFailure> {
    let prepared = context.prepare_captured_source(state, target).unwrap();
    let mut admission = saved.admission();
    if prepared
        .size_binding()
        .sizes_for(target, &context.ledger)
        .unwrap()
        .is_some()
    {
        // A measured proof without the independently held receipt is rejected
        // before reservation commit, charging or text access.
        assert!(
            prepared
                .reserve(
                    context,
                    state,
                    saved.execution(),
                    saved.revision(),
                    &clock(),
                    &admission
                )
                .is_err()
        );
        assert_eq!(saved.counts(), (0, 0, 0));
        assert_eq!(read_count(), 0);
    }
    admission.size_binding = Some(prepared.size_binding());
    let reserved = prepared
        .reserve(
            context,
            state,
            saved.execution(),
            saved.revision(),
            &clock(),
            &admission,
        )
        .unwrap();
    prepared.read_reserved(context, state, saved.execution(), &reserved.basis())
}
#[test]
fn prepared_source_publication_acquired_installs_actual_read_only_after_ack() {
    assert_acquired_publication(false);
}
// AC-0487: execute the production attachment callsite, not a manual test attach.
#[test]
fn prepared_source_publication_gated_source_uses_attached_candidate() {
    assert_acquired_publication(true);
}
// AC-0565: native captured-source read/publication for the new role, no model.
#[test]
fn prepared_source_publication_analyst19_source_uses_attached_candidate() {
    assert_acquired_publication_role(Some(SpecialistId::DomainAnalystV19));
}
fn assert_acquired_publication(gated: bool) {
    assert_acquired_publication_role(gated.then_some(SpecialistId::DomainAnalystV18));
}
fn assert_acquired_publication_role(role: Option<SpecialistId>) {
    assert_acquired_publication_capture(role, role == Some(SpecialistId::DomainAnalystV24));
}
fn assert_acquired_publication_capture(role: Option<SpecialistId>, measured: bool) {
    let gated = role.is_some();
    let (saved, state, mut context, target, _) = fixture_for_role_capture(role, measured);
    reset_reads();
    let candidate = match read(&saved, &state, &context, &target) {
        Ok(candidate) => candidate,
        Err(error) => panic!("read failed: {:?}", error.kind()),
    };
    assert_eq!(read_count(), 1);
    assert!(context.evidence.is_empty());
    let expected_ledger = candidate.ledger().clone();
    let expected_text = candidate.evidence().text.clone();
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let published = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Ok(result) => result,
        Err(error) => panic!("publication failed: {}", error.error()),
    };
    assert_eq!(context.ledger, expected_ledger);
    assert_eq!(context.evidence.len(), 1);
    assert_eq!(context.evidence[0].text, expected_text);
    assert_eq!(published.input.ledger(), &context.ledger);
    assert!(
        published
            .input
            .seal_disclosure_with_policy(&published.prepared, &context.policy)
            .unwrap()
            .is_some()
    );
    let admission = published
        .input
        .seal_disclosure_with_policy(&published.prepared, &context.policy)
        .unwrap()
        .unwrap()
        .admission
        .unwrap();
    assert_eq!(admission.acquisition.is_some(), gated);
    assert_eq!(published.detail.usage.evidence_items, 1);
    assert_eq!(
        saved.publication_after_reopen(),
        (context.ledger.revision, 1, 1)
    );
    assert_eq!(read_count(), 1);
}
#[test]
fn prepared_source_publication_true_unavailable_adds_attempt_without_acquisition() {
    let (saved, state, mut context, target, private) = fixture();
    rusqlite::Connection::open(private.join("retained-source/captures.sqlite"))
        .unwrap()
        .execute("DELETE FROM objects", [])
        .unwrap();
    reset_reads();
    let base = context.ledger.clone();
    let failure = match read(&saved, &state, &context, &target) {
        Err(failure) => failure,
        Ok(_) => panic!("missing object acquired"),
    };
    assert_eq!(failure.kind(), SourceReadFailureKind::ReadUnavailable);
    assert_eq!(read_count(), 1);
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let published = match failure.publish_unavailable(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Ok(result) => result,
        Err(error) => panic!("unavailable failed: {}", error.error()),
    };
    assert_eq!(context.ledger, base);
    assert!(context.evidence.is_empty());
    assert_eq!(published.input.ledger(), &base);
    assert_eq!(published.detail.usage.evidence_items, 0);
    assert_eq!(saved.publication_after_reopen(), (base.revision, 0, 1));
    assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
    assert_eq!(read_count(), 1);
}
#[test]
fn prepared_source_publication_cancel_or_retention_failure_keeps_known_read_unpublished() {
    for cancel in [false, true] {
        let (mut saved, state, mut context, target, private) = fixture();
        reset_reads();
        let candidate = match read(&saved, &state, &context, &target) {
            Ok(candidate) => candidate,
            Err(error) => panic!("read failed: {:?}", error.kind()),
        };
        let base = context.ledger.clone();
        if cancel {
            saved.cancel();
        } else {
            rusqlite::Connection::open(private.join("retained-source/captures.sqlite"))
                .unwrap()
                .execute("DELETE FROM captures", [])
                .unwrap();
        }
        let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
        let provider = provider();
        let failure = match candidate.publish_acquired(
            &mut context,
            &state,
            saved.execution(),
            &clock(),
            &publication(&saved, &firewall, &provider),
        ) {
            Err(error) => error,
            Ok(_) => panic!("invalidated known read published"),
        };
        assert_eq!(failure.stage(), SourceAccessStage::AccessStarted);
        assert!(failure.validation_receipt().is_some());
        assert!(
            matches!(
                failure.error(),
                SourcePublicationError::Store(InvestigationStoreError::Stale)
            ) == cancel
        );
        assert_eq!(context.ledger, base);
        assert!(context.evidence.is_empty());
        assert_eq!(read_count(), 1);
        assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
    }
}
#[test]
fn prepared_source_publication_rollback_and_unknown_ack_never_install_or_repeat_read() {
    for lost_ack in [false, true] {
        let (saved, state, mut context, target, _) = fixture();
        reset_reads();
        let candidate = match read(&saved, &state, &context, &target) {
            Ok(candidate) => candidate,
            Err(error) => panic!("read failed: {:?}", error.kind()),
        };
        let base = context.ledger.clone();
        let next = candidate.ledger().revision;
        crate::jobs::investigations::fail_next_source_publication(lost_ack);
        let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
        let provider = provider();
        let failure = match candidate.publish_acquired(
            &mut context,
            &state,
            saved.execution(),
            &clock(),
            &publication(&saved, &firewall, &provider),
        ) {
            Err(error) => error,
            Ok(_) => panic!("fault acknowledged publication"),
        };
        assert!(matches!(
            failure.error(),
            SourcePublicationError::Store(InvestigationStoreError::Storage)
        ));
        assert_eq!(failure.stage(), SourceAccessStage::AccessStarted);
        assert!(failure.validation_receipt().is_some());
        assert_eq!(context.ledger, base);
        assert!(context.evidence.is_empty());
        assert_eq!(read_count(), 1);
        assert_eq!(
            saved.publication_after_reopen(),
            if lost_ack {
                (next, 1, 1)
            } else {
                (base.revision, 0, 0)
            }
        );
        assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
        // The failure retains guards and charge but exposes no candidate/read
        // retry. Unknown acknowledgement is resolved by durable recovery.
    }
}

#[path = "prepared_source_cleanup_tests.rs"]
mod cleanup_tests;

// AC-0566: the activated role retains the real source-publication path.
#[test]
fn prepared_source_publication_analyst20_source_uses_attached_candidate() {
    assert_acquired_publication_role(Some(SpecialistId::DomainAnalystV20));
}

// AC-0287 / #1698: acquired evidence, real admission, schema5 and durable reopen.
#[test]
fn analyst23_configuration_claim_refusal_survives_captured_source_reopen() {
    let (mut saved, state, mut context, target, _) =
        fixture_for_role(Some(SpecialistId::DomainAnalystV23));
    let candidate = match read(&saved, &state, &context, &target) {
        Ok(candidate) => candidate,
        Err(error) => panic!("read failed: {:?}", error.kind()),
    };
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let published = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Ok(result) => result,
        Err(error) => panic!("publication failed: {}", error.error()),
    };
    let citation = &context.ledger.citations.last().unwrap().citation_id;
    saved.assert_configuration_refusal_after_capture(
        &published.input,
        &published.prepared,
        &provider,
        &firewall,
        citation,
    );
}

#[test]
fn analyst25_partial_survives_captured_source_reopen() {
    let (mut saved, state, mut context, target, _) =
        fixture_for_role_capture(Some(SpecialistId::DomainAnalystV25), true);
    let candidate = match read(&saved, &state, &context, &target) {
        Ok(candidate) => candidate,
        Err(error) => panic!("read failed: {:?}", error.kind()),
    };
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let _published = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Ok(result) => result,
        Err(error) => panic!("publication failed: {}", error.error()),
    };
    let citation = &context.ledger.citations.last().unwrap().citation_id;
    saved.assert_diagnostic_retention_invariant(
        &provider,
        &firewall,
        citation,
        &context.query_pages,
        &context.evidence,
    );
}

#[test]
fn analyst26_partial_survives_captured_source_reopen() {
    let (mut saved, state, mut context, target, _) =
        fixture_for_role_capture(Some(SpecialistId::DomainAnalystV26), true);
    let candidate = match read(&saved, &state, &context, &target) {
        Ok(candidate) => candidate,
        Err(error) => panic!("read failed: {:?}", error.kind()),
    };
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let _published = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Ok(result) => result,
        Err(error) => panic!("publication failed: {}", error.error()),
    };
    let citation = &context.ledger.citations.last().unwrap().citation_id;
    saved.assert_diagnostic_retention_invariant(
        &provider,
        &firewall,
        citation,
        &context.query_pages,
        &context.evidence,
    );
}

// AC-0287: measured proof crosses real reservation, charged read, actual prompt
// publication and durable reopen; the model transport is never called.
#[test]
fn prepared_source_publication_analyst24_measured_receipt_roundtrip() {
    assert_acquired_publication_role(Some(SpecialistId::DomainAnalystV24));
}

// AC-0287: old receipts remain readable under conservative legacy certification.
#[test]
fn prepared_source_publication_analyst24_unmeasured_receipt_uses_legacy_bound() {
    assert_acquired_publication_capture(Some(SpecialistId::DomainAnalystV24), false);
}

// #2431: an @29 patch the merge rejects goes back to the model as
// invalid_action within the run's allowance; the third is terminal.
#[test]
fn analyst29_rejected_patch_retries_within_invalid_action_allowance() {
    for case in ["retry", "retry_cap"] {
        let (saved, state, mut context, target, _) =
            fixture_for_role_capture(Some(SpecialistId::DomainAnalystV29), true);
        reset_reads();
        eprintln!("patch retry case: {case}");
        let candidate = read(&saved, &state, &context, &target)
            .unwrap_or_else(|e| panic!("read failed: {:?}", e.kind()));
        let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
        let provider = provider();
        let _published = candidate
            .publish_acquired(
                &mut context,
                &state,
                saved.execution(),
                &clock(),
                &publication(&saved, &firewall, &provider),
            )
            .unwrap_or_else(|e| panic!("publication failed: {}", e.error()));
        let citation = &context.ledger.citations.last().unwrap().citation_id;
        saved.assert_volatile_patch_after_capture(
            &provider,
            &firewall,
            citation,
            &context.query_pages,
            &context.evidence,
            case,
        );
    }
}

// AC-0572: every case starts fresh and invokes no model or provider transport.
#[test]
fn analyst27_volatile_patch_admission_and_reopen() {
    for case in [
        "replace",
        "withdraw",
        "withhold",
        "duplicate",
        "unflagged",
        "all_withdrawn",
        "tool",
        "binding_mismatch",
        "missing_marker",
        "restart",
        "cancel",
        "audit_replace",
        "fallback",
        "fallback_missing",
        "fallback_hash",
        "fallback_swapped",
        "retry_refused",
    ] {
        let (saved, state, mut context, target, _) =
            fixture_for_role_capture(Some(SpecialistId::DomainAnalystV27), true);
        reset_reads();
        eprintln!("patch fixture case: {case}");
        let candidate = read(&saved, &state, &context, &target)
            .unwrap_or_else(|e| panic!("read failed: {:?}", e.kind()));
        let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
        let provider = provider();
        let _published = candidate
            .publish_acquired(
                &mut context,
                &state,
                saved.execution(),
                &clock(),
                &publication(&saved, &firewall, &provider),
            )
            .unwrap_or_else(|e| panic!("publication failed: {}", e.error()));
        let citation = &context.ledger.citations.last().unwrap().citation_id;
        saved.assert_volatile_patch_after_capture(
            &provider,
            &firewall,
            citation,
            &context.query_pages,
            &context.evidence,
            case,
        );
    }
}
