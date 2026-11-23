//! AC-0666 (T-0666): accepted section drafts in the curated projection.

use super::*;

const GRAPH_HASH: &str = "graph-hash-1";

fn address(section: &str) -> DraftAddress {
    DraftAddress {
        catalogue_version: "1.0".into(),
        document: "D1".into(),
        section: section.into(),
        scope: "system".into(),
    }
}

/// An accepted draft for §1.3 citing `F-1` and `F-2` as the graph holds them.
fn draft(id: &str) -> ReviewedDraft {
    ReviewedDraft {
        draft_id: id.into(),
        address: address("1.3"),
        role: "section-writer@1".into(),
        claims: vec![DraftClaim {
            statement: "Checkout takes a cart to an order.".into(),
            kind: "implemented".into(),
            citations: vec!["F-1".into(), "F-2".into()],
        }],
        cited_hashes: BTreeMap::from([
            ("F-1".to_owned(), "hash-1".to_owned()),
            ("F-2".to_owned(), "hash-2".to_owned()),
        ]),
        replaces: None,
        decision: Some(DraftReview::Accepted),
    }
}

fn current() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("F-1".to_owned(), "hash-1".to_owned()),
        ("F-2".to_owned(), "hash-2".to_owned()),
        ("F-3".to_owned(), "hash-3".to_owned()),
    ])
}

fn run_with(
    drafts: &[ReviewedDraft],
    current: &BTreeMap<String, String>,
    graph_hash: &str,
) -> Result<CuratedProjection, ProjectionError> {
    project(&ProjectionInput {
        nodes: &[],
        edges: &[],
        graph_content_hash: graph_hash,
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts,
        cited_fact_hashes: current,
        findings: &[],
        cited_fact_digests: &std::collections::BTreeMap::new(),
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
}

fn run(drafts: &[ReviewedDraft]) -> CuratedProjection {
    run_with(drafts, &current(), GRAPH_HASH).unwrap()
}

fn outcome(projection: &CuratedProjection, id: &str) -> (DraftState, DraftStateReason) {
    let mut found = projection.drafts.iter().filter(|d| d.draft_id == id);
    let entry = found.next().unwrap();
    assert!(found.next().is_none(), "{id} is listed once");
    (entry.state, entry.reason)
}

#[test]
fn an_accepted_draft_whose_cited_facts_are_unchanged_is_eligible_and_stays_t3() {
    let projection = run(&[draft("d1")]);
    let [entry] = projection.drafts.as_slice() else {
        panic!("one draft");
    };
    assert_eq!(entry.state, DraftState::Eligible);
    assert_eq!(entry.reason, DraftStateReason::AllConditionsHold);
    assert_eq!(entry.tier, Tier::Agentic);
    assert_eq!(entry.confidence, ConfidenceTier::InferredWeak);
    assert_eq!(entry.review_status, DraftReview::Accepted);
    assert_eq!(entry.cited_facts, 2);
    assert_eq!(entry.address, address("1.3"));
    assert_eq!(entry.role, "section-writer@1");
    // The claims are carried as stored, for the document that prints them.
    assert_eq!(entry.claims, draft("d1").claims);
    assert_eq!(
        projection.draft_counts,
        DraftCounts {
            accepted: 1,
            eligible: 1,
            ..DraftCounts::default()
        }
    );
    // Drafts are not proposals: no record is an item or listed.
    assert!(projection.items.is_empty() && projection.listed.is_empty());
    assert_eq!(projection.counts, CuratedCounts::default());
    assert_eq!(projection.graph_basis, GRAPH_HASH);
    assert!(projection.revision.starts_with("curated-revision-v4:"));
}

#[test]
fn a_draft_is_stale_exactly_when_a_cited_fact_has_another_hash() {
    // An uncited fact changing, or a new fact, leaves the draft eligible.
    let mut other = current();
    other.insert("F-3".into(), "hash-3b".into());
    other.insert("F-4".into(), "hash-4".into());
    let projection = run_with(&[draft("d1")], &other, GRAPH_HASH).unwrap();
    assert_eq!(outcome(&projection, "d1").0, DraftState::Eligible);

    for cited in ["F-1", "F-2"] {
        let mut changed = current();
        changed.insert(cited.into(), "hash-new".into());
        let projection = run_with(&[draft("d1")], &changed, GRAPH_HASH).unwrap();
        assert_eq!(
            outcome(&projection, "d1"),
            (DraftState::Stale, DraftStateReason::CitedFactChanged),
            "{cited}"
        );
        assert_eq!(projection.draft_counts.stale, 1);
        assert_eq!(projection.draft_counts.eligible, 0);
    }

    // The graph hash alone moving does not make a draft stale.
    let projection = run_with(&[draft("d1")], &current(), "graph-hash-2").unwrap();
    assert_eq!(outcome(&projection, "d1").0, DraftState::Eligible);
}

#[test]
fn a_cited_id_with_no_current_fact_is_unresolvable_before_stale() {
    let mut gone = current();
    gone.remove("F-2");
    gone.insert("F-1".into(), "hash-new".into());
    let projection = run_with(&[draft("d1")], &gone, GRAPH_HASH).unwrap();
    assert_eq!(
        outcome(&projection, "d1"),
        (DraftState::Unresolvable, DraftStateReason::CitedFactMissing)
    );
    assert_eq!(projection.draft_counts.unresolvable, 1);
}

#[test]
fn an_accepted_successor_supersedes_the_draft_it_replaces() {
    let successor = ReviewedDraft {
        replaces: Some("d1".into()),
        ..draft("d2")
    };
    let projection = run(&[draft("d1"), successor.clone()]);
    assert_eq!(
        outcome(&projection, "d1"),
        (
            DraftState::Superseded,
            DraftStateReason::ReplacedByAcceptedDraft
        )
    );
    // The pair does not conflict: one replaces the other.
    assert_eq!(outcome(&projection, "d2").0, DraftState::Eligible);

    // Superseded comes first, even when the replaced draft is also stale.
    let mut changed = current();
    changed.insert("F-1".into(), "hash-new".into());
    let projection = run_with(&[draft("d1"), successor.clone()], &changed, GRAPH_HASH).unwrap();
    assert_eq!(outcome(&projection, "d1").0, DraftState::Superseded);
    assert_eq!(outcome(&projection, "d2").0, DraftState::Stale);

    // A successor that is not accepted supersedes nothing.
    for decision in [None, Some(DraftReview::Rejected)] {
        let projection = run(&[
            draft("d1"),
            ReviewedDraft {
                decision,
                ..successor.clone()
            },
        ]);
        assert_eq!(outcome(&projection, "d1").0, DraftState::Eligible);
        assert_eq!(projection.drafts.len(), 1);
    }
}

#[test]
fn a_replacement_chain_leaves_only_its_last_draft_eligible() {
    // d1 <- d2 <- d3: a section re-drafted and accepted twice.
    let d2 = ReviewedDraft {
        replaces: Some("d1".into()),
        ..draft("d2")
    };
    let d3 = ReviewedDraft {
        replaces: Some("d2".into()),
        ..draft("d3")
    };
    let projection = run(&[draft("d1"), d2.clone(), d3.clone()]);
    assert_eq!(outcome(&projection, "d1").0, DraftState::Superseded);
    assert_eq!(outcome(&projection, "d2").0, DraftState::Superseded);
    assert_eq!(
        outcome(&projection, "d3"),
        (DraftState::Eligible, DraftStateReason::AllConditionsHold)
    );
    assert_eq!(projection.draft_counts.superseded, 2);
    assert_eq!(projection.draft_counts.conflicted, 0);

    // Another accepted draft beside the chain conflicts with its last draft
    // only.
    let projection = run(&[draft("d1"), d2, d3, draft("d4")]);
    assert_eq!(outcome(&projection, "d3").0, DraftState::Conflicted);
    assert_eq!(outcome(&projection, "d4").0, DraftState::Conflicted);
    assert_eq!(projection.draft_counts.conflicted, 2);
}

#[test]
fn two_accepted_drafts_for_one_address_conflict_unless_one_replaces_the_other() {
    let projection = run(&[draft("d1"), draft("d2")]);
    for id in ["d1", "d2"] {
        assert_eq!(
            outcome(&projection, id),
            (
                DraftState::Conflicted,
                DraftStateReason::AnotherDraftAccepted
            )
        );
    }
    assert_eq!(projection.draft_counts.conflicted, 2);

    // Another address or another scope is another section.
    let elsewhere = ReviewedDraft {
        address: address("1.2"),
        ..draft("d2")
    };
    let scoped = ReviewedDraft {
        address: DraftAddress {
            scope: "F-1".into(),
            ..address("1.3")
        },
        ..draft("d3")
    };
    let projection = run(&[draft("d1"), elsewhere, scoped]);
    for id in ["d1", "d2", "d3"] {
        assert_eq!(outcome(&projection, id).0, DraftState::Eligible, "{id}");
    }

    // A third accepted draft conflicts with a replacing pair's survivor.
    let successor = ReviewedDraft {
        replaces: Some("d1".into()),
        ..draft("d2")
    };
    let projection = run(&[draft("d1"), successor, draft("d3")]);
    assert_eq!(outcome(&projection, "d1").0, DraftState::Superseded);
    assert_eq!(outcome(&projection, "d2").0, DraftState::Conflicted);
    assert_eq!(outcome(&projection, "d3").0, DraftState::Conflicted);

    // A stale draft still counts against the other.
    let stale = ReviewedDraft {
        cited_hashes: BTreeMap::from([("F-1".to_owned(), "hash-old".to_owned())]),
        ..draft("d2")
    };
    let projection = run(&[draft("d1"), stale]);
    assert_eq!(outcome(&projection, "d1").0, DraftState::Conflicted);
    assert_eq!(outcome(&projection, "d2").0, DraftState::Stale);
}

#[test]
fn every_accepted_draft_has_exactly_one_state_and_the_counts_add_up() {
    let drafts = [
        draft("a"),
        ReviewedDraft {
            replaces: Some("a".into()),
            ..draft("b")
        },
        ReviewedDraft {
            address: address("1.1"),
            cited_hashes: BTreeMap::from([("F-9".to_owned(), "hash-9".to_owned())]),
            ..draft("c")
        },
        ReviewedDraft {
            address: address("1.2"),
            cited_hashes: BTreeMap::from([("F-1".to_owned(), "hash-old".to_owned())]),
            ..draft("d")
        },
        ReviewedDraft {
            address: address("1.4"),
            ..draft("e")
        },
        ReviewedDraft {
            address: address("1.4"),
            ..draft("f")
        },
        ReviewedDraft {
            address: address("1.5"),
            decision: Some(DraftReview::Rejected),
            ..draft("g")
        },
        ReviewedDraft {
            address: address("1.5"),
            decision: Some(DraftReview::Returned),
            ..draft("h")
        },
        ReviewedDraft {
            address: address("1.5"),
            decision: None,
            ..draft("i")
        },
    ];
    let projection = run(&drafts);
    let states: Vec<(&str, DraftState)> = projection
        .drafts
        .iter()
        .map(|d| (d.draft_id.as_str(), d.state))
        .collect();
    assert_eq!(
        states,
        [
            ("a", DraftState::Superseded),
            ("b", DraftState::Eligible),
            ("c", DraftState::Unresolvable),
            ("d", DraftState::Stale),
            ("e", DraftState::Conflicted),
            ("f", DraftState::Conflicted),
        ]
    );
    assert_eq!(
        projection.draft_counts,
        DraftCounts {
            accepted: 6,
            eligible: 1,
            superseded: 1,
            unresolvable: 1,
            stale: 1,
            conflicted: 2,
        }
    );
    // Input order does not matter.
    let mut reversed = drafts.to_vec();
    reversed.reverse();
    assert_eq!(run(&reversed), projection);
}

#[test]
fn the_revision_covers_drafts_and_any_review_changes_it() {
    let base = run(&[draft("d1")]);
    assert_eq!(run(&[draft("d1")]).revision, base.revision);
    let with = |decision| {
        run(&[ReviewedDraft {
            decision,
            ..draft("d1")
        }])
    };
    let pending = with(None);
    let rejected = with(Some(DraftReview::Rejected));
    let returned = with(Some(DraftReview::Returned));
    // Accepting and rejecting each give a revision of their own.
    assert_ne!(pending.revision, base.revision);
    assert_ne!(rejected.revision, base.revision);
    assert_ne!(rejected.revision, pending.revision);
    // A pending or returned draft does not enter: the revision is that of
    // no draft at all.
    assert_eq!(pending.revision, run(&[]).revision);
    assert_eq!(returned.revision, pending.revision);
    assert!(rejected.drafts.is_empty() && returned.drafts.is_empty());

    // A state change is a revision change.
    let mut changed = current();
    changed.insert("F-1".into(), "hash-new".into());
    let stale = run_with(&[draft("d1")], &changed, GRAPH_HASH).unwrap();
    assert_ne!(stale.revision, base.revision);

    // The graph content hash is carried as given and not derived from drafts.
    for projection in [&base, &pending, &rejected, &stale] {
        assert_eq!(projection.graph_basis, GRAPH_HASH);
    }
}

#[test]
fn two_drafts_with_one_identity_fail_closed() {
    for decision in [None, Some(DraftReview::Accepted)] {
        assert_eq!(
            run_with(
                &[
                    draft("d1"),
                    ReviewedDraft {
                        decision,
                        ..draft("d1")
                    }
                ],
                &current(),
                GRAPH_HASH
            ),
            Err(ProjectionError::DuplicateRecord)
        );
    }
}

// AC-0671 (ADR-0045 decision 2): the changed and missing cited ids are
// named, in id order, whatever state the draft has, and the same inputs
// give the same lists.
#[test]
fn the_changed_and_missing_cited_facts_are_named_in_id_order() {
    let lists = |projection: &CuratedProjection, id: &str| {
        let entry = projection
            .drafts
            .iter()
            .find(|entry| entry.draft_id == id)
            .unwrap();
        (entry.changed.clone(), entry.missing.clone())
    };
    let none: Vec<String> = Vec::new();

    // Unchanged: both lists are empty.
    let projection = run_with(&[draft("d1")], &current(), GRAPH_HASH).unwrap();
    assert_eq!(lists(&projection, "d1"), (none.clone(), none.clone()));

    // Both cited facts changed: stale, both named, in id order.
    let mut both = current();
    both.insert("F-2".into(), "hash-2b".into());
    both.insert("F-1".into(), "hash-1b".into());
    let projection = run_with(&[draft("d1")], &both, GRAPH_HASH).unwrap();
    assert_eq!(outcome(&projection, "d1").0, DraftState::Stale);
    assert_eq!(
        lists(&projection, "d1"),
        (vec!["F-1".to_owned(), "F-2".to_owned()], none.clone())
    );

    // One changed and one gone: unresolvable, and each list names its own.
    let mut gone = current();
    gone.remove("F-2");
    gone.insert("F-1".into(), "hash-1b".into());
    let projection = run_with(&[draft("d1")], &gone, GRAPH_HASH).unwrap();
    assert_eq!(outcome(&projection, "d1").0, DraftState::Unresolvable);
    assert_eq!(
        lists(&projection, "d1"),
        (vec!["F-1".to_owned()], vec!["F-2".to_owned()])
    );
    // The same inputs give the same lists.
    let again = run_with(&[draft("d1")], &gone, GRAPH_HASH).unwrap();
    assert_eq!(lists(&again, "d1"), lists(&projection, "d1"));
    assert_eq!(again.revision, projection.revision);

    // A superseded draft still names what changed under it.
    let successor = ReviewedDraft {
        replaces: Some("d1".into()),
        ..draft("d2")
    };
    let projection = run_with(&[draft("d1"), successor], &both, GRAPH_HASH).unwrap();
    assert_eq!(outcome(&projection, "d1").0, DraftState::Superseded);
    assert_eq!(lists(&projection, "d1").0, ["F-1", "F-2"]);

    // An uncited fact changing names nothing.
    let mut other = current();
    other.insert("F-3".into(), "hash-3b".into());
    let projection = run_with(&[draft("d1")], &other, GRAPH_HASH).unwrap();
    assert_eq!(lists(&projection, "d1"), (none.clone(), none));
}
