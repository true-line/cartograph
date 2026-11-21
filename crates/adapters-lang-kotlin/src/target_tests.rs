//! Kotlin target tests (#2021, AC-1102): JUnit and `kotlin.test` test
//! functions and the HTTP requests they perform against the repository's
//! own endpoints. These are static references with declared expectations:
//! they do not prove that a test ran, or which branches it reached.
//!
//! A test is a function annotated `@Test`, `@ParameterizedTest` or
//! `@RepeatedTest`, proven by the qualified spelling, a named import, or a
//! star import of `org.junit.jupiter.api` / `org.junit.jupiter.params`
//! (JUnit 5), `org.junit` (JUnit 4) or `kotlin.test`. One proven only by a
//! star import is no test when a product source declares that name in the
//! test's package. Each becomes a Confirmed `TestCase` (framework `junit`)
//! citing its function, `DEFINED_IN` its file and `IMPLEMENTED_BY` its
//! function Symbol.
//!
//! A request is, in the test's own body:
//!
//! - Spring's Kotlin MockMvc DSL, `mockMvc.get("/path", …) { … }` (and
//!   `post`, `put`, `patch`, `delete`, `head`, `options`), each verb
//!   proven by a named or star import of `org.springframework.test.web.servlet`;
//! - a `MockMvcRequestBuilders` builder (`get(…)`, `request(HttpMethod.X,
//!   …)`), proven by a named import, a star import of the class's members,
//!   or the class name as receiver;
//! - a Ktor test client call, `client.get("/path")`, proven by a named or
//!   star import of `io.ktor.client.request`.
//!
//! Its URL is a string literal; a `$x` or `${…}` template part stands for
//! one variable path segment, as a URI template `{id}` does. The request is
//! joined, once the repository is read, to the Spring or Ktor endpoint whose
//! route matches the path and serves the method; the most specific route
//! wins. A computed URL, a path no recovered endpoint serves, and two
//! equally specific matches become a Gap that names the request.
//!
//! Each resolved link is one InferredStrong `REQUESTS` edge per test and
//! endpoint, marked `handler_assumed`, with the statuses the test expects
//! (`status { isOk() }`, `status().isNotFound()`) anywhere in its body.
//! A verb call on a receiver counts only when the receiver is a declared
//! test client: a property or parameter typed `MockMvc` or `HttpClient`, a
//! value built by `HttpClient(…)` or `createClient`, or `client` inside a
//! `testApplication { … }` block. MockK stubs and verifications
//! (`every { … }`, `verify { … }`) are not read.

use super::rest_template::named_children;
use super::*;

/// A request a test performs, held until the repository's endpoints are
/// known.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    test: String,
    method: Option<String>,
    path: Option<String>,
    request: String,
    statuses: BTreeSet<u16>,
    evidence: EvidenceRef,
}

impl Request {
    pub(crate) fn retarget(&mut self, commit: &str) {
        self.evidence.commit_sha = commit.into();
    }
}

const JUNIT: &[(&str, &str)] = &[
    ("Test", "org.junit.jupiter.api"),
    ("RepeatedTest", "org.junit.jupiter.api"),
    ("ParameterizedTest", "org.junit.jupiter.params"),
    ("Test", "org.junit"),
    ("Test", "kotlin.test"),
];

const VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];
const MOCK_MVC_DSL: &str = "org.springframework.test.web.servlet";
const BUILDERS: &str = "org.springframework.test.web.servlet.request.MockMvcRequestBuilders";
const KTOR_CLIENT: &str = "io.ktor.client.request";

/// Whether an annotation is a JUnit test marker: `Some(false)` when its
/// spelling or a named import proves it, `Some(true)` when only a star
/// import does.
fn test_annotation(annotation: &AnnotationUse<'_>, imports: &Imports) -> Option<bool> {
    let name = annotation.simple.as_str();
    let mut star = None;
    for (simple, package) in JUNIT {
        let fqn = format!("{package}.{simple}");
        // A qualified spelling, or a named import under its own name or an
        // alias.
        if annotation.spelled == fqn || imports.bindings.get(name) == Some(&fqn) {
            return Some(false);
        }
        if *simple == name
            && annotation.spelled == name
            && !imports.bindings.contains_key(name)
            && imports.wildcard_packages.iter().any(|p| p == package)
        {
            star = Some(true);
        }
    }
    star
}

/// Whether the file proves `member` of `package`: a named import of it, or
/// a star import of the package that no named import of `member` overrides.
fn imported(imports: &Imports, package: &str, member: &str) -> bool {
    match imports.bindings.get(member) {
        Some(bound) => *bound == format!("{package}.{member}"),
        None => imports.wildcard_packages.iter().any(|p| p == package),
    }
}

/// A call's callee as `(receiver, name)`, for `f(…)` and `x.f(…)`.
fn callee<'t>(cx: &FileCx<'_>, call: TsNode<'t>) -> Option<(Option<TsNode<'t>>, String)> {
    let head = call.named_child(0)?;
    match head.kind() {
        "identifier" => Some((None, cx.text(&head).to_string())),
        "navigation_expression" => {
            let parts = named_children(head);
            let [receiver, name] = parts.as_slice() else {
                return None;
            };
            (name.kind() == "identifier").then(|| (Some(*receiver), cx.text(name).to_string()))
        }
        _ => None,
    }
}

/// A call's value arguments, comments left out.
fn value_arguments<'t>(call: TsNode<'t>) -> Option<Vec<TsNode<'t>>> {
    let list = named_children(call)
        .into_iter()
        .find(|child| child.kind() == "value_arguments")?;
    Some(
        named_children(list)
            .into_iter()
            .filter(|argument| argument.kind() == "value_argument")
            .filter_map(|argument| named_children(argument).into_iter().last())
            .collect(),
    )
}

const MOCK_MVC: (&str, &str) = ("org.springframework.test.web.servlet", "MockMvc");
const HTTP_CLIENT: (&str, &str) = ("io.ktor.client", "HttpClient");
const KTOR_TESTING: &str = "io.ktor.server.testing";
/// MockK calls whose lambdas stub or verify, never perform, a call.
const MOCKK_BLOCKS: &[&str] = &[
    "every",
    "coEvery",
    "verify",
    "coVerify",
    "verifyAll",
    "coVerifyAll",
    "verifyOrder",
    "coVerifyOrder",
    "verifySequence",
    "coVerifySequence",
    "excludeRecords",
];

/// The names a file declares as test clients: properties and parameters
/// typed `MockMvc` or Ktor's `HttpClient`, and values built by
/// `HttpClient(…)` or a test application's `createClient { … }`.
#[derive(Default)]
struct Clients<'t> {
    mock_mvc: BTreeSet<String>,
    ktor: BTreeSet<String>,
    /// `testApplication { … }` lambdas, whose receiver has a `client`.
    test_applications: Vec<TsNode<'t>>,
}

impl Clients<'_> {
    fn in_test_application(&self, call: TsNode<'_>) -> bool {
        self.test_applications.iter().any(|lambda| {
            lambda.start_byte() <= call.start_byte() && call.end_byte() <= lambda.end_byte()
        })
    }
}

/// Whether a type as spelled is `package.simple`.
fn names_type(spelled: &str, imports: &Imports, (package, simple): (&str, &str)) -> bool {
    let spelled = spelled.trim_end_matches('?');
    spelled == format!("{package}.{simple}")
        || (spelled == simple && imported(imports, package, simple))
}

fn clients<'t>(cx: &FileCx<'_>, root: TsNode<'t>, imports: &Imports) -> Clients<'t> {
    let mut out = Clients::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        match node.kind() {
            "property_declaration" | "class_parameter" | "parameter" => {
                let parts = named_children(node);
                let holder = parts
                    .iter()
                    .find(|c| c.kind() == "variable_declaration")
                    .copied()
                    .unwrap_or(node);
                let held = named_children(holder);
                let Some(name) = held.iter().find(|c| c.kind() == "identifier") else {
                    continue;
                };
                let name = cx.text(name).to_string();
                let ty = held
                    .iter()
                    .chain(parts.iter())
                    .find(|c| matches!(c.kind(), "user_type" | "nullable_type"))
                    .map(|ty| cx.text(ty).split_whitespace().collect::<String>());
                let built = parts
                    .iter()
                    .filter(|c| c.kind() == "call_expression")
                    .filter_map(|c| callee(cx, *c))
                    .any(|(receiver, callee)| {
                        (receiver.is_none()
                            && callee == "HttpClient"
                            && imported(imports, HTTP_CLIENT.0, HTTP_CLIENT.1))
                            || callee == "createClient"
                    });
                match ty {
                    Some(ty) if names_type(&ty, imports, MOCK_MVC) => {
                        out.mock_mvc.insert(name);
                    }
                    Some(ty) if names_type(&ty, imports, HTTP_CLIENT) => {
                        out.ktor.insert(name);
                    }
                    None if built => {
                        out.ktor.insert(name);
                    }
                    _ => {}
                }
            }
            "call_expression" => {
                if let Some((None, name)) = callee(cx, node)
                    && name == "testApplication"
                    && imported(imports, KTOR_TESTING, "testApplication")
                    && let Some(lambda) = trailing_lambda(node)
                {
                    out.test_applications.push(lambda);
                }
            }
            _ => {}
        }
    }
    out
}

/// A call's trailing lambda.
fn trailing_lambda(call: TsNode<'_>) -> Option<TsNode<'_>> {
    named_children(call)
        .into_iter()
        .find(|c| c.kind() == "annotated_lambda")
        .and_then(|annotated| {
            named_children(annotated)
                .into_iter()
                .find(|c| c.kind() == "lambda_literal")
        })
}

/// Whether `call` is a MockK stub or verification block (`every { … }`).
fn mockk_block(cx: &FileCx<'_>, call: TsNode<'_>) -> bool {
    callee(cx, call)
        .is_some_and(|(receiver, name)| receiver.is_none() && MOCKK_BLOCKS.contains(&name.as_str()))
}

/// A request call: its method (`None` when computed) and URL argument.
fn request<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    imports: &Imports,
    clients: &Clients,
) -> Option<(Option<String>, Option<TsNode<'t>>)> {
    let (receiver, name) = callee(cx, call)?;
    let args = value_arguments(call)?;
    let builder_receiver = receiver.is_some_and(|receiver| {
        let text = cx.text(&receiver);
        text == "MockMvcRequestBuilders" || text == BUILDERS
    });
    let proven = if builder_receiver {
        true
    } else if let Some(receiver) = receiver {
        // A member of the receiver's own type wins over an imported
        // extension, so the receiver must be the test client itself.
        let receiver = cx.text(&receiver);
        (imported(imports, MOCK_MVC_DSL, &name) && clients.mock_mvc.contains(receiver))
            || (imported(imports, KTOR_CLIENT, &name)
                && (clients.ktor.contains(receiver)
                    || (receiver == "client" && clients.in_test_application(call))))
    } else {
        imported(imports, BUILDERS, &name)
    };
    if !proven {
        return None;
    }
    if VERBS.contains(&name.as_str()) {
        return Some((Some(name.to_ascii_uppercase()), args.first().copied()));
    }
    if name == "request" && (builder_receiver || receiver.is_none()) {
        let method = args.first().and_then(|m| {
            cx.text(m)
                .rsplit_once('.')
                .filter(|(class, _)| class.ends_with("HttpMethod"))
                .map(|(_, verb)| verb.to_ascii_uppercase())
        });
        return Some((method, args.get(1).copied()));
    }
    None
}

/// A URL string literal with each `$x` / `${…}` template part as `{}`;
/// `None` for anything else (a constant, a raw string, an escape).
fn url_text(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let text = cx.text(&node);
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    if inner.starts_with('"') || inner.contains('\\') {
        return None;
    }
    let mut out = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                let mut depth = 0;
                for c in chars.by_ref() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                out.push_str("{}");
            }
            Some(c) if c.is_alphabetic() || *c == '_' => {
                while chars
                    .peek()
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                {
                    chars.next();
                }
                out.push_str("{}");
            }
            _ => out.push('$'),
        }
    }
    Some(out)
}

/// HTTP statuses `isXxx()` names.
const STATUSES: &[(&str, u16)] = &[
    ("isOk", 200),
    ("isCreated", 201),
    ("isAccepted", 202),
    ("isNoContent", 204),
    ("isMovedPermanently", 301),
    ("isFound", 302),
    ("isNotModified", 304),
    ("isBadRequest", 400),
    ("isUnauthorized", 401),
    ("isForbidden", 403),
    ("isNotFound", 404),
    ("isMethodNotAllowed", 405),
    ("isNotAcceptable", 406),
    ("isConflict", 409),
    ("isGone", 410),
    ("isUnsupportedMediaType", 415),
    ("isUnprocessableEntity", 422),
    ("isTooManyRequests", 429),
    ("isInternalServerError", 500),
    ("isServiceUnavailable", 503),
];

/// Whether `node` sits in the trailing lambda of a `status { … }` call.
fn in_status_block(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "lambda_literal" {
            return parent
                .parent()
                .filter(|annotated| annotated.kind() == "annotated_lambda")
                .and_then(|annotated| annotated.parent())
                .filter(|call| call.kind() == "call_expression")
                .and_then(|call| callee(cx, call))
                .is_some_and(|(receiver, name)| receiver.is_none() && name == "status");
        }
        current = parent;
    }
    false
}

/// The statuses a test expects of its responses.
fn statuses(cx: &FileCx<'_>, body: TsNode<'_>) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() != "call_expression" {
            continue;
        }
        let Some((receiver, name)) = callee(cx, node) else {
            continue;
        };
        let on_status = match receiver {
            // `status().isOk()`
            Some(receiver) => {
                receiver.kind() == "call_expression"
                    && callee(cx, receiver)
                        .is_some_and(|(inner, name)| inner.is_none() && name == "status")
            }
            // `status { isOk() }`
            None => in_status_block(cx, node),
        };
        if !on_status {
            continue;
        }
        if let Some((_, code)) = STATUSES.iter().find(|(n, _)| *n == name) {
            out.insert(*code);
        } else if name == "is"
            && let Some(code) = value_arguments(node)
                .and_then(|args| args.first().copied())
                .filter(|arg| arg.kind() == "number_literal")
                .and_then(|arg| cx.text(&arg).parse::<u16>().ok())
        {
            out.insert(code);
        }
    }
    out
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

/// The test cases a file defines, into `nodes`/`edges`, and the requests
/// they perform, into `requests`.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    functions: &[(TsNode<'_>, String)],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    requests: &mut Vec<Request>,
) {
    // The file's test clients, read once from its root.
    let Some(mut root) = functions.first().map(|(function, _)| *function) else {
        return;
    };
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let clients = &clients(cx, root, imports);
    for (function, symbol) in functions {
        let proofs: Vec<(String, bool)> = declaration_annotations(cx, *function)
            .iter()
            .filter_map(|annotation| {
                Some((
                    annotation.simple.clone(),
                    test_annotation(annotation, imports)?,
                ))
            })
            .collect();
        if proofs.is_empty() {
            continue;
        }
        let shadowed_by: Vec<String> = if proofs.iter().any(|(_, star)| !star) {
            Vec::new()
        } else {
            proofs
                .into_iter()
                .map(|(name, _)| match package {
                    Some(package) => format!("{package}.{name}"),
                    None => name,
                })
                .collect()
        };
        let qualified = symbol.rsplit_once('#').map_or(symbol.as_str(), |(_, q)| q);
        let test = format!("test:{}@{}#{qualified}", cx.id.repo, cx.path);
        let mut props = serde_json::json!({
            "name": qualified,
            "framework": "junit",
            "language": "kotlin",
            "path": cx.path,
            "prov": cx.prov(function, &format!("TestCase {test}")),
        });
        if !shadowed_by.is_empty() {
            props["shadowed_by"] = shadowed_by.into();
        }
        nodes.push(Node {
            id: test.clone(),
            label: "TestCase".into(),
            props,
        });
        edges.push(Edge {
            src: test.clone(),
            dst: file_id(cx.id.repo, cx.path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(function, &format!("DEFINED_IN {test}")),
            }),
        });
        edges.push(Edge {
            src: test.clone(),
            dst: symbol.clone(),
            label: "IMPLEMENTED_BY".into(),
            props: serde_json::json!({
                "prov": cx.prov(function, &format!("IMPLEMENTED_BY {test} -> {symbol}")),
            }),
        });
        let Some(body) = named_children(*function)
            .into_iter()
            .find(|child| child.kind() == "function_body")
        else {
            continue;
        };
        let expected = statuses(cx, body);
        let mut found = Vec::new();
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression" && mockk_block(cx, node) {
                continue;
            }
            stack.extend(named_children(node));
            if node.kind() == "call_expression"
                && let Some((method, url)) = request(cx, node, imports, clients)
            {
                found.push((node, method, url));
            }
        }
        found.sort_by_key(|(node, _, _)| node.start_byte());
        for (node, method, url) in found {
            requests.push(Request {
                test: test.clone(),
                method,
                path: url.and_then(|url| url_text(cx, url)),
                request: cx
                    .text(&node)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                statuses: expected.clone(),
                evidence: evidence(cx, node),
            });
        }
    }
}

/// Whether a TestCase proven only by a star import waits on the shadow
/// check, which needs the repository's product Java types.
pub(crate) fn awaits_shadow_check(nodes: &[Node]) -> bool {
    nodes
        .iter()
        .any(|n| n.label == "TestCase" && n.props.get("shadowed_by").is_some())
}

/// A request URL's path: without origin, query or fragment.
fn path(url: &str) -> Option<String> {
    let path = match url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        Some(rest) => rest.find('/').map_or("/", |at| &rest[at..]).to_string(),
        None if url.starts_with('/') => url.to_string(),
        None => return None,
    };
    let path = path.split(['?', '#']).next().unwrap_or("/");
    Some(if path.is_empty() {
        "/".into()
    } else {
        path.into()
    })
}

/// The number of literal segments a route matches `path` with, or `None`.
/// A route's `{var}` matches one segment, a literal or the test's own
/// `{var}`; a literal route segment needs the same literal.
fn route_match(route: &str, path: &str) -> Option<usize> {
    let var = |s: &str| s.starts_with('{') && s.ends_with('}');
    let rs: Vec<&str> = route.split('/').filter(|s| !s.is_empty()).collect();
    let ps: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if rs.len() != ps.len() {
        return None;
    }
    let mut fixed = 0;
    for (r, p) in rs.iter().zip(&ps) {
        if r == p && !var(r) {
            fixed += 1;
        } else if !var(r) {
            return None;
        }
    }
    Some(fixed)
}

/// Settle star-proven tests and join each held request to the endpoint it
/// reaches, or to a Gap. `declared(fqn, path)` says whether a product
/// source obscures a star import of `fqn` for a file at `path`.
pub(crate) fn resolve(
    requests: Vec<Request>,
    repo: &str,
    declared: impl Fn(&str, &str) -> bool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let mut shadowed: BTreeSet<String> = BTreeSet::new();
    for node in nodes.iter_mut() {
        if node.label != "TestCase" {
            continue;
        }
        let path = node.props["path"].as_str().unwrap_or_default().to_string();
        let Some(names) = node
            .props
            .as_object_mut()
            .and_then(|props| props.remove("shadowed_by"))
        else {
            continue;
        };
        let names: Vec<&str> = names
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n.as_str())
            .collect();
        if !names.is_empty() && names.iter().all(|fqn| declared(fqn, &path)) {
            shadowed.insert(node.id.clone());
        }
    }
    if !shadowed.is_empty() {
        nodes.retain(|n| !shadowed.contains(&n.id));
        edges.retain(|e| !shadowed.contains(&e.src));
    }
    let requests: Vec<Request> = requests
        .into_iter()
        .filter(|r| !shadowed.contains(&r.test))
        .collect();
    if requests.is_empty() {
        return;
    }
    let endpoints: BTreeMap<String, (String, String)> = nodes
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
        let resolved = (|| {
            let method = request
                .method
                .clone()
                .ok_or("test request method is computed")?;
            let url = request
                .path
                .as_deref()
                .ok_or("test request URL is computed")?;
            let path = path(url).ok_or("test request URL is not a path")?;
            let mut matches: Vec<(usize, &str)> = endpoints
                .iter()
                .filter(|(_, (m, _))| *m == method || m == "ALL")
                .filter_map(|(id, (_, route))| Some((route_match(route, &path)?, id.as_str())))
                .collect();
            matches.sort_by_key(|(fixed, _)| std::cmp::Reverse(*fixed));
            match matches.as_slice() {
                [] => Err("no recovered endpoint serves this path and method"),
                [(_, id)] => Ok(id.to_string()),
                [(best, id), (next, _), ..] if best > next => Ok(id.to_string()),
                _ => Err("test request path matches more than one recovered endpoint"),
            }
        })();
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
        edges.push(Edge {
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
        nodes.push(Node {
            id: gap.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "request": request.request.chars().take(200).collect::<String>(),
                "language": "kotlin",
                "attempted_tiers": ["T0"],
                "prov": prov(vec![request.evidence.clone()], ConfidenceTier::Gap, &format!("Gap {gap}")),
            }),
        });
        edges.push(Edge {
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

#[cfg(test)]
mod tests {
    use super::{path, route_match};

    #[test]
    fn request_paths_match_the_most_literal_route() {
        assert_eq!(
            path("http://localhost:8080/a/b?x=1").as_deref(),
            Some("/a/b")
        );
        assert_eq!(path("relative"), None);
        assert_eq!(route_match("/owners/{id}", "/owners/{}"), Some(1));
        assert_eq!(route_match("/owners/new", "/owners/new"), Some(2));
        assert_eq!(route_match("/owners/new", "/owners/{}"), None);
        assert_eq!(route_match("/owners/{id}", "/owners/1/pets"), None);
    }
}
