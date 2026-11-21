//! Server-rendered Flask pages for journey recovery (AC-0854).
//!
//! A Flask application's pages are its `GET` routes that answer with a
//! rendered template or a redirect. Each such route becomes a `Screen`
//! (`screen:{repo}@flask:{path}`, `router: "flask"`, `server_rendered: true`)
//! that `RENDERS` its handler, so `journey-candidate@1` can walk from page to
//! page the way a person clicks through the application:
//!
//! - `redirect(url_for("bp.name"))` in a function, with Flask's `redirect`
//!   and `url_for` imported and the endpoint name a literal (`.name` resolves
//!   to the function's own Blueprint when it handles routes of exactly one),
//!   is a `NAVIGATES_TO` from that function to each page of that name.
//! - `render_template("x.html")` with a literal name is a
//!   `RENDERS_TEMPLATE` from the function to the one template file whose
//!   path below a `templates/` directory is `x.html` (none, or more than
//!   one, records nothing). A template `INCLUDES` what it `{% extends %}`
//!   or `{% include %}`s by a literal name.
//! - In a template, `<a href="{{ url_for('bp.name') }}">` is a
//!   `NAVIGATES_TO` to each page of that name; `<form action="{{
//!   url_for('bp.name') }}" method="post">` `SUBMITS` to that name's route
//!   with the form's method (`GET` when absent: a `GET` form navigates).
//!   A `method="post"` form with no `action` posts back to the page that
//!   rendered it, so it `SUBMITS` from the rendering handler to the `POST`
//!   route at the page's own path.
//!
//! Every navigation and submission lists its `sites`, each with its span
//! and whether it is `conditional` (under a Python branch, loop, `try` or
//! `with`, or inside a template `{% if %}` / `{% for %}` block). Names no
//! route answers record nothing. `INCLUDES`, `RENDERS_TEMPLATE` and
//! `SUBMITS` are read only by journey recovery, so traced server flows and
//! other artifacts are unchanged.
use crate::{Extraction, FileCx, literal_string, prov_from};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node as TsNode;

/// What one function call says about the page a handler serves.
#[derive(Debug, Clone)]
pub(crate) struct PageFact {
    function: String,
    kind: PageKind,
    evidence: EvidenceRef,
    conditional: bool,
}

#[derive(Debug, Clone)]
enum PageKind {
    /// `render_template("name")`.
    Render(String),
    /// `redirect(url_for("endpoint"))`.
    Redirect(String),
}

const NAMES: [&str; 3] = ["render_template", "redirect", "url_for"];

/// True for nodes that open a scope of their own: their bindings and
/// imports are not the enclosing scope's.
fn opens_scope(node: TsNode<'_>) -> bool {
    matches!(
        node.kind(),
        "function_definition"
            | "lambda"
            | "class_definition"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression"
    )
}

/// The statements of `scope` itself, not of the scopes nested in it.
fn own_nodes<'t>(scope: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        out.push(node);
        let mut cursor = node.walk();
        stack.extend(
            node.named_children(&mut cursor)
                .filter(|child| !opens_scope(*child)),
        );
    }
    out
}

/// Names `scope`'s own imports bind: each to the Flask helper it is, or
/// `None` when it binds something else. A `flask` module alias binds
/// `alias.render_template` and its siblings.
fn scope_imports(cx: &FileCx<'_>, scope: TsNode<'_>) -> BTreeMap<String, Option<&'static str>> {
    let mut out = BTreeMap::new();
    for node in own_nodes(scope) {
        match node.kind() {
            "import_from_statement" => {
                let module = node
                    .child_by_field_name("module_name")
                    .map(|m| cx.text(&m))
                    .unwrap_or_default();
                let flask = matches!(module, "flask" | "flask.helpers" | "flask.templating");
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    let (exported, local) = match name.kind() {
                        "aliased_import" => (
                            name.child_by_field_name("name").map(|n| cx.text(&n)),
                            name.child_by_field_name("alias").map(|n| cx.text(&n)),
                        ),
                        _ => (Some(cx.text(&name)), Some(cx.text(&name))),
                    };
                    let (Some(exported), Some(local)) = (exported, local) else {
                        continue;
                    };
                    let helper = NAMES.iter().find(|n| **n == exported).copied();
                    out.insert(local.to_owned(), helper.filter(|_| flask));
                }
            }
            "import_statement" => {
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    let (module, local) = match name.kind() {
                        "aliased_import" => (
                            name.child_by_field_name("name").map(|n| cx.text(&n)),
                            name.child_by_field_name("alias").map(|n| cx.text(&n)),
                        ),
                        _ => {
                            let text = cx.text(&name);
                            (Some(text), text.split('.').next())
                        }
                    };
                    let (Some(module), Some(local)) = (module, local) else {
                        continue;
                    };
                    out.insert(local.to_owned(), None);
                    if module == "flask" {
                        for helper in NAMES {
                            out.insert(format!("{local}.{helper}"), Some(helper));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The identifiers an assignment, loop or `as` target binds.
fn target_names<'s>(cx: &'s FileCx<'s>, target: TsNode<'_>, out: &mut BTreeSet<&'s str>) {
    match target.kind() {
        "identifier" => {
            out.insert(cx.text(&target));
        }
        "pattern_list"
        | "tuple_pattern"
        | "list_pattern"
        | "tuple"
        | "list"
        | "parenthesized_expression"
        | "list_splat_pattern"
        | "list_splat"
        | "as_pattern_target" => {
            let mut cursor = target.walk();
            for child in target.named_children(&mut cursor) {
                target_names(cx, child, out);
            }
        }
        _ => {}
    }
}

/// Names `function` binds itself other than by import: its parameters,
/// assignment, loop, `with` / `except` and walrus targets, and the
/// functions and classes it defines.
fn rebound<'s>(cx: &'s FileCx<'s>, function: TsNode<'_>) -> BTreeSet<&'s str> {
    let mut out = BTreeSet::new();
    if let Some(parameters) = function.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let name = match parameter.kind() {
                "identifier" => Some(parameter),
                "default_parameter" | "typed_default_parameter" => {
                    parameter.child_by_field_name("name")
                }
                _ => {
                    let mut inner = parameter.walk();
                    let found = parameter.named_children(&mut inner).find(|c| {
                        c.kind() == "identifier"
                            || matches!(c.kind(), "list_splat_pattern" | "dictionary_splat_pattern")
                    });
                    found.and_then(|c| {
                        if c.kind() == "identifier" {
                            Some(c)
                        } else {
                            let mut deeper = c.walk();
                            c.named_children(&mut deeper)
                                .find(|d| d.kind() == "identifier")
                        }
                    })
                }
            };
            if let Some(name) = name {
                out.insert(cx.text(&name));
            }
        }
    }
    let Some(body) = function.child_by_field_name("body") else {
        return out;
    };
    for node in own_nodes(body) {
        match node.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                if let Some(left) = node.child_by_field_name("left") {
                    target_names(cx, left, &mut out);
                }
            }
            "named_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    target_names(cx, name, &mut out);
                }
            }
            "as_pattern" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    target_names(cx, alias, &mut out);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(child.kind(), "function_definition" | "class_definition")
                && let Some(name) = child.child_by_field_name("name")
            {
                out.insert(cx.text(&name));
            }
        }
    }
    out
}

/// Record each function's literal `render_template` and
/// `redirect(url_for(…))` calls, skipping nested functions, lambdas and
/// classes, which are their own owners. A helper is Flask's where the call
/// is: imported from Flask at module level or in the function itself, and
/// not rebound by the function (AC-0854).
pub(crate) fn collect(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    out: &mut Extraction,
) {
    let module = scope_imports(cx, root);
    for (function, symbol) in functions {
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let mut names = module.clone();
        for name in rebound(cx, *function) {
            names.retain(|key, _| key != name && !key.starts_with(&format!("{name}.")));
        }
        for (local, helper) in scope_imports(cx, body) {
            names.retain(|key, _| key != &local && !key.starts_with(&format!("{local}.")));
            names.insert(local, helper);
        }
        if names.values().all(Option::is_none) {
            continue;
        }
        let flask = |call: TsNode<'_>| {
            let callee = call.child_by_field_name("function")?;
            names.get(cx.text(&callee)).copied().flatten()
        };
        let first_literal = |call: TsNode<'_>| {
            let arguments = call.child_by_field_name("arguments")?;
            let mut cursor = arguments.walk();
            let first = arguments.named_children(&mut cursor).next()?;
            literal_string(cx, first)
        };
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                "function_definition" | "lambda" | "class_definition"
            ) {
                continue;
            }
            if node.kind() == "call" {
                let kind = match flask(node) {
                    Some("render_template") => first_literal(node).map(PageKind::Render),
                    Some("redirect") => node
                        .child_by_field_name("arguments")
                        .and_then(|arguments| {
                            let mut cursor = arguments.walk();
                            arguments.named_children(&mut cursor).next()
                        })
                        .filter(|inner| inner.kind() == "call" && flask(*inner) == Some("url_for"))
                        .and_then(first_literal)
                        .map(PageKind::Redirect),
                    _ => None,
                };
                if let Some(kind) = kind {
                    out.page_facts.push(PageFact {
                        function: symbol.clone(),
                        kind,
                        evidence: cx.evidence(&node),
                        conditional: conditional(node, body),
                    });
                }
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
    }
}

/// True when `call` runs only on some paths through `body`.
fn conditional(call: TsNode<'_>, body: TsNode<'_>) -> bool {
    let mut node = call.parent();
    while let Some(current) = node {
        if current.id() == body.id() {
            return false;
        }
        if matches!(
            current.kind(),
            "if_statement"
                | "match_statement"
                | "for_statement"
                | "while_statement"
                | "try_statement"
                | "with_statement"
                | "conditional_expression"
                | "boolean_operator"
                | "list_comprehension"
                | "set_comprehension"
                | "dictionary_comprehension"
                | "generator_expression"
        ) {
            return true;
        }
        node = current.parent();
    }
    true
}

pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    for fact in &mut extraction.page_facts {
        fact.evidence.commit_sha = commit.into();
    }
}

/// One static site in a template: a link, a form, or an `extends` /
/// `include`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplateSite {
    Link(String),
    Form {
        action: Option<String>,
        method: String,
    },
    Includes(String),
}

/// A parsed template file.
struct Template {
    path: String,
    len: usize,
    sites: Vec<(TemplateSite, usize, usize, bool)>,
    /// The words a reader sees on a link or a form's submit control, by the
    /// site's start (#2005).
    labels: BTreeMap<usize, String>,
}

/// Most characters of a visible label kept.
const MAX_LABEL_CHARS: usize = 80;

/// The visible words of `html`: tags and template expressions removed,
/// whitespace collapsed, at most [`MAX_LABEL_CHARS`]; `None` when nothing
/// readable is left or what is left is token-shaped.
fn visible_words(html: &str) -> Option<String> {
    let mut words = String::new();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        let rest = &html[i..];
        let skip = if rest.starts_with("{{") {
            rest.find("}}").map(|e| e + 2)
        } else if rest.starts_with("{%") {
            rest.find("%}").map(|e| e + 2)
        } else if rest.starts_with("{#") {
            rest.find("#}").map(|e| e + 2)
        } else if rest.starts_with('<') {
            rest.find('>').map(|e| e + 1)
        } else {
            None
        };
        if let Some(skip) = skip {
            words.push(' ');
            i += skip;
            continue;
        }
        if rest.starts_with(['{', '<']) {
            break;
        }
        let c = rest.chars().next().unwrap_or(' ');
        words.push(c);
        i += c.len_utf8();
    }
    let words = words
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let words: String = words.chars().take(MAX_LABEL_CHARS).collect();
    (words.chars().any(char::is_alphabetic)
        && !words.split_whitespace().any(core_redact::is_token_shaped))
    .then_some(words)
}

/// The label of a link (its text) or a form (its first submit button's text
/// or value) whose opening tag ends at `end`.
fn site_label(text: &str, end: usize, form: bool) -> Option<String> {
    let lower = text[end..].to_ascii_lowercase();
    let close = if form { "</form" } else { "</a" };
    let body = &text[end..end + lower.find(close)?];
    if !form {
        return visible_words(body);
    }
    let lower = body.to_ascii_lowercase();
    if let Some(at) = lower.find("<button") {
        let open = tag_end(body, at)?;
        let close = lower[open..].find("</button")? + open;
        if let Some(words) = visible_words(&body[open..close]) {
            return Some(words);
        }
    }
    let mut from = 0;
    while let Some(at) = lower[from..].find("<input").map(|a| a + from) {
        let open = tag_end(body, at)?;
        let tag = &body[at..open];
        if attribute(tag, "type").is_some_and(|t| t.eq_ignore_ascii_case("submit")) {
            return attribute(tag, "value").and_then(visible_words);
        }
        from = open;
    }
    None
}

/// Template files below a `templates/` directory, keyed by the name
/// `render_template` uses: the path after the last `templates/`.
fn template_files(root: &Path) -> BTreeMap<String, Vec<String>> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "__pycache__" | "venv" | "site-packages" | "node_modules" | "dist" | "build"
            )
    };
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(files) = source_walk::files(root, &skip, source_walk::Gitignores::Honor) else {
        return out;
    };
    for file in files {
        let rel = file.rel;
        if !(rel.ends_with(".html") || rel.ends_with(".jinja") || rel.ends_with(".j2")) {
            continue;
        }
        let key = match rel.rfind("templates/") {
            Some(at) if at == 0 || rel[..at].ends_with('/') => &rel[at + "templates/".len()..],
            _ => continue,
        };
        out.entry(key.to_owned()).or_default().push(rel.clone());
    }
    out
}

/// The literal endpoint name of the first `url_for(…)` call in `text`: the
/// name itself, not the tail of a longer one such as `safe_url_for` or
/// `helpers.url_for`. Any non-empty literal is a name; only a route that
/// declares it resolves (AC-0854).
fn url_for_name(text: &str) -> Option<String> {
    const CALL: &str = "url_for(";
    let mut from = 0;
    let rest = loop {
        let at = from + text[from..].find(CALL)?;
        from = at + CALL.len();
        let before = text[..at].bytes().next_back();
        if !before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') {
            break text[from..].trim_start();
        }
    };
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    let name = &rest[..end];
    (!name.is_empty() && !name.contains(['\\', '{', '}'])).then(|| name.to_owned())
}

/// The value of attribute `name` in a tag's text, quoted or bare.
fn attribute<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = lower[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let offset = tag.len() - rest.len();
        let value = tag[offset..].trim_start();
        let start = tag.len() - value.len();
        return Some(match value.as_bytes().first() {
            Some(&quote @ (b'"' | b'\'')) => {
                // A Jinja expression may hold the other quote.
                let bytes = value.as_bytes();
                let mut depth = 0usize;
                let mut end = bytes.len();
                let mut i = 1;
                while i < bytes.len() {
                    let rest = &bytes[i..];
                    if rest.starts_with(b"{{") || rest.starts_with(b"{%") {
                        depth += 1;
                        i += 2;
                    } else if depth > 0 && (rest.starts_with(b"}}") || rest.starts_with(b"%}")) {
                        depth -= 1;
                        i += 2;
                    } else if depth == 0 && bytes[i] == quote {
                        end = i;
                        break;
                    } else {
                        i += 1;
                    }
                }
                &tag[start + 1..start + end]
            }
            _ => {
                let end = value
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(value.len());
                &tag[start..start + end]
            }
        });
    }
    None
}

/// End of the HTML tag that opens at `start`: the first `>` outside a
/// Jinja `{{ … }}` / `{% … %}`.
fn tag_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let close: &[u8] = if rest.starts_with(b"{{") {
            b"}}"
        } else if rest.starts_with(b"{%") {
            b"%}"
        } else {
            if bytes[i] == b'>' {
                return Some(i + 1);
            }
            i += 1;
            continue;
        };
        let inner = rest[2..].windows(2).position(|w| w == close)?;
        i += 2 + inner + 2;
    }
    None
}

/// Read a template's links, forms and `extends` / `include` names, each
/// with whether a `{% if %}` / `{% for %}` block encloses it. Jinja
/// comments are skipped.
fn parse_template(path: &str, text: &str) -> Template {
    let mut sites = Vec::new();
    let mut labels = BTreeMap::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("{#") {
            i += rest.find("#}").map_or(rest.len(), |end| end + 2);
            continue;
        }
        if rest.starts_with("{%") {
            let Some(close) = rest.find("%}") else {
                break;
            };
            let end = close + 2;
            let inner =
                rest[2..close].trim_matches(|c: char| c == '-' || c == '+' || c.is_whitespace());
            let word = inner.split_whitespace().next().unwrap_or("");
            match word {
                "if" | "for" => depth += 1,
                "endif" | "endfor" => depth = depth.saturating_sub(1),
                "extends" | "include" => {
                    let arg = inner[word.len()..].trim_start();
                    if let Some(quote) = arg.chars().next().filter(|c| *c == '"' || *c == '\'')
                        && let Some(close) = arg[1..].find(quote)
                    {
                        sites.push((
                            TemplateSite::Includes(arg[1..1 + close].to_owned()),
                            i,
                            i + end,
                            depth > 0,
                        ));
                    }
                }
                _ => {}
            }
            i += end;
            continue;
        }
        let lower_tag = |name: &str| {
            let bytes = rest.as_bytes();
            bytes.len() > name.len() + 1
                && bytes[0] == b'<'
                && bytes[1..1 + name.len()].eq_ignore_ascii_case(name.as_bytes())
                && bytes[1 + name.len()].is_ascii_whitespace()
        };
        if lower_tag("a") || lower_tag("form") {
            let Some(end) = tag_end(text, i) else {
                break;
            };
            let tag = &text[i..end];
            if let Some(label) = site_label(text, end, lower_tag("form")) {
                labels.insert(i, label);
            }
            if lower_tag("a") {
                if let Some(name) = attribute(tag, "href").and_then(url_for_name) {
                    sites.push((TemplateSite::Link(name), i, end, depth > 0));
                }
            } else {
                let method = attribute(tag, "method")
                    .map(|m| m.trim().to_ascii_uppercase())
                    .filter(|m| m == "POST" || m == "GET")
                    .unwrap_or_else(|| "GET".to_owned());
                match attribute(tag, "action") {
                    Some(action) => {
                        if let Some(name) = url_for_name(action) {
                            sites.push((
                                TemplateSite::Form {
                                    action: Some(name),
                                    method,
                                },
                                i,
                                end,
                                depth > 0,
                            ));
                        }
                    }
                    None => sites.push((
                        TemplateSite::Form {
                            action: None,
                            method,
                        },
                        i,
                        end,
                        depth > 0,
                    )),
                }
            }
            i = end;
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    labels.retain(|at, _| sites.iter().any(|(_, start, ..)| start == at));
    Template {
        path: path.to_owned(),
        len: text.len(),
        sites,
        labels,
    }
}

/// The Flask routes recovered so far, by endpoint name.
struct Routes {
    /// `(endpoint name, method)` -> Endpoint ids.
    by_name: BTreeMap<(String, String), BTreeSet<String>>,
    /// `(method, path)` -> Endpoint id.
    by_path: BTreeMap<(String, String), String>,
    /// Handler Symbol -> endpoint names it serves.
    names_of: BTreeMap<String, BTreeSet<String>>,
    /// Page Endpoint id -> (path, name, handler, evidence).
    pages: BTreeMap<String, (String, String, String, Vec<EvidenceRef>)>,
}

impl Routes {
    fn new(nodes: &[Node], renders: &BTreeSet<&str>) -> Self {
        let mut routes = Routes {
            by_name: BTreeMap::new(),
            by_path: BTreeMap::new(),
            names_of: BTreeMap::new(),
            pages: BTreeMap::new(),
        };
        for node in nodes
            .iter()
            .filter(|n| n.label == "Endpoint" && n.props["framework"].as_str() == Some("flask"))
        {
            let (Some(name), Some(method), Some(path)) = (
                node.props["endpoint_name"].as_str(),
                node.props["method"].as_str(),
                node.props["path"].as_str(),
            ) else {
                continue;
            };
            routes
                .by_name
                .entry((name.to_owned(), method.to_owned()))
                .or_default()
                .insert(node.id.clone());
            routes
                .by_path
                .insert((method.to_owned(), path.to_owned()), node.id.clone());
            let Some(handler) = node.props["handler_sym"].as_str() else {
                continue;
            };
            routes
                .names_of
                .entry(handler.to_owned())
                .or_default()
                .insert(name.to_owned());
            let kinds = node.props["responses"]["kinds"]
                .as_array()
                .map(|kinds| {
                    kinds
                        .iter()
                        .any(|k| matches!(k.as_str(), Some("html" | "redirect")))
                })
                .unwrap_or(false);
            if method == "GET" && (kinds || renders.contains(handler)) {
                let evidence =
                    serde_json::from_value::<core_prov::Provenance>(node.props["prov"].clone())
                        .map(|p| p.evidence)
                        .unwrap_or_default();
                routes.pages.insert(
                    node.id.clone(),
                    (
                        path.to_owned(),
                        name.to_owned(),
                        handler.to_owned(),
                        evidence,
                    ),
                );
            }
        }
        routes
    }

    /// The endpoint name `name` means in `function`: `.x` is the function's
    /// own Blueprint when it serves routes of exactly one.
    fn absolute(&self, name: &str, function: Option<&str>) -> Option<String> {
        let Some(local) = name.strip_prefix('.') else {
            return Some(name.to_owned());
        };
        let blueprints: BTreeSet<&str> = self
            .names_of
            .get(function?)?
            .iter()
            .filter_map(|n| n.rsplit_once('.').map(|(bp, _)| bp))
            .collect();
        match blueprints.into_iter().collect::<Vec<_>>()[..] {
            [blueprint] => Some(format!("{blueprint}.{local}")),
            _ => None,
        }
    }

    /// The page Screens of endpoint `name`.
    fn screens(&self, repo: &str, name: &str) -> Vec<String> {
        self.by_name
            .get(&(name.to_owned(), "GET".to_owned()))
            .into_iter()
            .flatten()
            .filter_map(|id| self.pages.get(id))
            .map(|(path, ..)| screen_id(repo, path))
            .collect()
    }
}

fn screen_id(repo: &str, path: &str) -> String {
    format!("screen:{repo}@flask:{path}")
}

fn template_id(repo: &str, path: &str) -> String {
    format!("template:{repo}@{path}")
}

/// One edge's sites, each with whether it is conditional, and its method.
type EdgeSites = (Vec<(EvidenceRef, bool)>, Option<String>);

/// Edges keyed by `(src, label, dst)`, each with its sites in order.
#[derive(Default)]
struct Sites(BTreeMap<(String, &'static str, String), EdgeSites>);

impl Sites {
    fn add(
        &mut self,
        src: &str,
        label: &'static str,
        dst: &str,
        site: (EvidenceRef, bool),
        method: Option<&str>,
    ) {
        let entry = self
            .0
            .entry((src.to_owned(), label, dst.to_owned()))
            .or_insert_with(|| (Vec::new(), method.map(str::to_owned)));
        if !entry.0.contains(&site) {
            entry.0.push(site);
        }
    }

    fn into_edges(self, labels: &BTreeMap<(String, u64), String>, out: &mut Vec<Edge>) {
        for ((src, label, dst), (sites, method)) in self.0 {
            let evidence: Vec<EvidenceRef> = sites.iter().map(|(e, _)| e.clone()).collect();
            let fact = format!("{label} {src} -> {dst}");
            let mut props = serde_json::json!({
                "router": "flask",
                "conditional": sites.iter().all(|(_, c)| *c),
                "sites": sites
                    .iter()
                    .map(|(e, conditional)| {
                        let mut site = serde_json::json!({
                            "path": e.path,
                            "byte_start": e.byte_start,
                            "byte_end": e.byte_end,
                            "conditional": conditional,
                        });
                        if let Some(label) = labels.get(&(e.path.clone(), e.byte_start)) {
                            site["label"] = label.as_str().into();
                        }
                        site
                    })
                    .collect::<Vec<_>>(),
                "prov": prov_from(evidence, ConfidenceTier::InferredStrong, &fact),
            });
            if let Some(method) = method {
                props["method"] = method.into();
            }
            out.push(Edge {
                src,
                dst,
                label: label.into(),
                props,
            });
        }
    }
}

/// Turn page facts and the templates under `root` into `Screen` and
/// `Template` nodes and their edges.
pub(crate) fn resolve(out: &mut Extraction, repo: &str, commit: &str, root: Option<&Path>) {
    let facts = std::mem::take(&mut out.page_facts);
    let renders: BTreeSet<&str> = facts
        .iter()
        .filter(|f| matches!(f.kind, PageKind::Render(_)))
        .map(|f| f.function.as_str())
        .collect();
    let routes = Routes::new(&out.nodes, &renders);
    if routes.pages.is_empty() {
        return;
    }
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for (path, name, handler, evidence) in routes.pages.values() {
        let id = screen_id(repo, path);
        let fact = format!("Screen {id}");
        nodes.push(Node {
            id: id.clone(),
            label: "Screen".into(),
            props: serde_json::json!({
                "route": path,
                "name": name,
                "router": "flask",
                "server_rendered": true,
                "prov": prov_from(evidence.clone(), ConfidenceTier::InferredStrong, &fact),
            }),
        });
        edges.push(Edge {
            src: id.clone(),
            dst: handler.clone(),
            label: "RENDERS".into(),
            props: serde_json::json!({
                "router": "flask",
                "prov": prov_from(
                    evidence.clone(),
                    ConfidenceTier::InferredStrong,
                    &format!("RENDERS {id} -> {handler}"),
                ),
            }),
        });
    }
    let mut sites = Sites::default();
    let files = root.map(template_files).unwrap_or_default();
    let unique = |name: &str| match files.get(name).map(Vec::as_slice) {
        Some([path]) => Some(path.clone()),
        _ => None,
    };
    // Templates each function renders directly, and every template reached.
    let mut rendered: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut pending: Vec<String> = Vec::new();
    for fact in &facts {
        let site = (fact.evidence.clone(), fact.conditional);
        match &fact.kind {
            PageKind::Redirect(name) => {
                let Some(name) = routes.absolute(name, Some(&fact.function)) else {
                    continue;
                };
                for screen in routes.screens(repo, &name) {
                    sites.add(&fact.function, "NAVIGATES_TO", &screen, site.clone(), None);
                }
            }
            PageKind::Render(name) => {
                let Some(path) = unique(name) else {
                    continue;
                };
                sites.add(
                    &fact.function,
                    "RENDERS_TEMPLATE",
                    &template_id(repo, &path),
                    site,
                    None,
                );
                rendered
                    .entry(fact.function.as_str())
                    .or_default()
                    .insert(path.clone());
                pending.push(path);
            }
        }
    }
    let mut parsed: BTreeMap<String, Template> = BTreeMap::new();
    while let Some(path) = pending.pop() {
        if parsed.contains_key(&path) {
            continue;
        }
        let text = root
            .and_then(|root| std::fs::read(root.join(&path)).ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        let template = parse_template(&path, &text);
        for (site, ..) in &template.sites {
            if let TemplateSite::Includes(name) = site
                && let Some(included) = unique(name)
            {
                pending.push(included);
            }
        }
        parsed.insert(path, template);
    }
    let evidence = |path: &str, start: usize, end: usize| EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: start as u64,
        byte_end: end as u64,
        commit_sha: commit.into(),
    };
    for template in parsed.values() {
        let id = template_id(repo, &template.path);
        let fact = format!("Template {id}");
        nodes.push(Node {
            id: id.clone(),
            label: "Template".into(),
            props: serde_json::json!({
                "path": template.path,
                "language": "jinja",
                "prov": prov_from(
                    vec![evidence(&template.path, 0, template.len.max(1))],
                    ConfidenceTier::Confirmed,
                    &fact,
                ),
            }),
        });
        for (site, start, end, conditional) in &template.sites {
            let at = (evidence(&template.path, *start, *end), *conditional);
            match site {
                TemplateSite::Includes(name) => {
                    if let Some(included) = unique(name) {
                        sites.add(&id, "INCLUDES", &template_id(repo, &included), at, None);
                    }
                }
                TemplateSite::Link(name) => {
                    for screen in routes.screens(repo, name) {
                        sites.add(&id, "NAVIGATES_TO", &screen, at.clone(), None);
                    }
                }
                TemplateSite::Form {
                    action: Some(name),
                    method,
                } => {
                    if method == "GET" {
                        for screen in routes.screens(repo, name) {
                            sites.add(&id, "NAVIGATES_TO", &screen, at.clone(), None);
                        }
                        continue;
                    }
                    for endpoint in routes
                        .by_name
                        .get(&(name.clone(), method.clone()))
                        .into_iter()
                        .flatten()
                    {
                        sites.add(&id, "SUBMITS", endpoint, at.clone(), Some(method));
                    }
                }
                TemplateSite::Form { action: None, .. } => {}
            }
        }
    }
    // A form without `action` posts back to the page that rendered it.
    for (path, _, handler, _) in routes.pages.values() {
        let Some(post) = routes.by_path.get(&("POST".to_owned(), path.clone())) else {
            continue;
        };
        for template in rendered.get(handler.as_str()).into_iter().flatten() {
            let Some(parsed) = parsed.get(template) else {
                continue;
            };
            for (site, start, end, conditional) in &parsed.sites {
                if let TemplateSite::Form {
                    action: None,
                    method,
                } = site
                    && method == "POST"
                {
                    sites.add(
                        handler,
                        "SUBMITS",
                        post,
                        (evidence(template, *start, *end), *conditional),
                        Some("POST"),
                    );
                }
            }
        }
    }
    let labels: BTreeMap<(String, u64), String> = parsed
        .values()
        .flat_map(|t| {
            t.labels
                .iter()
                .map(|(at, label)| ((t.path.clone(), *at as u64), label.clone()))
        })
        .collect();
    sites.into_edges(&labels, &mut edges);
    out.nodes.extend(nodes);
    out.edges.extend(edges);
}

#[cfg(test)]
mod tests;
