//! Java target tests (#2020): JUnit test methods and the Spring MockMvc
//! requests they perform, linked to the recovered Spring endpoints they
//! exercise. These are static references with declared expectations: they
//! do not prove that a test ran, or which branches it reached.
//!
//! A test is a method annotated `@Test`, `@ParameterizedTest` or
//! `@RepeatedTest` whose annotation is proven by a named or wildcard import
//! of `org.junit.jupiter.api` / `org.junit.jupiter.params` (JUnit 5) or
//! `org.junit` (JUnit 4), or by its qualified spelling; one proven by a
//! wildcard import alone is no test when the file's package declares a type
//! of that name, which Java would bind instead. Each becomes a
//! Confirmed `TestCase` citing its method, `DEFINED_IN` its file and
//! `IMPLEMENTED_BY` its method Symbol.
//!
//! A request is a `MockMvcRequestBuilders` call (`get`, `post`, `put`,
//! `patch`, `delete`, `head`, `options`, or `request(HttpMethod.X, …)`) in
//! the test's own body, proven by a static import or its class name. Its URL
//! is a literal, a `+` concatenation, or a `static final String` constant the
//! repository declares once (constants built from other constants included);
//! a URI template (`/api/labels/{id}`) keeps its variables. The request is
//! joined, once the repository is read, to the Spring endpoint whose route
//! matches the path and serves the method; the most specific route wins. A
//! computed URL, a path no recovered endpoint serves, or two equally specific
//! matches become a Gap that names the request.
//!
//! Each resolved link is one InferredStrong `REQUESTS` edge per test and
//! endpoint, marked `handler_assumed`, with the statuses the test expects
//! (`status().isCreated()`, `status().is(404)`) anywhere in its body.

use super::method_security::{Part, parts};
use super::*;

const BUILDERS: &str = "org.springframework.test.web.servlet.request.MockMvcRequestBuilders";

/// A request a test performs, held until the repository's endpoints and
/// constants are known.
#[derive(Debug, Clone)]
pub(super) struct Request {
    test: String,
    method: Option<String>,
    url: Option<Vec<Part>>,
    request: String,
    statuses: BTreeSet<u16>,
    evidence: EvidenceRef,
}

impl Request {
    pub(super) fn retarget(&mut self, commit: &str) {
        self.evidence.commit_sha = commit.into();
    }
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

/// The file's static imports: (class FQN, member or `*`).
pub(super) fn static_imports(cx: &FileCx<'_>, root: TsNode<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut walk = root.walk();
    for node in root.named_children(&mut walk) {
        if node.kind() != "import_declaration" {
            continue;
        }
        let text = cx.text(&node).split_whitespace().collect::<String>();
        let Some(rest) = text
            .strip_prefix("importstatic")
            .map(|r| r.trim_end_matches(';'))
        else {
            continue;
        };
        if let Some((class, member)) = rest.rsplit_once('.') {
            out.push((class.to_string(), member.to_string()));
        }
    }
    out
}

const JUNIT: &[(&str, &str)] = &[
    ("Test", "org.junit.jupiter.api"),
    ("RepeatedTest", "org.junit.jupiter.api"),
    ("ParameterizedTest", "org.junit.jupiter.params"),
    ("Test", "org.junit"),
];

/// Whether `node` is a JUnit test annotation: `Some(false)` when a named
/// import or its spelling proves it, `Some(true)` when only a wildcard import
/// does (a same-package type of that name would shadow it).
fn test_annotation(
    cx: &FileCx<'_>,
    imports: &Imports,
    name: &str,
    node: TsNode<'_>,
) -> Option<bool> {
    let spelled = node.child_by_field_name("name").map_or("", |n| cx.text(&n));
    let mut wildcard = None;
    for (simple, package) in JUNIT {
        if *simple != name {
            continue;
        }
        let fqn = format!("{package}.{simple}");
        if imports.bindings.get(name) == Some(&fqn) || spelled == fqn {
            return Some(false);
        }
        if !imports.bindings.contains_key(name)
            && imports.wildcard_packages.iter().any(|p| p == package)
        {
            wildcard = Some(true);
        }
    }
    wildcard
}

const BUILDER_VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

/// A `MockMvcRequestBuilders` call: its method and URL argument.
fn builder<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    statics: &[(String, String)],
) -> Option<(Option<String>, Option<TsNode<'t>>)> {
    let name = cx.text(&call.child_by_field_name("name")?);
    let proven = match call.child_by_field_name("object") {
        Some(object) => {
            let object = cx.text(&object);
            object == "MockMvcRequestBuilders" || object == BUILDERS
        }
        None => statics
            .iter()
            .any(|(class, member)| class == BUILDERS && (member == "*" || member == name)),
    };
    if !proven {
        return None;
    }
    let list = call.child_by_field_name("arguments")?;
    let mut walk = list.walk();
    let args: Vec<TsNode<'t>> = list
        .named_children(&mut walk)
        .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        .collect();
    if BUILDER_VERBS.contains(&name) {
        return Some((Some(name.to_ascii_uppercase()), args.first().copied()));
    }
    if name == "request" {
        let method = args.first().and_then(|m| {
            let text = cx.text(m);
            text.rsplit_once('.')
                .filter(|(class, _)| class.ends_with("HttpMethod"))
                .map(|(_, verb)| verb.to_string())
        });
        return Some((method, args.get(1).copied()));
    }
    None
}

/// HTTP statuses `status().isXxx()` names.
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

/// The statuses a test expects of its responses.
fn statuses(cx: &FileCx<'_>, body: TsNode<'_>) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "method_invocation" {
            continue;
        }
        let Some(object) = node.child_by_field_name("object") else {
            continue;
        };
        let on_status = object.kind() == "method_invocation"
            && object
                .child_by_field_name("name")
                .is_some_and(|n| cx.text(&n) == "status");
        if !on_status {
            continue;
        }
        let name = node.child_by_field_name("name").map_or("", |n| cx.text(&n));
        if let Some((_, code)) = STATUSES.iter().find(|(n, _)| *n == name) {
            out.insert(*code);
        } else if name == "is"
            && let Some(code) = node
                .child_by_field_name("arguments")
                .and_then(|a| a.named_child(0))
                .filter(|a| a.kind() == "decimal_integer_literal")
                .and_then(|a| cx.text(&a).parse::<u16>().ok())
        {
            out.insert(code);
        }
    }
    out
}

/// The test cases a file defines and the requests they perform.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
    methods: &[(TsNode<'_>, String, Vec<String>, String)],
    out: &mut Extraction,
) {
    let statics = static_imports(cx, root);
    // Each test with the same-package names that would shadow it, when
    // only wildcard imports prove its annotations.
    let tests: Vec<_> = methods
        .iter()
        .filter_map(|entry| {
            let proofs: Vec<(String, bool)> = declaration_annotations(cx, entry.0)
                .iter()
                .filter_map(|(n, a)| Some((n.clone(), test_annotation(cx, imports, n, *a)?)))
                .collect();
            if proofs.is_empty() {
                return None;
            }
            let shadowed_by: Vec<String> = if proofs.iter().any(|(_, wildcard)| !wildcard) {
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
            Some((entry, shadowed_by))
        })
        .collect();
    let mut overloads: HashMap<String, usize> = HashMap::new();
    for ((_, _, chain, name), _) in &tests {
        *overloads
            .entry(format!("{}.{name}", chain.join(".")))
            .or_default() += 1;
    }
    for ((method, symbol, chain, name), shadowed_by) in tests {
        let Some(body) = method.child_by_field_name("body") else {
            continue;
        };
        let qualified = format!("{}.{name}", chain.join("."));
        // Overloaded tests (`check()`, `check(int)`) are told apart by the
        // signature their method Symbol carries.
        let key = if overloads[&qualified] > 1 {
            symbol.rsplit('#').next().unwrap_or(&qualified).to_string()
        } else {
            qualified.clone()
        };
        let test = format!("test:{}@{}#{key}", cx.id.repo, cx.path);
        let mut props = serde_json::json!({
            "name": qualified,
            "framework": "junit",
            "language": "java",
            "path": cx.path,
            "prov": cx.prov(method, &format!("TestCase {test}")),
        });
        if !shadowed_by.is_empty() {
            props["shadowed_by"] = shadowed_by.into();
        }
        out.nodes.push(Node {
            id: test.clone(),
            label: "TestCase".into(),
            props,
        });
        out.edges.push(Edge {
            src: test.clone(),
            dst: file_id(cx.id.repo, cx.path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(method, &format!("DEFINED_IN {test}")),
            }),
        });
        out.edges.push(Edge {
            src: test.clone(),
            dst: symbol.clone(),
            label: "IMPLEMENTED_BY".into(),
            props: serde_json::json!({
                "prov": cx.prov(method, &format!("IMPLEMENTED_BY {test} -> {symbol}")),
            }),
        });
        let expected = statuses(cx, body);
        let mut stack = vec![body];
        let mut found = Vec::new();
        while let Some(node) = stack.pop() {
            let mut walk = node.walk();
            stack.extend(node.named_children(&mut walk));
            if node.kind() == "method_invocation"
                && let Some((method, url)) = builder(cx, node, &statics)
            {
                found.push((node, method, url));
            }
        }
        found.sort_by_key(|(node, _, _)| node.start_byte());
        for (node, method, url) in found {
            let url = url.and_then(|url| {
                let mut out = Vec::new();
                parts(cx, imports, package, url, &mut out).map(|()| out)
            });
            out.test_requests.push(Request {
                test: test.clone(),
                method,
                url,
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
/// `{var}`.
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
            // A literal route segment takes a literal: a test's `{id}`
            // stands for a value, not for `count`.
            return None;
        }
    }
    Some(fixed)
}

/// Join each held request to the endpoint it reaches, or to a Gap.
pub(super) fn resolve(
    requests: Vec<Request>,
    (nodes, edges): (&mut Vec<Node>, &mut Vec<Edge>),
    repo: &str,
    text: &dyn Fn(&[Part]) -> Option<String>,
    declared: &dyn Fn(&str) -> bool,
) {
    // A test proven only by wildcard imports whose names the repository
    // declares in the test's package is annotated with those types instead.
    let mut shadowed: BTreeSet<String> = BTreeSet::new();
    for node in nodes.iter_mut() {
        if node.label != "TestCase" {
            continue;
        }
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
        if !names.is_empty() && names.iter().all(|fqn| declared(fqn)) {
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
        .filter(|n| n.label == "Endpoint" && n.props["framework"] == "spring")
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
                .url
                .as_deref()
                .and_then(text)
                .ok_or("test request URL is computed")?;
            let path = path(&url).ok_or("test request URL is not a path")?;
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
