//! Flask authentication guards on Endpoint nodes (#1128, AC-0380):
//! `props.auth` in one of three states, computed after every file is read.
//!
//! - `required`: a non-route decorator is a framework guard
//!   (`flask_login.login_required` / `fresh_login_required` from
//!   `flask_login` or `flask_login.utils`, used bare; or
//!   `flask_jwt_extended.jwt_required` as a call with no arguments), or a
//!   bare repository decorator whose followed wrapper responses (see
//!   `responses`) contain status 401 or 403, or whose wrapper (or a check
//!   it returns the result of) calls Flask's `abort` with a literal 401 or
//!   403 in its own scope (#1166). The local name of a framework
//!   guard must not be rebound in the file or in the handler.
//! - `none`: the handler has no non-route decorator, every
//!   `before_request` / `before_app_request` hook in the repository is
//!   non-blocking, and every `wsgi_app` assignment wraps it in werkzeug's
//!   `ProxyFix`. A hook is non-blocking when walking all descendants of
//!   its body finds no `return` of a value other than the literal `None`
//!   or a `(body, <status>)` pair with a literal 429 or 5xx status other
//!   than 511, Network Authentication Required (#1317,
//!   recorded as `request_hook_statuses_do_not_authenticate`; Flask applies
//!   the pair's status over the body's), no `raise`, and no `abort` call, and every repository function it
//!   calls is itself non-blocking (followed at most four deep; deeper or
//!   a cycle is blocking). A called function's own `return` of a value
//!   does not block: only the hook's return value ends the request, and a
//!   hook that `yield`s returns a generator, which is a value. Calls that do not resolve to a repository
//!   function are assumed not to abort; that assumption is recorded.
//! - `unknown`: anything else, with sorted reasons and no
//!   `authenticated` key: an unclassified or not-classified decorator, a
//!   hook that may block, or an unknown `wsgi_app` middleware. A hook that
//!   may block is listed in `blocking_hooks` by its recovered Symbol
//!   (`null` when the registered argument does not resolve to exactly one)
//!   and its registration span: the decorator, or the registering call.
//!   Past the evidence bound, `blocking_hooks_omitted` counts the rest.
//!
//! A hook reaches only the endpoints that may be served by the application
//! it is registered on (see `app_scope`, #1149); a `none` fact that one
//! excluded records `request_hooks_reach_only_their_application`.
//!
//! Only decorators below every route decorator wrap the registered view.
//! One above every route decorator is applied after registration and never
//! runs for the route, so it is listed in `ineffective_decorators` and not
//! classified; one between two route decorators wraps only some
//! registrations and makes the state `unknown`.
//!
//! A `jwt_required` call with any argument, a call decorator such as
//! `@limiter.limit("2/minute")`, and a hook registered by a call whose
//! argument does not resolve to a repository function are never guards.
//! An import-proven flask-cors `cross_origin(…)`, or `limit(…)` /
//! `shared_limit(…)` on a proven module-level flask-limiter `Limiter`, with
//! inert arguments (literals, or a `lambda` calling only `hasattr`,
//! `getattr` or `isinstance`), is non-authenticating: it is listed in
//! `non_auth_decorators` instead of making the state `unknown` (#1157). A
//! hook's call into a repository file where no function of that name was
//! recovered (a re-export, a class) is blocking.
//! A function carrying any decorator other than a hook registration is
//! blocking: the registered or called object is the wrapper, not its body.
//! An imported guard or `ProxyFix` name counts only when neither the module
//! nor any function or class scope enclosing its use rebinds it.
//! A framework guard reassigned on its module anywhere in the repository
//! (`flask_login.login_required = …`, or `setattr` on its module, where a
//! name that is not a plain literal string, or holds an escape, may be any
//! of that module's guards) is
//! never a guard: the reason is `framework_guard_reassigned:<name>`.
//! `ProxyFix` is known non-auth only when it wraps the assigned attribute
//! itself (`app.wsgi_app = ProxyFix(app.wsgi_app, …)`); the assignment is
//! cited on a `none` fact.
//!
//! A repository decorator that is not a guard, or a `@factory(…)` call with
//! inert arguments whose factory returns a class-based decorator, is
//! non-authenticating when `decorator_proof` proves its wrapper ends a
//! request only through the wrapped view, `None` or a non-401/403 werkzeug
//! error, and every repository function it names is deny-free: it raises
//! only such errors, never aborts or calls into an authentication library,
//! is undecorated, and names only deny-free functions (at most four deep)
//! (#1173). It is listed in
//! `non_auth_decorators`; the `none` fact records that non-repository calls
//! in it do not abort and that those errors keep their status. The proof is
//! void once the decorator, factory or class name is written elsewhere, or
//! `werkzeug.exceptions` or `functools.wraps` is replaced. An unproven
//! factory stays `unclassified_decorator`.
use crate::app_scope::{self, AppRef, Apps};
use crate::decorator_proof::{self, WrapperProof};
use crate::request_inputs::{binding_sites, handler_bindings};
use crate::responses::{self, FnKey, Resolver};
use crate::{
    Extraction, FileCx, ImportBinding, module_binding, prov_from, route_decorator, scope_binding,
    symbol_id,
};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeSet, HashMap, HashSet};
use tree_sitter::Node as TsNode;

/// Evidence spans kept per endpoint, as in `responses`.
const MAX_EVIDENCE: usize = 64;

/// Repository calls followed from a request hook before giving up.
const MAX_HOOK_DEPTH: usize = 4;

/// One non-route decorator of a handler, as read in its file.
#[derive(Debug, Clone)]
pub(crate) enum DecoratorFact {
    /// An import-proven framework guard, by canonical name.
    Framework { name: String, evidence: EvidenceRef },
    /// A bare decorator naming a repository function; `text` is its
    /// source text, for the unclassified reason when resolve finds no
    /// such recovered function.
    Repository {
        key: FnKey,
        evidence: EvidenceRef,
        text: String,
    },
    /// A decorator that never authenticates: an import-proven
    /// `flask_cors.cross_origin(…)` with inert arguments (#1157).
    NonAuth { text: String, evidence: EvidenceRef },
    /// `@factory(…)` with inert arguments, `key` naming a repository
    /// function: non-authenticating once resolve finds its proof. `package`
    /// is `(package module file, name)` when `key` reads
    /// `from package import name` as a submodule, which holds only when the
    /// package binds no such name itself.
    Factory {
        key: FnKey,
        package: Option<FnKey>,
        text: String,
        evidence: EvidenceRef,
    },
    /// `<name>.limit(…)` / `.shared_limit(…)` with inert arguments: non-
    /// authenticating once resolve proves `key` a flask-limiter `Limiter`.
    Limit {
        key: (String, String),
        text: String,
        evidence: EvidenceRef,
    },
    /// Anything else, with its `unknown` reason.
    Unclassified { reason: String },
}

/// Auth facts of one route handler.
#[derive(Debug, Clone)]
pub(crate) struct HandlerAuthFacts {
    pub handler: String,
    pub route_evidence: Vec<EvidenceRef>,
    /// Decorators below every route decorator, which wrap the registered view.
    pub decorators: Vec<DecoratorFact>,
    /// Decorators above every route decorator: applied after registration,
    /// so they never run for the route.
    pub ineffective: Vec<String>,
}

/// Whether one function can block a request, as its own body proves.
/// `blocks` is a `raise`, an `abort` or a wrapping decorator, which stop a
/// request from any depth; `returns_value` stops it only when the function
/// is the hook itself, since a callee's return value is the hook's to use.
#[derive(Debug, Clone)]
pub(crate) struct FnEffects {
    pub key: FnKey,
    /// The recovered Symbol, `None` once two functions share `key`.
    pub symbol: Option<String>,
    pub blocks: bool,
    pub returns_value: bool,
    /// Returns a `(body, 429 | 5xx)` pair, which ends a request without
    /// authenticating it (#1317).
    pub status_returns: bool,
    pub calls: Vec<FnKey>,
    /// It may end a request with 401/403 when called: it aborts, raises
    /// anything but a non-auth werkzeug error, or is decorated (#1173).
    pub denies: bool,
    /// Every repository function it names, called or not.
    pub refs: Vec<FnKey>,
    /// What it proves as a non-authenticating decorator or factory.
    pub proof: Option<WrapperProof>,
    pub evidence: EvidenceRef,
}

/// One `before_request` / `before_app_request` registration.
#[derive(Debug, Clone)]
pub(crate) enum HookFact {
    /// The hook is this repository function, registered at `evidence`;
    /// `symbol` is the decorated function's own Symbol when known.
    Resolved {
        key: FnKey,
        symbol: Option<String>,
        app: AppRef,
        evidence: EvidenceRef,
    },
    /// Registered at `evidence` by a call whose argument does not
    /// resolve: blocking.
    Unresolved { app: AppRef, evidence: EvidenceRef },
}

/// One assignment to a `wsgi_app` attribute.
#[derive(Debug, Clone)]
pub(crate) struct WsgiFact {
    pub proxy_fix: bool,
    pub evidence: EvidenceRef,
}

pub(crate) fn named(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Every node under `node`, nested scopes included.
pub(crate) fn every_descendant<'t>(node: TsNode<'t>, f: &mut dyn FnMut(TsNode<'t>)) {
    f(node);
    for child in named(node) {
        every_descendant(child, f);
    }
}

pub(crate) fn unparenthesize(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "parenthesized_expression"
        && node.named_child_count() == 1
        && let Some(inner) = node.named_child(0)
    {
        node = inner;
    }
    node
}

/// The dotted parts of an identifier / attribute chain.
fn dotted_parts(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![cx.text(&node).to_string()]),
        "attribute" => {
            let mut parts = dotted_parts(cx, node.child_by_field_name("object")?)?;
            parts.push(cx.text(&node.child_by_field_name("attribute")?).to_string());
            Some(parts)
        }
        _ => None,
    }
}

/// The first name of a dotted `node`, as `rebound` checks it.
pub(crate) fn dotted_root(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    dotted_parts(cx, node)?.into_iter().next()
}

/// The full dotted name an identifier / attribute chain names through
/// the file's import bindings.
pub(crate) fn import_dotted(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<String> {
    let parts = dotted_parts(cx, node)?;
    let root = parts.first()?;
    match bindings.get(root)? {
        ImportBinding::Module { module } => {
            if module.split('.').next() == Some(root.as_str()) {
                // `import a.b` binds `a`; the chain is as written.
                Some(parts.join("."))
            } else if parts.len() == 1 {
                Some(module.clone())
            } else {
                Some(format!("{}.{}", module, parts[1..].join(".")))
            }
        }
        ImportBinding::Symbol {
            module, exported, ..
        } => {
            let base = if module.is_empty() {
                exported.clone()
            } else {
                format!("{module}.{exported}")
            };
            if parts.len() == 1 {
                Some(base)
            } else {
                Some(format!("{}.{}", base, parts[1..].join(".")))
            }
        }
    }
}

/// Whether the file rebinds `name` after its import, or any function or
/// class enclosing `scope` (the handler itself included) binds it, so the
/// name at `scope` may not be the imported one.
pub(crate) fn rebound(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    name: &str,
    scope: Option<TsNode<'_>>,
) -> bool {
    if module_binding(cx, root, name).writes.len() > 1 {
        return true;
    }
    std::iter::successors(scope, |node| node.parent()).any(|node| {
        let Some(body) = node.child_by_field_name("body") else {
            return false;
        };
        match node.kind() {
            // `scope_binding` also sees `match` captures and other forms
            // `handler_bindings` does not name.
            "function_definition" => {
                handler_bindings(cx, node, body).contains(name)
                    || !scope_binding(cx, node, name).writes.is_empty()
            }
            "class_definition" => {
                let mut bound = false;
                binding_sites(cx, body, &mut |_, names| bound |= names.contains(name));
                bound
            }
            _ => false,
        }
    })
}

/// The canonical guard name of a `flask_login` dotted name.
fn login_guard(dotted: &str) -> Option<String> {
    match dotted {
        "flask_login.login_required" | "flask_login.utils.login_required" => {
            Some("flask_login.login_required".into())
        }
        "flask_login.fresh_login_required" | "flask_login.utils.fresh_login_required" => {
            Some("flask_login.fresh_login_required".into())
        }
        _ => None,
    }
}

/// Source text truncated to 80 bytes, on a char boundary.
fn truncated(text: &str) -> String {
    if text.len() <= 80 {
        return text.to_string();
    }
    let mut end = 80;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Whether a call has no argument other than a comment.
fn no_arguments(call: TsNode<'_>) -> bool {
    call.child_by_field_name("arguments")
        .is_some_and(|arguments| {
            named(arguments)
                .iter()
                .all(|argument| argument.kind() == "comment")
        })
}

/// Classify one non-route `decorator` of `handler`.
#[allow(clippy::too_many_arguments)]
fn classify(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    decorator: TsNode<'_>,
    handler: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    resolver: &Resolver<'_>,
) -> DecoratorFact {
    let Some(expression) = decorator.named_child(0) else {
        return DecoratorFact::Unclassified {
            reason: format!("unclassified_decorator:{}", truncated(cx.text(&decorator))),
        };
    };
    let expression = unparenthesize(expression);
    let unclassified = || DecoratorFact::Unclassified {
        reason: format!("unclassified_decorator:{}", truncated(cx.text(&expression))),
    };
    let evidence = cx.evidence(&decorator);
    match expression.kind() {
        "call" => {
            let callee = expression.child_by_field_name("function");
            let guard = callee
                .filter(|_| no_arguments(expression))
                .and_then(|callee| {
                    let dotted = import_dotted(cx, callee, bindings)?;
                    let root_name = dotted_parts(cx, callee)?.into_iter().next()?;
                    (dotted == "flask_jwt_extended.jwt_required"
                        && !rebound(cx, root, &root_name, Some(handler)))
                    .then(|| "flask_jwt_extended.jwt_required".to_string())
                });
            if let Some(name) = guard {
                return DecoratorFact::Framework { name, evidence };
            }
            let text = truncated(cx.text(&expression));
            match callee.filter(|_| inert_arguments(cx, root, expression)) {
                Some(callee) if cors_decorator(cx, root, callee, handler, bindings) => {
                    DecoratorFact::NonAuth { text, evidence }
                }
                Some(callee) => match limit_receiver(cx, callee, bindings) {
                    Some(key) => DecoratorFact::Limit {
                        key,
                        text,
                        evidence,
                    },
                    None => match resolver.target(cx, callee, &BTreeSet::new()) {
                        Some(key) => DecoratorFact::Factory {
                            key,
                            package: None,
                            text,
                            evidence,
                        },
                        None => match submodule_member(cx, callee, bindings) {
                            Some((key, package)) => DecoratorFact::Factory {
                                key,
                                package: Some(package),
                                text,
                                evidence,
                            },
                            None => unclassified(),
                        },
                    },
                },
                None => unclassified(),
            }
        }
        "identifier" | "attribute" => {
            let guard = import_dotted(cx, expression, bindings)
                .and_then(|dotted| login_guard(&dotted))
                .filter(|_| {
                    dotted_parts(cx, expression)
                        .and_then(|parts| parts.into_iter().next())
                        .is_some_and(|name| !rebound(cx, root, &name, Some(handler)))
                });
            if let Some(name) = guard {
                return DecoratorFact::Framework { name, evidence };
            }
            match resolver.target(cx, expression, &BTreeSet::new()) {
                Some(key) => DecoratorFact::Repository {
                    key,
                    evidence,
                    text: truncated(cx.text(&expression)),
                },
                None => unclassified(),
            }
        }
        _ => unclassified(),
    }
}

/// `name.member` where `from package import name` may import submodule
/// `package.name`: its member's key, and `(package module file, name)`.
fn submodule_member(
    cx: &FileCx<'_>,
    callee: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(FnKey, FnKey)> {
    let object = callee.child_by_field_name("object")?;
    let member = callee.child_by_field_name("attribute")?;
    if callee.kind() != "attribute" || object.kind() != "identifier" {
        return None;
    }
    let ImportBinding::Symbol {
        module, exported, ..
    } = bindings.get(cx.text(&object))?
    else {
        return None;
    };
    let submodule = if module.chars().all(|ch| ch == '.') {
        format!("{module}{exported}")
    } else {
        format!("{module}.{exported}")
    };
    Some((
        (
            crate::module_file(cx.path, &submodule)?,
            cx.text(&member).to_string(),
        ),
        (crate::module_file(cx.path, module)?, exported.clone()),
    ))
}

/// Whether call decorator `callee` is flask-cors's `cross_origin`, its local
/// name not rebound.
fn cors_decorator(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    callee: TsNode<'_>,
    handler: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    import_dotted(cx, callee, bindings).is_some_and(|dotted| {
        matches!(
            dotted.as_str(),
            "flask_cors.cross_origin" | "flask_cors.decorator.cross_origin"
        )
    }) && dotted_parts(cx, callee)
        .and_then(|parts| parts.into_iter().next())
        .is_some_and(|name| !rebound(cx, root, &name, Some(handler)))
}

/// The definition key of `<name>` in `<name>.limit` / `<name>.shared_limit`,
/// a module-level name not rebound where it is read.
fn limit_receiver(
    cx: &FileCx<'_>,
    callee: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(String, String)> {
    let attribute = callee.child_by_field_name("attribute")?;
    if callee.kind() != "attribute" || !matches!(cx.text(&attribute), "limit" | "shared_limit") {
        return None;
    }
    match app_scope::app_ref(cx, callee.child_by_field_name("object")?, bindings) {
        AppRef::Def(key) => Some(key),
        _ => None,
    }
}

/// Builtins an inert `lambda` may call.
const INERT_BUILTINS: [&str; 3] = ["hasattr", "getattr", "isinstance"];

/// Whether every argument of `call` is a literal, a container of literals,
/// or a `lambda` that calls nothing but unbound `INERT_BUILTINS`.
fn inert_arguments(cx: &FileCx<'_>, root: TsNode<'_>, call: TsNode<'_>) -> bool {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return false;
    };
    arguments.kind() == "argument_list"
        && named(arguments)
            .into_iter()
            .all(|argument| match argument.kind() {
                "comment" => true,
                "keyword_argument" => argument
                    .child_by_field_name("value")
                    .is_some_and(|value| inert(cx, root, value)),
                _ => inert(cx, root, argument),
            })
}

pub(crate) fn inert(cx: &FileCx<'_>, root: TsNode<'_>, value: TsNode<'_>) -> bool {
    match value.kind() {
        "integer" | "float" | "true" | "false" | "none" => true,
        "string" | "concatenated_string" => {
            let mut interpolated = false;
            every_descendant(value, &mut |node| {
                interpolated |= node.kind() == "interpolation";
            });
            !interpolated
        }
        "unary_operator"
        | "parenthesized_expression"
        | "list"
        | "tuple"
        | "set"
        | "dictionary"
        | "pair" => named(value)
            .into_iter()
            .filter(|child| child.kind() != "comment")
            .all(|child| inert(cx, root, child)),
        "lambda" => {
            let local = lambda_bindings(cx, value);
            let mut calls_only_builtins = true;
            every_descendant(value, &mut |node| {
                if node.kind() != "call" {
                    return;
                }
                calls_only_builtins &= node.child_by_field_name("function").is_some_and(|callee| {
                    let name = cx.text(&callee);
                    callee.kind() == "identifier"
                        && INERT_BUILTINS.contains(&name)
                        && !local.contains(name)
                        && module_binding(cx, root, name).writes.is_empty()
                        && !rebound(cx, root, name, Some(value))
                });
            });
            calls_only_builtins
        }
        _ => false,
    }
}

/// Every name `lambda` binds anywhere inside it: its own and nested
/// lambdas' parameters (defaults included, conservatively), comprehension
/// targets and `:=` names.
fn lambda_bindings(cx: &FileCx<'_>, lambda: TsNode<'_>) -> HashSet<String> {
    let mut targets = Vec::new();
    every_descendant(lambda, &mut |node| match node.kind() {
        "lambda" => targets.extend(node.child_by_field_name("parameters")),
        "for_in_clause" => targets.extend(node.child_by_field_name("left")),
        "named_expression" => targets.extend(node.child_by_field_name("name")),
        _ => {}
    });
    let mut names = HashSet::new();
    for target in targets {
        every_descendant(target, &mut |node| {
            if node.kind() == "identifier" {
                names.insert(cx.text(&node).to_string());
            }
        });
    }
    names
}

/// Whether `decorator` is a route decorator: `route_decorator` also
/// matches a call such as `@limiter.limit("2/minute")`, whose verb no
/// framework routes on (`methods_for` in the crate root).
pub(crate) fn is_route_decorator(cx: &FileCx<'_>, decorator: TsNode<'_>) -> bool {
    route_decorator(cx, decorator).is_some_and(|route| {
        matches!(
            route.verb.as_str(),
            "get" | "post" | "put" | "delete" | "patch" | "options" | "head" | "route"
        )
    })
}

/// Auth facts of a decorated function, when it is a route handler.
#[allow(clippy::too_many_arguments)]
fn handler_facts(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    parent: TsNode<'_>,
    function: TsNode<'_>,
    handler: &str,
    bindings: &HashMap<String, ImportBinding>,
    resolver: &Resolver<'_>,
) -> Option<HandlerAuthFacts> {
    let all = named(parent)
        .into_iter()
        .filter(|child| child.kind() == "decorator")
        .collect::<Vec<_>>();
    let routes = all
        .iter()
        .enumerate()
        .filter(|(_, decorator)| is_route_decorator(cx, **decorator))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let (Some(&first), Some(&last)) = (routes.first(), routes.last()) else {
        return None;
    };
    let mut route_evidence = Vec::new();
    let mut decorators = Vec::new();
    let mut ineffective = Vec::new();
    for (index, decorator) in all.into_iter().enumerate() {
        if routes.contains(&index) {
            route_evidence.push(cx.evidence(&decorator));
        } else if index < first {
            // Applied after registration: the registered view never runs it.
            ineffective.push(truncated(cx.text(&decorator)));
        } else if index < last {
            decorators.push(DecoratorFact::Unclassified {
                reason: format!(
                    "decorator_between_route_decorators:{}",
                    truncated(cx.text(&decorator))
                ),
            });
        } else {
            decorators.push(classify(cx, root, decorator, function, bindings, resolver));
        }
    }
    Some(HandlerAuthFacts {
        handler: handler.to_string(),
        route_evidence,
        decorators,
        ineffective,
    })
}

/// What `function` proves about blocking a request, by its own body.
fn fn_effects(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    symbol: &str,
    resolver: &Resolver<'_>,
) -> Option<FnEffects> {
    let name = cx.text(&function.child_by_field_name("name")?).to_string();
    let body = function.child_by_field_name("body")?;
    let bound = handler_bindings(cx, function, body);
    // The registered or called object is the decorated wrapper, whose body
    // is not this one: any decorator other than a hook registration may block.
    let mut blocks = function
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
        .is_some_and(|parent| {
            named(parent)
                .into_iter()
                .filter(|child| child.kind() == "decorator")
                .filter_map(|decorator| decorator.named_child(0).map(unparenthesize))
                .any(|expression| !is_hook_attribute(cx, expression))
        });
    let root = std::iter::successors(Some(function), |node| node.parent()).last()?;
    let mut denies = function
        .parent()
        .is_some_and(|parent| parent.kind() == "decorated_definition");
    let mut returns_value = false;
    let mut status_returns = false;
    let mut calls = BTreeSet::new();
    let mut refs = BTreeSet::new();
    every_descendant(body, &mut |node| match node.kind() {
        "return_statement" => {
            if let Some(value) = node.named_child(0).map(unparenthesize)
                && value.kind() != "none"
            {
                if non_auth_status_pair(cx, value) {
                    status_returns = true;
                } else {
                    returns_value = true;
                }
            }
        }
        "raise_statement" => {
            blocks = true;
            denies |= !decorator_proof::allowed_raise(cx, root, node, resolver);
        }
        "identifier" | "attribute" => {
            let field = node.parent().is_some_and(|parent| {
                parent.kind() == "attribute"
                    && parent.child_by_field_name("attribute") == Some(node)
            });
            if !field && let Some(key) = resolver.target(cx, node, &bound) {
                refs.insert(key);
            }
        }
        // A generator function returns a generator object, never `None`.
        "yield" => returns_value = true,
        "call" => {
            let Some(callee) = node.child_by_field_name("function") else {
                return;
            };
            let abort = match callee.kind() {
                "identifier" => cx.text(&callee) == "abort",
                "attribute" => callee
                    .child_by_field_name("attribute")
                    .is_some_and(|attribute| cx.text(&attribute) == "abort"),
                _ => false,
            };
            denies |= decorator_proof::denying_call(cx, callee, resolver);
            if abort {
                blocks = true;
                denies = true;
            } else if let Some(key) = resolver.target(cx, callee, &bound) {
                calls.insert(key);
            }
        }
        _ => {}
    });
    Some(FnEffects {
        key: (cx.path.to_string(), name),
        symbol: Some(symbol.to_string()),
        blocks,
        returns_value,
        status_returns,
        calls: calls.into_iter().collect(),
        denies,
        refs: refs.into_iter().collect(),
        proof: decorator_proof::proof(cx, root, function, resolver),
        evidence: cx.evidence(&function),
    })
}

/// Whether `value` is a `(body, <status>)` pair whose literal status is 429
/// or 5xx other than 511: never a redirect, a credential challenge, a hidden resource
/// (404) or a page served in the view's place (2xx). A third element may
/// carry headers such as `WWW-Authenticate`, so it is not one.
fn non_auth_status_pair(cx: &FileCx<'_>, value: TsNode<'_>) -> bool {
    if !matches!(value.kind(), "tuple" | "expression_list") {
        return false;
    }
    let parts = named(value)
        .into_iter()
        .filter(|part| part.kind() != "comment")
        .collect::<Vec<_>>();
    let [_, status] = parts.as_slice() else {
        return false;
    };
    status.kind() == "integer"
        && cx
            .text(status)
            .parse::<u16>()
            // 511 is Network Authentication Required (RFC 6585).
            .is_ok_and(|status| status == 429 || ((500..=599).contains(&status) && status != 511))
}

/// Whether `callee` is `<receiver>.before_request` (or the `before_app`
/// spelling), on any receiver.
fn is_hook_attribute(cx: &FileCx<'_>, callee: TsNode<'_>) -> bool {
    callee.kind() == "attribute"
        && callee
            .child_by_field_name("attribute")
            .is_some_and(|attribute| {
                matches!(cx.text(&attribute), "before_request" | "before_app_request")
            })
}

/// What the receiver of hook attribute `callee` may be (#1149).
fn hook_app(
    cx: &FileCx<'_>,
    callee: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> AppRef {
    callee
        .child_by_field_name("object")
        .map_or(AppRef::Any, |object| {
            app_scope::app_ref(cx, object, bindings)
        })
}

/// Whether `left = right` wraps `left` itself, the application's current
/// `wsgi_app`, in werkzeug's `ProxyFix`: `app.wsgi_app = ProxyFix(app.wsgi_app, …)`.
/// Any other wrapped application may be middleware of its own.
fn is_proxy_fix(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    left: TsNode<'_>,
    right: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    let right = unparenthesize(right);
    let Some(callee) = (right.kind() == "call")
        .then(|| right.child_by_field_name("function"))
        .flatten()
    else {
        return false;
    };
    let known = matches!(
        import_dotted(cx, callee, bindings).as_deref(),
        Some("werkzeug.middleware.proxy_fix.ProxyFix" | "werkzeug.contrib.fixers.ProxyFix")
    );
    let wraps_itself = right
        .child_by_field_name("arguments")
        .and_then(|arguments| {
            named(arguments)
                .into_iter()
                .find(|argument| argument.kind() != "comment")
        })
        .map(unparenthesize)
        .is_some_and(|wrapped| {
            wrapped.kind() == "attribute"
                && dotted_parts(cx, wrapped).is_some()
                && dotted_parts(cx, wrapped) == dotted_parts(cx, left)
        });
    known
        && wraps_itself
        && dotted_parts(cx, callee)
            .and_then(|parts| parts.into_iter().next())
            .is_some_and(|name| !rebound(cx, root, &name, Some(right)))
}

/// The canonical framework guard name of a dotted module member.
fn framework_guard(dotted: &str) -> Option<String> {
    login_guard(dotted)
        .or_else(|| (dotted == "flask_jwt_extended.jwt_required").then(|| dotted.to_string()))
}

/// The canonical name of a dotted module member this module trusts: a
/// framework guard, flask-cors's `cross_origin`, or flask-limiter's
/// `Limiter` and its decorator methods.
fn trusted_member(dotted: &str) -> Option<String> {
    framework_guard(dotted).or_else(|| match dotted {
        "flask_cors.cross_origin"
        | "flask_cors.decorator"
        | "flask_cors.decorator.cross_origin" => Some(CORS.into()),
        "flask_limiter.extension" => Some(LIMITER.into()),
        "flask.abort" | "flask.helpers" | "flask.helpers.abort" => Some(ABORT.into()),
        "functools.wraps" => Some(WRAPS.into()),
        _ if dotted == "werkzeug.exceptions" || dotted.starts_with("werkzeug.exceptions.") => {
            Some(WERKZEUG_ERRORS.into())
        }
        "flask_login.current_user"
        | "flask_login.utils"
        | "flask_login.utils.current_user"
        | "flask_login.utils._get_user" => Some(CURRENT_USER.into()),
        "flask.redirect"
        | "flask.helpers.redirect"
        | "flask.make_response"
        | "flask.helpers.make_response"
        | "flask.render_template"
        | "flask.templating"
        | "flask.templating.render_template" => Some(RESPONSE.into()),
        _ => ["flask_limiter.Limiter", "flask_limiter.extension.Limiter"]
            .into_iter()
            .any(|class| {
                dotted == class
                    || dotted
                        .strip_prefix(class)
                        .is_some_and(|rest| matches!(rest, ".limit" | ".shared_limit"))
            })
            .then(|| LIMITER.into()),
    })
}

/// Canonical trusted names a write may replace.
const CORS: &str = "flask_cors.cross_origin";
const LIMITER: &str = "flask_limiter.Limiter";
pub(crate) const ABORT: &str = "flask.abort";
const WRAPS: &str = "functools.wraps";
const WERKZEUG_ERRORS: &str = "werkzeug.exceptions";
/// `flask_login.current_user`, or what it reads, replaced.
pub(crate) const CURRENT_USER: &str = "flask_login.current_user";
/// A Flask response helper other than `abort` replaced.
pub(crate) const RESPONSE: &str = "flask.redirect";

/// Record what binding target `target` writes: every trusted module member
/// it reassigns, as in `flask_login.login_required = passthrough` (also in
/// a tuple, a loop, `with … as` or `del`), and every name an attribute write
/// goes through, as in `limiter.limit = wrapper`.
fn target_writes(
    cx: &FileCx<'_>,
    target: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let written = |object: TsNode<'_>, out: &mut Extraction| {
        every_descendant(object, &mut |part| {
            if part.kind() == "identifier" {
                out.attribute_write_objects
                    .insert(cx.text(&part).to_string());
            }
        });
    };
    match target.kind() {
        // Only the member written is replaced: `flask_limiter.extension.C.X`
        // read inside a subscript index replaces nothing.
        "attribute" => {
            if let Some(member) =
                import_dotted(cx, target, bindings).and_then(|dotted| trusted_member(&dotted))
            {
                out.patched_guards.push(member);
            }
            if let Some(object) = target.child_by_field_name("object") {
                written(object, out);
            }
            if let Some(attribute) = target.child_by_field_name("attribute") {
                out.written_attributes
                    .insert(cx.text(&attribute).to_string());
            }
        }
        "subscript" => {
            if let Some(object) = target.child_by_field_name("value") {
                written(object, out);
            }
        }
        _ => {
            for child in named(target) {
                target_writes(cx, child, bindings, out);
            }
        }
    }
}

/// Trusted members a `setattr(<module>, <name>, …)` may replace: the named
/// one for a literal name, and every trusted member of that module when the
/// name is not a literal.
fn setattr_guards(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    callee: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Vec<String> {
    if callee.kind() != "identifier" || cx.text(&callee) != "setattr" {
        return Vec::new();
    }
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let arguments = named(arguments)
        .into_iter()
        .filter(|argument| argument.kind() != "comment")
        .map(unparenthesize)
        .collect::<Vec<_>>();
    let (Some(object), Some(name)) = (arguments.first(), arguments.get(1)) else {
        return Vec::new();
    };
    let Some(module) = import_dotted(cx, *object, bindings) else {
        return Vec::new();
    };
    // An escape is not decoded here, so it may spell any member name.
    match crate::literal_string(cx, *name).filter(|member| !member.contains('\\')) {
        Some(member) => trusted_member(&format!("{module}.{member}"))
            .into_iter()
            .collect(),
        None => [
            "login_required",
            "fresh_login_required",
            "jwt_required",
            "cross_origin",
            "decorator",
            "extension",
            "Limiter",
            "limit",
            "shared_limit",
            "abort",
            "helpers",
            "current_user",
            "utils",
            "_get_user",
            "redirect",
            "make_response",
            "render_template",
            "templating",
            "wraps",
            "exceptions",
        ]
        .into_iter()
        .filter_map(|member| trusted_member(&format!("{module}.{member}")))
        .collect(),
    }
}

/// Collect this file's handler, function-effect, hook and `wsgi_app`
/// facts. `functions` is every function definition with its symbol id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    resolver: &Resolver<'_>,
    locals: &HashMap<String, String>,
    functions: &[(TsNode<'_>, String)],
    out: &mut Extraction,
) {
    if cx.path == "__init__.py" || cx.path.ends_with("/__init__.py") {
        binding_sites(cx, root, &mut |_, names| {
            for name in names {
                out.package_names
                    .insert((cx.path.to_string(), name.clone()));
            }
        });
        every_descendant(root, &mut |node| {
            if node.kind() == "wildcard_import" {
                out.package_names
                    .insert((cx.path.to_string(), "*".to_string()));
            }
        });
    }
    // Every member a file reaches by name, so a factory's class that is
    // imported, read as an attribute or fetched by a literal `getattr`
    // elsewhere is not proven (#1173).
    every_descendant(root, &mut |node| {
        let last = |name: TsNode<'_>| cx.text(&name).rsplit('.').next().map(str::to_string);
        // Importing `a.b.c` runs `a`, `a.b` and `a.b.c`, whichever name it binds.
        let segments = |name: &str| {
            name.split('.')
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        match node.kind() {
            "attribute" => out.member_references.extend(
                node.child_by_field_name("attribute")
                    .map(|name| cx.text(&name).to_string()),
            ),
            "import_statement" => {
                for imported in named(node) {
                    let name = match imported.kind() {
                        "dotted_name" => Some(imported),
                        "aliased_import" => imported.child_by_field_name("name"),
                        _ => None,
                    };
                    if let Some(name) = name {
                        out.member_references.extend(segments(cx.text(&name)));
                    }
                }
            }
            "import_from_statement" => {
                let module = node.child_by_field_name("module_name");
                if let Some(module) = module {
                    out.member_references.extend(segments(cx.text(&module)));
                }
                // `from m import *` may bind any public name of `m`.
                if named(node)
                    .iter()
                    .any(|child| child.kind() == "wildcard_import")
                {
                    out.member_references
                        .extend(module.and_then(last).map(|stem| format!("*{stem}")));
                }
                for imported in named(node) {
                    let name = match imported.kind() {
                        "dotted_name" if Some(imported) != module => Some(imported),
                        "aliased_import" => imported.child_by_field_name("name"),
                        _ => None,
                    };
                    out.member_references.extend(name.and_then(last));
                }
            }
            "call" => {
                let fetched = node
                    .child_by_field_name("function")
                    .filter(|callee| callee.kind() == "identifier" && cx.text(callee) == "getattr")
                    .and_then(|_| node.child_by_field_name("arguments"))
                    .and_then(|arguments| {
                        named(arguments)
                            .into_iter()
                            .filter(|argument| argument.kind() != "comment")
                            .nth(1)
                    })
                    .and_then(|name| crate::literal_string(cx, unparenthesize(name)));
                out.member_references.extend(fetched);
            }
            // Any loader, however spelled or aliased, may import a module
            // named by a literal, so every dotted-name literal counts.
            "string" => {
                let dotted = crate::literal_string(cx, node).filter(|text| {
                    !text.is_empty()
                        && text
                            .chars()
                            .all(|c| c == '.' || c == '_' || c.is_alphanumeric())
                });
                out.member_references
                    .extend(dotted.iter().flat_map(|text| segments(text)));
            }
            _ => {}
        }
    });
    let top_level = |function: &TsNode<'_>, symbol: &str| {
        function
            .child_by_field_name("name")
            .is_some_and(|name| locals.get(cx.text(&name)) == Some(&symbol.to_string()))
    };
    for (function, symbol) in functions {
        if top_level(function, symbol)
            && let Some(effects) = fn_effects(cx, *function, symbol, resolver)
        {
            out.fn_effects.push(effects);
        }
    }
    for (function, handler) in functions {
        let Some(parent) = function
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")
        else {
            continue;
        };
        if let Some(facts) = handler_facts(cx, root, parent, *function, handler, bindings, resolver)
        {
            out.auth_handlers.push(facts);
        }
        for decorator in named(parent)
            .into_iter()
            .filter(|child| child.kind() == "decorator")
        {
            let Some(expression) = decorator.named_child(0).map(unparenthesize) else {
                continue;
            };
            if !is_hook_attribute(cx, expression) {
                continue;
            }
            let Some(name) = function.child_by_field_name("name") else {
                continue;
            };
            let key = (cx.path.to_string(), cx.text(&name).to_string());
            out.hooks.push(HookFact::Resolved {
                key,
                symbol: Some(handler.clone()),
                app: hook_app(cx, expression, bindings),
                evidence: cx.evidence(&decorator),
            });
            if !top_level(function, handler)
                && let Some(effects) = fn_effects(cx, *function, handler, resolver)
            {
                out.fn_effects.push(effects);
            }
        }
    }
    every_descendant(root, &mut |node| match node.kind() {
        "call" => {
            let Some(callee) = node.child_by_field_name("function") else {
                return;
            };
            if callee.kind() == "identifier"
                && matches!(cx.text(&callee), "setattr" | "delattr")
                && let Some(object) = node.child_by_field_name("arguments").and_then(|arguments| {
                    named(arguments)
                        .into_iter()
                        .find(|argument| argument.kind() != "comment")
                })
            {
                every_descendant(object, &mut |part| {
                    if part.kind() == "identifier" {
                        out.attribute_write_objects
                            .insert(cx.text(&part).to_string());
                    }
                });
                let name = node
                    .child_by_field_name("arguments")
                    .and_then(|arguments| {
                        named(arguments)
                            .into_iter()
                            .filter(|argument| argument.kind() != "comment")
                            .nth(1)
                    })
                    .and_then(|name| crate::literal_string(cx, unparenthesize(name)));
                out.written_attributes.extend(name);
            }
            let patched = setattr_guards(cx, node, callee, bindings);
            if !patched.is_empty() {
                out.patched_guards.extend(patched);
                return;
            }
            if !is_hook_attribute(cx, callee) {
                return;
            }
            let argument = node
                .child_by_field_name("arguments")
                .and_then(|arguments| {
                    named(arguments)
                        .into_iter()
                        .find(|argument| argument.kind() != "comment")
                })
                .map(unparenthesize);
            let evidence = cx.evidence(&node);
            let app = hook_app(cx, callee, bindings);
            match argument.and_then(|argument| resolver.target(cx, argument, &BTreeSet::new())) {
                Some(key) => out.hooks.push(HookFact::Resolved {
                    key,
                    symbol: None,
                    app,
                    evidence,
                }),
                None => out.hooks.push(HookFact::Unresolved { app, evidence }),
            }
        }
        "assignment" => {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return;
            };
            let wsgi = left.kind() == "attribute"
                && left
                    .child_by_field_name("attribute")
                    .is_some_and(|attribute| cx.text(&attribute) == "wsgi_app");
            if let Some(name) = limiter_def(cx, root, node, left, right, bindings) {
                if has_keyword(cx, right, "on_breach") {
                    out.limiter_breach.push((cx.path.to_string(), name.clone()));
                }
                out.limiter_defs.push((cx.path.to_string(), name));
            }
            if wsgi {
                out.wsgi_facts.push(WsgiFact {
                    proxy_fix: is_proxy_fix(cx, root, left, right, bindings),
                    evidence: cx.evidence(&node),
                });
            }
            target_writes(cx, left, bindings, out);
        }
        "augmented_assignment" | "for_statement" | "for_in_clause" => {
            if let Some(target) = node.child_by_field_name("left") {
                target_writes(cx, target, bindings, out);
            }
        }
        "as_pattern" => {
            if let Some(target) = node.child_by_field_name("alias") {
                target_writes(cx, target, bindings, out);
            }
        }
        "delete_statement" => target_writes(cx, node, bindings, out),
        _ => {}
    });
}

/// Whether `call` passes the keyword argument `name`.
fn has_keyword(cx: &FileCx<'_>, call: TsNode<'_>, name: &str) -> bool {
    call.child_by_field_name("arguments")
        .is_some_and(|arguments| {
            named(arguments).into_iter().any(|argument| {
                argument.kind() == "keyword_argument"
                    && argument
                        .child_by_field_name("name")
                        .is_some_and(|key| cx.text(&key) == name)
            })
        })
}

/// The name `assignment` binds when it is a module-level
/// `name = flask_limiter.Limiter(…)` and the module's only binding of it.
fn limiter_def(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    assignment: TsNode<'_>,
    left: TsNode<'_>,
    right: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<String> {
    let at_module = assignment
        .parent()
        .and_then(|statement| statement.parent())
        .is_some_and(|parent| parent.kind() == "module");
    if !at_module || left.kind() != "identifier" || right.kind() != "call" {
        return None;
    }
    let constructor = right.child_by_field_name("function")?;
    let dotted = import_dotted(cx, constructor, bindings)?;
    let constructor_root = dotted_parts(cx, constructor)?.into_iter().next()?;
    if !matches!(
        dotted.as_str(),
        "flask_limiter.Limiter" | "flask_limiter.extension.Limiter"
    ) || rebound(cx, root, &constructor_root, Some(right))
    {
        return None;
    }
    let name = cx.text(&left);
    let writes = module_binding(cx, root, name).writes;
    (writes.len() == 1 && writes[0] == assignment).then(|| name.to_string())
}

/// The module-level flask-limiter `Limiter` definitions that stay one.
/// A limiter assigned through an imported module, or with an attribute
/// written anywhere, may be anything; so may every limiter once
/// flask-limiter's own `Limiter` is replaced.
pub(crate) fn proven_limiters(
    out: &Extraction,
    patched: &BTreeSet<String>,
) -> HashSet<(String, String)> {
    out.limiter_defs
        .iter()
        .filter(|(_, name)| {
            !patched.contains(LIMITER)
                && !out.module_attr_writes.contains(name)
                && !out.attribute_write_objects.contains(name)
        })
        .cloned()
        .collect()
}

/// Effects by key, with the package fallback `responses` uses.
fn lookup_effects<'a>(
    effects: &'a HashMap<FnKey, FnEffects>,
    key: &FnKey,
) -> Option<&'a FnEffects> {
    effects.get(key).or_else(|| {
        let package = format!("{}/__init__.py", key.0.strip_suffix(".py")?);
        effects.get(&(package, key.1.clone()))
    })
}

/// Whether `key` names a file the repository holds (or its package).
fn repository_file(files: &HashSet<String>, key: &FnKey) -> bool {
    files.contains(&key.0)
        || key
            .0
            .strip_suffix(".py")
            .is_some_and(|stem| files.contains(&format!("{stem}/__init__.py")))
}

/// Whether `key` and the repository functions it calls do not block.
fn non_blocking(
    key: &FnKey,
    effects: &HashMap<FnKey, FnEffects>,
    files: &HashSet<String>,
    visited: &mut HashSet<FnKey>,
    depth: usize,
) -> bool {
    if depth > MAX_HOOK_DEPTH {
        return false;
    }
    if !visited.insert(key.clone()) {
        return false;
    }
    let result = lookup_effects(effects, key).is_some_and(|found| {
        !found.blocks
            && (depth > 0 || !found.returns_value)
            && found.calls.iter().all(|call| {
                // A callee outside the repository (`Resolver::target`
                // names a file for any module import) is assumed not to
                // abort; one in a repository file must be a recovered,
                // non-blocking function.
                if lookup_effects(effects, call).is_some() {
                    non_blocking(call, effects, files, visited, depth + 1)
                } else {
                    !repository_file(files, call)
                }
            })
    });
    visited.remove(key);
    result
}

/// Whether `key`, and every repository function it names, cannot end a
/// request with 401/403 (#1173). A name in a repository file with no
/// recovered function is unproven; one outside the repository is assumed
/// not to abort.
fn deny_free(
    key: &FnKey,
    effects: &HashMap<FnKey, FnEffects>,
    files: &HashSet<String>,
    visited: &mut HashSet<FnKey>,
    depth: usize,
) -> bool {
    if lookup_effects(effects, key).is_none() {
        return !repository_file(files, key);
    }
    if depth > MAX_HOOK_DEPTH || !visited.insert(key.clone()) {
        return false;
    }
    let result = lookup_effects(effects, key).is_some_and(|found| {
        !found.denies
            && found
                .refs
                .iter()
                .all(|named| deny_free(named, effects, files, visited, depth + 1))
    });
    visited.remove(key);
    result
}

/// Evidence sorted and deduplicated by span, truncated to the bound.
pub(crate) fn bounded(mut evidence: Vec<EvidenceRef>) -> Vec<EvidenceRef> {
    evidence.sort_by(|a, b| {
        (&a.path, a.byte_start, a.byte_end).cmp(&(&b.path, b.byte_start, b.byte_end))
    });
    evidence.dedup_by(|a, b| {
        (&a.path, a.byte_start, a.byte_end) == (&b.path, b.byte_start, b.byte_end)
    });
    evidence.truncate(MAX_EVIDENCE);
    evidence
}

/// Write `props.auth` on every Flask Endpoint, once followed decorator
/// responses are resolved.
pub(crate) fn resolve(
    out: &mut Extraction,
    repo: &str,
    functions: &HashMap<FnKey, responses::FunctionFacts>,
) {
    let handlers = std::mem::take(&mut out.auth_handlers);
    let hooks = std::mem::take(&mut out.hooks);
    let calls = std::mem::take(&mut out.app_calls);
    let used = std::mem::take(&mut out.name_uses);
    let mut app_defs = std::mem::take(&mut out.app_defs);
    app_defs.extend(app_scope::app_defs(
        &out.receiver_defs,
        &out.module_attr_writes,
    ));
    let scope = app_scope::Scope::new(&calls, &used, &app_defs);
    let wsgi = std::mem::take(&mut out.wsgi_facts);
    let patched = std::mem::take(&mut out.patched_guards)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let limiters = proven_limiters(out, &patched);
    out.limiter_defs.clear();
    let mut handler_apps: HashMap<String, Apps> = HashMap::new();
    for (handler, app) in std::mem::take(&mut out.endpoint_apps) {
        let apps = scope.apps(&app);
        let merged = match handler_apps.remove(&handler) {
            Some(known) => known.union(apps),
            None => apps,
        };
        handler_apps.insert(handler, merged);
    }
    let mut effects: HashMap<FnKey, FnEffects> = HashMap::new();
    for found in std::mem::take(&mut out.fn_effects) {
        // Two functions sharing a key (a nested hook and a top-level
        // function of one name) are merged conservatively.
        match effects.entry(found.key.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let merged = entry.get_mut();
                if merged.symbol != found.symbol {
                    merged.symbol = None;
                }
                merged.blocks |= found.blocks;
                merged.returns_value |= found.returns_value;
                merged.status_returns |= found.status_returns;
                merged.calls.extend(found.calls);
                merged.calls.sort();
                merged.calls.dedup();
                merged.denies = true;
                merged.proof = None;
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(found);
            }
        }
    }
    let files = out
        .nodes
        .iter()
        .filter(|node| node.label == "File")
        .filter_map(|node| node.props["path"].as_str().map(str::to_string))
        .collect::<HashSet<_>>();

    let mut repo_reasons = BTreeSet::new();
    let mut hook_evidence = Vec::new();
    // Each registration whose hook may block, with the applications it is
    // registered on: cited on every `unknown` fact it reaches.
    let mut blocking = Vec::new();
    // Non-blocking hooks that may end a request with a 429 / 5xx pair.
    let mut status_hooks = Vec::new();
    for hook in &hooks {
        match hook {
            HookFact::Resolved {
                key,
                symbol,
                app,
                evidence,
            } => {
                let found = lookup_effects(&effects, key);
                if let Some(found) = found {
                    hook_evidence.push(found.evidence.clone());
                }
                if !non_blocking(key, &effects, &files, &mut HashSet::new(), 0) {
                    // Never a name built from the key: a nested hook's
                    // Symbol is qualified, and a shared key names no one.
                    let symbol = symbol
                        .clone()
                        .or_else(|| found.and_then(|found| found.symbol.clone()));
                    blocking.push((symbol, evidence.clone(), scope.apps(app)));
                } else if found.is_some_and(|found| found.status_returns) {
                    status_hooks.push(scope.apps(app));
                }
            }
            HookFact::Unresolved { app, evidence } => {
                blocking.push((None, evidence.clone(), scope.apps(app)));
            }
        }
    }
    blocking
        .sort_by(|a, b| (&a.1.path, a.1.byte_start, &a.0).cmp(&(&b.1.path, b.1.byte_start, &b.0)));
    blocking.dedup_by(|a, b| {
        let same = a.0 == b.0 && a.1 == b.1;
        if same {
            b.2 = std::mem::replace(&mut b.2, Apps::Any).union(a.2.clone());
        }
        same
    });
    if wsgi.iter().any(|fact| !fact.proxy_fix) {
        repo_reasons.insert("wsgi_middleware_unknown".to_string());
    }

    // Whether the repository decorator or factory `key` is proven not to
    // authenticate (#1173).
    let trusted = !patched.contains(WRAPS) && !patched.contains(WERKZEUG_ERRORS);
    let proven = |key: &FnKey, factory: bool| {
        let written = |name: &String| {
            out.module_attr_writes.contains(name) || out.attribute_write_objects.contains(name)
        };
        // A helper may write a proof name through any object it is passed,
        // as in `m.lock = …`; the class itself may not escape at all.
        let attribute_written = |name: &String| out.written_attributes.contains(name);
        trusted
            && lookup_effects(&effects, key)
                .and_then(|found| found.proof.as_ref())
                .is_some_and(|proof| {
                    proof.factory == factory
                        && proof.clean
                        && !proof.names.iter().any(written)
                        && !proof.names.iter().any(attribute_written)
                        && !proof.class.as_ref().is_some_and(|class| {
                            let stem = key.0.rsplit('/').next().unwrap_or(&key.0);
                            let stem = stem.strip_suffix(".py").unwrap_or(stem);
                            out.member_references.contains(class)
                                || out.member_references.contains(&format!("*{stem}"))
                        })
                        && proof
                            .calls
                            .iter()
                            .all(|call| deny_free(call, &effects, &files, &mut HashSet::new(), 1))
                })
    };

    let mut by_handler: HashMap<String, HandlerAuthFacts> = HashMap::new();
    for facts in handlers {
        by_handler.entry(facts.handler.clone()).or_insert(facts);
    }
    let mut auth: HashMap<String, serde_json::Value> = HashMap::new();
    for (handler, facts) in &by_handler {
        // A hook reaches the handler only when they may share an application.
        let apps = handler_apps.get(handler).unwrap_or(&Apps::Any);
        let reaching = blocking
            .iter()
            .filter(|(_, _, hook)| hook.meets(apps))
            .collect::<Vec<_>>();
        let scoped_out = reaching.len() < blocking.len();
        let mut guards = BTreeSet::new();
        let mut basis = BTreeSet::new();
        let mut reasons = BTreeSet::new();
        let mut evidence = Vec::new();
        let mut framework = false;
        // Decorators proven not to authenticate, cited on a `none` fact.
        let mut non_auth = BTreeSet::new();
        let mut non_auth_evidence = Vec::new();
        // Which kinds of non-authenticating decorator the fact rests on.
        let mut trusted_non_auth = false;
        let mut proven_non_auth = false;
        for decorator in &facts.decorators {
            match decorator {
                DecoratorFact::NonAuth { text, .. } if patched.contains(CORS) => {
                    reasons.insert(format!("unclassified_decorator:{text}"));
                }
                DecoratorFact::NonAuth {
                    text,
                    evidence: span,
                } => {
                    non_auth.insert(text.clone());
                    non_auth_evidence.push(span.clone());
                    trusted_non_auth = true;
                }
                DecoratorFact::Limit {
                    key,
                    text,
                    evidence: span,
                } => {
                    if limiters.contains(key) {
                        non_auth.insert(text.clone());
                        non_auth_evidence.push(span.clone());
                        trusted_non_auth = true;
                    } else {
                        reasons.insert(format!("unclassified_decorator:{text}"));
                    }
                }
                DecoratorFact::Framework { name, .. } if patched.contains(name) => {
                    // The module attribute is reassigned somewhere in the
                    // repository, so the decorator may not be the guard.
                    reasons.insert(format!("framework_guard_reassigned:{name}"));
                }
                DecoratorFact::Framework {
                    name,
                    evidence: span,
                } => {
                    guards.insert(name.clone());
                    basis.insert("framework_guard");
                    evidence.push(span.clone());
                    framework = true;
                }
                DecoratorFact::Repository {
                    key,
                    evidence: span,
                    text,
                } => {
                    if lookup_effects(&effects, key).is_none() {
                        // `Resolver::target` names a file for any import;
                        // with no recovered function there it is not a
                        // repository decorator.
                        reasons.insert(format!("unclassified_decorator:{text}"));
                        continue;
                    }
                    let merged = responses::decorator_responses(key, functions, repo);
                    let denied = |status: &u16| matches!(status, 401 | 403);
                    let aborts = merged
                        .aborts
                        .iter()
                        .filter(|(status, _)| denied(status))
                        .map(|(_, span)| span.clone())
                        .collect::<Vec<_>>();
                    // `flask.abort` replaced anywhere may be anything.
                    let aborts = if patched.contains(ABORT) {
                        Vec::new()
                    } else {
                        aborts
                    };
                    let returns = merged.statuses.iter().any(denied);
                    if returns || !aborts.is_empty() {
                        guards.insert(symbol_id(repo, &key.0, &key.1));
                        evidence.push(span.clone());
                    }
                    if returns {
                        basis.insert("decorator_returns_401_or_403");
                        evidence.extend(merged.evidence.iter().cloned());
                    }
                    if !aborts.is_empty() {
                        basis.insert("decorator_aborts_401_or_403");
                        evidence.extend(aborts.iter().cloned());
                    }
                    if returns || !aborts.is_empty() {
                        // A guard: recorded above.
                    } else if proven(key, false) {
                        non_auth.insert(text.clone());
                        non_auth_evidence.push(span.clone());
                        proven_non_auth = true;
                    } else {
                        reasons.insert(format!(
                            "decorator_not_classified:{}",
                            symbol_id(repo, &key.0, &key.1)
                        ));
                    }
                }
                DecoratorFact::Factory {
                    key,
                    package,
                    text,
                    evidence: span,
                } => {
                    // A package module, or a package binding the name, makes
                    // `from package import name` something else.
                    let submodule = package.as_ref().is_none_or(|(module, name)| {
                        let init = module
                            .strip_suffix(".py")
                            .map(|stem| format!("{stem}/__init__.py"));
                        !files.contains(module)
                            && init.is_none_or(|init| {
                                !out.package_names.contains(&(init.clone(), name.clone()))
                                    && !out.package_names.contains(&(init.clone(), "*".to_string()))
                                    // A module `__getattr__` may answer any name.
                                    && !out
                                        .package_names
                                        .contains(&(init, "__getattr__".to_string()))
                            })
                    });
                    if submodule && proven(key, true) {
                        non_auth.insert(text.clone());
                        non_auth_evidence.push(span.clone());
                        proven_non_auth = true;
                    } else {
                        reasons.insert(format!("unclassified_decorator:{text}"));
                    }
                }
                DecoratorFact::Unclassified { reason } => {
                    reasons.insert(reason.clone());
                }
            }
        }
        let mut value = if !guards.is_empty() {
            let confidence = if framework {
                ConfidenceTier::Confirmed
            } else {
                ConfidenceTier::InferredStrong
            };
            serde_json::json!({
                "state": "required",
                "authenticated": true,
                "guards": guards,
                "basis": basis,
                "prov": prov_from(
                    bounded(evidence),
                    confidence,
                    &format!("Auth of {handler}"),
                ),
            })
        } else if reasons.is_empty() && repo_reasons.is_empty() && reaching.is_empty() {
            let mut evidence = facts.route_evidence.clone();
            evidence.extend(non_auth_evidence);
            evidence.extend(hook_evidence.iter().cloned());
            evidence.extend(wsgi.iter().map(|fact| fact.evidence.clone()));
            let mut assumptions = vec!["non_repository_calls_in_request_hooks_do_not_abort"];
            if scoped_out {
                assumptions.push("request_hooks_reach_only_their_application");
            }
            if trusted_non_auth {
                assumptions.push("rate_limit_and_cors_decorators_do_not_authenticate");
            }
            if proven_non_auth {
                assumptions.push("non_repository_calls_in_repository_decorators_do_not_abort");
                assumptions.push("werkzeug_errors_keep_their_status");
            }
            if status_hooks.iter().any(|hook| hook.meets(apps)) {
                assumptions.push("request_hook_statuses_do_not_authenticate");
            }
            let mut value = serde_json::json!({
                "state": "none",
                "authenticated": false,
                "basis": "no_guard_decorator_or_blocking_request_hook",
                "assumptions": assumptions,
                "prov": prov_from(
                    bounded(evidence),
                    ConfidenceTier::InferredStrong,
                    &format!("Auth of {handler}"),
                ),
            });
            if !non_auth.is_empty() {
                value["non_auth_decorators"] = serde_json::json!(non_auth);
            }
            value
        } else {
            reasons.extend(repo_reasons.iter().cloned());
            if !reaching.is_empty() {
                reasons.insert("request_hook_may_block".to_string());
            }
            let mut value = serde_json::json!({ "state": "unknown", "reasons": reasons });
            if !reaching.is_empty() {
                value["blocking_hooks"] = reaching
                    .iter()
                    .take(MAX_EVIDENCE)
                    .map(|(hook, evidence, _)| serde_json::json!({ "hook": hook, "evidence": evidence }))
                    .collect();
            }
            if reaching.len() > MAX_EVIDENCE {
                value["blocking_hooks_omitted"] = serde_json::json!(reaching.len() - MAX_EVIDENCE);
            }
            value
        };
        if !facts.ineffective.is_empty() {
            let mut ineffective = facts.ineffective.clone();
            ineffective.sort();
            ineffective.dedup();
            value["ineffective_decorators"] = serde_json::json!(ineffective);
        }
        auth.insert(handler.clone(), value);
    }
    for node in &mut out.nodes {
        if node.label != "Endpoint" || node.props["framework"] != "flask" {
            continue;
        }
        let Some(handler) = node.props["handler_sym"].as_str() else {
            continue;
        };
        if let Some(value) = auth.get(handler) {
            node.props["auth"] = value.clone();
        }
    }
}

/// Retarget the evidence a cached file's auth facts carry to `commit`.
pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    for facts in &mut extraction.auth_handlers {
        for span in &mut facts.route_evidence {
            span.commit_sha = commit.into();
        }
        for decorator in &mut facts.decorators {
            match decorator {
                DecoratorFact::Framework { evidence, .. }
                | DecoratorFact::Repository { evidence, .. }
                | DecoratorFact::NonAuth { evidence, .. }
                | DecoratorFact::Factory { evidence, .. }
                | DecoratorFact::Limit { evidence, .. } => {
                    evidence.commit_sha = commit.into();
                }
                DecoratorFact::Unclassified { .. } => {}
            }
        }
    }
    for effects in &mut extraction.fn_effects {
        effects.evidence.commit_sha = commit.into();
    }
    for fact in &mut extraction.wsgi_facts {
        fact.evidence.commit_sha = commit.into();
    }
    for hook in &mut extraction.hooks {
        match hook {
            HookFact::Resolved { evidence, .. } | HookFact::Unresolved { evidence, .. } => {
                evidence.commit_sha = commit.into();
            }
        }
    }
}
