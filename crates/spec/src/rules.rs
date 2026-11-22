//! Stored, typed rule observations → source-evidence inventory (SPEC-02).
//!
//! No source loader is accepted here: all displays came through the producer's
//! sanitizer before persistence. Parsing checks the typed payload, not secrecy.

use crate::workbench::largest_groups;
use crate::{ExportMode, SpecAssertion, provenance};
use core_graph::rules::{
    BranchPolarity, DefinitionExpressionKind, DependencyResolution, GuardedExitEvidence,
    KnownLiteral, LiteralEvidence, LocalExit, RuleValidationError, SourceExpression,
};
use core_graph::{Edge, Node};
use core_prov::{EvidenceRef, Provenance};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub mod journey;
pub(crate) mod render;
pub(crate) mod use_case;
use render::{
    Counter, DisplayString, Escaped, EvidenceText, Formatted, MethodLabel, NestedHeadings,
    TableSafe,
};

// Source-authored strings are escaped while writing; formatting them never
// creates an unadmitted intermediate output string.
fn text(value: impl fmt::Display) -> impl fmt::Display {
    Escaped(value)
}

fn source(reference: &EvidenceRef) -> impl fmt::Display + '_ {
    text(Formatted(move |f: &mut fmt::Formatter<'_>| {
        write!(
            f,
            "{}:{} bytes {}..{} @ {}",
            reference.repo,
            reference.path,
            reference.byte_start,
            reference.byte_end,
            reference.commit_sha
        )
    }))
}

fn literal(value: &Option<LiteralEvidence>) -> impl fmt::Display + '_ {
    Formatted(move |f: &mut fmt::Formatter<'_>| match value {
        None => f.write_str("Not a captured scalar literal"),
        Some(LiteralEvidence::Known { value }) => match value {
            KnownLiteral::Boolean(value) => write!(f, "boolean {value}"),
            KnownLiteral::String(value) => {
                f.write_str("string ")?;
                render::escaped_json_string(f, value)
            }
            KnownLiteral::Number(value) => write!(f, "number {value}"),
            KnownLiteral::Null => f.write_str("null"),
        },
        Some(LiteralEvidence::Withheld { kind, reasons }) => {
            write!(f, "Withheld {kind:?}: {reasons:?}")
        }
    })
}

fn expression(output: &mut impl fmt::Write, value: &SourceExpression) -> fmt::Result {
    writeln!(
        output,
        "- Stored expression: {}\n- Capture: {:?} ({})\n- Literal: {}\n- Expression source: {}\n",
        text(value.display.as_str()),
        value.capture,
        text(&value.syntax_kind),
        literal(&value.literal),
        source(&value.source)
    )?;
    Ok(())
}

fn reference<'a>(id: &'a str, visible: &'a BTreeMap<&str, &Node>) -> impl fmt::Display + 'a {
    Formatted(move |f: &mut fmt::Formatter<'_>| {
        if let Some(node) = visible.get(id) {
            write!(f, "{}", text(id))?;
            if node.props["computed_name"] == true {
                f.write_str(" (computed source name omitted; runtime key unresolved)")
            } else if node.props["name_capture"] == "nonidentifier_key_omitted" {
                f.write_str(" (source name omitted)")
            } else {
                Ok(())
            }
        } else {
            f.write_str("Unavailable in this export")
        }
    })
}

fn resolution<'a>(
    value: &'a DependencyResolution,
    visible: &'a BTreeMap<&str, &Node>,
) -> impl fmt::Display + 'a {
    Formatted(move |f: &mut fmt::Formatter<'_>| match value {
        DependencyResolution::Binding {
            binding_id,
            declaration,
        } => write!(
            f,
            "Binding {} declared at {}",
            text(binding_id),
            source(declaration)
        ),
        DependencyResolution::Target { node_id } => {
            write!(f, "Target {}", reference(node_id, visible))
        }
        DependencyResolution::Unresolved { gap_id } => {
            write!(f, "Unresolved: {}", reference(gap_id, visible))
        }
    })
}

fn definition_shape<'a>(
    value: &'a DefinitionExpressionKind,
    visible: &'a BTreeMap<&str, &Node>,
) -> impl fmt::Display + 'a {
    Formatted(move |f: &mut fmt::Formatter<'_>| match value {
        DefinitionExpressionKind::Literal => f.write_str("Literal syntax"),
        DefinitionExpressionKind::Identifier { dependency } => write!(
            f,
            "Identifier read; dependency {dependency}; runtime value unresolved"
        ),
        DefinitionExpressionKind::PropertyName => f.write_str("Property-name syntax"),
        DefinitionExpressionKind::Member {
            object,
            property,
            computed,
            optional,
            dependency,
        } => write!(
            f,
            "Member read; object {object}; property {property}; computed {computed}; optional {optional}; dependency {dependency}; runtime value unresolved"
        ),
        DefinitionExpressionKind::Parenthesized { value } => {
            write!(f, "Parentheses around {value}")
        }
        DefinitionExpressionKind::Unary { operator, operand } => {
            write!(f, "Unary {operator:?}; operand {operand}")
        }
        DefinitionExpressionKind::Binary {
            operator,
            left,
            right,
        } => write!(f, "Binary {operator:?}; left {left}; right {right}"),
        DefinitionExpressionKind::Logical {
            operator,
            left,
            right,
        } => write!(
            f,
            "Logical {operator:?}; left {left}; right {right}; short-circuit syntax"
        ),
        DefinitionExpressionKind::Call {
            callee,
            arguments,
            dependency,
        } => write!(
            f,
            "Call; callee {callee}; arguments {arguments:?}; dependency {dependency}; returned value unresolved"
        ),
        DefinitionExpressionKind::KeywordArgument { name, value } => {
            write!(f, "Keyword argument; name {name}; value {value}")
        }
        DefinitionExpressionKind::Unsupported { gap_id } => {
            write!(f, "Unsupported: {}", reference(gap_id, visible))
        }
    })
}

fn local_definitions(
    output: &mut impl fmt::Write,
    rule: &GuardedExitEvidence,
    visible: &BTreeMap<&str, &Node>,
) -> fmt::Result {
    output.write_str("\n### Local const initializers\n\nInitializer as written; value at use and business meaning are not established.\n\n")?;
    let Some(definitions) = &rule.local_definitions else {
        output.write_str(
            "Local-definition evidence was not collected in this version 1 observation.\n\n",
        )?;
        return Ok(());
    };
    if definitions.is_empty() {
        output.write_str("No local const initializer evidence was captured; completeness is not established.\n\n")?;
    }
    for (index, definition) in definitions.iter().enumerate() {
        writeln!(
            output,
            "#### Definition {index}\n\nBinding: {}\n\nDeclaration source: {}\n\nUse sources:\n",
            text(&definition.binding_id),
            source(&definition.declaration)
        )?;
        for usage in &definition.uses {
            writeln!(output, "- {}", source(usage))?;
        }
        output.write_str("\nStored initializer:\n\n")?;
        expression(output, &definition.initializer)?;
        writeln!(
            output,
            "Structured expression evidence (root {}):\n\n| Node | Shape | Stored expression | Capture | Literal | Source |\n|---|---|---|---|---|---|",
            definition.expression.root
        )?;
        for (node_index, node) in definition.expression.nodes.iter().enumerate() {
            writeln!(
                output,
                "| {} | {} | {} | {:?} | {} | {} |",
                node_index,
                definition_shape(&node.kind, visible),
                text(node.expression.display.as_str()),
                node.expression.capture,
                literal(&node.expression.literal),
                source(&node.expression.source)
            )?;
        }
        output.write_str(
            "\nInitializer dependencies:\n\n| Index | Resolution | Source |\n|---|---|---|\n",
        )?;
        for (dependency_index, dependency) in definition.dependencies.iter().enumerate() {
            writeln!(
                output,
                "| {} | {} | {} |",
                dependency_index,
                resolution(&dependency.resolution, visible),
                source(&dependency.source)
            )?;
        }
        if definition.dependencies.is_empty() {
            output.write_str("| — | No initializer dependencies captured; runtime value remains unestablished | — |\n")?;
        }
        output.write_char('\n')?;
    }
    Ok(())
}

/// Inventories at or below this many observations render every observation
/// in full (#487).
pub const RULE_EVIDENCE_FLAT_LIMIT: usize = 50;
/// Source files detailed in `rule-evidence.md` past the flat limit; files
/// past the cap are counted in one explicit line.
pub const RULE_EVIDENCE_MAX_FILES: usize = 50;
/// Observations rendered in full per source file past the flat limit.
pub const RULE_EVIDENCE_REPRESENTATIVES: usize = 3;

const PREAMBLE: &str = "# Source rule evidence\n\nThese are guarded local exit observations from stored source evidence. \
        A complete execution predicate and the consumer effect are not established. \
        Source order is lexical, not runtime order. Coverage is limited to the facts in this export.\n\n";

/// Languages whose adapter has a rule-evidence producer (#2009).
const RULE_LANGUAGES: &[&str] = &[
    "python",
    "typescript",
    "javascript",
    "tsx",
    "jsx",
    "go",
    "java",
    "kotlin",
    "rust",
];

/// Source files of each language in `nodes` that no rule-evidence producer
/// reads, by language: `File` nodes whose `language` has none (#2009).
pub fn unread_rule_languages<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
) -> BTreeMap<&'a str, usize> {
    let mut counts = BTreeMap::new();
    for node in nodes {
        if node.label != "File" {
            continue;
        }
        if let Some(language) = node.props["language"].as_str()
            && !RULE_LANGUAGES.contains(&language)
        {
            *counts.entry(language).or_default() += 1;
        }
    }
    counts
}

/// The sentence that states which source no rule evidence was read from,
/// or `None` when every language in the export has a producer (#2009).
pub fn unread_rule_sentence(unread: &BTreeMap<&str, usize>) -> Option<String> {
    if unread.is_empty() {
        return None;
    }
    let named: Vec<String> = unread
        .iter()
        .map(|(language, files)| {
            let name = match *language {
                "go" => "Go".to_owned(),
                "rust" => "Rust".to_owned(),
                "java" => "Java".to_owned(),
                "kotlin" => "Kotlin".to_owned(),
                "swift" => "Swift".to_owned(),
                other => other.to_owned(),
            };
            let noun = if *files == 1 { "file" } else { "files" };
            format!("{files} {name} {noun}")
        })
        .collect();
    let listed = match named.as_slice() {
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
        [] => unreachable!(),
    };
    Some(format!(
        "Rule evidence is recovered from Python, TypeScript/JavaScript, Go, Java, Kotlin and Rust source only. \
         It is not extracted from the {listed} in this export, so the guards in them are \
         not listed here and these rules do not cover them.\n\n"
    ))
}

/// Structured index of every observation (`rule-evidence.json`).
#[derive(Serialize)]
struct RuleEvidenceIndex<'a> {
    schema: &'static str,
    mode: ExportMode,
    observations: usize,
    assertions: usize,
    files: Vec<RuleFileIndex<'a>>,
}

#[derive(Serialize)]
struct RuleFileIndex<'a> {
    id: String,
    /// `None` when the observation's payload is malformed or unsupported.
    repo: Option<&'a str>,
    path: Option<&'a str>,
    observations: usize,
    /// Observation assertion ids; each carries its full provenance in the
    /// bundle.
    members: Vec<DisplayString<AssertionText<'a>>>,
    /// Relationship assertion ids asserted by these observations.
    relationships: Vec<DisplayString<AssertionText<'a>>>,
}

/// One observation with its parsed payload and its assertions: the
/// observation first, then each relationship it asserts.
struct Observation<'a> {
    node: &'a Node,
    rule: Option<GuardedExitEvidence>,
    assertions: Vec<Assertion<'a>>,
}

/// Each selected rule's partition relationships to visible nodes, sorted by
/// label then target, indexed once so rendering every rule is not a scan of
/// every edge per rule. Ties keep edge order, as the stable per-rule sort did.
/// Only the selected rules are keys, so edges from other sources are never
/// retained and an inventory without rules allocates nothing.
struct Relationships<'a>(BTreeMap<&'a str, Vec<&'a Edge>>);

impl<'a> Relationships<'a> {
    fn new(edges: &[&'a Edge], visible: &BTreeMap<&str, &Node>, rules: &[&'a Node]) -> Self {
        let mut by_source: BTreeMap<&'a str, Vec<&'a Edge>> = rules
            .iter()
            .map(|rule| (rule.id.as_str(), Vec::new()))
            .collect();
        if by_source.is_empty() {
            return Self(by_source);
        }
        for edge in edges.iter().copied().filter(|edge| {
            PARTITION_EDGE_LABELS.contains(&edge.label.as_str())
                && visible.contains_key(edge.dst.as_str())
        }) {
            if let Some(relationships) = by_source.get_mut(edge.src.as_str()) {
                relationships.push(edge);
            }
        }
        for relationships in by_source.values_mut() {
            relationships
                .sort_by(|left, right| (&left.label, &left.dst).cmp(&(&right.label, &right.dst)));
        }
        Self(by_source)
    }

    fn of(&self, node: &Node) -> &[&'a Edge] {
        self.0.get(node.id.as_str()).map_or(&[], Vec::as_slice)
    }
}

fn render_observation(
    output: &mut impl fmt::Write,
    observation: &Observation<'_>,
    relations: &Relationships<'_>,
    visible: &BTreeMap<&str, &Node>,
) -> fmt::Result {
    let node = observation.node;
    let producing = observation.assertions[0]
        .provenance
        .as_ref()
        .expect("Markdown provenance parsed");
    writeln!(
        output,
        "## Observation {}\n\nProducing tier: {:?} · Confidence: {:?}\n",
        text(&node.id),
        producing.tier,
        producing.confidence_tier
    )?;
    let Some(rule) = &observation.rule else {
        output.write_str("Source-rule payload unavailable: malformed or unsupported schema. No source expressions were rendered.\n\n")?;
        return Ok(());
    };
    writeln!(
        output,
        "Owner: {}\n\nExit source: {}\n\nSource order within callable: {} (zero-based)\n\n\
        Complete execution predicate: not established.\n\nConsumer effect: not established.\n",
        reference(&rule.owner_id, visible),
        source(&rule.exit_source),
        rule.source_order
    )?;
    output.write_str("### Same-callable branch conditions\n\n| Order | Polarity | Stored expression | Capture | Branch source | Expression source |\n|---|---|---|---|---|---|\n")?;
    if rule.conditions.is_empty() {
        output.write_str("| — | — | No branch conditions captured | — | — | — |\n")?;
    }
    for (index, condition) in rule.conditions.iter().enumerate() {
        let polarity = match condition.polarity {
            BranchPolarity::TruthyBranch => "truthy branch",
            BranchPolarity::FalsyBranch => "falsy branch",
            BranchPolarity::RaisedBranch => "raised branch",
            BranchPolarity::UnraisedBranch => "not raised branch",
        };
        writeln!(
            output,
            "| {} | {} | {} | {:?} | {} | {} |",
            index,
            polarity,
            text(condition.expression.display.as_str()),
            condition.expression.capture,
            source(&condition.branch_source),
            source(&condition.expression.source)
        )?;
    }
    output.write_str("\n### Local effect\n\n")?;
    match &rule.effect {
        LocalExit::Return { value } => {
            output.write_str("Return from this callable.\n\n")?;
            match value {
                Some(value) => expression(output, value)?,
                None => output.write_str("Bare return: no value expression.\n\n")?,
            }
        }
        LocalExit::Throw { value } => {
            output.write_str(
                "Throw from this source location; exception handling is not established.\n\n",
            )?;
            expression(output, value)?;
        }
        LocalExit::Assign { target, value } => {
            output.write_str("Assignment; the callable continues.\n\nAssigned place:\n\n")?;
            expression(output, target)?;
            output.write_str("Assigned value:\n\n")?;
            expression(output, value)?;
        }
        LocalExit::Call { value } => {
            output.write_str(
                "Call statement; its result is unused and the callable continues.\n\n",
            )?;
            expression(output, value)?;
        }
    }
    output.write_str("### Dependencies\n\n| Role | Resolution | Source |\n|---|---|---|\n")?;
    for dependency in &rule.dependencies {
        writeln!(
            output,
            "| {:?} | {} | {} |",
            dependency.role,
            resolution(&dependency.resolution, visible),
            source(&dependency.source)
        )?;
    }
    if rule.dependencies.is_empty() {
        output
            .write_str("| — | No dependencies captured; completeness is not established | — |\n")?;
    }
    local_definitions(output, rule, visible)?;
    output.write_str("\nInterpretation gaps:\n\n")?;
    if rule.interpretation.gap_ids.is_empty() {
        output.write_str(
            "- No gap references supplied; behavioral interpretation remains not established.\n",
        )?;
    }
    for gap in &rule.interpretation.gap_ids {
        writeln!(output, "- {}", reference(gap, visible))?;
    }
    output.write_str("\n### Redactions\n\n")?;
    if rule.redactions.is_empty() {
        output.write_str("No redactions recorded in this payload.\n")?;
    }
    for redaction in &rule.redactions {
        writeln!(
            output,
            "- {:?}: {}",
            redaction.reason,
            source(&redaction.source)
        )?;
    }
    output.write_str("\n### Visible graph relationships\n\n| Relation | Target | Tier | Confidence |\n|---|---|---|---|\n")?;
    let relationships = relations.of(node);
    if relationships.is_empty() {
        output.write_str("| — | No relationships available in this export | — | — |\n")?;
    }
    for edge in relationships {
        let identity = format!("{} {} {}", edge.src, edge.label, edge.dst);
        let producing = provenance(&edge.props, &identity);
        writeln!(
            output,
            "| {} | {} | {:?} | {:?} |",
            edge.label,
            reference(&edge.dst, visible),
            producing.tier,
            producing.confidence_tier
        )?;
    }
    output.write_char('\n')?;
    Ok(())
}

#[derive(Clone, Copy)]
enum AssertionSource<'a> {
    Node { node: &'a Node, parsed: bool },
    Edge(&'a Edge),
}

#[derive(Clone, Copy)]
enum AssertionField {
    Id,
    Subject,
    Summary,
}
#[derive(Clone, Copy)]
struct AssertionText<'a> {
    source: AssertionSource<'a>,
    field: AssertionField,
}
impl fmt::Display for AssertionText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.source, self.field) {
            (AssertionSource::Node { node, .. }, AssertionField::Id) => {
                write!(f, "node:{}", node.id)
            }
            (AssertionSource::Node { node, .. }, AssertionField::Subject) => f.write_str(&node.id),
            (AssertionSource::Node { parsed, .. }, AssertionField::Summary) => {
                f.write_str(if parsed {
                    "Guarded local exit observation; behavioral interpretation not established"
                } else {
                    "Source-rule payload unavailable: malformed or unsupported schema"
                })
            }
            (AssertionSource::Edge(edge), AssertionField::Id) => {
                write!(f, "edge:{} {} {}", edge.src, edge.label, edge.dst)
            }
            (AssertionSource::Edge(edge), AssertionField::Subject) => {
                write!(f, "{} {} {}", edge.src, edge.label, edge.dst)
            }
            (AssertionSource::Edge(edge), AssertionField::Summary) => {
                write!(f, "Source-rule {} relationship", edge.label)
            }
        }
    }
}

// Provenance is parsed once per asserted fact, as linear admitted-input
// scratch. Output identities/summaries are borrowed formatting descriptors;
// returned assertion strings are created only after cumulative admission.
struct Assertion<'a> {
    source: AssertionSource<'a>,
    provenance: Option<Provenance>,
    omission: Option<core_graph::omission::Omission>,
}
#[derive(Serialize)]
struct AssertionView<'a, 'b> {
    id: DisplayString<AssertionText<'a>>,
    subject_id: DisplayString<AssertionText<'a>>,
    subject_kind: &'a str,
    summary: DisplayString<AssertionText<'a>>,
    provenance: &'b Provenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    omission: Option<&'b core_graph::omission::Omission>,
}
impl<'a> Assertion<'a> {
    fn text(&self, field: AssertionField) -> AssertionText<'a> {
        AssertionText {
            source: self.source,
            field,
        }
    }
    fn view(&self) -> AssertionView<'a, '_> {
        let (subject_kind, edge_label) = match self.source {
            AssertionSource::Node { .. } => ("BusinessRule", None),
            AssertionSource::Edge(edge) => (edge.label.as_str(), Some(edge.label.as_str())),
        };
        AssertionView {
            id: DisplayString(self.text(AssertionField::Id)),
            subject_id: DisplayString(self.text(AssertionField::Subject)),
            subject_kind,
            summary: DisplayString(self.text(AssertionField::Summary)),
            provenance: self
                .provenance
                .as_ref()
                .expect("Markdown provenance parsed"),
            edge_label,
            omission: self.omission.as_ref(),
        }
    }
    /// How the subject id maps onto a fact key (ADR-0058).
    fn subject(&self) -> crate::workbench::fact_index::Subject {
        use crate::workbench::fact_index::Subject;
        match self.source {
            AssertionSource::Node { .. } => Subject::Node,
            AssertionSource::Edge(edge) => Subject::edge(&edge.src, &edge.label),
        }
    }
    fn into_owned(self) -> SpecAssertion {
        #[cfg(test)]
        OWNED_ASSERTIONS.with(|counter| counter.set(counter.get() + 1));
        let view = self.view();
        SpecAssertion {
            id: view.id.0.to_string(),
            subject_id: view.subject_id.0.to_string(),
            subject_kind: view.subject_kind.into(),
            summary: view.summary.0.to_string(),
            edge_label: view.edge_label.map(String::from),
            omission: self.omission,
            provenance: self.provenance.expect("Markdown provenance parsed"),
        }
    }
}

struct AssertionSequence<'a, 'b> {
    observations: &'b [Observation<'a>],
    included: bool,
}
impl Serialize for AssertionSequence<'_, '_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.included {
            serializer.collect_seq(
                self.observations
                    .iter()
                    .flat_map(|observation| &observation.assertions)
                    .map(Assertion::view),
            )
        } else {
            serializer.collect_seq(std::iter::empty::<()>())
        }
    }
}

/// Fixed Gap omission details, charged by the typed accounting.
pub(crate) const RULE_PAYLOAD_UNAVAILABLE: &str = "source-rule payload unavailable";
pub(crate) const RULE_RELATIONSHIP_UNRESOLVED: &str = "unresolved source-rule relationship";

fn observations<'a>(
    rules: &[&'a Node],
    relations: &Relationships<'a>,
    include_provenance: bool,
) -> Vec<Observation<'a>> {
    let mut asserted_edges = BTreeSet::new();
    rules
        .iter()
        .map(|node| {
            let parsed = rule_payload(node).ok();
            let mut assertions = vec![Assertion {
                source: AssertionSource::Node {
                    node,
                    parsed: parsed.is_some(),
                },
                provenance: include_provenance.then(|| provenance(&node.props, &node.id)),
                omission: include_provenance
                    .then(|| {
                        crate::workbench::gap_omission(
                            &provenance(&node.props, &node.id),
                            &node.props,
                            RULE_PAYLOAD_UNAVAILABLE,
                        )
                    })
                    .flatten(),
            }];
            if parsed.is_some() {
                for edge in relations.of(node) {
                    // Keep the existing native concatenated-identity deduplication,
                    // including its legacy collision behavior. Keys are linear
                    // in the admitted edge input, not rendered output expansion.
                    let identity = format!("{} {} {}", edge.src, edge.label, edge.dst);
                    if asserted_edges.insert(identity.clone()) {
                        assertions.push(Assertion {
                            source: AssertionSource::Edge(edge),
                            provenance: include_provenance
                                .then(|| provenance(&edge.props, &identity)),
                            omission: include_provenance
                                .then(|| {
                                    crate::workbench::gap_omission(
                                        &provenance(&edge.props, &identity),
                                        &edge.props,
                                        RULE_RELATIONSHIP_UNRESOLVED,
                                    )
                                })
                                .flatten(),
                        });
                    }
                }
            }
            Observation {
                node,
                rule: parsed,
                assertions,
            }
        })
        .collect()
}

fn file_id(index: usize) -> String {
    format!("R-{:02}", index + 1)
}

fn file_label<'a>(key: &'a Option<(&str, &str)>) -> impl fmt::Display + 'a {
    Formatted(move |f: &mut fmt::Formatter<'_>| match key {
        Some((repo, path)) => write!(
            f,
            "{}",
            text(Formatted(|f: &mut fmt::Formatter<'_>| write!(
                f,
                "{repo}:{path}"
            )))
        ),
        None => f.write_str("Source-rule payload unavailable"),
    })
}

fn append_rule_assertions<'a, 'b>(
    output: &mut impl fmt::Write,
    assertions: impl Iterator<Item = &'b Assertion<'a>>,
) -> fmt::Result
where
    'a: 'b,
{
    output.write_str("\n## Assertions and inline provenance\n\n")?;
    let mut assertions = assertions.peekable();
    if assertions.peek().is_none() {
        return output.write_str("No graph-backed assertions were recovered for this artifact.\n");
    }
    output.write_str("| Assertion | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n|---|---|---|---|---|---|---|\n")?;
    for assertion in assertions {
        let p = assertion
            .provenance
            .as_ref()
            .expect("Markdown provenance parsed");
        writeln!(
            output,
            "| {} | {:?} | {:?} | {} | {} | `{}` | `{}` |",
            TableSafe(assertion.text(AssertionField::Summary)),
            p.tier,
            p.confidence_tier,
            TableSafe(MethodLabel(&p.method)),
            TableSafe(EvidenceText(p)),
            TableSafe(&p.extractor_id),
            TableSafe(&p.content_hash)
        )?;
    }
    Ok(())
}

/// The source-rule inventory, its JSON index, and its complete assertions
/// (#487). Past [`RULE_EVIDENCE_FLAT_LIMIT`] observations the Markdown groups
/// them by exit-source file, renders at most [`RULE_EVIDENCE_REPRESENTATIVES`]
/// observations in full for each of at most [`RULE_EVIDENCE_MAX_FILES`] files,
/// and states every omission as a counted line; the index lists every
/// observation and relationship by file.
#[cfg(test)]
pub(crate) fn inventory(
    nodes: &[&Node],
    edges: &[&Edge],
    mode: ExportMode,
) -> (String, String, Vec<SpecAssertion>) {
    let output = inventory_selected(
        nodes,
        edges,
        mode,
        InventorySelection {
            markdown: true,
            index: true,
        },
        None,
    )
    .expect("unlimited rule inventory");
    (
        output.markdown.unwrap(),
        output.index.unwrap(),
        output.assertions,
    )
}

/// Which native rule artifacts to construct from the complete projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InventorySelection {
    pub markdown: bool,
    pub index: bool,
}
#[derive(Debug)]
pub(crate) struct InventoryOutput {
    pub markdown: Option<String>,
    pub index: Option<String>,
    pub assertions: Vec<SpecAssertion>,
    /// The fact-key shape of each assertion's subject (ADR-0058).
    pub subjects: Vec<crate::workbench::fact_index::Subject>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuleInventoryLimitExceeded {
    pub maximum: usize,
}
impl fmt::Display for RuleInventoryLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rule inventory byte limit exceeded (maximum {})",
            self.maximum
        )
    }
}
impl std::error::Error for RuleInventoryLimitExceeded {}

type FileGroups<'a, 'b> = Vec<(Option<(&'b str, &'b str)>, Vec<&'b Observation<'a>>)>;

/// Count selected UTF-8 content plus the exact JSON array of returned
/// assertions before constructing any output strings or assertion identities.
/// Index-only selection returns an empty assertion array (charged as `[]`).
/// Selecting both siblings charges this return value's shared assertion array
/// once; a caller wrapping both into separate artifacts must additionally
/// charge the index artifact's empty array. Single-artifact calls are exact.
/// Parsing and grouping are linear workspace over an already admitted graph.
pub(crate) fn inventory_selected(
    nodes: &[&Node],
    edges: &[&Edge],
    mode: ExportMode,
    selection: InventorySelection,
    max_bytes: Option<usize>,
) -> Result<InventoryOutput, RuleInventoryLimitExceeded> {
    inventory_selected_roots(nodes, edges, mode, selection, max_bytes, None)
}

/// Explicit observation roots; context rules remain lookup-only.
pub(crate) fn inventory_selected_roots(
    nodes: &[&Node],
    edges: &[&Edge],
    mode: ExportMode,
    selection: InventorySelection,
    max_bytes: Option<usize>,
    roots: Option<&BTreeSet<String>>,
) -> Result<InventoryOutput, RuleInventoryLimitExceeded> {
    let visible: BTreeMap<_, _> = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
    let mut rules: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "BusinessRule")
        .filter(|node| roots.is_none_or(|roots| roots.contains(&node.id)))
        .collect();
    rules.sort_by(|left, right| left.id.cmp(&right.id));
    let relations = Relationships::new(edges, &visible, &rules);
    let observations = observations(&rules, &relations, selection.markdown);
    let assertion_count: usize = observations
        .iter()
        .map(|observation| observation.assertions.len())
        .sum();
    let files = largest_groups(observations.iter(), |observation| {
        observation.rule.as_ref().map(|rule| {
            (
                rule.exit_source.repo.as_str(),
                rule.exit_source.path.as_str(),
            )
        })
    });
    let mut markdown_bytes = 0;
    let mut index_bytes = 0;
    if let Some(maximum) = max_bytes {
        let mut counter = Counter { bytes: 0, maximum };
        let measured = (|| -> fmt::Result {
            if selection.markdown {
                render_markdown(
                    &mut counter,
                    &observations,
                    &files,
                    assertion_count,
                    &relations,
                    &visible,
                )?;
                markdown_bytes = counter.bytes;
            }
            if selection.index {
                let before = counter.bytes;
                render_index(&mut counter, &observations, &files, assertion_count, mode)?;
                index_bytes = counter.bytes - before;
            }
            render::json(
                &mut counter,
                &AssertionSequence {
                    observations: &observations,
                    included: selection.markdown,
                },
            )
        })();
        measured.map_err(|_| RuleInventoryLimitExceeded { maximum })?;
    }
    let markdown = if selection.markdown {
        #[cfg(test)]
        OWNED_CONTENT.with(|counter| counter.set(counter.get() + 1));
        let mut output = String::with_capacity(markdown_bytes);
        render_markdown(
            &mut output,
            &observations,
            &files,
            assertion_count,
            &relations,
            &visible,
        )
        .expect("admitted rule Markdown");
        Some(output)
    } else {
        None
    };
    let index = if selection.index {
        #[cfg(test)]
        OWNED_CONTENT.with(|counter| counter.set(counter.get() + 1));
        let mut output = String::with_capacity(index_bytes);
        render_index(&mut output, &observations, &files, assertion_count, mode)
            .expect("admitted rule index");
        Some(output)
    } else {
        None
    };
    let (assertions, subjects) = if selection.markdown {
        observations
            .into_iter()
            .flat_map(|observation| observation.assertions)
            .map(|assertion| {
                let subject = assertion.subject();
                (assertion.into_owned(), subject)
            })
            .unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(InventoryOutput {
        markdown,
        index,
        assertions,
        subjects,
    })
}

fn render_markdown(
    output: &mut impl fmt::Write,
    observations: &[Observation<'_>],
    files: &FileGroups<'_, '_>,
    assertion_count: usize,
    relations: &Relationships<'_>,
    visible: &BTreeMap<&str, &Node>,
) -> fmt::Result {
    #[cfg(test)]
    MARKDOWN_RENDERS.with(|counter| counter.set(counter.get() + 1));
    output.write_str(PREAMBLE)?;
    if let Some(unread) = unread_rule_sentence(&unread_rule_languages(visible.values().copied())) {
        output.write_str(&unread)?;
    }
    if observations.is_empty() {
        output.write_str("No source-rule observations are available in this export.\n")?;
    }
    if observations.len() <= RULE_EVIDENCE_FLAT_LIMIT {
        for observation in observations {
            render_observation(output, observation, relations, visible)?;
        }
        append_rule_assertions(
            output,
            observations
                .iter()
                .flat_map(|observation| &observation.assertions),
        )?;
    } else {
        let shown = &files[..files.len().min(RULE_EVIDENCE_MAX_FILES)];
        writeln!(
            output,
            "{} observations from {} source files, largest first. Each file lists up to \
             {RULE_EVIDENCE_REPRESENTATIVES} representative observations in full; \
             `rule-evidence.json` lists every observation and relationship by file, and \
             each keeps its inline provenance in the bundle's assertions.\n",
            observations.len(),
            files.len(),
        )?;
        output.write_str("| File | Source file | Observations |\n|---|---|---|\n")?;
        for (index, (key, members)) in shown.iter().enumerate() {
            writeln!(
                output,
                "| {} | {} | {} |",
                file_id(index),
                file_label(key),
                members.len()
            )?;
        }
        let hidden = &files[shown.len()..];
        if !hidden.is_empty() {
            writeln!(
                output,
                "| … | {} more files, not detailed here — listed in `rule-evidence.json` | {} |",
                hidden.len(),
                hidden
                    .iter()
                    .map(|(_, members)| members.len())
                    .sum::<usize>(),
            )?;
        }
        let mut representatives: Vec<&Assertion> = Vec::new();
        for (index, (key, members)) in shown.iter().enumerate() {
            writeln!(
                output,
                "\n## {} — {} ({} observations)\n",
                file_id(index),
                file_label(key),
                members.len()
            )?;
            for observation in members.iter().take(RULE_EVIDENCE_REPRESENTATIVES) {
                // Nest the observation's headings under its file heading.
                // Stored text is escaped by `text`, so only headings start
                // a line with `#`.
                render_observation(
                    &mut NestedHeadings {
                        output,
                        line_start: true,
                    },
                    observation,
                    relations,
                    visible,
                )?;
                representatives.extend(&observation.assertions);
            }
            let more = members.len().saturating_sub(RULE_EVIDENCE_REPRESENTATIVES);
            if more > 0 {
                writeln!(
                    output,
                    "… {more} more observations from this file — listed in `rule-evidence.json` under `{}`.",
                    file_id(index),
                )?;
            }
        }
        append_rule_assertions(output, representatives.iter().copied())?;
        writeln!(
            output,
            "\nInline provenance is shown for the {} assertions of the representative \
             observations above; the other {} carry theirs in the bundle's structured assertions.",
            representatives.len(),
            assertion_count - representatives.len(),
        )?;
    }
    Ok(())
}

fn render_index(
    output: &mut impl fmt::Write,
    observations: &[Observation<'_>],
    files: &FileGroups<'_, '_>,
    assertion_count: usize,
    mode: ExportMode,
) -> fmt::Result {
    #[cfg(test)]
    INDEX_RENDERS.with(|counter| counter.set(counter.get() + 1));
    let index = RuleEvidenceIndex {
        schema: "cartograph.rule-evidence-index/v1",
        mode,
        observations: observations.len(),
        assertions: assertion_count,
        files: files
            .iter()
            .enumerate()
            .map(|(index, (key, members))| RuleFileIndex {
                id: file_id(index),
                repo: key.map(|(repo, _)| repo),
                path: key.map(|(_, path)| path),
                observations: members.len(),
                members: members
                    .iter()
                    .map(|observation| {
                        DisplayString(observation.assertions[0].text(AssertionField::Id))
                    })
                    .collect(),
                relationships: members
                    .iter()
                    .flat_map(|observation| observation.assertions[1..].iter())
                    .map(|assertion| DisplayString(assertion.text(AssertionField::Id)))
                    .collect(),
            })
            .collect(),
    };
    render::json(output, &index)?;
    output.write_char('\n')?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static OWNED_ASSERTIONS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};
    static OWNED_CONTENT:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};
    static MARKDOWN_RENDERS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};
    static INDEX_RENDERS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};
}

pub(crate) fn logical_workspace_bytes(bytes: usize, rows: usize, values: usize) -> Option<usize> {
    use crate::workbench::accounting::{map_slots, set_slots, slots, sum, typed};
    sum(&[
        map_slots::<&str, &Node>(rows)?,
        slots::<&Node>(rows)?, // visible/rules
        slots::<Observation<'_>>(rows)?,
        slots::<Assertion<'_>>(rows)?,
        slots::<GuardedExitEvidence>(rows)?,
        core_graph::rules::decoded_heap_bytes(bytes, values)?, // retained Observation.rule heap
        core_graph::rules::validation_workspace(bytes, values)?, // one current rule decode
        typed::provenance_heap(bytes, values, rows)?,          // retained Assertion.provenance heap
        typed::omission_heap(bytes, rows)?, // retained Assertion.omission details
        slots::<core_graph::omission::Omission>(rows)?,
        set_slots::<String>(rows)?,
        bytes,
        rows.checked_mul(2)?,                 // deduplicated asserted edge IDs
        bytes, // current concatenated edge ID before set clone/insertion
        map_slots::<&str, Vec<&Edge>>(rows)?, // relationship index, one entry per rule
        slots::<&Edge>(rows)?, // indexed relationships, borrowed
        map_slots::<Option<(&str, &str)>, Vec<&Observation<'_>>>(rows)?,
        slots::<(Option<(&str, &str)>, Vec<&Observation<'_>>)>(rows)?,
        slots::<&Observation<'_>>(rows)?,
        slots::<RuleFileIndex<'_>>(rows)?,
        slots::<DisplayString<AssertionText<'_>>>(rows)?, // member IDs
        slots::<DisplayString<AssertionText<'_>>>(rows)?, // relationship IDs
        slots::<&Assertion<'_>>(rows)?,
        slots::<&str>(values)?,
        rows.checked_mul(usize::MAX.to_string().len().checked_add("R-".len())?)?,
        std::mem::size_of::<RuleEvidenceIndex<'_>>(),
        std::mem::size_of::<AssertionView<'_, '_>>(),
    ])
}

/// A `BusinessRule` node's `rule` payload, typed. Every spec reader goes
/// through here, so the payload may be held as the object the store keeps or
/// as its marked JSON text (see [`compact_rule_payloads`]): both parse alike,
/// and a string the store holds stays malformed.
pub(crate) fn rule_payload(node: &Node) -> Result<GuardedExitEvidence, RuleValidationError> {
    GuardedExitEvidence::from_value(core_graph::stored::stored(&node.props["rule"]).into_owned())
}

/// Hold each `BusinessRule` payload as its compact JSON text instead of a
/// decoded object, for a base that only the spec compiler reads from here
/// on. A decoded payload costs about six times its text: each JSON object is
/// a B-tree leaf of some 620 bytes however few keys it has, and on REA the
/// rules' payloads were 1.0 GB of a 3.0 GB base (#2215). [`rule_payload`]
/// parses the text to the same value the object gives. Only object payloads
/// are rewritten, so a malformed one stays as it was read.
pub fn compact_rule_payloads(nodes: &mut [Node]) {
    for node in nodes.iter_mut().filter(|node| node.label == "BusinessRule") {
        if let Some(rule) = node.props.get_mut("rule") {
            core_graph::stored::compact_object(rule);
        }
    }
}

/// A test's nodes with their rule payloads compacted ([`compact_rule_payloads`]).
#[cfg(test)]
pub(crate) fn compacted_for_tests(nodes: &[Node]) -> Vec<Node> {
    let mut nodes = nodes.to_vec();
    compact_rule_payloads(&mut nodes);
    nodes
}

/// Relationships actually queried by the source-rule inventory renderer.
pub(crate) const PARTITION_EDGE_LABELS: &[&str] = &["GOVERNS", "DEPENDS_ON"];

/// Graph identities dereferenced by rendering one typed observation. Lexical
/// binding IDs are not graph nodes and are deliberately not acquired.
pub(crate) fn partition_references(node: &Node) -> BTreeSet<String> {
    let Ok(rule) = rule_payload(node) else {
        return BTreeSet::new();
    };
    let mut ids = BTreeSet::from([rule.owner_id]);
    ids.extend(rule.interpretation.gap_ids.iter().cloned());
    let mut resolution = |value: &DependencyResolution| match value {
        DependencyResolution::Target { node_id } => {
            ids.insert(node_id.clone());
        }
        DependencyResolution::Unresolved { gap_id } => {
            ids.insert(gap_id.clone());
        }
        DependencyResolution::Binding { .. } => {}
    };
    for dependency in &rule.dependencies {
        resolution(&dependency.resolution);
    }
    if let Some(definitions) = &rule.local_definitions {
        for definition in definitions {
            for dependency in &definition.dependencies {
                resolution(&dependency.resolution);
            }
        }
    }
    if let Some(definitions) = &rule.local_definitions {
        for definition in definitions {
            for expression in &definition.expression.nodes {
                if let DefinitionExpressionKind::Unsupported { gap_id } = &expression.kind {
                    ids.insert(gap_id.clone());
                }
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExportMode, SpecBundle, compile_spec};
    use core_graph::rules::*;
    use core_prov::{ConfidenceTier, Provenance, Tier};
    use serde_json::json;

    fn evidence() -> EvidenceRef {
        EvidenceRef {
            repo: "example/shop".into(),
            path: "unavailable/source.ts".into(),
            byte_start: 10,
            byte_end: 25,
            commit_sha: "abc123".into(),
        }
    }

    fn payload() -> GuardedExitEvidence {
        let value = SourceExpression {
            source: evidence(),
            syntax_kind: "false".into(),
            display: SanitizedDisplay::from_sanitized("false".into()),
            capture: ExpressionCapture::CompleteSyntax,
            literal: Some(LiteralEvidence::Known {
                value: KnownLiteral::Boolean(false),
            }),
        };
        GuardedExitEvidence {
            schema_version: 1,
            kind: RuleKind::GuardedExit,
            owner_id: "sym:owner".into(),
            exit_source: evidence(),
            source_order: 0,
            conditions: vec![BranchCondition {
                branch_source: evidence(),
                expression: SourceExpression {
                    source: evidence(),
                    syntax_kind: "binary_expression".into(),
                    display: SanitizedDisplay::from_sanitized("options.enabled !== false".into()),
                    capture: ExpressionCapture::CompleteSyntax,
                    literal: None,
                },
                polarity: BranchPolarity::TruthyBranch,
            }],
            effect: LocalExit::Return { value: Some(value) },
            dependencies: vec![
                RuleDependency {
                    role: DependencyRole::Condition,
                    source: evidence(),
                    resolution: DependencyResolution::Binding {
                        binding_id: "binding:options".into(),
                        declaration: evidence(),
                    },
                },
                RuleDependency {
                    role: DependencyRole::ExitValue,
                    source: evidence(),
                    resolution: DependencyResolution::Target {
                        node_id: "sym:owner".into(),
                    },
                },
                RuleDependency {
                    role: DependencyRole::ControlFlow,
                    source: evidence(),
                    resolution: DependencyResolution::Unresolved {
                        gap_id: "gap:consumer".into(),
                    },
                },
            ],
            interpretation: Interpretation {
                execution_predicate: InterpretationStatus::NotEstablished,
                consumer_effect: InterpretationStatus::NotEstablished,
                gap_ids: vec!["gap:consumer".into()],
            },
            redactions: vec![],
            local_definitions: None,
        }
    }

    fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        let provenance = Provenance::new(
            tier,
            confidence,
            vec![evidence()],
            "spec.rules.test",
            id.as_bytes(),
        )
        .unwrap();
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({"name": id, "prov": provenance}),
        }
    }

    #[test]
    fn relationships_index_keeps_visible_partition_edges_in_label_target_order() {
        let (t, c) = (Tier::Deterministic, ConfidenceTier::Confirmed);
        let nodes = [
            node("rule:a", "BusinessRule", t, c),
            node("fn:x", "Function", t, c),
            node("fn:y", "Function", t, c),
        ];
        let visible: BTreeMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let edges = [
            edge("rule:a", "fn:y", "GOVERNS", t, c),
            edge("rule:a", "fn:hidden", "GOVERNS", t, c),
            edge("rule:a", "fn:x", "CALLS", t, c),
            edge("rule:a", "fn:x", "GOVERNS", t, c),
            edge("rule:a", "fn:y", "DEPENDS_ON", t, c),
            edge("fn:x", "fn:y", "DEPENDS_ON", t, c),
        ];
        let edges: Vec<&Edge> = edges.iter().collect();
        let rules = [visible["rule:a"], visible["fn:x"]];
        let relations = Relationships::new(&edges, &visible, &rules);
        let of = |id: &str| -> Vec<(String, String)> {
            relations
                .of(visible[id])
                .iter()
                .map(|e| (e.label.clone(), e.dst.clone()))
                .collect()
        };
        let pair = |l: &str, d: &str| (l.to_owned(), d.to_owned());
        assert_eq!(
            of("rule:a"),
            [
                pair("DEPENDS_ON", "fn:y"),
                pair("GOVERNS", "fn:x"),
                pair("GOVERNS", "fn:y"),
            ]
        );
        assert_eq!(of("fn:x"), [pair("DEPENDS_ON", "fn:y")]);
        assert!(of("fn:y").is_empty());
        let only_rule = Relationships::new(&edges, &visible, &[visible["rule:a"]]);
        assert!(only_rule.of(visible["fn:x"]).is_empty());
        assert!(Relationships::new(&edges, &visible, &[]).0.is_empty());
    }

    fn rule_node(id: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        let mut node = node(id, "BusinessRule", tier, confidence);
        node.props["rule"] = serde_json::to_value(payload()).unwrap();
        node
    }

    fn edge(src: &str, dst: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: json!({"prov":
            Provenance::new(tier, confidence, vec![evidence()], "spec.rules.test", format!("{src} {label} {dst}").as_bytes()).unwrap()}),
        }
    }

    fn base() -> Vec<Node> {
        vec![
            node(
                "sym:owner",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "gap:consumer",
                "Gap",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            ),
        ]
    }

    fn artifact(bundle: &SpecBundle) -> &crate::SpecArtifact {
        bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "rule-evidence")
            .unwrap()
    }

    #[test]
    fn rule_inventory_renders_cited_local_observations_deterministically() {
        // AC-0125: inventory uses the shared stored facts, not a source re-read.
        let mut nodes = base();
        nodes.push(rule_node(
            "rule:guard",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        let edges = vec![
            edge(
                "rule:guard",
                "sym:owner",
                "GOVERNS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "rule:guard",
                "gap:consumer",
                "DEPENDS_ON",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            ),
        ];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let output = artifact(&bundle);
        assert_eq!(output.file_name, "rule-evidence.md");
        for expected in [
            "guarded local exit observations",
            "Complete execution predicate: not established",
            "Consumer effect: not established",
            "sym:owner",
            "options.enabled",
            "false",
            "truthy branch",
            "boolean false",
            "Return from this callable",
            "Binding binding:options",
            "gap:consumer",
            "unavailable/source.ts bytes 10..25 @ abc123",
            "Deterministic",
            "Confirmed",
            "DEPENDS_ON",
            "GOVERNS",
        ] {
            assert!(
                output.content.contains(expected),
                "inventory omitted {expected}"
            );
        }
        assert_eq!(output.assertions.len(), 3);
        assert_eq!(
            output.assertions[0].provenance.confidence_tier,
            ConfidenceTier::Confirmed
        );
        assert!(
            output
                .assertions
                .iter()
                .all(|assertion| assertion.summary.starts_with("Guarded local")
                    || assertion.summary.starts_with("Source-rule"))
        );
        let mut reversed_edges = edges;
        reversed_edges.reverse();
        nodes.reverse();
        assert_eq!(
            bundle,
            compile_spec(
                &nodes,
                &reversed_edges,
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new()
            )
        );
    }

    #[test]
    fn rule_inventory_states_the_source_it_reads_no_rules_from() {
        // AC-0922 (#2009): Swift files in the export are named as source no
        // rule evidence was read from, so the rules are not taken to cover
        // them; Python, Go (AC-0923), Java (AC-0967), Kotlin (AC-1106) and
        // Rust (AC-1017) files are not.
        let mut nodes = base();
        nodes.push(rule_node(
            "rule:guard",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        for (id, language) in [
            ("file:A.swift", "swift"),
            ("file:B.swift", "swift"),
            ("file:g.kt", "kotlin"),
            ("file:c.py", "python"),
            ("file:d.go", "go"),
            ("file:E.java", "java"),
            ("file:f.rs", "rust"),
        ] {
            let mut file = node(id, "File", Tier::Deterministic, ConfidenceTier::Confirmed);
            file.props["language"] = language.into();
            nodes.push(file);
        }
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::BestEffort, &BTreeSet::new());
        let output = artifact(&bundle);
        assert!(
            output.content.contains(
                "It is not extracted from the 2 Swift files in this export, so the guards in \
                 them are not listed here"
            ),
            "{}",
            output.content
        );
        assert!(!output.content.contains("python"), "{}", output.content);
        // D1 §1.5 states it too, whether or not rules are arranged per
        // capability.
        let d1 = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "product-specification.md")
            .expect("D1 is in the bundle");
        let rules_section = d1
            .content
            .split("## 1.5 ")
            .nth(1)
            .and_then(|rest| rest.split("## 1.6 ").next())
            .expect("D1 has §1.5");
        assert!(
            rules_section.contains("not extracted from the 2 Swift files"),
            "{rules_section}"
        );
        assert!(!output.content.contains("Go file"), "{}", output.content);
        assert!(!output.content.contains("Java file"), "{}", output.content);
        assert!(!output.content.contains("Rust file"), "{}", output.content);
        assert!(
            !output.content.contains("Kotlin file"),
            "{}",
            output.content
        );
        let only_python: Vec<Node> = nodes
            .into_iter()
            .filter(|n| n.props["language"] != "swift")
            .collect();
        let bundle = compile_spec(
            &only_python,
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        assert!(
            !artifact(&bundle)
                .content
                .contains("Rule evidence is recovered from")
        );
    }

    #[test]
    fn rule_inventory_and_relationships_share_export_and_rejection_policy() {
        // AC-0125 / R-INT-5: a confirmed relation cannot leak an excluded rule.
        let mut nodes = base();
        for (id, tier, confidence) in [
            (
                "rule:confirmed",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            (
                "rule:strong",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            ("rule:weak", Tier::Agentic, ConfidenceTier::InferredWeak),
        ] {
            nodes.push(rule_node(id, tier, confidence));
        }
        let edges: Vec<_> = ["rule:confirmed", "rule:strong", "rule:weak"]
            .into_iter()
            .flat_map(|id| {
                [
                    edge(
                        id,
                        "sym:owner",
                        "GOVERNS",
                        Tier::Deterministic,
                        ConfidenceTier::Confirmed,
                    ),
                    edge(
                        id,
                        "gap:consumer",
                        "DEPENDS_ON",
                        Tier::Deterministic,
                        ConfidenceTier::Gap,
                    ),
                ]
            })
            .collect();
        let verified = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let verified_json = serde_json::to_string(&verified).unwrap();
        assert!(verified_json.contains("rule:confirmed"));
        assert!(verified_json.contains("rule:strong"));
        assert!(!verified_json.contains("rule:weak"));
        let best = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        assert!(artifact(&best).content.contains("rule:weak"));
        assert!(artifact(&best).content.contains("InferredWeak"));
        let rejected: BTreeSet<_> = nodes
            .iter()
            .filter(|node| ["rule:weak", "rule:strong"].contains(&node.id.as_str()))
            .map(|node| {
                node.props["prov"]["content_hash"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let curated = compile_spec(&nodes, &edges, &[], ExportMode::BestEffort, &rejected);
        let curated_json = serde_json::to_string(&curated).unwrap();
        assert!(curated_json.contains("rule:confirmed"));
        assert!(!curated_json.contains("rule:weak"));
        assert!(!curated_json.contains("rule:strong"));
        assert!(
            curated_json.contains("gap:consumer"),
            "independent explicit gaps remain visible"
        );
    }

    #[test]
    fn rule_inventory_hides_policy_excluded_dependency_targets_and_links() {
        // AC-0125: payload references do not bypass target visibility policy.
        let mut nodes = base();
        nodes.push(node(
            "sym:hidden",
            "Symbol",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        ));
        let mut rule = rule_node("rule:guard", Tier::Deterministic, ConfidenceTier::Confirmed);
        let mut observed = payload();
        observed.owner_id = "sym:hidden".into();
        observed.dependencies[1].resolution = DependencyResolution::Target {
            node_id: "sym:hidden".into(),
        };
        rule.props["rule"] = serde_json::to_value(observed).unwrap();
        nodes.push(rule);
        let edges = vec![edge(
            "rule:guard",
            "sym:hidden",
            "GOVERNS",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        )];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        assert!(
            artifact(&bundle)
                .content
                .contains("Unavailable in this export")
        );
        assert!(
            !serde_json::to_string(&bundle)
                .unwrap()
                .contains("sym:hidden")
        );
    }

    // AC-1207: a base whose rule payloads are held as JSON text compiles to
    // the same bundle, byte for byte, in both modes, malformed payloads and
    // their fixed diagnostic included (#2215).
    #[test]
    fn compacted_rule_payloads_compile_to_the_same_bundle() {
        let mut invalid_version = serde_json::to_value(payload()).unwrap();
        invalid_version["schema_version"] = json!(999);
        let mut nodes = base();
        for (id, rule) in [
            ("rule:valid", serde_json::to_value(payload()).unwrap()),
            ("rule:text", json!("synthetic-private-payload")),
            ("rule:version", invalid_version),
            // A stored string whose text is a valid payload stays malformed.
            (
                "rule:string",
                json!(serde_json::to_string(&payload()).unwrap()),
            ),
        ] {
            let mut node = rule_node(id, Tier::Deterministic, ConfidenceTier::Confirmed);
            node.props["rule"] = rule;
            nodes.push(node);
        }
        let edges = vec![edge(
            "rule:valid",
            "sym:owner",
            "GOVERNS",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        )];
        let mut compacted = nodes.clone();
        compact_rule_payloads(&mut compacted);
        let held = |id: &str| compacted.iter().find(|n| n.id == id).unwrap().props["rule"].clone();
        assert!(held("rule:valid").is_string());
        assert!(held("rule:version").is_string());
        assert_eq!(held("rule:text"), json!("synthetic-private-payload"));
        assert_eq!(
            held("rule:string"),
            nodes.iter().find(|n| n.id == "rule:string").unwrap().props["rule"]
        );
        let string = compacted.iter().find(|n| n.id == "rule:string").unwrap();
        assert!(rule_payload(string).is_err());
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let stored = compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
            let text = compile_spec(&compacted, &edges, &[], mode, &BTreeSet::new());
            assert!(
                artifact(&stored)
                    .content
                    .contains("Source-rule payload unavailable: malformed or unsupported schema")
            );
            assert_eq!(
                serde_json::to_string(&stored).unwrap(),
                serde_json::to_string(&text).unwrap()
            );
        }
    }

    #[test]
    fn malformed_rule_payload_uses_a_fixed_diagnostic_without_echoing_input() {
        // AC-0125: malformed/unknown payloads cannot become source prose.
        let mut invalid_version = serde_json::to_value(payload()).unwrap();
        invalid_version["schema_version"] = json!(999);
        let mut unknown_field = serde_json::to_value(payload()).unwrap();
        unknown_field["raw_source"] = json!("synthetic-private-payload");
        for invalid in [
            json!("synthetic-private-payload"),
            invalid_version,
            unknown_field,
        ] {
            let mut nodes = base();
            let mut rule = rule_node(
                "rule:invalid",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            );
            rule.props["rule"] = invalid;
            rule.props["name"] = json!("synthetic-private-payload");
            nodes.push(rule);
            let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
            let output = artifact(&bundle);
            assert!(
                output
                    .content
                    .contains("Source-rule payload unavailable: malformed or unsupported schema")
            );
            assert!(
                !serde_json::to_string(&bundle)
                    .unwrap()
                    .contains("synthetic-private-payload")
            );
            assert!(!output.content.contains("options.enabled"));
            assert_eq!(output.assertions.len(), 1);
        }
    }

    #[test]
    fn rule_inventory_distinguishes_legacy_and_empty_definition_evidence() {
        // AC-0170: readable v1 evidence stays absent, rather than appearing as
        // a new v2 analysis which happened to find no eligible initializer.
        let mut nodes = base();
        let legacy = rule_node(
            "rule:legacy",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let original = serde_json::to_vec(&legacy).unwrap();
        let mut current = rule_node(
            "rule:current",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut current_payload = payload();
        current_payload.schema_version = RULE_EVIDENCE_SCHEMA_VERSION;
        current_payload.local_definitions = Some(vec![]);
        current.props["rule"] = serde_json::to_value(current_payload).unwrap();
        nodes.extend([legacy, current]);
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let output = artifact(&bundle);
        assert!(output.content.contains(
            "Local-definition evidence was not collected in this version 1 observation."
        ));
        assert!(output.content.contains(
            "No local const initializer evidence was captured; completeness is not established."
        ));
        assert_eq!(
            output
                .content
                .matches(
                    "Initializer as written; value at use and business meaning are not established."
                )
                .count(),
            2
        );
        assert_eq!(output.assertions.len(), 2);
        assert!(output.assertions.iter().all(|assertion| assertion.summary
            == "Guarded local exit observation; behavioral interpretation not established"));
        assert_eq!(
            serde_json::to_vec(nodes.iter().find(|node| node.id == "rule:legacy").unwrap())
                .unwrap(),
            original
        );
    }

    #[test]
    fn rule_inventory_preserves_withholding_and_typed_false_without_raw_fallback() {
        // AC-0124 / AC-0125: render sanitized producer output only. The extra
        // raw field is deliberately hostile; this tests the renderer boundary,
        // not the separate source sanitizer's ability to detect secrets.
        let mut nodes = base();
        let mut rule = rule_node(
            "rule:redacted",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut observed = payload();
        observed.conditions[0].expression.display = SanitizedDisplay::from_sanitized(
            "password === \"[withheld string]\" && enabled !== false".into(),
        );
        observed.conditions[0].expression.capture = ExpressionCapture::Redacted;
        observed.redactions.push(SourceRedaction {
            source: evidence(),
            reason: RedactionReason::CredentialValue,
        });
        rule.props["rule"] = serde_json::to_value(&observed).unwrap();
        rule.props["raw_source"] = json!("ghp_synthetic_fixture_secret");
        nodes.push(rule);
        let mut withheld = rule_node(
            "rule:withheld",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        observed.effect = LocalExit::Return {
            value: Some(SourceExpression {
                source: evidence(),
                syntax_kind: "string".into(),
                display: SanitizedDisplay::from_sanitized("[withheld string]".into()),
                capture: ExpressionCapture::Redacted,
                literal: Some(LiteralEvidence::Withheld {
                    kind: LiteralKind::String,
                    reasons: vec![RedactionReason::CredentialValue],
                }),
            }),
        };
        withheld.props["rule"] = serde_json::to_value(observed).unwrap();
        nodes.push(withheld);
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let output = artifact(&bundle);
        assert!(output.content.contains("boolean false"));
        assert!(
            output
                .content
                .contains("Withheld String: [CredentialValue]")
        );
        assert!(output.content.contains("Redacted"));
        assert!(output.content.contains("CredentialValue"));
        assert!(
            !serde_json::to_string(&bundle)
                .unwrap()
                .contains("ghp_synthetic_fixture_secret")
        );
    }

    #[test]
    fn stored_rule_syntax_cannot_inject_inventory_markup() {
        // AC-0125: source fragments remain data in the portable Markdown view.
        let mut nodes = base();
        let mut rule = rule_node(
            "rule:markup",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut observed = payload();
        observed.conditions[0].expression.display = SanitizedDisplay::from_sanitized(
            "</table>\n# Forged | [claim](untrusted) `code`".into(),
        );
        rule.props["rule"] = serde_json::to_value(observed).unwrap();
        nodes.push(rule);
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let output = &artifact(&bundle).content;
        assert!(!output.contains("</table>"));
        assert!(!output.contains("\n# Forged"));
        assert!(output.contains("&lt;/table&gt;"));
        assert!(output.contains("\\# Forged \\| \\[claim\\]"));
    }

    fn index(bundle: &SpecBundle) -> serde_json::Value {
        let index = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "rule-evidence.json")
            .unwrap();
        assert!(index.assertions.is_empty());
        serde_json::from_str(&index.content).unwrap()
    }

    /// `big` observations in `big.ts`, 4 in `mid.ts`, one each in `extra`
    /// further files, and one malformed payload; every observation GOVERNS
    /// `sym:owner`.
    fn scaled_rules(big: usize, extra: usize) -> (Vec<Node>, Vec<Edge>) {
        let mut nodes = base();
        let mut edges = Vec::new();
        let mut add = |id: String, path: Option<String>| {
            let mut rule = rule_node(&id, Tier::Deterministic, ConfidenceTier::Confirmed);
            match path {
                Some(path) => rule.props["rule"]["exit_source"]["path"] = json!(path),
                None => rule.props["rule"] = json!({"schema_version": 99}),
            }
            nodes.push(rule);
            edges.push(edge(
                &id,
                "sym:owner",
                "GOVERNS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ));
        };
        for index in 0..big {
            add(format!("rule:big:{index:05}"), Some("big.ts".into()));
        }
        for index in 0..4 {
            add(format!("rule:mid:{index:05}"), Some("mid.ts".into()));
        }
        for file in 0..extra {
            add(
                format!("rule:extra:{file:03}"),
                Some(format!("extra{file:03}.ts")),
            );
        }
        add("rule:malformed".into(), None);
        (nodes, edges)
    }

    #[test]
    fn rule_evidence_groups_caps_and_indexes_every_observation() {
        // AC-0221 (T-0221): past 50 observations, rule-evidence.md groups by
        // exit-source file with counted omissions, and rule-evidence.json
        // indexes every observation and relationship exactly once.
        let extra = RULE_EVIDENCE_MAX_FILES + 5;
        let (nodes, edges) = scaled_rules(10, extra);
        let observations = 10 + 4 + extra + 1;
        let compile = || {
            compile_spec(
                &nodes,
                &edges,
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
            )
        };
        let bundle = compile();
        let output = artifact(&bundle);
        // The malformed observation asserts no relationship, as before.
        assert_eq!(output.assertions.len(), 2 * observations - 1);
        for expected in [
            "| R-01 | example/shop:big.ts | 10 |",
            "| R-02 | example/shop:mid.ts | 4 |",
            "| R-03 | Source-rule payload unavailable | 1 |",
            "… 7 more observations from this file — listed in `rule-evidence.json` under `R-01`.",
            "… 1 more observations from this file — listed in `rule-evidence.json` under `R-02`.",
            "| … | 8 more files, not detailed here — listed in `rule-evidence.json` | 8 |",
            "Source-rule payload unavailable: malformed or unsupported schema. No source expressions were rendered.",
        ] {
            assert!(output.content.contains(expected), "missing {expected}");
        }
        let rendered = 3 + 3 + (RULE_EVIDENCE_MAX_FILES - 2);
        assert_eq!(
            output.content.matches("\n### Observation ").count(),
            rendered
        );
        assert!(!output.content.contains("\n## Observation "));
        let shown_assertions = 2 * rendered - 1;
        assert!(output.content.contains(&format!(
            "Inline provenance is shown for the {shown_assertions} assertions of the representative observations above; the other {} carry",
            output.assertions.len() - shown_assertions
        )));

        let index = index(&bundle);
        assert_eq!(index["schema"], "cartograph.rule-evidence-index/v1");
        assert_eq!(index["observations"], observations);
        assert_eq!(index["assertions"], output.assertions.len());
        let files = index["files"].as_array().unwrap();
        assert_eq!(files.len(), 2 + extra + 1);
        assert_eq!(files[0]["path"], "big.ts");
        assert_eq!(files[2]["path"], serde_json::Value::Null);
        let mut indexed: Vec<&str> = files
            .iter()
            .flat_map(|file| {
                file["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(file["relationships"].as_array().unwrap())
            })
            .map(|id| id.as_str().unwrap())
            .collect();
        indexed.sort_unstable();
        let mut asserted: Vec<&str> = output
            .assertions
            .iter()
            .map(|assertion| assertion.id.as_str())
            .collect();
        asserted.sort_unstable();
        assert_eq!(indexed, asserted, "every assertion is indexed exactly once");

        assert_eq!(
            serde_json::to_string(&bundle.artifacts).unwrap(),
            serde_json::to_string(&compile().artifacts).unwrap()
        );
    }

    #[test]
    fn rule_evidence_prose_stays_bounded_as_observations_grow() {
        // AC-0221 (T-0221): prose size does not track observation count.
        let content_for = |big: usize| {
            let (nodes, edges) = scaled_rules(big, RULE_EVIDENCE_MAX_FILES + 5);
            let bundle = compile_spec(
                &nodes,
                &edges,
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
            );
            assert_eq!(
                index(&bundle)["observations"],
                big + 4 + RULE_EVIDENCE_MAX_FILES + 6
            );
            artifact(&bundle).content.clone()
        };
        let small = content_for(20);
        let large = content_for(2_000);
        assert!(
            large.len() < small.len() + 64,
            "{} vs {}",
            large.len(),
            small.len()
        );
    }
    fn frozen_fixtures() -> Vec<(Vec<Node>, Vec<Edge>)> {
        let mut fixtures = vec![
            scaled_rules(2, 0),
            scaled_rules(10, RULE_EVIDENCE_MAX_FILES + 5),
        ];
        let (mut nodes, edges) = scaled_rules(2, 0);
        nodes
            .iter_mut()
            .filter(|node| node.label == "BusinessRule")
            .for_each(|node| {
                if node.props["rule"]["schema_version"] == 1 {
                    node.props["rule"]["conditions"][0]["expression"]["display"] =
                        json!("<&|\\\"\n é 😀");
                    node.props["rule"]["exit_source"]["path"] = json!("source|<é>.ts");
                }
            });
        fixtures.push((nodes, edges));
        fixtures
    }

    // AC-0258: captured from the original renderer before this refactor;
    // no second implementation is retained in the production or test tree.
    #[test]
    fn selected_rule_inventory_matches_prechange_frozen_bytes() {
        let digests = [
            [
                "dda5f1331408b0bd8c0560eb6220908ca3b3b2936b10a5aa8efee86e8c61d781",
                "4bfd8ed1a3ec3ac5cb04f78b31b3d6bbe7f41ee881b07238f9f3b16c7ebc53c0",
            ],
            [
                "6d1c608fa97062322df8776d34188f251fcaa4a70a493477fb637b4ce06ee704",
                "cf4bc556dbf3e6b74a37536db24f9b62dfa3d1c58fdb9596649f48c13ee2551b",
            ],
            [
                "7949deb647f00eabdba7489fc0e9a66b55fee313bfcc9e0a15c1ee11ac79a56a",
                "65f7c28c4d80badd09452e6e77818e84b4ba8751e30c4af8bc3db16ae43e5834",
            ],
        ];
        for (fixture, (nodes, edges)) in frozen_fixtures().iter().enumerate() {
            let nodes: Vec<_> = nodes.iter().collect();
            let edges: Vec<_> = edges.iter().collect();
            for (projection, mode) in [ExportMode::VerifiedOnly, ExportMode::BestEffort]
                .into_iter()
                .enumerate()
            {
                let result = inventory(&nodes, &edges, mode);
                assert_eq!(
                    blake3::hash(&serde_json::to_vec(&result).unwrap())
                        .to_hex()
                        .as_str(),
                    digests[fixture][projection]
                );
            }
        }
    }

    fn reset_inventory_instrumentation() {
        OWNED_ASSERTIONS.with(|c| c.set(0));
        OWNED_CONTENT.with(|c| c.set(0));
        MARKDOWN_RENDERS.with(|c| c.set(0));
        INDEX_RENDERS.with(|c| c.set(0));
    }
    fn assert_no_inventory_output_owned() {
        OWNED_ASSERTIONS.with(|c| assert_eq!(c.get(), 0));
        OWNED_CONTENT.with(|c| assert_eq!(c.get(), 0));
    }

    // AC-0258: selected siblings are charged exactly, including the complete
    // assertion tail and the empty array of an index-only native artifact.
    #[test]
    fn selected_rule_inventory_enforces_exact_combined_boundaries() {
        for (nodes, edges) in frozen_fixtures() {
            let nodes: Vec<_> = nodes.iter().collect();
            let edges: Vec<_> = edges.iter().collect();
            for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
                let (markdown, index, assertions) = inventory(&nodes, &edges, mode);
                for selection in [
                    InventorySelection {
                        markdown: true,
                        index: true,
                    },
                    InventorySelection {
                        markdown: true,
                        index: false,
                    },
                    InventorySelection {
                        markdown: false,
                        index: true,
                    },
                    InventorySelection {
                        markdown: false,
                        index: false,
                    },
                ] {
                    let expected_assertions = if selection.markdown {
                        assertions.as_slice()
                    } else {
                        &[]
                    };
                    let exact = usize::from(selection.markdown) * markdown.len()
                        + usize::from(selection.index) * index.len()
                        + serde_json::to_vec(expected_assertions).unwrap().len();
                    for bound in [exact, exact + 1] {
                        let actual =
                            inventory_selected(&nodes, &edges, mode, selection, Some(bound))
                                .unwrap();
                        assert_eq!(
                            actual.markdown.as_ref(),
                            selection.markdown.then_some(&markdown)
                        );
                        assert_eq!(actual.index.as_ref(), selection.index.then_some(&index));
                        assert_eq!(actual.assertions, expected_assertions);
                    }
                    reset_inventory_instrumentation();
                    assert_eq!(
                        inventory_selected(&nodes, &edges, mode, selection, Some(exact - 1))
                            .unwrap_err(),
                        RuleInventoryLimitExceeded { maximum: exact - 1 }
                    );
                    assert_no_inventory_output_owned();
                    reset_inventory_instrumentation();
                    inventory_selected(&nodes, &edges, mode, selection, Some(exact)).unwrap();
                    if !selection.markdown {
                        MARKDOWN_RENDERS.with(|c| assert_eq!(c.get(), 0));
                        OWNED_ASSERTIONS.with(|c| assert_eq!(c.get(), 0));
                    }
                    if !selection.index {
                        INDEX_RENDERS.with(|c| assert_eq!(c.get(), 0));
                    }
                }
            }
        }
    }

    // AC-0258: escaping, JSON scalar formatting and provenance metadata never
    // allocate oversized output while the budget is still being measured.
    #[test]
    fn selected_rule_inventory_rejects_large_output_before_owning_it() {
        for field in ["display", "literal", "source", "method", "assertion_id"] {
            let mut nodes = base();
            let mut rule = rule_node("rule:large", Tier::Deterministic, ConfidenceTier::Confirmed);
            let mut value = payload();
            let huge = "<&|\\\"\n é".repeat(50_000);
            match field {
                "display" => {
                    value.conditions[0].expression.display = SanitizedDisplay::from_sanitized(huge)
                }
                "literal" => {
                    if let LocalExit::Return {
                        value: Some(expression),
                    } = &mut value.effect
                    {
                        expression.literal = Some(LiteralEvidence::Known {
                            value: KnownLiteral::String(huge),
                        });
                    }
                }
                "source" => value.exit_source.path = huge,
                "method" => rule.props["prov"]["method"] = json!({"kind":"human","actor":huge}),
                "assertion_id" => rule.id = huge,
                _ => unreachable!(),
            }
            rule.props["rule"] = serde_json::to_value(value).unwrap();
            nodes.push(rule);
            let nodes: Vec<_> = nodes.iter().collect();
            reset_inventory_instrumentation();
            assert!(
                inventory_selected(
                    &nodes,
                    &[],
                    ExportMode::VerifiedOnly,
                    InventorySelection {
                        markdown: true,
                        index: false
                    },
                    Some(4096)
                )
                .is_err(),
                "{field}"
            );
            assert_no_inventory_output_owned();
            INDEX_RENDERS.with(|c| assert_eq!(c.get(), 0));
            if ["display", "literal", "method"].contains(&field) {
                let expected = inventory_selected(
                    &nodes,
                    &[],
                    ExportMode::VerifiedOnly,
                    InventorySelection {
                        markdown: false,
                        index: true,
                    },
                    None,
                )
                .unwrap();
                let bytes = expected.index.as_ref().unwrap().len() + 2;
                reset_inventory_instrumentation();
                let actual = inventory_selected(
                    &nodes,
                    &[],
                    ExportMode::VerifiedOnly,
                    InventorySelection {
                        markdown: false,
                        index: true,
                    },
                    Some(bytes),
                )
                .unwrap();
                assert_eq!(actual.index, expected.index);
                assert!(actual.assertions.is_empty());
                MARKDOWN_RENDERS.with(|c| assert_eq!(c.get(), 0));
            }
        }
    }

    // AC-0258: projection policy remains the caller's complete input, and
    // selecting a native artifact preserves the ordinary compiler's bytes.
    #[test]
    fn selected_rule_inventory_matches_export_policy_and_rejected_inputs() {
        let mut nodes = base();
        nodes.extend([
            rule_node(
                "rule:confirmed",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            rule_node(
                "rule:strong",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            rule_node("rule:weak", Tier::Agentic, ConfidenceTier::InferredWeak),
        ]);
        let edges: Vec<_> = nodes
            .iter()
            .filter(|node| node.label == "BusinessRule")
            .map(|node| {
                edge(
                    &node.id,
                    "sym:owner",
                    "GOVERNS",
                    Tier::Deterministic,
                    ConfidenceTier::Confirmed,
                )
            })
            .collect();
        let rejected_hash = nodes
            .iter()
            .find(|node| node.id == "rule:strong")
            .unwrap()
            .props["prov"]["content_hash"]
            .as_str()
            .unwrap()
            .to_string();
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            for rejected in [BTreeSet::new(), BTreeSet::from([rejected_hash.clone()])] {
                let bundle = compile_spec(&nodes, &edges, &[], mode, &rejected);
                let projected: Vec<_> = nodes
                    .iter()
                    .filter(|node| {
                        let p = provenance(&node.props, &node.id);
                        !(p.confidence_tier == ConfidenceTier::InferredWeak
                            && mode == ExportMode::VerifiedOnly
                            || rejected.contains(&p.content_hash))
                    })
                    .collect();
                let projected_edges: Vec<_> = edges
                    .iter()
                    .filter(|edge| projected.iter().any(|node| node.id == edge.src))
                    .collect();
                let expected = artifact(&bundle);
                let index = bundle
                    .artifacts
                    .iter()
                    .find(|a| a.id == "rule-evidence-index")
                    .unwrap();
                let bytes = expected.content.len()
                    + index.content.len()
                    + serde_json::to_vec(&expected.assertions).unwrap().len();
                let actual = inventory_selected(
                    &projected,
                    &projected_edges,
                    mode,
                    InventorySelection {
                        markdown: true,
                        index: true,
                    },
                    Some(bytes),
                )
                .unwrap();
                assert_eq!(actual.markdown.as_deref(), Some(expected.content.as_str()));
                assert_eq!(actual.index.as_deref(), Some(index.content.as_str()));
                assert_eq!(actual.assertions, expected.assertions);
            }
        }
    }
}
