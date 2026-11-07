//! AC-0287: real captured fixture -> saved Invocation -> host reserve -> durable
//! validation charge -> actual task_text. No scripted provider is dispatched.
use super::*;
use crate::jobs::investigations::{CapturedSourceInvocationFixture, SourceValidationInitialData};
use context_hub::coverage::{CoverageQualification, ScopedRepo};
use serde_json::json;

fn fixture() -> (
    CapturedSourceInvocationFixture,
    AppState,
    FrozenContext,
    InvestigationInspectionTarget,
    std::path::PathBuf,
) {
    fixture_for(false)
}
fn fixture_for(
    gated: bool,
) -> (
    CapturedSourceInvocationFixture,
    AppState,
    FrozenContext,
    InvestigationInspectionTarget,
    std::path::PathBuf,
) {
    fixture_for_role(gated.then_some(SpecialistId::DomainAnalystV18))
}
fn fixture_for_role(
    role: Option<SpecialistId>,
) -> (
    CapturedSourceInvocationFixture,
    AppState,
    FrozenContext,
    InvestigationInspectionTarget,
    std::path::PathBuf,
) {
    fixture_for_role_capture(
        role,
        role.is_some_and(SpecialistId::uses_exact_source_reservation),
    )
}
fn fixture_for_role_capture(
    role: Option<SpecialistId>,
    measured: bool,
) -> (
    CapturedSourceInvocationFixture,
    AppState,
    FrozenContext,
    InvestigationInspectionTarget,
    std::path::PathBuf,
) {
    let mut actual = None;
    let mut saved = CapturedSourceInvocationFixture::with_role(role, |private| {
        let state = Fixture::state_at_with_sizes(private, measured.then_some(false));
        let (mut context, target) = Fixture::context_for(&state);
        let qualification = CoverageQualification::new(
            &[ScopedRepo {
                repo_key: target.source.repo.clone(),
                registered_source_id: Some(target.registered_source_id.clone()),
                config_hash: "source-validation-fixture".into(),
                layer_hints: None,
            }],
            &[],
        )
        .unwrap();
        let proof = qualification
            .durable_proof(&context.ledger.graph_snapshot_id)
            .unwrap();
        context.ledger.schema_version = 2;
        context.ledger.coverage = Some(proof.clone());
        context.policy = std::sync::Arc::new(INVESTIGATION_V2_POLICY.clone());
        for (raw, manifest) in context
            .query_pages
            .iter_mut()
            .zip(&mut context.ledger.queries)
        {
            let mut page: serde_json::Value = serde_json::from_str(raw).unwrap();
            let response = page["context"].take();
            page["context"] = json!({"context_id":proof.context_id,"coverage":proof.qualification,"response":response});
            *raw = page.to_string();
            manifest.response_hash = content_hash(raw.as_bytes());
            manifest.response_bytes = raw.len();
        }
        let archive = InvestigationCoverageArchive::new(proof, vec![]).unwrap();
        context.coverage_archive = Some(archive.clone());
        context
            .ledger
            .validate_with_policy(&context.policy)
            .unwrap();
        let data = SourceValidationInitialData {
            ledger: context.ledger.clone(),
            query_pages: context.query_pages.clone(),
            archive,
            target: target.clone(),
        };
        actual = Some((state, context, target, private.to_owned()));
        data
    });
    let (state, context, target, private) = actual.unwrap();
    if role.is_some_and(SpecialistId::uses_exact_source_reservation) && measured {
        let source = context.prepare_captured_source(&state, &target).unwrap();
        let sizes = source
            .size_binding()
            .sizes_for(&target, &context.ledger)
            .unwrap()
            .unwrap();
        saved.use_measured_proof(sizes);
    }
    (saved, state, context, target, private)
}
fn clock() -> crate::investigations::runtime::RunClock {
    crate::investigations::runtime::RunClock::new(
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    )
}

#[test]
fn prepared_source_host_reservation_then_charge_then_real_source_read() {
    let (saved, state, context, target, _) = fixture();
    reset_reads();
    assert_eq!(saved.counts(), (0, 0, 0));
    let prepared = context.prepare_captured_source(&state, &target).unwrap();
    assert_eq!(read_count(), 0);
    let reservation = prepared
        .reserve(
            &context,
            &state,
            saved.execution(),
            saved.revision(),
            &clock(),
            &saved.admission(),
        )
        .unwrap();
    assert_eq!(read_count(), 0);
    assert_eq!(saved.counts(), (1, 0, 0));
    let candidate =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Ok(candidate) => candidate,
            Err(error) => panic!("reserved read failed: {:?}", error.kind()),
        };
    assert_eq!(read_count(), 1);
    assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
    let authored = "export function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n";
    assert_eq!(
        candidate.evidence().text,
        authored[target.source.byte_start as usize..target.source.byte_end as usize]
    );
    assert!(target.matches(candidate.ledger().citations.last().unwrap()));
    assert!(candidate.validation_receipt().is_some());
    candidate.revalidate(&context, &state).unwrap();
    assert!(context.evidence.is_empty());
    assert_eq!(
        context.ledger.citations.len() + 1,
        candidate.ledger().citations.len()
    );
}

#[cfg(unix)]
#[test]
fn prepared_source_retained_currency_covers_actual_reserved_read_owners() {
    // AC-0327: real captured receipt and durable charge, not fabricated DTOs.
    let (saved, state, context, target, private) = fixture();
    let limits = crate::mcp_reads::dispatch::EvidenceWorkLimits {
        max_file_validation_bytes: std::num::NonZeroU64::new(target.captured.file.byte_len)
            .unwrap(),
        max_span_bytes: std::num::NonZeroU64::new(
            target.captured.byte_end - target.captured.byte_start,
        )
        .unwrap(),
    };
    let prepared = context
        .prepare_captured_read(&state, &target.fact, target.role, target.index)
        .unwrap()
        .with_read_limits(Some(limits))
        .unwrap();
    let reservation = prepared
        .reserve(
            &context,
            &state,
            saved.execution(),
            saved.revision(),
            &clock(),
            &saved.admission(),
        )
        .unwrap();
    let candidate =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Ok(candidate) => candidate,
            Err(error) => panic!("reserved read failed: {:?}", error.kind()),
        };
    let validation = candidate.validation_receipt().unwrap();
    // Serialized fields are an independent payload lower bound, not a peak/RSS
    // measurement. The implementation separately accounts typed/map/guard roles.
    let payload = [
        serde_json::to_vec(&candidate.prepared.base_ledger)
            .unwrap()
            .len(),
        serde_json::to_vec(candidate.ledger()).unwrap().len(),
        serde_json::to_vec(&target).unwrap().len(),
        candidate.prepared.receipt.to_json().unwrap().len(),
        serde_json::to_vec(reservation.detail()).unwrap().len(),
        serde_json::to_vec(validation.detail()).unwrap().len(),
        serde_json::to_vec(candidate.evidence()).unwrap().len(),
    ]
    .into_iter()
    .sum::<usize>();
    let retained = crate::investigations::source_retained_workspace::retained_bytes(
        limits,
        private.as_os_str().as_encoded_bytes().len(),
    )
    .unwrap();
    assert!(retained >= payload);
    let construction = crate::investigations::source_candidate_workspace::workspace(
        serde_json::to_vec(&context.ledger).unwrap().len(),
        serde_json::to_vec(&target).unwrap().len(),
    )
    .unwrap();
    assert!(serde_json::to_vec(candidate.ledger()).unwrap().len() <= construction.candidate_wire);
    assert!(!candidate.prepared.selected.is_empty());
    assert!(!candidate.prepared.guards.guards.is_empty());
    assert_eq!(
        candidate.evidence().text.len() as u64,
        limits.max_span_bytes.get()
    );
    assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
    assert!(context.evidence.is_empty()); // candidate remains unpublished
}

#[test]
fn prepared_source_host_reservation_stale_and_actual_cancel_never_charge_or_read() {
    for cancel in [false, true] {
        let (mut saved, state, context, target, _) = fixture();
        let prepared = context.prepare_captured_source(&state, &target).unwrap();
        reset_reads();
        let revision = if cancel {
            saved.cancel()
        } else {
            saved.revision() + 1
        };
        let error = match prepared.reserve(
            &context,
            &state,
            saved.execution(),
            revision,
            &clock(),
            &saved.admission(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("stale/cancelled invocation reserved"),
        };
        match error {
            SourceReservationError::Store(error) => assert_eq!(
                error,
                if cancel {
                    InvestigationStoreError::Cancelled
                } else {
                    InvestigationStoreError::Stale
                }
            ),
            other => panic!("unexpected reservation error: {other}"),
        }
        assert_eq!(read_count(), 0);
        assert_eq!(saved.counts(), (0, 0, 0));
    }
}

#[test]
fn prepared_source_host_reservation_retained_capture_invalidation_never_reads() {
    let (saved, state, context, target, private) = fixture();
    let prepared = context.prepare_captured_source(&state, &target).unwrap();
    rusqlite::Connection::open(private.join("retained-source/captures.sqlite"))
        .unwrap()
        .execute("DELETE FROM captures", [])
        .unwrap();
    reset_reads();
    let result = prepared.reserve(
        &context,
        &state,
        saved.execution(),
        saved.revision(),
        &clock(),
        &saved.admission(),
    );
    assert!(matches!(
        result,
        Err(SourceReservationError::Host(HostError::SourceUnavailable))
    ));
    assert_eq!(read_count(), 0);
    assert_eq!(saved.counts(), (0, 0, 0));
}

#[test]
fn prepared_source_host_reservation_unknown_ack_keeps_reservation_without_charge_or_read() {
    let (saved, state, context, target, _) = fixture();
    let prepared = context.prepare_captured_source(&state, &target).unwrap();
    reset_reads();
    crate::jobs::investigations::lose_next_source_admission_ack();
    let result = prepared.reserve(
        &context,
        &state,
        saved.execution(),
        saved.revision(),
        &clock(),
        &saved.admission(),
    );
    assert!(matches!(
        result,
        Err(SourceReservationError::Store(
            InvestigationStoreError::Storage
        ))
    ));
    assert_eq!(read_count(), 0);
    assert_eq!(saved.counts(), (1, 0, 0));
    // No receipt was returned. A second admission cannot silently replay it.
    assert!(matches!(
        prepared.reserve(
            &context,
            &state,
            saved.execution(),
            saved.revision(),
            &clock(),
            &saved.admission()
        ),
        Err(SourceReservationError::Store(
            InvestigationStoreError::Stale
        ))
    ));
    assert_eq!(saved.counts(), (1, 0, 0));
}

#[cfg(unix)]
#[test]
fn prepared_source_host_reservation_retained_lock_substitution_never_reads() {
    use std::os::unix::fs::PermissionsExt;
    let (saved, state, context, target, private) = fixture();
    let prepared = context.prepare_captured_source(&state, &target).unwrap();
    let lock = private
        .join("retained-source/locks")
        .join(format!("{}.lock", target.registered_source_id));
    std::fs::rename(&lock, lock.with_extension("held")).unwrap();
    std::fs::write(&lock, b"").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    reset_reads();
    let result = prepared.reserve(
        &context,
        &state,
        saved.execution(),
        saved.revision(),
        &clock(),
        &saved.admission(),
    );
    assert!(matches!(
        result,
        Err(SourceReservationError::Host(HostError::InvalidInput))
    ));
    assert_eq!(read_count(), 0);
    assert_eq!(saved.counts(), (0, 0, 0));
}

#[path = "prepared_source_publication_tests.rs"]
mod publication_tests;

#[path = "prepared_source_membership_tests.rs"]
mod membership_tests;
