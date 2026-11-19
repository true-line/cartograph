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
    // AC-0510: the move-owned coherent base preserves official artifact and catalog parity.
    let session = fixture(ExportMode::BestEffort);
    let owned = (session.base.graph.0.as_ptr(), session.base.graph.1.as_ptr());
    let artifact = produced(&session);
    let flows = flowtracer::trace(&session.base.graph.0, &session.base.graph.1);
    let bundle = spec::compile_spec(
        &session.base.graph.0,
        &session.base.graph.1,
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
    assert_eq!(catalog, super::readable_catalog());
    assert!(
        catalog
            .iter()
            .any(|metadata| metadata.id == official.id && metadata.schema == official.schema)
    );
    // The curated artifacts, the D1 document and the bundle manifest read
    // the session's projection and basis, which a bare compile is not
    // supplied with; `curated_read::tests` and `curated::drafts_tests`
    // compare them with the host export (AC-0654, AC-0667, AC-0682).
    for descriptor in catalog.into_iter().filter(|descriptor| {
        !descriptor.id.starts_with("curated")
            && descriptor.id != "d1-product-specification"
            && descriptor.id != "bundle-manifest"
    }) {
        let ArtifactCompilation::Produced(actual) = session
            .compile_artifact(descriptor.id, trace_limits(), compile_limits())
            .unwrap()
        else {
            panic!("fixture fits")
        };
        let legacy = bundle
            .artifacts
            .iter()
            .find(|a| a.id == descriptor.id)
            .unwrap();
        assert_eq!(&actual.artifact, legacy, "{}", descriptor.id);
    }
    // Compiler inputs borrow the one retained raw base; the existing projected
    // ContextSnapshot is separate, and no compiler call replaces that base.
    assert_eq!(
        owned,
        (session.base.graph.0.as_ptr(), session.base.graph.1.as_ptr())
    );
}

// AC-0595 (ADR-0058): the fact index is not readable over MCP, because the
// reserved compile workspace does not model it. Both ids are refused like an
// unknown id before any tracing or compilation, are not advertised, and every
// advertised artifact still reads.
#[test]
fn mcp_read_refuses_the_fact_index_without_compiling() {
    let session = fixture(ExportMode::BestEffort);
    // Limits under which any tracing or compilation reports a limit.
    let no_trace = flowtracer::TraceLimits {
        max_output_bytes: 0,
        ..trace_limits()
    };
    let no_output = spec::SelectedCompileLimits {
        max_bytes: 0,
        ..compile_limits()
    };
    let official: Vec<&str> = spec::artifact_catalog().iter().map(|a| a.id).collect();
    let advertised: Vec<&str> = session.artifact_catalog().iter().map(|a| a.id).collect();
    let schema =
        crate::mcp_reads::dispatch::tool_descriptor(mcp_wire::admission::ReadTool::ReadArtifact);
    let selectable = schema["inputSchema"]["properties"]["artifact_id"]["enum"]
        .as_array()
        .expect("closed artifact id enum");
    for id in ["fact-index", "fact-index-json"] {
        assert!(official.contains(&id), "{id} is an official artifact");
        assert!(!advertised.contains(&id), "{id} is not advertised");
        assert!(!selectable.iter().any(|value| value == id));
        assert!(matches!(
            session.compile_artifact(id, no_trace, no_output),
            Err(ReadError::InvalidRequest)
        ));
        assert!(matches!(
            session.compile_artifact(id, trace_limits(), compile_limits()),
            Err(ReadError::InvalidRequest)
        ));
    }
    // The two fact-index ids are withheld and `curated-json`, which is not
    // an export artifact, is added (AC-0654). The D1 document is readable
    // (AC-0667).
    assert!(advertised.contains(&"d1-product-specification"));
    assert_eq!(advertised.len(), official.len() - 2 + 1);
    assert_eq!(selectable.len(), advertised.len());
    for id in advertised {
        assert!(
            matches!(
                session.compile_artifact(id, trace_limits(), compile_limits()),
                Ok(ArtifactCompilation::Produced(artifact)) if artifact.artifact.id == id
            ),
            "{id} still reads"
        );
    }
}

// AC-0654 (SPEC-12 slice 4): the session computes the curated projection,
// so `curated` and its structured form are in the catalogue and the tool
// schema and are read like any artifact. The fact index stays unreadable.
// The structured form is admitted against the output budget whole.
#[test]
fn mcp_read_lists_and_reads_the_curated_document_and_its_structured_form() {
    let session = fixture(ExportMode::BestEffort);
    let catalog = session.artifact_catalog();
    let ids: Vec<_> = catalog.iter().map(|artifact| artifact.id).collect();
    let at = ids.iter().position(|id| *id == "curated").expect("curated");
    assert_eq!(ids[at + 1], "curated-json");
    assert!(
        spec::artifact_catalog()
            .iter()
            .all(|artifact| artifact.id != "curated-json"),
        "the structured form is not an export artifact"
    );
    assert!(ids.iter().all(|id| !id.starts_with("fact-index")));
    let schema =
        crate::mcp_reads::dispatch::tool_descriptor(mcp_wire::admission::ReadTool::ReadArtifact);
    let readable = schema["inputSchema"]["properties"]["artifact_id"]["enum"]
        .as_array()
        .expect("closed artifact id enum");
    for id in ["curated", "curated-json"] {
        assert!(readable.iter().any(|value| value == id), "{id}");
        let ArtifactCompilation::Produced(artifact) = session
            .compile_artifact(id, trace_limits(), compile_limits())
            .unwrap()
        else {
            panic!("{id} is produced");
        };
        assert_eq!(artifact.basis_id(), session.basis_id().unwrap());
        assert_eq!(
            artifact.content_id(),
            artifact.artifact().compute_content_id()
        );
    }
    assert!(session.basis().h3_curated_available);
    let no_output = spec::SelectedCompileLimits {
        max_bytes: 0,
        ..compile_limits()
    };
    for id in ["curated", "curated-json"] {
        assert!(
            matches!(
                session.compile_artifact(id, trace_limits(), no_output),
                Ok(ArtifactCompilation::Limited { .. })
            ),
            "{id}"
        );
    }
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

#[test]
fn security_reservation_bound_overflow_fails_before_tracing_or_compilation() {
    // AC-0288: derived admission is checked, never silently saturated.
    let mut session = fixture(ExportMode::VerifiedOnly);
    session.graph_limits.max_bytes = usize::MAX;
    assert!(matches!(
        session.compile_artifact("topology", trace_limits(), compile_limits()),
        Err(ReadError::Limit)
    ));
    assert!(matches!(
        session.compile_artifact("unknown", trace_limits(), compile_limits()),
        Err(ReadError::InvalidRequest)
    ));
}

#[test]
fn mcp_security_artifact_preserves_legacy_compiler_identity() {
    let source = fixture(ExportMode::BestEffort);
    let mut endpoint = source.base.graph.0[0].clone();
    endpoint.id = "endpoint:public".into();
    endpoint.label = "Endpoint".into();
    endpoint.props["authenticated"] = json!(false);
    endpoint.props["method"] = json!("GET");
    endpoint.props["path"] = json!("/public\0\n\"\\🧭");
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    graph.put_node(&endpoint).unwrap();
    let session = ReadSession::acquire(
        &Mutex::new(graph),
        &Mutex::new(DecisionLog::open(":memory:").unwrap()),
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    )
    .unwrap();
    let ArtifactCompilation::Produced(actual) = session
        .compile_artifact("security-view", trace_limits(), compile_limits())
        .unwrap()
    else {
        panic!("admitted security fixture")
    };
    let flows =
        flowtracer::trace_bounded(&session.base.graph.0, &session.base.graph.1, trace_limits())
            .unwrap();
    let expected = spec::compile_spec_selected(
        &session.base.graph.0,
        &session.base.graph.1,
        &flows,
        session.basis.export_mode,
        &session.rejected_hashes,
        &["security-view"],
        compile_limits(),
    )
    .unwrap();
    let spec::SelectedArtifactOutcome::Produced(expected) = &expected[0] else {
        panic!("legacy security fixture")
    };
    assert_eq!(&actual.artifact, expected);
    assert!(
        actual
            .artifact
            .content
            .contains("No authentication check recovered")
    );
}

mod overlay_capture;

mod overlay_adoption;
