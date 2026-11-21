//! AC-0430: authenticated invalid citations retain a content-free terminal reason.
use super::*;

#[test]
fn next_prompt_invalid_finish_is_journaled_with_usage_and_reopens() {
    for cancel in [false, true] {
        let mut f = gated_finish_fixture(false);
        f.original = serde_json::json!({
            "schema_version": 1, "outcome": "supported",
            "action": {"type": "finish", "findings": [{
                "claim_kind": "inferred_interpretation",
                "title": "Unadmitted finding sentinel",
                "statement": "Unadmitted finding content must never be retained.",
                "citation_ids": [f.input.ledger().coverage_reads[0].request.inventory_hash], "limitations": ["The available material does not establish the endpoint behavior."]
            }], "knowledge_completeness": "partial", "limitations": ["The available material does not establish the endpoint behavior."]}
        });
        f.response.action_hash =
            core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
        f.begin();
        let charged = f.base.f.task.detail.usage.clone();
        if cancel {
            let id = f.base.id().to_owned();
            f.base
                .f
                .store
                .cancel_next_prompt_investigation(&id)
                .unwrap();
            f.base.refresh();
        }
        let detail = f.publish().unwrap();
        assert_eq!(
            detail.summary.status,
            if cancel {
                InvestigationStatus::Cancelled
            } else {
                InvestigationStatus::Failed
            }
        );
        assert_eq!(
            detail.error.as_deref(),
            Some("The model's answer cited evidence it had not acquired. No findings were saved.")
        );
        assert!(!detail.summary.has_result);
        // AC-0591: this historical role has no typed-correction capability.
        assert!(
            !detail
                .summary
                .specialist_id
                .uses_typed_citation_correction()
        );
        assert_eq!(
            detail.terminal_diagnostic,
            Some(
                agents::investigation::InvestigationTerminalDiagnostic::RejectedFinish {
                    reason: agents::investigation::InvestigationFinishRejection::InvalidCitation,
                }
            )
        );
        assert_eq!(detail.usage.model_invocations, charged.model_invocations);
        assert_eq!(detail.usage.tool_actions, charged.tool_actions);
        assert_eq!(
            detail.usage.reported_input_tokens,
            Some(charged.reported_input_tokens.unwrap_or(0) + 17)
        );
        assert_eq!(
            detail.usage.reported_output_tokens,
            Some(charged.reported_output_tokens.unwrap_or(0) + 3)
        );
        let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(f.base.id()).unwrap(), detail);
        assert!(
            reopened
                .investigation_result(f.base.id())
                .unwrap()
                .is_none()
        );
        let rows = f.rows();
        let events = reopened.investigation_events(f.base.id(), 0).unwrap();
        for kind in [
            InvestigationEventKind::FinishRefused,
            if cancel {
                InvestigationEventKind::Cancelled
            } else {
                InvestigationEventKind::Failed
            },
        ] {
            let event = events
                .items
                .iter()
                .find(|event| event.kind == kind)
                .unwrap();
            assert_eq!(
                event.summary,
                "The model's answer cited evidence it had not acquired. No findings were saved."
            );
            assert_eq!(event.step_id.as_deref(), Some(f.response.step_id.as_str()));
        }
        assert!(
            !rows
                .iter()
                .any(|row| String::from_utf8_lossy(row).contains("Unadmitted finding"))
        );
        let before = f.rows();
        assert_eq!(f.publish(), Err(InvestigationStoreError::Stale));
        assert_eq!(f.rows(), before);

        // Reproduce the pre-diagnostic terminal wire shape. Only the newly
        // optional field is omitted; the genuine response, event and admission
        // journal rows above remain byte-identical and must still authenticate.
        let mut historical = f.saved();
        historical.detail.terminal_diagnostic = None;
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        admission_journal::tests::save_task(&tx, &historical);
        tx.commit().unwrap();
        let legacy = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
        let mut legacy_detail = detail.clone();
        legacy_detail.terminal_diagnostic = None;
        assert_eq!(legacy.investigation(f.base.id()).unwrap(), legacy_detail);
        assert_eq!(legacy.investigation_events(f.base.id(), 0).unwrap(), events);
        assert!(legacy.investigation_result(f.base.id()).unwrap().is_none());
    }
}

// AC-1404 (ADR-0076): a finish refused at admission keeps the bounded
// redacted record of the response the worker handed over, in the same
// transaction, without changing the terminal outcome.
#[test]
fn refused_finish_keeps_bounded_redacted_rejected_response() {
    for bound in [false, true] {
        refused_finish_record_case(bound);
    }
}

fn refused_finish_record_case(bound: bool) {
    let mut f = gated_finish_fixture(false);
    f.original = serde_json::json!({
        "schema_version": 1, "outcome": "supported",
        "action": {"type": "finish", "findings": [{
            "claim_kind": "inferred_interpretation",
            "title": "Unadmitted finding sentinel",
            "statement": "Unadmitted finding content must never be retained.",
            "citation_ids": [f.input.ledger().coverage_reads[0].request.inventory_hash], "limitations": ["The available material does not establish the endpoint behavior."]
        }], "knowledge_completeness": "partial", "limitations": ["The available material does not establish the endpoint behavior."]}
    });
    f.response.action_hash =
        core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
    f.begin();
    let text = format!(
        "{} token=AKIA0000000000000000 {}",
        f.original,
        "x".repeat(2048)
    );
    if bound {
        f.response.response_hash = core_prov::content_hash(text.as_bytes());
    }
    let rejected = RejectedCompletion {
        step_id: f.response.step_id.clone(),
        text: text.clone(),
    };
    let detail = f.publish_keeping(Some(&rejected)).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.terminal_diagnostic,
        Some(
            agents::investigation::InvestigationTerminalDiagnostic::RejectedFinish {
                reason: agents::investigation::InvestigationFinishRejection::InvalidCitation,
            }
        )
    );
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(f.base.id()).unwrap(), detail);
    let kept = reopened
        .investigation_rejected_response(f.base.id())
        .unwrap();
    // A completion that is not the authenticated response is never kept.
    if !bound {
        assert_eq!(kept, None);
        return;
    }
    let kept = kept.unwrap();
    assert_eq!(
        kept,
        agents::investigation::rejected_response::RejectedResponse::capture(
            &f.response.step_id,
            &text,
            detail.terminal_diagnostic.as_ref(),
        )
        .unwrap()
    );
    let agents::investigation::rejected_response::ExcerptRetention::Excerpt {
        text: excerpt,
        truncated,
        redactions,
    } = kept.retention
    else {
        panic!("expected an excerpt");
    };
    assert!(excerpt.contains("Unadmitted finding sentinel"));
    assert!(!excerpt.contains("AKIA0000000000000000"));
    assert!(excerpt.len() <= 1024 && truncated && redactions == 1);
    assert_eq!(kept.response_bytes, text.len() as u64);
}
