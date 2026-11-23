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
        Self::state_at_with_sizes(private, None)
    }
    fn state_at_with_sizes(private: &std::path::Path, understated: Option<bool>) -> AppState {
        let root = private.join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("rule.ts"),"export function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n").unwrap();
        let state = crate::registered_source_tests::app_state(private);
        let source = state.sources.lock().unwrap().register_local(&root).unwrap();
        let input = state
            .primary_sources
            .prepare(&source, source.root(), &["client".into()])
            .unwrap();
        let id = adapters_lang_ts::SourceId {
            repo: &source.repo_key,
            commit: "workdir",
        };
        let (extraction, receipts) = if understated.is_some() {
            let (extraction, receipts, stats) = adapters_lang_ts::captured::extract_captured_dir(
                &root,
                &id,
                input.capture.as_ref().unwrap(),
                adapters_fw::events::Registries::builtin(),
                &mut |_| {},
            )
            .unwrap();
            assert!(stats.omitted_rule_receipts.is_empty());
            assert!(stats.omitted_declaration_receipts.is_empty());
            let receipts = receipts
                .iter()
                .map(|r| r.to_receipt().unwrap())
                .collect::<Vec<_>>();
            for receipt in &receipts {
                for index in 0..receipt.ranges().len() {
                    assert!(receipt.source_sizes(index).unwrap().is_some());
                }
            }
            (extraction, receipts)
        } else {
            adapters_lang_ts::captured::extract_file(
                input.capture.as_ref().unwrap().file("rule.ts").unwrap(),
                &id,
            )
            .unwrap()
        };
        let receipts = if let Some(understated) = understated {
            receipts
                .into_iter()
                .map(|receipt| {
                    if understated {
                        understated_receipt(receipt)
                    } else {
                        receipt
                    }
                })
                .collect()
        } else {
            receipts
        };
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
            base.fingerprint_with_policy(&context.policy).unwrap()
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
    #[cfg(unix)]
    {
        // AC-0327: retained captured bytes survive after their checkout is gone.
        use context_hub::logical::{count, json_bytes, slots, sum};
        let actual = sum(&[
            json_bytes(count(&old.ledger).unwrap()).unwrap(),
            slots::<InvestigationInputLedger>(1).unwrap(),
            old.query_pages.iter().map(String::len).sum(),
            slots::<String>(old.query_pages.len()).unwrap(),
            count(&old.evidence).unwrap(),
            slots::<agents::AgentEvidence>(old.evidence.len()).unwrap(),
        ])
        .unwrap();
        assert!(
            crate::investigations::preparation_workspace::retained_read_bytes(&old.policy).unwrap()
                >= actual
        );
    }
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
        // AC-1609: a known read failure keeps its closed step and class.
        assert_eq!(
            failure.discard_diagnostic(),
            (expected != SourceReadFailureKind::ReadUnavailable).then_some(
                InvestigationTerminalDiagnostic::SourceReadDiscarded {
                    step: agents::investigation::SourceDiscardStep::ReadText,
                    error_class: agents::investigation::SourceDiscardClass::HostInvalidInput,
                }
            )
        );
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
            context.policy = std::sync::Arc::new(INVESTIGATION_V2_POLICY.clone());
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

#[path = "prepared_source_reservation_tests.rs"]
mod reservation_tests;

// AC-0327: admitted file/span limits refuse before charges and text access.
#[test]
fn prepared_source_admitted_caps_accept_exact_and_refuse_one_below() {
    let fixture = Fixture::new();
    let (context, target) = fixture.context();
    let file = target.captured.file.byte_len;
    let span = target.captured.byte_end - target.captured.byte_start;
    assert!(file > 1 && span > 1);
    for (file_cap, span_cap, accepted) in [
        (file, span, true),
        (file - 1, span, false),
        (file, span - 1, false),
    ] {
        reset_reads();
        let limits = crate::mcp_reads::dispatch::EvidenceWorkLimits {
            max_file_validation_bytes: std::num::NonZeroU64::new(file_cap).unwrap(),
            max_span_bytes: std::num::NonZeroU64::new(span_cap).unwrap(),
        };
        let prepared = context
            .prepare_captured_source(&fixture.state, &target)
            .unwrap()
            .with_read_limits(Some(limits));
        assert_eq!(read_count(), 0);
        if accepted {
            let mut charges = 0;
            let candidate = prepared.unwrap().read(&context, &fixture.state, |_| {
                charges += 1;
                Ok(())
            });
            assert!(candidate.is_ok());
            assert_eq!(charges, 1);
            assert_eq!(read_count(), 1);
        } else {
            assert!(matches!(prepared, Err(HostError::LimitExceeded)));
            assert_eq!(read_count(), 0);
        }
        assert!(context.evidence.is_empty());
    }
}

// AC-0287 / #1701: a self-consistent receipt can still understate its bytes.
// Test-only rewrite deliberately updates the content hash and graph binding;
// rejection must therefore occur after the charged object read, not by accident
// through stale identity checks.
fn understated_receipt(
    receipt: adapters_lang_ts::captured::Receipt,
) -> adapters_lang_ts::captured::Receipt {
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            }
            value => value,
        }
    }
    let mut wire: serde_json::Value = serde_json::from_str(&receipt.to_json().unwrap()).unwrap();
    for range in wire["content"]["source_sizes"]["ranges"]
        .as_array_mut()
        .unwrap()
    {
        range["original"] = 0.into();
        range["redacted"] = 0.into();
    }
    let mut bytes = b"cartograph:ts-primary-receipt:v3\0".to_vec();
    bytes.extend(serde_json::to_vec(&canonical(wire["content"].clone())).unwrap());
    wire["receipt_id"] = format!("ts-primary-v3:{}", core_prov::content_hash(&bytes)).into();
    adapters_lang_ts::captured::Receipt::from_json(&serde_json::to_string(&wire).unwrap()).unwrap()
}

#[test]
fn prepared_source_size_claim_is_checked_after_charge_before_context_installation() {
    for understated in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private = crate::paths::canonicalize(private).unwrap();
        let state = Fixture::state_at_with_sizes(&private, Some(understated));
        let fixture = Fixture {
            _directory: directory,
            private,
            state,
        };
        let (context, target) = fixture.context();
        let base = context.ledger.clone();
        reset_reads();
        let prepared = context
            .prepare_captured_source(&fixture.state, &target)
            .unwrap();
        assert_eq!(read_count(), 0);
        // AC-0287: authority is borrowed from the held receipt, never supplied
        // as free lengths. A different descriptor or base ledger is rejected.
        let binding = prepared.size_binding();
        assert!(binding.sizes_for(&target, &base).unwrap().is_some());
        let mut wrong_target = target.clone();
        wrong_target.receipt_id.push('0');
        assert!(binding.sizes_for(&wrong_target, &base).is_err());
        let mut wrong_ledger = base.clone();
        wrong_ledger.revision += 1;
        assert!(binding.sizes_for(&target, &wrong_ledger).is_err());
        assert_eq!(read_count(), 0);
        let mut charges = 0;
        let result = prepared.read(&context, &fixture.state, |_| {
            assert_eq!(read_count(), 0);
            charges += 1;
            Ok(())
        });
        assert_eq!(charges, 1);
        assert_eq!(read_count(), 1);
        if understated {
            let failure = match result {
                Err(failure) => failure,
                Ok(_) => panic!("understated sizes admitted"),
            };
            assert_eq!(failure.stage(), SourceAccessStage::AccessStarted);
            assert_eq!(
                failure.kind(),
                SourceReadFailureKind::KnownReadFailure(HostError::InvalidInput)
            );
            // AC-1609: the understated receipt is named, not folded into a read error.
            assert_eq!(
                failure.discard_diagnostic(),
                Some(InvestigationTerminalDiagnostic::SourceReadDiscarded {
                    step: agents::investigation::SourceDiscardStep::ReadSizes,
                    error_class: agents::investigation::SourceDiscardClass::HostInvalidInput,
                })
            );
        } else {
            assert!(result.is_ok());
        }
        assert!(context.evidence.is_empty());
        assert_eq!(context.ledger, base);
    }
}
