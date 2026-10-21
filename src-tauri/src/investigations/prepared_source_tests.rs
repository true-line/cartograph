use super::*;
use crate::investigations::AcquisitionSink;

#[derive(Default)]
struct Sink {
    usage: InvestigationUsage,
    validations: Vec<u64>,
    publications: usize,
}
impl AcquisitionSink for Sink {
    fn usage(&self) -> &InvestigationUsage {
        &self.usage
    }
    fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError> {
        self.validations.push(bytes);
        self.usage.captured_validation_bytes += bytes;
        Ok(())
    }
    fn publish_input(
        &mut self,
        _: &InvestigationInputLedger,
        usage: InvestigationUsage,
    ) -> Result<(), HostError> {
        self.publications += 1;
        self.usage = usage;
        Ok(())
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    private: std::path::PathBuf,
    state: AppState,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private = crate::paths::canonicalize(private).unwrap();
        let state = Self::state_at(&private);
        Self {
            _directory: directory,
            private,
            state,
        }
    }
    fn state_at(private: &std::path::Path) -> AppState {
        let root = private.join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("rule.ts"),"export function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n").unwrap();
        let state = crate::registered_source_tests::app_state(private);
        let source = state.sources.lock().unwrap().register_local(&root).unwrap();
        let input = state
            .primary_sources
            .prepare(&source, source.root(), &["client".into()])
            .unwrap();
        let (extraction, receipts) = adapters_lang_ts::captured::extract_file(
            input.capture.as_ref().unwrap().file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        state
            .primary_sources
            .persist(&input, &source, &receipts)
            .unwrap();
        let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
        crate::load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &extraction,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
            None,
        )
        .unwrap();
        drop(input);
        // Retained-read authority must not depend on a ready/current checkout.
        std::fs::remove_dir_all(root).unwrap();
        state
    }
    fn context(&self) -> (FrozenContext, InvestigationInspectionTarget) {
        Self::context_for(&self.state)
    }
    fn context_for(state: &AppState) -> (FrozenContext, InvestigationInspectionTarget) {
        let request = StartInvestigationRequest {
            schema_version: 1,
            request_nonce: "prepared-read".into(),
            specialist_id: SpecialistId::DomainAnalyst,
            inspection_requirements: None,
            question: "Inspect bounded source evidence.".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision: None,
            expected_context_id: None,
            conversation_id: None,
            parent_id: None,
        };
        let mut context = FrozenContext::prepare(state, &request, &[]).unwrap();
        let mut sink = Sink::default();
        context
            .query(
                state,
                InvestigationQuery {
                    scope: InvestigationScope::All,
                    kind: Some(InvestigationFactKind::Node),
                    labels: vec!["BusinessRule".into()],
                    max_facts: 1,
                    max_bytes: 32768,
                    cursor: None,
                },
                &mut sink,
            )
            .unwrap();
        let selected = &context.ledger.selected_facts[0];
        let (target, guard) = crate::investigations::inspection::resolve(
            state,
            &selected.fact,
            &context.ledger.graph_snapshot_id,
            &selected.binding.as_ref().unwrap().receipt_id,
            0,
        )
        .unwrap();
        drop(guard);
        (context, target)
    }
    fn mutate_objects(&self, sql: &str) {
        rusqlite::Connection::open(self.private.join("retained-source/captures.sqlite"))
            .unwrap()
            .execute(sql, [])
            .unwrap();
    }
    fn cannot_forget(&self, source: &str) {
        let preview = crate::primary_source::preview(&self.state, source).unwrap();
        assert!(crate::primary_source::forget(&self.state, source, &preview.fingerprint).is_err());
    }
}
fn read_count() -> usize {
    TEXT_READS.with(|count| count.get())
}
fn reset_reads() {
    TEXT_READS.with(|count| count.set(0));
}

// AC-0287: actual captured bytes and citation/ledger representation are identical
// to the old immediate wrapper; the candidate itself publishes nothing.
#[test]
fn prepared_source_candidate_matches_existing_read_without_publication() {
    let fixture = Fixture::new();
    let (context, target) = fixture.context();
    let (mut old, _) = fixture.context();
    reset_reads();
    let base = context.ledger.clone();
    let prepared = context
        .prepare_captured_source(&fixture.state, &target)
        .unwrap();
    assert_eq!(read_count(), 0);
    assert_eq!(prepared.base_ledger(), &base);
    assert_eq!(prepared.target(), &target);
    fixture.cannot_forget(&target.registered_source_id);
    let mut charges = vec![];
    let candidate = match prepared.read(&context, &fixture.state, |request| {
        assert_eq!(request.target, &target);
        assert_eq!(
            request.base_ledger_hash,
            base.fingerprint_with_policy(context.policy).unwrap()
        );
        charges.push(request.validation_bytes);
        Ok(())
    }) {
        Ok(candidate) => candidate,
        Err(_) => panic!("valid captured read failed"),
    };
    assert_eq!(read_count(), 1);
    assert_eq!(context.ledger, base);
    assert!(context.evidence.is_empty());
    assert_eq!(candidate.base_ledger(), &base);
    assert_eq!(candidate.target(), &target);
    candidate.revalidate(&context, &fixture.state).unwrap();
    let mut sink = Sink::default();
    old.read_evidence(
        &fixture.state,
        target.fact.clone(),
        target.role,
        target.index,
        &mut sink,
    )
    .unwrap();
    assert_eq!(charges, sink.validations);
    assert_eq!(sink.publications, 1);
    assert_eq!(
        serde_json::to_vec(candidate.ledger()).unwrap(),
        serde_json::to_vec(&old.ledger).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(candidate.evidence()).unwrap(),
        serde_json::to_vec(&old.evidence[0]).unwrap()
    );
    fixture.cannot_forget(&target.registered_source_id);
    drop(candidate);
    let preview =
        crate::primary_source::preview(&fixture.state, &target.registered_source_id).unwrap();
    assert!(
        crate::primary_source::forget(
            &fixture.state,
            &target.registered_source_id,
            &preview.fingerprint
        )
        .unwrap()
            > 0
    );
}

// AC-0287: metadata preparation and a failed durable-charge callback never read
// captured objects, even when the stored object is already invalid.
#[test]
fn prepared_source_charge_failure_never_accesses_text_and_retains_guards() {
    let fixture = Fixture::new();
    let (context, target) = fixture.context();
    fixture.mutate_objects("UPDATE objects SET bytes=X'00'");
    reset_reads();
    let prepared = context
        .prepare_captured_source(&fixture.state, &target)
        .unwrap();
    assert_eq!(read_count(), 0);
    let failure = match prepared.read(&context, &fixture.state, |_| Err(HostError::Operational)) {
        Err(error) => error,
        Ok(_) => panic!("charge failure accepted"),
    };
    assert_eq!(failure.stage(), SourceAccessStage::BeforeAccess);
    assert_eq!(
        failure.kind(),
        SourceReadFailureKind::BeforeAccess(HostError::Operational)
    );
    assert_eq!(failure.target(), &target);
    failure.revalidate(&context, &fixture.state).unwrap();
    assert_eq!(read_count(), 0);
    assert!(context.evidence.is_empty());
    fixture.cannot_forget(&target.registered_source_id);
    drop(failure);
    let preview =
        crate::primary_source::preview(&fixture.state, &target.registered_source_id).unwrap();
    assert!(
        crate::primary_source::forget(
            &fixture.state,
            &target.registered_source_id,
            &preview.fingerprint
        )
        .is_ok()
    );
}

// AC-0287: after task_text starts, unavailable and corrupt object outcomes are
// truthful known reads, never ReleasedBeforeRead or acquired evidence.
#[test]
fn prepared_source_access_started_failures_preserve_truth_and_guards() {
    for (sql, expected) in [
        (
            "DELETE FROM objects",
            SourceReadFailureKind::ReadUnavailable,
        ),
        (
            "UPDATE objects SET bytes=X'00'",
            SourceReadFailureKind::KnownReadFailure(HostError::InvalidInput),
        ),
    ] {
        let fixture = Fixture::new();
        let (context, target) = fixture.context();
        let base = context.ledger.clone();
        fixture.mutate_objects(sql);
        reset_reads();
        let prepared = context
            .prepare_captured_source(&fixture.state, &target)
            .unwrap();
        let mut charges = 0;
        let failure = match prepared.read(&context, &fixture.state, |_| {
            charges += 1;
            Ok(())
        }) {
            Err(error) => error,
            Ok(_) => panic!("invalid object acquired"),
        };
        assert_eq!(charges, 1);
        assert_eq!(read_count(), 1);
        assert_eq!(failure.stage(), SourceAccessStage::AccessStarted);
        assert_eq!(failure.kind(), expected);
        assert!(context.evidence.is_empty());
        assert_eq!(context.ledger, base);
        fixture.cannot_forget(&target.registered_source_id);
    }
}

// AC-0287: equal revisions do not authorize a changed frozen basis/policy. No
// callback/charge or source read may happen against the wrong base.
#[test]
fn prepared_source_rejects_equal_revision_changed_basis_and_policy_before_charge() {
    for policy_swap in [false, true] {
        let fixture = Fixture::new();
        let (mut context, target) = fixture.context();
        let prepared = context
            .prepare_captured_source(&fixture.state, &target)
            .unwrap();
        reset_reads();
        if policy_swap {
            context.policy = &INVESTIGATION_V2_POLICY;
        } else {
            context.ledger.supplied_history_hash = Some("a".repeat(64));
        }
        let failure =
            match prepared.read(&context, &fixture.state, |_| panic!("stale basis charged")) {
                Err(error) => error,
                Ok(_) => panic!("stale basis accepted"),
            };
        assert_eq!(
            failure.kind(),
            SourceReadFailureKind::BeforeAccess(HostError::InputChanged)
        );
        assert_eq!(read_count(), 0);
    }
}

// AC-0287: revalidation after a successful charge must still precede task_text;
// metadata disappearance is a pre-access failure, with no fabricated unavailable.
#[test]
fn prepared_source_rechecks_metadata_after_charge_and_before_access() {
    let fixture = Fixture::new();
    let (context, target) = fixture.context();
    let prepared = context
        .prepare_captured_source(&fixture.state, &target)
        .unwrap();
    reset_reads();
    let mut charged = 0;
    let failure = match prepared.read(&context, &fixture.state, |_| {
        charged += 1;
        fixture.mutate_objects("DELETE FROM captures");
        Ok(())
    }) {
        Err(error) => error,
        Ok(_) => panic!("vanished capture accepted"),
    };
    assert_eq!(charged, 1);
    assert_eq!(read_count(), 0);
    assert_eq!(
        failure.kind(),
        SourceReadFailureKind::BeforeAccess(HostError::SourceUnavailable)
    );
    assert_eq!(failure.stage(), SourceAccessStage::BeforeAccess);
    assert!(context.evidence.is_empty());
}

// AC-0287: a validly rehashed target is still not the frozen receipt descriptor.
#[test]
fn prepared_source_rejects_foreign_target_without_source_access() {
    let fixture = Fixture::new();
    let (context, mut target) = fixture.context();
    reset_reads();
    target.source.commit_sha = "other-capture".into();
    target.target_id = target.content_id().unwrap();
    target.validate().unwrap();
    assert!(matches!(
        context.prepare_captured_source(&fixture.state, &target),
        Err(HostError::InputChanged)
    ));
    assert_eq!(read_count(), 0);
}

// AC-0287: bound actual already-held text before allocating a prospective ledger
// or source response; durable charge accounting is an additional independent gate.
#[test]
fn prepared_source_checks_actual_evidence_headroom_before_read() {
    let fixture = Fixture::new();
    let (mut context, target) = fixture.context();
    context.evidence.push(AgentEvidence {
        id: "held-text".into(),
        source: target.source.clone(),
        text: "x".repeat(context.policy.limits().evidence_bytes),
    });
    reset_reads();
    assert!(matches!(
        context.prepare_captured_source(&fixture.state, &target),
        Err(HostError::LimitExceeded)
    ));
    assert_eq!(read_count(), 0);
}

#[path = "prepared_source_validation_tests.rs"]
mod validation_tests;
