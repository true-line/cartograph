//! Authentic per-read firewall refusals (#679 slice 2a, ADR-0083).
//!
//! A refusal is written only from firewall-built [`ReadDenialWitness`]es and
//! only against completed acquired reads at the latest step. Each refused
//! acquisition gets one immutable observation row and one `FirewallRefused`
//! attempt bound to it, in the caller's transaction. The acquisition and its
//! ledger revision stay as they were: a refusal withholds a representation,
//! it never revokes an acquisition.
//!
//! Readback never trusts the stored hash alone: it reloads the observation,
//! re-derives its hash, and checks it against the acquired attempt and the
//! saved ledger citation it names.
use super::*;
use llm::read_deny::{READ_DENY_RECIPE, ReadDenialWitness};
use rusqlite::{Transaction, TransactionBehavior};

pub(in crate::jobs::investigations) const TABLE: &str = "read_refusal_observations";
const MAX_OBSERVATION_BYTES: usize = 4096;
const OBSERVATION_DOMAIN: &[u8] = b"cartograph:read-refusal-observation:v1\0";
pub(super) const REFUSED_MESSAGE: &str =
    "The firewall withheld an acquired read from the cloud provider.";

pub(in crate::jobs::investigations) fn schema() -> Vec<String> {
    let mut statements = vec![format!(
        "CREATE TABLE {TABLE} (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL CHECK(length(attempt_id)=64), body BLOB NOT NULL CHECK(length(body)<={MAX_OBSERVATION_BYTES}), body_hash TEXT NOT NULL CHECK(length(body_hash)=64), PRIMARY KEY(task_id,attempt_id)) STRICT"
    )];
    for verb in ["UPDATE", "DELETE"] {
        statements.push(format!(
            "CREATE TRIGGER {TABLE}_no_{} BEFORE {verb} ON {TABLE} BEGIN SELECT RAISE(ABORT, 'immutable read refusal'); END",
            verb.to_lowercase()
        ));
    }
    statements
}

fn present(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    let mut expected = schema();
    let ceiling = expected.iter().map(String::len).max().unwrap_or(0);
    let pattern = format!("{TABLE}*");
    let mut actual: Vec<Option<String>> = connection.prepare(
        "SELECT CASE WHEN typeof(sql)='text' AND length(CAST(sql AS BLOB))<=?1 THEN sql END FROM sqlite_schema WHERE (name GLOB ?3 OR tbl_name GLOB ?3) AND name NOT GLOB 'sqlite_*' LIMIT ?2"
    )?.query_map(params![ceiling as i64, expected.len() as i64 + 1, pattern], |row| row.get(0))?.collect::<Result<_, _>>()?;
    if actual.is_empty() {
        return Ok(false);
    }
    actual.sort();
    expected.sort();
    if actual != expected.into_iter().map(Some).collect::<Vec<_>>() {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(true)
}

/// The persisted form of one witness. It holds identities and hashes only,
/// never the withheld text. Decoding it confers no authority: readback checks
/// it against the acquisition and citation it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadRefusalObservation {
    pub schema_version: u32,
    pub recipe: String,
    pub rule_id: String,
    pub read: core_prov::EvidenceRef,
    pub representation_hash: String,
    pub acquired_attempt_id: String,
}

impl ReadRefusalObservation {
    fn from_witness(witness: &ReadDenialWitness) -> Self {
        let read = witness.read();
        Self {
            schema_version: 1,
            recipe: witness.recipe().into(),
            rule_id: witness.rule_id().into(),
            read: core_prov::EvidenceRef {
                repo: read.repo.clone(),
                path: read.path.clone(),
                byte_start: read.byte_start,
                byte_end: read.byte_end,
                commit_sha: read.commit_sha.clone(),
            },
            representation_hash: witness.representation_hash().into(),
            acquired_attempt_id: witness.acquired_attempt_id().into(),
        }
    }

    pub(super) fn hash(&self) -> Result<String, InvestigationStoreError> {
        let body = encode(self, MAX_OBSERVATION_BYTES)?;
        let mut input = Vec::with_capacity(OBSERVATION_DOMAIN.len() + body.len());
        input.extend_from_slice(OBSERVATION_DOMAIN);
        input.extend_from_slice(&body);
        Ok(core_prov::content_hash(&input))
    }

    /// The observation names exactly this recipe and this saved citation.
    fn matches(&self, acquired_attempt_id: &str, citation: &InvestigationCitation) -> bool {
        self.schema_version == 1
            && self.recipe == READ_DENY_RECIPE
            && !self.rule_id.is_empty()
            && self.rule_id.len() <= 64
            && self.acquired_attempt_id == acquired_attempt_id
            && citation.source.as_ref() == Some(&self.read)
            && citation.text_hash.as_deref() == Some(self.representation_hash.as_str())
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Test-only: treat the Local settled-acquisition fixture as a cloud task,
    /// so the rest of the refusal contract is exercised before slice 2b adds
    /// the cloud worker path. The gate itself is tested with it off.
    pub(in crate::jobs::investigations) static FIXTURE_EGRESSES_TO_CLOUD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// The firewall refuses only toward a cloud provider, so a refusal on a task
/// that never egresses to one is not authentic.
fn egresses_to_cloud(task: &StoredTask) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if FIXTURE_EGRESSES_TO_CLOUD.with(std::cell::Cell::get) {
        return true;
    }
    task.detail.summary.provider_mode == InvestigationProviderMode::Cloud
        && task.detail.provider.mode == InvestigationProviderMode::Cloud
}

fn citation<'a>(
    ledger: &'a InvestigationInputLedger,
    citation_id: &str,
) -> Result<&'a InvestigationCitation, InvestigationStoreError> {
    ledger
        .citations
        .iter()
        .find(|citation| citation.citation_id == citation_id)
        .ok_or(InvestigationStoreError::Invalid)
}

/// Persist one refusal per acquired attempt at the latest step, each from its
/// own firewall witness. Every acquisition of that step that is not already
/// refused must be covered exactly once; the caller owns the transaction.
fn refuse(
    tx: &Transaction<'_>,
    task: &mut StoredTask,
    witnesses: &[ReadDenialWitness],
) -> Result<(), InvestigationStoreError> {
    if task.completion_intent.is_none()
        || task.phase != Phase::Ready
        || task.action_tool.is_some()
        || task.detail.summary.cancel_requested
        || witnesses.is_empty()
    {
        return Err(InvestigationStoreError::Stale);
    }
    if !egresses_to_cloud(task) {
        return Err(InvestigationStoreError::Invalid);
    }
    let policy = saved_policy(task)?;
    let id = task.detail.summary.investigation_id.clone();
    let step = latest_step(tx, &id)?;
    let entries = attempts(tx, task)?;
    let refused: Vec<&str> = entries
        .iter()
        .filter_map(|entry| match &entry.record.outcome {
            AttemptOutcome::FirewallRefused {
                acquired_attempt_id,
                ..
            } => Some(acquired_attempt_id.as_str()),
            _ => None,
        })
        .collect();
    let pending: Vec<&AttemptEntry> = entries
        .iter()
        .filter(|entry| {
            entry.record.step_id == step
                && matches!(entry.record.outcome, AttemptOutcome::SourceAcquired { .. })
                && !refused.contains(&entry.record.attempt_id.as_str())
        })
        .collect();
    if pending.len() != witnesses.len()
        || entries
            .len()
            .checked_add(witnesses.len())
            .is_none_or(|count| count > policy.max_completed_attempts())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut planned = Vec::with_capacity(witnesses.len());
    for acquired in &pending {
        let mut matching = witnesses
            .iter()
            .filter(|witness| witness.acquired_attempt_id() == acquired.record.attempt_id);
        let (Some(witness), None) = (matching.next(), matching.next()) else {
            return Err(InvestigationStoreError::Invalid);
        };
        let AttemptOutcome::SourceAcquired { citation_id } = &acquired.record.outcome else {
            return Err(InvestigationStoreError::Invalid);
        };
        let ledger = ledger_at(tx, &id, acquired.record.input_revision)?;
        let observation = ReadRefusalObservation::from_witness(witness);
        if !observation.matches(&acquired.record.attempt_id, citation(&ledger, citation_id)?) {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut record = acquired.record.clone();
        record.outcome = AttemptOutcome::FirewallRefused {
            acquired_attempt_id: acquired.record.attempt_id.clone(),
            observation_hash: observation.hash()?,
        };
        record.attempt_id = record.content_id()?;
        let intent = validated_intent(task)?;
        record.validate_binding_with_policy(
            &intent.request_hash,
            &intent.obligations,
            &id,
            &ledger,
            &policy,
        )?;
        planned.push((record, observation, (*acquired).clone()));
    }
    bump(tx, task)?;
    event(
        tx,
        task,
        InvestigationEventKind::ReadFirewallRefused,
        Some(&step),
        Some(InvestigationTool::ReadEvidence),
        REFUSED_MESSAGE,
    )?;
    if !present(tx)? {
        for statement in schema() {
            tx.execute_batch(&statement)?;
        }
    }
    for (offset, (record, observation, acquired)) in planned.into_iter().enumerate() {
        insert_body(
            tx,
            TABLE,
            &id,
            &record.attempt_id,
            &observation,
            MAX_OBSERVATION_BYTES,
        )?;
        let entry = AttemptEntry {
            record,
            response_hash: acquired.response_hash,
            action_hash: acquired.action_hash,
            event_sequence: task.detail.summary.last_event_sequence,
        };
        insert_body(
            tx,
            "completion_attempts",
            &id,
            &signed((entries.len() + offset + 1) as u64)?,
            &entry,
            MAX_RECORD_BYTES,
        )?;
    }
    refresh_observation(tx, task)
}

/// Readback of one stored `FirewallRefused` attempt against the earlier
/// entries, its own observation row, the saved citation and its event.
pub(super) fn verify(
    connection: &Connection,
    task: &StoredTask,
    entry: &AttemptEntry,
    earlier: &[AttemptEntry],
    event: &InvestigationEvent,
) -> Result<(), InvestigationStoreError> {
    let AttemptOutcome::FirewallRefused {
        acquired_attempt_id,
        observation_hash,
    } = &entry.record.outcome
    else {
        return Err(InvestigationStoreError::Invalid);
    };
    let id = &task.detail.summary.investigation_id;
    let acquired = earlier
        .iter()
        .find(|earlier| earlier.record.attempt_id == *acquired_attempt_id)
        .ok_or(InvestigationStoreError::Invalid)?;
    let AttemptOutcome::SourceAcquired { citation_id } = &acquired.record.outcome else {
        return Err(InvestigationStoreError::Invalid);
    };
    let mut expected = acquired.record.clone();
    expected.outcome = entry.record.outcome.clone();
    expected.attempt_id = entry.record.attempt_id.clone();
    if expected != entry.record
        || acquired.response_hash != entry.response_hash
        || acquired.action_hash != entry.action_hash
        || acquired.event_sequence >= entry.event_sequence
        || earlier.iter().any(|other| {
            matches!(
                &other.record.outcome,
                AttemptOutcome::FirewallRefused { acquired_attempt_id: other_id, .. }
                    if other_id == acquired_attempt_id
            )
        })
        || event.kind != InvestigationEventKind::ReadFirewallRefused
        || event.step_id.as_deref() != Some(entry.record.step_id.as_str())
        || event.tool != Some(InvestigationTool::ReadEvidence)
        || event.summary != REFUSED_MESSAGE
        || !egresses_to_cloud(task)
        || !present(connection)?
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let observation: ReadRefusalObservation = read_body(
        connection,
        TABLE,
        "attempt_id",
        id,
        &entry.record.attempt_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    let ledger = ledger_at(connection, id, acquired.record.input_revision)?;
    if observation.hash()? != *observation_hash
        || !observation.matches(acquired_attempt_id, citation(&ledger, citation_id)?)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

impl JobStore {
    /// Commit firewall refusals for the latest step's acquired reads of the
    /// running next-prompt task (#679). Every witness comes from
    /// [`llm::EgressFirewall::evaluate_read`]; nothing is written on any error.
    /// Observation rows stay outside occupancy, like the stop records: each is
    /// at most 4 KiB and there is at most one per completed attempt.
    #[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
    pub fn refuse_acquired_reads(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        witnesses: &[ReadDenialWitness],
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        if task.detail.summary.revision != expected_revision {
            return Err(InvestigationStoreError::Stale);
        }
        transitions::check_job(&tx, execution, false)?;
        transitions::available_time(&task)?;
        refuse(&tx, &mut task, witnesses)?;
        save_next_prompt_task(&tx, &task, expected_revision, false)?;
        if stored_bytes(&tx, Some(&id))? > TASK_CAPACITY - TERMINAL_RESERVE
            || stored_bytes(&tx, None)? > STORE_CAPACITY
        {
            return Err(InvestigationStoreError::Capacity);
        }
        attempts(&tx, &task)?;
        tx.commit()?;
        Ok(task.detail)
    }
}
