//! D1 frames arranged by subsystem area (#260, AC-0871).
use super::*;
use crate::workbench::capabilities::{CapabilityStory, validate_knowledge_stories};
use core_graph::overlay::{BaseRef, OverlayFact, OverlayItem};
use core_graph::{RevisionedSnapshot, StoreRevision};
use core_prov::{EvidenceRef, ProducingMethod, Provenance, Tier};
use serde_json::{Value, json};

const REPO: &str = "local/fixture";

fn evidence() -> Vec<EvidenceRef> {
    vec![EvidenceRef {
        repo: REPO.into(),
        path: "app/views.py".into(),
        byte_start: 0,
        byte_end: 10,
        commit_sha: "abc123".into(),
    }]
}

fn base_prov(fact: &str) -> Value {
    json!(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            evidence(),
            "t0.fixture",
            fact.as_bytes(),
        )
        .unwrap()
    )
}

fn derived_prov(fact: &str, tier: Tier) -> Value {
    json!(
        Provenance::new(
            tier,
            ConfidenceTier::InferredWeak,
            evidence(),
            "t2.fixture",
            fact.as_bytes()
        )
        .unwrap()
        .with_method(ProducingMethod::Rule {
            rule: "fixture.rule".into(),
            version: "1".into(),
        })
    )
}

fn node(id: &str, label: &str, mut props: Value) -> Node {
    props["prov"] = base_prov(id);
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn edge(src: &str, dst: &str, label: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: json!({ "prov": base_prov(&format!("{src} {label} {dst}")) }),
    }
}

fn file_id(path: &str) -> String {
    format!("file:{REPO}@{path}")
}

fn subsystem(name: &str, kind: &str, tier: Tier, files: &[&str]) -> OverlayFact {
    let id = format!("subsystem:{kind}:{}", name.to_lowercase().replace(' ', "-"));
    OverlayFact {
        item: OverlayItem::Node(Node {
            id: id.clone(),
            label: "Subsystem".into(),
            props: json!({
                "name": name, "kind": kind, "terms": [], "files": files.len(),
                "prov": derived_prov(&id, tier),
            }),
        }),
        cites: files.iter().map(|f| BaseRef::Node(file_id(f))).collect(),
    }
}

/// Capabilities whose handlers are spread over layered files, and the
/// Subsystems those files belong to.
struct Fixture {
    base: RevisionedSnapshot,
    capabilities: Vec<(Node, BTreeSet<BaseRef>)>,
    subsystems: Vec<OverlayFact>,
}

const CAPABILITIES: [(&str, &str, &[&str]); 6] = [
    (
        "a",
        "Manage alias",
        &[
            "app/api/alias.py",
            "app/dashboard/alias.py",
            "app/mailbox.py",
        ],
    ),
    ("b", "Manage mailbox", &["app/mailbox.py"]),
    ("c", "Manage session", &["app/db.py"]),
    ("d", "View health", &["app/health.py"]),
    ("e", "Custom alias", &["app/dashboard/alias.py"]),
    // One file in each of two areas: the name that sorts first takes it.
    ("f", "Transfer", &["app/mailbox.py", "app/api/alias.py"]),
];

fn fixture() -> Fixture {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut capabilities = Vec::new();
    for path in [
        "app/api/alias.py",
        "app/dashboard/alias.py",
        "app/mailbox.py",
        "app/db.py",
        "app/health.py",
    ] {
        nodes.push(node(&file_id(path), "File", json!({ "path": path })));
    }
    for (key, title, files) in CAPABILITIES {
        let mut cites = BTreeSet::new();
        for (i, file) in files.iter().enumerate() {
            let endpoint = format!("ep:{REPO}@GET:/{key}/{i}");
            let handler = format!("sym:{REPO}@{file}#{key}{i}");
            let test = format!("test:{key}{i}");
            nodes.push(node(
                &endpoint,
                "Endpoint",
                json!({ "method": "GET", "path": format!("/{key}/{i}") }),
            ));
            nodes.push(node(&handler, "Symbol", json!({ "name": "view" })));
            nodes.push(node(
                &test,
                "TestCase",
                json!({ "name": format!("checks {key}{i}"), "path": "tests/test_views.py" }),
            ));
            edges.push(edge(&endpoint, &handler, "HANDLES"));
            edges.push(edge(&handler, &file_id(file), "DEFINED_IN"));
            edges.push(edge(&test, &endpoint, "REQUESTS"));
            cites.insert(BaseRef::Edge(
                endpoint.clone(),
                handler.clone(),
                "HANDLES".into(),
            ));
            cites.insert(BaseRef::Node(endpoint));
        }
        let id = format!("capability:{key}");
        capabilities.push((
            Node {
                id: id.clone(),
                label: "Capability".into(),
                props: json!({ "title": title, "prov": derived_prov(&id, Tier::Semantic) }),
            },
            cites,
        ));
    }
    Fixture {
        base: RevisionedSnapshot {
            revision: StoreRevision {
                graph_instance: "fixture".into(),
                generation: 1,
            },
            graph: (nodes, edges),
        },
        capabilities,
        subsystems: vec![
            subsystem(
                "Alias",
                "cluster",
                Tier::Semantic,
                &["app/api/alias.py", "app/dashboard/alias.py"],
            ),
            subsystem("Mailbox", "cluster", Tier::Semantic, &["app/mailbox.py"]),
            subsystem("Shared code", "shared", Tier::Semantic, &["app/db.py"]),
        ],
    }
}

impl Fixture {
    fn stories(
        &self,
        mode: ExportMode,
        rejected: &BTreeSet<String>,
    ) -> crate::workbench::capabilities::KnowledgeStories<'_> {
        let candidates: Vec<CapabilityStory<'_>> = self
            .capabilities
            .iter()
            .map(|(capability, cites)| CapabilityStory { capability, cites })
            .collect();
        validate_knowledge_stories(&self.base, &candidates, &[], mode, rejected)
            .unwrap()
            .with_areas(&self.base, &self.subsystems, mode, rejected)
    }
}

fn arrangement(stories: &Stories<'_>) -> Vec<(Option<String>, String)> {
    stories
        .capabilities()
        .into_iter()
        .map(|(area, story)| {
            (
                area.map(|node| area_name(node).to_owned()),
                NodeName(story.node).to_string(),
            )
        })
        .collect()
}

// AC-0871: a capability's area is the concept Subsystem holding most of its
// handler files; a tie goes to the name that sorts first; the shared group
// and files in no Subsystem give none. Areas come first, by name.
#[test]
fn a_capability_takes_the_area_holding_most_of_its_handler_files() {
    let fixture = fixture();
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    assert_eq!((stories.included(), stories.arranged()), (6, 4));
    let area = |name: &str| Some(name.to_owned());
    assert_eq!(
        arrangement(&stories.0),
        vec![
            (area("Alias"), "Manage alias".to_owned()),
            (area("Alias"), "Custom alias".to_owned()),
            (area("Alias"), "Transfer".to_owned()),
            (area("Mailbox"), "Manage mailbox".to_owned()),
            (None, "Manage session".to_owned()),
            (None, "View health".to_owned()),
        ]
    );
}

// AC-0871: a Subsystem that is filtered, rejected, malformed or shadows a
// base fact arranges nothing, and the stories are otherwise unchanged.
#[test]
fn only_admissible_subsystems_arrange_capabilities() {
    let mut fixture = fixture();
    // A verified-only export keeps no derived story to arrange.
    let verified = fixture.stories(ExportMode::VerifiedOnly, &BTreeSet::new());
    assert_eq!((verified.included(), verified.arranged()), (0, 0));
    // Rejected candidates are not areas.
    let rejected: BTreeSet<String> = fixture
        .subsystems
        .iter()
        .map(|fact| {
            let OverlayItem::Node(node) = &fact.item else {
                unreachable!()
            };
            crate::workbench::provenance(&node.props, &node.id).content_hash
        })
        .collect();
    let stories = fixture.stories(ExportMode::BestEffort, &rejected);
    assert_eq!((stories.included(), stories.arranged()), (6, 0));
    // Not a semantic candidate, unnamed, or carrying a base fact's id.
    let unnamed = subsystem("", "cluster", Tier::Semantic, &["app/mailbox.py"]);
    let mut shadowing = subsystem("Health", "cluster", Tier::Semantic, &["app/health.py"]);
    if let OverlayItem::Node(node) = &mut shadowing.item {
        node.id = file_id("app/health.py");
    }
    fixture.subsystems = vec![
        subsystem("Alias", "cluster", Tier::Agentic, &["app/api/alias.py"]),
        unnamed,
        shadowing,
    ];
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    assert_eq!((stories.included(), stories.arranged()), (6, 0));
    let unarranged: Vec<String> = CAPABILITIES
        .iter()
        .map(|(_, title, _)| (*title).to_owned())
        .collect();
    assert_eq!(
        arrangement(&stories.0)
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>(),
        unarranged
    );
}

fn frames(stories: &Stories<'_>, base: &RevisionedSnapshot) -> (String, String, usize, usize) {
    let budget = Budget::new(None).unwrap();
    let mut assertions = Assertions::new(&budget);
    let mut asserted = BTreeSet::new();
    let mut table = Text::new(&budget, "").unwrap();
    let cited = derived_frame(
        "Capability",
        stories,
        (&mut assertions, &mut asserted),
        &mut table,
    )
    .unwrap();
    let facts = assertions.values.len();
    let nodes: Vec<&Node> = base.graph.0.iter().collect();
    let edges: Vec<&Edge> = base.graph.1.iter().collect();
    let mut scenarios = Text::new(&budget, "").unwrap();
    acceptance::acceptance_per_capability(
        (&nodes, &edges),
        stories,
        (&mut assertions, &mut asserted),
        &mut scenarios,
    )
    .unwrap();
    (table.finish(), scenarios.finish(), cited, facts)
}

// AC-0871: §1.3 lists each area once with its citation, then every
// capability under its area; the per-capability frames put one area heading
// over its capabilities' headings.
#[test]
fn frames_list_capabilities_area_by_area() {
    let fixture = fixture();
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    let (table, scenarios, cited, facts) = frames(&stories.0, &fixture.base);
    // Six capabilities and the two areas that hold any.
    assert_eq!((cited, facts), (8, 8), "{table}");
    assert!(
        table.contains("Capability areas: 2.") && table.contains("Capabilities with no area: 2."),
        "{table}"
    );
    assert!(table.contains("| Alias | 3 | 2 | F-"), "{table}");
    assert!(table.contains("| Mailbox | 1 | 1 | F-"), "{table}");
    assert!(!table.contains("Shared code"), "{table}");
    let rows: Vec<&str> = table
        .lines()
        .skip_while(|line| !line.starts_with("| Area | Capability |"))
        .skip(2)
        .map(|line| line.rsplitn(4, " | ").last().unwrap())
        .collect();
    assert_eq!(
        rows,
        [
            "| Alias | Manage alias",
            "| Alias | Custom alias",
            "| Alias | Transfer",
            "| Mailbox | Manage mailbox",
            "| Other capabilities | Manage session",
            "| Other capabilities | View health",
        ],
        "{table}"
    );
    let headings: Vec<&str> = scenarios
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect();
    assert_eq!(
        headings,
        [
            "### Alias",
            "#### Manage alias",
            "#### Custom alias",
            "#### Transfer",
            "### Mailbox",
            "#### Manage mailbox",
            "### Other capabilities",
            "#### Manage session",
            "#### View health",
        ],
        "{scenarios}"
    );
}

// AC-0871: with no Subsystem supplied every frame keeps its flat form.
#[test]
fn frames_without_areas_are_unchanged() {
    let mut fixture = fixture();
    fixture.subsystems.clear();
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    let (table, scenarios, cited, facts) = frames(&stories.0, &fixture.base);
    assert_eq!((cited, facts), (6, 6));
    assert!(!table.contains("Area"), "{table}");
    assert!(
        table.contains("| Capability | Operations | Examples | Citation |"),
        "{table}"
    );
    assert!(
        scenarios.contains("### Manage alias\n") && !scenarios.contains("####"),
        "{scenarios}"
    );
}
