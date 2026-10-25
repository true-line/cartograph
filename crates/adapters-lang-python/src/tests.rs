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

#[test]
fn absolute_imports_resolve_to_files_from_the_repository_root() {
    // AC-0207 (#721): an absolute import names the recovered module or
    // package file from the root; an ambiguous or absent target stays `mod:`.
    let dir = tempfile::tempdir().unwrap();
    for path in [
        "app/__init__.py",
        "app/db.py",
        "tasks/clean.py",
        "dup.py",
        "dup/__init__.py",
        "src/orders/__init__.py",
        "plain.py",
        "plain/sub.py",
        "deep/__init__.py",
        "deep/mid.py",
        "deep/mid/leaf.py",
        "pkg.py",
        "pkg/__init__.py",
        "pkg/mod.py",
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "").unwrap();
    }
    std::fs::write(
        dir.path().join("main.py"),
        "import app\nfrom app.db import Session\nimport tasks.clean\nimport dup\nimport orders.models\nimport requests\nimport plain.sub\nimport deep.mid.leaf\nimport pkg.mod\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS" && edge.src.ends_with("@main.py"))
        .map(|edge| {
            (
                edge.props["specifier"].as_str().unwrap().to_string(),
                edge.dst.clone(),
                edge.props
                    .get("resolution")
                    .and_then(|r| r.as_str())
                    .map(str::to_string),
            )
        })
        .collect::<BTreeSet<_>>();
    let file = |path: &str| format!("file:{}@{path}", id().repo);
    let proven = Some("directory-proven".to_string());
    assert_eq!(
        imports,
        BTreeSet::from([
            ("app".into(), file("app/__init__.py"), proven.clone()),
            ("app.db".into(), file("app/db.py"), proven.clone()),
            ("tasks.clean".into(), file("tasks/clean.py"), proven),
            ("dup".into(), "mod:dup".into(), None),
            ("orders.models".into(), "mod:orders.models".into(), None),
            ("requests".into(), "mod:requests".into(), None),
            // A plain module shadows the directory: `plain.sub` cannot import.
            ("plain.sub".into(), "mod:plain.sub".into(), None),
            ("deep.mid.leaf".into(), "mod:deep.mid.leaf".into(), None),
            // A package beside a same-named module still proves its children.
            (
                "pkg.mod".into(),
                file("pkg/mod.py"),
                Some("directory-proven".into())
            ),
        ])
    );
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
fn sqlalchemy_attribute_foreign_keys_resolve_to_declared_columns() {
    // AC-0337: `ForeignKey(Model.column)` resolves only when the class is a
    // recovered model (same file, or import-proven) and the attribute is one of
    // its declared columns (mixins included) under its own name.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/models.py",
        &r#"
from sqlalchemy import Column, ForeignKey, Integer, String
from sqlalchemy.orm import declarative_base

Base = declarative_base()

class ModelMixin:
    id = Column(Integer, primary_key=True)

class User(Base, ModelMixin):
    __tablename__ = "users"
    code = Column("user_code", String)
    label = Column(String, name="user_label")

class Employee(Base):
    __tablename__ = "employee"
    id = Column(Integer, primary_key=True)

class Manager(Employee):
    bonus = Column(Integer)

class Engineer(Employee):
    level = Column(Integer)

class Alias(Base, ModelMixin):
    __tablename__ = "alias"
    user_id = Column(Integer, ForeignKey(User.id, ondelete="cascade"))
    missing_id = Column(Integer, ForeignKey(User.missing))
    code_ref = Column(String, ForeignKey(User.code))
    label_ref = Column(String, ForeignKey(User.label))
    ghost_id = Column(Integer, ForeignKey(Ghost.id))
    call_id = Column(Integer, ForeignKey(lookup().id))
    bonus_ref = Column(Integer, ForeignKey(Manager.bonus))
    engineer_id = Column(Integer, ForeignKey(Engineer.id))
    parent_bonus = Column(Integer, ForeignKey(Employee.bonus))
    sibling_bonus = Column(Integer, ForeignKey(Engineer.bonus))
    long_ref = Column(Integer, ForeignKey(LONG.id))
"#
        .replace("LONG", &"V".repeat(300)),
    );
    // Evaluated where it is written: this mixin never imports `app`.
    write(
        dir.path(),
        "app/mixins.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer

class OwnedMixin:
    owner_id = Column(Integer, ForeignKey(app.models.User.id))
"#,
    );
    write(
        dir.path(),
        "app/audit.py",
        r#"
from sqlalchemy import Column, ForeignKey, Integer
from app.models import Base, User
from app.mixins import OwnedMixin
import app.models

class Audit(Base, OwnedMixin):
    __tablename__ = "audit"
    id = Column(Integer, primary_key=True)
    user_id = Column(Integer, ForeignKey(User.id))
    alias_id = Column(Integer, ForeignKey(app.models.Alias.id))
"#,
    );

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let references = |src: &str, dst: &str| {
        extraction
            .edges
            .iter()
            .find(|e| {
                e.label == "REFERENCES"
                    && e.src == format!("data:local/python-service@sqlalchemy:{src}")
                    && e.dst == format!("data:local/python-service@sqlalchemy:{dst}")
            })
            .map(|e| e.props["references"].clone())
    };
    assert_eq!(
        references("alias", "users"),
        Some(serde_json::json!([{
            "field": "user_id",
            "relation": "ForeignKey",
            "target": "User.id",
            "target_table": "users",
            "target_column": "id",
        }]))
    );
    assert_eq!(
        references("audit", "users").unwrap()[0]["target_table"],
        "users"
    );
    assert_eq!(
        references("audit", "alias").unwrap()[0]["target"],
        "app.models.Alias.id"
    );
    let mut gaps: Vec<(String, String)> = extraction
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.id.starts_with("gap:sqlalchemy:"))
        .filter(|n| !n.props["target"].as_str().unwrap().ends_with('…'))
        .map(|n| {
            (
                n.props["target"].as_str().unwrap().to_string(),
                n.props["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    gaps.sort();
    // STI siblings share the table, not their columns (#813 review).
    let sti = references("alias", "employee").unwrap();
    let targets: Vec<&str> = sti
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["target"].as_str().unwrap())
        .collect();
    assert_eq!(targets, ["Manager.bonus", "Engineer.id"]);
    // Target text is capped like any other unprovable target.
    let long = extraction
        .nodes
        .iter()
        .find(|n| n.label == "Gap" && n.props["target"].as_str().unwrap_or("").ends_with('…'))
        .unwrap();
    assert!(long.props["target"].as_str().unwrap().len() <= 200 + '…'.len_utf8());
    assert_eq!(
        long.props["reason"],
        "ForeignKey target class is not a recovered entity"
    );
    assert_eq!(
        gaps,
        [
            (
                "Employee.bonus",
                "ForeignKey target is not a declared column of the class"
            ),
            (
                "Engineer.bonus",
                "ForeignKey target is not a declared column of the class"
            ),
            (
                "Ghost.id",
                "ForeignKey target class is not a recovered entity"
            ),
            (
                "User.code",
                "ForeignKey target column declares its own name"
            ),
            (
                "User.label",
                "ForeignKey target column declares its own name"
            ),
            (
                "User.missing",
                "ForeignKey target is not a declared column of the class"
            ),
            (
                "app.models.User.id",
                "ForeignKey target class is not a recovered entity"
            ),
            ("lookup().id", "ForeignKey target is not a string literal"),
        ]
        .map(|(t, r)| (t.to_string(), r.to_string()))
    );
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

#[test]
fn local_mount_receivers_need_the_definition_that_reaches_the_call() {
    // #747: a local app proves a mount only when the scope's sole binding of
    // the receiver is the factory and it precedes the call; rebinding,
    // later construction and nested scopes fail closed.
    let dir = tempfile::tempdir().unwrap();
    let routers = [
        "ok", "closure", "over", "late", "nl", "branch", "loop", "inner", "glob", "early", "with",
        "exc", "imp", "star", "mod", "named",
    ];
    let mut source = String::from("from fastapi import APIRouter\n");
    for name in routers {
        source.push_str(&format!(
            "r_{name} = APIRouter(prefix=\"/{name}\")\n@r_{name}.get(\"/x\")\ndef {name}_x():\n    return {{}}\n"
        ));
    }
    write(dir.path(), "routers.py", &source);
    write(
        dir.path(),
        "main.py",
        r#"
from fastapi import FastAPI
from routers import r_ok, r_closure, r_over, r_late, r_nl, r_branch, r_loop, r_inner, r_glob
from routers import r_early, r_with, r_exc, r_imp, r_star, r_mod, r_named
from fake import Fake

def genuine():
    app = FastAPI()
    # A lambda's walrus binds the lambda's own scope, not this one.
    probe = lambda: (app := None)
    app.include_router(r_ok, prefix="/v1")

def closure():
    app = FastAPI()
    def wire():
        app.include_router(r_closure, prefix="/v1")
    return wire

def overwritten():
    app = FastAPI()
    app = Fake()
    app.include_router(r_over, prefix="/wrong")

def late():
    app.include_router(r_late, prefix="/wrong")
    app = FastAPI()

def nonlocal_rebind():
    app = FastAPI()
    def rebind():
        nonlocal app
        app = Fake()
    rebind()
    app.include_router(r_nl, prefix="/wrong")

def branch(flag):
    if flag:
        app = FastAPI()
    else:
        app = Fake()
    app.include_router(r_branch, prefix="/wrong")

def looped(apps):
    app = FastAPI()
    for app in apps:
        app.include_router(r_loop, prefix="/wrong")

def inner_constructor():
    def make():
        app = FastAPI()
    app.include_router(r_inner, prefix="/wrong")

def closure_called_early():
    def wire():
        app.include_router(r_early, prefix="/wrong")
    wire()
    app = FastAPI()

def with_alias(manager):
    app = FastAPI()
    with manager as app:
        pass
    def wire():
        app.include_router(r_with, prefix="/wrong")
    return wire

def except_alias():
    app = FastAPI()
    try:
        pass
    except Exception as app:
        pass
    app.include_router(r_exc, prefix="/wrong")

def import_mentions_only():
    app = FastAPI()
    from settings import app as app_settings
    import settings.app
    app.include_router(r_imp, prefix="/v1")

def star_import():
    app = FastAPI()
    from settings import *
    app.include_router(r_star, prefix="/wrong")

def module_import():
    app = FastAPI()
    import app.sub
    app.include_router(r_mod, prefix="/wrong")

def from_import():
    app = FastAPI()
    from settings import app
    app.include_router(r_named, prefix="/wrong")

def global_rebind():
    global app
    app = FastAPI()
    app.include_router(r_glob, prefix="/wrong")
"#,
    );
    write(
        dir.path(),
        "fake.py",
        "class Fake:\n    def include_router(self, router, prefix=''):\n        pass\n",
    );
    let facts = endpoint_facts(&extract_dir(dir.path(), &id()).unwrap());
    let fact = |path: &str, tier: &str, mount: &str| {
        (
            "GET".to_string(),
            path.to_string(),
            tier.to_string(),
            Some(mount.to_string()),
        )
    };
    let unproven = |name: &str| {
        fact(
            &format!("/wrong/{name}/x"),
            "InferredStrong",
            "mount_receiver_unproven",
        )
    };
    assert_eq!(
        facts,
        vec![
            fact("/v1/closure/x", "Confirmed", "mounted"),
            fact("/v1/imp/x", "Confirmed", "mounted"),
            fact("/v1/ok/x", "Confirmed", "mounted"),
            unproven("branch"),
            unproven("early"),
            unproven("exc"),
            unproven("glob"),
            unproven("inner"),
            unproven("late"),
            unproven("loop"),
            unproven("mod"),
            unproven("named"),
            unproven("nl"),
            unproven("over"),
            unproven("star"),
            unproven("with"),
        ]
    );
}

/// An endpoint's `request_inputs` without `prov`, and the source text each
/// cited read spans.
fn request_inputs_cited(
    extraction: &Extraction,
    source: &str,
    method: &str,
    path: &str,
) -> (serde_json::Value, Vec<String>) {
    let mut inputs = extraction
        .nodes
        .iter()
        .find(|node| {
            node.label == "Endpoint" && node.props["method"] == method && node.props["path"] == path
        })
        .unwrap_or_else(|| panic!("{method} {path}"))
        .props["request_inputs"]
        .clone();
    let reads = inputs
        .as_object_mut()
        .and_then(|inputs| inputs.remove("prov"))
        .map(|prov| {
            assert_eq!(prov["confidence_tier"], "Confirmed");
            prov["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| {
                    let range = span["byte_start"].as_u64().unwrap() as usize
                        ..span["byte_end"].as_u64().unwrap() as usize;
                    source[range].to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    (inputs, reads)
}

fn request_inputs(extraction: &Extraction, method: &str, path: &str) -> serde_json::Value {
    let mut inputs = extraction
        .nodes
        .iter()
        .find(|node| {
            node.label == "Endpoint" && node.props["method"] == method && node.props["path"] == path
        })
        .unwrap_or_else(|| panic!("{method} {path}"))
        .props["request_inputs"]
        .clone();
    if let Some(inputs) = inputs.as_object_mut() {
        inputs.remove("prov");
    }
    inputs
}

#[test]
fn flask_handlers_record_literal_request_inputs() {
    // #721: the literal JSON body, query and form names a Flask handler reads,
    // with the reason whenever a read is not literal.
    let source = br#"
from flask import Flask, request

app = Flask(__name__)

@app.route("/login", methods=["POST"])
def login():
    data = request.get_json() or {}
    email = data.get("email")
    password = data["password"]
    device = data.get("device", "unknown")
    hostname = request.args.get("hostname")
    page = request.args["page_id"]
    tags = request.args.getlist("tag")
    pinned = "pinned" in request.args
    remember = "remember" not in data
    return ""

@app.route("/direct", methods=["POST"])
def direct():
    name = request.get_json(silent=True).get("name")
    note = request.json["note"]
    form = request.form.get("csrf")
    both = request.values.get("q")
    agent = request.headers.get("User-Agent")
    return ""

@app.route("/dynamic", methods=["POST"])
def dynamic(key):
    data = request.get_json()
    value = data.get(key)
    helper(data)
    q = request.args.get(key)
    return ""

@app.route("/passed")
def passed():
    def inner():
        return request.args.get("hidden")
    return process(request)

@app.route("/none")
def nothing():
    return ""
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    assert_eq!(
        request_inputs(&extraction, "POST", "/login"),
        serde_json::json!({
            "body": ["device", "email", "password", "remember"],
            "query": ["hostname", "page_id", "pinned", "tag"],
            "scope": "handler_body",
        })
    );
    assert_eq!(
        request_inputs(&extraction, "POST", "/direct"),
        serde_json::json!({
            "body": ["name", "note"],
            "query": ["q"],
            "form": ["csrf", "q"],
            "scope": "handler_body",
        })
    );
    assert_eq!(
        request_inputs(&extraction, "POST", "/dynamic"),
        serde_json::json!({
            "partial": ["body_read_dynamically", "query_read_dynamically"],
            "scope": "handler_body",
        })
    );
    // A nested function is outside the handler; passing `request` itself is
    // a partial read.
    assert_eq!(
        request_inputs(&extraction, "GET", "/passed"),
        serde_json::json!({ "partial": ["request_passed"], "scope": "handler_body" })
    );
    assert!(request_inputs(&extraction, "GET", "/none").is_null());
}

#[test]
fn request_inputs_need_flask_request_and_follow_blueprint_routes() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/base.py",
        "from flask import Blueprint\napi_bp = Blueprint(\"api\", __name__, url_prefix=\"/api\")\n",
    );
    write(
        dir.path(),
        "app/views.py",
        r#"
import flask
from app.base import api_bp

@api_bp.route("/aliases", methods=["POST"])
def create():
    data = flask.request.get_json()
    return data["alias_prefix"] + flask.request.args.get("hostname")
"#,
    );
    write(
        dir.path(),
        "app/other.py",
        r#"
from app.base import api_bp
from mylib import request

@api_bp.route("/lookalike")
def lookalike():
    return request.args.get("not_flask")
"#,
    );
    write(
        dir.path(),
        "server.py",
        "from flask import Flask\nfrom app.base import api_bp\napp = Flask(__name__)\napp.register_blueprint(api_bp)\n",
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        request_inputs(&extraction, "POST", "/api/aliases"),
        serde_json::json!({
            "body": ["alias_prefix"],
            "query": ["hostname"],
            "scope": "handler_body",
        })
    );
    assert!(request_inputs(&extraction, "GET", "/api/lookalike").is_null());
}

#[test]
fn request_inputs_cite_reads_and_skip_writes_and_shadowed_request() {
    // #721 review: each name cites the read expression; written or deleted
    // keys are not reads; a parameter or local named `request` is not Flask's.
    let source = r#"
from flask import Flask, request

app = Flask(__name__)

@app.route("/save", methods=["POST"])
def save():
    data = request.get_json()
    name = data["name"]
    data["user_id"] = 1
    data["count"] += 1
    data["a"], other = 1, 2
    del data["tmp"]
    del request.args["gone"]
    q = request.args.get("q")
    return ""

@app.route("/param/<request>")
def param(request):
    return request.args.get("x")

@app.route("/local")
def local():
    request = make_request()
    return request.args.get("y")
"#;
    let extraction = extract_source(source.as_bytes(), "app.py", &id()).unwrap();
    let (inputs, reads) = request_inputs_cited(&extraction, source, "POST", "/save");
    assert_eq!(
        inputs,
        serde_json::json!({
            "body": ["count", "name"],
            "query": ["q"],
            "scope": "handler_body",
        })
    );
    assert_eq!(
        reads,
        [
            r#"data["name"]"#,
            r#"data["count"]"#,
            r#"request.args.get("q")"#
        ]
    );
    assert!(request_inputs(&extraction, "GET", "/param/<request>").is_null());
    assert!(request_inputs(&extraction, "GET", "/local").is_null());
}

fn responses_cited(
    extraction: &Extraction,
    source: &str,
    method: &str,
    path: &str,
) -> (serde_json::Value, Vec<String>) {
    let mut responses = extraction
        .nodes
        .iter()
        .find(|node| {
            node.label == "Endpoint" && node.props["method"] == method && node.props["path"] == path
        })
        .unwrap_or_else(|| panic!("{method} {path}"))
        .props["responses"]
        .clone();
    let cited = responses
        .as_object_mut()
        .and_then(|responses| responses.remove("prov"))
        .map(|prov| {
            assert_eq!(prov["confidence_tier"], "Confirmed");
            prov["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| {
                    source[span["byte_start"].as_u64().unwrap() as usize
                        ..span["byte_end"].as_u64().unwrap() as usize]
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    (responses, cited)
}

#[test]
fn flask_handlers_record_literal_response_statuses_and_keys() {
    // #721: statuses and top-level JSON keys a handler's own returns prove,
    // with the reason whenever a response is built elsewhere.
    let source = r#"
from flask import Flask, jsonify, redirect, render_template

app = Flask(__name__)

@app.route("/login", methods=["POST"])
def login():
    if bad():
        return jsonify(error="wrong password"), 400
    if locked():
        return (jsonify(error="locked"), 403)
    if legacy():
        return {"ok": False, "reason": "legacy"}, 410
    return jsonify({"api_key": key(), "email": "e"})

@app.route("/built")
def built():
    if a():
        return jsonify(user_to_dict(u))
    if b():
        return jsonify(**payload), 201
    if c():
        return jsonify(ok=True), code
    def inner():
        return jsonify(hidden=1), 500
    return redirect("/")

@app.route("/page")
def page():
    if moved():
        return redirect("/new", code=301)
    if other():
        return response
    return render_template("page.html")

@app.route("/none")
def nothing():
    do()
"#;
    let extraction = extract_source(source.as_bytes(), "app.py", &id()).unwrap();
    let (responses, cited) = responses_cited(&extraction, source, "POST", "/login");
    assert_eq!(
        responses,
        serde_json::json!({
            "kinds": ["json"],
            "statuses": [200, 400, 403, 410],
            "keys": ["api_key", "email", "error", "ok", "reason"],
            "scope": "handler_body",
        })
    );
    assert_eq!(cited.len(), 4);
    assert!(cited[0].starts_with("return jsonify(error="));
    let (responses, cited) = responses_cited(&extraction, source, "GET", "/built");
    assert_eq!(
        responses,
        serde_json::json!({
            "kinds": ["json", "redirect"],
            "statuses": [200, 201, 302],
            "keys": ["ok"],
            "partial": ["response_built_elsewhere", "status_computed"],
            "scope": "handler_body",
        })
    );
    assert_eq!(cited.len(), 4);
    let (responses, cited) = responses_cited(&extraction, source, "GET", "/page");
    assert_eq!(
        responses,
        serde_json::json!({
            "kinds": ["html", "redirect"],
            "statuses": [200, 301],
            "partial": ["response_not_proven"],
            "scope": "handler_body",
        })
    );
    assert_eq!(
        cited,
        [
            r#"return redirect("/new", code=301)"#,
            r#"return render_template("page.html")"#
        ]
    );
    assert!(
        extraction
            .nodes
            .iter()
            .find(|node| node.label == "Endpoint" && node.props["path"] == "/none")
            .unwrap()
            .props
            .get("responses")
            .is_none()
    );
}

#[test]
fn responses_need_flask_and_an_unshadowed_jsonify() {
    let fastapi = br#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/items")
def items():
    return {"items": []}, 201
"#;
    let extraction = extract_source(fastapi, "main.py", &id()).unwrap();
    let endpoint = extraction
        .nodes
        .iter()
        .find(|node| node.label == "Endpoint")
        .unwrap();
    assert!(endpoint.props.get("responses").is_none());
    let shadowed = r#"
import flask

app = flask.Flask(__name__)

@app.route("/a")
def a(jsonify):
    return flask.jsonify(ok=True)

@app.route("/b")
def b():
    flask = other()
    return flask.jsonify(ok=True)
"#;
    let extraction = extract_source(shadowed.as_bytes(), "app.py", &id()).unwrap();
    let (a, _) = responses_cited(&extraction, shadowed, "GET", "/a");
    assert_eq!(
        a,
        serde_json::json!({
            "kinds": ["json"],
            "statuses": [200],
            "keys": ["ok"],
            "scope": "handler_body",
        })
    );
    let (b, _) = responses_cited(&extraction, shadowed, "GET", "/b");
    assert_eq!(
        b,
        serde_json::json!({ "partial": ["response_not_proven"], "scope": "handler_body" })
    );
}

#[test]
fn handler_inputs_and_responses_follow_the_route_and_the_commit() {
    // #941 review: nested `prov` is retargeted on cache reuse, including for
    // pending Blueprint routes; responses are only read for Flask routes; a
    // handler-local `from … import jsonify` or nested `def jsonify` is not
    // Flask's.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/base.py",
        "from flask import Blueprint\napi_bp = Blueprint(\"api\", __name__, url_prefix=\"/api\")\n",
    );
    write(
        dir.path(),
        "app/views.py",
        r#"
from flask import Flask, jsonify, request
from fastapi import FastAPI
from app.base import api_bp

app = Flask(__name__)
app.register_blueprint(api_bp)
fast = FastAPI()

@api_bp.route("/items", methods=["POST"])
def create():
    name = request.args.get("name")
    return jsonify(id=1), 201

@fast.get("/fast")
def fast_items():
    return {"items": []}, 201

@app.route("/imported")
def imported():
    from custom import jsonify
    return jsonify(ok=True)

@app.route("/nested")
def nested():
    def jsonify(**kwargs):
        return kwargs
    return jsonify(ok=True)
"#,
    );
    let endpoint = |extraction: &Extraction, method: &str, path: &str| {
        extraction
            .nodes
            .iter()
            .find(|node| {
                node.label == "Endpoint"
                    && node.props["method"] == method
                    && node.props["path"] == path
            })
            .unwrap_or_else(|| panic!("{method} {path}"))
            .props
            .clone()
    };
    let mut cache = IncrementalCache::default();
    let (first, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    let created = endpoint(&first, "POST", "/api/items");
    assert_eq!(created["responses"]["statuses"], serde_json::json!([201]));
    assert!(endpoint(&first, "GET", "/fast").get("responses").is_none());
    for path in ["/imported", "/nested"] {
        assert_eq!(
            endpoint(&first, "GET", path)["responses"]["partial"],
            serde_json::json!(["response_not_proven"]),
            "{path}"
        );
    }
    let next_id = SourceId {
        repo: "local/python-service",
        commit: "def456",
    };
    let (reused, stats) = extract_dir_incremental(dir.path(), &next_id, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    let created = endpoint(&reused, "POST", "/api/items");
    for nested in ["request_inputs", "responses"] {
        assert_eq!(
            created[nested]["prov"]["evidence"][0]["commit_sha"], "def456",
            "{nested}"
        );
    }
}

#[test]
fn flask_responses_follow_repository_decorators_and_dict_builders() {
    // #940: an auth decorator's wrapper responses (through the check it
    // returns the result of) and the keys a helper's or a local's dict is
    // built with are followed across files, each citing its statement.
    let dir = tempfile::tempdir().unwrap();
    let base = r#"
from functools import wraps
from flask import Blueprint, jsonify

api_bp = Blueprint("api", __name__, url_prefix="/api")

def authorize():
    if missing():
        return jsonify(error="Wrong api key"), 401
    return None

def require_auth(f):
    @wraps(f)
    def decorated(*args, **kwargs):
        error_return = authorize()
        if error_return:
            return error_return
        if not sudo():
            return jsonify(error="Need sudo"), 440
        return f(*args, **kwargs)
    return decorated

def tagged(f):
    f.tagged = True
    return f

def typed_auth(f: Callable):
    def wrapper(*args, **kwargs):
        if banned():
            return jsonify(error="Banned"), 403
        return f(*args, **kwargs)
    return wrapper

def rebound(f):
    def wrapper(*args, **kwargs):
        result = authorize()
        try:
            check()
        except Error as result:
            pass
        if result:
            return result
        return f(*args, **kwargs)
    return wrapper
"#;
    let views = r#"
from flask import jsonify
from app.base import api_bp, require_auth, tagged, typed_auth, rebound
from app.serial import payload, loose

@api_bp.route("/me")
@require_auth
@tagged
def me():
    return jsonify(**payload(user)), 200

@api_bp.route("/local")
@limiter.limit("1/minute")
def local():
    ret = {"a": 1}
    ret["b"] = 2
    return jsonify(ret)

@api_bp.route("/mutated")
def mutated():
    ret = {"a": 1}
    ret.update(extra)
    if other():
        return jsonify(loose())
    return jsonify(ret)

@api_bp.route("/typed")
@typed_auth
def typed():
    return jsonify(ok=True)

@api_bp.route("/rebound")
@rebound
def rebound_view():
    return jsonify(ok=True)

@api_bp.route("/alias")
def alias():
    ret = {"a": 1}
    other = ret
    other[key] = 2
    return jsonify(ret)
"#;
    let serial = r#"
def payload(user):
    ret = {"name": user.name}
    if user.otp:
        ret["mfa_key"] = sign(user)
    return ret

def loose():
    return dict(a=1)
"#;
    write(dir.path(), "app/base.py", base);
    write(dir.path(), "app/views.py", views);
    write(dir.path(), "app/serial.py", serial);
    write(
        dir.path(),
        "app/main.py",
        "from flask import Flask\nfrom app.base import api_bp\napp = Flask(__name__)\napp.register_blueprint(api_bp)\n",
    );
    let sources = [
        ("app/base.py", base),
        ("app/views.py", views),
        ("app/serial.py", serial),
    ];
    let responses = |extraction: &Extraction, path: &str| {
        let mut responses = extraction
            .nodes
            .iter()
            .find(|node| node.label == "Endpoint" && node.props["path"] == path)
            .unwrap_or_else(|| panic!("{path}"))
            .props["responses"]
            .clone();
        let prov = responses.as_object_mut().unwrap().remove("prov").unwrap();
        let cited = prov["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|span| {
                let (_, source) = sources
                    .iter()
                    .find(|(file, _)| span["path"] == *file)
                    .unwrap();
                let text = &source[span["byte_start"].as_u64().unwrap() as usize
                    ..span["byte_end"].as_u64().unwrap() as usize];
                (
                    span["commit_sha"].as_str().unwrap().to_string(),
                    text.to_string(),
                )
            })
            .collect::<Vec<_>>();
        (responses, cited)
    };
    let mut cache = IncrementalCache::default();
    let (first, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    let (me, cited) = responses(&first, "/api/me");
    assert_eq!(
        me,
        serde_json::json!({
            "kinds": ["json"],
            "statuses": [200, 401, 440],
            "keys": ["error", "mfa_key", "name"],
            "followed": [
                "sym:local/python-service@app/base.py#authorize",
                "sym:local/python-service@app/base.py#require_auth",
                "sym:local/python-service@app/serial.py#payload",
            ],
            "scope": "handler_body_and_followed",
        })
    );
    let cited = cited.into_iter().map(|(_, text)| text).collect::<Vec<_>>();
    assert_eq!(
        cited,
        [
            r#"return jsonify(error="Wrong api key"), 401"#,
            "return error_return",
            r#"return jsonify(error="Need sudo"), 440"#,
            r#"ret = {"name": user.name}"#,
            r#"ret["mfa_key"] = sign(user)"#,
            "return ret",
            "return jsonify(**payload(user)), 200",
        ]
    );
    let (local, cited) = responses(&first, "/api/local");
    assert_eq!(
        local,
        serde_json::json!({
            "kinds": ["json"],
            "statuses": [200],
            "keys": ["a", "b"],
            "scope": "handler_body",
        })
    );
    assert_eq!(cited.len(), 3);
    let (mutated, _) = responses(&first, "/api/mutated");
    assert_eq!(
        mutated,
        serde_json::json!({
            "kinds": ["json"],
            "statuses": [200],
            "partial": ["response_built_elsewhere"],
            "scope": "handler_body",
        })
    );
    // #942 review: a typed wrapped parameter is still a decorator; a check
    // result rebound by `except … as` is not followed; a dict escaping
    // through an alias is not proven.
    let (typed, _) = responses(&first, "/api/typed");
    assert_eq!(typed["statuses"], serde_json::json!([200, 403]));
    let (rebound, _) = responses(&first, "/api/rebound");
    assert_eq!(rebound["statuses"], serde_json::json!([200]));
    assert_eq!(
        rebound["partial"],
        serde_json::json!(["response_not_proven"])
    );
    let (alias, _) = responses(&first, "/api/alias");
    assert_eq!(
        alias["partial"],
        serde_json::json!(["response_built_elsewhere"])
    );
    assert!(alias.get("keys").is_none());
    let next_id = SourceId {
        repo: "local/python-service",
        commit: "def456",
    };
    let (reused, stats) = extract_dir_incremental(dir.path(), &next_id, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    let (_, cited) = responses(&reused, "/api/me");
    assert!(cited.iter().all(|(commit, _)| commit == "def456"));
}

#[test]
fn pytest_flask_client_calls_link_tests_to_recovered_endpoints() {
    // #946 (W10a): a pytest test's calls on a proven Flask test client link
    // to the endpoints they request, by `url_for` endpoint name or by path,
    // with the statuses asserted and keys read; anything unresolved is a Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/base.py",
        "from flask import Blueprint\napi_bp = Blueprint(\"api\", __name__, url_prefix=\"/api\")\n",
    );
    write(
        dir.path(),
        "app/views.py",
        r#"
from flask import jsonify
from app.base import api_bp

@api_bp.route("/aliases")
def get_aliases():
    return jsonify(aliases=[])

@api_bp.route("/aliases/<int:alias_id>", methods=["GET", "PATCH"])
def update_alias(alias_id):
    return jsonify(ok=True)

@api_bp.route("/aliases/<int:alias_id>", methods=["DELETE"])
def delete_alias(alias_id):
    return jsonify(deleted=True)

@api_bp.route("/named", endpoint="custom")
def named():
    return jsonify(ok=True)
"#,
    );
    write(
        dir.path(),
        "app/admin.py",
        r#"
from flask import Blueprint, jsonify
admin_bp = Blueprint(name="admin", import_name=__name__)
users_bp = Blueprint("users", __name__)

@users_bp.route("/")
def list_users():
    return jsonify(users=[])

admin_bp.register_blueprint(users_bp, url_prefix="/users")
"#,
    );
    write(
        dir.path(),
        "app/main.py",
        r#"
from flask import Flask, jsonify
from app.base import api_bp
from app.admin import admin_bp
app = Flask(__name__)
app.register_blueprint(api_bp)
app.register_blueprint(admin_bp, url_prefix="/admin")

@app.route("/health")
def health():
    return jsonify(ok=True)
"#,
    );
    write(
        dir.path(),
        "tests/conftest.py",
        r#"
import pytest
from app.main import app

@pytest.fixture
def flask_client():
    with app.test_client() as client:
        yield client

@pytest.fixture
def user():
    return make_user()
"#,
    );
    write(
        dir.path(),
        "tests/api/test_alias.py",
        r#"
from flask import url_for

def test_get_aliases(flask_client, user):
    r = flask_client.get(url_for("api.get_aliases"), headers={})
    assert r.status_code == 200
    assert r.json["aliases"] == []
    assert "total" in r.json
    r = flask_client.patch(url_for("api.update_alias", alias_id=1), json={})
    assert r.status_code == 400
    assert r.get_json()["error"]
    r = flask_client.delete(url_for("api.update_alias", alias_id=1))
    assert r.status_code == 200

def test_paths(flask_client, user):
    assert flask_client.get("/api/aliases/12?x=1").status_code == 404
    r = flask_client.get("/health", follow_redirects=True)
    assert r.status_code == 200
    flask_client.get(f"/api/aliases/{alias.id}")
    flask_client.post(url_for("api.get_aliases"))
    flask_client.get(url_for("dashboard.index"))
    user.get("x")

class TestNested:
    def test_named(self, flask_client):
        flask_client.get(url_for("api.custom"))
        flask_client.get(url_for("admin.users.list_users"))

def test_local_client():
    from app.main import app
    client = app.test_client()
    client.get("/health")

def test_rebound(flask_client):
    flask_client = other()
    flask_client.get("/health")

def helper(flask_client):
    flask_client.get("/health")
"#,
    );
    write(
        dir.path(),
        "scripts/test_outside.py",
        "def test_outside(flask_client):\n    flask_client.get(\"/health\")\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let name = |path: &str| {
        out.nodes
            .iter()
            .find(|node| node.label == "Endpoint" && node.props["path"] == path)
            .unwrap_or_else(|| panic!("{path}"))
            .props["endpoint_name"]
            .clone()
    };
    assert_eq!(name("/api/aliases"), "api.get_aliases");
    assert_eq!(name("/api/named"), "api.custom");
    assert_eq!(name("/health"), "health");
    assert_eq!(name("/admin/users/"), "admin.users.list_users");

    let mut tests = out
        .nodes
        .iter()
        .filter(|node| node.label == "TestCase")
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    tests.sort();
    let t = |name: &str| format!("test:local/python-service@tests/api/test_alias.py#{name}");
    assert_eq!(
        tests,
        [
            "test:local/python-service@scripts/test_outside.py#test_outside".to_string(),
            t("TestNested.test_named"),
            t("test_get_aliases"),
            t("test_local_client"),
            t("test_paths"),
            t("test_rebound"),
        ]
    );

    let ep = |method: &str, path: &str| format!("ep:local/python-service@{method}:{path}");
    let mut links = out
        .edges
        .iter()
        .filter(|edge| edge.label == "REQUESTS" && edge.dst.starts_with("ep:"))
        .map(|edge| {
            assert_eq!(edge.props["link"], "client_app_assumed");
            assert_eq!(edge.props["prov"]["confidence_tier"], "InferredStrong");
            (
                edge.src.clone(),
                edge.dst.clone(),
                edge.props["via"].clone(),
                edge.props["statuses"].clone(),
                edge.props["keys"].clone(),
            )
        })
        .collect::<Vec<_>>();
    links.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let json = |value: serde_json::Value| value;
    assert_eq!(
        links,
        [
            (
                t("TestNested.test_named"),
                ep("GET", "/admin/users/"),
                json(serde_json::json!(["url_for"])),
                json(serde_json::json!([])),
                json(serde_json::json!([])),
            ),
            (
                t("TestNested.test_named"),
                ep("GET", "/api/named"),
                json(serde_json::json!(["url_for"])),
                json(serde_json::json!([])),
                json(serde_json::json!([])),
            ),
            (
                t("test_get_aliases"),
                ep("DELETE", "/api/aliases/<int:alias_id>"),
                json(serde_json::json!(["url_for"])),
                json(serde_json::json!([200])),
                json(serde_json::json!([])),
            ),
            (
                t("test_get_aliases"),
                ep("GET", "/api/aliases"),
                json(serde_json::json!(["url_for"])),
                json(serde_json::json!([200])),
                json(serde_json::json!(["aliases", "total"])),
            ),
            (
                t("test_get_aliases"),
                ep("PATCH", "/api/aliases/<int:alias_id>"),
                json(serde_json::json!(["url_for"])),
                json(serde_json::json!([400])),
                json(serde_json::json!(["error"])),
            ),
            (
                t("test_local_client"),
                ep("GET", "/health"),
                json(serde_json::json!(["path"])),
                json(serde_json::json!([])),
                json(serde_json::json!([])),
            ),
            (
                t("test_paths"),
                ep("GET", "/api/aliases/<int:alias_id>"),
                json(serde_json::json!(["path"])),
                json(serde_json::json!([404])),
                json(serde_json::json!([])),
            ),
            (
                t("test_paths"),
                ep("GET", "/health"),
                json(serde_json::json!(["path"])),
                json(serde_json::json!([])),
                json(serde_json::json!([])),
            ),
        ]
    );

    let mut gaps = out
        .edges
        .iter()
        .filter(|edge| edge.label == "REQUESTS" && edge.dst.starts_with("gap:"))
        .map(|edge| {
            let gap = out.nodes.iter().find(|node| node.id == edge.dst).unwrap();
            (
                edge.src.clone(),
                gap.props["request"].as_str().unwrap().to_string(),
                gap.props["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            (
                t("test_paths"),
                "GET".to_string(),
                "request URL is computed".to_string()
            ),
            (
                t("test_paths"),
                "GET url_for(dashboard.index)".to_string(),
                "test client endpoint name is not a recovered endpoint".to_string()
            ),
            (
                t("test_paths"),
                "POST url_for(api.get_aliases)".to_string(),
                "no recovered endpoint serves this method at the endpoint name's URL".to_string()
            ),
        ]
    );

    // A followed redirect's response may be another page's: no expectation.
    let health = out
        .edges
        .iter()
        .find(|edge| edge.src == t("test_paths") && edge.dst == ep("GET", "/health"))
        .unwrap();
    assert_eq!(health.props["follows_redirects"], true);

    // Each link cites the call and the fixture that proves the client.
    let link = out
        .edges
        .iter()
        .find(|edge| edge.src == t("test_get_aliases") && edge.dst == ep("GET", "/api/aliases"))
        .unwrap();
    let paths = link.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|span| span["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["tests/api/test_alias.py", "tests/conftest.py"]);
}

#[test]
fn pytest_client_proof_respects_shadowing_and_opaque_options() {
    // #946 review: a nearer non-client fixture shadows an outer client
    // fixture; a test-local or parameter `url_for` / `flask` is not Flask's;
    // `**mapping` that may enable `follow_redirects` records no expectation.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/main.py",
        r#"
from flask import Flask, jsonify, redirect
app = Flask(__name__)

@app.route("/known")
def known():
    return jsonify(ok=True)

@app.route("/redirect")
def go():
    return redirect("/known")
"#,
    );
    let client_fixture = "import pytest\nfrom app.main import app\n\n@pytest.fixture\ndef client():\n    return app.test_client()\n";
    let fake_fixture = "import pytest\n\n@pytest.fixture\ndef client():\n    return FakeClient()\n";
    write(dir.path(), "tests/conftest.py", client_fixture);
    write(dir.path(), "tests/nearer/conftest.py", fake_fixture);
    write(
        dir.path(),
        "tests/nearer/test_nearer.py",
        "def test_nearer(client):\n    client.get(\"/known\")\n",
    );
    write(
        dir.path(),
        "tests/test_module.py",
        &format!("{fake_fixture}\ndef test_module(client):\n    client.get(\"/known\")\n"),
    );
    write(
        dir.path(),
        "tests/control/test_control.py",
        "def test_control(client):\n    client.get(\"/known\")\n",
    );
    write(
        dir.path(),
        "tests/test_url_for.py",
        r#"
import flask
from flask import url_for

def test_local_shadow(client):
    url_for = lambda name: "/other"
    client.get(url_for("known"))

def test_parameter_shadow(client, url_for):
    client.get(url_for("known"))

def test_module_shadow(client, flask):
    client.get(flask.url_for("known"))

def test_qualified(client):
    client.get(flask.url_for("known"))
"#,
    );
    write(
        dir.path(),
        "tests/test_redirects.py",
        r#"
def test_literal_true(client):
    r = client.get("/redirect", **{"follow_redirects": True})
    assert r.status_code == 200

def test_opaque(client, opts):
    r = client.get("/redirect", **opts)
    assert r.status_code == 200

def test_explicit_false(client):
    r = client.get("/redirect", follow_redirects=False)
    assert r.status_code == 302

def test_literal_false(client):
    r = client.get("/redirect", **{"follow_redirects": False})
    assert r.status_code == 302
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut requests = out
        .edges
        .iter()
        .filter(|edge| edge.label == "REQUESTS")
        .map(|edge| {
            let test = edge.src.rsplit_once('#').unwrap().1.to_string();
            let target = if edge.dst.starts_with("ep:") {
                edge.dst.rsplit_once('@').unwrap().1.to_string()
            } else {
                edge.props["reason"].as_str().unwrap().to_string()
            };
            (
                test,
                target,
                edge.props["statuses"].clone(),
                edge.props["follows_redirects"] == true,
            )
        })
        .collect::<Vec<_>>();
    requests.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let row = |test: &str, target: &str, statuses: serde_json::Value, follows: bool| {
        (test.to_string(), target.to_string(), statuses, follows)
    };
    let none = serde_json::Value::Null;
    let empty = serde_json::json!([]);
    assert_eq!(
        requests,
        [
            row("test_control", "GET:/known", empty.clone(), false),
            row(
                "test_explicit_false",
                "GET:/redirect",
                serde_json::json!([302]),
                false
            ),
            row(
                "test_literal_false",
                "GET:/redirect",
                serde_json::json!([302]),
                false
            ),
            row("test_literal_true", "GET:/redirect", empty.clone(), true),
            row(
                "test_local_shadow",
                "request URL is computed",
                none.clone(),
                false
            ),
            row(
                "test_module_shadow",
                "request URL is computed",
                none.clone(),
                false
            ),
            row("test_opaque", "GET:/redirect", empty.clone(), true),
            row(
                "test_parameter_shadow",
                "request URL is computed",
                none,
                false
            ),
            row("test_qualified", "GET:/known", empty, false),
        ]
    );
}

#[test]
fn pytest_tests_link_to_their_own_function_symbols() {
    // #965: each TestCase is IMPLEMENTED_BY the Symbol of its own function,
    // so the function's direct calls are the test's static references.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "tests/test_jobs.py",
        "from app.jobs import cleanup\n\ndef test_cleanup():\n    cleanup()\n\nclass TestJobs:\n    def test_again(self):\n        cleanup()\n",
    );
    write(dir.path(), "app/jobs.py", "def cleanup():\n    return 1\n");
    let out = extract_dir(dir.path(), &id()).unwrap();
    let symbols: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Symbol")
        .map(|n| n.id.as_str())
        .collect();
    let mut implemented: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|e| e.label == "IMPLEMENTED_BY")
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    implemented.sort();
    let at = |name: &str| format!("local/python-service@tests/test_jobs.py#{name}");
    assert_eq!(
        implemented,
        [
            (
                format!("test:{}", at("TestJobs.test_again")).as_str(),
                format!("sym:{}", at("TestJobs.test_again")).as_str()
            ),
            (
                format!("test:{}", at("test_cleanup")).as_str(),
                format!("sym:{}", at("test_cleanup")).as_str()
            ),
        ]
    );
    assert!(
        implemented
            .iter()
            .all(|(_, symbol)| symbols.contains(symbol))
    );
    // The test function's direct call is an ordinary CALLS edge.
    assert!(out.edges.iter().any(|e| e.label == "CALLS"
        && e.src == format!("sym:{}", at("test_cleanup"))
        && e.dst == "sym:local/python-service@app/jobs.py#cleanup"));
}

#[test]
fn sqlalchemy_direct_session_and_model_helper_writes_emit_writes_edges() {
    // AC-0357 (T-0357), #1066 (W11d): direct session writes (Confirmed, resolution "session")
    // and @classmethod model-helper writes (InferredStrong, resolution
    // "model-helper", via "<Class>.<method>") emit WRITES edges from the
    // enclosing function Symbol to the recovered SQLAlchemy DataEntity.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/db.py",
        r#"
import sqlalchemy
from sqlalchemy.orm import scoped_session
from sqlalchemy.orm import sessionmaker

Session = scoped_session(sessionmaker(bind=engine))
Session: sqlalchemy.orm.Session
"#,
    );
    write(
        dir.path(),
        "app/models.py",
        r#"
from sqlalchemy import Column, Integer, String
from sqlalchemy.orm import declarative_base
import third_party
from app.db import Session

Base = declarative_base()

class ModelMixin:
    @classmethod
    def create(cls, **kw):
        r = cls(**kw)
        Session.add(r)
        return r

    @classmethod
    def delete(cls, obj_id, commit=False):
        Session.query(cls).filter(cls.id == obj_id).delete()

    @classmethod
    def get_or_create(cls, **kw):
        r = cls.get_by(**kw)
        if not r:
            r = cls(**kw)
            Session.add(r)
        return r

    @classmethod
    def build(cls, **kw):
        return cls(**kw)

    @classmethod
    def log(cls, **kw):
        Session.add(AliasMailbox(**kw))

class Alias(Base, ModelMixin):
    __tablename__ = "alias"
    id = Column(Integer, primary_key=True)
    email = Column(String(128), nullable=False)

class AliasMailbox(Base, ModelMixin):
    __tablename__ = "alias_mailbox"
    id = Column(Integer, primary_key=True)

class User(Base, ModelMixin):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)

    @classmethod
    def create(cls, **kw):
        return super().create(**kw)

class NotModel(ModelMixin):
    pass

class BlockedModel(Base, third_party.Mixin, ModelMixin):
    __tablename__ = "blocked"
    id = Column(Integer, primary_key=True)
"#,
    );
    write(
        dir.path(),
        "app/views.py",
        r#"
import app.models as models
from app.db import Session
from app.models import Alias, AliasMailbox, BlockedModel, NotModel, User

def new_alias():
    Alias.create(email="a@b.c")

def rm(i):
    Alias.delete(i)

def direct():
    a = Alias(email=1)
    Session.add(a)

def q():
    Session.query(AliasMailbox).filter(AliasMailbox.id == 1).delete()

def both():
    Alias.create()
    Session.query(Alias).filter().delete()

def goc():
    Alias.get_or_create(email=1)

def neg_override():
    User.create(email="u@b.c")

def neg_local_session():
    Session = make_session()
    a = Alias(email=1)
    Session.add(a)

def neg_param_session(Session):
    a = Alias(email=1)
    Session.add(a)

def neg_not_model():
    NotModel.create()

def neg_build():
    Alias.build(email="a@b.c")

def neg_getattr():
    getattr(models, "Alias").create()

def neg_split_caller():
    x = Alias()
    neg_split_callee(x)

def neg_split_callee(x):
    Session.add(x)

def neg_unresolvable_base():
    BlockedModel.create()

def neg_other_model():
    # The helper writes another model, never `cls`.
    Alias.log()

def neg_instance_call():
    a = Alias(email=1)
    a.create()
"#,
    );
    write(
        dir.path(),
        "app/unimported_session.py",
        r#"
from app.models import Alias

def neg_unimported():
    a = Alias(email=1)
    Session.add(a)
"#,
    );

    let mut cache = IncrementalCache::default();
    let (extraction, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();

    let writes: BTreeMap<(&str, &str), &Edge> = extraction
        .edges
        .iter()
        .filter(|e| e.label == "WRITES")
        .map(|e| ((e.src.as_str(), e.dst.as_str()), e))
        .collect();

    let sym = |name: &str| format!("sym:local/python-service@app/views.py#{name}");
    let alias_entity = "data:local/python-service@sqlalchemy:alias";
    let mailbox_entity = "data:local/python-service@sqlalchemy:alias_mailbox";

    // `ModelMixin.log` itself writes alias_mailbox directly; its callers
    // (`neg_other_model`) get nothing, since it never writes `cls`.
    assert!(writes.contains_key(&(
        "sym:local/python-service@app/models.py#ModelMixin.log",
        "data:local/python-service@sqlalchemy:alias_mailbox"
    )));
    assert_eq!(
        writes.len(),
        7,
        "unexpected WRITES edges: {:?}",
        writes.keys().collect::<Vec<_>>()
    );

    let e_new = writes[&(sym("new_alias").as_str(), alias_entity)];
    assert_eq!(e_new.props["operation"], "insert");
    assert_eq!(e_new.props["resolution"], "model-helper");
    assert_eq!(e_new.props["via"], "ModelMixin.create");
    assert_eq!(e_new.props["prov"]["confidence_tier"], "InferredStrong");

    let e_rm = writes[&(sym("rm").as_str(), alias_entity)];
    assert_eq!(e_rm.props["operation"], "delete");
    assert_eq!(e_rm.props["resolution"], "model-helper");
    assert_eq!(e_rm.props["via"], "ModelMixin.delete");
    assert_eq!(e_rm.props["prov"]["confidence_tier"], "InferredStrong");

    let e_direct = writes[&(sym("direct").as_str(), alias_entity)];
    assert_eq!(e_direct.props["operation"], "insert");
    assert_eq!(e_direct.props["resolution"], "session");
    assert!(e_direct.props.get("via").is_none());
    assert_eq!(e_direct.props["prov"]["confidence_tier"], "Confirmed");

    let e_q = writes[&(sym("q").as_str(), mailbox_entity)];
    assert_eq!(e_q.props["operation"], "delete");
    assert_eq!(e_q.props["resolution"], "session");
    assert!(e_q.props.get("via").is_none());
    assert_eq!(e_q.props["prov"]["confidence_tier"], "Confirmed");

    let e_both = writes[&(sym("both").as_str(), alias_entity)];
    assert_eq!(e_both.props["operation"], "insert");
    assert_eq!(
        e_both.props["operations"],
        serde_json::json!(["delete", "insert"])
    );
    assert_eq!(e_both.props["resolution"], "session");
    assert_eq!(e_both.props["prov"]["confidence_tier"], "Confirmed");
    let both_ev = &e_both.props["prov"]["evidence"][0];
    let views_src = std::fs::read_to_string(dir.path().join("app/views.py")).unwrap();
    let cited = &views_src[both_ev["byte_start"].as_u64().unwrap() as usize
        ..both_ev["byte_end"].as_u64().unwrap() as usize];
    // A Confirmed edge cites the session write that confirms it.
    assert_eq!(cited, "Session.query(Alias).filter().delete()");

    let e_goc = writes[&(sym("goc").as_str(), alias_entity)];
    assert_eq!(e_goc.props["operation"], "insert");
    assert_eq!(e_goc.props["resolution"], "model-helper");
    assert_eq!(e_goc.props["via"], "ModelMixin.get_or_create");
    assert_eq!(e_goc.props["prov"]["confidence_tier"], "InferredStrong");

    // Incremental cache reuse retargets commit SHAs on WRITES edges.
    let next_id = SourceId {
        repo: "local/python-service",
        commit: "def456",
    };
    let (reused, stats) = extract_dir_incremental(dir.path(), &next_id, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    for edge in reused.edges.iter().filter(|e| e.label == "WRITES") {
        assert_eq!(edge.props["prov"]["evidence"][0]["commit_sha"], "def456");
    }
}

#[test]
fn sqlalchemy_writes_fail_closed_on_rebinding_decorators_and_c3_order() {
    // AC-0357 (T-0357), #1066 (W11d): a session or factory name bound twice
    // at module level proves nothing; a helper under any decorator besides
    // `@classmethod` proves nothing; helper lookup follows Python's C3 order,
    // so a diamond reaches the non-writing override. Each has a control.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/db.py",
        r#"
from sqlalchemy.orm import scoped_session

Session = scoped_session(None)
"#,
    );
    write(
        dir.path(),
        "app/models.py",
        r#"
from sqlalchemy import Column, Integer
from sqlalchemy.orm import declarative_base
from app.db import Session

Base = declarative_base()

class Root(object):
    @classmethod
    def create(cls, **kw):
        Session.add(cls(**kw))

class Left(Root):
    pass

class Right(Root):
    @classmethod
    def create(cls, **kw):
        return None

class Diamond(Left, Right, Base):
    __tablename__ = "diamond"
    id = Column(Integer, primary_key=True)

class LeftOnly(Left, Base):
    __tablename__ = "left_only"
    id = Column(Integer, primary_key=True)

class Deco:
    @classmethod
    @replace_with_noop
    def make(cls, **kw):
        Session.add(cls(**kw))

    @replace_with_noop
    @classmethod
    def make2(cls, **kw):
        Session.add(cls(**kw))

    @classmethod
    def make3(cls, **kw):
        Session.add(cls(**kw))

class Decorated(Base, Deco):
    __tablename__ = "decorated"
    id = Column(Integer, primary_key=True)
"#,
    );
    write(
        dir.path(),
        "app/good.py",
        r#"
from sqlalchemy.orm import scoped_session
from app.models import Decorated, Diamond, LeftOnly

Session = scoped_session(None)
Other = Session

def pos_unshadowed():
    Session.add(LeftOnly())

def pos_alias():
    Other.add(LeftOnly())

def pos_inherited():
    LeftOnly.create()

def neg_diamond():
    Diamond.create()

def neg_decorated_outer():
    Decorated.make()

def neg_decorated_inner():
    Decorated.make2()

def pos_decorated():
    Decorated.make3()
"#,
    );
    write(
        dir.path(),
        "app/aliased.py",
        r#"
from sqlalchemy.orm import scoped_session
from app.models import LeftOnly

Session = fake
Other = Session
Session = scoped_session(None)

def neg_alias_order():
    Other.add(LeftOnly())

def neg_rebound_session():
    Session.add(LeftOnly())
"#,
    );
    // Any binding form counts: loop, `with`/`except` aliases, walrus, and a
    // nested `global`. A comprehension target is its own scope.
    for (file, body) in [
        (
            "loop_factory",
            "for scoped_session in [fake_factory]:\n    pass\nSession = scoped_session(None)\n",
        ),
        (
            "loop_session",
            "Session = scoped_session(None)\nfor Session in [fake]:\n    pass\n",
        ),
        (
            "with_session",
            "Session = scoped_session(None)\nwith fake() as Session:\n    pass\n",
        ),
        (
            "except_session",
            "Session = scoped_session(None)\ntry:\n    pass\nexcept Exception as Session:\n    pass\n",
        ),
        (
            "walrus_session",
            "Session = scoped_session(None)\nif (Session := fake):\n    pass\n",
        ),
        (
            "global_session",
            "Session = scoped_session(None)\ndef reset():\n    global Session\n    Session = fake\n",
        ),
        (
            "comprehension_control",
            "Session = scoped_session(None)\nnames = [Session for Session in []]\n",
        ),
    ] {
        write(
            dir.path(),
            &format!("app/{file}.py"),
            &format!(
                "from sqlalchemy.orm import scoped_session\nfrom app.models import LeftOnly\n\n{body}\ndef write_{file}():\n    Session.add(LeftOnly())\n"
            ),
        );
    }
    write(
        dir.path(),
        "app/twice.py",
        r#"
from app.db import Session
from app.models import LeftOnly

Twice = Session
Twice = fake

def neg_rebound_alias():
    Twice.add(LeftOnly())
"#,
    );
    write(
        dir.path(),
        "app/shadowed_factory.py",
        r#"
from sqlalchemy.orm import scoped_session
from app.models import LeftOnly

scoped_session = fake_factory
Session = scoped_session(None)

def neg_shadowed_factory():
    Session.add(LeftOnly())
"#,
    );
    write(
        dir.path(),
        "app/def_factory.py",
        r#"
from sqlalchemy.orm import sessionmaker
from app.models import LeftOnly

def sessionmaker():
    return fake

Session = sessionmaker()()

def neg_def_factory():
    Session.add(LeftOnly())
"#,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let short = |s: &str| s.rsplit_once('#').map_or(s, |(_, t)| t).to_string();
    let entity = |s: &str| s.rsplit_once(':').unwrap().1.to_string();
    let mut writes: Vec<(String, String, String)> = out
        .edges
        .iter()
        .filter(|e| e.label == "WRITES")
        .map(|e| {
            (
                short(&e.src),
                entity(&e.dst),
                e.props["resolution"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    writes.sort();
    let w = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
    assert_eq!(
        writes,
        vec![
            w("pos_alias", "left_only", "session"),
            w("pos_decorated", "decorated", "model-helper"),
            w("pos_inherited", "left_only", "model-helper"),
            w("pos_unshadowed", "left_only", "session"),
            w("write_comprehension_control", "left_only", "session"),
        ]
    );
    let inherited = out
        .edges
        .iter()
        .find(|e| e.label == "WRITES" && e.src.ends_with("#pos_inherited"))
        .unwrap();
    assert_eq!(inherited.props["via"], "Root.create");
}

#[test]
fn sqlalchemy_writes_cover_sessionmaker_flask_db_session_and_direct_variants() {
    // #1066 (W11d): `sessionmaker(...)()`, `db.session.add(...)` on a proven
    // Flask-SQLAlchemy instance, `S.add(M(...))`, `S.delete(x)`, `S.add_all([...])`,
    // and `S.query(M)...update(...)` emit Confirmed WRITES edges, while cyclic
    // or shadowed bases/sessions fail closed.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/ext.py",
        r#"
from flask_sqlalchemy import SQLAlchemy
from sqlalchemy.orm.session import sessionmaker

db = SQLAlchemy()
DirectSession = sessionmaker(bind=engine)()
"#,
    );
    write(
        dir.path(),
        "app/models.py",
        r#"
from sqlalchemy import Column, Integer
from app.ext import db, DirectSession

class CycleA(CycleB):
    pass

class CycleB(CycleA):
    pass

class Item(db.Model, CycleA):
    __tablename__ = "items"
    id = Column(Integer, primary_key=True)

class Audit(db.Model):
    __tablename__ = "audits"
    id = Column(Integer, primary_key=True)
"#,
    );
    write(
        dir.path(),
        "app/service.py",
        r#"
from app.ext import db, DirectSession
from app.models import Audit, Item

def inline_add():
    db.session.add(Item())

def delete_bound():
    item = Item()
    DirectSession.delete(item)

def query_update():
    DirectSession.query(Item).filter(Item.id == 1).update({"id": 2})

def batch_add():
    db.session.add_all([Item(), Audit()])

def cyclic_helper():
    Item.missing_helper()
"#,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut writes: Vec<(String, String, String)> = out
        .edges
        .iter()
        .filter(|e| e.label == "WRITES")
        .map(|e| {
            assert_eq!(e.props["resolution"], "session");
            assert_eq!(e.props["prov"]["confidence_tier"], "Confirmed");
            (
                e.src.rsplit_once('#').unwrap().1.to_string(),
                e.dst.rsplit_once(':').unwrap().1.to_string(),
                e.props["operation"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    writes.sort();
    assert_eq!(
        writes,
        vec![
            ("batch_add".into(), "audits".into(), "insert".into()),
            ("batch_add".into(), "items".into(), "insert".into()),
            ("delete_bound".into(), "items".into(), "delete".into()),
            ("inline_add".into(), "items".into(), "insert".into()),
            ("query_update".into(), "items".into(), "update".into()),
        ]
    );
}
