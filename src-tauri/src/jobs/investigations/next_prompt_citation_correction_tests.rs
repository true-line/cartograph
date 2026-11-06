//! AC-0565: synthetic source target in a genuine @19 start, then host invocation/refusal
//! transactions. No provider dispatch or source read is performed by this fixture.
use super::*;

fn correction_fixture_role(role: &str) -> Fixture {
    correction_fixture_configured(role, None)
}

fn correction_fixture_configured(role: &str, active_seconds: Option<u64>) -> Fixture {
    correction_fixture_sources(role, active_seconds, false)
}

fn correction_fixture_sources(
    role: &str,
    active_seconds: Option<u64>,
    two_sources: bool,
) -> Fixture {
    use completion::next_prompt_snapshot::tests::fixture::Fixture as Acquisition;
    let mut target_id = String::new();
    let base = Acquisition::configured_request(
        false,
        |request, archive| {
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
            if two_sources {
                let mut second = source.clone();
                second.index = 1;
                second.source.byte_end = 6;
                second.captured.byte_end = 6;
                second.target_id = second.content_id().unwrap();
                second.validate().unwrap();
                let mut extra = Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(second),
                };
                let id = extra.content_id().unwrap();
                if let Obligation::Source { obligation_id, .. } = &mut extra {
                    *obligation_id = id;
                }
                request.obligations.obligations.push(extra);
            }
            let mut obligation = Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(source),
            };
            let id = obligation.content_id().unwrap();
            if let Obligation::Source { obligation_id, .. } = &mut obligation {
                *obligation_id = id;
            }
            request.specialist_id = role.into();
            request.expected_graph_revision = Some(archive.proof().snapshot_id.clone());
            request.obligations.obligations.push(obligation);
            request.obligations = request
                .obligations
                .normalized_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap();
        },
        |_| {},
        active_seconds,
    );
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
    for role in ["domain-analyst@19", "domain-analyst@20"] {
        assert_citation_correction_is_once_only_after_reopen_and_duplicate_publication(role);
    }
}

fn assert_citation_correction_is_once_only_after_reopen_and_duplicate_publication(role: &str) {
    let mut f = correction_fixture_role(role);
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
    for role in ["domain-analyst@19", "domain-analyst@20"] {
        assert_citation_correction_late_cancel_never_offers_another_call(role);
    }
}

fn assert_citation_correction_late_cancel_never_offers_another_call(role: &str) {
    let mut f = correction_fixture_role(role);
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
    for role in ["domain-analyst@19", "domain-analyst@20"] {
        assert_citation_correction_mixed_foreign_id_is_terminal_without_feedback_allowance(role);
    }
}

fn assert_citation_correction_mixed_foreign_id_is_terminal_without_feedback_allowance(role: &str) {
    let mut f = correction_fixture_role(role);
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
    for role in ["domain-analyst@19", "domain-analyst@20"] {
        assert_citation_correction_stale_revision_cannot_consume_allowance(role);
    }
}

fn assert_citation_correction_stale_revision_cannot_consume_allowance(role: &str) {
    let mut f = correction_fixture_role(role);
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
    for role in ["domain-analyst@19", "domain-analyst@20"] {
        assert_citation_correction_shares_allowance_with_ordinary_finish_refusal(role);
    }
}

fn assert_citation_correction_shares_allowance_with_ordinary_finish_refusal(role: &str) {
    let mut f = correction_fixture_role(role);
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

// AC-0566: genuine @20 admission, durable refusal, store reopen and new
// firewall preparation. No real provider dispatch or acceptance claim.
#[test]
fn typed_citation_correction_survives_store_reopen_into_prepared_prompt() {
    for active_seconds in [None, Some(3_600)] {
        let mut f = correction_fixture_configured("domain-analyst@20", active_seconds);
        assert_eq!(
            f.base.f.task.detail.limits.active_seconds,
            active_seconds.unwrap_or(600)
        );
        f.begin();
        let first = f.publish().unwrap();
        assert_eq!(first.summary.status, InvestigationStatus::Running);
        assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
        let before = f.rows();
        assert!(
            before
                .iter()
                .all(|row| !String::from_utf8_lossy(row).contains("Withheld candidate"))
        );
        f.base.f.store = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
        f.next();
        let prompt: Value =
            serde_json::from_str(&f.prepared.preview().egress.payload.prompt).unwrap();
        assert_ollama_wire(&f.input, &f.prepared.preview().egress.payload.prompt);
        let correction = &prompt["turn_progress"]["citation_correction"];
        let entries = correction["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["target_id"],
            f.original["action"]["findings"][0]["citation_ids"][0]
        );
        assert_eq!(entries[0]["action"]["type"], "query_context");
        let choices = &prompt["turn_progress"]["acquisition_choices"];
        assert!(
            choices["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|offered| {
                    offered["obligation_id"] == entries[0]["obligation_id"]
                        && offered["action"] == entries[0]["action"]
                })
        );
        assert_eq!(
            prompt["completion_metadata"]["feedback"]["schema_version"],
            2
        );
        assert!(
            !f.prepared
                .preview()
                .egress
                .payload
                .prompt
                .contains("Withheld candidate")
        );
        assert_eq!(
            f.rows(),
            before,
            "reopening and preparation must not spend another call"
        );
        f.begin();
        let second = f.publish().unwrap();
        assert_eq!(second.summary.status, InvestigationStatus::Failed);
        assert_eq!(second.completion.as_ref().unwrap().refusal_count, 2);
        assert!(!second.summary.has_result);
    }
}

// Synthetic loopback only: exercise the production Ollama serializer using the
// reopened input. The fixture's offline provider is not dispatched, and this
// transport check publishes no invocation/result or source acquisition.
fn assert_ollama_wire(input: &InvestigationInput, expected_prompt: &str) {
    use llm::LlmProvider;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no synthetic request");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        };
        // Windows sockets inherit the listener's non-blocking mode; read the
        // request blocking (bounded by the timeout) on every platform.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut received = Vec::new();
        let (header_end, length) = loop {
            let mut chunk = [0; 4096];
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            received.extend_from_slice(&chunk[..n]);
            assert!(received.len() <= 1024 * 1024);
            if let Some(end) = received.windows(4).position(|s| s == b"\r\n\r\n") {
                let header = std::str::from_utf8(&received[..end]).unwrap();
                assert!(header.starts_with("POST /api/chat HTTP/1.1"));
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(length <= 1024 * 1024);
                break (end + 4, length);
            }
        };
        while received.len() < header_end + length {
            let mut chunk = [0; 4096];
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            received.extend_from_slice(&chunk[..n]);
        }
        let body: Value =
            serde_json::from_slice(&received[header_end..header_end + length]).unwrap();
        let response = serde_json::json!({"model":"test-model","message":{"role":"assistant","content":"{}"},"done":true,"done_reason":"stop"}).to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .unwrap();
        body
    });
    let provider =
        llm::OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let firewall = next_prompt_invocation::tests::firewall();
    let action = input.action("synthetic-reopened-wire".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &next_prompt_invocation::tests::limits())
        .unwrap();
    let authorized = firewall
        .authorize_bounded(&provider, &prepared, None)
        .unwrap();
    provider
        .complete_bounded(
            &authorized,
            &llm::bounded::CompletionControl {
                deadline: Instant::now() + Duration::from_secs(10),
                check: &|| llm::bounded::CallDirective::Continue,
            },
        )
        .unwrap();
    let wire = server.join().unwrap();
    assert_eq!(
        wire["messages"][1]["content"],
        format!(
            "{}\n\nEvidence spans (JSON):\n{}",
            expected_prompt,
            serde_json::to_string(&prepared.preview().egress.payload.spans).unwrap()
        )
    );
    assert_eq!(wire["stream"], false);
}

#[test]
fn analyst21_durable_capacity_refusals_exclude_two_distinct_reads() {
    let initial = correction_fixture_sources("domain-analyst@21", None, true);
    let mut query = metadata_admission::MetadataProposalFixture::from_fixture(
        initial,
        ContextPreviewKind::Query,
        false,
    );
    let snapshot = query.snapshot().unwrap();
    query.publish(&snapshot);
    query.f.base.f.ledger = query.ledger.clone();
    query.f.base.f.metadata = snapshot.metadata().clone();
    let page = query.raw;
    let mut f = query.f;
    f.next_with_pages(&[&page]);
    let mut refused = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let choices = {
            let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, f.base.id()).unwrap();
            completion::SavedMetadataHistory::load(&tx, &task)
                .unwrap()
                .current_choices(&f.base.f.ledger, &f.base.f.metadata)
                .unwrap()
                .unwrap()
        };
        let offered: Vec<Value> = choices
            .entries()
            .iter()
            .map(|entry| serde_json::to_value(&entry.action).unwrap())
            .collect();
        for action in &offered {
            let hash = core_prov::content_hash(&encode(action, MAX_ACTION_BYTES).unwrap());
            assert!(!refused.contains(&hash));
        }
        let action = offered
            .iter()
            .find(|action| action["type"] == "read_evidence")
            .unwrap();
        let hash = core_prov::content_hash(&encode(action, MAX_ACTION_BYTES).unwrap());
        assert!(refused.insert(hash.clone()));
        f.begin();
        let mut response = f.response.clone();
        response.action_hash = hash;
        f.base
            .f
            .store
            .refuse_next_prompt_capacity(
                &f.base.f.execution,
                f.base.f.task.detail.summary.revision,
                &response,
                action,
                CapacityRefusalReason::PromptCapacity,
            )
            .unwrap();
        f.next_with_pages(&[&page]);
    }
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    let snapshot = reopened
        .investigation_next_prompt_snapshot(f.base.id(), &f.base.f.request)
        .unwrap();
    assert_eq!(snapshot.capacity_refusal_count(), 2);
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task(&tx, f.base.id()).unwrap();
    let choices = completion::SavedMetadataHistory::load(&tx, &task)
        .unwrap()
        .current_choices(snapshot.ledger(), snapshot.metadata())
        .unwrap()
        .unwrap();
    for entry in choices.entries() {
        let action = serde_json::to_value(&entry.action).unwrap();
        assert!(!refused.contains(&core_prov::content_hash(
            &encode(&action, MAX_ACTION_BYTES).unwrap()
        )));
    }
    assert_eq!(refused.len(), 2);
    tx.commit().unwrap();
    drop(reopened);
    let prior_revision = snapshot.ledger().revision;
    let Fixture {
        mut base,
        input,
        prepared,
        ..
    } = f;
    base.input = input;
    base.prepared = prepared;
    base.acquire_inventory_with_pages(&[&page]);
    let reset = base.snapshot();
    assert_eq!(reset.ledger().revision, prior_revision + 1);
    assert!(reset.capacity_feedback().is_none());
    assert_eq!(reset.capacity_refusal_count(), 2); // Historical charges survive reset.
    let tx = base.f.store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task(&tx, base.id()).unwrap();
    let choices = completion::SavedMetadataHistory::load(&tx, &task)
        .unwrap()
        .current_choices(reset.ledger(), reset.metadata())
        .unwrap()
        .unwrap();
    let offered = choices
        .entries()
        .iter()
        .map(|entry| {
            let action = serde_json::to_value(&entry.action).unwrap();
            core_prov::content_hash(&encode(&action, MAX_ACTION_BYTES).unwrap())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(refused.is_subset(&offered));
}
