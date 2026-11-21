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

// AC-0854: a helper is Flask's only where the call is: a parameter, local
// assignment or local import of the same name shadows it, a function-local
// Flask import does not leak into another function, a template's
// `safe_url_for(…)` is not `url_for`, and a non-identifier endpoint name
// resolves.
#[test]
fn flask_helpers_resolve_at_the_call_site_and_endpoint_names_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/main.py",
        r#"
from flask import Flask, render_template

app = Flask(__name__)

def own_render(name):
    return name

@app.route("/a")
def a(render_template):
    return render_template("a.html")

@app.route("/b")
def b():
    render_template = own_render
    return render_template("b.html")

@app.route("/c")
def c():
    from myapp.views import render_template
    return render_template("c.html")

@app.route("/d")
def d():
    from flask import redirect, url_for
    return redirect(url_for("reset-password"))

@app.route("/e")
def e():
    return redirect(url_for("reset-password"))

@app.route("/f")
def f():
    return render_template("f.html")

@app.route("/reset", endpoint="reset-password")
def reset():
    return render_template("f.html")
"#,
    );
    write(
        dir.path(),
        "templates/f.html",
        r#"<a href="{{ safe_url_for('a') }}">x</a>
<a href="{{ url_for('reset-password') }}">reset</a>
"#,
    );
    for name in ["a", "b", "c"] {
        write(dir.path(), &format!("templates/{name}.html"), "<p></p>");
    }
    let out = extract_dir(dir.path(), &id()).unwrap();
    let rendered: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "RENDERS_TEMPLATE")
        .map(|e| e.src.rsplit('#').next().unwrap())
        .collect();
    assert_eq!(
        rendered
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        ["f", "reset"].into()
    );
    edge(&out.edges, "#d", "NAVIGATES_TO", "@flask:/reset");
    assert!(
        !out.edges
            .iter()
            .any(|e| e.src.ends_with("#e") && e.label == "NAVIGATES_TO")
    );
    let from_template: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.src.starts_with("template:") && e.label == "NAVIGATES_TO")
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(from_template.len(), 1, "{from_template:?}");
    assert!(from_template[0].ends_with("@flask:/reset"));
}

// AC-0954: a link's site records its visible text and a form's site its
// submit control's text or value; template expressions, tags and
// token-shaped words are not kept.
#[test]
fn template_sites_record_what_a_reader_sees() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "app/auth.py", AUTH);
    write(dir.path(), "app/helpers.py", HELPERS);
    write(dir.path(), "app/dashboard.py", DASHBOARD);
    write(dir.path(), "app/main.py", MAIN);
    write(dir.path(), "templates/auth/login.html", LOGIN);
    write(dir.path(), "templates/base.html", BASE);
    write(
        dir.path(),
        "templates/dashboard/index.html",
        r#"{% extends 'base.html' %}
<form action="{{ url_for('dashboard.create_alias') }}" method="POST">
  <input name="note">
  <button type="submit"><i class="fa fa-plus"></i> Create {{ _("alias") }}</button>
</form>
<form action="{{ url_for('dashboard.create_alias') }}" method="POST">
  <input type="submit" value="Quick create">
</form>
<a href="{{ url_for('auth.logout') }}">{{ user.name }}</a>
<a href="{{ url_for('dashboard.index') }}">ghp_0123456789abcdefghijklmnopqrstuvwxyzAB</a>
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let labels = |edge: &Edge| -> Vec<Option<String>> {
        edge.props["sites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["label"].as_str().map(str::to_owned))
            .collect()
    };
    let submit = edge(
        &out.edges,
        "dashboard/index.html",
        "SUBMITS",
        "/dashboard/aliases",
    );
    assert_eq!(
        labels(submit),
        [Some("Create".to_owned()), Some("Quick create".to_owned())]
    );
    let logout = edge(
        &out.edges,
        "base.html",
        "NAVIGATES_TO",
        "@flask:/auth/logout",
    );
    assert_eq!(labels(logout), [Some("Log out".to_owned())]);
    // Only an expression, or a token: no label.
    let own = edge(
        &out.edges,
        "dashboard/index.html",
        "NAVIGATES_TO",
        "@flask:/auth/logout",
    );
    assert_eq!(labels(own), [None]);
    let token = edge(
        &out.edges,
        "dashboard/index.html",
        "NAVIGATES_TO",
        "@flask:/dashboard/",
    );
    assert_eq!(labels(token), [None]);
}
