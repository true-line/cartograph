//! Target tests (W10a, #946): pytest test functions and the Flask
//! test-client calls they make, linked to the recovered endpoints they
//! request. These are static references with declared expectations: they do
//! not prove that a test ran, or which branches it reached.
//!
//! A test is a `test*` function at module level, or a `test*` method of a
//! module-level `Test*` class, in a `test_*.py` or `*_test.py` file. Each
//! becomes a Confirmed `TestCase` citing its definition.
//!
//! A call `client.get(…)` (or `post`, `put`, `patch`, `delete`, `head`,
//! `options`) is a test-client call only when `client` is proven to be a
//! Flask test client:
//!
//! - a test parameter that names a `@pytest.fixture` returning or yielding
//!   `….test_client(…)`, defined in the same file or in a `conftest.py` of
//!   its directory or an ancestor (the nearest wins, as pytest resolves it);
//! - or a local bound once, in the test, by `c = ….test_client(…)` or
//!   `with ….test_client(…) as c`.
//!
//! A name bound more than once, or a parameter rebound in the test, proves
//! nothing, and any other `x.get(…)` is ordinary code that is not read. A
//! nearer fixture of the same name that is not proven a client shadows an
//! outer client fixture, so its parameter proves nothing either.
//!
//! The request target is the first argument: a string literal is a path,
//! matched against recovered endpoints' route templates, and Flask's
//! `url_for("<blueprint>.<view>")` builds the URL of the endpoint whose
//! `endpoint_name` it names; the call's method then selects the endpoint
//! served at that URL. A computed target, a name or path no recovered endpoint
//! serves, a method the endpoint does not serve, or a path that matches more
//! than one endpoint becomes a Gap that names the call.
//!
//! Calls in helper functions that a test passes its client to are not read.
//!
//! Each resolved link is one `REQUESTS` edge per test and endpoint. It
//! records the statuses the test asserts (`assert r.status_code == 400`, or
//! on the call itself) and the response keys it reads (`r.json["k"]`,
//! `r.get_json()["k"]`, `"k" in r.json`) between the call and the next
//! binding of `r`. It is InferredStrong and marked `client_app_assumed`:
//! the fixture's application is not proven to be the one that serves the
//! endpoint.

use crate::request_inputs::{binding_sites, parameter_name, visit_scope};
use crate::{Extraction, FileCx, ImportBinding, file_id, literal_string, prov_from};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

const VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

/// A `@pytest.fixture` in a test file or `conftest.py`. Every fixture is
/// kept so a nearer one that is not a client still shadows an outer client.
#[derive(Debug, Clone)]
pub(crate) struct ClientFixture {
    file: String,
    name: String,
    /// Whether it returns or yields a Flask test client.
    client: bool,
    evidence: EvidenceRef,
}

/// What a test-client call requests.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// `url_for("<name>")`.
    Name(String),
    /// A literal path, without its query string.
    Path(String),
    /// Neither: the reason it is not read.
    Computed(&'static str),
}

/// The client a call is made on.
#[derive(Debug, Clone)]
enum Client {
    /// A test parameter, proven at the join against a visible fixture.
    Fixture(String),
    /// A local bound once to `….test_client(…)`.
    Local(EvidenceRef),
}

/// One test-client call, resolved after every file is read.
#[derive(Debug, Clone)]
pub(crate) struct ClientCall {
    test: String,
    file: String,
    client: Client,
    verb: String,
    target: Target,
    statuses: BTreeSet<u16>,
    keys: BTreeSet<String>,
    /// `follow_redirects=` other than a literal `False`: the response read
    /// may be the redirect target's, so no expectation is recorded.
    follows_redirects: bool,
    evidence: EvidenceRef,
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn is_test_file(path: &str) -> bool {
    let name = base_name(path);
    name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py"))
}

/// Whether `path` is test code: a pytest module, a `conftest.py`, or a
/// file under a `tests`/`test` directory.
pub(crate) fn in_test_tree(path: &str) -> bool {
    let mut dirs: Vec<&str> = path.split(['/', '\\']).collect();
    dirs.pop();
    is_test_file(path) || is_conftest(path) || dirs.iter().any(|d| matches!(*d, "tests" | "test"))
}

pub(crate) fn is_conftest(path: &str) -> bool {
    base_name(path) == "conftest.py"
}

fn test_id(repo: &str, path: &str, name: &str) -> String {
    format!("test:{repo}@{path}#{name}")
}

/// Whether `callee` names pytest's `fixture` through an import.
fn is_fixture(callee: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    match callee.split_once('.') {
        Some((base, "fixture")) => matches!(
            bindings.get(base),
            Some(ImportBinding::Module { module }) if module == "pytest"
        ),
        None => matches!(
            bindings.get(callee),
            Some(ImportBinding::Symbol { module, exported, .. })
                if module == "pytest" && exported == "fixture"
        ),
        _ => false,
    }
}

/// Whether `callee` names Flask's `url_for` through an import that no
/// parameter or local of the test shadows.
fn is_url_for(
    callee: &str,
    bindings: &HashMap<String, ImportBinding>,
    shadowed: &BTreeSet<String>,
) -> bool {
    let root = callee.split_once('.').map_or(callee, |(base, _)| base);
    if shadowed.contains(root) {
        return false;
    }
    match callee.split_once('.') {
        Some((base, "url_for")) => matches!(
            bindings.get(base),
            Some(ImportBinding::Module { module }) if module == "flask"
        ),
        None => matches!(
            bindings.get(callee),
            Some(ImportBinding::Symbol { module, exported, .. })
                if module == "flask" && exported == "url_for"
        ),
        _ => false,
    }
}

/// The fixture name a `@pytest.fixture` / `@pytest.fixture(name="…")`
/// decorator gives `function`, if it is one.
fn fixture_name(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<String> {
    let parent = function
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")?;
    let mut walk = parent.walk();
    for decorator in parent
        .named_children(&mut walk)
        .filter(|child| child.kind() == "decorator")
    {
        let Some(expression) = decorator.named_child(0) else {
            continue;
        };
        let (callee, call) = match expression.kind() {
            "call" => (
                expression.child_by_field_name("function")?,
                Some(expression),
            ),
            _ => (expression, None),
        };
        if !is_fixture(cx.text(&callee), bindings) {
            continue;
        }
        let renamed = call.and_then(|call| {
            crate::named_arguments(call)
                .into_iter()
                .find_map(|argument| {
                    let name = argument.child_by_field_name("name")?;
                    (argument.kind() == "keyword_argument" && cx.text(&name) == "name")
                        .then(|| argument.child_by_field_name("value"))
                        .flatten()
                })
        });
        return match renamed {
            Some(value) => literal_string(cx, value),
            None => function
                .child_by_field_name("name")
                .map(|name| cx.text(&name).to_string()),
        };
    }
    None
}

/// Whether `node` is a call of a `test_client` method.
fn is_test_client_call(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    node.kind() == "call"
        && node
            .child_by_field_name("function")
            .filter(|function| function.kind() == "attribute")
            .and_then(|function| function.child_by_field_name("attribute"))
            .is_some_and(|attribute| cx.text(&attribute) == "test_client")
}

/// Locals of `body`'s own scope bound exactly once, and to a test client.
fn client_locals(cx: &FileCx<'_>, body: TsNode<'_>) -> BTreeMap<String, EvidenceRef> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut clients = BTreeMap::new();
    binding_sites(cx, body, &mut |site, names| {
        for name in names {
            *counts.entry(name.clone()).or_default() += 1;
        }
        let value = match site.kind() {
            "assignment" => site
                .child_by_field_name("left")
                .filter(|left| left.kind() == "identifier")
                .and_then(|_| site.child_by_field_name("right")),
            "as_pattern" => site.named_child(0),
            _ => None,
        };
        if let Some(value) = value.filter(|value| is_test_client_call(cx, *value))
            && names.len() == 1
        {
            let name = names.iter().next().expect("one name").clone();
            clients.insert(name, cx.evidence(&value));
        }
    });
    clients.retain(|name, _| counts.get(name) == Some(&1));
    clients
}

/// Whether `function` returns or yields a test client.
fn returns_client(cx: &FileCx<'_>, function: TsNode<'_>) -> bool {
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    let locals = client_locals(cx, body);
    let mut found = false;
    visit_scope(body, &mut |node| {
        let value = match node.kind() {
            "return_statement" => node.named_child(0),
            "yield" => node.named_child(0),
            _ => None,
        };
        if let Some(value) = value {
            found |= is_test_client_call(cx, value)
                || (value.kind() == "identifier" && locals.contains_key(cx.text(&value)));
        }
    });
    found
}

/// Module-level functions and `Test*` class methods named `test*`, with
/// their qualified names.
fn test_functions<'t>(cx: &FileCx<'_>, root: TsNode<'t>) -> Vec<(TsNode<'t>, String)> {
    let definition = |node: TsNode<'t>| {
        if node.kind() == "decorated_definition" {
            node.child_by_field_name("definition")
        } else {
            Some(node)
        }
    };
    let name_of = |node: TsNode<'t>| {
        node.child_by_field_name("name")
            .map(|name| cx.text(&name).to_string())
    };
    let mut tests = Vec::new();
    let mut walk = root.walk();
    for child in root.named_children(&mut walk).filter_map(definition) {
        match child.kind() {
            "function_definition" => {
                if let Some(name) = name_of(child).filter(|name| name.starts_with("test")) {
                    tests.push((child, name));
                }
            }
            "class_definition" => {
                let Some(class) = name_of(child).filter(|name| name.starts_with("Test")) else {
                    continue;
                };
                let Some(body) = child.child_by_field_name("body") else {
                    continue;
                };
                let mut walk = body.walk();
                for method in body.named_children(&mut walk).filter_map(definition) {
                    if method.kind() != "function_definition" {
                        continue;
                    }
                    if let Some(name) = name_of(method).filter(|name| name.starts_with("test")) {
                        tests.push((method, format!("{class}.{name}")));
                    }
                }
            }
            _ => {}
        }
    }
    tests
}

/// The path of a literal request target, without its query or fragment.
fn literal_path(path: &str) -> String {
    path.split(['?', '#']).next().unwrap_or(path).to_string()
}

fn request_target(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    shadowed: &BTreeSet<String>,
) -> Target {
    let Some(first) = crate::named_arguments(call).into_iter().next() else {
        return Target::Path("/".into());
    };
    if first.kind() == "keyword_argument" {
        return Target::Computed("request URL is not a positional argument");
    }
    if let Some(path) = literal_string(cx, first) {
        return Target::Path(literal_path(&path));
    }
    if first.kind() == "call"
        && let Some(callee) = first.child_by_field_name("function")
        && is_url_for(cx.text(&callee), bindings, shadowed)
    {
        return crate::named_arguments(first)
            .into_iter()
            .next()
            .and_then(|name| literal_string(cx, name))
            .map_or(
                Target::Computed("endpoint name is not a string literal"),
                Target::Name,
            );
    }
    Target::Computed("request URL is computed")
}

/// The integer a literal node holds.
fn integer(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<u16> {
    (node.kind() == "integer")
        .then(|| cx.text(&node).parse().ok())
        .flatten()
}

fn in_assert(node: TsNode<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(node) = parent {
        match node.kind() {
            "assert_statement" => return true,
            "function_definition" | "class_definition" | "lambda" => return false,
            _ => parent = node.parent(),
        }
    }
    false
}

/// `status == <int>` asserted on `response` (an attribute `….status_code`).
fn asserted_status(cx: &FileCx<'_>, status_code: TsNode<'_>) -> Option<u16> {
    let comparison = status_code
        .parent()
        .filter(|parent| parent.kind() == "comparison_operator")?;
    if !in_assert(comparison) || comparison.named_child_count() != 2 {
        return None;
    }
    let mut walk = comparison.walk();
    let operator = comparison
        .children(&mut walk)
        .find(|child| !child.is_named())
        .map(|child| cx.text(&child).to_string())?;
    if operator != "==" {
        return None;
    }
    let other = if comparison.named_child(0) == Some(status_code) {
        comparison.named_child(1)?
    } else {
        comparison.named_child(0)?
    };
    integer(cx, other)
}

/// Whether `node` is the response's JSON body: `r.json` or `r.get_json()`.
fn is_json_body(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    is_response: &dyn Fn(TsNode<'_>) -> bool,
) -> bool {
    let attribute = match node.kind() {
        "attribute" => node,
        "call" if crate::named_arguments(node).is_empty() => {
            match node.child_by_field_name("function") {
                Some(function) if function.kind() == "attribute" => function,
                _ => return false,
            }
        }
        _ => return false,
    };
    let (Some(object), Some(name)) = (
        attribute.child_by_field_name("object"),
        attribute.child_by_field_name("attribute"),
    ) else {
        return false;
    };
    let name = cx.text(&name);
    let shape = match node.kind() {
        "attribute" => name == "json",
        _ => name == "get_json",
    };
    shape && is_response(object)
}

/// Statuses asserted and response keys read on `is_response` nodes within
/// `scope`, between byte offsets `from` and `to`.
fn expectations(
    cx: &FileCx<'_>,
    scope: TsNode<'_>,
    from: usize,
    to: usize,
    is_response: &dyn Fn(TsNode<'_>) -> bool,
    statuses: &mut BTreeSet<u16>,
    keys: &mut BTreeSet<String>,
) {
    visit_scope(scope, &mut |node| {
        if node.start_byte() < from || node.start_byte() >= to {
            return;
        }
        match node.kind() {
            "attribute" => {
                let (Some(object), Some(name)) = (
                    node.child_by_field_name("object"),
                    node.child_by_field_name("attribute"),
                ) else {
                    return;
                };
                if cx.text(&name) == "status_code"
                    && is_response(object)
                    && let Some(status) = asserted_status(cx, node)
                {
                    statuses.insert(status);
                }
            }
            "subscript" => {
                let (Some(value), Some(key)) = (
                    node.child_by_field_name("value"),
                    node.child_by_field_name("subscript"),
                ) else {
                    return;
                };
                if is_json_body(cx, value, is_response)
                    && let Some(key) = literal_string(cx, key)
                {
                    keys.insert(key);
                }
            }
            "comparison_operator" if node.named_child_count() == 2 => {
                let mut walk = node.walk();
                let operator = node
                    .children(&mut walk)
                    .filter(|child| !child.is_named())
                    .map(|child| cx.text(&child).to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                if operator != "in" && operator != "not in" {
                    return;
                }
                let (Some(key), Some(container)) = (node.named_child(0), node.named_child(1))
                else {
                    return;
                };
                if is_json_body(cx, container, is_response)
                    && let Some(key) = literal_string(cx, key)
                {
                    keys.insert(key);
                }
            }
            _ => {}
        }
    });
}

/// Record the tests in a test file, and the client fixtures in a test file
/// or `conftest.py`.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let test_file = is_test_file(cx.path);
    if !test_file && !is_conftest(cx.path) {
        return;
    }
    let mut walk = root.walk();
    for child in root.named_children(&mut walk) {
        let function = if child.kind() == "decorated_definition" {
            child.child_by_field_name("definition")
        } else {
            Some(child)
        };
        let Some(function) = function.filter(|f| f.kind() == "function_definition") else {
            continue;
        };
        if let Some(name) = fixture_name(cx, function, bindings) {
            out.client_fixtures.push(ClientFixture {
                file: cx.path.to_string(),
                name,
                client: returns_client(cx, function),
                evidence: cx.evidence(&function),
            });
        }
    }
    if !test_file {
        return;
    }
    for (function, name) in test_functions(cx, root) {
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let test = test_id(cx.id.repo, cx.path, &name);
        out.nodes.push(Node {
            id: test.clone(),
            label: "TestCase".into(),
            props: serde_json::json!({
                "name": name,
                "framework": "pytest",
                "language": "python",
                "path": cx.path,
                "prov": cx.prov(&function, &format!("TestCase {test}")),
            }),
        });
        out.edges.push(Edge {
            src: test.clone(),
            dst: file_id(cx.id.repo, cx.path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("DEFINED_IN {test}")),
            }),
        });
        // The test's own function, whose direct calls are the test's static
        // references to functions (#965).
        let symbol = crate::symbol_id(cx.id.repo, cx.path, &name);
        out.edges.push(Edge {
            src: test.clone(),
            dst: symbol.clone(),
            label: "IMPLEMENTED_BY".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("IMPLEMENTED_BY {test} -> {symbol}")),
            }),
        });
        let mut bound: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        binding_sites(cx, body, &mut |site, names| {
            for name in names {
                bound
                    .entry(name.clone())
                    .or_default()
                    .push(site.start_byte());
            }
        });
        let mut clients: BTreeMap<String, Client> = BTreeMap::new();
        // Parameters and locals shadow the file's imports, `url_for` included.
        let mut shadowed: BTreeSet<String> = bound.keys().cloned().collect();
        if let Some(parameters) = function.child_by_field_name("parameters") {
            let mut walk = parameters.walk();
            for parameter in parameters.named_children(&mut walk) {
                let Some(name) = parameter_name(parameter).map(|name| cx.text(&name).to_string())
                else {
                    continue;
                };
                shadowed.insert(name.clone());
                if !bound.contains_key(&name) {
                    clients.insert(name.clone(), Client::Fixture(name));
                }
            }
        }
        for (name, evidence) in client_locals(cx, body) {
            clients.insert(name, Client::Local(evidence));
        }
        visit_scope(body, &mut |call| {
            if call.kind() != "call" {
                return;
            }
            let Some(function) = call
                .child_by_field_name("function")
                .filter(|function| function.kind() == "attribute")
            else {
                return;
            };
            let (Some(object), Some(verb)) = (
                function.child_by_field_name("object"),
                function.child_by_field_name("attribute"),
            ) else {
                return;
            };
            let verb = cx.text(&verb);
            if object.kind() != "identifier" || !VERBS.contains(&verb) {
                return;
            }
            let Some(client) = clients.get(cx.text(&object)).cloned() else {
                return;
            };
            let mut statuses = BTreeSet::new();
            let mut keys = BTreeSet::new();
            // The call itself, within its statement:
            // `assert client.get(…).status_code == 200`.
            let this = call.id();
            let mut statement = call;
            while let Some(parent) = statement.parent().filter(|p| p.id() != body.id()) {
                statement = parent;
            }
            expectations(
                cx,
                body,
                statement.start_byte(),
                statement.end_byte(),
                &|node| node.id() == this,
                &mut statuses,
                &mut keys,
            );
            // `r = client.get(…)`, read until `r` is bound again.
            if let Some(assignment) = call.parent().filter(|p| p.kind() == "assignment")
                && let Some(left) = assignment
                    .child_by_field_name("left")
                    .filter(|left| left.kind() == "identifier")
            {
                let response = cx.text(&left).to_string();
                let end = bound
                    .get(&response)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|start| *start > assignment.start_byte())
                    .min()
                    .unwrap_or(usize::MAX);
                expectations(
                    cx,
                    body,
                    assignment.end_byte(),
                    end,
                    &|node| node.kind() == "identifier" && cx.text(&node) == response,
                    &mut statuses,
                    &mut keys,
                );
            }
            let follows_redirects = crate::named_arguments(call)
                .into_iter()
                .any(|argument| may_follow_redirects(cx, argument));
            if follows_redirects {
                statuses.clear();
                keys.clear();
            }
            out.client_calls.push(ClientCall {
                test: test.clone(),
                file: cx.path.to_string(),
                client,
                verb: verb.to_ascii_uppercase(),
                target: request_target(cx, call, bindings, &shadowed),
                statuses,
                keys,
                follows_redirects,
                evidence: cx.evidence(&call),
            });
        });
    }
}

/// Whether a call argument may enable `follow_redirects`: the keyword with
/// any value but a literal `False`, or `**mapping` unless it is a literal
/// dict whose string keys are all known and whose `follow_redirects`, if
/// present, is a literal `False`.
fn may_follow_redirects(cx: &FileCx<'_>, argument: TsNode<'_>) -> bool {
    let not_false = |value: Option<TsNode<'_>>| value.is_none_or(|v| cx.text(&v) != "False");
    match argument.kind() {
        "keyword_argument" => {
            argument
                .child_by_field_name("name")
                .is_some_and(|name| cx.text(&name) == "follow_redirects")
                && not_false(argument.child_by_field_name("value"))
        }
        "dictionary_splat" => {
            let Some(mapping) = argument
                .named_child(0)
                .filter(|mapping| mapping.kind() == "dictionary")
            else {
                return true;
            };
            let mut walk = mapping.walk();
            mapping.named_children(&mut walk).any(|entry| {
                if entry.kind() != "pair" {
                    return entry.kind() != "comment";
                }
                match entry
                    .child_by_field_name("key")
                    .and_then(|key| literal_string(cx, key))
                {
                    Some(key) => {
                        key == "follow_redirects" && not_false(entry.child_by_field_name("value"))
                    }
                    None => true,
                }
            })
        }
        _ => false,
    }
}

/// Whether one path segment matches a route template segment.
fn segment_matches(template: &str, segment: &str) -> bool {
    if let Some(inner) = template
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return match inner.split_once(':').map(|(converter, _)| converter) {
            Some("int") => !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()),
            _ => !segment.is_empty(),
        };
    }
    if template.starts_with('{') && template.ends_with('}') {
        return !segment.is_empty();
    }
    template == segment
}

/// How many static segments `template` matches `path` with, if it does.
fn route_match(template: &str, path: &str) -> Option<usize> {
    let template: Vec<&str> = template.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    if template.len() != path.len() {
        return None;
    }
    let mut fixed = 0;
    for (template, segment) in template.iter().zip(&path) {
        if !segment_matches(template, segment) {
            return None;
        }
        if template == segment {
            fixed += 1;
        }
    }
    Some(fixed)
}

/// The nearest fixture named `name` visible from `file`, client or not; the
/// last definition wins within a file, as pytest resolves it.
fn visible_fixture<'f>(
    fixtures: &'f [ClientFixture],
    file: &str,
    name: &str,
) -> Option<&'f ClientFixture> {
    let directory = |path: &str| path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
    let test_dir = directory(file);
    fixtures
        .iter()
        .filter(|fixture| fixture.name == name)
        .filter_map(|fixture| {
            if fixture.file == file {
                return Some((usize::MAX, fixture));
            }
            if !is_conftest(&fixture.file) {
                return None;
            }
            let dir = directory(&fixture.file);
            let visible =
                dir.is_empty() || test_dir == dir || test_dir.starts_with(&format!("{dir}/"));
            visible.then_some((dir.len(), fixture))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, fixture)| fixture)
}

/// One test's merged link to one endpoint.
#[derive(Default)]
struct Link {
    via: BTreeSet<&'static str>,
    statuses: BTreeSet<u16>,
    keys: BTreeSet<String>,
    follows_redirects: bool,
    evidence: Vec<EvidenceRef>,
}

/// Link every proven test-client call to the endpoint it requests, or to a
/// Gap naming why it cannot.
pub(crate) fn resolve(out: &mut Extraction, repo: &str) {
    let fixtures = std::mem::take(&mut out.client_fixtures);
    let calls = std::mem::take(&mut out.client_calls);
    if calls.is_empty() {
        return;
    }
    let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut by_method: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for node in &out.nodes {
        if node.label != "Endpoint" {
            continue;
        }
        let (Some(method), Some(path)) =
            (node.props["method"].as_str(), node.props["path"].as_str())
        else {
            continue;
        };
        if let Some(name) = node.props["endpoint_name"].as_str() {
            by_name.entry(name).or_default().insert(path);
        }
        by_method.entry(method).or_default().push((path, &node.id));
    }
    let mut links: BTreeMap<(String, String), Link> = BTreeMap::new();
    let mut gaps = Vec::new();
    for call in calls {
        let client_evidence = match &call.client {
            Client::Fixture(name) => match visible_fixture(&fixtures, &call.file, name) {
                Some(fixture) if fixture.client => fixture.evidence.clone(),
                _ => continue,
            },
            Client::Local(evidence) => evidence.clone(),
        };
        let resolved: Result<(String, &'static str), &'static str> = match &call.target {
            Target::Computed(reason) => Err(reason),
            // `url_for` builds the name's URL; the method then selects the
            // endpoint served there, which may be another view.
            Target::Name(name) => match by_name.get(name.as_str()) {
                None => Err("test client endpoint name is not a recovered endpoint"),
                Some(paths) => {
                    let served = by_method
                        .get(call.verb.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|(path, _)| paths.contains(path))
                        .collect::<Vec<_>>();
                    match served.as_slice() {
                        [] => Err(
                            "no recovered endpoint serves this method at the endpoint name's URL",
                        ),
                        [(_, id)] => Ok((id.to_string(), "url_for")),
                        _ => Err("endpoint name builds more than one recovered URL"),
                    }
                }
            },
            Target::Path(path) => {
                let mut matches: Vec<(usize, &str)> = by_method
                    .get(call.verb.as_str())
                    .into_iter()
                    .flatten()
                    .filter_map(|(template, id)| {
                        route_match(template, path).map(|fixed| (fixed, *id))
                    })
                    .collect();
                matches.sort_by_key(|entry| std::cmp::Reverse(entry.0));
                match matches.as_slice() {
                    [] => Err("no recovered endpoint serves this path"),
                    [(_, id)] => Ok((id.to_string(), "path")),
                    [(best, id), (next, _), ..] if best > next => Ok((id.to_string(), "path")),
                    _ => Err("test client path matches more than one recovered endpoint"),
                }
            }
        };
        match resolved {
            Ok((endpoint, via)) => {
                let link = links.entry((call.test.clone(), endpoint)).or_default();
                link.via.insert(via);
                link.statuses.extend(call.statuses);
                link.keys.extend(call.keys);
                link.follows_redirects |= call.follows_redirects;
                link.evidence.push(call.evidence);
                if !link.evidence.contains(&client_evidence) {
                    link.evidence.push(client_evidence);
                }
            }
            Err(reason) => gaps.push((call, client_evidence, reason)),
        }
    }
    for ((test, endpoint), link) in links {
        let mut props = serde_json::json!({
            "via": link.via,
            "statuses": link.statuses,
            "keys": link.keys,
            "link": "client_app_assumed",
            "prov": prov_from(
                link.evidence,
                ConfidenceTier::InferredStrong,
                &format!("REQUESTS {test} -> {endpoint}"),
            ),
        });
        if link.follows_redirects {
            props["follows_redirects"] = true.into();
        }
        out.edges.push(Edge {
            src: test,
            dst: endpoint,
            label: "REQUESTS".into(),
            props,
        });
    }
    for (call, client_evidence, reason) in gaps {
        let gap = format!(
            "gap:test-call:{repo}@{}@{}",
            call.evidence.path, call.evidence.byte_start
        );
        let request = match &call.target {
            Target::Name(name) => format!("{} url_for({name})", call.verb),
            Target::Path(path) => format!("{} {path}", call.verb),
            Target::Computed(_) => call.verb.clone(),
        };
        let evidence = vec![call.evidence, client_evidence];
        out.nodes.push(Node {
            id: gap.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "request": request,
                "attempted_tiers": ["T0"],
                "prov": prov_from(evidence.clone(), ConfidenceTier::Gap, &format!("Gap {gap}")),
            }),
        });
        out.edges.push(Edge {
            src: call.test.clone(),
            dst: gap.clone(),
            label: "REQUESTS".into(),
            props: serde_json::json!({
                "reason": reason,
                "prov": prov_from(evidence, ConfidenceTier::Gap, &format!("REQUESTS {} -> {gap}", call.test)),
            }),
        });
    }
}

pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    for fixture in &mut extraction.client_fixtures {
        fixture.evidence.commit_sha = commit.into();
    }
    for call in &mut extraction.client_calls {
        call.evidence.commit_sha = commit.into();
        if let Client::Local(evidence) = &mut call.client {
            evidence.commit_sha = commit.into();
        }
    }
}
