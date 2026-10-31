use super::*;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;

fn fixture() -> Vec<Node> {
    [ConfidenceTier::Confirmed,ConfidenceTier::InferredWeak].into_iter().enumerate().map(|(i,confidence)|{
        let prov=Provenance::new(if confidence==ConfidenceTier::Confirmed{Tier::Deterministic}else{Tier::Agentic},confidence,vec![EvidenceRef{repo:"fixture".into(),path:"source".into(),byte_start:0,byte_end:5,commit_sha:"fixed".into()}],"fixture",format!("endpoint{i}").as_bytes()).unwrap();
        Node{id:format!("endpoint:{i}"),label:"Endpoint".into(),props:json!({"method":"GET","path":"/control\0\n\"\\🧭","authenticated":false,"prov":prov})}
    }).collect()
}
fn limits() -> SelectedCompileLimits {
    SelectedCompileLimits {
        max_bytes: 1_000_000,
        adr: crate::AdrLimits {
            max_rows: 128,
            max_output_bytes: 1_000_000,
        },
    }
}

#[test]
fn security_admitted_selected_compilation_preserves_every_official_artifact() {
    // AC-0288: adding admission cannot change official selected output identities.
    let nodes = fixture();
    let ids: Vec<_> = artifact_catalog().iter().map(|a| a.id).collect();
    let size = crate::security_projection_size(&nodes, &[]).unwrap();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let legacy =
            compile_spec_selected(&nodes, &[], &[], mode, &BTreeSet::new(), &ids, limits())
                .unwrap();
        let admitted = compile_spec_selected_with_security(
            &nodes,
            &[],
            &[],
            mode,
            &BTreeSet::new(),
            &ids,
            limits(),
            crate::SecurityLimits {
                max_rows: size.rows,
                max_output_bytes: size.bytes,
            },
        )
        .unwrap();
        assert_eq!(admitted, legacy);
    }
}

#[test]
fn selected_security_failure_precedes_output_node_ownership_and_returns_no_artifact() {
    let nodes = fixture();
    let size = crate::security_projection_size(&nodes, &[]).unwrap();
    let selected = ["security-view", "topology"];
    for security in [
        crate::SecurityLimits {
            max_rows: 0,
            max_output_bytes: size.bytes,
        },
        crate::SecurityLimits {
            max_rows: size.rows,
            max_output_bytes: size.bytes - 1,
        },
    ] {
        crate::security::SECURITY_OUTPUT_MATERIALIZATIONS.with(|count| count.set(0));
        let outcomes = compile_spec_selected_with_security(
            &nodes,
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &selected,
            limits(),
            security,
        )
        .unwrap();
        assert_eq!(outcomes.len(), selected.len());
        assert!(outcomes.iter().all(|outcome| matches!(
            outcome,
            SelectedArtifactOutcome::Limited {
                limit: SelectedArtifactLimit::SecurityDerivation(_),
                ..
            }
        )));
        crate::security::SECURITY_OUTPUT_MATERIALIZATIONS.with(|count| assert_eq!(count.get(), 0));
    }
    assert!(
        compile_spec_selected(
            &nodes,
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &selected,
            limits()
        )
        .unwrap()
        .iter()
        .all(|outcome| matches!(outcome, SelectedArtifactOutcome::Produced(_)))
    );
}

#[test]
fn selection_validation_and_empty_selection_precede_security_derivation() {
    let nodes = fixture();
    let security = crate::SecurityLimits {
        max_rows: 0,
        max_output_bytes: 0,
    };
    crate::security::SECURITY_OUTPUT_MATERIALIZATIONS.with(|count| count.set(0));
    assert!(matches!(
        compile_spec_selected_with_security(
            &nodes,
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &["unknown"],
            limits(),
            security
        ),
        Err(ArtifactSelectionError::UnknownArtifact { index: 0 })
    ));
    assert!(
        compile_spec_selected_with_security(
            &nodes,
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &[],
            limits(),
            security
        )
        .unwrap()
        .is_empty()
    );
    crate::security::SECURITY_OUTPUT_MATERIALIZATIONS.with(|count| assert_eq!(count.get(), 0));
}

// AC-0504: the sidecar route retains caller security admission, even empty.
#[test]
fn capability_stories_preserve_caller_security_admission() {
    let nodes = fixture();
    let size = crate::security_projection_size(&nodes, &[]).unwrap();
    assert!(size.rows > 0);
    let mut candidate = nodes[0].clone();
    candidate.id = "capability:security-fixture".into();
    candidate.label = "Capability".into();
    let source = Provenance::deserialize(&nodes[0].props["prov"]).unwrap();
    candidate.props["prov"] = json!(
        Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredWeak,
            source.evidence,
            "fixture",
            b"capability"
        )
        .unwrap()
        .with_method(core_prov::ProducingMethod::Rule {
            rule: "fixture".into(),
            version: "1".into()
        })
    );
    let cites = BTreeSet::from([core_graph::overlay::BaseRef::Node(nodes[0].id.clone())]);
    let candidates = [crate::CapabilityStory {
        capability: &candidate,
        cites: &cites,
    }];
    let base = core_graph::RevisionedSnapshot {
        revision: core_graph::StoreRevision {
            graph_instance: "f".repeat(32),
            generation: 1,
        },
        graph: (nodes, vec![]),
    };
    for candidates in [&[][..], &candidates[..]] {
        let legacy = crate::compile_capability_user_stories(
            &base,
            candidates,
            ExportMode::BestEffort,
            &BTreeSet::new(),
            limits(),
        )
        .unwrap();
        assert!(matches!(legacy, SelectedArtifactOutcome::Produced(_)));
        let compile = |security| {
            crate::compile_capability_user_stories_with_security(
                &base,
                candidates,
                ExportMode::BestEffort,
                &BTreeSet::new(),
                limits(),
                security,
            )
            .unwrap()
        };
        assert_eq!(
            compile(crate::SecurityLimits {
                max_rows: size.rows,
                max_output_bytes: size.bytes
            }),
            legacy
        );
        for (security, expected) in [
            (
                crate::SecurityLimits {
                    max_rows: size.rows - 1,
                    max_output_bytes: size.bytes,
                },
                crate::SecurityLimit::Rows,
            ),
            (
                crate::SecurityLimits {
                    max_rows: size.rows,
                    max_output_bytes: size.bytes - 1,
                },
                crate::SecurityLimit::OutputBytes,
            ),
        ] {
            crate::security::SECURITY_OUTPUT_MATERIALIZATIONS.with(|n| n.set(0));
            assert!(
                matches!(compile(security), SelectedArtifactOutcome::Limited {
                artifact_id, limit: SelectedArtifactLimit::SecurityDerivation(error)
            } if artifact_id == "user-stories" && error.limit == expected)
            );
            crate::security::SECURITY_OUTPUT_MATERIALIZATIONS.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
