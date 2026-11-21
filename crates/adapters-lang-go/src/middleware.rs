//! Endpoint auth from Go middleware (#2163, AC-1304): `props.auth` on every
//! `net/http`, chi and gin `Endpoint`, in one of two states.
//!
//! - `required`: a middleware applied to the endpoint's registration is a
//!   guard. It is applied by wrapping the handler (`mux.Handle("/x",
//!   requireAuth(h))`), by chi `r.With(m).Get(...)`, or by `r.Use(m)` on the
//!   same chi router value (every route on it) or gin engine (routes
//!   registered after a `Use` statement earlier in the same function; a gin
//!   `Use` elsewhere is an `unordered_middleware` reason). A repository
//!   function wrapping the handler counts only when it takes a handler, so
//!   a handler factory (`makeHandler(db)`) is not middleware.
//!
//!   A guard is a library guard bound by its import (gin `BasicAuth` /
//!   `BasicAuthForRealm`, chi middleware `BasicAuth`, jwtauth
//!   `Authenticator`) or a repository function whose body answers the
//!   request with 401 or 403 (a literal, or `http.StatusUnauthorized` /
//!   `http.StatusForbidden` through the `net/http` import) or with a
//!   package-level error value built with one (a `var`, or a function
//!   returning a composite literal that names the status, as in
//!   `render.Render(w, r, ErrForbidden)`; the value's declaration is
//!   cited). A call answers the request when it is `http.Error`,
//!   `WriteHeader` on an `http.ResponseWriter` parameter, a response call on
//!   a `*gin.Context` parameter (`AbortWithStatus`, `AbortWithStatusJSON`,
//!   `AbortWithError`, `JSON`, `String`, `Status`), or any call handed one
//!   of those parameters; a status that is only logged does not count.
//! - `unknown`: anything else, with sorted reasons. Go has no declarative
//!   way to say a route is public, and a handler may check the caller
//!   itself, so an endpoint is never `none`.
//!
//! A middleware expression that does not resolve to a library entry or a
//! repository function is a Gap the endpoint `DEPENDS_ON`, naming it. Known
//! non-authenticating library middleware (loggers, recoverers, request ids,
//! jwtauth `Verifier`, ...) is listed in `non_auth_middleware`. Middleware
//! on an enclosing router (chi `Group` / `Route` parents), a server-wide
//! wrapper around the mux, and gin route groups are not read.

use super::{
    EXTRACTOR_ID, Extraction, FileCx, Framework, ImportBinding, Receivers, call_arguments,
    enclosing_receiver, package_dir,
};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

/// Library middleware that rejects a request without credentials:
/// `(import path, name, guard as recorded)`.
const GUARDS: &[(&str, &str, &str)] = &[
    ("github.com/gin-gonic/gin", "BasicAuth", "gin.BasicAuth"),
    (
        "github.com/gin-gonic/gin",
        "BasicAuthForRealm",
        "gin.BasicAuthForRealm",
    ),
    (
        "github.com/go-chi/chi/middleware",
        "BasicAuth",
        "chi.middleware.BasicAuth",
    ),
    (
        "github.com/go-chi/chi/v5/middleware",
        "BasicAuth",
        "chi.middleware.BasicAuth",
    ),
    (
        "github.com/go-chi/jwtauth",
        "Authenticator",
        "jwtauth.Authenticator",
    ),
    (
        "github.com/go-chi/jwtauth/v5",
        "Authenticator",
        "jwtauth.Authenticator",
    ),
];

/// Library middleware that never rejects for lack of credentials.
const NON_AUTH: &[(&str, &[&str])] = &[
    (
        "github.com/gin-gonic/gin",
        &[
            "Logger",
            "LoggerWithConfig",
            "LoggerWithFormatter",
            "LoggerWithWriter",
            "Recovery",
            "RecoveryWithWriter",
            "CustomRecovery",
        ],
    ),
    ("github.com/go-chi/chi/middleware", CHI_MIDDLEWARE),
    ("github.com/go-chi/chi/v5/middleware", CHI_MIDDLEWARE),
    ("github.com/go-chi/jwtauth", &["Verifier", "Verify"]),
    ("github.com/go-chi/jwtauth/v5", &["Verifier", "Verify"]),
    ("github.com/go-chi/cors", &["Handler", "AllowAll"]),
    (
        "github.com/go-chi/httprate",
        &["LimitByIP", "LimitByRealIP", "Limit", "LimitAll"],
    ),
    (
        "net/http",
        &["StripPrefix", "TimeoutHandler", "MaxBytesHandler"],
    ),
];

const CHI_MIDDLEWARE: &[&str] = &[
    "AllowContentEncoding",
    "AllowContentType",
    "CleanPath",
    "Compress",
    "ContentCharset",
    "GetHead",
    "Heartbeat",
    "Logger",
    "NoCache",
    "RealIP",
    "Recoverer",
    "RedirectSlashes",
    "RequestID",
    "RequestLogger",
    "SetHeader",
    "StripSlashes",
    "Throttle",
    "ThrottleBacklog",
    "Timeout",
    "URLFormat",
    "WithValue",
];

/// `*gin.Context` methods whose status argument answers the request.
const GIN_RESPONSE_CALLS: &[&str] = &[
    "AbortWithStatus",
    "AbortWithStatusJSON",
    "AbortWithError",
    "JSON",
    "String",
    "Status",
];

/// What one applied middleware is.
#[derive(Debug, Clone)]
pub(super) enum Kind {
    /// A library guard: its recorded name, and `path.Name`.
    Guard(String, String),
    /// Library middleware that does not authenticate.
    NonAuth,
    /// A repository function, resolved after every file is read.
    Repo {
        package_dir: String,
        exported: String,
    },
    /// An expression this pass cannot read.
    Unreadable,
    /// A gin `Use` on the route's engine whose order against the route is
    /// not provable (another function, or a conditional statement).
    Unordered,
}

/// One middleware applied to a registration.
#[derive(Debug, Clone)]
pub(super) struct Applied {
    name: String,
    kind: Kind,
    site: EvidenceRef,
    /// Applied by wrapping the handler (`m(h)`): a repository function
    /// here is middleware only when it takes a handler.
    wrapper: bool,
}

/// The middleware applied to one route registration.
#[derive(Debug, Clone)]
pub(super) struct Pending {
    endpoint: String,
    applied: Vec<Applied>,
}

/// What a repository function's body says about 401 and 403: a response
/// call it makes with one, and the repository values it passes to calls
/// (`render.Render(w, r, ErrForbidden)`), resolved once every file is read.
#[derive(Debug, Clone)]
pub(super) struct Denial {
    package_dir: String,
    qualified: String,
    direct: Option<(u16, EvidenceRef)>,
    refs: Vec<(String, String, EvidenceRef)>,
    /// The function takes a handler (`next http.Handler`), so it can wrap
    /// one.
    wraps: bool,
}

/// A package-level error value built with 401 or 403: a `var` whose value,
/// or a function whose `return`, holds a composite literal naming one
/// (`&ErrResponse{HTTPStatusCode: http.StatusUnauthorized}`).
#[derive(Debug, Clone)]
pub(super) struct DenyValue {
    package_dir: String,
    name: String,
    status: u16,
    /// The declaration that names the status.
    evidence: EvidenceRef,
}

/// Most repository values read from one function body.
const MAX_REFS: usize = 32;

/// Move cached sites to `commit`.
pub(super) fn retarget(out: &mut Extraction, commit: &str) {
    for pending in &mut out.pending_auth {
        for applied in &mut pending.applied {
            applied.site.commit_sha = commit.into();
        }
    }
    for denial in &mut out.denials {
        if let Some((_, site)) = &mut denial.direct {
            site.commit_sha = commit.into();
        }
        for (_, _, site) in &mut denial.refs {
            site.commit_sha = commit.into();
        }
    }
    for value in &mut out.deny_values {
        value.evidence.commit_sha = commit.into();
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

fn shorten(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() <= 120 {
        return text;
    }
    let mut cut = 120;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// The 401 or 403 a status argument names, if any.
fn deny_status(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<u16> {
    match node.kind() {
        "int_literal" => match cx.text(&node) {
            "401" => Some(401),
            "403" => Some(403),
            _ => None,
        },
        "selector_expression" => {
            let base = cx.text(&node.child_by_field_name("operand")?);
            if bindings.get(base)?.path != "net/http" {
                return None;
            }
            match cx.text(&node.child_by_field_name("field")?) {
                "StatusUnauthorized" => Some(401),
                "StatusForbidden" => Some(403),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The 401 or 403 a composite literal under `node` names.
fn literal_status(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<u16> {
    let mut stack = vec![(node, false)];
    while let Some((node, in_literal)) = stack.pop() {
        let in_literal = in_literal || node.kind() == "composite_literal";
        if in_literal && let Some(status) = deny_status(cx, node, bindings) {
            return Some(status);
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk).map(|c| (c, in_literal)));
    }
    None
}

/// The file's package-level error values built with 401 or 403.
pub(super) fn deny_values(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Vec<DenyValue> {
    let mut found = Vec::new();
    let mut walk = root.walk();
    for item in root.named_children(&mut walk) {
        match item.kind() {
            "var_declaration" => {
                let mut stack = vec![item];
                while let Some(node) = stack.pop() {
                    let mut walk = node.walk();
                    stack.extend(node.named_children(&mut walk));
                    if node.kind() != "var_spec" {
                        continue;
                    }
                    let Some(status) = node
                        .child_by_field_name("value")
                        .and_then(|value| literal_status(cx, value, bindings))
                    else {
                        continue;
                    };
                    let mut walk = node.walk();
                    for name in node.children_by_field_name("name", &mut walk) {
                        found.push(DenyValue {
                            package_dir: package_dir(cx.path),
                            name: cx.text(&name).to_string(),
                            status,
                            evidence: evidence(cx, node),
                        });
                    }
                }
            }
            "function_declaration" => {
                let (Some(name), Some(body)) = (
                    item.child_by_field_name("name"),
                    item.child_by_field_name("body"),
                ) else {
                    continue;
                };
                let mut stack = vec![body];
                while let Some(node) = stack.pop() {
                    let mut walk = node.walk();
                    stack.extend(node.named_children(&mut walk));
                    if node.kind() == "return_statement"
                        && let Some(status) = literal_status(cx, node, bindings)
                    {
                        found.push(DenyValue {
                            package_dir: package_dir(cx.path),
                            name: cx.text(&name).to_string(),
                            status,
                            evidence: evidence(cx, node),
                        });
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// The repository value an argument names: a same-package identifier or a
/// call of one, or `pkg.Name` / `pkg.Name(...)` of an imported repository
/// package.
fn value_ref(
    cx: &FileCx<'_>,
    argument: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(String, String)> {
    let target = if argument.kind() == "call_expression" {
        argument.child_by_field_name("function")?
    } else {
        argument
    };
    match target.kind() {
        "identifier" => Some((package_dir(cx.path), cx.text(&target).to_string())),
        "selector_expression" => {
            let base = target.child_by_field_name("operand")?;
            let dir = bindings.get(cx.text(&base))?.package_dir.clone()?;
            Some((
                dir,
                cx.text(&target.child_by_field_name("field")?).to_string(),
            ))
        }
        _ => None,
    }
}

/// The parameters in `function` (nested func literals included) that hold
/// the response, keyed by name: `true` for an `http.ResponseWriter`,
/// `false` for gin's `*gin.Context`.
fn response_params(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> HashMap<String, bool> {
    let mut params = HashMap::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "parameter_declaration" {
            continue;
        }
        let Some(ty) = node.child_by_field_name("type") else {
            continue;
        };
        let ty = cx.text(&ty).trim_start_matches('*');
        let Some((alias, name)) = ty.split_once('.') else {
            continue;
        };
        let writer = match (bindings.get(alias).map(|b| b.path.as_str()), name) {
            (Some("net/http"), "ResponseWriter") => true,
            (Some("github.com/gin-gonic/gin"), "Context") => false,
            _ => continue,
        };
        let mut walk = node.walk();
        for name in node.children_by_field_name("name", &mut walk) {
            params.insert(cx.text(&name).to_string(), writer);
        }
    }
    params
}

/// Whether `call` answers the request: `http.Error`, `WriteHeader` on a
/// response writer parameter, a response method on a `*gin.Context`
/// parameter, or any call handed one of those parameters (a repository
/// helper or `render.Render(w, r, ...)`). A call that only mentions a
/// status, such as `log.Println(http.StatusUnauthorized)`, does not.
fn answers(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    arguments: &[TsNode<'_>],
    params: &HashMap<String, bool>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    if arguments
        .iter()
        .any(|argument| argument.kind() == "identifier" && params.contains_key(cx.text(argument)))
    {
        return true;
    }
    let Some(callee) = call
        .child_by_field_name("function")
        .filter(|callee| callee.kind() == "selector_expression")
    else {
        return false;
    };
    let (Some(operand), Some(field)) = (
        callee.child_by_field_name("operand"),
        callee.child_by_field_name("field"),
    ) else {
        return false;
    };
    let (base, field) = (cx.text(&operand), cx.text(&field));
    match params.get(base) {
        Some(true) => field == "WriteHeader",
        Some(false) => GIN_RESPONSE_CALLS.contains(&field),
        None => bindings.get(base).is_some_and(|b| b.path == "net/http") && field == "Error",
    }
}

/// Whether `function` takes a handler: a parameter typed `http.Handler`,
/// `http.HandlerFunc`, `gin.HandlerFunc` or `func(http.ResponseWriter, ...)`.
fn takes_handler(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return false;
    };
    let mut walk = parameters.walk();
    parameters.named_children(&mut walk).any(|parameter| {
        let Some(ty) = parameter.child_by_field_name("type") else {
            return false;
        };
        let text: String = cx.text(&ty).split_whitespace().collect();
        if let Some(inner) = text.strip_prefix("func(") {
            return inner.split_once('.').is_some_and(|(alias, rest)| {
                rest.starts_with("ResponseWriter")
                    && bindings.get(alias).is_some_and(|b| b.path == "net/http")
            });
        }
        text.split_once('.').is_some_and(|(alias, name)| {
            match bindings.get(alias).map(|b| b.path.as_str()) {
                Some("net/http") => matches!(name, "Handler" | "HandlerFunc"),
                Some("github.com/gin-gonic/gin") => name == "HandlerFunc",
                _ => false,
            }
        })
    })
}

/// What `function`'s body (nested func literals included) says about 401
/// and 403: its first response call writing one, and the repository values
/// its response calls pass. Whether it takes a handler is recorded too.
pub(super) fn denial(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    qualified: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<Denial> {
    let body = function.child_by_field_name("body")?;
    let wraps = takes_handler(cx, function, bindings);
    let params = response_params(cx, function, bindings);
    if params.is_empty() && !wraps {
        return None;
    }
    let mut stack = vec![body];
    let mut calls = Vec::new();
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() == "call_expression" {
            calls.push(node);
        }
    }
    calls.sort_by_key(|call| call.start_byte());
    let mut direct = None;
    let mut refs = Vec::new();
    for call in calls {
        let arguments = call_arguments(call);
        if !answers(cx, call, &arguments, &params, bindings) {
            continue;
        }
        if direct.is_none()
            && let Some(status) = arguments
                .iter()
                .find_map(|argument| deny_status(cx, *argument, bindings))
        {
            direct = Some((status, evidence(cx, call)));
        }
        for argument in arguments {
            if refs.len() < MAX_REFS
                && !params.contains_key(cx.text(&argument))
                && let Some((dir, name)) = value_ref(cx, argument, bindings)
            {
                refs.push((dir, name, evidence(cx, call)));
            }
        }
    }
    (wraps || direct.is_some() || !refs.is_empty()).then(|| Denial {
        package_dir: package_dir(cx.path),
        qualified: qualified.to_string(),
        direct,
        refs,
        wraps,
    })
}

/// What a middleware expression is: a call (`AuthRequired()`,
/// `middleware.BasicAuth("realm", creds)`) is read by its callee.
fn classify(
    cx: &FileCx<'_>,
    expression: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    receivers: &Receivers,
) -> Applied {
    let target = if expression.kind() == "call_expression" {
        expression
            .child_by_field_name("function")
            .unwrap_or(expression)
    } else {
        expression
    };
    let name = shorten(cx.text(&target));
    let kind = match target.kind() {
        "selector_expression" => {
            let operand = target.child_by_field_name("operand");
            let field = target
                .child_by_field_name("field")
                .map(|field| cx.text(&field).to_string());
            match (operand, field) {
                (Some(operand), Some(field)) if operand.kind() == "identifier" => {
                    let base = cx.text(&operand);
                    if let Some(binding) = bindings.get(base) {
                        let path = binding.path.as_str();
                        if let Some((_, _, guard)) =
                            GUARDS.iter().find(|(p, f, _)| *p == path && *f == field)
                        {
                            Kind::Guard((*guard).to_string(), format!("{path}.{field}"))
                        } else if NON_AUTH
                            .iter()
                            .any(|(p, names)| *p == path && names.contains(&field.as_str()))
                        {
                            Kind::NonAuth
                        } else if let Some(package_dir) = binding.package_dir.clone() {
                            Kind::Repo {
                                package_dir,
                                exported: field,
                            }
                        } else {
                            Kind::Unreadable
                        }
                    } else {
                        match enclosing_receiver(cx, target) {
                            Some((receiver, ty)) if receiver == base => Kind::Repo {
                                package_dir: package_dir(cx.path),
                                exported: format!("{ty}.{field}"),
                            },
                            _ => Kind::Unreadable,
                        }
                    }
                }
                _ => Kind::Unreadable,
            }
        }
        // A package-level function, unless a local of that name is visible.
        "identifier" if receivers.visible(&name, target.start_byte()).is_none() => Kind::Repo {
            package_dir: package_dir(cx.path),
            exported: name.clone(),
        },
        _ => Kind::Unreadable,
    };
    Applied {
        name,
        kind,
        site: evidence(cx, expression),
        wrapper: false,
    }
}

/// A `net/http` conversion, not a middleware: `http.HandlerFunc(h)`.
fn is_conversion(
    cx: &FileCx<'_>,
    callee: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    callee.kind() == "selector_expression"
        && callee.child_by_field_name("operand").is_some_and(|base| {
            bindings
                .get(cx.text(&base))
                .is_some_and(|b| b.path == "net/http")
        })
        && callee
            .child_by_field_name("field")
            .is_some_and(|field| matches!(cx.text(&field), "HandlerFunc" | "Handler"))
}

/// One `X.Use(...)` on a chi router or gin engine.
#[derive(Debug)]
pub(super) struct Use<'t> {
    receiver: (String, usize),
    framework: Framework,
    at: usize,
    arguments: Vec<TsNode<'t>>,
    /// Start of the innermost enclosing function, and whether the call is
    /// a statement directly in its body (so it runs before what follows).
    scope: Option<usize>,
    straight: bool,
}

/// The innermost function (declaration, method or func literal) around
/// `node`, by start byte.
fn enclosing_function(node: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut at = node.parent();
    while let Some(parent) = at {
        if matches!(
            parent.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) {
            return Some(parent);
        }
        at = parent.parent();
    }
    None
}

/// The receiver a call's `X.Member` names, as its visible declaration.
fn receiver_key(
    cx: &FileCx<'_>,
    operand: TsNode<'_>,
    receivers: &Receivers,
) -> Option<((String, usize), Framework)> {
    if operand.kind() != "identifier" {
        return None;
    }
    let name = cx.text(&operand).to_string();
    let (_, declaration) = receivers.visible(&name, operand.start_byte())?;
    let framework = declaration.framework?;
    Some(((name, declaration.at), framework))
}

/// Every `Use` call in the file on a proven chi router or gin engine.
pub(super) fn uses<'t>(cx: &FileCx<'_>, root: TsNode<'t>, receivers: &Receivers) -> Vec<Use<'t>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = node
            .child_by_field_name("function")
            .filter(|c| c.kind() == "selector_expression")
        else {
            continue;
        };
        if callee
            .child_by_field_name("field")
            .is_none_or(|field| cx.text(&field) != "Use")
        {
            continue;
        }
        let Some(operand) = callee.child_by_field_name("operand") else {
            continue;
        };
        if let Some((receiver, framework)) = receiver_key(cx, operand, receivers)
            && matches!(framework, Framework::Chi | Framework::Gin)
        {
            let function = enclosing_function(node);
            // `e.Use(m)` as a statement of the function body itself.
            let straight = function.is_some_and(|function| {
                let body = function.child_by_field_name("body");
                let statement = node.parent().filter(|p| p.kind() == "expression_statement");
                let list = statement.and_then(|s| s.parent());
                list.zip(body).is_some_and(|(list, body)| {
                    list.id() == body.id()
                        || (list.kind() == "statement_list"
                            && list.parent().is_some_and(|p| p.id() == body.id()))
                })
            });
            found.push(Use {
                receiver,
                framework,
                at: node.start_byte(),
                arguments: call_arguments(node),
                scope: function.map(|f| f.start_byte()),
                straight,
            });
        }
    }
    found.sort_by_key(|u| u.at);
    found
}

/// chi `r.With(a).With(b).Get`: the callee `r.Get` and the `With`
/// arguments, when `r` is a proven chi router. Anything else is the
/// callee as written.
pub(super) fn peel_with<'t>(
    cx: &FileCx<'_>,
    function: TsNode<'t>,
    receivers: &Receivers,
) -> (String, Vec<TsNode<'t>>) {
    let as_written = || (cx.text(&function).to_string(), Vec::new());
    if function.kind() != "selector_expression" {
        return as_written();
    }
    let (Some(mut operand), Some(field)) = (
        function.child_by_field_name("operand"),
        function.child_by_field_name("field"),
    ) else {
        return as_written();
    };
    let mut with = Vec::new();
    while operand.kind() == "call_expression" {
        let Some(inner) = operand
            .child_by_field_name("function")
            .filter(|f| f.kind() == "selector_expression")
        else {
            return as_written();
        };
        if inner
            .child_by_field_name("field")
            .is_none_or(|f| cx.text(&f) != "With")
        {
            return as_written();
        }
        with.extend(call_arguments(operand));
        let Some(next) = inner.child_by_field_name("operand") else {
            return as_written();
        };
        operand = next;
    }
    if with.is_empty() {
        return as_written();
    }
    match receiver_key(cx, operand, receivers) {
        Some((_, Framework::Chi)) => (format!("{}.{}", cx.text(&operand), cx.text(&field)), with),
        _ => as_written(),
    }
}

/// The middleware one registration applies: `Use` on its receiver, chi
/// `With`, then handler wrappers from the outside in.
#[allow(clippy::too_many_arguments)]
pub(super) fn registration(
    cx: &FileCx<'_>,
    endpoint: &str,
    call: TsNode<'_>,
    handler: TsNode<'_>,
    with: &[TsNode<'_>],
    uses: &[Use<'_>],
    bindings: &HashMap<String, ImportBinding>,
    receivers: &Receivers,
) -> Pending {
    let mut applied = Vec::new();
    let key = call.child_by_field_name("function").and_then(|function| {
        let mut operand = function.child_by_field_name("operand")?;
        // Through `With` calls to the router they are called on.
        while operand.kind() == "call_expression" {
            operand = operand
                .child_by_field_name("function")?
                .child_by_field_name("operand")?;
        }
        receiver_key(cx, operand, receivers)
    });
    if let Some((receiver, framework)) = key {
        let scope = enclosing_function(call).map(|f| f.start_byte());
        for found in uses.iter().filter(|u| u.receiver == receiver) {
            match framework {
                Framework::Chi => applied.extend(
                    found
                        .arguments
                        .iter()
                        .map(|m| classify(cx, *m, bindings, receivers)),
                ),
                // gin applies `Use` to routes registered after it at run
                // time: proven only for a straight-line `Use` earlier in
                // the same function.
                Framework::Gin if found.framework == Framework::Gin => {
                    let ordered = found.straight && found.scope == scope;
                    if ordered && found.at < call.start_byte() {
                        applied.extend(
                            found
                                .arguments
                                .iter()
                                .map(|m| classify(cx, *m, bindings, receivers)),
                        );
                    } else if !ordered {
                        applied.extend(found.arguments.iter().map(|m| {
                            let mut unordered = classify(cx, *m, bindings, receivers);
                            unordered.kind = Kind::Unordered;
                            unordered
                        }));
                    }
                }
                _ => {}
            }
        }
    }
    applied.extend(with.iter().map(|m| classify(cx, *m, bindings, receivers)));
    let mut wrapper = Some(handler);
    while let Some(node) = wrapper.filter(|n| n.kind() == "call_expression") {
        let Some(callee) = node.child_by_field_name("function") else {
            break;
        };
        if !is_conversion(cx, callee, bindings) {
            let mut wrapper = classify(cx, callee, bindings, receivers);
            // `m(args)(h)` applies the middleware `m(args)` returns; a plain
            // `f(x)` is middleware only if `f` takes a handler.
            wrapper.wrapper = callee.kind() != "call_expression";
            applied.push(wrapper);
        }
        wrapper = call_arguments(node)
            .into_iter()
            .rev()
            .find(|a| a.kind() == "call_expression");
    }
    Pending {
        endpoint: endpoint.to_string(),
        applied,
    }
}

/// Set `auth` on every Go route endpoint once all files are read, and add
/// a Gap for each middleware that cannot be read.
pub(super) fn resolve(
    out: &mut Extraction,
    repo: &str,
    symbols: &BTreeMap<(String, String), String>,
) {
    let values: BTreeMap<(&str, &str), &DenyValue> = out
        .deny_values
        .iter()
        .map(|v| ((v.package_dir.as_str(), v.name.as_str()), v))
        .collect();
    // Repository functions that take a handler, so can wrap one.
    let wrappers: BTreeSet<&String> = out
        .denials
        .iter()
        .filter(|d| d.wraps)
        .filter_map(|d| symbols.get(&(d.package_dir.clone(), d.qualified.clone())))
        .collect();
    // A function's denial: its own response call, else the first deny value
    // its calls pass, with the call and the value's declaration cited.
    let denials: BTreeMap<String, (u16, Vec<EvidenceRef>)> = out
        .denials
        .iter()
        .filter_map(|d| {
            let found = d
                .direct
                .clone()
                .map(|(status, site)| (status, vec![site]))
                .or_else(|| {
                    d.refs.iter().find_map(|(dir, name, site)| {
                        values
                            .get(&(dir.as_str(), name.as_str()))
                            .map(|value| (value.status, vec![site.clone(), value.evidence.clone()]))
                    })
                })?;
            symbols
                .get(&(d.package_dir.clone(), d.qualified.clone()))
                .map(|symbol| (symbol.clone(), found))
        })
        .collect();
    let mut by_endpoint: BTreeMap<&str, Vec<&Pending>> = BTreeMap::new();
    for pending in &out.pending_auth {
        by_endpoint
            .entry(pending.endpoint.as_str())
            .or_default()
            .push(pending);
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
        for registration in registrations {
            let mut guarded = false;
            for applied in &registration.applied {
                let unreadable = |reasons: &mut BTreeSet<String>,
                                  gaps: &mut BTreeMap<String, Node>,
                                  depends: &mut BTreeSet<(String, String)>,
                                  reason: &str| {
                    reasons.insert(format!("{reason}:{}", applied.name));
                    let id = format!(
                        "gap:go-middleware:{repo}@{}@{}",
                        applied.site.path, applied.site.byte_start
                    );
                    gaps.entry(id.clone()).or_insert_with(|| Node {
                        id: id.clone(),
                        label: "Gap".into(),
                        props: serde_json::json!({
                            "middleware": applied.name,
                            "reason": "unreadable Go middleware",
                            "detail": reason,
                            "attempted_tiers": ["T0"],
                            "prov": prov(vec![applied.site.clone()], ConfidenceTier::Gap, &id),
                        }),
                    });
                    depends.insert((endpoint.to_string(), id));
                };
                match &applied.kind {
                    Kind::Guard(guard, path) => {
                        guarded = true;
                        guards.insert(guard.clone());
                        basis.insert(format!("library guard {path}"));
                        sites.push(applied.site.clone());
                    }
                    Kind::NonAuth => {
                        non_auth.insert(applied.name.clone());
                    }
                    Kind::Repo {
                        package_dir,
                        exported,
                    } => match symbols.get(&(package_dir.clone(), exported.clone())) {
                        // A handler factory (`makeHandler(db)`), not middleware.
                        Some(symbol) if applied.wrapper && !wrappers.contains(symbol) => {}
                        Some(symbol) => match denials.get(symbol) {
                            Some((status, cited)) => {
                                guarded = true;
                                library_only = false;
                                guards.insert(symbol.clone());
                                basis.insert(format!("responds {status} in {exported}"));
                                sites.push(applied.site.clone());
                                sites.extend(cited.iter().cloned());
                            }
                            None => {
                                reasons.insert(format!("no_deny_status:{}", applied.name));
                            }
                        },
                        None => unreadable(
                            &mut reasons,
                            &mut gaps,
                            &mut depends,
                            "unresolved_middleware",
                        ),
                    },
                    Kind::Unordered => {
                        reasons.insert(format!("unordered_middleware:{}", applied.name));
                    }
                    Kind::Unreadable => unreadable(
                        &mut reasons,
                        &mut gaps,
                        &mut depends,
                        "unreadable_middleware",
                    ),
                }
            }
            every_guarded &= guarded;
        }
        let value = if every_guarded && !guards.is_empty() {
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
            let mut value = serde_json::json!({
                "state": "required",
                "authenticated": true,
                "guards": guards.into_iter().collect::<Vec<_>>(),
                "basis": basis.into_iter().collect::<Vec<_>>(),
                "prov": prov(sites, confidence, &format!("auth {endpoint}")),
            });
            if !non_auth.is_empty() {
                value["non_auth_middleware"] = non_auth.into_iter().collect::<Vec<_>>().into();
            }
            value
        } else {
            reasons.insert("no_guard_recovered".into());
            let mut value = serde_json::json!({
                "state": "unknown",
                "reasons": reasons.into_iter().collect::<Vec<_>>(),
            });
            if !non_auth.is_empty() {
                value["non_auth_middleware"] = non_auth.into_iter().collect::<Vec<_>>().into();
            }
            value
        };
        auth.insert(endpoint.to_string(), value);
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
                "reason": "unreadable Go middleware",
                "prov": site,
            }),
        });
    }
    out.nodes.extend(gaps.into_values());
    out.pending_auth.clear();
    out.denials.clear();
    out.deny_values.clear();
}

/// [`resolve`] for one file read on its own: its Symbols are the index.
pub(super) fn resolve_file(out: &mut Extraction, repo: &str) {
    let symbols = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .filter_map(|node| {
            Some((
                (
                    node.props["package_dir"].as_str()?.to_string(),
                    node.props["name"].as_str()?.to_string(),
                ),
                node.id.clone(),
            ))
        })
        .collect();
    resolve(out, repo, &symbols);
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
