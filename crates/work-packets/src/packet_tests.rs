use super::*;
use crate::*;
use core_graph::Node;
use core_prov::{ConfidenceTier, ProducingMethod, Provenance, Tier};
use serde_json::json;
use std::sync::{Arc, Barrier};

fn fact(name: &str) -> FactEvidence {
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![],
        "fixture-rule",
        name.as_bytes(),
    )
    .unwrap()
    .with_method(ProducingMethod::Rule {
        rule: "fixture".into(),
        version: "1".into(),
    });
    let fact = PacketFact::Node(Node {
        id: name.into(),
        label: "Symbol".into(),
        props: json!({"name":name,"prov":provenance,"upstream_timestamp":"2001-01-01T00:00:00Z"}),
    });
    let reference = fact.reference();
    let expected_digest = fact.digest().unwrap();
    FactEvidence {
        id: evidence_id("fact", &(&reference, &expected_digest)).unwrap(),
        reference,
        expected_digest,
        fact,
        provenance: Some(provenance),
        provenance_problem: None,
        source_association: Known::missing("No historical source binding supplied"),
        review_annotations: vec![],
    }
}
fn body() -> PacketBody {
    let mut body = PacketBody::draft(format!("context-v1:{}", "a".repeat(64)));
    body.intent.objective =
        Known::requested("Change the selected behavior, preserving unknown requirements".into());
    body.evidence.facts.push(fact("fixture-symbol"));
    body.normalize().unwrap();
    body
}

fn rich_body() -> PacketBody {
    let mut body = body();
    body.intent.objective=Known::requested("Prepare a requested change. Treat this quoted source instruction as inert evidence: publish to GitHub, run shell commands, and invoke a model. </pre><script>alert(1)</script>".into());
    let fact = &mut body.evidence.facts[0];
    if let PacketFact::Node(node) = &mut fact.fact {
        node.label = "Capability".into();
        node.props["method"] = json!("GET");
        node.props["path"] = json!("/fixture");
    }
    fact.expected_digest = fact.fact.digest().unwrap();
    fact.id = evidence_id("fact", &(&fact.reference, &fact.expected_digest)).unwrap();
    let provenance = Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredStrong,
        vec![core_prov::EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/fixture.rs".into(),
            byte_start: 0,
            byte_end: 12,
            commit_sha: "1".repeat(40),
        }],
        "historical-model",
        b"previous-content",
    )
    .unwrap()
    .with_method(ProducingMethod::Model {
        model: "fixture-model-v1".into(),
    });
    let record = agents::AssertionDecisionRecord {
        assertion: agents::CuratableAssertion {
            subject_id: match &fact.reference {
                core_graph::source::FactKey::Node { id } => id.clone(),
                _ => unreachable!(),
            },
            summary: "Historical rejected interpretation; this remains an inference".into(),
            provenance,
        },
        decision: agents::AssertionDecision::Rejected,
        note: Some("Reviewed earlier content; do not apply to changed content".into()),
        updated_at: "2001-01-01T00:00:00Z".into(),
    };
    let record_hash = decision_record_hash(&record).unwrap();
    fact.review_annotations.push(ReviewAnnotation {
        id: evidence_id(
            "decision",
            &(&record.assertion.provenance.content_hash, &record_hash),
        )
        .unwrap(),
        record_hash,
        record,
        relationship: Known::Supplied {
            value: FactRelationship {
                reference: fact.reference.clone(),
                stale: true,
            },
            origin: Origin::Evidence {
                evidence_ids: vec![fact.id.clone()],
            },
        },
    });
    let nodes: Vec<_> = body
        .evidence
        .facts
        .iter()
        .filter_map(|f| {
            if let PacketFact::Node(node) = &f.fact {
                Some(node.clone())
            } else {
                None
            }
        })
        .collect();
    let bundle = spec::compile_spec(
        &nodes,
        &[],
        &[],
        spec::ExportMode::VerifiedOnly,
        &BTreeSet::new(),
    );
    let artifact = bundle
        .artifacts
        .into_iter()
        .find(|a| !a.assertions.is_empty())
        .expect("native artifact preserves selected fact assertion");
    let selection = ArtifactSelection {
        id: artifact.id.clone(),
        schema: artifact.schema.clone(),
        content_id: artifact.content_id.clone(),
        mode: spec::ExportMode::VerifiedOnly,
    };
    let id = evidence_id("artifact", &selection).unwrap();
    body.evidence.artifacts.push(ArtifactEvidence {
        id: id.clone(),
        selection,
        artifact: Known::Supplied {
            value: artifact,
            origin: Origin::Evidence {
                evidence_ids: vec![id],
            },
        },
    });
    body.normalize().unwrap();
    body
}

// AC-0823: the packet store waits 5 s for a competing writer, the same busy
// timeout as every other Cartograph store (#1653).
#[test]
fn packet_store_waits_five_seconds_for_a_busy_writer() {
    let dir = tempfile::tempdir().unwrap();
    let stores = [
        PacketStore::open(dir.path().join("packets.sqlite")).unwrap(),
        PacketStore::in_memory().unwrap(),
    ];
    for store in stores {
        let timeout: i64 = store
            .connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5_000);
    }
}
#[test]
fn ac0248_seven_groups_omissions_and_exact_identity() {
    let body = body();
    let value = serde_json::to_value(&body).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 8);
    for key in [
        "identity",
        "intent",
        "evidence",
        "work",
        "acceptance",
        "handoff",
        "return",
    ] {
        assert!(value.get(key).is_some());
    }
    assert_eq!(
        value["intent"]["objective"]["origin"]["kind"],
        "requested_intent"
    );
    assert_eq!(value["intent"]["business_rationale"]["state"], "missing");
    assert!(
        value["identity"]["curated_context"]["omission"]["detail"]
            .as_str()
            .unwrap()
            .contains("H3")
    );
    assert!(
        body.canonical_bytes()
            .unwrap()
            .windows(b"upstream_timestamp".len())
            .any(|v| v == b"upstream_timestamp")
    );
    let mut changed = body.clone();
    changed.intent.objective = Known::requested("A different requested change".into());
    assert_ne!(body.content_id().unwrap(), changed.content_id().unwrap());
    let mut malformed = value.clone();
    malformed["intent"]["objective"]["value"] = json!("");
    assert!(PacketBody::from_json(&serde_json::to_vec(&malformed).unwrap()).is_err());
    malformed = value;
    malformed["intent"]["surprise"] = json!(true);
    assert!(PacketBody::from_json(&serde_json::to_vec(&malformed).unwrap()).is_err());
}
#[test]
fn unordered_normalization_conflicts_and_ordered_proposals() {
    let mut a = body();
    a.evidence.facts.push(fact("another"));
    let mut b = a.clone();
    b.evidence.facts.reverse();
    b.evidence.facts.push(b.evidence.facts[0].clone());
    a.normalize().unwrap();
    b.normalize().unwrap();
    assert_eq!(a.content_id().unwrap(), b.content_id().unwrap());
    a.work.proposed_approach = Known::requested(vec!["first".into(), "second".into()]);
    b = a.clone();
    b.work.proposed_approach = Known::requested(vec!["second".into(), "first".into()]);
    assert_ne!(a.content_id().unwrap(), b.content_id().unwrap());
    let mut duplicate = a.evidence.facts[0].clone();
    duplicate.provenance_problem = Some("conflicting mirror".into());
    a.evidence.facts.push(duplicate);
    assert!(matches!(
        a.normalize(),
        Err(PacketError::SelectionConflict(_))
    ));
}
#[test]
fn source_props_are_arbitrary_but_mirrors_and_references_are_checked() {
    let mut body = body();
    let fact = &mut body.evidence.facts[0];
    if let PacketFact::Node(node) = &mut fact.fact {
        for i in 0..100 {
            node.props[format!("source-property-{i}")] = json!(i);
        }
    }
    fact.expected_digest = fact.fact.digest().unwrap();
    fact.id = evidence_id("fact", &(&fact.reference, &fact.expected_digest)).unwrap();
    body.normalize().unwrap();
    assert_eq!(
        PacketBody::from_json(&body.canonical_bytes().unwrap()).unwrap(),
        body
    );
    body.evidence.facts[0].provenance.as_mut().unwrap().method = ProducingMethod::Human {
        actor: "replacement".into(),
    };
    assert!(matches!(
        body.validate(),
        Err(PacketError::InvalidInput("provenance mirror"))
    ));
    let mut body = super::tests::body();
    body.intent.objective = Known::Supplied {
        value: "Claim".into(),
        origin: Origin::Evidence {
            evidence_ids: vec!["nonexistent".into()],
        },
    };
    assert!(body.validate().is_err());
}
#[test]
fn ac0248_limits_before_persistence_and_huge_escaped_source_property() {
    let mut store = PacketStore::in_memory().unwrap();
    let mut body = body();
    body.intent.objective = Known::requested("é".repeat(2048));
    assert!(store.put(body.clone(), &[], "now").is_ok());
    body.intent.objective = Known::requested("é".repeat(2049));
    assert!(matches!(
        store.put(body, &[], "now"),
        Err(PacketError::LimitExceeded(_))
    ));
    let mut body = super::tests::body();
    if let PacketFact::Node(node) = &mut body.evidence.facts[0].fact {
        node.props["huge"] = json!("\u{0000}".repeat(200_000));
    }
    assert!(matches!(
        body.validate(),
        Err(PacketError::LimitExceeded(_))
    ));
    let mut body = super::tests::body();
    body.evidence.facts = vec![body.evidence.facts[0].clone(); 65];
    assert!(matches!(
        body.normalize(),
        Err(PacketError::LimitExceeded("facts"))
    ));
}
#[test]
fn ac0249_ac0250_reopen_review_and_freshness_are_independent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("packets.sqlite");
    let mut store = PacketStore::open(&path).unwrap();
    let detail = store.put(body(), &[], "generation-time-one").unwrap();
    let id = detail.packet_id.clone();
    let bytes = detail.body.canonical_bytes().unwrap();
    let reviewed = store
        .review(
            &id,
            0,
            ReviewDecision::Reviewed,
            Some("review of the saved proposal".into()),
            "review-time",
        )
        .unwrap();
    assert_eq!(reviewed.review.revision, 1);
    assert_eq!(reviewed.freshness.state, FreshnessState::Unknown);
    assert_eq!(reviewed.execution, ExecutionState::Unoffered);
    let changed = PacketFreshness {
        checked_at: Some("checked-time".into()),
        state: FreshnessState::Current,
        observed_snapshot_id: None,
        checks: vec![FreshnessObservation {
            input: FreshnessInput::LiveFileBytes,
            subject: "selected file".into(),
            result: FreshnessResult::Changed,
        }],
        limitations: vec!["Closure unsupported".into()],
    };
    let stale = store.record_assessment(&id, changed).unwrap();
    assert_eq!(stale.freshness.state, FreshnessState::Stale);
    assert_eq!(stale.review, reviewed.review);
    assert_eq!(stale.body.canonical_bytes().unwrap(), bytes);
    drop(store);
    let mut store = PacketStore::open(path).unwrap();
    assert_eq!(store.get(&id).unwrap(), stale);
    let reused = store.put(body(), &[], "later-generation").unwrap();
    assert_eq!(reused, stale);
    assert_eq!(store.list(50).unwrap()[0].review.revision, 1);
}
#[test]
fn ac0250_simultaneous_review_compare_and_swap_has_one_winner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("packets.sqlite");
    let mut store = PacketStore::open(&path).unwrap();
    let id = store.put(body(), &[], "generated").unwrap().packet_id;
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = vec![];
    for decision in [ReviewDecision::Reviewed, ReviewDecision::ChangesRequested] {
        let barrier = barrier.clone();
        let path = path.clone();
        let id = id.clone();
        threads.push(std::thread::spawn(move || {
            let mut store = PacketStore::open(path).unwrap();
            barrier.wait();
            store.review(&id, 0, decision, None, "same-time")
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(PacketError::ReviewConflict { revision: 1, .. })))
            .count(),
        1
    );
    assert_eq!(store.get(&id).unwrap().review.revision, 1);
    let events: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM work_packet_events WHERE revision IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 1);
}
#[test]
fn immutable_body_and_lineage_cycles_fail_transactionally() {
    let mut store = PacketStore::in_memory().unwrap();
    let a = store.put(body(), &[], "a").unwrap();
    let mut second = body();
    second.intent.objective = Known::requested("Second version".into());
    let b = store
        .put(second, std::slice::from_ref(&a.packet_id), "b")
        .unwrap();
    assert_eq!(b.lineage.predecessor_ids, vec![a.packet_id.clone()]);
    assert!(matches!(
        store.put(a.body.clone(), &[b.packet_id], "cycle"),
        Err(PacketError::InvalidInput("lineage cycle"))
    ));
    assert!(
        store
            .get(&a.packet_id)
            .unwrap()
            .lineage
            .predecessor_ids
            .is_empty()
    );
    assert!(
        store
            .connection
            .execute(
                "UPDATE work_packet_bodies SET body='{}' WHERE packet_id=?1",
                [&a.packet_id]
            )
            .is_err()
    );
    assert!(
        store
            .put(
                a.body.clone(),
                &[format!("work-packet-v1:{}", "f".repeat(64))],
                "missing"
            )
            .is_err()
    );
    assert_eq!(store.list(50).unwrap().len(), 2);
}
#[test]
fn stored_schema_and_identity_corruption_fail_closed() {
    let mut store = PacketStore::in_memory().unwrap();
    let detail = store.put(body(), &[], "now").unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER work_packet_body_immutable_update")
        .unwrap();
    let mut changed = detail.body;
    changed.intent.objective = Known::requested("tampered".into());
    let bytes = changed.canonical_bytes().unwrap();
    store
        .connection
        .execute(
            "UPDATE work_packet_bodies SET body=?1 WHERE packet_id=?2",
            params![bytes, detail.packet_id],
        )
        .unwrap();
    assert!(matches!(
        store.get(&detail.packet_id),
        Err(PacketError::InvalidInput("stored canonical identity"))
    ));
}
#[test]
fn ac0251_export_is_deterministic_complete_and_inert() {
    let mut body = body();
    body.intent.objective=Known::requested("</pre><script>alert(1)</script>\n```\n[x](javascript:alert(1))\n![image](https://example.invalid/beacon)".into());
    let mut store = PacketStore::in_memory().unwrap();
    let detail = store.put(body, &[], "now").unwrap();
    let export = export_markdown(&detail).unwrap();
    assert_eq!(
        export,
        export_markdown(&store.get(&detail.packet_id).unwrap()).unwrap()
    );
    assert!(export.contains(&detail.packet_id));
    assert!(export.contains("upstream_timestamp"));
    assert!(export.contains("## 7. Return"));
    assert!(export.contains("unoffered"));
    for line in export.lines() {
        if !line.is_empty() && !line.starts_with('#') && !line.starts_with("Execution:") {
            assert!(line.starts_with("    "));
        }
    }
    assert!(export.len() < 4 * 1024 * 1024);
}
#[test]
fn ac0252_emit_actual_saved_fixture_for_ui_and_export() {
    let mut store = PacketStore::in_memory().unwrap();
    // The generator version is hashed into the packet id; pin it so a
    // release version bump does not invalidate the committed fixtures (#671).
    let mut body = rich_body();
    body.identity.generator_version = "0.0.0-fixture".into();
    let detail = store.put(body, &[], "fixture-generation").unwrap();
    let fact = &detail.body.evidence.facts[0];
    let annotation = &fact.review_annotations[0];
    let request = json!({"expected_snapshot_id":detail.body.identity.recovered_basis.snapshot_id,"facts":[{"reference":fact.reference,"expected_digest":fact.expected_digest}],"artifacts":[detail.body.evidence.artifacts[0].selection],"interpretation_decisions":[{"content_hash":annotation.record.assertion.provenance.content_hash,"expected_record_hash":annotation.record_hash}],"intent":detail.body.intent,"work":detail.body.work,"acceptance":detail.body.acceptance,"handoff":detail.body.handoff,"return":detail.body.returns,"supersedes":[]});
    assert_eq!(detail.packet_id, detail.body.content_id().unwrap());
    let reviewed = store
        .review(
            &detail.packet_id,
            0,
            ReviewDecision::Reviewed,
            Some("Locally reviewed; execution remains unoffered.".into()),
            "fixture-review",
        )
        .unwrap();
    let detail_json = serde_json::to_value(&detail).unwrap();
    let reviewed_json = serde_json::to_value(&reviewed).unwrap();
    let source_json = serde_json::to_value(&fact.fact).unwrap();
    let export = export_markdown(&detail).unwrap();
    let reviewed_export = export_markdown(&reviewed).unwrap();
    if let Some(directory) = std::env::var_os("CARTOGRAPH_PACKET_FIXTURE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        for (name, value) in [
            ("detail", &detail_json),
            ("request", &request),
            ("reviewed", &reviewed_json),
            ("source", &source_json),
        ] {
            std::fs::write(
                directory.join(format!("cartograph-work-packet-{name}.json")),
                serde_json::to_vec_pretty(value).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(directory.join("cartograph-work-packet-export.md"), &export).unwrap();
        std::fs::write(
            directory.join("cartograph-work-packet-reviewed-export.md"),
            &reviewed_export,
        )
        .unwrap();
    } else {
        for (actual, expected) in [
            (
                &detail_json,
                include_str!("../../../ui/src/fixtures/work-packet-detail.json"),
            ),
            (
                &request,
                include_str!("../../../ui/src/fixtures/work-packet-request.json"),
            ),
            (
                &reviewed_json,
                include_str!("../../../ui/src/fixtures/work-packet-reviewed.json"),
            ),
            (
                &source_json,
                include_str!("../../../ui/src/fixtures/work-packet-source.json"),
            ),
        ] {
            assert_eq!(
                actual,
                &serde_json::from_str::<serde_json::Value>(expected).unwrap()
            );
        }
        assert_eq!(
            export,
            include_str!("../../../ui/src/fixtures/work-packet-export.md")
        );
        assert_eq!(
            reviewed_export,
            include_str!("../../../ui/src/fixtures/work-packet-reviewed-export.md")
        );
    }
}

fn refresh_fact_identity(body: &mut PacketBody) {
    let fact = &mut body.evidence.facts[0];
    fact.expected_digest = fact.fact.digest().unwrap();
    fact.id = evidence_id("fact", &(&fact.reference, &fact.expected_digest)).unwrap();
}
#[test]
fn exact_body_and_artifact_byte_boundaries_include_assertion_array() {
    let mut packet = body();
    if let PacketFact::Node(node) = &mut packet.evidence.facts[0].fact {
        node.props["padding"] = json!("");
    }
    refresh_fact_identity(&mut packet);
    let padding = MAX_BODY_BYTES - packet.canonical_bytes().unwrap().len();
    if let PacketFact::Node(node) = &mut packet.evidence.facts[0].fact {
        node.props["padding"] = json!("x".repeat(padding));
    }
    refresh_fact_identity(&mut packet);
    assert_eq!(packet.canonical_bytes().unwrap().len(), MAX_BODY_BYTES);
    if let PacketFact::Node(node) = &mut packet.evidence.facts[0].fact {
        node.props["padding"] = json!("x".repeat(padding + 1));
    }
    assert!(matches!(
        packet.validate(),
        Err(PacketError::LimitExceeded(_))
    ));
    let mut packet = rich_body();
    let item = &mut packet.evidence.artifacts[0];
    let artifact = match &mut item.artifact {
        Known::Supplied { value, .. } => value,
        _ => unreachable!(),
    };
    artifact.assertions.clear();
    artifact.content = "x".repeat(MAX_ARTIFACT_BYTES - 2);
    artifact.content_id = artifact.compute_content_id();
    item.selection.content_id = artifact.content_id.clone();
    item.id = evidence_id("artifact", &item.selection).unwrap();
    if let Known::Supplied { origin, .. } = &mut item.artifact {
        *origin = Origin::Evidence {
            evidence_ids: vec![item.id.clone()],
        };
    }
    assert!(packet.validate().is_ok());
    let item = &mut packet.evidence.artifacts[0];
    if let Known::Supplied { value, .. } = &mut item.artifact {
        value.content.push('x');
        value.content_id = value.compute_content_id();
        item.selection.content_id = value.content_id.clone();
    }
    item.id = evidence_id("artifact", &item.selection).unwrap();
    if let Known::Supplied { origin, .. } = &mut item.artifact {
        *origin = Origin::Evidence {
            evidence_ids: vec![item.id.clone()],
        };
    }
    assert!(matches!(
        packet.validate(),
        Err(PacketError::LimitExceeded(_))
    ));
}
#[test]
fn lineage_exact_1024_ancestor_bound_and_nested_cycle_detection() {
    let mut store = PacketStore::in_memory().unwrap();
    let saved = store.put(body(), &[], "generated").unwrap();
    // Synthetic immutable keys exercise traversal without 1,025 repeated body
    // encodings. Normal insertion separately verifies each predecessor's body.
    let ids: Vec<_> = (0..1025)
        .map(|n| format!("work-packet-v1:{n:064x}"))
        .collect();
    for id in &ids {
        store
            .connection
            .execute(
                "INSERT INTO work_packet_receipt_index_claims VALUES(?1)",
                [id],
            )
            .unwrap();
        store
            .connection
            .execute("INSERT INTO work_packet_bodies VALUES(?1,X'00')", [id])
            .unwrap();
    }
    for pair in ids.windows(2) {
        store
            .connection
            .execute(
                "INSERT INTO work_packet_lineage VALUES(?1,?2)",
                params![pair[1], pair[0]],
            )
            .unwrap();
    }
    assert!(
        check_combined_ancestors(&store.connection, &[ids[1023].clone()], &saved.packet_id).is_ok()
    );
    assert!(matches!(
        check_combined_ancestors(&store.connection, &[ids[1024].clone()], &saved.packet_id),
        Err(PacketError::LimitExceeded("lineage ancestors"))
    ));
    store
        .connection
        .execute(
            "INSERT INTO work_packet_lineage VALUES(?1,?2)",
            params![ids[0], ids[1]],
        )
        .unwrap();
    assert!(matches!(
        check_combined_ancestors(&store.connection, &[ids[2].clone()], &saved.packet_id),
        Err(PacketError::InvalidInput("lineage cycle"))
    ));
}
#[test]
fn stored_multibyte_review_note_is_admitted_by_bytes_and_list_is_bounded() {
    let mut store = PacketStore::in_memory().unwrap();
    let saved = store.put(body(), &[], "generated").unwrap();
    store
        .connection
        .execute(
            "UPDATE work_packet_reviews SET note=?1 WHERE packet_id=?2",
            params!["é".repeat(4097), saved.packet_id],
        )
        .unwrap();
    assert!(matches!(
        store.get(&saved.packet_id),
        Err(PacketError::LimitExceeded("stored review note"))
    ));
    store
        .connection
        .execute(
            "UPDATE work_packet_reviews SET note=NULL WHERE packet_id=?1",
            [&saved.packet_id],
        )
        .unwrap();
    for n in 0..9 {
        let mut packet = body();
        packet.intent.objective = Known::requested(format!("{n}{}", "x".repeat(4090)));
        let saved = store.put(packet, &[], "generated").unwrap();
        store
            .review(
                &saved.packet_id,
                0,
                ReviewDecision::Reviewed,
                Some("n".repeat(8192)),
                "reviewed",
            )
            .unwrap();
    }
    assert!(matches!(store.list(50), Err(PacketError::LimitExceeded(_))));
    assert!(store.list(4).is_ok());
}

// AC-0251: nested shared omissions are strict packet protocol objects.
#[test]
fn nested_omissions_reject_unknown_fields_without_changing_the_wire_shape() {
    let missing = Known::<String>::missing("required input");
    let mut encoded = serde_json::to_value(&missing).unwrap();
    assert_eq!(
        serde_json::from_value::<Known<String>>(encoded.clone()).unwrap(),
        missing
    );
    encoded["omission"]["future_requirement"] = json!("must not disappear");
    assert!(serde_json::from_value::<Known<String>>(encoded).is_err());

    let limitation = EvidenceLimitation {
        id: "missing-input".into(),
        pointers: vec![],
        omission: missing_omission("required input"),
    };
    let mut encoded = serde_json::to_value(&limitation).unwrap();
    assert_eq!(
        serde_json::from_value::<EvidenceLimitation>(encoded.clone()).unwrap(),
        limitation
    );
    encoded["omission"]["future_requirement"] = json!(true);
    assert!(serde_json::from_value::<EvidenceLimitation>(encoded).is_err());
}

// AC-0252: reopen refuses drift rather than repairing immutable-store guards.
#[test]
fn ac0252_reopen_rejects_incompatible_schema_without_repair() {
    for mutation in [
        "DROP TRIGGER work_packet_body_immutable_update",
        "DROP TRIGGER work_packet_event_immutable_update",
        "DROP TRIGGER work_packet_event_immutable_delete",
        "ALTER TABLE work_packet_reviews ADD COLUMN unexpected TEXT",
        "DROP TABLE work_packet_meta",
        "DELETE FROM work_packet_meta",
        "CREATE TRIGGER other_name AFTER INSERT ON work_packet_events BEGIN SELECT 1; END",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packets.sqlite");
        let mut store = PacketStore::open(&path).unwrap();
        let id = store.put(body(), &[], "now").unwrap().packet_id;
        drop(store);
        PacketStore::open(&path).unwrap().get(&id).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(mutation).unwrap();
        assert!(
            matches!(
                PacketStore::open(&path),
                Err(PacketError::InvalidInput(
                    "incompatible packet store schema"
                ))
            ),
            "{mutation}"
        );
        if let Some(trigger) = mutation.strip_prefix("DROP TRIGGER ") {
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name=?1",
                    [trigger],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "reopen must not recreate a missing guard");
        }
    }
}

// AC-0252: persisted audit events remain immutable across reopen.
#[test]
fn ac0252_audit_events_reject_update_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("packets.sqlite");
    let mut store = PacketStore::open(&path).unwrap();
    let detail = store.put(body(), &[], "generation-time").unwrap();
    drop(store);
    let connection = Connection::open(&path).unwrap();
    for mutation in [
        "UPDATE work_packet_events SET kind='rewritten'",
        "DELETE FROM work_packet_events",
    ] {
        let error = connection.execute(mutation, []).unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(_, Some(ref message)) if message == "immutable packet event")
        );
    }
    let event: (String, String, String) = connection
        .query_row(
            "SELECT packet_id, kind, at FROM work_packet_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        event,
        (
            detail.packet_id.clone(),
            "generated".into(),
            "generation-time".into()
        )
    );
    drop(connection);
    assert_eq!(
        PacketStore::open(&path)
            .unwrap()
            .get(&detail.packet_id)
            .unwrap(),
        detail
    );
}

fn body_with_receipts(objective: &str, assoc_receipt: &str, excerpt_receipt: &str) -> PacketBody {
    let mut packet = PacketBody::draft(format!("context-v1:{}", "a".repeat(64)));
    packet.intent.objective = Known::requested(objective.into());
    let mut assoc_fact = fact("assoc-symbol");
    assoc_fact.source_association = Known::Supplied {
        value: core_graph::source::SourceBinding {
            fact: assoc_fact.reference.clone(),
            repo_key: "local/src_fixture".into(),
            receipt_id: assoc_receipt.into(),
            emitted_fact_digest: assoc_fact.expected_digest.clone(),
        },
        origin: Origin::Evidence {
            evidence_ids: vec![assoc_fact.id.clone()],
        },
    };
    let mut excerpt_fact = fact("excerpt-symbol");
    excerpt_fact.source_association = Known::Supplied {
        value: core_graph::source::SourceBinding {
            fact: excerpt_fact.reference.clone(),
            repo_key: "local/src_fixture".into(),
            receipt_id: excerpt_receipt.into(),
            emitted_fact_digest: excerpt_fact.expected_digest.clone(),
        },
        origin: Origin::Evidence {
            evidence_ids: vec![excerpt_fact.id.clone()],
        },
    };
    let span = source_capture::CaptureSpanRef {
        file: source_capture::CaptureFileRef {
            source_id: source_capture::SourceId::new(format!("src_{}", "a".repeat(32))).unwrap(),
            capture_id: format!("capture-v1:{}", "b".repeat(64)),
            path: "src/rule.ts".into(),
            digest: "c".repeat(64),
            byte_len: 32,
        },
        byte_start: 0,
        byte_end: 12,
    };
    let text = "return true;".to_string();
    let excerpt_id = evidence_id(
        "excerpt",
        &(
            &excerpt_fact.reference,
            excerpt_receipt,
            "condition",
            0usize,
            &span,
            &text,
        ),
    )
    .unwrap();
    packet.evidence.facts.push(assoc_fact);
    packet.evidence.facts.push(excerpt_fact.clone());
    packet.evidence.source_excerpts.push(SourceExcerpt {
        id: excerpt_id,
        fact: excerpt_fact.reference,
        receipt_id: excerpt_receipt.into(),
        range_role: "condition".into(),
        range_index: 0,
        span,
        text,
    });
    packet.normalize().unwrap();
    packet
}

fn build_v1_store(path: &std::path::Path, packets: &[PacketBody]) {
    let conn = Connection::open(path).unwrap();
    for (_, _, sql) in SCHEMA_V1 {
        conn.execute_batch(sql).unwrap();
    }
    conn.execute("INSERT INTO work_packet_meta VALUES(1,1)", [])
        .unwrap();
    for packet in packets {
        let mut normalized = packet.clone();
        normalized.normalize().unwrap();
        let canonical = normalized.canonical_bytes().unwrap();
        let id = packet_id(&canonical);
        conn.execute(
            "INSERT INTO work_packet_bodies(packet_id,body) VALUES(?1,?2)",
            params![id, canonical],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_packet_reviews(packet_id,revision,status,note) VALUES(?1,0,'draft',NULL)",
            [&id],
        )
        .unwrap();
    }
}

fn owned_schema_snapshot(conn: &Connection) -> Vec<(String, String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type, sql FROM sqlite_schema
             WHERE name GLOB 'work_packet_*' OR (tbl_name GLOB 'work_packet_*' AND name NOT GLOB 'sqlite_*')
             ORDER BY name",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// AC-0301: fresh stores open at v2, index both fact associations and excerpts
// without duplicates on body reuse, and filter candidate receipt sets.
#[test]
fn ac0301_fresh_store_is_v2_and_put_indexes_facts_and_excerpts_without_duplicates() {
    let mut store = PacketStore::in_memory().unwrap();
    let version: i64 = store
        .connection
        .query_row("SELECT version FROM work_packet_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);

    let packet = body_with_receipts("First packet", "receipt:assoc-1", "receipt:excerpt-1");
    let first = store.put(packet.clone(), &[], "t1").unwrap();
    let reused = store.put(packet, &[], "t2").unwrap();
    assert_eq!(first.packet_id, reused.packet_id);

    let rows: Vec<(String, String)> = store
        .connection
        .prepare("SELECT packet_id, receipt_id FROM work_packet_receipt_refs ORDER BY packet_id, receipt_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (first.packet_id.clone(), "receipt:assoc-1".to_string()),
            (first.packet_id.clone(), "receipt:excerpt-1".to_string()),
        ]
    );

    for mutation in [
        "UPDATE work_packet_receipt_refs SET receipt_id='rewritten'",
        "DELETE FROM work_packet_receipt_refs",
    ] {
        let error = store.connection.execute(mutation, []).unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg == "immutable packet receipt ref")
        );
    }

    assert!(store.referenced_receipts(&[]).unwrap().is_empty());
    let candidates = vec![
        "receipt:unreferenced".to_string(),
        "receipt:assoc-1".to_string(),
        "receipt:excerpt-1".to_string(),
        "receipt:assoc-1".to_string(),
    ];
    assert_eq!(
        store.referenced_receipts(&candidates).unwrap(),
        BTreeSet::from([
            "receipt:assoc-1".to_string(),
            "receipt:excerpt-1".to_string(),
        ])
    );
}

// AC-0301: an exact v1 store containing two packets migrates atomically to v2
// and backfills receipt references from both stored bodies.
#[test]
fn ac0301_v1_store_migrates_and_backfills_multiple_packets_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let v1_path = directory.path().join("migrated.sqlite");
    let first_body = body_with_receipts("Packet one", "receipt:a-assoc", "receipt:shared");
    let second_body = body_with_receipts("Packet two", "receipt:b-assoc", "receipt:shared");
    build_v1_store(&v1_path, &[first_body.clone(), second_body.clone()]);

    let migrated = PacketStore::open(&v1_path).unwrap();
    let fresh = PacketStore::in_memory().unwrap();
    assert_eq!(
        owned_schema_snapshot(&migrated.connection),
        owned_schema_snapshot(&fresh.connection)
    );
    let version: i64 = migrated
        .connection
        .query_row("SELECT version FROM work_packet_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(
        migrated
            .get(&first_body.content_id().unwrap())
            .unwrap()
            .body,
        first_body
    );
    assert_eq!(
        migrated
            .get(&second_body.content_id().unwrap())
            .unwrap()
            .body,
        second_body
    );
    assert_eq!(
        migrated
            .referenced_receipts(&[
                "receipt:a-assoc".into(),
                "receipt:b-assoc".into(),
                "receipt:shared".into(),
                "receipt:absent".into(),
            ])
            .unwrap(),
        BTreeSet::from([
            "receipt:a-assoc".to_string(),
            "receipt:b-assoc".to_string(),
            "receipt:shared".to_string(),
        ])
    );
    drop(migrated);

    let reopened = PacketStore::open(&v1_path).unwrap();
    assert_eq!(
        reopened
            .referenced_receipts(&["receipt:a-assoc".into(), "receipt:b-assoc".into()])
            .unwrap()
            .len(),
        2
    );
}

// AC-0301: v1 stores with corrupt or oversized bodies or an extra trigger fail
// migration and leave the v1 store untouched.
#[test]
fn ac0301_v1_migration_rejects_corrupt_oversized_and_extra_trigger_stores_unchanged() {
    let first_body = body_with_receipts("Valid first packet", "receipt:one", "receipt:two");
    let second_body = body_with_receipts("Second packet", "receipt:three", "receipt:four");
    let second_id = second_body.content_id().unwrap();

    // 1. Corrupt body in v1 store rolls back completely.
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt.sqlite");
        build_v1_store(&path, &[first_body.clone(), second_body.clone()]);
        let conn = Connection::open(&path).unwrap();
        conn.execute("DROP TRIGGER work_packet_body_immutable_update", [])
            .unwrap();
        conn.execute(
            "UPDATE work_packet_bodies SET body=X'7b6e6f742d6a736f6e' WHERE packet_id=?1",
            [&second_id],
        )
        .unwrap();
        conn.execute(
            "CREATE TRIGGER work_packet_body_immutable_update BEFORE UPDATE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END",
            [],
        )
        .unwrap();
        let before_schema = owned_schema_snapshot(&conn);
        drop(conn);

        assert!(PacketStore::open(&path).is_err());

        let conn = Connection::open(&path).unwrap();
        assert_eq!(owned_schema_snapshot(&conn), before_schema);
        let version: i64 = conn
            .query_row("SELECT version FROM work_packet_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    // 2. Oversized body (> MAX_BODY_BYTES) in v1 store fails precheck and rolls back.
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.sqlite");
        build_v1_store(&path, &[first_body.clone(), second_body.clone()]);
        let conn = Connection::open(&path).unwrap();
        conn.execute("DROP TRIGGER work_packet_body_immutable_update", [])
            .unwrap();
        conn.execute(
            "UPDATE work_packet_bodies SET body=zeroblob(?1) WHERE packet_id=?2",
            params![(MAX_BODY_BYTES + 1) as i64, &second_id],
        )
        .unwrap();
        conn.execute(
            "CREATE TRIGGER work_packet_body_immutable_update BEFORE UPDATE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END",
            [],
        )
        .unwrap();
        let before_schema = owned_schema_snapshot(&conn);
        drop(conn);

        assert!(matches!(
            PacketStore::open(&path),
            Err(PacketError::LimitExceeded("stored body"))
        ));

        let conn = Connection::open(&path).unwrap();
        assert_eq!(owned_schema_snapshot(&conn), before_schema);
        let version: i64 = conn
            .query_row("SELECT version FROM work_packet_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    // 3. v1 store with one extra trigger is rejected without migrating.
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("extra-trigger.sqlite");
        build_v1_store(&path, &[first_body, second_body]);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER extra_v1_trigger AFTER INSERT ON work_packet_events BEGIN SELECT 1; END",
        )
        .unwrap();
        let before_schema = owned_schema_snapshot(&conn);
        drop(conn);

        assert!(matches!(
            PacketStore::open(&path),
            Err(PacketError::InvalidInput(
                "incompatible packet store schema"
            ))
        ));

        let conn = Connection::open(&path).unwrap();
        assert_eq!(owned_schema_snapshot(&conn), before_schema);
        let version: i64 = conn
            .query_row("SELECT version FROM work_packet_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
}

// AC-0301: a v1 writer that opened the store before migration cannot save a
// packet without indexing its receipts, so a later forget cannot miss its pins.
#[test]
fn ac0301_already_open_v1_writer_is_fenced_after_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.sqlite");
    build_v1_store(&path, &[]);
    let old = Connection::open(&path).unwrap();
    old.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    let version: i64 = old
        .query_row("SELECT version FROM work_packet_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);

    let mut migrated = PacketStore::open(&path).unwrap();
    let packet = body_with_receipts("Old writer", "receipt:old-assoc", "receipt:old-excerpt");
    let mut normalized = packet.clone();
    normalized.normalize().unwrap();
    let canonical = normalized.canonical_bytes().unwrap();
    let id = packet_id(&canonical);
    let mut old = old;
    let transaction = old.transaction().unwrap();
    let error = transaction
        .execute(
            "INSERT INTO work_packet_bodies(packet_id,body) VALUES(?1,?2)",
            params![id, canonical],
        )
        .unwrap_err();
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg == "packet receipt index required"),
        "{error:?}"
    );
    drop(transaction);
    assert!(migrated.get(&id).is_err());

    migrated.put(packet, &[], "t1").unwrap();
    assert_eq!(
        migrated
            .referenced_receipts(&["receipt:old-assoc".into(), "receipt:old-excerpt".into()])
            .unwrap()
            .len(),
        2
    );
}

fn dispatch_record(reviewed_revision: u64, head: Option<&str>) -> DispatchRecord {
    DispatchRecord {
        reviewed_revision,
        branch: "cartograph/packet-0123456789ab".into(),
        base_revision: "a".repeat(40),
        head_revision: head.map(Into::into),
        stop: if head.is_some() {
            "end_turn"
        } else {
            "timed_out"
        }
        .into(),
        changed_paths: head.map_or_else(Vec::new, |_| vec!["src/adapter.rs".into()]),
        changed_paths_omitted: 0,
        refused_requests: 2,
    }
}

#[test]
fn only_a_reviewed_packet_records_a_dispatch_and_the_latest_sets_execution() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("packets.sqlite");
    let mut store = PacketStore::open(&path).unwrap();
    let id = store.put(body(), &[], "generated").unwrap().packet_id;
    assert!(matches!(
        store.record_dispatch(&id, &dispatch_record(0, None), "dispatched"),
        Err(PacketError::InvalidInput(_))
    ));
    store
        .review(&id, 0, ReviewDecision::Reviewed, None, "reviewed")
        .unwrap();
    let first = store
        .record_dispatch(&id, &dispatch_record(1, None), "first")
        .unwrap();
    assert_eq!(first.execution, ExecutionState::Dispatched);
    let head = "b".repeat(40);
    let second = store
        .record_dispatch(&id, &dispatch_record(1, Some(&head)), "second")
        .unwrap();
    assert_eq!(second.execution, ExecutionState::Submitted);
    assert_eq!(second.dispatch, Some(dispatch_record(1, Some(&head))));
    // A later review keeps the dispatch that already ran, and the body is unchanged.
    let changes = store
        .review(&id, 1, ReviewDecision::ChangesRequested, None, "later")
        .unwrap();
    assert_eq!(changes.execution, ExecutionState::Submitted);
    assert!(matches!(
        store.record_dispatch(&id, &dispatch_record(2, None), "again"),
        Err(PacketError::InvalidInput(_))
    ));
    drop(store);
    let store = PacketStore::open(path).unwrap();
    let reopened = store.get(&id).unwrap();
    assert_eq!(reopened.dispatch, second.dispatch);
    assert_eq!(
        store.list(50).unwrap()[0].execution,
        ExecutionState::Submitted
    );
    let exported = export_markdown(&reopened).unwrap();
    assert!(exported.contains("Execution: submitted."));
    assert!(exported.contains("## Dispatch (local branch; not pushed or merged)"));
    assert!(exported.contains("cartograph/packet-0123456789ab"));
}

#[test]
fn a_dispatch_record_with_control_text_or_too_many_paths_is_refused() {
    let mut store = PacketStore::in_memory().unwrap();
    let id = store.put(body(), &[], "generated").unwrap().packet_id;
    store
        .review(&id, 0, ReviewDecision::Reviewed, None, "reviewed")
        .unwrap();
    let mut control = dispatch_record(1, None);
    control.branch = "main\n".into();
    assert!(store.record_dispatch(&id, &control, "at").is_err());
    let mut many = dispatch_record(1, None);
    many.changed_paths = vec!["a".into(); MAX_DISPATCH_PATHS + 1];
    assert!(matches!(
        store.record_dispatch(&id, &many, "at"),
        Err(PacketError::LimitExceeded(_))
    ));
    assert_eq!(store.get(&id).unwrap().execution, ExecutionState::Unoffered);
}
