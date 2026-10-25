//! Android Navigation 3 screens, initial destinations, startup navigations,
//! and entry-callback registrations (W11a-3, W11b-2a, AC-0351; ADR-0050
//! decisions 2, 3, and 4).
//!
//! An `entryProvider { … }` call, with `entryProvider` import-proven from
//! `androidx.navigation3.runtime`, registers each `entry<X>(…) { … }` call in
//! its trailing lambda as a key-based `Screen` (`screen:{repo}@navigation3:{fqn}`)
//! when `X` resolves to a type declared in this repository, or a
//! `route-unresolved` Gap when it does not. Top-level call statements in the
//! entry's trailing lambda that resolve to repository function Symbols become
//! Confirmed `RENDERS` edges. Callback arguments (`::fn`, `recv::fn`, or a
//! single-call lambda `{ fn(…) }` / `{ recv.fn(…) }`; inside `with(recv) { … }`
//! an unqualified name is `recv`'s member first) passed to a rendered
//! composable `C` are recorded on the `Screen` node under `entry_callbacks`
//! (`{param, symbol, composable, evidence, read, forwarded}`), following
//! direct parameter forwarding up to [`CALLBACK_DEPTH`] hops (`read` is false
//! only when no composable on the chain references the parameter); they never
//! emit T0 call or navigation edges.
//!
//! In a class with a proven back-stack property (`mutableStateListOf<NavKey>(…)`,
//! `MutableStateFlow(mutableStateListOf<NavKey>(…))`, or
//! `rememberNavBackStack(…)`), a single initial key argument `K` or `K(…)`
//! marks the corresponding recovered `Screen` with `initial: true` and
//! `back_stack_owner` (plus `initial_owners` when multiple classes name the
//! same screen). Direct `add` calls, parameter helpers and builder helpers in
//! the same class emit InferredStrong `NAVIGATES_TO` edges (or
//! `navigation-unresolved` Gaps when the target is computed or names a type
//! with no recovered screen); navigation sites in `init` blocks or property
//! initializers carry `phase: "init"`.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tree_sitter::Node as TsNode;

use super::{Declared, EXTRACTOR_ID, FileCx, Imports, enclosing_type_chain, symbol_id};

const RUNTIME_PACKAGE: &str = "androidx.navigation3.runtime";
const FLOW_PACKAGE: &str = "kotlinx.coroutines.flow";
const COMPOSE_PACKAGE: &str = "androidx.compose.runtime";

pub(crate) const ROUTER: &str = "navigation3";
pub(crate) const ROUTER_BASIS: &str = "back_stack";
pub(crate) const CALLBACK_DEPTH: usize = 4;

/// Gap reason: an `entry<X>` key that is not a declared type in this repository.
pub const UNDECLARED_KEY_REASON: &str = "navigation key is not a declared type";
/// Gap reason: a navigation target that is not a key constructor or object reference.
pub const COMPUTED_TARGET_REASON: &str = "navigation target is computed";
/// Gap reason: a navigation target whose key has no recovered screen.
pub const UNKNOWN_SCREEN_REASON: &str = "navigation target is not a recovered screen";

fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

fn prov(evidence: Vec<EvidenceRef>, confidence: ConfidenceTier, fact: &str) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("navigation3 provenance has evidence");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// A type reference waiting for repo-wide resolution against `types_by_fqn`.
#[derive(Debug, Clone)]
pub(crate) struct TypeRef {
    exact_candidates: Vec<String>,
    wildcard_candidates: Vec<String>,
}

impl TypeRef {
    fn from_name(name: &str, package: Option<&str>, imports: &Imports, chain: &[String]) -> Self {
        let name = name.trim();
        match name.split_once('.') {
            None => {
                if let Some(fqn) = imports.bindings.get(name) {
                    let mut exact = Vec::new();
                    if let Some(pkg) = package {
                        for len in (1..=chain.len()).rev() {
                            exact.push(format!("{pkg}.{}.{name}", chain[..len].join(".")));
                        }
                    }
                    exact.push(fqn.clone());
                    return Self {
                        exact_candidates: exact,
                        wildcard_candidates: Vec::new(),
                    };
                }
                let mut exact = Vec::new();
                if let Some(pkg) = package {
                    for len in (1..=chain.len()).rev() {
                        exact.push(format!("{pkg}.{}.{name}", chain[..len].join(".")));
                    }
                    exact.push(format!("{pkg}.{name}"));
                }
                let wildcard_candidates = imports
                    .wildcard_packages
                    .iter()
                    .map(|w| format!("{w}.{name}"))
                    .collect();
                Self {
                    exact_candidates: exact,
                    wildcard_candidates,
                }
            }
            Some((first, rest)) => {
                if let Some(prefix_fqn) = imports.bindings.get(first) {
                    return Self {
                        exact_candidates: vec![format!("{prefix_fqn}.{rest}")],
                        wildcard_candidates: Vec::new(),
                    };
                }
                let mut exact = Vec::new();
                if let Some(pkg) = package {
                    for len in (1..=chain.len()).rev() {
                        exact.push(format!("{pkg}.{}.{name}", chain[..len].join(".")));
                    }
                    exact.push(format!("{pkg}.{name}"));
                }
                exact.push(name.to_string());
                let wildcard_candidates = imports
                    .wildcard_packages
                    .iter()
                    .map(|w| format!("{w}.{name}"))
                    .collect();
                Self {
                    exact_candidates: exact,
                    wildcard_candidates,
                }
            }
        }
    }

    fn resolve<'a>(
        &self,
        types_by_fqn: &'a BTreeMap<String, Option<Declared>>,
    ) -> Option<&'a Declared> {
        for cand in &self.exact_candidates {
            if let Some(entry) = types_by_fqn.get(cand) {
                return entry.as_ref();
            }
        }
        let mut matches = Vec::new();
        for cand in &self.wildcard_candidates {
            if types_by_fqn.contains_key(cand) && !matches.contains(&cand) {
                matches.push(cand);
            }
        }
        let [only] = matches.as_slice() else {
            return None;
        };
        types_by_fqn.get(*only).and_then(Option::as_ref)
    }
}

/// Top-level call inside an `entry<X> { … }` trailing lambda.
#[derive(Debug, Clone)]
enum RenderCall {
    Local(String),
    Imported {
        fqn: String,
        member: Option<String>,
    },
    Unqualified {
        package: Option<String>,
        wildcards: Vec<String>,
        name: String,
    },
}

impl RenderCall {
    fn resolve(
        &self,
        repo: &str,
        types_by_fqn: &BTreeMap<String, Option<Declared>>,
        functions_by_fqn: &BTreeMap<String, Option<Declared>>,
        known: &HashSet<String>,
    ) -> Option<String> {
        let candidate = match self {
            RenderCall::Local(sym) => Some(sym.clone()),
            RenderCall::Imported { fqn, member: None } => functions_by_fqn
                .get(fqn)
                .and_then(Option::as_ref)
                .map(|d| symbol_id(repo, &d.path, &d.qualified)),
            RenderCall::Imported {
                fqn,
                member: Some(member),
            } => types_by_fqn
                .get(fqn)
                .and_then(Option::as_ref)
                .map(|d| symbol_id(repo, &d.path, &format!("{}.{member}", d.qualified))),
            RenderCall::Unqualified {
                package,
                wildcards,
                name,
            } => {
                if let Some(pkg) = package
                    && let Some(entry) = functions_by_fqn.get(&format!("{pkg}.{name}"))
                {
                    entry
                        .as_ref()
                        .map(|d| symbol_id(repo, &d.path, &d.qualified))
                } else {
                    let mut matches = Vec::new();
                    for w in wildcards {
                        let fqn = format!("{w}.{name}");
                        if functions_by_fqn.contains_key(&fqn) && !matches.contains(&fqn) {
                            matches.push(fqn);
                        }
                    }
                    if let [only] = matches.as_slice() {
                        functions_by_fqn
                            .get(only)
                            .and_then(Option::as_ref)
                            .map(|d| symbol_id(repo, &d.path, &d.qualified))
                    } else {
                        None
                    }
                }
            }
        }?;
        known.contains(&candidate).then_some(candidate)
    }
}

#[derive(Debug, Clone)]
enum CallbackTarget {
    TopLevel(RenderCall),
    ReceiverMethod {
        receiver_type: TypeRef,
        method: String,
    },
    /// An unqualified name inside `with(recv) { … }`: a member of `recv`'s
    /// class wins over the top-level `fallback`, as in Kotlin resolution.
    WithReceiver {
        receiver_type: TypeRef,
        method: String,
        fallback: RenderCall,
    },
}

impl CallbackTarget {
    fn resolve(
        &self,
        repo: &str,
        types_by_fqn: &BTreeMap<String, Option<Declared>>,
        functions_by_fqn: &BTreeMap<String, Option<Declared>>,
        functions: &BTreeMap<String, Option<FunctionInfo>>,
        known: &HashSet<String>,
    ) -> Option<String> {
        let sym = match self {
            CallbackTarget::TopLevel(rc) => {
                rc.resolve(repo, types_by_fqn, functions_by_fqn, known)?
            }
            CallbackTarget::ReceiverMethod {
                receiver_type,
                method,
            } => {
                let declared = receiver_type.resolve(types_by_fqn)?;
                let sym = symbol_id(
                    repo,
                    &declared.path,
                    &format!("{}.{method}", declared.qualified),
                );
                if !known.contains(&sym) {
                    return None;
                }
                sym
            }
            CallbackTarget::WithReceiver {
                receiver_type,
                method,
                fallback,
            } => {
                let declared = receiver_type.resolve(types_by_fqn)?;
                let member = symbol_id(
                    repo,
                    &declared.path,
                    &format!("{}.{method}", declared.qualified),
                );
                if known.contains(&member) {
                    member
                } else {
                    fallback.resolve(repo, types_by_fqn, functions_by_fqn, known)?
                }
            }
        };
        // Overloads prove nothing (as in the helper rule): a function or method
        // whose symbol id was declared more than once has `functions[&sym] == None`.
        functions.get(&sym).and_then(Option::as_ref)?;
        Some(sym)
    }
}

#[derive(Debug, Clone)]
struct CallbackArg {
    name: Option<String>,
    index: usize,
    target: CallbackTarget,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
struct RenderSite {
    call: RenderCall,
    evidence: EvidenceRef,
    callbacks: Vec<CallbackArg>,
}

#[derive(Debug, Clone)]
struct EntrySite {
    key: Option<TypeRef>,
    renders: Vec<RenderSite>,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
enum NavTarget {
    ObjectKey(TypeRef),
    ConstructorKey(TypeRef),
    Computed,
}

#[derive(Debug, Clone)]
struct InitialSite {
    owner: String,
    target: NavTarget,
    /// The back-stack property whose initializer names the key.
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
struct NavSite {
    source: String,
    target: NavTarget,
    via: &'static str,
    conditional: bool,
    init_phase: bool,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
struct ForwardCall {
    source_param: String,
    target_call: RenderCall,
    target_arg_name: Option<String>,
    target_arg_idx: usize,
}

#[derive(Debug, Clone)]
struct FunctionInfo {
    params: Vec<String>,
    /// Parameters the body references unshadowed: invoked, forwarded (to any
    /// callee, repository or library), or otherwise read. Only a parameter the
    /// body never mentions is unread.
    used_params: HashSet<String>,
    forwards: Vec<ForwardCall>,
}

/// Per-file Navigation 3 facts carried to directory completion.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    entries: Vec<EntrySite>,
    initials: Vec<InitialSite>,
    navigations: Vec<NavSite>,
    functions: BTreeMap<String, Option<FunctionInfo>>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Pending) {
        self.entries.extend(other.entries);
        self.initials.extend(other.initials);
        self.navigations.extend(other.navigations);
        for (sym, info) in other.functions {
            self.functions
                .entry(sym)
                .and_modify(|existing| *existing = None)
                .or_insert(info);
        }
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for entry in &mut self.entries {
            entry.evidence.commit_sha = commit.into();
            for render in &mut entry.renders {
                render.evidence.commit_sha = commit.into();
                for cb in &mut render.callbacks {
                    cb.evidence.commit_sha = commit.into();
                }
            }
        }
        for init in &mut self.initials {
            init.evidence.commit_sha = commit.into();
        }
        for nav in &mut self.navigations {
            nav.evidence.commit_sha = commit.into();
        }
    }
}

/// Whether `name` at `site` is proven to come from `package.expected`,
/// without being shadowed by a local declaration in `local_decls`.
fn proven_name(
    cx: &FileCx<'_>,
    site: TsNode<'_>,
    raw: &str,
    package: &str,
    expected: &str,
    imports: &Imports,
    local_decls: &HashSet<String>,
) -> bool {
    let raw = raw.trim();
    let expected_fqn = format!("{package}.{expected}");
    if raw == expected_fqn {
        return true;
    }
    if raw.contains('.') {
        return false;
    }
    let chain = enclosing_type_chain(cx, site);
    if !chain.is_empty() && local_decls.contains(&format!("{}.{raw}", chain.join("."))) {
        return false;
    }
    if local_decls.contains(raw) {
        return false;
    }
    if let Some(fqn) = imports.bindings.get(raw) {
        return fqn == &expected_fqn;
    }
    raw == expected && imports.wildcard_packages.iter().any(|w| w == package)
}

/// Dotted identifier text (`A` or `A.B.C`) of an `identifier` or plain `.`
/// `navigation_expression`.
fn dotted_identifiers(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "identifier" => {
            let text = cx.text(&node);
            (!text.is_empty()).then(|| text.to_string())
        }
        "navigation_expression" => {
            let mut walk = node.walk();
            let children: Vec<_> = node.children(&mut walk).collect();
            let [left, op, right] = children.as_slice() else {
                return None;
            };
            if cx.text(op) != "." || right.kind() != "identifier" {
                return None;
            }
            let prefix = dotted_identifiers(cx, *left)?;
            Some(format!("{prefix}.{}", cx.text(right)))
        }
        _ => None,
    }
}

/// Single `user_type` inside a `<T>` `type_arguments` node, when `T` is a
/// plain type with no type arguments of its own and no variance modifier.
fn single_type_arg_name<'t>(cx: &FileCx<'_>, type_args: TsNode<'t>) -> Option<String> {
    let mut walk = type_args.walk();
    let projections: Vec<_> = type_args
        .named_children(&mut walk)
        .filter(|c| c.kind() == "type_projection")
        .collect();
    let [proj] = projections.as_slice() else {
        return None;
    };
    let mut pw = proj.walk();
    let parts: Vec<_> = proj.named_children(&mut pw).collect();
    let [user_type] = parts.as_slice() else {
        return None;
    };
    plain_user_type_name(cx, *user_type)
}

fn plain_user_type_name(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "user_type" {
        return None;
    }
    let mut walk = node.walk();
    let mut parts = Vec::new();
    for child in node.named_children(&mut walk) {
        if child.kind() != "identifier" {
            return None;
        }
        parts.push(cx.text(&child));
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

fn trailing_lambda<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut walk = call.walk();
    let annotated = call
        .named_children(&mut walk)
        .find(|c| c.kind() == "annotated_lambda")?;
    let mut lw = annotated.walk();
    annotated
        .named_children(&mut lw)
        .find(|c| c.kind() == "lambda_literal")
}

fn value_arguments<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut walk = call.walk();
    call.named_children(&mut walk)
        .find(|c| c.kind() == "value_arguments")
}

fn type_arguments<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut walk = call.walk();
    call.named_children(&mut walk)
        .find(|c| c.kind() == "type_arguments")
}

fn extract_render_call(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
) -> Option<RenderCall> {
    let callee = call.named_child(0)?;
    let (receiver, name) = match callee.kind() {
        "identifier" => (None, cx.text(&callee).to_string()),
        "navigation_expression" => {
            let mut walk = callee.walk();
            let parts: Vec<_> = callee.named_children(&mut walk).collect();
            let [receiver, name] = parts.as_slice() else {
                return None;
            };
            if name.kind() != "identifier" {
                return None;
            }
            (Some(*receiver), cx.text(name).to_string())
        }
        _ => return None,
    };
    let receiver_is_this = receiver.is_none_or(|r| r.kind() == "this_expression");
    if receiver_is_this {
        let chain = enclosing_type_chain(cx, call);
        let qualified = if chain.is_empty() {
            name.clone()
        } else {
            format!("{}.{name}", chain.join("."))
        };
        if local_functions.contains(&qualified) {
            return Some(RenderCall::Local(symbol_id(
                cx.id.repo, cx.path, &qualified,
            )));
        }
        if !chain.is_empty() && local_functions.contains(&name) {
            return Some(RenderCall::Local(symbol_id(cx.id.repo, cx.path, &name)));
        }
        if receiver.is_some() {
            return None;
        }
        if let Some(fqn) = imports.bindings.get(&name) {
            return Some(RenderCall::Imported {
                fqn: fqn.clone(),
                member: None,
            });
        }
        return Some(RenderCall::Unqualified {
            package: package.map(str::to_string),
            wildcards: imports.wildcard_packages.clone(),
            name,
        });
    }
    let receiver = receiver?;
    if receiver.kind() != "identifier" {
        return None;
    }
    let receiver_text = cx.text(&receiver);
    if let Some(fqn) = imports.bindings.get(receiver_text) {
        return Some(RenderCall::Imported {
            fqn: fqn.clone(),
            member: Some(name),
        });
    }
    package.map(|pkg| RenderCall::Imported {
        fqn: format!("{pkg}.{receiver_text}"),
        member: Some(name),
    })
}

fn extract_entries(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
    pending: &mut Pending,
) {
    // A property of function type shadows the import as a function does.
    let mut shadows = local_functions.clone();
    shadows.extend(declared_properties(cx, root));
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        let children: Vec<_> = node.named_children(&mut walk).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
        if node.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = node.named_child(0) else {
            continue;
        };
        let Some(callee_name) = dotted_identifiers(cx, callee) else {
            continue;
        };
        if !proven_name(
            cx,
            node,
            &callee_name,
            RUNTIME_PACKAGE,
            "entryProvider",
            imports,
            &shadows,
        ) {
            continue;
        }
        let Some(provider_lambda) = trailing_lambda(node) else {
            continue;
        };
        collect_provider_entries(
            cx,
            provider_lambda,
            package,
            imports,
            local_functions,
            pending,
        );
    }
}

fn collect_provider_entries(
    cx: &FileCx<'_>,
    provider_lambda: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
    pending: &mut Pending,
) {
    let mut walk = provider_lambda.walk();
    for stmt in provider_lambda.named_children(&mut walk) {
        let Some((key_name, entry_lambda)) = parse_entry_statement(cx, stmt) else {
            continue;
        };
        let chain = enclosing_type_chain(cx, stmt);
        let key = key_name.map(|name| TypeRef::from_name(&name, package, imports, &chain));
        let mut renders = Vec::new();
        let mut lw = entry_lambda.walk();
        for body_stmt in entry_lambda.named_children(&mut lw) {
            if body_stmt.kind() != "call_expression" {
                continue;
            }
            if let Some(rc) = extract_render_call(cx, body_stmt, package, imports, local_functions)
            {
                let callbacks =
                    extract_callback_args(cx, body_stmt, package, imports, local_functions);
                renders.push(RenderSite {
                    call: rc,
                    evidence: evidence(cx, body_stmt),
                    callbacks,
                });
            }
        }
        pending.entries.push(EntrySite {
            key,
            renders,
            evidence: evidence(cx, stmt),
        });
    }
}

fn extract_callback_args(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
) -> Vec<CallbackArg> {
    let mut out = Vec::new();
    let Some(args) = value_arguments(call) else {
        return out;
    };
    let mut aw = args.walk();
    for (index, varg) in args
        .named_children(&mut aw)
        .filter(|c| c.kind() == "value_argument")
        .enumerate()
    {
        let Some((name, expr)) = value_argument_name_and_expr(cx, varg) else {
            continue;
        };
        if let Some(target) = extract_callback_target(cx, expr, package, imports, local_functions) {
            out.push(CallbackArg {
                name,
                index,
                target,
                evidence: evidence(cx, varg),
            });
        }
    }
    out
}

fn extract_callback_target(
    cx: &FileCx<'_>,
    expr: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
) -> Option<CallbackTarget> {
    let expr = unwrap_parens(expr);
    if let Some((site, recv, name)) =
        parse_function_reference(cx, expr).or_else(|| parse_single_call_lambda(cx, expr))
    {
        return match recv {
            None => {
                let target = extract_unqualified_callback(
                    cx,
                    site,
                    &name,
                    package,
                    imports,
                    local_functions,
                )?;
                match (
                    enclosing_with_receiver(cx, site, package, imports, local_functions),
                    target,
                ) {
                    (None, target) => Some(target),
                    // A `with` receiver whose type is unproven could own the name.
                    (Some(None), _) => None,
                    (Some(Some(receiver_type)), CallbackTarget::TopLevel(fallback)) => {
                        Some(CallbackTarget::WithReceiver {
                            receiver_type,
                            method: name,
                            fallback,
                        })
                    }
                    (Some(Some(_)), _) => None,
                }
            }
            Some(recv_name) => {
                let receiver_type = resolve_receiver_type(cx, site, &recv_name, package, imports)?;
                Some(CallbackTarget::ReceiverMethod {
                    receiver_type,
                    method: name,
                })
            }
        };
    }
    None
}

fn parse_function_reference<'t>(
    cx: &FileCx<'_>,
    expr: TsNode<'t>,
) -> Option<(TsNode<'t>, Option<String>, String)> {
    match expr.kind() {
        "callable_reference" => {
            let mut walk = expr.walk();
            if expr.children(&mut walk).any(|c| c.kind() == "class") {
                return None;
            }
            let mut nw = expr.walk();
            let parts: Vec<_> = expr.named_children(&mut nw).collect();
            match parts.as_slice() {
                [ident] if ident.kind() == "identifier" => {
                    let name = cx.text(ident);
                    (!name.is_empty()).then(|| (*ident, None, name.to_string()))
                }
                [recv_node, ident] if ident.kind() == "identifier" => {
                    let name = cx.text(ident);
                    if name.is_empty() {
                        return None;
                    }
                    if recv_node.kind() == "this_expression" {
                        return Some((*ident, None, name.to_string()));
                    }
                    let recv = match recv_node.kind() {
                        "identifier" => Some(cx.text(recv_node).to_string()),
                        "user_type" => {
                            plain_user_type_name(cx, *recv_node).filter(|s| !s.contains('.'))
                        }
                        _ => None,
                    }?;
                    (!recv.is_empty()).then(|| (*recv_node, Some(recv), name.to_string()))
                }
                _ => None,
            }
        }
        "navigation_expression" => {
            let mut walk = expr.walk();
            let children: Vec<_> = expr.children(&mut walk).collect();
            let [left, op, right] = children.as_slice() else {
                return None;
            };
            if cx.text(op) != "::" || right.kind() != "identifier" {
                return None;
            }
            let name = cx.text(right);
            if name.is_empty() {
                return None;
            }
            match left.kind() {
                "this_expression" => Some((*right, None, name.to_string())),
                "identifier" => {
                    let recv = cx.text(left);
                    (!recv.is_empty()).then(|| (*left, Some(recv.to_string()), name.to_string()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_single_call_lambda<'t>(
    cx: &FileCx<'_>,
    expr: TsNode<'t>,
) -> Option<(TsNode<'t>, Option<String>, String)> {
    if expr.kind() != "lambda_literal" {
        return None;
    }
    let mut walk = expr.walk();
    let stmts: Vec<_> = expr
        .named_children(&mut walk)
        .filter(|c| {
            !matches!(
                c.kind(),
                "lambda_parameters" | "line_comment" | "block_comment"
            )
        })
        .collect();
    let [call] = stmts.as_slice() else {
        return None;
    };
    if call.kind() != "call_expression" {
        return None;
    }
    let callee = call.named_child(0)?;
    match callee.kind() {
        "identifier" => {
            let name = cx.text(&callee);
            (!name.is_empty()).then(|| (callee, None, name.to_string()))
        }
        "navigation_expression" => {
            let mut cw = callee.walk();
            let children: Vec<_> = callee.children(&mut cw).collect();
            let [left, op, right] = children.as_slice() else {
                return None;
            };
            if cx.text(op) != "." || right.kind() != "identifier" {
                return None;
            }
            let name = cx.text(right);
            if name.is_empty() {
                return None;
            }
            match left.kind() {
                "this_expression" => Some((*right, None, name.to_string())),
                "identifier" => {
                    let recv = cx.text(left);
                    (!recv.is_empty()).then(|| (*left, Some(recv.to_string()), name.to_string()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn extract_unqualified_callback(
    cx: &FileCx<'_>,
    site: TsNode<'_>,
    name: &str,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
) -> Option<CallbackTarget> {
    if is_value_in_scope(cx, site, name) {
        return None;
    }
    let chain = enclosing_type_chain(cx, site);
    let qualified = if chain.is_empty() {
        name.to_string()
    } else {
        format!("{}.{name}", chain.join("."))
    };
    if local_functions.contains(&qualified) {
        return Some(CallbackTarget::TopLevel(RenderCall::Local(symbol_id(
            cx.id.repo, cx.path, &qualified,
        ))));
    }
    if !chain.is_empty() && local_functions.contains(name) {
        return Some(CallbackTarget::TopLevel(RenderCall::Local(symbol_id(
            cx.id.repo, cx.path, name,
        ))));
    }
    if let Some(fqn) = imports.bindings.get(name) {
        return Some(CallbackTarget::TopLevel(RenderCall::Imported {
            fqn: fqn.clone(),
            member: None,
        }));
    }
    Some(CallbackTarget::TopLevel(RenderCall::Unqualified {
        package: package.map(str::to_string),
        wildcards: imports.wildcard_packages.clone(),
        name: name.to_string(),
    }))
}

/// The receiver type of the innermost `with(recv) { … }` lambda enclosing
/// `site`: `None` outside any `with`, `Some(None)` when `recv` is not a plain
/// identifier with a declared type. `with` must not be shadowed locally.
fn enclosing_with_receiver(
    cx: &FileCx<'_>,
    site: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
) -> Option<Option<TypeRef>> {
    let mut cur = site;
    while let Some(parent) = cur.parent() {
        cur = parent;
        if parent.kind() != "lambda_literal" {
            continue;
        }
        let Some(call) = parent
            .parent()
            .filter(|a| a.kind() == "annotated_lambda")
            .and_then(|a| a.parent())
            .filter(|c| c.kind() == "call_expression")
        else {
            continue;
        };
        // `with(recv) { … }` parses as a call of `with(recv)` with a trailing lambda.
        let Some(head) = call.named_child(0) else {
            continue;
        };
        let (callee, args_call) = match head.kind() {
            "call_expression" => (head.named_child(0), head),
            _ => (Some(head), call),
        };
        let Some(callee) = callee.filter(|c| c.kind() == "identifier") else {
            continue;
        };
        if cx.text(&callee) != "with" {
            continue;
        }
        if local_functions.contains("with") || imports.bindings.contains_key("with") {
            return Some(None);
        }
        let recv = single_call_arg(args_call)
            .map(unwrap_parens)
            .filter(|r| r.kind() == "identifier");
        return Some(
            recv.and_then(|r| resolve_receiver_type(cx, call, cx.text(&r), package, imports)),
        );
    }
    None
}

fn is_value_in_scope(cx: &FileCx<'_>, site: TsNode<'_>, name: &str) -> bool {
    let mut cur = site;
    while let Some(parent) = cur.parent() {
        match parent.kind() {
            "lambda_literal" => {
                if lambda_param_names(cx, parent).iter().any(|p| p == name) {
                    return true;
                }
                let mut walk = parent.walk();
                for stmt in parent.named_children(&mut walk) {
                    if stmt.end_byte() <= site.start_byte()
                        && stmt.kind() == "property_declaration"
                        && property_name(cx, stmt).as_deref() == Some(name)
                    {
                        return true;
                    }
                }
            }
            "block" => {
                let mut walk = parent.walk();
                for stmt in parent.named_children(&mut walk) {
                    if stmt.end_byte() <= site.start_byte()
                        && stmt.kind() == "property_declaration"
                        && property_name(cx, stmt).as_deref() == Some(name)
                    {
                        return true;
                    }
                }
            }
            "function_declaration"
                if function_parameters(cx, parent)
                    .iter()
                    .any(|(_, p, _)| p == name) =>
            {
                return true;
            }
            "class_declaration" | "object_declaration"
                if class_property_and_param_names(cx, parent).contains(name) =>
            {
                return true;
            }
            _ => {}
        }
        cur = parent;
    }
    false
}

fn property_declared_type(cx: &FileCx<'_>, prop: TsNode<'_>) -> Option<String> {
    let mut walk = prop.walk();
    let var_decl = prop
        .named_children(&mut walk)
        .find(|c| c.kind() == "variable_declaration")?;
    let mut vw = var_decl.walk();
    let user_type = var_decl
        .named_children(&mut vw)
        .find(|c| c.kind() == "user_type")?;
    plain_user_type_name(cx, user_type)
}

fn class_member_declared_type(
    cx: &FileCx<'_>,
    class_decl: TsNode<'_>,
    name: &str,
) -> Option<Option<String>> {
    let mut walk = class_decl.walk();
    for child in class_decl.named_children(&mut walk) {
        if child.kind() == "primary_constructor" {
            let mut cw = child.walk();
            for param in child.named_children(&mut cw) {
                if param.kind() == "class_parameter" {
                    let mut pw = param.walk();
                    let named: Vec<_> = param.named_children(&mut pw).collect();
                    if let Some(id_pos) = named.iter().position(|c| c.kind() == "identifier")
                        && cx.text(&named[id_pos]) == name
                    {
                        let ty = named
                            .get(id_pos + 1)
                            .and_then(|t| plain_user_type_name(cx, *t));
                        return Some(ty);
                    }
                }
            }
        } else if child.kind() == "class_body" {
            let mut bw = child.walk();
            for member in child.named_children(&mut bw) {
                if member.kind() == "property_declaration"
                    && property_name(cx, member).as_deref() == Some(name)
                {
                    return Some(property_declared_type(cx, member));
                }
            }
        }
    }
    None
}

fn resolve_receiver_type(
    cx: &FileCx<'_>,
    site: TsNode<'_>,
    recv: &str,
    package: Option<&str>,
    imports: &Imports,
) -> Option<TypeRef> {
    let mut cur = site;
    while let Some(parent) = cur.parent() {
        match parent.kind() {
            "lambda_literal" => {
                if lambda_param_names(cx, parent).iter().any(|p| p == recv) {
                    return None;
                }
                let mut walk = parent.walk();
                for stmt in parent.named_children(&mut walk) {
                    if stmt.end_byte() <= site.start_byte()
                        && stmt.kind() == "property_declaration"
                        && property_name(cx, stmt).as_deref() == Some(recv)
                    {
                        let ty = property_declared_type(cx, stmt)?;
                        let chain = enclosing_type_chain(cx, stmt);
                        return Some(TypeRef::from_name(&ty, package, imports, &chain));
                    }
                }
            }
            "block" => {
                let mut walk = parent.walk();
                for stmt in parent.named_children(&mut walk) {
                    if stmt.end_byte() <= site.start_byte()
                        && stmt.kind() == "property_declaration"
                        && property_name(cx, stmt).as_deref() == Some(recv)
                    {
                        let ty = property_declared_type(cx, stmt)?;
                        let chain = enclosing_type_chain(cx, stmt);
                        return Some(TypeRef::from_name(&ty, package, imports, &chain));
                    }
                }
            }
            "function_declaration" => {
                for (_, param_name, param_ty) in function_parameters(cx, parent) {
                    if param_name == recv {
                        let ty = plain_user_type_name(cx, param_ty)?;
                        let chain = enclosing_type_chain(cx, parent);
                        return Some(TypeRef::from_name(&ty, package, imports, &chain));
                    }
                }
            }
            "class_declaration" | "object_declaration" => {
                if let Some(ty_opt) = class_member_declared_type(cx, parent, recv) {
                    let ty = ty_opt?;
                    let mut chain = enclosing_type_chain(cx, parent);
                    if let Some(name_node) = parent.child_by_field_name("name") {
                        chain.push(cx.text(&name_node).to_string());
                    }
                    return Some(TypeRef::from_name(&ty, package, imports, &chain));
                }
            }
            _ => {}
        }
        cur = parent;
    }
    None
}

/// Parse an `entry<X>(…) { … }` statement inside an `entryProvider` lambda.
///
/// With `()` or multiple/named arguments, `tree-sitter-kotlin-ng` produces a
/// `call_expression` with `type_arguments`. Without `()`, or with a single
/// positional argument `(arg)`, the grammar parses `entry<X> …` as a pair of
/// `<` / `>` `binary_expression` nodes whose right-hand operand is the
/// `lambda_literal` (or a `call_expression` wrapping `(arg) { … }`).
fn parse_entry_statement<'t>(
    cx: &FileCx<'_>,
    stmt: TsNode<'t>,
) -> Option<(Option<String>, TsNode<'t>)> {
    match stmt.kind() {
        "call_expression" => {
            // `entry<X>(args) { … }` nests as `call(call(entry, <X>, (args)),
            // lambda)`; `entry<X> { … }` with no `()` is one call.
            let head = match stmt.named_child(0)? {
                inner if inner.kind() == "call_expression" => inner,
                _ => stmt,
            };
            let callee = head.named_child(0)?;
            if callee.kind() != "identifier" || cx.text(&callee) != "entry" {
                return None;
            }
            let type_args = type_arguments(head)?;
            let mut tw = type_args.walk();
            let proj_count = type_args
                .named_children(&mut tw)
                .filter(|c| c.kind() == "type_projection")
                .count();
            if proj_count != 1 {
                return None;
            }
            let entry_lambda = trailing_lambda(stmt)?;
            Some((single_type_arg_name(cx, type_args), entry_lambda))
        }
        "binary_expression" => {
            let (callee_name, type_name, right) = split_generic_binary(cx, stmt)?;
            if callee_name != "entry" {
                return None;
            }
            let entry_lambda = match right.kind() {
                "lambda_literal" => right,
                "call_expression"
                    if right
                        .named_child(0)
                        .is_some_and(|c| c.kind() == "parenthesized_expression") =>
                {
                    trailing_lambda(right)?
                }
                _ => return None,
            };
            Some((type_name, entry_lambda))
        }
        _ => None,
    }
}

/// Decompose the `<` / `>` `binary_expression` pair that `tree-sitter-kotlin-ng`
/// emits for `callee<Type> …` when the token after `>` is `{` or `(singleArg)`.
fn split_generic_binary<'t>(
    cx: &FileCx<'_>,
    node: TsNode<'t>,
) -> Option<(String, Option<String>, TsNode<'t>)> {
    if node.kind() != "binary_expression" {
        return None;
    }
    let op_gt = node.child_by_field_name("operator")?;
    if cx.text(&op_gt) != ">" {
        return None;
    }
    let right = node.child_by_field_name("right")?;
    let left = node.child_by_field_name("left")?;
    if left.kind() != "binary_expression" {
        return None;
    }
    let op_lt = left.child_by_field_name("operator")?;
    if cx.text(&op_lt) != "<" {
        return None;
    }
    let callee = left.child_by_field_name("left")?;
    let callee_name = dotted_identifiers(cx, callee)?;
    let type_node = left.child_by_field_name("right")?;
    Some((callee_name, dotted_identifiers(cx, type_node), right))
}

/// Every property declared in the file, keyed as `proven_name` looks up
/// shadowing: `Outer.Inner.name` inside types, the bare name elsewhere. Locals
/// inside functions count too, which can only refuse a proof.
fn declared_properties(cx: &FileCx<'_>, root: TsNode<'_>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() == "property_declaration"
            && let Some(name) = property_name(cx, node)
        {
            let chain = enclosing_type_chain(cx, node);
            if chain.is_empty() {
                out.insert(name);
            } else {
                out.insert(format!("{}.{name}", chain.join(".")));
            }
        }
    }
    out
}

fn property_name(cx: &FileCx<'_>, prop: TsNode<'_>) -> Option<String> {
    let mut walk = prop.walk();
    let var_decl = prop
        .named_children(&mut walk)
        .find(|c| c.kind() == "variable_declaration")?;
    let mut vw = var_decl.walk();
    let ident = var_decl
        .named_children(&mut vw)
        .find(|c| c.kind() == "identifier")?;
    let text = cx.text(&ident);
    (!text.is_empty()).then(|| text.to_string())
}

fn property_initializer(prop: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut walk = prop.walk();
    let mut saw_eq = false;
    for child in prop.children(&mut walk) {
        if !child.is_named() && child.kind() == "=" {
            saw_eq = true;
            continue;
        }
        if saw_eq && child.is_named() {
            return Some(child);
        }
        if child.kind() == "property_delegate" {
            return child.named_child(0);
        }
    }
    None
}

fn single_call_arg<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
    let args = value_arguments(call)?;
    let mut aw = args.walk();
    let vargs: Vec<_> = args
        .named_children(&mut aw)
        .filter(|c| c.kind() == "value_argument")
        .collect();
    let [arg] = vargs.as_slice() else {
        return None;
    };
    value_argument_expr(*arg)
}

fn mutable_state_list_of_nav_key<'t>(
    cx: &FileCx<'_>,
    node: TsNode<'t>,
    imports: &Imports,
    local_functions: &HashSet<String>,
    local_types: &HashSet<String>,
) -> Option<Option<TsNode<'t>>> {
    let (callee_name, type_name, initial_arg) = match node.kind() {
        "call_expression" => {
            let callee = node.named_child(0)?;
            let callee_name = dotted_identifiers(cx, callee)?;
            let type_args = type_arguments(node)?;
            let type_name = single_type_arg_name(cx, type_args)?;
            (callee_name, type_name, single_call_arg(node))
        }
        "binary_expression" => {
            let Some((callee_name, Some(type_name), right)) = split_generic_binary(cx, node) else {
                return None;
            };
            if right.kind() != "parenthesized_expression" {
                return None;
            }
            (callee_name, type_name, right.named_child(0))
        }
        _ => return None,
    };
    // Import-proven like every framework name: a same-package
    // `mutableStateListOf` from another file must not pass for Compose's.
    let list_ok = proven_name(
        cx,
        node,
        &callee_name,
        COMPOSE_PACKAGE,
        "mutableStateListOf",
        imports,
        local_functions,
    );
    if !list_ok {
        return None;
    }
    proven_name(
        cx,
        node,
        &type_name,
        RUNTIME_PACKAGE,
        "NavKey",
        imports,
        local_types,
    )
    .then_some(initial_arg)
}

fn back_stack_init<'t>(
    cx: &FileCx<'_>,
    init: TsNode<'t>,
    imports: &Imports,
    local_functions: &HashSet<String>,
    local_types: &HashSet<String>,
) -> Option<Option<TsNode<'t>>> {
    if let Some(initial_arg) =
        mutable_state_list_of_nav_key(cx, init, imports, local_functions, local_types)
    {
        return Some(initial_arg);
    }
    if init.kind() != "call_expression" {
        return None;
    }
    let callee = init.named_child(0)?;
    let callee_name = dotted_identifiers(cx, callee)?;
    if proven_name(
        cx,
        init,
        &callee_name,
        RUNTIME_PACKAGE,
        "rememberNavBackStack",
        imports,
        local_functions,
    ) {
        return Some(single_call_arg(init));
    }
    if !proven_name(
        cx,
        init,
        &callee_name,
        FLOW_PACKAGE,
        "MutableStateFlow",
        imports,
        local_functions,
    ) {
        return None;
    }
    let inner = single_call_arg(init)?;
    mutable_state_list_of_nav_key(cx, inner, imports, local_functions, local_types)
}

fn value_argument_expr(arg: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut walk = arg.walk();
    let children: Vec<_> = arg.named_children(&mut walk).collect();
    children.last().copied()
}

fn value_argument_name_and_expr<'t>(
    cx: &FileCx<'_>,
    arg: TsNode<'t>,
) -> Option<(Option<String>, TsNode<'t>)> {
    let mut walk = arg.walk();
    let children: Vec<_> = arg.named_children(&mut walk).collect();
    match children.as_slice() {
        [expr] => Some((None, *expr)),
        [key, expr] if key.kind() == "identifier" => Some((Some(cx.text(key).to_string()), *expr)),
        _ => None,
    }
}

/// Whether `name` is declared as a parameter or local variable strictly inside
/// `boundary` on the path to `site`.
fn is_shadowed(cx: &FileCx<'_>, site: TsNode<'_>, boundary: TsNode<'_>, name: &str) -> bool {
    let mut cur = site;
    while let Some(parent) = cur.parent() {
        if parent.id() == boundary.id() {
            break;
        }
        if parent.kind() == "lambda_literal" {
            for param in lambda_param_names(cx, parent) {
                if param == name {
                    return true;
                }
            }
        }
        if parent.kind() == "function_declaration" {
            for (_, param, _) in function_parameters(cx, parent) {
                if param == name {
                    return true;
                }
            }
        }
        if matches!(parent.kind(), "block" | "lambda_literal") {
            let mut walk = parent.walk();
            for stmt in parent.named_children(&mut walk) {
                if stmt.end_byte() <= site.start_byte()
                    && stmt.kind() == "property_declaration"
                    && property_name(cx, stmt).as_deref() == Some(name)
                {
                    return true;
                }
            }
        }
        cur = parent;
    }
    false
}

fn lambda_param_names(cx: &FileCx<'_>, lambda: TsNode<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let mut walk = lambda.walk();
    let Some(params) = lambda
        .named_children(&mut walk)
        .find(|c| c.kind() == "lambda_parameters")
    else {
        return out;
    };
    let mut pw = params.walk();
    for decl in params.named_children(&mut pw) {
        if decl.kind() == "variable_declaration" {
            let mut vw = decl.walk();
            if let Some(id) = decl
                .named_children(&mut vw)
                .find(|c| c.kind() == "identifier")
            {
                out.push(cx.text(&id).to_string());
            }
        }
    }
    out
}

fn function_parameters<'t>(cx: &FileCx<'_>, func: TsNode<'t>) -> Vec<(usize, String, TsNode<'t>)> {
    let mut out = Vec::new();
    let mut walk = func.walk();
    let Some(params) = func
        .named_children(&mut walk)
        .find(|c| c.kind() == "function_value_parameters")
    else {
        return out;
    };
    let mut pw = params.walk();
    for (idx, param) in params
        .named_children(&mut pw)
        .filter(|c| c.kind() == "parameter")
        .enumerate()
    {
        let mut inner = param.walk();
        let children: Vec<_> = param.named_children(&mut inner).collect();
        if let [ident, ty] = children.as_slice()
            && ident.kind() == "identifier"
        {
            out.push((idx, cx.text(ident).to_string(), *ty));
        }
    }
    out
}

fn is_back_stack_prop_ref(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    back_stacks: &HashSet<String>,
    boundary: TsNode<'_>,
) -> bool {
    match node.kind() {
        "identifier" => {
            let name = cx.text(&node);
            back_stacks.contains(name) && !is_shadowed(cx, node, boundary, name)
        }
        "navigation_expression" => {
            let mut walk = node.walk();
            let parts: Vec<_> = node.named_children(&mut walk).collect();
            let [recv, member] = parts.as_slice() else {
                return false;
            };
            recv.kind() == "this_expression"
                && member.kind() == "identifier"
                && back_stacks.contains(cx.text(member))
        }
        _ => false,
    }
}

fn is_back_stack_receiver(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    back_stacks: &HashSet<String>,
    boundary: TsNode<'_>,
) -> bool {
    if is_back_stack_prop_ref(cx, node, back_stacks, boundary) {
        return true;
    }
    if node.kind() == "navigation_expression" {
        let mut walk = node.walk();
        let parts: Vec<_> = node.named_children(&mut walk).collect();
        if let [recv, member] = parts.as_slice()
            && member.kind() == "identifier"
            && cx.text(member) == "value"
        {
            return is_back_stack_prop_ref(cx, *recv, back_stacks, boundary);
        }
    }
    false
}

fn in_back_stack_apply(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    back_stacks: &HashSet<String>,
    boundary: TsNode<'_>,
) -> bool {
    let mut cur = call;
    while let Some(parent) = cur.parent() {
        if parent.kind() == "annotated_lambda"
            && let Some(apply_call) = parent.parent()
            && apply_call.kind() == "call_expression"
            && let Some(callee) = apply_call.named_child(0)
            && callee.kind() == "navigation_expression"
        {
            let mut walk = callee.walk();
            let parts: Vec<_> = callee.named_children(&mut walk).collect();
            if let [recv, method] = parts.as_slice()
                && method.kind() == "identifier"
                && cx.text(method) == "apply"
            {
                return is_back_stack_receiver(cx, *recv, back_stacks, boundary);
            }
        }
        if parent.id() == boundary.id() {
            break;
        }
        cur = parent;
    }
    false
}

/// If `call` is `p.add(arg)`, `p.value.add(arg)`, or `add(arg)` inside
/// `p.apply { … }` / `p.value.apply { … }`, returns `Some(arg)`.
fn back_stack_add_arg<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    back_stacks: &HashSet<String>,
    boundary: TsNode<'t>,
) -> Option<TsNode<'t>> {
    if trailing_lambda(call).is_some() {
        return None;
    }
    let args = value_arguments(call)?;
    let mut aw = args.walk();
    let vargs: Vec<_> = args
        .named_children(&mut aw)
        .filter(|c| c.kind() == "value_argument")
        .collect();
    // `add(element)` or `MutableList.add(index, element)`: the key is last.
    let (&[varg] | &[_, varg]) = vargs.as_slice() else {
        return None;
    };
    let arg = value_argument_expr(varg)?;
    let callee = call.named_child(0)?;
    match callee.kind() {
        "identifier" => (cx.text(&callee) == "add"
            && in_back_stack_apply(cx, call, back_stacks, boundary))
        .then_some(arg),
        "navigation_expression" => {
            let mut walk = callee.walk();
            let parts: Vec<_> = callee.named_children(&mut walk).collect();
            let [recv, method] = parts.as_slice() else {
                return None;
            };
            if method.kind() != "identifier" || cx.text(method) != "add" {
                return None;
            }
            if recv.kind() == "this_expression" {
                in_back_stack_apply(cx, call, back_stacks, boundary).then_some(arg)
            } else {
                is_back_stack_receiver(cx, *recv, back_stacks, boundary).then_some(arg)
            }
        }
        _ => None,
    }
}

fn is_assigned(cx: &FileCx<'_>, root: TsNode<'_>, name: &str) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "assignment"
            && let Some(left) = node.child_by_field_name("left")
            && cx.text(&left).trim() == name
        {
            return true;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    false
}

/// Calls in `root` in source order, without descending into nested class or
/// object declarations.
fn walk_calls<'t>(root: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            out.push(node);
        }
        let mut walk = node.walk();
        let children: Vec<_> = node
            .named_children(&mut walk)
            .filter(|c| !matches!(c.kind(), "class_declaration" | "object_declaration"))
            .collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

fn walk_local_vals<'t>(root: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "property_declaration" {
            let mut walk = node.walk();
            let is_val = node
                .children(&mut walk)
                .any(|c| !c.is_named() && c.kind() == "val");
            if is_val {
                out.push(node);
            }
        }
        let mut walk = node.walk();
        let children: Vec<_> = node
            .named_children(&mut walk)
            .filter(|c| !matches!(c.kind(), "class_declaration" | "object_declaration"))
            .collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

fn function_type_return<'t>(ty: TsNode<'t>) -> Option<TsNode<'t>> {
    if ty.kind() != "function_type" {
        return None;
    }
    let mut walk = ty.walk();
    let mut saw_arrow = false;
    for child in ty.children(&mut walk) {
        if !child.is_named() && child.kind() == "->" {
            saw_arrow = true;
            continue;
        }
        if saw_arrow && child.is_named() {
            return Some(child);
        }
    }
    None
}

/// Whether `site` lies in an `if`/`when` branch, the right side of `?:`, or
/// a `?.let`/`?.run`/`?.also` lambda inside `boundary`.
fn conditional(cx: &FileCx<'_>, site: TsNode<'_>, boundary: TsNode<'_>) -> bool {
    let mut child = site;
    while let Some(parent) = child.parent() {
        let branch = match parent.kind() {
            "if_expression" => parent.child_by_field_name("condition") != Some(child),
            "when_entry" => {
                let mut walk = parent.walk();
                let mut arrow_end = None;
                for tok in parent.children(&mut walk) {
                    if !tok.is_named() && tok.kind() == "->" {
                        arrow_end = Some(tok.end_byte());
                        break;
                    }
                }
                arrow_end.is_none_or(|end| child.start_byte() >= end)
            }
            "binary_expression" => {
                parent.child_by_field_name("right") == Some(child)
                    && parent
                        .child_by_field_name("operator")
                        .is_some_and(|op| cx.text(&op) == "?:")
            }
            "call_expression" => {
                parent.named_child(0) != Some(child) && is_safe_scope_call(cx, parent)
            }
            _ => false,
        };
        if branch {
            return true;
        }
        if parent.id() == boundary.id() {
            break;
        }
        child = parent;
    }
    false
}

fn is_safe_scope_call(cx: &FileCx<'_>, call: TsNode<'_>) -> bool {
    let Some(callee) = call.named_child(0) else {
        return false;
    };
    if callee.kind() != "navigation_expression" {
        return false;
    }
    let mut walk = callee.walk();
    let children: Vec<_> = callee.children(&mut walk).collect();
    let [_, op, method] = children.as_slice() else {
        return false;
    };
    cx.text(op) == "?."
        && method.kind() == "identifier"
        && matches!(cx.text(method), "let" | "run" | "also")
}

struct TargetScope<'a, 't> {
    cx: &'a FileCx<'a>,
    package: Option<&'a str>,
    imports: &'a Imports,
    chain: &'a [String],
    class_decl: TsNode<'t>,
    class_props: &'a HashSet<String>,
    file_props: &'a HashSet<String>,
    local_functions: &'a HashSet<String>,
}

impl TargetScope<'_, '_> {
    fn is_local_or_property(&self, site: TsNode<'_>, name: &str) -> bool {
        if name == "it" {
            return true;
        }
        if is_shadowed(self.cx, site, self.class_decl, name) {
            return true;
        }
        self.class_props.contains(name) || self.file_props.contains(name)
    }

    fn classify(&self, expr: Option<TsNode<'_>>) -> NavTarget {
        let Some(expr) = expr.map(unwrap_parens) else {
            return NavTarget::Computed;
        };
        match expr.kind() {
            "identifier" => {
                let ident = self.cx.text(&expr);
                if ident.is_empty() || self.is_local_or_property(expr, ident) {
                    return NavTarget::Computed;
                }
                NavTarget::ObjectKey(TypeRef::from_name(
                    ident,
                    self.package,
                    self.imports,
                    self.chain,
                ))
            }
            "navigation_expression" => {
                let Some(dotted) = dotted_identifiers(self.cx, expr) else {
                    return NavTarget::Computed;
                };
                let first = dotted.split('.').next().unwrap_or(&dotted);
                if self.is_local_or_property(expr, first) {
                    return NavTarget::Computed;
                }
                NavTarget::ObjectKey(TypeRef::from_name(
                    &dotted,
                    self.package,
                    self.imports,
                    self.chain,
                ))
            }
            "call_expression" => {
                if trailing_lambda(expr).is_some() || value_arguments(expr).is_none() {
                    return NavTarget::Computed;
                }
                let Some(callee) = expr.named_child(0) else {
                    return NavTarget::Computed;
                };
                let Some(callee_name) = dotted_identifiers(self.cx, callee) else {
                    return NavTarget::Computed;
                };
                let first = callee_name.split('.').next().unwrap_or(&callee_name);
                if self.is_local_or_property(callee, first) {
                    return NavTarget::Computed;
                }
                if !callee_name.contains('.') {
                    let qualified = if self.chain.is_empty() {
                        callee_name.clone()
                    } else {
                        format!("{}.{callee_name}", self.chain.join("."))
                    };
                    if self.local_functions.contains(&qualified)
                        || self.local_functions.contains(&callee_name)
                    {
                        return NavTarget::Computed;
                    }
                }
                NavTarget::ConstructorKey(TypeRef::from_name(
                    &callee_name,
                    self.package,
                    self.imports,
                    self.chain,
                ))
            }
            _ => NavTarget::Computed,
        }
    }
}

fn unwrap_parens(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(inner) = node.named_child(0) else {
            break;
        };
        node = inner;
    }
    node
}

fn select_call_argument<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    param_idx: usize,
    param_name: &str,
) -> Option<TsNode<'t>> {
    let args = value_arguments(call)?;
    let mut walk = args.walk();
    let vargs: Vec<_> = args
        .named_children(&mut walk)
        .filter(|c| c.kind() == "value_argument")
        .collect();
    for (idx, varg) in vargs.into_iter().enumerate() {
        let (name, expr) = value_argument_name_and_expr(cx, varg)?;
        if let Some(name) = name {
            if name == param_name {
                return Some(expr);
            }
        } else if idx == param_idx {
            return Some(expr);
        }
    }
    None
}

fn last_lambda_statement(lambda: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut walk = lambda.walk();
    lambda
        .named_children(&mut walk)
        .filter(|c| c.kind() != "lambda_parameters")
        .last()
}

fn class_property_and_param_names(cx: &FileCx<'_>, class_decl: TsNode<'_>) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut walk = class_decl.walk();
    for child in class_decl.named_children(&mut walk) {
        if child.kind() == "primary_constructor" {
            let mut cw = child.walk();
            for param in child.named_children(&mut cw) {
                if param.kind() == "class_parameter" {
                    let mut pw = param.walk();
                    if let Some(id) = param
                        .named_children(&mut pw)
                        .find(|c| c.kind() == "identifier")
                    {
                        names.insert(cx.text(&id).to_string());
                    }
                }
            }
        } else if child.kind() == "class_body" {
            let mut bw = child.walk();
            for member in child.named_children(&mut bw) {
                if member.kind() == "property_declaration"
                    && let Some(name) = property_name(cx, member)
                {
                    names.insert(name);
                }
            }
        }
    }
    names
}

fn extract_function_info(
    cx: &FileCx<'_>,
    func: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    local_functions: &HashSet<String>,
) -> FunctionInfo {
    let params: Vec<String> = function_parameters(cx, func)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    let param_set: HashSet<&str> = params.iter().map(String::as_str).collect();
    let mut used_params = HashSet::new();
    let mut forwards = Vec::new();

    let mut fw = func.walk();
    let Some(body) = func
        .named_children(&mut fw)
        .find(|c| c.kind() == "function_body")
    else {
        return FunctionInfo {
            params,
            used_params,
            forwards,
        };
    };
    for ident in walk_identifiers(body) {
        let name = cx.text(&ident);
        if param_set.contains(name)
            && is_value_reference(ident)
            && !is_shadowed(cx, ident, body, name)
        {
            used_params.insert(name.to_string());
        }
    }

    for call in walk_calls(body) {
        if let Some(callee) = call.named_child(0)
            && callee.kind() == "identifier"
        {
            let name = cx.text(&callee);
            if param_set.contains(name) && !is_shadowed(cx, callee, body, name) {
                continue;
            }
            if is_value_in_scope(cx, callee, name) {
                continue;
            }
        }
        let Some(rc) = extract_render_call(cx, call, package, imports, local_functions) else {
            continue;
        };
        let Some(args) = value_arguments(call) else {
            continue;
        };
        let mut aw = args.walk();
        for (arg_idx, varg) in args
            .named_children(&mut aw)
            .filter(|c| c.kind() == "value_argument")
            .enumerate()
        {
            let Some((arg_name, expr)) = value_argument_name_and_expr(cx, varg) else {
                continue;
            };
            let expr = unwrap_parens(expr);
            if expr.kind() == "identifier" {
                let passed = cx.text(&expr);
                if param_set.contains(passed) && !is_shadowed(cx, expr, body, passed) {
                    forwards.push(ForwardCall {
                        source_param: passed.to_string(),
                        target_call: rc.clone(),
                        target_arg_name: arg_name,
                        target_arg_idx: arg_idx,
                    });
                }
            }
        }
    }

    FunctionInfo {
        params,
        used_params,
        forwards,
    }
}

/// Every identifier under `root`, not descending into nested type declarations.
fn walk_identifiers<'t>(root: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            out.push(node);
        }
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            if !matches!(child.kind(), "class_declaration" | "object_declaration") {
                stack.push(child);
            }
        }
    }
    out
}

/// Whether an identifier reads a value: not a named-argument label, a member
/// name after `.`, or the name in a declaration.
fn is_value_reference(ident: TsNode<'_>) -> bool {
    let Some(parent) = ident.parent() else {
        return true;
    };
    match parent.kind() {
        "value_argument" => parent.named_child_count() < 2 || parent.named_child(0) != Some(ident),
        "navigation_expression" => parent.named_child(0) == Some(ident),
        "variable_declaration"
        | "parameter"
        | "lambda_parameters"
        | "function_declaration"
        | "class_parameter"
        | "type_identifier" => false,
        _ => true,
    }
}

/// Extract Navigation 3 entries and back-stack navigations from one file.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    local_functions: &HashSet<String>,
    local_types: &HashSet<String>,
) -> Pending {
    let mut pending = Pending::default();
    extract_entries(cx, root, package, imports, local_functions, &mut pending);

    let mut file_props = HashSet::new();
    let mut rw = root.walk();
    for top in root.named_children(&mut rw) {
        if top.kind() == "property_declaration"
            && let Some(name) = property_name(cx, top)
        {
            file_props.insert(name);
        }
    }

    let mut stack = vec![root];
    while let Some(decl) = stack.pop() {
        let mut walk = decl.walk();
        let children: Vec<_> = decl.named_children(&mut walk).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
        if decl.kind() == "function_declaration" {
            if let Some(sym) = functions_by_start.get(&decl.start_byte()).cloned() {
                let info = extract_function_info(cx, decl, package, imports, local_functions);
                pending
                    .functions
                    .entry(sym)
                    .and_modify(|existing| *existing = None)
                    .or_insert(Some(info));
            }
            continue;
        }
        if !matches!(decl.kind(), "class_declaration" | "object_declaration") {
            continue;
        }
        let Some(name_node) = decl.child_by_field_name("name") else {
            continue;
        };
        let mut bw = decl.walk();
        let Some(class_body) = decl
            .named_children(&mut bw)
            .find(|c| c.kind() == "class_body")
        else {
            continue;
        };
        let mut back_stacks = HashSet::new();
        let mut back_stack_prop_ids = HashSet::new();
        let mut back_stack_initial_args = Vec::new();
        let mut cbw = class_body.walk();
        let members: Vec<_> = class_body.named_children(&mut cbw).collect();
        for member in &members {
            if member.kind() != "property_declaration" {
                continue;
            }
            let (Some(prop_name), Some(init)) =
                (property_name(cx, *member), property_initializer(*member))
            else {
                continue;
            };
            if let Some(initial_arg) =
                back_stack_init(cx, init, imports, local_functions, local_types)
            {
                back_stacks.insert(prop_name);
                back_stack_prop_ids.insert(member.id());
                if let Some(arg) = initial_arg {
                    back_stack_initial_args.push((arg, evidence(cx, *member)));
                }
            }
        }
        if back_stacks.is_empty() {
            continue;
        }

        let mut chain = enclosing_type_chain(cx, decl);
        chain.push(cx.text(&name_node).to_string());
        let class_qualified = chain.join(".");
        let class_symbol = symbol_id(cx.id.repo, cx.path, &class_qualified);
        let class_props = class_property_and_param_names(cx, decl);

        // Identify param helpers and builder helpers in the same class.
        let mut param_helpers: HashMap<String, (usize, String)> = HashMap::new();
        let mut builder_helpers: HashMap<String, (usize, String)> = HashMap::new();
        let mut helper_internal_calls: HashSet<usize> = HashSet::new();

        for member in &members {
            if member.kind() != "function_declaration" {
                continue;
            }
            let Some(fn_name_node) = member.child_by_field_name("name") else {
                continue;
            };
            let fn_name = cx.text(&fn_name_node).to_string();
            let mut fw = member.walk();
            let Some(body) = member
                .named_children(&mut fw)
                .find(|c| c.kind() == "function_body")
            else {
                continue;
            };
            let params = function_parameters(cx, *member);
            for (param_idx, param_name, param_ty) in &params {
                if let Some(ty_name) = plain_user_type_name(cx, *param_ty)
                    && proven_name(
                        cx,
                        *member,
                        &ty_name,
                        RUNTIME_PACKAGE,
                        "NavKey",
                        imports,
                        local_types,
                    )
                    && !is_assigned(cx, body, param_name)
                {
                    let mut matched_adds = Vec::new();
                    for call in walk_calls(body) {
                        if let Some(arg) = back_stack_add_arg(cx, call, &back_stacks, decl)
                            && arg.kind() == "identifier"
                            && cx.text(&arg) == param_name
                            && !is_shadowed(cx, arg, body, param_name)
                        {
                            matched_adds.push(call.id());
                        }
                    }
                    if !matched_adds.is_empty() {
                        param_helpers.insert(fn_name.clone(), (*param_idx, param_name.clone()));
                        helper_internal_calls.extend(matched_adds);
                    }
                } else if let Some(ret_ty) = function_type_return(*param_ty)
                    && let Some(ret_name) = plain_user_type_name(cx, ret_ty)
                    && proven_name(
                        cx,
                        *member,
                        &ret_name,
                        RUNTIME_PACKAGE,
                        "NavKey",
                        imports,
                        local_types,
                    )
                {
                    let mut local_built = HashSet::new();
                    for val_decl in walk_local_vals(body) {
                        let (Some(val_name), Some(val_init)) =
                            (property_name(cx, val_decl), property_initializer(val_decl))
                        else {
                            continue;
                        };
                        if val_init.kind() == "call_expression"
                            && let Some(callee) = val_init.named_child(0)
                            && callee.kind() == "identifier"
                            && cx.text(&callee) == param_name
                            && !is_shadowed(cx, callee, body, param_name)
                            && !is_assigned(cx, body, &val_name)
                        {
                            local_built.insert(val_name);
                        }
                    }
                    let mut matched_adds = Vec::new();
                    for call in walk_calls(body) {
                        let Some(arg) = back_stack_add_arg(cx, call, &back_stacks, decl) else {
                            continue;
                        };
                        let direct_block = arg.kind() == "call_expression"
                            && arg.named_child(0).is_some_and(|c| {
                                c.kind() == "identifier" && cx.text(&c) == param_name
                            })
                            && !is_shadowed(cx, arg, body, param_name);
                        let via_local =
                            arg.kind() == "identifier" && local_built.contains(cx.text(&arg));
                        if direct_block || via_local {
                            matched_adds.push(call.id());
                        }
                    }
                    if !matched_adds.is_empty() {
                        builder_helpers.insert(fn_name.clone(), (*param_idx, param_name.clone()));
                        helper_internal_calls.extend(matched_adds);
                    }
                }
            }
        }
        // Helpers are matched at call sites by name only, so an overloaded name
        // (`go(key: NavKey)` beside `go(route: String)`) proves nothing.
        let mut declared: HashMap<&str, usize> = HashMap::new();
        for member in &members {
            if member.kind() == "function_declaration"
                && let Some(name) = member.child_by_field_name("name")
            {
                *declared.entry(cx.text(&name)).or_default() += 1;
            }
        }
        param_helpers.retain(|name, _| declared.get(name.as_str()) == Some(&1));
        builder_helpers.retain(|name, _| declared.get(name.as_str()) == Some(&1));

        let target_scope = TargetScope {
            cx,
            package,
            imports,
            chain: &chain,
            class_decl: decl,
            class_props: &class_props,
            file_props: &file_props,
            local_functions,
        };

        for (arg, initializer) in back_stack_initial_args {
            let target = target_scope.classify(Some(arg));
            if matches!(
                target,
                NavTarget::ObjectKey(_) | NavTarget::ConstructorKey(_)
            ) {
                pending.initials.push(InitialSite {
                    owner: class_symbol.clone(),
                    target,
                    evidence: initializer,
                });
            }
        }

        // Scan members for navigation sites.
        for member in &members {
            let (source, scan_root, own_helper_param, init_phase) = match member.kind() {
                "function_declaration" => {
                    let Some(sym) = functions_by_start.get(&member.start_byte()).cloned() else {
                        continue;
                    };
                    let mut fw = member.walk();
                    let Some(body) = member
                        .named_children(&mut fw)
                        .find(|c| c.kind() == "function_body")
                    else {
                        continue;
                    };
                    let fn_name = member
                        .child_by_field_name("name")
                        .map(|n| cx.text(&n).to_string())
                        .unwrap_or_default();
                    let own_param = param_helpers.get(&fn_name).map(|(_, p)| p.clone());
                    (sym, body, own_param, false)
                }
                "anonymous_initializer" => (class_symbol.clone(), *member, None, true),
                "property_declaration" => {
                    if back_stack_prop_ids.contains(&member.id()) {
                        continue;
                    }
                    let Some(init) = property_initializer(*member) else {
                        continue;
                    };
                    (class_symbol.clone(), init, None, true)
                }
                _ => continue,
            };

            for call in walk_calls(scan_root) {
                if helper_internal_calls.contains(&call.id()) {
                    continue;
                }
                if let Some(arg) = back_stack_add_arg(cx, call, &back_stacks, decl) {
                    if let Some(own_p) = &own_helper_param
                        && arg.kind() == "identifier"
                        && cx.text(&arg) == own_p
                        && !is_shadowed(cx, arg, scan_root, own_p)
                    {
                        continue;
                    }
                    let target = target_scope.classify(Some(arg));
                    pending.navigations.push(NavSite {
                        source: source.clone(),
                        target,
                        via: "direct",
                        conditional: conditional(cx, call, *member),
                        init_phase,
                        evidence: evidence(cx, call),
                    });
                    continue;
                }

                let Some(callee) = call.named_child(0) else {
                    continue;
                };
                let helper_name = match callee.kind() {
                    "identifier" => {
                        let name = cx.text(&callee);
                        (!is_shadowed(cx, callee, decl, name)).then_some(name)
                    }
                    "navigation_expression" => {
                        let mut walk = callee.walk();
                        let parts: Vec<_> = callee.named_children(&mut walk).collect();
                        match parts.as_slice() {
                            [recv, method]
                                if recv.kind() == "this_expression"
                                    && method.kind() == "identifier" =>
                            {
                                Some(cx.text(method))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let Some(helper_name) = helper_name else {
                    continue;
                };

                if let Some((param_idx, param_name)) = param_helpers.get(helper_name) {
                    let arg = select_call_argument(cx, call, *param_idx, param_name);
                    if let (Some(own_p), Some(arg_node)) = (&own_helper_param, arg)
                        && arg_node.kind() == "identifier"
                        && cx.text(&arg_node) == own_p
                        && !is_shadowed(cx, arg_node, scan_root, own_p)
                    {
                        continue;
                    }
                    let target = target_scope.classify(arg);
                    pending.navigations.push(NavSite {
                        source: source.clone(),
                        target,
                        via: "param_helper",
                        conditional: conditional(cx, call, *member),
                        init_phase,
                        evidence: evidence(cx, call),
                    });
                } else if let Some((param_idx, param_name)) = builder_helpers.get(helper_name) {
                    // `h(x) { … }` nests as `call(call(h, (x)), lambda)`.
                    let outer = call.parent().filter(|p| {
                        p.kind() == "call_expression" && p.named_child(0) == Some(call)
                    });
                    let lambda = trailing_lambda(call)
                        .or_else(|| outer.and_then(trailing_lambda))
                        .or_else(|| {
                            select_call_argument(cx, call, *param_idx, param_name)
                                .filter(|n| n.kind() == "lambda_literal")
                        });
                    let last_expr = lambda.and_then(last_lambda_statement);
                    let target = target_scope.classify(last_expr);
                    pending.navigations.push(NavSite {
                        source: source.clone(),
                        target,
                        via: "builder_helper",
                        conditional: conditional(cx, call, *member),
                        init_phase,
                        evidence: evidence(cx, call),
                    });
                }
            }
        }
    }

    pending
}

fn trace_callback_chain(
    c_sym: &str,
    c_param: &str,
    repo: &str,
    types_by_fqn: &BTreeMap<String, Option<Declared>>,
    functions_by_fqn: &BTreeMap<String, Option<Declared>>,
    functions: &BTreeMap<String, Option<FunctionInfo>>,
    known: &HashSet<String>,
) -> (bool, Vec<serde_json::Value>) {
    let mut visited: HashSet<(String, String)> = HashSet::new();
    visited.insert((c_sym.to_string(), c_param.to_string()));
    let mut forwarded = Vec::new();
    let mut read = false;
    walk_callback_chain(
        c_sym,
        c_param,
        0,
        repo,
        types_by_fqn,
        functions_by_fqn,
        functions,
        known,
        &mut visited,
        &mut forwarded,
        &mut read,
    );
    (read, forwarded)
}

#[allow(clippy::too_many_arguments)]
fn walk_callback_chain(
    cur_sym: &str,
    cur_param: &str,
    depth: usize,
    repo: &str,
    types_by_fqn: &BTreeMap<String, Option<Declared>>,
    functions_by_fqn: &BTreeMap<String, Option<Declared>>,
    functions: &BTreeMap<String, Option<FunctionInfo>>,
    known: &HashSet<String>,
    visited: &mut HashSet<(String, String)>,
    forwarded: &mut Vec<serde_json::Value>,
    read: &mut bool,
) {
    let Some(Some(info)) = functions.get(cur_sym) else {
        return;
    };
    if info.used_params.contains(cur_param) {
        *read = true;
    }
    if depth >= CALLBACK_DEPTH {
        return;
    }
    for fw in &info.forwards {
        if fw.source_param != cur_param {
            continue;
        }
        let Some(d_sym) = fw
            .target_call
            .resolve(repo, types_by_fqn, functions_by_fqn, known)
        else {
            continue;
        };
        let Some(Some(d_info)) = functions.get(&d_sym) else {
            continue;
        };
        let Some(d_param) = (match &fw.target_arg_name {
            Some(name) => Some(name.clone()),
            None => d_info.params.get(fw.target_arg_idx).cloned(),
        }) else {
            continue;
        };
        if !visited.insert((d_sym.clone(), d_param.clone())) {
            continue;
        }
        forwarded.push(serde_json::json!({
            "composable": d_sym,
            "param": d_param,
        }));
        walk_callback_chain(
            &d_sym,
            &d_param,
            depth + 1,
            repo,
            types_by_fqn,
            functions_by_fqn,
            functions,
            known,
            visited,
            forwarded,
            read,
        );
    }
}

/// Complete Navigation 3 screen and navigation resolution across the repository.
pub(crate) fn complete(
    pending: Pending,
    repo: &str,
    types_by_fqn: &BTreeMap<String, Option<Declared>>,
    functions_by_fqn: &BTreeMap<String, Option<Declared>>,
    known: &HashSet<String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let mut screens: BTreeMap<String, Node> = BTreeMap::new();
    let mut screen_entry_evidence: BTreeMap<String, EvidenceRef> = BTreeMap::new();
    let mut screen_callbacks: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    let mut render_edges: BTreeMap<(String, String), Edge> = BTreeMap::new();
    let mut gap_nodes = Vec::new();
    let mut gap_edges = Vec::new();
    let functions = &pending.functions;

    for entry in pending.entries {
        let Some(declared) = entry.key.as_ref().and_then(|k| k.resolve(types_by_fqn)) else {
            let e = &entry.evidence;
            let id = format!("gap:route:{}@{}@{}", e.repo, e.path, e.byte_start);
            gap_nodes.push(Node {
                id: id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "route-unresolved",
                    "reason": UNDECLARED_KEY_REASON,
                    "attempted_tiers": ["T0"],
                    "prov": prov(vec![e.clone()], ConfidenceTier::Gap, &format!("Gap {id}")),
                }),
            });
            continue;
        };
        let fqn = &declared.fqn;
        let screen = format!("screen:{repo}@{ROUTER}:{fqn}");
        screen_entry_evidence
            .entry(screen.clone())
            .or_insert_with(|| entry.evidence.clone());
        screens.entry(screen.clone()).or_insert_with(|| Node {
            id: screen.clone(),
            label: "Screen".into(),
            props: serde_json::json!({
                "router": ROUTER,
                "key": fqn,
                "prov": prov(
                    vec![entry.evidence.clone()],
                    ConfidenceTier::Confirmed,
                    &format!("Screen {screen}"),
                ),
            }),
        });
        for render in entry.renders {
            if let Some(dst) = render
                .call
                .resolve(repo, types_by_fqn, functions_by_fqn, known)
            {
                let c_info = functions.get(&dst).and_then(Option::as_ref);
                for cb in render.callbacks {
                    let Some(cb_sym) =
                        cb.target
                            .resolve(repo, types_by_fqn, functions_by_fqn, functions, known)
                    else {
                        continue;
                    };
                    let Some(param) = (match cb.name {
                        Some(name) => Some(name),
                        None => c_info.and_then(|ci| ci.params.get(cb.index)).cloned(),
                    }) else {
                        continue;
                    };
                    let (read, forwarded) = trace_callback_chain(
                        &dst,
                        &param,
                        repo,
                        types_by_fqn,
                        functions_by_fqn,
                        functions,
                        known,
                    );
                    screen_callbacks
                        .entry(screen.clone())
                        .or_default()
                        .push(serde_json::json!({
                            "param": param,
                            "symbol": cb_sym,
                            "composable": dst,
                            "evidence": {
                                "path": cb.evidence.path,
                                "byte_start": cb.evidence.byte_start,
                                "byte_end": cb.evidence.byte_end,
                            },
                            "read": read,
                            "forwarded": forwarded,
                        }));
                }
                render_edges
                    .entry((screen.clone(), dst.clone()))
                    .or_insert_with(|| Edge {
                        src: screen.clone(),
                        dst: dst.clone(),
                        label: "RENDERS".into(),
                        props: serde_json::json!({
                            "prov": prov(
                                vec![render.evidence],
                                ConfidenceTier::Confirmed,
                                &format!("RENDERS {screen} -> {dst}"),
                            ),
                        }),
                    });
            }
        }
    }

    for (screen_id, callbacks) in screen_callbacks {
        if callbacks.is_empty() {
            continue;
        }
        if let Some(node) = screens.get_mut(&screen_id)
            && let Some(obj) = node.props.as_object_mut()
        {
            obj.insert(
                "entry_callbacks".into(),
                serde_json::Value::Array(callbacks),
            );
        }
    }

    let mut initial_owners_by_screen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut initializers_by_screen: BTreeMap<String, Vec<EvidenceRef>> = BTreeMap::new();
    for init in pending.initials {
        if !known.contains(&init.owner) {
            continue;
        }
        let resolved_key = match &init.target {
            NavTarget::ObjectKey(type_ref) => type_ref
                .resolve(types_by_fqn)
                .filter(|d| d.object)
                .map(|d| d.fqn.clone()),
            NavTarget::ConstructorKey(type_ref) => type_ref
                .resolve(types_by_fqn)
                .filter(|d| d.constructible)
                .map(|d| d.fqn.clone()),
            NavTarget::Computed => None,
        };
        let Some(fqn) = resolved_key else {
            continue;
        };
        let screen = format!("screen:{repo}@{ROUTER}:{fqn}");
        if screens.contains_key(&screen) {
            initializers_by_screen
                .entry(screen.clone())
                .or_default()
                .push(init.evidence);
            initial_owners_by_screen
                .entry(screen)
                .or_default()
                .insert(init.owner);
        }
    }
    for (screen_id, owners) in initial_owners_by_screen {
        let Some(node) = screens.get_mut(&screen_id) else {
            continue;
        };
        let Some(first_owner) = owners.first().cloned() else {
            continue;
        };
        let Some(obj) = node.props.as_object_mut() else {
            continue;
        };
        // The initial-destination facts rest on the initializers, so the
        // Screen cites each one beside its entry registration.
        let mut cited: Vec<EvidenceRef> = screen_entry_evidence
            .get(&screen_id)
            .cloned()
            .into_iter()
            .collect();
        let mut initializers = initializers_by_screen
            .remove(&screen_id)
            .unwrap_or_default();
        initializers.sort_by_key(|e| (e.path.clone(), e.byte_start, e.byte_end));
        initializers.dedup();
        cited.extend(initializers);
        obj.insert(
            "prov".into(),
            prov(
                cited,
                ConfidenceTier::Confirmed,
                &format!("Screen {screen_id}"),
            ),
        );
        obj.insert("initial".into(), serde_json::Value::Bool(true));
        obj.insert(
            "back_stack_owner".into(),
            serde_json::Value::String(first_owner),
        );
        if owners.len() > 1 {
            let all: Vec<serde_json::Value> =
                owners.into_iter().map(serde_json::Value::String).collect();
            obj.insert("initial_owners".into(), serde_json::Value::Array(all));
        }
    }

    #[derive(Default)]
    struct Link {
        evidence: Vec<EvidenceRef>,
        sites: Vec<(EvidenceRef, bool, &'static str, bool)>,
    }
    let mut links: BTreeMap<(String, String), Link> = BTreeMap::new();

    for site in pending.navigations {
        if !known.contains(&site.source) {
            continue;
        }
        let resolved_key = match &site.target {
            NavTarget::ObjectKey(type_ref) => type_ref
                .resolve(types_by_fqn)
                .filter(|d| d.object)
                .map(|d| d.fqn.clone()),
            NavTarget::ConstructorKey(type_ref) => type_ref
                .resolve(types_by_fqn)
                .filter(|d| d.constructible)
                .map(|d| d.fqn.clone()),
            NavTarget::Computed => None,
        };
        let reason = match &resolved_key {
            None => COMPUTED_TARGET_REASON,
            Some(fqn) => {
                let screen = format!("screen:{repo}@{ROUTER}:{fqn}");
                if screens.contains_key(&screen) {
                    let link = links.entry((site.source, screen)).or_default();
                    link.sites.push((
                        site.evidence.clone(),
                        site.conditional,
                        site.via,
                        site.init_phase,
                    ));
                    link.evidence.push(site.evidence);
                    continue;
                }
                UNKNOWN_SCREEN_REASON
            }
        };
        let e = &site.evidence;
        let id = format!("gap:navigation:{}@{}@{}", e.repo, e.path, e.byte_start);
        let cited = vec![e.clone()];
        gap_nodes.push(Node {
            id: id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "kind": "navigation-unresolved",
                "reason": reason,
                "target": resolved_key,
                "attempted_tiers": ["T0"],
                "prov": prov(cited.clone(), ConfidenceTier::Gap, &format!("Gap {id}")),
            }),
        });
        gap_edges.push(Edge {
            src: site.source.clone(),
            dst: id.clone(),
            label: "NAVIGATES_TO".into(),
            props: serde_json::json!({
                "prov": prov(
                    cited,
                    ConfidenceTier::Gap,
                    &format!("NAVIGATES_TO {} -> {id}", site.source),
                ),
            }),
        });
    }

    nodes.extend(screens.into_values());
    edges.extend(render_edges.into_values());

    for ((source, screen), mut link) in links {
        link.evidence.sort_by(|a, b| {
            (&a.path, a.byte_start, a.byte_end).cmp(&(&b.path, b.byte_start, b.byte_end))
        });
        link.evidence.dedup();
        let order = |e: &EvidenceRef| (e.path.clone(), e.byte_start, e.byte_end);
        link.sites.sort_by_key(|(e, _, _, _)| order(e));
        let conditional = link.sites.iter().all(|(_, conditional, _, _)| *conditional);
        let sites: Vec<_> = link
            .sites
            .iter()
            .map(|(e, conditional, via, init_phase)| {
                let mut site = serde_json::json!({
                    "path": e.path,
                    "byte_start": e.byte_start,
                    "byte_end": e.byte_end,
                    "conditional": conditional,
                    "via": via,
                });
                if *init_phase && let Some(obj) = site.as_object_mut() {
                    obj.insert("phase".into(), serde_json::Value::String("init".into()));
                }
                site
            })
            .collect();
        edges.push(Edge {
            src: source.clone(),
            dst: screen.clone(),
            label: "NAVIGATES_TO".into(),
            props: serde_json::json!({
                "router": ROUTER,
                "router_basis": ROUTER_BASIS,
                "conditional": conditional,
                "sites": sites,
                "prov": prov(
                    link.evidence,
                    ConfidenceTier::InferredStrong,
                    &format!("NAVIGATES_TO {source} -> {screen}"),
                ),
            }),
        });
    }

    nodes.extend(gap_nodes);
    edges.extend(gap_edges);
}
