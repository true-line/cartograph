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
@old.get('/all')
def everyone():
    return []
";

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
    // Two Blueprints named `users` in two files stay two candidates.
    assert_eq!(report.facts.len(), 2);
    assert!(
        report.facts.iter().all(|fact| {
            capability(fact).props["title"] == "HTTP operations in blueprint users"
        })
    );
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
    let all = format!("ep:{REPO}@GET:/legacy/all");
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
    let endpoint_edits: [&dyn Fn(&mut Node); 9] = [
        // No Blueprint name, an empty one, or no endpoint name at all.
        &|node| node.props["endpoint_name"] = "list_users".into(),
        &|node| node.props["endpoint_name"] = ".list_users".into(),
        &|node| node.props["endpoint_name"] = serde_json::Value::Null,
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
