// AC-0513: captured Capability input reaches the official compiler and identity-bound pages.
use super::overlay_capture::Fixture;
use super::*;
use crate::mcp_reads::KnowledgeBasis;
use core_graph::overlay::{BaseRef, OverlayItem};
use core_prov::Tier;
use std::collections::BTreeSet;

fn stories(session: &ReadSession, limits: spec::SelectedCompileLimits) -> CompiledArtifact {
    match session
        .compile_artifact("user-stories", trace_limits(), limits)
        .unwrap()
    {
        ArtifactCompilation::Produced(a) => a,
        ArtifactCompilation::Limited { .. } => panic!("fixture fits"),
    }
}

#[test]
fn official_stories_preserve_producer_support_provenance_and_old_pages() {
    let f = Fixture::new();
    let before = f.base();
    let absent = f.best();
    let flows = flowtracer::trace(&before.graph.0, &before.graph.1);
    let spec::SelectedArtifactOutcome::Produced(legacy) = spec::compile_spec_selected(
        &before.graph.0,
        &before.graph.1,
        &flows,
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &["user-stories"],
        compile_limits(),
    )
    .unwrap()
    .remove(0) else {
        panic!("legacy fits")
    };
    assert_eq!(stories(&absent, compile_limits()).artifact, legacy);
    let no_store = ReadSession::acquire(
        &f.graph,
        &f.decisions,
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    )
    .unwrap();
    assert_eq!(stories(&no_store, compile_limits()).artifact, legacy);
    f.publish(f.facts.clone());
    let current = f.best();
    assert!(matches!(
        current.basis.knowledge,
        KnowledgeBasis::Current(_)
    ));
    assert_eq!(
        current.basis.recovered_graph_id,
        absent.basis.recovered_graph_id
    );
    let artifact = stories(&current, compile_limits());
    let OverlayItem::Node(capability) = &f.facts[0].item else {
        unreachable!()
    };
    assert_eq!(
        artifact.artifact.assertions.len(),
        1 + f.facts[0].cites.len()
    );
    assert!(artifact.artifact.verify_identity());
    let spec::SelectedArtifactOutcome::Produced(official) = spec::compile_capability_user_stories(
        &before,
        &[spec::CapabilityStory {
            capability,
            cites: &f.facts[0].cites,
        }],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        compile_limits(),
    )
    .unwrap() else {
        panic!("fixture fits")
    };
    assert_eq!(artifact.artifact, official);
    let candidate_row = artifact
        .artifact
        .assertions
        .iter()
        .find(|a| a.id == format!("node:{}", capability.id))
        .unwrap();
    assert_eq!(
        candidate_row.provenance,
        serde_json::from_value::<Provenance>(capability.props["prov"].clone()).unwrap()
    );
    assert_eq!(artifact.basis_id(), current.basis_id().unwrap());
    for descriptor in current
        .artifact_catalog()
        .into_iter()
        .filter(|a| a.id != "user-stories")
    {
        let ArtifactCompilation::Produced(actual) = current
            .compile_artifact(descriptor.id, trace_limits(), compile_limits())
            .unwrap()
        else {
            panic!("fixture fits")
        };
        let legacy = spec::compile_spec_selected(
            &before.graph.0,
            &before.graph.1,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &[descriptor.id],
            compile_limits(),
        )
        .unwrap()
        .remove(0);
        let spec::SelectedArtifactOutcome::Produced(legacy) = legacy else {
            panic!("fixture fits")
        };
        assert_eq!(actual.artifact, legacy, "{}", descriptor.id);
    }
    for cite in &f.facts[0].cites {
        let props = match cite {
            BaseRef::Node(id) => &before.graph.0.iter().find(|n| &n.id == id).unwrap().props,
            BaseRef::Edge(src, dst, label) => {
                &before
                    .graph
                    .1
                    .iter()
                    .find(|e| (&e.src, &e.dst, &e.label) == (src, dst, label))
                    .unwrap()
                    .props
            }
        };
        let mut encoded = b"capability-story-support-v1\0".to_vec();
        encoded.extend(serde_json::to_vec(&(&capability.id, cite)).unwrap());
        let id = format!("support:{}", core_prov::content_hash(&encoded));
        let row = artifact
            .artifact
            .assertions
            .iter()
            .find(|a| a.id == id)
            .unwrap();
        assert_eq!(
            row.provenance,
            serde_json::from_value::<Provenance>(props["prov"].clone()).unwrap()
        );
        assert!(
            artifact
                .artifact
                .content
                .contains(&serde_json::to_string(&(id, cite)).unwrap())
        );
    }
    let old_page = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            0,
            cap(1 << 20),
            cap(1 << 21),
        )
        .unwrap()
        .content
        .to_owned();
    assert_eq!(old_page, artifact.artifact.content);
    assert_ne!(artifact.artifact, legacy);
    let ptr = current.base.graph.0.as_ptr();
    let mut changed = f.facts.clone();
    let OverlayItem::Node(n) = &mut changed[0].item else {
        unreachable!()
    };
    n.props["title"] = json!("new knowledge result");
    n.props["prov"]["content_hash"] = json!(core_prov::content_hash(b"new knowledge result"));
    f.publish(changed);
    let next = f.best();
    assert_ne!(next.basis_id().unwrap(), current.basis_id().unwrap());
    assert_ne!(
        stories(&next, compile_limits()).content_id(),
        artifact.content_id()
    );
    assert_eq!(
        stories(&current, compile_limits()).artifact,
        artifact.artifact
    );
    assert_eq!(current.base.graph.0.as_ptr(), ptr);
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                0,
                cap(1 << 20),
                cap(1 << 21)
            )
            .unwrap()
            .content,
        old_page
    );
    assert_eq!(
        artifact
            .page(
                &next.basis_id().unwrap(),
                artifact.content_id(),
                0,
                cap(1 << 20),
                cap(1 << 21)
            )
            .unwrap_err(),
        crate::mcp_reads::ReadError::StaleBasis
    );
    assert_eq!(f.base(), before);
}

#[test]
fn captured_modes_staleness_rejection_and_invalid_candidates_fail_closed() {
    let f = Fixture::new();
    f.publish(f.facts.clone());
    let verified = f
        .acquire(ExportMode::VerifiedOnly, SnapshotReadLimits::default())
        .unwrap();
    assert!(
        stories(&verified, compile_limits())
            .artifact
            .assertions
            .is_empty()
    );
    let verified_base = ReadSession::acquire(
        &f.graph,
        &f.decisions,
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::VerifiedOnly,
    )
    .unwrap();
    assert_eq!(
        stories(&verified, compile_limits()).artifact,
        stories(&verified_base, compile_limits()).artifact
    );
    // Keep the real producer's citations, but make one support curatable to
    // exercise the existing projection rule independently of producer eligibility.
    let BaseRef::Node(id) = f.facts[0]
        .cites
        .iter()
        .find(|r| matches!(r, BaseRef::Node(_)))
        .unwrap()
    else {
        unreachable!()
    };
    let mut node = f.base().graph.0.into_iter().find(|n| &n.id == id).unwrap();
    let mut p: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    p.tier = Tier::Semantic;
    p.confidence_tier = ConfidenceTier::InferredStrong;
    node.props["prov"] = json!(p);
    f.graph.lock().unwrap().put_node(&node).unwrap();
    let stale = f.best();
    assert!(matches!(stale.basis.knowledge, KnowledgeBasis::Stale(_)));
    let without_overlay = ReadSession::acquire(
        &f.graph,
        &f.decisions,
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    )
    .unwrap();
    assert_eq!(
        stories(&stale, compile_limits()).artifact,
        stories(&without_overlay, compile_limits()).artifact
    );
    f.publish(f.facts.clone());
    f.decisions
        .lock()
        .unwrap()
        .record_assertion(
            &agents::CuratableAssertion {
                subject_id: node.id,
                summary: "support".into(),
                provenance: p,
            },
            agents::AssertionDecision::Rejected,
            None,
        )
        .unwrap();
    let suppressed = stories(&f.best(), compile_limits());
    assert!(suppressed.artifact.assertions.is_empty());
    assert!(
        suppressed
            .artifact
            .content
            .contains("1 derived stories omitted because supporting facts are filtered.")
    );
    let mut invalid = f.facts.clone();
    let OverlayItem::Node(n) = &mut invalid[0].item else {
        unreachable!()
    };
    n.props["prov"]["confidence_tier"] = json!("InferredStrong");
    f.publish(invalid); // Valid storage provenance, invalid compiler candidate.
    let invalid = f.best();
    assert!(matches!(
        invalid.compile_artifact("user-stories", trace_limits(), compile_limits()),
        Err(ReadError::Storage)
    ));
    let no_trace = flowtracer::TraceLimits {
        max_output_bytes: 0,
        ..trace_limits()
    };
    assert!(matches!(
        invalid
            .compile_artifact("user-stories", no_trace, compile_limits())
            .unwrap(),
        ArtifactCompilation::Limited {
            limit: ArtifactLimit::Trace(flowtracer::TraceLimitExceeded {
                limit: flowtracer::TraceLimit::OutputBytes,
                maximum: 0
            }),
            ..
        }
    ));
    assert!(matches!(
        invalid.compile_artifact("unknown", no_trace, compile_limits()),
        Err(ReadError::InvalidRequest)
    ));
}

#[test]
fn adopted_candidate_curation_is_session_bound_and_prose_is_literal() {
    let f = Fixture::new();
    let title = "# injected\n<script>bad()</script>\n```";
    let mut facts = f.facts.clone();
    let OverlayItem::Node(n) = &mut facts[0].item else {
        unreachable!()
    };
    n.props["title"] = json!(title);
    n.props["prov"]["content_hash"] = json!(core_prov::content_hash(title.as_bytes()));
    let candidate = n.clone();
    f.publish(facts);
    let captured = f.best();
    let artifact = stories(&captured, compile_limits());
    assert!(
        artifact
            .artifact
            .content
            .contains(&serde_json::to_string(title).unwrap())
    );
    assert!(!artifact.artifact.content.contains(title));
    f.decisions
        .lock()
        .unwrap()
        .record_assertion(
            &agents::CuratableAssertion {
                subject_id: candidate.id,
                summary: "derived candidate".into(),
                provenance: serde_json::from_value(candidate.props["prov"].clone()).unwrap(),
            },
            agents::AssertionDecision::Rejected,
            None,
        )
        .unwrap();
    let rejected = f.best();
    let suppressed = stories(&rejected, compile_limits());
    assert!(suppressed.artifact.assertions.is_empty());
    assert_ne!(captured.basis_id().unwrap(), rejected.basis_id().unwrap());
    assert_eq!(
        stories(&captured, compile_limits()).artifact,
        artifact.artifact
    );
}

#[test]
fn adopted_output_and_page_boundaries_are_exact_without_partial_artifacts() {
    let f = Fixture::new();
    f.publish(f.facts.clone());
    let current = f.best();
    let artifact = stories(&current, compile_limits());
    let output = artifact.artifact.content.len()
        + serde_json::to_vec(&artifact.artifact.assertions)
            .unwrap()
            .len();
    assert_eq!(
        stories(
            &current,
            spec::SelectedCompileLimits {
                max_bytes: output,
                ..compile_limits()
            }
        )
        .artifact,
        artifact.artifact
    );
    assert!(
        matches!(current.compile_artifact("user-stories", trace_limits(),
        spec::SelectedCompileLimits { max_bytes: output - 1, ..compile_limits() }).unwrap(),
        ArtifactCompilation::Limited { artifact_id, limit: ArtifactLimit::Compiler(
            spec::SelectedArtifactLimit::OutputBytes { remaining }) }
            if artifact_id == "user-stories" && remaining == output - 1)
    );
    let page = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            0,
            cap(1),
            cap(1 << 20),
        )
        .unwrap();
    assert_eq!(page.content, &artifact.artifact.content[..1]);
    assert_eq!(page.basis_id, current.basis_id().unwrap());
    assert_eq!(page.content_id, artifact.artifact.content_id);
    assert!(!page.structured_assertions_included);
    let bytes = serde_json::to_vec(&page).unwrap().len();
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                0,
                cap(1),
                cap(bytes)
            )
            .unwrap(),
        page
    );
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                0,
                cap(1),
                cap(bytes - 1)
            )
            .unwrap_err(),
        ReadError::Limit
    );
}

#[cfg(unix)]
#[test]
fn capability_worker_role_admits_real_candidates_and_grows_with_selection() {
    use crate::mcp_reads::{
        reservation::{session::SessionReservation, worker_bound::WorkerReservation},
        worker,
    };
    use context_hub::logical::{json_bytes, slots, sum};
    let f = Fixture::new();
    f.publish(f.facts.clone());
    let captured = f.best();
    let mut policy = worker::tests::limits();
    policy.graph.max_rows = captured.base.graph.0.len() + captured.base.graph.1.len() + 1;
    policy.graph.max_bytes = serde_json::to_vec(&captured.base.graph).unwrap().len() * 2;
    let graph = flowtracer::TraceGraphBound {
        bytes: policy.graph.max_bytes,
        rows: policy.graph.max_rows,
        values: policy.graph.max_values,
    };
    let (n, bytes) = crate::mcp_reads::capability_caps(policy.graph);
    let compiler = spec::logical_capability_compile_bound(
        graph,
        policy.dispatch.trace,
        policy.dispatch.compile,
        n,
        bytes,
    )
    .unwrap();
    let candidates: Vec<_> = captured
        .capabilities
        .iter()
        .map(|f| {
            let OverlayItem::Node(n) = &f.item else {
                unreachable!()
            };
            spec::CapabilityStory {
                capability: n,
                cites: &f.cites,
            }
        })
        .collect();
    let actual_candidate_owners =
        std::mem::size_of_val(&candidates) + std::mem::size_of_val(candidates.as_slice());
    let bound = WorkerReservation::for_limits(policy).unwrap();
    assert!(bound.capability_compile_bytes >= compiler.peak_bytes + actual_candidate_owners);
    assert_eq!(
        bound.capability_compile_bytes,
        sum(&[
            compiler.peak_bytes,
            slots::<spec::CapabilityStory<'_>>(n).unwrap(),
            std::mem::size_of_val(&candidates)
        ])
        .unwrap()
    );
    let session = SessionReservation::for_limits(policy).unwrap();
    let read = policy.max_request_frame_bytes.get();
    let (ids, current_id) = crate::mcp_reads::reservation::session::request_ids_bytes(
        policy.dispatch.max_request_ids.get(),
        read,
    )
    .unwrap();
    let argument = sum(&[
        json_bytes(read).unwrap(),
        json_bytes(read).unwrap(),
        read,
        current_id,
    ])
    .unwrap();
    assert!(
        bound.required_bytes
            >= sum(&[
                session.retained_session_bytes,
                ids,
                argument,
                bound.capability_compile_bytes
            ])
            .unwrap()
    );
    for change in [true, false] {
        let mut larger = policy;
        if change {
            larger.graph.max_rows += 1;
        } else {
            larger.graph.max_bytes += 1;
        }
        assert!(
            WorkerReservation::for_limits(larger)
                .unwrap()
                .capability_compile_bytes
                > bound.capability_compile_bytes
        );
    }
    let mut overflow = policy;
    overflow.graph.max_values = usize::MAX;
    assert!(WorkerReservation::for_limits(overflow).is_none());
    // The existing worker::tests::complete_revision_reservation_accepts_exact_floor_and_refuses_one_under
    // runs production validate_reservation with this newly composed floor.
}

// AC-0513: official adopted stories retain the same security refusal as other artifacts.
#[test]
fn adopted_stories_preserve_host_security_projection_refusal() {
    let f = Fixture::new();
    let mut endpoint = f
        .base()
        .graph
        .0
        .into_iter()
        .find(|n| n.label == "Endpoint")
        .unwrap();
    endpoint.props["authenticated"] = serde_json::json!(false);
    f.graph.lock().unwrap().put_node(&endpoint).unwrap();
    let facts = spec::derive_graphql_resolver_capabilities(&f.base()).facts;
    assert!(!facts.is_empty());
    f.publish(facts);
    let mut session = f.best();
    assert!(matches!(
        session
            .compile_artifact("user-stories", trace_limits(), compile_limits())
            .unwrap(),
        ArtifactCompilation::Produced(_)
    ));
    // Tighten private fixture admission after capture solely to exercise propagation;
    // an admitted production graph normally fits its source-derived security bound.
    session.graph_limits.max_rows = 0;
    for id in ["user-stories", "topology"] {
        assert!(matches!(
            session
                .compile_artifact(id, trace_limits(), compile_limits())
                .unwrap(),
            ArtifactCompilation::Limited {
                limit: ArtifactLimit::Compiler(spec::SelectedArtifactLimit::SecurityDerivation(
                    spec::SecurityLimitExceeded {
                        limit: spec::SecurityLimit::Rows,
                        maximum: 0,
                    }
                )),
                ..
            }
        ));
    }
}
