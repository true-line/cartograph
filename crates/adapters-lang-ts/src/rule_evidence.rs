//! Guarded local exits as cited source observations, never business verdicts.

use super::{Extraction, FileCx, TsNode, callable, source_expression::sanitize_expression};
use core_graph::rules::*;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DEPENDENCIES: usize = 128;
const MAX_CONDITIONS: usize = 32;
const MAX_ANCESTOR_STEPS: usize = 256;
const MAX_EXPRESSION_BYTES: usize = 8 * 1024;
const MAX_RULE_BYTES: usize = 64 * 1024;
const MAX_FILE_RULE_BYTES: usize = 1024 * 1024;
const MAX_RULES_PER_FILE: usize = 256;

pub(super) fn source(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
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
            "The caller's interpretation of this local return or throw is unknown."
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
        "switch_control" => "Switch control flow in the callable has not been interpreted.",
        "exception_control" => {
            "Exception or finally behavior in the callable has not been interpreted."
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
        "local_definition_unsupported" => {
            "The initializer contains source forms outside supported structural capture."
        }
        "definition_limit" => {
            "A local-definition capture limit was reached; initializer evidence remains incomplete."
        }
        "definition_order_unproven" => {
            "Declaration initialization before this original read was not established."
        }
        "definition_scope_unsupported" => {
            "Initializer recovery crosses an unsupported callable or control-flow boundary."
        }
        "definition_binding_unsupported" => {
            "This binding is not a stable, directly initialized simple local const."
        }
        "runtime_value_unknown" => {
            "The value of this runtime input or member read has not been established."
        }
        _ => "Source-rule interpretation remains incomplete.",
    }
}

struct RuleBuilder<'cx, 'tree> {
    cx: &'cx FileCx<'cx>,
    rule_id: String,
    gaps: BTreeMap<String, (String, TsNode<'tree>)>,
    dependencies: Vec<RuleDependency>,
    definition_seeds: Vec<TsNode<'tree>>,
    /// Whether the exit value's names are followed to their definitions:
    /// for an unguarded choice the value is the rule (AC-0956).
    seed_values: bool,
}

impl<'cx, 'tree> RuleBuilder<'cx, 'tree> {
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
        // The Gap retains its own original provenance and is always emitted
        // with a DEPENDS_ON edge plus interpretation reference. Do not exceed
        // the v2 prerequisite cap merely to duplicate that source in this list.
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

    fn dependencies(
        &mut self,
        expression: TsNode<'tree>,
        role: DependencyRole,
        bindings: &callable::Bindings<'tree>,
        targets: &BTreeSet<String>,
    ) {
        let Some(owner) = callable::enclosing(self.cx, expression) else {
            self.control_gap(RuleGapReason::ExecutionPredicateUnknown, expression);
            return;
        };
        let mut stack = vec![expression];
        while let Some(node) = stack.pop() {
            if self.dependencies.len() >= MAX_DEPENDENCIES {
                self.control_gap(RuleGapReason::DependencyLimit, expression);
                return;
            }
            // Static initialization can execute during class creation, but its
            // separate execution scope is not modeled. Preserve the actual
            // omitted location even when the generic interpretation Gap exists.
            let static_initializer = node.parent().is_some_and(|parent| {
                if parent.kind() != "public_field_definition"
                    || parent.child_by_field_name("value") != Some(node)
                {
                    return false;
                }
                let mut cursor = parent.walk();
                parent
                    .children(&mut cursor)
                    .any(|child| child.kind() == "static")
            });
            if node.kind() == "class_static_block" || static_initializer {
                let gap_id = self.gap(RuleGapReason::ExecutionPredicateUnknown, node);
                self.dependencies.push(RuleDependency {
                    role,
                    source: source(self.cx, node),
                    resolution: DependencyResolution::Unresolved { gap_id },
                });
                continue;
            }
            if callable::enclosing(self.cx, node).as_deref() != Some(owner.as_str()) {
                // In particular, instance field initializers are deferred
                // until construction, not evaluation of the class expression.
                self.control_gap(RuleGapReason::ExecutionPredicateUnknown, node);
                continue;
            }
            // Callable bodies and parameter defaults are deferred execution.
            // A method's computed name and decorators instead execute while
            // the object/class is created and remain expression prerequisites.
            if callable::is_callable(node) {
                self.control_gap(RuleGapReason::ExecutionPredicateUnknown, node);
                if node.kind() == "method_definition" {
                    let mut creation = Vec::new();
                    if let Some(key) = node
                        .child_by_field_name("name")
                        .filter(|key| key.kind() == "computed_property_name")
                    {
                        creation.push(key);
                    }
                    let mut cursor = node.walk();
                    creation.extend(
                        node.named_children(&mut cursor)
                            .filter(|child| child.kind() == "decorator"),
                    );
                    if let Some(parameters) = node.child_by_field_name("parameters") {
                        let mut cursor = parameters.walk();
                        for parameter in parameters.named_children(&mut cursor) {
                            let mut cursor = parameter.walk();
                            creation.extend(
                                parameter
                                    .named_children(&mut cursor)
                                    .filter(|child| child.kind() == "decorator"),
                            );
                        }
                    }
                    creation.sort_by_key(|node| node.start_byte());
                    stack.extend(creation.into_iter().rev());
                }
                continue;
            }
            let resolution = if matches!(node.kind(), "call_expression" | "new_expression") {
                node.child_by_field_name("function")
                    .filter(|callee| callee.kind() == "identifier")
                    .and_then(|callee| bindings.local_target(callee, self.cx.text(&callee)))
                    .filter(|target| targets.contains(target))
                    .map(|node_id| DependencyResolution::Target { node_id })
                    .or_else(|| {
                        Some(DependencyResolution::Unresolved {
                            gap_id: self.gap(RuleGapReason::UnresolvedCall, node),
                        })
                    })
            } else if is_value_identifier(node) {
                if role == DependencyRole::Condition
                    || (self.seed_values && role == DependencyRole::ExitValue)
                {
                    self.definition_seeds.push(node);
                }
                Some(
                    bindings
                        .stable_declaration(node, self.cx.text(&node))
                        .map(|declaration| DependencyResolution::Binding {
                            binding_id: format!(
                                "binding:{}@{}#declaration@{}",
                                self.cx.id.repo,
                                self.cx.path,
                                declaration.start_byte()
                            ),
                            declaration: source(self.cx, declaration),
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

fn is_value_identifier(node: TsNode<'_>) -> bool {
    if !matches!(node.kind(), "identifier" | "shorthand_property_identifier") {
        return false;
    }
    !node.parent().is_some_and(|parent| {
        (parent.kind() == "pair" && parent.child_by_field_name("key") == Some(node))
            || (parent.kind() == "member_expression"
                && parent.child_by_field_name("property") == Some(node))
    })
}

fn branches(mut exit: TsNode<'_>) -> (Vec<(TsNode<'_>, TsNode<'_>, BranchPolarity)>, bool, bool) {
    let mut conditions = Vec::new();
    let mut steps = 0;
    let mut limited = false;
    let mut recovered = exit.has_error() || exit.is_missing() || exit.is_error();
    while let Some(parent) = exit.parent() {
        if callable::is_callable(parent) {
            break;
        }
        steps += 1;
        if steps > MAX_ANCESTOR_STEPS || conditions.len() == MAX_CONDITIONS {
            limited = true;
            break;
        }
        // A valid-looking child cannot establish an exit through a recovered
        // wrapper or controlling branch. Unsupported sanitized expressions
        // remain useful only when the parser established the original syntax.
        recovered |= parent.is_error()
            || parent.is_missing()
            || (parent.kind() == "if_statement" && parent.has_error());
        if parent.kind() == "if_statement"
            && let Some(condition) = parent.child_by_field_name("condition")
        {
            let polarity = if parent.child_by_field_name("consequence") == Some(exit) {
                Some(BranchPolarity::TruthyBranch)
            } else if parent.child_by_field_name("alternative") == Some(exit) {
                Some(BranchPolarity::FalsyBranch)
            } else {
                None
            };
            if let Some(polarity) = polarity {
                conditions.push((parent, condition, polarity));
            }
        }
        exit = parent;
    }
    conditions.reverse();
    (conditions, limited, recovered)
}

/// Interface names whose implementations are configured choices (#2069).
const STRATEGY_SUFFIXES: [&str; 4] = ["Strategy", "Handler", "Calculator", "Process"];

/// Whether an unguarded `return` states how a value is chosen (AC-0956),
/// so it is a rule though no branch guards it:
/// - the answer of a non-private predicate, a method named `isX`, `hasX`,
///   `canX`, `shouldX` or `mustX` or declared `: boolean`
///   (`test(zone, category): boolean { return … && … }`), unless a constant;
/// - a pick with a fallback, `return found || fallback` (or `??`) where
///   `found` is, or is declared as, a `.find(…)` (`rate || this.defaultTaxRate`);
/// - the answer of a configured strategy, a method of a class that
///   `implements` an interface named for one (`DefaultTaxZoneStrategy`).
fn answers_a_choice(cx: &FileCx<'_>, exit: TsNode<'_>) -> bool {
    if exit.kind() != "return_statement" {
        return false;
    }
    let Some(value) = exit_value(exit).map(unparenthesized) else {
        return false;
    };
    if matches!(value.kind(), "true" | "false" | "null" | "undefined") {
        return false;
    }
    let mut owner = exit;
    let callable = loop {
        let Some(parent) = owner.parent() else {
            return false;
        };
        if callable::is_callable(parent) {
            break parent;
        }
        owner = parent;
    };
    let name = callable
        .child_by_field_name("name")
        .map(|name| cx.text(&name))
        .unwrap_or_default();
    let private = cx.text(&callable).starts_with("private") || name.starts_with('#');
    let predicate = ["is", "has", "can", "should", "must"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
    }) || callable
        .child_by_field_name("return_type")
        .is_some_and(|ty| cx.text(&ty).trim_start_matches(':').trim() == "boolean");
    if predicate && !private {
        return true;
    }
    if value.kind() == "binary_expression"
        && value
            .child_by_field_name("operator")
            .is_some_and(|op| matches!(cx.text(&op), "||" | "??"))
        && let Some(left) = value.child_by_field_name("left").map(unparenthesized)
    {
        if is_find(cx, left) {
            return true;
        }
        if left.kind() == "identifier" && declared_as_find(cx, callable, left, exit) {
            return true;
        }
    }
    if computed_otherwise(cx, callable, exit, value) {
        return true;
    }
    callable.kind() == "method_definition" && !private && implements_a_strategy(cx, callable)
}

/// The value a callable computes when none of its guards returned first
/// (AC-1052): an arithmetic `return a - b - c` that is a statement of the
/// callable's own body, after an `if` that returns a value. The guarded
/// returns are rules already; this one says what is returned otherwise.
fn computed_otherwise(
    cx: &FileCx<'_>,
    callable: TsNode<'_>,
    exit: TsNode<'_>,
    value: TsNode<'_>,
) -> bool {
    let arithmetic = value.kind() == "binary_expression"
        && value
            .child_by_field_name("operator")
            .is_some_and(|op| matches!(cx.text(&op), "+" | "-" | "*" | "/" | "%"));
    let Some(body) = callable.child_by_field_name("body") else {
        return false;
    };
    if !arithmetic || exit.parent() != Some(body) {
        return false;
    }
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .take_while(|statement| statement.end_byte() <= exit.start_byte())
        .filter(|statement| statement.kind() == "if_statement")
        .any(|guard| returns_a_value(guard, 0))
}

/// Whether `node` holds a `return v;` outside nested callables, within a
/// bounded depth.
fn returns_a_value(node: TsNode<'_>, depth: usize) -> bool {
    if depth > 16 || callable::is_callable(node) {
        return false;
    }
    if node.kind() == "return_statement" {
        return exit_value(node).is_some();
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| returns_a_value(child, depth + 1))
}

/// Whether `node` is a `.find(…)` call, through parentheses and `await`.
fn is_find(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    let mut node = unparenthesized(node);
    while node.kind() == "await_expression" {
        let mut cursor = node.walk();
        let Some(inner) = node
            .named_children(&mut cursor)
            .find(|n| n.kind() != "comment")
        else {
            return false;
        };
        node = unparenthesized(inner);
    }
    node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "member_expression")
            .and_then(|f| f.child_by_field_name("property"))
            .is_some_and(|p| cx.text(&p) == "find")
}

/// Whether the returned `name` is declared once in `callable`'s own body,
/// before the return and outside any nested callable, as a `.find(…)`.
fn declared_as_find(
    cx: &FileCx<'_>,
    callable: TsNode<'_>,
    name: TsNode<'_>,
    exit: TsNode<'_>,
) -> bool {
    let Some(body) = callable.child_by_field_name("body") else {
        return false;
    };
    let wanted = cx.text(&name);
    let declarators: Vec<TsNode<'_>> = callable::walk(body)
        .into_iter()
        .filter(|node| {
            node.kind() == "variable_declarator"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|n| cx.text(&n) == wanted)
        })
        .collect();
    let [declarator] = declarators.as_slice() else {
        return false;
    };
    let mut up = *declarator;
    while let Some(parent) = up.parent() {
        if parent.id() == callable.id() {
            break;
        }
        if callable::is_callable(parent) {
            return false;
        }
        up = parent;
    }
    declarator.end_byte() <= exit.start_byte()
        && declarator
            .child_by_field_name("value")
            .is_some_and(|value| is_find(cx, value))
}

fn unparenthesized(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "parenthesized_expression" {
        let mut cursor = node.walk();
        let Some(inner) = node
            .named_children(&mut cursor)
            .find(|n| n.kind() != "comment")
        else {
            break;
        };
        node = inner;
    }
    node
}

/// Whether `method` is declared in a class whose `implements` clause names
/// an interface for a configured strategy: the implemented type's own name,
/// not a type argument or a comment.
fn implements_a_strategy(cx: &FileCx<'_>, method: TsNode<'_>) -> bool {
    let mut node = method;
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "class_declaration" | "class") {
            let mut cursor = parent.walk();
            let heritages: Vec<TsNode<'_>> = parent
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "class_heritage")
                .collect();
            return heritages.into_iter().any(|heritage| {
                let mut cursor = heritage.walk();
                let clauses: Vec<TsNode<'_>> = heritage
                    .named_children(&mut cursor)
                    .filter(|clause| clause.kind() == "implements_clause")
                    .collect();
                clauses.into_iter().any(|clause| {
                    let mut cursor = clause.walk();
                    let types: Vec<TsNode<'_>> = clause.named_children(&mut cursor).collect();
                    types.into_iter().any(|ty| {
                        let name = match ty.kind() {
                            "type_identifier" => Some(ty),
                            "generic_type" => ty.child_by_field_name("name"),
                            _ => None,
                        };
                        name.filter(|n| n.kind() == "type_identifier")
                            .is_some_and(|n| {
                                let word = cx.text(&n);
                                STRATEGY_SUFFIXES.iter().any(|s| word.ends_with(s))
                            })
                    })
                })
            });
        }
        node = parent;
    }
    false
}

fn exit_value(exit: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut cursor = exit.walk();
    exit.named_children(&mut cursor)
        .find(|node| node.kind() != "comment")
}

fn omission_source(mut node: TsNode<'_>) -> TsNode<'_> {
    // Missing parser nodes can be zero-width. Cite the enclosing source that
    // failed to parse instead of manufacturing an expression or empty span.
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

/// The Gap that ends a file's rule recovery at `at`. `omitted` counts the
/// exits and effects from `at` on that would each have been considered for
/// a rule (AC-1048), so a reader knows how much of the file is missing.
fn omission_gap(
    cx: &FileCx<'_>,
    at: TsNode<'_>,
    owner: &str,
    reason: RuleGapReason,
    omitted: usize,
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
        "omitted_rule_sites": omitted,
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

pub(super) fn capture(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
) -> (SourceExpression, Vec<SourceRedaction>) {
    if node.byte_range().len() <= MAX_EXPRESSION_BYTES {
        return sanitize_expression(cx, node);
    }
    let source = source(cx, node);
    (
        SourceExpression {
            source: source.clone(),
            syntax_kind: node.kind().into(),
            display: SanitizedDisplay::from_sanitized(
                "[expression omitted: analysis limit]".into(),
            ),
            capture: ExpressionCapture::Unsupported,
            literal: None,
        },
        vec![SourceRedaction {
            source,
            reason: RedactionReason::UnsupportedSyntax,
        }],
    )
}

fn control_reason(kind: &str) -> Option<RuleGapReason> {
    match kind {
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
            Some(RuleGapReason::LoopDependency)
        }
        "switch_statement" => Some(RuleGapReason::SwitchControl),
        "try_statement" | "catch_clause" | "finally_clause" => {
            Some(RuleGapReason::ExceptionControl)
        }
        "assignment_expression" | "augmented_assignment_expression" | "update_expression" => {
            Some(RuleGapReason::Mutation)
        }
        _ => None,
    }
}

pub(super) fn extract<'tree>(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'tree>],
    bindings: &callable::Bindings<'tree>,
    out: &mut Extraction,
    mut direct: Option<&mut super::captured::DirectFacts>,
) {
    let targets: BTreeSet<_> = out.nodes.iter().map(|node| node.id.clone()).collect();
    // Collect once by callable, rather than repeatedly scanning the whole file.
    // Recovery may erase an entire exit or callable. Keep that incomplete
    // syntax as an omission candidate, anchored to the existing File when no
    // real callable can be established; do not infer an exit from raw text.
    let mut exits: BTreeMap<String, Vec<TsNode<'tree>>> = BTreeMap::new();
    let mut controls: BTreeMap<String, BTreeMap<String, (RuleGapReason, TsNode<'tree>)>> =
        BTreeMap::new();
    let mut effects: BTreeMap<String, Vec<(TsNode<'tree>, EffectKind<'tree>)>> = BTreeMap::new();
    for node in nodes.iter().copied() {
        if node.is_error()
            || node.is_missing()
            || (node.kind() == "if_statement" && node.has_error())
        {
            let owner = callable::enclosing(cx, node)
                .filter(|owner| targets.contains(owner))
                .unwrap_or_else(|| super::file_id(cx.id.repo, cx.path));
            exits.entry(owner).or_default().push(node);
        } else if matches!(node.kind(), "return_statement" | "throw_statement") {
            if let Some(owner) = callable::enclosing(cx, node) {
                exits.entry(owner).or_default().push(node);
            }
        } else if let Some(kind) = effect_statement(cx, node)
            && let Some(owner) = callable::enclosing(cx, node)
        {
            effects.entry(owner).or_default().push((node, kind));
        } else if let Some(reason) = control_reason(node.kind())
            && let Some(owner) = callable::enclosing(cx, node)
        {
            controls
                .entry(owner)
                .or_default()
                .entry(reason_key(reason))
                .or_insert((reason, node));
        }
    }
    // Exits and effects that would each be considered for a rule, so a Gap
    // that ends the file's recovery can say how many were not (AC-1048).
    let exit_site = |exit: TsNode<'tree>| {
        let (ancestors, limited, recovered) = branches(exit);
        recovered
            || (exit.kind() == "throw_statement" && exit_value(exit).is_none())
            || !ancestors.is_empty()
            || limited
            || answers_a_choice(cx, exit)
    };
    let effect_site = |at: TsNode<'tree>| {
        let (ancestors, _, recovered) = branches(at);
        !recovered && !ancestors.is_empty()
    };
    let exits_total = exits
        .iter()
        .filter(|(owner, _)| targets.contains(*owner))
        .flat_map(|(_, sites)| sites.iter().copied())
        .filter(|exit| exit_site(*exit))
        .count();
    let effects_total = effects
        .iter()
        .filter(|(owner, _)| targets.contains(*owner))
        .flat_map(|(_, sites)| sites.iter().map(|(at, _)| *at))
        .filter(|at| effect_site(*at))
        .count();
    let mut exits_seen = 0;
    let mut effects_seen = 0;
    let mut rule_count = 0;
    let mut file_bytes = 0;
    for (owner_id, owner_exits) in &exits {
        let owner_id = owner_id.clone();
        if !targets.contains(&owner_id) {
            continue;
        }
        for (order, exit) in owner_exits.iter().copied().enumerate() {
            let (ancestors, limited, recovered) = branches(exit);
            if exit_site(exit) {
                exits_seen += 1;
            }
            // A Gap here omits this exit, every later one and every effect.
            let exits_left = exits_total + 1 - exits_seen;
            if recovered || (exit.kind() == "throw_statement" && exit_value(exit).is_none()) {
                omission_gap(
                    cx,
                    omission_source(exit),
                    &owner_id,
                    RuleGapReason::UnsupportedRuleSyntax,
                    exits_left + effects_total,
                    out,
                );
                return;
            }
            let choice = ancestors.is_empty() && !limited && answers_a_choice(cx, exit);
            if ancestors.is_empty() && !limited && !choice {
                continue;
            }
            if rule_count == MAX_RULES_PER_FILE || file_bytes >= MAX_FILE_RULE_BYTES {
                omission_gap(
                    cx,
                    exit,
                    &owner_id,
                    RuleGapReason::AnalysisLimit,
                    exits_left + effects_total,
                    out,
                );
                return;
            }
            if limited {
                omission_gap(
                    cx,
                    exit,
                    &owner_id,
                    RuleGapReason::AnalysisLimit,
                    exits_left + effects_total,
                    out,
                );
                return;
            }
            let rule_id = format!("rule:{}@{}#exit@{}", cx.id.repo, cx.path, exit.start_byte());
            let mut builder = RuleBuilder {
                cx,
                rule_id: rule_id.clone(),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
                definition_seeds: Vec::new(),
                seed_values: choice,
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
                    builder.dependencies(condition, DependencyRole::Condition, bindings, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, condition);
                }
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let value = exit_value(exit).map(|node| {
                let (expression, mut withheld) = capture(cx, node);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, node);
                }
                redactions.append(&mut withheld);
                if node.byte_range().len() <= MAX_EXPRESSION_BYTES {
                    builder.dependencies(node, DependencyRole::ExitValue, bindings, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, node);
                }
                expression
            });
            let effect = if exit.kind() == "throw_statement" {
                let Some(value) = value else {
                    omission_gap(
                        cx,
                        exit,
                        &owner_id,
                        RuleGapReason::UnsupportedRuleSyntax,
                        exits_left + effects_total,
                        out,
                    );
                    return;
                };
                LocalExit::Throw { value }
            } else {
                LocalExit::Return { value }
            };
            let seeds = std::mem::take(&mut builder.definition_seeds);
            let (local_definitions, mut definition_redactions) = super::local_definitions::recover(
                cx,
                bindings,
                &owner_id,
                &seeds,
                &mut |reason, at| {
                    builder.control_gap(reason, at);
                    builder.gap(reason, at)
                },
            );
            redactions.append(&mut definition_redactions);
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind: RuleKind::GuardedExit,
                owner_id: owner_id.clone(),
                exit_source: source(cx, exit),
                source_order: order as u32,
                conditions,
                effect,
                dependencies: builder.dependencies,
                local_definitions: Some(local_definitions),
                interpretation: Interpretation {
                    execution_predicate: InterpretationStatus::NotEstablished,
                    consumer_effect: InterpretationStatus::NotEstablished,
                    gap_ids: builder.gaps.values().map(|(id, _)| id.clone()).collect(),
                },
                redactions,
            };
            if let Err(error) = evidence.validate() {
                omission_gap(
                    cx,
                    exit,
                    &owner_id,
                    if matches!(error, RuleValidationError::Limit(_)) {
                        RuleGapReason::AnalysisLimit
                    } else {
                        RuleGapReason::UnsupportedRuleSyntax
                    },
                    exits_left + effects_total,
                    out,
                );
                return;
            }
            let payload = serde_json::to_value(evidence).expect("rule serializes");
            let canonical = serde_json::to_string(&payload).expect("rule serializes");
            if canonical.len() > MAX_RULE_BYTES
                || file_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(
                    cx,
                    exit,
                    &owner_id,
                    RuleGapReason::AnalysisLimit,
                    exits_left + effects_total,
                    out,
                );
                return;
            }
            file_bytes += canonical.len();
            rule_count += 1;
            let rule_node = Node {
                id: rule_id.clone(),
                label: "BusinessRule".into(),
                props: serde_json::json!({
                    "name": format!("Guarded local exit at {}:{}", cx.path, exit.start_position().row + 1),
                    "observation": "guarded_exit",
                    "rule": payload,
                    "prov": cx.prov(&exit, &canonical),
                }),
            };
            let governs = Edge {
                src: rule_id.clone(),
                dst: owner_id.clone(),
                label: "GOVERNS".into(),
                props: serde_json::json!({"observation": "lexical_owner", "prov": cx.prov(&exit, &format!("GOVERNS {rule_id} -> {owner_id}"))}),
            };
            // Only this actual lexical producer marks participating outputs.
            // Synthetic eval parses invoke the public parser without this sink.
            if let Some(direct) = direct.as_deref_mut() {
                direct.rules.push((rule_node.clone(), governs.clone()));
            }
            out.nodes.push(rule_node);
            out.edges.push(governs);
            for (reason, (gap_id, at)) in builder.gaps {
                let props = serde_json::json!({
                    "rule_evidence_gap": true,
                    "reason_code": reason,
                    "reason": reason_description(&reason),
                    "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                });
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: props.clone(),
                });
                out.edges.push(Edge {
                    src: rule_id.clone(),
                    dst: gap_id,
                    label: "DEPENDS_ON".into(),
                    props,
                });
            }
        }
    }

    // Guarded effects (#1998): a repository write, a published event, a
    // service call or a field change under at least one branch condition.
    let mut effect_count = 0;
    for (owner_id, owner_effects) in effects {
        if !targets.contains(&owner_id) {
            continue;
        }
        let owner_exits = exits.get(&owner_id).map_or(&[][..], Vec::as_slice);
        for (order, (at, kind)) in owner_effects.into_iter().enumerate() {
            let (ancestors, limited, recovered) = branches(at);
            if recovered || ancestors.is_empty() {
                continue;
            }
            effects_seen += 1;
            if limited || effect_count == MAX_RULES_PER_FILE || file_bytes >= MAX_FILE_RULE_BYTES {
                omission_gap(
                    cx,
                    at,
                    &owner_id,
                    RuleGapReason::AnalysisLimit,
                    effects_total + 1 - effects_seen,
                    out,
                );
                return;
            }
            let rule_id = format!("rule:{}@{}#effect@{}", cx.id.repo, cx.path, at.start_byte());
            let mut builder = RuleBuilder {
                cx,
                // One set of interpretation Gaps per callable's effects.
                rule_id: format!("{owner_id}#effects"),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
                definition_seeds: Vec::new(),
                seed_values: false,
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
                    builder.dependencies(condition, DependencyRole::Condition, bindings, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, condition);
                }
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let mut captured = |node: TsNode<'tree>, builder: &mut RuleBuilder<'_, 'tree>| {
                let (expression, mut withheld) = capture(cx, node);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, node);
                }
                redactions.append(&mut withheld);
                expression
            };
            let (effect, read) = match kind {
                EffectKind::Assign { target, value } => (
                    LocalExit::Assign {
                        target: captured(target, &mut builder),
                        value: captured(value, &mut builder),
                    },
                    value,
                ),
                EffectKind::Call { call } => (
                    LocalExit::Call {
                        value: captured(call, &mut builder),
                    },
                    call,
                ),
            };
            if read.byte_range().len() <= MAX_EXPRESSION_BYTES {
                builder.dependencies(read, DependencyRole::ExitValue, bindings, &targets);
            } else {
                builder.control_gap(RuleGapReason::AnalysisLimit, read);
            }
            // A push onto a collected list (AC-1052) and an accumulated
            // total (AC-1054) keep the definitions their conditions test, so
            // a reader learns what decides what is collected; other changes
            // keep none, which bounds every file's rule payload.
            let local_definitions =
                if collected_push(cx, read) || read.kind() == "augmented_assignment_expression" {
                    let seeds = std::mem::take(&mut builder.definition_seeds);
                    let (definitions, mut withheld) = super::local_definitions::recover(
                        cx,
                        bindings,
                        &owner_id,
                        &seeds,
                        &mut |reason, at| {
                            builder.control_gap(reason, at);
                            builder.gap(reason, at)
                        },
                    );
                    redactions.append(&mut withheld);
                    definitions
                } else {
                    Vec::new()
                };
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind: RuleKind::GuardedEffect,
                owner_id: owner_id.clone(),
                exit_source: source(cx, at),
                source_order: order as u32,
                conditions,
                effect,
                dependencies: builder.dependencies,
                local_definitions: Some(local_definitions),
                interpretation: Interpretation {
                    execution_predicate: InterpretationStatus::NotEstablished,
                    consumer_effect: InterpretationStatus::NotEstablished,
                    gap_ids: builder.gaps.values().map(|(id, _)| id.clone()).collect(),
                },
                redactions,
            };
            if let Err(error) = evidence.validate() {
                omission_gap(
                    cx,
                    at,
                    &owner_id,
                    if matches!(error, RuleValidationError::Limit(_)) {
                        RuleGapReason::AnalysisLimit
                    } else {
                        RuleGapReason::UnsupportedRuleSyntax
                    },
                    effects_total + 1 - effects_seen,
                    out,
                );
                return;
            }
            let payload = serde_json::to_value(evidence).expect("rule serializes");
            let canonical = serde_json::to_string(&payload).expect("rule serializes");
            if canonical.len() > MAX_RULE_BYTES
                || file_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(
                    cx,
                    at,
                    &owner_id,
                    RuleGapReason::AnalysisLimit,
                    effects_total + 1 - effects_seen,
                    out,
                );
                return;
            }
            file_bytes += canonical.len();
            effect_count += 1;
            let rule_node = Node {
                id: rule_id.clone(),
                label: "BusinessRule".into(),
                props: serde_json::json!({
                    "name": format!("Guarded local effect at {}:{}", cx.path, at.start_position().row + 1),
                    "observation": "guarded_effect",
                    "rule": payload,
                    "prov": cx.prov(&at, &canonical),
                }),
            };
            let governs = Edge {
                src: rule_id.clone(),
                dst: owner_id.clone(),
                label: "GOVERNS".into(),
                props: serde_json::json!({"observation": "lexical_owner", "prov": cx.prov(&at, &format!("GOVERNS {rule_id} -> {owner_id}"))}),
            };
            if let Some(direct) = direct.as_deref_mut() {
                direct.rules.push((rule_node.clone(), governs.clone()));
            }
            out.nodes.push(rule_node);
            out.edges.push(governs);
            for (reason, (gap_id, at)) in builder.gaps {
                // Shared by the callable's effects: emitted once.
                if !out.nodes.iter().any(|node| node.id == gap_id) {
                    out.nodes.push(Node {
                        id: gap_id.clone(),
                        label: "Gap".into(),
                        props: serde_json::json!({
                            "rule_evidence_gap": true,
                            "reason_code": reason,
                            "reason": reason_description(&reason),
                            "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                        }),
                    });
                }
                let props = serde_json::json!({
                    "rule_evidence_gap": true,
                    "reason_code": reason,
                    "reason": reason_description(&reason),
                    "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                });
                out.edges.push(Edge {
                    src: rule_id.clone(),
                    dst: gap_id,
                    label: "DEPENDS_ON".into(),
                    props,
                });
            }
        }
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

/// Repository methods that write (`getRepository(ctx, Order).save(order)`).
const REPOSITORY_WRITES: &[&str] = &[
    "save",
    "insert",
    "update",
    "upsert",
    "delete",
    "remove",
    "softDelete",
    "softRemove",
    "increment",
    "decrement",
];

/// Service method prefixes that read rather than act.
const SERVICE_READS: &[&str] = &[
    "get", "find", "is", "has", "can", "count", "list", "check", "validate", "assert",
];

/// A statement that is a guarded effect (#1998), optionally awaited: an
/// assignment through a member not on `this` (`order.active = false`), a repository write
/// (`getRepository(ctx, Order).save(…)`, `this.orderRepository.delete(…)`),
/// `this.eventBus.publish(…)`, or a call of a `this.…Service` method that is
/// not a read (`this.historyService.createHistoryEntryForOrder(…)`).
/// Constructor bodies are not effects.
fn effect_statement<'t>(cx: &FileCx<'_>, node: TsNode<'t>) -> Option<EffectKind<'t>> {
    if node.kind() != "expression_statement" || node.named_child_count() != 1 {
        return None;
    }
    let mut inner = unparenthesized(node.named_child(0)?);
    if inner.kind() == "await_expression" {
        inner = unparenthesized(inner.named_child(0)?);
    }
    let kind = match inner.kind() {
        "assignment_expression" => {
            let target = inner.child_by_field_name("left")?;
            let value = inner.child_by_field_name("right")?;
            // `this.cache = …` is the object's own bookkeeping.
            let own = target
                .child_by_field_name("object")
                .is_some_and(|object| object.kind() == "this");
            (target.kind() == "member_expression" && !own)
                .then_some(EffectKind::Assign { target, value })?
        }
        "call_expression" => effect_call(cx, inner).then_some(EffectKind::Call { call: inner })?,
        // `stockOnHand += stockLevel.stockOnHand` on a total the callable
        // starts at a number and returns (AC-1054). The value kept is the
        // whole statement, so its operator is kept with it.
        "augmented_assignment_expression" => {
            let target = inner.child_by_field_name("left")?;
            let adds = inner
                .child_by_field_name("operator")
                .is_some_and(|op| matches!(cx.text(&op), "+=" | "-="));
            (adds && target.kind() == "identifier" && accumulated_total(cx, target)).then_some(
                EffectKind::Assign {
                    target,
                    value: inner,
                },
            )?
        }
        _ => return None,
    };
    let mut up = node;
    while let Some(parent) = up.parent() {
        if callable::is_callable(parent) {
            let constructor = parent.kind() == "method_definition"
                && parent
                    .child_by_field_name("name")
                    .is_some_and(|name| cx.text(&name) == "constructor");
            return (!constructor).then_some(kind);
        }
        up = parent;
    }
    None
}

fn effect_call(cx: &FileCx<'_>, call: TsNode<'_>) -> bool {
    let Some(callee) = call
        .child_by_field_name("function")
        .filter(|callee| callee.kind() == "member_expression")
    else {
        return false;
    };
    let (Some(object), Some(property)) = (
        callee.child_by_field_name("object"),
        callee.child_by_field_name("property"),
    ) else {
        return false;
    };
    let verb = cx.text(&property);
    // `found.push(line.productVariant)` on a list the callable starts empty
    // (`const found = []`): a value collected for a later check (AC-1050).
    if verb == "push" && object.kind() == "identifier" && collected_list(cx, object) {
        return true;
    }
    if REPOSITORY_WRITES.contains(&verb) {
        let repository = object.kind() == "call_expression"
            && object
                .child_by_field_name("function")
                .filter(|f| f.kind() == "member_expression")
                .and_then(|f| f.child_by_field_name("property"))
                .is_some_and(|p| cx.text(&p) == "getRepository");
        let named = object.kind() == "member_expression"
            && object
                .child_by_field_name("property")
                .is_some_and(|p| cx.text(&p).ends_with("Repository"));
        if repository || named {
            return true;
        }
    }
    if object.kind() != "member_expression"
        || !object
            .child_by_field_name("object")
            .is_some_and(|o| o.kind() == "this")
    {
        return false;
    }
    let Some(field) = object.child_by_field_name("property").map(|p| cx.text(&p)) else {
        return false;
    };
    (field == "eventBus" && verb == "publish")
        || (field.ends_with("Service")
            && !SERVICE_READS.iter().any(|read| {
                verb.strip_prefix(read).is_some_and(|rest| {
                    rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_uppercase())
                })
            }))
}

/// Whether `name` is a total the callable accumulates (AC-1054): a `let
/// name = <number>` declared earlier in a block that encloses it, in the
/// same callable, which a `return` of that callable gives back (`return
/// name`, or `{ name }` / `{ key: name }` in a returned object).
fn accumulated_total(cx: &FileCx<'_>, name: TsNode<'_>) -> bool {
    let spelled = cx.text(&name);
    let mut up = name;
    let mut declared = false;
    let callable = loop {
        let Some(parent) = up.parent() else {
            return false;
        };
        if callable::is_callable(parent) {
            break parent;
        }
        if !declared && parent.kind() == "statement_block" {
            let mut cursor = parent.walk();
            for statement in parent.named_children(&mut cursor) {
                if statement.kind() != "lexical_declaration"
                    || statement.end_byte() > name.start_byte()
                    || statement.child(0).is_none_or(|k| k.kind() != "let")
                {
                    continue;
                }
                let mut inner = statement.walk();
                declared |= statement.named_children(&mut inner).any(|d| {
                    d.kind() == "variable_declarator"
                        && d.child_by_field_name("name")
                            .is_some_and(|n| n.kind() == "identifier" && cx.text(&n) == spelled)
                        && d.child_by_field_name("value")
                            .is_some_and(|v| v.kind() == "number")
                });
            }
        }
        up = parent;
    };
    declared && returns_name(cx, callable, spelled, 0)
}

/// Whether a `return` of `node` (not of a nested callable) gives `name`
/// back, alone or as a property of a returned object.
fn returns_name(cx: &FileCx<'_>, node: TsNode<'_>, name: &str, depth: usize) -> bool {
    if depth > 24 {
        return false;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).any(|child| {
        if callable::is_callable(child) {
            return false;
        }
        if child.kind() == "return_statement" {
            let Some(value) = exit_value(child).map(unparenthesized) else {
                return false;
            };
            return match value.kind() {
                "identifier" => cx.text(&value) == name,
                "object" => {
                    let mut walk = value.walk();
                    value
                        .named_children(&mut walk)
                        .any(|member| match member.kind() {
                            "shorthand_property_identifier" => cx.text(&member) == name,
                            "pair" => member
                                .child_by_field_name("value")
                                .is_some_and(|v| v.kind() == "identifier" && cx.text(&v) == name),
                            _ => false,
                        })
                }
                _ => false,
            };
        }
        returns_name(cx, child, name, depth + 1)
    })
}

/// Whether a binder (a pattern or declaration) binds `name` anywhere in
/// it, within a bounded depth.
fn binds(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    fn walk(cx: &FileCx<'_>, node: TsNode<'_>, name: &str, depth: usize) -> bool {
        if depth > 16 {
            return true;
        }
        if matches!(
            node.kind(),
            "identifier" | "shorthand_property_identifier_pattern"
        ) {
            return cx.text(&node) == name;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| walk(cx, child, name, depth + 1))
    }
    walk(cx, node, name, 0)
}

/// Whether `call` is `found.push(v)` on a [`collected_list`].
fn collected_push(cx: &FileCx<'_>, call: TsNode<'_>) -> bool {
    call.kind() == "call_expression"
        && call
            .child_by_field_name("function")
            .filter(|f| f.kind() == "member_expression")
            .is_some_and(|f| {
                f.child_by_field_name("property")
                    .is_some_and(|p| cx.text(&p) == "push")
                    && f.child_by_field_name("object")
                        .is_some_and(|o| o.kind() == "identifier" && collected_list(cx, o))
            })
}

/// Whether `name` reads a `const name = []` (optionally typed) declared
/// earlier in a block that encloses it, within the same callable
/// (AC-1050). A `const` cannot be rebound, so every value it holds was
/// pushed into it.
fn collected_list(cx: &FileCx<'_>, name: TsNode<'_>) -> bool {
    let spelled = cx.text(&name);
    let mut up = name;
    for _ in 0..256 {
        let Some(parent) = up.parent() else {
            return false;
        };
        if callable::is_callable(parent) {
            return false;
        }
        // A loop or catch binder of the same name shadows any outer list
        // (`for (const found of groups) { found.push(v) }`): not one.
        let binder = match parent.kind() {
            "for_in_statement" => parent.child_by_field_name("left"),
            "catch_clause" => parent.child_by_field_name("parameter"),
            "for_statement" => parent.child_by_field_name("initializer"),
            _ => None,
        };
        if binder.is_some_and(|b| binds(cx, b, spelled)) {
            return false;
        }
        if parent.kind() == "statement_block" {
            // The nearest block that declares the name decides.
            let mut cursor = parent.walk();
            for statement in parent.named_children(&mut cursor) {
                if !matches!(
                    statement.kind(),
                    "lexical_declaration" | "variable_declaration"
                ) {
                    continue;
                }
                let mut inner = statement.walk();
                for declarator in statement.named_children(&mut inner) {
                    let declares = declarator.kind() == "variable_declarator"
                        && declarator
                            .child_by_field_name("name")
                            .is_some_and(|n| n.kind() == "identifier" && cx.text(&n) == spelled);
                    if !declares {
                        continue;
                    }
                    return statement.kind() == "lexical_declaration"
                        && statement.end_byte() <= name.start_byte()
                        && statement.child(0).is_some_and(|k| k.kind() == "const")
                        && declarator
                            .child_by_field_name("value")
                            .is_some_and(|v| v.kind() == "array" && v.named_child_count() == 0);
                }
            }
        }
        up = parent;
    }
    false
}

/// Cached unchanged source still belongs to the revision being ingested now.
pub(super) fn retarget_commit(props: &mut serde_json::Value, commit: &str) {
    let Some(payload) = props.get("rule").cloned() else {
        return;
    };
    let Ok(mut rule) = GuardedExitEvidence::from_value(payload) else {
        return;
    };
    rule.visit_sources_mut(|source| source.commit_sha = commit.into());
    props["rule"] = serde_json::to_value(rule).expect("rule serializes");
    props["prov"]["content_hash"] = serde_json::json!(core_prov::content_hash(
        &serde_json::to_vec(&props["rule"]).expect("rule serializes")
    ));
}

#[cfg(test)]
mod tests;
