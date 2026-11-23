use super::*;
use crate::attested::{AgentVerb, AttestationSource, EngineerVerb};

// Placeholder contributors only (ADR-0090 decision 4).
const ENGINEER: &str = "Placeholder Engineer";
const HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn store() -> (tempfile::TempDir, AttestedStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = AttestedStore::open(dir.path().join("review.sqlite")).unwrap();
    (dir, store)
}

fn agent_says(
    agent: &Contributor,
    verb: AgentVerb,
    directed_by: Option<&Contributor>,
) -> Attestation {
    // The role, provider and model it was registered for, from its
    // display name `role (provider/model)`; placeholders for a contributor
    // that is not a registered agent.
    let (role, provider, model) = agent
        .display_name
        .split_once(" (")
        .and_then(|(role, rest)| {
            let (provider, model) = rest.trim_end_matches(')').split_once('/')?;
            Some((role, provider, model))
        })
        .unwrap_or(("section-writer@1", "local", "placeholder-model"));
    Attestation::Agent {
        contributor: agent.id.clone(),
        verb,
        role: role.into(),
        definition_fingerprint: "fp-1".into(),
        provider: provider.into(),
        model: model.into(),
        run: "run-1".into(),
        directed_by: directed_by.map(|engineer| engineer.id.clone()),
    }
}

fn engineer_says(engineer: &Contributor, verb: EngineerVerb) -> Attestation {
    Attestation::Engineer {
        contributor: engineer.id.clone(),
        verb,
        at_ms: 1_700_000_000_000,
        source: AttestationSource::GapsRegister,
        reference: None,
    }
}

fn record(statement: &str, attestations: Vec<Attestation>) -> AttestedRecord {
    AttestedRecord {
        target: AttestedTarget::Gap {
            gap_id: "gap:handler:login".into(),
            slot: "calls".into(),
        },
        statement: statement.into(),
        supporting: vec!["F-0123456789abcdef".into()],
        recorded_hashes: BTreeMap::from([("F-0123456789abcdef".into(), HASH.into())]),
        attestations,
    }
}

#[test]
fn a_record_is_stored_once_and_its_attestations_are_appended_at_the_count_read() {
    let (_dir, mut store) = store();
    let engineer = store.set_engineer(ENGINEER, None).unwrap();
    let writer = store
        .agent("section-writer@1", "local", "placeholder-model")
        .unwrap();

    // An agent states, directed by the engineer, before the engineer has
    // attested anything.
    let staged = store
        .stage(record(
            "The login handler is registered at runtime.",
            vec![agent_says(&writer, AgentVerb::Stated, Some(&engineer))],
        ))
        .unwrap();
    assert_eq!(staged.standing, Standing::AgentAttestedUnconfirmed);
    assert_eq!(staged.attestation_count(), 1);
    assert!(staged.citation_id.starts_with("A-"));

    // The same content again returns the stored record unchanged.
    let again = store
        .stage(record(
            "The login handler is registered at runtime.",
            vec![agent_says(&writer, AgentVerb::Stated, None)],
        ))
        .unwrap();
    assert_eq!(again, staged);

    let confirmed = store
        .attest(
            &staged.record_id,
            1,
            engineer_says(&engineer, EngineerVerb::Confirmed),
        )
        .unwrap();
    assert_eq!(confirmed.standing, Standing::EngineerBacked);
    assert_eq!(confirmed.citation_id, staged.citation_id);

    // A review made against the count read before is refused, not lost.
    let stale = store
        .attest(
            &staged.record_id,
            1,
            engineer_says(&engineer, EngineerVerb::Withdrew),
        )
        .unwrap_err();
    assert!(
        matches!(
            stale,
            AttestedStoreError::StaleAttestationCount {
                expected: 1,
                actual: 2
            }
        ),
        "{stale:?}"
    );
    let withdrawn = store
        .attest(
            &staged.record_id,
            2,
            engineer_says(&engineer, EngineerVerb::Withdrew),
        )
        .unwrap();
    assert_eq!(withdrawn.standing, Standing::Withdrawn);
    assert_eq!(store.get(&staged.record_id).unwrap().unwrap(), withdrawn);
    assert!(matches!(
        store.attest("missing", 0, engineer_says(&engineer, EngineerVerb::Stated)),
        Err(AttestedStoreError::UnknownRecord)
    ));
}

#[test]
fn the_project_holds_one_engineer_and_names_only_in_its_contributor_table() {
    let (dir, mut store) = store();
    let engineer = store
        .set_engineer(ENGINEER, Some("engineer@example.invalid"))
        .unwrap();
    assert!(engineer.id.well_formed());
    assert_eq!(engineer.kind, ContributorKind::Engineer);
    assert!(!engineer.id.0.contains("Placeholder"));
    assert!(matches!(
        store.set_engineer("Second Placeholder", None),
        Err(AttestedStoreError::EngineerExists)
    ));
    assert_eq!(store.engineer().unwrap(), Some(engineer.clone()));
    assert!(matches!(
        store.set_engineer(" ", None),
        Err(AttestedStoreError::InvalidContributor(_))
    ));

    // Agents are as many as their roles, providers and models; the same
    // triple is the same contributor.
    let first = store.agent("section-writer@1", "local", "model-a").unwrap();
    let second = store.agent("claim-reviewer@1", "local", "model-a").unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(
        store.agent("section-writer@1", "local", "model-a").unwrap(),
        first
    );

    let stored = store
        .stage(record(
            "The login handler is registered at runtime.",
            vec![
                engineer_says(&engineer, EngineerVerb::Stated),
                agent_says(&first, AgentVerb::Agreed, Some(&engineer)),
            ],
        ))
        .unwrap();
    let json = serde_json::to_string(&stored.record).unwrap();
    assert!(
        !json.contains(ENGINEER) && !json.contains("example.invalid"),
        "{json}"
    );
    drop(store);
    // The name is in the contributor table and in no record or attestation
    // row.
    let conn = Connection::open(dir.path().join("review.sqlite")).unwrap();
    let in_records: i64 = conn
        .query_row(
            "SELECT (SELECT count(*) FROM attested_records WHERE instr(content_json, ?1) > 0)
                  + (SELECT count(*) FROM attested_attestations WHERE instr(attestation_json, ?1) > 0)",
            [ENGINEER],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(in_records, 0);
}

#[test]
fn agents_fill_a_gap_together_and_stay_unconfirmed_until_the_engineer_confirms() {
    let (_dir, mut store) = store();
    let writer = store.agent("section-writer@1", "local", "model-a").unwrap();
    let reviewer = store.agent("claim-reviewer@1", "local", "model-b").unwrap();
    let staged = store
        .stage(record(
            "Sessions expire after the configured idle period.",
            vec![agent_says(&writer, AgentVerb::Stated, None)],
        ))
        .unwrap();
    let agreed = store
        .attest(
            &staged.record_id,
            1,
            agent_says(&reviewer, AgentVerb::Agreed, None),
        )
        .unwrap();
    assert_eq!(agreed.standing, Standing::AgentAttestedUnconfirmed);
    assert!(!agreed.standing.accepted());
    let engineer = store.set_engineer(ENGINEER, None).unwrap();
    let confirmed = store
        .attest(
            &staged.record_id,
            2,
            engineer_says(&engineer, EngineerVerb::Confirmed),
        )
        .unwrap();
    assert!(confirmed.standing.accepted());
}

#[test]
fn every_attestation_names_a_known_contributor_of_its_kind() {
    let (_dir, mut store) = store();
    let engineer = store.set_engineer(ENGINEER, None).unwrap();
    let writer = store.agent("section-writer@1", "local", "model-a").unwrap();
    let stranger = Contributor {
        id: ContributorId("C-00000000000000ff".into()),
        kind: ContributorKind::Agent,
        display_name: "unknown".into(),
        handle: None,
    };
    assert!(matches!(
        store.stage(record(
            "A statement.",
            vec![agent_says(&stranger, AgentVerb::Stated, None)]
        )),
        Err(AttestedStoreError::UnknownContributor { attestation: 0 })
    ));
    // The engineer's id used for an agent attestation.
    let posing = Contributor {
        kind: ContributorKind::Agent,
        ..engineer.clone()
    };
    assert!(matches!(
        store.stage(record(
            "A statement.",
            vec![agent_says(&posing, AgentVerb::Stated, None)]
        )),
        Err(AttestedStoreError::ContributorKind { attestation: 0 })
    ));
    // An agent directed by another agent.
    let other = store.agent("claim-reviewer@1", "local", "model-b").unwrap();
    assert!(matches!(
        store.stage(record(
            "A statement.",
            vec![agent_says(&writer, AgentVerb::Stated, Some(&other))]
        )),
        Err(AttestedStoreError::DirectorNotEngineer { attestation: 0 })
    ));
    // An inadmissible record or append is refused whole.
    assert!(matches!(
        store.stage(record(
            "A statement.",
            vec![engineer_says(&engineer, EngineerVerb::Confirmed)]
        )),
        Err(AttestedStoreError::Refused(
            AttestationRefusal::FirstDoesNotState
        ))
    ));
    let staged = store
        .stage(record(
            "A statement.",
            vec![engineer_says(&engineer, EngineerVerb::Stated)],
        ))
        .unwrap();
    assert!(matches!(
        store.attest(
            &staged.record_id,
            1,
            agent_says(&stranger, AgentVerb::Agreed, None)
        ),
        Err(AttestedStoreError::UnknownContributor { attestation: 1 })
    ));
    assert_eq!(
        store
            .get(&staged.record_id)
            .unwrap()
            .unwrap()
            .attestation_count(),
        1
    );
}

#[test]
fn stored_content_and_attestations_cannot_be_changed() {
    let (dir, mut store) = store();
    let engineer = store.set_engineer(ENGINEER, None).unwrap();
    let staged = store
        .stage(record(
            "A statement.",
            vec![engineer_says(&engineer, EngineerVerb::Stated)],
        ))
        .unwrap();
    drop(store);
    let conn = Connection::open(dir.path().join("review.sqlite")).unwrap();
    for sql in [
        "UPDATE attested_records SET content_json = '{}'",
        "UPDATE attested_attestations SET attestation_json = '{}'",
        "DELETE FROM attested_attestations",
    ] {
        let error = conn.execute(sql, []).unwrap_err().to_string();
        assert!(
            error.contains("immutable") || error.contains("append-only"),
            "{sql}: {error}"
        );
    }
    drop(conn);
    let store = AttestedStore::open(dir.path().join("review.sqlite")).unwrap();
    assert_eq!(store.get(&staged.record_id).unwrap().unwrap(), staged);
}

#[test]
fn records_are_listed_newest_first_in_bounded_pages() {
    let (_dir, mut store) = store();
    let engineer = store.set_engineer(ENGINEER, None).unwrap();
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(
            store
                .stage(record(
                    &format!("Statement number {n}."),
                    vec![engineer_says(&engineer, EngineerVerb::Stated)],
                ))
                .unwrap()
                .record_id,
        );
    }
    let first = store.list(2, None).unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.record_id.clone())
            .collect::<Vec<_>>(),
        vec![ids[2].clone(), ids[1].clone()]
    );
    let rest = store.list(2, first.next_before).unwrap();
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.items[0].record_id, ids[0]);
    assert_eq!(rest.next_before, None);
    assert!(matches!(
        store.list(0, None),
        Err(AttestedStoreError::InvalidLimit)
    ));
    assert!(matches!(
        store.list(MAX_ATTESTED_PAGE_ITEMS + 1, None),
        Err(AttestedStoreError::InvalidLimit)
    ));
}

#[test]
fn an_agent_attestation_must_match_the_role_provider_and_model_of_its_id() {
    let (_dir, mut store) = store();
    let writer = store.agent("section-writer@1", "local", "model-a").unwrap();
    // The id registered for model-a, claiming another model.
    let mut claimed = agent_says(&writer, AgentVerb::Stated, None);
    if let Attestation::Agent { model, .. } = &mut claimed {
        *model = "model-b".into();
    }
    assert!(matches!(
        store.stage(record("A statement.", vec![claimed])),
        Err(AttestedStoreError::AgentMismatch { attestation: 0 })
    ));
}

#[test]
fn a_stored_record_becomes_projection_input_with_ids_and_standing() {
    let (_dir, mut store) = store();
    let engineer = store.set_engineer(ENGINEER, None).unwrap();
    let writer = store
        .agent("section-writer@1", "local", "placeholder-model")
        .unwrap();
    let staged = store
        .stage(record(
            "The login handler is registered at runtime.",
            vec![agent_says(&writer, AgentVerb::Stated, Some(&engineer))],
        ))
        .unwrap();
    let input = reviewed_attested(&staged);
    assert_eq!(input.standing, AttestedStanding::AgentAttestedUnconfirmed);
    assert_eq!(input.citation_id, staged.citation_id);
    assert_eq!(
        input.answer,
        AttestedAnswer::Gap {
            gap_id: "gap:handler:login".into(),
            slot: "calls".into()
        }
    );
    let confirmed = store
        .attest(
            &staged.record_id,
            1,
            engineer_says(&engineer, EngineerVerb::Confirmed),
        )
        .unwrap();
    let input = reviewed_attested(&confirmed);
    assert_eq!(input.standing, AttestedStanding::EngineerBacked);
    assert_eq!(
        input
            .attestations
            .iter()
            .map(|a| (a.engineer, a.verb.as_str(), a.role.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            (false, "stated", Some("section-writer@1")),
            (true, "confirmed", None)
        ]
    );
    let json = serde_json::to_string(&input).unwrap();
    assert!(!json.contains(ENGINEER), "{json}");
}
