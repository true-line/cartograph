//! Endpoint auth for axum and actix-web routes (#2163, AC-1305):
//! `props.auth` on every Rust `Endpoint`, in one of two states.
//!
//! - `required`: the route has a guard. A guard is
//!   - a handler parameter whose type is an auth extractor: axum-extra
//!     `TypedHeader<Authorization<Bearer>>` / `<Basic>` (`TypedHeader` from
//!     axum-extra or axum, `Authorization` and the scheme from headers,
//!     axum-extra or axum), actix-web-httpauth `BearerAuth` / `BasicAuth`,
//!     each bound by `use` or written from its crate (Confirmed), or a repository
//!     type with an `impl FromRequestParts` / `impl FromRequest` whose body
//!     names a 401 or 403 (`StatusCode::UNAUTHORIZED`, `ErrorForbidden`,
//!     `HttpResponse::Unauthorized`, an `…::Unauthorized` variant;
//!     InferredStrong). A parameter wrapped in `Option` is not a guard, and
//!     a status that is only compared (`==`, `!=`) or matched against
//!     (`match` arm, `if let` pattern) is not a denial.
//!   - an axum `layer` / `route_layer` on the router after the route was
//!     added: tower-http `ValidateRequestHeaderLayer::bearer` / `::basic` or
//!     `RequireAuthorizationLayer::bearer` / `::basic` from tower-http
//!     (Confirmed), or axum's
//!     `middleware::from_fn(f)` / `from_fn_with_state(s, f)` whose `f` is a
//!     repository function naming a 401 or 403 (InferredStrong). The layers of
//!     a `ServiceBuilder` are read one by one.
//! - `unknown`: anything else, with sorted reasons. A Rust route has no
//!   declarative public marker, so an endpoint is never `none`.
//!
//! A layer expression this pass cannot read is a Gap the endpoint
//! `DEPENDS_ON`; known non-authenticating tower-http and axum layers are
//! listed in `non_auth_middleware`. actix `wrap(...)` middleware is not read.

use super::*;

/// One handler parameter's type.
#[derive(Debug, Clone)]
struct Param {
    /// The library guard the type is, if any.
    library: Option<&'static str>,
    /// The type's last path segment, generics stripped, when not optional.
    type_name: Option<String>,
    text: String,
    evidence: EvidenceRef,
}

/// The parameter types of one function, read for when it is a handler.
#[derive(Debug, Clone)]
pub(super) struct HandlerParams {
    symbol: String,
    crate_name: String,
    params: Vec<Param>,
}

/// A repository extractor (`impl FromRequestParts for T`) that names a 401
/// or 403.
#[derive(Debug, Clone)]
pub(super) struct Extractor {
    crate_name: String,
    type_name: String,
    status: u16,
    evidence: EvidenceRef,
}

/// A function whose body names a 401 or 403.
#[derive(Debug, Clone)]
pub(super) struct DenyFn {
    symbol: String,
    status: u16,
    evidence: EvidenceRef,
}

/// What one axum layer is.
#[derive(Debug, Clone)]
pub(super) enum LayerKind {
    Guard(&'static str),
    NonAuth,
    /// `middleware::from_fn(f)`: the function, resolved at finalize.
    FromFn(http::HandlerTargetRef),
    Unreadable,
}

/// One axum `layer` / `route_layer` argument.
#[derive(Debug, Clone)]
pub(super) struct Layer {
    pub(super) name: String,
    pub(super) kind: LayerKind,
    pub(super) evidence: EvidenceRef,
}

/// A route as emitted: its endpoint, handler and the layers over it.
#[derive(Debug, Clone)]
pub(super) struct Route {
    pub(super) endpoint: String,
    pub(super) handler: Option<String>,
    pub(super) layers: Vec<Layer>,
}

pub(super) fn retarget(out: &mut Extraction, commit: &str) {
    for handler in &mut out.handler_params {
        for param in &mut handler.params {
            param.evidence.commit_sha = commit.into();
        }
    }
    for extractor in &mut out.extractors {
        extractor.evidence.commit_sha = commit.into();
    }
    for deny in &mut out.deny_fns {
        deny.evidence.commit_sha = commit.into();
    }
}

/// The 401 or 403 a path or identifier names.
fn deny_marker(text: &str) -> Option<u16> {
    let last = text.rsplit("::").next().unwrap_or(text).trim();
    match last {
        "UNAUTHORIZED" | "Unauthorized" | "ErrorUnauthorized" => Some(401),
        "FORBIDDEN" | "Forbidden" | "ErrorForbidden" => Some(403),
        _ => None,
    }
}

/// The first 401 or 403 named under `node`, in source order.
fn first_denial<'t>(cx: &FileCx<'_>, node: TsNode<'t>) -> Option<(u16, TsNode<'t>)> {
    let mut stack = vec![node];
    let mut found: Option<(u16, TsNode<'t>)> = None;
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        // A path (`StatusCode::UNAUTHORIZED`, `Error::Forbidden`), or actix's
        // `ErrorUnauthorized` / `ErrorForbidden` by name.
        let readable = match node.kind() {
            "scoped_identifier" => true,
            "identifier" => {
                matches!(cx.text(&node), "ErrorUnauthorized" | "ErrorForbidden")
                    && node
                        .parent()
                        .is_none_or(|p| p.kind() != "scoped_identifier")
            }
            _ => false,
        };
        if !readable || only_inspected(node) {
            continue;
        }
        if let Some(status) = deny_marker(cx.text(&node))
            && found.is_none_or(|(_, at)| node.start_byte() < at.start_byte())
        {
            found = Some((status, node));
        }
    }
    found
}

/// Whether a status path is only inspected, not produced: compared
/// (`status == StatusCode::UNAUTHORIZED`) or matched against
/// (`StatusCode::FORBIDDEN => ...`, `if let ... = ...`).
fn only_inspected(node: TsNode<'_>) -> bool {
    let mut at = node.parent();
    while let Some(parent) = at {
        match parent.kind() {
            "match_pattern" => return true,
            "let_condition" | "let_declaration" => {
                if parent
                    .child_by_field_name("pattern")
                    .is_some_and(|pattern| {
                        pattern.start_byte() <= node.start_byte()
                            && node.end_byte() <= pattern.end_byte()
                    })
                {
                    return true;
                }
            }
            "binary_expression" => {
                let operator = parent.child_by_field_name("operator").map(|o| o.kind());
                if matches!(operator, Some("==" | "!=")) {
                    return true;
                }
            }
            "block" | "function_item" | "closure_expression" => return false,
            _ => {}
        }
        at = parent.parent();
    }
    false
}

/// Crates `TypedHeader` is taken from.
const TYPED_HEADER_CRATES: &[&str] = &["axum_extra", "axum"];
/// Crates `Authorization`, `Bearer` and `Basic` are taken from.
const HEADER_CRATES: &[&str] = &["headers", "axum_extra", "axum"];

/// Whether the path written as `segments` at `node` names an item of one of
/// `crates`: written from the crate (`tower_http::...`), or its first
/// segment bound by a `use` of that crate. A local type of the same name is
/// neither.
fn from_crate(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    bindings: &Bindings,
    segments: &[&str],
    crates: &[&str],
) -> bool {
    let Some(first) = segments.first().map(|s| s.trim_start_matches("::")) else {
        return false;
    };
    if segments.len() > 1 && crates.contains(&first) {
        return true;
    }
    bindings
        .get(&visible_scopes(cx, node), first)
        .is_some_and(|(path, external)| {
            *external && path.first().is_some_and(|c| crates.contains(&c.as_str()))
        })
}

/// Whether a `TypedHeader<Authorization<scheme>>` parameter type names the
/// library types: each of `TypedHeader`, `Authorization` and `scheme` is
/// taken from its crate.
fn library_typed_header(
    cx: &FileCx<'_>,
    ty: TsNode<'_>,
    bindings: &Bindings,
    compact: &str,
    scheme: &str,
) -> bool {
    let outer = compact.split('<').next().unwrap_or("");
    let outer: Vec<&str> = outer.split("::").filter(|s| !s.is_empty()).collect();
    from_crate(cx, ty, bindings, &outer, TYPED_HEADER_CRATES)
        && from_crate(cx, ty, bindings, &["Authorization"], HEADER_CRATES)
        && from_crate(cx, ty, bindings, &[scheme], HEADER_CRATES)
}

/// The base type of a parameter type: `&`, `mut` and a `Path<...>` wrapper's
/// generics stripped, last segment only.
fn base_name(text: &str) -> String {
    let text = text.trim().trim_start_matches('&').trim();
    let text = text.strip_prefix("mut ").unwrap_or(text).trim();
    let base = text.split('<').next().unwrap_or(text).trim();
    base.rsplit("::").next().unwrap_or(base).trim().to_string()
}

/// The parameter types of `function`.
pub(super) fn handler_params(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    symbol: &str,
    bindings: &Bindings,
) -> Option<HandlerParams> {
    let parameters = function.child_by_field_name("parameters")?;
    let scopes = visible_scopes(cx, function);
    let mut params = Vec::new();
    let mut walk = parameters.walk();
    for parameter in parameters.named_children(&mut walk) {
        if parameter.kind() != "parameter" {
            continue;
        }
        let Some(ty) = parameter.child_by_field_name("type") else {
            continue;
        };
        let text: String = cx
            .text(&ty)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let compact: String = text.split_whitespace().collect();
        let name = base_name(&text);
        let typed = |scheme: &str| {
            name == "TypedHeader"
                && compact.contains(&format!("Authorization<{scheme}>"))
                && library_typed_header(cx, ty, bindings, &compact, scheme)
        };
        let library = if typed("Bearer") {
            Some("TypedHeader<Authorization<Bearer>>")
        } else if typed("Basic") {
            Some("TypedHeader<Authorization<Basic>>")
        } else if matches!(name.as_str(), "BearerAuth" | "BasicAuth")
            && bindings
                .get(&scopes, &name)
                .is_some_and(|(path, _)| path.first().is_some_and(|c| c == "actix_web_httpauth"))
        {
            Some(if name == "BearerAuth" {
                "actix_web_httpauth.BearerAuth"
            } else {
                "actix_web_httpauth.BasicAuth"
            })
        } else {
            None
        };
        let optional = name == "Option";
        params.push(Param {
            library,
            type_name: (!optional).then_some(name),
            text,
            evidence: cx.evidence(&parameter),
        });
        // axum handlers take at most 16 extractors.
        if params.len() >= 16 {
            break;
        }
    }
    (!params.is_empty()).then(|| HandlerParams {
        symbol: symbol.to_string(),
        crate_name: cx.crate_name.clone(),
        params,
    })
}

/// Repository extractors that name a 401 or 403, and functions that do.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function_items: &[(TsNode<'_>, String)],
    out: &mut Extraction,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "impl_item" {
            continue;
        }
        let (Some(tr), Some(ty), Some(body)) = (
            node.child_by_field_name("trait"),
            node.child_by_field_name("type"),
            node.child_by_field_name("body"),
        ) else {
            continue;
        };
        if !matches!(
            base_name(cx.text(&tr)).as_str(),
            "FromRequestParts" | "FromRequest"
        ) {
            continue;
        }
        if let Some((status, at)) = first_denial(cx, body) {
            out.extractors.push(Extractor {
                crate_name: cx.crate_name.clone(),
                type_name: base_name(cx.text(&ty)),
                status,
                evidence: cx.evidence(&at),
            });
        }
    }
    for (function, symbol) in function_items {
        if let Some(body) = function.child_by_field_name("body")
            && let Some((status, at)) = first_denial(cx, body)
        {
            out.deny_fns.push(DenyFn {
                symbol: symbol.clone(),
                status,
                evidence: cx.evidence(&at),
            });
        }
    }
}

const NON_AUTH_LAYERS: &[&str] = &[
    "AddExtensionLayer",
    "BufferLayer",
    "CatchPanicLayer",
    "CompressionLayer",
    "ConcurrencyLimitLayer",
    "CorsLayer",
    "DecompressionLayer",
    "DefaultBodyLimit",
    "Extension",
    "HandleErrorLayer",
    "LoadShedLayer",
    "MapRequestBodyLayer",
    "MapResponseBodyLayer",
    "NormalizePathLayer",
    "PropagateHeaderLayer",
    "PropagateRequestIdLayer",
    "RateLimitLayer",
    "RequestBodyLimitLayer",
    "RequestDecompressionLayer",
    "SetRequestHeaderLayer",
    "SetRequestIdLayer",
    "SetResponseHeaderLayer",
    "SetSensitiveHeadersLayer",
    "SetSensitiveRequestHeadersLayer",
    "SetSensitiveResponseHeadersLayer",
    "TimeoutLayer",
    "TraceLayer",
];

/// `node`'s source with whitespace and comments left out.
fn compact(cx: &FileCx<'_>, node: TsNode<'_>) -> String {
    let mut out = String::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "line_comment" | "block_comment") {
            continue;
        }
        if node.child_count() == 0 {
            out.extend(cx.text(&node).chars().filter(|c| !c.is_whitespace()));
            continue;
        }
        let mut walk = node.walk();
        let children: Vec<_> = node.children(&mut walk).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// What an axum `layer(...)` argument is: one layer, or each layer of a
/// `ServiceBuilder` chain.
pub(super) fn layers(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    resolve_fn: &dyn Fn(TsNode<'_>) -> http::HandlerTargetRef,
) -> Vec<Layer> {
    let text = compact(cx, expr);
    // A tuple of layers applies each of them.
    if expr.kind() == "tuple_expression" {
        let mut walk = expr.walk();
        return expr
            .named_children(&mut walk)
            .filter(|c| !matches!(c.kind(), "line_comment" | "block_comment"))
            .flat_map(|c| layers(cx, c, bindings, resolve_fn))
            .collect();
    }
    // `ServiceBuilder::new().layer(a).layer(b)`.
    if text.starts_with("ServiceBuilder::new()") || text.contains("::ServiceBuilder::new()") {
        let mut found = Vec::new();
        let mut node = expr;
        while node.kind() == "call_expression" {
            let Some(function) = node.child_by_field_name("function") else {
                break;
            };
            if function.kind() != "field_expression" {
                break;
            }
            if function
                .child_by_field_name("field")
                .is_some_and(|f| cx.text(&f) == "layer")
                && let Some(argument) = http::call_arguments(node).first()
            {
                found.extend(layers(cx, *argument, bindings, resolve_fn));
            }
            let Some(value) = function.child_by_field_name("value") else {
                break;
            };
            node = value;
        }
        found.reverse();
        return found;
    }
    let name: String = text.chars().take(120).collect();
    let evidence = cx.evidence(&expr);
    let callee = (expr.kind() == "call_expression")
        .then(|| expr.child_by_field_name("function"))
        .flatten();
    let callee_text = callee
        .map(|c| strip_generic_args(cx.text(&c)))
        .unwrap_or_default();
    let segments: Vec<&str> = callee_text.split("::").map(str::trim).collect();
    // The path to the type a `Type::bearer` call names, and to the module (or
    // the function) a `from_fn` call names, must reach the library crate.
    let owner = &segments[..segments.len().saturating_sub(1)];
    let tower = from_crate(cx, expr, bindings, owner, &["tower_http"]);
    let axum = if owner.is_empty() {
        from_crate(cx, expr, bindings, &segments, &["axum"])
    } else {
        from_crate(cx, expr, bindings, owner, &["axum"])
    };
    let kind = match segments.as_slice() {
        [.., ty, "bearer"]
            if tower
                && matches!(
                    *ty,
                    "ValidateRequestHeaderLayer" | "RequireAuthorizationLayer"
                ) =>
        {
            LayerKind::Guard(if *ty == "ValidateRequestHeaderLayer" {
                "tower_http.ValidateRequestHeaderLayer::bearer"
            } else {
                "tower_http.RequireAuthorizationLayer::bearer"
            })
        }
        [.., ty, "basic"]
            if tower
                && matches!(
                    *ty,
                    "ValidateRequestHeaderLayer" | "RequireAuthorizationLayer"
                ) =>
        {
            LayerKind::Guard(if *ty == "ValidateRequestHeaderLayer" {
                "tower_http.ValidateRequestHeaderLayer::basic"
            } else {
                "tower_http.RequireAuthorizationLayer::basic"
            })
        }
        [.., "from_fn" | "from_fn_with_state"] if axum => match http::call_arguments(expr).last() {
            Some(f) => LayerKind::FromFn(resolve_fn(*f)),
            None => LayerKind::Unreadable,
        },
        [.., "ValidateRequestHeaderLayer", "accept"] => LayerKind::NonAuth,
        _ => {
            // By its type, however the path to it is written
            // (`tower_http::trace::TraceLayer::new_for_http()`).
            let base = text.split(['(', '<', '{']).next().unwrap_or("");
            if base
                .split("::")
                .chain(segments.iter().copied())
                .any(|segment| NON_AUTH_LAYERS.contains(&segment))
            {
                LayerKind::NonAuth
            } else {
                LayerKind::Unreadable
            }
        }
    };
    vec![Layer {
        name,
        kind,
        evidence,
    }]
}

fn strip_generic_args(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

fn prov(evidence: Vec<EvidenceRef>, confidence: ConfidenceTier, fact: &str) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic confidence is within its ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// Set `auth` on every Rust endpoint, and add a Gap for each layer that
/// cannot be read.
pub(super) fn resolve(out: &mut Extraction, routes: Vec<Route>, index: &SymbolIndex) {
    let params: BTreeMap<&str, &HandlerParams> = out
        .handler_params
        .iter()
        .map(|h| (h.symbol.as_str(), h))
        .collect();
    let mut extractors: BTreeMap<&str, Vec<&Extractor>> = BTreeMap::new();
    for extractor in &out.extractors {
        extractors
            .entry(extractor.type_name.as_str())
            .or_default()
            .push(extractor);
    }
    let deny_fns: BTreeMap<&str, &DenyFn> = out
        .deny_fns
        .iter()
        .map(|d| (d.symbol.as_str(), d))
        .collect();
    let mut by_endpoint: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    for route in routes {
        by_endpoint
            .entry(route.endpoint.clone())
            .or_default()
            .push(route);
    }
    let mut auth: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut gaps: BTreeMap<String, Node> = BTreeMap::new();
    let mut depends: BTreeSet<(String, String)> = BTreeSet::new();
    for (endpoint, registrations) in &by_endpoint {
        let mut guards = BTreeSet::new();
        let mut basis = BTreeSet::new();
        let mut sites: Vec<EvidenceRef> = Vec::new();
        let mut reasons = BTreeSet::new();
        let mut non_auth = BTreeSet::new();
        let mut library_only = true;
        let mut every_guarded = true;
        for route in registrations {
            let mut guarded = false;
            if let Some(handler) = route.handler.as_deref().and_then(|h| params.get(h)) {
                for param in &handler.params {
                    if let Some(guard) = param.library {
                        guarded = true;
                        guards.insert(guard.to_string());
                        basis.insert(format!("library extractor {guard}"));
                        sites.push(param.evidence.clone());
                        continue;
                    }
                    let Some(name) = &param.type_name else {
                        continue;
                    };
                    let Some(found) = extractors.get(name.as_str()) else {
                        continue;
                    };
                    // The extractor of the handler's own crate, else the only one.
                    let same: Vec<&&Extractor> = found
                        .iter()
                        .filter(|e| e.crate_name == handler.crate_name)
                        .collect();
                    let chosen = match (same.as_slice(), found.as_slice()) {
                        ([one], _) => Some(**one),
                        ([], [one]) => Some(*one),
                        _ => None,
                    };
                    match chosen {
                        Some(extractor) => {
                            guarded = true;
                            library_only = false;
                            guards.insert(name.clone());
                            basis.insert(format!(
                                "extractor {name} rejects with {}",
                                extractor.status
                            ));
                            sites.push(param.evidence.clone());
                            sites.push(extractor.evidence.clone());
                        }
                        None => {
                            reasons.insert(format!("ambiguous_extractor:{}", param.text));
                        }
                    }
                }
            }
            for layer in &route.layers {
                let mut unreadable = |reason: &str| {
                    reasons.insert(format!("{reason}:{}", layer.name));
                    let id = format!(
                        "gap:rust-middleware:{}@{}@{}",
                        layer.evidence.repo, layer.evidence.path, layer.evidence.byte_start
                    );
                    gaps.entry(id.clone()).or_insert_with(|| Node {
                        id: id.clone(),
                        label: "Gap".into(),
                        props: serde_json::json!({
                            "middleware": layer.name,
                            "reason": "unreadable Rust middleware",
                            "detail": reason,
                            "attempted_tiers": ["T0"],
                            "prov": prov(vec![layer.evidence.clone()], ConfidenceTier::Gap, &id),
                        }),
                    });
                    depends.insert((endpoint.clone(), id));
                };
                match &layer.kind {
                    LayerKind::Guard(guard) => {
                        guarded = true;
                        guards.insert(guard.to_string());
                        basis.insert(format!("library layer {guard}"));
                        sites.push(layer.evidence.clone());
                    }
                    LayerKind::NonAuth => {
                        non_auth.insert(layer.name.clone());
                    }
                    LayerKind::FromFn(target) => match http::resolve_target_ref(target, index) {
                        Some(symbol) => match deny_fns.get(symbol.as_str()) {
                            Some(deny) => {
                                guarded = true;
                                library_only = false;
                                basis.insert(format!(
                                    "middleware {} responds {}",
                                    symbol.rsplit('#').next().unwrap_or(&symbol),
                                    deny.status
                                ));
                                guards.insert(symbol);
                                sites.push(layer.evidence.clone());
                                sites.push(deny.evidence.clone());
                            }
                            None => {
                                reasons.insert(format!("no_deny_status:{}", layer.name));
                            }
                        },
                        None => unreadable("unresolved_middleware"),
                    },
                    LayerKind::Unreadable => unreadable("unreadable_middleware"),
                }
            }
            every_guarded &= guarded;
        }
        let mut value = if every_guarded && !guards.is_empty() {
            sites.sort_by(|a, b| {
                (&a.path, a.byte_start, a.byte_end).cmp(&(&b.path, b.byte_start, b.byte_end))
            });
            sites.dedup_by(|a, b| {
                (&a.path, a.byte_start, a.byte_end) == (&b.path, b.byte_start, b.byte_end)
            });
            let confidence = if library_only {
                ConfidenceTier::Confirmed
            } else {
                ConfidenceTier::InferredStrong
            };
            serde_json::json!({
                "state": "required",
                "authenticated": true,
                "guards": guards.into_iter().collect::<Vec<_>>(),
                "basis": basis.into_iter().collect::<Vec<_>>(),
                "prov": prov(sites, confidence, &format!("auth {endpoint}")),
            })
        } else {
            reasons.insert("no_guard_recovered".into());
            serde_json::json!({
                "state": "unknown",
                "reasons": reasons.into_iter().collect::<Vec<_>>(),
            })
        };
        if !non_auth.is_empty() {
            value["non_auth_middleware"] = non_auth.into_iter().collect::<Vec<_>>().into();
        }
        auth.insert(endpoint.clone(), value);
    }
    for node in &mut out.nodes {
        if node.label == "Endpoint"
            && let Some(value) = auth.get(&node.id)
        {
            node.props["auth"] = value.clone();
        }
    }
    for (src, dst) in depends {
        let site = gaps[&dst].props["prov"].clone();
        out.edges.push(Edge {
            src,
            dst,
            label: "DEPENDS_ON".into(),
            props: serde_json::json!({
                "reason": "unreadable Rust middleware",
                "prov": site,
            }),
        });
    }
    out.nodes.extend(gaps.into_values());
    out.handler_params.clear();
    out.extractors.clear();
    out.deny_fns.clear();
}
