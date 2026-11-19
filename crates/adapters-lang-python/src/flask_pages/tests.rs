use crate::{SourceId, extract_dir};
use core_graph::Edge;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/pages",
        commit: "abc123",
    }
}

fn write(dir: &std::path::Path, path: &str, source: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
}

const AUTH: &str = r#"
from flask import Blueprint, redirect, render_template, url_for
from app.helpers import after_login

auth_bp = Blueprint("auth", __name__)

@auth_bp.route("/login", methods=["GET", "POST"])
def login():
    if form.validate_on_submit():
        return after_login(user)
    return render_template("auth/login.html")

@auth_bp.route("/logout")
def logout():
    return redirect(url_for(".login"))
"#;

const HELPERS: &str = r#"
from flask import redirect, url_for

def after_login(user):
    return redirect(url_for("dashboard.index"))
"#;

const DASHBOARD: &str = r#"
import flask
from flask import Blueprint, jsonify

dashboard_bp = Blueprint("dashboard", __name__)

@dashboard_bp.route("/")
def index():
    return flask.render_template("dashboard/index.html")

@dashboard_bp.route("/aliases", methods=["POST"])
def create_alias():
    return jsonify(ok=True)

@dashboard_bp.route("/stats")
def stats():
    return jsonify(count=1)
"#;

const MAIN: &str = r#"
from flask import Flask, redirect, url_for
from app.auth import auth_bp
from app.dashboard import dashboard_bp

app = Flask(__name__)
app.register_blueprint(auth_bp, url_prefix="/auth")
app.register_blueprint(dashboard_bp, url_prefix="/dashboard")

@app.route("/")
def home():
    if current_user.is_authenticated:
        return redirect(url_for("dashboard.index"))
    return redirect(url_for("auth.login"))
"#;

const LOGIN: &str = r#"{% extends "base.html" %}
{# <a href="{{ url_for('dashboard.stats') }}">commented out</a> #}
{% block content %}
<form method="post" class="card">
  <input name="email">
</form>
<a class="small" href="{{ url_for("auth.register") }}">Sign up</a>
{% endblock %}
"#;

const BASE: &str = r#"<html><body>
{% if current_user.is_authenticated %}
  <a href="{{ url_for('dashboard.index') }}" class="nav">Dashboard — home</a>
  <a href="{{ url_for('auth.logout') }}">Log out</a>
{% endif %}
<a href="/static/help.html">Help</a>
{% block content %}{% endblock %}
</body></html>
"#;

const INDEX: &str = r#"{% extends 'base.html' %}
{% block content %}
<form action="{{ url_for('dashboard.create_alias') }}" method="POST">
  <button>Create</button>
</form>
<form action="{{ url_for('dashboard.index') }}">
  <input name="query">
</form>
<a href="{{ url_for('dashboard.stats') }}">Stats</a>
{% endblock %}
"#;

fn edge<'a>(edges: &'a [Edge], src: &str, label: &str, dst: &str) -> &'a Edge {
    edges
        .iter()
        .find(|e| e.src.ends_with(src) && e.label == label && e.dst.ends_with(dst))
        .unwrap_or_else(|| panic!("{src} {label} {dst}"))
}

fn sites(edge: &Edge) -> Vec<(String, bool)> {
    edge.props["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["path"].as_str().unwrap().to_owned(),
                s["conditional"].as_bool().unwrap(),
            )
        })
        .collect()
}

// AC-0854: a Flask application's GET pages are Screens; a redirect to a
// named route, a template link and a form are navigations or submissions
// between them, each citing its site.
#[test]
fn flask_pages_become_screens_linked_by_redirects_links_and_forms() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "app/auth.py", AUTH);
    write(dir.path(), "app/helpers.py", HELPERS);
    write(dir.path(), "app/dashboard.py", DASHBOARD);
    write(dir.path(), "app/main.py", MAIN);
    write(dir.path(), "templates/auth/login.html", LOGIN);
    write(dir.path(), "templates/base.html", BASE);
    write(dir.path(), "templates/dashboard/index.html", INDEX);
    let out = extract_dir(dir.path(), &id()).unwrap();

    // Pages: GET routes answering with a template or a redirect; the JSON
    // `stats` route and the POST-only route are not.
    let mut screens: Vec<(&str, &str)> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| {
            assert_eq!(n.props["router"], "flask");
            assert_eq!(n.props["server_rendered"], true);
            (
                n.props["route"].as_str().unwrap(),
                n.props["name"].as_str().unwrap(),
            )
        })
        .collect();
    screens.sort_unstable();
    assert_eq!(
        screens,
        [
            ("/", "home"),
            ("/auth/login", "auth.login"),
            ("/auth/logout", "auth.logout"),
            ("/dashboard/", "dashboard.index"),
        ]
    );
    let edges = &out.edges;
    edge(edges, "@flask:/auth/login", "RENDERS", "app/auth.py#login");

    // Redirects: conditional in `home`, unconditional in a helper, and a
    // Blueprint-relative `.login`.
    let home = edge(edges, "#home", "NAVIGATES_TO", "@flask:/dashboard/");
    assert_eq!(sites(home), [("app/main.py".into(), true)]);
    edge(edges, "#home", "NAVIGATES_TO", "@flask:/auth/login");
    let helper = edge(edges, "#after_login", "NAVIGATES_TO", "@flask:/dashboard/");
    assert_eq!(helper.props["conditional"], false);
    edge(edges, "#logout", "NAVIGATES_TO", "@flask:/auth/login");

    // Templates: each handler renders its own; `extends` is an include.
    edge(
        edges,
        "#login",
        "RENDERS_TEMPLATE",
        "templates/auth/login.html",
    );
    edge(
        edges,
        "#index",
        "RENDERS_TEMPLATE",
        "templates/dashboard/index.html",
    );
    edge(edges, "auth/login.html", "INCLUDES", "templates/base.html");
    edge(
        edges,
        "dashboard/index.html",
        "INCLUDES",
        "templates/base.html",
    );
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "Template" && n.props["path"] == "templates/base.html")
    );

    // Links: only to pages, conditional inside `{% if %}`; a commented-out
    // link, a plain `href`, an unknown name and a JSON route are not.
    let nav = edge(
        edges,
        "templates/base.html",
        "NAVIGATES_TO",
        "@flask:/dashboard/",
    );
    assert_eq!(sites(nav), [("templates/base.html".into(), true)]);
    edge(
        edges,
        "templates/base.html",
        "NAVIGATES_TO",
        "@flask:/auth/logout",
    );
    let from_templates: Vec<&str> = edges
        .iter()
        .filter(|e| e.src.starts_with("template:") && e.label == "NAVIGATES_TO")
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(from_templates.len(), 3, "{from_templates:?}");

    // Forms: a POST form submits to its named route; a GET form navigates;
    // an action-less POST form posts back to the page that rendered it.
    let create = edge(
        edges,
        "dashboard/index.html",
        "SUBMITS",
        "@POST:/dashboard/aliases",
    );
    assert_eq!(create.props["method"], "POST");
    let search = edge(
        edges,
        "dashboard/index.html",
        "NAVIGATES_TO",
        "@flask:/dashboard/",
    );
    assert_eq!(sites(search).len(), 1);
    let back = edge(edges, "#login", "SUBMITS", "@POST:/auth/login");
    assert_eq!(sites(back), [("templates/auth/login.html".into(), false)]);
    let span = &back.props["sites"][0];
    let start = span["byte_start"].as_u64().unwrap() as usize;
    let end = span["byte_end"].as_u64().unwrap() as usize;
    assert_eq!(&LOGIN[start..end], r#"<form method="post" class="card">"#);
}

// AC-0854: a template name more than one `templates/` directory holds is
// not resolved, and a single-file extraction still records pages and
// redirects.
#[test]
fn ambiguous_templates_record_nothing_and_single_files_keep_redirects() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "app/main.py", MAIN);
    write(dir.path(), "app/auth.py", AUTH);
    write(dir.path(), "app/helpers.py", HELPERS);
    write(dir.path(), "a/templates/auth/login.html", LOGIN);
    write(dir.path(), "b/templates/auth/login.html", LOGIN);
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(!out.edges.iter().any(|e| e.label == "RENDERS_TEMPLATE"));
    assert!(!out.nodes.iter().any(|n| n.label == "Template"));

    let single = crate::extract_source(MAIN.as_bytes(), "app/main.py", &id()).unwrap();
    assert!(
        single
            .nodes
            .iter()
            .any(|n| n.label == "Screen" && n.props["route"] == "/")
    );
}
