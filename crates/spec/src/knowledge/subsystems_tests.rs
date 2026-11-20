// AC-0870: subsystem candidates group files by concept across directories.
use super::*;
use core_graph::StoreRevision;
use core_graph::overlay::OverlayStore;

const REPO: &str = "local/fixture";

fn evidence(path: &str) -> EvidenceRef {
    EvidenceRef {
        repo: REPO.into(),
        path: path.into(),
        byte_start: 0,
        byte_end: 10,
        commit_sha: "abc123".into(),
    }
}

fn base_prov(path: &str) -> serde_json::Value {
    serde_json::json!(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence(path)],
            "t0.fixture",
            path.as_bytes(),
        )
        .unwrap()
    )
}

fn file_id(path: &str) -> String {
    format!("file:{REPO}@{path}")
}

fn symbol_id(path: &str, name: &str) -> String {
    format!("sym:{REPO}@{path}#{name}")
}

#[derive(Default)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn edge(&mut self, src: String, dst: String, label: &str) {
        self.edges.push(Edge {
            src,
            dst,
            label: label.into(),
            props: serde_json::json!({}),
        });
    }

    /// A source file and the symbols it defines.
    fn file(&mut self, path: &str, symbols: &[&str]) {
        self.nodes.push(Node {
            id: file_id(path),
            label: "File".into(),
            props: serde_json::json!({ "path": path, "prov": base_prov(path) }),
        });
        for name in symbols {
            self.member(path, name, "Symbol");
        }
    }

    fn member(&mut self, path: &str, name: &str, label: &str) {
        self.nodes.push(Node {
            id: symbol_id(path, name),
            label: label.into(),
            props: serde_json::json!({ "name": name, "prov": base_prov(path) }),
        });
        self.edge(symbol_id(path, name), file_id(path), "DEFINED_IN");
    }

    fn calls(&mut self, from: (&str, &str), to: (&str, &str)) {
        self.edge(symbol_id(from.0, from.1), symbol_id(to.0, to.1), "CALLS");
    }

    fn imports(&mut self, from: &str, to: &str) {
        self.edge(file_id(from), file_id(to), "IMPORTS");
    }

    fn snapshot(&self) -> RevisionedSnapshot {
        RevisionedSnapshot {
            revision: StoreRevision {
                graph_instance: "fixture".into(),
                generation: 1,
            },
            graph: (self.nodes.clone(), self.edges.clone()),
        }
    }
}

const ALIAS: [&str; 4] = [
    "app/api/views/alias.py",
    "app/dashboard/views/alias_transfer.py",
    "app/alias_utils.py",
    "tasks/cleanup_alias.py",
];
const MAILBOX: [&str; 4] = [
    "app/api/views/mailbox.py",
    "app/dashboard/views/mailbox_detail.py",
    "app/mailbox_utils.py",
    "app/admin/mailbox.py",
];
const DOMAIN: [&str; 4] = [
    "app/api/views/custom_domain.py",
    "app/dashboard/views/custom_domain.py",
    "app/custom_domain_utils.py",
    "app/custom_domain_validation.py",
];

/// Three concepts, each filed under an API layer, a dashboard layer and a
/// utility module, as a layered legacy application files them.
fn layered() -> Fixture {
    let mut fixture = Fixture::default();
    for (paths, noun) in [(ALIAS, "alias"), (MAILBOX, "mailbox"), (DOMAIN, "domain")] {
        for path in paths {
            fixture.file(path, &[&format!("get_{noun}"), &format!("delete_{noun}")]);
        }
        // Views call their utility module.
        for path in [paths[0], paths[1], paths[3]] {
            fixture.calls(
                (path, &format!("get_{noun}")),
                (paths[2], &format!("get_{noun}")),
            );
            fixture.imports(path, paths[2]);
        }
    }
    fixture
}

fn groups(report: &SubsystemReport) -> Vec<(String, String, Vec<String>)> {
    report
        .facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) => Some((
                node.props["name"].as_str().unwrap().to_owned(),
                node.props["kind"].as_str().unwrap().to_owned(),
                fact.cites
                    .iter()
                    .map(|cite| match cite {
                        BaseRef::Node(id) => id.split_once('@').unwrap().1.to_owned(),
                        BaseRef::Edge(..) => panic!("a group cites its files"),
                    })
                    .collect(),
            )),
            OverlayItem::Edge(_) => None,
        })
        .collect()
}

fn sorted(paths: [&str; 4]) -> Vec<String> {
    let mut paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
    paths.sort();
    paths
}

// AC-0870: one group per concept, whatever directory its files are in, named
// by the term its files share.
#[test]
fn files_of_one_concept_are_one_subsystem_across_directories() {
    let base = layered().snapshot();
    let report = derive_subsystems(&base);
    assert_eq!(
        (report.files, report.grouped, report.shared, report.omission),
        (12, 12, 0, None)
    );
    let mut found = groups(&report);
    found.sort();
    assert_eq!(
        found,
        vec![
            ("Alias".to_owned(), "cluster".to_owned(), sorted(ALIAS)),
            (
                "Custom domain".to_owned(),
                "cluster".to_owned(),
                sorted(DOMAIN)
            ),
            ("Mailbox".to_owned(), "cluster".to_owned(), sorted(MAILBOX)),
        ]
    );
}

// AC-0870: every group and membership is a weak semantic candidate naming
// its rule, and each member file has one MEMBER_OF edge citing that file.
#[test]
fn groups_are_weak_candidates_with_one_membership_per_file() {
    let base = layered().snapshot();
    let report = derive_subsystems(&base);
    let mut members = 0;
    for fact in &report.facts {
        let props = match &fact.item {
            OverlayItem::Node(node) => {
                assert_eq!(node.label, "Subsystem");
                assert!(node.id.starts_with("subsystem:cluster:"));
                assert_eq!(node.props["files"], 4);
                &node.props
            }
            OverlayItem::Edge(edge) => {
                members += 1;
                assert_eq!(edge.label, "MEMBER_OF");
                assert!(edge.dst.starts_with("subsystem:cluster:"));
                assert_eq!(
                    fact.cites,
                    BTreeSet::from([BaseRef::Node(edge.src.clone())])
                );
                &edge.props
            }
        };
        let prov: Provenance = serde_json::from_value(props["prov"].clone()).unwrap();
        assert_eq!(prov.tier, Tier::Semantic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
        assert_eq!(
            prov.method,
            ProducingMethod::Rule {
                rule: SUBSYSTEM_RULE.into(),
                version: SUBSYSTEM_RULE_VERSION.into(),
            }
        );
        assert!(!prov.evidence.is_empty());
    }
    assert_eq!(members, 12);
}

// AC-0870: the overlay admits the candidates as published (R-INT-1, R-INT-2).
#[test]
fn the_overlay_admits_the_candidates() {
    let base = layered().snapshot();
    let report = derive_subsystems(&base);
    let mut overlay = OverlayStore::open_in_memory().unwrap();
    overlay.bind_instance("fixture").unwrap();
    overlay
        .publish(
            &base,
            core_graph::overlay::OverlayRecipe {
                recipe: SUBSYSTEM_RULE.into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            report.facts,
        )
        .unwrap();
}

// AC-0870: the same base gives the same candidates in any input order, and
// the base is not changed.
#[test]
fn candidates_do_not_depend_on_input_order() {
    let base = layered().snapshot();
    let before = base.clone();
    let report = derive_subsystems(&base);
    assert_eq!(base, before);
    let mut reversed = base.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_subsystems(&reversed), report);
}

// AC-0870: tests, vendored code and versioned third-party trees are not units.
#[test]
fn test_and_vendored_files_are_not_grouped() {
    let mut fixture = layered();
    fixture.file("tests/test_alias.py", &["test_alias_create"]);
    fixture.file("app/alias_checks.py", &[]);
    fixture.member("app/alias_checks.py", "checks alias", "TestCase");
    fixture.file("static/vendor/alias.js", &["alias"]);
    fixture.file("static/DataTables-1.10.16/alias.js", &["alias"]);
    fixture.file("static/alias.min.js", &["alias"]);
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!((report.files, report.grouped), (12, 12));
    assert!(excluded("packages/core/e2e/order.e2e-spec.ts"));
    assert!(excluded("src/order.spec.ts") && excluded("types/order.d.ts"));
    assert!(!excluded("app/testing_tools/latest.py") && !excluded("app/contest/a-b.py"));
}

// AC-0870: a file most other files call or import is set aside as shared
// instead of joining, and gluing, the concept groups.
#[test]
fn a_file_used_from_everywhere_is_shared() {
    let mut fixture = layered();
    fixture.file("app/db.py", &["session"]);
    for path in ALIAS.iter().chain(&MAILBOX).chain(&DOMAIN) {
        fixture.imports(path, "app/db.py");
    }
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!((report.files, report.grouped, report.shared), (13, 12, 1));
    let shared: Vec<_> = groups(&report)
        .into_iter()
        .filter(|(_, kind, _)| kind == "shared")
        .collect();
    assert_eq!(
        shared,
        vec![(
            "Shared code".to_owned(),
            "shared".to_owned(),
            vec!["app/db.py".to_owned()]
        )]
    );
}

// AC-0870: a base past the file bound derives nothing and says why; a base
// with nothing to separate derives nothing.
#[test]
fn a_base_past_the_file_bound_is_a_named_omission() {
    let mut fixture = Fixture::default();
    for i in 0..=MAX_SUBSYSTEM_FILES {
        fixture.file(&format!("app/f{i}.py"), &["run"]);
    }
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!(report.omission, Some(SUBSYSTEM_FILE_CAP));
    assert_eq!(report.files, MAX_SUBSYSTEM_FILES + 1);
    assert!(report.facts.is_empty());

    let mut lone = Fixture::default();
    lone.file("app/main.py", &["main"]);
    assert_eq!(
        derive_subsystems(&lone.snapshot()),
        SubsystemReport {
            files: 1,
            ..SubsystemReport::default()
        }
    );
    assert_eq!(
        derive_subsystems(&Fixture::default().snapshot()),
        SubsystemReport::default()
    );
}

// AC-0870: a plural folds onto its singular only when the corpus has both.
#[test]
fn plurals_fold_onto_a_singular_the_corpus_uses() {
    let vocabulary: BTreeSet<String> = ["domain", "alias", "policy", "box"]
        .map(str::to_owned)
        .into();
    assert_eq!(fold("domains", &vocabulary).as_deref(), Some("domain"));
    assert_eq!(fold("policies", &vocabulary).as_deref(), Some("policy"));
    assert_eq!(fold("boxes", &vocabulary).as_deref(), Some("box"));
    assert_eq!(fold("alias", &vocabulary), None);
    assert_eq!(fold("status", &vocabulary), None);
    assert_eq!(
        subject_words("getCustomDomainById"),
        vec!["custom".to_owned(), "domain".to_owned()]
    );
}

// AC-0870: a local source's registry key is random per registration; the
// groups follow file paths, not that key.
#[test]
fn groups_do_not_depend_on_the_registry_key() {
    let base = layered().snapshot();
    let renamed: (Vec<Node>, Vec<Edge>) = serde_json::from_str(
        &serde_json::to_string(&base.graph)
            .unwrap()
            .replace(REPO, "local/zz-another-registration"),
    )
    .unwrap();
    let other = RevisionedSnapshot {
        revision: base.revision.clone(),
        graph: renamed,
    };
    assert_eq!(
        groups(&derive_subsystems(&other)),
        groups(&derive_subsystems(&base))
    );
}
