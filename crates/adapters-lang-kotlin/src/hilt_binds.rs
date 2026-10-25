//! Hilt `@Binds` interface-to-implementation member dispatch (W11c-3).
//!
//! When an abstract class or interface in the main source set is proven to carry
//! `@dagger.Module` and `@dagger.hilt.InstallIn(SingletonComponent::class)`,
//! each unqualified `@dagger.Binds` method (with at most `@Singleton` alongside
//! it) binds its single non-generic parameter type `Impl` to its non-generic
//! return type `Iface`. At the directory join, when `Iface` is a repository
//! interface or abstract class, `Impl` is a constructible repository class that
//! declares `Iface` as a direct supertype, and no second surviving binding in
//! the repository targets `Iface`, every direct member `m` declared on both
//! `Iface` and `Impl` receives an `InferredStrong` `CALLS` edge `Iface.m -> Impl.m`
//! citing the `@Binds` function declaration as provenance evidence.

use core_graph::Edge;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node as TsNode;

use super::{
    Declared, EXTRACTOR_ID, FileCx, Imports, annotation_class_name, declaration_kind,
    enclosing_type_chain, extension_receiver, is_abstract, receivers, symbol_id, type_candidates,
};

const MODULE_FQN: &str = "dagger.Module";
const INSTALL_IN_FQN: &str = "dagger.hilt.InstallIn";
const BINDS_FQN: &str = "dagger.Binds";
const SINGLETON_COMPONENT_FQN: &str = "dagger.hilt.components.SingletonComponent";
const SINGLETON_FQNS: &[&str] = &["javax.inject.Singleton", "jakarta.inject.Singleton"];

/// A Hilt `@Binds` declaration waiting for repo-wide interface/implementation
/// resolution at the directory join.
#[derive(Debug, Clone)]
pub(crate) struct PendingBind {
    binding: String,
    impl_candidates: Vec<String>,
    iface_candidates: Vec<String>,
    evidence: EvidenceRef,
}

pub(crate) fn retarget(binds: &mut [PendingBind], commit: &str) {
    for bind in binds {
        bind.evidence.commit_sha = commit.into();
    }
}

/// Whether `path` belongs to the main source set (or has no `src/` segment).
/// Any `src/<x>` segment with `x != "main"` fails closed.
fn in_main_source_set(path: &str) -> bool {
    let segments: Vec<&str> = path.split(['/', '\\']).collect();
    segments
        .windows(2)
        .all(|pair| pair[0] != "src" || pair[1] == "main")
}

/// Whether `raw` at `site` is proven to denote one of `expected_fqns` via a
/// fully-qualified name or an explicit import in `imports.bindings`, without
/// being shadowed by a same-named local declaration.
fn is_proven_fqn(
    cx: &FileCx<'_>,
    site: TsNode<'_>,
    raw: &str,
    expected_fqns: &[&str],
    imports: &Imports,
    local_decls: &HashSet<String>,
) -> bool {
    let raw = raw.trim();
    if raw.contains('.') {
        return expected_fqns.contains(&raw);
    }
    let chain = enclosing_type_chain(cx, site);
    for depth in 0..=chain.len() {
        let candidate = if depth == 0 {
            raw.to_string()
        } else {
            format!("{}.{raw}", chain[..depth].join("."))
        };
        if local_decls.contains(&candidate) {
            return false;
        }
    }
    imports
        .bindings
        .get(raw)
        .is_some_and(|fqn| expected_fqns.contains(&fqn.as_str()))
}

/// Dotted identifier text of a plain `user_type` with no `type_arguments` or
/// `type_modifiers`.
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

/// Dotted identifier text (`A` or `a.b.C`) of an `identifier` or `.`
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

/// Target type name `X` of a `X::class` class literal expression.
fn class_literal_type(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "navigation_expression" {
        return None;
    }
    let mut walk = node.walk();
    let children: Vec<_> = node.children(&mut walk).collect();
    let [left, op, right] = children.as_slice() else {
        return None;
    };
    if cx.text(op) != "::" || right.kind() != "identifier" || cx.text(right) != "class" {
        return None;
    }
    dotted_identifiers(cx, *left)
}

/// One individual annotation entry extracted from an `annotation` or
/// `@[...]` multi-annotation node.
struct RawAnnotation<'t> {
    type_name: Option<String>,
    args: Option<Vec<Option<TsNode<'t>>>>,
}

fn push_annotation_entries<'t>(
    cx: &FileCx<'_>,
    annotation: TsNode<'t>,
    out: &mut Vec<RawAnnotation<'t>>,
) {
    let mut walk = annotation.walk();
    let children: Vec<_> = annotation.named_children(&mut walk).collect();
    if children.is_empty() {
        out.push(RawAnnotation {
            type_name: None,
            args: None,
        });
        return;
    }
    for child in children {
        match child.kind() {
            "user_type" => out.push(RawAnnotation {
                type_name: plain_user_type_name(cx, child),
                args: None,
            }),
            "constructor_invocation" => {
                let mut inner = child.walk();
                let parts: Vec<_> = child.named_children(&mut inner).collect();
                let [user_type, value_args] = parts.as_slice() else {
                    out.push(RawAnnotation {
                        type_name: None,
                        args: None,
                    });
                    continue;
                };
                if user_type.kind() != "user_type" || value_args.kind() != "value_arguments" {
                    out.push(RawAnnotation {
                        type_name: None,
                        args: None,
                    });
                    continue;
                }
                let mut aw = value_args.walk();
                let mut parsed_args = Vec::new();
                for arg in value_args.named_children(&mut aw) {
                    if arg.kind() != "value_argument" {
                        parsed_args.push(None);
                        continue;
                    }
                    let mut vw = arg.walk();
                    let arg_parts: Vec<_> = arg.named_children(&mut vw).collect();
                    let has_spread = {
                        let mut tw = arg.walk();
                        arg.children(&mut tw).any(|tok| cx.text(&tok) == "*")
                    };
                    match arg_parts.as_slice() {
                        [expr] if !has_spread => parsed_args.push(Some(*expr)),
                        _ => parsed_args.push(None),
                    }
                }
                out.push(RawAnnotation {
                    type_name: plain_user_type_name(cx, *user_type),
                    args: Some(parsed_args),
                });
            }
            _ => out.push(RawAnnotation {
                type_name: None,
                args: None,
            }),
        }
    }
}

/// All annotation entries on a declaration, including class-level annotations
/// the grammar attached to a preceding `annotated_expression` sibling.
fn collect_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
    include_preceding: bool,
) -> Vec<RawAnnotation<'t>> {
    let mut out = Vec::new();
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for modifier in child.named_children(&mut inner) {
            if modifier.kind() == "annotation" {
                push_annotation_entries(cx, modifier, &mut out);
            }
        }
    }
    if include_preceding {
        let mut sibling = declaration.prev_named_sibling();
        while let Some(candidate) = sibling {
            if candidate.kind() != "annotated_expression" {
                break;
            }
            let mut level = candidate;
            loop {
                let mut next_level = None;
                let mut lw = level.walk();
                for child in level.named_children(&mut lw) {
                    match child.kind() {
                        "annotation" => push_annotation_entries(cx, child, &mut out),
                        "annotated_expression" => next_level = Some(child),
                        "parenthesized_expression" => {
                            let mut pw = child.walk();
                            let exprs: Vec<_> = child.named_children(&mut pw).collect();
                            if let Some(last) = out.last_mut()
                                && last.args.is_none()
                            {
                                last.args = Some(match exprs.as_slice() {
                                    [expr] => vec![Some(*expr)],
                                    _ => vec![None],
                                });
                            }
                        }
                        _ => {}
                    }
                }
                match next_level {
                    Some(next) => level = next,
                    None => break,
                }
            }
            sibling = candidate.prev_named_sibling();
        }
    }
    out
}

/// Whether `module` is an `abstract class` or `interface` carrying both
/// `@dagger.Module` and `@dagger.hilt.InstallIn(SingletonComponent::class)`.
fn is_singleton_hilt_module(
    cx: &FileCx<'_>,
    module: TsNode<'_>,
    imports: &Imports,
    local_decls: &HashSet<String>,
) -> bool {
    let kind = declaration_kind(cx, module);
    if kind != "interface" && !(kind == "class" && is_abstract(cx, module)) {
        return false;
    }
    let annotations = collect_annotations(cx, module, true);
    let mut has_module = false;
    let mut install_in_count = 0usize;
    for ann in &annotations {
        let Some(name) = ann.type_name.as_deref() else {
            continue;
        };
        if is_proven_fqn(cx, module, name, &[MODULE_FQN], imports, local_decls)
            && ann.args.as_ref().is_none_or(Vec::is_empty)
        {
            has_module = true;
        } else if name == "InstallIn"
            || name.ends_with(".InstallIn")
            || is_proven_fqn(cx, module, name, &[INSTALL_IN_FQN], imports, local_decls)
        {
            if !is_proven_fqn(cx, module, name, &[INSTALL_IN_FQN], imports, local_decls) {
                return false;
            }
            let Some(args) = ann.args.as_ref() else {
                return false;
            };
            let [Some(arg)] = args.as_slice() else {
                return false;
            };
            let Some(comp) = class_literal_type(cx, *arg) else {
                return false;
            };
            if !is_proven_fqn(
                cx,
                module,
                &comp,
                &[SINGLETON_COMPONENT_FQN],
                imports,
                local_decls,
            ) {
                return false;
            }
            install_in_count += 1;
        }
    }
    has_module && install_in_count == 1
}

/// Extract a pending binding from one `@Binds` method declaration if all
/// signature and annotation preconditions hold.
#[allow(clippy::too_many_arguments)]
fn extract_bind_function(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    local_types: &HashSet<String>,
    local_decls: &HashSet<String>,
) -> Option<PendingBind> {
    let annotations = collect_annotations(cx, function, false);
    if annotations.is_empty() {
        return None;
    }
    let mut has_binds = false;
    for ann in &annotations {
        if ann.args.as_ref().is_some_and(|args| !args.is_empty()) {
            return None;
        }
        let name = ann.type_name.as_deref()?;
        if is_proven_fqn(cx, function, name, &[BINDS_FQN], imports, local_decls) {
            has_binds = true;
        } else if is_proven_fqn(cx, function, name, SINGLETON_FQNS, imports, local_decls) {
            // @Singleton is the only other permitted annotation on a @Binds method.
        } else {
            return None;
        }
    }
    if !has_binds {
        return None;
    }

    let name_node = function.child_by_field_name("name")?;
    if extension_receiver(cx, function, name_node).is_some() {
        return None;
    }
    let mut walk = function.walk();
    let parts: Vec<TsNode<'_>> = function.named_children(&mut walk).collect();
    if parts.iter().any(|p| {
        matches!(
            p.kind(),
            "type_parameters" | "type_constraints" | "function_body"
        )
    }) {
        return None;
    }
    let params_idx = parts
        .iter()
        .position(|p| p.kind() == "function_value_parameters")?;
    if parts.len() != params_idx + 2 {
        return None;
    }
    let params_node = parts[params_idx];
    let return_node = parts[params_idx + 1];
    let iface_type = plain_user_type_name(cx, return_node)?;

    let mut pw = params_node.walk();
    let param_children: Vec<TsNode<'_>> = params_node.named_children(&mut pw).collect();
    let [param] = param_children.as_slice() else {
        return None;
    };
    if param.kind() != "parameter" {
        return None;
    }
    let mut inner = param.walk();
    let param_parts: Vec<TsNode<'_>> = param.named_children(&mut inner).collect();
    let [param_ident, param_type] = param_parts.as_slice() else {
        return None;
    };
    if param_ident.kind() != "identifier" {
        return None;
    }
    let impl_type = plain_user_type_name(cx, *param_type)?;

    if receivers::is_type_parameter(
        cx,
        function,
        impl_type.split('.').next().unwrap_or(&impl_type),
    ) || receivers::is_type_parameter(
        cx,
        function,
        iface_type.split('.').next().unwrap_or(&iface_type),
    ) {
        return None;
    }

    let impl_candidates = type_candidates(cx, imports, package, local_types, function, &impl_type);
    let iface_candidates =
        type_candidates(cx, imports, package, local_types, function, &iface_type);
    let binding = functions_by_start.get(&function.start_byte())?.clone();
    let evidence = EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: function.start_byte() as u64,
        byte_end: function.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    };
    Some(PendingBind {
        binding,
        impl_candidates,
        iface_candidates,
        evidence,
    })
}

/// Extract pending `@Binds` declarations from a single Kotlin file.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    local_types: &HashSet<String>,
    local_functions: &HashSet<String>,
    declared_importables: &[Declared],
) -> Vec<PendingBind> {
    if !in_main_source_set(cx.path) {
        return Vec::new();
    }
    let mut local_decls: HashSet<String> = local_types
        .iter()
        .chain(local_functions.iter())
        .cloned()
        .collect();
    local_decls.extend(declared_importables.iter().map(|d| d.qualified.clone()));
    let mut rw = root.walk();
    for top in root.named_children(&mut rw) {
        if top.kind() == "annotated_expression"
            && let Some(name) = annotation_class_name(cx, top)
        {
            local_decls.insert(cx.text(&name).to_string());
        }
    }

    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        let children: Vec<_> = node.named_children(&mut walk).collect();
        for child in children.into_iter().rev() {
            if matches!(
                child.kind(),
                "class_declaration" | "object_declaration" | "class_body"
            ) {
                stack.push(child);
            }
        }
        if node.kind() != "class_declaration" {
            continue;
        }
        if !is_singleton_hilt_module(cx, node, imports, &local_decls) {
            continue;
        }
        let Some(body) = ({
            let mut bw = node.walk();
            node.named_children(&mut bw)
                .find(|c| c.kind() == "class_body")
        }) else {
            continue;
        };
        let mut fw = body.walk();
        for member in body.named_children(&mut fw) {
            if member.kind() != "function_declaration" {
                continue;
            }
            if let Some(bind) = extract_bind_function(
                cx,
                member,
                package,
                imports,
                functions_by_start,
                local_types,
                &local_decls,
            ) {
                out.push(bind);
            }
        }
    }
    out
}

/// Complete the repo-wide Hilt `@Binds` join, emitting `InferredStrong`
/// `CALLS` edges from each uniquely bound interface's members to the
/// corresponding members of its implementation.
pub(crate) fn complete(
    pending: Vec<PendingBind>,
    repo: &str,
    types_by_fqn: &BTreeMap<String, Option<Declared>>,
    known: &HashSet<String>,
    edges: &mut Vec<Edge>,
) {
    let resolve_type = |candidates: &[String]| -> Option<(&str, &Declared)> {
        let fqn = candidates
            .iter()
            .find(|fqn| types_by_fqn.contains_key(fqn.as_str()))?;
        let declared = types_by_fqn.get(fqn.as_str())?.as_ref()?;
        Some((declared.fqn.as_str(), declared))
    };

    let mut by_iface: BTreeMap<String, Option<(Declared, Declared, PendingBind)>> = BTreeMap::new();
    for bind in pending {
        let Some((iface_fqn, iface)) = resolve_type(&bind.iface_candidates) else {
            continue;
        };
        let Some((impl_fqn, impl_decl)) = resolve_type(&bind.impl_candidates) else {
            continue;
        };
        if iface_fqn == impl_fqn
            || !iface.interface_or_abstract
            || !impl_decl.constructible
            || impl_decl.interface_or_abstract
        {
            continue;
        }
        let implements_iface = impl_decl
            .supers
            .iter()
            .any(|cands| resolve_type(cands).is_some_and(|(super_fqn, _)| super_fqn == iface_fqn));
        if !implements_iface {
            continue;
        }
        by_iface
            .entry(iface_fqn.to_string())
            .and_modify(|slot| *slot = None)
            .or_insert_with(|| Some((iface.clone(), impl_decl.clone(), bind)));
    }

    for slot in by_iface.into_values() {
        let Some((iface, impl_decl, bind)) = slot else {
            continue;
        };
        // Only members both declare directly: a companion's or a local
        // function shares the `Iface.name` symbol shape but is never bound.
        for m in iface.members.intersection(&impl_decl.members) {
            let src = symbol_id(repo, &iface.path, &format!("{}.{m}", iface.qualified));
            let dst = symbol_id(
                repo,
                &impl_decl.path,
                &format!("{}.{m}", impl_decl.qualified),
            );
            if src == dst || !known.contains(&dst) {
                continue;
            }
            let fact = format!(
                "CALLS {}.{m} -> {}.{m} via @Binds",
                iface.qualified, impl_decl.qualified
            );
            let provenance = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::InferredStrong,
                vec![bind.evidence.clone()],
                EXTRACTOR_ID,
                fact.as_bytes(),
            )
            .expect("Deterministic confidence is within its ceiling");
            edges.push(Edge {
                src,
                dst,
                label: "CALLS".into(),
                props: serde_json::json!({
                    "resolution": "hilt-binds",
                    "assumption": "hilt_dispatch",
                    "binding": bind.binding,
                    "prov": serde_json::to_value(provenance).expect("provenance serializes"),
                }),
            });
        }
    }
}
