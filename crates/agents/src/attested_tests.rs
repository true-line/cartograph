use super::*;

// Placeholder contributors only (ADR-0090 decision 4).
fn engineer_id() -> ContributorId {
    ContributorId("C-00000000000000e1".into())
}
fn agent_id(n: u8) -> ContributorId {
    ContributorId(format!("C-00000000000000a{n}"))
}

fn agent(n: u8, verb: AgentVerb, directed_by: Option<ContributorId>) -> Attestation {
    Attestation::Agent {
        contributor: agent_id(n),
        verb,
        role: "section-writer@1".into(),
        definition_fingerprint: "fp-1".into(),
        provider: "local".into(),
        model: "placeholder-model".into(),
        run: format!("run-{n}"),
        directed_by,
    }
}

fn engineer(verb: EngineerVerb) -> Attestation {
    Attestation::Engineer {
        contributor: engineer_id(),
        verb,
        at_ms: 1_700_000_000_000,
        source: AttestationSource::GapsRegister,
        reference: None,
    }
}

fn record(attestations: Vec<Attestation>) -> AttestedRecord {
    AttestedRecord {
        target: AttestedTarget::Gap {
            gap_id: "gap:handler:login".into(),
            slot: "calls".into(),
        },
        statement: "The login handler is registered at runtime by the plugin loader.".into(),
        supporting: vec!["F-0123456789abcdef".into()],
        recorded_hashes: BTreeMap::from([("F-0123456789abcdef".into(), "h1".into())]),
        attestations,
    }
}

#[test]
fn an_attested_record_is_admitted_with_named_contributors_and_an_a_id() {
    let admitted = admit(record(vec![
        agent(1, AgentVerb::Stated, Some(engineer_id())),
        engineer(EngineerVerb::Confirmed),
    ]))
    .expect("admitted");
    let id = citation_id(&admitted);
    assert!(citation_id_well_formed(&id), "{id}");
    assert!(id.starts_with("A-") && id.len() == 18);
    assert!(
        admitted
            .attestations
            .iter()
            .all(|a| a.contributor().well_formed())
    );
    // The identity covers what is attested, not who reviewed it.
    let mut reviewed = admitted.clone();
    reviewed.attestations.push(engineer(EngineerVerb::Withdrew));
    assert_eq!(citation_id(&reviewed), id);
    let mut changed = admitted.clone();
    changed.statement.push_str(" Twice.");
    assert_ne!(citation_id(&changed), id);
    let mut rehashed = admitted;
    rehashed
        .recorded_hashes
        .insert("F-0123456789abcdef".into(), "h2".into());
    assert_ne!(citation_id(&rehashed), id);
    // A record holds contributor ids only, never a name.
    let json = serde_json::to_string(&reviewed).unwrap();
    assert!(
        !json.contains("\"display_name\"") && !json.contains("\"handle\""),
        "{json}"
    );
}

#[test]
fn an_engineer_statement_or_confirmation_backs_the_record() {
    assert_eq!(
        standing(&[engineer(EngineerVerb::Stated)]),
        Standing::EngineerBacked
    );
    let confirmed = [
        agent(1, AgentVerb::Stated, Some(engineer_id())),
        engineer(EngineerVerb::Confirmed),
    ];
    assert_eq!(standing(&confirmed), Standing::EngineerBacked);
    assert!(Standing::EngineerBacked.accepted());
    assert_eq!(Standing::EngineerBacked.label(), "attested");
}

#[test]
fn agents_alone_are_agent_attested_unconfirmed_and_never_accepted() {
    let agents_only = [
        agent(1, AgentVerb::Stated, None),
        agent(2, AgentVerb::Agreed, None),
    ];
    let standing = standing(&agents_only);
    assert_eq!(standing, Standing::AgentAttestedUnconfirmed);
    assert_eq!(standing.label(), "agent-attested, unconfirmed");
    assert!(!standing.accepted());
    assert!(standing.citable());
    assert!(admit(record(agents_only.to_vec())).is_ok());
}

#[test]
fn a_withdrawal_or_dispute_wins_over_backing() {
    let withdrawn = [
        engineer(EngineerVerb::Stated),
        engineer(EngineerVerb::Withdrew),
    ];
    assert_eq!(standing(&withdrawn), Standing::Withdrawn);
    assert!(!Standing::Withdrawn.citable());
    let disputed = [
        agent(1, AgentVerb::Stated, Some(engineer_id())),
        engineer(EngineerVerb::Disputed),
    ];
    assert_eq!(standing(&disputed), Standing::Disputed);
    let agent_dispute = [
        engineer(EngineerVerb::Stated),
        agent(1, AgentVerb::Disputed, Some(engineer_id())),
    ];
    assert_eq!(standing(&agent_dispute), Standing::Disputed);
    assert!(!Standing::Disputed.citable());
    // A contributor's latest attestation counts: a dispute taken back by a
    // later confirmation no longer disputes.
    let settled = [
        agent(1, AgentVerb::Stated, Some(engineer_id())),
        engineer(EngineerVerb::Disputed),
        engineer(EngineerVerb::Confirmed),
    ];
    assert_eq!(standing(&settled), Standing::EngineerBacked);
    // The engineer's withdrawal holds after later agent attestations.
    let restated = [
        engineer(EngineerVerb::Stated),
        engineer(EngineerVerb::Withdrew),
        agent(1, AgentVerb::Agreed, None),
    ];
    assert_eq!(standing(&restated), Standing::Withdrawn);
}

#[test]
fn a_second_engineer_needs_only_another_contributor_id() {
    let second = ContributorId("C-00000000000000e2".into());
    let mut both = vec![
        engineer(EngineerVerb::Stated),
        Attestation::Engineer {
            contributor: second.clone(),
            verb: EngineerVerb::Disputed,
            at_ms: 1_700_000_000_001,
            source: AttestationSource::Workbench,
            reference: Some("review 7".into()),
        },
    ];
    assert!(admit(record(both.clone())).is_ok());
    assert_eq!(standing(&both), Standing::Disputed);
    both.push(Attestation::Engineer {
        contributor: second,
        verb: EngineerVerb::Confirmed,
        at_ms: 1_700_000_000_002,
        source: AttestationSource::Workbench,
        reference: None,
    });
    assert_eq!(standing(&both), Standing::EngineerBacked);
}

#[test]
fn a_malformed_record_is_refused_with_a_closed_code() {
    use AttestationRefusal as R;
    let refuse = |change: &dyn Fn(&mut AttestedRecord)| {
        let mut value = record(vec![engineer(EngineerVerb::Stated)]);
        change(&mut value);
        admit(value).expect_err("refused")
    };
    assert_eq!(
        refuse(&|r| r.target = AttestedTarget::Gap {
            gap_id: " ".into(),
            slot: "calls".into()
        }),
        R::TargetIncomplete
    );
    assert_eq!(refuse(&|r| r.statement = " .".into()), R::BlankStatement);
    assert_eq!(
        refuse(&|r| r.statement = "x".repeat(MAX_STATEMENT_BYTES + 1)),
        R::StatementTooLong
    );
    assert_eq!(
        refuse(&|r| r.statement = "two\nlines".into()),
        R::StatementNotPlainText
    );
    assert_eq!(
        refuse(&|r| r.statement = "The key is AKIAIOSFODNN7EXAMPLE.".into()),
        R::StatementHoldsSecret
    );
    assert_eq!(
        refuse(&|r| r.supporting.push("F-0123456789abcdef".into())),
        R::DuplicateCitation
    );
    assert_eq!(
        refuse(&|r| {
            r.supporting = vec!["A-0123456789abcdef".into()];
            r.recorded_hashes = BTreeMap::from([("A-0123456789abcdef".into(), "h".into())]);
        }),
        R::CitationMalformed
    );
    assert_eq!(
        refuse(&|r| r.recorded_hashes.clear()),
        R::RecordedHashesMismatch
    );
    assert_eq!(refuse(&|r| r.attestations.clear()), R::NoAttestation);
    assert_eq!(
        refuse(&|r| r.attestations = vec![engineer(EngineerVerb::Confirmed)]),
        R::FirstDoesNotState
    );
    assert_eq!(
        refuse(&|r| r.attestations = vec![engineer(EngineerVerb::Stated); MAX_ATTESTATIONS + 1]),
        R::TooManyAttestations
    );
    assert_eq!(
        refuse(&|r| r.attestations.push(Attestation::Engineer {
            contributor: ContributorId("engineer".into()),
            verb: EngineerVerb::Confirmed,
            at_ms: 0,
            source: AttestationSource::Workbench,
            reference: None,
        })),
        R::ContributorMalformed { attestation: 1 }
    );
    assert_eq!(
        refuse(&|r| r.attestations.push(Attestation::Agent {
            contributor: engineer_id(),
            verb: AgentVerb::Agreed,
            role: "r".into(),
            definition_fingerprint: "f".into(),
            provider: "p".into(),
            model: "m".into(),
            run: "x".into(),
            directed_by: None,
        })),
        R::ContributorKindConflict { attestation: 1 }
    );
    assert_eq!(
        refuse(&|r| r.attestations.push(Attestation::Agent {
            contributor: agent_id(1),
            verb: AgentVerb::Agreed,
            role: " ".into(),
            definition_fingerprint: "f".into(),
            provider: "p".into(),
            model: "m".into(),
            run: "x".into(),
            directed_by: None,
        })),
        R::AgentFieldInvalid { attestation: 1 }
    );
    assert_eq!(
        refuse(&|r| r
            .attestations
            .push(agent(1, AgentVerb::Agreed, Some(agent_id(2))))),
        R::DirectorNotEngineer { attestation: 1 }
    );
    assert_eq!(
        refuse(&|r| r.attestations.push(Attestation::Engineer {
            contributor: engineer_id(),
            verb: EngineerVerb::Confirmed,
            at_ms: 0,
            source: AttestationSource::Workbench,
            reference: Some("a\u{7}b".into()),
        })),
        R::ReferenceInvalid { attestation: 1 }
    );
    assert_eq!(R::FirstDoesNotState.code(), "first_does_not_state");
}

#[test]
fn a_section_address_is_a_target_too() {
    let mut value = record(vec![agent(1, AgentVerb::Stated, None)]);
    value.target = AttestedTarget::Section {
        address: SectionAddress {
            catalogue_version: "1.0".into(),
            document: "D1".into(),
            section: "1.3".into(),
            scope: crate::section_draft::SYSTEM_SCOPE.into(),
        },
    };
    value.supporting.clear();
    value.recorded_hashes.clear();
    let admitted = admit(value).expect("admitted");
    let json = serde_json::to_value(&admitted).unwrap();
    assert_eq!(json["target"]["kind"], "section");
    let back: AttestedRecord = serde_json::from_value(json).unwrap();
    assert_eq!(citation_id(&back), citation_id(&admitted));
}
