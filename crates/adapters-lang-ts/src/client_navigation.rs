//! Vue Router navigations (W11a-2; the journey ADR in #987, decision 3).
//!
//! A navigation is a `NAVIGATES_TO` edge from the Symbol whose body
//! performs it to the target `Screen`. Two shapes are read:
//!
//! - **Direct:** `this.$router.push(target)` or `.replace(target)`, in a
//!   directory with a proven Vue Router registration.
//! - **Wrapper:** a call of a relatively imported function or class static
//!   method that passes one parameter to `router.push(param)` or
//!   `.replace(param)`, where `router` is a module variable assigned only by
//!   setters of the same file, and some file calls such a setter with
//!   `this.$router` (SimpleLogin's `Navigation.setRouter(this.$router)` and
//!   `Navigation.navigateTo(Navigation.PATH.MAIN)`).
//!
//! The target is a literal or a constant-table member, in the caller's file
//! or a relatively imported class's static table. Both shapes assume that
//! `this.$router` is the registered router (Vue's plugin injection), so the
//! edges are InferredStrong. A computed target, or one that names no
//! recovered Screen, is a Gap linked by `NAVIGATES_TO`. Back navigations
//! (`router.go(-1)`, `router.back()`) are not read yet.

use crate::client_routes::{
    self, Constants, Import, Proofs, constants, evidence, exported_functions, file_candidates,
    locally_bound, member_root, parameter_names, path_value, prov,
};
use crate::{FileCx, callable};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node as TsNode;

/// Gap reason: a navigation target that is not a literal or a constant.
pub const COMPUTED_TARGET_REASON: &str =
    "navigation target is not a literal or a recovered constant";
/// Gap reason: a navigation target that names no recovered screen.
pub const UNKNOWN_SCREEN_REASON: &str = "navigation target is not a recovered screen";

/// `(file, exported owner, member)` of a function or static method.
type Key = (String, String, Option<String>);

/// A [`Key`] whose file is still the import as written, resolved at
/// directory completion.
type Callee = (Import, String, Option<String>);

#[derive(Debug, Clone)]
enum Target {
    Path(String),
    /// `Owner.FIELD.KEY` with `Owner` relatively imported.
    Static {
        file: Import,
        export: String,
        field: String,
        key: String,
    },
    /// `this.$router`.
    Router,
    Other,
}

#[derive(Debug, Clone)]
struct Call {
    /// `None` for a direct `this.$router` navigation.
    callee: Option<Callee>,
    args: Vec<Target>,
    /// The calling Symbol, or the caller's file at module level.
    source: String,
    conditional: bool,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
struct Wrapper {
    param: usize,
    var: String,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
struct Setter {
    var: String,
    param: usize,
}

/// Per-file facts carried to directory completion.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    wrappers: HashMap<Key, Wrapper>,
    setters: HashMap<Key, Setter>,
    /// `(file, module variable)` whose every assignment is a setter's.
    setter_only: HashSet<(String, String)>,
    /// Exported classes' static tables: `(file, export, field)` → table.
    statics: HashMap<(String, String, String), HashMap<String, String>>,
    calls: Vec<Call>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Pending) {
        self.wrappers.extend(other.wrappers);
        self.setters.extend(other.setters);
        self.setter_only.extend(other.setter_only);
        self.statics.extend(other.statics);
        self.calls.extend(other.calls);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for wrapper in self.wrappers.values_mut() {
            wrapper.evidence.commit_sha = commit.into();
        }
        for call in &mut self.calls {
            call.evidence.commit_sha = commit.into();
        }
    }
}

/// Whether `node` is `this.$router`.
fn this_router(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    node.kind() == "member_expression"
        && node
            .child_by_field_name("object")
            .is_some_and(|o| o.kind() == "this")
        && node
            .child_by_field_name("property")
            .is_some_and(|p| cx.text(&p) == "$router")
}

/// `(receiver, method, arguments)` of `receiver.method(…)`.
fn method_call<'tree>(
    cx: &FileCx<'_>,
    call: TsNode<'tree>,
) -> Option<(TsNode<'tree>, String, Vec<TsNode<'tree>>)> {
    let function = call.child_by_field_name("function")?;
    (function.kind() == "member_expression").then_some(())?;
    let receiver = function.child_by_field_name("object")?;
    let method = cx
        .text(&function.child_by_field_name("property")?)
        .to_string();
    let arguments = call.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    let args = arguments.named_children(&mut walk).collect();
    Some((receiver, method, args))
}

/// Whether a callable between `site` and `function` declares `name` again.
fn shadowed(cx: &FileCx<'_>, site: TsNode<'_>, function: TsNode<'_>, name: &str) -> bool {
    let mut node = site;
    while let Some(parent) = node.parent() {
        if parent.id() == function.id() {
            return false;
        }
        if callable::is_callable(parent) && parameter_names(cx, parent).iter().any(|p| p == name) {
            return true;
        }
        node = parent;
    }
    true
}

/// Whether `node` runs only on some paths of its enclosing callable: under
/// an `if` branch, a `switch` case, a `catch`, a conditional expression, or
/// the right of `&&`, `||` or `??`.
fn conditional(node: TsNode<'_>) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        if callable::is_callable(parent) {
            return false;
        }
        let branch = match parent.kind() {
            "if_statement" => parent.child_by_field_name("condition") != Some(child),
            "ternary_expression" => parent.child_by_field_name("condition") != Some(child),
            "switch_case" | "switch_default" | "catch_clause" => true,
            "binary_expression" => {
                parent.child_by_field_name("right") == Some(child)
                    && parent
                        .child_by_field_name("operator")
                        .is_some_and(|o| matches!(o.kind(), "&&" | "||" | "??"))
            }
            _ => false,
        };
        if branch {
            return true;
        }
        child = parent;
    }
    false
}

fn target(cx: &FileCx<'_>, proofs: &Proofs<'_, '_>, local: &Constants, arg: TsNode<'_>) -> Target {
    if this_router(cx, arg) {
        return Target::Router;
    }
    // A parameter or local named like a module table is not that table.
    if member_root(arg).is_some_and(|root| locally_bound(cx, arg, cx.text(&root))) {
        return Target::Other;
    }
    if let Some(path) = path_value(cx, local, arg) {
        return Target::Path(path);
    }
    // `Owner.FIELD.KEY` with `Owner` relatively imported.
    let static_member = || {
        (arg.kind() == "member_expression").then_some(())?;
        let key = cx.text(&arg.child_by_field_name("property")?).to_string();
        let object = arg.child_by_field_name("object")?;
        (object.kind() == "member_expression").then_some(())?;
        let owner = object.child_by_field_name("object")?;
        (owner.kind() == "identifier").then_some(())?;
        let field = cx
            .text(&object.child_by_field_name("property")?)
            .to_string();
        let (file, export) = proofs.relative(cx, arg, cx.text(&owner))?;
        Some(Target::Static {
            file,
            export,
            field,
            key,
        })
    };
    static_member().unwrap_or(Target::Other)
}

/// Index the file's navigation wrappers, router setters and calls.
pub(crate) fn extract<'tree>(
    cx: &FileCx<'_>,
    root: TsNode<'tree>,
    proofs: &Proofs<'_, 'tree>,
    pending: &mut Pending,
) {
    let local = constants(cx, root);
    // Module-level `let`/`var` bindings, and how often each is assigned.
    let mut module_vars = HashSet::new();
    let mut initialized: HashMap<String, usize> = HashMap::new();
    let mut walk = root.walk();
    for statement in root.named_children(&mut walk) {
        if matches!(
            statement.kind(),
            "lexical_declaration" | "variable_declaration"
        ) && statement
            .child(0)
            .is_some_and(|keyword| keyword.kind() != "const")
        {
            let mut inner = statement.walk();
            for declarator in statement.named_children(&mut inner) {
                if let Some(name) = declarator.child_by_field_name("name") {
                    let name = cx.text(&name).to_string();
                    // `let router = other` competes with any setter.
                    if declarator
                        .child_by_field_name("value")
                        .is_some_and(|v| !matches!(v.kind(), "null" | "undefined"))
                    {
                        *initialized.entry(name.clone()).or_default() += 1;
                    }
                    module_vars.insert(name);
                }
            }
        }
    }
    let nodes = callable::walk(root);
    let mut assignments: HashMap<String, usize> = initialized;
    for node in &nodes {
        if matches!(
            node.kind(),
            "assignment_expression" | "augmented_assignment_expression" | "update_expression"
        ) {
            let left = node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("argument"));
            if let Some(left) = left.filter(|l| l.kind() == "identifier") {
                *assignments.entry(cx.text(&left).to_string()).or_default() += 1;
            }
        }
    }
    let mut setter_assignments: HashMap<String, usize> = HashMap::new();
    for (export, member, function) in exported_functions(cx, root) {
        let key = (cx.path.to_string(), export.clone(), member.clone());
        let params = parameter_names(cx, function);
        for node in callable::walk(function) {
            // `var = param`: a setter.
            if node.kind() == "assignment_expression"
                && let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                )
                && left.kind() == "identifier"
                && module_vars.contains(cx.text(&left))
                && !locally_bound(cx, node, cx.text(&left))
                && right.kind() == "identifier"
                && let Some(param) = params.iter().position(|p| p == cx.text(&right))
                && !shadowed(cx, node, function, cx.text(&right))
            {
                let var = cx.text(&left).to_string();
                *setter_assignments.entry(var.clone()).or_default() += 1;
                pending.setters.insert(key.clone(), Setter { var, param });
            }
            // `var.push(param)` / `var.replace(param)`: a wrapper.
            if node.kind() == "call_expression"
                && let Some((receiver, method, args)) = method_call(cx, node)
                && matches!(method.as_str(), "push" | "replace")
                && receiver.kind() == "identifier"
                && module_vars.contains(cx.text(&receiver))
                && !locally_bound(cx, node, cx.text(&receiver))
                && let Some(arg) = args.first().filter(|a| a.kind() == "identifier")
                && let Some(param) = params.iter().position(|p| p == cx.text(arg))
                && !shadowed(cx, node, function, cx.text(arg))
            {
                pending.wrappers.entry(key.clone()).or_insert(Wrapper {
                    param,
                    var: cx.text(&receiver).to_string(),
                    evidence: evidence(cx, node),
                });
            }
        }
    }
    for (var, count) in setter_assignments {
        if assignments.get(&var) == Some(&count) {
            pending.setter_only.insert((cx.path.to_string(), var));
        }
    }
    let exported: HashMap<String, String> = exported_functions(cx, root)
        .into_iter()
        .filter_map(|(export, member, function)| {
            member.is_some().then_some(())?;
            let class = callable::direct_class(function)?;
            Some((callable::class_name(cx, class), export))
        })
        .collect();
    for ((class, field), table) in &local.statics {
        if let Some(export) = exported.get(class) {
            pending.statics.insert(
                (cx.path.to_string(), export.clone(), field.clone()),
                table.clone(),
            );
        }
    }
    // Calls of relatively imported functions, and direct navigations.
    for node in nodes {
        if node.kind() != "call_expression" {
            continue;
        }
        let Some(function) = node.child_by_field_name("function") else {
            continue;
        };
        let source =
            callable::enclosing(cx, node).unwrap_or_else(|| crate::file_id(cx.id.repo, cx.path));
        let read = |args: &[TsNode<'_>]| {
            args.iter()
                .map(|a| target(cx, proofs, &local, *a))
                .collect::<Vec<_>>()
        };
        if let Some((receiver, method, args)) = method_call(cx, node) {
            if matches!(method.as_str(), "push" | "replace") && this_router(cx, receiver) {
                pending.calls.push(Call {
                    callee: None,
                    args: read(&args[..args.len().min(1)]),
                    source,
                    conditional: conditional(node),
                    evidence: evidence(cx, node),
                });
                continue;
            }
            if receiver.kind() == "identifier"
                && let Some((file, export)) = proofs.relative(cx, node, cx.text(&receiver))
            {
                pending.calls.push(Call {
                    callee: Some((file, export, Some(method))),
                    args: read(&args),
                    source,
                    conditional: conditional(node),
                    evidence: evidence(cx, node),
                });
            }
        } else if function.kind() == "identifier"
            && let Some((file, export)) = proofs.relative(cx, node, cx.text(&function))
            && let Some(arguments) = node.child_by_field_name("arguments")
        {
            let mut aw = arguments.walk();
            let args: Vec<_> = arguments.named_children(&mut aw).collect();
            pending.calls.push(Call {
                callee: Some((file, export, None)),
                args: read(&args),
                source,
                conditional: conditional(node),
                evidence: evidence(cx, node),
            });
        }
    }
}

/// Resolve every navigation against the recovered Vue Router screens.
pub(crate) fn complete(
    pending: Pending,
    files: &HashSet<String>,
    repo: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let screens: HashSet<String> = nodes
        .iter()
        .filter(|n| n.label == "Screen" && n.props["router"] == client_routes::ROUTER)
        .map(|n| n.id.clone())
        .collect();
    if screens.is_empty() {
        return;
    }
    let resolve = |import: &Import| {
        file_candidates(import)
            .into_iter()
            .find(|candidate| files.contains(candidate))
    };
    let resolve_key =
        |(file, export, member): &Callee| Some((resolve(file)?, export.clone(), member.clone()));
    // A setter is proven when some call passes it `this.$router`.
    let mut routers: HashMap<(String, String), EvidenceRef> = HashMap::new();
    for call in &pending.calls {
        let Some(key) = call.callee.as_ref().and_then(resolve_key) else {
            continue;
        };
        if let Some(setter) = pending.setters.get(&key)
            && matches!(call.args.get(setter.param), Some(Target::Router))
            && pending
                .setter_only
                .contains(&(key.0.clone(), setter.var.clone()))
        {
            routers
                .entry((key.0.clone(), setter.var.clone()))
                .or_insert_with(|| call.evidence.clone());
        }
    }
    #[derive(Default)]
    struct Link {
        evidence: Vec<EvidenceRef>,
        /// Each call: its evidence, whether it is conditional, and how the
        /// router was proven.
        sites: Vec<(EvidenceRef, bool, &'static str)>,
    }
    let mut links: BTreeMap<(String, String), Link> = BTreeMap::new();
    let (mut gap_nodes, mut gap_edges) = (Vec::new(), Vec::new());
    for call in pending.calls {
        let (arg, mut cited, via) = match &call.callee {
            None => (call.args.first(), vec![call.evidence.clone()], "direct"),
            Some(key) => {
                let Some(key) = resolve_key(key) else {
                    continue;
                };
                let Some(wrapper) = pending.wrappers.get(&key) else {
                    continue;
                };
                let Some(setter_call) = routers.get(&(key.0.clone(), wrapper.var.clone())) else {
                    continue;
                };
                (
                    call.args.get(wrapper.param),
                    vec![
                        call.evidence.clone(),
                        wrapper.evidence.clone(),
                        setter_call.clone(),
                    ],
                    "wrapper",
                )
            }
        };
        let path = match arg {
            Some(Target::Path(path)) => Some(path.clone()),
            Some(Target::Static {
                file,
                export,
                field,
                key,
            }) => resolve(file).and_then(|file| {
                pending
                    .statics
                    .get(&(file, export.clone(), field.clone()))?
                    .get(key)
                    .cloned()
            }),
            _ => None,
        };
        let screen = path.as_ref().map(|path| format!("screen:{repo}@{path}"));
        let reason = match &screen {
            None => COMPUTED_TARGET_REASON,
            Some(screen) if !screens.contains(screen) => UNKNOWN_SCREEN_REASON,
            Some(screen) => {
                let link = links
                    .entry((call.source.clone(), screen.clone()))
                    .or_default();
                link.sites
                    .push((call.evidence.clone(), call.conditional, via));
                link.evidence.append(&mut cited);
                continue;
            }
        };
        let e = &call.evidence;
        let id = format!("gap:navigation:{}@{}@{}", e.repo, e.path, e.byte_start);
        gap_nodes.push(Node {
            id: id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "kind": "navigation-unresolved",
                "reason": reason,
                "target": path,
                "attempted_tiers": ["T0"],
                "prov": prov(cited.clone(), ConfidenceTier::Gap, &format!("Gap {id}")),
            }),
        });
        gap_edges.push(Edge {
            src: call.source.clone(),
            dst: id.clone(),
            label: "NAVIGATES_TO".into(),
            props: serde_json::json!({
                "prov": prov(cited, ConfidenceTier::Gap, &format!("NAVIGATES_TO {} -> {id}", call.source)),
            }),
        });
    }
    for ((source, screen), mut link) in links {
        link.evidence.sort_by(|a, b| {
            (&a.path, a.byte_start, a.byte_end).cmp(&(&b.path, b.byte_start, b.byte_end))
        });
        link.evidence.dedup();
        let order = |e: &EvidenceRef| (e.path.clone(), e.byte_start, e.byte_end);
        link.sites.sort_by_key(|(e, _, _)| order(e));
        // The store keeps one edge per (source, target): every call stays
        // listed with its own flag (ADR in #987, decision 3).
        let conditional = link.sites.iter().all(|(_, conditional, _)| *conditional);
        let sites: Vec<_> = link
            .sites
            .iter()
            .map(|(e, conditional, via)| {
                serde_json::json!({
                    "path": e.path,
                    "byte_start": e.byte_start,
                    "byte_end": e.byte_end,
                    "conditional": conditional,
                    "via": via,
                })
            })
            .collect();
        edges.push(Edge {
            src: source.clone(),
            dst: screen.clone(),
            label: "NAVIGATES_TO".into(),
            props: serde_json::json!({
                "router": client_routes::ROUTER,
                "conditional": conditional,
                "sites": sites,
                "router_basis": "this.$router",
                "prov": prov(link.evidence, ConfidenceTier::InferredStrong, &format!("NAVIGATES_TO {source} -> {screen}")),
            }),
        });
    }
    nodes.extend(gap_nodes);
    edges.extend(gap_edges);
}
