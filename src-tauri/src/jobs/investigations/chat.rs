//! Append-only dispatch chat identity and typed result references (AC-0375/0377).
use super::*;
use serde::{Deserialize, Serialize};

pub(super) const MAX_RESULT_REFERENCE_BYTES: usize = 1024;
// Slot covers generated message/chat/task IDs, kind, bounded body/hash and the
// existing per-row metadata charge used by storage accounting.
pub(super) const RESULT_REFERENCE_SLOT_BYTES: usize = 1536;
pub(super) const MAX_USER_MESSAGE_BYTES: usize = 16 * 1024;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_AFTER_RESULT_REFERENCE_INSERT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn fail_after_result_reference_insert_once() {
    FAIL_AFTER_RESULT_REFERENCE_INSERT.with(|flag| flag.set(true));
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoredChatMessage {
    UserQuestion {
        question: String,
    },
    ResultReference {
        task_id: String,
        result_id: String,
        result_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredChatTranscript {
    pub chat_id: String,
    pub task_id: String,
    pub producer: StoredChatProducer,
    pub messages: Vec<StoredChatEntry>,
}

/// Identity copied from the task at admission, never reconstructed from the
/// current specialist catalog or provider configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredChatProducer {
    pub specialist_id: SpecialistId,
    pub specialist_version: u32,
    pub prompt_fingerprint: String,
    pub tier: core_prov::Tier,
    pub confidence_tier: core_prov::ConfidenceTier,
    pub provider_mode: InvestigationProviderMode,
    pub provider_id: String,
    pub model: String,
    pub protocol_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredChatAuthor {
    User,
    Specialist,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredChatEntry {
    pub id: String,
    pub ordinal: u8,
    pub author: StoredChatAuthor,
    pub message: StoredChatMessage,
}

pub(super) fn schema() -> Vec<(String, String, String)> {
    let mut objects = vec![
        (
            "investigation_chats".into(),
            "table".into(),
            "CREATE TABLE investigation_chats (id TEXT PRIMARY KEY NOT NULL, task_id TEXT NOT NULL UNIQUE REFERENCES investigation_tasks(id), created_at TEXT NOT NULL) STRICT".into(),
        ),
        (
            "investigation_chat_messages".into(),
            "table".into(),
            "CREATE TABLE investigation_chat_messages (id TEXT PRIMARY KEY NOT NULL, chat_id TEXT NOT NULL REFERENCES investigation_chats(id), task_id TEXT NOT NULL REFERENCES investigation_tasks(id), ordinal INTEGER NOT NULL CHECK(ordinal IN (0,1)), kind TEXT NOT NULL CHECK(kind IN ('user_question','result_reference')), body BLOB NOT NULL, body_hash TEXT NOT NULL, UNIQUE(chat_id,ordinal), CHECK((ordinal=0 AND kind='user_question') OR (ordinal=1 AND kind='result_reference'))) STRICT".into(),
        ),
    ];
    for table in ["investigation_chats", "investigation_chat_messages"] {
        for verb in ["UPDATE", "DELETE"] {
            let name = format!("{table}_no_{}", verb.to_lowercase());
            objects.push((
                name.clone(),
                "trigger".into(),
                format!("CREATE TRIGGER {name} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable investigation chat record'); END"),
            ));
        }
    }
    objects
}

pub(super) fn insert_dispatch_chat(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let task_id = &task.detail.summary.investigation_id;
    let chat_id = storage::new_id(connection, "chat")?;
    let created_at = task.detail.summary.created_at.clone();
    super::one(connection.execute(
        "INSERT INTO investigation_chats(id,task_id,created_at) VALUES (?1,?2,?3)",
        params![chat_id, task_id, created_at],
    )?)?;
    insert_message(
        connection,
        &chat_id,
        task_id,
        0,
        "user_question",
        &StoredChatMessage::UserQuestion {
            question: task.detail.summary.question.clone(),
        },
        MAX_USER_MESSAGE_BYTES,
    )
}

pub(super) fn insert_result_reference(
    connection: &Connection,
    task_id: &str,
    result_id: &str,
    result_hash: &str,
) -> Result<(), InvestigationStoreError> {
    let chat_row: Option<Option<String>> = connection
        .query_row(
            "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB)) BETWEEN 1 AND 128 THEN id END FROM investigation_chats WHERE task_id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let chat_id = match chat_row {
        None => {
            // Tasks predating schema v4 have no chat. Their historical
            // completion remains valid; migration deliberately does not backfill.
            return Ok(());
        }
        Some(Some(id)) => id,
        Some(None) => return Err(InvestigationStoreError::Invalid),
    };
    insert_message(
        connection,
        &chat_id,
        task_id,
        1,
        "result_reference",
        &StoredChatMessage::ResultReference {
            task_id: task_id.to_owned(),
            result_id: result_id.to_owned(),
            result_hash: result_hash.to_owned(),
        },
        MAX_RESULT_REFERENCE_BYTES,
    )
}

fn insert_message(
    connection: &Connection,
    chat_id: &str,
    task_id: &str,
    ordinal: i64,
    kind: &str,
    message: &StoredChatMessage,
    maximum: usize,
) -> Result<(), InvestigationStoreError> {
    let body = storage::encode(message, maximum)?;
    let expected_kind = match message {
        StoredChatMessage::UserQuestion { .. } => "user_question",
        StoredChatMessage::ResultReference { .. } => "result_reference",
    };
    if kind != expected_kind {
        return Err(InvestigationStoreError::Invalid);
    }
    let id = storage::new_id(connection, "message")?;
    let row_charge = id
        .len()
        .checked_add(chat_id.len())
        .and_then(|size| size.checked_add(task_id.len()))
        .and_then(|size| size.checked_add(kind.len()))
        .and_then(|size| size.checked_add(body.len()))
        .and_then(|size| size.checked_add(64 + 256))
        .ok_or(InvestigationStoreError::Capacity)?;
    if ordinal == 1 && row_charge > RESULT_REFERENCE_SLOT_BYTES {
        return Err(InvestigationStoreError::Capacity);
    }
    super::one(connection.execute(
        "INSERT INTO investigation_chat_messages(id,chat_id,task_id,ordinal,kind,body,body_hash) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![id, chat_id, task_id, ordinal, kind, body, core_prov::content_hash(&body)],
    )?)?;
    #[cfg(any(test, feature = "test-support"))]
    if ordinal == 1 && FAIL_AFTER_RESULT_REFERENCE_INSERT.with(|flag| flag.replace(false)) {
        return Err(InvestigationStoreError::Storage);
    }
    Ok(())
}

/// Bounded test wrapper around the same snapshot validator used by the app read.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn read_for_task(
    connection: &Connection,
    task_id: &str,
) -> Result<StoredChatTranscript, InvestigationStoreError> {
    let tx = connection.unchecked_transaction()?;
    let transcript = read_from_snapshot(&tx, task_id)?.ok_or(InvestigationStoreError::Missing)?;
    tx.commit()?;
    Ok(transcript)
}

pub(super) fn read_from_snapshot(
    connection: &Connection,
    task_id: &str,
) -> Result<Option<StoredChatTranscript>, InvestigationStoreError> {
    let task = storage::load_execution_task(connection, task_id)?;
    let chat_row: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END, CASE WHEN typeof(task_id)='text' AND length(CAST(task_id AS BLOB))<=128 THEN task_id END FROM investigation_chats WHERE task_id=?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (chat_id, saved_task_id) = match chat_row {
        None => return Ok(None),
        Some((Some(chat_id), Some(saved_task_id))) => (chat_id, saved_task_id),
        Some(_) => return Err(InvestigationStoreError::Invalid),
    };
    if chat_id.is_empty() || saved_task_id != task_id {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut statement = connection.prepare(
        "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END, CASE WHEN typeof(task_id)='text' AND length(CAST(task_id AS BLOB))<=128 THEN task_id END, CASE WHEN typeof(ordinal)='integer' AND ordinal IN (0,1) THEN ordinal END, CASE WHEN typeof(kind)='text' AND length(CAST(kind AS BLOB))<=32 THEN kind END, CASE WHEN typeof(body)='blob' AND length(body)<=16384 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM investigation_chat_messages WHERE chat_id=?1 ORDER BY ordinal LIMIT 3",
    )?;
    let rows = statement
        .query_map([&chat_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<Vec<u8>>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if rows.is_empty() || rows.len() > 2 {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut messages = Vec::with_capacity(rows.len());
    for (expected_ordinal, row) in rows.into_iter().enumerate() {
        let (Some(id), Some(saved_id), Some(ordinal), Some(kind), Some(body), Some(hash)) = row
        else {
            return Err(InvestigationStoreError::Invalid);
        };
        if id.is_empty() || saved_id != task_id || ordinal != expected_ordinal as i64 {
            return Err(InvestigationStoreError::Invalid);
        }
        let maximum = if ordinal == 0 {
            MAX_USER_MESSAGE_BYTES
        } else {
            MAX_RESULT_REFERENCE_BYTES
        };
        let message: StoredChatMessage = storage::decode_bounded(&body, &hash, maximum)?;
        match (&message, kind.as_str(), ordinal) {
            (StoredChatMessage::UserQuestion { question }, "user_question", 0)
                if question == &task.detail.summary.question => {}
            (
                StoredChatMessage::ResultReference {
                    task_id: referenced,
                    result_id,
                    result_hash,
                },
                "result_reference",
                1,
            ) => {
                let result = storage::load_result(connection, &task)?
                    .ok_or(InvestigationStoreError::Invalid)?;
                let saved_hash: String = connection.query_row(
                    "SELECT CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM investigation_results WHERE task_id=?1",
                    [task_id],
                    |row| row.get(0),
                )?;
                if referenced != task_id
                    || result_id != &result.result_id
                    || result_hash != &saved_hash
                {
                    return Err(InvestigationStoreError::Invalid);
                }
            }
            _ => return Err(InvestigationStoreError::Invalid),
        }
        messages.push(StoredChatEntry {
            id,
            ordinal: ordinal as u8,
            author: if ordinal == 0 {
                StoredChatAuthor::User
            } else {
                StoredChatAuthor::Specialist
            },
            message,
        });
    }
    if task.detail.summary.has_result != (messages.len() == 2) {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(Some(StoredChatTranscript {
        chat_id,
        task_id: task_id.to_owned(),
        producer: StoredChatProducer {
            specialist_id: task.detail.specialist.id,
            specialist_version: task.detail.specialist.version,
            prompt_fingerprint: task.detail.specialist.prompt_fingerprint,
            tier: task.detail.specialist.tier,
            confidence_tier: task.detail.specialist.confidence_tier,
            provider_mode: task.detail.provider.mode,
            provider_id: task.detail.provider.provider_id,
            model: task.detail.provider.model,
            protocol_version: task.detail.provider.protocol_version,
        },
        messages,
    }))
}
