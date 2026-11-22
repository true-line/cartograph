//! Selected artifact dispatch; projection and native renderers are shared.
use super::*;
use core_graph::coverage::StoredCoverage;
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
    /// Complete security projection exceeded its explicit admission.
    SecurityDerivation(crate::SecurityLimitExceeded),
    /// The fact index could not be built: a citation does not resolve or two
    /// facts share one citation id (ADR-0058). Not a size limit; no index is
    /// returned.
    FactIndex(FactIndexFailure),
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

/// What the host supplies beside the graph facts. A bare compile supplies
/// neither, and the artifacts that read them say so.
#[derive(Debug, Clone, Copy, Default)]
pub struct Supplied<'a> {
    /// The coverage inventories stored with the graph (AC-0592).
    pub coverage: &'a [StoredCoverage],
    /// The repositories in `coverage` whose stored inventory the host does
    /// not hold as current (AC-0657). Each is listed as stale with what was
    /// compared, and nothing is read from its inventory. A host that makes
    /// no such comparison supplies none.
    pub stale_coverage: &'a [StaleCoverage<'a>],
    /// The curated projection the host computed for this graph, or why it
    /// computed none (SPEC-12, AC-0653).
    pub curated: Curated<'a>,
    /// The content hash of the graph being compiled, when the host has
    /// it. It is supplied apart from `curated`, so a document header can
    /// state it when no projection was computed (AC-0663).
    pub graph_content_hash: Option<&'a str>,
    /// The source registrations in the graph and the revision each was
    /// recovered at, when the host has them. The bundle manifest states
    /// them (AC-0682); no other artifact reads them.
    pub source_revisions: Option<&'a [crate::SourceRevision]>,
    /// The knowledge-layer Capability and Actor stories the host validated
    /// against this graph (#1443). When present, `user-stories` renders them
    /// beside the base Capabilities, exactly as the MCP read does.
    pub knowledge: Option<&'a capabilities::KnowledgeStories<'a>>,
    /// The display name of each registered repository, by registry key,
    /// when the host has them. `architecture.md` names client applications
    /// and repositories by them (#2049); no other artifact reads them.
    pub repo_names: &'a [(String, String)],
}

/// One stored inventory the host compared with the repository's current
/// state and found recorded under another (AC-0657).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleCoverage<'a> {
    /// The repository the stored inventory belongs to.
    pub repo_key: &'a str,
    /// What the host compared at read time, such as `registration` and
    /// `plugins`. Which of them differs is not known.
    pub compared: &'a [String],
}

/// What an artifact reads beside the admitted graph facts.
#[derive(Clone, Copy, Default)]
pub(crate) struct Beside<'a> {
    /// The coverage inventories stored with the graph.
    pub(crate) coverage: &'a [StoredCoverage],
    /// The repositories whose stored inventory is stale.
    pub(crate) stale_coverage: &'a [StaleCoverage<'a>],
    /// Pipeline facts the export mode and the rejections withheld, counted
    /// before the filter (`pipelines::withheld`).
    pub(crate) pipelines_withheld: usize,
    /// Configuration facts withheld the same way (`configuration::withheld`).
    pub(crate) configuration_withheld: usize,
    /// Boundary facts withheld the same way (`external::withheld`).
    pub(crate) external_withheld: usize,
    /// Environment facts withheld the same way (`environments::withheld`).
    pub(crate) environments_withheld: usize,
    /// Endpoint facts withheld the same way (`endpoint_inventory::withheld`).
    pub(crate) endpoints_withheld: usize,
    /// The curated projection, or why there is none.
    pub(crate) curated: Curated<'a>,
    /// The graph content hash the host supplied, if any.
    pub(crate) graph_content_hash: Option<&'a str>,
    /// The source revisions the host supplied, if any.
    pub(crate) source_revisions: Option<&'a [crate::SourceRevision]>,
    /// Validated knowledge stories the host supplied, if any (#1443).
    pub(crate) knowledge: Option<&'a capabilities::Stories<'a>>,
    /// Repository display names the host supplied, if any (#2049).
    pub(crate) repo_names: &'a [(String, String)],
}

impl<'a> Beside<'a> {
    /// Counted over the stored facts, before the projection and the export
    /// filter drop what the mode and the rejections withhold.
    pub(crate) fn new<N: Borrow<Node>, E: Borrow<Edge>>(
        nodes: &[N],
        edges: &[E],
        mode: ExportMode,
        rejected: &BTreeSet<String>,
        supplied: Supplied<'a>,
    ) -> Self {
        Self {
            coverage: supplied.coverage,
            stale_coverage: supplied.stale_coverage,
            pipelines_withheld: pipelines::withheld(nodes, edges, mode, rejected),
            configuration_withheld: configuration::withheld(nodes, edges, mode, rejected),
            external_withheld: external::withheld(nodes, edges, mode, rejected),
            environments_withheld: environments::withheld(nodes, edges, mode, rejected),
            endpoints_withheld: endpoint_inventory::withheld(nodes, mode, rejected),
            curated: supplied.curated,
            graph_content_hash: supplied.graph_content_hash,
            source_revisions: supplied.source_revisions,
            knowledge: supplied.knowledge.map(|k| &k.0),
            repo_names: supplied.repo_names,
        }
    }
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
        id: "test-arrangements",
        file: "test_arrangements.md",
        title: "Test arrangements",
        format: "markdown",
    },
    Artifact {
        id: "journeys",
        file: "journeys.md",
        title: "Journey candidates",
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
    // After the frozen graph artifacts, so their bytes are unchanged (#1787).
    Artifact {
        id: "endpoint-permissions",
        file: "endpoint-permissions.md",
        title: "Endpoints and permission sites",
        format: "markdown",
    },
    // After it, for the same reason (#1818).
    Artifact {
        id: "pipelines",
        file: "pipelines.md",
        title: "Build and deployment pipelines",
        format: "markdown",
    },
    // After it, for the same reason (#1821).
    Artifact {
        id: "configuration",
        file: "configuration.md",
        title: "Configuration keys",
        format: "markdown",
    },
    // After it, for the same reason (#1788).
    Artifact {
        id: "external",
        file: "external.md",
        title: "Calls and publishes that leave the system boundary",
        format: "markdown",
    },
    // After it, for the same reason (#1791).
    Artifact {
        id: "environments",
        file: "environments.md",
        title: "Environments and the workloads they declare",
        format: "markdown",
    },
    // After it, for the same reason (#1750).
    Artifact {
        id: documentation::ARTIFACT_ID,
        file: "documentation-evidence.md",
        title: "Documentation evidence",
        format: "markdown",
    },
    // After it, for the same reason (#1658).
    Artifact {
        id: "endpoint-inventory",
        file: "endpoint-inventory.md",
        title: "Endpoint inventory",
        format: "markdown",
    },
    // After it, for the same reason (#1746).
    Artifact {
        id: curated::ARTIFACT_ID,
        file: "curated.md",
        title: "Curated items",
        format: "markdown",
    },
    // After it, for the same reason (#2049): the C4 views and their
    // Structurizr DSL workspace.
    Artifact {
        id: architecture::MARKDOWN_ID,
        file: "architecture.md",
        title: "Architecture (C4 levels 1 to 3)",
        format: "markdown",
    },
    Artifact {
        id: architecture::DSL_ID,
        file: "architecture.dsl",
        title: "Architecture as a Structurizr DSL workspace",
        format: "structurizr-dsl",
    },
    // After it: the document's header carries the curated revision, and its
    // frames are cited before the index (#1748, AC-0663).
    Artifact {
        id: document::D1_ARTIFACT_ID,
        file: "product-specification.md",
        title: "Product specification (D1)",
        format: "markdown",
    },
    Artifact {
        id: "coverage",
        file: "coverage.md",
        title: "Coverage",
        format: "markdown",
    },
    Artifact {
        id: "coverage-index",
        file: "coverage.json",
        title: "Coverage index",
        format: "json",
    },
    // What the export was compiled from and which file holds each part of
    // the handoff bundle (AC-0682). It reads no fact and cites none.
    Artifact {
        id: manifest::ARTIFACT_ID,
        file: "bundle-manifest.md",
        title: "Bundle manifest",
        format: "markdown",
    },
    // Last: the index lists what the artifacts before it cite (ADR-0058).
    Artifact {
        id: fact_index::MARKDOWN_ID,
        file: "fact-index.md",
        title: "Fact and evidence index",
        format: "markdown",
    },
    Artifact {
        id: fact_index::JSON_ID,
        file: "fact-index.json",
        title: "Fact and evidence index data",
        format: "json",
    },
];

/// Metadata for a supported official producer, not a claim that its output
/// exists in a read session. Content identity is available only after successful
/// selected compilation; limited outcomes never acquire a fabricated identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactDescriptor {
    /// Official selection key accepted by the compiler.
    pub id: &'static str,
    /// Portable output file name.
    pub file_name: &'static str,
    /// Human-readable producer title.
    pub title: &'static str,
    /// Output format, such as markdown or json.
    pub format: &'static str,
    /// Versioned schema expected for this producer's completed output.
    pub schema: String,
}

/// The authoritative finite producer catalog in official compilation order.
/// This performs no graph reads, derivation or artifact construction.
pub fn artifact_catalog() -> Vec<ArtifactDescriptor> {
    ARTIFACTS
        .iter()
        .map(|artifact| ArtifactDescriptor {
            id: artifact.id,
            file_name: artifact.file,
            title: artifact.title,
            format: artifact.format,
            schema: SpecArtifact::expected_schema(artifact.id),
        })
        .collect()
}

/// The recovered facts underlying official artifact compilation, in stable order.
/// Reuses export mode and rejection policy, including suppression of relations
/// touching hidden BusinessRule nodes. No accepted-proposal projection or derived
/// ADR/security facts are added. References preserve the original stored bytes;
/// callers must separately identify raw and filtered snapshots and admit inputs
/// under their graph limits before calling this allocation-bounded-by-input helper.
pub fn project_recovered_facts<'a>(
    nodes: &'a [Node],
    edges: &'a [Edge],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> (Vec<&'a Node>, Vec<&'a Edge>) {
    let selected_nodes = filter_nodes(nodes, mode, rejected);
    let all_rules: BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.label == "BusinessRule")
        .map(|n| n.id.as_str())
        .collect();
    let visible: BTreeSet<_> = selected_nodes.iter().map(|n| n.id.as_str()).collect();
    let selected_edges = filter_edges(edges, mode, rejected)
        .into_iter()
        .filter(|e| {
            let touches = all_rules.contains(e.src.as_str()) || all_rules.contains(e.dst.as_str());
            !touches || (visible.contains(e.src.as_str()) && visible.contains(e.dst.as_str()))
        })
        .collect();
    (selected_nodes, selected_edges)
}

/// Selected graph facts stay borrowed; only derived ADR, drift and security
/// facts are owned, so a projection never clones the graph (#725).
pub(crate) struct Projection<'a> {
    pub(crate) nodes: Vec<Cow<'a, Node>>,
    pub(crate) edges: Vec<Cow<'a, Edge>>,
    pub(crate) flows: Vec<Flow>,
}
pub(crate) fn project<'a>(
    nodes: &'a [Node],
    edges: &'a [Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    adr: Option<crate::AdrLimits>,
) -> Result<Projection<'a>, crate::AdrLimitExceeded> {
    project_with_security(nodes, edges, flows, mode, rejected, adr, None).map_err(|limit| {
        match limit {
            SelectedArtifactLimit::AdrDerivation(limit) => limit,
            _ => unreachable!("legacy projection has no security/output limit"),
        }
    })
}
fn project_with_security<'a>(
    nodes: &'a [Node],
    edges: &'a [Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    adr: Option<crate::AdrLimits>,
    security: Option<crate::SecurityLimits>,
) -> Result<Projection<'a>, SelectedArtifactLimit> {
    let (base_nodes, base_edges) = project_recovered_facts(nodes, edges, mode, rejected);
    let flows = project_flows(flows, mode, rejected);
    let derived = crate::adr::derive_inner(&base_nodes, &base_edges, &flows, adr)
        .map_err(SelectedArtifactLimit::AdrDerivation)?;
    let security = match security {
        Some(limits) => crate::security::derive_bounded(&base_nodes, &base_edges, limits)
            .map_err(SelectedArtifactLimit::SecurityDerivation)?,
        None => crate::security::derive_security_findings(&base_nodes, &base_edges),
    };
    let mut nodes: Vec<_> = base_nodes.into_iter().map(Cow::Borrowed).collect();
    nodes.extend(derived.nodes.into_iter().chain(security).map(Cow::Owned));
    let mut edges: Vec<_> = base_edges.into_iter().map(Cow::Borrowed).collect();
    edges.extend(derived.edges.into_iter().map(Cow::Owned));
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
    (mode, rejected, stories, beside): (
        ExportMode,
        &BTreeSet<String>,
        Option<&capabilities::Stories<'_>>,
        Beside<'_>,
    ),
    max: Option<usize>,
) -> Result<Cited, render::RenderLimit> {
    let coverage = beside.coverage;
    use producers::Output;
    let Output {
        content,
        assertions,
        subjects,
    } = match a.id {
        "user-stories" => match stories.or(beside.knowledge) {
            Some(stories) => {
                producers::stories_with_support(nodes, Some(stories), (mode, rejected), max)?
            }
            None => producers::stories(nodes, max)?,
        },
        "flow-dossiers" => producers::flow(nodes, edges, flows, max)?,
        "topology" => producers::topology(nodes, edges, max)?,
        "data-model" => producers::data(nodes, edges, max)?,
        "adrs" => producers::adrs(nodes, edges, max)?,
        "drift-register" => producers::drift(nodes, edges, max)?,
        "security-view" => producers::security(
            (nodes, edges),
            stories.or(beside.knowledge),
            (mode, rejected),
            max,
        )?,
        "toolchain" => producers::toolchain(nodes, edges, max)?,
        "test-arrangements" => producers::tests(nodes, edges, max)?,
        // Journeys are derived after curation, so mode and rejection apply
        // to the candidates themselves as well as to their inputs.
        "journeys" => producers::journeys(nodes, edges, flows, (mode, rejected), max)?,
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
                subjects: output.subjects,
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
                subjects: output.subjects,
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
                subjects: output.subjects,
            }
        }
        // A permission site carries its own provenance inside the endpoint
        // fact, so mode and rejection apply to it here (AC-0599).
        "endpoint-permissions" => {
            endpoint_permissions::output(nodes, edges, (mode, rejected), max)?
        }
        // Pipeline facts in the admitted graph, the count the mode and the
        // rejections withheld, and the files the inventories record as
        // present and not read (AC-0650).
        "pipelines" => pipelines::output(
            nodes,
            edges,
            coverage,
            (mode, beside.pipelines_withheld),
            max,
        )?,
        // Environment-file and configuration-read facts in the admitted
        // graph, and the count the mode and the rejections withheld. No
        // stored inventory is read (AC-0651).
        "configuration" => {
            configuration::output(nodes, edges, (mode, beside.configuration_withheld), max)?
        }
        // Outbound-call Gaps, observed clients and channels in the admitted
        // graph, the admitted flows, and the count the mode and the
        // rejections withheld. No stored inventory is read (AC-0660).
        "external" => external::output(nodes, edges, flows, (mode, beside.external_withheld), max)?,
        // Environment, Workload and environment-Gap facts in the admitted
        // graph with their relations, and the count the mode and the
        // rejections withheld. No stored inventory is read (AC-0661).
        "environments" => {
            environments::output(nodes, edges, (mode, beside.environments_withheld), max)?
        }
        // Document and section facts in the admitted graph with the facts
        // their exact references name, and the documentation drift counts
        // (AC-0681). No stored inventory is read.
        documentation::ARTIFACT_ID => documentation::output(nodes, edges, mode, max)?,
        // Endpoint facts in the admitted graph and the count the mode and
        // the rejections withheld. No stored inventory is read (AC-0694).
        "endpoint-inventory" => {
            endpoint_inventory::output(nodes, (mode, beside.endpoints_withheld), max)?
        }
        // The curated projection the host supplied, the admitted nodes its
        // items cite and the admitted facts its findings cite (AC-0653,
        // AC-0845). No stored inventory is read.
        curated::ARTIFACT_ID => curated::output((nodes, edges), beside.curated, mode, max)?,
        // The admitted facts and flows, the subsystem areas of the supplied
        // knowledge stories and the repository names the host supplied
        // (#2049). No stored inventory is read.
        architecture::MARKDOWN_ID | architecture::DSL_ID => {
            let model = architecture::Model::new(
                nodes,
                edges,
                flows,
                &architecture::Supplied {
                    knowledge: stories.or(beside.knowledge),
                    repo_names: beside.repo_names,
                    mode,
                },
            );
            if a.id == architecture::MARKDOWN_ID {
                architecture::markdown(&model, mode, max)?
            } else {
                architecture::dsl(&model, max)?
            }
        }
        // The catalogue's D1 over the admitted nodes and flows, the gap
        // count and the supplied projection's revision (AC-0663).
        document::D1_ARTIFACT_ID => document::d1(
            (nodes, edges),
            flows,
            gaps,
            (beside.curated, beside.graph_content_hash),
            beside.knowledge,
            (mode, rejected),
            max,
        )?,
        // The official file list and what the host supplied as the as-of
        // (AC-0682). No fact is read.
        manifest::ARTIFACT_ID => {
            let files: Vec<_> = ARTIFACTS.iter().map(|a| (a.id, a.file)).collect();
            manifest::output(
                &files,
                manifest::AsOf {
                    mode,
                    curated: beside.curated,
                    graph_content_hash: beside.graph_content_hash,
                    source_revisions: beside.source_revisions,
                },
                max,
            )?
        }
        // The stored inventories only: no graph fact, mode or rejection applies.
        "coverage" | "coverage-index" => {
            coverage::output(coverage, beside.stale_coverage, a.id == "coverage", max)?
        }
        // The two index artifacts are rendered from the collected citations.
        _ => unreachable!("validated static artifact id"),
    };
    Ok(Cited {
        artifact: identified_artifact(a.id, a.file, a.title, a.format, content, assertions),
        subjects,
    })
}
/// A produced artifact with the fact-key shape of each assertion's subject,
/// which the artifact itself does not serialize (ADR-0058).
struct Cited {
    artifact: SpecArtifact,
    subjects: Vec<fact_index::Subject>,
}
/// One of the two index artifacts over a finished index.
fn index_artifact(
    a: &Artifact,
    index: &FactIndex,
    max: Option<usize>,
) -> Result<SpecArtifact, render::RenderLimit> {
    let content = fact_index::render(index, a.id == fact_index::MARKDOWN_ID, max)?;
    Ok(identified_artifact(
        a.id,
        a.file,
        a.title,
        a.format,
        content,
        Vec::new(),
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
///
/// The two fact-index artifacts list what the whole export cites (ADR-0058),
/// so selecting either also compiles every artifact that carries assertions,
/// one at a time, as workspace: an unselected one is read for its citations
/// and dropped, is not returned and is not charged to `max_bytes`. The index
/// itself is admitted against the remaining output budget like any other
/// artifact. Citations that do not resolve are
/// [`SelectedArtifactLimit::FactIndex`].
pub fn compile_spec_selected(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    requested_ids: &[&str],
    limits: SelectedCompileLimits,
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    compile_selected(
        nodes,
        edges,
        flows,
        mode,
        rejected,
        (requested_ids, limits, None, None, Supplied::default()),
    )
}

/// [`compile_spec_selected`] with the stored coverage inventories the
/// `coverage` and `coverage-index` artifacts project (AC-0592). Every other
/// artifact is unchanged by them.
#[allow(
    clippy::too_many_arguments,
    reason = "additive coverage input preserves the existing selected compiler arguments"
)]
pub fn compile_spec_selected_with_coverage(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    requested_ids: &[&str],
    limits: SelectedCompileLimits,
    coverage: &[StoredCoverage],
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    compile_spec_selected_supplied(
        nodes,
        edges,
        flows,
        mode,
        rejected,
        requested_ids,
        limits,
        Supplied {
            knowledge: None,
            coverage,
            stale_coverage: &[],
            curated: Curated::NotSupplied,
            graph_content_hash: None,
            source_revisions: None,
            repo_names: &[],
        },
    )
}

/// [`compile_spec_selected`] with everything the host supplies beside the
/// graph: the stored coverage inventories (AC-0592) and the curated
/// projection the `curated` artifact lists (AC-0653). Every other artifact is
/// unchanged by them, except the fact index, which lists what `curated`
/// cites.
#[allow(
    clippy::too_many_arguments,
    reason = "additive host input preserves the existing selected compiler arguments"
)]
pub fn compile_spec_selected_supplied(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    requested_ids: &[&str],
    limits: SelectedCompileLimits,
    supplied: Supplied<'_>,
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    compile_selected(
        nodes,
        edges,
        flows,
        mode,
        rejected,
        (requested_ids, limits, None, None, supplied),
    )
}

/// Selected compilation with additional complete security-output admission.
/// The legacy selected compiler retains its original security behavior. All
/// produced artifacts preserve ordinary schemas, hashes, content and order.
#[allow(
    clippy::too_many_arguments,
    reason = "additive security admission preserves the existing selected compiler arguments"
)]
pub fn compile_spec_selected_with_security(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    requested_ids: &[&str],
    limits: SelectedCompileLimits,
    security: crate::SecurityLimits,
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    compile_selected(
        nodes,
        edges,
        flows,
        mode,
        rejected,
        (
            requested_ids,
            limits,
            Some(security),
            None,
            Supplied::default(),
        ),
    )
}
/// Selected compilation with security-output admission and supplied
/// inputs: the MCP read of `security-view` frames its per-Capability access
/// table with the overlay's derived Capabilities (AC-0853).
#[allow(
    clippy::too_many_arguments,
    reason = "additive security admission preserves the existing selected compiler arguments"
)]
pub fn compile_spec_selected_with_security_supplied(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    requested_ids: &[&str],
    limits: SelectedCompileLimits,
    (security, supplied): (crate::SecurityLimits, Supplied<'_>),
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    compile_selected(
        nodes,
        edges,
        flows,
        mode,
        rejected,
        (requested_ids, limits, Some(security), None, supplied),
    )
}
fn compile_selected(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    selection: (
        &[&str],
        SelectedCompileLimits,
        Option<crate::SecurityLimits>,
        Option<&capabilities::Stories<'_>>,
        Supplied<'_>,
    ),
) -> Result<Vec<SelectedArtifactOutcome>, ArtifactSelectionError> {
    // Every compile under test reads a compacted base, so a reader of a
    // rule payload that skips `rule_payload` fails (#2215).
    #[cfg(test)]
    let compacted = crate::rules::compacted_for_tests(nodes);
    #[cfg(test)]
    let nodes = compacted.as_slice();
    let (requested_ids, limits, security, stories, supplied) = selection;
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
    let projection = match project_with_security(
        nodes,
        edges,
        flows,
        mode,
        rejected,
        Some(limits.adr),
        security,
    ) {
        Ok(value) => value,
        Err(limit) => {
            return Ok(selected
                .into_iter()
                .map(|a| SelectedArtifactOutcome::Limited {
                    artifact_id: a.id.into(),
                    limit,
                })
                .collect());
        }
    };
    let beside = Beside::new(nodes, edges, mode, rejected, supplied);
    let nodes = filter_nodes(&projection.nodes, mode, rejected);
    let edges = filter_edges(&projection.edges, mode, rejected);
    let gaps = count_gap_findings(nodes.iter().copied(), edges.iter().copied());
    let mut remaining = limits.max_bytes;
    // The index lists what the whole export cites, whatever else was
    // selected, so that it equals the bundle's. A citing artifact that was
    // not selected is compiled, read for its citations and dropped before
    // the next: it is workspace, never output, so it is not charged to the
    // output budget and nothing unselected is retained (#1582).
    let mut citations = if selected.iter().any(|a| fact_index::is_index(a.id)) {
        Citations::Collecting(fact_index::Collector::new(mode, citation_id))
    } else {
        Citations::Unwanted
    };
    let mut outcomes = Vec::with_capacity(selected.len());
    for a in ARTIFACTS {
        let is_selected = selected.iter().any(|s| s.id == a.id);
        if fact_index::is_index(a.id) {
            if !is_selected {
                continue;
            }
            citations = citations.finished();
            let limit = match &citations {
                Citations::Finished(index) => match index_artifact(a, index, Some(remaining)) {
                    Ok(artifact) => {
                        remaining = remaining
                            .checked_sub(bytes(&artifact))
                            .expect("renderer admitted cumulative output");
                        outcomes.push(SelectedArtifactOutcome::Produced(artifact));
                        continue;
                    }
                    Err(_) => SelectedArtifactLimit::OutputBytes { remaining },
                },
                Citations::TooLarge => SelectedArtifactLimit::OutputBytes { remaining },
                Citations::Failed(failure) => SelectedArtifactLimit::FactIndex(*failure),
                Citations::Unwanted | Citations::Collecting(_) => {
                    unreachable!("a selected index finishes its citations")
                }
            };
            outcomes.push(SelectedArtifactOutcome::Limited {
                artifact_id: a.id.into(),
                limit,
            });
            continue;
        }
        let produce = |max| {
            produce(
                a,
                &nodes,
                &edges,
                &projection.flows,
                gaps,
                (mode, rejected, stories, beside),
                max,
            )
        };
        let produced = is_selected.then(|| produce(Some(remaining)));
        if let Citations::Collecting(collector) = &mut citations
            && fact_index::cites(a.id)
        {
            let cited = match &produced {
                Some(Ok(cited)) => {
                    collector.cite(a.id, &cited.artifact.assertions, &cited.subjects)
                }
                // Not selected, or selected and past the output budget: its
                // citations still belong to the export.
                Some(Err(_)) | None => {
                    let cited = produce(None).expect("unbounded native artifact");
                    collector.cite(a.id, &cited.artifact.assertions, &cited.subjects)
                }
            };
            match cited {
                // Already certain not to fit: stop collecting rows.
                Ok(()) if collector.lower_bound() > remaining => citations = Citations::TooLarge,
                Ok(()) => {}
                Err(error) => citations = Citations::Failed(error.failure()),
            }
        }
        match produced {
            Some(Ok(cited)) => {
                remaining = remaining
                    .checked_sub(bytes(&cited.artifact))
                    .expect("renderer admitted cumulative output");
                outcomes.push(SelectedArtifactOutcome::Produced(cited.artifact));
            }
            Some(Err(_)) => outcomes.push(SelectedArtifactOutcome::Limited {
                artifact_id: a.id.into(),
                limit: SelectedArtifactLimit::OutputBytes { remaining },
            }),
            None => {}
        }
    }
    Ok(outcomes)
}
/// The citations of a selected compilation, on the way to its index.
enum Citations {
    /// No index artifact was selected.
    Unwanted,
    Collecting(fact_index::Collector),
    Finished(FactIndex),
    /// The rows collected so far already exceed the output budget, which
    /// only shrinks: the index is refused when its turn comes.
    TooLarge,
    /// The citations do not resolve: no index can be returned.
    Failed(FactIndexFailure),
}
impl Citations {
    fn finished(self) -> Self {
        match self {
            Self::Collecting(collector) => match collector.finish() {
                Ok(index) => Self::Finished(index),
                Err(error) => Self::Failed(error.failure()),
            },
            other => other,
        }
    }
}
pub(super) fn all<A>(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    supplied: Supplied<'_>,
    keep: impl FnMut(SpecArtifact) -> A,
) -> Result<SpecBundle<A>, FactIndexError> {
    // Every compile under test reads a compacted base, so a reader of a
    // rule payload that skips `rule_payload` fails (#2215).
    #[cfg(test)]
    let compacted = crate::rules::compacted_for_tests(nodes);
    #[cfg(test)]
    let nodes = compacted.as_slice();
    let projection =
        project(nodes, edges, flows, mode, rejected, None).expect("unbounded ADR derivation");
    let beside = Beside::new(nodes, edges, mode, rejected, supplied);
    let nodes = filter_nodes(&projection.nodes, mode, rejected);
    let edges = filter_edges(&projection.edges, mode, rejected);
    try_bundle_from_filtered_kept(
        &nodes,
        &edges,
        &projection.flows,
        mode,
        rejected,
        beside,
        keep,
    )
}
pub(crate) fn flow_artifact_from_filtered(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> SpecArtifact {
    let descriptor = ARTIFACTS
        .iter()
        .find(|artifact| artifact.id == "flow-dossiers")
        .expect("official flow dossier descriptor");
    produce(
        descriptor,
        nodes,
        edges,
        flows,
        0,
        (mode, rejected, None, Beside::default()),
        None,
    )
    .expect("unbounded flow dossier renderer")
    .artifact
}

/// The complete bundle, or the reason its citations do not resolve
/// (ADR-0058 decision 6): nothing is returned for an export whose own
/// assertions cite an id without a row or share an id between two facts.
/// `keep(artifact)` is what is kept of each artifact, as it is compiled
/// (#2215).
pub(crate) fn try_bundle_from_filtered_kept<A>(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    beside: Beside<'_>,
    keep: impl FnMut(SpecArtifact) -> A,
) -> Result<SpecBundle<A>, FactIndexError> {
    bundle_kept(
        nodes,
        edges,
        flows,
        (mode, rejected, beside),
        citation_id,
        keep,
    )
}

fn cite(
    citations: &mut Option<fact_index::Collector>,
    artifact_id: &str,
    assertions: &[SpecAssertion],
    subjects: &[fact_index::Subject],
) -> Result<(), FactIndexError> {
    citations
        .as_mut()
        .expect("every citing artifact precedes the index")
        .cite(artifact_id, assertions, subjects)
}

/// [`try_bundle_from_filtered_kept`] with the id function named, so a test
/// can force two facts onto one id.
#[cfg(test)]
pub(super) fn bundle_with_citation_ids(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    (mode, rejected, beside): (ExportMode, &BTreeSet<String>, Beside<'_>),
    id_of: fn(&core_graph::FactKey) -> String,
) -> Result<SpecBundle, FactIndexError> {
    bundle_kept(nodes, edges, flows, (mode, rejected, beside), id_of, |a| a)
}

/// The bundle loop: each artifact is cited, counted and handed to `keep`
/// before the next is compiled.
fn bundle_kept<A>(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    (mode, rejected, beside): (ExportMode, &BTreeSet<String>, Beside<'_>),
    id_of: fn(&core_graph::FactKey) -> String,
    mut keep: impl FnMut(SpecArtifact) -> A,
) -> Result<SpecBundle<A>, FactIndexError> {
    let mut assertion_count = 0;
    let mut push = |artifacts: &mut Vec<A>, artifact: SpecArtifact| {
        assertion_count += artifact.assertions.len();
        artifacts.push(keep(artifact));
    };
    let mut citations = Some(fact_index::Collector::new(mode, id_of));
    let mut finished: Option<FactIndex> = None;
    let gap_count = count_gap_findings(nodes.iter().copied(), edges.iter().copied());
    let drift_count = nodes.iter().filter(|n| is_drift_node(n)).count();
    let security_count = nodes
        .iter()
        .filter(|n| n.label == "Finding" && n.props["kind"].as_str() == Some("security"))
        .count();
    // Each Markdown register and its JSON index come from one producer pass;
    // the index artifact follows its Markdown artifact in ARTIFACTS (#1478).
    let mut artifacts = Vec::with_capacity(ARTIFACTS.len());
    let mut pending_index: Option<(&str, String)> = None;
    for a in ARTIFACTS {
        if fact_index::is_index(a.id) {
            // Every citing artifact precedes the index in ARTIFACTS.
            if let Some(collector) = citations.take() {
                finished = Some(collector.finish()?);
            }
            push(
                &mut artifacts,
                index_artifact(a, finished.as_ref().expect("finished index"), None)
                    .expect("unbounded native artifact"),
            );
            continue;
        }
        if let Some((id, content)) = pending_index.take() {
            assert_eq!(a.id, id, "paired index follows its Markdown artifact");
            push(
                &mut artifacts,
                identified_artifact(a.id, a.file, a.title, a.format, content, Vec::new()),
            );
            continue;
        }
        let pair = match a.id {
            "us-tm" => Some((
                "us-tm-index",
                registers::matrix_selected(edges, mode, true, true, None)
                    .map(|o| (o.markdown, o.index, o.assertions, o.subjects)),
            )),
            "gap-register" => Some((
                "gap-register-index",
                registers::gap_selected(nodes, edges, flows, gap_count, mode, true, true, None)
                    .map(|o| (o.markdown, o.index, o.assertions, o.subjects)),
            )),
            "rule-evidence" => Some((
                "rule-evidence-index",
                crate::rules::inventory_selected(
                    nodes,
                    edges,
                    mode,
                    crate::rules::InventorySelection {
                        markdown: true,
                        index: true,
                    },
                    None,
                )
                .map(|o| (o.markdown, o.index, o.assertions, o.subjects))
                .map_err(|_| registers::RegisterLimit),
            )),
            _ => None,
        };
        if let Some((index_id, output)) = pair {
            let (markdown, index, assertions, subjects) =
                output.expect("unbounded native artifact");
            cite(&mut citations, a.id, &assertions, &subjects)?;
            push(
                &mut artifacts,
                identified_artifact(
                    a.id,
                    a.file,
                    a.title,
                    a.format,
                    markdown.expect("paired Markdown content"),
                    assertions,
                ),
            );
            pending_index = Some((index_id, index.expect("paired index content")));
            continue;
        }
        let cited = produce(
            a,
            nodes,
            edges,
            flows,
            gap_count,
            (mode, rejected, None, beside),
            None,
        )
        .expect("unbounded native artifact");
        cite(
            &mut citations,
            a.id,
            &cited.artifact.assertions,
            &cited.subjects,
        )?;
        push(&mut artifacts, cited.artifact);
    }
    assert!(pending_index.is_none(), "paired index artifact missing");
    Ok(SpecBundle {
        mode,
        artifacts,
        assertion_count,
        gap_count,
        drift_count,
        security_count,
        graph_basis: None,
        curated_basis: None,
    })
}

#[cfg(test)]
#[path = "selected_security_tests.rs"]
mod security_tests;

// Same selected dispatcher and official artifact identity; never a second compiler.
pub(super) fn compile_stories_with_support(
    base: &core_graph::RevisionedSnapshot,
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    stories: &capabilities::Stories<'_>,
) -> SelectedArtifactOutcome {
    compile_stories_with_security(base, mode, rejected, limits, stories, None)
}
// Preserve the legacy helper signature, including pending combined-Actor callers.
pub(super) fn compile_stories_with_security(
    base: &core_graph::RevisionedSnapshot,
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    stories: &capabilities::Stories<'_>,
    security: Option<crate::SecurityLimits>,
) -> SelectedArtifactOutcome {
    compile_stories_with_flows_and_security(base, &[], mode, rejected, limits, stories, security)
}
pub(super) fn compile_stories_with_flows_and_security(
    base: &core_graph::RevisionedSnapshot,
    flows: &[Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    stories: &capabilities::Stories<'_>,
    security: Option<crate::SecurityLimits>,
) -> SelectedArtifactOutcome {
    compile_selected(
        &base.graph.0,
        &base.graph.1,
        flows,
        mode,
        rejected,
        (
            &["user-stories"],
            limits,
            security,
            Some(stories),
            Supplied::default(),
        ),
    )
    .expect("official artifact id")
    .remove(0)
}
