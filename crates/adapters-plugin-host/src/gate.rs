//! Conformance gate (#200, AC-0068): a plugin may only join extraction
//! after proving, against its own generator-supplied golden corpus, that
//! it (1) honors the SPI contract under the standard bounds, (2) emits
//! exactly the expected facts with exact evidence spans, and (3) is
//! deterministic across a double run, with (4) every declared coverage cell
//! backed by a golden case it names (AC-0263). Anything less and the adapter
//! stays in the proposed state — discovery and enablement never bypass this.

use crate::{HostError, PluginExtraction, PluginHost, PluginLimits, SourceId};
use serde::{Deserialize, Serialize};

/// One golden case: source text in, the exact expected facts out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenCase {
    /// Repo-relative path handed to the plugin.
    pub path: String,
    /// UTF-8 source content.
    pub source: String,
    /// Expected nodes, exact (id, label, props) — evidence spans included.
    #[serde(default)]
    pub nodes: Vec<ExpectedNode>,
    /// Expected edges, exact (src, dst, label, props).
    #[serde(default)]
    pub edges: Vec<ExpectedEdge>,
}

/// Expected node shape; `props` compares exactly (JSON equality).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedNode {
    /// Expected node id.
    pub id: String,
    /// Expected node label.
    pub label: String,
    /// Expected props, compared by exact JSON equality.
    #[serde(default)]
    pub props: serde_json::Value,
}

/// Expected edge shape; `props` compares exactly (JSON equality).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedEdge {
    /// Expected source node id.
    pub src: String,
    /// Expected target node id.
    pub dst: String,
    /// Expected edge label.
    pub label: String,
    /// Expected props, compared by exact JSON equality.
    #[serde(default)]
    pub props: serde_json::Value,
}

/// The generator-supplied corpus shipped next to the artifact as
/// `{plugin-id}.golden.json`. `extensions` declares which source files the
/// adapter claims once gated (#201 routing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenCorpus {
    /// Source-file extensions the adapter claims once gated (#201).
    #[serde(default)]
    pub extensions: Vec<String>,
    /// The golden cases; an empty corpus fails the gate closed.
    pub cases: Vec<GoldenCase>,
    /// Coverage-matrix cells the plugin claims (AC-0263). Each supported or
    /// partial cell names the golden cases that prove it, so it counts only
    /// when the gate passes those cases.
    #[serde(default)]
    pub coverage: Vec<DeclaredCell>,
    /// Language names for claimed extensions the core cannot name (AC-0263),
    /// e.g. `{"foo": "Foo"}`; cells are reported under the language their
    /// file's extension names.
    #[serde(default)]
    pub languages: std::collections::BTreeMap<String, String>,
}

/// How far a declared cell is covered — the same scale as compiled-in
/// adapters' matrix cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredSupport {
    /// Recognized; the named cases prove the canonical shape.
    Supported,
    /// Recognized for some shapes; `note` names what is missed.
    Partial,
    /// Not recognized; `note` names the gap. Names no cases.
    Unsupported,
}

/// One language × framework × capability cell a plugin declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredCell {
    /// Language the plugin adapts.
    pub language: String,
    /// Framework or library, or `core` for language-level capabilities.
    pub framework: String,
    /// Capability in snake_case; plugins may name capabilities the core
    /// does not, which is how new kinds of facts become visible.
    pub capability: String,
    /// Declared support level.
    pub support: DeclaredSupport,
    /// Shortfall; required for partial and unsupported, empty otherwise.
    #[serde(default)]
    pub note: String,
    /// Paths of the golden cases proving a supported or partial cell.
    #[serde(default)]
    pub cases: Vec<String>,
}

/// Key a gate verdict binds to: the artifact hash *and* the exact corpus
/// bytes. The corpus carries the extension and coverage claims, so editing it
/// after gating leaves the plugin ungated rather than trusting new claims.
pub fn gate_key(content_hash: &str, corpus: Option<&[u8]>) -> String {
    let mut bytes = b"cartograph:plugin-gate:v2\0".to_vec();
    bytes.extend_from_slice(content_hash.as_bytes());
    bytes.push(0);
    match corpus {
        Some(corpus) => {
            bytes.push(1);
            bytes.extend_from_slice(corpus);
        }
        None => bytes.push(0),
    }
    core_prov::content_hash(&bytes)
}

/// First problem with the coverage declarations, if any.
fn declaration_problem(corpus: &GoldenCorpus) -> Option<String> {
    // Path → whether the case asserts any fact; an empty-output case is a
    // negative case and cannot on its own prove a claim.
    let asserts: std::collections::BTreeMap<&str, bool> = corpus
        .cases
        .iter()
        .map(|case| {
            let any = !case.nodes.is_empty() || !case.edges.is_empty();
            (case.path.as_str(), any)
        })
        .collect();
    if let Some((extension, _)) = corpus.languages.iter().find(|(extension, language)| {
        !corpus.extensions.contains(extension) || language.trim().is_empty()
    }) {
        return Some(format!(
            "languages.{extension}: names a claimed extension and a language"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for cell in &corpus.coverage {
        let id = format!("{}.{}.{}", cell.language, cell.framework, cell.capability);
        let snake = !cell.capability.is_empty()
            && cell
                .capability
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if cell.language.trim().is_empty() || cell.framework.trim().is_empty() || !snake {
            return Some(format!(
                "{id}: language, framework and snake_case capability required"
            ));
        }
        // Languages match case-insensitively downstream, so `Ruby` and
        // `ruby` are the same cell.
        let language = cell.language.to_ascii_lowercase();
        if !seen.insert((language, &cell.framework, &cell.capability)) {
            return Some(format!("{id}: declared twice"));
        }
        let noted = !cell.note.trim().is_empty();
        let problem = match cell.support {
            DeclaredSupport::Supported if noted => Some("a limited cell must be partial"),
            DeclaredSupport::Partial | DeclaredSupport::Unsupported if !noted => {
                Some("needs a note naming what is missed")
            }
            DeclaredSupport::Unsupported if !cell.cases.is_empty() => {
                Some("an unsupported cell names no cases")
            }
            DeclaredSupport::Supported | DeclaredSupport::Partial if cell.cases.is_empty() => {
                Some("names no golden case proving it")
            }
            _ => None,
        };
        if let Some(problem) = problem {
            return Some(format!("{id}: {problem}"));
        }
        if let Some(missing) = cell
            .cases
            .iter()
            .find(|path| !asserts.contains_key(path.as_str()))
        {
            return Some(format!("{id}: names unknown case {missing}"));
        }
        let proven = cell.cases.iter().any(|path| asserts[path.as_str()]);
        if !cell.cases.is_empty() && !proven {
            return Some(format!(
                "{id}: names only cases that assert no nodes or edges"
            ));
        }
    }
    None
}

/// One named gate check with its verdict and a human-readable detail.
#[derive(Debug, Clone, Serialize)]
pub struct GateCheck {
    /// Stable check name, e.g. `spi-compiles`, `golden:src/lib.rs`.
    pub name: String,
    /// Whether this check passed.
    pub passed: bool,
    /// Human-readable evidence or failure reason.
    pub detail: String,
}

/// The whole gate outcome. `passed` is the conjunction of every check —
/// one failure keeps the adapter proposed.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    /// Conjunction of every check.
    pub passed: bool,
    /// Every check run, pass or fail, in execution order.
    pub checks: Vec<GateCheck>,
}

fn canonical(extraction: &PluginExtraction) -> Vec<String> {
    let mut facts: Vec<String> = extraction
        .nodes
        .iter()
        .map(|node| format!("node:{}:{}:{}", node.id, node.label, node.props))
        .chain(extraction.edges.iter().map(|edge| {
            format!(
                "edge:{}:{}:{}:{}",
                edge.src, edge.dst, edge.label, edge.props
            )
        }))
        .collect();
    facts.sort();
    facts
}

/// Expected facts, expressed exactly as the host will return them: built
/// into a `PluginExtraction`, pinned with the same identity, and given the
/// same host-built provenance — golden authors never hand-write
/// `plugin_artifact_hash`, the pinned extractor id, the content hash, or
/// baseline `prov`. An expected `prov` is held to the same rules as a
/// guest's (AC-0594), so a corpus cannot expect what the host rejects.
fn expected_canonical(
    case: &GoldenCase,
    plugin_id: &str,
    artifact_hash: &str,
    source_id: &SourceId,
) -> Result<Vec<String>, HostError> {
    let mut expected = PluginExtraction {
        nodes: case
            .nodes
            .iter()
            .map(|node| core_graph::Node {
                id: node.id.clone(),
                label: node.label.clone(),
                props: node.props.clone(),
            })
            .collect(),
        edges: case
            .edges
            .iter()
            .map(|edge| core_graph::Edge {
                src: edge.src.clone(),
                dst: edge.dst.clone(),
                label: edge.label.clone(),
                props: edge.props.clone(),
            })
            .collect(),
    };
    crate::enforce_provenance(
        &mut expected,
        plugin_id,
        artifact_hash,
        case.source.as_bytes(),
        &case.path,
        source_id,
    )?;
    Ok(canonical(&expected))
}

/// Run the full conformance gate for one artifact against its corpus.
/// Never partial: any bound violation, contract error, golden mismatch, or
/// nondeterminism fails the gate closed with the reason named.
pub fn run_gate(
    host: &PluginHost,
    plugin_id: &str,
    wasm_bytes: &[u8],
    corpus: &GoldenCorpus,
    limits: PluginLimits,
    source_id: &SourceId,
) -> GateReport {
    let mut checks: Vec<GateCheck> = Vec::new();
    let mut check = |name: &str, passed: bool, detail: String| {
        checks.push(GateCheck {
            name: name.to_string(),
            passed,
            detail,
        });
        passed
    };

    let plugin = match host.load(wasm_bytes, plugin_id) {
        Ok(plugin) => {
            check("spi-compiles", true, "component compiles".into());
            plugin
        }
        Err(error) => {
            check("spi-compiles", false, error.to_string());
            return GateReport {
                passed: false,
                checks,
            };
        }
    };

    if !check(
        "corpus-nonempty",
        !corpus.cases.is_empty(),
        format!("{} golden case(s)", corpus.cases.len()),
    ) {
        return GateReport {
            passed: false,
            checks,
        };
    }

    // Coverage claims must each be backed by a case this gate runs; since
    // every case must pass for the gate to pass, a gated plugin's supported
    // and partial cells are proven (AC-0263).
    let problem = declaration_problem(corpus);
    check(
        "coverage-declarations",
        problem.is_none(),
        problem.unwrap_or_else(|| format!("{} declared cell(s)", corpus.coverage.len())),
    );

    let mut first_runs: Vec<Option<PluginExtraction>> = Vec::new();
    for case in &corpus.cases {
        let run: Result<PluginExtraction, HostError> = host.call_extract(
            &plugin,
            case.source.as_bytes(),
            &case.path,
            source_id,
            limits,
        );
        match run {
            Ok(extraction) => {
                check(
                    &format!("contract:{}", case.path),
                    true,
                    "extract-source honored the SPI within bounds".into(),
                );
                let got = canonical(&extraction);
                let want = expected_canonical(case, plugin_id, plugin.artifact_hash(), source_id);
                let matched = want.as_ref().is_ok_and(|want| *want == got);
                check(
                    &format!("golden:{}", case.path),
                    matched,
                    match &want {
                        Ok(_) if matched => "facts match the golden expectation exactly".into(),
                        Ok(want) => {
                            format!("expected {} fact(s), got {}", want.len(), got.len())
                        }
                        Err(error) => format!("golden expectation is not acceptable: {error}"),
                    },
                );
                first_runs.push(matched.then_some(extraction));
            }
            Err(error) => {
                check(&format!("contract:{}", case.path), false, error.to_string());
                first_runs.push(None);
            }
        }
    }

    // Double-run determinism over the whole corpus: identical canonical
    // facts, case by case.
    let mut deterministic = true;
    let mut detail = "second run identical".to_string();
    for (case, first) in corpus.cases.iter().zip(&first_runs) {
        let Some(first) = first else {
            deterministic = false;
            detail = "skipped: a first-run case already failed".into();
            break;
        };
        match host.call_extract(
            &plugin,
            case.source.as_bytes(),
            &case.path,
            source_id,
            limits,
        ) {
            Ok(second) if canonical(&second) == canonical(first) => {}
            Ok(_) => {
                deterministic = false;
                detail = format!("case {} differed across runs", case.path);
                break;
            }
            Err(error) => {
                deterministic = false;
                detail = format!("case {} failed on rerun: {error}", case.path);
                break;
            }
        }
    }
    check("determinism-double-run", deterministic, detail);

    GateReport {
        passed: checks.iter().all(|check| check.passed),
        checks,
    }
}
