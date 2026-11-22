use super::*;
use core_graph::{GraphPatch, coverage::CoverageRecord};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    directory: tempfile::TempDir,
    state: AppState,
    scope: Vec<ScopedRepo>,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private = crate::paths::canonicalize(&private).unwrap();
        let state = crate::registered_source_tests::app_state(&private);
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let source = state.sources.lock().unwrap().register_local(&root).unwrap();
        let scoped = ScopedRepo {
            repo_key: source.repo_key,
            registered_source_id: Some(source.source_id),
            config_hash: crate::coverage_config_hash(&[], crate::Registries::builtin()),
            layer_hints: None,
        };
        let record = CoverageRecord {
            repo_key: scoped.repo_key.clone(),
            registered_source_id: scoped.registered_source_id.clone(),
            ingest_id: "prepared-read-fixture".into(),
            config_hash: scoped.config_hash.clone(),
            inventory_json: json!({"inventory":"coverage@1", "repo":scoped.repo_key,
                "commit":"workdir", "layer_hints":[], "producers":[],
                "unsupported":[], "limited":false})
            .to_string(),
        };
        let mut graph = state.graph.lock().unwrap();
        let expected = graph.read_snapshot().unwrap();
        assert!(
            graph
                .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                    &expected,
                    &GraphPatch {
                        upsert_nodes: vec![Node {
                            id: "module:prepared".into(),
                            label: "Module".into(),
                            props: json!({"name":"prepared"})
                        }],
                        ..GraphPatch::default()
                    },
                    &scoped.repo_key,
                    &[],
                    Some(&record),
                    core_graph::source::SourceOmissions::NONE,
                )
                .unwrap()
        );
        drop(graph);
        Self {
            directory,
            state,
            scope: vec![scoped],
        }
    }
    fn request(&self) -> StartInvestigationRequest {
        serde_json::from_value(
            json!({"schema_version":3, "request_nonce":"prepared-context",
            "specialist_id":"domain-analyst@6", "question":"Describe the bounded scope.",
            "scope":{"type":"all"}, "provider_mode":"local", "limit_profile":"investigation-v1"}),
        )
        .unwrap()
    }
    fn prepare(&self) -> FrozenContext {
        FrozenContext::prepare_qualified_with_scope(
            &self.state,
            &self.request(),
            &[],
            1024 * 1024,
            || Ok(self.scope.clone()),
        )
        .unwrap()
    }
    fn inventory(&self, context: &FrozenContext) -> InvestigationCoverageRead {
        let proof = context.ledger.coverage.as_ref().unwrap();
        let repo = &proof.qualification.repos[0];
        InvestigationCoverageRead {
            context_id: proof.context_id.clone(),
            repo: repo.repo.clone(),
            inventory_hash: repo.inventory_hash.clone().unwrap(),
            max_bytes: MAX_QUERY_BYTES,
        }
    }
    fn candidate(
        &self,
        context: &FrozenContext,
        kind: PreparedContextReadKind,
    ) -> PreparedContextRead {
        match kind {
            PreparedContextReadKind::Artifact => {
                unreachable!("artifact reads are tested separately")
            }
            PreparedContextReadKind::Query => context.prepare_query(&self.state, query()).unwrap(),
            PreparedContextReadKind::Inventory => {
                context.prepare_inventory(self.inventory(context)).unwrap()
            }
        }
    }
}
fn query() -> InvestigationQuery {
    InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: 3,
        max_bytes: MAX_QUERY_BYTES,
        cursor: None,
    }
}
type Snapshot = (InvestigationInputLedger, Vec<String>, Vec<String>, usize);
fn snapshot(context: &FrozenContext) -> Snapshot {
    (
        context.ledger.clone(),
        context.query_pages.clone(),
        context.inventory_details.clone(),
        context.evidence.len(),
    )
}
#[derive(Default)]
struct Sink {
    usage: InvestigationUsage,
    calls: usize,
    fail: bool,
    path: Option<PathBuf>,
}
impl AcquisitionSink for Sink {
    fn usage(&self) -> &InvestigationUsage {
        &self.usage
    }
    fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
        panic!("context metadata reserves no source bytes")
    }
    fn publish_input(
        &mut self,
        ledger: &InvestigationInputLedger,
        usage: InvestigationUsage,
    ) -> Result<(), HostError> {
        self.calls += 1;
        if self.fail {
            return Err(HostError::Operational);
        }
        ledger.validate()?;
        if let Some(path) = &self.path {
            std::fs::write(path, serde_json::to_vec(ledger).unwrap())
                .map_err(|_| HostError::Operational)?;
        }
        self.usage = usage;
        Ok(())
    }
}

#[test]
fn prepared_context_reads_do_not_publish_or_mutate_and_match_existing_wrappers() {
    // AC-0287 groundwork: preparing exact UTF-8 bytes is distinct from publication;
    // neither constructor has a sink or mutable access to the frozen context.
    let fixture = Fixture::new();
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        let mut context = fixture.prepare();
        let mut wrapped = fixture.prepare();
        let before = snapshot(&context);
        let mut sink = Sink {
            path: Some(fixture.directory.path().join(format!("{kind:?}.json"))),
            ..Sink::default()
        };
        let prepared = fixture.candidate(&context, kind);
        assert_eq!(snapshot(&context), before);
        assert_eq!(sink.calls, 0);
        assert_eq!(prepared.base_ledger(), &before.0);
        assert_eq!(prepared.policy_profile(), INVESTIGATION_V1_POLICY.profile());
        assert_eq!(prepared.kind(), kind);
        assert_eq!(prepared.ledger().revision, before.0.revision + 1);
        prepared.ledger().validate().unwrap();
        serde_json::from_str::<serde_json::Value>(prepared.raw()).unwrap();
        let expected_ledger = prepared.ledger().clone();
        let expected_raw = prepared.raw().to_owned();
        context.publish_context_read(prepared, &mut sink).unwrap();
        assert_eq!(context.ledger, expected_ledger);
        assert_eq!(sink.calls, 1);
        let reopened: InvestigationInputLedger =
            serde_json::from_slice(&std::fs::read(sink.path.as_ref().unwrap()).unwrap()).unwrap();
        reopened.validate().unwrap();
        assert_eq!(reopened, expected_ledger);
        let mut old_sink = Sink::default();
        match kind {
            PreparedContextReadKind::Artifact => {
                unreachable!("artifact reads are tested separately")
            }
            PreparedContextReadKind::Query => {
                assert_eq!(context.query_pages, vec![expected_raw]);
                assert!(context.inventory_details.is_empty());
                wrapped
                    .query(&fixture.state, query(), &mut old_sink)
                    .unwrap();
            }
            PreparedContextReadKind::Inventory => {
                assert_eq!(context.inventory_details, vec![expected_raw]);
                assert!(context.query_pages.is_empty());
                wrapped
                    .read_inventory(fixture.inventory(&wrapped), &mut old_sink)
                    .unwrap();
            }
        }
        assert_eq!(snapshot(&context), snapshot(&wrapped));
        assert_eq!(sink.usage, old_sink.usage);
    }
    // Legacy graph-only queries retain their original response envelope too.
    let legacy: StartInvestigationRequest = serde_json::from_value(json!({"schema_version":1,
        "request_nonce":"legacy-prepared", "specialist_id":"domain-analyst@1", "question":"Describe the scope.",
        "scope":{"type":"all"}, "provider_mode":"local", "limit_profile":"investigation-v1"})).unwrap();
    let mut context = FrozenContext::prepare(&fixture.state, &legacy, &[]).unwrap();
    let prepared = context.prepare_query(&fixture.state, query()).unwrap();
    let raw: serde_json::Value = serde_json::from_str(prepared.raw()).unwrap();
    assert!(raw["context"].get("context_id").is_none());
    context
        .publish_context_read(prepared, &mut Sink::default())
        .unwrap();
    assert_eq!(context.query_pages.len(), 1);
}

#[test]
fn prepared_context_reads_reject_stale_equal_revision_and_policy_before_sink() {
    // AC-0287 groundwork: a revision number alone is not the complete frozen basis.
    let fixture = Fixture::new();
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        for changed in [0, 1, 2] {
            let mut context = fixture.prepare();
            let candidate = fixture.candidate(&context, kind);
            match changed {
                0 => context.ledger.revision += 1,
                1 => {
                    context.ledger.supplied_history_hash = Some(content_hash(b"[]"));
                    context.ledger.supplied_history_bytes = 2;
                    assert_eq!(context.ledger.revision, candidate.base_ledger().revision);
                }
                _ => context.policy = std::sync::Arc::new(INVESTIGATION_V2_POLICY.clone()),
            }
            let before = snapshot(&context);
            assert!(matches!(
                context.prospective_context(&candidate),
                Err(HostError::InputChanged)
            ));
            let mut sink = Sink::default();
            assert_eq!(
                context.publish_context_read(candidate, &mut sink),
                Err(HostError::InputChanged)
            );
            assert_eq!(snapshot(&context), before);
            assert_eq!(sink.calls, 0);
        }
        let mut context = fixture.prepare();
        let first = fixture.candidate(&context, kind);
        let stale = fixture.candidate(&context, kind);
        let mut sink = Sink::default();
        context.publish_context_read(first, &mut sink).unwrap();
        let before = snapshot(&context);
        assert_eq!(
            context.publish_context_read(stale, &mut sink),
            Err(HostError::InputChanged)
        );
        assert_eq!(sink.calls, 1);
        assert_eq!(snapshot(&context), before);
    }
}

#[test]
fn prepared_context_read_publication_failure_installs_no_bytes_or_ledger() {
    // AC-0287 groundwork: every fallible conversion/check/allocation precedes the
    // sink, and a sink error cannot install prospective input in memory.
    let fixture = Fixture::new();
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        let mut context = fixture.prepare();
        let before = snapshot(&context);
        let candidate = fixture.candidate(&context, kind);
        let mut sink = Sink {
            fail: true,
            ..Sink::default()
        };
        assert_eq!(
            context.publish_context_read(candidate, &mut sink),
            Err(HostError::Operational)
        );
        assert_eq!(snapshot(&context), before);
        assert_eq!(sink.calls, 1);
        let mut corrupt = fixture.candidate(&context, kind);
        corrupt.raw.push(' ');
        assert!(matches!(
            context.prospective_context(&corrupt),
            Err(HostError::InvalidInput)
        ));
        let mut unused = Sink::default();
        assert_eq!(
            context.publish_context_read(corrupt, &mut unused),
            Err(HostError::InvalidInput)
        );
        assert_eq!(unused.calls, 0);
        assert_eq!(snapshot(&context), before);
    }
}

#[test]
fn prepared_query_holds_retention_guards_through_sink_and_releases_on_return() {
    // AC-0287 groundwork: real retained-source locks survive prepare and publish;
    // preparation is deliberately not claimed to perform zero local I/O.
    for fail in [false, true] {
        let fixture = Fixture::new();
        let source = fixture
            .state
            .sources
            .lock()
            .unwrap()
            .get_by_repo(&fixture.scope[0].repo_key)
            .unwrap()
            .unwrap();
        std::fs::write(
            source.root().join("rule.ts"),
            "export function allowed(ok: boolean) { if (!ok) return false; }\n",
        )
        .unwrap();
        let input = fixture
            .state
            .primary_sources
            .prepare(&source, source.root(), &["client".into()])
            .unwrap();
        let capture = input.capture.as_ref().unwrap();
        let (extraction, receipts) = adapters_lang_ts::captured::extract_file(
            capture.file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        fixture
            .state
            .primary_sources
            .persist(&input, &source, &receipts)
            .unwrap();
        let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
        assert!(!bindings.is_empty());
        crate::load_into_graph_with_bindings(
            &mut fixture.state.graph.lock().unwrap(),
            &extraction,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
            None,
        )
        .unwrap();
        drop(input);
        let preview = crate::primary_source::preview(&fixture.state, &source.source_id).unwrap();
        assert!(preview.captures > 0);
        let mut context = fixture.prepare();
        let before = snapshot(&context);
        let mut requested = query();
        requested.kind = Some(InvestigationFactKind::Node);
        requested.labels = vec!["BusinessRule".into()];
        requested.max_facts = 1;
        let candidate = context.prepare_query(&fixture.state, requested).unwrap();
        assert!(!candidate.ledger().receipt_references.is_empty());
        assert_eq!(snapshot(&context), before);
        assert!(
            crate::primary_source::forget(&fixture.state, &source.source_id, &preview.fingerprint)
                .is_err()
        );
        struct GuardSink<'a> {
            state: &'a AppState,
            source: &'a str,
            fingerprint: &'a str,
            usage: InvestigationUsage,
            fail: bool,
            calls: usize,
        }
        impl AcquisitionSink for GuardSink<'_> {
            fn usage(&self) -> &InvestigationUsage {
                &self.usage
            }
            fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
                panic!("metadata read")
            }
            fn publish_input(
                &mut self,
                ledger: &InvestigationInputLedger,
                _: InvestigationUsage,
            ) -> Result<(), HostError> {
                self.calls += 1;
                ledger.validate()?;
                assert!(
                    crate::primary_source::forget(self.state, self.source, self.fingerprint)
                        .is_err()
                );
                if self.fail {
                    Err(HostError::Operational)
                } else {
                    Ok(())
                }
            }
        }
        let mut sink = GuardSink {
            state: &fixture.state,
            source: &source.source_id,
            fingerprint: &preview.fingerprint,
            usage: InvestigationUsage::default(),
            fail,
            calls: 0,
        };
        let result = context.publish_context_read(candidate, &mut sink);
        assert_eq!(sink.calls, 1);
        if fail {
            assert_eq!(result, Err(HostError::Operational));
            assert_eq!(snapshot(&context), before);
        } else {
            result.unwrap();
            assert_eq!(context.query_pages.len(), 1);
        }
        // The fixture sink retained no task refs: after return, successful forget
        // proves the candidate's real shared locks were released on both paths.
        assert_eq!(
            crate::primary_source::forget(&fixture.state, &source.source_id, &preview.fingerprint)
                .unwrap(),
            preview.captures as u64
        );
    }
}

#[test]
fn prepared_context_reads_require_exactly_one_manifest_extension() {
    // AC-0287: a locally corrupted candidate cannot replace prior history or
    // publish two acquisitions through a single prepared-read boundary.
    let fixture = Fixture::new();
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        for corruption in 0..5 {
            let mut context = fixture.prepare();
            let first = fixture.candidate(&context, kind);
            context
                .publish_context_read(first, &mut Sink::default())
                .unwrap();
            let mut candidate = fixture.candidate(&context, kind);
            match corruption {
                0 => candidate.ledger.revision += 1,
                1 => match kind {
                    PreparedContextReadKind::Artifact => {
                        unreachable!("artifact reads are tested separately")
                    }
                    PreparedContextReadKind::Query => {
                        candidate.ledger.queries.pop();
                    }
                    PreparedContextReadKind::Inventory => {
                        candidate.ledger.coverage_reads.pop();
                    }
                },
                2 => match kind {
                    PreparedContextReadKind::Artifact => {
                        unreachable!("artifact reads are tested separately")
                    }
                    PreparedContextReadKind::Query => {
                        candidate.ledger.queries[0].response_hash = content_hash(b"changed")
                    }
                    PreparedContextReadKind::Inventory => {
                        candidate.ledger.coverage_reads[0].response_hash = content_hash(b"changed")
                    }
                },
                3 => match kind {
                    PreparedContextReadKind::Artifact => {
                        unreachable!("artifact reads are tested separately")
                    }
                    PreparedContextReadKind::Query => candidate
                        .ledger
                        .queries
                        .push(candidate.ledger.queries.last().unwrap().clone()),
                    PreparedContextReadKind::Inventory => candidate
                        .ledger
                        .coverage_reads
                        .push(candidate.ledger.coverage_reads.last().unwrap().clone()),
                },
                _ => {
                    let opposite = match kind {
                        PreparedContextReadKind::Artifact => {
                            unreachable!("artifact reads are tested separately")
                        }
                        PreparedContextReadKind::Query => PreparedContextReadKind::Inventory,
                        PreparedContextReadKind::Inventory => PreparedContextReadKind::Query,
                    };
                    let other = fixture.candidate(&context, opposite);
                    match kind {
                        PreparedContextReadKind::Artifact => {
                            unreachable!("artifact reads are tested separately")
                        }
                        PreparedContextReadKind::Query => {
                            candidate.ledger.coverage_reads = other.ledger.coverage_reads
                        }
                        PreparedContextReadKind::Inventory => {
                            candidate.ledger.queries = other.ledger.queries
                        }
                    }
                }
            }
            let before = snapshot(&context);
            assert!(matches!(
                context.prospective_context(&candidate),
                Err(HostError::InvalidInput)
            ));
            let mut sink = Sink::default();
            assert_eq!(
                context.publish_context_read(candidate, &mut sink),
                Err(HostError::InvalidInput)
            );
            assert_eq!(sink.calls, 0);
            assert_eq!(snapshot(&context), before);
        }
    }
}

#[test]
fn prospective_context_borrows_exact_candidate_and_rejects_changed_basis() {
    // AC-0287: sizing sees the same raw bytes publication will install.
    let fixture = Fixture::new();
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        let mut context = fixture.prepare();
        let first = fixture.candidate(&context, kind);
        context
            .publish_context_read(first, &mut Sink::default())
            .unwrap();
        let prepared = fixture.candidate(&context, kind);
        let before = snapshot(&context);
        let view = context.prospective_context(&prepared).unwrap();
        assert!(std::ptr::eq(view.ledger, prepared.ledger()));
        assert!(std::ptr::eq(view.evidence, context.evidence.as_slice()));
        for (borrowed, original) in view.query_pages.iter().zip(&context.query_pages) {
            assert!(std::ptr::eq(*borrowed, original.as_str()));
        }
        for (borrowed, original) in view
            .inventory_details
            .iter()
            .zip(&context.inventory_details)
        {
            assert!(std::ptr::eq(*borrowed, original.as_str()));
        }
        match kind {
            PreparedContextReadKind::Artifact => {
                unreachable!("artifact reads are tested separately")
            }
            PreparedContextReadKind::Query => {
                assert_eq!(view.query_pages.len(), context.query_pages.len() + 1);
                assert_eq!(
                    view.inventory_details.len(),
                    context.inventory_details.len()
                );
                assert!(std::ptr::eq(
                    *view.query_pages.last().unwrap(),
                    prepared.raw()
                ));
            }
            PreparedContextReadKind::Inventory => {
                assert_eq!(
                    view.inventory_details.len(),
                    context.inventory_details.len() + 1
                );
                assert_eq!(view.query_pages.len(), context.query_pages.len());
                assert!(std::ptr::eq(
                    *view.inventory_details.last().unwrap(),
                    prepared.raw()
                ));
            }
        }
        drop(view);
        assert_eq!(snapshot(&context), before);
        context.ledger.revision += 1;
        assert!(matches!(
            context.prospective_context(&prepared),
            Err(HostError::InputChanged)
        ));
    }
}

// AC-0327: compare the shared DTO model with real host-owned preparation,
// including an appended candidate rejected after the original ledger validates.
#[test]
fn inventory_workspace_covers_host_preparation_and_candidate_refusal() {
    use context_hub::logical::{count, json_bytes, slots, sum};
    let fixture = Fixture::new();
    let mut context = fixture.prepare();
    let read = fixture.inventory(&context);
    let wire = count(&context.ledger).unwrap();
    let model = inventory_preparation_workspace(&context.policy, wire).unwrap();
    let prepared = context.prepare_inventory(read.clone()).unwrap();
    let actual = sum(&[
        slots::<InvestigationInputLedger>(2).unwrap(),
        json_bytes(count(prepared.base_ledger()).unwrap()).unwrap(),
        json_bytes(count(prepared.ledger()).unwrap()).unwrap(),
        prepared.raw.len(),
        slots::<String>(1).unwrap(),
    ])
    .unwrap();
    #[cfg(unix)]
    {
        let wrapper = std::mem::size_of::<PreparedContextRead>();
        assert!(inventory_admission_workspace(0).unwrap() >= actual + wrapper);
        let admitted =
            inventory_preparation_workspace(&context.policy, MAX_MANIFEST_BYTES).unwrap();
        // AC-0327: replay dominates so preparation cannot mask a missing owner.
        let replay = admitted
            .peak
            .checked_add(wrapper)
            .unwrap()
            .checked_add(1)
            .unwrap();
        let expected = replay
            .checked_add(admitted.retained)
            .unwrap()
            .checked_add(wrapper)
            .unwrap();
        assert_eq!(inventory_admission_workspace(replay), Some(expected));
        assert!(inventory_admission_workspace(usize::MAX).is_none());
    }
    assert!(model.retained >= actual);
    assert!(model.peak >= model.retained);
    assert!(inventory_preparation_workspace(&context.policy, usize::MAX).is_none());
    let remaining = context.policy.limits().tool_actions as usize - context.ledger.queries.len();
    context.ledger.coverage_reads = vec![
        InvestigationCoverageReadManifest {
            request: read.clone(),
            response_hash: "a".repeat(64),
            response_bytes: 1,
        };
        remaining
    ];
    context
        .ledger
        .validate_with_policy(&context.policy)
        .unwrap();
    let before = context.ledger.clone();
    assert!(matches!(
        context.prepare_inventory(read),
        Err(HostError::LimitExceeded)
    ));
    assert_eq!(context.ledger, before);
}

// AC-0327: accepted mixed reads remain resident after each publication.
#[cfg(unix)]
#[test]
fn retained_context_workspace_covers_published_query_and_inventory() {
    use context_hub::logical::{count, json_bytes, slots, sum};
    let fixture = Fixture::new();
    let mut context = fixture.prepare();
    let mut sink = Sink::default();
    for _ in 0..2 {
        for kind in [
            PreparedContextReadKind::Query,
            PreparedContextReadKind::Inventory,
        ] {
            let prepared = fixture.candidate(&context, kind);
            context.publish_context_read(prepared, &mut sink).unwrap();
            let pages = context.query_pages.iter().chain(&context.inventory_details);
            let actual = sum(&[
                json_bytes(count(&context.ledger).unwrap()).unwrap(),
                slots::<InvestigationInputLedger>(1).unwrap(),
                pages.clone().map(String::len).sum(),
                slots::<String>(pages.count()).unwrap(),
            ])
            .unwrap();
            assert!(preparation_workspace::retained_read_bytes(&context.policy).unwrap() >= actual);
        }
    }
    assert_eq!(context.query_pages.len(), 2);
    assert_eq!(context.inventory_details.len(), 2);
    assert!(
        preparation_workspace::retained_read_bytes(&INVESTIGATION_V2_POLICY).unwrap()
            >= preparation_workspace::retained_read_bytes(&INVESTIGATION_V1_POLICY).unwrap()
    );
}
