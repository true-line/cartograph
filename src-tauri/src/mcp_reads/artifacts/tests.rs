//! #718 / AC-0288 frozen compiler/paging foundation, not MCP acceptance.
use super::*;
use crate::mcp_reads::ReadSession;
use agents::DecisionLog;
use core_graph::coverage::CoverageReadLimits;
use core_graph::{GraphStore, Node, SnapshotReadLimits, SqliteGraphStore};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;
use spec::ExportMode;
use std::sync::Mutex;

fn fixture(mode: ExportMode) -> ReadSession {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "main.tf".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "fixture",
        b"resource",
    )
    .unwrap();
    graph
        .put_node(&Node {
            id: "resource:orders".into(),
            label: "Resource".into(),
            props: json!({"logical_id":"orders é🙂\"\\\n","type":"queue","prov":provenance}),
        })
        .unwrap();
    ReadSession::acquire(
        &Mutex::new(graph),
        &Mutex::new(DecisionLog::open(":memory:").unwrap()),
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        mode,
    )
    .unwrap()
}
fn trace_limits() -> flowtracer::TraceLimits {
    flowtracer::TraceLimits {
        max_flows: 64,
        max_hops: 512,
        max_output_bytes: 1024 * 1024,
    }
}
fn compile_limits() -> spec::SelectedCompileLimits {
    spec::SelectedCompileLimits {
        max_bytes: 1024 * 1024,
        adr: spec::AdrLimits {
            max_rows: 1024,
            max_output_bytes: 1024 * 1024,
        },
    }
}
fn produced(session: &ReadSession) -> CompiledArtifact {
    match session
        .compile_artifact("topology", trace_limits(), compile_limits())
        .unwrap()
    {
        ArtifactCompilation::Produced(artifact) => artifact,
        _ => panic!("fixture must fit"),
    }
}
fn cap(bytes: usize) -> NonZeroUsize {
    NonZeroUsize::new(bytes).unwrap()
}

#[test]
fn real_selected_compilation_and_catalog_match_official_compiler() {
    let session = fixture(ExportMode::BestEffort);
    let artifact = produced(&session);
    let flows = flowtracer::trace(&session.raw_nodes, &session.raw_edges);
    let bundle = spec::compile_spec(
        &session.raw_nodes,
        &session.raw_edges,
        &flows,
        session.basis.export_mode,
        &session.rejected_hashes,
    );
    let official = bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == "topology")
        .unwrap();
    assert_eq!(&artifact.artifact, official);
    assert!(artifact.artifact.verify_identity());
    assert_eq!(artifact.basis_id(), session.basis_id().unwrap());
    assert_eq!(artifact.content_id(), official.content_id);
    let catalog = session.artifact_catalog();
    assert_eq!(catalog, spec::artifact_catalog());
    assert!(
        catalog
            .iter()
            .any(|metadata| metadata.id == official.id && metadata.schema == official.schema)
    );
}

#[test]
fn unknown_id_precedes_tracing_and_every_limit_has_no_produced_handle() {
    let session = fixture(ExportMode::BestEffort);
    let no_trace = flowtracer::TraceLimits {
        max_output_bytes: 0,
        ..trace_limits()
    };
    assert!(matches!(
        session.compile_artifact("arbitrary", no_trace, compile_limits()),
        Err(ReadError::InvalidRequest)
    ));
    assert!(
        matches!(session.compile_artifact("topology",no_trace,compile_limits()).unwrap(),ArtifactCompilation::Limited {artifact_id,limit:ArtifactLimit::Trace(flowtracer::TraceLimitExceeded {limit:flowtracer::TraceLimit::OutputBytes,maximum:0})} if artifact_id=="topology")
    );
    assert!(matches!(
        session
            .compile_artifact(
                "topology",
                trace_limits(),
                spec::SelectedCompileLimits {
                    max_bytes: 0,
                    ..compile_limits()
                }
            )
            .unwrap(),
        ArtifactCompilation::Limited {
            limit: ArtifactLimit::Compiler(spec::SelectedArtifactLimit::OutputBytes {
                remaining: 0
            }),
            ..
        }
    ));
    assert!(matches!(
        session
            .compile_artifact(
                "topology",
                trace_limits(),
                spec::SelectedCompileLimits {
                    adr: spec::AdrLimits {
                        max_rows: 0,
                        max_output_bytes: 0
                    },
                    ..compile_limits()
                }
            )
            .unwrap(),
        ArtifactCompilation::Limited {
            limit: ArtifactLimit::Compiler(spec::SelectedArtifactLimit::AdrDerivation(_)),
            ..
        }
    ));
}

#[test]
fn pages_bind_complete_basis_content_and_utf8_offsets() {
    let session = fixture(ExportMode::BestEffort);
    let artifact = produced(&session);
    let mut other = fixture(ExportMode::BestEffort);
    // Same projected context/content, different complete registration basis.
    other.basis.registration_basis_digest = "changed-registration".into();
    assert_eq!(session.basis.context_id, other.basis.context_id);
    let other = produced(&other);
    assert_eq!(artifact.content_id(), other.content_id());
    assert_ne!(artifact.basis_id(), other.basis_id());
    assert_eq!(
        artifact
            .page(
                other.basis_id(),
                artifact.content_id(),
                0,
                cap(16),
                cap(4096)
            )
            .unwrap_err(),
        ReadError::StaleBasis
    );
    assert_eq!(
        artifact
            .page(artifact.basis_id(), "wrong-content", 0, cap(16), cap(4096))
            .unwrap_err(),
        ReadError::StaleBasis
    );
    let position = artifact.artifact.content.find('é').unwrap();
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                position + 1,
                cap(16),
                cap(4096)
            )
            .unwrap_err(),
        ReadError::InvalidRequest
    );
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                usize::MAX,
                cap(16),
                cap(4096)
            )
            .unwrap_err(),
        ReadError::InvalidRequest
    );
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                position,
                cap(1),
                cap(4096)
            )
            .unwrap_err(),
        ReadError::Limit
    );
    let page = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            position,
            cap(3),
            cap(4096),
        )
        .unwrap();
    assert_eq!(page.content, "é");
    assert_eq!(page.next_offset, Some(position + 2));
    assert!(page.partial);
}

#[test]
fn all_text_pages_reconstruct_exact_content_without_fabricating_full_artifacts() {
    let artifact = produced(&fixture(ExportMode::BestEffort));
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let page = artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                offset,
                cap(5),
                cap(4096),
            )
            .unwrap();
        assert_eq!(page.content_id, artifact.artifact.content_id);
        assert_eq!(page.representation, "artifact-content");
        assert!(!page.structured_assertions_included);
        text.push_str(page.content);
        match page.next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(text, artifact.artifact.content);
    let end = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            text.len(),
            cap(1),
            cap(4096),
        )
        .unwrap();
    assert_eq!(end.content, "");
    assert!(end.next_offset.is_none());
}

#[test]
fn full_response_caps_count_escaped_text_and_exact_metadata_envelope() {
    let artifact = produced(&fixture(ExportMode::BestEffort));
    let page = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            0,
            cap(usize::MAX),
            cap(65536),
        )
        .unwrap();
    assert!(!page.partial);
    assert!(page.content.contains('🙂'));
    assert!(page.content.contains('\n'));
    let exact = serde_json::to_vec(&page).unwrap().len();
    assert!(exact > page.content.len());
    let at_limit = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            0,
            cap(usize::MAX),
            cap(exact),
        )
        .unwrap();
    assert_eq!(at_limit, page);
    let smaller = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            0,
            cap(usize::MAX),
            cap(exact - 1),
        )
        .unwrap();
    assert!(smaller.partial);
    assert!(serde_json::to_vec(&smaller).unwrap().len() < exact);
    assert_eq!(
        artifact
            .page(
                artifact.basis_id(),
                artifact.content_id(),
                0,
                cap(1),
                cap(1)
            )
            .unwrap_err(),
        ReadError::Limit
    );
    // Check the chosen prefix is maximal against brute-force UTF-8 boundaries,
    // including cursor numeric widths and the smaller final null envelope.
    for budget in (exact.saturating_sub(80)..=exact).step_by(3) {
        let best = (1..=artifact.artifact.content.len())
            .filter(|end| artifact.artifact.content.is_char_boundary(*end))
            .filter(|end| {
                serde_json::to_vec(&artifact.page_at(0, *end))
                    .unwrap()
                    .len()
                    <= budget
            })
            .max();
        match (
            best,
            artifact.page(
                artifact.basis_id(),
                artifact.content_id(),
                0,
                cap(usize::MAX),
                cap(budget),
            ),
        ) {
            (Some(end), Ok(actual)) => assert_eq!(actual.content.len(), end),
            (None, Err(ReadError::Limit)) => {}
            _ => panic!("prefix selection disagrees with actual serialized envelope"),
        }
    }
}
