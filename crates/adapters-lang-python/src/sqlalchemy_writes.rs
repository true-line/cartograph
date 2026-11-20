//! SQLAlchemy deterministic (T0) data write recovery (#1066):
//! `WRITES` edges from Python functions and methods to recovered SQLAlchemy
//! `DataEntity` nodes via import-proven sessions (`scoped_session`,
//! `sessionmaker`, Flask-SQLAlchemy `<db>.session`) and `@classmethod`
//! model-helper methods resolved along class MRO.
//!
//! An assignment to a mapped column of a model instance (`alias.enabled =
//! False`) is an update of that model (#1998, AC-0884) when the instance is
//! a parameter annotated with the model class (`alias: Alias`) or a local
//! bound once by an annotated assignment (`alias: Alias = …`), the name is
//! not otherwise rebound in the function, and the attribute is a column the
//! model class itself declares. The session flush is not proven, so the edge
//! is InferredStrong with resolution `attribute-assignment`.
//!
//! `Model.meth(...)…delete()` / `.update(...)` is a write of `Model` (#1998,
//! AC-0900) when the first class in Model's MRO defining `meth` is a
//! `@classmethod` whose every `return` is `<proven session>.query(cls)`
//! followed only by query builders (`filter`, `filter_by`, `order_by`, …).
//! It is InferredStrong with resolution `model-helper` and `via` naming the
//! defining class and method.

use super::sqlalchemy::{
    BaseExpr, OrmClassDecl, OrmFile, call_arguments, is_dotted_identifier, resolve_imported_symbol,
    resolve_symbol,
};
use super::{
    BTreeMap, BTreeSet, ConfidenceTier, Edge, EvidenceRef, Extraction, FileCx, HashMap,
    ImportBinding, TsNode, enclosing_function_node, prov_from, qualified_function_name,
    scope_binding, symbol_id,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

/// Kind of data mutation performed on a SQLAlchemy model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WriteOp {
    Delete,
    Insert,
    Update,
}

impl WriteOp {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Insert => "insert",
            Self::Update => "update",
        }
    }
}

/// Candidate Part-1 session write targeting a `@classmethod`'s first parameter `P`.
#[derive(Debug, Clone)]
pub(crate) struct ClassMethodWrite {
    pub(crate) session_expr: String,
    pub(crate) op: WriteOp,
}

/// Candidate write call site inside a Python function or method body.
#[derive(Debug, Clone)]
pub(crate) enum WriteCandidate {
    /// Direct session write (`S.add`, `S.delete`, `S.add_all`, `S.query(M)...delete/update`).
    Session {
        session_expr: String,
        model_expr: String,
        op: WriteOp,
        evidence: EvidenceRef,
    },
    /// Model classmethod helper call (`X.meth(...)`).
    ModelHelper {
        class_expr: String,
        method: String,
        evidence: EvidenceRef,
    },
    /// `X.meth(...)...delete()` / `.update()` where `meth` is a classmethod
    /// returning a query of its class.
    QueryHelper {
        class_expr: String,
        method: String,
        op: WriteOp,
        evidence: EvidenceRef,
    },
    /// Column assignment on an instance annotated as model `class_expr`.
    Attribute {
        class_expr: String,
        attribute: String,
        evidence: EvidenceRef,
    },
}

/// Per-function pending write candidates collected during the file pass.
#[derive(Debug, Clone)]
pub(crate) struct FunctionWrites {
    pub(crate) qualified_name: String,
    pub(crate) candidates: Vec<WriteCandidate>,
}

struct LocalVarBinding {
    name: String,
    effective_at: usize,
    order: usize,
    model_callee: Option<String>,
}

struct ResolvedWrite {
    op: WriteOp,
    via: Option<String>,
    /// An instance column assignment, not a session or helper call.
    attribute: bool,
    evidence: EvidenceRef,
}

enum MroOutcome<'a> {
    Writer {
        class_name: &'a str,
        operations: &'a [WriteOp],
    },
    StopNoEdge,
    NotFound,
}

fn is_sqlalchemy_session_module(module: &str) -> bool {
    matches!(
        module,
        "sqlalchemy.orm" | "sqlalchemy.orm.scoping" | "sqlalchemy.orm.session"
    )
}

fn is_scoped_session_fn(callee: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    matches!(
        resolve_imported_symbol(callee, bindings),
        Some((ref m, ref s)) if is_sqlalchemy_session_module(m) && s == "scoped_session"
    )
}

fn is_sessionmaker_fn(callee: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    matches!(
        resolve_imported_symbol(callee, bindings),
        Some((ref m, ref s)) if is_sqlalchemy_session_module(m) && s == "sessionmaker"
    )
}

/// Whether `rhs` is `scoped_session(...)` or `sessionmaker(...)()` with an
/// import-proven SQLAlchemy session factory that the module never rebinds.
pub(crate) fn is_session_factory_call(
    cx: &FileCx<'_>,
    rhs: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    rebound: &BTreeSet<String>,
) -> bool {
    if rhs.kind() != "call" {
        return false;
    }
    let Some(function) = rhs.child_by_field_name("function") else {
        return false;
    };
    let kept = |callee: &str| !rebound.contains(callee.split('.').next().unwrap_or(callee));
    if is_dotted_identifier(function) {
        let callee = cx.text(&function);
        return kept(callee) && is_scoped_session_fn(callee, bindings);
    }
    if function.kind() == "call"
        && let Some(inner_fn) = function.child_by_field_name("function")
        && is_dotted_identifier(inner_fn)
    {
        let callee = cx.text(&inner_fn);
        return kept(callee) && is_sessionmaker_fn(callee, bindings);
    }
    false
}

/// Retarget commit SHAs on cached write candidates.
pub(crate) fn retarget_file(file: &mut OrmFile, commit: &str) {
    for func in &mut file.functions {
        for candidate in &mut func.candidates {
            match candidate {
                WriteCandidate::Session { evidence, .. }
                | WriteCandidate::ModelHelper { evidence, .. }
                | WriteCandidate::QueryHelper { evidence, .. }
                | WriteCandidate::Attribute { evidence, .. } => {
                    evidence.commit_sha = commit.into();
                }
            }
        }
    }
}

fn parameter_name<'a>(cx: &'a FileCx<'_>, param: TsNode<'_>) -> Option<&'a str> {
    match param.kind() {
        "identifier" => Some(cx.text(&param)),
        "default_parameter" | "typed_default_parameter" => {
            param.child_by_field_name("name").map(|n| cx.text(&n))
        }
        "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {
            let mut walk = param.walk();
            param
                .named_children(&mut walk)
                .find_map(|child| parameter_name(cx, child))
        }
        _ => None,
    }
}

fn first_parameter_name<'a>(cx: &'a FileCx<'_>, function: TsNode<'_>) -> Option<&'a str> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut walk = parameters.walk();
    parameters
        .named_children(&mut walk)
        .find_map(|param| parameter_name(cx, param))
}

fn has_parameter(cx: &FileCx<'_>, function: TsNode<'_>, name: &str) -> bool {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return false;
    };
    let mut walk = parameters.walk();
    parameters
        .named_children(&mut walk)
        .any(|param| parameter_name(cx, param) == Some(name))
}

fn is_locally_bound(cx: &FileCx<'_>, function: TsNode<'_>, name: &str) -> bool {
    let mut scope = Some(function);
    while let Some(current) = scope {
        let binding = scope_binding(cx, current, name);
        let is_param = has_parameter(cx, current, name);
        if binding.global {
            return is_param || !binding.writes.is_empty();
        }
        if is_param || !binding.writes.is_empty() {
            return true;
        }
        scope = enclosing_function_node(current);
    }
    false
}

pub(crate) fn collect_bound_identifiers(
    cx: &FileCx<'_>,
    target: TsNode<'_>,
    out: &mut Vec<String>,
) {
    match target.kind() {
        "identifier" => out.push(cx.text(&target).to_string()),
        "attribute" | "subscript" => {}
        _ => {
            let mut walk = target.walk();
            for child in target.named_children(&mut walk) {
                collect_bound_identifiers(cx, child, out);
            }
        }
    }
}

fn collect_body_nodes<'t>(node: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        if matches!(
            child.kind(),
            "function_definition" | "class_definition" | "lambda"
        ) {
            continue;
        }
        if child.kind() == "decorated_definition" {
            let mut dwalk = child.walk();
            for dchild in child.named_children(&mut dwalk) {
                if dchild.kind() == "decorator" {
                    out.push(dchild);
                    collect_body_nodes(dchild, out);
                }
            }
            continue;
        }
        out.push(child);
        collect_body_nodes(child, out);
    }
}

fn first_positional_arg(call: TsNode<'_>) -> Option<TsNode<'_>> {
    for arg in call_arguments(call) {
        match arg.kind() {
            "keyword_argument" | "dictionary_splat" => continue,
            "list_splat" => return None,
            _ => return Some(arg),
        }
    }
    None
}

/// The innermost receiver of a `.delete(...)` / `.update(...)` call chain.
enum ChainRoot<'t> {
    /// `<session>.query(<model>)`.
    Session {
        session: TsNode<'t>,
        model: TsNode<'t>,
    },
    /// `<Model>.<method>(...)`: a model classmethod that may return a query
    /// of the model (#1998, AC-0900).
    Helper { class: TsNode<'t>, method: String },
}

/// Match a call chain whose innermost receiver is `<session>.query(<model>)`
/// or `<Model>.<method>(...)` and whose outermost method is `.delete(...)` or
/// `.update(...)`.
fn query_chain_target<'t>(cx: &FileCx<'_>, call: TsNode<'t>) -> Option<(ChainRoot<'t>, WriteOp)> {
    if let Some(parent) = call.parent()
        && parent.kind() == "attribute"
        && parent.child_by_field_name("object") == Some(call)
        && parent.parent().is_some_and(|gp| {
            gp.kind() == "call" && gp.child_by_field_name("function") == Some(parent)
        })
    {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    if callee.kind() != "attribute" {
        return None;
    }
    let op = match cx.text(&callee.child_by_field_name("attribute")?).trim() {
        "delete" => WriteOp::Delete,
        "update" => WriteOp::Update,
        _ => return None,
    };
    let mut curr = callee.child_by_field_name("object")?;
    if curr.kind() != "call" {
        return None;
    }
    // Whether every step between the root and `.delete()` is a query
    // builder: a row-returning step (`.first()`) leaves no proven query.
    let mut only_builders = true;
    while curr.kind() == "call" {
        let inner_callee = curr.child_by_field_name("function")?;
        if inner_callee.kind() != "attribute" {
            return None;
        }
        let obj = inner_callee.child_by_field_name("object")?;
        let attr = cx
            .text(&inner_callee.child_by_field_name("attribute")?)
            .trim();
        if obj.kind() == "call" {
            if attr == "query" {
                return None;
            }
            only_builders &= QUERY_BUILDERS.contains(&attr);
            curr = obj;
        } else {
            if !is_dotted_identifier(obj) {
                return None;
            }
            if attr != "query" {
                if !only_builders {
                    return None;
                }
                return Some((
                    ChainRoot::Helper {
                        class: obj,
                        method: attr.to_string(),
                    },
                    op,
                ));
            }
            let model_arg = first_positional_arg(curr)?;
            if !is_dotted_identifier(model_arg) {
                return None;
            }
            return Some((
                ChainRoot::Session {
                    session: obj,
                    model: model_arg,
                },
                op,
            ));
        }
    }
    None
}

fn nearest_preceding_model<'a>(
    bindings: &'a [LocalVarBinding],
    var_name: &str,
    before_byte: usize,
) -> Option<&'a str> {
    bindings
        .iter()
        .filter(|b| b.name == var_name && b.effective_at <= before_byte)
        .max_by_key(|b| b.order)
        .and_then(|b| b.model_callee.as_deref())
}

fn analyze_function_body(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    first_param: Option<&str>,
) -> (Vec<WriteCandidate>, Vec<ClassMethodWrite>) {
    let Some(body) = function.child_by_field_name("body") else {
        return (Vec::new(), Vec::new());
    };
    let active_first_param =
        first_param.filter(|param| scope_binding(cx, function, param).writes.is_empty());

    let mut nodes = Vec::new();
    collect_body_nodes(body, &mut nodes);

    let mut bindings_in_body = Vec::new();
    for (order, &node) in nodes.iter().enumerate() {
        match node.kind() {
            "assignment" => {
                let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) else {
                    continue;
                };
                if left.kind() == "identifier" {
                    let name = cx.text(&left).to_string();
                    let model_callee = if right.kind() == "call" {
                        right
                            .child_by_field_name("function")
                            .filter(|f| is_dotted_identifier(*f))
                            .map(|f| cx.text(&f).trim().to_string())
                    } else {
                        None
                    };
                    bindings_in_body.push(LocalVarBinding {
                        name,
                        effective_at: node.end_byte(),
                        order,
                        model_callee,
                    });
                } else {
                    let mut bound = Vec::new();
                    collect_bound_identifiers(cx, left, &mut bound);
                    for name in bound {
                        bindings_in_body.push(LocalVarBinding {
                            name,
                            effective_at: node.end_byte(),
                            order,
                            model_callee: None,
                        });
                    }
                }
            }
            "augmented_assignment" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let mut bound = Vec::new();
                    collect_bound_identifiers(cx, left, &mut bound);
                    for name in bound {
                        bindings_in_body.push(LocalVarBinding {
                            name,
                            effective_at: node.end_byte(),
                            order,
                            model_callee: None,
                        });
                    }
                }
            }
            "for_statement" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let mut bound = Vec::new();
                    collect_bound_identifiers(cx, left, &mut bound);
                    for name in bound {
                        bindings_in_body.push(LocalVarBinding {
                            name,
                            effective_at: left.end_byte(),
                            order,
                            model_callee: None,
                        });
                    }
                }
            }
            "named_expression" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    bindings_in_body.push(LocalVarBinding {
                        name: cx.text(&name_node).to_string(),
                        effective_at: node.end_byte(),
                        order,
                        model_callee: None,
                    });
                }
            }
            "as_pattern" | "except_clause" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    let mut bound = Vec::new();
                    collect_bound_identifiers(cx, alias, &mut bound);
                    for name in bound {
                        bindings_in_body.push(LocalVarBinding {
                            name,
                            effective_at: alias.end_byte(),
                            order,
                            model_callee: None,
                        });
                    }
                }
            }
            "delete_statement" => {
                let mut bound = Vec::new();
                collect_bound_identifiers(cx, node, &mut bound);
                for name in bound {
                    bindings_in_body.push(LocalVarBinding {
                        name,
                        effective_at: node.end_byte(),
                        order,
                        model_callee: None,
                    });
                }
            }
            _ => {}
        }
    }

    let mut candidates = Vec::new();
    let mut classmethod_writes = Vec::new();

    let mut record_session_write = |candidates: &mut Vec<WriteCandidate>,
                                    session_expr: &str,
                                    model_expr: &str,
                                    op: WriteOp,
                                    evidence: EvidenceRef| {
        let session_head = session_expr.split('.').next().unwrap_or(session_expr);
        if is_locally_bound(cx, function, session_head) {
            return;
        }
        if active_first_param == Some(model_expr) {
            classmethod_writes.push(ClassMethodWrite {
                session_expr: session_expr.to_string(),
                op,
            });
        }
        let model_head = model_expr.split('.').next().unwrap_or(model_expr);
        if !is_locally_bound(cx, function, model_head) {
            candidates.push(WriteCandidate::Session {
                session_expr: session_expr.to_string(),
                model_expr: model_expr.to_string(),
                op,
                evidence,
            });
        }
    };

    for &node in &nodes {
        if node.kind() != "call" {
            continue;
        }
        match query_chain_target(cx, node) {
            Some((ChainRoot::Session { session, model }, op)) => {
                record_session_write(
                    &mut candidates,
                    cx.text(&session).trim(),
                    cx.text(&model).trim(),
                    op,
                    cx.evidence(&node),
                );
                continue;
            }
            Some((ChainRoot::Helper { class, method }, op)) => {
                let class_expr = cx.text(&class).trim();
                let head = class_expr.split('.').next().unwrap_or(class_expr);
                if active_first_param != Some(class_expr) && !is_locally_bound(cx, function, head) {
                    candidates.push(WriteCandidate::QueryHelper {
                        class_expr: class_expr.to_string(),
                        method,
                        op,
                        evidence: cx.evidence(&node),
                    });
                }
                continue;
            }
            None => {}
        }
        let Some(callee) = node.child_by_field_name("function") else {
            continue;
        };
        if callee.kind() != "attribute" {
            continue;
        }
        let (Some(recv_node), Some(attr_node)) = (
            callee.child_by_field_name("object"),
            callee.child_by_field_name("attribute"),
        ) else {
            continue;
        };
        if !is_dotted_identifier(recv_node) {
            continue;
        }
        let recv_expr = cx.text(&recv_node).trim();
        let recv_head = recv_expr.split('.').next().unwrap_or(recv_expr);
        if is_locally_bound(cx, function, recv_head) {
            continue;
        }
        let method = cx.text(&attr_node).trim();
        let evidence = cx.evidence(&node);

        match method {
            "add" => {
                if let Some(arg) = first_positional_arg(node) {
                    if arg.kind() == "call"
                        && let Some(m_node) = arg.child_by_field_name("function")
                        && is_dotted_identifier(m_node)
                    {
                        record_session_write(
                            &mut candidates,
                            recv_expr,
                            cx.text(&m_node).trim(),
                            WriteOp::Insert,
                            evidence.clone(),
                        );
                    } else if arg.kind() == "identifier"
                        && let Some(model_expr) = nearest_preceding_model(
                            &bindings_in_body,
                            cx.text(&arg),
                            node.start_byte(),
                        )
                    {
                        record_session_write(
                            &mut candidates,
                            recv_expr,
                            model_expr,
                            WriteOp::Insert,
                            evidence.clone(),
                        );
                    }
                }
            }
            "delete" => {
                if let Some(arg) = first_positional_arg(node)
                    && arg.kind() == "identifier"
                    && let Some(model_expr) =
                        nearest_preceding_model(&bindings_in_body, cx.text(&arg), node.start_byte())
                {
                    record_session_write(
                        &mut candidates,
                        recv_expr,
                        model_expr,
                        WriteOp::Delete,
                        evidence.clone(),
                    );
                }
            }
            "add_all" => {
                if let Some(arg) = first_positional_arg(node)
                    && arg.kind() == "list"
                {
                    let mut walk = arg.walk();
                    for elem in arg.named_children(&mut walk) {
                        if elem.kind() == "call"
                            && let Some(m_node) = elem.child_by_field_name("function")
                            && is_dotted_identifier(m_node)
                        {
                            record_session_write(
                                &mut candidates,
                                recv_expr,
                                cx.text(&m_node).trim(),
                                WriteOp::Insert,
                                evidence.clone(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }

        candidates.push(WriteCandidate::ModelHelper {
            class_expr: recv_expr.to_string(),
            method: method.to_string(),
            evidence,
        });
    }

    candidates.extend(attribute_writes(cx, function, &nodes));
    (candidates, classmethod_writes)
}

/// The model class an instance name is annotated with in `function`: a
/// parameter `name: Model` the body never rebinds, or a local whose only
/// binding is `name: Model = …`.
fn annotated_model(cx: &FileCx<'_>, function: TsNode<'_>, name: &str) -> Option<String> {
    let annotation = |node: TsNode<'_>| {
        node.child_by_field_name("type")
            .map(|t| t.named_child(0).unwrap_or(t))
            .filter(|t| is_dotted_identifier(*t))
            .map(|t| cx.text(&t).trim().to_string())
    };
    let writes = scope_binding(cx, function, name).writes;
    if let Some(parameters) = function.child_by_field_name("parameters") {
        let mut walk = parameters.walk();
        for param in parameters.named_children(&mut walk) {
            if parameter_name(cx, param) != Some(name) {
                continue;
            }
            if !writes.is_empty()
                || !matches!(param.kind(), "typed_parameter" | "typed_default_parameter")
            {
                return None;
            }
            return annotation(param);
        }
    }
    match writes.as_slice() {
        [only]
            if only.kind() == "assignment"
                && only
                    .child_by_field_name("left")
                    .is_some_and(|left| left.kind() == "identifier" && cx.text(&left) == name) =>
        {
            annotation(*only)
        }
        _ => None,
    }
}

/// `x.col = …` where `x` is annotated with a model class (see the module
/// docs); the column is checked at resolve time.
fn attribute_writes(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    nodes: &[TsNode<'_>],
) -> Vec<WriteCandidate> {
    let mut out = Vec::new();
    let mut models: HashMap<String, Option<String>> = HashMap::new();
    for node in nodes {
        if !matches!(node.kind(), "assignment" | "augmented_assignment") {
            continue;
        }
        let Some(left) = node.child_by_field_name("left") else {
            continue;
        };
        if left.kind() != "attribute" {
            continue;
        }
        let (Some(object), Some(attribute)) = (
            left.child_by_field_name("object"),
            left.child_by_field_name("attribute"),
        ) else {
            continue;
        };
        if object.kind() != "identifier" {
            continue;
        }
        let name = cx.text(&object).to_string();
        let model = models
            .entry(name.clone())
            .or_insert_with(|| annotated_model(cx, function, &name))
            .clone();
        if let Some(class_expr) = model {
            out.push(WriteCandidate::Attribute {
                class_expr,
                attribute: cx.text(&attribute).to_string(),
                evidence: cx.evidence(node),
            });
        }
    }
    out
}

fn has_classmethod_decorator(
    cx: &FileCx<'_>,
    decorated: TsNode<'_>,
    shadowed: &BTreeSet<String>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    if shadowed.contains("classmethod") || bindings.contains_key("classmethod") {
        return false;
    }
    // The builtin alone: any other decorator may replace what is installed.
    let mut walk = decorated.walk();
    let decorators: Vec<TsNode<'_>> = decorated
        .named_children(&mut walk)
        .filter(|child| child.kind() == "decorator")
        .collect();
    let [decorator] = decorators[..] else {
        return false;
    };
    let mut dwalk = decorator.walk();
    let exprs: Vec<TsNode<'_>> = decorator.named_children(&mut dwalk).collect();
    matches!(exprs[..], [expr] if expr.kind() == "identifier" && cx.text(&expr) == "classmethod")
}

/// Extract defined member names and `@classmethod` write candidates from a class body.
pub(crate) fn extract_class_methods(
    cx: &FileCx<'_>,
    class: TsNode<'_>,
    shadowed_for_import: &BTreeSet<String>,
    bindings: &HashMap<String, ImportBinding>,
) -> ClassMethods {
    let mut defined_members = BTreeSet::new();
    let mut classmethods = BTreeMap::new();
    let mut query_methods = BTreeMap::new();
    let Some(body) = class.child_by_field_name("body") else {
        return ClassMethods {
            defined_members,
            classmethods,
            query_methods,
        };
    };
    let mut walk = body.walk();
    for stmt in body.named_children(&mut walk) {
        match stmt.kind() {
            "function_definition" => {
                if let Some(name) = stmt.child_by_field_name("name") {
                    defined_members.insert(cx.text(&name).to_string());
                }
            }
            "class_definition" => {
                if let Some(name) = stmt.child_by_field_name("name") {
                    defined_members.insert(cx.text(&name).to_string());
                }
            }
            "decorated_definition" => {
                let mut dwalk = stmt.walk();
                let Some(inner) = stmt
                    .named_children(&mut dwalk)
                    .find(|c| matches!(c.kind(), "function_definition" | "class_definition"))
                else {
                    continue;
                };
                let Some(name_node) = inner.child_by_field_name("name") else {
                    continue;
                };
                let method_name = cx.text(&name_node).to_string();
                defined_members.insert(method_name.clone());
                if inner.kind() == "function_definition"
                    && has_classmethod_decorator(cx, stmt, shadowed_for_import, bindings)
                    && let Some(first_param) = first_parameter_name(cx, inner)
                {
                    let (_, writes) = analyze_function_body(cx, inner, Some(first_param));
                    if let Some(session) = returned_own_query(cx, inner, first_param) {
                        query_methods.insert(method_name.clone(), session);
                    }
                    classmethods.insert(method_name, writes);
                }
            }
            "assignment" => {
                if let Some(left) = stmt.child_by_field_name("left")
                    && left.kind() == "identifier"
                {
                    defined_members.insert(cx.text(&left).to_string());
                }
            }
            "expression_statement" => {
                let mut ewalk = stmt.walk();
                if let Some(assign) = stmt
                    .named_children(&mut ewalk)
                    .find(|c| c.kind() == "assignment")
                    && let Some(left) = assign.child_by_field_name("left")
                    && left.kind() == "identifier"
                {
                    defined_members.insert(cx.text(&left).to_string());
                }
            }
            _ => {}
        }
    }
    ClassMethods {
        defined_members,
        classmethods,
        query_methods,
    }
}

/// What a class body defines: member names, `@classmethod` session writes on
/// the class, and classmethods that return a query of the class.
pub(crate) struct ClassMethods {
    pub(crate) defined_members: BTreeSet<String>,
    pub(crate) classmethods: BTreeMap<String, Vec<ClassMethodWrite>>,
    /// Classmethod name -> the session expression of the query it returns.
    pub(crate) query_methods: BTreeMap<String, String>,
}

/// SQLAlchemy `Query` methods that return another `Query`.
const QUERY_BUILDERS: &[&str] = &[
    "filter",
    "filter_by",
    "order_by",
    "join",
    "outerjoin",
    "options",
    "where",
    "limit",
    "offset",
    "distinct",
    "group_by",
    "having",
    "enable_eagerloads",
    "yield_per",
    "execution_options",
    "select_from",
    "with_for_update",
];

/// The session expression `S` when every `return` of the classmethod
/// `function` returns `S.query(<first_param>)` followed only by query
/// builder calls, and `first_param` is never rebound (#1998, AC-0900).
fn returned_own_query(cx: &FileCx<'_>, function: TsNode<'_>, first_param: &str) -> Option<String> {
    if !scope_binding(cx, function, first_param).writes.is_empty() {
        return None;
    }
    let body = function.child_by_field_name("body")?;
    let mut nodes = Vec::new();
    collect_body_nodes(body, &mut nodes);
    let mut session = None;
    let mut returns = 0;
    for node in nodes {
        if node.kind() != "return_statement" {
            continue;
        }
        returns += 1;
        let mut curr = node.named_child(0)?;
        let found = loop {
            if curr.kind() != "call" {
                break None;
            }
            let callee = curr.child_by_field_name("function")?;
            if callee.kind() != "attribute" {
                break None;
            }
            let obj = callee.child_by_field_name("object")?;
            let attr = cx.text(&callee.child_by_field_name("attribute")?).trim();
            if obj.kind() == "call" {
                if !QUERY_BUILDERS.contains(&attr) {
                    break None;
                }
                curr = obj;
                continue;
            }
            if attr != "query" || !is_dotted_identifier(obj) {
                break None;
            }
            let model = first_positional_arg(curr)?;
            if model.kind() != "identifier" || cx.text(&model) != first_param {
                break None;
            }
            break Some(cx.text(&obj).trim().to_string());
        };
        let found = found?;
        if session.as_ref().is_some_and(|s| s != &found) {
            return None;
        }
        session = Some(found);
    }
    (returns > 0).then_some(session).flatten()
}

/// Collect per-function write candidates across the file.
pub(crate) fn extract_functions(cx: &FileCx<'_>, root: TsNode<'_>, file: &mut OrmFile) {
    let function_query = crate::queries::query("(function_definition) @function");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(function_query, root, cx.source);
    while let Some(found) = matches.next() {
        let function = found.captures()[0].node;
        let Some(qualified_name) = qualified_function_name(cx, function) else {
            continue;
        };
        let (candidates, _) = analyze_function_body(cx, function, None);
        if !candidates.is_empty() {
            file.functions.push(FunctionWrites {
                qualified_name,
                candidates,
            });
        }
    }
}

fn is_proven_session(
    file: &str,
    session_expr: &str,
    imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
    known_files: &BTreeSet<String>,
    proven_sessions: &BTreeSet<(String, String)>,
    proven_flask_dbs: &BTreeSet<(String, String)>,
) -> bool {
    if resolve_symbol(file, session_expr, imports_by_file, known_files)
        .is_some_and(|target| proven_sessions.contains(&target))
    {
        return true;
    }
    if let Some(db_expr) = session_expr.strip_suffix(".session")
        && resolve_symbol(file, db_expr, imports_by_file, known_files)
            .is_some_and(|target| proven_flask_dbs.contains(&target))
    {
        return true;
    }
    false
}

/// What a class's bases are looked up against.
struct MroScope<'a, 'b> {
    classes_by_key: &'b BTreeMap<(String, String), &'a OrmClassDecl>,
    proven_bases: &'b BTreeSet<(String, String)>,
    proven_registries: &'b BTreeSet<(String, String)>,
    proven_flask_dbs: &'b BTreeSet<(String, String)>,
    imports_by_file: &'b BTreeMap<String, HashMap<String, ImportBinding>>,
    known_files: &'b BTreeSet<String>,
}

/// One class in a method resolution order: a repo class, or a proven base
/// that defines no helper (a declarative base, `db.Model`, `object`).
#[derive(Clone)]
enum MroEntry<'a> {
    Class(&'a OrmClassDecl),
    Opaque((String, String)),
}

impl MroEntry<'_> {
    fn key(&self) -> &(String, String) {
        match self {
            MroEntry::Class(class) => &class.key,
            MroEntry::Opaque(key) => key,
        }
    }
}

/// Python's C3 linearization of `class`. `None` when a base is not a repo
/// class or a proven base (its own ancestors are unknown), on a cycle, or
/// when the hierarchy has no consistent order.
fn linearize<'a>(
    class: &'a OrmClassDecl,
    scope: &MroScope<'a, '_>,
    stack: &mut Vec<(String, String)>,
) -> Option<Vec<MroEntry<'a>>> {
    if stack.contains(&class.key) {
        return None;
    }
    stack.push(class.key.clone());
    let resolve =
        |expr: &str| resolve_symbol(&class.key.0, expr, scope.imports_by_file, scope.known_files);
    let mut seqs: Vec<Vec<MroEntry<'a>>> = Vec::new();
    for base in &class.bases {
        let seq = match base {
            BaseExpr::DeclarativeBase => {
                vec![MroEntry::Opaque((String::new(), "DeclarativeBase".into()))]
            }
            BaseExpr::InlineProvenBase => vec![MroEntry::Opaque((
                class.key.0.clone(),
                format!("{}()", class.key.1),
            ))],
            BaseExpr::GenerateBase(recv) => {
                let target = resolve(recv).filter(|t| scope.proven_registries.contains(t))?;
                vec![MroEntry::Opaque((
                    target.0,
                    format!("{}.generate_base()", target.1),
                ))]
            }
            BaseExpr::DotModel(recv) => match resolve(recv) {
                Some(target) if scope.proven_flask_dbs.contains(&target) => {
                    vec![MroEntry::Opaque((target.0, format!("{}.Model", target.1)))]
                }
                _ => {
                    let target = resolve(&format!("{recv}.Model"))?;
                    let base_class = scope.classes_by_key.get(&target).copied()?;
                    linearize(base_class, scope, stack)?
                }
            },
            BaseExpr::Symbol(sym) => {
                let target = resolve(sym)?;
                match scope.classes_by_key.get(&target).copied() {
                    Some(base_class) => linearize(base_class, scope, stack)?,
                    None if scope.proven_bases.contains(&target) => vec![MroEntry::Opaque(target)],
                    // The builtin: an unbound `object` resolves to this file.
                    None if sym == "object" && target == (class.key.0.clone(), sym.clone()) => {
                        vec![MroEntry::Opaque((String::new(), "object".into()))]
                    }
                    None => return None,
                }
            }
            BaseExpr::Unresolvable => return None,
        };
        seqs.push(seq);
    }
    seqs.push(seqs.iter().map(|seq| seq[0].clone()).collect());
    let mut order = vec![MroEntry::Class(class)];
    loop {
        seqs.retain(|seq| !seq.is_empty());
        if seqs.is_empty() {
            break;
        }
        let head = seqs.iter().map(|seq| seq[0].clone()).find(|candidate| {
            !seqs
                .iter()
                .any(|seq| seq[1..].iter().any(|e| e.key() == candidate.key()))
        })?;
        for seq in &mut seqs {
            if seq[0].key() == head.key() {
                seq.remove(0);
            }
        }
        order.push(head);
    }
    stack.pop();
    Some(order)
}

/// The first class in `class`'s C3 order that defines `method` decides: a
/// self-model writer is an edge, anything else is none.
fn resolve_mro_helper<'a>(
    class: &'a OrmClassDecl,
    method: &str,
    scope: &MroScope<'a, '_>,
    self_writers: &'a BTreeMap<(String, String), BTreeMap<String, Vec<WriteOp>>>,
) -> MroOutcome<'a> {
    let Some(order) = linearize(class, scope, &mut Vec::new()) else {
        return MroOutcome::StopNoEdge;
    };
    for entry in order {
        let MroEntry::Class(defining) = entry else {
            continue;
        };
        if !defining.defined_members.contains(method) {
            continue;
        }
        return match self_writers
            .get(&defining.key)
            .and_then(|methods| methods.get(method))
            .filter(|ops| !ops.is_empty())
        {
            Some(ops) => MroOutcome::Writer {
                class_name: &defining.name,
                operations: ops,
            },
            None => MroOutcome::StopNoEdge,
        };
    }
    MroOutcome::NotFound
}

/// The first class in `class`'s C3 order that defines `method` decides: a
/// classmethod returning a query of its class through a proven session gives
/// that defining class's name; anything else gives none.
fn resolve_mro_query<'a>(
    class: &'a OrmClassDecl,
    method: &str,
    scope: &MroScope<'a, '_>,
    proven_sessions: &BTreeSet<(String, String)>,
) -> Option<&'a str> {
    let order = linearize(class, scope, &mut Vec::new())?;
    for entry in order {
        let MroEntry::Class(defining) = entry else {
            continue;
        };
        if !defining.defined_members.contains(method) {
            continue;
        }
        let session = defining.query_methods.get(method)?;
        return is_proven_session(
            &defining.key.0,
            session,
            scope.imports_by_file,
            scope.known_files,
            proven_sessions,
            scope.proven_flask_dbs,
        )
        .then_some(defining.name.as_str());
    }
    None
}

/// Resolve all per-function write candidates across files and emit `WRITES` edges.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_writes(
    out: &mut Extraction,
    repo: &str,
    functions_by_file: Vec<(String, Vec<FunctionWrites>)>,
    classes_by_key: &BTreeMap<(String, String), &OrmClassDecl>,
    by_class_key: &HashMap<(String, String), String>,
    proven_bases: &BTreeSet<(String, String)>,
    proven_registries: &BTreeSet<(String, String)>,
    proven_flask_dbs: &BTreeSet<(String, String)>,
    proven_sessions: &BTreeSet<(String, String)>,
    imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
    known_files: &BTreeSet<String>,
) {
    // Determine which `@classmethod` definitions on repo classes are self-model writers.
    let mut self_writers: BTreeMap<(String, String), BTreeMap<String, Vec<WriteOp>>> =
        BTreeMap::new();
    for (key, &class) in classes_by_key {
        let mut method_ops = BTreeMap::new();
        for (method, writes) in &class.classmethods {
            let ops: Vec<WriteOp> = writes
                .iter()
                .filter(|w| {
                    is_proven_session(
                        &key.0,
                        &w.session_expr,
                        imports_by_file,
                        known_files,
                        proven_sessions,
                        proven_flask_dbs,
                    )
                })
                .map(|w| w.op)
                .collect();
            if !ops.is_empty() {
                method_ops.insert(method.clone(), ops);
            }
        }
        if !method_ops.is_empty() {
            self_writers.insert(key.clone(), method_ops);
        }
    }

    let scope = MroScope {
        classes_by_key,
        proven_bases,
        proven_registries,
        proven_flask_dbs,
        imports_by_file,
        known_files,
    };
    let mut resolved_by_pair: BTreeMap<(String, String), Vec<ResolvedWrite>> = BTreeMap::new();

    for (file_path, functions) in functions_by_file {
        for func in functions {
            let src = symbol_id(repo, &file_path, &func.qualified_name);
            for candidate in func.candidates {
                match candidate {
                    WriteCandidate::Session {
                        session_expr,
                        model_expr,
                        op,
                        evidence,
                    } => {
                        if !is_proven_session(
                            &file_path,
                            &session_expr,
                            imports_by_file,
                            known_files,
                            proven_sessions,
                            proven_flask_dbs,
                        ) {
                            continue;
                        }
                        let Some(model_key) =
                            resolve_symbol(&file_path, &model_expr, imports_by_file, known_files)
                        else {
                            continue;
                        };
                        let Some(entity_id) = by_class_key.get(&model_key) else {
                            continue;
                        };
                        resolved_by_pair
                            .entry((src.clone(), entity_id.clone()))
                            .or_default()
                            .push(ResolvedWrite {
                                op,
                                via: None,
                                attribute: false,
                                evidence,
                            });
                    }
                    WriteCandidate::ModelHelper {
                        class_expr,
                        method,
                        evidence,
                    } => {
                        let Some(model_key) =
                            resolve_symbol(&file_path, &class_expr, imports_by_file, known_files)
                        else {
                            continue;
                        };
                        let Some(entity_id) = by_class_key.get(&model_key) else {
                            continue;
                        };
                        let Some(model_class) = classes_by_key.get(&model_key).copied() else {
                            continue;
                        };
                        if let MroOutcome::Writer {
                            class_name,
                            operations,
                        } = resolve_mro_helper(model_class, &method, &scope, &self_writers)
                        {
                            let via = format!("{class_name}.{method}");
                            let entry = resolved_by_pair
                                .entry((src.clone(), entity_id.clone()))
                                .or_default();
                            for &op in operations {
                                entry.push(ResolvedWrite {
                                    op,
                                    via: Some(via.clone()),
                                    attribute: false,
                                    evidence: evidence.clone(),
                                });
                            }
                        }
                    }
                    WriteCandidate::QueryHelper {
                        class_expr,
                        method,
                        op,
                        evidence,
                    } => {
                        let Some(model_key) =
                            resolve_symbol(&file_path, &class_expr, imports_by_file, known_files)
                        else {
                            continue;
                        };
                        let Some(entity_id) = by_class_key.get(&model_key) else {
                            continue;
                        };
                        let Some(model_class) = classes_by_key.get(&model_key).copied() else {
                            continue;
                        };
                        if let Some(defining) =
                            resolve_mro_query(model_class, &method, &scope, proven_sessions)
                        {
                            resolved_by_pair
                                .entry((src.clone(), entity_id.clone()))
                                .or_default()
                                .push(ResolvedWrite {
                                    op,
                                    via: Some(format!("{defining}.{method}")),
                                    attribute: false,
                                    evidence,
                                });
                        }
                    }
                    WriteCandidate::Attribute {
                        class_expr,
                        attribute,
                        evidence,
                    } => {
                        let Some(model_key) =
                            resolve_symbol(&file_path, &class_expr, imports_by_file, known_files)
                        else {
                            continue;
                        };
                        let Some(entity_id) = by_class_key.get(&model_key) else {
                            continue;
                        };
                        let Some(model_class) = classes_by_key.get(&model_key).copied() else {
                            continue;
                        };
                        if !model_class.declares_column(&attribute) {
                            continue;
                        }
                        resolved_by_pair
                            .entry((src.clone(), entity_id.clone()))
                            .or_default()
                            .push(ResolvedWrite {
                                op: WriteOp::Update,
                                via: Some(format!("{}.{attribute}", model_class.name)),
                                attribute: true,
                                evidence,
                            });
                    }
                }
            }
        }
    }

    for ((src, dst), writes) in resolved_by_pair {
        let first = &writes[0];
        let operation = first.op.as_str();
        let operations: Vec<&'static str> = writes
            .iter()
            .map(|w| w.op)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(WriteOp::as_str)
            .collect();
        // The strongest write decides confidence and cites its own site.
        let session = writes.iter().find(|w| w.via.is_none());
        let helper = writes.iter().find(|w| !w.attribute);
        let (confidence, resolution, via, cited) = match (session, helper) {
            (Some(direct), _) => (ConfidenceTier::Confirmed, "session", None, direct),
            (None, Some(helper)) => (
                ConfidenceTier::InferredStrong,
                "model-helper",
                helper.via.as_deref(),
                helper,
            ),
            (None, None) => (
                ConfidenceTier::InferredStrong,
                "attribute-assignment",
                first.via.as_deref(),
                first,
            ),
        };
        let mut props = serde_json::json!({
            "operation": operation,
            "operations": operations,
            "resolution": resolution,
            "prov": prov_from(
                vec![cited.evidence.clone()],
                confidence,
                &format!("WRITES {src} -> {dst} {operation}"),
            ),
        });
        if let Some(via) = via {
            props["via"] = serde_json::Value::String(via.to_string());
        }
        out.edges.push(Edge {
            src,
            dst,
            label: "WRITES".into(),
            props,
        });
    }
}
