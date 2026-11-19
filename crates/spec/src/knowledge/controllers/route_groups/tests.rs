use super::*;
use core_graph::StoreRevision;

const REPO: &str = "repo";
const COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";
const APP: &str = "from flask import Flask
from users import bp
app = Flask(__name__)
app.register_blueprint(bp, url_prefix='/api/users')
@app.route('/health')
def health():
    return 'ok'
@app.route('/ready', endpoint='users.ready')
def ready():
    return 'ok'
";
const USERS: &str = "from flask import Blueprint
bp = Blueprint('users', __name__)
@bp.route('/', methods=['GET'])
def list_users():
    return []
@bp.post('/<id>/ban')
def ban(id):
    return ''
class Views:
    @bp.get('/me')
    def me(self):
        return ''
";
// A second Blueprint that shares the name, never registered.
const LEGACY: &str = "from flask import Blueprint
old = Blueprint('users', __name__, url_prefix='/legacy')
@old.post('/all')
def everyone():
    return []
";

// AC-0801: a Blueprint name is whatever non-empty literal Flask accepts.
#[test]
fn controller_capability_keeps_a_flask_blueprint_name_with_a_space() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("admin.py"),
        "from flask import Blueprint
bp = Blueprint('user admin', __name__)
@bp.get('/all')
def everyone():
    return []
",
    )
    .unwrap();
    let facts = adapters_lang_python::extract_dir(
        dir.path(),
        &adapters_lang_python::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    let report = derive_controller_capabilities(&revisioned(facts.nodes, facts.edges));
    assert_eq!(titles(&report), ["View user"]);
}

fn snapshot() -> RevisionedSnapshot {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in [("app.py", APP), ("users.py", USERS), ("legacy.py", LEGACY)] {
        std::fs::write(dir.path().join(name), source).unwrap();
    }
    let facts = adapters_lang_python::extract_dir(
        dir.path(),
        &adapters_lang_python::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (facts.nodes, facts.edges),
    }
}

fn capability(fact: &OverlayFact) -> &Node {
    let OverlayItem::Node(node) = &fact.item else {
        panic!("Capability is a node")
    };
    node
}

/// The candidate whose support is in `path`, of the two that share a title.
fn from_file<'a>(report: &'a CapabilityReport, path: &str) -> Option<&'a OverlayFact> {
    let handler = format!("sym:{REPO}@{path}#");
    report.facts.iter().find(|fact| {
        fact.cites
            .iter()
            .any(|cite| matches!(cite, BaseRef::Node(id) if id.starts_with(&handler)))
    })
}

// AC-0801: a Flask Blueprint gives one Capability per handler file, citing
// each endpoint, handler and relation; an application route gives none.
#[test]
fn controller_capability_groups_real_flask_blueprint_routes() {
    let base = snapshot();
    let before = base.clone();
    let report = derive_controller_capabilities(&base);
    assert_eq!(base, before);
    assert_eq!(report.oversized_groups, 0);
    // Two Blueprints named `users` in two files stay two candidates, and the
    // application route that spells its endpoint `users.ready` is in neither.
    // AC-0851: each file names its subject, served under the Blueprint.
    assert_eq!(report.facts.len(), 2);
    assert_eq!(titles(&report), ["Manage legacy (users)", "Manage users"]);
    assert_ne!(
        capability(&report.facts[0]).id,
        capability(&report.facts[1]).id
    );

    let users = from_file(&report, "users.py").unwrap();
    let node = capability(users);
    assert!(node.id.starts_with("capability:http-controller:"));
    assert_eq!(node.props.as_object().unwrap().len(), 2);
    let sym = |name: &str| format!("sym:{REPO}@users.py#{name}");
    let list = format!("ep:{REPO}@GET:/api/users/");
    let ban = format!("ep:{REPO}@POST:/api/users/<id>/ban");
    let me = format!("ep:{REPO}@GET:/api/users/me");
    let expected: BTreeSet<_> = [
        BaseRef::Node(list.clone()),
        BaseRef::Node(ban.clone()),
        BaseRef::Node(me.clone()),
        BaseRef::Node(sym("list_users")),
        BaseRef::Node(sym("ban")),
        BaseRef::Node(sym("Views.me")),
        BaseRef::Edge(list, sym("list_users"), "HANDLES".into()),
        BaseRef::Edge(ban, sym("ban"), "HANDLES".into()),
        BaseRef::Edge(me, sym("Views.me"), "HANDLES".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(users.cites, expected);
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert!(prov.validate().is_ok());
    assert_eq!(prov.tier, Tier::Semantic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
    assert_eq!(
        prov.method,
        ProducingMethod::Rule {
            rule: CAPABILITY_CONTROLLER_RULE.into(),
            version: CAPABILITY_CONTROLLER_RULE_VERSION.into()
        }
    );
    // The registration in app.py is part of what proves the served routes.
    let paths: BTreeSet<_> = prov.evidence.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, BTreeSet::from(["app.py", "users.py"]));

    let legacy = from_file(&report, "legacy.py").unwrap();
    let all = format!("ep:{REPO}@POST:/legacy/all");
    let handler = format!("sym:{REPO}@legacy.py#everyone");
    let expected: BTreeSet<_> = [
        BaseRef::Node(all.clone()),
        BaseRef::Node(handler.clone()),
        BaseRef::Edge(all, handler, "HANDLES".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(legacy.cites, expected);

    let mut reversed = base.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_controller_capabilities(&reversed), report);
}

// AC-0801: a Flask route whose group, handler or support is not what the
// Python adapter proves is left out of its Blueprint's candidate.
#[test]
fn controller_capability_omits_unproven_flask_routes() {
    let base = snapshot();
    let list = format!("ep:{REPO}@GET:/api/users/");
    let handler = format!("sym:{REPO}@users.py#list_users");
    let edit = |change: &dyn Fn(&mut Node)| {
        let mut changed = base.clone();
        for node in &mut changed.graph.0 {
            change(node);
        }
        changed
    };
    let without_list = |snapshot: &RevisionedSnapshot| {
        let report = derive_controller_capabilities(snapshot);
        let users = from_file(&report, "users.py").unwrap();
        report.facts.len() == 2
            && users.cites.len() == 6
            && !users.cites.contains(&BaseRef::Node(list.clone()))
    };
    assert!(!without_list(&base));
    let endpoint_edits: [&dyn Fn(&mut Node); 10] = [
        // No Blueprint name, an empty one, or no endpoint name at all.
        &|node| node.props["endpoint_name"] = "list_users".into(),
        &|node| node.props["endpoint_name"] = ".list_users".into(),
        &|node| node.props["endpoint_name"] = serde_json::Value::Null,
        // Not a route a Blueprint serves.
        &|node| node.props["mount"] = serde_json::Value::Null,
        // Another framework, language or handler than the adapter's own.
        &|node| node.props["framework"] = "fastapi".into(),
        &|node| node.props["language"] = "java".into(),
        &|node| node.props["handler_sym"] = "sym:other".into(),
        // Support that is another extractor's, not source-backed, or from
        // another repository.
        &|node| node.props["prov"]["extractor_id"] = "t0.adapter-ts".into(),
        &|node| node.props["prov"]["basis"] = "structural".into(),
        &|node| node.props["prov"]["evidence"][2]["repo"] = "other".into(),
    ];
    for change in endpoint_edits {
        assert!(without_list(&edit(&|node| if node.id == list {
            change(node);
        })));
    }
    let handler_edits: [&dyn Fn(&mut Node); 3] = [
        &|node| node.props["kind"] = "class".into(),
        &|node| node.props["language"] = "java".into(),
        &|node| node.props["prov"]["evidence"][0]["path"] = "other.py".into(),
    ];
    for change in handler_edits {
        assert!(without_list(&edit(&|node| if node.id == handler {
            change(node);
        })));
    }
    // A second handler for the route is ambiguous.
    let mut twice = base.clone();
    let mut second = twice
        .graph
        .1
        .iter()
        .find(|edge| edge.src == list && edge.label == "HANDLES")
        .unwrap()
        .clone();
    second.dst = format!("sym:{REPO}@users.py#ban");
    twice.graph.1.push(second);
    assert!(without_list(&twice));
}

const GO_MAIN: &str = "package main

import (
\t\"net/http\"
\t\"github.com/go-chi/chi/v5\"
\t\"example.com/shop/internal/orders\"
)

func health(w http.ResponseWriter, r *http.Request) {}

func main() {
\tr := chi.NewRouter()
\tr.Get(\"/health\", health)
\tr.Get(\"/orders\", orders.List)
\tr.Post(\"/orders\", orders.Create)
\thttp.ListenAndServe(\":8080\", r)
}
";
const GO_HANDLERS: &str = "package orders

import \"net/http\"

func List(w http.ResponseWriter, r *http.Request) {}

func Create(w http.ResponseWriter, r *http.Request) {}
";
const RUST_MAIN: &str = "mod routes;
use axum::{routing::get, Router};

async fn health() -> &'static str { \"ok\" }

fn app() -> Router {
    Router::new()
        .route(\"/health\", get(health))
        .route(\"/orders\", get(routes::orders::list).post(routes::orders::create))
}

fn main() { let _ = app(); }
";
const RUST_ORDERS: &str = "pub async fn list() -> &'static str { \"x\" }
pub async fn create() -> &'static str { \"x\" }
";

fn revisioned(nodes: Vec<Node>, edges: Vec<core_graph::Edge>) -> RevisionedSnapshot {
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (nodes, edges),
    }
}

fn go_snapshot() -> RevisionedSnapshot {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("internal/orders")).unwrap();
    for (name, source) in [
        ("go.mod", "module example.com/shop\n\ngo 1.22\n"),
        ("main.go", GO_MAIN),
        ("internal/orders/handlers.go", GO_HANDLERS),
    ] {
        std::fs::write(dir.path().join(name), source).unwrap();
    }
    let facts = adapters_lang_go::extract_dir(
        dir.path(),
        &adapters_lang_go::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    revisioned(facts.nodes, facts.edges)
}

fn rust_snapshot() -> RevisionedSnapshot {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/routes")).unwrap();
    for (name, source) in [
        (
            "Cargo.toml",
            "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\naxum = \"0.7\"\n",
        ),
        ("src/main.rs", RUST_MAIN),
        ("src/routes/mod.rs", "pub mod orders;\n"),
        ("src/routes/orders.rs", RUST_ORDERS),
    ] {
        std::fs::write(dir.path().join(name), source).unwrap();
    }
    let facts = adapters_lang_rust::extract_dir(
        dir.path(),
        &adapters_lang_rust::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    revisioned(facts.nodes, facts.edges)
}

fn titles(report: &CapabilityReport) -> Vec<&str> {
    let mut titles: Vec<_> = report
        .facts
        .iter()
        .map(|fact| capability(fact).props["title"].as_str().unwrap())
        .collect();
    titles.sort();
    titles
}

/// The candidate for the two `/orders` routes whose handlers are in `file`,
/// checked for its citations and provenance.
fn orders_group<'a>(
    report: &'a CapabilityReport,
    file: &str,
    (list, create): (&str, &str),
    registered_in: &str,
) -> &'a OverlayFact {
    let fact = from_file(report, file).unwrap();
    let sym = |name: &str| format!("sym:{REPO}@{file}#{name}");
    let get = format!("ep:{REPO}@GET:/orders");
    let post = format!("ep:{REPO}@POST:/orders");
    let expected: BTreeSet<_> = [
        BaseRef::Node(get.clone()),
        BaseRef::Node(post.clone()),
        BaseRef::Node(sym(list)),
        BaseRef::Node(sym(create)),
        BaseRef::Edge(get, sym(list), "HANDLES".into()),
        BaseRef::Edge(post, sym(create), "HANDLES".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(fact.cites, expected);
    let node = capability(fact);
    assert!(node.id.starts_with("capability:http-controller:"));
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert!(prov.validate().is_ok());
    assert_eq!(prov.tier, Tier::Semantic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
    // The registration in another file is part of the support.
    let paths: BTreeSet<_> = prov.evidence.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, BTreeSet::from([file, registered_in]));
    fact
}

// AC-0801: Go handler functions give one Capability per package and file,
// and Rust handler functions one per module and file.
#[test]
fn controller_capability_groups_real_go_and_rust_routes_by_module() {
    let go = go_snapshot();
    let before = go.clone();
    let report = derive_controller_capabilities(&go);
    assert_eq!(go, before);
    assert_eq!(titles(&report), ["Manage orders", "View operations"]);
    let orders = orders_group(
        &report,
        "internal/orders/handlers.go",
        ("List", "Create"),
        "main.go",
    );
    assert_eq!(capability(orders).props["title"], "Manage orders");
    let mut reversed = go.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_controller_capabilities(&reversed), report);

    let rust = rust_snapshot();
    let before = rust.clone();
    let report = derive_controller_capabilities(&rust);
    assert_eq!(rust, before);
    assert_eq!(titles(&report), ["Manage orders", "View operations"]);
    let orders = orders_group(
        &report,
        "src/routes/orders.rs",
        ("list", "create"),
        "src/main.rs",
    );
    assert_eq!(capability(orders).props["title"], "Manage orders");
    let mut reversed = rust.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_controller_capabilities(&reversed), report);
}

// AC-0801: a Go or Rust route whose handler is not the plain function the
// adapter names in a package or module of its own is left out.
#[test]
fn controller_capability_omits_unproven_go_and_rust_routes() {
    let get = format!("ep:{REPO}@GET:/orders");
    let check =
        |base: &RevisionedSnapshot, file: &str, list: &str, edits: &[&dyn Fn(&mut Node)]| {
            let handler = format!("sym:{REPO}@{file}#{list}");
            let without_list = |snapshot: &RevisionedSnapshot| {
                let report = derive_controller_capabilities(snapshot);
                from_file(&report, file).is_some_and(|orders| {
                    report.facts.len() == 2
                        && orders.cites.len() == 3
                        && !orders.cites.contains(&BaseRef::Node(get.clone()))
                })
            };
            assert!(!without_list(base));
            for (target, changes) in [(&handler, edits), (&get, ENDPOINT_EDITS)] {
                for change in changes {
                    let mut changed = base.clone();
                    for node in &mut changed.graph.0 {
                        if node.id == *target {
                            change(node);
                        }
                    }
                    assert!(without_list(&changed));
                }
            }
        };
    const ENDPOINT_EDITS: &[&dyn Fn(&mut Node)] = &[
        &|node| node.props["framework"] = "flask".into(),
        &|node| node.props["language"] = "python".into(),
        &|node| node.props["handler_sym"] = "sym:other".into(),
        &|node| node.props["prov"]["extractor_id"] = "t0.adapter-ts".into(),
        &|node| node.props["prov"]["basis"] = "structural".into(),
        &|node| node.props["prov"]["evidence"][0]["repo"] = "other".into(),
    ];
    check(
        &go_snapshot(),
        "internal/orders/handlers.go",
        "List",
        &[
            &|node| node.props["kind"] = "type".into(),
            // A method's name carries its receiver; a package that is not
            // the file's directory is not the adapter's.
            &|node| node.props["simple_name"] = "Other".into(),
            &|node| node.props["package_dir"] = "internal".into(),
            &|node| node.props["package_dir"] = serde_json::Value::Null,
            &|node| node.props["prov"]["evidence"][0]["path"] = "other.go".into(),
        ],
    );
    check(
        &rust_snapshot(),
        "src/routes/orders.rs",
        "list",
        &[
            &|node| node.props["kind"] = "struct".into(),
            &|node| node.props["type_name"] = "Orders".into(),
            &|node| node.props["fn_local"] = true.into(),
            &|node| node.props["module_path"] = "other::routes".into(),
            &|node| node.props["module_path"] = "".into(),
            &|node| node.props["crate_name"] = serde_json::Value::Null,
            &|node| node.props["prov"]["evidence"][0]["path"] = "src/other.rs".into(),
        ],
    );
}

// AC-0851: only whole words of the Blueprint that repeat the file name are
// dropped from the audience.
#[test]
fn a_blueprint_ending_in_the_file_name_letters_keeps_its_audience() {
    let dir = tempfile::tempdir().unwrap();
    for (name, blueprint) in [
        ("alias.py", "foalias"),
        ("domain_check.py", "admin.domain_check"),
    ] {
        std::fs::write(
            dir.path().join(name),
            format!(
                "from flask import Blueprint
bp = Blueprint('{blueprint}', __name__)
@bp.get('/{blueprint}')
def show():
    return []
"
            ),
        )
        .unwrap();
    }
    let facts = adapters_lang_python::extract_dir(
        dir.path(),
        &adapters_lang_python::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    let report = derive_controller_capabilities(&revisioned(facts.nodes, facts.edges));
    assert_eq!(
        titles(&report),
        ["View alias (foalias)", "View domain check (admin)"]
    );
}
