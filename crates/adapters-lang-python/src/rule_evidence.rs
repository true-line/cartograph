//! Guarded local exits in Python functions as cited source observations,
//! never business verdicts (SPEC-02, #1973).
//!
//! A `return` or `raise` under same-function `if` / `elif` / `else`
//! branches becomes a `BusinessRule` carrying [`GuardedExitEvidence`], owned
//! by the enclosing function Symbol through `GOVERNS`. The branch conditions
//! and the exit value are stored as sanitized displays; the execution
//! predicate and the consumer effect stay unestablished, and every reason
//! they do is a Gap the rule `DEPENDS_ON`. The caps, omission Gaps and gap
//! vocabulary follow the TypeScript producer.

use super::{Extraction, FileCx, file_id, qualified_function_name, symbol_id};
use core_graph::rules::*;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use core_redact::{SecretKind, is_sensitive_name, recognized_secrets, rule_literal_token_shaped};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

mod definitions;

const MAX_DEPENDENCIES: usize = 128;
const MAX_CONDITIONS: usize = 32;
const MAX_ANCESTOR_STEPS: usize = 256;
const MAX_EXPRESSION_BYTES: usize = 8 * 1024;
const MAX_EXPRESSION_NODES: usize = 1024;
const MAX_CONTEXT_ANCESTORS: usize = 128;
const MAX_RULE_BYTES: usize = 64 * 1024;
const MAX_FILE_RULE_BYTES: usize = 1024 * 1024;
const MAX_RULES_PER_FILE: usize = 256;
/// Guarded effects (#1998) have their own per-file budget, so they never
/// crowd out a file's exits.
const MAX_EFFECTS_PER_FILE: usize = 128;
const WITHHELD: &str = "[REDACTED]";
const UNSUPPORTED: &str = "[UNSUPPORTED EXPRESSION]";

fn source(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    cx.evidence(&node)
}

fn reason_key(reason: RuleGapReason) -> String {
    serde_json::to_value(reason)
        .expect("fixed reason serializes")
        .as_str()
        .expect("reason is a string")
        .into()
}

fn reason_description(reason: &str) -> &'static str {
    match reason {
        "execution_predicate_unknown" => {
            "The complete execution predicate has not been established."
        }
        "consumer_semantics_unknown" => {
            "The caller's interpretation of this local return or raise is unknown."
        }
        "unresolved_call" => {
            "A call prerequisite lacks a proven target or behavioral interpretation."
        }
        "unresolved_binding" => "A value prerequisite lacks a stable scope-proven declaration.",
        "preceding_exit" => {
            "An earlier exit is present; its effect on reachability has not been analyzed."
        }
        "mutation" => {
            "Mutation is present in the callable; value changes have not been interpreted."
        }
        "loop_dependency" => {
            "Loop or accumulator behavior in the callable has not been interpreted."
        }
        "switch_control" => "Match control flow in the callable has not been interpreted.",
        "exception_control" => {
            "Exception, finally or context-manager behavior in the callable has not been interpreted."
        }
        "redacted_expression" => "An expression was withheld or incompletely captured.",
        "dependency_limit" => {
            "The dependency observation limit was reached; additional prerequisites remain unanalyzed."
        }
        "analysis_limit" => {
            "A source-rule capture limit was reached; additional analysis remains incomplete."
        }
        "unsupported_rule_syntax" => {
            "Incomplete source syntax prevented supported rule analysis; remaining file analysis was omitted."
        }
        other => RuleGapReason::describe_key(other),
    }
}

/// Where a callable boundary stops ownership: a function owns what it
/// encloses; a lambda or class body between the node and its function runs
/// at another time and owns nothing here.
fn owner_function(node: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        match current.kind() {
            "function_definition" => return Some(current),
            "lambda" | "class_definition" => return None,
            _ => {}
        }
        parent = current.parent();
    }
    None
}

/// Whether a function is a method: defined directly in a class body,
/// decorated or not, not nested in another function (AC-1030).
fn in_class(function: TsNode<'_>) -> bool {
    let mut parent = function.parent();
    while let Some(current) = parent {
        match current.kind() {
            "class_definition" => return true,
            "block" | "decorated_definition" => parent = current.parent(),
            _ => return false,
        }
    }
    false
}

fn owner_id(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    let function = owner_function(node)?;
    let name = qualified_function_name(cx, function)?;
    Some(symbol_id(cx.id.repo, cx.path, &name))
}

/// A function's parameter names, each with its declaring node.
fn parameters<'t>(cx: &FileCx<'_>, function: TsNode<'t>) -> BTreeMap<String, TsNode<'t>> {
    let mut out = BTreeMap::new();
    let Some(list) = function.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = list.walk();
    for parameter in list.named_children(&mut cursor) {
        if let Some(name) = parameter_name(parameter) {
            out.insert(cx.text(&name).to_owned(), name);
        }
    }
    out
}

/// The identifier a parameter declares: plain, typed, defaulted, or the
/// `*args` / `**kwargs` splat forms (typed or not).
fn parameter_name(parameter: TsNode<'_>) -> Option<TsNode<'_>> {
    match parameter.kind() {
        "identifier" => Some(parameter),
        "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {
            let mut cursor = parameter.walk();
            let inner = parameter.named_children(&mut cursor).find(|child| {
                matches!(
                    child.kind(),
                    "identifier" | "list_splat_pattern" | "dictionary_splat_pattern"
                )
            });
            inner.and_then(parameter_name)
        }
        "default_parameter" | "typed_default_parameter" => parameter
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier"),
        _ => None,
    }
}

/// Names a function body rebinds: assignment and augmented-assignment
/// targets, `for` / `with` / `except` targets, walrus, `del`, `global`,
/// `nonlocal`, imports, and nested `def` / `class` names. A parameter among
/// them is not a stable declaration. Nested functions, lambdas, classes and
/// comprehensions are their own scopes: only a `nonlocal` there (or a walrus
/// in a comprehension, which binds in the enclosing function) rebinds here.
/// Every name `function` binds in its own scope, with the nodes that bind
/// it (parameters aside).
fn bindings<'t>(cx: &FileCx<'_>, function: TsNode<'t>) -> BTreeMap<String, Vec<TsNode<'t>>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Scope {
        Own,
        Comprehension,
        Nested,
    }
    let mut out: BTreeMap<String, Vec<TsNode<'t>>> = BTreeMap::new();
    let Some(body) = function.child_by_field_name("body") else {
        return out;
    };
    let mut stack = vec![(body, Scope::Own)];
    while let Some((node, scope)) = stack.pop() {
        let kind = node.kind();
        let targets: Vec<TsNode<'t>> = match (kind, scope) {
            ("nonlocal_statement", _) => {
                let mut cursor = node.walk();
                node.named_children(&mut cursor).collect()
            }
            ("named_expression", Scope::Own | Scope::Comprehension) => {
                node.child_by_field_name("name").into_iter().collect()
            }
            (_, Scope::Comprehension | Scope::Nested) => Vec::new(),
            ("assignment" | "augmented_assignment" | "for_statement", _) => {
                node.child_by_field_name("left").into_iter().collect()
            }
            ("function_definition" | "class_definition", _) => {
                node.child_by_field_name("name").into_iter().collect()
            }
            ("import_statement" | "import_from_statement", _) => imported(node),
            ("as_pattern" | "delete_statement" | "global_statement" | "except_clause", _) => {
                let mut cursor = node.walk();
                node.named_children(&mut cursor).collect()
            }
            _ => Vec::new(),
        };
        for target in targets {
            let mut inner = vec![target];
            while let Some(t) = inner.pop() {
                if t.kind() == "identifier" {
                    out.entry(cx.text(&t).to_owned()).or_default().push(t);
                } else if matches!(
                    t.kind(),
                    "pattern_list"
                        | "tuple_pattern"
                        | "list_pattern"
                        | "list_splat_pattern"
                        | "as_pattern_target"
                        | "expression_list"
                        | "tuple"
                        | "list"
                ) {
                    let mut cursor = t.walk();
                    inner.extend(t.named_children(&mut cursor));
                }
            }
        }
        let child_scope = match kind {
            "function_definition" | "lambda" | "class_definition" => Scope::Nested,
            "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression"
                if scope == Scope::Own =>
            {
                Scope::Comprehension
            }
            _ => scope,
        };
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor).map(|c| (c, child_scope)));
    }
    out
}

/// The names an `import` / `from ... import` statement binds: the alias
/// when there is one, else the first segment of `import a.b` or the
/// imported name of `from m import n`.
fn imported(statement: TsNode<'_>) -> Vec<TsNode<'_>> {
    let from = statement.kind() == "import_from_statement";
    let mut cursor = statement.walk();
    statement
        .children_by_field_name("name", &mut cursor)
        .filter_map(|name| match name.kind() {
            "aliased_import" => name.child_by_field_name("alias"),
            "dotted_name" if from => {
                name.named_child(u32::try_from(name.named_child_count().checked_sub(1)?).ok()?)
            }
            "dotted_name" => name.named_child(0),
            _ => None,
        })
        .collect()
}

struct RuleBuilder<'cx, 'src, 'tree> {
    cx: &'cx FileCx<'src>,
    rule_id: String,
    gaps: BTreeMap<String, (String, TsNode<'tree>)>,
    dependencies: Vec<RuleDependency>,
    /// Condition names bound by one assignment, with it (AC-1034).
    seeds: Vec<(TsNode<'tree>, TsNode<'tree>)>,
}

impl<'tree> RuleBuilder<'_, '_, 'tree> {
    fn gap(&mut self, reason: RuleGapReason, at: TsNode<'tree>) -> String {
        let key = reason_key(reason);
        self.gaps
            .entry(key.clone())
            .or_insert_with(|| (format!("gap:{}:{key}", self.rule_id), at))
            .0
            .clone()
    }

    fn control_gap(&mut self, reason: RuleGapReason, at: TsNode<'tree>) {
        let gap_id = self.gap(reason, at);
        if self.dependencies.len() >= MAX_DEPENDENCIES {
            return;
        }
        if !self.dependencies.iter().any(|dependency| {
            dependency.resolution
                == DependencyResolution::Unresolved {
                    gap_id: gap_id.clone(),
                }
        }) {
            self.dependencies.push(RuleDependency {
                role: DependencyRole::ControlFlow,
                source: source(self.cx, at),
                resolution: DependencyResolution::Unresolved { gap_id },
            });
        }
    }

    /// The calls and names `expression` reads. A call to a function this
    /// file defines at top level is a Target; a parameter the body never
    /// rebinds is a Binding; anything else is Unresolved.
    fn dependencies(
        &mut self,
        expression: TsNode<'tree>,
        role: DependencyRole,
        scope: &Scope<'_, 'tree>,
        targets: &BTreeSet<String>,
    ) {
        let mut stack = vec![expression];
        while let Some(node) = stack.pop() {
            if self.dependencies.len() >= MAX_DEPENDENCIES {
                self.control_gap(RuleGapReason::DependencyLimit, expression);
                return;
            }
            // A lambda body or comprehension scope runs later or elsewhere.
            if matches!(
                node.kind(),
                "lambda"
                    | "list_comprehension"
                    | "set_comprehension"
                    | "dictionary_comprehension"
                    | "generator_expression"
            ) {
                self.control_gap(RuleGapReason::ExecutionPredicateUnknown, node);
                continue;
            }
            let resolution = if node.kind() == "call" {
                node.child_by_field_name("function")
                    .filter(|callee| callee.kind() == "identifier")
                    .and_then(|callee| scope.locals.get(self.cx.text(&callee)))
                    .filter(|target| targets.contains(*target))
                    .map(|node_id| DependencyResolution::Target {
                        node_id: node_id.clone(),
                    })
                    .or_else(|| {
                        Some(DependencyResolution::Unresolved {
                            gap_id: self.gap(RuleGapReason::UnresolvedCall, node),
                        })
                    })
            } else if is_value_identifier(node)
                && role == DependencyRole::Condition
                && let Some(assignment) =
                    definitions::sole_assignment(self.cx.text(&node), node, scope)
            {
                // A local assigned once before the condition (AC-1034).
                self.seeds.push((node, assignment));
                Some(DependencyResolution::Binding {
                    binding_id: definitions::binding_id(self.cx, assignment),
                    declaration: source(self.cx, assignment),
                })
            } else if is_value_identifier(node) {
                let name = self.cx.text(&node);
                Some(
                    scope
                        .parameters
                        .get(name)
                        .filter(|_| !scope.rebound.contains(name))
                        .map(|declaration| DependencyResolution::Binding {
                            binding_id: format!(
                                "binding:{}@{}#declaration@{}",
                                self.cx.id.repo,
                                self.cx.path,
                                declaration.start_byte()
                            ),
                            declaration: source(self.cx, *declaration),
                        })
                        .unwrap_or_else(|| DependencyResolution::Unresolved {
                            gap_id: self.gap(RuleGapReason::UnresolvedBinding, node),
                        }),
                )
            } else {
                None
            };
            if let Some(resolution) = resolution {
                self.dependencies.push(RuleDependency {
                    role,
                    source: source(self.cx, node),
                    resolution,
                });
            }
            let mut cursor = node.walk();
            stack.extend(
                node.named_children(&mut cursor)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
    }
}

struct Scope<'a, 'tree> {
    locals: &'a HashMap<String, String>,
    parameters: BTreeMap<String, TsNode<'tree>>,
    rebound: BTreeSet<String>,
    bindings: BTreeMap<String, Vec<TsNode<'tree>>>,
}

impl<'a, 'tree> Scope<'a, 'tree> {
    fn new(cx: &FileCx<'_>, function: TsNode<'tree>, locals: &'a HashMap<String, String>) -> Self {
        let bindings = bindings(cx, function);
        Scope {
            locals,
            parameters: parameters(cx, function),
            rebound: bindings.keys().cloned().collect(),
            bindings,
        }
    }
}

impl<'tree> RuleBuilder<'_, '_, 'tree> {
    /// The definitions this rule's condition names lead to (AC-1034); a
    /// use whose definition is not recorded falls back to a Gap.
    fn definitions(
        &mut self,
        scope: &Scope<'_, 'tree>,
        redactions: &mut Vec<SourceRedaction>,
    ) -> Vec<LocalDefinition> {
        let seeds = std::mem::take(&mut self.seeds);
        let (found, mut withheld, dropped) = definitions::recover(self, scope, &seeds);
        redactions.append(&mut withheld);
        for usage in dropped {
            let at = source(self.cx, usage);
            let gap_id = self.gap(RuleGapReason::DefinitionLimit, usage);
            for dependency in &mut self.dependencies {
                if dependency.role == DependencyRole::Condition && dependency.source == at {
                    dependency.resolution = DependencyResolution::Unresolved {
                        gap_id: gap_id.clone(),
                    };
                }
            }
        }
        found
    }
}

fn is_value_identifier(node: TsNode<'_>) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    !node.parent().is_some_and(|parent| {
        (parent.kind() == "keyword_argument" && parent.child_by_field_name("name") == Some(node))
            || (parent.kind() == "attribute"
                && parent.child_by_field_name("attribute") == Some(node))
    })
}

type Branch<'t> = (TsNode<'t>, TsNode<'t>, BranchPolarity);

/// The `if` / `elif` / `else` branches around `exit` up to its function,
/// outermost first: the taken branch's condition truthy, and every earlier
/// condition of the same `if` falsy.
///
/// A `try` handler is a branch too (AC-1036): an exit in `except T:` runs
/// when `T` is raised, and an exit that is itself a statement of the `try`
/// body returns only while none of its handlers' types is raised. A bare
/// `except:` or one naming several types states no branch.
fn branches(mut exit: TsNode<'_>) -> (Vec<Branch<'_>>, bool) {
    let mut conditions: Vec<Branch<'_>> = Vec::new();
    let mut steps = 0;
    let statement = exit;
    while let Some(parent) = exit.parent() {
        if matches!(
            parent.kind(),
            "function_definition" | "lambda" | "class_definition"
        ) {
            break;
        }
        steps += 1;
        if steps > MAX_ANCESTOR_STEPS || conditions.len() >= MAX_CONDITIONS {
            return (conditions, true);
        }
        if parent.kind() == "except_clause"
            && exit.kind() == "block"
            && let Some(caught) = caught(parent)
        {
            conditions.push((parent, caught, BranchPolarity::RaisedBranch));
        }
        if parent.kind() == "try_statement"
            && parent.child_by_field_name("body") == Some(exit)
            && statement.parent() == Some(exit)
        {
            let mut cursor = parent.walk();
            let handlers: Vec<_> = parent
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "except_clause")
                .filter_map(|c| Some((c, caught(c)?, BranchPolarity::UnraisedBranch)))
                .collect();
            conditions.extend(handlers.into_iter().rev());
        }
        let statement = match parent.kind() {
            "if_statement" => Some(parent),
            "elif_clause" | "else_clause" => parent.parent(),
            _ => None,
        };
        if let Some(statement) = statement {
            let mut taken = Vec::new();
            // Walk the clauses in order up to the one holding the exit.
            if parent.kind() == "if_statement" {
                if parent.child_by_field_name("consequence") == Some(exit)
                    && let Some(condition) = parent.child_by_field_name("condition")
                {
                    taken.push((parent, condition, BranchPolarity::TruthyBranch));
                }
            } else {
                if let Some(condition) = statement.child_by_field_name("condition") {
                    taken.push((statement, condition, BranchPolarity::FalsyBranch));
                }
                let mut cursor = statement.walk();
                for clause in statement.children_by_field_name("alternative", &mut cursor) {
                    if clause == parent {
                        break;
                    }
                    if let Some(condition) = clause.child_by_field_name("condition") {
                        taken.push((clause, condition, BranchPolarity::FalsyBranch));
                    }
                }
                if parent.kind() == "elif_clause"
                    && parent.child_by_field_name("consequence") == Some(exit)
                    && let Some(condition) = parent.child_by_field_name("condition")
                {
                    taken.push((parent, condition, BranchPolarity::TruthyBranch));
                }
            }
            // Ancestors are visited innermost first; reversed at the end.
            conditions.extend(taken.into_iter().rev());
            if parent.kind() != "if_statement" {
                // Skip the `if_statement` itself: its clauses are taken.
                exit = statement;
                continue;
            }
        }
        exit = parent;
    }
    conditions.reverse();
    let limited = conditions.len() > MAX_CONDITIONS;
    conditions.truncate(MAX_CONDITIONS);
    (conditions, limited)
}

/// The one error type an `except` clause names (`except T:` or
/// `except T as e:`); `None` for a bare `except:`, an `except*`, or one
/// naming several types.
fn caught(clause: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut cursor = clause.walk();
    if clause.children(&mut cursor).any(|c| c.kind() == "*") {
        return None;
    }
    let mut cursor = clause.walk();
    let values: Vec<_> = clause
        .children_by_field_name("value", &mut cursor)
        .collect();
    // `except (A, B):` is one tuple value naming several types.
    match values[..] {
        [one] if one.kind() != "tuple" => Some(one),
        _ => None,
    }
}

/// The value a `return` returns or a `raise` raises; `None` for a bare one.
fn exit_value(exit: TsNode<'_>) -> Option<TsNode<'_>> {
    let cause = exit.child_by_field_name("cause");
    let mut cursor = exit.walk();
    exit.named_children(&mut cursor)
        .find(|node| node.kind() != "comment" && Some(*node) != cause)
}

fn omission_gap(
    cx: &FileCx<'_>,
    at: TsNode<'_>,
    owner: &str,
    reason: RuleGapReason,
    out: &mut Extraction,
) {
    let id = format!(
        "gap:{}@{}#rule-analysis@{}",
        cx.id.repo,
        cx.path,
        at.start_byte()
    );
    let reason = reason_key(reason);
    let props = serde_json::json!({
        "rule_evidence_gap": true,
        "reason_code": reason,
        "reason": reason_description(&reason),
        "scope": "rule_or_remaining_file_analysis",
        "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {id}")),
    });
    out.nodes.push(Node {
        id: id.clone(),
        label: "Gap".into(),
        props: props.clone(),
    });
    out.edges.push(Edge {
        src: owner.into(),
        dst: id,
        label: "DEPENDS_ON".into(),
        props,
    });
}

fn control_reason(node: TsNode<'_>) -> Option<RuleGapReason> {
    match node.kind() {
        "for_statement" | "while_statement" => Some(RuleGapReason::LoopDependency),
        "match_statement" => Some(RuleGapReason::SwitchControl),
        "try_statement" | "except_clause" | "finally_clause" | "with_statement" => {
            Some(RuleGapReason::ExceptionControl)
        }
        "augmented_assignment" => Some(RuleGapReason::Mutation),
        // Rebinding a plain name is not a mutation; writing through an
        // attribute or subscript is.
        "assignment" => node
            .child_by_field_name("left")
            .filter(|left| matches!(left.kind(), "attribute" | "subscript"))
            .map(|_| RuleGapReason::Mutation),
        "delete_statement" => Some(RuleGapReason::Mutation),
        _ => None,
    }
}

pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    locals: &HashMap<String, String>,
    out: &mut Extraction,
) {
    let targets: BTreeSet<String> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect();
    // Recovered syntax cannot establish an exit or its branches: one
    // omission Gap, and no rules from this file.
    if root.has_error() {
        let mut at = root;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.is_error() || node.is_missing() {
                at = node;
                break;
            }
            let mut cursor = node.walk();
            stack.extend(
                node.children(&mut cursor)
                    .filter(|child| child.has_error() || child.is_missing())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
        let at = omission_source(at);
        let owner = owner_id(cx, at)
            .filter(|owner| targets.contains(owner))
            .unwrap_or_else(|| file_id(cx.id.repo, cx.path));
        omission_gap(cx, at, &owner, RuleGapReason::UnsupportedRuleSyntax, out);
        return;
    }
    let mut exits: BTreeMap<String, Vec<TsNode<'_>>> = BTreeMap::new();
    let mut effects: BTreeMap<String, Vec<Effect<'_>>> = BTreeMap::new();
    let modules = module_imports(cx, root);
    let mut functions: BTreeMap<String, TsNode<'_>> = BTreeMap::new();
    let mut controls: BTreeMap<String, BTreeMap<String, (RuleGapReason, TsNode<'_>)>> =
        BTreeMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(
            node.named_children(&mut cursor)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
        if let Some(effect) = effect_statement(cx, node, &modules)
            && let Some(owner) = owner_id(cx, node)
            && let Some(function) = owner_function(node)
        {
            functions.entry(owner.clone()).or_insert(function);
            effects.entry(owner).or_default().push(effect);
        }
        if matches!(node.kind(), "return_statement" | "raise_statement") {
            if let Some(owner) = owner_id(cx, node)
                && let Some(function) = owner_function(node)
            {
                functions.entry(owner.clone()).or_insert(function);
                exits.entry(owner).or_default().push(node);
            }
        } else if let Some(reason) = control_reason(node)
            && let Some(owner) = owner_id(cx, node)
        {
            controls
                .entry(owner)
                .or_default()
                .entry(reason_key(reason))
                .or_insert((reason, node));
        }
    }
    let mut rule_count = 0;
    let mut file_bytes = 0;
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let exits_by_owner = exits.clone();
    'exits: for (owner_id, owner_exits) in exits {
        if !targets.contains(&owner_id) {
            continue;
        }
        let function = functions[&owner_id];
        let scope = Scope::new(cx, function, locals);
        // A method that answers `True` or `False` somewhere is a predicate
        // whatever its name (`lifetime_or_active_subscription`, AC-1030).
        // Only a method: its rows state the earlier exits as "otherwise",
        // which a free function's rows do not.
        let answers_bool = in_class(function)
            && owner_exits.iter().any(|exit| {
                exit.kind() == "return_statement"
                    && exit_value(*exit).is_some_and(|v| matches!(v.kind(), "true" | "false"))
            });
        for (order, exit) in owner_exits.iter().copied().enumerate() {
            let (ancestors, limited) = branches(exit);
            let value = exit_value(exit);
            // An unguarded exit is a rule only as the answer of a predicate
            // (`def is_active(self): return self.delete_on is None`, #2029),
            // or as the last answer of a method that answers a boolean
            // after an earlier exit (AC-1030); a constant `return True`
            // with no exit before it answers no question, and "otherwise"
            // would invent a branch.
            let constant = value.is_some_and(|v| matches!(v.kind(), "true" | "false" | "none"));
            if ancestors.is_empty()
                && !limited
                && !(exit.kind() == "return_statement"
                    && value.is_some()
                    && (predicate_name(&owner_id) || (order > 0 && answers_bool && !constant))
                    && !(order == 0 && constant))
            {
                continue;
            }
            // A bare `raise` re-raises what is being handled: not a guard's
            // own outcome.
            if exit.kind() == "raise_statement" && value.is_none() {
                continue;
            }
            if limited || rule_count == MAX_RULES_PER_FILE || file_bytes >= MAX_FILE_RULE_BYTES {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                break 'exits;
            }
            let rule_id = format!("rule:{}@{}#exit@{}", cx.id.repo, cx.path, exit.start_byte());
            let mut builder = RuleBuilder {
                cx,
                rule_id: rule_id.clone(),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
                seeds: Vec::new(),
            };
            builder.control_gap(RuleGapReason::ExecutionPredicateUnknown, exit);
            builder.control_gap(RuleGapReason::ConsumerSemanticsUnknown, exit);
            if let Some(prior) = order.checked_sub(1).map(|index| owner_exits[index]) {
                builder.control_gap(RuleGapReason::PrecedingExit, prior);
            }
            if let Some(controls) = controls.get(&owner_id) {
                for (reason, at) in controls.values() {
                    builder.control_gap(*reason, *at);
                }
            }
            let mut redactions = Vec::new();
            let mut conditions = Vec::new();
            for (branch, condition, polarity) in ancestors {
                let (expression, mut withheld) = capture(cx, condition);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, condition);
                }
                redactions.append(&mut withheld);
                if condition.byte_range().len() <= MAX_EXPRESSION_BYTES {
                    builder.dependencies(condition, DependencyRole::Condition, &scope, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, condition);
                }
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let value = value.map(|node| {
                let (expression, mut withheld) = capture(cx, node);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, node);
                }
                redactions.append(&mut withheld);
                if node.byte_range().len() <= MAX_EXPRESSION_BYTES {
                    builder.dependencies(node, DependencyRole::ExitValue, &scope, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, node);
                }
                expression
            });
            let effect = match value {
                Some(value) if exit.kind() == "raise_statement" => LocalExit::Throw { value },
                value => LocalExit::Return { value },
            };
            let local_definitions = builder.definitions(&scope, &mut redactions);
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind: RuleKind::GuardedExit,
                owner_id: owner_id.clone(),
                exit_source: source(cx, exit),
                source_order: order as u32,
                conditions,
                effect,
                local_definitions: Some(local_definitions),
                dependencies: builder.dependencies,
                interpretation: Interpretation {
                    execution_predicate: InterpretationStatus::NotEstablished,
                    consumer_effect: InterpretationStatus::NotEstablished,
                    gap_ids: builder.gaps.values().map(|(id, _)| id.clone()).collect(),
                },
                redactions,
            };
            let Some(canonical) = admitted(cx, &evidence, exit, &owner_id, out) else {
                break 'exits;
            };
            if canonical.len() > MAX_RULE_BYTES
                || file_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                break 'exits;
            }
            file_bytes += canonical.len();
            rule_count += 1;
            emit(
                cx,
                (&rule_id, &owner_id),
                (evidence, canonical),
                exit,
                "guarded_exit",
                format!(
                    "Guarded local exit at {}:{}",
                    cx.path,
                    exit.start_position().row + 1
                ),
                builder.gaps,
                &mut emitted,
                out,
            );
        }
    }

    // Guarded effects (#1998): assignments through an attribute and call
    // statements, with the branches around them (possibly none).
    let mut effect_count = 0;
    let mut effect_bytes = 0;
    for (owner_id, owner_effects) in effects {
        if !targets.contains(&owner_id) {
            continue;
        }
        let function = functions[&owner_id];
        let scope = Scope::new(cx, function, locals);
        let owner_exits = exits_by_owner.get(&owner_id).map_or(&[][..], Vec::as_slice);
        for (order, effect) in owner_effects.iter().enumerate() {
            let at = effect.statement;
            let (ancestors, limited) = branches(at);
            if limited
                || effect_count == MAX_EFFECTS_PER_FILE
                || effect_bytes >= MAX_FILE_RULE_BYTES
            {
                omission_gap(cx, at, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            let rule_id = format!("rule:{}@{}#effect@{}", cx.id.repo, cx.path, at.start_byte());
            let mut builder = RuleBuilder {
                cx,
                // One set of interpretation Gaps per function's effects,
                // not per effect: they state the same reasons (#1998).
                rule_id: format!("{owner_id}#effects"),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
                seeds: Vec::new(),
            };
            builder.control_gap(RuleGapReason::ExecutionPredicateUnknown, at);
            builder.control_gap(RuleGapReason::ConsumerSemanticsUnknown, at);
            // The last exit before the effect may leave the callable first.
            if let Some(prior) = owner_exits
                .iter()
                .rev()
                .find(|exit| exit.end_byte() <= at.start_byte())
            {
                builder.control_gap(RuleGapReason::PrecedingExit, *prior);
            }
            if let Some(controls) = controls.get(&owner_id) {
                for (reason, at) in controls.values() {
                    builder.control_gap(*reason, *at);
                }
            }
            let mut redactions = Vec::new();
            let mut conditions = Vec::new();
            for (branch, condition, polarity) in ancestors {
                let (expression, mut withheld) = capture(cx, condition);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, condition);
                }
                redactions.append(&mut withheld);
                if condition.byte_range().len() <= MAX_EXPRESSION_BYTES {
                    builder.dependencies(condition, DependencyRole::Condition, &scope, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, condition);
                }
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let local = match effect.kind {
                EffectKind::Assign { target, value } => LocalExit::Assign {
                    target: captured(cx, target, &mut builder, &mut redactions),
                    value: captured(cx, value, &mut builder, &mut redactions),
                },
                EffectKind::Call { call } => LocalExit::Call {
                    value: captured(cx, call, &mut builder, &mut redactions),
                },
            };
            let read = match effect.kind {
                EffectKind::Assign { value, .. } => value,
                EffectKind::Call { call } => call,
            };
            if read.byte_range().len() <= MAX_EXPRESSION_BYTES {
                builder.dependencies(read, DependencyRole::ExitValue, &scope, &targets);
            } else {
                builder.control_gap(RuleGapReason::AnalysisLimit, read);
            }
            let local_definitions = builder.definitions(&scope, &mut redactions);
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind: RuleKind::GuardedEffect,
                owner_id: owner_id.clone(),
                exit_source: source(cx, at),
                source_order: order as u32,
                conditions,
                effect: local,
                local_definitions: Some(local_definitions),
                dependencies: builder.dependencies,
                interpretation: Interpretation {
                    execution_predicate: InterpretationStatus::NotEstablished,
                    consumer_effect: InterpretationStatus::NotEstablished,
                    gap_ids: builder.gaps.values().map(|(id, _)| id.clone()).collect(),
                },
                redactions,
            };
            let Some(canonical) = admitted(cx, &evidence, at, &owner_id, out) else {
                return;
            };
            if canonical.len() > MAX_RULE_BYTES
                || effect_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(cx, at, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            effect_bytes += canonical.len();
            effect_count += 1;
            emit(
                cx,
                (&rule_id, &owner_id),
                (evidence, canonical),
                at,
                "guarded_effect",
                format!(
                    "Guarded local effect at {}:{}",
                    cx.path,
                    at.start_position().row + 1
                ),
                builder.gaps,
                &mut emitted,
                out,
            );
        }
    }
}

/// Whether a function id names a predicate: its last name segment starts
/// with `is_`, `has_`, `can_`, `should_` or `must_`.
fn predicate_name(owner_id: &str) -> bool {
    let name = owner_id.rsplit(['#', '.']).next().unwrap_or(owner_id);
    ["is_", "has_", "can_", "should_", "must_"]
        .iter()
        .any(|p| name.strip_prefix(p).is_some_and(|r| !r.is_empty()))
}

/// One captured effect expression, its redactions and capture Gap.
fn captured<'t>(
    cx: &FileCx<'_>,
    node: TsNode<'t>,
    builder: &mut RuleBuilder<'_, '_, 't>,
    redactions: &mut Vec<SourceRedaction>,
) -> SourceExpression {
    let (expression, mut withheld) = capture(cx, node);
    if expression.capture != ExpressionCapture::CompleteSyntax {
        builder.control_gap(RuleGapReason::RedactedExpression, node);
    }
    redactions.append(&mut withheld);
    expression
}

/// Validate and serialize a rule payload; on failure, the omission Gap.
fn admitted(
    cx: &FileCx<'_>,
    evidence: &GuardedExitEvidence,
    at: TsNode<'_>,
    owner_id: &str,
    out: &mut Extraction,
) -> Option<String> {
    if let Err(error) = evidence.validate() {
        omission_gap(
            cx,
            at,
            owner_id,
            if matches!(error, RuleValidationError::Limit(_)) {
                RuleGapReason::AnalysisLimit
            } else {
                RuleGapReason::UnsupportedRuleSyntax
            },
            out,
        );
        return None;
    }
    let payload = serde_json::to_value(evidence).expect("rule serializes");
    Some(serde_json::to_string(&payload).expect("rule serializes"))
}

/// The `BusinessRule`, its `GOVERNS` edge and its Gaps.
#[allow(clippy::too_many_arguments)]
fn emit(
    cx: &FileCx<'_>,
    (rule_id, owner_id): (&str, &str),
    (evidence, canonical): (GuardedExitEvidence, String),
    at: TsNode<'_>,
    observation: &str,
    name: String,
    gaps: BTreeMap<String, (String, TsNode<'_>)>,
    emitted: &mut BTreeSet<String>,
    out: &mut Extraction,
) {
    let payload = serde_json::to_value(evidence).expect("rule serializes");
    out.nodes.push(Node {
        id: rule_id.to_owned(),
        label: "BusinessRule".into(),
        props: serde_json::json!({
            "name": name,
            "observation": observation,
            "rule": payload,
            "prov": cx.prov(&at, &canonical),
        }),
    });
    out.edges.push(Edge {
        src: rule_id.to_owned(),
        dst: owner_id.to_owned(),
        label: "GOVERNS".into(),
        props: serde_json::json!({"observation": "lexical_owner", "prov": cx.prov(&at, &format!("GOVERNS {rule_id} -> {owner_id}"))}),
    });
    for (reason, (gap_id, gap_at)) in gaps {
        let fresh = emitted.insert(gap_id.clone());
        let props = serde_json::json!({
            "rule_evidence_gap": true,
            "reason_code": reason,
            "reason": reason_description(&reason),
            "prov": cx.prov_with_confidence(&gap_at, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
        });
        if fresh {
            out.nodes.push(Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: props.clone(),
            });
        }
        out.edges.push(Edge {
            src: rule_id.to_owned(),
            dst: gap_id,
            label: "DEPENDS_ON".into(),
            props,
        });
    }
}

#[derive(Clone, Copy)]
enum EffectKind<'t> {
    Assign {
        target: TsNode<'t>,
        value: TsNode<'t>,
    },
    Call {
        call: TsNode<'t>,
    },
}

#[derive(Clone, Copy)]
struct Effect<'t> {
    statement: TsNode<'t>,
    kind: EffectKind<'t>,
}

/// Names bound by the module's top-level imports.
fn module_imports(cx: &FileCx<'_>, root: TsNode<'_>) -> BTreeSet<String> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|n| matches!(n.kind(), "import_statement" | "import_from_statement"))
        .flat_map(imported)
        .map(|name| cx.text(&name).to_owned())
        .collect()
}

/// Class methods that create or delete a record (`Model.create(…)`): a call
/// statement of one on an imported upper-case name is an effect (#1998).
const MODEL_WRITE_VERBS: &[&str] = &["create", "delete"];

/// Logging receivers and verbs: a call to one is a diagnostic, not an effect.
const LOGGING: &[&str] = &[
    "LOG", "log", "logger", "logging", "LOGGER", "_logger", "_log",
];
const LOG_VERBS: &[&str] = &[
    "debug",
    "info",
    "warning",
    "warn",
    "error",
    "exception",
    "critical",
    "d",
    "i",
    "w",
    "e",
];

/// Python builtins: a call statement to one is not an effect of interest.
const BUILTINS: &[&str] = &[
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "exit",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "quit",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

/// Whether `name` is bound in `function` itself, as a parameter or by an
/// assignment, loop or `with` target, so it is not the module of that name.
fn bound_locally(cx: &FileCx<'_>, function: TsNode<'_>, name: &str) -> bool {
    bound_before(cx, function, name, usize::MAX)
}

/// Whether `function` binds `name` (as [`bound_locally`]) at a node that
/// starts before byte `before`.
fn bound_before(cx: &FileCx<'_>, function: TsNode<'_>, name: &str, before: usize) -> bool {
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        if node != function
            && matches!(
                node.kind(),
                "function_definition" | "class_definition" | "lambda"
            )
        {
            continue;
        }
        let bound = match node.kind() {
            // A parameter's name, not its annotation or default value.
            "identifier" => node.parent().is_some_and(|p| {
                matches!(p.kind(), "parameters" | "typed_parameter")
                    || (matches!(p.kind(), "default_parameter" | "typed_default_parameter")
                        && p.child_by_field_name("name") == Some(node))
                    || (matches!(
                        p.kind(),
                        "assignment" | "augmented_assignment" | "for_statement"
                    ) && p.child_by_field_name("left") == Some(node))
                    || p.kind() == "as_pattern_target"
                    || unpacked_target(node)
            }),
            _ => false,
        };
        if bound && node.start_byte() < before && cx.text(&node) == name {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

/// Whether `node` is a name unpacked by an assignment or `for` target
/// (`alias, meta = load()`, `for (a, b) in rows`), at any nesting.
fn unpacked_target(node: TsNode<'_>) -> bool {
    let mut pattern = node;
    while let Some(parent) = pattern.parent() {
        match parent.kind() {
            "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern" => {
                pattern = parent;
            }
            "assignment" | "for_statement" => {
                return pattern != node && parent.child_by_field_name("left") == Some(pattern);
            }
            _ => return false,
        }
    }
    false
}

/// Names bound by the imports inside `function` itself.
fn function_imports(cx: &FileCx<'_>, function: TsNode<'_>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        if node != function
            && matches!(
                node.kind(),
                "function_definition" | "class_definition" | "lambda"
            )
        {
            continue;
        }
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            found.extend(imported(node).iter().map(|n| cx.text(n).to_owned()));
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    found
}

/// A statement that is a guarded effect (#1998): `x.attr = value` (one
/// attribute target), or a call statement (optionally awaited) to a
/// function name that is not a builtin, to a method on `self` or `cls`, or
/// to a function of an imported lowercase module (`alias_utils.delete(…)`),
/// or to `create`/`delete` on an imported class (`AliasUsedOn.create(…)`).
/// Logging calls and `__init__` bodies are not effects.
fn effect_statement<'t>(
    cx: &FileCx<'_>,
    node: TsNode<'t>,
    modules: &BTreeSet<String>,
) -> Option<Effect<'t>> {
    if node.kind() != "expression_statement" || node.named_child_count() != 1 {
        return None;
    }
    if owner_function(node)
        .and_then(|f| f.child_by_field_name("name"))
        .is_some_and(|name| cx.text(&name) == "__init__")
    {
        return None;
    }
    let inner = node.named_child(0)?;
    let kind = match inner.kind() {
        "assignment" => {
            let target = inner.child_by_field_name("left")?;
            let value = inner.child_by_field_name("right")?;
            // `alias = Alias.create(…)`: the record created is the effect,
            // whatever local it is kept in (#1998).
            if target.kind() == "identifier"
                && inner.child_by_field_name("type").is_none()
                && let Some(create) = record_create(cx, node, value, modules)
            {
                return Some(Effect {
                    statement: node,
                    kind: EffectKind::Call { call: create },
                });
            }
            // `alias = None` or `name = name.replace("\n", "")` under a
            // branch: a local bound earlier is discarded or cleaned (#1998).
            if target.kind() == "identifier"
                && inner.child_by_field_name("type").is_none()
                && local_rebind(cx, node, target, value)
            {
                return Some(Effect {
                    statement: node,
                    kind: EffectKind::Assign { target, value },
                });
            }
            if target.kind() != "attribute" || inner.child_by_field_name("type").is_some() {
                return None;
            }
            EffectKind::Assign { target, value }
        }
        "call" => EffectKind::Call { call: inner },
        "await" => {
            let call = inner.named_child(0).filter(|c| c.kind() == "call")?;
            EffectKind::Call { call }
        }
        _ => return None,
    };
    // `Session.add(DeletedAlias(…))`, or of a local assigned one: the record
    // created is the constructor call (#1998).
    if let EffectKind::Call { call } = kind
        && let Some(constructor) = session_added(cx, node, call, modules)
    {
        return Some(Effect {
            statement: node,
            kind: EffectKind::Call { call: constructor },
        });
    }
    if let EffectKind::Call { call } = kind {
        let callee = call.child_by_field_name("function")?;
        let admitted = match callee.kind() {
            // A bare function: not a builtin, a class (a constructor) or a
            // logging verb imported directly (`from logging import info`).
            "identifier" => {
                let name = cx.text(&callee);
                !BUILTINS.contains(&name)
                    && !LOG_VERBS.contains(&name)
                    && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            }
            "attribute" => {
                let object = callee.child_by_field_name("object")?;
                let verb = cx.text(&callee.child_by_field_name("attribute")?);
                let receiver = cx.text(&object);
                let function = owner_function(node);
                // A module imported at the top or in the function itself,
                // and not rebound there (`def run(service): service.save()`).
                let module = || {
                    receiver.starts_with(|c: char| c.is_ascii_lowercase())
                        && function.is_none_or(|f| !bound_locally(cx, f, receiver))
                        && (modules.contains(receiver)
                            || function.is_some_and(|f| function_imports(cx, f).contains(receiver)))
                };
                // `AliasUsedOn.create(…)` / `DeletedAlias.delete(…)` on an
                // imported class: a record created or deleted (#1998).
                let model_write = || {
                    MODEL_WRITE_VERBS.contains(&verb)
                        && receiver.starts_with(|c: char| c.is_ascii_uppercase())
                        && receiver != "Session"
                        && function.is_none_or(|f| !bound_locally(cx, f, receiver))
                        && (modules.contains(receiver)
                            || function.is_some_and(|f| function_imports(cx, f).contains(receiver)))
                };
                // `Alias.filter(…).delete()`: the matching records deleted.
                let query_delete = || {
                    verb == "delete"
                        && object.kind() == "call"
                        && object
                            .child_by_field_name("function")
                            .filter(|f| f.kind() == "attribute")
                            .is_some_and(|f| {
                                f.child_by_field_name("attribute")
                                    .is_some_and(|a| matches!(cx.text(&a), "filter" | "filter_by"))
                                    && f.child_by_field_name("object").is_some_and(|model| {
                                        imported_class(cx, node, model, modules)
                                    })
                            })
                };
                (object.kind() == "identifier"
                    && !LOGGING.contains(&receiver)
                    && !LOG_VERBS.contains(&verb)
                    && (matches!(receiver, "self" | "cls") || module() || model_write()))
                    || query_delete()
            }
            _ => false,
        };
        if !admitted {
            return None;
        }
    }
    Some(Effect {
        statement: node,
        kind,
    })
}

/// Whether `target = value` rebinds, under an `if` branch of its function, a
/// local the function bound earlier, to `None` or to its own
/// `.replace("…", "…")` (#1998). Any other plain-name assignment is no
/// effect.
fn local_rebind(
    cx: &FileCx<'_>,
    statement: TsNode<'_>,
    target: TsNode<'_>,
    value: TsNode<'_>,
) -> bool {
    let Some(function) = owner_function(statement) else {
        return false;
    };
    let name = cx.text(&target);
    let mut parent = statement.parent();
    let mut branched = false;
    while let Some(node) = parent.filter(|n| *n != function) {
        branched |= matches!(node.kind(), "if_statement" | "elif_clause" | "else_clause");
        parent = node.parent();
    }
    if !branched || !bound_before(cx, function, name, statement.start_byte()) {
        return false;
    }
    match value.kind() {
        "none" => true,
        "call" => {
            let Some(callee) = value
                .child_by_field_name("function")
                .filter(|c| c.kind() == "attribute")
            else {
                return false;
            };
            let receiver = callee.child_by_field_name("object");
            let arguments = value.child_by_field_name("arguments");
            callee
                .child_by_field_name("attribute")
                .is_some_and(|a| cx.text(&a) == "replace")
                && receiver.is_some_and(|r| r.kind() == "identifier" && cx.text(&r) == name)
                && arguments.is_some_and(|a| {
                    let mut cursor = a.walk();
                    let args: Vec<_> = a.named_children(&mut cursor).collect();
                    args.len() == 2 && args.iter().all(|arg| arg.kind() == "string")
                })
        }
        _ => false,
    }
}

/// Whether `name` is an upper-case class imported at the top of the file or
/// in the statement's function, other than `Session`, and not rebound there.
fn imported_class(
    cx: &FileCx<'_>,
    statement: TsNode<'_>,
    name: TsNode<'_>,
    modules: &BTreeSet<String>,
) -> bool {
    let text = cx.text(&name);
    let function = owner_function(statement);
    name.kind() == "identifier"
        && text.starts_with(|c: char| c.is_ascii_uppercase())
        && text != "Session"
        && function.is_none_or(|f| !bound_locally(cx, f, text))
        && (modules.contains(text)
            || function.is_some_and(|f| function_imports(cx, f).contains(text)))
}

/// `Alias.create(…)` (optionally awaited) on an imported class: the value
/// of an assignment that creates a record (#1998).
fn record_create<'t>(
    cx: &FileCx<'_>,
    statement: TsNode<'t>,
    value: TsNode<'t>,
    modules: &BTreeSet<String>,
) -> Option<TsNode<'t>> {
    let call = if value.kind() == "await" {
        value.named_child(0)?
    } else {
        value
    };
    let callee = call
        .child_by_field_name("function")
        .filter(|_| call.kind() == "call")?;
    (callee.kind() == "attribute"
        && callee
            .child_by_field_name("attribute")
            .is_some_and(|a| cx.text(&a) == "create")
        && callee
            .child_by_field_name("object")
            .is_some_and(|model| imported_class(cx, statement, model, modules)))
    .then_some(call)
}

/// The constructor call of the record `Session.add(…)` adds (#1998): its
/// argument `DeletedAlias(…)`, or the last `deleted = DeletedAlias(…)` of a
/// local before the statement in the same function.
fn session_added<'t>(
    cx: &FileCx<'_>,
    statement: TsNode<'t>,
    call: TsNode<'t>,
    modules: &BTreeSet<String>,
) -> Option<TsNode<'t>> {
    let callee = call.child_by_field_name("function")?;
    if callee.kind() != "attribute"
        || callee
            .child_by_field_name("object")
            .is_none_or(|o| o.kind() != "identifier" || cx.text(&o) != "Session")
        || callee
            .child_by_field_name("attribute")
            .is_none_or(|a| cx.text(&a) != "add")
    {
        return None;
    }
    let arguments = call.child_by_field_name("arguments")?;
    if arguments.named_child_count() != 1 {
        return None;
    }
    let argument = arguments.named_child(0)?;
    let constructs = |value: TsNode<'t>| {
        value.kind() == "call"
            && value
                .child_by_field_name("function")
                .is_some_and(|f| imported_class(cx, statement, f, modules))
    };
    match argument.kind() {
        "call" => constructs(argument).then_some(argument),
        "identifier" => {
            // Only the local's one assignment in the function, earlier in the
            // same block as the statement, so it is the value added on every
            // path; a reassigned local is withheld.
            let name = cx.text(&argument);
            let function = owner_function(statement)?;
            let mut found = Vec::new();
            let mut stack = vec![function];
            while let Some(node) = stack.pop() {
                if node != function
                    && matches!(
                        node.kind(),
                        "function_definition" | "class_definition" | "lambda"
                    )
                {
                    continue;
                }
                if matches!(
                    node.kind(),
                    "assignment" | "augmented_assignment" | "for_statement"
                ) && node.child_by_field_name("left").is_some_and(|left| {
                    cx.text(&left)
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .any(|word| word == name)
                }) {
                    found.push(node);
                }
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
            let [assignment] = found[..] else {
                return None;
            };
            let same_block = assignment.parent().and_then(|p| p.parent()) == statement.parent();
            let value = assignment.child_by_field_name("right")?;
            (assignment.kind() == "assignment"
                && assignment
                    .child_by_field_name("left")
                    .is_some_and(|left| left.kind() == "identifier")
                && assignment.end_byte() <= statement.start_byte()
                && same_block
                && constructs(value))
            .then_some(value)
        }
        _ => None,
    }
}

fn omission_source(mut node: TsNode<'_>) -> TsNode<'_> {
    // Missing parser nodes can be zero-width. Cite the enclosing source that
    // failed to parse instead of manufacturing an empty span.
    for _ in 0..MAX_ANCESTOR_STEPS {
        if !node.byte_range().is_empty() {
            break;
        }
        let Some(parent) = node.parent() else {
            break;
        };
        node = parent;
    }
    node
}

/// Cached unchanged source still belongs to the revision being ingested now.
pub(super) fn retarget(extraction: &mut Extraction, commit: &str) {
    for node in &mut extraction.nodes {
        if node.label != "BusinessRule" {
            continue;
        }
        let props = &mut node.props;
        let Some(payload) = props.get("rule").cloned() else {
            continue;
        };
        let Ok(mut rule) = GuardedExitEvidence::from_value(payload) else {
            continue;
        };
        rule.visit_sources_mut(|source| source.commit_sha = commit.into());
        props["rule"] = serde_json::to_value(rule).expect("rule serializes");
        props["prov"]["content_hash"] = serde_json::json!(core_prov::content_hash(
            &serde_json::to_vec(&props["rule"]).expect("rule serializes")
        ));
    }
}

// ---------------------------------------------------------------------------
// Sanitized capture (SPEC-02, AC-0124): decoded literals are inspected with
// the shared `core-redact` recognizers, comments are removed, and a form
// outside the supported set withholds the whole display.

struct Replacement {
    start: usize,
    end: usize,
    text: &'static str,
}

fn capture(cx: &FileCx<'_>, node: TsNode<'_>) -> (SourceExpression, Vec<SourceRedaction>) {
    let source = source(cx, node);
    let mut result = SourceExpression {
        source: source.clone(),
        syntax_kind: node.kind().into(),
        display: SanitizedDisplay::from_sanitized(UNSUPPORTED.into()),
        capture: ExpressionCapture::Unsupported,
        literal: None,
    };
    if node.byte_range().len() > MAX_EXPRESSION_BYTES {
        result.display =
            SanitizedDisplay::from_sanitized("[expression omitted: analysis limit]".into());
        return (
            result,
            vec![SourceRedaction {
                source,
                reason: RedactionReason::UnsupportedSyntax,
            }],
        );
    }
    let Some(bytes) = cx.source.get(node.byte_range()) else {
        return withheld(result, source, RedactionReason::UnsupportedSyntax);
    };
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return withheld(result, source, RedactionReason::UnsupportedDecoding);
    };
    if node.has_error() || node.is_missing() {
        return withheld(result, source, RedactionReason::UnsupportedSyntax);
    }
    let mut replacements = vec![];
    let mut redactions = vec![];
    let mut stack = vec![node];
    let mut visited = 0;
    while let Some(current) = stack.pop() {
        visited += 1;
        if visited > MAX_EXPRESSION_NODES {
            return withheld(result, source, RedactionReason::UnsupportedSyntax);
        }
        match current.kind() {
            "comment" => {
                replacements.push(Replacement {
                    start: current.start_byte(),
                    end: current.end_byte(),
                    text: " ",
                });
                redactions.push(SourceRedaction {
                    source: cx.evidence(&current),
                    reason: RedactionReason::RemovedComment,
                });
                continue;
            }
            "true" | "false" | "none" => {
                if current == node {
                    result.literal = Some(LiteralEvidence::Known {
                        value: match current.kind() {
                            "true" => KnownLiteral::Boolean(true),
                            "false" => KnownLiteral::Boolean(false),
                            _ => KnownLiteral::Null,
                        },
                    });
                }
                continue;
            }
            "integer" | "float" => {
                // Python numerals are not secrets; only plain decimal forms
                // are decoded into a literal value.
                if current == node
                    && let Ok(value) = decode_number(cx.text(&current))
                {
                    result.literal = Some(LiteralEvidence::Known {
                        value: KnownLiteral::Number(value),
                    });
                }
                continue;
            }
            "string" => {
                let Ok(context) = sensitive_context(cx, current) else {
                    return withheld(result, source, RedactionReason::UnsupportedSyntax);
                };
                match string_parts(cx, current) {
                    Err(reason) => {
                        if current == node {
                            result.literal = Some(LiteralEvidence::Withheld {
                                kind: LiteralKind::String,
                                reasons: vec![reason],
                            });
                        }
                        return withheld(result, source, reason);
                    }
                    Ok(StringParts::Plain(decoded)) => {
                        let mut reasons = secret_reasons(&decoded);
                        if context && !reasons.contains(&RedactionReason::CredentialValue) {
                            reasons.push(RedactionReason::CredentialValue);
                        }
                        if reasons.is_empty() {
                            if current == node {
                                result.literal = Some(LiteralEvidence::Known {
                                    value: KnownLiteral::String(decoded),
                                });
                            }
                        } else {
                            replacements.push(Replacement {
                                start: current.start_byte(),
                                end: current.end_byte(),
                                text: WITHHELD,
                            });
                            for reason in &reasons {
                                redactions.push(SourceRedaction {
                                    source: cx.evidence(&current),
                                    reason: *reason,
                                });
                            }
                            if current == node {
                                result.literal = Some(LiteralEvidence::Withheld {
                                    kind: LiteralKind::String,
                                    reasons,
                                });
                            }
                        }
                    }
                    Ok(StringParts::Formatted {
                        chunks,
                        expressions,
                    }) => {
                        // An f-string: each literal chunk is inspected on its
                        // own, each interpolated expression as any other.
                        for chunk in chunks {
                            let text = cx.text(&chunk);
                            let mut reasons = secret_reasons(text);
                            if context && !reasons.contains(&RedactionReason::CredentialValue) {
                                reasons.push(RedactionReason::CredentialValue);
                            }
                            if !reasons.is_empty() {
                                replacements.push(Replacement {
                                    start: chunk.start_byte(),
                                    end: chunk.end_byte(),
                                    text: WITHHELD,
                                });
                                for reason in reasons {
                                    redactions.push(SourceRedaction {
                                        source: cx.evidence(&chunk),
                                        reason,
                                    });
                                }
                            }
                        }
                        stack.extend(expressions);
                    }
                }
                continue;
            }
            "identifier" => {
                let text = cx.text(&current);
                if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return withheld(result, source, RedactionReason::UnsupportedSyntax);
                }
                let reasons = recognized_secrets(text);
                if !reasons.is_empty() {
                    replacements.push(Replacement {
                        start: current.start_byte(),
                        end: current.end_byte(),
                        text: WITHHELD,
                    });
                    for reason in reasons {
                        redactions.push(SourceRedaction {
                            source: cx.evidence(&current),
                            reason: redaction_reason(reason),
                        });
                    }
                }
                continue;
            }
            kind if current.is_named() && supported_container(kind) => {}
            kind if !current.is_named() && supported_token(kind) => continue,
            _ => return withheld(result, source, RedactionReason::UnsupportedSyntax),
        }
        let mut cursor = current.walk();
        stack.extend(current.children(&mut cursor));
    }
    replacements.sort_by_key(|replacement| replacement.start);
    redactions.sort_by_key(|redaction| (redaction.source.byte_start, redaction.source.byte_end));
    let mut display = String::new();
    let mut copied_to = 0;
    for replacement in replacements {
        let start = replacement.start.saturating_sub(node.start_byte());
        let end = replacement.end.saturating_sub(node.start_byte());
        let Some(prefix) = raw.get(copied_to..start) else {
            return withheld(result, source, RedactionReason::UnsupportedSyntax);
        };
        display.push_str(prefix);
        display.push_str(replacement.text);
        copied_to = end;
    }
    let Some(tail) = raw.get(copied_to..) else {
        return withheld(result, source, RedactionReason::UnsupportedSyntax);
    };
    display.push_str(tail);
    result.display = SanitizedDisplay::from_sanitized(display);
    result.capture = if redactions.is_empty() {
        ExpressionCapture::CompleteSyntax
    } else {
        ExpressionCapture::Redacted
    };
    (result, redactions)
}

fn withheld(
    mut expression: SourceExpression,
    source: EvidenceRef,
    reason: RedactionReason,
) -> (SourceExpression, Vec<SourceRedaction>) {
    expression.display = SanitizedDisplay::from_sanitized(UNSUPPORTED.into());
    expression.capture = ExpressionCapture::Unsupported;
    (expression, vec![SourceRedaction { source, reason }])
}

fn redaction_reason(kind: SecretKind) -> RedactionReason {
    match kind {
        SecretKind::PrivateKey => RedactionReason::PrivateKey,
        SecretKind::BearerToken => RedactionReason::BearerToken,
        SecretKind::ProviderToken => RedactionReason::ProviderToken,
        SecretKind::AwsAccessKey => RedactionReason::AwsAccessKey,
        SecretKind::CredentialAssignment => RedactionReason::CredentialValue,
    }
}

fn secret_reasons(value: &str) -> Vec<RedactionReason> {
    let mut reasons: Vec<_> = recognized_secrets(value)
        .into_iter()
        .map(redaction_reason)
        .collect();
    if rule_literal_token_shaped(value) {
        reasons.push(RedactionReason::TokenShaped);
    }
    reasons
}

fn supported_container(kind: &str) -> bool {
    matches!(
        kind,
        "parenthesized_expression"
            | "boolean_operator"
            | "not_operator"
            | "comparison_operator"
            | "binary_operator"
            | "unary_operator"
            | "conditional_expression"
            | "attribute"
            | "subscript"
            | "slice"
            | "call"
            | "argument_list"
            | "keyword_argument"
            | "list"
            | "tuple"
            | "set"
            | "dictionary"
            | "pair"
            | "expression_list"
            | "list_splat"
            | "dictionary_splat"
            | "await"
            | "concatenated_string"
    )
}

fn supported_token(kind: &str) -> bool {
    matches!(
        kind,
        "(" | ")"
            | "["
            | "]"
            | "{"
            | "}"
            | "."
            | ","
            | ":"
            | "="
            | "+"
            | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "**"
            | "@"
            | "~"
            | "&"
            | "|"
            | "^"
            | "<<"
            | ">>"
            | "<"
            | ">"
            | "<="
            | ">="
            | "=="
            | "!="
            | "<>"
            | "and"
            | "or"
            | "not"
            | "in"
            | "is"
            | "is not"
            | "not in"
            | "if"
            | "else"
            | "await"
    )
}

enum StringParts<'t> {
    /// The decoded value of a plain string.
    Plain(String),
    /// An f-string's literal chunks and interpolated expressions.
    Formatted {
        chunks: Vec<TsNode<'t>>,
        expressions: Vec<TsNode<'t>>,
    },
}

fn string_parts<'t>(
    cx: &FileCx<'_>,
    string: TsNode<'t>,
) -> Result<StringParts<'t>, RedactionReason> {
    let mut cursor = string.walk();
    let children: Vec<_> = string.named_children(&mut cursor).collect();
    let start = children
        .iter()
        .find(|child| child.kind() == "string_start")
        .map(|start| cx.text(start))
        .ok_or(RedactionReason::UnsupportedSyntax)?;
    let prefix: String = start
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let raw = prefix.contains('r');
    if prefix.contains('b') || prefix.contains('t') {
        return Err(RedactionReason::UnsupportedDecoding);
    }
    if prefix.contains('f') {
        let mut chunks = Vec::new();
        let mut expressions = Vec::new();
        for child in children {
            match child.kind() {
                "string_start" | "string_end" => {}
                "string_content" => chunks.push(child),
                "interpolation" => {
                    if child.child_by_field_name("format_specifier").is_some() {
                        return Err(RedactionReason::UnsupportedSyntax);
                    }
                    let Some(expression) = child.child_by_field_name("expression") else {
                        return Err(RedactionReason::UnsupportedSyntax);
                    };
                    expressions.push(expression);
                }
                _ => return Err(RedactionReason::UnsupportedSyntax),
            }
        }
        return Ok(StringParts::Formatted {
            chunks,
            expressions,
        });
    }
    let mut value = String::new();
    for child in children {
        match child.kind() {
            "string_start" | "string_end" => {}
            "string_content" => {
                let text = cx.text(&child);
                if raw {
                    value.push_str(text);
                } else {
                    value.push_str(
                        &decode_escapes(text).map_err(|()| RedactionReason::UnsupportedDecoding)?,
                    );
                }
            }
            _ => return Err(RedactionReason::UnsupportedSyntax),
        }
    }
    Ok(StringParts::Plain(value))
}

fn decode_escapes(text: &str) -> Result<String, ()> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().ok_or(())? {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            'x' => out.push(read_hex(&mut chars, 2)?),
            'u' => out.push(read_hex(&mut chars, 4)?),
            'U' => out.push(read_hex(&mut chars, 8)?),
            // Octal and named escapes are not decoded here.
            _ => return Err(()),
        }
    }
    Ok(out)
}

fn read_hex(chars: &mut std::str::Chars<'_>, count: usize) -> Result<char, ()> {
    let mut value = 0u32;
    for _ in 0..count {
        value = value * 16 + chars.next().and_then(|c| c.to_digit(16)).ok_or(())?;
    }
    char::from_u32(value).ok_or(())
}

fn decode_number(raw: &str) -> Result<serde_json::Number, ()> {
    if (raw.starts_with('0') && raw.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
        || !raw
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        return Err(());
    }
    if raw.chars().all(|c| c.is_ascii_digit()) {
        return raw
            .parse::<u64>()
            .map(serde_json::Number::from)
            .map_err(|_| ());
    }
    serde_json::Number::from_f64(raw.parse::<f64>().map_err(|_| ())?).ok_or(())
}

/// Whether a string literal is the value of a sensitive name: assigned to
/// it, passed as its keyword, keyed by it, or compared with it.
fn sensitive_context(cx: &FileCx<'_>, mut child: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        let Some(parent) = child.parent() else {
            return Ok(false);
        };
        match parent.kind() {
            "function_definition" | "lambda" | "class_definition" | "block" => return Ok(false),
            "assignment" | "augmented_assignment" => {
                if parent.child_by_field_name("right") == Some(child)
                    && let Some(key) = parent.child_by_field_name("left")
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
            }
            "keyword_argument" => {
                if parent.child_by_field_name("value") == Some(child)
                    && let Some(key) = parent.child_by_field_name("name")
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
            }
            "pair" => {
                if parent.child_by_field_name("value") == Some(child)
                    && let Some(key) = parent.child_by_field_name("key")
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
            }
            "comparison_operator" => {
                let mut cursor = parent.walk();
                for operand in parent.named_children(&mut cursor) {
                    if operand != child && sensitive_key(cx, operand)? {
                        return Ok(true);
                    }
                }
            }
            _ => {}
        }
        child = parent;
    }
    Err(())
}

fn sensitive_key(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        match node.kind() {
            "identifier" => return Ok(is_sensitive_name(cx.text(&node))),
            "string" => {
                return Ok(match string_parts(cx, node) {
                    Ok(StringParts::Plain(value)) => is_sensitive_name(&value),
                    _ => false,
                });
            }
            "attribute" => node = node.child_by_field_name("attribute").ok_or(())?,
            "subscript" => {
                let key = node.child_by_field_name("subscript").ok_or(())?;
                if key.kind() != "string" {
                    return Ok(false);
                }
                node = key;
            }
            "parenthesized_expression" => {
                let mut cursor = node.walk();
                node = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() != "comment")
                    .ok_or(())?;
            }
            _ => return Ok(false),
        }
    }
    Err(())
}

#[cfg(test)]
mod tests;
