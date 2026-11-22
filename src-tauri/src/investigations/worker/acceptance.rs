//! Manual real-provider evidence, not a scripted quality score or native UI test.
//! The independent oracle is deliberately absent from all request construction.

use super::*;
#[path = "acceptance/completion.rs"]
mod completion;
#[path = "acceptance/expanded.rs"]
mod expanded;
#[path = "acceptance/inspection.rs"]
mod inspection;
#[path = "acceptance/vendure.rs"]
mod vendure;
use inspection::local_provider_acceptance;
use std::collections::BTreeSet;
use std::path::Path;

fn required(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("Set {name}; see MT-H4-02"));
    assert!(
        !value.trim().is_empty() && value.len() <= 4096,
        "Invalid {name}"
    );
    value
}

fn write_report(directory: &Path, report: &Value) {
    std::fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(report).unwrap(),
    )
    .unwrap();
}

fn source_revision() -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(args)
            .output()
            .expect("Git is required to record the tested source revision")
    };
    let status = git(&["status", "--porcelain"]);
    assert!(
        status.status.success() && status.stdout.is_empty(),
        "Real-provider acceptance requires a clean checkout so the source revision identifies the tested code"
    );
    let revision = git(&["rev-parse", "--verify", "HEAD"]);
    assert!(revision.status.success());
    let revision = String::from_utf8(revision.stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
    revision
}

pub(super) use super::ordered_events;

/// Read the actual durable records even on a failed/unknown model invocation.
/// Source text stays in the retained capture; the report includes read status and
/// hash, not transient prompts, raw model replies or source excerpts.
fn task_evidence(fixture: &Fixture, id: &str, needs_history: bool) -> (Value, bool) {
    let state = fixture.app.state::<AppState>();
    let (detail, result, ledger, events) = {
        let jobs = state.jobs.lock().unwrap();
        (
            jobs.investigation(id).unwrap(),
            jobs.investigation_result(id).unwrap(),
            jobs.investigation_ledger(id).unwrap(),
            ordered_events(&jobs, id),
        )
    };
    let ordered = events.first().is_some_and(|event| event.sequence == 1)
        && events
            .windows(2)
            .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
        && events
            .last()
            .is_some_and(|event| event.sequence == detail.summary.last_event_sequence);
    let tool_completed = |tool| {
        events.iter().any(|event| {
            event.kind == InvestigationEventKind::ToolCompleted && event.tool == Some(tool)
        })
    };
    let queried = tool_completed(InvestigationTool::QueryContext);
    let read = tool_completed(InvestigationTool::ReadEvidence);
    let saved_finish = events
        .iter()
        .any(|event| event.kind == InvestigationEventKind::ResultPersisted);
    let weak = result.as_ref().is_some_and(|result| {
        !result.findings.is_empty()
            && result.findings.iter().all(|finding| {
                finding.tier == Tier::Agentic
                    && finding.confidence_tier == ConfidenceTier::InferredWeak
            })
    });
    let history = ledger.as_ref().is_some_and(|ledger| {
        ledger.supplied_history_hash.is_some() && ledger.supplied_history_bytes > 0
    });
    let mut captured_reads = Vec::new();
    let mut captured_available = true;
    if let (Some(result), Some(ledger)) = (&result, &ledger) {
        let cited = result
            .findings
            .iter()
            .flat_map(|finding| &finding.citation_ids)
            .collect::<BTreeSet<_>>();
        for citation in &ledger.citations {
            if !cited.contains(&citation.citation_id)
                || !matches!(
                    citation.origin,
                    InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
                )
            {
                continue;
            }
            let read =
                crate::investigations::history::read(&state, id, ledger, &citation.citation_id)
                    .unwrap();
            let text_hash = read.text.as_ref().map(|text| content_hash(text.as_bytes()));
            captured_available &= read.status == InvestigationCitationStatus::Available
                && text_hash.is_some()
                && text_hash == citation.text_hash;
            captured_reads.push(json!({
                "citation_id": citation.citation_id,
                "status": read.status,
                "observed_text_hash": text_hash,
            }));
        }
    }
    captured_available &= !captured_reads.is_empty();
    // This is a fresh database connection, not a claim that the native app was
    // restarted. Full restart/forgetting and semantic review remain manual.
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    let reopen_equal = reopened.investigation(id).unwrap() == detail
        && reopened.investigation_result(id).unwrap() == result
        && reopened.investigation_ledger(id).unwrap() == ledger
        && ordered_events(&reopened, id) == events;
    let graph_unchanged =
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap()
            == fixture.graph_before;
    let checks = json!({
        "completed": detail.summary.status == InvestigationStatus::Completed,
        "events_ordered": ordered,
        "query_completed": queried,
        "read_completed": read,
        "finish_persisted": saved_finish,
        "findings_t3_weak": weak,
        "captured_citations_available_without_checkout": captured_available,
        "coordinator_reopen_equal": reopen_equal,
        "graph_unchanged": graph_unchanged,
        "parent_history_supplied_when_required": !needs_history || history,
    });
    let passed = checks
        .as_object()
        .unwrap()
        .values()
        .all(|check| check == &Value::Bool(true));
    (
        json!({
            "detail": detail, "result": result, "ledger": ledger, "events": events,
            "captured_citation_reads": captured_reads, "checks": checks,
            "mechanical_pass": passed,
            "independent_citation_review": "pending",
        }),
        passed,
    )
}

#[derive(Clone, Copy)]
struct AcceptanceSpecialists {
    analyst: SpecialistId,
    auditor: SpecialistId,
}

impl AcceptanceSpecialists {
    const V2: Self = Self {
        analyst: SpecialistId::DomainAnalyst,
        auditor: SpecialistId::EvidenceAuditor,
    };
    const COMPACT_V3: Self = Self {
        analyst: SpecialistId::DomainAnalystV3,
        auditor: SpecialistId::EvidenceAuditorV3,
    };

    const TURN_V4: Self = Self {
        analyst: SpecialistId::DomainAnalystV4,
        auditor: SpecialistId::EvidenceAuditorV4,
    };

    const SOURCE_INSPECTION_V5: Self = Self {
        analyst: SpecialistId::DomainAnalystV5,
        auditor: SpecialistId::EvidenceAuditorV5,
    };

    const QUALIFIED_V6: Self = Self {
        analyst: SpecialistId::DomainAnalystV6,
        auditor: SpecialistId::EvidenceAuditorV6,
    };

    // Explicit immutable definitions, recorded even if the first call fails and
    // the auditor is never started. No capability lookup selects a latest role.
    fn intended_definitions(self) -> Value {
        json!({
            "analyst": self.analyst.definition().unwrap(),
            "auditor": self.auditor.definition().unwrap(),
        })
    }

    fn analyst_fixture(self, provider: &dyn LlmProvider, directory: tempfile::TempDir) -> Fixture {
        if self.analyst == SpecialistId::DomainAnalystV5 {
            return Fixture::with_contract(
                provider,
                directory,
                "Inspect the available guard evidence and its limits. Query context, inspect retained source, and explain only behavior supported by citations.",
                self.analyst,
                Some(agents::TaskRangeRole::DefinitionInitializer),
                ORIGINAL,
            );
        }
        Fixture::with_specialist(
            provider,
            directory,
            "Inspect the available guard evidence and its limits. Query context, inspect retained source, and explain only behavior supported by citations.",
            self.analyst,
        )
    }
}

fn auditor(
    fixture: &Fixture,
    provider: Arc<dyn LlmProvider>,
    specialists: AcceptanceSpecialists,
) -> (String, u128) {
    run_auditor(fixture, provider, auditor_request(fixture, specialists))
}

fn auditor_request(
    fixture: &Fixture,
    specialists: AcceptanceSpecialists,
) -> StartInvestigationRequest {
    let state = fixture.app.state::<AppState>();
    let parent = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    StartInvestigationRequest {
        request_nonce: "local-acceptance-auditor".into(),
        specialist_id: specialists.auditor,
        question: "Audit the selected parent's findings against available context and retained evidence. Query context, inspect source, and explain unsupported claims and scope limits.".into(),
        conversation_id: Some(parent.summary.conversation_id),
        parent_id: Some(fixture.id.clone()),
        ..fixture.request.clone()
    }
}

fn run_auditor(
    fixture: &Fixture,
    provider: Arc<dyn LlmProvider>,
    request: StartInvestigationRequest,
) -> (String, u128) {
    let state = fixture.app.state::<AppState>();
    let start_time = Instant::now();
    let started = state
        .jobs
        .lock()
        .unwrap()
        .start_investigation(
            &request,
            Some(&descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap()),
        )
        .unwrap();
    let start_milliseconds = start_time.elapsed().as_millis();
    let id = started.detail.summary.investigation_id;
    let plan = state
        .jobs
        .lock()
        .unwrap()
        .claim_plan(
            started.detail.summary.job_id,
            crate::jobs::ClaimMode::StartQueued,
        )
        .unwrap();
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = state
        .jobs
        .lock()
        .unwrap()
        .claim_execution(&plan, reservation)
        .unwrap();
    let live = state.investigations.insert(&id).unwrap();
    run(
        fixture.app.handle().clone(),
        id.clone(),
        request,
        execution,
        provider,
        live,
        Instant::now(),
    );
    (id, start_milliseconds)
}

#[test]
#[ignore = "MT-H4-02: requires an installed local model and explicit fresh artifact directory"]
fn investigation_local_provider_acceptance() {
    local_provider_acceptance(AcceptanceSpecialists::V2);
}

#[test]
#[ignore = "MT-H4-02: requires an installed local model and explicit fresh artifact directory"]
fn investigation_compact_local_provider_acceptance() {
    local_provider_acceptance(AcceptanceSpecialists::COMPACT_V3);
}

#[test]
#[ignore = "MT-H4-02: requires an installed local model and explicit fresh artifact directory"]
fn investigation_turn_progress_local_provider_acceptance() {
    local_provider_acceptance(AcceptanceSpecialists::TURN_V4);
}

#[test]
#[ignore = "MT-H4-02: requires an installed local model and explicit fresh artifact directory"]
fn investigation_source_inspection_local_provider_acceptance() {
    local_provider_acceptance(AcceptanceSpecialists::SOURCE_INSPECTION_V5);
}

#[test]
#[ignore = "MT-H4-02 V6: requires clean reviewed code, installed model and exact-hash publication gate"]
fn investigation_qualified_local_provider_acceptance() {
    inspection::qualified::local_provider_acceptance();
}

#[test]
fn investigation_acceptance_report_preserves_failed_and_unknown_outcomes() {
    // AC-0191: report calibration only; these scripted cases cannot replace the
    // ignored real-provider procedure or its independent citation review.
    for (mode, status, expected_pass) in [
        (Mode::Finish, InvestigationStatus::Completed, true),
        (Mode::Malformed, InvestigationStatus::Failed, false),
        (Mode::Unknown, InvestigationStatus::OutcomeUnknown, false),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = Fixture::new(provider.as_ref());
        fixture.run(provider);
        let (report, passed) = task_evidence(&fixture, &fixture.id, false);
        assert_eq!(passed, expected_pass);
        assert_eq!(report["detail"]["status"], json!(status));
        assert_eq!(report["independent_citation_review"], "pending");
        if matches!(mode, Mode::Unknown) {
            assert_eq!(
                report["detail"]["usage"]["reported_input_tokens"],
                Value::Null
            );
            assert_eq!(report["detail"]["usage"]["model_invocations"], 1);
        }
        let artifacts = tempfile::tempdir().unwrap();
        write_report(artifacts.path(), &report);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(artifacts.path().join("run.json")).unwrap())
                .unwrap();
        assert_eq!(saved, report);
    }
}

#[test]
fn investigation_acceptance_explicit_specialists_match_prepared_report_and_durable_tasks() {
    // AC-0191: dispatch calibration only; real-provider and independent acceptance gates
    // remain unchanged. Exercise both actual coordinator paths and saved reports.
    let mut fingerprints = Vec::new();
    for (specialists, expected_analyst, expected_auditor, version) in [
        (
            AcceptanceSpecialists::V2,
            "domain-analyst@2",
            "evidence-auditor@2",
            2,
        ),
        (
            AcceptanceSpecialists::COMPACT_V3,
            "domain-analyst@3",
            "evidence-auditor@3",
            3,
        ),
        (
            AcceptanceSpecialists::TURN_V4,
            "domain-analyst@4",
            "evidence-auditor@4",
            4,
        ),
    ] {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let artifacts = tempfile::tempdir().unwrap();
        let preparation = json!({
            "phase": "preparation",
            "intended_specialists": specialists.intended_definitions(),
        });
        write_report(artifacts.path(), &preparation);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(artifacts.path().join("run.json")).unwrap())
                .unwrap();
        assert_eq!(saved, preparation);
        let intended = &saved["intended_specialists"];
        assert_eq!(intended["analyst"]["id"], expected_analyst);
        assert_eq!(intended["auditor"]["id"], expected_auditor);
        assert_eq!(intended["analyst"]["version"], version);
        assert_eq!(intended["auditor"]["version"], version);
        let fixture = specialists.analyst_fixture(provider.as_ref(), tempfile::tempdir().unwrap());
        fixture.run(provider.clone());
        let (analyst, passed) = task_evidence(&fixture, &fixture.id, false);
        assert!(passed);
        assert_eq!(analyst["detail"]["specialist"], intended["analyst"]);
        assert_eq!(
            analyst["detail"]["limits"],
            json!(InvestigationLimits::default())
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        // A separate failing scripted provider checks auditor dispatch and
        // history/report provenance even when no usable response is admitted.
        let auditor_provider = Arc::new(Scripted::new(Mode::Malformed));
        let (id, _) = auditor(&fixture, auditor_provider.clone(), specialists);
        let (auditor, passed) = task_evidence(&fixture, &id, true);
        assert!(!passed);
        assert_eq!(
            auditor["detail"]["status"],
            json!(InvestigationStatus::Failed)
        );
        assert_eq!(
            auditor["checks"]["parent_history_supplied_when_required"],
            true
        );
        assert_eq!(auditor["detail"]["specialist"], intended["auditor"]);
        assert_eq!(auditor["detail"]["limits"], analyst["detail"]["limits"]);
        assert_eq!(auditor_provider.calls.load(Ordering::SeqCst), 1);
        if version == 4 {
            let prompts = auditor_provider.prompts.lock().unwrap();
            assert_eq!(prompts[0]["turn_progress"]["phase"], "initial");
            assert!(
                !prompts[0]["prior_findings_as_t3_history"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        fingerprints.push((
            intended["analyst"]["prompt_fingerprint"].clone(),
            intended["auditor"]["prompt_fingerprint"].clone(),
        ));
    }
    for (index, current) in fingerprints.iter().enumerate() {
        for other in &fingerprints[index + 1..] {
            assert_ne!(current.0, other.0);
            assert_ne!(current.1, other.1);
        }
    }
}

#[test]
fn investigation_prior_finding_replay_allows_one_bounded_finish_correction() {
    // AC-0184 (#937): the rejected finish remains discarded and one fixed,
    // durable finish-only correction may use the unchanged source ledger.
    let parent_provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = Fixture::new(parent_provider.as_ref());
    fixture.run(parent_provider.clone());
    let state = fixture.app.state::<AppState>();
    let parent = state
        .jobs
        .lock()
        .unwrap()
        .investigation_result(&fixture.id)
        .unwrap()
        .unwrap();
    let provider = Arc::new(Scripted::new(Mode::PriorFindingCorrection));
    let (id, _) = auditor(&fixture, provider.clone(), AcceptanceSpecialists::V2);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "exactly one finish-only correction follows the replay refusal"
    );
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    let result = jobs.investigation_result(&id).unwrap().unwrap();
    assert_ne!(result.findings[0].statement, parent.findings[0].statement);
    let events = ordered_events(&jobs, &id);
    let refusal = events
        .iter()
        .find(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
        .expect("the original finish refusal is durable");
    assert_eq!(refusal.step_id.as_deref(), Some("step-3"));
    assert_eq!(refusal.summary, "A source-replay finish was discarded.");
    assert!(events.iter().any(|event| {
        event.kind == InvestigationEventKind::ModelStarted
            && event.sequence > refusal.sequence
            && event.step_id.as_deref() == Some("step-4")
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
            .count(),
        1
    );
    assert_eq!(
        provider.prompts.lock().unwrap()[3]["finish_correction"]["refusal"],
        "source_replay"
    );
    assert!(
        !events.iter().any(|event| {
            event.kind == InvestigationEventKind::ModelCompleted
                && event.step_id.as_deref() == Some("step-3")
        }),
        "the rejected response is not admitted into the event history"
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), detail);
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
    assert_eq!(ordered_events(&reopened, &id), events);
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(parent)
    );
}

#[test]
fn investigation_second_prior_finding_replay_is_terminal() {
    let parent_provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = Fixture::new(parent_provider.as_ref());
    fixture.run(parent_provider);
    let state = fixture.app.state::<AppState>();
    let parent = state
        .jobs
        .lock()
        .unwrap()
        .investigation_result(&fixture.id)
        .unwrap()
        .unwrap();
    let provider = Arc::new(Scripted::new(Mode::PriorFindingReplay));
    let (id, _) = auditor(&fixture, provider.clone(), AcceptanceSpecialists::V2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    refusal_diagnostics::assert_saved_refusal(
        &fixture,
        &id,
        "Investigation finish refused: source_replay.",
        &parent.findings[0].statement,
    );
    let jobs = state.jobs.lock().unwrap();
    let events = ordered_events(&jobs, &id);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
            .count(),
        1,
        "the corrective attempt cannot open a second correction"
    );
}

#[test]
fn investigation_correction_tool_proposal_is_terminal_without_tool_work() {
    let parent_provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = Fixture::new(parent_provider.as_ref());
    fixture.run(parent_provider);
    let provider = Arc::new(Scripted::new(Mode::PriorFindingCorrectionToolProposal));
    let (id, _) = auditor(&fixture, provider.clone(), AcceptanceSpecialists::V2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some("The provider proposed a tool when only a finishing response was permitted.")
    );
    let events = ordered_events(&jobs, &id);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
            .count(),
        1
    );
    let refusal_sequence = events
        .iter()
        .find(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
        .unwrap()
        .sequence;
    assert!(!events.iter().any(|event| {
        event.sequence > refusal_sequence
            && matches!(
                event.kind,
                InvestigationEventKind::ToolStarted | InvestigationEventKind::ToolCompleted
            )
    }));
    assert_eq!(
        provider.prompts.lock().unwrap()[3]["finish_correction"]["refusal"],
        "source_replay"
    );
}

#[test]
fn investigation_correction_provider_failure_is_terminal() {
    let parent_provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = Fixture::new(parent_provider.as_ref());
    fixture.run(parent_provider);
    let provider = Arc::new(Scripted::new(Mode::PriorFindingCorrectionProviderFailure));
    let (id, _) = auditor(&fixture, provider.clone(), AcceptanceSpecialists::V2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some("Provider call failed: transport.")
    );
    let events = ordered_events(&jobs, &id);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
            .count(),
        1
    );
    assert_eq!(
        provider.prompts.lock().unwrap()[3]["finish_correction"]["refusal"],
        "source_replay"
    );
}
