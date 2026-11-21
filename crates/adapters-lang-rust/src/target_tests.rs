//! Rust target tests (#2018, AC-1016): `cargo test` functions and the HTTP
//! requests they build, linked to the recovered endpoints they exercise.
//! These are static references with declared expectations: they do not
//! prove that a test ran, or which branches it reached.
//!
//! A test is a `fn` annotated `#[test]`, or `#[X::test]` for a test runtime
//! (`tokio`, `actix_web`, `actix_rt`, `async_std`, `sqlx`, `test_log`), or
//! `#[rstest]`. Each becomes a Confirmed `TestCase` citing its definition,
//! `DEFINED_IN` its file and `IMPLEMENTED_BY` the Symbol of its function, so
//! the calls that Symbol makes are the test's static references.
//!
//! A request is a builder chain in the test's own body that starts at
//! `Request::builder()`, `Request::get(uri)` (and the other verbs) of `http`,
//! `axum::http` or `hyper`, or at actix-web's `TestRequest::get()` (and the
//! other verbs), `TestRequest::default()` or `TestRequest::with_uri(uri)`.
//! Its method is the constructor's verb, a `.method(…)` string literal or
//! `Method::X`, else GET; its target is a literal path or absolute URL in
//! the constructor or `.uri(…)`, read without origin and query. Once every
//! file is read the request is joined to the endpoint whose route matches
//! the path and that serves the method (`ANY` serves every method); the
//! most specific route wins. A computed method or target, a path no
//! recovered endpoint serves, or two equally specific matches become a Gap
//! that names the request.
//!
//! Each resolved link is one InferredStrong `REQUESTS` edge per test and
//! endpoint, marked `handler_assumed`: the router the test sends the request
//! to is not proven to be the one that serves it. It records the statuses
//! the test compares a response's `status()` with in `assert_eq!`,
//! `assert_ne!` or `assert!(… == …)` (an integer, `.as_u16()` or
//! `StatusCode::X`) anywhere in its body.

use super::*;

/// Test runtimes whose `test` attribute marks a test.
const RUNTIMES: &[&str] = &[
    "tokio",
    "actix_web",
    "actix_rt",
    "async_std",
    "sqlx",
    "test_log",
];

const VERBS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "options"];

/// A request a test builds, held until every file's endpoints are known.
#[derive(Debug, Clone)]
pub(super) struct TestRequest {
    test: String,
    method: Result<String, &'static str>,
    path: Result<String, &'static str>,
    request: String,
    statuses: BTreeSet<u16>,
    evidence: EvidenceRef,
}

pub(super) fn retarget(requests: &mut [TestRequest], commit: &str) {
    for request in requests {
        request.evidence.commit_sha = commit.into();
    }
}

fn test_id(repo: &str, path: &str, name: &str) -> String {
    format!("test:{repo}@{path}#{name}")
}

fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

fn named_children(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The test attribute on `function` (`test`, `tokio::test`, `rstest`), if
/// it has one.
pub(super) fn test_attribute(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    let mut sibling = function.prev_sibling();
    while let Some(node) = sibling {
        match node.kind() {
            "attribute_item" => {
                let attribute = named_children(node)
                    .into_iter()
                    .find(|child| child.kind() == "attribute")?;
                let path = named_children(attribute)
                    .into_iter()
                    .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"));
                if let Some(path) = path {
                    let text: String = cx.text(&path).split_whitespace().collect();
                    let is_test = match text.rsplit_once("::") {
                        Some((runtime, "test")) => RUNTIMES.contains(&runtime),
                        _ => matches!(text.as_str(), "test" | "rstest"),
                    };
                    if is_test {
                        return Some(text);
                    }
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = node.prev_sibling();
    }
    None
}

/// The names of out-of-line modules a file declares under `#[cfg(test)]`
/// (`#[cfg(test)] mod fixtures;`, other attributes and `pub` allowed).
pub(super) fn cfg_test_declarations(source: &[u8]) -> Vec<String> {
    const CFG: &str = "#[cfg(test)]";
    let Ok(text) = std::str::from_utf8(source) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(CFG) {
        from += at + CFG.len();
        let mut rest = text[from..].trim_start();
        // Further attributes and comments between the cfg and the item.
        loop {
            if rest.starts_with("#[") {
                let Some(end) = rest.find(']') else { break };
                rest = rest[end + 1..].trim_start();
            } else if rest.starts_with("//") {
                let end = rest.find('\n').unwrap_or(rest.len());
                rest = rest[end..].trim_start();
            } else {
                break;
            }
        }
        if let Some(after) = rest.strip_prefix("pub") {
            rest = after.trim_start();
            if rest.starts_with('(') {
                let Some(end) = rest.find(')') else { continue };
                rest = rest[end + 1..].trim_start();
            }
        }
        let Some(after) = rest.strip_prefix("mod") else {
            continue;
        };
        if !after.starts_with(char::is_whitespace) {
            continue;
        }
        let after = after.trim_start();
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && after[name.len()..].trim_start().starts_with(';') {
            names.push(name);
        }
    }
    names
}

/// Whether `node` is test code: in a crate's `tests/` integration tests, in
/// a module an out-of-line `#[cfg(test)] mod x;` declares, in a module
/// named `tests` or `*_tests` (the convention when the declaration is not
/// read), inside a test function, or in a module under `#[cfg(test)]`.
pub(super) fn in_test_code(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    if cx.test_module
        || cx.path.starts_with("tests/")
        || cx.path.contains("/tests/")
        || cx
            .module
            .iter()
            .any(|segment| segment == "tests" || segment.ends_with("_tests"))
    {
        return true;
    }
    let mut current = Some(node);
    while let Some(node) = current {
        match node.kind() {
            "function_item" if test_attribute(cx, node).is_some() => return true,
            "mod_item" if cfg_test(cx, node) => return true,
            _ => {}
        }
        current = node.parent();
    }
    false
}

fn cfg_test(cx: &FileCx<'_>, item: TsNode<'_>) -> bool {
    let mut sibling = item.prev_sibling();
    while let Some(node) = sibling {
        match node.kind() {
            "attribute_item" => {
                let text: String = cx.text(&node).split_whitespace().collect();
                if text == "#[cfg(test)]" {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
        sibling = node.prev_sibling();
    }
    false
}

/// A status a node's text names: an integer, or `StatusCode::X`.
fn status(text: &str) -> Option<u16> {
    let text = text.trim();
    if let Ok(code) = text.parse::<u16>() {
        return (100..600).contains(&code).then_some(code);
    }
    let name = text.rsplit("::").next()?;
    text.contains("StatusCode::")
        .then(|| core_graph::rules::rust_http_status(name))
        .flatten()
}

/// Whether an expression's text reads a response's status.
fn reads_status(text: &str) -> bool {
    let text: String = text.split_whitespace().collect();
    text.ends_with(".status()") || text.ends_with(".status().as_u16()")
}

/// Split a macro's token-tree text at its top-level commas.
fn macro_arguments(text: &str) -> Vec<&str> {
    let inner = text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(text);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut quoted = false;
    for (at, c) in inner.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' | '[' | '{' if !quoted => depth += 1,
            ')' | ']' | '}' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                out.push(&inner[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push(&inner[start..]);
    out
}

/// The statuses a test compares a response's status with.
fn statuses(cx: &FileCx<'_>, body: TsNode<'_>) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() != "macro_invocation" {
            continue;
        }
        let Some(name) = node.child_by_field_name("macro") else {
            continue;
        };
        let Some(tree) = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "token_tree")
        else {
            continue;
        };
        let arguments = macro_arguments(cx.text(&tree));
        let pair = match cx.text(&name).trim() {
            "assert_eq" | "assert_ne" | "pretty_assertions::assert_eq" => {
                arguments.get(0..2).map(|pair| (pair[0], pair[1]))
            }
            "assert" | "debug_assert" => arguments.first().and_then(|condition| {
                condition
                    .split_once("==")
                    .or_else(|| condition.split_once("!="))
            }),
            _ => None,
        };
        let Some((left, right)) = pair else {
            continue;
        };
        for (read, other) in [(left, right), (right, left)] {
            if reads_status(read)
                && let Some(code) = status(other)
            {
                out.insert(code);
            }
        }
    }
    out
}

/// A request method: a string literal or `Method::X`.
fn method(cx: &FileCx<'_>, node: TsNode<'_>) -> Result<String, &'static str> {
    if let Some(text) = http::unquote_string_literal(cx, node) {
        let upper = text.to_ascii_uppercase();
        return if !upper.is_empty() && upper.chars().all(|c| c.is_ascii_uppercase()) {
            Ok(upper)
        } else {
            Err("test request method is not an HTTP method")
        };
    }
    let text: String = cx.text(&node).split_whitespace().collect();
    match text.rsplit_once("::") {
        Some((head, name))
            if head.ends_with("Method")
                && !name.is_empty()
                && name.chars().all(|c| c.is_ascii_uppercase()) =>
        {
            Ok(name.into())
        }
        _ => Err("test request method is computed"),
    }
}

/// A request target's path: a literal path, or an absolute URL's path,
/// without its query or fragment.
fn path(cx: &FileCx<'_>, node: TsNode<'_>) -> Result<String, &'static str> {
    let text = http::unquote_string_literal(cx, node).ok_or("test request URL is computed")?;
    let path = match text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
    {
        Some(rest) => rest.find('/').map_or("/", |at| &rest[at..]).to_string(),
        None if text.starts_with('/') => text,
        None => return Err("test request URL is not a path"),
    };
    let path = path.split(['?', '#']).next().unwrap_or("/");
    Ok(if path.is_empty() {
        "/".into()
    } else {
        path.into()
    })
}

/// Where a request chain starts: its default method and the argument of the
/// constructor that holds its target, if any. The type must resolve through
/// the file's `use`s (a test module's `use super::*` included) or a
/// qualified path to `http`, `axum`, `hyper` or `actix_web`, so a workspace
/// type named `Request` is not one.
fn chain_start<'t>(
    cx: &FileCx<'_>,
    bindings: &Bindings,
    call: TsNode<'t>,
) -> Option<(Option<&'static str>, Option<TsNode<'t>>)> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "scoped_identifier" {
        return None;
    }
    let text: String = cx.text(&function).split_whitespace().collect();
    let scopes = visible_scopes(cx, call);
    let mut paths = http::resolve_external_paths(cx, bindings, &scopes, &text, None);
    if paths.is_empty() {
        // `use super::*;` in a test module brings in the parent's own
        // `use`s: resolve the name there, once for each glob.
        let mut through: BTreeSet<Vec<String>> = BTreeSet::new();
        for prefix in bindings.globs(&scopes) {
            let scope = [prefix.join("::")];
            through.extend(http::resolve_external_paths(
                cx, bindings, &scope, &text, None,
            ));
        }
        paths = through.into_iter().collect();
    }
    let [path] = paths.as_slice() else {
        return None;
    };
    let (Some(krate), [.., ty, name]) = (path.first(), path.as_slice()) else {
        return None;
    };
    let (ty, name) = (ty.as_str(), name.as_str());
    let allowed = match ty {
        "Request" => matches!(krate.as_str(), "http" | "axum" | "hyper"),
        "TestRequest" => krate == "actix_web",
        _ => false,
    };
    if !allowed {
        return None;
    }
    let arguments = http::call_arguments(call);
    let verb = VERBS.iter().find(|verb| **verb == name).copied();
    match ty {
        "Request" if name == "builder" && arguments.is_empty() => Some((Some("get"), None)),
        "Request" if verb.is_some() && arguments.len() == 1 => Some((verb, Some(arguments[0]))),
        "TestRequest" if verb.is_some() && arguments.is_empty() => Some((verb, None)),
        "TestRequest" if name == "default" && arguments.is_empty() => Some((Some("get"), None)),
        "TestRequest" if name == "with_uri" && arguments.len() == 1 => {
            Some((Some("get"), Some(arguments[0])))
        }
        _ => None,
    }
}

/// The test cases a file defines and the requests they build.
pub(super) fn extract(
    cx: &FileCx<'_>,
    bindings: &Bindings,
    functions: &[(TsNode<'_>, String)],
    out: &mut Extraction,
    requests: &mut Vec<TestRequest>,
) {
    for (function, symbol) in functions {
        let Some(attribute) = test_attribute(cx, *function) else {
            continue;
        };
        let (Some(name), Some(body)) = (
            function.child_by_field_name("name"),
            function.child_by_field_name("body"),
        ) else {
            continue;
        };
        let name = cx.text(&name).to_string();
        let local = symbol
            .rsplit_once('#')
            .map_or(name.as_str(), |(_, local)| local);
        let test = test_id(cx.id.repo, cx.path, local);
        out.nodes.push(Node {
            id: test.clone(),
            label: "TestCase".into(),
            props: serde_json::json!({
                "name": name,
                "framework": "cargo test",
                "attribute": attribute,
                "language": "rust",
                "path": cx.path,
                "prov": cx.prov(function, &format!("TestCase {test}")),
            }),
        });
        out.edges.push(Edge {
            src: test.clone(),
            dst: file_id(cx.id.repo, cx.path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(function, &format!("DEFINED_IN {test}")),
            }),
        });
        out.edges.push(Edge {
            src: test.clone(),
            dst: symbol.clone(),
            label: "IMPLEMENTED_BY".into(),
            props: serde_json::json!({
                "prov": cx.prov(function, &format!("IMPLEMENTED_BY {test} -> {symbol}")),
            }),
        });
        let compared = statuses(cx, body);
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            stack.extend(named_children(node));
            if node.kind() != "call_expression" {
                continue;
            }
            let Some((verb, mut target)) = chain_start(cx, bindings, node) else {
                continue;
            };
            let mut method_node = None;
            // Walk up the builder chain: `start.uri(…).method(…).body(…)`.
            let mut link = node;
            while let Some(field) = link.parent().filter(|p| p.kind() == "field_expression")
                && let Some(call) = field.parent().filter(|p| p.kind() == "call_expression")
                && field
                    .child_by_field_name("value")
                    .is_some_and(|value| value.id() == link.id())
            {
                let step = field
                    .child_by_field_name("field")
                    .map(|f| cx.text(&f).to_string());
                let arguments = http::call_arguments(call);
                match (step.as_deref(), arguments.as_slice()) {
                    (Some("uri"), [uri]) => target = Some(*uri),
                    (Some("method"), [m]) => method_node = Some(*m),
                    _ => {}
                }
                link = call;
            }
            let Some(target) = target else {
                continue;
            };
            let method = match method_node {
                Some(m) => method(cx, m),
                None => Ok(verb.unwrap_or("get").to_ascii_uppercase()),
            };
            requests.push(TestRequest {
                test: test.clone(),
                method,
                path: path(cx, target),
                request: cx.text(&link).chars().take(160).collect(),
                statuses: compared.clone(),
                evidence: evidence(cx, link),
            });
        }
    }
}

/// The number of literal segments a route matches `path` with, or `None`
/// when it does not match. `:name` and `{name}` match one segment; a final
/// `*name`, `{*name}` or `{name:.*}` matches the rest.
fn route_match(route: &str, path: &str) -> Option<usize> {
    let rs: Vec<&str> = route.trim_end_matches('/').split('/').collect();
    let ps: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let mut fixed = 0;
    for (i, r) in rs.iter().enumerate() {
        let rest = r.starts_with('*') || r.starts_with("{*") || r.ends_with(":.*}");
        if rest && i + 1 == rs.len() {
            return (ps.len() > i).then_some(fixed);
        }
        let param = r.starts_with(':') || (r.len() > 2 && r.starts_with('{') && r.ends_with('}'));
        match ps.get(i) {
            Some(p) if p == r => fixed += 1,
            Some(p) if param && !p.is_empty() => {}
            _ => return None,
        }
    }
    (rs.len() == ps.len()).then_some(fixed)
}

/// Join each held request to the endpoint it reaches, or to a Gap.
pub(super) fn resolve(out: &mut Extraction, requests: Vec<TestRequest>) {
    if requests.is_empty() {
        return;
    }
    let endpoints: BTreeMap<String, (String, String)> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .filter_map(|n| {
            Some((
                n.id.clone(),
                (
                    n.props["method"].as_str()?.to_string(),
                    n.props["path"].as_str()?.to_string(),
                ),
            ))
        })
        .collect();
    let mut links: BTreeMap<(String, String), (BTreeSet<u16>, Vec<EvidenceRef>)> = BTreeMap::new();
    let mut gaps = Vec::new();
    for request in requests {
        let resolved = request.method.clone().and_then(|method| {
            let path = request.path.clone()?;
            let mut matches: Vec<(usize, &str)> = endpoints
                .iter()
                .filter(|(_, (m, _))| *m == method || m == "ANY")
                .filter_map(|(id, (_, route))| Some((route_match(route, &path)?, id.as_str())))
                .collect();
            matches.sort_by_key(|(fixed, _)| std::cmp::Reverse(*fixed));
            match matches.as_slice() {
                [] => Err("no recovered endpoint serves this path and method"),
                [(_, id)] => Ok(id.to_string()),
                [(best, id), (next, _), ..] if best > next => Ok(id.to_string()),
                _ => Err("test request path matches more than one recovered endpoint"),
            }
        });
        match resolved {
            Ok(endpoint) => {
                let link = links.entry((request.test, endpoint)).or_default();
                link.0.extend(request.statuses);
                link.1.push(request.evidence);
            }
            Err(reason) => gaps.push((request, reason)),
        }
    }
    let prov = |evidence: Vec<EvidenceRef>, confidence, fact: &str| {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                confidence,
                evidence,
                EXTRACTOR_ID,
                fact.as_bytes(),
            )
            .expect("Deterministic confidence is within its ceiling"),
        )
        .expect("provenance serializes")
    };
    for ((test, endpoint), (statuses, evidence)) in links {
        out.edges.push(Edge {
            props: serde_json::json!({
                "via": ["path"],
                "statuses": statuses,
                "keys": [],
                "link": "handler_assumed",
                "prov": prov(evidence, ConfidenceTier::InferredStrong, &format!("REQUESTS {test} -> {endpoint}")),
            }),
            src: test,
            dst: endpoint,
            label: "REQUESTS".into(),
        });
    }
    for (request, reason) in gaps {
        let gap = format!(
            "gap:test-call:{}@{}@{}",
            request.evidence.repo, request.evidence.path, request.evidence.byte_start
        );
        out.nodes.push(Node {
            id: gap.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "request": request.request,
                "attempted_tiers": ["T0"],
                "prov": prov(vec![request.evidence.clone()], ConfidenceTier::Gap, &format!("Gap {gap}")),
            }),
        });
        out.edges.push(Edge {
            props: serde_json::json!({
                "reason": reason,
                "prov": prov(vec![request.evidence], ConfidenceTier::Gap, &format!("REQUESTS {} -> {gap}", request.test)),
            }),
            src: request.test,
            dst: gap,
            label: "REQUESTS".into(),
        });
    }
}
