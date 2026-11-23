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
// and files in no Subsystem give none. Areas come first, by name, and each
// area's capabilities by name (AC-1205).
#[test]
fn a_capability_takes_the_area_holding_most_of_its_handler_files() {
    let fixture = fixture();
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    assert_eq!((stories.included(), stories.arranged()), (6, 4));
    let area = |name: &str| Some(name.to_owned());
    assert_eq!(
        arrangement(&stories.0),
        vec![
            (area("Alias"), "Custom alias".to_owned()),
            (area("Alias"), "Manage alias".to_owned()),
            (area("Alias"), "Transfer".to_owned()),
            (area("Mailbox"), "Manage mailbox".to_owned()),
            (None, "Manage session".to_owned()),
            (None, "View health".to_owned()),
        ]
    );
}

// AC-1602 (#2511): the areas a draft may cite are the concept Subsystems the
// arrangement kept, each once; the shared group is not one.
#[test]
fn area_nodes_are_the_kept_concept_subsystems_once_each() {
    let fixture = fixture();
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    let ids: Vec<&str> = stories.area_nodes().iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        ["subsystem:cluster:alias", "subsystem:cluster:mailbox"]
    );
    let candidates: Vec<CapabilityStory<'_>> = fixture
        .capabilities
        .iter()
        .map(|(capability, cites)| CapabilityStory { capability, cites })
        .collect();
    let unarranged = validate_knowledge_stories(
        &fixture.base,
        &candidates,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(unarranged.area_nodes().is_empty());
}

// AC-1205/T-1205 (#2224): a Capability id hashes the cited facts' ids,
// which carry the registration's random key, so stories (and the US-R
// numbers they are printed under) follow names, never ids: the same
// capabilities under ids in the reverse order are listed the same way.
#[test]
fn stories_follow_capability_names_not_ids() {
    let listed = |fixture: &Fixture| {
        let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
        let numbered: Vec<String> = stories
            .0
            .included
            .iter()
            .map(|story| NodeName(story.node).to_string())
            .collect();
        (numbered, arrangement(&stories.0))
    };
    let ordered = fixture();
    let mut reversed = fixture();
    for (node, _) in &mut reversed.capabilities {
        let key = node.id.strip_prefix("capability:").unwrap();
        let id = format!(
            "capability:{}",
            char::from(b'z' - (key.as_bytes()[0] - b'a'))
        );
        node.props["prov"] = derived_prov(&id, Tier::Semantic);
        node.id = id;
    }
    let (numbered, arranged) = listed(&ordered);
    let mut by_name = numbered.clone();
    by_name.sort();
    assert_eq!(numbered, by_name);
    assert_eq!(listed(&reversed), (numbered, arranged));
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
    // A file count that is not the cited membership.
    let mut miscounted = subsystem("Mailbox", "cluster", Tier::Semantic, &["app/mailbox.py"]);
    if let OverlayItem::Node(node) = &mut miscounted.item {
        node.props["files"] = json!(7);
    }
    let mut uncounted = subsystem("Db", "cluster", Tier::Semantic, &["app/db.py"]);
    if let OverlayItem::Node(node) = &mut uncounted.item {
        node.props["files"] = json!("one");
    }
    fixture.subsystems = vec![
        subsystem("Alias", "cluster", Tier::Agentic, &["app/api/alias.py"]),
        unnamed,
        shadowing,
        miscounted,
        uncounted,
    ];
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    assert_eq!((stories.included(), stories.arranged()), (6, 0));
    // In name order, never id order (#2224).
    let mut unarranged: Vec<String> = CAPABILITIES
        .iter()
        .map(|(_, title, _)| (*title).to_owned())
        .collect();
    unarranged.sort();
    assert_eq!(
        arrangement(&stories.0)
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>(),
        unarranged
    );
}

// AC-0913: work no request starts takes the area of the code its cited
// schedule or entry point enters.
#[test]
fn a_capability_started_by_a_schedule_takes_the_area_of_what_it_enters() {
    let mut fixture = fixture();
    let schedule = format!("sched:{REPO}@crontab.yml#0");
    let entered = format!("sym:{REPO}@app/mailbox.py#purge");
    let (nodes, edges) = &mut fixture.base.graph;
    nodes.push(node(
        &schedule,
        "Schedule",
        json!({ "file": "crontab.yml", "name": "purge mailboxes" }),
    ));
    nodes.push(node(&entered, "Symbol", json!({ "name": "purge" })));
    edges.push(edge(&schedule, &entered, "ENTRY"));
    edges.push(edge(&entered, &file_id("app/mailbox.py"), "DEFINED_IN"));
    // A second schedule enters a file directly, in no Subsystem.
    let loose = format!("sched:{REPO}@crontab.yml#1");
    nodes.push(node(
        &loose,
        "Schedule",
        json!({ "file": "crontab.yml", "name": "check health" }),
    ));
    edges.push(edge(&loose, &file_id("app/health.py"), "ENTRY"));
    for (key, title, cite) in [
        ("jobs", "Run scheduled jobs", &schedule),
        ("loose", "Run health checks", &loose),
    ] {
        let id = format!("capability:entry-point:{key}");
        fixture.capabilities.push((
            Node {
                id: id.clone(),
                label: "Capability".into(),
                props: json!({ "title": title, "prov": derived_prov(&id, Tier::Semantic) }),
            },
            BTreeSet::from([BaseRef::Node(cite.clone())]),
        ));
    }
    let stories = fixture.stories(ExportMode::BestEffort, &BTreeSet::new());
    assert_eq!((stories.included(), stories.arranged()), (8, 5));
    let arranged = arrangement(&stories.0);
    assert!(
        arranged.contains(&(Some("Mailbox".to_owned()), "Run scheduled jobs".to_owned())),
        "{arranged:?}"
    );
    assert!(
        arranged.contains(&(None, "Run health checks".to_owned())),
        "{arranged:?}"
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
            "| Alias | Custom alias",
            "| Alias | Manage alias",
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
            "#### Custom alias",
            "#### Manage alias",
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
