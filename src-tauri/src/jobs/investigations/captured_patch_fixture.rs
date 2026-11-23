//! AC-0572: production captured evidence and host admission, no transport calls.
use super::*;
use agents::investigation::finish_patch::VolatileFinish;

impl CapturedSourceInvocationFixture {
    fn patch_invocation(
        &mut self,
        provider: &dyn llm::LlmProvider,
        firewall: &llm::EgressFirewall,
        pages: &[String],
        evidence: &[agents::AgentEvidence],
    ) -> (
        InvestigationInput,
        PreparedBoundedCompletion,
        InvestigationDetail,
    ) {
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
            snapshot.preparation_mode().unwrap(),
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
        (current, prepared, invoked)
    }

    pub fn assert_volatile_patch_after_capture(
        mut self,
        provider: &dyn llm::LlmProvider,
        firewall: &llm::EgressFirewall,
        citation: &str,
        pages: &[String],
        evidence: &[agents::AgentEvidence],
        case: &str,
    ) {
        if case == "audit_replace" {
            // Synthetic fixture setup only: seed the already-enabled audit
            // account before the first finish. No enable/dispatch API is widened.
            let tx = self.inner.store.conn.unchecked_transaction().unwrap();
            let mut task = load_next_prompt_task(&tx, self.id()).unwrap();
            let revision = task.detail.summary.revision;
            task.semantic_audit =
                Some(super::super::semantic_audit_state::fixture_enabled_account(&task).unwrap());
            bump(&tx, &mut task).unwrap();
            save_next_prompt_task(&tx, &task, revision, false).unwrap();
            tx.commit().unwrap();
        }
        let good = json!({"claim_kind":"inferred_interpretation","title":"Selected evidence",
            "statement":"The selected material provides a bounded implementation view.","citation_ids":[citation],
            "limitations":["Only selected material was considered."]});
        let bad = json!({"claim_kind":"inferred_interpretation","title":"Default MissingStrategy",
            "statement":"MissingStrategy controls this behavior.","citation_ids":[citation],
            "limitations":["Volatile candidate sentinel must never be retained."]});
        let mut third = good.clone();
        third["title"] = json!("Bounded evidence scope");
        let findings = if case.starts_with("fallback") {
            vec![good.clone(), good.clone(), third]
        } else if case == "all_withdrawn" {
            vec![bad.clone(), bad.clone(), bad.clone()]
        } else {
            vec![good.clone(), bad.clone(), third]
        };
        let mut first = json!({"schema_version":1,"outcome":"supported","action":{
            "type":"finish","findings":findings,"knowledge_completeness":"partial",
            "limitations":["Only selected material was considered."]}});
        if case.starts_with("fallback") {
            first["action"]["limitations"] = json!(["limitation:"]);
        }
        let first_finish: StagedFinish = serde_json::from_value(first.clone()).unwrap();
        let (current, prepared, invoked) =
            self.patch_invocation(provider, firewall, pages, evidence);
        let response = patch_response(&first, &invoked);
        let detail = self
            .inner
            .store
            .admit_next_prompt_finish(
                &self.inner._execution_guard,
                invoked.summary.revision,
                &NextPromptFinish {
                    request: &self.inner.request,
                    response: &response,
                    original_action: &first,
                    current: &current,
                    current_prepared: &prepared,
                },
            )
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Running);
        if case.starts_with("fallback") {
            use super::super::volatile_patch::CorrectionMode;
            assert!(
                self.inner
                    .store
                    .investigation_pending_patch(&self.inner._execution_guard, self.id())
                    .unwrap()
                    .is_none()
            );
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert_eq!(reopened.investigation(self.id()).unwrap(), detail);
            let snapshot = reopened
                .investigation_next_prompt_snapshot(self.id(), &self.inner.request)
                .unwrap();
            assert_eq!(
                snapshot.detail().usage.model_invocations,
                detail.usage.model_invocations
            );
            drop(snapshot);
            drop(reopened);
            if case != "fallback" {
                let tx = self.inner.store.conn.unchecked_transaction().unwrap();
                let mut task = load_next_prompt_task(&tx, self.id()).unwrap();
                let Some(CorrectionMode::WholeAnswer {
                    finish_hash,
                    refusal_hash,
                    finding_count,
                }) = task.pending_correction.clone()
                else {
                    panic!("expected whole-answer correction mode");
                };
                task.pending_correction = match case {
                    "fallback_missing" => None,
                    "fallback_hash" => Some(CorrectionMode::WholeAnswer {
                        finish_hash: core_prov::content_hash(b"forged finish"),
                        refusal_hash,
                        finding_count,
                    }),
                    "fallback_swapped" => Some(CorrectionMode::Patch {
                        basis: agents::investigation::finish_patch::PatchBasis {
                            finish_hash,
                            refusal_hash,
                            finding_count,
                            flagged_indices: vec![1],
                            flagged_limitations: vec![],
                            capacity_gaps: vec![],
                        },
                    }),
                    _ => unreachable!(),
                };
                let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
                tx.execute(
                    "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                    params![self.id(), bytes, core_prov::content_hash(&bytes)],
                )
                .unwrap();
                tx.commit().unwrap();
                let reopened =
                    JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
                assert!(reopened.investigation(self.id()).is_err());
                scan_candidate_absence(self.inner.directory.path());
                return;
            }
            // A full ordinary finish, with no held candidate or patch schema.
            let mut corrected = first.clone();
            corrected["action"]["limitations"] = json!(["Only selected material was considered."]);
            let (current, prepared, invoked) =
                self.patch_invocation(provider, firewall, pages, evidence);
            let response = patch_response(&corrected, &invoked);
            let completed = self
                .inner
                .store
                .admit_next_prompt_finish(
                    &self.inner._execution_guard,
                    invoked.summary.revision,
                    &NextPromptFinish {
                        request: &self.inner.request,
                        response: &response,
                        original_action: &corrected,
                        current: &current,
                        current_prepared: &prepared,
                    },
                )
                .unwrap();
            assert_eq!(completed.summary.status, InvestigationStatus::Completed);
            assert_eq!(
                completed.usage.model_invocations,
                detail.usage.model_invocations + 1
            );
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert_eq!(reopened.investigation(self.id()).unwrap(), completed);
            assert!(reopened.investigation_result(self.id()).unwrap().is_some());
            scan_candidate_absence(self.inner.directory.path());
            return;
        }
        let marker = self
            .inner
            .store
            .investigation_pending_patch(&self.inner._execution_guard, self.id())
            .unwrap()
            .unwrap();
        assert_eq!(marker.finding_count, 3);
        assert_eq!(
            marker.flagged_indices,
            if case == "all_withdrawn" {
                vec![0, 1, 2]
            } else {
                vec![1]
            }
        );
        scan_candidate_absence(self.inner.directory.path());
        if case == "cancel" {
            let held = VolatileFinish::capture(first_finish, marker).unwrap();
            let id = self.id().to_owned();
            let cancelled = self
                .inner
                .store
                .cancel_next_prompt_investigation(&id)
                .unwrap();
            drop(held); // released, not securely erased
            let stopped = self
                .inner
                .store
                .stop_next_prompt_investigation(
                    &self.inner._execution_guard,
                    cancelled.summary.revision,
                    InvestigationStopReason::Cancelled,
                    cancelled.usage.active_milliseconds,
                )
                .unwrap();
            assert_eq!(stopped.summary.status, InvestigationStatus::Cancelled);
            assert_eq!(
                stopped.usage.model_invocations,
                detail.usage.model_invocations
            );
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert_eq!(reopened.investigation(&id).unwrap(), stopped);
            assert!(reopened.investigation_result(&id).unwrap().is_none());
            scan_candidate_absence(self.inner.directory.path());
            return;
        }
        if case == "missing_marker" {
            let tx = self.inner.store.conn.unchecked_transaction().unwrap();
            let mut task = load_next_prompt_task(&tx, self.id()).unwrap();
            task.pending_correction = None;
            let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
            tx.execute(
                "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                params![self.id(), bytes, core_prov::content_hash(&bytes)],
            )
            .unwrap();
            tx.commit().unwrap();
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert!(reopened.investigation(self.id()).is_err());
            scan_candidate_absence(self.inner.directory.path());
            return;
        }
        if case == "restart" {
            let id = self.id().to_owned();
            let candidates = self
                .inner
                .store
                .investigation_recovery_candidates()
                .unwrap();
            let candidate = candidates.into_iter().find(|c| c.id == id).unwrap();
            let calls = detail.usage.model_invocations;
            drop(self.inner._execution_guard);
            let locks = crate::job_execution::JobExecutionLocks::open(
                self.inner.directory.path(),
                self.inner.store.execution_namespace(),
            )
            .unwrap();
            let owner = locks.try_reserve(candidate.lock_target()).unwrap();
            let recovered = self
                .inner
                .store
                .recover_investigation(&candidate, owner)
                .unwrap()
                .unwrap();
            assert_eq!(recovered.summary.status, InvestigationStatus::Failed);
            assert_eq!(recovered.usage.model_invocations, calls);
            assert_eq!(
                recovered.error.as_deref(),
                Some(super::super::volatile_patch::LOST_CANDIDATE)
            );
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert_eq!(reopened.investigation(&id).unwrap(), recovered);
            scan_candidate_absence(self.inner.directory.path());
            return;
        }
        if case == "retry_refused" {
            // Earlier roles keep the terminal patch rejection.
            let bad =
                json!({"type":"finish_patch","patches":[{"finding_index":0,"withdraw":true}]});
            let (_, _, invoked) = self.patch_invocation(provider, firewall, pages, evidence);
            let response = patch_response(&bad, &invoked);
            assert!(
                self.inner
                    .store
                    .refuse_next_prompt_patch(
                        &self.inner._execution_guard,
                        invoked.summary.revision,
                        &response,
                        &bad,
                        agents::investigation::finish_patch::PatchRejection::InvalidIndex,
                    )
                    .is_err()
            );
            return;
        }
        if case.starts_with("retry") {
            self.assert_patch_retry(
                provider,
                firewall,
                pages,
                evidence,
                case,
                &first_finish,
                &marker,
                &good,
            );
            return;
        }
        let mut held = VolatileFinish::capture(first_finish.clone(), marker.clone()).unwrap();
        if case == "binding_mismatch" {
            let mut wrong = marker.clone();
            wrong.refusal_hash = core_prov::content_hash(b"wrong refusal");
            held = VolatileFinish::capture(first_finish, wrong).unwrap();
        }
        let mut replacement = good.clone();
        replacement["title"] = json!("Corrected selected evidence");
        let patches = match case {
            "replace" | "audit_replace" => json!([{"finding_index":1,"replace":replacement}]),
            "withdraw" => json!([{"finding_index":1,"withdraw":true}]),
            "duplicate" => {
                json!([{"finding_index":1,"withdraw":true},{"finding_index":1,"withdraw":true}])
            }
            "unflagged" => json!([{"finding_index":0,"withdraw":true}]),
            "all_withdrawn" => {
                json!([{"finding_index":0,"withdraw":true},{"finding_index":1,"withdraw":true},{"finding_index":2,"withdraw":true}])
            }
            _ => json!([]),
        };
        let patch = if case == "tool" {
            json!({"type":"query_context"})
        } else {
            json!({"type":"finish_patch","patches":patches})
        };
        let raw = patch.to_string();
        let (current, prepared, invoked) =
            self.patch_invocation(provider, firewall, pages, evidence);
        let response = patch_response(&patch, &invoked);
        let detail = self
            .inner
            .store
            .admit_next_prompt_finish_with_patch(
                &self.inner._execution_guard,
                invoked.summary.revision,
                &NextPromptFinish {
                    request: &self.inner.request,
                    response: &response,
                    original_action: &patch,
                    current: &current,
                    current_prepared: &prepared,
                },
                Some(held),
                Some(&raw),
                None,
            )
            .unwrap();
        let success = matches!(case, "replace" | "withdraw" | "withhold" | "audit_replace");
        assert_eq!(
            detail.summary.status,
            if case == "audit_replace" {
                InvestigationStatus::Running
            } else if success {
                InvestigationStatus::Completed
            } else {
                InvestigationStatus::Failed
            }
        );
        let tx = self.inner.store.conn.unchecked_transaction().unwrap();
        let stored: InvestigationResponseMeta = read_body(
            &tx,
            "investigation_responses",
            "step_id",
            self.id(),
            &response.step_id,
        )
        .unwrap()
        .unwrap();
        let telemetry = stored.finish_telemetry.as_ref().unwrap();
        if success {
            assert_eq!(
                telemetry.finding_count,
                Some(if case == "withdraw" { 2 } else { 3 })
            );
            assert_eq!(
                telemetry.patched_count,
                Some(usize::from(matches!(case, "replace" | "audit_replace")))
            );
            assert_eq!(
                telemetry.withdrawn_count,
                Some(usize::from(case == "withdraw"))
            );
            assert_eq!(
                telemetry.flagged_count,
                Some(usize::from(case == "withhold"))
            );
            assert!(telemetry.projection_rejection.is_none());
        } else {
            assert!(telemetry.projection_rejection.is_some());
            assert!(telemetry.finding_count.is_none());
        }
        tx.commit().unwrap();
        if case == "audit_replace" {
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert_eq!(reopened.investigation(self.id()).unwrap(), detail);
            assert!(reopened.investigation_result(self.id()).unwrap().is_none());
            for mutation in [None, Some("missing_witness"), Some("changed_action")] {
                let tx = reopened.conn.unchecked_transaction().unwrap();
                let task = load_next_prompt_task(&tx, self.id()).unwrap();
                assert_eq!(task.phase, Phase::AuditPending);
                assert!(task.pending_correction.is_none());
                assert_eq!(
                    task.semantic_audit
                        .as_ref()
                        .unwrap()
                        .candidate
                        .as_ref()
                        .unwrap()
                        .findings
                        .len(),
                    3
                );
                if let Some(mutation) = mutation {
                    let mut changed = stored.clone();
                    if mutation == "missing_witness" {
                        changed.finish_feedback_witness = None;
                    } else {
                        changed.action_hash = "f".repeat(64);
                    }
                    let trigger: String = tx.query_row("SELECT sql FROM sqlite_schema WHERE name='investigation_responses_no_update'", [], |row| row.get(0)).unwrap();
                    tx.execute_batch("DROP TRIGGER investigation_responses_no_update")
                        .unwrap();
                    let body = encode(&changed, MAX_RECORD_BYTES).unwrap();
                    tx.execute("UPDATE investigation_responses SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2", params![self.id(), changed.step_id, body, core_prov::content_hash(&body)]).unwrap();
                    tx.execute_batch(&trigger).unwrap();
                    assert!(load_next_prompt_task(&tx, self.id()).is_err(), "{mutation}");
                }
            }
            scan_candidate_absence(self.inner.directory.path());
            return;
        }
        let result = self.inner.store.investigation_result(self.id()).unwrap();
        if case == "withhold" {
            let result = result.as_ref().unwrap();
            let witness = result
                .completion
                .as_ref()
                .unwrap()
                .withholding
                .as_ref()
                .unwrap();
            assert_eq!(witness.withheld_indexes, vec![1]);
            assert_eq!(witness.survivor_indexes, vec![0, 2]);
        }
        if success {
            assert_eq!(
                result.as_ref().unwrap().findings.len(),
                if case == "replace" { 3 } else { 2 }
            );
        } else {
            assert!(result.is_none());
        }
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(self.id()).unwrap(), detail);
        assert_eq!(reopened.investigation_result(self.id()).unwrap(), result);
        if matches!(case, "replace" | "withdraw") {
            for mutation in [
                "missing_witness",
                "changed_witness",
                "changed_response",
                "changed_action",
            ] {
                let tx = reopened.conn.unchecked_transaction().unwrap();
                let task = load_next_prompt_task(&tx, self.id()).unwrap();
                assert!(load_result(&tx, &task).unwrap().is_some());
                let mut changed = stored.clone();
                match mutation {
                    "missing_witness" => changed.finish_feedback_witness = None,
                    "changed_witness" => changed.finish_feedback_witness = Some("f".repeat(64)),
                    "changed_response" => changed.response_hash = "f".repeat(64),
                    _ => changed.action_hash = "f".repeat(64),
                }
                let trigger: String = tx.query_row("SELECT sql FROM sqlite_schema WHERE name='investigation_responses_no_update'", [], |row| row.get(0)).unwrap();
                tx.execute_batch("DROP TRIGGER investigation_responses_no_update")
                    .unwrap();
                let body = encode(&changed, MAX_RECORD_BYTES).unwrap();
                tx.execute("UPDATE investigation_responses SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2", params![self.id(), changed.step_id, body, core_prov::content_hash(&body)]).unwrap();
                tx.execute_batch(&trigger).unwrap();
                assert!(load_result(&tx, &task).is_err(), "{mutation}");
                // Each transaction rolls back, including independent response bytes.
            }
        }
        scan_candidate_absence(self.inner.directory.path());
    }
}

impl CapturedSourceInvocationFixture {
    /// @29 (#2431): a rejected patch is journaled as invalid_action and the
    /// patch step is offered again with the rejection as feedback, at most
    /// MAX_INVALID_ACTION_REFUSALS times a run. `retry` then recovers with a
    /// valid patch; `retry_cap` sends one more, which settles as terminal.
    #[allow(clippy::too_many_arguments)]
    fn assert_patch_retry(
        mut self,
        provider: &dyn llm::LlmProvider,
        firewall: &llm::EgressFirewall,
        pages: &[String],
        evidence: &[agents::AgentEvidence],
        case: &str,
        first_finish: &StagedFinish,
        marker: &agents::investigation::finish_patch::PatchBasis,
        good: &Value,
    ) {
        use agents::investigation::MAX_INVALID_ACTION_REFUSALS;
        use agents::investigation::finish_patch::PatchRejection;
        let role = self.inner.task.detail.summary.specialist_id;
        let whole = role.patches_whole_finish();
        // Withdrawing the unflagged finding 0 is outside the patch's rows.
        let bad = json!({"type":"finish_patch","patches":[{"finding_index":0,"withdraw":true}]});
        let refusals = if case == "retry" {
            1
        } else {
            MAX_INVALID_ACTION_REFUSALS
        };
        for attempt in 0..refusals {
            let held = VolatileFinish::capture(first_finish.clone(), marker.clone()).unwrap();
            let raw = bad.to_string();
            assert_eq!(
                held.rejection(&raw, marker, whole),
                Some(PatchRejection::InvalidIndex)
            );
            let (current, _, invoked) = self.patch_invocation(provider, firewall, pages, evidence);
            let prompt: Value =
                serde_json::from_str(&current.action("probe".into()).unwrap().payload.prompt)
                    .unwrap();
            let feedback = &prompt["turn_progress"]["action_feedback"];
            if attempt == 0 {
                assert!(feedback.is_null(), "{feedback}");
            } else {
                assert_eq!(feedback["reason"], "invalid_action");
                assert_eq!(feedback["patch_rejection"], "invalid_index");
            }
            assert!(prompt["turn_progress"]["finish_patch"].is_object());
            let response = patch_response(&bad, &invoked);
            let detail = self
                .inner
                .store
                .refuse_next_prompt_patch(
                    &self.inner._execution_guard,
                    invoked.summary.revision,
                    &response,
                    &bad,
                    PatchRejection::InvalidIndex,
                )
                .unwrap();
            assert_eq!(detail.summary.status, InvestigationStatus::Running);
            assert_eq!(
                self.inner
                    .store
                    .investigation_pending_patch(&self.inner._execution_guard, self.id())
                    .unwrap()
                    .as_ref(),
                Some(marker)
            );
            // Reopen replays the journal, including the patch-mode entry.
            let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
            assert_eq!(reopened.investigation(self.id()).unwrap(), detail);
            drop(reopened);
            scan_candidate_absence(self.inner.directory.path());
        }
        let held = VolatileFinish::capture(first_finish.clone(), marker.clone()).unwrap();
        let (current, prepared, invoked) =
            self.patch_invocation(provider, firewall, pages, evidence);
        let prompt: Value =
            serde_json::from_str(&current.action("probe".into()).unwrap().payload.prompt).unwrap();
        assert_eq!(
            prompt["turn_progress"]["action_feedback"]["patch_rejection"],
            "invalid_index"
        );
        let mut replacement = good.clone();
        replacement["title"] = json!("Corrected selected evidence");
        let patch = if case == "retry" {
            json!({"type":"finish_patch","patches":[{"finding_index":1,"replace":replacement}]})
        } else {
            bad.clone()
        };
        let response = patch_response(&patch, &invoked);
        if case != "retry" {
            // The allowance is spent: the journal refuses a third.
            assert_eq!(
                self.inner
                    .store
                    .refuse_next_prompt_patch(
                        &self.inner._execution_guard,
                        invoked.summary.revision,
                        &response,
                        &patch,
                        PatchRejection::InvalidIndex,
                    )
                    .unwrap_err(),
                InvestigationStoreError::Capacity
            );
        }
        let raw = patch.to_string();
        let settled = self
            .inner
            .store
            .admit_next_prompt_finish_with_patch(
                &self.inner._execution_guard,
                invoked.summary.revision,
                &NextPromptFinish {
                    request: &self.inner.request,
                    response: &response,
                    original_action: &patch,
                    current: &current,
                    current_prepared: &prepared,
                },
                Some(held),
                Some(&raw),
                None,
            )
            .unwrap();
        assert_eq!(
            settled.summary.status,
            if case == "retry" {
                InvestigationStatus::Completed
            } else {
                InvestigationStatus::Failed
            },
            "{:?}",
            settled.error
        );
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(self.id()).unwrap(), settled);
        assert_eq!(
            reopened.investigation_result(self.id()).unwrap().is_some(),
            case == "retry"
        );
        scan_candidate_absence(self.inner.directory.path());
    }
}

fn patch_response(value: &Value, detail: &InvestigationDetail) -> InvestigationResponseMeta {
    InvestigationResponseMeta {
        finish_feedback_witness: None,
        finish_telemetry: None,
        step_id: format!("step-{}", detail.usage.model_invocations),
        response_hash: core_prov::content_hash(value.to_string().as_bytes()),
        action_hash: core_prov::content_hash(&encode(value, MAX_ACTION_BYTES).unwrap()),
        observed_model: None,
        reported_input_tokens: Some(17),
        reported_output_tokens: Some(9),
        reported_cache_read_tokens: None,
        active_milliseconds: detail.usage.active_milliseconds + 1,
    }
}
fn scan_candidate_absence(root: &std::path::Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            scan_candidate_absence(&path);
        } else {
            let bytes = std::fs::read(path).unwrap();
            for needle in [
                b"Volatile candidate sentinel must never be retained.".as_slice(),
                b"MissingStrategy controls this behavior.".as_slice(),
            ] {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "candidate prose reached durable files"
                );
            }
        }
    }
}
