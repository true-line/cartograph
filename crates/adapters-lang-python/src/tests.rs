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

#[test]
fn sqlalchemy_models_recover_across_files_with_mixins_columns_and_references() {
    // #738: happy path with a cross-file base, subclassed base, repo-local
    // mixin vs unresolvable mixin, Column/mapped_column metadata, evidence
    // spans on the class and each column, and ForeignKey / relationship
    // REFERENCES edges.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/db.py",
        r#"
from sqlalchemy.orm import declarative_base, registry

Base = declarative_base()
AuditBase = registry().generate_base()
"#,
    );
    write(
        dir.path(),
        "app/mixins.py",
        r#"
from sqlalchemy import Column, DateTime
from app.db import Base

class ModelMixin:
    created_at = Column(DateTime, nullable=False)

class SubBase(Base):
    __abstract__ = True
"#,
    );
    write(
        dir.path(),
        "app/models.py",
        r#"
from sqlalchemy import Boolean, Column, ForeignKey, Integer, String
from sqlalchemy.orm import DeclarativeBase, mapped_column, relationship
from external_lib import UnresolvableMixin
from app.db import AuditBase, Base
from app.mixins import ModelMixin, SubBase

class DeclarativeRoot(DeclarativeBase):
    pass

class User(Base, ModelMixin, UnresolvableMixin):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)
    email = Column(String(255), nullable=False)
    active = Column(Boolean)

class File(SubBase):
    __tablename__ = "files"
    id = mapped_column(Integer, primary_key=True)
    user_id = Column(Integer, ForeignKey("users.id"), nullable=False)
    user = relationship("User")

class AuditLog(AuditBase):
    __tablename__ = "audit_logs"
    id = mapped_column(Integer, primary_key=True)
    actor = relationship(User)

class Tag(DeclarativeRoot):
    __tablename__ = "tags"
    id = mapped_column(Integer, primary_key=True)
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let entities: BTreeMap<&str, &Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| (node.id.as_str(), node))
        .collect();
    assert_eq!(
        entities.keys().copied().collect::<Vec<_>>(),
        vec![
            "data:local/python-service@sqlalchemy:audit_logs",
            "data:local/python-service@sqlalchemy:files",
            "data:local/python-service@sqlalchemy:tags",
            "data:local/python-service@sqlalchemy:users",
        ]
    );

    let users = entities["data:local/python-service@sqlalchemy:users"];
    assert_eq!(users.props["storage"], "sqlalchemy");
    assert_eq!(users.props["store"], "users");
    assert_eq!(users.props["name"], "User");
    assert_eq!(
        users.props["columns"],
        serde_json::json!([
            { "name": "id", "type": "Integer", "primary_key": true },
            { "name": "email", "type": "String", "nullable": false },
            { "name": "active", "type": "Boolean" },
            { "name": "created_at", "type": "DateTime", "nullable": false },
        ])
    );
    // Evidence cites the class and each recovered column (including the
    // repo-local mixin column in app/mixins.py).
    let evidence = users.props["prov"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 5);
    assert_eq!(evidence[0]["path"], "app/models.py");
    assert_eq!(evidence[4]["path"], "app/mixins.py");
    for span in evidence {
        assert!(span["byte_end"].as_u64().unwrap() > span["byte_start"].as_u64().unwrap());
    }

    let refs = edge_pairs(&extraction, "REFERENCES");
    assert_eq!(
        refs,
        vec![
            (
                "data:local/python-service@sqlalchemy:audit_logs",
                "data:local/python-service@sqlalchemy:users"
            ),
            (
                "data:local/python-service@sqlalchemy:files",
                "data:local/python-service@sqlalchemy:users"
            ),
        ]
    );
    let files_edge = extraction
        .edges
        .iter()
        .find(|edge| {
            edge.label == "REFERENCES"
                && edge.src == "data:local/python-service@sqlalchemy:files"
                && edge.dst == "data:local/python-service@sqlalchemy:users"
        })
        .unwrap();
    assert!(files_edge.props.get("field").is_none());
    assert!(files_edge.props.get("relation").is_none());
    assert!(files_edge.props.get("target").is_none());
    assert_eq!(
        files_edge.props["references"],
        serde_json::json!([
            {
                "field": "user",
                "relation": "relationship",
                "target": "User",
            },
            {
                "field": "user_id",
                "relation": "ForeignKey",
                "target": "users.id",
                "target_table": "users",
                "target_column": "id",
            },
        ])
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["framework"] == "sqlalchemy")
    );

    // Incremental reuse retargets commit SHAs across cached SQLAlchemy facts.
    let mut cache = IncrementalCache::default();
    let (_, first_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first_stats.recomputed_files, 3);
    let next_id = SourceId {
        repo: "local/python-service",
        commit: "def456",
    };
    let (reused, reused_stats) = extract_dir_incremental(dir.path(), &next_id, &mut cache).unwrap();
    assert_eq!(reused_stats.recomputed_files, 0);
    assert_eq!(reused_stats.reused_files, 3);
    let reused_users = reused
        .nodes
        .iter()
        .find(|node| node.id == "data:local/python-service@sqlalchemy:users")
        .unwrap();
    assert_eq!(
        reused_users.props["prov"]["evidence"][0]["commit_sha"],
        "def456"
    );
}

#[test]
fn flask_sqlalchemy_db_model_recovers_entities_and_relations() {
    // #738: Flask-SQLAlchemy `db.Model` where `db = SQLAlchemy(...)` and
    // SQLAlchemy is imported from `flask_sqlalchemy`.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/extensions.py",
        "from flask_sqlalchemy import SQLAlchemy\ndb = SQLAlchemy()\n",
    );
    write(
        dir.path(),
        "app/models.py",
        r#"
from app.extensions import db

class Mailbox(db.Model):
    __tablename__ = "mailboxes"
    id = db.Column(db.Integer, primary_key=True)
    email = db.Column(db.String(128), nullable=False)

class Alias(db.Model):
    __tablename__ = "aliases"
    id = db.Column(db.Integer, primary_key=True)
    mailbox_id = db.Column(db.Integer, db.ForeignKey("mailboxes.id"), nullable=False)
    mailbox = db.relationship("Mailbox")
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let mailboxes = extraction
        .nodes
        .iter()
        .find(|node| node.id == "data:local/python-service@sqlalchemy:mailboxes")
        .expect("mailboxes entity");
    assert_eq!(mailboxes.props["storage"], "sqlalchemy");
    assert_eq!(mailboxes.props["store"], "mailboxes");
    assert_eq!(
        mailboxes.props["columns"],
        serde_json::json!([
            { "name": "id", "type": "Integer", "primary_key": true },
            { "name": "email", "type": "String", "nullable": false },
        ])
    );
    assert!(edge_pairs(&extraction, "REFERENCES").contains(&(
        "data:local/python-service@sqlalchemy:aliases",
        "data:local/python-service@sqlalchemy:mailboxes"
    )));
}

#[test]
fn sqlalchemy_lookalike_base_asserts_nothing() {
    // #738: a lookalike `Base` or `db.Model` without an import proof from
    // sqlalchemy / flask_sqlalchemy asserts no DataEntity, Gap, or REFERENCES.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from other_orm import declarative_base, Column, Integer, ForeignKey

class LocalBase:
    pass

FakeBase = declarative_base()

class LookalikeOne(LocalBase):
    __tablename__ = "lookalike_one"
    id = Column(Integer, primary_key=True)

class LookalikeTwo(FakeBase):
    id = Column(Integer, ForeignKey("missing.id"))
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        extraction
            .nodes
            .iter()
            .all(|node| node.label != "DataEntity")
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["framework"] == "sqlalchemy")
    );
    assert!(edge_pairs(&extraction, "REFERENCES").is_empty());
}

#[test]
fn sqlalchemy_missing_tablename_yields_a_gap() {
    // #738: when `__tablename__` is absent on a proven model, emit a Gap
    // ("table name not declared") rather than deriving a table name.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, Integer
from sqlalchemy.ext.declarative import declarative_base

Base = declarative_base()

class Account(Base):
    id = Column(Integer, primary_key=True)
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        extraction
            .nodes
            .iter()
            .all(|node| node.label != "DataEntity")
    );
    let gap = extraction
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.id.starts_with("gap:sqlalchemy:"))
        .expect("missing __tablename__ yields a Gap");
    assert_eq!(gap.props["reason"], "table name not declared");
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
}

#[test]
fn sqlalchemy_foreign_key_to_unknown_table_yields_a_gap() {
    // #738: a ForeignKey or relationship targeting an unknown entity yields an
    // explicit Gap and a REFERENCES edge to that Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class Order(Base):
    __tablename__ = "orders"
    id = Column(Integer, primary_key=True)
    customer_id = Column(Integer, ForeignKey("customers.id"), nullable=False)
    invoice = relationship("MissingInvoice")
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        extraction
            .nodes
            .iter()
            .any(|node| node.id == "data:local/python-service@sqlalchemy:orders")
    );
    let gaps: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:sqlalchemy:"))
        .collect();
    assert_eq!(gaps.len(), 2);
    for gap in &gaps {
        assert!(
            gap.props["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
        assert!(edge_pairs(&extraction, "REFERENCES").contains(&(
            "data:local/python-service@sqlalchemy:orders",
            gap.id.as_str()
        )));
    }
}

#[test]
fn sqlalchemy_duplicate_tablename_across_files_emits_gap_and_no_entity() {
    // #738 review: when two proven non-abstract models declare the same literal
    // `__tablename__`, emit no entity for that table, emit one Gap citing both
    // claiming classes with their names in props, and treat FKs to that table
    // as ambiguous (yielding a Gap).
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/db.py",
        "from sqlalchemy.orm import declarative_base\nBase = declarative_base()\n",
    );
    write(
        dir.path(),
        "app/users_a.py",
        r#"
from sqlalchemy import Column, Integer
from app.db import Base

class PrimaryUser(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)
"#,
    );
    write(
        dir.path(),
        "app/users_b.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer
from app.db import Base

class LegacyUser(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)

class AuditEntry(Base):
    __tablename__ = "audit_entries"
    id = Column(Integer, primary_key=True)
    user_id = Column(Integer, ForeignKey("users.id"), nullable=False)
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let seen_ids: BTreeSet<&str> = extraction.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        seen_ids.len(),
        extraction.nodes.len(),
        "every emitted node id must be unique"
    );

    let entities: Vec<&str> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        entities,
        vec!["data:local/python-service@sqlalchemy:audit_entries"]
    );

    let dup_gaps: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| {
            node.label == "Gap" && node.props["reason"] == "table declared by more than one model"
        })
        .collect();
    assert_eq!(dup_gaps.len(), 1);
    let dup_gap = dup_gaps[0];
    assert_eq!(dup_gap.props["table"], "users");
    assert_eq!(
        dup_gap.props["classes"],
        serde_json::json!(["PrimaryUser", "LegacyUser"])
    );
    let evidence = dup_gap.props["prov"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[0]["path"], "app/users_a.py");
    assert_eq!(evidence[1]["path"], "app/users_b.py");

    // ForeignKey to the ambiguous "users" table becomes a Gap rather than
    // resolving to an entity.
    let fk_gap = extraction
        .nodes
        .iter()
        .find(|node| {
            node.label == "Gap"
                && node.props["reason"] == "ForeignKey target table is not a recovered entity"
        })
        .expect("FK to ambiguous table yields a Gap");
    assert!(edge_pairs(&extraction, "REFERENCES").contains(&(
        "data:local/python-service@sqlalchemy:audit_entries",
        fk_gap.id.as_str()
    )));
}

#[test]
fn sqlalchemy_single_table_inheritance_attaches_to_parent_entity() {
    // #738 review: a proven model whose bases include a concrete recovered
    // model and which has no `__tablename__` is SQLAlchemy single-table
    // inheritance: no Gap or separate entity is emitted, and its columns, FKs,
    // and relationships attach to the parent entity without duplicating names.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer, String
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class Department(Base):
    __tablename__ = "departments"
    id = Column(Integer, primary_key=True)

class Employee(Base):
    __tablename__ = "employees"
    id = Column(Integer, primary_key=True)
    name = Column(String(64), nullable=False)

class Manager(Employee):
    name = Column(String(64), nullable=False)
    bonus = Column(Integer)
    department_id = Column(Integer, ForeignKey("departments.id"))
    department = relationship("Department")
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["framework"] == "sqlalchemy")
    );
    let entities: BTreeMap<&str, &Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| (node.id.as_str(), node))
        .collect();
    assert_eq!(
        entities.keys().copied().collect::<Vec<_>>(),
        vec![
            "data:local/python-service@sqlalchemy:departments",
            "data:local/python-service@sqlalchemy:employees",
        ]
    );
    let employees = entities["data:local/python-service@sqlalchemy:employees"];
    assert_eq!(
        employees.props["columns"],
        serde_json::json!([
            { "name": "id", "type": "Integer", "primary_key": true },
            { "name": "name", "type": "String", "nullable": false },
            { "name": "bonus", "type": "Integer" },
            { "name": "department_id", "type": "Integer" },
        ])
    );
    // Evidence cites the Employee class + id + name + Manager's bonus + department_id.
    let evidence = employees.props["prov"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 5);
    assert!(edge_pairs(&extraction, "REFERENCES").contains(&(
        "data:local/python-service@sqlalchemy:employees",
        "data:local/python-service@sqlalchemy:departments"
    )));
}

#[test]
fn sqlalchemy_unprovable_target_text_is_capped_at_char_boundary() {
    // #738 review: non-literal ForeignKey / relationship targets cap stored raw
    // source text at 200 bytes on a UTF-8 char boundary and append `…`.
    let dir = tempfile::tempdir().unwrap();
    let long_expr = format!("resolve_target(\"{}é{}\")", "a".repeat(180), "b".repeat(80));
    let source = format!(
        r#"
from sqlalchemy import Column, ForeignKey, Integer
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class Item(Base):
    __tablename__ = "items"
    id = Column(Integer, primary_key=True)
    parent_id = Column(Integer, ForeignKey({long_expr}))
    rel = relationship(lambda: {long_expr})
"#
    );
    write(dir.path(), "models.py", &source);

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let gaps: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:sqlalchemy:"))
        .collect();
    assert_eq!(gaps.len(), 2);
    for gap in gaps {
        let target = gap.props["target"].as_str().unwrap();
        assert!(
            target.ends_with('…'),
            "expected truncated target to end with …: {target}"
        );
        let prefix = target.strip_suffix('…').unwrap();
        assert!(
            prefix.len() <= 200,
            "prefix byte length {} > 200",
            prefix.len()
        );
        assert!(prefix.len() >= 197);
    }
}

#[test]
fn sqlalchemy_inherited_mixin_gaps_have_distinct_ids_per_entity() {
    // #738 review: a mixin's unresolved FK or relationship inherited by multiple
    // models yields distinct Gap ids per (entity, gap) pair, emitted once each.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class TenantMixin:
    tenant_id = Column(Integer, ForeignKey("missing_tenants.id"), nullable=False)
    tenant = relationship("MissingTenant")

class Invoice(Base, TenantMixin):
    __tablename__ = "invoices"
    id = Column(Integer, primary_key=True)

class Payment(Base, TenantMixin):
    __tablename__ = "payments"
    id = Column(Integer, primary_key=True)
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let gaps: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:sqlalchemy:"))
        .collect();
    assert_eq!(gaps.len(), 4);
    let unique_gap_ids: BTreeSet<&str> = gaps.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(unique_gap_ids.len(), 4);
    assert!(
        gaps.iter()
            .filter(|g| g
                .id
                .contains("data:local/python-service@sqlalchemy:invoices"))
            .count()
            == 2
    );
    assert!(
        gaps.iter()
            .filter(|g| g
                .id
                .contains("data:local/python-service@sqlalchemy:payments"))
            .count()
            == 2
    );
}

#[test]
fn sqlalchemy_multiple_fks_and_relationship_to_same_target_emit_single_edge() {
    // #738 review: multiple ForeignKey and relationship declarations between the
    // same (src, dst) pair collapse into a single REFERENCES edge with a sorted
    // `references` array and matching evidence order.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer
from sqlalchemy.orm import declarative_base, relationship

Base = declarative_base()

class User(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)

class Document(Base):
    __tablename__ = "documents"
    id = Column(Integer, primary_key=True)
    updated_by_id = Column(Integer, ForeignKey("users.id"), nullable=False)
    author = relationship("User")
    created_by_id = Column(Integer, ForeignKey("users.id"), nullable=False)
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let ref_edges: Vec<&Edge> = extraction
        .edges
        .iter()
        .filter(|edge| edge.label == "REFERENCES")
        .collect();
    assert_eq!(ref_edges.len(), 1);
    let edge = ref_edges[0];
    assert_eq!(edge.src, "data:local/python-service@sqlalchemy:documents");
    assert_eq!(edge.dst, "data:local/python-service@sqlalchemy:users");
    assert_eq!(edge.props["framework"], "sqlalchemy");
    assert!(edge.props.get("field").is_none());
    assert!(edge.props.get("relation").is_none());
    assert!(edge.props.get("target").is_none());
    assert_eq!(
        edge.props["references"],
        serde_json::json!([
            {
                "field": "author",
                "relation": "relationship",
                "target": "User",
            },
            {
                "field": "created_by_id",
                "relation": "ForeignKey",
                "target": "users.id",
                "target_table": "users",
                "target_column": "id",
            },
            {
                "field": "updated_by_id",
                "relation": "ForeignKey",
                "target": "users.id",
                "target_table": "users",
                "target_column": "id",
            },
        ])
    );
    let evidence = edge.props["prov"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 3);
    // `author` is declared before `created_by_id`, which is declared after `updated_by_id`.
    let author_start = evidence[0]["byte_start"].as_u64().unwrap();
    let created_start = evidence[1]["byte_start"].as_u64().unwrap();
    let updated_start = evidence[2]["byte_start"].as_u64().unwrap();
    assert!(updated_start < author_start && author_start < created_start);
    let expected_prov = prov_from(
        serde_json::from_value(edge.props["prov"]["evidence"].clone()).unwrap(),
        ConfidenceTier::Confirmed,
        "REFERENCES data:local/python-service@sqlalchemy:documents -> data:local/python-service@sqlalchemy:users",
    );
    assert_eq!(edge.props["prov"], expected_prov);
}

#[test]
fn sqlalchemy_push_reference_gap_emits_node_and_edge_once_per_entity_and_evidence() {
    // #738 review: calling `push_reference_gap` twice for the same
    // (src_entity_id, evidence) emits only one Gap node and one REFERENCES edge.
    let mut extraction = Extraction::default();
    let mut emitted_gaps = BTreeSet::new();
    let evidence = EvidenceRef {
        repo: "local/python-service".into(),
        commit_sha: "abc123".into(),
        path: "models.py".into(),
        byte_start: 42,
        byte_end: 64,
    };
    for _ in 0..2 {
        sqlalchemy::push_reference_gap(
            &mut extraction,
            &mut emitted_gaps,
            "local/python-service",
            "data:local/python-service@sqlalchemy:orders",
            "customer_id",
            "customers.id",
            "ForeignKey target table is not a recovered entity",
            evidence.clone(),
        );
    }
    assert_eq!(extraction.nodes.len(), 1);
    assert_eq!(extraction.edges.len(), 1);
    assert_eq!(extraction.edges[0].dst, extraction.nodes[0].id);
}

#[test]
fn sqlalchemy_sti_child_of_ambiguous_parent_emits_parent_table_ambiguous_gap() {
    // #738 review: a tablename-less model whose nearest concrete ancestor has a
    // literal `__tablename__` claimed by more than one model emits a Gap with
    // reason "parent table declared by more than one model" and the parent
    // class name in props.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, Integer, String
from sqlalchemy.orm import declarative_base

Base = declarative_base()

class PrimaryUser(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)

class LegacyUser(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)

class AdminUser(PrimaryUser):
    role = Column(String(32))
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        extraction
            .nodes
            .iter()
            .all(|node| node.label != "DataEntity")
    );
    let child_gap = extraction
        .nodes
        .iter()
        .find(|node| {
            node.label == "Gap"
                && node.props["reason"] == "parent table declared by more than one model"
        })
        .expect("STI child of ambiguous parent yields dedicated Gap reason");
    assert_eq!(child_gap.props["class"], "AdminUser");
    assert_eq!(child_gap.props["parent"], "PrimaryUser");
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["reason"] == "table name not declared")
    );
}

#[test]
fn sqlalchemy_orm_member_calls_require_import_or_flask_db_proof() {
    // #738/#746 review: `Column`, `mapped_column`, `relationship`, and
    // `ForeignKey` count only when resolved to `sqlalchemy`, `sqlalchemy.orm`,
    // `sqlalchemy.schema` (bare imports with `as` aliases or module aliases),
    // or a proven Flask-SQLAlchemy `db` instance (including across files).
    // `custom.relationship("User")`, `custom.ForeignKey(...)`, and a locally
    // defined `def Column(...)` are ignored with no column, edge, or gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/extensions.py",
        "from flask_sqlalchemy import SQLAlchemy\ndb = SQLAlchemy()\n",
    );
    write(
        dir.path(),
        "app/models.py",
        r#"
import sqlalchemy as sa
from sqlalchemy import orm
from sqlalchemy.schema import ForeignKey as SchemaFK
from app.extensions import db
import custom_dsl as custom

Base = orm.declarative_base()

def Column(*args, **kwargs):
    return None

class User(Base):
    __tablename__ = "users"
    id = sa.Column(sa.Integer, primary_key=True)
    ignored_local = Column(sa.String(64))

class Post(db.Model):
    __tablename__ = "posts"
    id = db.Column(db.Integer, primary_key=True)
    author_id = sa.Column(sa.Integer, SchemaFK("users.id"), nullable=False)
    author = orm.relationship("User")
    bogus_fk = sa.Column(sa.Integer, custom.ForeignKey("missing.id"))
    bogus_rel = custom.relationship("User")
    bogus_missing_rel = custom.relationship("NoSuchEntity")
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let entities: BTreeMap<&str, &Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| (node.id.as_str(), node))
        .collect();
    let users = entities["data:local/python-service@sqlalchemy:users"];
    assert_eq!(
        users.props["columns"],
        serde_json::json!([
            { "name": "id", "type": "Integer", "primary_key": true },
        ])
    );

    let posts = entities["data:local/python-service@sqlalchemy:posts"];
    assert_eq!(
        posts.props["columns"],
        serde_json::json!([
            { "name": "id", "type": "Integer", "primary_key": true },
            { "name": "author_id", "type": "Integer", "nullable": false },
            { "name": "bogus_fk", "type": "Integer" },
        ])
    );

    let ref_edges: Vec<&Edge> = extraction
        .edges
        .iter()
        .filter(|edge| edge.label == "REFERENCES")
        .collect();
    assert_eq!(ref_edges.len(), 1);
    assert_eq!(
        ref_edges[0].src,
        "data:local/python-service@sqlalchemy:posts"
    );
    assert_eq!(
        ref_edges[0].dst,
        "data:local/python-service@sqlalchemy:users"
    );
    assert_eq!(
        ref_edges[0].props["references"],
        serde_json::json!([
            {
                "field": "author",
                "relation": "relationship",
                "target": "User",
            },
            {
                "field": "author_id",
                "relation": "ForeignKey",
                "target": "users.id",
                "target_table": "users",
                "target_column": "id",
            },
        ])
    );
    assert!(
        !extraction
            .nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["framework"] == "sqlalchemy")
    );
}

#[test]
fn sqlalchemy_mapped_annotation_infers_relationship_targets_and_gaps_on_union() {
    // #738/#746 review: SQLAlchemy 2 `Mapped[...]` annotations supply the
    // relationship target when `relationship(...)` has no positional or
    // `argument=` target, unwrapping one level of `list`/`List`/`set`/`Set`/
    // `Optional`/`typing.*` for string literals and bare/dotted symbols, while
    // irreducible annotations such as `Mapped[Union[A, B]]` stay a Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
import typing
from typing import List, Optional, Set, Union
from sqlalchemy import Integer
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship

class Base(DeclarativeBase):
    pass

class Child(Base):
    __tablename__ = "children"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    parent: Mapped["Parent"] = relationship()
    opt_parent: Mapped[Optional["Parent"]] = relationship()

class Tag(Base):
    __tablename__ = "tags"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)

class Parent(Base):
    __tablename__ = "parents"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    children: Mapped[list["Child"]] = relationship(back_populates="parent")
    typed_children: Mapped[List[Child]] = relationship()
    tags: Mapped[Set[Tag]] = relationship()
    extra_tags: Mapped[typing.Set[Tag]] = relationship()
    ambiguous_rel: Mapped[Union[Child, Tag]] = relationship()
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let refs = edge_pairs(&extraction, "REFERENCES");
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:children",
        "data:local/python-service@sqlalchemy:parents"
    )));
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:parents",
        "data:local/python-service@sqlalchemy:children"
    )));
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:parents",
        "data:local/python-service@sqlalchemy:tags"
    )));

    let parent_to_child = extraction
        .edges
        .iter()
        .find(|edge| {
            edge.label == "REFERENCES"
                && edge.src == "data:local/python-service@sqlalchemy:parents"
                && edge.dst == "data:local/python-service@sqlalchemy:children"
        })
        .expect("parents -> children REFERENCES edge");
    assert_eq!(
        parent_to_child.props["references"],
        serde_json::json!([
            {
                "field": "children",
                "relation": "relationship",
                "target": "Child",
            },
            {
                "field": "typed_children",
                "relation": "relationship",
                "target": "Child",
            },
        ])
    );

    let gaps: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "sqlalchemy")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].props["field"], "ambiguous_rel");
    assert_eq!(
        gaps[0].props["reason"],
        "relationship target is not provable"
    );
    assert_eq!(gaps[0].props["target"], "Mapped[Union[Child, Tag]]");
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:parents",
        gaps[0].id.as_str()
    )));
}

#[test]
fn sqlalchemy_schema_qualified_entities_and_foreign_keys_resolve_by_schema() {
    // #738/#746 review: entities record `schema` from `__table_args__` dict or
    // tuple trailing dict, include `{schema}.{table}` in entity ids, key
    // duplicate detection and `by_table` by `(Option<schema>, table)`, match
    // 3-part FKs against `Some(schema)` and 2-part FKs against `None`, and emit
    // Gaps for unknown schemas or >3-part FK targets.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "models.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer, UniqueConstraint
from sqlalchemy.orm import declarative_base

Base = declarative_base()

class PublicUser(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)

class AuditUser(Base):
    __tablename__ = "users"
    __table_args__ = {"schema": "audit"}
    id = Column(Integer, primary_key=True)

class BillingAccount(Base):
    __tablename__ = "accounts"
    __table_args__ = (UniqueConstraint("id"), {"schema": "billing"})
    id = Column(Integer, primary_key=True)

class Event(Base):
    __tablename__ = "events"
    id = Column(Integer, primary_key=True)
    public_user_id = Column(Integer, ForeignKey("users.id"), nullable=False)
    audit_user_id = Column(Integer, ForeignKey("audit.users.id"), nullable=False)
    billing_account_id = Column(Integer, ForeignKey("billing.accounts.id"), nullable=False)
    missing_schema_user_id = Column(Integer, ForeignKey("other.users.id"))
    malformed_fk_id = Column(Integer, ForeignKey("cat.audit.users.id"))
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let entities: BTreeMap<&str, &Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| (node.id.as_str(), node))
        .collect();
    assert_eq!(
        entities.keys().copied().collect::<Vec<_>>(),
        vec![
            "data:local/python-service@sqlalchemy:audit.users",
            "data:local/python-service@sqlalchemy:billing.accounts",
            "data:local/python-service@sqlalchemy:events",
            "data:local/python-service@sqlalchemy:users",
        ]
    );
    assert!(
        entities["data:local/python-service@sqlalchemy:users"]
            .props
            .get("schema")
            .is_none()
    );
    assert_eq!(
        entities["data:local/python-service@sqlalchemy:audit.users"].props["schema"],
        "audit"
    );
    assert_eq!(
        entities["data:local/python-service@sqlalchemy:billing.accounts"].props["schema"],
        "billing"
    );

    let refs = edge_pairs(&extraction, "REFERENCES");
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:events",
        "data:local/python-service@sqlalchemy:users"
    )));
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:events",
        "data:local/python-service@sqlalchemy:audit.users"
    )));
    assert!(refs.contains(&(
        "data:local/python-service@sqlalchemy:events",
        "data:local/python-service@sqlalchemy:billing.accounts"
    )));

    let gaps: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "sqlalchemy")
        .collect();
    assert_eq!(gaps.len(), 2);
    let missing_schema_gap = gaps
        .iter()
        .find(|g| g.props["field"] == "missing_schema_user_id")
        .expect("missing schema gap");
    assert_eq!(
        missing_schema_gap.props["reason"],
        "ForeignKey target table is not a recovered entity"
    );
    assert_eq!(missing_schema_gap.props["target"], "other.users.id");

    let malformed_gap = gaps
        .iter()
        .find(|g| g.props["field"] == "malformed_fk_id")
        .expect("4-part FK gap");
    assert_eq!(
        malformed_gap.props["reason"],
        "ForeignKey target is not a table.column reference"
    );
    assert_eq!(malformed_gap.props["target"], "cat.audit.users.id");
}
