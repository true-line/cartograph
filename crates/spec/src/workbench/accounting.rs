//! Module-owned logical payload/slot accounting for one selected artifact.
//! It is not a native allocation, allocator-capacity or process RSS guarantee.
use super::*;
use core_prov::EvidenceRef;
use flowtracer::{TraceGraphBound, TraceLimits};
use std::{io, mem::size_of};

mod capability;
pub use capability::{CapabilityCompileWorkspaceBound, logical_capability_compile_bound};

/// Version of the compiler's logical accounting currency.
pub const COMPILE_WORKSPACE_MODEL: &str = "spec-logical-compile-v1";

/// Named simultaneous compiler roles, excluding the caller's retained graph,
/// snapshot/coverage/session and transport-reserved response DOM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileWorkspaceBound {
    /// Version of this logical accounting contract.
    pub model: &'static str,
    /// Original traced flow output retained through selected compilation.
    pub original_flows_bytes: usize,
    /// Full disposable graph projection, including admitted ADR/security rows.
    pub projected_graph_bytes: usize,
    /// Owned projected flows, including fixed suppression-gap expansion.
    pub projected_flows_bytes: usize,
    /// Per-hop projection-gap identity tuple scratch.
    pub flow_identity_scratch_bytes: usize,
    /// Largest ADR construction workspace in addition to retained projections.
    pub adr_workspace_bytes: usize,
    /// Current security supporting-row construction workspace.
    pub security_workspace_bytes: usize,
    /// Largest single selected producer's workspace.
    pub renderer_workspace_bytes: usize,
    /// Admitted artifact content/assertions and fixed official metadata.
    pub artifact_bytes: usize,
    /// Complete compiler phase logical requirement.
    pub peak_bytes: usize,
}

pub(crate) fn sum(parts: &[usize]) -> Option<usize> {
    parts.iter().try_fold(0usize, |n, p| n.checked_add(*p))
}
pub(crate) fn slots<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}
pub(crate) fn sized_slots(count: usize, bytes: usize) -> Option<usize> {
    count.checked_mul(bytes)
}
pub(crate) fn set_slots<K>(count: usize) -> Option<usize> {
    sized_slots(
        count,
        size_of::<K>().checked_add(size_of::<usize>().checked_mul(2)?)?,
    )
}
pub(crate) fn map_slots<K, V>(count: usize) -> Option<usize> {
    sized_slots(
        count,
        sum(&[
            size_of::<K>(),
            size_of::<V>(),
            size_of::<usize>().checked_mul(3)?,
        ])?,
    )
}
/// Uniform logical JSON/typed currency: payload plus one Value/key/index role
/// per admitted value. These headers are accounting units, not an assertion
/// that a serde Value is physically retained by every typed producer.
pub(crate) fn json(bytes: usize, values: usize) -> Option<usize> {
    sum(&[
        bytes,
        slots::<serde_json::Value>(values)?,
        slots::<String>(values)?,
        map_slots::<(), ()>(values)?,
    ])
}
pub(crate) fn json_bytes(bytes: usize) -> Option<usize> {
    json(bytes, bytes.checked_add(1)?)
}
fn graph_charge(bytes: usize, rows: usize, values: usize) -> Option<usize> {
    json(bytes, values)?.checked_add(sized_slots(rows, size_of::<Node>().max(size_of::<Edge>()))?)
}
fn count(value: &impl Serialize) -> Option<usize> {
    struct Counter(usize);
    impl io::Write for Counter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(b.len())
                .ok_or_else(|| io::Error::other("logical count overflow"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.0)
}

fn projection_templates() -> Option<(usize, usize)> {
    let hop = Hop {
        label: String::new(),
        src: String::new(),
        dst: String::new(),
        src_name: String::new(),
        dst_name: String::new(),
        tier: String::new(),
        confidence: String::new(),
        evidence: None,
        provenance: Provenance::new(Tier::Deterministic, ConfidenceTier::Gap, vec![], "", b"")
            .ok()?,
        gap_reason: None,
        attempted_tiers: vec![],
    };
    let mut expanded = 0;
    let mut identity = 0;
    for reason in [
        super::PROJECTION_GAP_REJECTED,
        super::PROJECTION_GAP_VERIFIED_ONLY,
    ] {
        expanded = expanded.max(count(&projection_gap(&hop, reason))?);
        identity = identity.max(count(&("projection-gap", "", "", "", "", reason))?);
    }
    Some((expanded, identity))
}
fn artifact_metadata() -> Option<usize> {
    selected::artifact_catalog()
        .into_iter()
        .map(|a| {
            count(&identified_artifact(
                a.id,
                a.file_name,
                a.title,
                a.format,
                String::new(),
                vec![],
            ))
        })
        .try_fold(0, |largest, next| Some(largest.max(next?)))
}
fn minimum_assertion_bytes() -> Option<usize> {
    let mut provenance =
        Provenance::new(Tier::Agentic, ConfidenceTier::Gap, vec![], "", b"").ok()?;
    provenance.content_hash.clear();
    count(&SpecAssertion {
        id: String::new(),
        subject_id: String::new(),
        subject_kind: String::new(),
        summary: String::new(),
        provenance,
        edge_label: None,
        omission: None,
    })
}

/// Pure checked logical peak for one official selected artifact. The host must
/// admit the input graph and trace result separately. Source graph fields are
/// copied only in the graph projection/ADR/topology roles; register identity
/// fragments and relation/group indexes remain borrowed.
///
/// Typed byte-derived roles use `json_bytes` as a deliberately broad currency
/// (one header/index role per input byte), including nested DTO structure and
/// current-row parse/format workspace. They are not imaginary retained Value
/// clones or a bound on native allocator slack/internal collection machinery.
/// Overflow rejects the model rather than saturating or reducing a reservation.
pub fn logical_compile_bound(
    graph: TraceGraphBound,
    trace: TraceLimits,
    compile: SelectedCompileLimits,
) -> Option<CompileWorkspaceBound> {
    let security = crate::security_projection_bound(graph.bytes, graph.rows, graph.values)?;
    let (gap_template, identity_template) = projection_templates()?;
    let projected_flow_payload = trace
        .max_output_bytes
        .checked_add(trace.max_hops.checked_mul(gap_template)?)?;
    let flow_identity_scratch_bytes = trace.max_output_bytes.checked_add(identity_template)?;
    let original_flows_bytes =
        flowtracer::logical_workspace_bound(graph, trace)?.retained_output_bytes;
    // A generated suppressed hop adds one attempted-tier element even when the
    // original input had no property-array values. Header accounting includes it.
    let projected_flow_graph = TraceGraphBound {
        values: graph.values.checked_add(1)?,
        ..graph
    };
    let projected_flows_bytes = flowtracer::logical_workspace_bound(
        projected_flow_graph,
        TraceLimits {
            max_output_bytes: projected_flow_payload,
            ..trace
        },
    )?
    .retained_output_bytes;
    let bytes = sum(&[graph.bytes, compile.adr.max_output_bytes, security.bytes])?;
    let rows = sum(&[graph.rows, compile.adr.max_rows, security.rows])?;
    // Security findings are typed generated rows: their value count has its
    // own proven bound, rather than one value per serialized byte.
    let security_values = crate::security_projection_value_bound(graph.rows, graph.values)?;
    let values = sum(&[graph.values, compile.adr.max_output_bytes, security_values])?;
    let projected_graph_bytes = graph_charge(bytes, rows, values)?;
    let filters = sum(&[
        slots::<&Node>(rows)?,
        slots::<&Edge>(rows)?,
        set_slots::<&str>(rows)?,
        set_slots::<&str>(rows)?,
    ])?;
    // One current source-property clone, one typed/diagnostic parse role, and
    // simultaneous names/relation-value formatting roles (four relation fields
    // appear in one writeln! invocation in data-model).
    let current_row = sum(&[
        json(bytes, values)?,
        json_bytes(bytes)?,
        bytes,
        bytes,
        bytes,
        bytes,
        bytes,
        slots::<EvidenceRef>(values)?,
        size_of::<Provenance>(),
        size_of::<Omission>(),
    ])?;
    let adr_workspace_bytes = crate::adr::logical_workspace_bytes(
        graph.bytes,
        graph.rows,
        graph.values,
        trace,
        compile.adr,
    )?;
    let security_workspace_bytes = sum(&[
        current_row,
        graph.bytes,
        graph.bytes,
        graph.bytes,
        graph.values.checked_mul(size_of::<String>())?,
    ])?;
    let native = sum(&[
        current_row,
        filters,
        super::producers::logical_index_bytes(rows, values)?,
        super::diagrams::logical_index_bytes(bytes, rows, trace.max_hops)?,
        graph_charge(bytes, rows, values)?,
    ])?; // topology alone owns a second selected graph
    let registers = super::registers::logical_workspace_bytes(
        bytes,
        rows,
        values,
        projected_flow_payload,
        trace.max_hops,
        trace.max_flows,
    )?
    .checked_add(current_row)?;
    let rules =
        crate::rules::logical_workspace_bytes(bytes, rows, values)?.checked_add(current_row)?;
    let renderer_workspace_bytes = native.max(registers).max(rules);
    let assertions = compile
        .max_bytes
        .checked_div(minimum_assertion_bytes()?.max(1))?;
    let artifact_bytes = sum(&[
        compile.max_bytes,
        json_bytes(compile.max_bytes)?,
        slots::<SpecAssertion>(assertions)?,
        size_of::<SpecArtifact>(),
        artifact_metadata()?,
        super::render::logical_fixed_headers()?,
    ])?;
    let build = adr_workspace_bytes.max(security_workspace_bytes);
    let render = renderer_workspace_bytes.checked_add(artifact_bytes)?;
    let peak_bytes = sum(&[
        original_flows_bytes,
        projected_graph_bytes,
        projected_flows_bytes,
        flow_identity_scratch_bytes,
        build.max(render),
    ])?;
    Some(CompileWorkspaceBound {
        model: COMPILE_WORKSPACE_MODEL,
        original_flows_bytes,
        projected_graph_bytes,
        projected_flows_bytes,
        flow_identity_scratch_bytes,
        adr_workspace_bytes,
        security_workspace_bytes,
        renderer_workspace_bytes,
        artifact_bytes,
        peak_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn graph() -> TraceGraphBound {
        TraceGraphBound {
            bytes: 4096,
            rows: 20,
            values: 100,
        }
    }
    fn trace() -> TraceLimits {
        TraceLimits {
            max_flows: 10,
            max_hops: 30,
            max_output_bytes: 8192,
        }
    }
    fn compile() -> SelectedCompileLimits {
        SelectedCompileLimits {
            max_bytes: 8192,
            adr: crate::AdrLimits {
                max_rows: 30,
                max_output_bytes: 4096,
            },
        }
    }

    #[test]
    fn compiler_workspace_roles_are_checked_and_monotone() {
        let base = logical_compile_bound(graph(), trace(), compile()).unwrap();
        assert_eq!(base.model, COMPILE_WORKSPACE_MODEL);
        assert!(
            base.peak_bytes
                >= base.original_flows_bytes
                    + base.projected_graph_bytes
                    + base.projected_flows_bytes
        );
        for wider in [
            TraceGraphBound {
                bytes: 8192,
                ..graph()
            },
            TraceGraphBound {
                rows: 40,
                ..graph()
            },
            TraceGraphBound {
                values: 200,
                ..graph()
            },
        ] {
            assert!(
                logical_compile_bound(wider, trace(), compile())
                    .unwrap()
                    .peak_bytes
                    >= base.peak_bytes
            );
        }
        for wider in [
            TraceLimits {
                max_flows: 20,
                ..trace()
            },
            TraceLimits {
                max_hops: 60,
                ..trace()
            },
            TraceLimits {
                max_output_bytes: 16384,
                ..trace()
            },
        ] {
            assert!(
                logical_compile_bound(graph(), wider, compile())
                    .unwrap()
                    .peak_bytes
                    >= base.peak_bytes
            );
        }
        for wider in [
            SelectedCompileLimits {
                max_bytes: 16384,
                ..compile()
            },
            SelectedCompileLimits {
                adr: crate::AdrLimits {
                    max_rows: 60,
                    ..compile().adr
                },
                ..compile()
            },
            SelectedCompileLimits {
                adr: crate::AdrLimits {
                    max_output_bytes: 8192,
                    ..compile().adr
                },
                ..compile()
            },
        ] {
            assert!(
                logical_compile_bound(graph(), trace(), wider)
                    .unwrap()
                    .peak_bytes
                    >= base.peak_bytes
            );
        }
        assert!(
            logical_compile_bound(
                TraceGraphBound {
                    bytes: usize::MAX,
                    ..graph()
                },
                trace(),
                compile()
            )
            .is_none()
        );
        assert!(
            logical_compile_bound(
                TraceGraphBound {
                    rows: usize::MAX,
                    ..graph()
                },
                trace(),
                compile()
            )
            .is_none()
        );
        assert!(
            logical_compile_bound(
                TraceGraphBound {
                    values: usize::MAX,
                    ..graph()
                },
                trace(),
                compile()
            )
            .is_none()
        );
        assert!(
            logical_compile_bound(
                graph(),
                TraceLimits {
                    max_hops: usize::MAX,
                    ..trace()
                },
                compile()
            )
            .is_none()
        );
        assert!(
            logical_compile_bound(
                graph(),
                trace(),
                SelectedCompileLimits {
                    max_bytes: usize::MAX,
                    ..compile()
                }
            )
            .is_none()
        );
        assert!(
            logical_compile_bound(
                graph(),
                trace(),
                SelectedCompileLimits {
                    adr: crate::AdrLimits {
                        max_output_bytes: usize::MAX,
                        ..compile().adr
                    },
                    ..compile()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn projection_gap_expansion_and_identity_scratch_follow_real_templates() {
        let provenance = Provenance::new(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            vec![],
            "source extractor",
            b"source",
        )
        .unwrap();
        let hop = Hop {
            label: "CALLS\n\"é".into(),
            src: "source\"\n".repeat(100),
            dst: "destination".repeat(100),
            src_name: "source display".repeat(100),
            dst_name: "destination display".into(),
            tier: "Agentic".into(),
            confidence: "InferredWeak".into(),
            evidence: None,
            provenance,
            gap_reason: None,
            attempted_tiers: vec![],
        };
        let original = vec![Flow {
            trigger: "screen".into(),
            trigger_kind: "Screen".into(),
            trigger_name: "Screen /".into(),
            hops: vec![hop.clone()],
            status: FlowStatus::Inferred,
            score: 0.3,
            depth_limited: false,
        }];
        let original_bytes = count(&original).unwrap();
        let projected = project_flows(&original, ExportMode::VerifiedOnly, &BTreeSet::new());
        assert_eq!(projected[0].hops[0].label, "UNRESOLVED");
        let (gap, identity) = projection_templates().unwrap();
        assert!(count(&projected).unwrap() <= original_bytes + gap);
        let actual_identity = count(&(
            "projection-gap",
            &hop.src,
            &hop.label,
            &hop.dst,
            &hop.provenance.content_hash,
            PROJECTION_GAP_VERIFIED_ONLY,
        ))
        .unwrap();
        assert!(actual_identity <= original_bytes + identity);
        let boundary = TraceLimits {
            max_flows: 1,
            max_hops: 1,
            max_output_bytes: original_bytes,
        };
        let model = logical_compile_bound(graph(), boundary, compile()).unwrap();
        assert!(model.projected_flows_bytes >= count(&projected).unwrap());
        assert!(model.flow_identity_scratch_bytes >= actual_identity);
    }

    #[test]
    fn compiler_accounting_preserves_official_artifact_identity_and_admitted_output() {
        let support = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![EvidenceRef {
                repo: "repo".into(),
                path: "source".into(),
                byte_start: 0,
                byte_end: 1,
                commit_sha: "sha".into(),
            }],
            "fixture",
            b"endpoint",
        )
        .unwrap();
        let nodes = vec![Node {
            id: "endpoint:/".into(),
            label: "Endpoint".into(),
            props: json!({"method":"GET","path":"/","authenticated":false,"prov":support}),
        }];
        let input_bytes = count(&(&nodes, &Vec::<Edge>::new())).unwrap();
        let graph = TraceGraphBound {
            bytes: input_bytes,
            rows: 1,
            values: 100,
        };
        let trace = TraceLimits {
            max_flows: 1,
            max_hops: 0,
            max_output_bytes: 8192,
        };
        let flows = flowtracer::trace_bounded(&nodes, &[], trace).unwrap();
        let ordinary = compile_spec(
            &nodes,
            &[],
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let admitted = logical_compile_bound(graph, trace, compile()).unwrap();
        let security =
            crate::security_projection_bound(graph.bytes, graph.rows, graph.values).unwrap();
        for descriptor in selected::artifact_catalog() {
            let selected = selected::compile_spec_selected_with_security(
                &nodes,
                &[],
                &flows,
                ExportMode::BestEffort,
                &BTreeSet::new(),
                &[descriptor.id],
                compile(),
                crate::SecurityLimits {
                    max_rows: security.rows,
                    max_output_bytes: security.bytes,
                },
            )
            .unwrap();
            if let SelectedArtifactOutcome::Produced(artifact) = &selected[0] {
                assert_eq!(
                    ordinary.artifacts.iter().find(|a| a.id == artifact.id),
                    Some(artifact)
                );
                assert!(count(artifact).unwrap() <= admitted.artifact_bytes);
            }
        }
    }
}
