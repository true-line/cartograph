//! Production capture, publication, inspection and retention across app restarts.

use crate::registered_source_tests::{app_state, historical_proposal, staged_bytes};
use crate::*;
use adapters_lang_ts::captured::Receipt;
use core_graph::rules::{GuardedExitEvidence, InterpretationStatus};
use core_graph::source::{FactKey, SourceBinding};
use core_prov::{ConfidenceTier, Provenance, Tier};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const ORIGINAL: &str = concat!(
    "// café — 🧭\n",
    "export function retained(enabled: boolean) {\n",
    "  if (enabled /* primary-comment-canary */) return { ",
    "password: \"primary-password-canary\", token: \"\\x67hp_primarycapture1234\" };\n",
    "  if (!enabled) return false;\n",
    "}\n",
    "// primary-trailing-source-canary\n",
);
const STABLE_A: &str =
    "export function stable(enabled: boolean) { if (enabled) return false; }\n// trailing-A\n";
const STABLE_B: &str =
    "export function stable(enabled: boolean) { if (enabled) return false; }\n// trailing-B\n";

fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

struct Parsed {
    // The production retention guard must survive persistence and publication.
    input: primary_source::PrimaryInput,
    extraction: adapters_lang_ts::Extraction,
    receipts: Vec<Receipt>,
    delta: DeltaSummary,
}

fn parse(state: &AppState, source: &RegisteredSource, after_capture: impl FnOnce()) -> Parsed {
    // Application-only selection avoids making claims about auxiliary adapters'
    // live config reads. This is the actual shared production extraction branch.
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(source, source.root(), &layers)
        .unwrap();
    assert!(input.capture.is_some());
    after_capture();
    let mut receipts = Vec::new();
    let (extraction, _, delta) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            input.capture.as_ref(),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(!receipts.is_empty());
    Parsed {
        input,
        extraction,
        receipts: receipts
            .iter()
            .map(|receipt| receipt.to_receipt().unwrap())
            .collect(),
        delta,
    }
}

fn publish(state: &AppState, source: &RegisteredSource, parsed: &Parsed) -> Vec<SourceBinding> {
    state
        .primary_sources
        .persist(&parsed.input, source, &parsed.receipts)
        .unwrap();
    let bindings = primary_source::matching_bindings(&parsed.extraction, &parsed.receipts);
    load_into_graph_with_bindings(
        &mut state.graph.lock().unwrap(),
        &parsed.extraction,
        &source.repo_key,
        source.root(),
        "workdir",
        &bindings,
        None,
    )
    .unwrap();
    bindings
}

fn rule_node(parsed: &Parsed) -> Node {
    parsed
        .extraction
        .nodes
        .iter()
        .find(|node| node.label == "BusinessRule")
        .unwrap()
        .clone()
}

fn assert_original_ranges(
    state: &AppState,
    description: &primary_source::CapturedDescription,
    original: &str,
) {
    assert_eq!(description.scope, "primary_source_only");
    assert_eq!(description.input_closure, "input_closure_not_established");
    assert!(!description.ranges.is_empty());
    for range in &description.ranges {
        let read = primary_source::read(
            state,
            &description.fact,
            &description.receipt_id,
            range.index,
        )
        .unwrap();
        assert_eq!(read.receipt_id, description.receipt_id);
        assert_eq!(read.range_index, range.index);
        assert_eq!(read.path, "source.ts");
        assert_eq!(read.path, range.path);
        assert_eq!(
            (read.byte_start, read.byte_end),
            (range.byte_start, range.byte_end)
        );
        assert_eq!(
            read.text.as_bytes(),
            &original.as_bytes()[range.byte_start as usize..range.byte_end as usize]
        );
    }
}

fn receipt_rows(app_data: &Path) -> Vec<(String, String, String, String)> {
    let conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut query = conn
        .prepare("SELECT id,source_id,repo_key,payload FROM receipts ORDER BY id")
        .unwrap();
    query
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn proposal_page(state: &AppState) -> serde_json::Value {
    serde_json::to_value(state.proposals.lock().unwrap().list(20, None).unwrap()).unwrap()
}

/// Publish an actual captured fact's selected binding through the durable
/// coordinator before reading any source. This exercises retention's JobStore
/// seam without fabricating a source citation or invoking a provider.
fn retain_investigation_selection(
    state: &AppState,
    source: &RegisteredSource,
    binding: &SourceBinding,
    nonce: &str,
) -> (String, agents::investigation::InvestigationInputLedger) {
    use crate::jobs::investigations::{InvestigationStopReason, InvestigationTransition};
    use agents::investigation::*;
    use agents::{TaskFactSelection, TaskSourceAssociation};
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    let snapshot = context_hub::ContextSnapshot::new(nodes, edges).unwrap();
    let page = snapshot
        .query(context_hub::QueryRequest {
            scope: context_hub::QueryScope::All,
            kind: Some(context_hub::FactKind::Node),
            labels: vec!["BusinessRule".into()],
            max_facts: 32,
            max_bytes: 32 * 1024,
            cursor: None,
        })
        .unwrap();
    let raw = serde_json::to_vec(&page).unwrap();
    let fact = crate::task_evidence::task_fact(&binding.fact);
    let ledger = InvestigationInputLedger {
        coverage_reads: vec![],
        artifact_reads: vec![],
        schema_version: 1,
        coverage: None,
        graph_snapshot_id: snapshot.id().into(),
        scope_snapshot_id: snapshot.id().into(),
        revision: 1,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: binding.emitted_fact_digest.clone(),
            binding: Some(TaskSourceAssociation {
                repo_key: binding.repo_key.clone(),
                receipt_id: binding.receipt_id.clone(),
                emitted_fact_digest: binding.emitted_fact_digest.clone(),
            }),
        }],
        receipt_references: vec![InvestigationReceiptReference {
            source_id: source.source_id.clone(),
            repo_key: binding.repo_key.clone(),
            receipt_id: binding.receipt_id.clone(),
        }],
        citations: vec![InvestigationCitation {
            citation_id: "fact-1".into(),
            fact,
            fact_digest: binding.emitted_fact_digest.clone(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }],
        queries: vec![InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: Some(InvestigationFactKind::Node),
                labels: vec!["BusinessRule".into()],
                max_facts: 32,
                max_bytes: 32 * 1024,
                cursor: None,
            },
            response_hash: core_prov::content_hash(&raw),
            response_bytes: raw.len(),
            returned_facts: page.facts.len(),
            total_selected: page.total_selected,
            has_more: page.next_cursor.is_some(),
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    ledger.validate().unwrap();
    let request = StartInvestigationRequest {
        inspection_requirements: None,
        schema_version: 1,
        request_nonce: nonce.into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "Which recovered rule needs further evidence?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: Some(snapshot.id().into()),
        expected_context_id: None,
        conversation_id: None,
        parent_id: None,
    };
    let provider = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: "local:retention-fixture".into(),
        model: "fixture".into(),
        endpoint: "http://127.0.0.1:11434".into(),
        deployment: None,
        protocol_version: None,
        available: true,
        unavailable_reason: None,
    };
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .start_investigation(&request, Some(&provider))
        .unwrap()
        .detail;
    let plan = state
        .jobs
        .lock()
        .unwrap()
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let (_, execution) = claim_job(state, &plan).unwrap();
    let id = detail.summary.investigation_id;
    {
        // The same source-guard → short JobStore ordering as production input
        // acquisition, retaining the lease until its immutable index is durable.
        let guard = state.primary_sources.task_guard(&source.source_id).unwrap();
        state
            .primary_sources
            .task_receipt(
                &guard,
                &binding.fact,
                &binding.emitted_fact_digest,
                &binding.repo_key,
                &binding.receipt_id,
            )
            .unwrap();
        let mut jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&id).unwrap();
        let admitted = jobs
            .advance_investigation(
                &execution,
                detail.summary.revision,
                InvestigationTransition::InputPrepared {
                    coverage_archive: None,
                    ledger: Box::new(ledger.clone()),
                    usage: InvestigationUsage {
                        selected_facts: 1,
                        ..detail.usage
                    },
                },
            )
            .unwrap();
        assert_eq!(admitted.usage.evidence_items, 0);
        assert_eq!(admitted.usage.model_invocations, 0);
    }
    let mut jobs = state.jobs.lock().unwrap();
    let cancelled = jobs.cancel_investigation(&id).unwrap();
    jobs.advance_investigation(
        &execution,
        cancelled.summary.revision,
        InvestigationTransition::StopWithElapsed {
            reason: InvestigationStopReason::Cancelled,
            active_milliseconds: cancelled.usage.active_milliseconds,
        },
    )
    .unwrap();
    (id, ledger)
}

struct HistoricalStage {
    id: String,
    immutable_json: String,
    reviewed_json: String,
}

fn reviewed_historical_stage(state: &AppState, app_data: &Path) -> HistoricalStage {
    let conn = rusqlite::Connection::open(app_data.join("state.db")).unwrap();
    conn.execute("INSERT INTO jobs(kind,status,error) VALUES ('ingest:/historical/project','failed','historical failure')", []).unwrap();
    let job = state
        .jobs
        .lock()
        .unwrap()
        .get(conn.last_insert_rowid())
        .unwrap();
    let (task, proposal) = historical_proposal();
    let mut store = state.proposals.lock().unwrap();
    let staged = store
        .stage(&task, &proposal, job.id, "snapshot:historical")
        .unwrap();
    let immutable_json = staged_bytes(&app_data.join("proposals.sqlite"), &staged.proposal_id);
    let accepted = store
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            Some("first review"),
        )
        .unwrap();
    assert_eq!(accepted.proposal_id, staged.proposal_id);
    assert_eq!(accepted.review_revision, 1);
    assert_eq!(
        accepted.evidence_binding,
        agents::EvidenceBinding::WorkingTreeUnverified
    );
    assert_eq!(
        accepted.context_status,
        agents::ContextStatus::AwaitingReconciliation
    );
    assert_eq!(accepted.proposal.provenance, proposal.provenance);
    let reviewed = store
        .review(
            &staged.proposal_id,
            1,
            agents::ProposalDecision::Rejected,
            Some("second review"),
        )
        .unwrap();
    assert_eq!(reviewed.proposal_id, staged.proposal_id);
    assert_eq!(reviewed.job_id, job.id);
    assert_eq!(
        staged_bytes(&app_data.join("proposals.sqlite"), &staged.proposal_id),
        immutable_json
    );
    HistoricalStage {
        id: staged.proposal_id,
        immutable_json,
        reviewed_json: serde_json::to_string(&reviewed).unwrap(),
    }
}

fn assert_historical_stage(state: &AppState, app_data: &Path, historical: &HistoricalStage) {
    let reviewed = state
        .proposals
        .lock()
        .unwrap()
        .get(&historical.id)
        .unwrap()
        .unwrap();
    assert_eq!(reviewed.proposal_id, historical.id);
    assert_eq!(reviewed.review_revision, 2);
    assert_eq!(
        reviewed.review_decision,
        Some(agents::ProposalDecision::Rejected)
    );
    assert_eq!(
        reviewed.evidence_binding,
        agents::EvidenceBinding::WorkingTreeUnverified
    );
    assert_eq!(
        reviewed.context_status,
        agents::ContextStatus::AwaitingReconciliation
    );
    assert_eq!(reviewed.proposal.provenance.tier, Tier::Agentic);
    assert_eq!(
        reviewed.proposal.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(
        serde_json::to_string(&reviewed).unwrap(),
        historical.reviewed_json
    );
    assert_eq!(
        staged_bytes(&app_data.join("proposals.sqlite"), &historical.id),
        historical.immutable_json
    );
}

#[test]
fn primary_source_pipeline_reads_original_bytes_after_restart_and_source_deletion() {
    // AC-0151, AC-0152, AC-0155: actual captured parsing and publication, exact
    // original ranges after deletion/restart, unchanged legacy fact semantics.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let proposals_before = proposal_page(&state);
    let parsed = parse(&state, &source, || {
        std::fs::write(
            root.join("source.ts"),
            "export function replacement() { return 'live-replacement-canary'; }\n",
        )
        .unwrap();
    });
    assert_eq!(parsed.delta.recomputed_files, 1);
    assert_eq!(parsed.delta.reused_files, 0);
    let rule = rule_node(&parsed);
    let fact = FactKey::from_node(&rule);
    let evidence = GuardedExitEvidence::from_value(rule.props["rule"].clone()).unwrap();
    let prov: Provenance = serde_json::from_value(rule.props["prov"].clone()).unwrap();
    assert_eq!(prov.tier, Tier::Deterministic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
    assert_eq!(
        evidence.interpretation.consumer_effect,
        InterpretationStatus::NotEstablished
    );
    let legacy_ref = serde_json::to_value(&evidence.exit_source).unwrap();
    assert_eq!(
        legacy_ref,
        json!({
            "repo": source.repo_key, "commit_sha": "workdir", "path": "source.ts",
            "byte_start": evidence.exit_source.byte_start, "byte_end": evidence.exit_source.byte_end,
        })
    );
    publish(&state, &source, &parsed);
    let description = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(description.source_id, source.source_id);
    assert_eq!(description.repo_key, source.repo_key);
    assert_original_ranges(&state, &description, ORIGINAL);
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert_eq!(
        snapshot.0.iter().find(|node| node.id == rule.id).unwrap(),
        &rule
    );
    let page = context_hub::ContextSnapshot::new(snapshot.0.clone(), snapshot.1.clone())
        .unwrap()
        .query(context_hub::QueryRequest::default())
        .unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = spec::compile_spec(&snapshot.0, &snapshot.1, &[], mode, &BTreeSet::new());
        let surfaces = serde_json::to_string(&(
            &snapshot,
            &page,
            &bundle,
            &receipt_rows(&app_data),
            &proposals_before,
        ))
        .unwrap();
        for canary in [
            "primary-comment-canary",
            "primary-password-canary",
            "ghp_primarycapture1234",
            "primary-trailing-source-canary",
            "live-replacement-canary",
        ] {
            assert!(!surfaces.contains(canary), "source disclosed: {canary}");
        }
    }
    drop(parsed);
    std::fs::remove_dir_all(&root).unwrap();
    drop(state);

    let state = app_state(&app_data);
    let restored = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(restored.receipt_id, description.receipt_id);
    assert_original_ranges(&state, &restored, ORIGINAL);
    assert!(primary_source::read(&state, &fact, &restored.receipt_id, usize::MAX).is_err());
    assert!(
        primary_source::describe(
            &state,
            &FactKey::Node {
                id: "missing".into()
            }
        )
        .is_err()
    );
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        snapshot
    );
    // This fixture covers an unchanged empty proposal store, not recertification
    // of a populated immutable historical staging record.
    assert_eq!(proposal_page(&state), proposals_before);
}

#[test]
fn primary_source_reingest_of_unchanged_bytes_parses_nothing_and_matches_a_cold_run() {
    // AC-1517: the captured lane reuses the attested parse of unchanged
    // bytes; its facts and receipts equal a cold run's, and changed bytes
    // parse again.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let work = |parsed: &Parsed| (parsed.delta.recomputed_files, parsed.delta.reused_files);
    let cold = parse(&state, &source, || {});
    assert_eq!(work(&cold), (1, 0));
    let warm = parse(&state, &source, || {});
    assert_eq!(work(&warm), (0, 1));
    assert_eq!(warm.extraction.nodes, cold.extraction.nodes);
    assert_eq!(warm.extraction.edges, cold.extraction.edges);
    assert_eq!(warm.receipts, cold.receipts);
    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let changed = parse(&state, &source, || {});
    assert_eq!(work(&changed), (1, 0));
    assert_ne!(changed.receipts, cold.receipts);
    state.extraction_caches.lock().unwrap().repos.clear();
    let fresh = parse(&state, &source, || {});
    assert_eq!(work(&fresh), (1, 0));
    assert_eq!(changed.extraction.nodes, fresh.extraction.nodes);
    assert_eq!(changed.receipts, fresh.receipts);
}

#[test]
fn primary_source_equal_facts_publish_distinct_current_receipts() {
    // AC-0151, AC-0152: a complete fact/graph digest cannot select a raw capture;
    // the ordinary no-change reconciliation still replaces current receipts.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    publish(&state, &source, &first);
    let fact = FactKey::from_node(&rule_node(&first));
    let old = primary_source::describe(&state, &fact).unwrap();
    let before = state.graph.lock().unwrap().read_snapshot().unwrap();
    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let second = parse(&state, &source, || {});
    assert_eq!(second.delta.reused_files, 0);
    assert_eq!(second.delta.recomputed_files, 1);
    assert_eq!(first.extraction.nodes, second.extraction.nodes);
    assert_eq!(first.extraction.edges, second.extraction.edges);
    assert_ne!(
        first.input.capture.as_ref().unwrap().id(),
        second.input.capture.as_ref().unwrap().id()
    );
    publish(&state, &source, &second);
    let current = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(current.emitted_fact_digest, old.emitted_fact_digest);
    assert_ne!(current.receipt_id, old.receipt_id);
    assert_eq!(state.graph.lock().unwrap().read_snapshot().unwrap(), before);
    assert_eq!(
        primary_source::read(&state, &fact, &old.receipt_id, 0)
            .err()
            .unwrap(),
        "Captured source selection is stale; select the fact again."
    );
    assert_original_ranges(&state, &current, STABLE_B);
    drop(first);
    drop(second);
    drop(state);
    let state = app_state(&app_data);
    assert_eq!(
        primary_source::describe(&state, &fact).unwrap().receipt_id,
        current.receipt_id
    );
    assert!(primary_source::read(&state, &fact, &old.receipt_id, 0).is_err());
}

#[test]
fn primary_source_republication_invalidates_historical_retention_preview() {
    // AC-0153: republishing identical retained content changes the current
    // associations without changing the immutable capture/receipt inventory.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    let original_bindings = publish(&state, &source, &first);
    let fact = FactKey::from_node(&rule_node(&first));
    let original = primary_source::describe(&state, &fact).unwrap();
    let original_graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let original_receipts = receipt_rows(&app_data);
    drop(first);

    state.graph.lock().unwrap().clear().unwrap();
    let historical = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(historical.captures, 1);
    assert_eq!(historical.current_references, 0);
    assert_eq!(historical.historical_references, historical.receipts);
    assert!(historical.receipts > 0);

    let republished = parse(&state, &source, || {});
    assert_eq!(publish(&state, &source, &republished), original_bindings);
    drop(republished);
    let current = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(current.capture_ids, historical.capture_ids);
    assert_eq!(current.receipts, historical.receipts);
    assert_eq!(receipt_rows(&app_data), original_receipts);
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        original_graph
    );
    assert!(current.current_references > historical.current_references);
    assert!(current.historical_references < historical.historical_references);
    assert_ne!(current.fingerprint, historical.fingerprint);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &historical.fingerprint)
            .err()
            .unwrap(),
        "Retained source changed; refresh the preview before forgetting."
    );
    assert_original_ranges(&state, &original, STABLE_A);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &current.fingerprint).unwrap(),
        current.captures as u64
    );
    assert!(primary_source::read(&state, &fact, &original.receipt_id, 0).is_err());
    assert!(receipt_rows(&app_data).is_empty());
}

#[test]
fn primary_source_investigation_references_bind_forget_confirmation() {
    // AC-0188/0189: selecting a present receipt already creates a durable use,
    // before source reads. New uses invalidate confirmations even when the
    // capture, receipt and current graph inventories have not changed.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let fact = FactKey::from_node(&rule_node(&parsed));
    let binding = publish(&state, &source, &parsed)
        .into_iter()
        .find(|binding| binding.fact == fact)
        .unwrap();
    drop(parsed);
    let before = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(before.investigation_references, 0);
    let (first_id, first_ledger) =
        retain_investigation_selection(&state, &source, &binding, "retention-first");
    let first = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(first.investigation_references, 1);
    assert_eq!(first.capture_ids, before.capture_ids);
    assert_eq!(first.receipts, before.receipts);
    assert_eq!(first.current_references, before.current_references);
    assert_eq!(first.staged_references, before.staged_references);
    assert_ne!(first.fingerprint, before.fingerprint);
    let (second_id, second_ledger) =
        retain_investigation_selection(&state, &source, &binding, "retention-second");
    let second = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(second.investigation_references, 2);
    assert_eq!(second.capture_ids, first.capture_ids);
    assert_eq!(second.receipts, first.receipts);
    assert_eq!(second.current_references, first.current_references);
    assert_ne!(second.fingerprint, first.fingerprint);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &first.fingerprint)
            .err()
            .unwrap(),
        "Retained source changed; refresh the preview before forgetting."
    );
    let other_root = directory(dir.path(), "other/project");
    let other = register_local_source(&state, &other_root).unwrap();
    assert_eq!(
        primary_source::preview(&state, &other.source_id)
            .unwrap()
            .investigation_references,
        0
    );
    let receipts = receipt_rows(&app_data);
    state.jobs.lock().unwrap().clear_finished().unwrap();
    state.graph.lock().unwrap().clear().unwrap();
    std::fs::remove_dir_all(source.root()).unwrap();
    drop(state);
    let state = app_state(&app_data);
    let resumed = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(resumed.investigation_references, 2);
    assert_eq!(resumed.capture_ids, before.capture_ids);
    assert_eq!(resumed.current_references, 0);
    let retained = [&first_id, &second_id]
        .into_iter()
        .map(|id| {
            state
                .jobs
                .lock()
                .unwrap()
                .investigation_ledger(id)
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(retained, vec![first_ledger, second_ledger]);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &resumed.fingerprint).unwrap(),
        resumed.captures as u64
    );
    let forgotten = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(forgotten.captures, 0);
    assert_eq!(forgotten.investigation_references, 2);
    assert_eq!(forgotten.receipts, 1);
    let pinned_receipts: Vec<_> = receipts
        .into_iter()
        .filter(|(id, ..)| id == &binding.receipt_id)
        .collect();
    assert_eq!(receipt_rows(&app_data), pinned_receipts);
    for (id, ledger) in [&first_id, &second_id].into_iter().zip(retained) {
        assert_eq!(
            state.jobs.lock().unwrap().investigation_ledger(id).unwrap(),
            Some(ledger)
        );
    }
}

#[test]
fn primary_source_invalid_investigation_reference_blocks_forgetting() {
    // AC-0188/0189: a corrupt reference index is an explicit failure, never a
    // smaller reassuring preview or authority to delete retained content.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let fact = FactKey::from_node(&rule_node(&parsed));
    let binding = publish(&state, &source, &parsed)
        .into_iter()
        .find(|binding| binding.fact == fact)
        .unwrap();
    drop(parsed);
    let (id, ledger) =
        retain_investigation_selection(&state, &source, &binding, "retention-corrupt");
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    let conn = rusqlite::Connection::open(app_data.join("state.db")).unwrap();
    conn.execute(
        "INSERT INTO investigation_refs (task_id,input_revision,source_id,repo_key,receipt_id) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![id, i64::try_from(ledger.revision).unwrap(), source.source_id, source.repo_key,
            format!("ts-primary-v2:{}", "0".repeat(64))],
    ).unwrap();
    assert!(primary_source::preview(&state, &source.source_id).is_err());
    assert!(primary_source::forget(&state, &source.source_id, &preview.fingerprint).is_err());
    let captures = source_capture::CaptureStore::open(
        &app_data.join("retained-source/captures.sqlite"),
        source_capture::StoreLimits::default(),
    )
    .unwrap();
    let still_retained = captures
        .source_inventory(&source_capture::SourceId::new(&source.source_id).unwrap())
        .unwrap();
    assert_eq!(
        still_retained
            .into_iter()
            .map(|capture| capture.capture_id)
            .collect::<Vec<_>>(),
        preview.capture_ids
    );
}

#[test]
fn primary_source_same_name_sources_forget_only_previewed_retention() {
    // AC-0151, AC-0153, AC-0155: source identity and preview-bound forgetting
    // survive restart while shared raw objects and populated, reviewed historical
    // staging bytes remain unchanged and cannot acquire captured-source semantics.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    for root in [&first_root, &second_root] {
        std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    }
    let state = app_state(&app_data);
    let historical = reviewed_historical_stage(&state, &app_data);
    assert_historical_stage(&state, &app_data, &historical);
    let jobs = serde_json::to_value(state.jobs.lock().unwrap().list().unwrap()).unwrap();
    let proposals = proposal_page(&state);
    let first_source = register_local_source(&state, &first_root).unwrap();
    let second_source = register_local_source(&state, &second_root).unwrap();
    assert_eq!(first_source.display_name, second_source.display_name);
    assert_ne!(first_source.source_id, second_source.source_id);
    assert_ne!(first_source.repo_key, second_source.repo_key);
    let first = parse(&state, &first_source, || {});
    let second = parse(&state, &second_source, || {});
    let first_capture = first.input.capture.as_ref().unwrap();
    let second_capture = second.input.capture.as_ref().unwrap();
    assert_ne!(first_capture.id(), second_capture.id());
    assert_eq!(
        first_capture.file("source.ts").unwrap().reference().digest,
        second_capture.file("source.ts").unwrap().reference().digest,
    );
    publish(&state, &first_source, &first);
    publish(&state, &second_source, &second);
    assert_historical_stage(&state, &app_data, &historical);
    assert_eq!(proposal_page(&state), proposals);
    let first_fact = FactKey::from_node(&rule_node(&first));
    let second_fact = FactKey::from_node(&rule_node(&second));
    let second_description = primary_source::describe(&state, &second_fact).unwrap();
    assert!(primary_source::read(&state, &first_fact, &second_description.receipt_id, 0).is_err());
    drop(first);
    drop(second);
    let old_preview = primary_source::preview(&state, &first_source.source_id).unwrap();
    std::fs::write(first_root.join("source.ts"), STABLE_B).unwrap();
    let changed = parse(&state, &first_source, || {});
    publish(&state, &first_source, &changed);
    assert_historical_stage(&state, &app_data, &historical);
    drop(changed);
    assert!(
        primary_source::forget(&state, &first_source.source_id, &old_preview.fingerprint).is_err()
    );
    let preview = primary_source::preview(&state, &first_source.source_id).unwrap();
    assert_eq!(preview.captures, 2);
    assert!(preview.current_references > 0);
    assert!(preview.historical_references > 0);
    let first_description = primary_source::describe(&state, &first_fact).unwrap();
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    let bindings = state
        .graph
        .lock()
        .unwrap()
        .source_bindings_for_repo(&first_source.repo_key)
        .unwrap();
    let receipts = receipt_rows(&app_data);

    // A fresh shared guard held by actual acquisition excludes forgetting. Drop
    // must explicitly unlock; an unrelated test fork may inherit a file handle.
    // No sleeps/retries mask an accidentally retained lock after this drop.
    let active = state
        .primary_sources
        .prepare(&first_source, &first_root, &["client".into()])
        .unwrap();
    assert!(primary_source::forget(&state, &first_source.source_id, &preview.fingerprint).is_err());
    drop(active);
    assert_eq!(
        primary_source::forget(&state, &first_source.source_id, &preview.fingerprint).unwrap(),
        preview.captures as u64
    );
    let forgotten = primary_source::preview(&state, &first_source.source_id).unwrap();
    assert_eq!(
        (forgotten.captures, forgotten.files, forgotten.bytes),
        (0, 0, 0)
    );
    assert_eq!(forgotten.receipts, 0);
    let second_receipts: Vec<_> = receipts
        .into_iter()
        .filter(|(_, source_id, ..)| source_id == &second_source.source_id)
        .collect();
    assert_eq!(receipt_rows(&app_data), second_receipts);
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        snapshot
    );
    assert_eq!(
        state
            .graph
            .lock()
            .unwrap()
            .source_bindings_for_repo(&first_source.repo_key)
            .unwrap(),
        bindings
    );
    assert!(primary_source::read(&state, &first_fact, &first_description.receipt_id, 0).is_err());
    assert_original_ranges(&state, &second_description, STABLE_A);
    assert_eq!(
        serde_json::to_value(state.jobs.lock().unwrap().list().unwrap()).unwrap(),
        jobs
    );
    assert_eq!(proposal_page(&state), proposals);
    assert_historical_stage(&state, &app_data, &historical);
    drop(state);

    let state = app_state(&app_data);
    assert_original_ranges(&state, &second_description, STABLE_A);
    assert!(primary_source::read(&state, &first_fact, &first_description.receipt_id, 0).is_err());
    assert_eq!(receipt_rows(&app_data), second_receipts);
    assert_eq!(
        serde_json::to_value(state.jobs.lock().unwrap().list().unwrap()).unwrap(),
        jobs
    );
    assert_eq!(proposal_page(&state), proposals);
    assert_historical_stage(&state, &app_data, &historical);
}

#[test]
fn primary_source_collapsed_relation_publishes_unbound_without_digest_failure() {
    // AC-0203 (#456 review): when a receipt-bound relation also occurs at a
    // second site, the stored edge unions both spans. No single receipt
    // attested that fact, so publication succeeds with the edge unbound while
    // the rule node keeps its receipt.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let mut parsed = parse(&state, &source, || {});
    let rule = rule_node(&parsed);
    let position = parsed
        .extraction
        .edges
        .iter()
        .position(|edge| edge.label == "GOVERNS" && edge.src == rule.id)
        .unwrap();
    let edge = parsed.extraction.edges[position].clone();
    let edge_fact = FactKey::from_edge(&edge);
    assert!(
        parsed
            .receipts
            .iter()
            .any(|receipt| receipt.matches_edge(&edge))
    );
    let mut earlier = edge.clone();
    let span = &mut earlier.props["prov"]["evidence"][0];
    let (start, end) = (
        span["byte_start"].as_u64().unwrap(),
        span["byte_end"].as_u64().unwrap(),
    );
    assert!(start > 0);
    span["byte_start"] = json!(start - 1);
    span["byte_end"] = json!(end - 1);
    // The original stays the last occurrence, so it still supplies the props.
    parsed.extraction.edges.insert(position, earlier);
    let bindings = publish(&state, &source, &parsed);
    assert!(!bindings.iter().any(|binding| binding.fact == edge_fact));
    assert!(
        bindings
            .iter()
            .any(|binding| binding.fact == FactKey::from_node(&rule))
    );
    let graph = state.graph.lock().unwrap();
    assert!(graph.current_source_binding(&edge_fact).unwrap().is_none());
    let stored = graph
        .all_edges()
        .unwrap()
        .into_iter()
        .find(|candidate| FactKey::from_edge(candidate) == edge_fact)
        .unwrap();
    assert_eq!(
        stored.props["prov"]["evidence"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn primary_source_enrichment_mutation_cannot_reuse_direct_receipt() {
    // AC-0151, AC-0152, AC-0155: unchanged provenance/span is insufficient when
    // a later enrichment changes any complete node or edge property.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    publish(&state, &source, &first);
    let rule = rule_node(&first);
    let fact = FactKey::from_node(&rule);
    let evidence = GuardedExitEvidence::from_value(rule.props["rule"].clone()).unwrap();
    let owner = FactKey::Node {
        id: evidence.owner_id,
    };
    let edge = first
        .extraction
        .edges
        .iter()
        .find(|edge| edge.label == "GOVERNS" && edge.src == rule.id)
        .unwrap()
        .clone();
    let edge_fact = FactKey::from_edge(&edge);
    let old = primary_source::describe(&state, &fact).unwrap();
    primary_source::describe(&state, &edge_fact).unwrap();
    drop(first);
    let mut enriched = parse(&state, &source, || {});
    let final_node = enriched
        .extraction
        .nodes
        .iter_mut()
        .find(|node| node.id == rule.id)
        .unwrap();
    final_node.props["later_enrichment"] = json!(true);
    assert_eq!(final_node.props["prov"], rule.props["prov"]);
    let final_edge = enriched
        .extraction
        .edges
        .iter_mut()
        .find(|candidate| FactKey::from_edge(candidate) == edge_fact)
        .unwrap();
    final_edge.props["later_enrichment"] = json!(true);
    assert_eq!(final_edge.props["prov"], edge.props["prov"]);
    let bindings = publish(&state, &source, &enriched);
    assert!(
        !bindings
            .iter()
            .any(|binding| binding.fact == fact || binding.fact == edge_fact)
    );
    assert!(bindings.iter().any(|binding| binding.fact == owner));
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .current_source_binding(&fact)
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .current_source_binding(&edge_fact)
            .unwrap()
            .is_none()
    );
    assert!(primary_source::read(&state, &fact, &old.receipt_id, 0).is_err());
    assert_original_ranges(
        &state,
        &primary_source::describe(&state, &owner).unwrap(),
        STABLE_A,
    );
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    let stored = snapshot.0.iter().find(|node| node.id == rule.id).unwrap();
    assert_eq!(stored.props["later_enrichment"], json!(true));
    assert_eq!(stored.props["prov"], rule.props["prov"]);
}

#[test]
fn primary_source_persistence_rejects_foreign_receipts_before_publication() {
    // AC-0151: an actual ownership failure in durable receipt persistence must
    // leave existing published facts/current bindings intact. Orphans are allowed.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first");
    let second_root = directory(dir.path(), "second");
    std::fs::write(first_root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(second_root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let first_source = register_local_source(&state, &first_root).unwrap();
    let second_source = register_local_source(&state, &second_root).unwrap();
    let first = parse(&state, &first_source, || {});
    publish(&state, &first_source, &first);
    let fact = FactKey::from_node(&rule_node(&first));
    let old = primary_source::describe(&state, &fact).unwrap();
    let before = state.graph.lock().unwrap().read_snapshot().unwrap();
    let receipts = receipt_rows(&app_data);
    drop(first);
    std::fs::write(first_root.join("source.ts"), STABLE_B).unwrap();
    let changed = parse(&state, &first_source, || {});
    assert!(
        state
            .primary_sources
            .persist(&changed.input, &second_source, &changed.receipts)
            .is_err()
    );
    assert_eq!(state.graph.lock().unwrap().read_snapshot().unwrap(), before);
    assert_eq!(receipt_rows(&app_data), receipts);
    assert_eq!(
        primary_source::describe(&state, &fact).unwrap().receipt_id,
        old.receipt_id
    );
    assert_original_ranges(&state, &old, STABLE_A);
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .source_bindings_for_repo(&second_source.repo_key)
            .unwrap()
            .is_empty()
    );
    publish(&state, &first_source, &changed);
    assert_ne!(
        primary_source::describe(&state, &fact).unwrap().receipt_id,
        old.receipt_id
    );
}

fn header_rows(app_data: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut query = conn
        .prepare("SELECT id,header FROM receipt_headers ORDER BY id")
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn streamed_persistence_rolls_back_rows_written_before_a_later_rejection() {
    // AC-0503: persistence admits receipts one at a time inside one
    // transaction; a receipt rejected after valid ones were written leaves
    // no receipt or header row of the batch, also after reopening.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first");
    let second_root = directory(dir.path(), "second");
    std::fs::write(first_root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(second_root.join("source.ts"), STABLE_B).unwrap();
    let state = app_state(&app_data);
    let first_source = register_local_source(&state, &first_root).unwrap();
    let second_source = register_local_source(&state, &second_root).unwrap();
    let first = parse(&state, &first_source, || {});
    publish(&state, &first_source, &first);
    let (receipts, headers) = (receipt_rows(&app_data), header_rows(&app_data));
    assert!(!receipts.is_empty() && !headers.is_empty());
    drop(first);
    std::fs::write(first_root.join("source.ts"), STABLE_B).unwrap();
    let changed = parse(&state, &first_source, || {});
    let foreign = parse(&state, &second_source, || {});
    // The valid receipts are new rows under a new header, written first.
    assert!(
        changed
            .receipts
            .iter()
            .all(|receipt| !receipts.iter().any(|row| row.0 == receipt.id()))
    );
    let mut batch = changed.receipts.clone();
    batch.push(foreign.receipts[0].clone());
    assert!(
        state
            .primary_sources
            .persist(&changed.input, &first_source, &batch)
            .is_err()
    );
    assert_eq!(
        (receipt_rows(&app_data), header_rows(&app_data)),
        (receipts.clone(), headers.clone())
    );
    drop(foreign);
    let changed_receipts = changed.receipts.clone();
    drop(changed);
    drop(state);
    let reopened = app_state(&app_data);
    assert_eq!(
        (receipt_rows(&app_data), header_rows(&app_data)),
        (receipts.clone(), headers)
    );
    // Without the rejected receipt the same valid receipts are admitted, so
    // they were written, then rolled back.
    let again = parse(&reopened, &first_source, || {});
    assert_eq!(again.receipts, changed_receipts);
    let retained = reopened
        .primary_sources
        .persist(&again.input, &first_source, &again.receipts)
        .unwrap();
    drop(retained);
    assert!(receipt_rows(&app_data).len() > receipts.len());
}

fn retain_staged_proposal_reference(
    state: &AppState,
    source: &RegisteredSource,
    receipt: &Receipt,
) -> agents::StagedProposal {
    use agents::*;
    use core_prov::EvidenceRef;
    let evidence = |id: &str, path: &str, text: &str| AgentEvidence {
        id: id.into(),
        source: EvidenceRef {
            repo: source.repo_key.clone(),
            path: path.into(),
            byte_start: 0,
            byte_end: text.len() as u64,
            commit_sha: "workdir".into(),
        },
        text: text.into(),
    };
    let task = AgentTask {
        action_id: "escalate:prepared".into(),
        gap_id: "gap:resolution".into(),
        source_id: "symbol:source".into(),
        edge_label: "CALLS".into(),
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids: vec!["source".into(), "gap".into()],
        evidence: vec![
            evidence(
                "source",
                &receipt.file().path,
                "source private marker 73194",
            ),
            evidence("gap", "gap.ts", "uncited private marker 76213"),
            evidence("target", "target.ts", "target private marker 97814"),
        ],
        candidates: vec![AgentCandidate {
            node_id: "symbol:target".into(),
            label: "Symbol".into(),
            summary: "unpublished candidate summary 87613".into(),
            evidence_ids: vec!["target".into()],
        }],
    };
    let fact = |id: &str| TaskFactKey::Node { id: id.into() };
    let digest = format!("node-v1:{}", "d".repeat(64));
    let mut selected_facts = vec![
        TaskFactSelection {
            fact: fact(&task.source_id),
            fact_digest: receipt.fact_digest().to_owned(),
            binding: Some(TaskSourceAssociation {
                repo_key: source.repo_key.clone(),
                receipt_id: receipt.id().to_owned(),
                emitted_fact_digest: receipt.fact_digest().to_owned(),
            }),
        },
        TaskFactSelection {
            fact: fact(&task.gap_id),
            fact_digest: digest.clone(),
            binding: None,
        },
        TaskFactSelection {
            fact: fact(&task.candidates[0].node_id),
            fact_digest: digest,
            binding: None,
        },
        TaskFactSelection {
            fact: TaskFactKey::Edge {
                source: task.source_id.clone(),
                label: task.edge_label.clone(),
                destination: task.gap_id.clone(),
            },
            fact_digest: format!("edge-v1:{}", "e".repeat(64)),
            binding: None,
        },
    ];
    selected_facts.sort_by(|a, b| a.fact.cmp(&b.fact));
    let snapshot_id = format!("context-v1:{}", "f".repeat(64));
    let source_basis = TaskSourceBasisV2 {
        schema_version: 2,
        graph_snapshot_id: snapshot_id.clone(),
        selected_facts,
        evidence: vec![
            TaskEvidenceOrigin {
                evidence_id: "source".into(),
                fact: fact(&task.source_id),
                role: TaskRangeRole::Provenance,
                index: 0,
                origin: TaskEvidenceKind::CapturedPrimarySource {
                    registered_source_id: source.source_id.clone(),
                    receipt_id: receipt.id().to_owned(),
                    receipt_inventory_index: 0,
                    captured: TaskCaptureSpanRef {
                        file: TaskCaptureFileRef {
                            source_id: source.source_id.clone(),
                            capture_id: receipt.file().capture_id.clone(),
                            path: receipt.file().path.clone(),
                            digest: receipt.file().digest.clone(),
                            byte_len: receipt.file().byte_len,
                        },
                        byte_start: 0,
                        byte_end: task.evidence[0].text.len() as u64,
                    },
                    scope: PrimarySourceScope::PrimarySourceOnly,
                    input_closure: InputClosureStatus::InputClosureNotEstablished,
                },
            },
            TaskEvidenceOrigin {
                evidence_id: "gap".into(),
                fact: fact(&task.gap_id),
                role: TaskRangeRole::Provenance,
                index: 0,
                origin: TaskEvidenceKind::WorkingTreeUnverified,
            },
            TaskEvidenceOrigin {
                evidence_id: "target".into(),
                fact: fact(&task.candidates[0].node_id),
                role: TaskRangeRole::Provenance,
                index: 0,
                origin: TaskEvidenceKind::WorkingTreeUnverified,
            },
        ],
        selection: SelectionReport {
            metadata_lookahead: 3,
            acquisition_attempts: 3,
            supplied_evidence: 3,
            supplied_candidates: 1,
            captured_validation_bytes: receipt.file().byte_len,
            limits: SelectionLimits::default(),
            omissions: vec![],
            metadata_preselected_not_read: 0,
            unread_tail: false,
            stop_reason: SelectionStopReason::NeighborhoodExhausted,
        },
    };
    struct PreparedFixtureProvider;
    impl llm::LlmProvider for PreparedFixtureProvider {
        fn id(&self) -> &str {
            "fixture:prepared-proposal"
        }
        fn locality(&self) -> llm::Locality {
            llm::Locality::Local
        }
        fn capabilities(&self) -> llm::ProviderCaps {
            llm::ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
            Err(llm::ProviderError::Unsupported("embeddings"))
        }
        fn complete(
            &self,
            _: &llm::ProviderCompletionRequest,
        ) -> Result<llm::Completion, llm::ProviderError> {
            Ok(llm::Completion {
                text: json!({
                    "target_id": "symbol:target",
                    "annotation": "Possible target link",
                    "citations": ["source", "target"]
                })
                .to_string(),
            })
        }
    }
    let prepared = PreparedAgentTask::new(task, source_basis).unwrap();
    let proposal = AgentBroker::bounded_default()
        .propose_prepared(
            &PreparedFixtureProvider,
            &llm::EgressFirewall::new(llm::EgressPolicy::local_only()),
            &prepared,
            None,
        )
        .unwrap();
    state
        .proposals
        .lock()
        .unwrap()
        .stage_prepared(&prepared, &proposal, 1, &snapshot_id)
        .unwrap()
}

fn receipt_store_accounting(app_data: &Path) -> (u64, u64) {
    let conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    let bytes: i64 = conn
        .query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM receipts)
                  + (SELECT coalesce(sum(length(CAST(header AS BLOB))),0) FROM receipt_headers)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    (count as u64, bytes as u64)
}

#[test]
fn primary_source_forget_reclaims_unpinned_and_retains_pinned_receipts_as_capture_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    assert!(parsed.receipts.len() >= 3);
    let bindings = publish(&state, &source, &parsed);
    let rule_fact = FactKey::from_node(&rule_node(&parsed));
    let inv_binding = bindings
        .iter()
        .find(|binding| binding.fact == rule_fact)
        .unwrap()
        .clone();
    let staged_receipt = parsed
        .receipts
        .iter()
        .find(|receipt| {
            receipt.id() != inv_binding.receipt_id
                && matches!(receipt.fact_key(), FactKey::Node { .. })
        })
        .unwrap()
        .clone();
    let unpinned_receipts: Vec<Receipt> = parsed
        .receipts
        .iter()
        .filter(|receipt| {
            receipt.id() != inv_binding.receipt_id && receipt.id() != staged_receipt.id()
        })
        .cloned()
        .collect();
    assert!(!unpinned_receipts.is_empty());
    let before_conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let reclaimed_bytes: u64 = unpinned_receipts
        .iter()
        .map(|receipt| {
            before_conn
                .query_row(
                    "SELECT length(CAST(payload AS BLOB)) FROM receipts WHERE id = ?1",
                    [receipt.id()],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap() as u64
        })
        .sum();
    drop(before_conn);
    let (count_before, bytes_before) = receipt_store_accounting(&app_data);
    assert_eq!(count_before, parsed.receipts.len() as u64);
    for receipt in &parsed.receipts {
        assert_eq!(
            state
                .primary_sources
                .receipts
                .lock()
                .unwrap()
                .receipt_status(receipt.id())
                .unwrap(),
            primary_source::ReceiptStatus::Present
        );
    }
    drop(parsed);

    retain_investigation_selection(&state, &source, &inv_binding, "reclaim-inv-pin");
    retain_staged_proposal_reference(&state, &source, &staged_receipt);

    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(preview.investigation_references, 1);
    assert_eq!(preview.staged_references, 1);
    assert_eq!(preview.receipts, count_before as usize);

    assert_eq!(
        primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap(),
        preview.captures as u64
    );

    let (count_after, bytes_after) = receipt_store_accounting(&app_data);
    assert_eq!(count_after, 2);
    assert_eq!(count_before - count_after, unpinned_receipts.len() as u64);
    assert_eq!(bytes_before - bytes_after, reclaimed_bytes);

    let conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stored_pinned: Vec<(String, i64)> = conn
        .prepare("SELECT id, capture_forgotten_tombstone IS NOT NULL FROM receipts ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut expected_pinned = vec![
        (inv_binding.receipt_id.clone(), 1),
        (staged_receipt.id().to_owned(), 1),
    ];
    expected_pinned.sort();
    assert_eq!(stored_pinned, expected_pinned);
    stored_pinned.clear();

    let receipts_guard = state.primary_sources.receipts.lock().unwrap();
    let inv_status = receipts_guard
        .receipt_status(&inv_binding.receipt_id)
        .unwrap();
    let staged_status = receipts_guard.receipt_status(staged_receipt.id()).unwrap();
    let tombstone = match &inv_status {
        primary_source::ReceiptStatus::CaptureForgotten { tombstone } => tombstone.clone(),
        other => panic!("expected CaptureForgotten, got {other:?}"),
    };
    assert_eq!(
        staged_status,
        primary_source::ReceiptStatus::CaptureForgotten {
            tombstone: tombstone.clone()
        }
    );
    assert_eq!(tombstone.source_id, source.source_id);
    assert_eq!(tombstone.receipts_reclaimed, unpinned_receipts.len() as u64);
    assert_eq!(tombstone.receipts_retained_pinned, 2);
    assert_eq!(tombstone.forgotten_at.len(), 20);
    assert!(tombstone.forgotten_at.ends_with('Z'));

    for unpinned in &unpinned_receipts {
        assert_eq!(
            receipts_guard.receipt_status(unpinned.id()).unwrap(),
            primary_source::ReceiptStatus::Reclaimed {
                tombstone: tombstone.clone()
            }
        );
    }
}

#[test]
fn primary_source_forget_retry_after_reclaim_failure_completes_reclaim() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let all_ids: Vec<String> = parsed.receipts.iter().map(|r| r.id().to_owned()).collect();
    let bindings = publish(&state, &source, &parsed);
    let rule_fact = FactKey::from_node(&rule_node(&parsed));
    let pinned_binding = bindings
        .into_iter()
        .find(|binding| binding.fact == rule_fact)
        .unwrap();
    drop(parsed);

    retain_investigation_selection(&state, &source, &pinned_binding, "retry-inv-pin");
    let first_preview = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(first_preview.captures, 1);
    assert_eq!(first_preview.receipts, all_ids.len());

    state
        .primary_sources
        .receipts
        .lock()
        .unwrap()
        .fail_next_reclaim = true;
    let err = primary_source::forget(&state, &source.source_id, &first_preview.fingerprint)
        .expect_err("injected reclaim failure must fail forget");
    assert_eq!(
        err,
        "Retained source storage is unavailable or invalid; no source was substituted."
    );

    // Captures were deleted first, but receipts remain untouched until retry.
    let retry_preview = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(retry_preview.captures, 0);
    assert!(retry_preview.capture_ids.is_empty());
    assert_eq!(retry_preview.receipts, all_ids.len());
    assert_ne!(retry_preview.fingerprint, first_preview.fingerprint);
    for id in &all_ids {
        assert_eq!(
            state
                .primary_sources
                .receipts
                .lock()
                .unwrap()
                .receipt_status(id)
                .unwrap(),
            primary_source::ReceiptStatus::Present
        );
    }

    assert_eq!(
        primary_source::forget(&state, &source.source_id, &retry_preview.fingerprint).unwrap(),
        0
    );
    let after_retry = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(after_retry.captures, 0);
    assert_eq!(after_retry.receipts, 1);

    let receipts_guard = state.primary_sources.receipts.lock().unwrap();
    for id in &all_ids {
        let status = receipts_guard.receipt_status(id).unwrap();
        if id == &pinned_binding.receipt_id {
            assert!(matches!(
                status,
                primary_source::ReceiptStatus::CaptureForgotten { .. }
            ));
        } else {
            assert!(matches!(
                status,
                primary_source::ReceiptStatus::Reclaimed { .. }
            ));
        }
    }
}

#[test]
fn primary_source_reclaim_ledger_prunes_oldest_rows_and_orphaned_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first");
    let second_root = directory(dir.path(), "second");
    let third_root = directory(dir.path(), "third");
    for root in [&first_root, &second_root, &third_root] {
        std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    }
    let state = app_state(&app_data);
    let first_source = register_local_source(&state, &first_root).unwrap();
    let second_source = register_local_source(&state, &second_root).unwrap();
    let third_source = register_local_source(&state, &third_root).unwrap();

    let first_parsed = parse(&state, &first_source, || {});
    publish(&state, &first_source, &first_parsed);
    let mut first_ids: Vec<String> = first_parsed
        .receipts
        .iter()
        .map(|r| r.id().to_owned())
        .collect();
    first_ids.sort();
    // The rule, its owner, GOVERNS and the File node (AC-1019).
    assert_eq!(first_ids.len(), 4);
    drop(first_parsed);

    // Narrow capacity to 4 so the reclaim ledger holds at most 4 rows.
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 4;

    let preview1 = primary_source::preview(&state, &first_source.source_id).unwrap();
    primary_source::forget(&state, &first_source.source_id, &preview1.fingerprint).unwrap();
    for id in &first_ids {
        assert!(matches!(
            state
                .primary_sources
                .receipts
                .lock()
                .unwrap()
                .receipt_status(id)
                .unwrap(),
            primary_source::ReceiptStatus::Reclaimed { .. }
        ));
    }

    // Second source: 1 pinned receipt + 3 unpinned receipts -> pushes reclaim ledger from 4 to 7,
    // pruning the 3 oldest rows (first_ids[0..3]) while keeping first_ids[3].
    let second_parsed = parse(&state, &second_source, || {});
    let second_bindings = publish(&state, &second_source, &second_parsed);
    let second_rule = FactKey::from_node(&rule_node(&second_parsed));
    let second_pinned = second_bindings
        .into_iter()
        .find(|b| b.fact == second_rule)
        .unwrap();
    let mut second_unpinned: Vec<String> = second_parsed
        .receipts
        .iter()
        .map(|r| r.id().to_owned())
        .filter(|id| id != &second_pinned.receipt_id)
        .collect();
    second_unpinned.sort();
    assert_eq!(second_unpinned.len(), 3);
    drop(second_parsed);

    retain_investigation_selection(&state, &second_source, &second_pinned, "prune-second-pin");
    let preview2 = primary_source::preview(&state, &second_source.source_id).unwrap();
    primary_source::forget(&state, &second_source.source_id, &preview2.fingerprint).unwrap();

    {
        let receipts_guard = state.primary_sources.receipts.lock().unwrap();
        assert_eq!(
            receipts_guard.receipt_status(&first_ids[0]).unwrap(),
            primary_source::ReceiptStatus::Unknown
        );
        assert_eq!(
            receipts_guard.receipt_status(&first_ids[1]).unwrap(),
            primary_source::ReceiptStatus::Unknown
        );
        assert_eq!(
            receipts_guard.receipt_status(&first_ids[2]).unwrap(),
            primary_source::ReceiptStatus::Unknown
        );
        assert!(matches!(
            receipts_guard.receipt_status(&first_ids[3]).unwrap(),
            primary_source::ReceiptStatus::Reclaimed { .. }
        ));
        assert!(matches!(
            receipts_guard
                .receipt_status(&second_pinned.receipt_id)
                .unwrap(),
            primary_source::ReceiptStatus::CaptureForgotten { .. }
        ));
        for id in &second_unpinned {
            assert!(matches!(
                receipts_guard.receipt_status(id).unwrap(),
                primary_source::ReceiptStatus::Reclaimed { .. }
            ));
        }
    }

    // Third source: 4 unpinned receipts (raise capacity.0 to 5 during persist because second_pinned
    // occupies 1 slot in receipts, then narrow capacity.0 back to 4 before forget).
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 5;
    let third_parsed = parse(&state, &third_source, || {});
    publish(&state, &third_source, &third_parsed);
    let mut third_ids: Vec<String> = third_parsed
        .receipts
        .iter()
        .map(|r| r.id().to_owned())
        .collect();
    third_ids.sort();
    assert_eq!(third_ids.len(), 4);
    drop(third_parsed);
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 4;

    let preview3 = primary_source::preview(&state, &third_source.source_id).unwrap();
    primary_source::forget(&state, &third_source.source_id, &preview3.fingerprint).unwrap();

    {
        let receipts_guard = state.primary_sources.receipts.lock().unwrap();
        for id in &first_ids {
            assert_eq!(
                receipts_guard.receipt_status(id).unwrap(),
                primary_source::ReceiptStatus::Unknown
            );
        }
        for id in &second_unpinned {
            assert_eq!(
                receipts_guard.receipt_status(id).unwrap(),
                primary_source::ReceiptStatus::Unknown
            );
        }
        // second_source still has a pinned capture_forgotten receipt, so its tombstone survives.
        assert!(matches!(
            receipts_guard
                .receipt_status(&second_pinned.receipt_id)
                .unwrap(),
            primary_source::ReceiptStatus::CaptureForgotten { .. }
        ));
        for id in &third_ids {
            assert!(matches!(
                receipts_guard.receipt_status(id).unwrap(),
                primary_source::ReceiptStatus::Reclaimed { .. }
            ));
        }
    }

    // first_source's tombstone is now completely orphaned and must be deleted.
    let conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tombstone_sources: Vec<String> = conn
        .prepare("SELECT source_id FROM receipt_tombstones ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tombstone_sources,
        vec![second_source.source_id, third_source.source_id]
    );
}

#[test]
fn primary_source_forget_then_reimport_same_directory_preserves_old_and_new_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    let first_bindings = publish(&state, &source, &first);
    let rule_fact = FactKey::from_node(&rule_node(&first));
    let pinned_binding = first_bindings
        .into_iter()
        .find(|binding| binding.fact == rule_fact)
        .unwrap();
    let old_unpinned_ids: Vec<String> = first
        .receipts
        .iter()
        .map(|receipt| receipt.id().to_owned())
        .filter(|id| id != &pinned_binding.receipt_id)
        .collect();
    assert!(!old_unpinned_ids.is_empty());
    drop(first);

    retain_investigation_selection(&state, &source, &pinned_binding, "reimport-pin");
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();

    // Re-import the same directory after updating source.ts.
    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let reimported_source = register_local_source(&state, &root).unwrap();
    assert_eq!(reimported_source.source_id, source.source_id);
    let second = parse(&state, &reimported_source, || {});
    publish(&state, &reimported_source, &second);
    let new_ids: Vec<String> = second
        .receipts
        .iter()
        .map(|receipt| receipt.id().to_owned())
        .collect();
    assert!(!new_ids.is_empty());
    drop(second);

    let receipts_guard = state.primary_sources.receipts.lock().unwrap();
    for new_id in &new_ids {
        assert_ne!(new_id, &pinned_binding.receipt_id);
        assert!(!old_unpinned_ids.contains(new_id));
        assert_eq!(
            receipts_guard.receipt_status(new_id).unwrap(),
            primary_source::ReceiptStatus::Present
        );
    }
    assert!(matches!(
        receipts_guard
            .receipt_status(&pinned_binding.receipt_id)
            .unwrap(),
        primary_source::ReceiptStatus::CaptureForgotten { .. }
    ));
    for old_id in &old_unpinned_ids {
        assert!(matches!(
            receipts_guard.receipt_status(old_id).unwrap(),
            primary_source::ReceiptStatus::Reclaimed { .. }
        ));
    }
    let unrelated = format!("ts-primary-v2:{}", "0".repeat(64));
    assert_eq!(
        receipts_guard.receipt_status(&unrelated).unwrap(),
        primary_source::ReceiptStatus::Unknown
    );
}

fn retain_work_packet(state: &AppState, fact: &FactKey) -> ::work_packets::PacketDetail {
    let selection = crate::work_packets::prepare_selection(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        std::slice::from_ref(fact),
        &[],
    )
    .unwrap();
    let request = serde_json::json!({
        "expected_snapshot_id": selection.snapshot_id,
        "facts": selection.facts,
    });
    crate::work_packets::generate(state, &state.work_packets, &request.to_string()).unwrap()
}

// AC-0301: a receipt referenced only by a saved work packet survives forget as
// capture-forgotten and the saved packet still resolves its excerpt receipt.
#[test]
fn primary_source_work_packet_pin_survives_forget_and_resolves_excerpt_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    assert!(parsed.receipts.len() >= 2);
    let bindings = publish(&state, &source, &parsed);
    let rule_fact = FactKey::from_node(&rule_node(&parsed));
    let pinned_binding = bindings
        .iter()
        .find(|binding| binding.fact == rule_fact)
        .unwrap()
        .clone();
    let unpinned_ids: Vec<String> = parsed
        .receipts
        .iter()
        .map(|receipt| receipt.id().to_owned())
        .filter(|id| id != &pinned_binding.receipt_id)
        .collect();
    assert!(!unpinned_ids.is_empty());
    drop(parsed);

    let packet = retain_work_packet(&state, &rule_fact);
    assert!(!packet.body.evidence.source_excerpts.is_empty());
    assert_eq!(
        packet.body.evidence.source_excerpts[0].receipt_id,
        pinned_binding.receipt_id
    );

    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(preview.investigation_references, 0);
    assert_eq!(preview.staged_references, 0);
    assert_eq!(preview.packet_references, 1);

    assert_eq!(
        primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap(),
        preview.captures as u64
    );

    let (count_after, _) = receipt_store_accounting(&app_data);
    assert_eq!(count_after, 1);

    {
        let receipts_guard = state.primary_sources.receipts.lock().unwrap();
        let tombstone = match receipts_guard
            .receipt_status(&pinned_binding.receipt_id)
            .unwrap()
        {
            primary_source::ReceiptStatus::CaptureForgotten { tombstone } => tombstone,
            other => panic!("expected CaptureForgotten, got {other:?}"),
        };
        assert_eq!(tombstone.receipts_retained_pinned, 1);
        assert_eq!(tombstone.receipts_reclaimed, unpinned_ids.len() as u64);
        for unpinned in &unpinned_ids {
            assert_eq!(
                receipts_guard.receipt_status(unpinned).unwrap(),
                primary_source::ReceiptStatus::Reclaimed {
                    tombstone: tombstone.clone()
                }
            );
        }
    }

    let saved = state
        .work_packets
        .store
        .lock()
        .unwrap()
        .get(&packet.packet_id)
        .unwrap();
    let excerpt = &saved.body.evidence.source_excerpts[0];
    let fact = &saved.body.evidence.facts[0];
    let guard = state.primary_sources.task_guard(&source.source_id).unwrap();
    let resolved = state
        .primary_sources
        .task_receipt(
            &guard,
            &fact.reference,
            &fact.expected_digest,
            &pinned_binding.repo_key,
            &excerpt.receipt_id,
        )
        .unwrap();
    assert_eq!(resolved.id(), excerpt.receipt_id);
    assert_eq!(
        state.primary_sources.task_text(&guard, &resolved, 0),
        Err(primary_source::PinnedReadError::Unavailable)
    );
}

// AC-0301: saving a work packet after preview changes the fingerprint so a
// stale confirmation is refused.
#[test]
fn primary_source_work_packet_saved_after_preview_invalidates_forget_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let total_receipts = parsed.receipts.len() as u64;
    publish(&state, &source, &parsed);
    let rule_fact = FactKey::from_node(&rule_node(&parsed));
    drop(parsed);

    let before = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(before.packet_references, 0);

    let _packet = retain_work_packet(&state, &rule_fact);

    let after = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(after.packet_references, 1);
    assert_ne!(after.fingerprint, before.fingerprint);

    let err = primary_source::forget(&state, &source.source_id, &before.fingerprint)
        .expect_err("stale preview fingerprint must be refused");
    assert_eq!(
        err,
        "Retained source changed; refresh the preview before forgetting."
    );
    assert_eq!(receipt_store_accounting(&app_data).0, total_receipts);

    assert_eq!(
        primary_source::forget(&state, &source.source_id, &after.fingerprint).unwrap(),
        after.captures as u64
    );
    assert_eq!(receipt_store_accounting(&app_data).0, 1);
}

// AC-0301: while a forget holds the source's exclusive guard, a work packet
// citing that source is refused rather than saved without a pin, so the
// forget can never reclaim a receipt the packet was just written to cite.
#[test]
fn primary_source_work_packet_is_refused_while_forget_holds_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    publish(&state, &source, &parsed);
    let rule_fact = FactKey::from_node(&rule_node(&parsed));
    drop(parsed);
    let selection = crate::work_packets::prepare_selection(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        std::slice::from_ref(&rule_fact),
        &[],
    )
    .unwrap();
    let request = serde_json::json!({
        "expected_snapshot_id": selection.snapshot_id,
        "facts": selection.facts,
    });

    let forgetting = state
        .primary_sources
        .exclusive_guard_for_test(&source.source_id);
    let refused = crate::work_packets::generate(&state, &state.work_packets, &request.to_string())
        .unwrap_err();
    assert_eq!(refused.code, "selection_conflict");
    assert!(
        state
            .work_packets
            .store
            .lock()
            .unwrap()
            .list(10)
            .unwrap()
            .is_empty()
    );
    drop(forgetting);

    crate::work_packets::generate(&state, &state.work_packets, &request.to_string()).unwrap();
    assert_eq!(
        primary_source::preview(&state, &source.source_id)
            .unwrap()
            .packet_references,
        1
    );
}

#[test]
fn mcp_retained_span_uses_session_receipt_and_refuses_changed_or_over_budget_evidence() {
    // AC-0288: actual capture/publication/receipt read; no checkout fallback.
    use crate::mcp_reads::{ReadError, ReadSession, evidence::EvidenceLimits};
    let temp = tempfile::tempdir().unwrap();
    let app_data = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let rule = rule_node(&parsed);
    publish(&state, &source, &parsed);
    let description = primary_source::describe(&state, &FactKey::from_node(&rule)).unwrap();
    let fact = agents::TaskFactKey::Node {
        id: rule.id.clone(),
    };
    let (target, guard) = crate::investigations::inspection::resolve(
        &state,
        &fact,
        &description.graph_snapshot_id,
        &description.receipt_id,
        0,
    )
    .unwrap();
    drop(guard);
    let session = ReadSession::acquire(
        &state.graph,
        &state.decisions,
        || {
            Ok(vec![context_hub::coverage::ScopedRepo {
                repo_key: source.repo_key.clone(),
                registered_source_id: Some(source.source_id.clone()),
                config_hash: "fixture".into(),
                layer_hints: None,
            }])
        },
        core_graph::SnapshotReadLimits::default(),
        core_graph::coverage::CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let row_count = {
        let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
        nodes.len() + edges.len()
    };
    let bounded_session = ReadSession::acquire(
        &state.graph,
        &state.decisions,
        || Ok(vec![]),
        core_graph::SnapshotReadLimits {
            max_rows: row_count,
            ..core_graph::SnapshotReadLimits::default()
        },
        core_graph::coverage::CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let original_bytes = {
        let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
        serde_json::to_vec(&graph).unwrap().len()
    };
    let byte_bounded_session = ReadSession::acquire(
        &state.graph,
        &state.decisions,
        || Ok(vec![]),
        core_graph::SnapshotReadLimits {
            max_bytes: original_bytes,
            ..core_graph::SnapshotReadLimits::default()
        },
        core_graph::coverage::CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let byte_basis = byte_bounded_session.basis_id().unwrap();
    let bounded_basis = bounded_session.basis_id().unwrap();
    assert_eq!(
        bounded_session
            .describe_evidence(&state, &bounded_basis, &fact, 0, 65536)
            .unwrap()
            .target,
        target
    );
    let basis = session.basis_id().unwrap();
    let described = session
        .describe_evidence(&state, &basis, &fact, 0, 65536)
        .unwrap();
    assert_eq!(described.target, target);
    assert!(described.total_ranges > 0);
    assert_eq!(
        session
            .describe_evidence(&state, &basis, &fact, 0, 1)
            .unwrap_err(),
        ReadError::Limit
    );
    // AC-0288: invalid ordinal input must not trigger stale-basis reconnects.
    // Cover the actual receipt boundary as well as the resolver's hard cap.
    for ordinal in [described.total_ranges, 1024, usize::MAX] {
        assert_eq!(
            session
                .describe_evidence(&state, &basis, &fact, ordinal, 65536)
                .unwrap_err(),
            ReadError::InvalidRequest
        );
    }
    // Failed selection leaves the same immutable session usable.
    assert_eq!(
        session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap()
            .target,
        target
    );
    let limits = || EvidenceLimits {
        max_file_validation_bytes: ORIGINAL.len() as u64,
        max_span_bytes: 8192,
        max_response_bytes: 65536,
    };
    std::fs::write(root.join("source.ts"), "live-replacement-canary").unwrap();
    let response = session
        .read_evidence(&state, &basis, &target, limits())
        .unwrap();
    // AC-0581 / #1754: the retained span is returned redacted, with the count.
    let captured =
        &ORIGINAL[target.captured.byte_start as usize..target.captured.byte_end as usize];
    let (redacted, replacements) = core_redact::redact_text(captured);
    assert!(replacements > 0, "the fixture span carries a credential");
    assert_ne!(response.text, captured);
    assert_eq!(response.text, redacted);
    assert_eq!(response.redaction.replacements, replacements);
    assert_eq!(response.redaction.policy, core_redact::TEXT_POLICY_VERSION);
    assert!(!response.text.contains("primary-password-canary"));
    assert_eq!(response.scope, "primary_source_only");
    assert_eq!(response.input_closure, "input_closure_not_established");
    assert!(!response.text.contains("live-replacement-canary"));
    assert_eq!(
        session
            .read_evidence(&state, "wrong-basis", &target, limits())
            .unwrap_err(),
        ReadError::StaleBasis
    );
    let mut small = limits();
    small.max_file_validation_bytes = 1;
    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, small)
            .unwrap_err(),
        ReadError::Limit
    );
    let mut small = limits();
    small.max_response_bytes = 1;
    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, small)
            .unwrap_err(),
        ReadError::Limit
    );
    let mut forged = target.clone();
    forged.captured.byte_end -= 1;
    assert_eq!(
        session
            .read_evidence(&state, &basis, &forged, limits())
            .unwrap_err(),
        ReadError::InvalidRequest
    );
    forged.source.byte_end = forged.captured.byte_end;
    forged.target_id = forged.content_id().unwrap();
    forged.validate().unwrap();
    assert_eq!(
        session
            .read_evidence(&state, &basis, &forged, limits())
            .unwrap_err(),
        ReadError::StaleBasis
    );
    std::fs::remove_file(root.join("source.ts")).unwrap();
    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, limits())
            .unwrap()
            .text,
        response.text
    );
    // Exercise the real MCP route over the same capture after checkout deletion.
    use crate::mcp_reads::dispatch::{
        DispatchAction, DispatchLimits, Dispatcher, EvidenceWorkLimits,
    };
    use std::num::{NonZeroU64, NonZeroUsize};
    let wire_session = ReadSession::acquire(
        &state.graph,
        &state.decisions,
        || {
            Ok(vec![context_hub::coverage::ScopedRepo {
                repo_key: source.repo_key.clone(),
                registered_source_id: Some(source.source_id.clone()),
                config_hash: "fixture".into(),
                layer_hints: None,
            }])
        },
        core_graph::SnapshotReadLimits::default(),
        core_graph::coverage::CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let mut wire = Dispatcher::with_evidence(
        wire_session,
        "fixture-session".into(),
        DispatchLimits {
            max_response_bytes: NonZeroUsize::new(65536).unwrap(),
            max_request_ids: NonZeroUsize::new(16).unwrap(),
            trace: flowtracer::TraceLimits {
                max_flows: 16,
                max_hops: 64,
                max_output_bytes: 65536,
            },
            compile: spec::SelectedCompileLimits {
                max_bytes: 65536,
                adr: spec::AdrLimits {
                    max_rows: 128,
                    max_output_bytes: 65536,
                },
            },
        },
        &state,
        EvidenceWorkLimits {
            max_file_validation_bytes: NonZeroU64::new(ORIGINAL.len() as u64).unwrap(),
            max_span_bytes: NonZeroU64::new(8192).unwrap(),
        },
    )
    .unwrap();
    let answer = |response| match response {
        DispatchAction::Respond(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    answer(wire.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})));
    assert_eq!(
        wire.receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        DispatchAction::Ignore
    );
    let tools = answer(wire.receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "cartograph_evidence")
    );
    let described = answer(wire.receive(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cartograph_evidence","arguments":{"session_id":"fixture-session","basis_id":basis,"kind":"retained_primary_source","mode":"describe","fact":fact,"ordinal":0,"max_bytes":65536}}})));
    assert_eq!(described["result"]["isError"], false);
    let wire_target = described["result"]["structuredContent"]["description"]["target"].clone();
    assert_eq!(wire_target, serde_json::to_value(&target).unwrap());
    let read_args = json!({"session_id":"fixture-session","basis_id":basis,"kind":"retained_primary_source","mode":"read","target":wire_target,"max_bytes":65536});
    let read = answer(wire.receive(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cartograph_evidence","arguments":read_args}})));
    assert_eq!(read["result"]["isError"], false);
    assert_eq!(
        read["result"]["structuredContent"]["span"]["text"],
        response.text
    );
    let mut unknown = read_args.clone();
    unknown["target"]["source"]["injected"] = json!(true);
    let rejected = answer(wire.receive(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cartograph_evidence","arguments":unknown}})));
    assert_eq!(rejected["error"]["code"], -32602);
    // AC-0288: later graph growth must use the session's admitted limits in
    // both describe and read, rather than the resolver's larger defaults.
    let mut extra = rule.clone();
    extra.id.push_str(":extra-row");
    state.graph.lock().unwrap().put_node(&extra).unwrap();
    assert_eq!(
        bounded_session
            .describe_evidence(&state, &bounded_basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::Limit
    );
    assert_eq!(
        bounded_session
            .read_evidence(&state, &bounded_basis, &target, limits())
            .unwrap_err(),
        ReadError::Limit
    );
    let mut changed = rule;
    changed.props["mcp_changed"] = json!(true);
    state.graph.lock().unwrap().put_node(&changed).unwrap();
    let stale = answer(wire.receive(json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"cartograph_evidence","arguments":read_args}})));
    assert_eq!(stale["result"]["isError"], true);

    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, limits())
            .unwrap_err(),
        ReadError::StaleBasis
    );
    // Changed selected fact properties must hit the session byte preflight
    // before any preliminary unbounded digest lookup or receipt decode.
    changed.props["mcp_oversized"] = json!("x".repeat(original_bytes));
    state.graph.lock().unwrap().put_node(&changed).unwrap();
    assert_eq!(
        byte_bounded_session
            .describe_evidence(&state, &byte_basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::Limit
    );
}

#[test]
fn mcp_evidence_preserves_typed_retention_failures_and_legacy_refresh_message() {
    // AC-0288: production typed absence/corruption/operational evidence, never a
    // guessed reason such as forgotten, omitted, or a working-checkout fallback.
    use crate::investigations::inspection::{InspectionError, resolve, resolve_typed};
    use crate::mcp_reads::{ReadError, ReadSession, evidence::EvidenceLimits};
    let temp = tempfile::tempdir().unwrap();
    let app_data = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let rule = rule_node(&parsed);
    publish(&state, &source, &parsed);
    drop(parsed);
    let fact = agents::TaskFactKey::Node { id: rule.id };
    let session = ReadSession::acquire(
        &state.graph,
        &state.decisions,
        || {
            Ok(vec![context_hub::coverage::ScopedRepo {
                repo_key: source.repo_key.clone(),
                registered_source_id: Some(source.source_id.clone()),
                config_hash: "fixture".into(),
                layer_hints: None,
            }])
        },
        core_graph::SnapshotReadLimits::default(),
        core_graph::coverage::CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let basis = session.basis_id().unwrap();
    let target = session
        .describe_evidence(&state, &basis, &fact, 0, 65536)
        .unwrap()
        .target;
    let limits = || EvidenceLimits {
        max_file_validation_bytes: ORIGINAL.len() as u64,
        max_span_bytes: 8192,
        max_response_bytes: 65536,
    };
    let typed = || {
        resolve_typed(
            &state,
            &fact,
            &target.graph_snapshot_id,
            &target.receipt_id,
            0,
        )
    };
    let legacy = || {
        resolve(
            &state,
            &fact,
            &target.graph_snapshot_id,
            &target.receipt_id,
            0,
        )
    };
    let refresh = resolve(&state, &fact, "wrong-snapshot", &target.receipt_id, 0)
        .err()
        .unwrap();
    assert_eq!(
        refresh,
        "Captured inspection target changed or is unavailable; refresh and select the source again."
    );
    assert_eq!(
        resolve_typed(&state, &fact, "wrong-snapshot", &target.receipt_id, 0)
            .err()
            .unwrap(),
        InspectionError::Stale
    );
    let receipts =
        rusqlite::Connection::open(app_data.join("retained-source/receipts.sqlite")).unwrap();
    let original: String = receipts
        .query_row(
            "SELECT payload FROM receipts WHERE id=?1",
            [&target.receipt_id],
            |row| row.get(0),
        )
        .unwrap();
    receipts
        .execute(
            "UPDATE receipts SET payload=?1 WHERE id=?2",
            rusqlite::params!["corrupt-receipt-canary", target.receipt_id],
        )
        .unwrap();
    assert_eq!(typed().err().unwrap(), InspectionError::Invalid);
    assert_eq!(legacy().err().unwrap(), refresh);
    assert_eq!(
        session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::EvidenceInvalid
    );
    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, limits())
            .unwrap_err(),
        ReadError::EvidenceInvalid
    );
    assert_eq!(ReadError::EvidenceInvalid.to_string(), "evidence_invalid");
    receipts
        .execute(
            "UPDATE receipts SET payload=?1 WHERE id=?2",
            rusqlite::params![original, target.receipt_id],
        )
        .unwrap();
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
    assert_eq!(typed().err().unwrap(), InspectionError::Unavailable);
    assert_eq!(legacy().err().unwrap(), refresh);
    assert_eq!(
        session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::EvidenceForgotten
    );
    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, limits())
            .unwrap_err(),
        ReadError::EvidenceForgotten
    );
    assert_eq!(
        ReadError::EvidenceUnavailable.to_string(),
        "evidence_unavailable"
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _graph = state.graph.lock().unwrap();
            panic!("fixture poisons graph lock");
        }))
        .is_err()
    );
    assert_eq!(typed().err().unwrap(), InspectionError::Operational);
    assert_eq!(legacy().err().unwrap(), refresh);
    assert_eq!(
        session
            .read_evidence(&state, &basis, &target, limits())
            .unwrap_err(),
        ReadError::Storage
    );
}

fn mcp_forgotten_session(
    state: &AppState,
    source: &RegisteredSource,
) -> crate::mcp_reads::ReadSession {
    crate::mcp_reads::ReadSession::acquire(
        &state.graph,
        &state.decisions,
        || {
            Ok(vec![context_hub::coverage::ScopedRepo {
                repo_key: source.repo_key.clone(),
                registered_source_id: Some(source.source_id.clone()),
                config_hash: "forgotten-fixture".into(),
                layer_hints: None,
            }])
        },
        core_graph::SnapshotReadLimits::default(),
        core_graph::coverage::CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap()
}
#[test]
fn mcp_exact_pinned_and_reclaimed_forgotten_receipts_keep_v3_and_basis_boundaries() {
    use crate::mcp_reads::{ReadError, evidence::EvidenceLimits};
    for pinned in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let private = directory(temp.path(), "private");
        let root = directory(temp.path(), "project");
        std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
        let state = app_state(&private);
        let source = register_local_source(&state, &root).unwrap();
        let parsed = parse(&state, &source, || {});
        let rule = rule_node(&parsed);
        let bindings = publish(&state, &source, &parsed);
        let binding = bindings
            .into_iter()
            .find(|b| b.fact == FactKey::from_node(&rule))
            .unwrap();
        assert!(binding.receipt_id.starts_with("ts-primary-v3:"));
        drop(parsed);
        if pinned {
            retain_investigation_selection(&state, &source, &binding, "mcp-forgotten-pin");
        }
        let session = mcp_forgotten_session(&state, &source);
        let basis = session.basis_id().unwrap();
        let fact = agents::TaskFactKey::Node { id: rule.id };
        let target = session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap()
            .target;
        let limits = || EvidenceLimits {
            max_file_validation_bytes: 4096,
            max_span_bytes: 1024,
            max_response_bytes: 65536,
        };
        assert!(
            !session
                .read_evidence(&state, &basis, &target, limits())
                .unwrap()
                .text
                .is_empty()
        );
        let preview = primary_source::preview(&state, &source.source_id).unwrap();
        primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
        let status = state
            .primary_sources
            .receipts
            .lock()
            .unwrap()
            .receipt_status(&binding.receipt_id)
            .unwrap();
        assert!(if pinned {
            matches!(
                status,
                primary_source::ReceiptStatus::CaptureForgotten { .. }
            )
        } else {
            matches!(status, primary_source::ReceiptStatus::Reclaimed { .. })
        });
        // The producer may truncate its capture-id list to [] under its
        // store-wide byte pool. Exact receipt/tombstone identity still proves
        // forgetting; this list is not an absence-completeness oracle.
        let raw =
            rusqlite::Connection::open(private.join("retained-source/receipts.sqlite")).unwrap();
        raw.execute(
            "UPDATE receipt_tombstones SET capture_ids='[]',capture_ids_truncated=1",
            [],
        )
        .unwrap();
        let wrong_guard = state
            .primary_sources
            .task_guard(&format!("src_{}", "f".repeat(32)))
            .unwrap();
        assert_eq!(
            state
                .primary_sources
                .task_binding_forgotten(&wrong_guard, &binding, None)
                .unwrap_err(),
            primary_source::PinnedReadError::Invalid
        );
        drop(wrong_guard);
        assert_eq!(
            session
                .describe_evidence(&state, &basis, &fact, 0, 65536)
                .unwrap_err(),
            ReadError::EvidenceForgotten
        );
        assert_eq!(
            session
                .read_evidence(&state, &basis, &target, limits())
                .unwrap_err(),
            ReadError::EvidenceForgotten
        );
        for field in ["receipt", "digest", "source"] {
            let mut forged = target.clone();
            match field {
                "receipt" => forged.receipt_id = format!("ts-primary-v3:{}", "f".repeat(64)),
                "digest" => forged.fact_digest = format!("node-v1:{}", "f".repeat(64)),
                _ => forged.registered_source_id = format!("src_{}", "f".repeat(32)),
            }
            forged.target_id = forged.content_id().unwrap();
            // Source substitution also has to preserve the target's internal
            // file-source equality to reach association validation.
            if field == "source" {
                forged.captured.file.source_id = forged.registered_source_id.clone();
                forged.target_id = forged.content_id().unwrap();
            }
            assert_eq!(
                session
                    .read_evidence(&state, &basis, &forged, limits())
                    .unwrap_err(),
                ReadError::StaleBasis,
                "{field}"
            );
        }
        assert_eq!(
            session
                .describe_evidence(&state, "wrong-basis", &fact, 0, 65536)
                .unwrap_err(),
            ReadError::StaleBasis
        );
        // Identical recapture positively clears the old forgotten mark; an
        // absent capture alone never overrides a newly Present receipt.
        let identical = parse(&state, &source, || {});
        let identical_bindings = publish(&state, &source, &identical);
        drop(identical);
        assert!(
            identical_bindings
                .iter()
                .any(|b| b.receipt_id == binding.receipt_id)
        );
        let fresh = mcp_forgotten_session(&state, &source);
        let fresh_basis = fresh.basis_id().unwrap();
        assert!(
            fresh
                .describe_evidence(&state, &fresh_basis, &fact, 0, 65536)
                .is_ok()
        );
        // Changed reimport establishes a different basis and a new receipt.
        std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
        let changed = parse(&state, &source, || {});
        publish(&state, &source, &changed);
        drop(changed);
        assert_eq!(
            session
                .read_evidence(&state, &basis, &target, limits())
                .unwrap_err(),
            ReadError::StaleBasis
        );
        let new_session = mcp_forgotten_session(&state, &source);
        let new_basis = new_session.basis_id().unwrap();
        let new_target = new_session
            .describe_evidence(&state, &new_basis, &fact, 0, 65536)
            .unwrap()
            .target;
        assert_ne!(new_target.receipt_id, target.receipt_id);
    }
}
#[test]
fn mcp_unknown_missing_receipt_is_not_forgotten_and_status_errors_are_operational() {
    use crate::mcp_reads::ReadError;
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&private);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let rule = rule_node(&parsed);
    publish(&state, &source, &parsed);
    drop(parsed);
    let session = mcp_forgotten_session(&state, &source);
    let basis = session.basis_id().unwrap();
    let fact = agents::TaskFactKey::Node { id: rule.id };
    let target = session
        .describe_evidence(&state, &basis, &fact, 0, 65536)
        .unwrap()
        .target;
    let raw = rusqlite::Connection::open(private.join("retained-source/receipts.sqlite")).unwrap();
    raw.execute("DELETE FROM receipts WHERE id=?1", [&target.receipt_id])
        .unwrap();
    assert_eq!(
        session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::EvidenceUnavailable
    );
    assert!(matches!(
        state
            .primary_sources
            .receipts
            .lock()
            .unwrap()
            .receipt_status(&target.receipt_id)
            .unwrap(),
        primary_source::ReceiptStatus::Unknown
    ));
    raw.execute_batch("DROP TABLE receipt_reclaims").unwrap();
    assert_eq!(
        session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::Storage
    );
}

#[test]
fn mcp_current_unbound_fact_is_unavailable_without_weakening_stale_or_invalid_checks() {
    // AC-0288: current absence must not collapse into stale or infer omission.
    use crate::mcp_reads::ReadError;
    use core_graph::GraphStore;
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&private);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let rule = rule_node(&parsed);
    publish(&state, &source, &parsed);
    drop(parsed);
    let raw = rusqlite::Connection::open(private.join("graph.db")).unwrap();
    let fact = agents::TaskFactKey::Node {
        id: rule.id.clone(),
    };
    let bound = mcp_forgotten_session(&state, &source);
    let target = bound
        .describe_evidence(&state, &bound.basis_id().unwrap(), &fact, 0, 65536)
        .unwrap()
        .target;
    raw.execute(
        "DELETE FROM source_node_bindings WHERE node_id=?1",
        [&rule.id],
    )
    .unwrap();
    let unbound = mcp_forgotten_session(&state, &source);
    let basis = unbound.basis_id().unwrap();
    assert_eq!(
        unbound
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::EvidenceUnavailable
    );
    // A previously supplied receipt target still requires that exact binding.
    assert_eq!(
        bound
            .read_evidence(
                &state,
                &bound.basis_id().unwrap(),
                &target,
                crate::mcp_reads::evidence::EvidenceLimits {
                    max_file_validation_bytes: 4096,
                    max_span_bytes: 1024,
                    max_response_bytes: 65536
                }
            )
            .unwrap_err(),
        ReadError::StaleBasis
    );
    assert!(matches!(
        crate::investigations::inspection::resolve_typed(
            &state,
            &fact,
            &unbound.basis().recovered_graph_id,
            &target.receipt_id,
            0
        ),
        Err(crate::investigations::inspection::InspectionError::Stale)
    ));
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "unrelated:changed".into(),
            label: "Module".into(),
            props: json!({}),
        })
        .unwrap();
    assert_eq!(
        unbound
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err(),
        ReadError::StaleBasis
    );
    let fresh = mcp_forgotten_session(&state, &source);
    assert_eq!(
        fresh
            .describe_evidence(&state, &fresh.basis_id().unwrap(), &fact, 0, 65536)
            .unwrap_err(),
        ReadError::EvidenceUnavailable
    );
    raw.execute("INSERT INTO source_node_bindings(node_id,repo_key,receipt_id,emitted_fact_digest) VALUES (?1,?2,?3,?4)", rusqlite::params![rule.id,source.repo_key,target.receipt_id,format!("node-v1:{}","0".repeat(64))]).unwrap();
    assert_eq!(
        fresh
            .describe_evidence(&state, &fresh.basis_id().unwrap(), &fact, 0, 65536)
            .unwrap_err(),
        ReadError::EvidenceInvalid
    );
    raw.execute("DELETE FROM edges WHERE src=?1 OR dst=?1", [&rule.id])
        .unwrap();
    raw.execute("DELETE FROM nodes WHERE id=?1", [&rule.id])
        .unwrap();
    assert_eq!(
        fresh
            .describe_evidence(&state, &fresh.basis_id().unwrap(), &fact, 0, 65536)
            .unwrap_err(),
        ReadError::StaleBasis
    );
}

// AC-0288 / T-0288: actual bounded producer omission, not missing-binding inference.
#[test]
fn mcp_real_store_capacity_omission_is_positive_guarded_and_stale_safe() {
    use crate::mcp_reads::ReadError;
    use core_graph::source::{OmissionLookup, ReceiptOmissionKind};
    use tauri::Manager;
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("rules.ts"), "export function alpha(ok: boolean) { if (!ok) return false; return true; }\nexport function beta() { return 2; }\n").unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&private));
    app.manage(crate::PreflightRuns::default());
    let state = app.state::<AppState>();
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 1;
    let source = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(source.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &source.ingest_job_kind()).unwrap();
    run_ingest(&source, &operation, &execution, app.handle(), &state).unwrap();
    let graph = state.graph.lock().unwrap();
    let (nodes, _) = graph.read_snapshot().unwrap();
    let node = nodes
        .into_iter()
        .find(|node| {
            let selected = graph
                .read_source_selection_snapshot(&[FactKey::from_node(node)])
                .unwrap();
            matches!(
                selected.selections[0].omission,
                OmissionLookup::Omitted {
                    kind: ReceiptOmissionKind::StoreCapacity,
                    ..
                }
            )
        })
        .unwrap();
    drop(graph);
    let session = mcp_forgotten_session(&state, &source);
    let basis = session.basis_id().unwrap();
    let fact = agents::TaskFactKey::Node {
        id: node.id.clone(),
    };
    let describe = || {
        session
            .describe_evidence(&state, &basis, &fact, 0, 65536)
            .unwrap_err()
    };
    assert_eq!(
        describe(),
        ReadError::EvidenceOmitted(ReceiptOmissionKind::StoreCapacity)
    );
    assert!(matches!(
        crate::investigations::inspection::resolve_typed(
            &state,
            &fact,
            &session.basis().recovered_graph_id,
            "unused",
            0
        ),
        Err(crate::investigations::inspection::InspectionError::Stale)
    ));
    // An exclusive retention lock prevents positive classification without protection.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            private
                .join("retained-source/locks")
                .join(format!("{}.lock", source.source_id)),
        )
        .unwrap();
    lock.try_lock().unwrap();
    assert_eq!(describe(), ReadError::Storage);
    drop(lock);
    let db_path = private.join("graph.db");
    let changed_node = node.id.clone();
    state
        .graph
        .lock()
        .unwrap()
        .set_source_binding_after_lookup_hook(move || {
            let writer = rusqlite::Connection::open(db_path).unwrap();
            writer
                .execute(
                    "UPDATE source_node_omissions SET kind='rules' WHERE node_id=?1",
                    [&changed_node],
                )
                .unwrap();
            Ok(())
        });
    // Same graph hash, but omission changed between the two guarded observations.
    assert_eq!(describe(), ReadError::StaleBasis);
    assert_eq!(
        describe(),
        ReadError::EvidenceOmitted(ReceiptOmissionKind::Rules)
    );
    let db = rusqlite::Connection::open(private.join("graph.db")).unwrap();
    let bound = state
        .graph
        .lock()
        .unwrap()
        .source_bindings_for_repo(&source.repo_key)
        .unwrap()
        .into_iter()
        .find(|b| matches!(b.fact, FactKey::Node { .. }))
        .unwrap();
    let bound_id = match &bound.fact {
        FactKey::Node { id } => id,
        _ => unreachable!(),
    };
    // Contradictory test metadata cannot override a real current binding.
    db.execute("INSERT INTO source_node_omissions(node_id,repo_key,kind,emitted_fact_digest) VALUES (?1,?2,'rules',?3)",rusqlite::params![bound_id,source.repo_key,bound.emitted_fact_digest]).unwrap();
    let bound_fact = crate::task_evidence::task_fact(&bound.fact);
    let bound_target = session
        .describe_evidence(&state, &basis, &bound_fact, 0, 65536)
        .unwrap()
        .target;
    // An unchanged raw graph does not authorize an invalid omission digest.
    db.execute(
        "UPDATE source_node_omissions SET emitted_fact_digest=?1 WHERE node_id=?2",
        rusqlite::params![format!("node-v1:{}", "0".repeat(64)), node.id],
    )
    .unwrap();
    assert_eq!(describe(), ReadError::EvidenceUnavailable);
    assert_eq!(
        state
            .graph
            .lock()
            .unwrap()
            .read_source_selection_snapshot(&[FactKey::from_node(&node)])
            .unwrap()
            .selections[0]
            .omission,
        OmissionLookup::Unrecorded
    );
    db.execute(
        "DELETE FROM source_node_omissions WHERE node_id=?1",
        [&node.id],
    )
    .unwrap();
    assert_eq!(
        state
            .graph
            .lock()
            .unwrap()
            .read_source_selection_snapshot(&[FactKey::from_node(&node)])
            .unwrap()
            .selections[0]
            .omission,
        OmissionLookup::NotOmitted
    );
    assert_eq!(describe(), ReadError::EvidenceUnavailable);
    // A current binding still takes forgotten precedence over contradictory omission metadata.
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
    assert_eq!(
        session
            .read_evidence(
                &state,
                &basis,
                &bound_target,
                crate::mcp_reads::evidence::EvidenceLimits {
                    max_file_validation_bytes: 4096,
                    max_span_bytes: 1024,
                    max_response_bytes: 65536
                }
            )
            .unwrap_err(),
        ReadError::EvidenceForgotten
    );
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "changed-omission-basis".into(),
            label: "Module".into(),
            props: json!({}),
        })
        .unwrap();
    assert_eq!(describe(), ReadError::StaleBasis);
}

// ADR-0046 decision 5: once a later publish of the same source lands, the
// receipts its graph no longer binds are reclaimed as superseded; bound and
// pinned receipts stay, and a busy source is left for its next publish.
#[test]
fn primary_source_publish_reclaims_superseded_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    publish(&state, &source, &first);
    let pinned = first.receipts[0].clone();
    let mut stored: BTreeSet<String> = first
        .receipts
        .iter()
        .map(|receipt| receipt.id().to_owned())
        .collect();
    drop(first);
    retain_staged_proposal_reference(&state, &source, &pinned);

    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let second = parse(&state, &source, || {});
    let bound: BTreeSet<String> = publish(&state, &source, &second)
        .into_iter()
        .map(|binding| binding.receipt_id)
        .collect();
    stored.extend(
        second
            .receipts
            .iter()
            .map(|receipt| receipt.id().to_owned()),
    );
    let capture_id = second.input.capture.as_ref().unwrap().id().to_owned();
    // The publish still holds its prepared input: no reclaim runs.
    assert_eq!(
        primary_source::reclaim_superseded(&state, &source, &capture_id).unwrap(),
        0
    );
    drop(second);

    let superseded: Vec<&String> = stored
        .iter()
        .filter(|id| !bound.contains(*id) && **id != pinned.id())
        .collect();
    assert!(!superseded.is_empty());
    assert!(!bound.contains(pinned.id()));
    assert_eq!(
        primary_source::reclaim_superseded(&state, &source, &capture_id).unwrap(),
        superseded.len() as u64
    );
    let receipts = state.primary_sources.receipts.lock().unwrap();
    for id in &superseded {
        match receipts.receipt_status(id).unwrap() {
            primary_source::ReceiptStatus::Superseded { tombstone } => {
                assert_eq!(tombstone.source_id, source.source_id);
                assert_eq!(tombstone.receipts_reclaimed, superseded.len() as u64);
                assert_eq!(tombstone.receipts_retained_pinned, 0);
            }
            other => panic!("expected Superseded, got {other:?}"),
        }
    }
    for id in bound.iter().chain([&pinned.id().to_owned()]) {
        assert_eq!(
            receipts.receipt_status(id).unwrap(),
            primary_source::ReceiptStatus::Present
        );
    }
    drop(receipts);
    // Settled: a repeat finds nothing more to reclaim.
    assert_eq!(
        primary_source::reclaim_superseded(&state, &source, &capture_id).unwrap(),
        0
    );
}

// ADR-0046 decision 5 through the production recovery path: a second local
// recovery reclaims the receipts only the first one bound.
#[test]
fn second_recovery_reclaims_the_first_recoverys_superseded_receipts() {
    use tauri::Manager;
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&private));
    app.manage(crate::PreflightRuns::default());
    let state = app.state::<AppState>();
    let source = register_local_source(&state, &root).unwrap();
    let recover = || {
        let operation = source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = start_job(&state, &source.ingest_job_kind()).unwrap();
        run_ingest(&source, &operation, &execution, app.handle(), &state).unwrap();
        let conn = rusqlite::Connection::open_with_flags(
            private.join("retained-source/receipts.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let ids: BTreeSet<String> = conn
            .prepare("SELECT id FROM receipts")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        ids
    };
    let first = recover();
    assert!(!first.is_empty());
    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let second = recover();
    let superseded: Vec<_> = first.difference(&second).collect();
    assert!(!superseded.is_empty());
    let receipts = state.primary_sources.receipts.lock().unwrap();
    for id in superseded {
        assert!(matches!(
            receipts.receipt_status(id).unwrap(),
            primary_source::ReceiptStatus::Superseded { .. }
        ));
    }
}

// AC-0327: exercise the real v3 producer and decoder, not a second receipt DTO.
#[test]
fn receipt_workspace_covers_capture_roundtrip_and_range_refusal() {
    use adapters_lang_ts::captured::{MAX_RECEIPT_BYTES, MAX_RECEIPT_RANGES, ReceiptRange};
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&private);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let model = crate::mcp_reads::reservation::receipt::workspace_bytes().unwrap();
    for receipt in &parsed.receipts {
        let raw = receipt.to_json().unwrap();
        assert!(receipt.id().starts_with("ts-primary-v3:"));
        let decoded = Receipt::from_json(&raw).unwrap();
        assert_eq!(decoded.to_json().unwrap(), raw);
        let range_wire = serde_json::to_vec(decoded.ranges()).unwrap().len();
        let actual = std::mem::size_of::<Receipt>()
            + raw.len()
            + range_wire
            + std::mem::size_of_val::<[ReceiptRange]>(decoded.ranges());
        assert!(model.retained >= actual);
        assert!(model.peak >= model.retained + raw.len());
        let mut invalid: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let range = invalid["content"]["ranges"][0].clone();
        let expanded = vec![decoded.ranges()[0].clone(); MAX_RECEIPT_RANGES];
        assert!(serde_json::to_vec(&expanded).unwrap().len() > MAX_RECEIPT_BYTES);
        for count in [MAX_RECEIPT_RANGES, MAX_RECEIPT_RANGES + 1] {
            invalid["content"]["ranges"] = serde_json::json!(vec![range.clone(); count]);
            let rejected = serde_json::to_string(&invalid).unwrap();
            assert!(rejected.len() <= MAX_RECEIPT_BYTES);
            assert!(Receipt::from_json(&rejected).is_err());
        }
    }
}

#[test]
fn receipt_share_follows_each_sources_captured_bytes() {
    // AC-0497: a source's receipts are admitted up to its share of the store,
    // proportional to its captured bytes among all retained sources; the rest
    // are named omissions rather than a failed recovery (#725).
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let large_root = directory(dir.path(), "large");
    let small_root = directory(dir.path(), "small");
    // Three times the small source's bytes: the small source's share is a quarter.
    let padding = "x".repeat(2 * STABLE_A.len() - 3);
    std::fs::write(
        large_root.join("source.ts"),
        format!("{STABLE_A}//{padding}\n"),
    )
    .unwrap();
    std::fs::write(small_root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let large_source = register_local_source(&state, &large_root).unwrap();
    let small_source = register_local_source(&state, &small_root).unwrap();
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 8;

    let retain = |source: &RegisteredSource| {
        let parsed = parse(&state, source, || {});
        let mut kept = parsed.receipts.clone();
        let (omitted, _) = state
            .primary_sources
            .persist(&parsed.input, source, &parsed.receipts)
            .unwrap()
            .keep_retained(&mut kept);
        (parsed.receipts.len(), kept.len(), omitted)
    };
    // Alone in the store, the large source has all of it.
    let (total, kept, omitted) = retain(&large_source);
    assert_eq!((total, kept), (4, 4));
    assert!(omitted.is_empty());
    let (total, kept, omitted) = retain(&small_source);
    assert_eq!((total, kept), (4, 2));
    assert_eq!(omitted, BTreeMap::from([("source.ts".to_string(), 2)]));
    // Three quarters of the store still holds every large-source receipt.
    let (_, kept, omitted) = retain(&large_source);
    assert_eq!(kept, 4);
    assert!(omitted.is_empty());
}

// AC-0838: reclamation demonstrably frees receipt store capacity. A recovery
// that a full store left with store-capacity omissions keeps every receipt
// once forget or supersession has reclaimed the rows nothing pins (#725).
#[test]
fn ac0838_forget_and_supersession_free_room_for_omitted_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let forgotten_root = directory(dir.path(), "forgotten");
    let waiting_root = directory(dir.path(), "waiting");
    std::fs::write(forgotten_root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(waiting_root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let forgotten = register_local_source(&state, &forgotten_root).unwrap();
    let waiting = register_local_source(&state, &waiting_root).unwrap();
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 4;

    // Persist, publish the retained receipts' bindings, and report what was
    // kept, what the store named as omitted, and the capture id.
    let recover = |source: &RegisteredSource| {
        let parsed = parse(&state, source, || {});
        let mut kept = parsed.receipts.clone();
        let (omitted, _) = state
            .primary_sources
            .persist(&parsed.input, source, &parsed.receipts)
            .unwrap()
            .keep_retained(&mut kept);
        let bindings = primary_source::matching_bindings(&parsed.extraction, &kept);
        load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &parsed.extraction,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
            None,
        )
        .unwrap();
        let capture_id = parsed.input.capture.as_ref().unwrap().id().to_owned();
        (parsed.receipts.len(), kept.len(), omitted, capture_id)
    };
    let all_omitted = BTreeMap::from([("source.ts".to_string(), 4)]);

    // Forget: the first source fills the store, so the second keeps nothing.
    assert_eq!(recover(&forgotten).1, 4);
    let (total, kept, omitted, _) = recover(&waiting);
    assert_eq!((total, kept), (4, 0));
    assert_eq!(omitted, all_omitted);
    let preview = primary_source::preview(&state, &forgotten.source_id).unwrap();
    primary_source::forget(&state, &forgotten.source_id, &preview.fingerprint).unwrap();
    assert_eq!(receipt_store_accounting(&app_data).0, 0);
    let (total, kept, omitted, capture_id) = recover(&waiting);
    assert_eq!((total, kept), (4, 4));
    assert!(omitted.is_empty());
    assert_eq!(
        primary_source::reclaim_superseded(&state, &waiting, &capture_id).unwrap(),
        0
    );

    // Supersession: a changed capture finds the store full of its own
    // earlier receipts, which its publish no longer binds.
    std::fs::write(waiting_root.join("source.ts"), STABLE_B).unwrap();
    let (total, kept, omitted, capture_id) = recover(&waiting);
    assert_eq!((total, kept), (4, 0));
    assert_eq!(omitted, all_omitted);
    assert_eq!(
        primary_source::reclaim_superseded(&state, &waiting, &capture_id).unwrap(),
        4
    );
    assert_eq!(receipt_store_accounting(&app_data).0, 0);
    let (total, kept, omitted, _) = recover(&waiting);
    assert_eq!((total, kept), (4, 4));
    assert!(omitted.is_empty());
}

fn git(args: &[&str], cwd: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

// AC-1022: given a commit, the selected files are read from that commit's
// Git objects, not the checkout, and the parser cites those blob bytes; a
// selected file that is no blob of the commit fails without a fallback.
#[test]
fn a_commit_capture_reads_git_blobs_not_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "clone");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    git(&["init", "-q", "-b", "main"], &root);
    git(&["add", "."], &root);
    git(&["commit", "-q", "-m", "init"], &root);
    let commit = git(&["rev-parse", "HEAD"], &root);
    // The checkout no longer matches the commit.
    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            Some(&commit),
            source_capture::CaptureLimits::default(),
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            Default::default(),
        )
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    assert_eq!(
        capture.manifest().kind,
        source_capture::CaptureKind::Git {
            commit: commit.clone(),
            tree: git(&["rev-parse", "HEAD^{tree}"], &root),
        }
    );
    let file = capture.file("source.ts").unwrap();
    assert_eq!(file.bytes(), STABLE_A.as_bytes());
    assert!(capture.manifest().files[0].git.is_some());
    let mut receipts = Vec::new();
    {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            &commit,
            &layers,
            &BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap();
    }
    assert!(!receipts.is_empty());
    for receipt in &receipts {
        assert_eq!(receipt.file().capture_id, capture.id());
        assert_eq!(receipt.file().digest, file.reference().digest);
    }
    drop(input);
    // Infrastructure alone still runs the TS pass for Pulumi, so a commit is
    // captured for it; a local folder's infra-only recovery stays uncaptured.
    let infra = vec!["infra".to_string()];
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &infra,
            Some(&commit),
            source_capture::CaptureLimits::default(),
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            Default::default(),
        )
        .unwrap();
    assert!(matches!(
        input.capture.as_ref().unwrap().manifest().kind,
        source_capture::CaptureKind::Git { .. }
    ));
    drop(input);
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &infra)
        .unwrap();
    assert!(input.capture.is_none());
    drop(input);
    // An untracked selected file is no blob of the commit.
    std::fs::write(root.join("extra.ts"), STABLE_B).unwrap();
    let error = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            Some(&commit),
            source_capture::CaptureLimits::default(),
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            Default::default(),
        )
        .err()
        .unwrap();
    assert!(error.contains("cloned commit's Git objects"), "{error}");
}

// AC-1802 (#1653): the TS/JS selection walk takes its file bound from the
// capture limits and its entry bound from the caller, so a project's raised
// or lowered limits reach the selection, not only the capture.
#[test]
fn the_selection_walk_follows_the_projects_capture_and_entry_limits() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "local");
    std::fs::write(root.join("a.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("b.ts"), STABLE_B).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let prepare = |max_files, max_visited_entries| {
        state.primary_sources.prepare_at(
            &source,
            source.root(),
            &[],
            None,
            source_capture::CaptureLimits {
                max_files,
                ..source_capture::CaptureLimits::default()
            },
            max_visited_entries,
            Default::default(),
        )
    };
    let error = prepare(1, adapters_lang_ts::captured::MAX_VISITED_ENTRIES)
        .err()
        .unwrap();
    assert!(
        error.contains("captured safely (") && error.contains("selected files"),
        "{error}"
    );
    let error = prepare(2, 1).err().unwrap();
    assert!(error.contains("visited entries"), "{error}");
    let input = prepare(2, adapters_lang_ts::captured::MAX_VISITED_ENTRIES).unwrap();
    assert_eq!(input.capture.as_ref().unwrap().manifest().files.len(), 2);
}

// AC-1059: when the environment selection does not fit beside the lanes,
// the lanes keep their capture without it and the environment reader reads
// the checkout.
#[test]
fn an_environment_selection_that_does_not_fit_never_costs_the_lanes() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "local");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("app.py"), "print('app')\n").unwrap();
    std::fs::write(
        root.join("compose.yaml"),
        "services:\n  web:\n    image: nginx\n",
    )
    .unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            None,
            source_capture::CaptureLimits {
                max_files: 2,
                ..source_capture::CaptureLimits::default()
            },
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            Default::default(),
        )
        .unwrap();
    assert!(!input.environments_captured);
    let paths: Vec<_> = input
        .capture
        .as_ref()
        .unwrap()
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["app.py", "source.ts"]);
}

// AC-1057: a source's workflows and Dockerfiles join its capture, and the
// pipeline reader parses the retained bytes, not the checkout.
#[test]
fn pipelines_are_read_from_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(
        root.join("Dockerfile"),
        "FROM node:20 AS build\nRUN npm ci\n",
    )
    .unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["Dockerfile", "source.ts"]);
    std::fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3 AS checkout\nFROM nginx:1\n",
    )
    .unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    let pipeline = extraction
        .nodes
        .iter()
        .find(|node| node.label == "Pipeline")
        .expect("the captured Dockerfile was read");
    assert_eq!(pipeline.props["stages"], 1, "{:?}", pipeline.props);
}

// AC-1057: a capture taken with no pipeline files has an empty pipeline
// selection, so a Dockerfile created in the checkout afterwards is not read.
#[test]
fn a_capture_without_pipelines_never_reads_the_checkouts() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    std::fs::write(
        root.join("Dockerfile"),
        "FROM node:20 AS build\nRUN npm ci\n",
    )
    .unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        !extraction.nodes.iter().any(|node| node.label == "Pipeline"),
        "a Dockerfile outside the capture was read"
    );
}

// AC-1059: the environment reader's selection joins the capture, and with
// the capture the reader reads the retained bytes: a Compose file edited or
// created in the checkout afterwards changes nothing. Without the flag that
// says the selection was retained, the checkout is read.
#[test]
fn environments_are_read_from_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(
        root.join("compose.yaml"),
        "services:\n  web:\n    image: nginx:1.27\n",
    )
    .unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    assert!(input.environments_captured);
    let capture = input.capture.as_ref().unwrap();
    assert!(
        capture
            .manifest()
            .files
            .iter()
            .any(|entry| entry.path == "compose.yaml")
    );
    std::fs::write(
        root.join("compose.yaml"),
        "services:\n  web:\n    image: busybox:1\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("late")).unwrap();
    std::fs::write(
        root.join("late/compose.yaml"),
        "services:\n  late:\n    image: late:1\n",
    )
    .unwrap();
    let extract = |captured: bool| {
        let mut receipts = Vec::new();
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads {
                environments_captured: captured,
                ..crate::ProducerReads::default()
            },
        )
        .unwrap()
        .0
    };
    let images = |extraction: &adapters_lang_ts::Extraction| -> Vec<String> {
        extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Workload")
            .filter_map(|node| node.props["image"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(images(&extract(true)), ["nginx:1.27"]);
    let mut checkout = images(&extract(false));
    checkout.sort();
    assert_eq!(checkout, ["busybox:1", "late:1"]);
}

// AC-1061: the schedule and entry-point readers' selections join the
// capture, and with the capture they read the retained bytes: a crontab or
// Procfile edited in the checkout afterwards changes nothing. Without the
// flag that says the selections were retained, the checkout is read.
#[test]
fn schedules_and_entry_points_are_read_from_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("crontab"), "*/5 * * * * python cron.py\n").unwrap();
    std::fs::write(root.join("cron.py"), "print('cron')\n").unwrap();
    std::fs::write(root.join("Procfile"), "worker: python worker.py\n").unwrap();
    std::fs::write(root.join("worker.py"), "print('worker')\n").unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    assert!(input.starts_captured);
    let capture = input.capture.as_ref().unwrap();
    let held: Vec<&str> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(
        held.contains(&"crontab") && held.contains(&"Procfile"),
        "{held:?}"
    );
    std::fs::write(root.join("crontab"), "0 0 * * * python cron.py\n").unwrap();
    std::fs::write(root.join("Procfile"), "web: python web.py\n").unwrap();
    std::fs::write(root.join("web.py"), "print('web')\n").unwrap();
    let extract = |captured: bool| {
        let mut receipts = Vec::new();
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads {
                starts_captured: captured,
                ..crate::ProducerReads::default()
            },
        )
        .unwrap()
        .0
    };
    let starts = |extraction: &adapters_lang_ts::Extraction| -> Vec<String> {
        let mut found: Vec<String> = extraction
            .nodes
            .iter()
            .filter_map(|node| match node.label.as_str() {
                "Schedule" => node.props["schedule"].as_str().map(str::to_string),
                "EntryPoint" => node.props["name"].as_str().map(str::to_string),
                _ => None,
            })
            .collect();
        found.sort();
        found
    };
    let captured = starts(&extract(true));
    let checkout = starts(&extract(false));
    assert!(captured.iter().any(|s| s == "*/5 * * * *"), "{captured:?}");
    assert!(
        captured.iter().any(|s| s.contains("worker")),
        "{captured:?}"
    );
    assert!(!captured.iter().any(|s| s.contains("web")), "{captured:?}");
    assert!(checkout.iter().any(|s| s == "0 0 * * *"), "{checkout:?}");
    assert!(checkout.iter().any(|s| s.contains("web")), "{checkout:?}");
}

// AC-1063: the toolchain reader's selection joins the capture, and with the
// capture it reads the retained bytes: a package.json edited in the
// checkout afterwards changes nothing, while an env file is never retained.
// Without the flag that says the selection was retained, the checkout is
// read.
#[test]
fn toolchain_configs_are_read_from_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(
        root.join("package.json"),
        "{\"name\":\"shop\",\"dependencies\":{\"react\":\"^18\"}}",
    )
    .unwrap();
    std::fs::write(root.join(".env"), "PORT=3000\n").unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    assert!(input.toolchain_captured);
    let capture = input.capture.as_ref().unwrap();
    let held: Vec<&str> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(held.contains(&"package.json"), "{held:?}");
    assert!(!held.contains(&".env"), "{held:?}");
    std::fs::write(
        root.join("package.json"),
        "{\"name\":\"shop\",\"dependencies\":{\"vue\":\"^3\"}}",
    )
    .unwrap();
    let extract = |captured: bool| {
        let mut receipts = Vec::new();
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads {
                toolchain_captured: captured,
                ..crate::ProducerReads::default()
            },
        )
        .unwrap()
        .0
    };
    let tools = |extraction: &adapters_lang_ts::Extraction| -> Vec<String> {
        let mut found: Vec<String> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Tool")
            .map(|node| node.id.clone())
            .collect();
        found.sort();
        found
    };
    let captured = tools(&extract(true));
    let checkout = tools(&extract(false));
    assert!(
        captured.iter().any(|id| id.contains("react")),
        "{captured:?}"
    );
    assert!(
        !captured.iter().any(|id| id.contains("vue")),
        "{captured:?}"
    );
    assert!(
        captured.iter().any(|id| id.contains(".env")),
        "{captured:?}"
    );
    assert!(checkout.iter().any(|id| id.contains("vue")), "{checkout:?}");
}

// AC-1065: the documentation walk's selection joins the capture, and with
// the capture both the recovery's documentation read and the system-wide
// ADR relinking read the retained bytes: a README and a decision record
// edited and a decision record created in the checkout afterwards change
// nothing. Without the flag that says the selection was retained, the
// checkout is read.
#[test]
fn documents_and_found_adrs_are_read_from_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("README.md"), "# Shop\n\nOrders.\n").unwrap();
    std::fs::create_dir_all(root.join("docs/adr")).unwrap();
    std::fs::write(root.join("docs/adr/0001-orders.md"), "# 1. Orders\n").unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    assert!(input.documents_captured);
    let capture = input.capture.as_ref().unwrap();
    let held: Vec<&str> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(
        held.contains(&"README.md") && held.contains(&"docs/adr/0001-orders.md"),
        "{held:?}"
    );
    std::fs::write(root.join("README.md"), "# Other\n\nPayments.\n").unwrap();
    std::fs::write(root.join("docs/adr/0001-orders.md"), "# 1. Payments\n").unwrap();
    std::fs::write(root.join("docs/adr/0002-late.md"), "# 2. Late\n").unwrap();
    let documents = |nodes: &[core_graph::Node]| -> String {
        let mut found: Vec<String> = nodes
            .iter()
            .filter(|node| matches!(node.label.as_str(), "Document" | "DocSection" | "ADR"))
            .map(|node| format!("{} {}", node.id, node.props))
            .collect();
        found.sort();
        found.join("\n")
    };
    let extract = |captured: bool| {
        let mut receipts = Vec::new();
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        let extraction = extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads {
                documents_captured: captured,
                ..crate::ProducerReads::default()
            },
        )
        .unwrap()
        .0;
        documents(&extraction.nodes)
    };
    let captured = extract(true);
    let checkout = extract(false);
    assert!(captured.contains("Orders"), "{captured}");
    assert!(!captured.contains("Payments"), "{captured}");
    assert!(!captured.contains("0002-late"), "{captured}");
    assert!(
        checkout.contains("Payments") && checkout.contains("0002-late"),
        "{checkout}"
    );

    // The relinking pass reads the documents the capture retained.
    let repo = core_graph::Node {
        id: format!("repo:{}", source.repo_key),
        label: "Repo".into(),
        props: serde_json::json!({"commit": "workdir"}),
    };
    let relink = |captures: &crate::DocumentCaptures| {
        let (adrs, docs) = crate::collect_documentation(
            &spec::ReferenceIndex::default(),
            std::slice::from_ref(&repo),
            &|_| false,
            &crate::Documented::from([(source.repo_key.clone(), Vec::new())]),
            captures,
            ingest::api_docs::ApiDocLimits::default(),
            |_| Ok(source.root()),
        )
        .unwrap();
        let mut nodes: Vec<core_graph::Node> =
            docs.into_iter().flat_map(|(_, d)| d.nodes).collect();
        nodes.extend(adrs.into_iter().flat_map(|(_, a)| a.nodes));
        documents(&nodes)
    };
    let retained = crate::CapturedDocuments::of(&input).unwrap();
    let relinked = relink(&crate::DocumentCaptures::from([(
        source.repo_key.clone(),
        retained,
    )]));
    assert!(relinked.contains("Orders"), "{relinked}");
    assert!(!relinked.contains("Payments"), "{relinked}");
    assert!(!relinked.contains("0002-late"), "{relinked}");
    let live = relink(&crate::DocumentCaptures::new());
    assert!(live.contains("Payments") && live.contains("0002-late"));
}

// AC-1065: a documentation selection that does not fit beside the readers
// is dropped first: the readers and lanes keep their capture and only the
// documentation walk reads the checkout.
#[test]
fn a_documentation_selection_that_does_not_fit_never_costs_the_readers() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "local");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("app.py"), "print('app')\n").unwrap();
    std::fs::write(
        root.join("compose.yaml"),
        "services:\n  web:\n    image: nginx\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Local\n").unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            None,
            source_capture::CaptureLimits {
                max_files: 3,
                ..source_capture::CaptureLimits::default()
            },
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            Default::default(),
        )
        .unwrap();
    assert!(input.environments_captured);
    assert!(!input.documents_captured);
    let paths: Vec<_> = input
        .capture
        .as_ref()
        .unwrap()
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["app.py", "compose.yaml", "source.ts"]);
}

// AC-1069: the files the active plugins claim join the capture, and with
// the capture each plugin is routed the retained bytes of exactly those
// files: a claimed file edited to one the plugin refuses, a claimed file
// deleted and a claimed file created in the checkout afterwards change no
// plugin fact and no refusal. Without the flag that says the files were
// retained, the checkout is routed.
#[test]
fn plugin_claimed_files_are_routed_from_the_capture() {
    // The first byte of a source picks the fixture's behaviour: `M` is a
    // fact the host refuses.
    const REFUSING_ADAPTER: &[u8] =
        include_bytes!("../../crates/adapters-plugin-host/tests/fixtures/refusing-adapter.wat");
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("a.zz"), "fine").unwrap();
    std::fs::write(root.join("src/b.zz"), "fine too").unwrap();
    let artifact = dir.path().join("t0.plugin-refusing.wasm");
    std::fs::write(&artifact, REFUSING_ADAPTER).unwrap();
    let active = [crate::ActivePlugin {
        plugin_id: "t0.plugin-refusing".into(),
        path: artifact,
        content_hash: core_prov::content_hash(REFUSING_ADAPTER),
        extensions: vec!["zz".into()],
        cells: Vec::new(),
        languages: BTreeMap::new(),
        gate_key: "gate".into(),
        limits: adapters_plugin_host::PluginLimits::default(),
    }];
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let claimed = crate::plugin_claimed_paths(source.root(), &active).unwrap();
    assert_eq!(claimed, ["a.zz", "src/b.zz"]);
    assert_eq!(crate::plugin_claimed_paths(source.root(), &[]), None);
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            None,
            source_capture::CaptureLimits::default(),
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            crate::primary_source::CallerSelections {
                plugins: Some(&claimed),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(input.plugin_paths.as_deref(), Some(&claimed[..]));
    let capture = input.capture.as_ref().unwrap();

    // After capture: a refused edit, a deletion and a new claimed file.
    std::fs::write(root.join("a.zz"), "Malformed").unwrap();
    std::fs::remove_file(root.join("src/b.zz")).unwrap();
    std::fs::write(root.join("c.zz"), "fine").unwrap();
    let pinned = format!(
        "t0.plugin-refusing@{}",
        &core_prov::content_hash(REFUSING_ADAPTER)[..12]
    );
    let route = |captured: bool| {
        let mut receipts = Vec::new();
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        let mut reads = crate::ProducerReads {
            plugin_paths: captured.then(|| claimed.clone()),
            ..crate::ProducerReads::default()
        };
        let extraction = extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &active,
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut reads,
        )
        .unwrap()
        .0;
        let mut routed: Vec<String> = extraction
            .nodes
            .iter()
            .filter(|node| node.props["prov"]["extractor_id"] == serde_json::json!(pinned))
            .map(|node| node.id.trim_start_matches(&source.repo_key).to_string())
            .collect();
        routed.sort();
        let refused: Vec<String> = reads
            .plugins
            .iter()
            .flat_map(|read| read.refused.iter().map(|(path, _)| path.clone()))
            .collect();
        (routed, refused)
    };
    let (routed, refused) = route(true);
    assert_eq!(routed, [":a.zz", ":src/b.zz"]);
    assert!(refused.is_empty(), "{refused:?}");
    let (routed, refused) = route(false);
    assert_eq!(routed, [":c.zz"]);
    assert_eq!(refused, ["a.zz"]);
}

// AC-1069 (Codex review on #2497): a file another reader retains is never
// routed to a plugin unless the plugin's own listing claimed it. An ignored
// API reference with a claimed extension is in the capture, yet the plugin
// is routed only the claimed files the checkout walk listed, as the
// checkout route is.
#[test]
fn a_file_only_another_reader_retained_is_never_routed_to_a_plugin() {
    const REFUSING_ADAPTER: &[u8] =
        include_bytes!("../../crates/adapters-plugin-host/tests/fixtures/refusing-adapter.wat");
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::create_dir_all(root.join(".cartograph")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join(".gitignore"), "docs/private.md\n").unwrap();
    std::fs::write(root.join("docs/private.md"), "#### GET /api/items\n").unwrap();
    std::fs::write(root.join("notes.md"), "fine").unwrap();
    std::fs::write(
        root.join(ingest::api_docs::API_DOCS_REL_PATH),
        "[[doc]]\npath = \"docs/private.md\"\n",
    )
    .unwrap();
    let artifact = dir.path().join("t0.plugin-refusing.wasm");
    std::fs::write(&artifact, REFUSING_ADAPTER).unwrap();
    let active = [crate::ActivePlugin {
        plugin_id: "t0.plugin-refusing".into(),
        path: artifact,
        content_hash: core_prov::content_hash(REFUSING_ADAPTER),
        extensions: vec!["md".into()],
        cells: Vec::new(),
        languages: BTreeMap::new(),
        gate_key: "gate".into(),
        limits: adapters_plugin_host::PluginLimits::default(),
    }];
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let limits = ingest::api_docs::ApiDocLimits::default();
    let api_docs = ingest::api_docs::api_doc_paths(source.root(), &[], limits).unwrap();
    assert!(api_docs.contains(&"docs/private.md".to_string()));
    let claimed = crate::plugin_claimed_paths(source.root(), &active).unwrap();
    assert!(
        !claimed.contains(&"docs/private.md".to_string()),
        "{claimed:?}"
    );
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            None,
            source_capture::CaptureLimits::default(),
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            crate::primary_source::CallerSelections {
                api_docs: Some(&api_docs),
                plugins: Some(&claimed),
            },
        )
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    assert!(capture.file("docs/private.md").is_ok());
    let pinned = format!(
        "t0.plugin-refusing@{}",
        &core_prov::content_hash(REFUSING_ADAPTER)[..12]
    );
    let route = |plugin_paths: Option<Vec<String>>| {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        let extraction = extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &active,
            &mut |_| {},
            Some(capture),
            &mut Vec::new(),
            &mut crate::ProducerReads {
                plugin_paths,
                ..crate::ProducerReads::default()
            },
        )
        .unwrap()
        .0;
        let mut routed: Vec<String> = extraction
            .nodes
            .iter()
            .filter(|node| node.props["prov"]["extractor_id"] == serde_json::json!(pinned))
            .map(|node| node.id.trim_start_matches(&source.repo_key).to_string())
            .collect();
        routed.sort();
        routed
    };
    let captured = route(input.plugin_paths.clone());
    assert!(
        !captured.iter().any(|id| id.contains("private.md")),
        "{captured:?}"
    );
    assert_eq!(captured, route(None));
}

// AC-1069: the plugins' claimed files are kept before the documentation
// selection and left out before the readers: when only one of them fits,
// the documentation walk reads the checkout first, and then the plugins
// are routed the checkout while the readers keep their capture.
#[test]
fn plugin_claimed_files_never_cost_the_readers_their_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "local");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(
        root.join("compose.yaml"),
        "services:\n  web:\n    image: nginx\n",
    )
    .unwrap();
    std::fs::write(root.join("a.zz"), "fine").unwrap();
    std::fs::write(root.join("README.md"), "# Local\n").unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let claimed = vec!["a.zz".to_string()];
    let prepare = |max_files| {
        let input = state
            .primary_sources
            .prepare_at(
                &source,
                source.root(),
                &layers,
                None,
                source_capture::CaptureLimits {
                    max_files,
                    ..source_capture::CaptureLimits::default()
                },
                adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
                crate::primary_source::CallerSelections {
                    plugins: Some(&claimed),
                    ..Default::default()
                },
            )
            .unwrap();
        let paths: Vec<String> = input
            .capture
            .as_ref()
            .unwrap()
            .manifest()
            .files
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        (input, paths)
    };
    let (input, paths) = prepare(3);
    assert!(input.environments_captured && input.plugin_paths.is_some());
    assert!(!input.documents_captured);
    assert_eq!(paths, ["a.zz", "compose.yaml", "source.ts"]);
    drop(input);
    let (input, paths) = prepare(2);
    assert!(input.environments_captured);
    assert!(input.plugin_paths.is_none() && !input.documents_captured);
    assert_eq!(paths, ["compose.yaml", "source.ts"]);
}

// AC-1067: the declared API references and their config join the capture,
// and with the capture both recovery's contract read and the relinking
// pass's documents read the retained bytes: a reference and its config
// edited in the checkout afterwards change nothing. Without the capture's
// API references, the checkout is read.
#[test]
fn declared_api_references_are_read_from_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::create_dir_all(root.join(".cartograph")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/api.md"), "#### GET /api/items\n").unwrap();
    std::fs::write(
        root.join(ingest::api_docs::API_DOCS_REL_PATH),
        "[[doc]]\npath = \"docs/api.md\"\n",
    )
    .unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let limits = ingest::api_docs::ApiDocLimits::default();
    let selection = ingest::api_docs::api_doc_paths(source.root(), &[], limits).unwrap();
    let input = state
        .primary_sources
        .prepare_at(
            &source,
            source.root(),
            &layers,
            None,
            source_capture::CaptureLimits::default(),
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
            crate::primary_source::CallerSelections {
                api_docs: Some(&selection),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(input.api_doc_paths.as_deref(), Some(&selection[..]));
    let capture = input.capture.as_ref().unwrap();
    let held: Vec<&str> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(
        held.contains(&ingest::api_docs::API_DOCS_REL_PATH) && held.contains(&"docs/api.md"),
        "{held:?}"
    );
    std::fs::write(root.join("docs/api.md"), "#### GET /api/other\n").unwrap();
    std::fs::write(
        root.join(ingest::api_docs::API_DOCS_REL_PATH),
        "[[doc]]\npath = \"docs/api.md\"\nprefix = \"/api\"\n",
    )
    .unwrap();
    let cited = |nodes: &[core_graph::Node]| -> String {
        let mut found: Vec<String> = nodes
            .iter()
            .map(|node| format!("{} {}", node.id, node.props))
            .filter(|line| line.contains("docs/api.md"))
            .collect();
        found.sort();
        found.join("\n")
    };
    let extract = |captured: bool| {
        let mut receipts = Vec::new();
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        let extraction = extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads {
                api_docs_captured: captured,
                ..crate::ProducerReads::default()
            },
        )
        .unwrap()
        .0;
        cited(&extraction.nodes)
    };
    let captured = extract(true);
    let checkout = extract(false);
    assert!(captured.contains("/api/items"), "{captured}");
    assert!(!captured.contains("/api/other"), "{captured}");
    assert!(checkout.contains("/api/other"), "{checkout}");

    // The relinking pass reads the references the capture retained.
    let repo = core_graph::Node {
        id: format!("repo:{}", source.repo_key),
        label: "Repo".into(),
        props: serde_json::json!({"commit": "workdir"}),
    };
    let relink = |captures: &crate::DocumentCaptures| {
        let (_, docs) = crate::collect_documentation(
            &spec::ReferenceIndex::default(),
            std::slice::from_ref(&repo),
            &|_| false,
            &crate::Documented::from([(source.repo_key.clone(), Vec::new())]),
            captures,
            limits,
            |_| Ok(source.root()),
        )
        .unwrap();
        let nodes: Vec<core_graph::Node> = docs.into_iter().flat_map(|(_, d)| d.nodes).collect();
        cited(&nodes)
    };
    let retained = crate::CapturedDocuments::of(&input).unwrap();
    assert!(retained.api_docs.is_some());
    let relinked = relink(&crate::DocumentCaptures::from([(
        source.repo_key.clone(),
        retained,
    )]));
    let live = relink(&crate::DocumentCaptures::new());
    assert!(relinked.contains("/api/items"), "{relinked}");
    assert!(!relinked.contains("/api/other"), "{relinked}");
    assert!(live.contains("/api/other"), "{live}");
}

// AC-1055: the Python observed-infrastructure pass reads the same captured
// bytes as the Python lane, so a checkout edit after capture changes neither.
#[test]
fn python_observed_infrastructure_is_read_from_the_capture() {
    const PYTHON: &str =
        "import redis\nclient = redis.Redis.from_url(\"redis://captured.internal:6379/0\")\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("app.py"), PYTHON).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    std::fs::write(
        root.join("app.py"),
        "import redis\nclient = redis.Redis.from_url(\"redis://checkout.internal:6379/0\")\n",
    )
    .unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    let resources: Vec<&str> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Resource")
        .map(|node| node.id.as_str())
        .collect();
    assert!(
        resources
            .iter()
            .any(|id| id.contains("redis://captured.internal:6379")),
        "{resources:?}"
    );
    assert!(
        !resources.iter().any(|id| id.contains("checkout.internal")),
        "{resources:?}"
    );
}

// AC-1024: a source's `.py` files join its capture; the Python pass parses
// the retained bytes, not the checkout, and each surviving Python fact whose
// provenance lies in one captured file gets a provenance-only Python receipt
// that reads back its captured span.
#[test]
fn python_facts_are_parsed_from_the_capture_and_cited() {
    const PYTHON: &str = "def handler(user):\n    return user\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("app.py"), PYTHON).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["app.py", "source.ts"]);
    // The checkout changes after capture; the parse must not see it.
    std::fs::write(root.join("app.py"), "def other():\n    pass\n").unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("handler")),
        "the captured Python bytes were parsed"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("other"))
    );
    let python: Vec<_> = receipts
        .iter()
        .filter(|receipt| receipt.file().path == "app.py")
        .collect();
    assert!(!python.is_empty());
    let file = capture.file("app.py").unwrap();
    for minted in &python {
        assert!(minted.id().starts_with("py-primary-v1:"), "{}", minted.id());
        let receipt = minted.to_receipt().unwrap();
        assert_eq!(
            receipt.grammar(),
            adapters_lang_ts::captured::Grammar::Python
        );
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        for range in receipt.ranges() {
            assert_eq!(
                range.role,
                adapters_lang_ts::captured::RangeRole::Provenance
            );
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.evidence.byte_start, range.evidence.byte_end)
                    .is_ok()
            );
        }
    }
    // Every Python receipt binds a fact the recovery publishes.
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(python.iter().all(|receipt| {
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    }));
    // A function node carries one.
    assert!(python.iter().any(|receipt| matches!(
        receipt.fact_key(),
        FactKey::Node { id } if id.contains("handler")
    )));
}

// AC-1026: a source's `.go` files join its capture; the Go pass parses the
// retained bytes, not the checkout, and each surviving Go fact whose
// provenance lies in one captured file gets a provenance-only Go receipt that
// reads back its captured span.
#[test]
fn go_facts_are_parsed_from_the_capture_and_cited() {
    const GO: &str = "package main\n\nfunc Handler() {}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("main.go"), GO).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["main.go", "source.ts"]);
    // The checkout changes after capture; the parse must not see it.
    std::fs::write(root.join("main.go"), "package main\n\nfunc Other() {}\n").unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("Handler")),
        "the captured Go bytes were parsed"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("Other"))
    );
    let go: Vec<_> = receipts
        .iter()
        .filter(|receipt| receipt.file().path == "main.go")
        .collect();
    assert!(!go.is_empty());
    let file = capture.file("main.go").unwrap();
    for minted in &go {
        assert!(minted.id().starts_with("go-primary-v1:"), "{}", minted.id());
        let receipt = minted.to_receipt().unwrap();
        assert_eq!(receipt.grammar(), adapters_lang_ts::captured::Grammar::Go);
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        for range in receipt.ranges() {
            assert_eq!(
                range.role,
                adapters_lang_ts::captured::RangeRole::Provenance
            );
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.evidence.byte_start, range.evidence.byte_end)
                    .is_ok()
            );
        }
    }
    // Every Go receipt binds a fact the recovery publishes.
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(go.iter().all(|receipt| {
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    }));
    // A function node carries one.
    assert!(go.iter().any(|receipt| matches!(
        receipt.fact_key(),
        FactKey::Node { id } if id.contains("Handler")
    )));
}

// AC-1031: a source's `.java` files join its capture; the Java pass parses
// the retained bytes, not the checkout, and each surviving Java fact whose
// provenance lies in one captured file gets a provenance-only Java receipt
// that reads back its captured span.
#[test]
fn java_facts_are_parsed_from_the_capture_and_cited() {
    const JAVA: &str = "class App {\n  void handler() {}\n}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("App.java"), JAVA).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["App.java", "source.ts"]);
    // The checkout changes after capture; the parse must not see it.
    std::fs::write(root.join("App.java"), "class App {\n  void other() {}\n}\n").unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("handler")),
        "the captured Java bytes were parsed"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("other"))
    );
    let java: Vec<_> = receipts
        .iter()
        .filter(|receipt| receipt.file().path == "App.java")
        .collect();
    assert!(!java.is_empty());
    let file = capture.file("App.java").unwrap();
    for minted in &java {
        assert!(
            minted.id().starts_with("java-primary-v1:"),
            "{}",
            minted.id()
        );
        let receipt = minted.to_receipt().unwrap();
        assert_eq!(receipt.grammar(), adapters_lang_ts::captured::Grammar::Java);
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        for range in receipt.ranges() {
            assert_eq!(
                range.role,
                adapters_lang_ts::captured::RangeRole::Provenance
            );
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.evidence.byte_start, range.evidence.byte_end)
                    .is_ok()
            );
        }
    }
    // Every Java receipt binds a fact the recovery publishes.
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(java.iter().all(|receipt| {
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    }));
    // A function node carries one.
    assert!(java.iter().any(|receipt| matches!(
        receipt.fact_key(),
        FactKey::Node { id } if id.contains("handler")
    )));
}

// AC-1033: a source's `.rs` files join its capture; the Rust pass parses the
// retained bytes, not the checkout, and each surviving Rust fact whose
// provenance lies in one captured file gets a provenance-only Rust receipt
// that reads back its captured span.
#[test]
fn rust_facts_are_parsed_from_the_capture_and_cited() {
    const RUST: &str = "pub fn handler() {}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("lib.rs"), RUST).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["lib.rs", "source.ts"]);
    // The checkout changes after capture; the parse must not see it.
    std::fs::write(root.join("lib.rs"), "pub fn other() {}\n").unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("handler")),
        "the captured Rust bytes were parsed"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("other"))
    );
    let rust: Vec<_> = receipts
        .iter()
        .filter(|receipt| receipt.file().path == "lib.rs")
        .collect();
    assert!(!rust.is_empty());
    let file = capture.file("lib.rs").unwrap();
    for minted in &rust {
        assert!(minted.id().starts_with("rs-primary-v1:"), "{}", minted.id());
        let receipt = minted.to_receipt().unwrap();
        assert_eq!(receipt.grammar(), adapters_lang_ts::captured::Grammar::Rust);
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        for range in receipt.ranges() {
            assert_eq!(
                range.role,
                adapters_lang_ts::captured::RangeRole::Provenance
            );
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.evidence.byte_start, range.evidence.byte_end)
                    .is_ok()
            );
        }
    }
    // Every Rust receipt binds a fact the recovery publishes.
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(rust.iter().all(|receipt| {
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    }));
    // A function node carries one.
    assert!(rust.iter().any(|receipt| matches!(
        receipt.fact_key(),
        FactKey::Node { id } if id.contains("handler")
    )));
}

// AC-1035: a source's `.kt` and `.kts` files join its capture; the Kotlin
// pass parses the retained bytes, not the checkout, and each surviving Kotlin fact whose
// provenance lies in one captured file gets a provenance-only Kotlin receipt
// that reads back its captured span.
#[test]
fn kotlin_facts_are_parsed_from_the_capture_and_cited() {
    const KOTLIN: &str = "fun handler() {}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("App.kt"), KOTLIN).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["App.kt", "source.ts"]);
    // The checkout changes after capture; the parse must not see it.
    std::fs::write(root.join("App.kt"), "fun other() {}\n").unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("handler")),
        "the captured Kotlin bytes were parsed"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("other"))
    );
    let kotlin: Vec<_> = receipts
        .iter()
        .filter(|receipt| receipt.file().path == "App.kt")
        .collect();
    assert!(!kotlin.is_empty());
    let file = capture.file("App.kt").unwrap();
    for minted in &kotlin {
        assert!(minted.id().starts_with("kt-primary-v1:"), "{}", minted.id());
        let receipt = minted.to_receipt().unwrap();
        assert_eq!(
            receipt.grammar(),
            adapters_lang_ts::captured::Grammar::Kotlin
        );
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        for range in receipt.ranges() {
            assert_eq!(
                range.role,
                adapters_lang_ts::captured::RangeRole::Provenance
            );
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.evidence.byte_start, range.evidence.byte_end)
                    .is_ok()
            );
        }
    }
    // Every Kotlin receipt binds a fact the recovery publishes.
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(kotlin.iter().all(|receipt| {
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    }));
    // A function node carries one.
    assert!(kotlin.iter().any(|receipt| matches!(
        receipt.fact_key(),
        FactKey::Node { id } if id.contains("handler")
    )));
}

// AC-1215: a source's shipped `.class` files join its capture; the JVM class
// reader parses the retained bytes, not the checkout.
#[test]
fn shipped_jvm_classes_are_read_from_the_capture() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crates/adapters-artifact-jvm/tests/fixtures/classes/com/acme/pay/Ledger$Store.class"
    );
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "shipped");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::copy(fixture, root.join("lib/Store.class")).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["server".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    assert!(
        capture
            .manifest()
            .files
            .iter()
            .any(|entry| entry.path == "lib/Store.class")
    );
    // The checkout changes after capture; the read must not see it: neither
    // the class bytes nor a Java source added for the class.
    std::fs::write(root.join("lib/Store.class"), b"not a class").unwrap();
    std::fs::create_dir_all(root.join("com/acme/pay")).unwrap();
    std::fs::write(
        root.join("com/acme/pay/Ledger.java"),
        b"package com.acme.pay;",
    )
    .unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    let store = format!("sym:{}@lib/Store.class#Ledger.Store", source.repo_key);
    assert!(
        extraction.nodes.iter().any(|node| node.id == store),
        "the captured class bytes were read"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["path"] == "lib/Store.class")
    );
}

// AC-1053: a source's `.tf` files join its capture when infrastructure is
// recovered; the Terraform pass parses the retained bytes, not the checkout,
// and each surviving Terraform fact whose provenance lies in one captured file
// gets a provenance-only Terraform receipt that reads back its captured span.
#[test]
fn terraform_facts_are_parsed_from_the_capture_and_cited() {
    const TERRAFORM: &str = "resource \"aws_sqs_queue\" \"handler\" {}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("main.tf"), TERRAFORM).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string(), "infra".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let paths: Vec<_> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["main.tf", "source.ts"]);
    // The checkout changes after capture; the parse must not see it.
    std::fs::write(
        root.join("main.tf"),
        "resource \"aws_sqs_queue\" \"other\" {}\n",
    )
    .unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("handler")),
        "the captured Terraform bytes were parsed"
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.id.contains("other"))
    );
    let terraform: Vec<_> = receipts
        .iter()
        .filter(|receipt| receipt.file().path == "main.tf")
        .collect();
    assert!(!terraform.is_empty());
    let file = capture.file("main.tf").unwrap();
    for minted in &terraform {
        assert!(minted.id().starts_with("tf-primary-v1:"), "{}", minted.id());
        let receipt = minted.to_receipt().unwrap();
        assert_eq!(
            receipt.grammar(),
            adapters_lang_ts::captured::Grammar::Terraform
        );
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        for range in receipt.ranges() {
            assert_eq!(
                range.role,
                adapters_lang_ts::captured::RangeRole::Provenance
            );
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.evidence.byte_start, range.evidence.byte_end)
                    .is_ok()
            );
        }
    }
    // Every Terraform receipt binds a fact the recovery publishes.
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(terraform.iter().all(|receipt| {
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    }));
    // The resource node carries one.
    assert!(terraform.iter().any(|receipt| matches!(
        receipt.fact_key(),
        FactKey::Node { id } if id.contains("handler")
    )));
}

// AC-1037: a fact the Java directory join builds, a Spring endpoint composed
// from the class and method mappings of one captured file, gets a Java
// receipt bound to its final form and citing both mapping spans.
#[test]
fn directory_built_lane_facts_are_cited_from_the_capture() {
    const CONTROLLER: &str = "package com.demo.web;\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\n@RequestMapping(\"/api/users\")\npublic class UserController {\n    @GetMapping(\"/{id}\")\n    public String get() { return \"u\"; }\n}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("UserController.java"), CONTROLLER).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string(), "server".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let mut receipts = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    let endpoint = extraction
        .nodes
        .iter()
        .find(|node| node.label == "Endpoint" && node.props["path"] == "/api/users/{id}")
        .expect("the Spring endpoint is recovered");
    let receipt = receipts
        .iter()
        .find(|receipt| matches!(receipt.fact_key(), FactKey::Node { id } if *id == endpoint.id))
        .expect("the directory-built endpoint carries a receipt")
        .to_receipt()
        .unwrap();
    assert!(receipt.id().starts_with("java-primary-v1:"));
    assert!(receipt.matches_node(endpoint));
    // Both the class base and the method mapping are cited and read back.
    assert_eq!(receipt.ranges().len(), 2);
    let file = capture.file("UserController.java").unwrap();
    for range in receipt.ranges() {
        assert_eq!(range.captured.file, *file.reference());
        assert!(
            file.span(range.evidence.byte_start, range.evidence.byte_end)
                .is_ok()
        );
    }
    let bound = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(
        bound
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    );
}

// AC-1037: a lane relation repeated across occurrences, and a node id emitted
// twice, are minted in the form publication keeps (merged evidence, last
// value), so the receipt binds the published fact.
#[test]
fn lane_receipts_cite_published_merged_relations() {
    const SOURCE: &str = "def a():\n    b()\n    b()\n\ndef b():\n    pass\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.py"), SOURCE).unwrap();
    let source_id = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
    let capture = source_capture::capture_working_tree(
        dir.path(),
        &source_id,
        &["a.py".to_string()],
        source_capture::CaptureLimits::default(),
    )
    .unwrap();
    let repo = "local/src_11111111111111111111111111111111";
    let prov = |span: (u64, u64)| {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![core_prov::EvidenceRef {
                    repo: repo.into(),
                    path: "a.py".into(),
                    byte_start: span.0,
                    byte_end: span.1,
                    commit_sha: "workdir".into(),
                }],
                "t0.adapter-python",
                b"fact",
            )
            .unwrap(),
        )
        .unwrap()
    };
    let node = |id: &str, span, name: &str| core_graph::Node {
        id: id.into(),
        label: "Symbol".into(),
        props: json!({ "name": name, "prov": prov(span) }),
    };
    let call = |span| core_graph::Edge {
        src: "sym:a".into(),
        dst: "sym:b".into(),
        label: "CALLS".into(),
        props: json!({ "prov": prov(span) }),
    };
    let nodes = [
        node("sym:a", (0, 9), "stale"),
        node("sym:b", (29, 38), "b"),
        node("sym:a", (0, 9), "a"),
    ];
    let edges = [call((14, 17)), call((22, 25))];
    let id = adapters_lang_ts::SourceId {
        repo,
        commit: "workdir",
    };
    let mut receipts = Vec::new();
    stage_lane_publication_receipts(
        adapters_lang_ts::captured::Grammar::Python,
        &capture,
        &["a.py".to_string()],
        &id,
        &nodes,
        &edges,
        &mut receipts,
    );
    let edge = receipts
        .iter()
        .find(|receipt| matches!(receipt.fact_key(), FactKey::Edge { .. }))
        .expect("the merged relation carries a receipt")
        .to_receipt()
        .unwrap();
    // Both call sites are cited, as the published relation unions them.
    assert_eq!(edge.ranges().len(), 2);
    let node_a = receipts
        .iter()
        .find(|receipt| matches!(receipt.fact_key(), FactKey::Node { id } if id == "sym:a"))
        .expect("the last value of a repeated node carries a receipt")
        .to_receipt()
        .unwrap();
    assert!(node_a.matches_node(&nodes[2]));
    assert_eq!(receipts.len(), 3);
}

// AC-1039: a Flask route whose Blueprint mount lies in another captured file
// gets one Python receipt citing both files; it is stored, bound at
// publication, and each range reads back its own file's captured text.
#[test]
fn facts_citing_several_captured_files_are_cited_and_read_back() {
    const BLUEPRINT: &str =
        "from flask import Blueprint\n\nbp = Blueprint(\"api\", __name__, url_prefix=\"/api\")\n";
    const VIEWS: &str =
        "from bp import bp\n\n\n@bp.route(\"/users\")\ndef users():\n    return \"u\"\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "mixed");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(root.join("bp.py"), BLUEPRINT).unwrap();
    std::fs::write(root.join("views.py"), VIEWS).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string(), "server".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &layers)
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let mut minted = Vec::new();
    let (extraction, _, _) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            Some(capture),
            &mut minted,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
    };
    let endpoint = extraction
        .nodes
        .iter()
        .find(|node| node.label == "Endpoint" && node.props["path"] == "/api/users")
        .expect("the Blueprint route is recovered")
        .clone();
    let receipt = minted
        .iter()
        .find(|receipt| matches!(receipt.fact_key(), FactKey::Node { id } if *id == endpoint.id))
        .expect("the route citing two files carries a receipt")
        .to_receipt()
        .unwrap();
    assert!(receipt.id().starts_with("py-primary-v1:"));
    assert!(receipt.matches_node(&endpoint));
    let others: Vec<_> = receipt
        .other_files()
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(others.len(), 1);
    assert_ne!(others[0], receipt.file().path);
    assert_eq!(
        Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
        receipt
    );
    let parsed = Parsed {
        input,
        extraction,
        receipts: minted
            .iter()
            .map(|receipt| receipt.to_receipt().unwrap())
            .collect(),
        delta: Default::default(),
    };
    let bindings = publish(&state, &source, &parsed);
    assert!(
        bindings
            .iter()
            .any(|binding| binding.receipt_id == receipt.id())
    );
    // It is stored in the shared-header form, not verbatim.
    let header: Option<String> =
        rusqlite::Connection::open(app_data.join("retained-source/receipts.sqlite"))
            .unwrap()
            .query_row(
                "SELECT header_id FROM receipts WHERE id = ?1",
                [receipt.id()],
                |row| row.get(0),
            )
            .unwrap();
    assert!(header.is_some());
    let fact = FactKey::Node {
        id: endpoint.id.clone(),
    };
    let description = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(description.receipt_id, receipt.id());
    let mut paths = BTreeSet::new();
    for range in &description.ranges {
        let read =
            primary_source::read(&state, &fact, &description.receipt_id, range.index).unwrap();
        let original = if read.path == "bp.py" {
            BLUEPRINT
        } else {
            VIEWS
        };
        assert_eq!(
            read.text.as_bytes(),
            &original.as_bytes()[range.byte_start as usize..range.byte_end as usize]
        );
        paths.insert(read.path);
    }
    assert_eq!(
        paths,
        BTreeSet::from(["bp.py".to_string(), "views.py".to_string()])
    );
}

#[test]
fn a_java_only_source_is_captured_and_its_symbols_read_back() {
    // AC-1312 (#1727): a source with no TS/JS still gets a capture equal to its
    // Java files, and a Java symbol is described and read back from it.
    const JAVA: &str =
        "package demo;\n\nclass Orders {\n  int total(int a, int b) {\n    return a + b;\n  }\n}\n";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "java-only");
    std::fs::write(root.join("Orders.java"), JAVA).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {
        std::fs::write(root.join("Orders.java"), "class Changed {}\n").unwrap();
    });
    let paths: Vec<_> = parsed
        .input
        .capture
        .as_ref()
        .unwrap()
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert_eq!(paths, ["Orders.java"]);
    let symbol = parsed
        .extraction
        .nodes
        .iter()
        .find(|node| node.label == "Symbol" && node.id.contains("total"))
        .expect("the captured Java method is recovered")
        .clone();
    let bindings = publish(&state, &source, &parsed);
    assert!(
        bindings
            .iter()
            .all(|binding| binding.receipt_id.starts_with("java-primary-v1:"))
    );
    std::fs::remove_dir_all(&root).unwrap();
    let fact = FactKey::from_node(&symbol);
    let description = primary_source::describe(&state, &fact).unwrap();
    assert!(description.receipt_id.starts_with("java-primary-v1:"));
    assert_eq!(description.scope, "primary_source_only");
    assert!(!description.ranges.is_empty());
    for range in &description.ranges {
        let read =
            primary_source::read(&state, &fact, &description.receipt_id, range.index).unwrap();
        assert_eq!(read.path, "Orders.java");
        assert_eq!(
            read.text.as_bytes(),
            &JAVA.as_bytes()[range.byte_start as usize..range.byte_end as usize]
        );
        assert!(read.text.contains("total"));
    }
}

/// Receipts the production TS lane mints from `capture` (ADR-0072 slice 2).
fn receipts_from(
    state: &AppState,
    source: &RegisteredSource,
    commit: &str,
    capture: &source_capture::Capture,
) -> Vec<Receipt> {
    let layers = vec!["client".to_string()];
    let mut receipts = Vec::new();
    let mut caches = state.extraction_caches.lock().unwrap();
    let cache = caches.repos.entry(source.repo_key.clone()).or_default();
    extract_tree_with_primary(
        source.root(),
        &source.repo_key,
        commit,
        &layers,
        &BTreeMap::new(),
        &std::collections::BTreeSet::new(),
        None,
        None,
        &[],
        cache,
        &[],
        &mut |_| {},
        Some(capture),
        &mut receipts,
        &mut crate::ProducerReads::default(),
    )
    .unwrap();
    assert!(!receipts.is_empty());
    receipts
        .iter()
        .map(|receipt| receipt.to_receipt().unwrap())
        .collect()
}

// AC-1068 (ADR-0072 slice 2, #385): with an EOL rule and a checkout filter,
// the checkout bytes differ from the blob bytes. A working-tree capture and a
// Git-object capture of the same path get different digests and capture ids,
// every receipt names the capture it parsed, a commit id never makes a
// working-tree capture a Git one, capture never runs the filter, and a record
// staged from the checkout bytes is `changed` against the blob recapture.
#[test]
fn checkout_bytes_and_blob_bytes_never_stand_in_for_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "repo");
    let marker = dir.path().join("filter-ran");
    git(&["init", "-q", "-b", "main"], &root);
    let filter = format!("sh -c 'touch \"{}\"; cat'", marker.display());
    git(&["config", "filter.mark.clean", &filter], &root);
    git(&["config", "filter.mark.smudge", &filter], &root);
    std::fs::write(
        root.join(".gitattributes"),
        "*.ts text eol=crlf filter=mark\n",
    )
    .unwrap();
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    git(&["add", "."], &root);
    git(&["commit", "-q", "-m", "init"], &root);
    let commit = git(&["rev-parse", "HEAD"], &root);
    // A fresh checkout applies the EOL rule (and the filter) to the blob.
    std::fs::remove_file(root.join("source.ts")).unwrap();
    git(&["checkout", "--", "source.ts"], &root);
    let checkout = std::fs::read(root.join("source.ts")).unwrap();
    assert_eq!(checkout, STABLE_A.replace('\n', "\r\n").as_bytes());
    std::fs::remove_file(&marker).unwrap();

    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let layers = vec!["client".to_string()];
    let prepare = |commit: Option<&str>| {
        state
            .primary_sources
            .prepare_at(
                &source,
                source.root(),
                &layers,
                commit,
                source_capture::CaptureLimits::default(),
                adapters_lang_ts::captured::MAX_VISITED_ENTRIES,
                Default::default(),
            )
            .unwrap()
    };
    let working = prepare(None);
    let working = working.capture.as_ref().unwrap();
    let blob = prepare(Some(&commit));
    let blob = blob.capture.as_ref().unwrap();
    assert_eq!(
        working.manifest().kind,
        source_capture::CaptureKind::WorkingTree
    );
    assert!(matches!(
        blob.manifest().kind,
        source_capture::CaptureKind::Git { .. }
    ));
    let working_file = working.file("source.ts").unwrap();
    let blob_file = blob.file("source.ts").unwrap();
    assert_eq!(working_file.bytes(), checkout.as_slice());
    assert_eq!(blob_file.bytes(), STABLE_A.as_bytes());
    assert_ne!(working.id(), blob.id());
    assert_ne!(
        working_file.reference().digest,
        blob_file.reference().digest
    );

    // The commit id passed to the producer does not make the working-tree
    // capture a Git capture: its receipts name the working-tree capture.
    for (capture, file) in [(working, working_file), (blob, blob_file)] {
        for receipt in receipts_from(&state, &source, &commit, capture) {
            assert_eq!(receipt.file().capture_id, capture.id());
            assert_eq!(receipt.file().digest, file.reference().digest);
        }
    }
    assert!(!marker.exists(), "capture ran the checkout filter");

    // A record staged from the checkout bytes does not reconcile against the
    // blob recapture of the same path and range.
    let span = 0..checkout.len().min(STABLE_A.len());
    let staged = agents::EvidenceBasis {
        id: "source".into(),
        source: core_prov::EvidenceRef {
            repo: source.repo_key.clone(),
            path: "source.ts".into(),
            byte_start: 0,
            byte_end: span.end as u64,
            commit_sha: commit.clone(),
        },
        text_hash: core_prov::content_hash(&checkout[span.clone()]),
        text_bytes: span.len(),
    };
    let reference = blob_file.reference();
    let recaptured = agents::reconciliation::RecapturedItem::Range {
        origin: agents::reconciliation::ReconciledOrigin {
            receipt_id: format!("ts-primary-v3:{}", "0".repeat(64)),
            receipt_inventory_index: 0,
            captured: agents::TaskCaptureSpanRef {
                file: agents::TaskCaptureFileRef {
                    source_id: reference.source_id.as_str().to_owned(),
                    capture_id: reference.capture_id.clone(),
                    path: reference.path.clone(),
                    digest: reference.digest.clone(),
                    byte_len: reference.byte_len,
                },
                byte_start: 0,
                byte_end: span.end as u64,
            },
            input_closure: agents::InputClosureStatus::InputClosureNotEstablished,
        },
        fact_digest: "node-v1:unchanged",
        bytes: &blob_file.bytes()[span],
    };
    assert_eq!(
        agents::reconciliation::compare_captured(&staged, "node-v1:unchanged", recaptured).0,
        agents::reconciliation::ItemOutcome::Changed {
            reason: agents::reconciliation::ChangedReason::SpanBytesDiffer
        }
    );
}

// AC-1068 (ADR-0072 slice 2, #385): a main worktree and a linked worktree of
// one repository at one commit, with one file different, are distinct
// sources with distinct captures; an unchanged file has equal digests in both,
// and neither source accepts the other's receipts.
#[test]
fn linked_worktrees_are_distinct_sources_and_refuse_each_others_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let main = directory(dir.path(), "main");
    std::fs::write(main.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(main.join("shared.ts"), ORIGINAL).unwrap();
    git(&["init", "-q", "-b", "main"], &main);
    // The linked checkout must hold the committed bytes; Git for Windows
    // turns on autocrlf system-wide, which would rewrite shared.ts there.
    git(&["config", "core.autocrlf", "false"], &main);
    git(&["add", "."], &main);
    git(&["commit", "-q", "-m", "init"], &main);
    let linked_path = dir.path().join("linked");
    git(
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked_path.to_str().unwrap(),
            "HEAD",
        ],
        &main,
    );
    let linked = crate::paths::canonicalize(&linked_path).unwrap();
    std::fs::write(linked.join("source.ts"), STABLE_B).unwrap();

    let state = app_state(&app_data);
    let main_source = register_local_source(&state, &main).unwrap();
    let linked_source = register_local_source(&state, &linked).unwrap();
    assert_ne!(main_source.source_id, linked_source.source_id);
    assert_ne!(main_source.repo_key, linked_source.repo_key);
    let first = parse(&state, &main_source, || {});
    let second = parse(&state, &linked_source, || {});
    let (a, b) = (
        first.input.capture.as_ref().unwrap(),
        second.input.capture.as_ref().unwrap(),
    );
    assert_ne!(a.id(), b.id());
    let digest = |capture: &source_capture::Capture, path: &str| {
        capture.file(path).unwrap().reference().digest.clone()
    };
    assert_eq!(digest(a, "shared.ts"), digest(b, "shared.ts"));
    assert_ne!(digest(a, "source.ts"), digest(b, "source.ts"));
    for (parsed, source) in [(&first, &main_source), (&second, &linked_source)] {
        for receipt in &parsed.receipts {
            assert_eq!(receipt.repo_key(), source.repo_key);
            assert_eq!(
                receipt.file().capture_id,
                parsed.input.capture.as_ref().unwrap().id()
            );
        }
    }
    // Each source refuses the other's receipts and accepts its own.
    for (parsed, other) in [(&first, &linked_source), (&second, &main_source)] {
        assert!(
            state
                .primary_sources
                .persist(&parsed.input, other, &parsed.receipts)
                .is_err()
        );
    }
    publish(&state, &main_source, &first);
    publish(&state, &linked_source, &second);
}
