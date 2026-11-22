use super::super::chat::StoredChatAuthor;
use super::*;
use crate::jobs::investigations::chat::StoredChatMessage;

// AC-0375: a durable user message is created once after fresh admission and
// contains the already-redacted task question only.
#[test]
fn dispatch_chat_replay_is_unique_and_question_is_redacted_across_reopen() {
    let mut fixture = Fixture::new();
    let token = "ghp_abcdefgh1234_private_chat_canary";
    let mut request = request("chat-replay", InvestigationProviderMode::Local);
    request.question = format!("Review this design token {token}");
    let started = fixture
        .store
        .start_investigation(&request, Some(&provider(InvestigationProviderMode::Local)))
        .unwrap();
    let id = started.detail.summary.investigation_id.clone();
    assert!(started.created);
    assert!(
        !fixture
            .store
            .start_investigation(&request, None)
            .unwrap()
            .created
    );
    let mut conflicting = request.clone();
    conflicting.question.push_str(" changed");
    assert_eq!(
        fixture
            .store
            .start_investigation(&conflicting, None)
            .unwrap_err(),
        InvestigationStoreError::Conflict
    );
    let transcript = chat::read_for_task(&fixture.store.conn, &id).unwrap();
    assert_eq!(transcript.messages.len(), 1);
    assert_eq!(transcript.messages[0].ordinal, 0);
    assert!(!transcript.messages[0].id.is_empty());
    let StoredChatMessage::UserQuestion { question } = &transcript.messages[0].message else {
        panic!("first chat row must be the redacted user question");
    };
    assert_eq!(question, &started.detail.summary.question);
    assert!(!question.contains(token));
    assert_eq!(
        fixture
            .store
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_chat_messages WHERE task_id=?1",
                [&id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );

    fixture.store.cancel_investigation(&id).unwrap();
    fixture.store.clear_finished().unwrap();
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        chat::read_for_task(&fixture.store.conn, &id).unwrap(),
        transcript
    );
}

// AC-0377: the app-facing read returns only saved, validated attribution and
// identifies the pending question as user-authored.
#[test]
fn investigation_chat_read_returns_saved_pending_identity_and_legacy_absence() {
    let mut fixture = Fixture::new();
    let started = fixture
        .store
        .start_investigation(
            &request("chat-read-pending", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap();
    let id = started.detail.summary.investigation_id.clone();
    let transcript = fixture.store.investigation_chat(&id).unwrap().unwrap();
    assert_eq!(transcript.task_id, id);
    assert_eq!(transcript.messages.len(), 1);
    assert_eq!(transcript.messages[0].ordinal, 0);
    assert_eq!(transcript.messages[0].author, StoredChatAuthor::User);
    assert_eq!(
        transcript.producer.specialist_id,
        started.detail.specialist.id
    );
    assert_eq!(
        transcript.producer.specialist_version,
        started.detail.specialist.version
    );
    assert_eq!(
        transcript.producer.prompt_fingerprint,
        started.detail.specialist.prompt_fingerprint
    );
    assert_eq!(transcript.producer.tier, started.detail.specialist.tier);
    assert_eq!(
        transcript.producer.confidence_tier,
        started.detail.specialist.confidence_tier
    );
    assert_eq!(
        transcript.producer.provider_mode,
        started.detail.provider.mode
    );
    assert_eq!(
        transcript.producer.provider_id,
        started.detail.provider.provider_id
    );
    assert_eq!(transcript.producer.model, started.detail.provider.model);
    assert_eq!(
        transcript.producer.protocol_version,
        started.detail.provider.protocol_version
    );

    fixture.store.cancel_investigation(&id).unwrap();
    fixture.store.clear_finished().unwrap();
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        fixture.store.investigation_chat(&id).unwrap(),
        Some(transcript)
    );
    assert_eq!(
        fixture.store.investigation_chat("investigation:missing"),
        Err(InvestigationStoreError::Missing),
        "unknown task is an error, distinct from a legacy task without chat"
    );
}

// AC-0377: completed chat reads retain stable IDs and bind the specialist
// message to the exact result already admitted for this task.
#[test]
fn investigation_chat_read_reopens_completed_task_with_bound_result_reference() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("chat-read-completed", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    begin(&mut fixture, &id, &execution, step(&ledger, 1));
    let result = finish(&id, &ledger);
    let current = fixture.store.investigation(&id).unwrap();
    fixture
        .store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 100),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();

    let mut reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let transcript = reopened.investigation_chat(&id).unwrap().unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[0].author, StoredChatAuthor::User);
    assert_eq!(transcript.messages[1].author, StoredChatAuthor::Specialist);
    let saved_hash: String = reopened
        .conn
        .query_row(
            "SELECT body_hash FROM investigation_results WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(matches!(
        &transcript.messages[1].message,
        StoredChatMessage::ResultReference { task_id, result_id, result_hash }
            if task_id == &id
                && result_id == &result.result_id
                && result_hash == &saved_hash
    ));
    assert_eq!(transcript.messages[0].ordinal, 0);
    assert_eq!(transcript.messages[1].ordinal, 1);
    assert!(!transcript.messages[0].id.is_empty());
    assert!(!transcript.messages[1].id.is_empty());
    reopened.clear_finished().unwrap();
    assert_eq!(reopened.investigation_chat(&id).unwrap(), Some(transcript));
}

#[test]
fn investigation_chat_public_read_fails_closed_on_hash_and_size_corruption() {
    let mut fixture = Fixture::new();
    let started = fixture
        .store
        .start_investigation(
            &request("chat-read-corrupt", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap();
    let id = started.detail.summary.investigation_id;
    let message_id: String = fixture
        .store
        .conn
        .query_row(
            "SELECT id FROM investigation_chat_messages WHERE task_id=?1 AND ordinal=0",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    let trigger: String = fixture
        .store
        .conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_chat_messages_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    fixture
        .store
        .conn
        .execute_batch("DROP TRIGGER investigation_chat_messages_no_update")
        .unwrap();
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_chat_messages SET body_hash=?1 WHERE id=?2",
            params!["0".repeat(64), message_id],
        )
        .unwrap();
    fixture.store.conn.execute_batch(&trigger).unwrap();
    assert_eq!(
        fixture.store.investigation_chat(&id),
        Err(InvestigationStoreError::Invalid),
        "the app reader rejects a bounded body with a mismatched digest"
    );
    let oversized = vec![b'x'; chat::MAX_USER_MESSAGE_BYTES + 1];
    fixture
        .store
        .conn
        .execute_batch("DROP TRIGGER investigation_chat_messages_no_update")
        .unwrap();
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_chat_messages SET body=?1,body_hash=?2 WHERE id=?3",
            params![&oversized, core_prov::content_hash(&oversized), message_id],
        )
        .unwrap();
    fixture.store.conn.execute_batch(&trigger).unwrap();
    assert_eq!(
        fixture.store.investigation_chat(&id),
        Err(InvestigationStoreError::Invalid),
        "the app reader rejects a row beyond the message byte cap"
    );
}

// AC-0375: capacity accounting reserves the exact bounded result-reference
// row before provider work, then replaces the claim with actual stored bytes.
#[test]
fn dispatch_chat_result_reference_slot_is_reserved_and_released_on_failure() {
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("chat-slot", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let id = detail.summary.investigation_id;
    let occupied = storage::stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    let actual = storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    assert_eq!(occupied - actual, chat::RESULT_REFERENCE_SLOT_BYTES);
    fixture.store.cancel_investigation(&id).unwrap();
    let occupied = storage::stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    let actual = storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    assert_eq!(
        occupied, actual,
        "terminal failure releases unused future-card slot"
    );
    assert_eq!(
        chat::read_for_task(&fixture.store.conn, &id)
            .unwrap()
            .messages
            .len(),
        1
    );
}

#[test]
fn unspent_result_reference_reservation_counts_toward_nonterminal_limit() {
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("chat-near-capacity", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let id = detail.summary.investigation_id;
    let current = storage::stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    let nonterminal_limit = TASK_CAPACITY - TERMINAL_RESERVE;
    let padding = nonterminal_limit - current + 1 - (64 + 256);
    fixture
        .store
        .conn
        .execute(
            "INSERT INTO investigation_events VALUES (?1,999,zeroblob(?2),?3)",
            params![id, i64::try_from(padding).unwrap(), "0".repeat(64)],
        )
        .unwrap();
    assert_eq!(
        storage::stored_bytes(&fixture.store.conn, Some(&id)).unwrap(),
        nonterminal_limit + 1
    );
    assert!(
        storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap() < nonterminal_limit
    );
    assert_eq!(
        storage::check_task_capacity(&fixture.store.conn, &id, false),
        Err(InvestigationStoreError::Capacity),
        "capacity checks must include the future result reference before provider work"
    );
}

// AC-0404: a disposable TEMP trigger fills storage only inside result publication.
impl JobStore {
    pub fn fill_test_finish_capacity(&mut self, id: &str, headroom: usize) {
        let current = storage::stored_bytes(&self.conn, Some(id)).unwrap();
        let padding = TASK_CAPACITY
            .checked_sub(current + headroom + 64 + 256)
            .unwrap();
        let id = id.replace('\'', "''");
        self.conn.execute_batch(&format!("CREATE TEMP TRIGGER message_finish_capacity AFTER INSERT ON main.investigation_results WHEN NEW.task_id='{id}' BEGIN INSERT INTO investigation_responses VALUES ('{id}','message-capacity-padding',zeroblob({padding}),'{}'); END", "0".repeat(64))).unwrap();
    }
    pub fn assert_test_finish_padding_rolled_back(&self, id: &str) {
        let rows: i64 = self.conn.query_row("SELECT count(*) FROM investigation_responses WHERE task_id=?1 AND step_id='message-capacity-padding'", [id], |row| row.get(0)).unwrap();
        assert_eq!(rows, 0);
    }
}
