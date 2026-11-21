//! Go target tests (#2019): `go test` functions and the HTTP requests they
//! build, linked to the recovered endpoints they exercise. These are static
//! references with declared expectations: they do not prove that a test
//! ran, or which branches it reached.
//!
//! A test file (`*_test.go`) is read for its tests only: its functions are
//! Symbols with the calls they make, so a test's static references reach
//! the code it calls, and nothing else in it (routes, client calls, config
//! reads, resources, rules) is a fact about the system.
//!
//! A test is a top-level `func TestXxx(t *testing.T)` whose `testing`
//! import is proven. Each becomes a Confirmed `TestCase` citing its
//! definition, `DEFINED_IN` its file and `IMPLEMENTED_BY` its function.
//!
//! A request is `httptest.NewRequest(method, target, body)` or
//! `http.NewRequest(…)` / `http.NewRequestWithContext(ctx, …)` in the test's
//! own body, through a proven import. Its method is a string literal or an
//! `http.MethodXxx` constant; its target is a literal path or absolute URL,
//! read without origin and query. The request is joined, once the package
//! is read, to the endpoint whose route matches the path and that serves
//! the method (`ANY` serves every method); the most specific route wins. A
//! computed method or target, a path no recovered endpoint serves, or two
//! equally specific matches become a Gap that names the request.
//!
//! Each resolved link is one InferredStrong `REQUESTS` edge per test and
//! endpoint, marked `handler_assumed`: the handler the test serves the
//! request to is not proven to be the one routed there. It records the
//! statuses the test compares a response code with (`rec.Code != 201`,
//! `resp.StatusCode == http.StatusOK`) anywhere in its body.

use super::*;
use core_prov::EvidenceRef;

/// A request a test builds, held until the package's endpoints are known.
#[derive(Debug, Clone)]
pub(super) struct Request {
    test: String,
    method: Result<String, &'static str>,
    path: Result<String, &'static str>,
    request: String,
    statuses: BTreeSet<u16>,
    evidence: EvidenceRef,
}

impl Request {
    pub(super) fn retarget(&mut self, commit: &str) {
        self.evidence.commit_sha = commit.into();
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

/// The local name a proven import of `path` is bound to.
fn import_name<'b>(bindings: &'b HashMap<String, ImportBinding>, path: &str) -> Option<&'b str> {
    bindings
        .iter()
        .find(|(_, binding)| binding.path == path)
        .map(|(name, _)| name.as_str())
}

/// Whether `function` is `func TestXxx(t *testing.T)`.
fn is_test(cx: &FileCx<'_>, function: TsNode<'_>, testing: &str) -> Option<String> {
    if function.kind() != "function_declaration" {
        return None;
    }
    let name = cx.text(&function.child_by_field_name("name")?);
    let rest = name.strip_prefix("Test")?;
    // `Testing` is not a test: the name after `Test` must not start lower.
    if rest.chars().next().is_some_and(|c| c.is_lowercase()) {
        return None;
    }
    let parameters = function.child_by_field_name("parameters")?;
    let mut walk = parameters.walk();
    let params: Vec<_> = parameters.named_children(&mut walk).collect();
    let [param] = params.as_slice() else {
        return None;
    };
    let ty = cx.text(&param.child_by_field_name("type")?);
    (ty.replace(' ', "") == format!("*{testing}.T")).then(|| name.to_string())
}

/// A status code a node names: an integer literal or `http.StatusXxx`.
fn status(cx: &FileCx<'_>, node: TsNode<'_>, http: Option<&str>) -> Option<u16> {
    let text = cx.text(&node);
    if node.kind() == "int_literal" {
        return text.parse().ok().filter(|code| (100..600).contains(code));
    }
    let name = text.strip_prefix(&format!("{}.", http?))?;
    STATUSES
        .iter()
        .find(|(constant, _)| *constant == name)
        .map(|(_, code)| *code)
}

/// The statuses a test compares a response code with.
fn statuses(cx: &FileCx<'_>, body: TsNode<'_>, http: Option<&str>) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.kind() == "binary_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|op| matches!(cx.text(&op), "==" | "!="))
            && let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            )
        {
            for (code, other) in [(left, right), (right, left)] {
                let field = (code.kind() == "selector_expression")
                    .then(|| code.child_by_field_name("field"))
                    .flatten()
                    .map(|f| cx.text(&f));
                if matches!(field, Some("Code" | "StatusCode"))
                    && let Some(status) = status(cx, other, http)
                {
                    out.insert(status);
                }
            }
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// A request method: a string literal or `http.MethodXxx`.
fn method(cx: &FileCx<'_>, node: TsNode<'_>, http: Option<&str>) -> Result<String, &'static str> {
    if let Some(text) = literal_string(cx, node) {
        let upper = text.to_ascii_uppercase();
        return if upper.chars().all(|c| c.is_ascii_uppercase()) && !upper.is_empty() {
            Ok(upper)
        } else {
            Err("test request method is not an HTTP method")
        };
    }
    let text = cx.text(&node);
    http.and_then(|http| text.strip_prefix(&format!("{http}.Method")))
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_uppercase)
        .ok_or("test request method is computed")
}

/// A request target's path: a literal path, or an absolute URL's path,
/// without its query or fragment.
fn path(cx: &FileCx<'_>, node: TsNode<'_>) -> Result<String, &'static str> {
    let text = literal_string(cx, node).ok_or("test request URL is computed")?;
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

/// The test cases a test file defines and the requests they build; the
/// caller keeps only the test-relevant facts of the file.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let Some(testing) = import_name(bindings, "testing") else {
        return;
    };
    let http = import_name(bindings, "net/http");
    let httptest = import_name(bindings, "net/http/httptest");
    let mut walk = root.walk();
    for function in root.named_children(&mut walk) {
        let Some(name) = is_test(cx, function, testing) else {
            continue;
        };
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        let test = test_id(cx.id.repo, cx.path, &name);
        let symbol = symbol_id(cx.id.repo, cx.path, &name);
        out.nodes.push(Node {
            id: test.clone(),
            label: "TestCase".into(),
            props: serde_json::json!({
                "name": name,
                "framework": "go test",
                "language": "go",
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
        out.edges.push(Edge {
            src: test.clone(),
            dst: symbol.clone(),
            label: "IMPLEMENTED_BY".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("IMPLEMENTED_BY {test} -> {symbol}")),
            }),
        });
        let compared = statuses(cx, body, http);
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            let mut walk = node.walk();
            stack.extend(node.named_children(&mut walk));
            if node.kind() != "call_expression" {
                continue;
            }
            let Some(callee) = node.child_by_field_name("function") else {
                continue;
            };
            let callee = cx.text(&callee);
            let arguments = call_arguments(node);
            let (method_at, target_at) = match callee.split_once('.') {
                Some((pkg, "NewRequest")) if Some(pkg) == httptest || Some(pkg) == http => (0, 1),
                Some((pkg, "NewRequestWithContext")) if Some(pkg) == http => (1, 2),
                _ => continue,
            };
            let (Some(m), Some(t)) = (arguments.get(method_at), arguments.get(target_at)) else {
                continue;
            };
            out.test_requests.push(Request {
                test: test.clone(),
                method: method(cx, *m, http),
                path: path(cx, *t),
                request: format!("{} {}", cx.text(m), cx.text(t)),
                statuses: compared.clone(),
                evidence: evidence(cx, node),
            });
        }
    }
}

/// Keep only what a test file says about its tests: the file, its
/// functions and the calls they make, and its test cases.
pub(super) fn keep_test_facts(out: &mut Extraction) {
    out.nodes
        .retain(|n| matches!(n.label.as_str(), "File" | "Symbol" | "TestCase"));
    out.edges
        .retain(|e| matches!(e.label.as_str(), "DEFINED_IN" | "CALLS" | "IMPLEMENTED_BY"));
    out.fetch_sites.clear();
    out.event_sites.clear();
    out.pending_endpoints.clear();
    out.handler_functions.clear();
    out.pending_bases.clear();
    out.base_bindings.clear();
}

/// The number of literal segments a route matches `path` with, or `None`
/// when it does not match. `{name}` and `:name` match one segment; a final
/// `{name...}` or `*` matches the rest; a final `{$}` matches nothing more.
fn route_match(route: &str, path: &str) -> Option<usize> {
    // `{$}` (Go 1.22) anchors the end: the path must stop there.
    if let Some(prefix) = route.strip_suffix("{$}") {
        return (path == prefix).then(|| prefix.split('/').filter(|s| !s.is_empty()).count() + 1);
    }
    let rs: Vec<&str> = route.trim_end_matches('/').split('/').collect();
    let ps: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let mut fixed = 0;
    for (i, r) in rs.iter().enumerate() {
        let rest = *r == "*" || (r.starts_with('{') && r.ends_with("...}"));
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
pub(super) fn resolve(out: &mut Extraction, repo: &str) {
    let requests = std::mem::take(&mut out.test_requests);
    if requests.is_empty() {
        return;
    }
    let endpoints: Vec<(String, String, String)> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .filter_map(|n| {
            Some((
                n.props["method"].as_str()?.to_string(),
                n.props["path"].as_str()?.to_string(),
                n.id.clone(),
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
                .filter(|(m, _, _)| *m == method || m == "ANY")
                .filter_map(|(_, route, id)| Some((route_match(route, &path)?, id.as_str())))
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
            "gap:test-call:{repo}@{}@{}",
            request.evidence.path, request.evidence.byte_start
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

/// `net/http` status constants by name.
const STATUSES: &[(&str, u16)] = &[
    ("StatusContinue", 100),
    ("StatusSwitchingProtocols", 101),
    ("StatusOK", 200),
    ("StatusCreated", 201),
    ("StatusAccepted", 202),
    ("StatusNoContent", 204),
    ("StatusResetContent", 205),
    ("StatusPartialContent", 206),
    ("StatusMovedPermanently", 301),
    ("StatusFound", 302),
    ("StatusSeeOther", 303),
    ("StatusNotModified", 304),
    ("StatusTemporaryRedirect", 307),
    ("StatusPermanentRedirect", 308),
    ("StatusBadRequest", 400),
    ("StatusUnauthorized", 401),
    ("StatusPaymentRequired", 402),
    ("StatusForbidden", 403),
    ("StatusNotFound", 404),
    ("StatusMethodNotAllowed", 405),
    ("StatusNotAcceptable", 406),
    ("StatusRequestTimeout", 408),
    ("StatusConflict", 409),
    ("StatusGone", 410),
    ("StatusLengthRequired", 411),
    ("StatusPreconditionFailed", 412),
    ("StatusRequestEntityTooLarge", 413),
    ("StatusUnsupportedMediaType", 415),
    ("StatusUnprocessableEntity", 422),
    ("StatusLocked", 423),
    ("StatusPreconditionRequired", 428),
    ("StatusTooManyRequests", 429),
    ("StatusRequestHeaderFieldsTooLarge", 431),
    ("StatusInternalServerError", 500),
    ("StatusNotImplemented", 501),
    ("StatusBadGateway", 502),
    ("StatusServiceUnavailable", 503),
    ("StatusGatewayTimeout", 504),
];
