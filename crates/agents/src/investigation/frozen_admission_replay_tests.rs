//! AC-0287: opt-in offline replay of locally retained run artifacts. No dispatch.
use super::*;
use context_hub::coverage::QualifiedRequest;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize)]
struct FrozenReplay {
    objects: std::collections::BTreeMap<String, String>,
    next: Option<HistoricalNext>,
    ledgers: Vec<InvestigationInputLedger>,
    ledger: InvestigationInputLedger,
    previous: InvestigationInputLedger,
    tool: InvestigationTool,
    request: obligations::StagedObligationRequest,
    usage: InvestigationUsage,
    metadata: staged_refusal::ProtectedCompletionMetadata,
    contract: InvestigationCompletionContract,
    attempts: Vec<staged_attempt::AttemptRecord>,
    archive: InvestigationCoverageArchive,
    evidence: Vec<crate::AgentEvidence>,
    options: Vec<Options>,
    nodes: Vec<core_graph::Node>,
    edges: Vec<core_graph::Edge>,
    target: InvestigationInspectionTarget,
    source: String,
    task_id: String,
    input_bytes: u64,
}
#[derive(Clone, Deserialize)]
struct HistoricalNext {
    ledger: InvestigationInputLedger,
    usage: InvestigationUsage,
    metadata: staged_refusal::ProtectedCompletionMetadata,
    attempts: Vec<staged_attempt::AttemptRecord>,
    evidence: Vec<crate::AgentEvidence>,
    input_bytes: u64,
}
#[derive(Clone, Deserialize, Serialize)]
struct Options {
    fact: crate::TaskFactKey,
    origin: String,
    ranges: Vec<Range>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Range {
    role: crate::TaskRangeRole,
    index: u32,
    source: core_prov::EvidenceRef,
}
#[derive(Serialize)]
struct Page<'a> {
    context: context_hub::coverage::QualifiedResponse<context_hub::QueryResponse>,
    evidence_options: Vec<&'a Options>,
}

#[test]
#[ignore = "requires CARTOGRAPH_FROZEN_REPLAY; read-only local artifacts"]
fn frozen_run_production_admission_replay() {
    let data = std::fs::read(std::env::var("CARTOGRAPH_FROZEN_REPLAY").unwrap()).unwrap();
    let f: FrozenReplay = serde_json::from_slice(&data).unwrap();
    let policy = &INVESTIGATION_V2_POLICY;
    f.ledger.validate_with_policy(policy).unwrap();
    let captured_text = |span: &crate::TaskCaptureSpanRef| {
        let object = f.objects.get(&span.file.digest).unwrap();
        assert_eq!(core_prov::content_hash(object.as_bytes()), span.file.digest);
        assert_eq!(object.len() as u64, span.file.byte_len);
        object
            .get(span.byte_start as usize..span.byte_end as usize)
            .unwrap()
    };
    assert_eq!(captured_text(&f.target.captured), f.source);
    let verify_evidence = |ledger: &InvestigationInputLedger, evidence: &[crate::AgentEvidence]| {
        for item in evidence {
            let citation = ledger
                .citations
                .iter()
                .find(|c| c.citation_id == item.id)
                .unwrap();
            let InvestigationEvidenceOrigin::CapturedPrimarySource { captured, .. } =
                &citation.origin
            else {
                panic!("expected captured source")
            };
            assert_eq!(Some(&item.source), citation.source.as_ref());
            assert_eq!(captured_text(captured), item.text);
        }
    };
    verify_evidence(&f.ledger, &f.evidence);
    if let Some(next) = &f.next {
        verify_evidence(&next.ledger, &next.evidence);
    }

    let snapshot = context_hub::ContextSnapshot::new(f.nodes.clone(), f.edges.clone()).unwrap();
    assert_eq!(snapshot.id(), f.ledger.scope_snapshot_id);
    let qualified = snapshot
        .with_coverage(f.ledger.coverage.as_ref().unwrap().qualification.clone())
        .unwrap();
    let mut pages = Vec::new();
    for manifest in &f.ledger.queries {
        let query: context_hub::QueryRequest =
            serde_json::from_value(serde_json::to_value(&manifest.query).unwrap()).unwrap();
        let context = qualified
            .query(QualifiedRequest {
                request: query,
                context_id: Some(qualified.context_id().into()),
            })
            .unwrap();
        let evidence_options = context
            .response
            .facts
            .iter()
            .map(|fact| {
                let key: crate::TaskFactKey =
                    serde_json::from_value(serde_json::to_value(&fact.reference).unwrap()).unwrap();
                f.options.iter().find(|option| option.fact == key).unwrap()
            })
            .collect();
        let raw = serde_json::to_string(&Page {
            context,
            evidence_options,
        })
        .unwrap();
        assert_eq!(raw.len(), manifest.response_bytes);
        assert_eq!(
            core_prov::content_hash(raw.as_bytes()),
            manifest.response_hash
        );
        pages.push(raw);
    }
    let inventories = f
        .ledger
        .coverage_reads
        .iter()
        .map(|manifest| {
            let raw = serde_json::to_string(&f.archive.read(&manifest.request).unwrap()).unwrap();
            assert_eq!(raw.len(), manifest.response_bytes);
            assert_eq!(
                core_prov::content_hash(raw.as_bytes()),
                manifest.response_hash
            );
            raw
        })
        .collect::<Vec<_>>();
    eprintln!(
        "reconstructed {} query pages and {} inventory pages with exact saved hashes",
        pages.len(),
        inventories.len()
    );
    let original_role = if f.request.specialist_id == "domain-analyst@23" {
        SpecialistId::DomainAnalystV23
    } else {
        SpecialistId::DomainAnalystV22
    };
    let input = build(&f, &pages, &inventories, original_role);
    let action = input.action(format!("{}:step-15", f.task_id)).unwrap();
    let limits = CompletionLimits::default();
    let measurement = measure_bounded_action(&profile(), &action, &limits).unwrap();
    eprintln!(
        "historical production payload={} saved={}",
        measurement.original_payload_bytes, f.input_bytes
    );
    assert_eq!(measurement.original_payload_bytes, f.input_bytes);
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let prepared = firewall
        .prepare_bounded(&DescriptorOnly, &action, &limits)
        .unwrap();
    if original_role == SpecialistId::DomainAnalystV23 {
        assert_eq!(
            input.certify_unread_source_v9_with_policy(
                &prepared,
                &f.target,
                &profile(),
                &limits,
                &format!("{}:step-16", f.task_id),
                policy,
            ),
            Err(InvestigationError::LimitExceeded)
        );
    }
    if let Some(next) = &f.next {
        let mut after = f.clone();
        after.previous = f.ledger.clone();
        after.ledger = next.ledger.clone();
        after.usage = next.usage.clone();
        after.metadata = next.metadata.clone();
        after.attempts = next.attempts.clone();
        after.evidence = next.evidence.clone();
        let actual = build(&after, &pages, &inventories, original_role);
        let measured = measure_bounded_action(
            &profile(),
            &actual.action(format!("{}:step-16", f.task_id)).unwrap(),
            &limits,
        )
        .unwrap();
        assert_eq!(next.input_bytes, 72369);
        assert_eq!(measured.original_payload_bytes, next.input_bytes);
        eprintln!(
            "r3 actual next-payload golden reproduced: {}",
            measured.original_payload_bytes
        );
    }
    // Counterfactual family migration changes only role/request/attempt identity.
    // Recompute identities; never present the modified contract as saved authority.
    for role in [
        SpecialistId::DomainAnalystV24,
        SpecialistId::DomainAnalystV25,
    ] {
        let mut f = f.clone();
        f.request.specialist_id = serde_json::to_value(role).unwrap().as_str().unwrap().into();
        let request_hash = f.request.intent_hash_with_policy(policy).unwrap();
        f.contract.request_hash = request_hash.clone();
        f.metadata.request_hash = request_hash.clone();
        for record in &mut f.attempts {
            record.request_hash = request_hash.clone();
            record.attempt_id = record.content_id().unwrap();
        }
        f.metadata.completed_attempt_ids =
            f.attempts.iter().map(|r| r.attempt_id.clone()).collect();
        let input = build(&f, &pages, &inventories, role);
        let action = input.action(format!("{}:step-15", f.task_id)).unwrap();
        let prepared = firewall
            .prepare_bounded(&DescriptorOnly, &action, &limits)
            .unwrap();
        let sizes = core_redact::measure_direct_source_json_content(&f.source).unwrap();
        let bound = input
            .certify_measured_unread_source_v9_with_policy(
                &prepared,
                &f.target,
                &profile(),
                &limits,
                &format!("{}:step-16", f.task_id),
                policy,
                sizes,
            )
            .unwrap();
        eprintln!(
            "joint {:?} production bound: {:?}; original={} redacted={}",
            role,
            bound.continuation(),
            sizes.original,
            sizes.redacted
        );
        assert_eq!(
            bound.continuation().choice.original_payload_bytes,
            if original_role == SpecialistId::DomainAnalystV23 {
                126365
            } else {
                126359
            }
        );
        assert!(bound.continuation().choice.original_payload_bytes <= 131072);
        assert!(bound.continuation().choice.redacted_payload_bytes <= 131072);
        eprintln!(
            "{:?} choice headroom={}",
            role,
            131072 - bound.continuation().choice.original_payload_bytes
        );
    }
}

fn build(
    f: &FrozenReplay,
    pages: &[String],
    inventories: &[String],
    role: SpecialistId,
) -> InvestigationInput {
    let policy = &INVESTIGATION_V2_POLICY;
    let input = InvestigationInput::prepare_v9_with_policy(
        &f.request.question,
        role,
        f.ledger.clone(),
        &pages.iter().map(String::as_str).collect::<Vec<_>>(),
        &f.evidence,
        &[],
        &f.usage,
        &InvestigationTurnBasis {
            input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: Some(f.previous.clone()),
            completed_tool: Some(f.tool),
        },
        &f.request,
        &f.metadata,
        &inventories.iter().map(String::as_str).collect::<Vec<_>>(),
        CompletionCallMode::Choice,
        0,
        None,
        policy,
    )
    .unwrap();
    let attempts = f
        .attempts
        .iter()
        .map(|record| staged_attempt::AttemptEvidence {
            record,
            ledger: f
                .ledgers
                .iter()
                .find(|ledger| ledger.revision == record.input_revision)
                .unwrap(),
        })
        .collect::<Vec<_>>();
    let choices = acquisition_choices::AcquisitionChoices::derive(
        &f.contract,
        &f.metadata,
        &f.task_id,
        &f.ledger,
        &attempts,
        policy,
    )
    .unwrap();
    input
        .with_acquisition_choices(std::sync::Arc::new(choices))
        .unwrap()
}
