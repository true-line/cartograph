//! AC-0287: real captured evidence, correction, partial publication and reopen.
use super::*;

impl CapturedSourceInvocationFixture {
    pub fn assert_diagnostic_retention_invariant(
        &mut self,
        provider: &dyn llm::LlmProvider,
        firewall: &llm::EgressFirewall,
        citation: &str,
        pages: &[String],
        evidence: &[agents::AgentEvidence],
    ) {
        let _clock = refusal_diagnostics::fix_timestamp(timestamp(&self.inner.store.conn).unwrap());
        let mut snapshot = Connection::open_in_memory().unwrap();
        assert_eq!(
            rusqlite::backup::Backup::new(&self.inner.store.conn, &mut snapshot)
                .unwrap()
                .step(-1)
                .unwrap(),
            rusqlite::backup::StepResult::Done
        );
        let omitted = refusal_diagnostics::without_detail_room(|| {
            self.assert_partial_after_capture(provider, firewall, citation, pages, evidence)
        });
        assert!(
            self.inner
                .store
                .investigation_refusal_diagnostics(self.id())
                .unwrap()
                .iter()
                .all(|s| matches!(
                    s.retention,
                    agents::investigation::diagnostic_sidecar::Retention::Unrecorded { .. }
                ))
        );
        // Restore through SQLite while the fixture's other connections remain open.
        assert_eq!(
            rusqlite::backup::Backup::new(&snapshot, &mut self.inner.store.conn)
                .unwrap()
                .step(-1)
                .unwrap(),
            rusqlite::backup::StepResult::Done
        );
        let retained =
            self.assert_partial_after_capture(provider, firewall, citation, pages, evidence);
        assert!(
            self.inner
                .store
                .investigation_refusal_diagnostics(self.id())
                .unwrap()
                .iter()
                .all(|s| matches!(
                    s.retention,
                    agents::investigation::diagnostic_sidecar::Retention::Recorded { .. }
                ))
        );
        assert_eq!(
            rusqlite::backup::Backup::new(&snapshot, &mut self.inner.store.conn)
                .unwrap()
                .step(-1)
                .unwrap(),
            rusqlite::backup::StepResult::Done
        );
        let rejected = refusal_diagnostics::with_collector_rejected(|| {
            self.assert_partial_after_capture(provider, firewall, citation, pages, evidence)
        });
        assert!(self.inner.store.investigation_refusal_diagnostics(self.id()).unwrap()
            .iter().all(|s| matches!(s.retention,
                agents::investigation::diagnostic_sidecar::Retention::Unrecorded {
                    reason: agents::investigation::diagnostic_sidecar::UnrecordedReason::CollectorRejected,
                    omitted_entries: 0,
                })));
        assert_eq!(
            rejected, retained,
            "collector failure changed execution observations"
        );
        assert_eq!(omitted.len(), retained.len());
        for (index, (left, right)) in omitted.iter().zip(&retained).enumerate() {
            assert!(
                left == right,
                "diagnostic retention changed execution observation {index}"
            );
        }
    }

    pub fn assert_partial_after_capture(
        &mut self,
        provider: &dyn llm::LlmProvider,
        firewall: &llm::EgressFirewall,
        citation: &str,
        pages: &[String],
        evidence: &[agents::AgentEvidence],
    ) -> Vec<Value> {
        let mut observations = Vec::new();
        for turn in 0..2 {
            let snapshot = self
                .inner
                .store
                .investigation_next_prompt_snapshot(self.id(), &self.inner.request)
                .unwrap();
            let mut usage = snapshot.detail().usage.clone();
            usage.active_milliseconds += 1;
            let current = InvestigationInput::prepare_v9_with_policy(
                &self.inner.request.question,
                snapshot.detail().summary.specialist_id,
                snapshot.ledger().clone(),
                &pages.iter().map(String::as_str).collect::<Vec<_>>(),
                evidence,
                &[],
                &usage,
                snapshot.basis(),
                &self.inner.request,
                snapshot.metadata(),
                &[],
                snapshot.mode(),
                snapshot.capacity_refusal_count(),
                snapshot.capacity_feedback(),
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
            let current = snapshot.attach_acquisition(current).unwrap();
            let mut limits = CompletionLimits::default().values();
            let tx = self.inner.store.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, self.id()).unwrap();
            limits.request_timeout_ms = task
                .completion_intent
                .as_ref()
                .unwrap()
                .effective_request_timeout_ms();
            drop(tx);
            limits.max_output_tokens = INVESTIGATION_V2_POLICY
                .limits()
                .generated_tokens_per_invocation;
            limits.max_spans = INVESTIGATION_V2_POLICY.limits().evidence_items as u32;
            let prepared = firewall
                .prepare_bounded(
                    provider,
                    &current
                        .action(format!(
                            "{}:step-{}",
                            self.id(),
                            usage.model_invocations + 1
                        ))
                        .unwrap(),
                    &CompletionLimits::new(limits).unwrap(),
                )
                .unwrap();
            let authorized = firewall
                .authorize_bounded(provider, &prepared, None)
                .unwrap();
            observations.push(serde_json::to_value(authorized.payload()).unwrap());
            let invoked = self
                .inner
                .store
                .begin_next_prompt_invocation(
                    &self.inner._execution_guard,
                    snapshot.detail().summary.revision,
                    &NextPromptInvocation {
                        input: &current,
                        prepared: &prepared,
                        active_milliseconds: usage.active_milliseconds,
                    },
                    &authorized,
                    |_| Ok(()),
                )
                .unwrap();
            let good = json!({"claim_kind":"inferred_interpretation","title":"Selected evidence",
                "statement":"The selected material provides a bounded implementation view.","citation_ids":[citation],
                "limitations":["Only selected material was considered."]});
            // AC-0566: preserve the r6-shaped @25 bare limitation through live
            // admission and saved-result reopen; @26 uses ordinary survivor prose.
            let mut good = good;
            if snapshot.detail().summary.specialist_id == SpecialistId::DomainAnalystV25 {
                good["limitations"] = json!(["statement"]);
            }
            let bad = json!({"claim_kind":"inferred_interpretation","title":"Default MissingStrategy",
                "statement":"MissingStrategy controls this behavior.","citation_ids":[citation],
                "limitations":["Only selected material was considered."]});
            let mut second = good.clone();
            second["title"] = json!("Bounded evidence scope");
            let findings = if turn == 0 {
                vec![good, bad, second]
            } else {
                vec![bad, good, second]
            };
            let original = json!({"schema_version":1,"outcome":"supported","action":{
                "type":"finish","findings":findings,"knowledge_completeness":"partial",
                "limitations":["Only selected material was considered."]}});
            let response = InvestigationResponseMeta {
                finish_feedback_witness: None,
                finish_telemetry: None,
                step_id: format!("step-{}", invoked.usage.model_invocations),
                response_hash: core_prov::content_hash(
                    format!("partial fixture response {turn}").as_bytes(),
                ),
                action_hash: core_prov::content_hash(&encode(&original, MAX_ACTION_BYTES).unwrap()),
                observed_model: None,
                reported_input_tokens: Some(17),
                reported_output_tokens: Some(9),
                reported_cache_read_tokens: None,
                active_milliseconds: invoked.usage.active_milliseconds + 1,
            };
            let mut before_finish = Connection::open_in_memory().unwrap();
            if turn == 1 {
                rusqlite::backup::Backup::new(&self.inner.store.conn, &mut before_finish)
                    .unwrap()
                    .step(-1)
                    .unwrap();
            }
            let detail = self
                .inner
                .store
                .admit_next_prompt_finish(
                    &self.inner._execution_guard,
                    invoked.summary.revision,
                    &NextPromptFinish {
                        request: &self.inner.request,
                        response: &response,
                        original_action: &original,
                        current: &current,
                        current_prepared: &prepared,
                    },
                )
                .unwrap();
            if turn == 1 {
                // AC-0287: a later real finish admission has the same exact-fit
                // boundary after either retained or omitted refusal diagnostics.
                let mut completed = Connection::open_in_memory().unwrap();
                rusqlite::backup::Backup::new(&self.inner.store.conn, &mut completed)
                    .unwrap()
                    .step(-1)
                    .unwrap();
                let used = {
                    let tx = self.inner.store.conn.unchecked_transaction().unwrap();
                    stored_bytes(&tx, Some(self.id())).unwrap()
                };
                let before_used = {
                    let tx = before_finish.unchecked_transaction().unwrap();
                    stored_bytes(&tx, Some(self.id())).unwrap()
                };
                // Find the real admission boundary, including its protected
                // publication reservation, rather than guessing from final size.
                let mut admit_with_padding = |charged: usize| {
                    rusqlite::backup::Backup::new(&before_finish, &mut self.inner.store.conn)
                        .unwrap()
                        .step(-1)
                        .unwrap();
                    if charged != 0 {
                        // Unreferenced disposable rows exercise ordinary storage
                        // accounting while respecting replay's per-row preflight.
                        let rows = charged.div_ceil(4096 + 64 + 256);
                        let mut body_bytes = charged.checked_sub(rows * (64 + 256)).unwrap();
                        for row in 0..rows {
                            let size = body_bytes.min(4096);
                            self.inner.store.conn.execute(
                                "INSERT INTO investigation_responses VALUES (?1,?2,zeroblob(?3),?4)",
                                params![self.id(), format!("capacity-padding-{row}"), i64::try_from(size).unwrap(), "0".repeat(64)],
                            ).unwrap();
                            body_bytes -= size;
                        }
                        assert_eq!(body_bytes, 0);
                    }
                    self.inner.store.admit_next_prompt_finish(
                        &self.inner._execution_guard,
                        invoked.summary.revision,
                        &NextPromptFinish {
                            request: &self.inner.request,
                            response: &response,
                            original_action: &original,
                            current: &current,
                            current_prepared: &prepared,
                        },
                    )
                };
                assert_eq!(admit_with_padding(0).unwrap(), detail);
                let mut fits = 0;
                let mut refused = TASK_CAPACITY - before_used + 1;
                assert_eq!(
                    admit_with_padding(refused),
                    Err(InvestigationStoreError::Capacity)
                );
                while refused - fits > 1 {
                    let midpoint = fits + (refused - fits) / 2;
                    match admit_with_padding(midpoint) {
                        Ok(actual) => {
                            assert_eq!(actual, detail);
                            fits = midpoint;
                        }
                        Err(InvestigationStoreError::Capacity) => refused = midpoint,
                        Err(error) => panic!("invalid capacity fixture: {error:?}"),
                    }
                }
                assert!(fits > 4096);
                assert_eq!(admit_with_padding(fits).unwrap(), detail);
                assert_eq!(
                    admit_with_padding(fits + 1),
                    Err(InvestigationStoreError::Capacity)
                );
                observations
                    .push(json!({"later_finish_padding_limit": fits, "unpadded_bytes": used}));
                rusqlite::backup::Backup::new(&completed, &mut self.inner.store.conn)
                    .unwrap()
                    .step(-1)
                    .unwrap();
            }
            {
                let tx = self.inner.store.conn.unchecked_transaction().unwrap();
                observations.push(
                    json!({"status": detail.summary.status, "usage": detail.usage,
                    "task_bytes": stored_bytes(&tx, Some(self.id())).unwrap(),
                    "global_bytes": stored_bytes(&tx, None).unwrap()}),
                );
            }
            if turn == 0 {
                assert_eq!(detail.summary.status, InvestigationStatus::Running);
                assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 1);
            } else {
                if detail.summary.status != InvestigationStatus::Completed {
                    let tx = self.inner.store.conn.unchecked_transaction().unwrap();
                    let task = load_next_prompt_task(&tx, self.id()).unwrap();
                    let refusals = completion::raw_refusals(&tx, &task).unwrap();
                    panic!(
                        "partial failed: {:?}; feedback {:?}",
                        detail.error,
                        refusals
                            .iter()
                            .map(|e| &e.record.feedback)
                            .collect::<Vec<_>>()
                    );
                }
                let result = self
                    .inner
                    .store
                    .investigation_result(self.id())
                    .unwrap()
                    .unwrap();
                let completion = result.completion.as_ref().unwrap();
                assert_eq!(
                    completion.outcome,
                    CompletionOutcome::FindingsWithheldAfterCorrection
                );
                assert_eq!(
                    completion.withholding.as_ref().unwrap().withheld_indexes,
                    vec![0]
                );
                assert_eq!(result.findings.len(), 2);
                assert!(result.narrative.is_none());
                assert_eq!(
                    completion.withholding.as_ref().unwrap().survivor_indexes,
                    vec![1, 2]
                );
                for (index, finding) in result.findings.iter().enumerate() {
                    assert_eq!(finding.finding_id, format!("finding-{}", index + 1));
                    assert_eq!(finding.citation_ids, vec![citation.to_owned()]);
                }
                assert!(
                    !String::from_utf8(encode(&result, MAX_RESULT_BYTES).unwrap())
                        .unwrap()
                        .contains("MissingStrategy")
                );
                let audit = current
                    .semantic_audit_payload(&result, &INVESTIGATION_V2_POLICY)
                    .unwrap();
                let audit_prompt: Value = serde_json::from_str(&audit.payload().prompt).unwrap();
                assert_eq!(
                    audit_prompt["candidate"]["completion_disposition"]["outcome"],
                    "findings_withheld_after_correction"
                );
                assert_eq!(
                    audit_prompt["candidate"]["completion_disposition"]["message"],
                    agents::investigation::FINDINGS_WITHHELD_MESSAGE
                );
                assert!(!audit.payload().prompt.contains("MissingStrategy"));
                assert_eq!(
                    audit_prompt["candidate"]["findings"],
                    serde_json::to_value(&result.findings).unwrap()
                );
                assert!(audit_prompt["candidate"]["narrative"].is_null());
                observations.push(json!({"findings": result.findings,
                    "withheld": completion.withholding.as_ref().unwrap().withheld_indexes,
                    "survivors": completion.withholding.as_ref().unwrap().survivor_indexes,
                    "audit_payload": audit.payload()}));
                let history = self
                    .inner
                    .store
                    .investigation_next_parent_history(self.id(), &detail.summary.conversation_id)
                    .unwrap()
                    .into_history();
                let parent: Value = serde_json::from_str(&history[0]).unwrap();
                assert_eq!(parent["saved_execution_status"], "completed");
                assert_eq!(
                    parent["original_scope"],
                    serde_json::to_value(&detail.summary.scope).unwrap()
                );
                assert_eq!(
                    parent["saved_result"]["completion"]["outcome"],
                    "findings_withheld_after_correction"
                );
                assert!(!history[0].contains("MissingStrategy"));
                assert_eq!(
                    parent["saved_result"]["findings"],
                    serde_json::to_value(&result.findings).unwrap()
                );
                assert!(parent["saved_result"]["narrative"].is_null());
                let reopened =
                    JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
                assert_eq!(reopened.investigation(self.id()).unwrap(), detail);
                assert_eq!(
                    reopened.investigation_result(self.id()).unwrap().unwrap(),
                    result
                );
                assert_eq!(
                    serde_json::to_vec(&reopened.investigation_result(self.id()).unwrap().unwrap())
                        .unwrap(),
                    serde_json::to_vec(&result).unwrap(),
                );
                if detail.summary.specialist_id == SpecialistId::DomainAnalystV25 {
                    assert!(
                        result
                            .findings
                            .iter()
                            .any(|f| f.limitations.iter().any(|v| v == "statement"))
                    );
                }
                // Development projection authenticates both refusal and final sidecar.
                let projected = reopened
                    .investigation_refusal_diagnostics(self.id())
                    .unwrap();
                assert_eq!(projected.len(), 2);
                let projection_bytes = serde_json::to_string(&projected).unwrap();
                assert!(!projection_bytes.contains("MissingStrategy"));
                // AC-0287 / #1707: current proposal identity and sidecar loss.
                let sidecar = refusal_diagnostics::load(&reopened.conn, self.id(), 2)
                    .unwrap()
                    .unwrap();
                assert_eq!(projected[1], sidecar);
                assert_eq!(
                    sidecar.finish_hash,
                    completion
                        .withholding
                        .as_ref()
                        .unwrap()
                        .original_finish_hash
                );
                match &sidecar.retention {
                    agents::investigation::diagnostic_sidecar::Retention::Recorded {
                        entries,
                        ..
                    } => {
                        assert!(!entries.is_empty());
                        assert!(entries.iter().all(|entry| matches!(
                            entry.location.start.field,
                            agents::investigation::ProseField::FindingTitle { finding: 0 }
                                | agents::investigation::ProseField::FindingStatement {
                                    finding: 0
                                }
                        )));
                    }
                    agents::investigation::diagnostic_sidecar::Retention::Unrecorded {
                        omitted_entries,
                        reason,
                    } => assert!(*omitted_entries > 0 || *reason == agents::investigation::diagnostic_sidecar::UnrecordedReason::CollectorRejected),
                }
                for verb in ["delete", "update"] {
                    let tx = reopened.conn.unchecked_transaction().unwrap();
                    let task = load_next_prompt_task(&tx, self.id()).unwrap();
                    let trigger = format!("refusal_diagnostics_records_no_{verb}");
                    let sql: String = tx
                        .query_row(
                            "SELECT sql FROM sqlite_schema WHERE name=?1",
                            [&trigger],
                            |row| row.get(0),
                        )
                        .unwrap();
                    tx.execute_batch(&format!("DROP TRIGGER {trigger}"))
                        .unwrap();
                    if verb == "delete" {
                        tx.execute("DELETE FROM refusal_diagnostics_records WHERE task_id=?1 AND sequence=2", [self.id()]).unwrap();
                    } else {
                        let mut changed = sidecar.clone();
                        changed.finish_hash = "e".repeat(64);
                        let body = changed.bytes().unwrap();
                        tx.execute("UPDATE refusal_diagnostics_records SET body=?2,body_hash=?3 WHERE task_id=?1 AND sequence=2", params![self.id(), body, core_prov::content_hash(&body)]).unwrap();
                    }
                    tx.execute_batch(&sql).unwrap();
                    assert!(load_result(&tx, &task).is_err(), "sidecar {verb}");
                    assert!(refusal_diagnostics::project(&tx, self.id()).is_err());
                }
                for mutation in ["missing_witness", "changed_witness", "changed_response"] {
                    let tx = reopened.conn.unchecked_transaction().unwrap();
                    let task = load_next_prompt_task(&tx, self.id()).unwrap();
                    assert!(load_result(&tx, &task).unwrap().is_some());
                    let step = result.invocation_step_id.as_ref().unwrap();
                    let mut stored: InvestigationResponseMeta =
                        read_body(&tx, "investigation_responses", "step_id", self.id(), step)
                            .unwrap()
                            .unwrap();
                    match mutation {
                        "missing_witness" => stored.finish_feedback_witness = None,
                        "changed_witness" => stored.finish_feedback_witness = Some("f".repeat(64)),
                        _ => stored.response_hash = "f".repeat(64),
                    }
                    let trigger: String = tx.query_row("SELECT sql FROM sqlite_schema WHERE name='investigation_responses_no_update'", [], |row| row.get(0)).unwrap();
                    tx.execute_batch("DROP TRIGGER investigation_responses_no_update")
                        .unwrap();
                    let body = encode(&stored, 4096).unwrap();
                    tx.execute("UPDATE investigation_responses SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2", params![self.id(), step, body, core_prov::content_hash(&body)]).unwrap();
                    tx.execute_batch(&trigger).unwrap();
                    assert!(load_result(&tx, &task).is_err(), "{mutation}");
                    // Rollback restores authority and triggers before the next mutation.
                }
                assert_eq!(
                    reopened.investigation_result(self.id()).unwrap().unwrap(),
                    result
                );
            }
        }
        observations
    }
}
