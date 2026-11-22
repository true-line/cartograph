//! AC-1410: authentic per-read firewall refusals persist atomically after a
//! real acquisition and reopen only when every bound identity still holds.
use super::*;
use completion::read_refusals::{ReadDecider, TABLE, schema};
use llm::read_deny::{ReadDecision, ReadDenialWitness, ReadDenyRules, ReadIdentity};
use llm::{EgressFirewall, EgressPolicy, LlmProvider, Locality, ProviderCaps};

struct Cloud;
impl LlmProvider for Cloud {
    fn id(&self) -> &str {
        "cloud:test"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        panic!("no provider dispatch")
    }
}

/// Settle one real acquisition and commit it; return the fixture and the
/// acquired attempt id.
fn acquired() -> (Fixture, String) {
    let mut fixture = Fixture::new();
    fixture.charge();
    let (candidate, prepared) = fixture.candidate(None);
    let continuation = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
        refusal: None,
    };
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let active = fixture.f.task.detail.usage.active_milliseconds + 1;
    let plan = project(
        &tx,
        &fixture.f.task,
        SourceSettlement::Acquired(continuation),
        active,
    )
    .unwrap();
    let next = persist(&tx, &fixture.f.task, plan).unwrap();
    let attempt = completion::attempts(&tx, &next).unwrap()[0]
        .record
        .attempt_id
        .clone();
    tx.commit().unwrap();
    fixture.refresh();
    (fixture, attempt)
}

/// A settled acquisition on a cloud task.
fn acquired_on_cloud() -> (Fixture, String) {
    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(true));
    acquired()
}

fn witness(fixture: &Fixture, attempt: &str, bytes: &[u8]) -> ReadDenialWitness {
    let source = &fixture.r.source.source;
    let rules = ReadDenyRules::with_project_rules([source.path.clone()]).unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default()).with_read_deny_rules(rules);
    let read = ReadIdentity {
        repo: source.repo.clone(),
        path: source.path.clone(),
        commit_sha: source.commit_sha.clone(),
        byte_start: source.byte_start,
        byte_end: source.byte_end,
    };
    match firewall.evaluate_read(&Cloud, attempt, &read, bytes) {
        ReadDecision::Refused(witness) => witness,
        ReadDecision::Allowed => panic!("the project rule must refuse the fixture path"),
    }
}

fn commit(
    fixture: &mut Fixture,
    witnesses: &[ReadDenialWitness],
) -> Result<InvestigationDetail, InvestigationStoreError> {
    let revision = fixture.f.task.detail.summary.revision;
    let result =
        fixture
            .f
            .store
            .refuse_acquired_reads(&fixture.f._execution_guard, revision, witnesses);
    fixture.refresh();
    result
}

fn reload(fixture: &Fixture) -> Result<Vec<completion::AttemptEntry>, InvestigationStoreError> {
    let store = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id)?;
    completion::attempts(&tx, &task)
}

// AC-1410: the refusal follows the real acquisition, keeps its revision and
// citation, and reopens with its observation and event; no refused text is
// stored.
#[test]
fn firewall_refusal_persists_after_acquisition_and_reopens() {
    let (mut fixture, attempt) = acquired_on_cloud();
    let revision = fixture.f.task.ledger_revision;
    let witness = witness(&fixture, &attempt, b"private text");
    let expected = fixture.f.task.detail.summary.revision;
    fixture
        .f
        .store
        .refuse_acquired_reads(&fixture.f._execution_guard, expected, &[witness])
        .unwrap();

    let entries = reload(&fixture).unwrap();
    assert_eq!(entries.len(), 2);
    let AttemptOutcome::FirewallRefused {
        acquired_attempt_id,
        ..
    } = &entries[1].record.outcome
    else {
        panic!("second attempt must be the refusal");
    };
    assert_eq!(acquired_attempt_id, &attempt);
    assert!(matches!(
        entries[0].record.outcome,
        AttemptOutcome::SourceAcquired { .. }
    ));
    assert_eq!(
        entries[1].record.input_revision,
        entries[0].record.input_revision
    );
    fixture.refresh();
    assert_eq!(fixture.f.task.ledger_revision, revision);
    let events = fixture
        .f
        .store
        .investigation_events(&fixture.f.basis.task_id, 0)
        .unwrap();
    assert_eq!(
        events.items.last().unwrap().kind,
        InvestigationEventKind::ReadFirewallRefused
    );
    let body: Vec<u8> = fixture
        .f
        .store
        .conn
        .query_row(&format!("SELECT body FROM {TABLE}"), [], |r| r.get(0))
        .unwrap();
    assert!(!body.windows(12).any(|w| w == b"private text"));
}

// AC-1410: a refused commit (here a stale revision) rolls back whole: no
// attempt, event or observation table survives.
#[test]
fn failed_refusal_leaves_nothing() {
    let (mut fixture, attempt) = acquired_on_cloud();
    let sequence = fixture.f.task.detail.summary.last_event_sequence;
    let good = witness(&fixture, &attempt, b"private text");
    let stale = fixture.f.task.detail.summary.revision + 1;
    assert_eq!(
        fixture
            .f
            .store
            .refuse_acquired_reads(&fixture.f._execution_guard, stale, &[good]),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(reload(&fixture).unwrap().len(), 1);
    fixture.refresh();
    assert_eq!(fixture.f.task.detail.summary.last_event_sequence, sequence);
    let present: bool = fixture
        .f
        .store
        .conn
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='{TABLE}')"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!present);
}

// AC-1410: a witness for other bytes, another attempt, a duplicate, an empty
// set or a second refusal of the same acquisition is refused unwritten.
#[test]
fn mismatched_or_repeated_witnesses_are_refused() {
    let (mut fixture, attempt) = acquired_on_cloud();
    let other_bytes = witness(&fixture, &attempt, b"other text");
    let other_attempt = witness(&fixture, &"0".repeat(64), b"private text");
    let good = witness(&fixture, &attempt, b"private text");
    for witnesses in [
        vec![other_bytes],
        vec![other_attempt],
        vec![good.clone(), good.clone()],
    ] {
        assert_eq!(
            commit(&mut fixture, &witnesses),
            Err(InvestigationStoreError::Invalid)
        );
    }
    assert_eq!(
        commit(&mut fixture, &[]),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(reload(&fixture).unwrap().len(), 1);
    commit(&mut fixture, std::slice::from_ref(&good)).unwrap();
    assert_eq!(
        commit(&mut fixture, &[good]),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(reload(&fixture).unwrap().len(), 2);
}

// AC-1410: readback rejects an altered or missing observation, even when the
// row's own body hash is recomputed.
#[test]
fn altered_or_missing_observation_fails_fresh_readback() {
    let (mut fixture, attempt) = acquired_on_cloud();
    let witness = witness(&fixture, &attempt, b"private text");
    commit(&mut fixture, &[witness]).unwrap();
    assert!(reload(&fixture).is_ok());
    let conn = &fixture.f.store.conn;
    let body: Vec<u8> = conn
        .query_row(&format!("SELECT body FROM {TABLE}"), [], |r| r.get(0))
        .unwrap();
    let altered = String::from_utf8(body)
        .unwrap()
        .replace("\"rule_id\":\"project:0\"", "\"rule_id\":\"env-file\"")
        .into_bytes();
    conn.execute_batch(&format!(
        "DROP TRIGGER {TABLE}_no_update; DROP TRIGGER {TABLE}_no_delete"
    ))
    .unwrap();
    conn.execute(
        &format!("UPDATE {TABLE} SET body=?1, body_hash=?2"),
        rusqlite::params![altered, core_prov::content_hash(&altered)],
    )
    .unwrap();
    for statement in &schema()[1..] {
        conn.execute_batch(statement).unwrap();
    }
    assert_eq!(
        reload(&fixture).err(),
        Some(InvestigationStoreError::Invalid)
    );

    conn.execute_batch(&format!(
        "DROP TRIGGER {TABLE}_no_update; DROP TRIGGER {TABLE}_no_delete; DELETE FROM {TABLE}"
    ))
    .unwrap();
    for statement in &schema()[1..] {
        conn.execute_batch(statement).unwrap();
    }
    assert_eq!(
        reload(&fixture).err(),
        Some(InvestigationStoreError::Invalid)
    );
}

// AC-1410: a task that never egresses to a cloud provider refuses a refusal,
// even one the firewall built against some cloud provider.
#[test]
fn local_task_refuses_a_cloud_refusal_and_its_readback() {
    let (mut fixture, attempt) = acquired();
    let witness = witness(&fixture, &attempt, b"private text");
    assert_eq!(
        commit(&mut fixture, std::slice::from_ref(&witness)),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(reload(&fixture).unwrap().len(), 1);
    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(true));
    commit(&mut fixture, &[witness]).unwrap();
    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(false));
    assert_eq!(
        reload(&fixture).err(),
        Some(InvestigationStoreError::Invalid)
    );
}

fn refusing(fixture: &Fixture, project_rule: bool) -> impl Fn(&str) -> ReadDecision + use<> {
    let source = fixture.r.source.source.clone();
    let rules = if project_rule {
        ReadDenyRules::with_project_rules([source.path.clone()]).unwrap()
    } else {
        ReadDenyRules::default()
    };
    let firewall = EgressFirewall::new(EgressPolicy::default()).with_read_deny_rules(rules);
    let read = ReadIdentity {
        repo: source.repo.clone(),
        path: source.path.clone(),
        commit_sha: source.commit_sha.clone(),
        byte_start: source.byte_start,
        byte_end: source.byte_end,
    };
    move |attempt: &str| firewall.evaluate_read(&Cloud, attempt, &read, b"private text")
}

/// Project and persist one settlement whose candidate was built with
/// `withhold` and whose continuation carries `decide`.
fn settle(
    fixture: &mut Fixture,
    withhold: Option<ReadDecider<'_>>,
    decide: Option<ReadDecider<'_>>,
) -> Result<StoredTask, InvestigationStoreError> {
    fixture.charge();
    let (candidate, prepared) = fixture.candidate_with(None, withhold);
    let continuation = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
        refusal: decide,
    };
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let active = fixture.f.task.detail.usage.active_milliseconds + 1;
    let next = project(
        &tx,
        &fixture.f.task,
        SourceSettlement::Acquired(continuation),
        active,
    )
    .and_then(|plan| persist(&tx, &fixture.f.task, plan))?;
    tx.commit().unwrap();
    fixture.refresh();
    Ok(next)
}

// AC-1414: on a cloud task the firewall's refusal of the acquired read
// commits with the acquisition in one settlement: acquired then refused
// attempts, one refusal event after the completion event, one observation
// without the text, and a reopen that verifies all of it.
#[test]
fn refusal_settles_with_its_acquisition_and_reopens() {
    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(true));
    let mut fixture = Fixture::new();
    let decide = refusing(&fixture, true);
    let revision = fixture.f.task.ledger_revision;
    settle(&mut fixture, Some(&decide), Some(&decide)).unwrap();

    let entries = reload(&fixture).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        entries[0].record.outcome,
        AttemptOutcome::SourceAcquired { .. }
    ));
    let AttemptOutcome::FirewallRefused {
        acquired_attempt_id,
        ..
    } = &entries[1].record.outcome
    else {
        panic!("the refusal follows its acquisition");
    };
    assert_eq!(acquired_attempt_id, &entries[0].record.attempt_id);
    assert_eq!(fixture.f.task.ledger_revision, revision.map(|r| r + 1));
    let kinds: Vec<_> = fixture
        .f
        .store
        .investigation_events(&fixture.f.basis.task_id, 0)
        .unwrap()
        .items
        .into_iter()
        .map(|event| event.kind)
        .rev()
        .take(2)
        .collect();
    assert_eq!(
        kinds,
        [
            InvestigationEventKind::ReadFirewallRefused,
            InvestigationEventKind::ToolCompleted
        ]
    );
    let body: Vec<u8> = fixture
        .f
        .store
        .conn
        .query_row(&format!("SELECT body FROM {TABLE}"), [], |r| r.get(0))
        .unwrap();
    assert!(!body.windows(12).any(|w| w == b"private text"));
}

// AC-1414: the candidate withholds the read exactly when the settlement
// records its refusal, and a refusal is authentic only on a cloud task.
// Every mismatch is refused with nothing written.
#[test]
fn withholding_and_refusal_must_agree() {
    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(true));
    let mut fixture = Fixture::new();
    let decide = refusing(&fixture, true);
    assert_eq!(
        settle(&mut fixture, Some(&decide), None).map(|_| ()),
        Err(InvestigationStoreError::Invalid)
    );
    let mut fixture = Fixture::new();
    let decide = refusing(&fixture, true);
    assert_eq!(
        settle(&mut fixture, None, Some(&decide)).map(|_| ()),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(reload(&fixture).unwrap().len(), 0);

    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(false));
    let mut fixture = Fixture::new();
    let decide = refusing(&fixture, true);
    assert_eq!(
        settle(&mut fixture, None, Some(&decide)).map(|_| ()),
        Err(InvestigationStoreError::Invalid)
    );
}

// AC-1414: an allowed read settles as before, with no refusal.
#[test]
fn allowed_read_settles_without_refusal() {
    completion::read_refusals::FIXTURE_EGRESSES_TO_CLOUD.with(|cloud| cloud.set(true));
    let mut fixture = Fixture::new();
    let allow = refusing(&fixture, false);
    settle(&mut fixture, None, Some(&allow)).unwrap();
    let entries = reload(&fixture).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].record.outcome,
        AttemptOutcome::SourceAcquired { .. }
    ));
}
