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
    // The calls are guarded effects (#1998); their interpretation Gaps are
    // rule evidence, not unresolved calls.
    assert!(
        extraction
            .nodes
            .iter()
            .all(|node| node.label != "Gap" || node.props["rule_evidence_gap"] == true)
    );
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
    // each key under the status of its return (AC-0902, T-0902),
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
            "keys_by_status": {"200": ["api_key", "email"], "400": ["error"], "403": ["error"], "410": ["ok", "reason"]},
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
fn flask_text_bodies_and_made_responses_are_proven() {
    // AC-0399 (#1182): a returned `str` is a `text` response, and
    // `make_response(body[, status])` is read as `(body, status)`, directly
    // or through a local that is only returned or given cookies or headers.
    let source = r#"
from flask import Flask, jsonify, make_response, render_template

app = Flask(__name__)

@app.route("/text")
def text():
    if bad():
        return "redirect_uri must be set", 400
    if odd():
        return f"only {FLOWS} are supported", 400
    return ("plain " "text")

@app.route("/made")
def made():
    if created():
        return make_response(jsonify(ok=True), 201)
    return make_response(render_template("page.html"))

@app.route("/local", methods=["POST"])
def local():
    response = make_response(jsonify(msg="User is logged out"), 200)
    response.delete_cookie("session")
    response.set_cookie("seen", "1")
    response.headers["X-Frame-Options"] = "DENY"
    return response

@app.route("/override")
def override():
    made = make_response("accepted", 201)
    return made, 202

@app.route("/status")
def status():
    response = make_response("x")
    response.status_code = 500
    return response

@app.route("/passed")
def passed():
    response = make_response("x")
    log(response)
    return response

@app.route("/twice")
def twice():
    response = make_response("a")
    response = make_response("b")
    return response

@app.route("/keyword")
def keyword():
    return make_response(response="x")

@app.route("/rebound")
def rebound():
    make_response = lambda body: body
    return make_response(jsonify(ok=True))

@app.route("/named")
def named():
    response = make_response("x")
    audit(response=meta.response)
    return response

@app.route("/bytes")
def raw():
    return b"payload"

def install(make_response):
    @app.route("/closure")
    def closure():
        return make_response("claimed", 201)

    @app.route("/closure-local")
    def closure_local():
        response = make_response("claimed", 201)
        return response

def install_page(render_template):
    @app.route("/closure-page")
    def closure_page():
        return render_template("page.html")

def unrelated(other):
    @app.route("/outer")
    def outer():
        return make_response("ok", 201)
"#;
    let extraction = extract_source(source.as_bytes(), "app.py", &id()).unwrap();
    let (responses, cited) = responses_cited(&extraction, source, "GET", "/text");
    assert_eq!(
        responses,
        serde_json::json!({ "kinds": ["text"], "statuses": [200, 400], "scope": "handler_body" })
    );
    assert_eq!(cited.len(), 3);
    let (responses, _) = responses_cited(&extraction, source, "GET", "/made");
    assert_eq!(
        responses,
        serde_json::json!({
            "kinds": ["html", "json"],
            "statuses": [200, 201],
            "keys": ["ok"],
            "keys_by_status": {"201": ["ok"]},
            "scope": "handler_body",
        })
    );
    let (responses, cited) = responses_cited(&extraction, source, "POST", "/local");
    assert_eq!(
        responses,
        serde_json::json!({
            "kinds": ["json"],
            "statuses": [200],
            "keys": ["msg"],
            "keys_by_status": {"200": ["msg"]},
            "scope": "handler_body",
        })
    );
    assert_eq!(cited, ["return response"]);
    let (responses, _) = responses_cited(&extraction, source, "GET", "/override");
    assert_eq!(
        responses,
        serde_json::json!({ "kinds": ["text"], "statuses": [202], "scope": "handler_body" })
    );
    // A keyword or member name spelled like the local is not a read of it.
    let (responses, _) = responses_cited(&extraction, source, "GET", "/named");
    assert_eq!(
        responses,
        serde_json::json!({ "kinds": ["text"], "statuses": [200], "scope": "handler_body" })
    );
    // An enclosing parameter named `make_response` is not Flask's; an
    // unrelated one leaves the import in force.
    let (responses, _) = responses_cited(&extraction, source, "GET", "/outer");
    assert_eq!(
        responses,
        serde_json::json!({ "kinds": ["text"], "statuses": [201], "scope": "handler_body" })
    );
    for path in [
        "/status",
        "/passed",
        "/twice",
        "/keyword",
        "/rebound",
        "/bytes",
        "/closure",
        "/closure-local",
        "/closure-page",
    ] {
        let (responses, _) = responses_cited(&extraction, source, "GET", path);
        assert_eq!(
            responses,
            serde_json::json!({ "partial": ["response_not_proven"], "scope": "handler_body" }),
            "{path}"
        );
    }
}

#[test]
fn flask_handlers_follow_returned_helpers_that_cannot_return_none() {
    // AC-0407 (#1187): a handler's `return helper(…)` adds the repository
    // function's responses when it cannot return `None`; the builtin
    // `str(…)` is a `text` body.
    let dir = tempfile::tempdir().unwrap();
    let utils = r#"
from flask import make_response, redirect, url_for

def after_login(user, next_url):
    if user.mfa:
        return redirect(url_for("auth.mfa"))
    # the dashboard unless the user came from another page
    if next_url:
        return redirect(next_url)
    elif user.new:
        return redirect(url_for("onboarding"), code=303)
    else:
        return redirect(url_for("dashboard.index"))

def maybe(user):
    if user:
        return redirect("/")
    return None

def falls(user):
    if user:
        return redirect("/")

def bare(user):
    if user:
        return
    return redirect("/")

def chained(user):
    return maybe(user)

def chained_ok(user):
    return after_login(user, None)

def streamed(user):
    yield "chunk"
    return {"done": True}

async def later(user):
    return redirect("/")

def mixed(user):
    if user:
        return redirect("/")
    return falls(user)

def export(user):
    output = make_response("csv")
    output.headers["Content-Disposition"] = "attachment"
    return output
"#;
    let views = r#"
from flask import Blueprint, render_template
from app.plain import message
from app.utils import after_login, bare, chained, chained_ok, export, falls, later, maybe, mixed, streamed

bp = Blueprint("auth", __name__)

@bp.route("/login")
def login():
    if form.ok():
        return after_login(user, next_url)
    return render_template("login.html")

@bp.route("/chained-ok")
def chained_ok_view():
    return chained_ok(user)

@bp.route("/export")
def export_view():
    return export(user)

@bp.route("/sms")
def sms():
    resp = build()
    return str(resp)

@bp.route("/maybe")
def maybe_view():
    return maybe(user)

@bp.route("/falls")
def falls_view():
    return falls(user)

@bp.route("/bare")
def bare_view():
    return bare(user)

@bp.route("/chained")
def chained_view():
    return chained(user)

@bp.route("/plain")
def plain():
    return message()

@bp.route("/mixed")
def mixed_view():
    return mixed(user)

@bp.route("/streamed")
def streamed_view():
    return streamed(user)

@bp.route("/later")
def later_view():
    return later(user)

@bp.route("/shadow")
def shadow():
    str = lambda value: value
    return str(1)
"#;
    // Neither module's `str` is the builtin.
    let assigned = r#"
from flask import Blueprint

shadow_bp = Blueprint("assigned", __name__)
str = lambda value: {"actual": value}

@shadow_bp.route("/assigned")
def assigned():
    return str(1)
"#;
    let classed = r#"
from flask import Blueprint

class_bp = Blueprint("classed", __name__)

class str:
    pass

@class_bp.route("/classed")
def classed():
    return str(1)
"#;
    write(dir.path(), "app/assigned.py", assigned);
    write(dir.path(), "app/classed.py", classed);
    // A helper need not import Flask for its body to be read.
    let plain = "def message():\n    return \"ok\"\n";
    write(dir.path(), "app/plain.py", plain);
    write(dir.path(), "app/utils.py", utils);
    write(dir.path(), "app/views.py", views);
    write(
        dir.path(),
        "app/main.py",
        "from flask import Flask\nfrom app.assigned import shadow_bp\nfrom app.classed import class_bp\nfrom app.views import bp\napp = Flask(__name__)\napp.register_blueprint(bp)\napp.register_blueprint(shadow_bp)\napp.register_blueprint(class_bp)\n",
    );
    let sources = [
        ("app/utils.py", utils),
        ("app/views.py", views),
        ("app/plain.py", plain),
    ];
    let mut cache = IncrementalCache::default();
    let (extraction, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    let responses = |path: &str| {
        let mut responses = extraction
            .nodes
            .iter()
            .find(|node| node.label == "Endpoint" && node.props["path"] == path)
            .unwrap_or_else(|| panic!("{path}"))
            .props["responses"]
            .clone();
        // A response with nothing proven carries no provenance.
        let prov = responses.as_object_mut().unwrap().remove("prov");
        let cited = prov
            .iter()
            .flat_map(|prov| prov["evidence"].as_array().unwrap())
            .map(|span| {
                let (_, source) = sources
                    .iter()
                    .find(|(file, _)| span["path"] == *file)
                    .unwrap();
                source[span["byte_start"].as_u64().unwrap() as usize
                    ..span["byte_end"].as_u64().unwrap() as usize]
                    .to_string()
            })
            .collect::<Vec<_>>();
        (responses, cited)
    };
    let (login, cited) = responses("/login");
    assert_eq!(
        login,
        serde_json::json!({
            "kinds": ["html", "redirect"],
            "statuses": [200, 302, 303],
            "followed": ["sym:local/python-service@app/utils.py#after_login"],
            "scope": "handler_body_and_followed",
        })
    );
    assert!(cited.contains(&"return after_login(user, next_url)".to_string()));
    assert_eq!(cited.len(), 6, "{cited:?}");
    let (chained_ok, _) = responses("/chained-ok");
    assert_eq!(
        chained_ok,
        serde_json::json!({
            "kinds": ["redirect"],
            "statuses": [302, 303],
            "followed": [
                "sym:local/python-service@app/utils.py#after_login",
                "sym:local/python-service@app/utils.py#chained_ok",
            ],
            "scope": "handler_body_and_followed",
        })
    );
    let (export, _) = responses("/export");
    assert_eq!(
        export,
        serde_json::json!({
            "kinds": ["text"],
            "statuses": [200],
            "followed": ["sym:local/python-service@app/utils.py#export"],
            "scope": "handler_body_and_followed",
        })
    );
    let (sms, cited) = responses("/sms");
    assert_eq!(
        sms,
        serde_json::json!({ "kinds": ["text"], "statuses": [200], "scope": "handler_body" })
    );
    assert_eq!(cited, ["return str(resp)"]);
    let (plain, _) = responses("/plain");
    assert_eq!(
        plain,
        serde_json::json!({
            "kinds": ["text"],
            "statuses": [200],
            "followed": ["sym:local/python-service@app/plain.py#message"],
            "scope": "handler_body_and_followed",
        })
    );
    // A helper that can return `None` (explicitly, bare, by falling off its
    // end, or through any check it follows, however deep) is not proven, and
    // none of its responses are taken; nor is a rebound `str`.
    for path in [
        "/maybe",
        "/falls",
        "/bare",
        "/chained",
        "/mixed",
        "/streamed",
        "/later",
        "/shadow",
        "/assigned",
        "/classed",
    ] {
        let (responses, _) = responses(path);
        assert_eq!(
            responses["partial"],
            serde_json::json!(["response_not_proven"]),
            "{path}"
        );
        assert!(responses.get("kinds").is_none(), "{path}");
    }
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
            "keys_by_status": {"200": ["ok"]},
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

@api_bp.route("/mixed")
def mixed():
    return jsonify(alias="x", **payload(user), **{"extra": 1}), 201

@api_bp.route("/mixed_loose")
def mixed_loose():
    return jsonify(a=1, **loose())
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
            "keys_by_status": {"200": ["mfa_key", "name"], "401": ["error"], "440": ["error"]},
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
            "keys_by_status": {"200": ["a", "b"]},
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
    // #1998: keywords beside `**helper(…)` and `**{…}` splats are read with
    // the helper's built keys; an unproven helper splat is built elsewhere.
    let (mixed, _) = responses(&first, "/api/mixed");
    assert_eq!(
        mixed["keys_by_status"],
        serde_json::json!({"201": ["alias", "extra", "mfa_key", "name"]})
    );
    assert!(mixed.get("partial").is_none());
    let (loose, _) = responses(&first, "/api/mixed_loose");
    assert_eq!(
        loose["partial"],
        serde_json::json!(["response_built_elsewhere"])
    );
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
            "default_walrus_session",
            "Session = scoped_session(None)\ndef reset(x=(Session := object())):\n    pass\n",
        ),
        (
            "lambda_default_walrus_session",
            "Session = scoped_session(None)\nreset = lambda x=(Session := object()): x\n",
        ),
        (
            "class_base_walrus_session",
            "Session = scoped_session(None)\nclass Holder((Session := object)):\n    pass\n",
        ),
        (
            "body_local_control",
            "Session = scoped_session(None)\ndef local():\n    Session = 1\n    return Session\n",
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
            w("write_body_local_control", "left_only", "session"),
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

fn auth_at(extraction: &Extraction, path: &str) -> serde_json::Value {
    extraction
        .nodes
        .iter()
        .find(|node| node.label == "Endpoint" && node.props["path"] == path)
        .unwrap_or_else(|| panic!("{path}"))
        .props["auth"]
        .clone()
}

#[test]
fn flask_login_required_is_a_confirmed_guard() {
    // AC-0380 (T-0380): `@login_required` imported from flask_login is a
    // framework guard with Confirmed provenance.
    let source = br#"
import flask_login
from flask import Flask, jsonify
from flask_login import login_required
from flask_login.utils import fresh_login_required
from flask_jwt_extended import jwt_required

app = Flask(__name__)

@app.route("/me")
@login_required
def me():
    return jsonify(ok=True)

@app.route("/attr")
@flask_login.login_required
def attr():
    return jsonify(ok=True)

@app.route("/fresh")
@fresh_login_required
def fresh():
    return jsonify(ok=True)

@app.route("/jwt")
@jwt_required()
def jwt():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/me");
    assert_eq!(auth["state"], "required");
    assert_eq!(auth["authenticated"], true);
    assert_eq!(
        auth["guards"],
        serde_json::json!(["flask_login.login_required"])
    );
    assert_eq!(auth["basis"], serde_json::json!(["framework_guard"]));
    assert_eq!(auth["prov"]["confidence_tier"], "Confirmed");
    assert!(!auth["prov"]["evidence"].as_array().unwrap().is_empty());
    assert_eq!(
        auth_at(&extraction, "/attr")["guards"],
        serde_json::json!(["flask_login.login_required"])
    );
    assert_eq!(
        auth_at(&extraction, "/fresh")["guards"],
        serde_json::json!(["flask_login.fresh_login_required"])
    );
    let jwt = auth_at(&extraction, "/jwt");
    assert_eq!(jwt["state"], "required");
    assert_eq!(
        jwt["guards"],
        serde_json::json!(["flask_jwt_extended.jwt_required"])
    );
    assert_eq!(jwt["prov"]["confidence_tier"], "Confirmed");
}

#[test]
fn repository_decorator_returning_401_is_an_inferred_guard() {
    // AC-0380 (T-0380): a cross-file repository decorator whose wrapper
    // delegates to a check returning 401 is an InferredStrong guard.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/base.py",
        r#"
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
        return f(*args, **kwargs)
    return decorated
"#,
    );
    write(
        dir.path(),
        "app/views.py",
        r#"
from flask import jsonify
from app.base import api_bp, require_auth

@api_bp.route("/me")
@require_auth
def me():
    return jsonify(ok=True)
"#,
    );
    write(
        dir.path(),
        "app/main.py",
        "from flask import Flask\nfrom app.base import api_bp\napp = Flask(__name__)\napp.register_blueprint(api_bp)\n",
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let auth = auth_at(&extraction, "/api/me");
    assert_eq!(auth["state"], "required");
    assert_eq!(auth["authenticated"], true);
    assert_eq!(
        auth["guards"],
        serde_json::json!(["sym:local/python-service@app/base.py#require_auth"])
    );
    assert_eq!(
        auth["basis"],
        serde_json::json!(["decorator_returns_401_or_403"])
    );
    assert_eq!(auth["prov"]["confidence_tier"], "InferredStrong");
}

/// A blueprint route `/api/me` decorated with the repository decorator
/// `guard`, defined in `app/base.py` after `prelude`.
fn guarded_by(prelude: &str, guard: &str) -> serde_json::Value {
    guarded_with(prelude, guard, &[])
}

/// `guarded_by` with `extra` repository files.
fn guarded_with(prelude: &str, guard: &str, extra: &[(&str, &str)]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    for (path, source) in extra {
        write(dir.path(), path, source);
    }
    write(
        dir.path(),
        "app/base.py",
        &format!(
            "from functools import wraps\nfrom flask import Blueprint\n{prelude}\n\napi_bp = Blueprint(\"api\", __name__, url_prefix=\"/api\")\n\n{guard}\n"
        ),
    );
    write(
        dir.path(),
        "app/views.py",
        "from flask import jsonify\nfrom app.base import api_bp, guard\n\n@api_bp.route(\"/me\")\n@guard\ndef me():\n    return jsonify(ok=True)\n",
    );
    write(
        dir.path(),
        "app/main.py",
        "from flask import Flask\nfrom app.base import api_bp\napp = Flask(__name__)\napp.register_blueprint(api_bp)\n",
    );
    auth_at(&extract_dir(dir.path(), &id()).unwrap(), "/api/me")
}

#[test]
fn repository_decorator_aborting_401_or_403_is_a_guard() {
    // AC-0395 (T-0395): a wrapper, or a check it returns the result of,
    // that calls Flask's `abort` with a literal 401 or 403 is a guard.
    let cases = [
        (
            "from flask import abort",
            "def guard(f):\n    @wraps(f)\n    def decorated(*args, **kwargs):\n        if valid():\n            return f(*args, **kwargs)\n        else:\n            return abort(403)\n    return decorated",
        ),
        (
            "from flask import abort",
            "def guard(f):\n    def decorated(*args, **kwargs):\n        if not allowed():\n            abort(401, description=\"login\")\n        return f(*args, **kwargs)\n    return decorated",
        ),
        (
            "import flask",
            "def guard(f):\n    def decorated(*args, **kwargs):\n        if not allowed():\n            flask.abort(403)\n        return f(*args, **kwargs)\n    return decorated",
        ),
        (
            "from flask import abort",
            "def check():\n    if missing():\n        abort(401)\n    return None\n\ndef guard(f):\n    def decorated(*args, **kwargs):\n        error = check()\n        if error:\n            return error\n        return f(*args, **kwargs)\n    return decorated",
        ),
        // A check whose result the wrapper returns directly.
        (
            "from flask import abort",
            "def check(f):\n    if missing():\n        abort(403)\n    return f()\n\ndef guard(f):\n    def decorated(*args, **kwargs):\n        return check(f)\n    return decorated",
        ),
    ];
    for (prelude, guard) in cases {
        let auth = guarded_by(prelude, guard);
        assert_eq!(auth["state"], "required", "{guard}: {auth}");
        assert_eq!(
            auth["basis"],
            serde_json::json!(["decorator_aborts_401_or_403"]),
            "{guard}"
        );
        assert_eq!(
            auth["guards"],
            serde_json::json!(["sym:local/python-service@app/base.py#guard"])
        );
        assert_eq!(auth["prov"]["confidence_tier"], "InferredStrong");
    }
    // A returned denial and an abort are both recorded.
    let auth = guarded_by(
        "from flask import abort, jsonify",
        "def guard(f):\n    def decorated(*args, **kwargs):\n        if missing():\n            return jsonify(error=\"key\"), 401\n        if banned():\n            abort(403)\n        return f(*args, **kwargs)\n    return decorated",
    );
    assert_eq!(
        auth["basis"],
        serde_json::json!([
            "decorator_aborts_401_or_403",
            "decorator_returns_401_or_403"
        ])
    );
    // Writes to another member of `flask` leave the proof standing.
    let auth = guarded_with(
        "import flask",
        "def guard(f):\n    def decorated(*args, **kwargs):\n        if not allowed():\n            flask.abort(403)\n        return f(*args, **kwargs)\n    return decorated",
        &[("app/patch.py", "import flask\n\nflask.VERSION = \"x\"\n")],
    );
    assert_eq!(auth["state"], "required", "{auth}");
}

#[test]
fn an_unproven_abort_in_a_repository_decorator_is_not_a_guard() {
    // AC-0395 (T-0395): every form short of a literal 401/403 Flask `abort`
    // in the wrapper's own scope leaves the decorator not classified.
    let wrap = |body: &str| {
        format!(
            "def guard(f):\n    def decorated(*args, **kwargs):\n{body}\n        return f(*args, **kwargs)\n    return decorated"
        )
    };
    let cases = [
        // A computed or a non-auth status.
        ("from flask import abort", wrap("        abort(status())")),
        ("from flask import abort", wrap("        abort(404)")),
        // Not Flask's `abort`.
        ("from mylib import abort", wrap("        abort(403)")),
        // Rebound in the wrapper or at module level.
        (
            "from flask import abort",
            wrap("        abort = lenient\n        abort(403)"),
        ),
        (
            "from flask import abort\nabort = lenient",
            wrap("        abort(403)"),
        ),
        // Only inside a nested function or lambda that may never run.
        (
            "from flask import abort",
            wrap("        def deny():\n            abort(403)"),
        ),
        (
            "from flask import abort",
            wrap("        deny = lambda: abort(403)"),
        ),
        // Bound by the decorator around the wrapper.
        (
            "from flask import abort",
            "def guard(f):\n    abort = lambda status: None\n    def decorated(*args, **kwargs):\n        abort(403)\n        return f(*args, **kwargs)\n    return decorated".to_string(),
        ),
        // A check the decorator rebinds is not the module's check, whether
        // returned directly or through a local.
        (
            "from flask import abort",
            "def check(fn):\n    abort(403)\n\ndef guard(f):\n    check = lambda fn: fn()\n    def decorated(*args, **kwargs):\n        return check(f)\n    return decorated".to_string(),
        ),
        (
            "from flask import abort",
            "def check():\n    abort(403)\n\ndef guard(f, check=lambda: None):\n    def decorated(*args, **kwargs):\n        error = check()\n        if error:\n            return error\n        return f(*args, **kwargs)\n    return decorated".to_string(),
        ),
        // `flask.abort` replaced at module level or in the wrapper.
        (
            "import flask\nflask.abort = lambda status: None",
            "def guard(f):\n    def decorated(*args, **kwargs):\n        flask.abort(403)\n        return f(*args, **kwargs)\n    return decorated".to_string(),
        ),
        (
            "import flask",
            "def guard(f):\n    def decorated(*args, **kwargs):\n        flask.abort = print\n        flask.abort(403)\n        return f(*args, **kwargs)\n    return decorated".to_string(),
        ),
    ];
    for (prelude, guard) in cases {
        let auth = guarded_by(prelude, &guard);
        assert_eq!(auth["state"], "unknown", "{prelude} {guard}: {auth}");
    }
    // Replaced from another file, directly or through `setattr`.
    for patch in [
        "import flask\n\nflask.abort = lambda status: None\n",
        "import flask\n\nsetattr(flask, \"abort\", print)\n",
    ] {
        let auth = guarded_with(
            "from flask import abort",
            "def guard(f):\n    def decorated(*args, **kwargs):\n        abort(403)\n        return f(*args, **kwargs)\n    return decorated",
            &[("app/patch.py", patch)],
        );
        assert_eq!(auth["state"], "unknown", "{patch}: {auth}");
    }
}

#[test]
fn undecorated_route_with_non_blocking_hooks_is_unauthenticated() {
    // AC-0380 (T-0380): a route with no guard decorator, a non-blocking
    // `before_request` hook (following a repository helper), and only a
    // ProxyFix `wsgi_app` is `none`, with the recorded assumption.
    let source = br#"
import time
from flask import Flask, g, jsonify
from werkzeug.middleware.proxy_fix import ProxyFix

app = Flask(__name__)
app.wsgi_app = ProxyFix(app.wsgi_app)

def stamp():
    g.y = time.time()
    return None

@app.before_request
def before():
    g.x = time.time()
    stamp()

@app.route("/open")
def open_view():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/open");
    assert_eq!(auth["state"], "none");
    assert_eq!(auth["authenticated"], false);
    assert_eq!(auth["basis"], "no_guard_decorator_or_blocking_request_hook");
    assert_eq!(
        auth["assumptions"],
        serde_json::json!(["non_repository_calls_in_request_hooks_do_not_abort"])
    );
    assert_eq!(auth["prov"]["confidence_tier"], "InferredStrong");
    assert!(auth["prov"]["evidence"].as_array().unwrap().len() >= 2);
}

#[test]
fn blocking_hook_or_unknown_middleware_makes_undecorated_routes_unknown() {
    // AC-0380 (T-0380): a hook that aborts, or an unknown `wsgi_app`
    // middleware, leaves undecorated routes unknown; a guarded route in
    // the same app stays required.
    let blocking = br#"
from flask import Flask, abort, jsonify
from flask_login import login_required

app = Flask(__name__)

@app.before_request
def before():
    abort(401)

@app.route("/open")
def open_view():
    return jsonify(ok=True)

@app.route("/me")
@login_required
def me():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(blocking, "app.py", &id()).unwrap();
    let open = auth_at(&extraction, "/open");
    assert_eq!(open["state"], "unknown");
    assert_eq!(
        open["reasons"],
        serde_json::json!(["request_hook_may_block"])
    );
    assert!(open.get("authenticated").is_none());
    assert_eq!(auth_at(&extraction, "/me")["state"], "required");

    let middleware = br#"
from flask import Flask, jsonify
from flask_login import login_required

app = Flask(__name__)
app.wsgi_app = Custom(app.wsgi_app)

@app.route("/open")
def open_view():
    return jsonify(ok=True)

@app.route("/me")
@login_required
def me():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(middleware, "app.py", &id()).unwrap();
    let open = auth_at(&extraction, "/open");
    assert_eq!(open["state"], "unknown");
    assert_eq!(
        open["reasons"],
        serde_json::json!(["wsgi_middleware_unknown"])
    );
    assert!(open.get("authenticated").is_none());
    assert_eq!(auth_at(&extraction, "/me")["state"], "required");
}

#[test]
fn a_hook_returning_only_unavailable_statuses_does_not_authenticate() {
    // AC-0448 (T-0448): a maintenance hook that ends requests only with a
    // literal 429 / 5xx `(body, status)` pair leaves undecorated routes
    // `none`, with the assumption recorded; any other value-return blocks.
    let source = |returns: &str| {
        format!(
            r#"
from flask import Flask, jsonify, redirect, render_template, request

app = Flask(__name__)

@app.before_request
def maintenance():
    if request.path.startswith("/health"):
        return
{returns}

@app.route("/health")
def health():
    return "success", 200
"#
        )
    };
    let auth = |returns: &str| {
        let extraction = extract_source(source(returns).as_bytes(), "app.py", &id()).unwrap();
        auth_at(&extraction, "/health")
    };
    for returns in [
        "    if request.path.startswith(\"/api/\"):\n        return jsonify(error=\"maintenance\"), 503\n    return render_template(\"503.html\"), 503",
        "    return (\"slow down\", 429)",
        "    return redirect(\"/login\"), 500",
    ] {
        let found = auth(returns);
        assert_eq!(found["state"], "none", "{returns}: {found}");
        assert_eq!(
            found["assumptions"],
            serde_json::json!([
                "non_repository_calls_in_request_hooks_do_not_abort",
                "request_hook_statuses_do_not_authenticate"
            ]),
            "{returns}"
        );
    }
    for returns in [
        "    return \"no\", 401",
        "    return \"no\", 403",
        "    return \"proxy\", 407",
        "    return \"sign in to the network\", 511",
        "    return \"missing\", 404",
        "    return redirect(\"/login\"), 302",
        "    return render_template(\"login.html\"), 200",
        "    return \"down\", 503, {\"WWW-Authenticate\": \"Basic\"}",
        "    return \"down\", STATUS",
        "    return render_template(\"503.html\")",
        "    if request.path.startswith(\"/api/\"):\n        return \"down\", 503\n    return redirect(\"/login\")",
    ] {
        let found = auth(returns);
        assert_eq!(found["state"], "unknown", "{returns}: {found}");
        assert_eq!(
            found["reasons"],
            serde_json::json!(["request_hook_may_block"]),
            "{returns}"
        );
    }
}

#[test]
fn an_unknown_fact_cites_each_hook_that_may_block() {
    // AC-0380 (T-0380): `blocking_hooks` names every registration whose
    // hook may block, by symbol (null for an unresolved argument) and the
    // registration's own span; a non-blocking hook is not listed.
    let source = br#"
from flask import Flask, abort, jsonify

app = Flask(__name__)

@app.before_request
def quiet():
    return None

@app.before_request
def gate():
    abort(503)

app.before_request(make_hook())

@app.route("/open")
def open_view():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let open = auth_at(&extraction, "/open");
    assert_eq!(open["state"], "unknown");
    let text = std::str::from_utf8(source).unwrap();
    let hooks: Vec<_> = open["blocking_hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hook| {
            let evidence = &hook["evidence"];
            let start = evidence["byte_start"].as_u64().unwrap() as usize;
            let end = evidence["byte_end"].as_u64().unwrap() as usize;
            assert_eq!(evidence["path"], "app.py");
            (hook["hook"].clone(), &text[start..end])
        })
        .collect();
    assert_eq!(
        hooks,
        [
            (
                serde_json::json!(crate::symbol_id(id().repo, "app.py", "gate")),
                "@app.before_request"
            ),
            (serde_json::Value::Null, "app.before_request(make_hook())"),
        ]
    );

    // With no blocking hook there is nothing to cite.
    let quiet = br#"
from flask import Flask, jsonify

app = Flask(__name__)
app.wsgi_app = Custom(app.wsgi_app)

@app.route("/open")
def open_view():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(quiet, "app.py", &id()).unwrap();
    assert!(
        auth_at(&extraction, "/open")
            .get("blocking_hooks")
            .is_none()
    );

    // A hook nested in a factory is cited by its own qualified Symbol, never
    // by a top-level function of the same name.
    let factory = br#"
from flask import Flask, abort, jsonify

def gate():
    return None

def create_app():
    app = Flask(__name__)

    @app.before_request
    def gate():
        abort(503)

    @app.route("/open")
    def open_view():
        return jsonify(ok=True)

    return app
"#;
    let extraction = extract_source(factory, "app.py", &id()).unwrap();
    let open = auth_at(&extraction, "/open");
    let text = std::str::from_utf8(factory).unwrap();
    let hooks = open["blocking_hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    assert_eq!(
        hooks[0]["hook"],
        crate::symbol_id(id().repo, "app.py", "create_app.gate")
    );
    let evidence = &hooks[0]["evidence"];
    let span = evidence["byte_start"].as_u64().unwrap() as usize
        ..evidence["byte_end"].as_u64().unwrap() as usize;
    assert_eq!(&text[span], "@app.before_request");
    assert!(open.get("blocking_hooks_omitted").is_none());

    // Past the evidence bound the rest are counted, not dropped silently.
    let mut many =
        String::from("from flask import Flask, abort, jsonify\n\napp = Flask(__name__)\n\n");
    for index in 0..66 {
        many.push_str(&format!(
            "@app.before_request\ndef gate_{index}():\n    abort(503)\n\n"
        ));
    }
    many.push_str("@app.route(\"/open\")\ndef open_view():\n    return jsonify(ok=True)\n");
    let extraction = extract_source(many.as_bytes(), "app.py", &id()).unwrap();
    let open = auth_at(&extraction, "/open");
    assert_eq!(open["blocking_hooks"].as_array().unwrap().len(), 64);
    assert_eq!(open["blocking_hooks_omitted"], 2);
}

#[test]
fn call_and_unresolved_decorators_are_unknown() {
    // AC-0380 (T-0380): a call decorator, a `jwt_required` call with an
    // argument, and a repository decorator neither proven a guard nor
    // proven not to authenticate (AC-0446) are unknown.
    let source = br#"
from flask import Flask, jsonify
from flask_jwt_extended import jwt_required

app = Flask(__name__)

def passthrough(f):
    def wrapper(*args, **kwargs):
        rv = f(*args, **kwargs)
        return rv
    return wrapper

@app.route("/limited")
@limiter.limit("2/minute")
def limited():
    return jsonify(ok=True)

@app.route("/optional")
@jwt_required(optional=True)
def optional():
    return jsonify(ok=True)

@app.route("/passed")
@passthrough
def passed():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let limited = auth_at(&extraction, "/limited");
    assert_eq!(limited["state"], "unknown");
    assert_eq!(
        limited["reasons"],
        serde_json::json!([r#"unclassified_decorator:limiter.limit("2/minute")"#])
    );
    let optional = auth_at(&extraction, "/optional");
    assert_eq!(optional["state"], "unknown");
    assert_eq!(
        optional["reasons"],
        serde_json::json!(["unclassified_decorator:jwt_required(optional=True)"])
    );
    let passed = auth_at(&extraction, "/passed");
    assert_eq!(passed["state"], "unknown");
    assert_eq!(
        passed["reasons"],
        serde_json::json!(["decorator_not_classified:sym:local/python-service@app.py#passthrough"])
    );
    for auth in [limited, optional, passed] {
        assert!(auth.get("authenticated").is_none());
        assert!(auth.get("prov").is_none());
    }
}

#[test]
fn decorators_above_or_between_route_decorators_do_not_guard_the_route() {
    // AC-0380 (T-0380): a guard above every route decorator is applied after
    // registration, so the registered view is unguarded: it is listed as
    // ineffective and the route is `none`. A decorator between two route
    // decorators wraps only one registration, so the state is `unknown`.
    let source = br#"
from flask import Flask, jsonify
from flask_login import login_required

app = Flask(__name__)

@login_required
@app.route("/late")
def late():
    return jsonify(ok=True)

@app.route("/a")
@login_required
@app.route("/b")
def split():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let late = auth_at(&extraction, "/late");
    assert_eq!(late["state"], "none");
    assert_eq!(late["authenticated"], false);
    assert_eq!(
        late["ineffective_decorators"],
        serde_json::json!(["@login_required"])
    );
    for path in ["/a", "/b"] {
        let split = auth_at(&extraction, path);
        assert_eq!(split["state"], "unknown", "{path}");
        assert_eq!(
            split["reasons"],
            serde_json::json!(["decorator_between_route_decorators:@login_required"]),
            "{path}"
        );
    }
}

#[test]
fn a_hooks_callee_returning_a_value_does_not_block() {
    // AC-0380 (T-0380): only the hook's own return value ends a request. A
    // repository function it calls may return a value; one that raises
    // still blocks, and so does a hook returning a callee's value.
    let open = br#"
from flask import Flask, g, jsonify

app = Flask(__name__)

def make_id():
    return "id"

@app.before_request
def tag():
    g.request_id = make_id()

@app.route("/open")
def open_view():
    return jsonify(ok=True)
"#;
    let extraction = extract_source(open, "app.py", &id()).unwrap();
    assert_eq!(auth_at(&extraction, "/open")["state"], "none");

    for (case, hook) in [
        (
            "callee raises",
            "def check():\n    raise PermissionError()\n\n@app.before_request\ndef tag():\n    check()\n",
        ),
        (
            "hook returns the callee's value",
            "def make():\n    return 'stop'\n\n@app.before_request\ndef tag():\n    return make()\n",
        ),
        (
            "generator hook",
            "def token():\n    return 'denied'\n\n@app.before_request\ndef tag():\n    yield token()\n",
        ),
    ] {
        let source = format!(
            "from flask import Flask, jsonify\n\napp = Flask(__name__)\n\n{hook}\n@app.route(\"/open\")\ndef open_view():\n    return jsonify(ok=True)\n"
        );
        let extraction = extract_source(source.as_bytes(), "app.py", &id()).unwrap();
        let auth = auth_at(&extraction, "/open");
        assert_eq!(auth["state"], "unknown", "{case}");
        assert_eq!(
            auth["reasons"],
            serde_json::json!(["request_hook_may_block"]),
            "{case}"
        );
    }
}

#[test]
fn a_hook_calling_an_unrecovered_repository_name_may_block() {
    // AC-0380 (T-0380): a hook's call into a repository file where no
    // function of that name was recovered (here a re-export through a
    // package `__init__`) is not assumed non-blocking, and two functions
    // sharing a key are merged conservatively.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/checks/core.py",
        "from flask import abort\n\ndef check():\n    abort(401)\n",
    );
    write(
        dir.path(),
        "app/checks/__init__.py",
        "from app.checks.core import check\n",
    );
    write(
        dir.path(),
        "app/main.py",
        r#"
from flask import Flask, jsonify
from app.checks import check

app = Flask(__name__)

@app.before_request
def gate():
    check()

@app.route("/open")
def open_view():
    return jsonify(ok=True)
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let auth = auth_at(&extraction, "/open");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["request_hook_may_block"])
    );

    let shared = br#"
from flask import Flask, abort, jsonify

def gate():
    return None

def create_app():
    app = Flask(__name__)

    @app.before_request
    def gate():
        abort(403)

    @app.route("/open")
    def open_view():
        return jsonify(ok=True)

    return app
"#;
    let extraction = extract_source(shared, "factory.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/open");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["request_hook_may_block"])
    );
}

#[test]
fn decorated_request_hooks_may_block() {
    // AC-0380 (T-0380): Flask registers the decorated hook, so a hook with
    // any decorator besides its registration may block even when its own
    // body is `pass`; the undecorated `pass` hook stays non-blocking.
    let decorated = br#"
from flask import Flask
from flask_login import login_required

app = Flask(__name__)

@app.before_request
@login_required
def gate():
    pass

@app.route("/private")
def private():
    return "secret"
"#;
    let extraction = extract_source(decorated, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/private");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["request_hook_may_block"])
    );

    let plain = br#"
from flask import Flask

app = Flask(__name__)

@app.before_request
def gate():
    pass

@app.route("/open")
def open_view():
    return "open"
"#;
    let extraction = extract_source(plain, "app.py", &id()).unwrap();
    assert_eq!(auth_at(&extraction, "/open")["state"], "none");
}

#[test]
fn an_enclosing_binding_shadows_an_imported_guard() {
    // AC-0380 (T-0380): a factory parameter named like the imported guard is
    // what the decorator resolves to, so it is not a Confirmed guard; the
    // same factory without the parameter keeps the import.
    let shadowed = br#"
from flask import Flask
from flask_login import login_required

app = Flask(__name__)

def register(login_required):
    @app.route("/public")
    @login_required
    def public():
        return "public"

register(lambda function: function)
"#;
    let extraction = extract_source(shadowed, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/public");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["unclassified_decorator:login_required"])
    );

    let imported = br#"
from flask import Flask
from flask_login import login_required

app = Flask(__name__)

def register():
    @app.route("/private")
    @login_required
    def private():
        return "private"

register()
"#;
    let extraction = extract_source(imported, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/private");
    assert_eq!(auth["state"], "required");
    assert_eq!(
        auth["guards"],
        serde_json::json!(["flask_login.login_required"])
    );
}

#[test]
fn a_reassigned_framework_guard_is_not_proof() {
    // AC-0380 (T-0380): a guard reassigned on its module anywhere in the
    // repository, by assignment or a literal `setattr`, may not be the guard.
    let assigned = br#"
import flask_login
from flask import Flask

app = Flask(__name__)
flask_login.login_required = lambda function: function

@app.route("/public")
@flask_login.login_required
def public():
    return "public"
"#;
    let extraction = extract_source(assigned, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/public");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["framework_guard_reassigned:flask_login.login_required"])
    );

    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/patch.py",
        "import flask_jwt_extended\n\nsetattr(flask_jwt_extended, \"jwt_required\", lambda: (lambda f: f))\n",
    );
    write(
        dir.path(),
        "app/views.py",
        r#"
from flask import Flask
from flask_jwt_extended import jwt_required

app = Flask(__name__)

@app.route("/token")
@jwt_required()
def token():
    return "token"
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let auth = auth_at(&extraction, "/token");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["framework_guard_reassigned:flask_jwt_extended.jwt_required"])
    );
    for (patch, reason) in [
        (
            "setattr(flask_login, (\"login_required\"), lambda f: f)",
            "framework_guard_reassigned:flask_login.login_required",
        ),
        (
            "setattr(flask_login, u\"login_required\", lambda f: f)",
            "framework_guard_reassigned:flask_login.login_required",
        ),
        (
            "setattr(flask_login, \"login\\x5frequired\", lambda f: f)",
            "framework_guard_reassigned:flask_login.login_required",
        ),
        (
            "setattr(flask_login, NAME, lambda f: f)",
            "framework_guard_reassigned:flask_login.login_required",
        ),
        ("setattr(flask_login, \"other\", 1)", ""),
    ] {
        let source = format!(
            "import flask_login\nfrom flask import Flask\n\napp = Flask(__name__)\n{patch}\n\n@app.route(\"/p\")\n@flask_login.login_required\ndef p():\n    return \"p\"\n"
        );
        let extraction = extract_source(source.as_bytes(), "app.py", &id()).unwrap();
        let auth = auth_at(&extraction, "/p");
        if reason.is_empty() {
            assert_eq!(auth["state"], "required", "{patch}");
        } else {
            assert_eq!(auth["state"], "unknown", "{patch}");
            assert_eq!(auth["reasons"], serde_json::json!([reason]), "{patch}");
        }
    }
}

#[test]
fn proxy_fix_is_known_only_around_the_assigned_application() {
    // AC-0380 (T-0380): `ProxyFix` wrapping other middleware leaves that
    // middleware unknown; wrapping the assigned `wsgi_app` itself is cited
    // on the `none` fact.
    let nested = br#"
from flask import Flask
from werkzeug.middleware.proxy_fix import ProxyFix

app = Flask(__name__)
app.wsgi_app = ProxyFix(AuthMiddleware(app.wsgi_app))

@app.route("/open")
def open_view():
    return "open"
"#;
    let extraction = extract_source(nested, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/open");
    assert_eq!(auth["state"], "unknown");
    assert_eq!(
        auth["reasons"],
        serde_json::json!(["wsgi_middleware_unknown"])
    );

    let direct = br#"
from flask import Flask
from werkzeug.middleware.proxy_fix import ProxyFix

app = Flask(__name__)
app.wsgi_app = ProxyFix(app.wsgi_app, x_for=1)

@app.route("/open")
def open_view():
    return "open"
"#;
    let extraction = extract_source(direct, "app.py", &id()).unwrap();
    let auth = auth_at(&extraction, "/open");
    assert_eq!(auth["state"], "none");
    let source = std::str::from_utf8(direct).unwrap();
    let start = source.find("app.wsgi_app = ProxyFix").unwrap() as u64;
    assert!(
        auth["prov"]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|span| span["byte_start"].as_u64() == Some(start)),
        "{auth}"
    );
}

/// Two factories, each building its own Flask application, as SimpleLogin
/// does: the maintenance app's blocking hook and the main app's routes.
fn two_app_tree(dir: &std::path::Path, main: &str) {
    write(
        dir,
        "maintenance.py",
        r#"from flask import Flask, abort

def create_maintenance_app():
    app = Flask(__name__)

    @app.before_request
    def gate():
        abort(503)

    @app.route("/health")
    def health():
        return "ok"

    return app
"#,
    );
    write(
        dir,
        "views.py",
        r#"from flask import Blueprint

bp = Blueprint("views", __name__)

@bp.route("/open")
def open_view():
    return "ok"
"#,
    );
    write(dir, "main.py", main);
    write(
        dir,
        "server.py",
        r#"import config

def create_app():
    if config.MAINTENANCE:
        from maintenance import create_maintenance_app
        return create_maintenance_app()
    from main import create_main_app
    return create_main_app()
"#,
    );
}

const MAIN_APP: &str = r#"from flask import Flask
from views import bp

def register(app):
    app.register_blueprint(bp)

def create_main_app():
    app = Flask(__name__)
    register(app)

    @app.route("/direct")
    def direct():
        return "ok"

    return app
"#;

#[test]
fn a_request_hook_reaches_only_its_own_application() {
    // AC-0383 (T-0383): a blocking hook on one function-local Flask app does
    // not make another proven app's endpoints `unknown`; a parameter app is
    // followed to every call of its function.
    let dir = tempfile::tempdir().unwrap();
    two_app_tree(dir.path(), MAIN_APP);
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    for path in ["/open", "/direct"] {
        let auth = auth_at(&extraction, path);
        assert_eq!(auth["state"], "none", "{path}: {auth}");
        assert_eq!(
            auth["assumptions"],
            serde_json::json!([
                "non_repository_calls_in_request_hooks_do_not_abort",
                "request_hooks_reach_only_their_application"
            ]),
            "{path}"
        );
    }
    let health = auth_at(&extraction, "/health");
    assert_eq!(health["state"], "unknown");
    assert_eq!(health["blocking_hooks"].as_array().unwrap().len(), 1);
    assert_eq!(
        health["blocking_hooks"][0]["hook"],
        crate::symbol_id(id().repo, "maintenance.py", "create_maintenance_app.gate")
    );
}

#[test]
fn a_request_hook_stays_global_whenever_its_application_may_escape() {
    // AC-0383 (T-0383): every way the main app may be the hook's app keeps
    // the hook on the main app's endpoints.
    let escapes = [
        // The registering function is used as a value, so it may be called
        // with any application.
        MAIN_APP.replace(
            "    return app\n",
            "    return app\n\nREGISTRARS = [register]\n",
        ),
        // A spread argument may be any application.
        MAIN_APP.replace(
            "    register(app)\n",
            "    register(app)\n    register(**OPTIONS)\n",
        ),
        // An omitted argument is the parameter's default.
        MAIN_APP
            .replace("def register(app):", "def register(app=None):")
            .replace("    register(app)\n", "    register(app)\n    register()\n"),
        // Renamed on import, the function is called by another name.
        format!("{MAIN_APP}\nfrom main import register as install\n"),
        // A decorated function is called through its wrapper.
        MAIN_APP.replace("def register(app):", "@trace\ndef register(app):"),
        // An unproven receiver may be the maintenance app.
        MAIN_APP.replace(
            "    register(app)\n",
            "    register(app)\n    register(current())\n",
        ),
    ];
    for main in escapes {
        let dir = tempfile::tempdir().unwrap();
        two_app_tree(dir.path(), &main);
        let extraction = extract_dir(dir.path(), &id()).unwrap();
        let auth = auth_at(&extraction, "/open");
        assert_eq!(auth["state"], "unknown", "{main}");
        assert!(
            auth["reasons"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("request_hook_may_block")),
            "{main}"
        );
        // The main app's own direct route is still proven apart.
        assert_eq!(auth_at(&extraction, "/direct")["state"], "none", "{main}");
    }

    // A Blueprint registration this tier cannot name may register any
    // Blueprint on the maintenance app.
    let dir = tempfile::tempdir().unwrap();
    two_app_tree(dir.path(), MAIN_APP);
    let maintenance = std::fs::read_to_string(dir.path().join("maintenance.py")).unwrap();
    write(
        dir.path(),
        "maintenance.py",
        &maintenance.replace(
            "    return app\n",
            "    for blueprint in BLUEPRINTS:\n        app.register_blueprint(blueprint)\n    return app\n",
        ),
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(auth_at(&extraction, "/open")["state"], "unknown");
    assert_eq!(auth_at(&extraction, "/direct")["state"], "none");

    // A hook and a route on one module-level application share it.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app.py",
        r#"from flask import Flask, abort

app = Flask(__name__)

@app.before_request
def gate():
    abort(503)
"#,
    );
    write(
        dir.path(),
        "views.py",
        r#"from app import app

@app.route("/open")
def open_view():
    return "ok"
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(auth_at(&extraction, "/open")["state"], "unknown");
}

#[test]
fn a_rebound_module_application_is_not_proven_apart() {
    // AC-0383 (T-0383): a module-level name proves an application only when
    // its `Flask()` assignment is the module's sole binding of it, and no
    // module assigns it through an import.
    let gate = r#"from flask import Flask, abort

a = Flask(__name__)
b = Flask(__name__)
b = a

@a.before_request
def gate():
    abort(503)

@b.route("/same")
def same():
    return "ok"
"#;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "apps.py", gate);
    write(
        dir.path(),
        "views.py",
        r#"from apps import b

@b.route("/imported")
def imported():
    return "ok"
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(auth_at(&extraction, "/same")["state"], "unknown");
    assert_eq!(auth_at(&extraction, "/imported")["state"], "unknown");

    // Without the rebinding the two applications are apart ...
    let apart = gate.replace("b = a\n", "");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "apps.py", &apart);
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(auth_at(&extraction, "/same")["state"], "none");

    // ... unless another module assigns the name through its import, or a
    // module that imports it rebinds it.
    for other in [
        "import apps\n\napps.b = apps.a\n",
        "import apps\n\napps.b, other = (apps.a, 1)\n",
        "import apps\n\n[(apps.b, other)] = [(apps.a, 1)]\n",
        "import apps\n\nfor apps.b in [apps.a]:\n    pass\n",
        "import apps\n\nwith apps.a as apps.b:\n    pass\n",
        "from flask import Flask\nfrom apps import a, b\n\nb = a\n\n@b.route(\"/rebound\")\ndef rebound():\n    return \"ok\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "apps.py", &apart);
        write(dir.path(), "other.py", other);
        let extraction = extract_dir(dir.path(), &id()).unwrap();
        if other.contains("apps.b") {
            assert_eq!(auth_at(&extraction, "/same")["state"], "unknown", "{other}");
        } else {
            assert_eq!(
                auth_at(&extraction, "/rebound")["state"],
                "unknown",
                "{other}"
            );
        }
    }
}

/// A Flask app whose `/x` route carries `decorator`, with a flask-limiter
/// `Limiter` in `extensions.py` (`extensions` replaces it when given).
fn decorated_route(
    decorator: &str,
    extensions: Option<&str>,
    extra: &[(&str, &str)],
) -> Extraction {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "extensions.py",
        extensions.unwrap_or(
            "from flask_limiter import Limiter\n\nlimiter = Limiter(key_func=lambda: \"k\")\n",
        ),
    );
    write(
        dir.path(),
        "views.py",
        &format!(
            r#"from flask import Flask, g
from flask_cors import cross_origin
from flask_login import login_required
from extensions import limiter

app = Flask(__name__)

@app.route("/x")
{decorator}
def x():
    return "ok"

@app.route("/me")
@login_required
@limiter.limit("1/second")
def me():
    return "ok"
"#
        ),
    );
    for (path, source) in extra {
        write(dir.path(), path, source);
    }
    extract_dir(dir.path(), &id()).unwrap()
}

#[test]
fn rate_limit_and_cors_decorators_do_not_authenticate() {
    // AC-0387 (T-0387): an import-proven flask-cors `cross_origin` and a
    // proven flask-limiter `Limiter`'s `limit` / `shared_limit` with inert
    // arguments neither guard nor make the state unknown.
    for decorator in [
        "@limiter.limit(\"10/minute\", deduct_when=lambda r: hasattr(g, \"deduct\") and g.deduct)",
        "@limiter.shared_limit(\"5/hour\", scope=\"auth\")",
        "@cross_origin()",
        "@cross_origin(origins=[\"https://example.com\"], supports_credentials=True)",
        "@cross_origin()\n@limiter.limit(\"3/hour\")",
    ] {
        let extraction = decorated_route(decorator, None, &[]);
        let auth = auth_at(&extraction, "/x");
        assert_eq!(auth["state"], "none", "{decorator}: {auth}");
        assert!(
            auth["assumptions"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(
                    "rate_limit_and_cors_decorators_do_not_authenticate"
                )),
            "{decorator}"
        );
        assert!(!auth["non_auth_decorators"].as_array().unwrap().is_empty());
        // A guard beside a limiter still requires authentication.
        assert_eq!(auth_at(&extraction, "/me")["state"], "required");
    } // Writes to other members leave the proof standing.
    let extraction = decorated_route(
        "@cross_origin()\n@limiter.limit(\"1/s\")",
        None,
        &[(
            "patch.py",
            "import flask_cors\nimport flask_limiter\nfrom extensions import limiter as other\n\nflask_cors.VERSION = \"x\"\nflask_limiter.RateLimitExceeded = Exception\nsettings.limit = 3\nconfig = {}\nconfig[flask_limiter.extension.C.STORAGE_URL] = \"memory://\"\n",
        )],
    );
    assert_eq!(auth_at(&extraction, "/x")["state"], "none");
}

/// A route decorator, an `extensions.py` override and extra files.
type DecoratedCase = (
    &'static str,
    Option<&'static str>,
    &'static [(&'static str, &'static str)],
);

#[test]
fn an_unproven_limiter_or_cors_decorator_stays_unknown() {
    // AC-0387 (T-0387): every form short of the proof stays
    // `unclassified_decorator`.
    let cases: [DecoratedCase; 20] = [
        // A call argument may do anything when the decorator is built.
        ("@limiter.limit(get_limit())", None, &[]),
        ("@limiter.limit(f\"{N}/minute\")", None, &[]),
        // A lambda calling a repository function may abort.
        (
            "@limiter.limit(\"1/s\", exempt_when=lambda: check())",
            None,
            &[],
        ),
        // `limit` on something that is not a flask-limiter `Limiter`.
        (
            "@limiter.limit(\"1/s\")",
            Some("from other import Thing\n\nlimiter = Thing()\n"),
            &[],
        ),
        // A rebound limiter name.
        (
            "@limiter.limit(\"1/s\")",
            Some(
                "from flask_limiter import Limiter\n\nlimiter = Limiter()\nlimiter = wrap(limiter)\n",
            ),
            &[],
        ),
        // Assigned through an imported module.
        (
            "@limiter.limit(\"1/s\")",
            None,
            &[(
                "patch.py",
                "import extensions\n\nextensions.limiter = other\n",
            )],
        ),
        // A function-local limiter proves nothing at module level.
        (
            "@limiter.limit(\"1/s\")",
            Some(
                "from flask_limiter import Limiter\n\ndef make():\n    limiter = Limiter()\n    return limiter\n\nlimiter = make()\n",
            ),
            &[],
        ),
        // A `cross_origin` that is not flask-cors's.
        ("@cors.cross_origin()", None, &[]),
        // A spread argument.
        ("@cross_origin(**OPTIONS)", None, &[]),
        // A builtin name a lambda binds itself may be anything.
        (
            "@limiter.limit(\"1/s\", exempt_when=lambda getattr=abort: getattr(401))",
            None,
            &[],
        ),
        (
            "@limiter.limit(\"1/s\", exempt_when=lambda: [getattr(1) for getattr in [abort]])",
            None,
            &[],
        ),
        (
            "@limiter.limit(\"1/s\", exempt_when=lambda: (getattr := abort) and getattr(401))",
            None,
            &[],
        ),
        // A rebound `Limiter` constructor.
        (
            "@limiter.limit(\"1/s\")",
            Some(
                "from flask_limiter import Limiter\n\nclass AuthLimiter:\n    pass\n\nLimiter = AuthLimiter\nlimiter = Limiter()\n",
            ),
            &[],
        ),
        // flask-limiter's `Limiter` or its decorator replaced on its module.
        (
            "@limiter.limit(\"1/s\")",
            None,
            &[(
                "patch.py",
                "import flask_limiter\n\nflask_limiter.Limiter = AuthLimiter\n",
            )],
        ),
        (
            "@limiter.limit(\"1/s\")",
            None,
            &[(
                "patch.py",
                "from flask_limiter import Limiter\n\nLimiter.limit = wrapper\n",
            )],
        ),
        // A method written on the limiter instance itself.
        (
            "@limiter.limit(\"1/s\")",
            None,
            &[(
                "patch.py",
                "from extensions import limiter\n\nlimiter.limit = wrapper\n",
            )],
        ),
        (
            "@limiter.limit(\"1/s\")",
            None,
            &[(
                "patch.py",
                "from extensions import limiter\n\nsetattr(limiter, \"limit\", wrapper)\n",
            )],
        ),
        // flask-cors's `cross_origin` replaced on its module, directly,
        // through a tuple target or through `setattr`.
        (
            "@cross_origin()",
            None,
            &[(
                "patch.py",
                "import flask_cors\n\nflask_cors.cross_origin = auth_factory\n",
            )],
        ),
        (
            "@cross_origin()",
            None,
            &[(
                "patch.py",
                "import flask_cors\n\nflask_cors.cross_origin, x = auth_factory, 1\n",
            )],
        ),
        (
            "@cross_origin()",
            None,
            &[(
                "patch.py",
                "import flask_cors\n\nsetattr(flask_cors, \"cross_origin\", auth_factory)\n",
            )],
        ),
    ];
    for (decorator, extensions, extra) in cases {
        let extraction = decorated_route(decorator, extensions, extra);
        let auth = auth_at(&extraction, "/x");
        assert_eq!(
            auth["state"], "unknown",
            "{decorator} {extensions:?} {extra:?}: {auth}"
        );
        assert!(
            auth["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason
                    .as_str()
                    .unwrap()
                    .starts_with("unclassified_decorator:")),
            "{decorator}"
        );
    }
}

#[test]
fn flask_admin_exposed_view_methods_become_flask_endpoints() {
    // AC-0410 (#1193): Flask-Admin `@expose` methods of registered views are
    // endpoints named `<endpoint>.<method>` under the view's URL; library
    // routes and anything unreadable are Gaps.
    let dir = tempfile::tempdir().unwrap();
    let base = r#"
from flask_admin import expose, AdminIndexView, BaseView
from flask_admin.contrib import sqla
from flask_login import current_user

class BaseAdminView(BaseView):
    def is_accessible(self):
        return current_user.is_admin

class SLModelView(sqla.ModelView, BaseAdminView):
    page_size = 100

class SLAdminIndexView(AdminIndexView):
    @expose("/")
    def index(self):
        return "home"
"#;
    let search = r#"
from flask import jsonify
from flask_admin import BaseView, expose
from app.admin.base import BaseAdminView

class EmailSearchAdmin(BaseAdminView):
    @expose("/", methods=["GET", "POST"])
    def index(self):
        return jsonify(ok=True)

    @expose("/delete_user", methods=["POST"])
    def delete_user(self):
        return "deleted"

    def helper(self):
        return 1

class ChildSearch(EmailSearchAdmin):
    # Redefined without `@expose`, so no longer routed.
    def delete_user(self):
        return "nope"

    @expose("extra")
    def extra(self):
        return "extra"

class OpenView(BaseView):
    @expose()
    def index(self):
        return "open"

class Custom(BaseAdminView):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)

    @expose("/")
    def index(self):
        return "custom"
"#;
    let complaint = r#"
from flask_admin import expose
from app.admin.base import SLModelView

class ProviderComplaintAdmin(SLModelView):
    @expose("/mark_ok", methods=["GET"])
    def mark_ok(self):
        return "ok"
"#;
    let index = r#"
from flask import Flask
from flask_admin import Admin
from app.models import ProviderComplaint, User
from app.admin.base import SLAdminIndexView, SLModelView
from app.admin.search import EmailSearchAdmin, ChildSearch, OpenView, Custom
from app.admin.complaint import ProviderComplaintAdmin

class Menu:
    def add_view(self, view):
        pass

def init_admin(app: Flask, endpoint):
    admin = Admin(name="SimpleLogin", template_mode="bootstrap4")
    admin.init_app(app, index_view=SLAdminIndexView())
    admin.add_view(EmailSearchAdmin(name="Email Search", endpoint="admin.email_search"))
    admin.add_view(ChildSearch(endpoint="child", url="/kids"))
    admin.add_view(OpenView(url="open"))
    admin.add_view(ProviderComplaintAdmin(ProviderComplaint, None))
    admin.add_view(SLModelView(User, None))
    admin.add_view(EmailSearchAdmin(endpoint=endpoint))
    admin.add_view(Custom())
    Menu().add_view(OpenView())
"#;
    let site = r#"
from flask import Flask
from flask_admin import Admin
from app.admin.search import OpenView

app = Flask(__name__)
admin = Admin(app, url="/backoffice")
admin.add_view(OpenView(endpoint="o2"))
"#;
    // A rebound `expose` is not Flask-Admin's.
    let fake = r#"
from flask_admin import Admin, BaseView, expose

def expose(url="/"):
    return lambda view: view

class Lookalike(BaseView):
    @expose("/look")
    def index(self):
        return "look"

def setup(app):
    admin = Admin(app)
    admin.add_view(Lookalike())
"#;
    write(dir.path(), "app/admin/base.py", base);
    write(dir.path(), "app/admin/search.py", search);
    write(dir.path(), "app/admin/complaint.py", complaint);
    write(dir.path(), "app/admin/index.py", index);
    write(dir.path(), "app/site.py", site);
    write(dir.path(), "app/fake.py", fake);
    write(
        dir.path(),
        "app/models.py",
        "class ProviderComplaint:\n    pass\n\nclass User:\n    pass\n",
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let mut endpoints = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            assert_eq!(node.props["mount"], "flask_admin");
            assert_eq!(node.props["framework"], "flask");
            (
                node.props["method"].as_str().unwrap().to_string(),
                node.props["path"].as_str().unwrap().to_string(),
                node.props["endpoint_name"].as_str().unwrap().to_string(),
                node.props["prov"]["confidence_tier"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                node.props["auth"]["reasons"][0]
                    .as_str()
                    .unwrap()
                    .to_string(),
                node.props["handler_sym"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort();
    let sym = |path: &str, name: &str| format!("sym:local/python-service@{path}#{name}");
    let search_sym = |name: &str| sym("app/admin/search.py", name);
    let checked = "flask_admin_access_check:BaseAdminView";
    let open = "flask_admin_default_allows_all";
    let row = |method: &str, path: &str, name: &str, tier: &str, auth: &str, handler: String| {
        (
            method.to_string(),
            path.to_string(),
            name.to_string(),
            tier.to_string(),
            auth.to_string(),
            handler,
        )
    };
    assert_eq!(
        endpoints,
        vec![
            row(
                "GET",
                "/admin/",
                "admin.index",
                "InferredStrong",
                open,
                sym("app/admin/base.py", "SLAdminIndexView.index")
            ),
            row(
                "GET",
                "/admin/admin.email_search/",
                "admin.email_search.index",
                "InferredStrong",
                checked,
                search_sym("EmailSearchAdmin.index")
            ),
            row(
                "GET",
                "/admin/open/",
                "openview.index",
                "InferredStrong",
                open,
                search_sym("OpenView.index")
            ),
            row(
                "GET",
                "/admin/providercomplaint/mark_ok",
                "providercomplaint.mark_ok",
                "InferredStrong",
                checked,
                sym("app/admin/complaint.py", "ProviderComplaintAdmin.mark_ok")
            ),
            row(
                "GET",
                "/backoffice/o2/",
                "o2.index",
                "Confirmed",
                open,
                search_sym("OpenView.index")
            ),
            row(
                "GET",
                "/kids/",
                "child.index",
                "InferredStrong",
                checked,
                search_sym("EmailSearchAdmin.index")
            ),
            row(
                "GET",
                "/kids/extra",
                "child.extra",
                "InferredStrong",
                checked,
                search_sym("ChildSearch.extra")
            ),
            row(
                "POST",
                "/admin/admin.email_search/",
                "admin.email_search.index",
                "InferredStrong",
                checked,
                search_sym("EmailSearchAdmin.index")
            ),
            row(
                "POST",
                "/admin/admin.email_search/delete_user",
                "admin.email_search.delete_user",
                "InferredStrong",
                checked,
                search_sym("EmailSearchAdmin.delete_user")
            ),
            row(
                "POST",
                "/kids/",
                "child.index",
                "InferredStrong",
                checked,
                search_sym("EmailSearchAdmin.index")
            ),
        ]
    );
    // Exposed methods read as Flask handlers.
    let delete = extraction
        .nodes
        .iter()
        .find(|node| node.props["endpoint_name"] == "admin.email_search.delete_user")
        .unwrap();
    assert_eq!(
        delete.props["responses"]["kinds"],
        serde_json::json!(["text"])
    );
    let mut gaps = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:flask_admin:"))
        .map(|node| {
            (
                node.props["reason"].as_str().unwrap().to_string(),
                node.props["class"].as_str().unwrap().to_string(),
                node.props["prov"]["evidence"][0]["path"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    gaps.sort();
    let gap = |reason: &str, class: &str, path: &str| {
        (reason.to_string(), class.to_string(), path.to_string())
    };
    assert_eq!(
        gaps,
        vec![
            gap(
                "flask_admin_admin_instance_not_proven",
                "OpenView",
                "app/admin/index.py"
            ),
            gap(
                "flask_admin_library_index_route",
                "AdminIndexView",
                "app/fake.py"
            ),
            gap(
                "flask_admin_library_index_route",
                "AdminIndexView",
                "app/site.py"
            ),
            gap(
                "flask_admin_model_view_routes_in_library",
                "ProviderComplaintAdmin",
                "app/admin/index.py"
            ),
            gap(
                "flask_admin_model_view_routes_in_library",
                "SLModelView",
                "app/admin/index.py"
            ),
            gap(
                "flask_admin_view_endpoint_not_literal",
                "EmailSearchAdmin",
                "app/admin/index.py"
            ),
            gap(
                "flask_admin_view_overrides:__init__:Custom",
                "",
                "app/admin/index.py"
            ),
        ]
    );
}

#[test]
fn flask_admin_views_resolve_by_lexical_binding_and_c3_order() {
    // AC-0410 (#1193 review): a view or base name is the class it names only
    // where nothing shadows it, and a method's owner follows Python's C3
    // MRO, as `AdminViewMeta`'s `dir`/`getattr` do.
    let dir = tempfile::tempdir().unwrap();
    let shadow = r#"
from flask import Flask
from flask_admin import Admin, BaseView, expose
from app.views import Imp

class V(BaseView):
    @expose("/")
    def index(self):
        return "module view"

class Root(BaseView):
    @expose("/")
    def index(self):
        return "root"

    @expose("/base")
    def page(self):
        return "base"

class Left(Root):
    pass

class Right(Root):
    @expose("/right")
    def page(self):
        return "right"

class Quiet(Root):
    def page(self):
        return "quiet"

class Diamond(Left, Right):
    pass

class QuietDiamond(Left, Quiet):
    pass

class Single(Left):
    pass

class Inconsistent(Root, Left):
    pass

class Twice(BaseView):
    @expose("/")
    def index(self):
        return "twice"

Twice = V
Alias = V

class Dup(Root, Root):
    pass

app = Flask(__name__)
admin = Admin(app)
admin.add_view(V(endpoint="v"))
admin.add_view(Imp(endpoint="imp"))
admin.add_view(Diamond(url="/d", endpoint="d"))
admin.add_view(QuietDiamond(url="/q", endpoint="q"))
admin.add_view(Single(url="/s", endpoint="s"))
admin.add_view(Inconsistent(url="/i", endpoint="i"))
admin.add_view(Twice(endpoint="twice"))
admin.add_view(Alias(endpoint="alias"))
admin.add_view(Dup(endpoint="dup"))

def register(V):
    admin.add_view(V(endpoint="param"))

def register_local():
    Imp = V
    admin.add_view(Imp(endpoint="local"))

def register_match(choice):
    match choice:
        case Imp:
            admin.add_view(Imp(endpoint="captured"))

def register_keyword(choice):
    match choice:
        case dict(key=Imp):
            admin.add_view(Imp(endpoint="keyword"))

# A class pattern and a value pattern only read `Imp`.
def register_class(choice):
    match choice:
        case Imp():
            admin.add_view(Imp(endpoint="classpat"))

def register_value(choice):
    match choice:
        case Imp.X:
            admin.add_view(Imp(endpoint="valuepat"))
"#;
    let views = r#"
from flask_admin import BaseView, expose

class Imp(BaseView):
    @expose("/")
    def index(self):
        return "imported"
"#;
    // A rebound library base is no longer Flask-Admin's `BaseView`.
    let rebase = r#"
from flask import Flask
from flask_admin import Admin, BaseView, expose

BaseView = dict

class Bad(BaseView):
    @expose("/")
    def index(self):
        return "bad"

app = Flask(__name__)
admin = Admin(app)
admin.add_view(Bad(endpoint="bad"))
"#;
    write(dir.path(), "app/shadow.py", shadow);
    write(dir.path(), "app/views.py", views);
    write(dir.path(), "app/rebase.py", rebase);
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let mut endpoints = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            format!(
                "{} {} {} {} {}",
                node.props["method"].as_str().unwrap(),
                node.props["path"].as_str().unwrap(),
                node.props["endpoint_name"].as_str().unwrap(),
                node.props["prov"]["confidence_tier"].as_str().unwrap(),
                node.props["handler_sym"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort();
    let mut gaps = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:flask_admin:"))
        .map(|node| {
            format!(
                "{} {} {}",
                node.props["reason"].as_str().unwrap(),
                node.props["class"].as_str().unwrap(),
                node.props["prov"]["evidence"][0]["path"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    gaps.sort();
    let sym = |name: &str| format!("sym:local/python-service@app/shadow.py#{name}");
    assert_eq!(
        endpoints,
        vec![
            "GET /admin/classpat/ classpat.index Confirmed sym:local/python-service@app/views.py#Imp.index"
                .to_string(),
            "GET /admin/imp/ imp.index Confirmed sym:local/python-service@app/views.py#Imp.index"
                .to_string(),
            format!("GET /admin/v/ v.index Confirmed {}", sym("V.index")),
            "GET /admin/valuepat/ valuepat.index Confirmed sym:local/python-service@app/views.py#Imp.index"
                .to_string(),
            format!("GET /d/ d.index Confirmed {}", sym("Root.index")),
            // C3: Diamond, Left, Right, Root — Right's override wins.
            format!("GET /d/right d.page Confirmed {}", sym("Right.page")),
            // QuietDiamond, Left, Quiet, Root: Quiet's unexposed `page` owns it.
            format!("GET /q/ q.index Confirmed {}", sym("Root.index")),
            format!("GET /s/ s.index Confirmed {}", sym("Root.index")),
            format!("GET /s/base s.page Confirmed {}", sym("Root.page")),
        ]
    );
    assert_eq!(
        gaps,
        vec![
            "flask_admin_library_index_route AdminIndexView app/rebase.py",
            "flask_admin_library_index_route AdminIndexView app/shadow.py",
            "flask_admin_view_base_not_modelled:BaseView  app/rebase.py",
            // An assignment, not the class it names.
            "flask_admin_view_class_not_modelled:Alias  app/shadow.py",
            // `Imp = V` in `register_local`, and the captures `case Imp:` and
            // `key=Imp`.
            "flask_admin_view_class_not_modelled:Imp  app/shadow.py",
            "flask_admin_view_class_not_modelled:Imp  app/shadow.py",
            "flask_admin_view_class_not_modelled:Imp  app/shadow.py",
            "flask_admin_view_class_not_modelled:Twice  app/shadow.py",
            // The parameter `V` of `register`.
            "flask_admin_view_class_not_modelled:V  app/shadow.py",
            "flask_admin_view_mro_inconsistent:Dup  app/shadow.py",
            "flask_admin_view_mro_inconsistent:Inconsistent  app/shadow.py",
        ]
    );
}

#[test]
fn flask_admin_library_routes_follow_the_pinned_release() {
    // AC-0415 (#1203): with every root pin on a checked Flask-Admin release,
    // model views and a non-overriding index view gain the library's own
    // routes (InferredStrong, library handler); a repository redefinition
    // keeps its own `@expose` or removes the route. Otherwise, a Gap.
    let app = r#"
from flask import Flask
from flask_admin import Admin, AdminIndexView, expose
from flask_admin.contrib.sqla import ModelView

class User:
    pass

class UserAdmin(ModelView):
    def create_view(self):
        return "disabled"

    @expose("/edit/", methods=["POST"])
    def edit_view(self):
        return "edited"

class Home(AdminIndexView):
    pass

app = Flask(__name__)
admin = Admin(app, url="/back", index_view=Home(endpoint="home"))
admin.add_view(UserAdmin(User, None))
plain = Admin(app, url="/plain", endpoint="plain")
"#;
    let rows = |dir: &Path| {
        let extraction = extract_dir(dir, &id()).unwrap();
        let mut endpoints = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .map(|node| {
                format!(
                    "{} {} {} {} {} {}",
                    node.props["method"].as_str().unwrap(),
                    node.props["path"].as_str().unwrap(),
                    node.props["endpoint_name"].as_str().unwrap(),
                    node.props["prov"]["confidence_tier"].as_str().unwrap(),
                    node.props["handler_origin"]
                        .as_str()
                        .unwrap_or("repository"),
                    node.props["prov"]["evidence"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|evidence| evidence["path"] != "app.py")
                        .map(|evidence| evidence["path"].as_str().unwrap())
                        .collect::<Vec<_>>()
                        .join(","),
                )
            })
            .collect::<Vec<_>>();
        endpoints.sort();
        let mut gaps = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Gap" && node.id.starts_with("gap:flask_admin:"))
            .map(|node| node.props["reason"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        gaps.sort();
        let library = extraction
            .nodes
            .iter()
            .find(|node| {
                node.id == "sym:pypi/flask-admin@flask_admin/model/base.py#BaseModelView.index_view"
            })
            .map(|node| node.props["boundary"].as_str().unwrap_or("").to_string());
        (endpoints, gaps, library)
    };
    let pinned = tempfile::tempdir().unwrap();
    write(pinned.path(), "app.py", app);
    write(
        pinned.path(),
        "uv.lock",
        "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n",
    );
    write(
        pinned.path(),
        "requirements-dev.lock",
        "flask-admin==1.5.8\n",
    );
    let (endpoints, gaps, library) = rows(pinned.path());
    let lib = |rest: &str| format!("{rest} InferredStrong library requirements-dev.lock,uv.lock");
    assert_eq!(
        endpoints,
        vec![
            // A given index view keeps its own URL (`/admin` by default).
            lib("GET /admin/ home.index"),
            lib("GET /back/user/ user.index_view"),
            lib("GET /back/user/ajax/lookup/ user.ajax_lookup"),
            lib("GET /back/user/details/ user.details_view"),
            lib("GET /back/user/export/<export_type>/ user.export"),
            lib("GET /plain/ plain.index"),
            lib("POST /back/user/action/ user.action_view"),
            lib("POST /back/user/ajax/update/ user.ajax_update"),
            lib("POST /back/user/delete/ user.delete_view"),
            // The repository's own `edit_view` replaces the library's.
            "POST /back/user/edit/ user.edit_view Confirmed repository ".to_string(),
        ]
    );
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(library.as_deref(), Some("external"));

    // A checked pin in one lockfile never masks another's release, whatever
    // the table's key order, indentation or comments; a table in another
    // syntax is refused.
    for (uv, requirements, reason) in [
        (
            "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n",
            "flask-admin==1.6.0\n",
            "flask_admin_version_not_modelled:1.6.0",
        ),
        (
            "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n",
            "flask-admin>=1.5\n",
            "flask_admin_version_not_exact:requirements.txt",
        ),
        (
            "[[package]]\nversion = \"1.6.0\"\nname = \"flask-admin\"\n",
            "flask-admin==1.5.8\n",
            "flask_admin_version_not_modelled:1.6.0",
        ),
        (
            "  [[package]]  # admin\n  name = \"flask-admin\" # pinned\n  version = \"1.6.0\"  # x\n",
            "flask-admin==1.5.8\n",
            "flask_admin_version_not_modelled:1.6.0",
        ),
        (
            "[[ \"package\" ]]\n'name' = \"flask-admin\"\n\"version\" = \"1.6.0\"\n",
            "flask-admin==1.5.8\n",
            "flask_admin_version_not_modelled:1.6.0",
        ),
        (
            "[[package]]\n\"n\\u0061me\" = \"flask-admin\"\nversion = \"1.6.0\"\n",
            "flask-admin==1.5.8\n",
            "flask_admin_lockfile_syntax_not_modelled:uv.lock",
        ),
        (
            "[[\"p\\u0061ckage\"]]\nname = \"flask-admin\"\nversion = \"1.6.0\"\n",
            "flask-admin==1.5.8\n",
            "flask_admin_lockfile_syntax_not_modelled:uv.lock",
        ),
        (
            "[[package]]\nname = 'flask-admin'\nversion = \"1.6.0\"\n",
            "flask-admin==1.5.8\n",
            "flask_admin_version_not_exact:uv.lock",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.py", app);
        write(dir.path(), "uv.lock", uv);
        write(dir.path(), "requirements.txt", requirements);
        let (endpoints, gaps, _) = rows(dir.path());
        assert_eq!(
            endpoints,
            vec!["POST /back/user/edit/ user.edit_view Confirmed repository ".to_string()]
        );
        assert_eq!(gaps, vec![reason.to_string(); 3], "{uv}");
    }
    // A root lockfile that cannot be read may pin anything: a symlink here.
    let unread = tempfile::tempdir().unwrap();
    write(unread.path(), "app.py", app);
    write(
        unread.path(),
        "uv.lock",
        "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n",
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            unread.path().join("uv.lock"),
            unread.path().join("requirements.txt"),
        )
        .unwrap();
        let (_, gaps, _) = rows(unread.path());
        assert_eq!(
            gaps,
            vec!["flask_admin_lockfile_not_inspected:requirements.txt"; 3]
        );
    }
    // A repository mixin after the library base in C3 order does not
    // replace the library's routed methods.
    let late = tempfile::tempdir().unwrap();
    write(
        late.path(),
        "app.py",
        r#"
from flask import Flask
from flask_admin import Admin
from flask_admin.contrib.sqla import ModelView

class User:
    pass

class LateMixin:
    def delete_view(self):
        return "never routed"

class UserAdmin(ModelView, LateMixin):
    pass

class EarlyAdmin(LateMixin, ModelView):
    pass

app = Flask(__name__)
admin = Admin(app, url="/back")
admin.add_view(UserAdmin(User, None))
admin.add_view(EarlyAdmin(User, None, endpoint="early"))
"#,
    );
    write(
        late.path(),
        "uv.lock",
        "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n",
    );
    let (endpoints, gaps, _) = rows(late.path());
    assert!(gaps.is_empty(), "{gaps:?}");
    let deletes = endpoints
        .iter()
        .filter(|row| row.contains("delete_view"))
        .map(|row| row.split(' ').take(3).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>();
    assert_eq!(deletes, vec!["POST /back/user/delete/ user.delete_view"]);
    let unpinned = tempfile::tempdir().unwrap();
    write(unpinned.path(), "app.py", app);
    let (_, gaps, _) = rows(unpinned.path());
    assert_eq!(
        gaps,
        vec![
            "flask_admin_library_index_route",
            "flask_admin_library_index_route",
            "flask_admin_model_view_routes_in_library",
        ]
    );
}

#[test]
fn flask_admin_login_check_proves_auth_required() {
    // AC-0421 (#1216): a view whose `is_accessible` requires
    // `flask_login.current_user.is_authenticated`, whose callback never
    // returns `None`, on checked Flask-Admin and flask-login releases, is
    // `required`; any unproven condition keeps `unknown`, naming it.
    let app = r#"
from flask import Flask, redirect, url_for
from flask_admin import Admin, BaseView, expose
from flask_admin.contrib.sqla import ModelView
from flask_login import current_user

def check():
    return True

class User:
    pass

class Guarded(BaseView):
    def is_accessible(self):
        """Logged-in admins only."""
        return (current_user.is_authenticated and current_user.is_admin) and check()

    def inaccessible_callback(self, name, **kwargs):
        target = self.target()
        return target or redirect(url_for("login"))

class UserAdmin(ModelView, Guarded):
    pass

class Reports(Guarded):
    @expose("/")
    def index(self):
        return "ok"

app = Flask(__name__)
admin = Admin(app, url="/back")
admin.add_view(UserAdmin(User, None))
admin.add_view(Reports(endpoint="reports"))
"#;
    let auth = "from flask_login import LoginManager\n\nlogin_manager = LoginManager()\n";
    let lock = "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n\n[[package]]\nname = \"flask-login\"\nversion = \"0.5.0\"\n";
    let rows = |app: &str, auth: &str, lock: Option<&str>| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.py", app);
        write(dir.path(), "auth.py", auth);
        if let Some(lock) = lock {
            write(dir.path(), "uv.lock", lock);
        }
        let extraction = extract_dir(dir.path(), &id()).unwrap();
        let mut rows = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .map(|node| {
                let auth = &node.props["auth"];
                let detail = match auth["state"].as_str().unwrap() {
                    "required" => {
                        assert_eq!(auth["authenticated"], true);
                        assert_eq!(
                            auth["basis"],
                            serde_json::json!(["flask_admin_is_accessible_requires_login"])
                        );
                        assert_eq!(auth["prov"]["confidence_tier"], "InferredStrong");
                        let paths = auth["prov"]["evidence"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|evidence| evidence["path"].as_str().unwrap())
                            .collect::<BTreeSet<_>>();
                        assert_eq!(paths, BTreeSet::from(["app.py", "uv.lock"]));
                        auth["guards"][0].as_str().unwrap().to_string()
                    }
                    _ => auth["reasons"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|reason| reason.as_str().unwrap())
                        .collect::<Vec<_>>()
                        .join(","),
                };
                format!(
                    "{} {} {} {detail}",
                    node.props["method"].as_str().unwrap(),
                    node.props["path"].as_str().unwrap(),
                    auth["state"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows.dedup_by(|a, b| a.split(' ').skip(2).eq(b.split(' ').skip(2)));
        rows
    };
    // One row per distinct auth outcome: the default index, then the views.
    let guard = "sym:local/python-service@app.py#Guarded.is_accessible";
    let open = "GET /back/ unknown flask_admin_default_allows_all".to_string();
    assert_eq!(
        rows(app, auth, Some(lock)),
        vec![open.clone(), format!("GET /back/reports/ required {guard}")]
    );
    let unknown = |reason: &str| {
        vec![
            open.clone(),
            format!("GET /back/reports/ unknown flask_admin_access_check:Guarded,{reason}"),
        ]
    };
    let login = "return (current_user.is_authenticated and current_user.is_admin) and check()";
    let callback = "return target or redirect(url_for(\"login\"))";
    for (app, auth, lock, reason) in [
        (
            app.replace(login, "return current_user.is_authenticated or check()"),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_is_accessible_not_login_check:Guarded",
        ),
        (
            app.replace(
                "def check():",
                "current_user = None\n\ndef check():",
            ),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_is_accessible_not_login_check:Guarded",
        ),
        (
            app.replace(
                callback,
                "if target:\n            return target\n        return redirect(url_for(\"login\"))",
            ),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_inaccessible_callback_may_return_none:Guarded",
        ),
        (
            app.replace(callback, "return target and redirect(url_for(\"login\"))"),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_inaccessible_callback_may_return_none:Guarded",
        ),
        (
            app.replace(
                "class UserAdmin",
                "    is_accessible = check\n\nclass UserAdmin",
            ),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_hook_not_a_plain_method:is_accessible:Guarded",
        ),
        (
            app.replace("class Guarded(BaseView):", "class Guarded:")
                .replace("class Reports(Guarded):", "class Reports(BaseView, Guarded):"),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_hook_outside_view_class:Guarded",
        ),
        (
            app.to_string(),
            format!("{auth}login_manager.anonymous_user = User\n"),
            Some(lock.to_string()),
            "flask_login_anonymous_user_replaced",
        ),
        (
            app.to_string(),
            format!("{auth}setattr(login_manager, \"anonymous_user\", object)\n"),
            Some(lock.to_string()),
            "flask_login_anonymous_user_replaced",
        ),
        (
            app.to_string(),
            format!("import builtins\n{auth}builtins.setattr(login_manager, \"anonymous_user\", object)\n"),
            Some(lock.to_string()),
            "flask_login_anonymous_user_replaced",
        ),
        (
            app.to_string(),
            format!("{auth}for login_manager.anonymous_user in [object]:\n    pass\n"),
            Some(lock.to_string()),
            "flask_login_anonymous_user_replaced",
        ),
        (
            app.to_string(),
            format!("{auth}with open(\"x\") as login_manager.anonymous_user:\n    pass\n"),
            Some(lock.to_string()),
            "flask_login_anonymous_user_replaced",
        ),
        (
            app.replace(
                "from flask_login import current_user",
                "from custom import current_user\n\ndef other():\n    from flask_login import current_user\n    return current_user",
            ),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_is_accessible_not_login_check:Guarded",
        ),
        (
            app.replace(
                "from flask import Flask, redirect, url_for",
                "from flask import Flask, url_for\nfrom custom import redirect\n\ndef other():\n    from flask import redirect\n    return redirect",
            ),
            auth.to_string(),
            Some(lock.to_string()),
            "flask_admin_inaccessible_callback_may_return_none:Guarded",
        ),
        (
            app.to_string(),
            "from app import Guarded\n\nGuarded.is_accessible = lambda self: True\n".to_string(),
            Some(lock.to_string()),
            "flask_admin_view_hook_replaced",
        ),
        (
            app.to_string(),
            "from app import Guarded\n\nsetattr(Guarded, \"is_accessible\", None)\n".to_string(),
            Some(lock.to_string()),
            "flask_admin_view_hook_replaced",
        ),
        (
            app.to_string(),
            "from app import Guarded\n\ndel Guarded.inaccessible_callback\n".to_string(),
            Some(lock.to_string()),
            "flask_admin_view_hook_replaced",
        ),
        (
            app.to_string(),
            "import flask_login\n\nflask_login.current_user = None\n".to_string(),
            Some(lock.to_string()),
            "flask_login_current_user_replaced",
        ),
        (
            app.to_string(),
            "import flask\n\nflask.redirect = print\n".to_string(),
            Some(lock.to_string()),
            "flask_response_helper_replaced",
        ),
        (
            app.to_string(),
            auth.to_string(),
            Some(lock.replace("0.5.0", "0.2.11")),
            "flask_login_version_not_modelled:0.2.11",
        ),
        (
            app.to_string(),
            auth.to_string(),
            Some(lock.replace("flask-login", "flask-wtf")),
            "flask_login_version_not_pinned",
        ),
    ] {
        assert_eq!(rows(&app, &auth, lock.as_deref()), unknown(reason), "{reason}");
    }
    // A `_handle_view` on one view leaves the others proven.
    let overriding = app.replace(
        "    @expose(\"/\")",
        "    def _handle_view(self, name, **kwargs):\n        return None\n\n    @expose(\"/\")",
    );
    assert_eq!(
        rows(&overriding, auth, Some(lock)),
        vec![
            open.clone(),
            "GET /back/reports/ unknown flask_admin_access_check:Guarded,flask_admin_view_overrides:_handle_view:Reports".to_string(),
            format!("GET /back/user/ required {guard}"),
        ]
    );
    // A hook bound by a class-body loop (or any other binding form) is that
    // class's, and not a plain method, so it is never proven.
    let looped = app.replace(
        "    @expose(\"/\")",
        "    for is_accessible in [check]:\n        pass\n\n    @expose(\"/\")",
    );
    assert_eq!(
        rows(&looped, auth, Some(lock)),
        vec![
            open.clone(),
            "GET /back/reports/ unknown flask_admin_access_check:Reports,flask_admin_hook_not_a_plain_method:is_accessible:Reports".to_string(),
            format!("GET /back/user/ required {guard}"),
        ]
    );
    // No pins: no library routes and no proof.
    assert_eq!(
        rows(app, auth, None),
        vec!["GET /back/reports/ unknown flask_admin_access_check:Guarded,flask_admin_version_not_pinned".to_string()]
    );
}

#[test]
fn flask_admin_in_body_login_check_proves_auth_required() {
    // AC-0421 (#1320): a view no `is_accessible` guards, whose body answers
    // an anonymous request before anything else, directly or through the
    // helper it returns, is `required` on the body's own basis. Any other
    // shape stays `unknown` and names the function holding the check.
    let app = r#"
from flask import Flask, abort, redirect, render_template, request, url_for
from flask_admin import Admin, AdminIndexView, BaseView, expose
from flask_login import current_user

def _redirect_if_user_is_not_allowed():
    """Send anonymous users to the login page."""
    if not current_user.is_authenticated or not current_user.is_admin:
        return redirect(url_for("auth.login", next=request.url))
    return None

def load():
    return "data"

def cached(view):
    return view

class Index(AdminIndexView):
    @expose("/")
    def index(self):
        redirect_destination = _redirect_if_user_is_not_allowed()
        return redirect_destination or redirect(url_for("reports.index"))

class Reports(BaseView):
    @expose("/")
    def index(self):
        """Admins only."""
        if not (current_user.is_authenticated and current_user.is_admin):
            abort(403)
        return "ok"

    @expose("/open")
    def open(self):
        return load()

    @expose("/late")
    def late(self):
        data = load()
        if not current_user.is_authenticated:
            return redirect(url_for("auth.login"))
        return data

app = Flask(__name__)
admin = Admin(app, url="/back", index_view=Index(url="/back"))
admin.add_view(Reports(endpoint="reports"))
"#;
    let lock = "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n\n[[package]]\nname = \"flask-login\"\nversion = \"0.5.0\"\n";
    let rows = |app: &str, lock: Option<&str>| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.py", app);
        if let Some(lock) = lock {
            write(dir.path(), "uv.lock", lock);
        }
        let extraction = extract_dir(dir.path(), &id()).unwrap();
        let mut rows = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .map(|node| {
                let auth = &node.props["auth"];
                let detail = match auth["state"].as_str().unwrap() {
                    "required" => {
                        assert_eq!(auth["authenticated"], true);
                        assert_eq!(auth["prov"]["confidence_tier"], "InferredStrong");
                        let paths = auth["prov"]["evidence"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|evidence| evidence["path"].as_str().unwrap())
                            .collect::<BTreeSet<_>>();
                        assert_eq!(paths, BTreeSet::from(["app.py", "uv.lock"]));
                        format!(
                            "{} {} {}",
                            auth["guards"][0].as_str().unwrap(),
                            auth["basis"][0].as_str().unwrap(),
                            auth["assumptions"].as_array().unwrap().len(),
                        )
                    }
                    _ => auth["reasons"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|reason| reason.as_str().unwrap())
                        .collect::<Vec<_>>()
                        .join(","),
                };
                format!(
                    "{} {} {detail}",
                    node.props["path"].as_str().unwrap(),
                    auth["state"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows
    };
    let sym = "sym:local/python-service@app.py#";
    let basis = "flask_view_body_denies_anonymous";
    let open = "flask_admin_default_allows_all";
    let helper = "_redirect_if_user_is_not_allowed";
    let proven = format!("/back/ required {sym}{helper} {basis} 2");
    let rest = [
        format!("/back/reports/ required {sym}Reports.index {basis} 1"),
        format!("/back/reports/late unknown {open},in_body_auth_check:Reports.late"),
        format!("/back/reports/open unknown {open}"),
    ];
    let with = |index: String| {
        let mut rows = vec![index];
        rows.extend(rest.iter().cloned());
        rows
    };
    assert_eq!(rows(app, Some(lock)), with(proven.clone()));

    let assign = "redirect_destination = _redirect_if_user_is_not_allowed()";
    let returned = "return redirect_destination or redirect(url_for(\"reports.index\"))";
    let check = "if not current_user.is_authenticated or not current_user.is_admin:";
    let answer = "return redirect(url_for(\"auth.login\", next=request.url))";
    // Other proven spellings of the same check.
    for (from, to) in [
        (
            returned,
            "if redirect_destination:\n            return redirect_destination\n        return \"ok\"",
        ),
        (
            check,
            "if not current_user.is_admin or (not current_user.is_authenticated):",
        ),
        (
            check,
            "if not (current_user.is_authenticated and current_user.is_admin):",
        ),
        (answer, "abort(401)"),
        (answer, "load()\n        return abort(403)"),
    ] {
        assert!(app.contains(from), "{from}");
        assert_eq!(
            rows(&app.replace(from, to), Some(lock)),
            with(proven.clone()),
            "{to}"
        );
    }
    let direct = app.replace(&format!("{assign}\n        "), "").replace(
        returned,
        "return _redirect_if_user_is_not_allowed() or redirect(url_for(\"reports.index\"))",
    );
    assert_eq!(rows(&direct, Some(lock)), with(proven.clone()));

    // Shapes that are not proven keep `unknown`, naming where the check is.
    let seen = format!("/back/ unknown {open},in_body_auth_check:{helper}");
    for (from, to) in [
        // An anonymous user who is not an admin passes an `and` condition.
        (
            check,
            "if not current_user.is_authenticated and not current_user.is_admin:",
        ),
        (check, "if current_user.is_authenticated:"),
        // Calling a helper without its required argument raises before the check.
        (
            "def _redirect_if_user_is_not_allowed():",
            "def _redirect_if_user_is_not_allowed(request):",
        ),
        (
            "def _redirect_if_user_is_not_allowed():",
            "def _redirect_if_user_is_not_allowed(user=None):",
        ),
        // The view may drop the helper's answer.
        (
            returned,
            "return redirect_destination and redirect(url_for(\"reports.index\"))",
        ),
        (
            returned,
            "return redirect(url_for(\"reports.index\")) or redirect_destination",
        ),
        (
            returned,
            "load()\n        return redirect_destination or \"ok\"",
        ),
        // A page is content, not a denial; 404 is not an authentication answer.
        (answer, "return render_template(\"login.html\")"),
        (answer, "abort(404)"),
        (answer, "redirect(url_for(\"auth.login\"))"),
        (
            answer,
            "if request.args:\n            return \"ok\"\n        return redirect(\"/\")",
        ),
        // The check is not the first thing the helper does.
        (
            check,
            "data = load()\n    if not current_user.is_authenticated:",
        ),
        // An argument is evaluated before the helper runs.
        (
            assign,
            "redirect_destination = _redirect_if_user_is_not_allowed(load())",
        ),
        // Another decorator may answer without calling the view.
        (
            "    @expose(\"/\")\n    def index(self):\n        redirect_destination",
            "    @expose(\"/\")\n    @cached\n    def index(self):\n        redirect_destination",
        ),
        // A generator or coroutine helper runs nothing when called.
        ("    return None\n\ndef load", "    yield None\n\ndef load"),
        (
            "def _redirect_if_user_is_not_allowed",
            "async def _redirect_if_user_is_not_allowed",
        ),
    ] {
        assert!(app.contains(from), "{from}");
        assert_eq!(
            rows(&app.replace(from, to), Some(lock)),
            with(seen.clone()),
            "{to}"
        );
    }
    // `redirect` rebound in the helper is not Flask's.
    let rebound = app.replace(check, &format!("redirect = load\n    {check}"));
    assert_eq!(rows(&rebound, Some(lock)), with(seen.clone()));
    // A second binding of the helper's name: which one runs is not known,
    // so nothing is said about the body.
    let twice = format!("{app}\n_redirect_if_user_is_not_allowed = load\n");
    assert_eq!(
        rows(&twice, Some(lock)),
        with(format!("/back/ unknown {open}"))
    );

    // The proven body still needs every condition outside the view.
    let unpinned: Vec<String> = rows(app, None);
    assert_eq!(
        unpinned[0],
        format!("/back/ unknown {open},in_body_auth_check:{helper},flask_admin_version_not_pinned")
    );
    assert_eq!(
        unpinned[1],
        format!(
            "/back/reports/ unknown {open},in_body_auth_check:Reports.index,flask_admin_version_not_pinned"
        )
    );
    let overridden = app.replace(
        "class Index(AdminIndexView):",
        "class Index(AdminIndexView):\n    def _handle_view(self, name, **kwargs):\n        return None\n",
    );
    assert_eq!(
        rows(&overridden, Some(lock))[0],
        format!(
            "/back/ unknown {open},in_body_auth_check:{helper},flask_admin_view_overrides:_handle_view:Index"
        )
    );
    let anonymous = format!("{app}\napp.login_manager.anonymous_user = object\n");
    assert_eq!(
        rows(&anonymous, Some(lock))[0],
        format!(
            "/back/ unknown {open},in_body_auth_check:{helper},flask_login_anonymous_user_replaced"
        )
    );

    // A class that defines `is_accessible` is judged by it, as before.
    let guarded = app.replace(
        "class Index(AdminIndexView):",
        "class Index(AdminIndexView):\n    def is_accessible(self):\n        return True\n",
    );
    assert_eq!(
        rows(&guarded, Some(lock))[0],
        "/back/ unknown flask_admin_access_check:Index,flask_admin_is_accessible_not_login_check:Index"
    );
}

/// SimpleLogin's class-based concurrency lock (#1173), as one module.
const PARALLEL_LIMITER: &str = r#"import uuid
from datetime import timedelta
from functools import wraps
from typing import Callable, Any, Optional

from flask import request
from flask_login import current_user
from limits.storage import RedisStorage
from werkzeug import exceptions

lock_redis: Optional[RedisStorage] = None


def set_redis_concurrent_lock(redis: RedisStorage):
    global lock_redis
    lock_redis = redis


class _InnerLock:
    def __init__(
        self,
        lock_suffix: Optional[str] = None,
        max_wait_secs: int = 5,
        only_when: Optional[Callable[..., bool]] = None,
    ):
        self.lock_suffix = lock_suffix
        self.max_wait_secs = max_wait_secs
        self.only_when = only_when

    def acquire_lock(self, lock_name: str, lock_value: str):
        if not lock_redis.storage.set(
            lock_name, lock_value, ex=timedelta(seconds=self.max_wait_secs), nx=True
        ):
            raise exceptions.TooManyRequests()

    def release_lock(self, lock_name: str, lock_value: str):
        current_lock_value = lock_redis.storage.get(lock_name)
        if current_lock_value == lock_value.encode("utf-8"):
            lock_redis.storage.delete(lock_name)

    def __call__(self, f: Callable[..., Any]):
        if self.lock_suffix is None:
            lock_suffix = f.__name__
        else:
            lock_suffix = self.lock_suffix

        @wraps(f)
        def decorated(*args, **kwargs):
            if self.only_when and not self.only_when():
                return f(*args, **kwargs)
            if not lock_redis:
                return f(*args, **kwargs)

            lock_value = str(uuid.uuid4())[:10]
            if "id" in dir(current_user):
                lock_name = f"cl:{current_user.id}:{lock_suffix}"
            else:
                lock_name = f"cl:{request.remote_addr}:{lock_suffix}"
            self.acquire_lock(lock_name, lock_value)
            try:
                return f(*args, **kwargs)
            finally:
                self.release_lock(lock_name, lock_value)

        return decorated


def lock(
    name: Optional[str] = None,
    max_wait_secs: int = 5,
    only_when: Optional[Callable[..., bool]] = None,
):
    return _InnerLock(name, max_wait_secs, only_when)
"#;

/// The auth of `/mfa`, decorated with `decorator` from `parallel_limiter`
/// as `limiter` (with `edit` applied), beside `extra` files.
fn lock_auth(edit: (&str, &str), decorator: &str, extra: &[(&str, &str)]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let limiter = PARALLEL_LIMITER.replacen(edit.0, edit.1, 1);
    assert!(
        edit.0.is_empty() || limiter != PARALLEL_LIMITER,
        "{}",
        edit.0
    );
    write(dir.path(), "app/parallel_limiter.py", &limiter);
    write(dir.path(), "app/__init__.py", "");
    write(
        dir.path(),
        "app/views.py",
        &format!(
            r#"from flask import Flask, jsonify
from app import parallel_limiter

app = Flask(__name__)

@app.route("/mfa", methods=["POST"])
{decorator}
def mfa():
    return jsonify(ok=True)
"#
        ),
    );
    for (path, source) in extra {
        write(dir.path(), path, source);
    }
    auth_at(&extract_dir(dir.path(), &id()).unwrap(), "/mfa")
}

#[test]
fn a_class_based_lock_factory_is_proven_not_to_authenticate() {
    // AC-0446 (T-0446): `@parallel_limiter.lock(name="mfa_auth")` follows the
    // factory to `_InnerLock.__call__`'s wrapper, which only calls the view or
    // raises a 429 through its own method, so the route is unauthenticated.
    let decorator = r#"@parallel_limiter.lock(name="mfa_auth")"#;
    let auth = lock_auth(("", ""), decorator, &[]);
    assert_eq!(auth["state"], "none", "{auth}");
    assert_eq!(auth["authenticated"], false);
    assert_eq!(
        auth["non_auth_decorators"],
        serde_json::json!([r#"parallel_limiter.lock(name="mfa_auth")"#])
    );
    let assumptions = auth["assumptions"].as_array().unwrap();
    for assumption in [
        "non_repository_calls_in_repository_decorators_do_not_abort",
        "werkzeug_errors_keep_their_status",
    ] {
        assert!(
            assumptions.contains(&serde_json::json!(assumption)),
            "{auth}"
        );
    }
    assert!(!assumptions.contains(&serde_json::json!(
        "rate_limit_and_cors_decorators_do_not_authenticate"
    )));
    // A repository helper the wrapper calls is proven too.
    let helper = (
        "            self.acquire_lock(lock_name, lock_value)\n",
        "            self.acquire_lock(lock_name, lock_value)\n            throttle()\n",
    );
    let throttle = "\n\ndef throttle():\n    raise exceptions.TooManyRequests()\n";
    let dir_auth = lock_auth(helper, decorator, &[]);
    // `throttle` is not defined: an unrecovered name is assumed external,
    // since it resolves to no repository file.
    assert_eq!(dir_auth["state"], "none", "{dir_auth}");
    let with_helper = |body: &str| {
        let mut module = PARALLEL_LIMITER.replacen(helper.0, helper.1, 1);
        module.push_str(&throttle.replace("    raise exceptions.TooManyRequests()\n", body));
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/parallel_limiter.py", &module);
        write(dir.path(), "app/__init__.py", "");
        write(
            dir.path(),
            "app/views.py",
            "from flask import Flask\nfrom app import parallel_limiter\n\napp = Flask(__name__)\n\n@app.route(\"/mfa\")\n@parallel_limiter.lock()\ndef mfa():\n    return \"ok\"\n",
        );
        auth_at(&extract_dir(dir.path(), &id()).unwrap(), "/mfa")
    };
    assert_eq!(
        with_helper("    raise exceptions.TooManyRequests()\n")["state"],
        "none"
    );
    for denying in [
        "    raise exceptions.Forbidden()\n",
        "    abort(429)\n",
        "    raise\n",
        "    raise ValueError()\n",
        // A replacement response, an aliased abort, or an auth library.
        "    raise exceptions.TooManyRequests(response=make_response(\"no\", 401))\n",
        "    from flask import abort as deny\n    deny(401)\n",
        "    from flask_jwt_extended import verify_jwt_in_request as verify\n    verify()\n",
    ] {
        let auth = with_helper(denying);
        assert_eq!(auth["state"], "unknown", "{denying}: {auth}");
        assert_eq!(
            auth["reasons"],
            serde_json::json!(["unclassified_decorator:parallel_limiter.lock()"]),
            "{denying}"
        );
    }
}

#[test]
fn unproven_class_based_decorators_stay_unknown() {
    // AC-0446 (T-0446): any step the proof cannot see through keeps the
    // factory `unclassified_decorator`.
    let decorator = r#"@parallel_limiter.lock(name="mfa_auth")"#;
    let reason =
        serde_json::json!([r#"unclassified_decorator:parallel_limiter.lock(name="mfa_auth")"#]);
    let cases: &[((&str, &str), &str)] = &[
        // A 401/403 or unknown error from the method the wrapper calls.
        (
            ("exceptions.TooManyRequests()", "exceptions.Unauthorized()"),
            decorator,
        ),
        (
            ("raise exceptions.TooManyRequests()", "abort(429)"),
            decorator,
        ),
        // The wrapper returning a response of its own.
        (
            (
                "            if not lock_redis:\n                return f(*args, **kwargs)",
                "            if not lock_redis:\n                return redirect(\"/login\")",
            ),
            decorator,
        ),
        // A non-inert decorator argument or factory default, or a factory
        // passing something else to the class.
        (("", ""), "@parallel_limiter.lock(only_when=is_admin)"),
        (
            (
                "max_wait_secs: int = 5,\n    only_when",
                "max_wait_secs: int = timeout(),\n    only_when",
            ),
            decorator,
        ),
        (
            (
                "_InnerLock(name, max_wait_secs, only_when)",
                "_InnerLock(name, max_wait_secs, check)",
            ),
            decorator,
        ),
        // An instance attribute from anything but a literal or parameter.
        (
            ("self.only_when = only_when", "self.only_when = check"),
            decorator,
        ),
        // A base class, a class decorator, or a dunder that changes lookup.
        (("class _InnerLock:", "class _InnerLock(Base):"), decorator),
        (
            ("class _InnerLock:", "@dataclass\nclass _InnerLock:"),
            decorator,
        ),
        (
            (
                "    def acquire_lock(",
                "    def __getattr__(self, name):\n        return None\n\n    def acquire_lock(",
            ),
            decorator,
        ),
        // A method passed on rather than called, or rebinding the instance.
        (
            (
                "            self.acquire_lock(lock_name, lock_value)",
                "            acquire = self.acquire_lock\n            acquire(lock_name, lock_value)",
            ),
            decorator,
        ),
        (
            (
                "        current_lock_value = ",
                "        self.max_wait_secs = 0\n        current_lock_value = ",
            ),
            decorator,
        ),
        // The instance aliased or passed on, which may reach a method the
        // proof never follows.
        (
            (
                "            self.acquire_lock(lock_name, lock_value)",
                "            other = self\n            other.acquire_lock(lock_name, lock_value)",
            ),
            decorator,
        ),
        (
            (
                "            lock_suffix = self.lock_suffix\n",
                "            lock_suffix = self.lock_suffix\n        other = self\n",
            ),
            decorator,
        ),
        (
            (
                "            self.release_lock(lock_name, lock_value)",
                "            release(self, lock_name, lock_value)",
            ),
            decorator,
        ),
        (
            (
                "        current_lock_value = ",
                "        other = self\n        current_lock_value = ",
            ),
            decorator,
        ),
        // The class name shadowed in the factory, by a binding or parameter.
        (
            (
                "    return _InnerLock(name",
                "    _InnerLock = Guard\n    return _InnerLock(name",
            ),
            decorator,
        ),
        (
            (
                "    name: Optional[str] = None,\n    max_wait_secs",
                "    name: Optional[str] = None,\n    _InnerLock=None,\n    max_wait_secs",
            ),
            decorator,
        ),
        // A decorator on the wrapper other than `wraps(f)`.
        (("        @wraps(f)\n", "        @retry\n"), decorator),
        // A call through an expression, which may be a method.
        (
            (
                "            self.acquire_lock(lock_name, lock_value)",
                "            getattr(self, \"acquire_lock\")(lock_name, lock_value)",
            ),
            decorator,
        ),
    ];
    for (edit, decorator) in cases {
        let auth = lock_auth(*edit, decorator, &[]);
        assert_eq!(auth["state"], "unknown", "{edit:?}: {auth}");
        if *decorator == r#"@parallel_limiter.lock(name="mfa_auth")"# {
            assert_eq!(auth["reasons"], reason, "{edit:?}");
        }
    }
    // The class or factory replaced elsewhere, or werkzeug's exceptions.
    for patch in [
        "from app import parallel_limiter\n\nparallel_limiter._InnerLock.acquire_lock = print\n",
        "from app import parallel_limiter\n\nparallel_limiter.lock = print\n",
        "from werkzeug import exceptions\n\nexceptions.TooManyRequests = exceptions.Unauthorized\n",
        "import functools\n\nfunctools.wraps = print\n",
        // A helper writing the class it is passed, or any computed name.
        "from app import parallel_limiter\n\ndef patch(cls):\n    cls.__call__ = print\n\npatch(parallel_limiter._InnerLock)\n",
        "from app.parallel_limiter import _InnerLock as Lock\n\ndef patch(cls):\n    cls.acquire_lock = print\n\npatch(Lock)\n",
        "from app import parallel_limiter\n\ndef patch(cls, name):\n    setattr(cls, name, print)\n\npatch(getattr(parallel_limiter, \"_InnerLock\"), \"acquire_lock\")\n",
        "from app.parallel_limiter import *\n",
        // Or the factory, through the module it is passed.
        "from app import parallel_limiter\n\ndef patch(m):\n    m.lock = print\n\npatch(parallel_limiter)\n",
    ] {
        let auth = lock_auth(("", ""), decorator, &[("app/patch.py", patch)]);
        assert_eq!(auth["state"], "unknown", "{patch}: {auth}");
    }
    // The class escaping its own module, and an unapplied helper.
    let escaped = (
        "    return _InnerLock(name, max_wait_secs, only_when)\n",
        "    return _InnerLock(name, max_wait_secs, only_when)\n\n\nREGISTRY = [_InnerLock]\n",
    );
    assert_eq!(lock_auth(escaped, decorator, &[])["state"], "unknown");
    // A pytest module importing the class to test it changes nothing.
    let tested = "from app.parallel_limiter import _InnerLock\n\ndef test_lock():\n    _InnerLock(\"t\").acquire_lock(\"a\", \"b\")\n";
    for path in ["tests/test_parallel_limiter.py", "tests/conftest.py"] {
        assert_eq!(
            lock_auth(("", ""), decorator, &[(path, tested)])["state"],
            "none",
            "{path}"
        );
    }
    // Unless the product imports the pytest module, directly or through
    // another one, where its helper runs at startup.
    let patching = "from app.parallel_limiter import _InnerLock\n\ndef patch(cls):\n    cls.__call__ = print\n\npatch(_InnerLock)\nmarker = 1\n";
    for importer in [
        "from app import test_patch\n",
        "import app.test_patch\n",
        "import importlib\n\nimportlib.import_module(\"app.test_patch\")\n",
        // Importing a name from it, or all of it, runs it too.
        // A loader by any spelling, given a literal module name.
        "from importlib import import_module as load\n\nload(\"app.test_patch\")\n",
        "import builtins\n\nbuiltins.__import__(\"app.test_patch\")\n",
        "from app.test_patch import marker\n",
        "from app.test_patch import *\n",
        "from .test_patch import marker\n",
    ] {
        let auth = lock_auth(
            ("", ""),
            decorator,
            &[
                ("app/test_patch.py", patching),
                ("app/startup.py", importer),
            ],
        );
        assert_eq!(auth["state"], "unknown", "{importer}");
    }
    let chained = lock_auth(
        ("", ""),
        decorator,
        &[
            ("app/test_patch.py", patching),
            ("app/test_chain.py", "from app import test_patch\n"),
            ("app/startup.py", "from app import test_chain\n"),
        ],
    );
    assert_eq!(chained["state"], "unknown");
    let isolated = lock_auth(("", ""), decorator, &[("tests/test_patch.py", patching)]);
    assert_eq!(isolated["state"], "none");
    let unapplied = "def patch(cls):\n    cls.acquire_lock = print\n";
    assert_eq!(
        lock_auth(("", ""), decorator, &[("app/patch.py", unapplied)])["state"],
        "none"
    );
    // A package `__getattr__` may answer `from app import parallel_limiter`.
    let dynamic = (
        "app/__init__.py",
        "def __getattr__(name):\n    return None\n",
    );
    assert_eq!(
        lock_auth(("", ""), decorator, &[dynamic])["state"],
        "unknown"
    );
}

#[test]
fn a_pass_through_repository_decorator_does_not_authenticate() {
    // AC-0446 (T-0446): a bare repository decorator whose wrapper only calls
    // the view is non-authenticating; one returning anything else is not
    // classified.
    let auth = |wrapper: &str| {
        let source = format!(
            "from functools import wraps\nfrom flask import Flask, jsonify\n\napp = Flask(__name__)\n\ndef passthrough(f):\n    @wraps(f)\n    def wrapper(*args, **kwargs):\n{wrapper}\n    return wrapper\n\n@app.route(\"/passed\")\n@passthrough\ndef passed():\n    return jsonify(ok=True)\n"
        );
        auth_at(
            &extract_source(source.as_bytes(), "app.py", &id()).unwrap(),
            "/passed",
        )
    };
    let passed = auth("        return f(*args, **kwargs)");
    assert_eq!(passed["state"], "none", "{passed}");
    assert_eq!(
        passed["non_auth_decorators"],
        serde_json::json!(["passthrough"])
    );
    for wrapper in [
        "        rv = f(*args, **kwargs)\n        return rv",
        "        yield f(*args, **kwargs)",
        "        return redirect(\"/login\")",
        // Authentication enforced by a call whose result is ignored.
        "        verify_jwt_in_request()\n        return f(*args, **kwargs)",
        "        current_app.login_manager.unauthorized()\n        return f(*args, **kwargs)",
    ] {
        let auth = auth(wrapper);
        assert_eq!(auth["state"], "unknown", "{wrapper}: {auth}");
    }
    // `f` rebound by the decorator is no longer the view, and a wrapper
    // name bound twice may be either function.
    for decorator in [
        "def passthrough(f):\n    f = login_required(f)\n    @wraps(f)\n    def wrapper(*args, **kwargs):\n        return f(*args, **kwargs)\n    return wrapper\n",
        "def passthrough(f):\n    def wrapper(*args, **kwargs):\n        return f(*args, **kwargs)\n    wrapper = login_required(wrapper)\n    return wrapper\n",
    ] {
        let source = format!(
            "from functools import wraps\nfrom flask import Flask\nfrom flask_login import login_required\n\napp = Flask(__name__)\n\n{decorator}\n@app.route(\"/passed\")\n@passthrough\ndef passed():\n    return \"ok\"\n"
        );
        let auth = auth_at(
            &extract_source(source.as_bytes(), "app.py", &id()).unwrap(),
            "/passed",
        );
        assert_ne!(auth["state"], "none", "{decorator}: {auth}");
    }
    // A 401 response is still a guard.
    assert_eq!(
        auth("        return jsonify(error=\"no\"), 401")["state"],
        "required"
    );
}

fn rate_limits_at(extraction: &Extraction, path: &str) -> serde_json::Value {
    extraction
        .nodes
        .iter()
        .find(|node| node.label == "Endpoint" && node.props["path"] == path)
        .unwrap_or_else(|| panic!("endpoint {path}"))
        .props["rate_limits"]
        .clone()
}

#[test]
fn rate_limits_are_recovered_from_literals_and_config_defaults() {
    // AC-0881 (T-0881): a proven flask-limiter limit decorator on a route
    // handler is recorded on its endpoint, its value read from a literal
    // (Confirmed) or from a module constant whose environment default is a
    // literal (InferredStrong, with the key), and the `/me` literal too.
    let config = (
        "config.py",
        "import os\n\nALIAS_LIMIT = os.environ.get(\"ALIAS_LIMIT\") or \"100/day;50/hour;5/minute\"\n\
         PLAIN = \"10 per minute\"\nTWICE = \"1/hour\"\nTWICE = \"2/hour\"\n",
    );
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "extensions.py",
        "from flask_limiter import Limiter\n\nlimiter = Limiter(key_func=lambda: \"k\")\n",
    );
    write(dir.path(), config.0, config.1);
    write(
        dir.path(),
        "views.py",
        r#"from flask import Flask, g
from extensions import limiter
from config import ALIAS_LIMIT, PLAIN, TWICE

app = Flask(__name__)
LOCAL = "3/second"

@app.route("/env", methods=["POST"])
@limiter.limit(ALIAS_LIMIT)
def env():
    return "ok"

@app.route("/plain")
@limiter.limit(PLAIN, key_func=lambda: g.user.id)
def plain():
    return "ok"

@app.route("/literal")
@limiter.limit("100/hour", deduct_when=lambda r: True)
@limiter.shared_limit(LOCAL, scope="s")
def literal():
    return "ok"

@app.route("/unresolved")
@limiter.limit(TWICE)
def unresolved():
    return "ok"

@limiter.limit("1/day")
@app.route("/above")
def above():
    return "ok"
"#,
    );
    let extraction = extract_dir(dir.path(), &id()).unwrap();

    let env = rate_limits_at(&extraction, "/env");
    assert_eq!(
        env["entries"][0]["value"], "100/day;50/hour;5/minute",
        "{env}"
    );
    assert_eq!(env["entries"][0]["source"], "environment_default");
    assert_eq!(env["entries"][0]["environment_key"], "ALIAS_LIMIT");
    assert_eq!(
        env["entries"][0]["limits"],
        serde_json::json!([
            {"count": 100, "per": 1, "unit": "day"},
            {"count": 50, "per": 1, "unit": "hour"},
            {"count": 5, "per": 1, "unit": "minute"},
        ])
    );
    assert_eq!(env["prov"]["confidence_tier"], "InferredStrong");
    assert_eq!(env["exceeded_status"], 429);
    let paths: Vec<&str> = env["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["config.py", "views.py"]);

    let plain = rate_limits_at(&extraction, "/plain");
    assert_eq!(plain["entries"][0]["source"], "literal", "{plain}");
    assert_eq!(plain["entries"][0]["keyed_by"], "custom");
    assert_eq!(plain["prov"]["confidence_tier"], "Confirmed");

    let literal = rate_limits_at(&extraction, "/literal");
    assert_eq!(literal["entries"].as_array().unwrap().len(), 2, "{literal}");
    assert_eq!(literal["entries"][0]["value"], "100/hour");
    assert_eq!(literal["entries"][0]["conditional"], true);
    assert_eq!(literal["entries"][1]["value"], "3/second");
    assert_eq!(literal["entries"][1]["shared"], true);

    // A constant bound twice is not proven: a Gap naming the decorator.
    let unresolved = rate_limits_at(&extraction, "/unresolved");
    assert_eq!(
        unresolved["entries"][0]["unresolved"], "limit_value_not_a_proven_constant",
        "{unresolved}"
    );
    assert_eq!(unresolved["prov"]["confidence_tier"], "Gap");
    assert_eq!(
        unresolved["partial"],
        serde_json::json!(["limit_value_unresolved"])
    );

    // Above the route decorator it never wraps the registered view.
    assert!(rate_limits_at(&extraction, "/above").is_null());
}

#[test]
fn a_limit_on_something_not_a_limiter_records_nothing() {
    // AC-0881 (T-0881): `.limit` on a name that is not a proven
    // flask-limiter `Limiter` is not a rate limit.
    let extraction = decorated_route(
        "@limiter.limit(\"5/minute\")",
        Some("from flask_limiter import Limiter\n\nlimiter = Limiter()\nlimiter = wrap(limiter)\n"),
        &[],
    );
    assert!(rate_limits_at(&extraction, "/x").is_null());
    assert!(rate_limits_at(&extraction, "/me").is_null());
}

#[test]
fn a_call_through_an_imported_submodule_is_an_inferred_call() {
    // AC-0883 (T-0883): `from pkg import sub; sub.f()` reaches `f` of
    // `pkg/sub.py` when that file defines it, as an InferredStrong edge;
    // a member that file does not define stays without an edge.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "app/__init__.py", "");
    write(
        dir.path(),
        "app/alias_delete.py",
        "def delete_alias(alias):\n    return alias\n",
    );
    write(
        dir.path(),
        "app/views.py",
        "from app import alias_delete\n\n\ndef remove(alias):\n    alias_delete.delete_alias(alias)\n    alias_delete.missing(alias)\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let calls: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "CALLS" && edge.src.ends_with("app/views.py#remove"))
        .collect();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].dst.ends_with("app/alias_delete.py#delete_alias"));
    assert_eq!(calls[0].props["resolution"], "submodule-attribute");
    assert_eq!(calls[0].props["prov"]["confidence_tier"], "InferredStrong");
}

#[test]
fn column_assignments_on_an_annotated_model_instance_are_inferred_updates() {
    // AC-0884 (T-0884): `x.col = v` on a parameter annotated with a model
    // class, or a local bound once as `x: Model = …`, is an InferredStrong
    // update of that model (resolution "attribute-assignment"); an
    // unannotated or rebound name, or an attribute the model does not
    // declare as a column, is not.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/models.py",
        r#"
from sqlalchemy import Column, Integer, Boolean
from sqlalchemy.orm import declarative_base

Base = declarative_base()

class Alias(Base):
    __tablename__ = "alias"
    id = Column(Integer, primary_key=True)
    enabled = Column(Boolean)
    delete_on = Column(Integer)
"#,
    );
    write(
        dir.path(),
        "app/utils.py",
        r#"
from app.models import Alias

def trash(alias: Alias, days: int = 30):
    alias.delete_on = days
    alias.enabled = False

def toggle(alias_id):
    alias: Alias = Alias.get(alias_id)
    alias.enabled = not alias.enabled

def untyped(alias):
    alias.enabled = False

def rebound(alias: Alias):
    alias = other()
    alias.enabled = False

def not_a_column(alias: Alias):
    alias.cache = 1
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let writes = |function: &str| -> Vec<&Edge> {
        out.edges
            .iter()
            .filter(|e| e.label == "WRITES" && e.src.ends_with(&format!("app/utils.py#{function}")))
            .collect()
    };
    for function in ["trash", "toggle"] {
        let found = writes(function);
        assert_eq!(found.len(), 1, "{function}: {found:?}");
        assert_eq!(found[0].props["operation"], "update");
        assert_eq!(found[0].props["resolution"], "attribute-assignment");
        assert_eq!(found[0].props["prov"]["confidence_tier"], "InferredStrong");
        assert!(
            found[0].props["via"]
                .as_str()
                .unwrap()
                .starts_with("Alias.")
        );
    }
    for function in ["untyped", "rebound", "not_a_column"] {
        assert!(writes(function).is_empty(), "{function}");
    }
}

#[test]
fn a_self_call_in_a_method_reaches_the_class_method() {
    // AC-0885 (T-0885): `self.m()` in method `C.f` reaches `C.m` defined in
    // the same file, InferredStrong with resolution "self-method"; a member
    // the class does not define yields no edge.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/models.py",
        "class User:\n    def can_create(self):\n        return self.can_create_num(1) and self.missing()\n\n    def can_create_num(self, n):\n        return n < 5\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let calls: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|e| e.label == "CALLS" && e.src.ends_with("app/models.py#User.can_create"))
        .collect();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].dst.ends_with("app/models.py#User.can_create_num"));
    assert_eq!(calls[0].props["resolution"], "self-method");
    assert_eq!(calls[0].props["prov"]["confidence_tier"], "InferredStrong");
}

#[test]
fn a_delete_through_a_query_returning_classmethod_is_an_inferred_delete() {
    // AC-0900 (T-0900): `Model.filter(...).delete()` / `.update(...)`, where
    // the first class in Model's MRO defining `filter` is a classmethod whose
    // every return is `<proven session>.query(cls)` followed by query
    // builders, is an InferredStrong write of Model via that helper; a
    // classmethod returning a row (`.first()`), a chain with a row-returning
    // step after the helper or a query of another model is not.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/db.py",
        r#"
from sqlalchemy.orm import scoped_session, sessionmaker
Session = scoped_session(sessionmaker())
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

class ModelMixin(object):
    @classmethod
    def filter(cls, *args, **kw):
        return Session.query(cls).filter(*args, **kw)

    @classmethod
    def get_by(cls, **kw):
        return Session.query(cls).filter_by(**kw).first()

class Other(Base):
    __tablename__ = "other"
    id = Column(Integer, primary_key=True)

class Alias(Base, ModelMixin):
    __tablename__ = "alias"
    id = Column(Integer, primary_key=True)

    @classmethod
    def others(cls):
        return Session.query(Other)
"#,
    );
    write(
        dir.path(),
        "app/utils.py",
        r#"
from app.models import Alias

def remove(alias_id):
    Alias.filter(Alias.id == alias_id).delete()

def bump(alias_id):
    Alias.filter(Alias.id == alias_id).order_by(Alias.id).update({"id": 1})

def via_row(alias_id):
    Alias.get_by(id=alias_id).delete()

def via_other():
    Alias.others().delete()

def via_first(alias_id):
    Alias.filter(Alias.id == alias_id).first().delete()
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let writes = |function: &str| -> Vec<&Edge> {
        out.edges
            .iter()
            .filter(|e| e.label == "WRITES" && e.src.ends_with(&format!("app/utils.py#{function}")))
            .collect()
    };
    for (function, op) in [("remove", "delete"), ("bump", "update")] {
        let found = writes(function);
        assert_eq!(found.len(), 1, "{function}: {found:?}");
        assert!(
            found[0].dst.ends_with(":alias"),
            "{function}: {}",
            found[0].dst
        );
        assert_eq!(found[0].props["operation"], op);
        assert_eq!(found[0].props["resolution"], "model-helper");
        assert_eq!(found[0].props["via"], "ModelMixin.filter");
        assert_eq!(found[0].props["prov"]["confidence_tier"], "InferredStrong");
    }
    for function in ["via_row", "via_other", "via_first"] {
        assert!(
            writes(function).is_empty(),
            "{function}: {:?}",
            writes(function)
        );
    }
}

// AC-0916: a module's plain string constants are kept on its File node,
// except a rebound name or a secret-shaped value; a function's locals bound
// to module constants are kept on its Symbol.
#[test]
fn reply_constants_and_the_locals_bound_to_them_are_kept() {
    let facts = extract_source(
        br#"
E200 = "250 Message accepted for delivery"
E502 = "550 SL E502 Email not exist"
TWICE = "a"
TWICE = "b"
API_KEY = "sk-live-0123456789abcdef0123456789"
lower = "not a constant"
NESTED = "550 old"
if configured:
    NESTED = "550 new"

def handle(user):
    res_status = status.E200
    if user.reject:
        res_status = status.E502
    def inner():
        hidden = status.E999
    return res_status
"#,
        "app/email/status.py",
        &id(),
    )
    .unwrap();
    let file = facts.nodes.iter().find(|n| n.label == "File").unwrap();
    assert_eq!(
        file.props["string_constants"],
        serde_json::json!({
            "E200": "250 Message accepted for delivery",
            "E502": "550 SL E502 Email not exist",
        })
    );
    let handle = facts
        .nodes
        .iter()
        .find(|n| n.label == "Symbol" && n.props["name"] == "handle")
        .unwrap();
    let bound = handle.props["constant_locals"]["res_status"]
        .as_array()
        .unwrap();
    let values: Vec<&str> = bound.iter().map(|b| b["value"].as_str().unwrap()).collect();
    assert_eq!(values, ["status.E200", "status.E502"]);
    assert!(bound[0]["at"].as_u64() < bound[1]["at"].as_u64());
    assert_eq!(
        handle.props["constant_locals"].as_object().unwrap().len(),
        1
    );
}
