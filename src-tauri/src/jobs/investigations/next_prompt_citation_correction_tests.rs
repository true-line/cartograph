//! AC-0565: synthetic source target in a genuine @19 start, then host invocation/refusal
//! transactions. No provider dispatch or source read is performed by this fixture.
use super::*;

fn correction_fixture() -> Fixture {
    use completion::next_prompt_snapshot::tests::fixture::Fixture as Acquisition;
    let mut target_id = String::new();
    let base = Acquisition::with_request(false, |request, archive| {
        let mut source: InvestigationInspectionTarget = serde_json::from_value(serde_json::json!({
            "schema_version":1,"target_id":"","graph_snapshot_id":archive.proof().snapshot_id,
            "fact":{"kind":"node","id":"node:checkout"},"fact_digest":format!("node-v1:{}","b".repeat(64)),
            "registered_source_id":format!("src_{}","d".repeat(32)),"receipt_id":format!("ts-primary-v2:{}","c".repeat(64)),
            "receipt_inventory_index":0,"role":"provenance","index":0,
            "source":{"repo":"fixture/repo","path":"guard.ts","byte_start":0,"byte_end":12,"commit_sha":"pinned-revision"},
            "captured":{"file":{"source_id":format!("src_{}","d".repeat(32)),"capture_id":format!("capture-v1:{}","e".repeat(64)),
            "path":"guard.ts","digest":core_prov::content_hash(b"private text"),"byte_len":12},"byte_start":0,"byte_end":12}
        })).unwrap();
        source.target_id = source.content_id().unwrap();
        source.validate().unwrap();
        target_id = source.target_id.clone();
        let mut obligation = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(source),
        };
        let id = obligation.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut obligation {
            *obligation_id = id;
        }
        request.specialist_id = "domain-analyst@19".into();
        request.expected_graph_revision = Some(archive.proof().snapshot_id.clone());
        request.obligations.obligations.push(obligation);
        request.obligations = request
            .obligations
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
    });
    let mut f = Fixture::from_base(base, false);
    f.original = serde_json::json!({
        "schema_version":1,"outcome":"supported",
        "action":{"type":"finish","findings":[{
            "claim_kind":"inferred_interpretation","title":"Withheld candidate",
            "statement":"This candidate still needs a source check.","citation_ids":[target_id],
            "limitations":["Only selected material was considered."]
        }],"knowledge_completeness":"partial","limitations":["Only selected material was considered."]}
    });
    f.response.action_hash =
        core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
    f
}

#[test]
fn citation_correction_is_once_only_after_reopen_and_duplicate_publication() {
    let mut f = correction_fixture();
    f.begin();
    let charged = f.base.f.task.detail.usage.clone();
    let first = f.publish().unwrap();
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    assert_eq!(first.usage.model_invocations, charged.model_invocations);
    assert_eq!(first.usage.tool_actions, charged.tool_actions);
    assert!(!first.summary.has_result);
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(f.base.id()).unwrap(), first);
    assert!(
        reopened
            .investigation_result(f.base.id())
            .unwrap()
            .is_none()
    );
    let before = f.rows();
    assert_eq!(f.publish(), Err(InvestigationStoreError::Stale));
    assert_eq!(f.rows(), before);
    assert!(
        before
            .iter()
            .all(|row| !String::from_utf8_lossy(row).contains("Withheld candidate"))
    );
    f.next();
    f.begin();
    let second = f.publish().unwrap();
    assert_eq!(second.summary.status, InvestigationStatus::Failed);
    assert_eq!(second.completion.as_ref().unwrap().refusal_count, 2);
    assert!(!second.summary.has_result);
    assert_eq!(reopened.investigation(f.base.id()).unwrap(), second);
}

#[test]
fn citation_correction_late_cancel_never_offers_another_call() {
    let mut f = correction_fixture();
    f.begin();
    let id = f.base.id().to_owned();
    f.base
        .f
        .store
        .cancel_next_prompt_investigation(&id)
        .unwrap();
    f.base.refresh();
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
    assert_eq!(f.saved().phase, Phase::Terminal);
    assert!(!detail.summary.has_result);
}

#[test]
fn citation_correction_mixed_foreign_id_is_terminal_without_feedback_allowance() {
    let mut f = correction_fixture();
    f.original["action"]["findings"][0]["citation_ids"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("foreign-target"));
    f.response.action_hash =
        core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
    f.begin();
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 0);
    assert_eq!(f.saved().phase, Phase::Terminal);
    assert!(!detail.summary.has_result);
}

#[test]
fn citation_correction_stale_revision_cannot_consume_allowance() {
    let mut f = correction_fixture();
    f.begin();
    let revision = f.base.f.task.detail.summary.revision;
    assert!(revision > 0);
    let before = f.rows();
    f.base.f.task.detail.summary.revision = revision - 1;
    assert_eq!(f.publish(), Err(InvestigationStoreError::Stale));
    assert_eq!(f.rows(), before);
    f.base.f.task.detail.summary.revision = revision;
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Running);
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 1);
}

#[test]
fn citation_correction_shares_allowance_with_ordinary_finish_refusal() {
    let mut f = correction_fixture();
    let correction = f.original.clone();
    f.original = serde_json::json!({
        "schema_version":1,"outcome":"insufficient_evidence",
        "action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
            "limitations":["The required passages have not been inspected."]}
    });
    f.response.action_hash =
        core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
    f.begin();
    let first = f.publish().unwrap();
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    f.original = correction;
    f.next();
    f.begin();
    let second = f.publish().unwrap();
    assert_eq!(second.summary.status, InvestigationStatus::Failed);
    assert_eq!(second.completion.as_ref().unwrap().refusal_count, 2);
    assert!(!second.summary.has_result);
}
