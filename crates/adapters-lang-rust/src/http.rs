//! axum and actix-web HTTP endpoint recovery (#999).
//!
//! Routes are read per file into pending router facts and resolved in the
//! directory-wide finalize pass, so nested and merged routers and handlers in
//! other files resolve through the same symbol index as calls.

use super::*;

#[derive(Debug, Clone)]
enum RoutePathArg {
    Literal(String),
    Computed(String),
}

#[derive(Debug, Clone)]
pub(super) enum HandlerTarget {
    Resolved(String),
    Pending(Target),
    Unresolved,
}

#[derive(Debug, Clone)]
struct HandlerRef {
    target: HandlerTarget,
    expression: String,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
enum AxumRouterItem {
    Route {
        path: RoutePathArg,
        method: String,
        handler: HandlerRef,
        evidence: EvidenceRef,
    },
    Nest {
        prefix: RoutePathArg,
        sub: AxumSubRouter,
        evidence: EvidenceRef,
    },
    Merge {
        sub: AxumSubRouter,
    },
    /// `layer` / `route_layer`: wraps every route added before it (#2163).
    Layer(Vec<auth::Layer>),
}

/// A route handler's target, as the auth pass reads a `from_fn` argument.
pub(super) type HandlerTargetRef = HandlerTarget;

/// [`resolve_handler_target`] for the auth pass.
pub(super) fn resolve_target_ref(target: &HandlerTarget, index: &SymbolIndex) -> Option<String> {
    resolve_handler_target(target, index)
}

#[derive(Debug, Clone)]
enum AxumSubRouter {
    Inline(Vec<AxumRouterItem>),
    /// A router-returning function, keyed by its resolved symbol (unique
    /// across workspace crates), wherever it is defined.
    Call {
        target: HandlerTarget,
    },
    None,
}

#[derive(Debug, Clone)]
pub(super) struct AxumRouterDef {
    owner_fn: Option<String>,
    items: Vec<AxumRouterItem>,
}

#[derive(Debug, Clone)]
pub(super) struct ActixAttrRoute {
    sym_id: String,
    methods: Vec<String>,
    path: RoutePathArg,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
pub(super) struct ActixServiceMount {
    /// The scope prefix the service is mounted under; computed when any
    /// enclosing scope's prefix is not a literal.
    prefix: RoutePathArg,
    target: HandlerTarget,
}

#[derive(Debug, Clone)]
pub(super) struct PendingDirectEndpoint {
    framework: &'static str,
    method: String,
    path: RoutePathArg,
    handler: Option<HandlerRef>,
    evidence: EvidenceRef,
}

pub(super) fn retarget_commit(extraction: &mut Extraction, commit: &str) {
    for router in &mut extraction.axum_routers {
        retarget_axum_items(&mut router.items, commit);
    }
    for attr in &mut extraction.actix_attr_routes {
        attr.evidence.commit_sha = commit.into();
    }
    for ep in &mut extraction.pending_endpoints {
        ep.evidence.commit_sha = commit.into();
        if let Some(handler) = &mut ep.handler {
            handler.evidence.commit_sha = commit.into();
        }
    }
}

fn retarget_axum_items(items: &mut [AxumRouterItem], commit: &str) {
    for item in items {
        match item {
            AxumRouterItem::Route {
                handler, evidence, ..
            } => {
                handler.evidence.commit_sha = commit.into();
                evidence.commit_sha = commit.into();
            }
            AxumRouterItem::Nest { sub, evidence, .. } => {
                evidence.commit_sha = commit.into();
                if let AxumSubRouter::Inline(inner) = sub {
                    retarget_axum_items(inner, commit);
                }
            }
            AxumRouterItem::Merge { sub } => {
                if let AxumSubRouter::Inline(inner) = sub {
                    retarget_axum_items(inner, commit);
                }
            }
            AxumRouterItem::Layer(layers) => {
                for layer in layers {
                    layer.evidence.commit_sha = commit.into();
                }
            }
        }
    }
}

fn strip_generics(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Resolve a written path against `use` bindings and external globs in `scopes`,
/// returning all candidate external paths (`axum::...`, `actix_web::...`,
/// `reqwest::...`, `http::...`, the SQL crates) it proves.
pub(super) fn resolve_external_paths(
    cx: &FileCx<'_>,
    bindings: &Bindings,
    scopes: &[String],
    raw_text: &str,
    value_shadows: Option<&BTreeSet<String>>,
) -> Vec<Vec<String>> {
    let parts = segs(&strip_generics(raw_text));
    if parts.is_empty() {
        return Vec::new();
    }
    if parts.len() == 1 && value_shadows.is_some_and(|s| s.contains(&parts[0])) {
        return Vec::new();
    }
    // Innermost scope first: a nearer item or `use` decides; globs only
    // speak for a name nothing nearer declares, and a workspace glob that
    // could also bring it in leaves the origin unproven.
    let name = &parts[0];
    let mut workspace_glob = false;
    let mut external_globs = Vec::new();
    for scope in scopes {
        let key = (scope.clone(), name.clone());
        if bindings.items.contains(&key) {
            return Vec::new();
        }
        if let Some((base, external)) = bindings.map.get(&key) {
            if !external || workspace_glob || !external_globs.is_empty() {
                return Vec::new();
            }
            let mut full = base.clone();
            full.extend(parts[1..].iter().cloned());
            return vec![full];
        }
        workspace_glob |= bindings.globs.contains_key(scope);
        external_globs.extend(
            bindings
                .external_globs
                .get(scope)
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    if matches!(
        name.as_str(),
        "axum"
            | "actix_web"
            | "reqwest"
            | "rdkafka"
            | "http"
            | "std"
            | "alloc"
            | "hyper"
            | "rusqlite"
            | "sqlx"
            | "postgres"
            | "tokio_postgres"
            | "diesel"
    ) && !cx.workspace_crates.contains(name)
        && !cx.declared_mods.contains(name)
    {
        return vec![parts];
    }
    if workspace_glob {
        return Vec::new();
    }
    external_globs
        .into_iter()
        .map(|mut full| {
            full.extend(parts.iter().cloned());
            full
        })
        .collect()
}

pub(super) fn unquote_string_literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    let text = cx.text(&node).trim();
    match node.kind() {
        "string_literal" => text
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .and_then(decode_rust_escapes),
        "raw_string_literal" => text
            .strip_prefix('r')
            .map(|s| s.trim_matches('#'))
            .and_then(|s| s.strip_prefix('"'))
            .and_then(|s| s.strip_suffix('"'))
            .map(str::to_string),
        _ => None,
    }
}

/// The value of a (non-raw) Rust string body: escapes decoded, `\`-newline
/// continuations dropped. `None` for an escape Rust rejects, so a route is
/// never claimed under its source spelling.
fn decode_rust_escapes(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'x' => {
                let hex: String = [chars.next()?, chars.next()?].iter().collect();
                let value = u8::from_str_radix(&hex, 16).ok().filter(u8::is_ascii)?;
                out.push(char::from(value));
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    match chars.next()? {
                        '}' => break,
                        '_' => {}
                        digit => hex.push(digit),
                    }
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            '\n' | '\r' => while chars.next_if(|c| c.is_whitespace()).is_some() {},
            _ => return None,
        }
    }
    Some(out)
}

fn parse_route_path_arg(cx: &FileCx<'_>, node: TsNode<'_>) -> RoutePathArg {
    if let Some(lit) = unquote_string_literal(cx, node) {
        RoutePathArg::Literal(lit)
    } else {
        RoutePathArg::Computed(cx.text(&node).trim().to_string())
    }
}

pub(super) fn call_arguments<'t>(call: TsNode<'t>) -> Vec<TsNode<'t>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = args.walk();
    args.named_children(&mut walk)
        .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        .collect()
}

fn resolve_handler_expr(
    cx: &FileCx<'_>,
    mut expr: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
) -> HandlerRef {
    let full_expr = cx.text(&expr).trim().to_string();
    let evidence = cx.evidence(&expr);
    while expr.kind() == "parenthesized_expression" {
        if let Some(inner) = expr.named_child(0) {
            expr = inner;
        } else {
            break;
        }
    }
    if expr.kind() == "generic_function"
        && let Some(base) = expr.child_by_field_name("function")
    {
        expr = base;
    }
    let scope = scope_of(cx, expr);
    let scopes = visible_scopes(cx, expr);
    let text = strip_generics(cx.text(&expr).trim());
    let target = match expr.kind() {
        "identifier" => {
            let name = text.as_str();
            if value_shadows.contains(name) {
                HandlerTarget::Unresolved
            } else {
                match resolve_name(&scopes, name, locals, bindings) {
                    NameResolution::Local(dst) => HandlerTarget::Resolved(dst.clone()),
                    NameResolution::Import((resolved, false)) => {
                        HandlerTarget::Pending(Target::Path(resolved.clone()))
                    }
                    NameResolution::Globs(globs) => HandlerTarget::Pending(Target::Candidates {
                        paths: globs
                            .into_iter()
                            .map(|mut path| {
                                path.push(name.to_string());
                                path
                            })
                            .collect(),
                        gap_on_miss: true,
                    }),
                    NameResolution::Import((_, true))
                    | NameResolution::Ambiguous
                    | NameResolution::None => HandlerTarget::Unresolved,
                }
            }
        }
        "scoped_identifier" => {
            let parts = segs(&text);
            if parts.len() < 2 {
                HandlerTarget::Unresolved
            } else if parts[0] == "Self" {
                if let Some(ty) = impl_type(cx, expr) {
                    HandlerTarget::Pending(Target::Method {
                        crate_name: cx.crate_name.clone(),
                        type_name: ty,
                        method: parts[parts.len() - 1].clone(),
                    })
                } else {
                    HandlerTarget::Unresolved
                }
            } else if let Some((base, external)) = bindings.get(&scopes, &parts[0]) {
                if *external {
                    HandlerTarget::Unresolved
                } else {
                    let mut target = base.clone();
                    target.extend(parts[1..].iter().cloned());
                    HandlerTarget::Pending(Target::Path(target))
                }
            } else {
                let (resolved, external) = resolve_path(cx, &scope, &parts);
                if external {
                    let mut paths = vec![{
                        let mut p = scope.clone();
                        p.extend(parts.iter().cloned());
                        p
                    }];
                    for glob in bindings.globs(&scopes) {
                        let mut p = glob.clone();
                        p.extend(parts.iter().cloned());
                        paths.push(p);
                    }
                    HandlerTarget::Pending(Target::Candidates {
                        paths,
                        gap_on_miss: true,
                    })
                } else {
                    HandlerTarget::Pending(Target::Path(resolved))
                }
            }
        }
        _ => HandlerTarget::Unresolved,
    };
    HandlerRef {
        target,
        expression: full_expr,
        evidence,
    }
}

fn split_token_tree_args<'t>(tt: TsNode<'t>) -> Vec<Vec<TsNode<'t>>> {
    let mut groups: Vec<Vec<TsNode<'t>>> = Vec::new();
    let mut current: Vec<TsNode<'t>> = Vec::new();
    let count = tt.child_count();
    for i in 0..count {
        let Some(child) = tt.child(i) else {
            continue;
        };
        let kind = child.kind();
        if (i == 0 && matches!(kind, "(" | "[" | "{"))
            || (i + 1 == count && matches!(kind, ")" | "]" | "}"))
        {
            continue;
        }
        if matches!(kind, "line_comment" | "block_comment") {
            continue;
        }
        if kind == "," {
            groups.push(std::mem::take(&mut current));
        } else {
            current.push(child);
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

fn collect_outer_attributes<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut attrs = Vec::new();
    let mut cur = node.prev_sibling();
    while let Some(sib) = cur {
        match sib.kind() {
            "attribute_item" => attrs.push(sib),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        cur = sib.prev_sibling();
    }
    attrs.reverse();
    attrs
}

pub(super) fn extract_actix_fn_attrs(
    cx: &FileCx<'_>,
    fn_node: TsNode<'_>,
    fn_sym: &str,
    bindings: &Bindings,
    out: &mut Extraction,
) {
    let scopes = visible_scopes(cx, fn_node);
    for attr_item in collect_outer_attributes(fn_node) {
        let mut walk = attr_item.walk();
        let Some(attr) = attr_item
            .named_children(&mut walk)
            .find(|n| n.kind() == "attribute")
        else {
            continue;
        };
        let mut aw = attr.walk();
        let Some(path_node) = attr
            .named_children(&mut aw)
            .find(|n| matches!(n.kind(), "identifier" | "scoped_identifier"))
        else {
            continue;
        };
        let Some(args_tt) = attr.child_by_field_name("arguments") else {
            continue;
        };
        let resolved_paths =
            resolve_external_paths(cx, bindings, &scopes, cx.text(&path_node), None);
        let Some(attr_name) = resolved_paths.iter().find_map(|p| match p.as_slice() {
            [krate, name] if krate == "actix_web" => Some(name.as_str()),
            _ => None,
        }) else {
            continue;
        };
        let groups = split_token_tree_args(args_tt);
        let Some(first_group) = groups.first().filter(|g| !g.is_empty()) else {
            continue;
        };
        let path_arg = if first_group.len() == 1 {
            parse_route_path_arg(cx, first_group[0])
        } else {
            let start = first_group[0].start_byte();
            let end = first_group.last().expect("non-empty").end_byte();
            let raw = std::str::from_utf8(&cx.source[start..end])
                .unwrap_or("")
                .trim()
                .to_string();
            RoutePathArg::Computed(raw)
        };
        let methods: Vec<String> = match attr_name {
            "get" | "post" | "put" | "patch" | "delete" | "head" => {
                vec![attr_name.to_ascii_uppercase()]
            }
            "route" => {
                let mut ms = Vec::new();
                for group in &groups[1..] {
                    if group.len() == 3
                        && cx.text(&group[0]).trim() == "method"
                        && cx.text(&group[1]).trim() == "="
                        && let Some(m) = unquote_string_literal(cx, group[2])
                    {
                        ms.push(m.to_ascii_uppercase());
                    }
                }
                ms
            }
            _ => continue,
        };
        if methods.is_empty() && matches!(path_arg, RoutePathArg::Literal(_)) {
            continue;
        }
        out.actix_attr_routes.push(ActixAttrRoute {
            sym_id: fn_sym.to_string(),
            methods,
            path: path_arg,
            evidence: cx.evidence(&attr_item),
        });
    }
}

fn is_axum_router_ctor(
    cx: &FileCx<'_>,
    bindings: &Bindings,
    scopes: &[String],
    callee_text: &str,
    value_shadows: &BTreeSet<String>,
) -> bool {
    resolve_external_paths(cx, bindings, scopes, callee_text, Some(value_shadows))
        .iter()
        .any(|p| {
            matches!(
                p.as_slice(),
                [a, b, c] if a == "axum" && b == "Router" && matches!(c.as_str(), "new" | "default")
            ) || matches!(
                p.as_slice(),
                [a, b, c, d]
                    if a == "axum"
                        && b == "routing"
                        && c == "Router"
                        && matches!(d.as_str(), "new" | "default")
            )
        })
}

fn axum_method_router_verb(
    cx: &FileCx<'_>,
    bindings: &Bindings,
    scopes: &[String],
    callee_text: &str,
    value_shadows: &BTreeSet<String>,
) -> Option<String> {
    const VERBS: &[&str] = &[
        "get", "post", "put", "patch", "delete", "head", "options", "any",
    ];
    resolve_external_paths(cx, bindings, scopes, callee_text, Some(value_shadows))
        .into_iter()
        .find_map(|p| match p.as_slice() {
            [a, b, v] if a == "axum" && b == "routing" && VERBS.contains(&v.as_str()) => {
                Some(v.to_ascii_uppercase())
            }
            [a, b, c, v]
                if a == "axum"
                    && b == "routing"
                    && c == "method_routing"
                    && VERBS.contains(&v.as_str()) =>
            {
                Some(v.to_ascii_uppercase())
            }
            _ => None,
        })
}

fn parse_axum_method_router(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
) -> Option<Vec<(String, HandlerRef)>> {
    if expr.kind() != "call_expression" {
        return None;
    }
    let func = expr.child_by_field_name("function")?;
    let args = call_arguments(expr);
    if func.kind() == "field_expression" {
        let receiver = func.child_by_field_name("value")?;
        let field = func.child_by_field_name("field")?;
        let method_name = cx.text(&field).trim();
        let mut chain = parse_axum_method_router(cx, receiver, bindings, locals, value_shadows)?;
        const VERBS: &[&str] = &[
            "get", "post", "put", "patch", "delete", "head", "options", "any",
        ];
        if VERBS.contains(&method_name) {
            let handler_node = *args.first()?;
            let handler = resolve_handler_expr(cx, handler_node, bindings, locals, value_shadows);
            chain.push((method_name.to_ascii_uppercase(), handler));
            return Some(chain);
        } else if matches!(
            method_name,
            "layer" | "route_layer" | "with_state" | "handle_error"
        ) {
            return Some(chain);
        }
        return None;
    }
    let scopes = visible_scopes(cx, expr);
    let verb = axum_method_router_verb(cx, bindings, &scopes, cx.text(&func), value_shadows)?;
    let handler_node = *args.first()?;
    let handler = resolve_handler_expr(cx, handler_node, bindings, locals, value_shadows);
    Some(vec![(verb, handler)])
}

#[allow(clippy::too_many_arguments)]
fn parse_axum_sub_router(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
    local_routers: &BTreeMap<String, Vec<AxumRouterItem>>,
    local_call_vars: &BTreeMap<String, HandlerTarget>,
    consumed_locals: &mut BTreeSet<String>,
) -> AxumSubRouter {
    if let Some(items) = eval_axum_expr(
        cx,
        expr,
        bindings,
        locals,
        value_shadows,
        local_routers,
        local_call_vars,
        consumed_locals,
    ) {
        return AxumSubRouter::Inline(items);
    }
    if expr.kind() == "identifier" {
        let name = cx.text(&expr).trim();
        if let Some(target) = local_call_vars.get(name) {
            return AxumSubRouter::Call {
                target: target.clone(),
            };
        }
    }
    if expr.kind() == "call_expression"
        && let Some(callee) = expr.child_by_field_name("function")
    {
        let mut shadows = value_shadows.clone();
        shadows.remove(cx.text(&callee).trim());
        let href = resolve_handler_expr(cx, callee, bindings, locals, &shadows);
        if !matches!(href.target, HandlerTarget::Unresolved) {
            return AxumSubRouter::Call {
                target: href.target,
            };
        }
    }
    AxumSubRouter::None
}

#[allow(clippy::too_many_arguments)]
fn eval_axum_expr(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
    local_routers: &BTreeMap<String, Vec<AxumRouterItem>>,
    local_call_vars: &BTreeMap<String, HandlerTarget>,
    consumed_locals: &mut BTreeSet<String>,
) -> Option<Vec<AxumRouterItem>> {
    if expr.kind() == "identifier" {
        let name = cx.text(&expr).trim();
        if let Some(items) = local_routers.get(name) {
            consumed_locals.insert(name.to_string());
            return Some(items.clone());
        }
        return None;
    }
    if expr.kind() != "call_expression" {
        return None;
    }
    let func = expr.child_by_field_name("function")?;
    if func.kind() == "field_expression" {
        let receiver = func.child_by_field_name("value")?;
        let field = func.child_by_field_name("field")?;
        let method_name = cx.text(&field).trim();
        let mut items = eval_axum_expr(
            cx,
            receiver,
            bindings,
            locals,
            value_shadows,
            local_routers,
            local_call_vars,
            consumed_locals,
        )?;
        let args = call_arguments(expr);
        match method_name {
            "route" => {
                if args.len() >= 2 {
                    let path_arg = parse_route_path_arg(cx, args[0]);
                    if let Some(method_handlers) =
                        parse_axum_method_router(cx, args[1], bindings, locals, value_shadows)
                    {
                        for (method, handler) in method_handlers {
                            items.push(AxumRouterItem::Route {
                                path: path_arg.clone(),
                                method,
                                handler,
                                evidence: cx.evidence(&expr),
                            });
                        }
                    }
                }
                Some(items)
            }
            "nest" => {
                if args.len() >= 2 {
                    let prefix = parse_route_path_arg(cx, args[0]);
                    let sub = parse_axum_sub_router(
                        cx,
                        args[1],
                        bindings,
                        locals,
                        value_shadows,
                        local_routers,
                        local_call_vars,
                        consumed_locals,
                    );
                    items.push(AxumRouterItem::Nest {
                        prefix,
                        sub,
                        evidence: cx.evidence(&expr),
                    });
                }
                Some(items)
            }
            "merge" => {
                if let Some(&arg) = args.first() {
                    let sub = parse_axum_sub_router(
                        cx,
                        arg,
                        bindings,
                        locals,
                        value_shadows,
                        local_routers,
                        local_call_vars,
                        consumed_locals,
                    );
                    items.push(AxumRouterItem::Merge { sub });
                }
                Some(items)
            }
            "layer" | "route_layer" => {
                if let Some(&arg) = args.first() {
                    let resolve = |node: TsNode<'_>| {
                        resolve_handler_expr(cx, node, bindings, locals, value_shadows).target
                    };
                    items.push(AxumRouterItem::Layer(auth::layers(cx, arg, &resolve)));
                }
                Some(items)
            }
            "with_state"
            | "fallback"
            | "fallback_service"
            | "nest_service"
            | "route_service"
            | "into_make_service"
            | "into_make_service_with_connect_info"
            | "boxed" => Some(items),
            _ => None,
        }
    } else {
        let scopes = visible_scopes(cx, expr);
        is_axum_router_ctor(cx, bindings, &scopes, cx.text(&func), value_shadows).then(Vec::new)
    }
}

#[derive(Debug, Clone)]
struct ActixResourceRoute {
    method: String,
    handler: HandlerRef,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
enum ActixChain {
    Resource {
        path: RoutePathArg,
        routes: Vec<ActixResourceRoute>,
        evidence: EvidenceRef,
    },
    Scope {
        prefix: RoutePathArg,
        endpoints: Vec<PendingDirectEndpoint>,
        mounts: Vec<ActixServiceMount>,
        evidence: EvidenceRef,
    },
    App {
        endpoints: Vec<PendingDirectEndpoint>,
        mounts: Vec<ActixServiceMount>,
    },
}

fn parse_actix_route_receiver(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    value_shadows: &BTreeSet<String>,
) -> Option<String> {
    if expr.kind() != "call_expression" {
        return None;
    }
    let func = expr.child_by_field_name("function")?;
    if func.kind() == "field_expression" {
        let inner = func.child_by_field_name("value")?;
        return parse_actix_route_receiver(cx, inner, bindings, value_shadows);
    }
    let scopes = visible_scopes(cx, expr);
    const VERBS: &[&str] = &[
        "get", "post", "put", "patch", "delete", "head", "options", "trace", "connect",
    ];
    resolve_external_paths(cx, bindings, &scopes, cx.text(&func), Some(value_shadows))
        .into_iter()
        .find_map(|p| match p.as_slice() {
            [a, b, v] if a == "actix_web" && b == "web" && VERBS.contains(&v.as_str()) => {
                Some(v.to_ascii_uppercase())
            }
            [a, b, v] if a == "actix_web" && b == "web" && v == "route" => Some("ANY".into()),
            _ => None,
        })
}

fn parse_actix_route_builder(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
) -> Option<(String, HandlerRef)> {
    if expr.kind() != "call_expression" {
        return None;
    }
    let func = expr.child_by_field_name("function")?;
    let args = call_arguments(expr);
    if func.kind() == "field_expression" {
        let receiver = func.child_by_field_name("value")?;
        let field = func.child_by_field_name("field")?;
        let method_name = cx.text(&field).trim();
        if method_name == "to" {
            let handler_node = *args.first()?;
            let method = parse_actix_route_receiver(cx, receiver, bindings, value_shadows)?;
            let handler = resolve_handler_expr(cx, handler_node, bindings, locals, value_shadows);
            return Some((method, handler));
        }
        return parse_actix_route_builder(cx, receiver, bindings, locals, value_shadows);
    }
    let scopes = visible_scopes(cx, expr);
    let is_web_to = resolve_external_paths(
        cx,
        bindings,
        &scopes,
        cx.text(&func),
        Some(value_shadows),
    )
    .iter()
    .any(|p| matches!(p.as_slice(), [a, b, c] if a == "actix_web" && b == "web" && c == "to"));
    if is_web_to {
        let handler_node = *args.first()?;
        let handler = resolve_handler_expr(cx, handler_node, bindings, locals, value_shadows);
        return Some(("ANY".into(), handler));
    }
    None
}

/// Axum nests: a nested root route (`""` or `"/"`) is the prefix itself.
fn join_route(prefix: &str, route: &str) -> String {
    if prefix.is_empty() {
        return route.to_string();
    }
    let p = prefix.trim_end_matches('/');
    if route.is_empty() || route == "/" {
        if p.is_empty() {
            "/".to_string()
        } else {
            p.to_string()
        }
    } else {
        let r = route.trim_start_matches('/');
        format!("{p}/{r}")
    }
}

/// Actix joins a scope prefix and a route pattern as written: `""` is the
/// scope itself (`/api`) and `"/"` keeps its slash (`/api/`), unlike an
/// axum nested root route.
fn join_actix_route(prefix: &str, route: &str) -> String {
    if prefix.is_empty() {
        return route.to_string();
    }
    if route.is_empty() {
        return prefix.to_string();
    }
    format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        route.trim_start_matches('/')
    )
}

fn flatten_actix_chain(chain: ActixChain) -> (Vec<PendingDirectEndpoint>, Vec<ActixServiceMount>) {
    match chain {
        ActixChain::Resource {
            path,
            routes,
            evidence,
        } => {
            let mut endpoints = Vec::new();
            match path {
                RoutePathArg::Literal(lit_path) => {
                    for r in routes {
                        endpoints.push(PendingDirectEndpoint {
                            framework: "actix-web",
                            method: r.method,
                            path: RoutePathArg::Literal(lit_path.clone()),
                            handler: Some(r.handler),
                            evidence: r.evidence,
                        });
                    }
                }
                RoutePathArg::Computed(expr) => {
                    if routes.is_empty() {
                        endpoints.push(PendingDirectEndpoint {
                            framework: "actix-web",
                            method: String::new(),
                            path: RoutePathArg::Computed(expr),
                            handler: None,
                            evidence,
                        });
                    } else {
                        for r in routes {
                            endpoints.push(PendingDirectEndpoint {
                                framework: "actix-web",
                                method: r.method,
                                path: RoutePathArg::Computed(expr.clone()),
                                handler: Some(r.handler),
                                evidence: evidence.clone(),
                            });
                        }
                    }
                }
            }
            (endpoints, Vec::new())
        }
        ActixChain::Scope {
            prefix,
            mut endpoints,
            mut mounts,
            evidence,
        } => match prefix {
            RoutePathArg::Literal(lit_prefix) => {
                for ep in &mut endpoints {
                    if let RoutePathArg::Literal(sub) = &ep.path {
                        ep.path = RoutePathArg::Literal(join_actix_route(&lit_prefix, sub));
                    }
                }
                for m in &mut mounts {
                    if let RoutePathArg::Literal(sub) = &m.prefix {
                        m.prefix = RoutePathArg::Literal(join_actix_route(&lit_prefix, sub));
                    }
                }
                (endpoints, mounts)
            }
            RoutePathArg::Computed(expr) => {
                // Services under a computed scope are served at an unknown
                // path: their attribute routes become Gaps, not root routes.
                for m in &mut mounts {
                    m.prefix = RoutePathArg::Computed(expr.clone());
                }
                (
                    vec![PendingDirectEndpoint {
                        framework: "actix-web",
                        method: String::new(),
                        path: RoutePathArg::Computed(expr),
                        handler: None,
                        evidence,
                    }],
                    mounts,
                )
            }
        },
        ActixChain::App { endpoints, mounts } => (endpoints, mounts),
    }
}

fn eval_actix_chain(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
    local_actix: &BTreeMap<String, ActixChain>,
    consumed_actix: &mut BTreeSet<String>,
) -> Option<ActixChain> {
    if expr.kind() == "identifier" {
        let name = cx.text(&expr).trim();
        if let Some(chain) = local_actix.get(name) {
            consumed_actix.insert(name.to_string());
            return Some(chain.clone());
        }
        return None;
    }
    if expr.kind() != "call_expression" {
        return None;
    }
    let func = expr.child_by_field_name("function")?;
    let args = call_arguments(expr);
    if func.kind() == "field_expression" {
        let receiver = func.child_by_field_name("value")?;
        let field = func.child_by_field_name("field")?;
        let method_name = cx.text(&field).trim();
        let mut chain = eval_actix_chain(
            cx,
            receiver,
            bindings,
            locals,
            value_shadows,
            local_actix,
            consumed_actix,
        )?;
        match &mut chain {
            ActixChain::Resource { routes, .. } => match method_name {
                "route" => {
                    if let Some(&arg) = args.first()
                        && let Some((method, handler)) =
                            parse_actix_route_builder(cx, arg, bindings, locals, value_shadows)
                    {
                        routes.push(ActixResourceRoute {
                            method,
                            handler,
                            evidence: cx.evidence(&expr),
                        });
                    }
                }
                "to" => {
                    if let Some(&arg) = args.first() {
                        let handler =
                            resolve_handler_expr(cx, arg, bindings, locals, value_shadows);
                        routes.push(ActixResourceRoute {
                            method: "ANY".into(),
                            handler,
                            evidence: cx.evidence(&expr),
                        });
                    }
                }
                _ => {}
            },
            ActixChain::Scope {
                endpoints, mounts, ..
            } => match method_name {
                "route" => {
                    if args.len() >= 2 {
                        let path = parse_route_path_arg(cx, args[0]);
                        if let Some((method, handler)) =
                            parse_actix_route_builder(cx, args[1], bindings, locals, value_shadows)
                        {
                            endpoints.push(PendingDirectEndpoint {
                                framework: "actix-web",
                                method,
                                path,
                                handler: Some(handler),
                                evidence: cx.evidence(&expr),
                            });
                        }
                    }
                }
                "service" => {
                    if let Some(&arg) = args.first() {
                        if let Some(child_chain) = eval_actix_chain(
                            cx,
                            arg,
                            bindings,
                            locals,
                            value_shadows,
                            local_actix,
                            consumed_actix,
                        ) {
                            let (child_eps, child_mounts) = flatten_actix_chain(child_chain);
                            endpoints.extend(child_eps);
                            mounts.extend(child_mounts);
                        } else {
                            let href =
                                resolve_handler_expr(cx, arg, bindings, locals, value_shadows);
                            if !matches!(href.target, HandlerTarget::Unresolved) {
                                mounts.push(ActixServiceMount {
                                    prefix: RoutePathArg::Literal(String::new()),
                                    target: href.target,
                                });
                            }
                        }
                    }
                }
                _ => {}
            },
            ActixChain::App { endpoints, mounts } => match method_name {
                "route" => {
                    if args.len() >= 2 {
                        let path = parse_route_path_arg(cx, args[0]);
                        if let Some((method, handler)) =
                            parse_actix_route_builder(cx, args[1], bindings, locals, value_shadows)
                        {
                            endpoints.push(PendingDirectEndpoint {
                                framework: "actix-web",
                                method,
                                path,
                                handler: Some(handler),
                                evidence: cx.evidence(&expr),
                            });
                        }
                    }
                }
                "service" => {
                    if let Some(&arg) = args.first() {
                        if let Some(child_chain) = eval_actix_chain(
                            cx,
                            arg,
                            bindings,
                            locals,
                            value_shadows,
                            local_actix,
                            consumed_actix,
                        ) {
                            let (child_eps, child_mounts) = flatten_actix_chain(child_chain);
                            endpoints.extend(child_eps);
                            mounts.extend(child_mounts);
                        } else {
                            // A handler served at the root keeps its bare
                            // path beside any scoped mount of it.
                            let href =
                                resolve_handler_expr(cx, arg, bindings, locals, value_shadows);
                            if !matches!(href.target, HandlerTarget::Unresolved) {
                                mounts.push(ActixServiceMount {
                                    prefix: RoutePathArg::Literal(String::new()),
                                    target: href.target,
                                });
                            }
                        }
                    }
                }
                _ => {}
            },
        }
        return Some(chain);
    }
    let scopes = visible_scopes(cx, expr);
    let resolved =
        resolve_external_paths(cx, bindings, &scopes, cx.text(&func), Some(value_shadows));
    for p in resolved {
        match p.as_slice() {
            [a, b, c]
                if a == "actix_web"
                    && ((b == "web" && c == "resource") || (b == "Resource" && c == "new")) =>
            {
                let arg = *args.first()?;
                let path = parse_route_path_arg(cx, arg);
                return Some(ActixChain::Resource {
                    path,
                    routes: Vec::new(),
                    evidence: cx.evidence(&expr),
                });
            }
            [a, b, c]
                if a == "actix_web"
                    && ((b == "web" && c == "scope") || (b == "Scope" && c == "new")) =>
            {
                let arg = *args.first()?;
                let prefix = parse_route_path_arg(cx, arg);
                return Some(ActixChain::Scope {
                    prefix,
                    endpoints: Vec::new(),
                    mounts: Vec::new(),
                    evidence: cx.evidence(&expr),
                });
            }
            [a, b, c] if a == "actix_web" && b == "App" && c == "new" => {
                return Some(ActixChain::App {
                    endpoints: Vec::new(),
                    mounts: Vec::new(),
                });
            }
            _ => {}
        }
    }
    None
}

struct FnRouteState {
    local_routers: BTreeMap<String, Vec<AxumRouterItem>>,
    local_router_order: Vec<String>,
    local_call_vars: BTreeMap<String, HandlerTarget>,
    consumed_locals: BTreeSet<String>,
    standalone_axum: Vec<Vec<AxumRouterItem>>,
    local_actix: BTreeMap<String, ActixChain>,
    local_actix_order: Vec<String>,
    consumed_actix: BTreeSet<String>,
    standalone_actix: Vec<ActixChain>,
}

impl FnRouteState {
    fn new() -> Self {
        Self {
            local_routers: BTreeMap::new(),
            local_router_order: Vec::new(),
            local_call_vars: BTreeMap::new(),
            consumed_locals: BTreeSet::new(),
            standalone_axum: Vec::new(),
            local_actix: BTreeMap::new(),
            local_actix_order: Vec::new(),
            consumed_actix: BTreeSet::new(),
            standalone_actix: Vec::new(),
        }
    }
}

fn walk_fn_routes(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    value_shadows: &BTreeSet<String>,
    state: &mut FnRouteState,
) {
    match node.kind() {
        "function_item" => return,
        "let_declaration" => {
            let var_name = node
                .child_by_field_name("pattern")
                .filter(|p| p.kind() == "identifier")
                .map(|p| cx.text(&p).trim().to_string());
            if let Some(val_node) = node.child_by_field_name("value") {
                if let Some(items) = eval_axum_expr(
                    cx,
                    val_node,
                    bindings,
                    locals,
                    value_shadows,
                    &state.local_routers,
                    &state.local_call_vars,
                    &mut state.consumed_locals,
                ) {
                    if let Some(name) = var_name {
                        state.local_routers.insert(name.clone(), items);
                        if !state.local_router_order.contains(&name) {
                            state.local_router_order.push(name.clone());
                        }
                        state.consumed_locals.remove(&name);
                    } else {
                        state.standalone_axum.push(items);
                    }
                    return;
                }
                if let Some(chain) = eval_actix_chain(
                    cx,
                    val_node,
                    bindings,
                    locals,
                    value_shadows,
                    &state.local_actix,
                    &mut state.consumed_actix,
                ) {
                    if let Some(name) = var_name {
                        state.local_actix.insert(name.clone(), chain);
                        if !state.local_actix_order.contains(&name) {
                            state.local_actix_order.push(name.clone());
                        }
                        state.consumed_actix.remove(&name);
                    } else {
                        state.standalone_actix.push(chain);
                    }
                    return;
                }
                if val_node.kind() == "call_expression"
                    && let (Some(name), Some(callee)) =
                        (&var_name, val_node.child_by_field_name("function"))
                {
                    let mut shadows = value_shadows.clone();
                    shadows.remove(name.as_str());
                    let href = resolve_handler_expr(cx, callee, bindings, locals, &shadows);
                    if !matches!(href.target, HandlerTarget::Unresolved) {
                        state.local_call_vars.insert(name.clone(), href.target);
                    }
                }
                walk_fn_routes(cx, val_node, bindings, locals, value_shadows, state);
            }
            return;
        }
        "assignment_expression" => {
            if let (Some(l), Some(r)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) && l.kind() == "identifier"
            {
                let name = cx.text(&l).trim().to_string();
                if let Some(items) = eval_axum_expr(
                    cx,
                    r,
                    bindings,
                    locals,
                    value_shadows,
                    &state.local_routers,
                    &state.local_call_vars,
                    &mut state.consumed_locals,
                ) {
                    state.local_routers.insert(name.clone(), items);
                    if !state.local_router_order.contains(&name) {
                        state.local_router_order.push(name.clone());
                    }
                    state.consumed_locals.remove(&name);
                    return;
                }
                if let Some(chain) = eval_actix_chain(
                    cx,
                    r,
                    bindings,
                    locals,
                    value_shadows,
                    &state.local_actix,
                    &mut state.consumed_actix,
                ) {
                    state.local_actix.insert(name.clone(), chain);
                    if !state.local_actix_order.contains(&name) {
                        state.local_actix_order.push(name.clone());
                    }
                    state.consumed_actix.remove(&name);
                    return;
                }
            }
        }
        "call_expression" => {
            if let Some(items) = eval_axum_expr(
                cx,
                node,
                bindings,
                locals,
                value_shadows,
                &state.local_routers,
                &state.local_call_vars,
                &mut state.consumed_locals,
            ) {
                state.standalone_axum.push(items);
                return;
            }
            if let Some(chain) = eval_actix_chain(
                cx,
                node,
                bindings,
                locals,
                value_shadows,
                &state.local_actix,
                &mut state.consumed_actix,
            ) {
                state.standalone_actix.push(chain);
                return;
            }
        }
        _ => {}
    }

    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        walk_fn_routes(cx, child, bindings, locals, value_shadows, state);
    }
}

pub(super) fn extract_function_routes(
    cx: &FileCx<'_>,
    fn_node: TsNode<'_>,
    fn_sym: &str,
    bindings: &Bindings,
    locals: &HashMap<(String, String), String>,
    out: &mut Extraction,
) {
    let Some(body) = fn_node.child_by_field_name("body") else {
        return;
    };
    let value_shadows = value_bindings(cx, fn_node);
    let mut state = FnRouteState::new();
    walk_fn_routes(cx, body, bindings, locals, &value_shadows, &mut state);

    let mut all_axum_items = Vec::new();
    for name in &state.local_router_order {
        if !state.consumed_locals.contains(name)
            && let Some(items) = state.local_routers.remove(name)
        {
            all_axum_items.extend(items);
        }
    }
    for items in state.standalone_axum {
        all_axum_items.extend(items);
    }
    if !all_axum_items.is_empty() {
        let owner_fn = fn_node
            .child_by_field_name("return_type")
            .map(|_| fn_sym.to_string());
        out.axum_routers.push(AxumRouterDef {
            owner_fn,
            items: all_axum_items,
        });
    }

    for name in &state.local_actix_order {
        if !state.consumed_actix.contains(name)
            && let Some(chain) = state.local_actix.remove(name)
        {
            let (eps, mounts) = flatten_actix_chain(chain);
            out.pending_endpoints.extend(eps);
            out.actix_service_mounts.extend(mounts);
        }
    }
    for chain in state.standalone_actix {
        let (eps, mounts) = flatten_actix_chain(chain);
        out.pending_endpoints.extend(eps);
        out.actix_service_mounts.extend(mounts);
    }
}

fn resolve_handler_target(target: &HandlerTarget, index: &SymbolIndex) -> Option<String> {
    match target {
        HandlerTarget::Resolved(sym) => Some(sym.clone()),
        HandlerTarget::Pending(Target::Path(parts)) => lookup(index, parts).ok(),
        HandlerTarget::Pending(Target::Method {
            crate_name,
            type_name,
            method,
        }) => {
            let key = (crate_name.clone(), type_name.clone(), method.clone());
            if index.ambiguous_methods.contains(&key) {
                None
            } else {
                index.by_method.get(&key).cloned()
            }
        }
        HandlerTarget::Pending(Target::Candidates { paths, .. }) => {
            let hits: BTreeSet<String> = paths
                .iter()
                .filter_map(|path| lookup(index, path).ok())
                .collect();
            (hits.len() == 1).then(|| hits.into_iter().next().expect("one hit"))
        }
        HandlerTarget::Unresolved => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_endpoint(
    framework: &str,
    method: &str,
    path: &str,
    handler: &HandlerRef,
    evidence: &EvidenceRef,
    index: &SymbolIndex,
    out: &mut Extraction,
    seen_nodes: &mut BTreeSet<String>,
    seen_edges: &mut BTreeSet<(String, String, String)>,
) -> (String, Option<String>) {
    let ep_id = format!("ep:{}@{method}:{path}", evidence.repo);
    let resolved_sym = resolve_handler_target(&handler.target, index);
    let dst_id = match &resolved_sym {
        Some(sym) => sym.clone(),
        None => format!(
            "gap:rust-handler:{}@{}@{}",
            handler.evidence.repo, handler.evidence.path, handler.evidence.byte_start
        ),
    };
    if seen_nodes.insert(ep_id.clone()) {
        out.nodes.push(Node {
            id: ep_id.clone(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": method,
                "path": path,
                "framework": framework,
                "language": "rust",
                "handler_sym": dst_id,
                "prov": prov_from_evidence(
                    evidence,
                    ConfidenceTier::Confirmed,
                    &format!("Endpoint {ep_id}"),
                ),
            }),
        });
    }
    match resolved_sym.clone() {
        Some(sym) => {
            if seen_edges.insert((ep_id.clone(), sym.clone(), "HANDLES".into())) {
                out.edges.push(Edge {
                    src: ep_id.clone(),
                    dst: sym.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "prov": prov_from_evidence(
                            evidence,
                            ConfidenceTier::Confirmed,
                            &format!("HANDLES {ep_id} -> {sym}"),
                        ),
                    }),
                });
            }
        }
        None => {
            if seen_nodes.insert(dst_id.clone()) {
                out.nodes.push(Node {
                    id: dst_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "handler_expression": handler.expression,
                        "reason": "unresolved Rust route handler expression",
                        "attempted_tiers": ["T0"],
                        "prov": prov_from_evidence(
                            &handler.evidence,
                            ConfidenceTier::Gap,
                            &format!("Gap {dst_id}"),
                        ),
                    }),
                });
            }
            if seen_edges.insert((ep_id.clone(), dst_id.clone(), "HANDLES".into())) {
                out.edges.push(Edge {
                    src: ep_id.clone(),
                    dst: dst_id.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "reason": "unresolved Rust route handler expression",
                        "attempted_resolution": "rust-handler-expression",
                        "prov": prov_from_evidence(
                            &handler.evidence,
                            ConfidenceTier::Gap,
                            &format!("HANDLES {ep_id} -> {dst_id}"),
                        ),
                    }),
                });
            }
        }
    }
    (ep_id, resolved_sym)
}

fn emit_computed_route(
    expression: &str,
    evidence: &EvidenceRef,
    handler_sym: Option<String>,
    out: &mut Extraction,
    seen_nodes: &mut BTreeSet<String>,
    seen_edges: &mut BTreeSet<(String, String, String)>,
) {
    let gap_id = format!(
        "gap:route:{}@{}@{}",
        evidence.repo, evidence.path, evidence.byte_start
    );
    if seen_nodes.insert(gap_id.clone()) {
        out.nodes.push(Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "expression": expression,
                "reason": "computed Rust route path",
                "attempted_tiers": ["T0"],
                "prov": prov_from_evidence(
                    evidence,
                    ConfidenceTier::Gap,
                    &format!("Gap {gap_id}"),
                ),
            }),
        });
    }
    if let Some(sym) = handler_sym
        && seen_edges.insert((gap_id.clone(), sym.clone(), "HANDLES".into()))
    {
        out.edges.push(Edge {
            src: gap_id.clone(),
            dst: sym.clone(),
            label: "HANDLES".into(),
            props: serde_json::json!({
                "reason": "computed Rust route path",
                "prov": prov_from_evidence(
                    evidence,
                    ConfidenceTier::Gap,
                    &format!("HANDLES {gap_id} -> {sym}"),
                ),
            }),
        });
    }
}

fn mark_consumed_axum_fns(
    items: &[AxumRouterItem],
    routers_by_fn: &BTreeMap<String, Vec<AxumRouterItem>>,
    index: &SymbolIndex,
    consumed: &mut BTreeSet<String>,
) {
    for item in items {
        let sub = match item {
            AxumRouterItem::Nest { sub, .. } | AxumRouterItem::Merge { sub } => sub,
            AxumRouterItem::Route { .. } | AxumRouterItem::Layer(_) => continue,
        };
        match sub {
            AxumSubRouter::Inline(inner) => {
                mark_consumed_axum_fns(inner, routers_by_fn, index, consumed);
            }
            AxumSubRouter::Call { target } => {
                if let Some(sym) = resolve_handler_target(target, index) {
                    let key = sym;
                    if routers_by_fn.contains_key(&key) {
                        consumed.insert(key);
                    }
                }
            }
            AxumSubRouter::None => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_axum_items(
    items: &[AxumRouterItem],
    prefix: &str,
    outer: &[auth::Layer],
    routers_by_fn: &BTreeMap<String, Vec<AxumRouterItem>>,
    index: &SymbolIndex,
    visited: &mut BTreeSet<String>,
    out: &mut Extraction,
    seen_nodes: &mut BTreeSet<String>,
    seen_edges: &mut BTreeSet<(String, String, String)>,
    routes: &mut Vec<auth::Route>,
) {
    for (at, item) in items.iter().enumerate() {
        // A layer wraps the routes added before it, so an item is under the
        // layers that follow it and every layer over this router.
        let mut layers = outer.to_vec();
        for later in &items[at + 1..] {
            if let AxumRouterItem::Layer(found) = later {
                layers.extend(found.iter().cloned());
            }
        }
        match item {
            AxumRouterItem::Layer(_) => {}
            AxumRouterItem::Route {
                path,
                method,
                handler,
                evidence,
            } => match path {
                RoutePathArg::Literal(sub_path) => {
                    let full_path = join_route(prefix, sub_path);
                    let (endpoint, handler) = emit_endpoint(
                        "axum", method, &full_path, handler, evidence, index, out, seen_nodes,
                        seen_edges,
                    );
                    routes.push(auth::Route {
                        endpoint,
                        handler,
                        layers: layers.clone(),
                    });
                }
                RoutePathArg::Computed(expr) => {
                    let sym = resolve_handler_target(&handler.target, index);
                    emit_computed_route(expr, evidence, sym, out, seen_nodes, seen_edges);
                }
            },
            AxumRouterItem::Nest {
                prefix: nest_prefix,
                sub,
                evidence,
            } => match nest_prefix {
                RoutePathArg::Computed(expr) => {
                    emit_computed_route(expr, evidence, None, out, seen_nodes, seen_edges);
                }
                RoutePathArg::Literal(lit_prefix) => {
                    let next_prefix = join_route(prefix, lit_prefix);
                    match sub {
                        AxumSubRouter::Inline(inner) => {
                            expand_axum_items(
                                inner,
                                &next_prefix,
                                &layers,
                                routers_by_fn,
                                index,
                                visited,
                                out,
                                seen_nodes,
                                seen_edges,
                                routes,
                            );
                        }
                        AxumSubRouter::Call { target } => {
                            if let Some(sym) = resolve_handler_target(target, index) {
                                let key = sym;
                                if let Some(inner) = routers_by_fn.get(&key)
                                    && visited.insert(key.clone())
                                {
                                    expand_axum_items(
                                        inner,
                                        &next_prefix,
                                        &layers,
                                        routers_by_fn,
                                        index,
                                        visited,
                                        out,
                                        seen_nodes,
                                        seen_edges,
                                        routes,
                                    );
                                    visited.remove(&key);
                                }
                            }
                        }
                        AxumSubRouter::None => {}
                    }
                }
            },
            AxumRouterItem::Merge { sub } => match sub {
                AxumSubRouter::Inline(inner) => {
                    expand_axum_items(
                        inner,
                        prefix,
                        &layers,
                        routers_by_fn,
                        index,
                        visited,
                        out,
                        seen_nodes,
                        seen_edges,
                        routes,
                    );
                }
                AxumSubRouter::Call { target } => {
                    if let Some(sym) = resolve_handler_target(target, index) {
                        let key = sym;
                        if let Some(inner) = routers_by_fn.get(&key)
                            && visited.insert(key.clone())
                        {
                            expand_axum_items(
                                inner,
                                prefix,
                                &layers,
                                routers_by_fn,
                                index,
                                visited,
                                out,
                                seen_nodes,
                                seen_edges,
                                routes,
                            );
                            visited.remove(&key);
                        }
                    }
                }
                AxumSubRouter::None => {}
            },
        }
    }
}

pub(super) fn finalize_endpoints(out: &mut Extraction, index: &SymbolIndex) {
    let mut seen_nodes = BTreeSet::new();
    let mut seen_edges = BTreeSet::new();
    let mut routes = Vec::new();

    let axum_routers = std::mem::take(&mut out.axum_routers);
    let mut routers_by_fn: BTreeMap<String, Vec<AxumRouterItem>> = BTreeMap::new();
    for router in &axum_routers {
        if let Some(key) = &router.owner_fn {
            routers_by_fn
                .entry(key.clone())
                .or_default()
                .extend(router.items.clone());
        }
    }
    let mut consumed_fns = BTreeSet::new();
    for router in &axum_routers {
        mark_consumed_axum_fns(&router.items, &routers_by_fn, index, &mut consumed_fns);
    }
    for router in &axum_routers {
        if router
            .owner_fn
            .as_ref()
            .is_some_and(|k| consumed_fns.contains(k))
        {
            continue;
        }
        let mut visited = BTreeSet::new();
        if let Some(key) = &router.owner_fn {
            visited.insert(key.clone());
        }
        expand_axum_items(
            &router.items,
            "",
            &[],
            &routers_by_fn,
            index,
            &mut visited,
            out,
            &mut seen_nodes,
            &mut seen_edges,
            &mut routes,
        );
    }

    let actix_mounts = std::mem::take(&mut out.actix_service_mounts);
    // Handler symbol -> scope prefixes it is mounted under (`None`: computed).
    let mut mounts_by_sym: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
    for mount in actix_mounts {
        let prefix = match mount.prefix {
            RoutePathArg::Literal(p) => Some(p),
            RoutePathArg::Computed(_) => None,
        };
        if let Some(sym) = resolve_handler_target(&mount.target, index) {
            let list = mounts_by_sym.entry(sym).or_default();
            if !list.contains(&prefix) {
                list.push(prefix);
            }
        }
    }

    for attr in std::mem::take(&mut out.actix_attr_routes) {
        match &attr.path {
            RoutePathArg::Computed(expr) => {
                emit_computed_route(
                    expr,
                    &attr.evidence,
                    Some(attr.sym_id.clone()),
                    out,
                    &mut seen_nodes,
                    &mut seen_edges,
                );
            }
            RoutePathArg::Literal(sub_path) => {
                let handler = HandlerRef {
                    target: HandlerTarget::Resolved(attr.sym_id.clone()),
                    expression: attr.sym_id.clone(),
                    evidence: attr.evidence.clone(),
                };
                let prefixes = mounts_by_sym
                    .get(&attr.sym_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if prefixes.is_empty() {
                    for method in &attr.methods {
                        let (endpoint, handler) = emit_endpoint(
                            "actix-web",
                            method,
                            sub_path,
                            &handler,
                            &attr.evidence,
                            index,
                            out,
                            &mut seen_nodes,
                            &mut seen_edges,
                        );
                        routes.push(auth::Route {
                            endpoint,
                            handler,
                            layers: Vec::new(),
                        });
                    }
                } else {
                    for pfx in prefixes {
                        let Some(pfx) = pfx else {
                            emit_computed_route(
                                sub_path,
                                &attr.evidence,
                                Some(attr.sym_id.clone()),
                                out,
                                &mut seen_nodes,
                                &mut seen_edges,
                            );
                            continue;
                        };
                        let full_path = join_actix_route(pfx, sub_path);
                        for method in &attr.methods {
                            let (endpoint, handler) = emit_endpoint(
                                "actix-web",
                                method,
                                &full_path,
                                &handler,
                                &attr.evidence,
                                index,
                                out,
                                &mut seen_nodes,
                                &mut seen_edges,
                            );
                            routes.push(auth::Route {
                                endpoint,
                                handler,
                                layers: Vec::new(),
                            });
                        }
                    }
                }
            }
        }
    }

    for ep in std::mem::take(&mut out.pending_endpoints) {
        match &ep.path {
            RoutePathArg::Computed(expr) => {
                let sym = ep
                    .handler
                    .as_ref()
                    .and_then(|h| resolve_handler_target(&h.target, index));
                emit_computed_route(
                    expr,
                    &ep.evidence,
                    sym,
                    out,
                    &mut seen_nodes,
                    &mut seen_edges,
                );
            }
            RoutePathArg::Literal(full_path) => {
                if let Some(handler) = &ep.handler {
                    let (endpoint, handler) = emit_endpoint(
                        ep.framework,
                        &ep.method,
                        full_path,
                        handler,
                        &ep.evidence,
                        index,
                        out,
                        &mut seen_nodes,
                        &mut seen_edges,
                    );
                    routes.push(auth::Route {
                        endpoint,
                        handler,
                        layers: Vec::new(),
                    });
                }
            }
        }
    }
    auth::resolve(out, routes, index);
}
