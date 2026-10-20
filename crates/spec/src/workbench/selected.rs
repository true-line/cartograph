//! Selected artifact dispatch; projection and native renderers are shared.
use super::*;
use std::{fmt, io};

/// Bounds on selected output and the complete shared ADR derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedCompileLimits {
    /// Cumulative content UTF-8 bytes plus compact complete assertion arrays.
    /// Artifact metadata/envelopes are not included. Failed artifacts consume none.
    pub max_bytes: usize,
    /// Bounds for derived nodes plus edges and complete serialized `AdrFacts`.
    pub adr: crate::AdrLimits,
}
/// A selection rejected before projection or artifact construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSelectionError {
    /// Zero-based requested entry whose id is not an official artifact id.
    UnknownArtifact {
        /// Zero-based position in the raw requested selection.
        index: usize,
    },
}
impl fmt::Display for ArtifactSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownArtifact { index } => {
                write!(f, "unknown artifact at selection index {index}")
            }
        }
    }
}
impl std::error::Error for ArtifactSelectionError {}
/// Resource that prevented a selected artifact from being produced completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedArtifactLimit {
    /// The artifact exceeds the remaining cumulative output budget.
    OutputBytes {
        /// Available cumulative output bytes before attempting this artifact.
        remaining: usize,
    },
    /// Coherent shared projection could not complete within the ADR budget.
    /// This does not report the selected artifact's eventual output size.
    AdrDerivation(crate::AdrLimitExceeded),
}
/// A complete artifact or explicit limit outcome, in official artifact order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectedArtifactOutcome {
    /// Complete ordinary artifact with unchanged schema and content identity.
    Produced(SpecArtifact),
    /// No partial artifact is returned; this attempt consumes no output budget.
    Limited {
        /// Official artifact id for the failed selected producer.
        artifact_id: String,
        /// Construction resource that prevented a complete result.
        limit: SelectedArtifactLimit,
    },
}

struct Artifact {
    id: &'static str,
    file: &'static str,
    title: &'static str,
    format: &'static str,
}
const ARTIFACTS: &[Artifact] = &[
    Artifact {
        id: "user-stories",
        file: "user_stories.md",
        title: "User stories",
        format: "markdown",
    },
    Artifact {
        id: "us-tm",
        file: "US-TM.md",
        title: "US traceability matrix",
        format: "markdown",
    },
    Artifact {
        id: "us-tm-index",
        file: "US-TM.json",
        title: "US traceability matrix index",
        format: "json",
    },
    Artifact {
        id: "flow-dossiers",
        file: "flow_dossiers.md",
        title: "Flow dossiers",
        format: "markdown",
    },
    Artifact {
        id: "topology",
        file: "topology.md",
        title: "Resource topology",
        format: "markdown",
    },
    Artifact {
        id: "data-model",
        file: "data_model.md",
        title: "Data model",
        format: "markdown",
    },
    Artifact {
        id: "adrs",
        file: "adrs.md",
        title: "Architecture decisions",
        format: "markdown",
    },
    Artifact {
        id: "gap-register",
        file: "gap_register.md",
        title: "Gap register",
        format: "markdown",
    },
    Artifact {
        id: "gap-register-index",
        file: "gap_register.json",
        title: "Gap register index",
        format: "json",
    },
    Artifact {
        id: "drift-register",
        file: "drift_register.md",
        title: "Drift register",
        format: "markdown",
    },
    Artifact {
        id: "security-view",
        file: "security.md",
        title: "Security findings",
        format: "markdown",
    },
    Artifact {
        id: "toolchain",
        file: "toolchain.md",
        title: "Toolchain",
        format: "markdown",
    },
    Artifact {
        id: "rule-evidence",
        file: "rule-evidence.md",
        title: "Source rule evidence",
        format: "markdown",
    },
    Artifact {
        id: "rule-evidence-index",
        file: "rule-evidence.json",
        title: "Source rule evidence index",
        format: "json",
    },
];

pub(super) struct Projection {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    flows: Vec<Flow>,
}
pub(super) fn project(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    adr: Option<crate::AdrLimits>,
) -> Result<Projection, crate::AdrLimitExceeded> {
    let base_nodes = filter_nodes(nodes, mode, rejected)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let all_rules: BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.label == "BusinessRule")
        .map(|n| n.id.as_str())
        .collect();
    let visible: BTreeSet<_> = base_nodes.iter().map(|n| n.id.as_str()).collect();
    let base_edges = filter_edges(edges, mode, rejected)
        .into_iter()
        .filter(|e| {
            let touches = all_rules.contains(e.src.as_str()) || all_rules.contains(e.dst.as_str());
            !touches || (visible.contains(e.src.as_str()) && visible.contains(e.dst.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let flows = project_flows(flows, mode, rejected);
    let derived = if let Some(limits) = adr {
        crate::derive_adr_facts_bounded(&base_nodes, &base_edges, &flows, limits)?
    } else {
        crate::derive_adr_facts(&base_nodes, &base_edges, &flows)
    };
    let security = crate::security::derive_security_findings(&base_nodes, &base_edges);
    let mut nodes = base_nodes;
    nodes.extend(derived.nodes);
    nodes.extend(security);
    let mut edges = base_edges;
    edges.extend(derived.edges);
    Ok(Projection {
        nodes,
        edges,
        flows,
    })
}
fn produce(
    a: &Artifact,
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    gaps: usize,
    mode: ExportMode,
    max: Option<usize>,
) -> Result<SpecArtifact, render::RenderLimit> {
    use producers::Output;
    let Output {
        content,
        assertions,
    } = match a.id {
        "user-stories" => producers::stories(nodes, max)?,
        "flow-dossiers" => producers::flow(nodes, edges, flows, max)?,
        "topology" => producers::topology(nodes, edges, max)?,
        "data-model" => producers::data(nodes, edges, max)?,
        "adrs" => producers::adrs(nodes, edges, max)?,
        "drift-register" => producers::drift(nodes, edges, max)?,
        "security-view" => producers::security(nodes, max)?,
        "toolchain" => producers::toolchain(nodes, edges, max)?,
        "us-tm" | "us-tm-index" => {
            let markdown = a.id == "us-tm";
            let output = registers::matrix_selected(edges, mode, markdown, !markdown, max)
                .map_err(|_| render::RenderLimit)?;
            Output {
                content: if markdown {
                    output.markdown
                } else {
                    output.index
                }
                .expect("selected register content"),
                assertions: output.assertions,
            }
        }
        "gap-register" | "gap-register-index" => {
            let markdown = a.id == "gap-register";
            let output =
                registers::gap_selected(nodes, edges, flows, gaps, mode, markdown, !markdown, max)
                    .map_err(|_| render::RenderLimit)?;
            Output {
                content: if markdown {
                    output.markdown
                } else {
                    output.index
                }
                .expect("selected register content"),
                assertions: output.assertions,
            }
        }
        "rule-evidence" | "rule-evidence-index" => {
            let markdown = a.id == "rule-evidence";
            let output = crate::rules::inventory_selected(
                nodes,
                edges,
                mode,
                crate::rules::InventorySelection {
                    markdown,
                    index: !markdown,
                },
                max,
            )
            .map_err(|_| render::RenderLimit)?;
            Output {
                content: if markdown {
                    output.markdown
                } else {
                    output.index
                }
                .expect("selected rule content"),
                assertions: output.assertions,
            }
        }
        _ => unreachable!("validated static artifact id"),
    };
    Ok(identified_artifact(
        a.id, a.file, a.title, a.format, content, assertions,
    ))
}
fn bytes(artifact: &SpecArtifact) -> usize {
    struct Count(usize);
    impl io::Write for Count {
        fn write(&mut self, s: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(s.len())
                .ok_or_else(|| io::Error::other("output bound"))?;
            Ok(s.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(artifact.content.len());
    serde_json::to_writer(&mut count, &artifact.assertions).expect("admitted artifact byte count");
    count.0
}
/// Compile only requested official artifacts with construction-time admission.
///
/// Unknown ids fail before projection. Repeated ids are normalized and results
/// follow the ordinary canonical artifact order, independent of request order.
/// The caller separately admits graph/flow input and selection cardinality.
/// Every success exactly matches ordinary compilation; output/derivation limits
/// return explicit outcomes and never return a truncated artifact.
pub fn compile_spec_selected(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    requested_ids: &[&str],
    limits: SelectedCompileLimits,
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    for (index, id) in requested_ids.iter().enumerate() {
        if !ARTIFACTS.iter().any(|a| a.id == *id) {
            return Err(ArtifactSelectionError::UnknownArtifact { index });
        }
    }
    let selected: Vec<_> = ARTIFACTS
        .iter()
        .filter(|a| requested_ids.contains(&a.id))
        .collect();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let projection = match project(nodes, edges, flows, mode, rejected, Some(limits.adr)) {
        Ok(value) => value,
        Err(limit) => {
            return Ok(selected
                .into_iter()
                .map(|a| SelectedArtifactOutcome::Limited {
                    artifact_id: a.id.into(),
                    limit: SelectedArtifactLimit::AdrDerivation(limit),
                })
                .collect());
        }
    };
    let nodes = filter_nodes(&projection.nodes, mode, rejected);
    let edges = filter_edges(&projection.edges, mode, rejected);
    let gaps = count_gap_findings(nodes.iter().copied(), edges.iter().copied());
    let mut remaining = limits.max_bytes;
    Ok(selected
        .into_iter()
        .map(|a| {
            match produce(
                a,
                &nodes,
                &edges,
                &projection.flows,
                gaps,
                mode,
                Some(remaining),
            ) {
                Ok(artifact) => {
                    remaining = remaining
                        .checked_sub(bytes(&artifact))
                        .expect("renderer admitted cumulative output");
                    SelectedArtifactOutcome::Produced(artifact)
                }
                Err(_) => SelectedArtifactOutcome::Limited {
                    artifact_id: a.id.into(),
                    limit: SelectedArtifactLimit::OutputBytes { remaining },
                },
            }
        })
        .collect())
}
pub(super) fn all(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> SpecBundle {
    let projection =
        project(nodes, edges, flows, mode, rejected, None).expect("unbounded ADR derivation");
    let nodes = filter_nodes(&projection.nodes, mode, rejected);
    let edges = filter_edges(&projection.edges, mode, rejected);
    let gap_count = count_gap_findings(nodes.iter().copied(), edges.iter().copied());
    let drift_count = nodes.iter().filter(|n| is_drift_node(n)).count();
    let security_count = nodes
        .iter()
        .filter(|n| n.label == "Finding" && n.props["kind"].as_str() == Some("security"))
        .count();
    let artifacts: Vec<_> = ARTIFACTS
        .iter()
        .map(|a| {
            produce(a, &nodes, &edges, &projection.flows, gap_count, mode, None)
                .expect("unbounded native artifact")
        })
        .collect();
    let assertion_count = artifacts.iter().map(|a| a.assertions.len()).sum();
    SpecBundle {
        mode,
        artifacts,
        assertion_count,
        gap_count,
        drift_count,
        security_count,
    }
}
