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
