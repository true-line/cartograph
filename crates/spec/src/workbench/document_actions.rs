//! D1 §1.3 user actions (#2005, AC-0955): what a user does that reaches each
//! capability.
//!
//! A capability is named after the code that serves it. A reader also needs
//! the action that starts it: the button, form or link on a page. The graph
//! already holds those as edges from the page:
//!
//! - `SUBMITS` from a template's form (or a page's form that posts back) to
//!   an Endpoint the capability cites;
//! - `NAVIGATES_TO` from a template's link to a Screen whose `RENDERS`
//!   handler is one of the capability's handlers;
//! - `FETCHES` from a client component to an Endpoint the capability cites.
//!
//! Under each capability, every such action is one row: what the user does
//! (with the words the page shows, when the adapter recorded them), the page
//! it is on (the routes of the Screens rendering its template, else the
//! template or component), the operation or page it reaches and the edge's
//! citation. Rows on the same page sit together, so the options of one
//! action (a password form and the social sign-in links on a login page) are
//! read as one place. A redirect a handler returns is not a user action and
//! is not listed. A link or form whose target is a Gap (its route is not
//! recovered) is listed after the capabilities with the name it gives and
//! why, since no capability can claim it (AC-0976). Nothing is read from
//! source.

use crate::workbench::capabilities::{Stories, Story, Support};
use crate::workbench::citation_id;
use crate::workbench::render::{Assertions, Literal, NodeName, RenderLimit, Text};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Most action rows under one capability; the rest are counted.
pub(super) const MAX_ACTION_ROWS: usize = 24;

/// The graph indexes the section reads.
struct Index<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    /// Screen id → handler Symbols it renders.
    renders: BTreeMap<&'a str, Vec<&'a str>>,
    /// Handler Symbol → Screens rendering it.
    screens_of: BTreeMap<&'a str, Vec<&'a str>>,
    /// Template id → handler Symbols rendering it.
    rendered_by: BTreeMap<&'a str, Vec<&'a str>>,
    /// The action edges: `SUBMITS`, `NAVIGATES_TO` from a page, `FETCHES`.
    actions: Vec<&'a Edge>,
    /// Actions whose target is a Gap (AC-0976).
    unresolved: Vec<&'a Edge>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut index = Self {
            by_id,
            renders: BTreeMap::new(),
            screens_of: BTreeMap::new(),
            rendered_by: BTreeMap::new(),
            actions: Vec::new(),
            unresolved: Vec::new(),
        };
        for edge in edges {
            let (Some(src), Some(dst)) = (
                index.by_id.get(edge.src.as_str()).copied(),
                index.by_id.get(edge.dst.as_str()).copied(),
            ) else {
                continue;
            };
            match (edge.label.as_str(), src.label.as_str(), dst.label.as_str()) {
                ("RENDERS", "Screen", "Symbol") => {
                    index.renders.entry(&src.id).or_default().push(&dst.id);
                    index.screens_of.entry(&dst.id).or_default().push(&src.id);
                }
                ("RENDERS_TEMPLATE", "Symbol", "Template") => {
                    index.rendered_by.entry(&dst.id).or_default().push(&src.id);
                }
                ("SUBMITS", "Template" | "Symbol" | "Screen", "Endpoint")
                | ("NAVIGATES_TO", "Template" | "Screen", "Screen")
                | ("FETCHES", "Symbol" | "Screen", "Endpoint") => index.actions.push(edge),
                ("SUBMITS" | "NAVIGATES_TO", "Template", "Gap") => index.unresolved.push(edge),
                _ => {}
            }
        }
        index
    }

    /// The page an action sits on: the routes of the Screens rendering its
    /// template (or, for a form that posts back, its handler), else the
    /// template or component.
    fn page(&self, edge: &Edge, src: &Node) -> String {
        let handlers: Vec<&str> = match src.label.as_str() {
            "Template" => self
                .rendered_by
                .get(src.id.as_str())
                .cloned()
                .unwrap_or_default(),
            "Symbol" if edge.label == "SUBMITS" => vec![src.id.as_str()],
            _ => Vec::new(),
        };
        let mut routes: BTreeSet<String> = handlers
            .iter()
            .flat_map(|h| self.screens_of.get(h).into_iter().flatten())
            .filter_map(|screen| self.by_id.get(screen))
            .map(|screen| screen_name(screen))
            .collect();
        if src.label == "Screen" {
            routes.insert(screen_name(src));
        }
        if routes.is_empty() {
            let name = NodeName(src).to_string();
            match src.label.as_str() {
                "Template" => format!("template {name}"),
                "Symbol" => format!("component {name}"),
                _ => name,
            }
        } else {
            routes.into_iter().collect::<Vec<_>>().join(", ")
        }
    }
}

impl<'a> Index<'a> {
    /// The action edges that reach `story`'s operations or the pages its
    /// handlers render, each with its source and what it reaches.
    fn reaching<'s>(
        &'s self,
        story: &Story<'a>,
    ) -> impl Iterator<Item = (&'a Edge, &'a Node, String)> + 's {
        let (endpoints, handlers) = reached(story);
        self.actions.iter().filter_map(move |edge| {
            let src = *self.by_id.get(edge.src.as_str())?;
            let dst = *self.by_id.get(edge.dst.as_str())?;
            let reaches = if dst.label == "Endpoint" {
                endpoints
                    .contains(dst.id.as_str())
                    .then(|| NodeName(dst).to_string())
            } else {
                self.renders
                    .get(dst.id.as_str())
                    .into_iter()
                    .flatten()
                    .any(|h| handlers.contains(h))
                    .then(|| format!("page {}", screen_name(dst)))
            }?;
            Some((*edge, src, reaches))
        })
    }
}

/// A Screen by its route, else its name.
fn screen_name(screen: &Node) -> String {
    screen.props["route"]
        .as_str()
        .map_or_else(|| NodeName(screen).to_string(), str::to_owned)
}

/// The words recorded on an action's sites, each once, in order.
fn labels(edge: &Edge) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for site in edge.props["sites"].as_array().into_iter().flatten() {
        if let Some(label) = site["label"].as_str()
            && !out.contains(&label)
        {
            out.push(label);
        }
    }
    out
}

/// What the user does: `Press "Create"` for a form, `Follow "Log out"` for
/// a link, or the bare kind when no words were recorded.
fn action(edge: &Edge, src: &Node) -> String {
    let words = labels(edge)
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(" or ");
    let (verb, bare) = match edge.label.as_str() {
        "SUBMITS" => ("Submit", "Submit a form"),
        "NAVIGATES_TO" if src.label == "Screen" => ("Go to", "Navigate"),
        "NAVIGATES_TO" => ("Follow", "Follow a link"),
        _ => ("Request from", "Request from the page"),
    };
    if words.is_empty() {
        let method = edge.props["method"]
            .as_str()
            .map(|m| format!(" ({m})"))
            .unwrap_or_default();
        format!("{bare}{method}")
    } else {
        format!("{verb} {words}")
    }
}

/// A capability's Endpoints and handler Symbols.
fn reached<'a>(story: &Story<'a>) -> (BTreeSet<&'a str>, BTreeSet<&'a str>) {
    let mut endpoints = BTreeSet::new();
    let mut handlers = BTreeSet::new();
    for (_, fact) in &story.support {
        match fact {
            Support::Node(node) if node.label == "Endpoint" => {
                endpoints.insert(node.id.as_str());
                if let Some(handler) = node.props["handler_sym"].as_str() {
                    handlers.insert(handler);
                }
            }
            Support::Edge(edge) if edge.label == "HANDLES" => {
                endpoints.insert(edge.src.as_str());
                handlers.insert(edge.dst.as_str());
            }
            _ => {}
        }
    }
    (endpoints, handlers)
}

/// One action row: its page, what the user does, what it reaches, and the
/// edge.
type Row<'a> = (String, String, String, &'a Edge);

/// §1.3: the user actions that reach each capability. Returns the number of
/// facts cited.
pub(super) fn actions_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let index = Index::new(nodes, edges);
    let mut cited = reached_actions(&index, knowledge, (assertions, asserted), out)?;
    cited += unresolved_actions(&index, (assertions, asserted), out)?;
    Ok(cited)
}

/// Cite `edge` once in the document; returns its citation and whether it
/// was new.
fn cite(
    edge: &Edge,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
) -> Result<(String, bool), RenderLimit> {
    let key = FactKey::Edge {
        source: edge.src.clone(),
        label: edge.label.clone(),
        destination: edge.dst.clone(),
    };
    let citation = citation_id(&key);
    let new = asserted.insert(citation.clone());
    if new {
        assertions.edge(edge)?;
    }
    Ok((citation, new))
}

/// The capability blocks of §1.3's user actions.
fn reached_actions(
    index: &Index<'_>,
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    if index.actions.is_empty() {
        return Ok(0);
    }
    // First the counts, then each capability's rows built and printed in
    // turn, so at most one capability's rows are held at once.
    let mut total = 0usize;
    let mut reached_capabilities = 0usize;
    for (_, story) in knowledge.capabilities() {
        let count = index.reaching(story).count();
        if count > 0 {
            total += count;
            reached_capabilities += 1;
        }
    }
    if total == 0 {
        return Ok(0);
    }
    writeln!(
        out,
        "\nUser actions that reach each capability: {total} actions under \
         {reached_capabilities} capabilities. Each is a form, link or client request on a page \
         whose target is one of the capability's operations or a page its handlers render; the \
         words are those the page shows, where they were recorded. Actions on the same page are \
         listed together, so the ways of doing one thing (a form and the sign-in links beside \
         it) read as one place. That an action is shown to every user, or enabled, is not \
         established.\n"
    )?;
    let mut cited = 0usize;
    for (_, story) in knowledge.capabilities() {
        let mut rows: Vec<Row<'_>> = index
            .reaching(story)
            .map(|(edge, src, reaches)| (index.page(edge, src), action(edge, src), reaches, edge))
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by(|a, b| (&a.0, &a.2, &a.1).cmp(&(&b.0, &b.2, &b.1)));
        writeln!(
            out,
            "#### User actions: {}\n",
            Literal(NodeName(story.node))
        )?;
        out.push_str("| On | User action | Reaches | Citation |\n|---|---|---|---|\n")?;
        for (page, act, reaches, edge) in rows.iter().take(MAX_ACTION_ROWS) {
            let (citation, new) = cite(edge, (&mut *assertions, &mut *asserted))?;
            cited += usize::from(new);
            writeln!(
                out,
                "| {} | {} | {} | {citation} |",
                Literal(page),
                Literal(act),
                Literal(reaches)
            )?;
        }
        if rows.len() > MAX_ACTION_ROWS {
            writeln!(
                out,
                "\n{} more actions are left out.",
                rows.len() - MAX_ACTION_ROWS
            )?;
        }
        out.push_str("\n")?;
    }
    Ok(cited)
}

/// §1.3's user actions whose target is not recovered, when no derived
/// Capability is listed (so [`actions_per_capability`] does not run).
pub(super) fn unresolved_actions_only(
    (nodes, edges): (&[&Node], &[&Edge]),
    cited: (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    unresolved_actions(&Index::new(nodes, edges), cited, out)
}

/// §1.3's user actions whose target is not recovered: each Gap a page's
/// link or form points at, with the name it gives and why nothing answers
/// it (AC-0976).
fn unresolved_actions(
    index: &Index<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let mut rows: Vec<(String, String, String, String, &Edge)> = index
        .unresolved
        .iter()
        .filter_map(|edge| {
            let src = *index.by_id.get(edge.src.as_str())?;
            let gap = *index.by_id.get(edge.dst.as_str())?;
            let name = gap.props["endpoint_name"].as_str().unwrap_or("?");
            let method = gap.props["method"].as_str().unwrap_or("GET");
            let reason = gap.props["reason"]
                .as_str()
                .unwrap_or("the target is not recovered");
            Some((
                index.page(edge, src),
                action(edge, src),
                format!("{name} ({method})"),
                reason.to_owned(),
                *edge,
            ))
        })
        .collect();
    if rows.is_empty() {
        return Ok(0);
    }
    rows.sort_by(|a, b| (&a.0, &a.2, &a.1).cmp(&(&b.0, &b.2, &b.1)));
    writeln!(
        out,
        "#### User actions with no recovered target\n\nLinks and forms that name a route no \
         recovered operation answers: {}. Each is a Gap: what it reaches, and so which capability \
         it starts, is not established.\n",
        rows.len()
    )?;
    out.push_str("| On | User action | Names | Why | Citation |\n|---|---|---|---|---|\n")?;
    let mut cited = 0usize;
    for (page, act, names, reason, edge) in rows.iter().take(MAX_ACTION_ROWS) {
        let (citation, new) = cite(edge, (&mut *assertions, &mut *asserted))?;
        cited += usize::from(new);
        writeln!(
            out,
            "| {} | {} | {} | {} | {citation} |",
            Literal(page),
            Literal(act),
            Literal(names),
            Literal(reason)
        )?;
    }
    if rows.len() > MAX_ACTION_ROWS {
        writeln!(
            out,
            "\n{} more actions are left out.",
            rows.len() - MAX_ACTION_ROWS
        )?;
    }
    out.push_str("\n")?;
    Ok(cited)
}

/// The retained workspace of [`actions_per_capability`]: the id and render
/// indexes, the action list, one capability's reached sets and its rows of
/// text. `None` on overflow.
pub(crate) fn actions_per_capability_workspace(
    graph: flowtracer::TraceGraphBound,
) -> Option<usize> {
    use crate::workbench::accounting::{map_slots, set_slots, slots, sum};
    let rows = graph.rows;
    sum(&[
        map_slots::<&str, &Node>(rows)?,
        map_slots::<&str, Vec<&str>>(rows)?.checked_mul(3)?,
        slots::<&str>(rows)?.checked_mul(3)?,
        slots::<&Edge>(rows)?.checked_mul(2)?,
        slots::<(String, String, String, String, &Edge)>(rows)?,
        set_slots::<&str>(rows)?.checked_mul(2)?,
        slots::<(String, String, String, &Edge)>(rows)?,
        graph.bytes.checked_mul(4)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::render::Budget;
    use core_graph::overlay::BaseRef;

    const AUTH: &str = r#"
from flask import Blueprint, render_template, redirect, url_for

auth_bp = Blueprint("auth", __name__)

@auth_bp.route("/login", methods=["GET", "POST"])
def login():
    return render_template("auth/login.html")

@auth_bp.route("/github/login")
def github_login():
    return redirect("https://github.com/login/oauth/authorize")

@auth_bp.route("/logout")
def logout():
    return redirect(url_for(".login"))
"#;

    const MAIN: &str = r#"
from flask import Flask
from app.auth import auth_bp

app = Flask(__name__)
app.register_blueprint(auth_bp, url_prefix="/auth")
"#;

    const LOGIN: &str = r#"<form method="post">
  <input name="email">
  <button type="submit">Log in</button>
</form>
<a href="{{ url_for('auth.github_login') }}">Sign in with GitHub</a>
<a href="{{ url_for('auth.logout') }}">Log out</a>
"#;

    // AC-0955: under a capability, each form, link or client request that
    // reaches one of its operations or a page its handlers render is a row
    // with the page it is on, the words the page shows and what it reaches;
    // rows on one page sit together; a redirect a handler returns is not a
    // user action; a capability nothing reaches has no block.
    #[test]
    fn user_actions_are_listed_under_the_capability_they_reach() {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("app/__init__.py", ""),
            ("app/auth.py", AUTH),
            ("app/main.py", MAIN),
            ("templates/auth/login.html", LOGIN),
        ] {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let facts = adapters_lang_python::extract_dir(
            dir.path(),
            &adapters_lang_python::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        // "Log in" cites the login and GitHub operations; "Log out" its own.
        let handles = |suffixes: &[&str]| -> Vec<&Edge> {
            edges
                .iter()
                .copied()
                .filter(|e| e.label == "HANDLES" && suffixes.iter().any(|s| e.dst.ends_with(s)))
                .collect()
        };
        let login = handles(&["#login", "#github_login"]);
        let logout = handles(&["#logout"]);
        assert_eq!((login.len(), logout.len()), (3, 1), "{edges:#?}");
        let refs: Vec<(BaseRef, &Edge)> = login
            .iter()
            .chain(&logout)
            .map(|e| {
                (
                    BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone()),
                    *e,
                )
            })
            .collect();
        let caps = [
            Node {
                id: "capability:test:login".into(),
                label: "Capability".into(),
                props: serde_json::json!({ "title": "Log in" }),
            },
            Node {
                id: "capability:test:logout".into(),
                label: "Capability".into(),
                props: serde_json::json!({ "title": "Log out" }),
            },
        ];
        let story = |cap: &'static str| -> Vec<(&BaseRef, Support<'_>)> {
            refs.iter()
                .filter(|(_, e)| (cap == "login") != e.dst.ends_with("#logout"))
                .map(|(r, e)| (r, Support::Edge(e)))
                .collect()
        };
        let knowledge = Stories {
            included: vec![
                Story {
                    node: &caps[0],
                    support: story("login"),
                },
                Story {
                    node: &caps[1],
                    support: story("logout"),
                },
            ],
            filtered: 0,
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
        };
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        let cited = actions_per_capability(
            (&nodes, &edges),
            &knowledge,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        let text = out.finish();
        assert_eq!(cited, 3, "{text}");
        assert!(text.contains("3 actions under 2 capabilities"), "{text}");
        let block = |title: &str| {
            let start = text.find(&format!("#### User actions: {title}\n")).unwrap();
            let rest = &text[start + 1..];
            rest[..rest.find("\n####").unwrap_or(rest.len())].to_owned()
        };
        let login = block("Log in");
        assert!(
            login.contains("| /auth/login | Submit \"Log in\" | POST /auth/login |"),
            "{login}"
        );
        assert!(
            login.contains(
                "| /auth/login | Follow \"Sign in with GitHub\" | page /auth/github/login |"
            ),
            "{login}"
        );
        // The logout redirect a handler returns is not an action.
        let logout = block("Log out");
        assert!(
            logout.contains("| /auth/login | Follow \"Log out\" | page /auth/logout |"),
            "{logout}"
        );
        assert_eq!(logout.matches("| /auth").count(), 1, "{logout}");
    }

    // AC-0976: a link or form whose target is a Gap is listed after the
    // capabilities, with its page, its words, the name it gives and why no
    // operation answers it, citing its edge; with no such Gap there is no
    // block.
    #[test]
    fn actions_with_no_recovered_target_are_listed_as_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let login =
            format!("{LOGIN}<a href=\"{{{{ url_for('auth.forgot') }}}}\">Forgot password?</a>\n");
        for (path, text) in [
            ("app/__init__.py", ""),
            ("app/auth.py", AUTH),
            ("app/main.py", MAIN),
            ("templates/auth/login.html", login.as_str()),
        ] {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let facts = adapters_lang_python::extract_dir(
            dir.path(),
            &adapters_lang_python::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let render = |nodes: &[&Node], edges: &[&Edge]| {
            let knowledge = Stories {
                included: Vec::new(),
                filtered: 0,
                areas: BTreeMap::new(),
                file_areas: BTreeMap::new(),
            };
            let budget = Budget::new(None).unwrap();
            let mut assertions = Assertions::new(&budget);
            let mut asserted = BTreeSet::new();
            let mut out = Text::new(&budget, "").unwrap();
            let cited = actions_per_capability(
                (nodes, edges),
                &knowledge,
                (&mut assertions, &mut asserted),
                &mut out,
            )
            .unwrap();
            (cited, out.finish())
        };
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let (cited, text) = render(&nodes, &edges);
        assert_eq!(cited, 1, "{text}");
        assert!(
            text.contains("#### User actions with no recovered target\n\nLinks and forms that name a route no \
                 recovered operation answers: 1."),
            "{text}"
        );
        assert!(
            text.contains(
                "| /auth/login | Follow \"Forgot password?\" | auth.forgot (GET) | no recovered \
                 route has this endpoint name |"
            ),
            "{text}"
        );
        // Without the Gap edge there is no block.
        let resolved: Vec<&Edge> = edges
            .iter()
            .copied()
            .filter(|e| !e.dst.starts_with("gap:user-action:"))
            .collect();
        let (cited, text) = render(&nodes, &resolved);
        assert_eq!((cited, text.as_str()), (0, ""));
        // Without derived Capabilities the block is still rendered.
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        let cited =
            unresolved_actions_only((&nodes, &edges), (&mut assertions, &mut asserted), &mut out)
                .unwrap();
        let only = out.finish();
        assert_eq!(cited, 1, "{only}");
        assert!(only.contains("Forgot password?"), "{only}");
    }

    #[test]
    fn action_workspace_is_checked() {
        let graph = |rows, bytes, values| flowtracer::TraceGraphBound {
            rows,
            bytes,
            values,
        };
        assert!(actions_per_capability_workspace(graph(1 << 10, 16, 64)).is_some());
        assert!(actions_per_capability_workspace(graph(usize::MAX, 1, 1)).is_none());
    }

    // A client request from a component that a Screen also renders sits on
    // the component, not on the Screen's route; a form posting back to its
    // handler sits on that handler's page.
    #[test]
    fn only_a_postback_form_takes_its_handlers_page() {
        let node = |id: &str, label: &str, props| Node {
            id: id.into(),
            label: label.into(),
            props,
        };
        let edge = |src: &str, label: &str, dst: &str| Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({}),
        };
        let nodes = [
            node(
                "screen:r@flask:/login",
                "Screen",
                serde_json::json!({ "route": "/login" }),
            ),
            node(
                "sym:r@a.py#login",
                "Symbol",
                serde_json::json!({ "name": "login" }),
            ),
            node(
                "endpoint:r@POST /login",
                "Endpoint",
                serde_json::json!({ "method": "POST", "path": "/login" }),
            ),
        ];
        let edges = [
            edge("screen:r@flask:/login", "RENDERS", "sym:r@a.py#login"),
            edge("sym:r@a.py#login", "SUBMITS", "endpoint:r@POST /login"),
            edge("sym:r@a.py#login", "FETCHES", "endpoint:r@POST /login"),
        ];
        let nodes: Vec<&Node> = nodes.iter().collect();
        let edge_refs: Vec<&Edge> = edges.iter().collect();
        let index = Index::new(&nodes, &edge_refs);
        let handler = nodes[1];
        assert_eq!(index.page(&edges[1], handler), "/login");
        assert!(
            index.page(&edges[2], handler).starts_with("component "),
            "{}",
            index.page(&edges[2], handler)
        );
    }
}
