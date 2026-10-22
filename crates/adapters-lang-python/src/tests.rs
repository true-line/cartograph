use super::*;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/python-service",
        commit: "abc123",
    }
}

fn edge_pairs<'a>(extraction: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    extraction
        .edges
        .iter()
        .filter(|edge| edge.label == label)
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect()
}

#[test]
fn fastapi_and_flask_routes_are_import_proven() {
    // AC-0053/T-0053: the receiver must be created by an import-proven
    // framework factory; matching decorator spellings on arbitrary objects
    // are not endpoints.
    let source = br#"
from fastapi import FastAPI
from flask import Flask

api = FastAPI()
web = Flask(__name__)
fake = object()

@api.get("/orders")
def list_orders():
    return []

@web.route("/health", methods=["GET", "POST"])
def health():
    return "ok"

@fake.get("/not-an-endpoint")
def lookalike():
    return None

prefix = "/v1"
@api.get(f"{prefix}/computed")
def computed_route():
    return None
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let mut endpoints = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            (
                node.props["method"].as_str().unwrap().to_string(),
                node.props["path"].as_str().unwrap().to_string(),
                node.props["framework"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort();
    assert_eq!(
        endpoints,
        vec![
            ("GET".into(), "/health".into(), "flask".into()),
            ("GET".into(), "/orders".into(), "fastapi".into()),
            ("POST".into(), "/health".into(), "flask".into()),
        ]
    );
    assert!(
        !endpoints
            .iter()
            .any(|(_, path, _)| path == "/not-an-endpoint")
    );
    assert!(
        !endpoints
            .iter()
            .any(|(_, path, _)| path.ends_with("/computed"))
    );
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/python-service@GET:/orders",
        "sym:local/python-service@app.py#list_orders"
    )));
}

#[test]
fn calls_resolve_locally_and_across_files() {
    // AC-0053/T-0053: local calls resolve in the file pass; explicitly local
    // imports resolve only after the directory pass proves the target symbol.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    std::fs::write(dir.path().join("pkg/__init__.py"), "").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper.py"),
        "def imported_helper():\n    return 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("pkg/app.py"),
        r#"
from .helper import imported_helper

def local_helper():
    return 2

def handler():
    local_helper()
    imported_helper()
"#,
    )
    .unwrap();

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let handler = "sym:local/python-service@pkg/app.py#handler";
    assert!(
        edge_pairs(&extraction, "CALLS")
            .contains(&(handler, "sym:local/python-service@pkg/app.py#local_helper"))
    );
    assert!(edge_pairs(&extraction, "CALLS").contains(&(
        handler,
        "sym:local/python-service@pkg/helper.py#imported_helper"
    )));
    assert!(extraction.nodes.iter().all(|node| node.label != "Gap"));
}

#[test]
fn every_fact_has_confirmed_provenance_and_spans() {
    // AC-0053/T-0053: exact Python source spans are first-class T0 evidence.
    let source =
        b"from fastapi import FastAPI\napp = FastAPI()\n\n@app.get('/x')\ndef x():\n    return x\n";
    let extraction = extract_source(source, "api.py", &id()).unwrap();
    for props in extraction
        .nodes
        .iter()
        .filter(|node| node.props.get("placeholder").is_none())
        .map(|node| &node.props)
        .chain(extraction.edges.iter().map(|edge| &edge.props))
    {
        assert_eq!(props["prov"]["tier"], "Deterministic");
        assert_eq!(props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(props["prov"]["extractor_id"], EXTRACTOR_ID);
        assert_eq!(props["prov"]["evidence"][0]["path"], "api.py");
        let start = props["prov"]["evidence"][0]["byte_start"].as_u64().unwrap();
        let end = props["prov"]["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end > start);
        assert!(end <= source.len() as u64);
    }
}

#[test]
fn incremental_reingest_is_deterministic() {
    // AC-0053/T-0053: unchanged Python contexts are reused and a byte change
    // recomputes only that file while directory joins rerun over the full set.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.py"), "def a():\n    return 1\n").unwrap();
    std::fs::write(dir.path().join("b.py"), "def b():\n    return 2\n").unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/vendor")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/vendor/third_party.py"),
        "def vendored():\n    return 0\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (first, first_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first_stats.recomputed_files, 2);
    let (same, same_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(same_stats.recomputed_files, 0);
    assert_eq!(same_stats.reused_files, 2);
    assert_eq!(first.nodes, same.nodes);
    assert_eq!(first.edges, same.edges);

    std::fs::write(dir.path().join("b.py"), "def changed():\n    return 3\n").unwrap();
    let (changed, changed_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(changed_stats.recomputed_files, 1);
    assert_eq!(changed_stats.reused_files, 1);
    assert!(
        changed
            .nodes
            .iter()
            .any(|node| node.id.ends_with("#changed"))
    );
    assert!(!changed.nodes.iter().any(|node| node.id.ends_with("#b")));

    std::fs::remove_file(dir.path().join("b.py")).unwrap();
    let (_, deleted_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(deleted_stats.deleted_files, 1);
}

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("app/main.py", ""),
        ("gen/api.py", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_python_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["app/main.py"]);
}

#[test]
fn import_targets_carry_provenance_and_only_proven_externals_confirm() {
    // AC-0207 (#237): an absolute import is external only when no directory
    // or module anywhere in the repository shares its top-level name.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/orders")).unwrap();
    std::fs::write(dir.path().join("src/orders/__init__.py"), "").unwrap();
    std::fs::write(
        dir.path().join("app.py"),
        "import requests\nimport orders.models\nfrom . import sibling\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let boundary = |id: &str| {
        let node = out
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("missing node {id}"));
        let prov: Provenance = serde_json::from_value(node.props["prov"].clone())
            .unwrap_or_else(|_| panic!("{id} carries no provenance"));
        assert!(!prov.evidence.is_empty(), "{id} cites its import");
        (
            node.props["boundary"].as_str().unwrap().to_string(),
            prov.confidence_tier,
        )
    };
    assert_eq!(boundary("mod:requests"), ("external".into(), Confirmed));
    assert_eq!(boundary("mod:orders.models"), ("unresolved".into(), Gap));
}

fn endpoint_facts(extraction: &Extraction) -> Vec<(String, String, String, Option<String>)> {
    let mut facts = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            (
                node.props["method"].as_str().unwrap().to_string(),
                node.props["path"].as_str().unwrap().to_string(),
                node.props["prov"]["confidence_tier"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                node.props["mount"].as_str().map(str::to_string),
            )
        })
        .collect::<Vec<_>>();
    facts.sort();
    facts
}

fn write(dir: &std::path::Path, path: &str, source: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
}

#[test]
fn flask_blueprint_routes_join_their_prefix_and_mounts_across_files() {
    // #721: a Blueprint defined in one module, routed in others and mounted
    // by the app factory, the way SimpleLogin and most Flask apps are laid out.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/api/base.py",
        "from flask import Blueprint\napi_bp = Blueprint(name=\"api\", import_name=__name__, url_prefix=\"/api\")\n",
    );
    write(
        dir.path(),
        "app/api/views/alias.py",
        r#"
from app.api.base import api_bp

@api_bp.route("/aliases/<int:alias_id>", methods=["GET", "DELETE"])
def alias(alias_id):
    return ""

@api_bp.route("/aliases/<int:alias_id>/toggle", methods=["POST"])
def toggle(alias_id):
    return ""
"#,
    );
    write(
        dir.path(),
        "app/oauth/base.py",
        "from flask import Blueprint\noauth_bp = Blueprint(name=\"oauth\", import_name=__name__, url_prefix=\"/oauth\")\n",
    );
    write(
        dir.path(),
        "app/oauth/views.py",
        "from app.oauth.base import oauth_bp\n\n@oauth_bp.route(\"/token\", methods=[\"POST\"])\ndef token():\n    return \"\"\n",
    );
    write(
        dir.path(),
        "server.py",
        r#"
from flask import Flask
from app.api.base import api_bp
from app.oauth.base import oauth_bp

def create_app():
    app = Flask(__name__)
    app.register_blueprint(api_bp)
    app.register_blueprint(oauth_bp, url_prefix="/oauth")
    app.register_blueprint(oauth_bp, url_prefix="/oauth2")
    return app
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let mounted = Some("mounted".to_string());
    let confirmed = "Confirmed".to_string();
    assert_eq!(
        endpoint_facts(&extraction),
        vec![
            (
                "DELETE".into(),
                "/api/aliases/<int:alias_id>".into(),
                confirmed.clone(),
                mounted.clone()
            ),
            (
                "GET".into(),
                "/api/aliases/<int:alias_id>".into(),
                confirmed.clone(),
                mounted.clone()
            ),
            (
                "POST".into(),
                "/api/aliases/<int:alias_id>/toggle".into(),
                confirmed.clone(),
                mounted.clone()
            ),
            (
                "POST".into(),
                "/oauth/token".into(),
                confirmed.clone(),
                mounted.clone()
            ),
            ("POST".into(), "/oauth2/token".into(), confirmed, mounted),
        ]
    );
    // Evidence cites the decorator, the Blueprint definition and the mount.
    let endpoint = extraction
        .nodes
        .iter()
        .find(|node| node.id == "ep:local/python-service@POST:/oauth2/token")
        .unwrap();
    let paths = endpoint.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|evidence| evidence["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec!["app/oauth/views.py", "app/oauth/base.py", "server.py"]
    );
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/python-service@POST:/oauth2/token",
        "sym:local/python-service@app/oauth/views.py#token"
    )));
}

#[test]
fn fastapi_router_prefixes_compose_and_unmounted_or_unreadable_prefixes_stay_honest() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "routers/items.py",
        "from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/items\")\n\n@router.get(\"/{item_id}\")\ndef read(item_id):\n    return {}\n",
    );
    write(
        dir.path(),
        "routers/orphan.py",
        "from fastapi import APIRouter\norphan = APIRouter(prefix=\"/orphan\")\n\n@orphan.post(\"/run\")\ndef run():\n    return {}\n",
    );
    write(
        dir.path(),
        "routers/dynamic.py",
        "from fastapi import APIRouter\nimport settings\ndynamic = APIRouter(prefix=settings.PREFIX)\n\n@dynamic.get(\"/x\")\ndef x():\n    return {}\n",
    );
    write(
        dir.path(),
        "main.py",
        "from fastapi import FastAPI\nfrom routers.items import router\nfrom routers.dynamic import dynamic\napp = FastAPI()\napp.include_router(router, prefix=\"/v1\")\napp.include_router(dynamic)\n",
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        endpoint_facts(&extraction),
        vec![
            (
                "GET".into(),
                "/v1/items/{item_id}".into(),
                "Confirmed".into(),
                Some("mounted".into())
            ),
            (
                "POST".into(),
                "/orphan/run".into(),
                "InferredStrong".into(),
                Some("mount_not_observed".into())
            ),
        ]
    );
    let gap = extraction
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.id.starts_with("gap:route:"))
        .expect("an unreadable prefix becomes a Gap");
    assert_eq!(gap.props["route"], "/x");
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        gap.id.as_str(),
        "sym:local/python-service@routers/dynamic.py#x"
    )));
}

#[test]
fn routes_on_an_imported_app_instance_resolve_and_lookalikes_do_not() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "web/__init__.py", "");
    write(
        dir.path(),
        "web/app.py",
        "from flask import Flask\napp = Flask(__name__)\n",
    );
    write(
        dir.path(),
        "web/views.py",
        "from web.app import app\nfrom web.helpers import thing\n\n@app.route(\"/health\")\ndef health():\n    return \"\"\n\n@thing.route(\"/nope\")\ndef nope():\n    return \"\"\n",
    );
    write(dir.path(), "web/helpers.py", "thing = object()\n");
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        endpoint_facts(&extraction),
        vec![("GET".into(), "/health".into(), "Confirmed".into(), None)]
    );
}

#[test]
fn mount_receivers_must_be_proven_before_a_route_is_confirmed() {
    // #741 review: a mount only confirms a path when its receiver is a proven
    // application of the same framework. An annotated factory parameter is
    // declared, not proven; a lookalike receiver never moves a route.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "routers.py",
        r#"
from fastapi import APIRouter
users = APIRouter(prefix="/users")
items = APIRouter(prefix="/items")
orphans = APIRouter(prefix="/orphans")

@users.get("/me")
def me():
    return {}

@items.get("/all")
def all_items():
    return {}

@orphans.get("/x")
def orphan():
    return {}
"#,
    );
    write(
        dir.path(),
        "main.py",
        r#"
from fastapi import FastAPI
from routers import users, items, orphans
from registry import registry

def wire(app: FastAPI):
    app.include_router(users, prefix="/v1")

def wire_untyped(app):
    app.include_router(items, prefix="/v2")

real = FastAPI()
real.include_router(items, prefix="/v3")
registry.include_router(items, prefix="/fake")
registry.include_router(orphans, prefix="/fake")
"#,
    );
    write(
        dir.path(),
        "registry.py",
        "class Registry:\n    def include_router(self, router, prefix=''):\n        pass\nregistry = Registry()\n",
    );
    let facts = endpoint_facts(&extract_dir(dir.path(), &id()).unwrap());
    let strong = "InferredStrong".to_string();
    assert_eq!(
        facts,
        vec![
            // A lookalike receiver alone never confirms; it stays inferred.
            (
                "GET".into(),
                "/fake/orphans/x".into(),
                strong.clone(),
                Some("mount_receiver_unproven".into())
            ),
            (
                "GET".into(),
                "/v1/users/me".into(),
                strong,
                Some("mount_receiver_annotated".into())
            ),
            // The proven app confirms /v3; the untyped-parameter and lookalike
            // mounts of the same router are dropped once a chain confirms it.
            (
                "GET".into(),
                "/v3/items/all".into(),
                "Confirmed".into(),
                Some("mounted".into())
            ),
        ]
    );
}

#[test]
fn nested_router_mounts_compose_their_whole_chain() {
    // #741 review: child -> parent -> app must serve under every prefix on the
    // chain, for FastAPI routers and Flask nested blueprints alike.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "api.py",
        r#"
from fastapi import FastAPI, APIRouter
child = APIRouter(prefix="/child")
parent = APIRouter(prefix="/parent")

@child.get("/x")
def x():
    return {}

parent.include_router(child, prefix="/nested")
app = FastAPI()
app.include_router(parent, prefix="/v1")
"#,
    );
    write(
        dir.path(),
        "web.py",
        r#"
from flask import Flask, Blueprint
leaf = Blueprint("leaf", __name__, url_prefix="/leaf")
outer = Blueprint("outer", __name__, url_prefix="/outer")

@leaf.route("/page")
def page():
    return ""

outer.register_blueprint(leaf)
site = Flask(__name__)
site.register_blueprint(outer)
"#,
    );
    let facts = endpoint_facts(&extract_dir(dir.path(), &id()).unwrap());
    let confirmed = "Confirmed".to_string();
    let mounted = Some("mounted".to_string());
    assert_eq!(
        facts,
        vec![
            (
                "GET".into(),
                "/outer/leaf/page".into(),
                confirmed.clone(),
                mounted.clone()
            ),
            (
                "GET".into(),
                "/v1/parent/nested/child/x".into(),
                confirmed,
                mounted
            ),
        ]
    );
}

#[test]
fn expanded_or_cyclic_prefixes_become_gaps_not_paths() {
    // #741 review: `**options` may carry the prefix, a Blueprint's sixth
    // positional argument is its url_prefix, and a mount cycle has no path.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "api.py",
        r#"
from fastapi import FastAPI, APIRouter
from flask import Blueprint
opts = {"prefix": "/secret"}
expanded = APIRouter(**opts)
mounted_expanded = APIRouter()
loop_a = APIRouter()
loop_b = APIRouter()
positional = Blueprint("p", __name__, None, None, None, "/pos")

@expanded.get("/a")
def a():
    return {}

@mounted_expanded.get("/b")
def b():
    return {}

@loop_a.get("/c")
def c():
    return {}

@positional.route("/d")
def d():
    return ""

app = FastAPI()
app.include_router(expanded)
app.include_router(mounted_expanded, **opts)
loop_a.include_router(loop_b)
loop_b.include_router(loop_a)
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let facts = endpoint_facts(&extraction);
    // Only the positional Blueprint prefix is readable (its mount is not
    // observed, so it stays inferred).
    assert_eq!(
        facts,
        vec![(
            "GET".into(),
            "/pos/d".into(),
            "InferredStrong".into(),
            Some("mount_not_observed".into())
        )]
    );
    let reasons: Vec<String> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:route:"))
        .map(|node| {
            node.props["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(reasons.len(), 3, "{reasons:?}");
    assert!(
        reasons.iter().any(|reason| reason.contains("cyclic")),
        "{reasons:?}"
    );
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| reason.as_str() == "router prefix is not a string literal")
            .count(),
        2
    );
}
