//! Guarded local exits and refusal responses in Go functions as cited
//! source observations, never business verdicts (SPEC-02, #2009).
//!
//! A `return` under same-function `if` / `else` branches becomes a
//! `BusinessRule` carrying [`GuardedExitEvidence`], owned by the enclosing
//! function or method Symbol through `GOVERNS`. The dominant `net/http`
//! refusal shape, a call statement that writes an error status
//! (`http.Error(w, "forbidden", http.StatusForbidden)`) under branches and
//! followed by a bare `return`, is a guarded effect of kind `Call` instead,
//! so the refusal it writes is what is stored. The branch conditions and
//! values are sanitized displays; the execution predicate and the consumer
//! effect stay unestablished, and every reason they do is a Gap the rule
//! `DEPENDS_ON`. Caps, omission Gaps and the Gap vocabulary follow the
//! Python and TypeScript producers. A `func` literal that is a Symbol (a
//! route handler) owns its own rules; returns inside any other literal are
//! the literal's own and are not read.

use super::{Extraction, FileCx, file_id};
use core_graph::rules::*;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use core_redact::{SecretKind, is_sensitive_name, recognized_secrets, rule_literal_token_shaped};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

const MAX_CONDITIONS: usize = 32;
const MAX_ANCESTOR_STEPS: usize = 256;
const MAX_EXPRESSION_BYTES: usize = 8 * 1024;
const MAX_EXPRESSION_NODES: usize = 1024;
const MAX_CONTEXT_ANCESTORS: usize = 128;
const MAX_RULE_BYTES: usize = 64 * 1024;
const MAX_FILE_RULE_BYTES: usize = 1024 * 1024;
const MAX_RULES_PER_FILE: usize = 256;
const WITHHELD: &str = "[REDACTED]";
const UNSUPPORTED: &str = "[UNSUPPORTED EXPRESSION]";

fn source(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
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
            "The caller's interpretation of this local return or response is unknown."
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
        "switch_control" => {
            "Switch or select control flow in the callable has not been interpreted."
        }
        "exception_control" => {
            "Deferred, panic or goroutine behavior in the callable has not been interpreted."
        }
        "redacted_expression" => "An expression was withheld or incompletely captured.",
        "analysis_limit" => {
            "A source-rule capture limit was reached; additional analysis remains incomplete."
        }
        "unsupported_rule_syntax" => {
            "Incomplete source syntax prevented supported rule analysis; remaining file analysis was omitted."
        }
        _ => "Source-rule interpretation remains incomplete.",
    }
}

/// The Symbol owning `node`: its function or method declaration, or the
/// `func` literal it is in when that literal is a Symbol (a route handler);
/// `None` inside any other literal, whose returns are the literal's own.
fn owner<'f>(mut node: TsNode<'_>, functions: &'f HashMap<usize, String>) -> Option<&'f String> {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "function_declaration" | "method_declaration" | "func_literal" => {
                return functions.get(&parent.start_byte());
            }
            _ => node = parent,
        }
    }
    None
}

type Branch<'t> = (TsNode<'t>, TsNode<'t>, BranchPolarity);

/// The `if` / `else` branches around `node` up to its function, outermost
/// first: the consequence's condition truthy, an `else` (or `else if`)
/// the earlier condition falsy.
fn branches(mut node: TsNode<'_>) -> (Vec<Branch<'_>>, bool) {
    let mut conditions: Vec<Branch<'_>> = Vec::new();
    let mut steps = 0;
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) {
            break;
        }
        steps += 1;
        if steps > MAX_ANCESTOR_STEPS || conditions.len() > MAX_CONDITIONS {
            return (conditions, true);
        }
        if parent.kind() == "if_statement"
            && let Some(condition) = parent.child_by_field_name("condition")
        {
            if parent.child_by_field_name("consequence") == Some(node) {
                conditions.push((parent, condition, BranchPolarity::TruthyBranch));
            } else if parent.child_by_field_name("alternative") == Some(node) {
                conditions.push((parent, condition, BranchPolarity::FalsyBranch));
            }
        }
        node = parent;
    }
    conditions.reverse();
    let limited = conditions.len() > MAX_CONDITIONS;
    conditions.truncate(MAX_CONDITIONS);
    (conditions, limited)
}

fn control_reason(node: TsNode<'_>) -> Option<RuleGapReason> {
    match node.kind() {
        "for_statement" => Some(RuleGapReason::LoopDependency),
        "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
            Some(RuleGapReason::SwitchControl)
        }
        "defer_statement" | "go_statement" => Some(RuleGapReason::ExceptionControl),
        "inc_dec_statement" => Some(RuleGapReason::Mutation),
        // Rebinding a plain name is not a mutation; writing through a field
        // or an index is.
        "assignment_statement" => {
            let left = node.child_by_field_name("left")?;
            let mut cursor = left.walk();
            left.named_children(&mut cursor)
                .any(|target| {
                    matches!(
                        target.kind(),
                        "selector_expression" | "index_expression" | "unary_expression"
                    )
                })
                .then_some(RuleGapReason::Mutation)
        }
        _ => None,
    }
}

/// Whether `node` is an argument writing an error status: a `net/http`
/// constant for 4xx or 5xx, or such an integer literal.
fn error_status(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    match node.kind() {
        "selector_expression" => {
            let (Some(operand), Some(field)) = (
                node.child_by_field_name("operand"),
                node.child_by_field_name("field"),
            ) else {
                return false;
            };
            cx.text(&operand) == "http"
                && go_http_status(cx.text(&field)).is_some_and(|code| code >= 400)
        }
        "int_literal" => cx
            .text(&node)
            .parse::<u16>()
            .is_ok_and(|code| (400..600).contains(&code)),
        _ => false,
    }
}

/// The call of an expression statement that writes an error response and
/// is followed by a bare `return` in its block: `http.Error(w, "…",
/// http.StatusForbidden); return`.
fn refusal_call<'t>(cx: &FileCx<'_>, statement: TsNode<'t>) -> Option<TsNode<'t>> {
    if statement.kind() != "expression_statement" {
        return None;
    }
    let call = statement.named_child(0)?;
    if call.kind() != "call_expression" {
        return None;
    }
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    if !arguments
        .named_children(&mut cursor)
        .any(|argument| error_status(cx, argument))
    {
        return None;
    }
    let mut next = statement.next_named_sibling();
    while let Some(sibling) = next.filter(|n| n.kind() == "comment") {
        next = sibling.next_named_sibling();
    }
    let next = next?;
    (next.kind() == "return_statement" && next.named_child_count() == 0).then_some(call)
}

/// Whether `exit` only passes an error on: its innermost condition is
/// `err != nil` and its last result is that `err`, or `fmt.Errorf` wrapping
/// it. Propagation states no rule of its own, so it is not observed.
fn propagates_error(cx: &FileCx<'_>, exit: TsNode<'_>, ancestors: &[Branch<'_>]) -> bool {
    let Some((_, condition, BranchPolarity::TruthyBranch)) = ancestors.last() else {
        return false;
    };
    let text = |node: TsNode<'_>| cx.text(&node).to_owned();
    let error = if condition.kind() == "binary_expression"
        && condition
            .child_by_field_name("operator")
            .is_some_and(|op| cx.text(&op) == "!=")
        && let (Some(left), Some(right)) = (
            condition.child_by_field_name("left"),
            condition.child_by_field_name("right"),
        )
        && right.kind() == "nil"
        && left.kind() == "identifier"
    {
        text(left)
    } else {
        return false;
    };
    let Some(list) = exit.named_child(0) else {
        return false;
    };
    let mut cursor = list.walk();
    let Some(last) = list
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .last()
    else {
        return false;
    };
    if last.kind() == "identifier" {
        return text(last) == error;
    }
    if last.kind() != "call_expression"
        || last
            .child_by_field_name("function")
            .is_none_or(|callee| cx.text(&callee) != "fmt.Errorf")
    {
        return false;
    }
    let Some(arguments) = last.child_by_field_name("arguments") else {
        return false;
    };
    let mut cursor = arguments.walk();
    let passed: Vec<_> = arguments.named_children(&mut cursor).collect();
    passed
        .first()
        .is_some_and(|format| cx.text(format).contains("%w"))
        && passed[1..].iter().any(|arg| text(*arg) == error)
}

/// The call whose error a condition tests (#2009): for `err != nil` on an
/// `if` whose initializer is `err := f(…)`, or whose nearest preceding
/// statement in its block assigns `err` from a call, the callee's last
/// two dotted names (`Store.Save`, `strconv.Atoi`; a chained call gives its
/// method alone). `None` when the condition tests no such error value or
/// its binding is not that plain.
fn failed_call(
    cx: &FileCx<'_>,
    imports: &HashMap<String, super::ImportBinding>,
    branch: TsNode<'_>,
    condition: TsNode<'_>,
) -> Option<String> {
    let tested = tested_error(cx, condition)?;
    let assigns = |statement: TsNode<'_>| -> Option<Option<String>> {
        if !matches!(
            statement.kind(),
            "short_var_declaration" | "assignment_statement"
        ) {
            return None;
        }
        let left = statement.child_by_field_name("left")?;
        let mut walk = left.walk();
        let at = left
            .named_children(&mut walk)
            .position(|name| cx.text(&name) == tested)?;
        // `v, err := f()` binds every name from one call; `a, err := f(), g()`
        // binds each name from the value at its position.
        let right = statement.child_by_field_name("right")?;
        let call = match right.named_child_count() {
            1 => right.named_child(0),
            _ => right.named_child(u32::try_from(at).ok()?),
        }
        .filter(|v| v.kind() == "call_expression");
        Some(call.and_then(|call| callee_name(cx, imports, call)))
    };
    if let Some(initializer) = branch.child_by_field_name("initializer")
        && let Some(found) = assigns(initializer)
    {
        return found;
    }
    // Only the statement just before the test: anything between could have
    // set `err` again (`if retry { err = again() }`).
    let mut previous = branch.prev_named_sibling();
    while let Some(statement) = previous {
        if statement.kind() != "comment" {
            return assigns(statement).flatten();
        }
        previous = statement.prev_named_sibling();
    }
    None
}

/// The error variable `condition` compares with `nil` (`err != nil`,
/// `nil == err`), when that is the whole condition.
fn tested_error<'s>(cx: &'s FileCx<'_>, condition: TsNode<'_>) -> Option<&'s str> {
    let mut node = condition;
    while node.kind() == "parenthesized_expression" {
        node = node.named_child(0)?;
    }
    if node.kind() != "binary_expression" {
        return None;
    }
    let operator = node.child_by_field_name("operator")?;
    if !matches!(cx.text(&operator), "!=" | "==") {
        return None;
    }
    let (left, right) = (
        node.child_by_field_name("left")?,
        node.child_by_field_name("right")?,
    );
    let name = match (left.kind(), right.kind()) {
        ("identifier", "nil") => left,
        ("nil", "identifier") => right,
        _ => return None,
    };
    let text = cx.text(&name);
    (text == "err" || text.ends_with("Err")).then_some(text)
}

/// `s.Apps.Store.Save` gives `Store.Save`, `strconv.Atoi` (an imported
/// package) `strconv.Atoi`, `s.save` (a local receiver) `save`;
/// `json.NewDecoder(r.Body).Decode` gives `Decode`; `load` gives `load`.
fn callee_name(
    cx: &FileCx<'_>,
    imports: &HashMap<String, super::ImportBinding>,
    call: TsNode<'_>,
) -> Option<String> {
    let callee = call.child_by_field_name("function")?;
    if callee.kind() == "selector_expression"
        && callee
            .child_by_field_name("operand")
            .is_some_and(|operand| operand.kind() == "call_expression")
    {
        return Some(cx.text(&callee.child_by_field_name("field")?).to_owned());
    }
    let text = cx.text(&callee);
    if !text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    // `strconv.Atoi` and `s.Apps.Store.Save` keep the package or field
    // that says what is called; a local receiver (`s.save`) does not.
    match text.rsplitn(3, '.').collect::<Vec<_>>()[..] {
        [method, owner, ..]
            if owner.starts_with(|c: char| c.is_ascii_uppercase())
                || (!text[..text.len() - method.len() - 1].contains('.')
                    && imports.contains_key(owner)) =>
        {
            Some(format!("{owner}.{method}"))
        }
        [method, ..] => Some(method.to_owned()),
        [] => None,
    }
}

/// The value a `return` returns: its one expression, or the whole list;
/// `None` for a bare one.
fn exit_value(exit: TsNode<'_>) -> Option<TsNode<'_>> {
    let list = exit.named_child(0)?;
    let mut cursor = list.walk();
    let values: Vec<_> = list
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect();
    match values[..] {
        [one] => Some(one),
        [] => None,
        _ => Some(list),
    }
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

struct RuleBuilder<'cx, 'tree> {
    cx: &'cx FileCx<'cx>,
    rule_id: String,
    gaps: BTreeMap<String, (String, TsNode<'tree>)>,
    dependencies: Vec<RuleDependency>,
}

impl<'tree> RuleBuilder<'_, 'tree> {
    fn gap(&mut self, reason: RuleGapReason, at: TsNode<'tree>) -> String {
        let key = reason_key(reason);
        self.gaps
            .entry(key.clone())
            .or_insert_with(|| (format!("gap:{}:{key}", self.rule_id), at))
            .0
            .clone()
    }

    fn depends(&mut self, role: DependencyRole, reason: RuleGapReason, at: TsNode<'tree>) {
        let gap_id = self.gap(reason, at);
        self.dependencies.push(RuleDependency {
            role,
            source: source(self.cx, at),
            resolution: DependencyResolution::Unresolved { gap_id },
        });
    }

    fn control_gap(&mut self, reason: RuleGapReason, at: TsNode<'tree>) {
        let gap_id = self.gap(reason, at);
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

    /// One captured expression; its values' declarations are not proven.
    fn captured(
        &mut self,
        node: TsNode<'tree>,
        role: DependencyRole,
        redactions: &mut Vec<SourceRedaction>,
    ) -> SourceExpression {
        let (expression, mut withheld) = capture(self.cx, node);
        if expression.capture != ExpressionCapture::CompleteSyntax {
            self.control_gap(RuleGapReason::RedactedExpression, node);
        }
        redactions.append(&mut withheld);
        if node.byte_range().len() <= MAX_EXPRESSION_BYTES {
            self.depends(role, RuleGapReason::UnresolvedBinding, node);
        } else {
            self.control_gap(RuleGapReason::AnalysisLimit, node);
        }
        expression
    }
}

enum Observed<'t> {
    /// A `return` with its value, if any.
    Exit(TsNode<'t>),
    /// A refusal response call statement before a bare `return`.
    Refusal(TsNode<'t>),
}

/// Guarded exits and refusals of the file's functions and methods.
/// `functions` maps a declaration's start byte to its Symbol id.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &HashMap<usize, String>,
    imports: &HashMap<String, super::ImportBinding>,
    out: &mut Extraction,
) {
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
        let owner = owner(at, functions)
            .cloned()
            .unwrap_or_else(|| file_id(cx.id.repo, cx.path));
        omission_gap(cx, at, &owner, RuleGapReason::UnsupportedRuleSyntax, out);
        return;
    }
    let mut observed: BTreeMap<String, Vec<Observed<'_>>> = BTreeMap::new();
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
        let Some(owner) = owner(node, functions) else {
            continue;
        };
        if node.kind() == "return_statement" {
            if node.named_child_count() > 0 {
                observed
                    .entry(owner.clone())
                    .or_default()
                    .push(Observed::Exit(node));
            }
        } else if let Some(call) = refusal_call(cx, node) {
            observed
                .entry(owner.clone())
                .or_default()
                .push(Observed::Refusal(call));
        } else if let Some(reason) = control_reason(node) {
            controls
                .entry(owner.clone())
                .or_default()
                .entry(reason_key(reason))
                .or_insert((reason, node));
        }
    }
    let mut rule_count = 0;
    let mut file_bytes = 0;
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    for (owner_id, owner_observed) in observed {
        let mut prior: Option<TsNode<'_>> = None;
        for (order, item) in owner_observed.iter().enumerate() {
            let at = match item {
                Observed::Exit(exit) => *exit,
                Observed::Refusal(call) => *call,
            };
            let (ancestors, limited) = branches(at);
            let earlier = prior.replace(at);
            if ancestors.is_empty() && !limited {
                continue;
            }
            if matches!(item, Observed::Exit(exit) if propagates_error(cx, *exit, &ancestors)) {
                continue;
            }
            if limited || rule_count == MAX_RULES_PER_FILE || file_bytes >= MAX_FILE_RULE_BYTES {
                omission_gap(cx, at, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            let (kind, label, observation) = match item {
                Observed::Exit(_) => (RuleKind::GuardedExit, "exit", "guarded_exit"),
                Observed::Refusal(_) => (RuleKind::GuardedEffect, "effect", "guarded_effect"),
            };
            let rule_id = format!(
                "rule:{}@{}#{label}@{}",
                cx.id.repo,
                cx.path,
                at.start_byte()
            );
            let mut builder = RuleBuilder {
                cx,
                rule_id: rule_id.clone(),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
            };
            builder.control_gap(RuleGapReason::ExecutionPredicateUnknown, at);
            builder.control_gap(RuleGapReason::ConsumerSemanticsUnknown, at);
            if let Some(earlier) = earlier {
                builder.control_gap(RuleGapReason::PrecedingExit, earlier);
            }
            if let Some(controls) = controls.get(&owner_id) {
                for (reason, at) in controls.values() {
                    builder.control_gap(*reason, *at);
                }
            }
            let mut redactions = Vec::new();
            let mut conditions = Vec::new();
            let failed: Vec<Option<String>> = ancestors
                .iter()
                .map(|(branch, condition, _)| failed_call(cx, imports, *branch, *condition))
                .collect();
            for (branch, condition, polarity) in ancestors {
                let expression =
                    builder.captured(condition, DependencyRole::Condition, &mut redactions);
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let effect = match item {
                Observed::Exit(exit) => LocalExit::Return {
                    value: exit_value(*exit).map(|value| {
                        builder.captured(value, DependencyRole::ExitValue, &mut redactions)
                    }),
                },
                Observed::Refusal(call) => LocalExit::Call {
                    value: builder.captured(*call, DependencyRole::ExitValue, &mut redactions),
                },
            };
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind,
                owner_id: owner_id.clone(),
                exit_source: source(cx, at),
                source_order: order as u32,
                conditions,
                effect,
                dependencies: builder.dependencies,
                local_definitions: Some(Vec::new()),
                interpretation: Interpretation {
                    execution_predicate: InterpretationStatus::NotEstablished,
                    consumer_effect: InterpretationStatus::NotEstablished,
                    gap_ids: builder.gaps.values().map(|(id, _)| id.clone()).collect(),
                },
                redactions,
            };
            if let Err(error) = evidence.validate() {
                let reason = if matches!(error, RuleValidationError::Limit(_)) {
                    RuleGapReason::AnalysisLimit
                } else {
                    RuleGapReason::UnsupportedRuleSyntax
                };
                omission_gap(cx, at, &owner_id, reason, out);
                return;
            }
            let payload = serde_json::to_value(&evidence).expect("rule serializes");
            let canonical = serde_json::to_string(&payload).expect("rule serializes");
            if canonical.len() > MAX_RULE_BYTES
                || file_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(cx, at, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            file_bytes += canonical.len();
            rule_count += 1;
            let what = match item {
                Observed::Exit(_) => "Guarded local exit",
                Observed::Refusal(_) => "Guarded refusal response",
            };
            out.nodes.push(Node {
                id: rule_id.clone(),
                label: "BusinessRule".into(),
                props: serde_json::json!({
                    "name": format!("{what} at {}:{}", cx.path, at.start_position().row + 1),
                    "observation": observation,
                    "rule": payload,
                    // The call whose error each condition tests, aligned
                    // with the conditions (#2009).
                    "failed_calls": failed,
                    "prov": cx.prov(&at, &canonical),
                }),
            });
            out.edges.push(Edge {
                src: rule_id.clone(),
                dst: owner_id.clone(),
                label: "GOVERNS".into(),
                props: serde_json::json!({
                    "observation": "lexical_owner",
                    "prov": cx.prov(&at, &format!("GOVERNS {rule_id} -> {owner_id}")),
                }),
            });
            for (reason, (gap_id, gap_at)) in builder.gaps {
                let props = serde_json::json!({
                    "rule_evidence_gap": true,
                    "reason_code": reason,
                    "reason": reason_description(&reason),
                    "prov": cx.prov_with_confidence(&gap_at, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                });
                if emitted.insert(gap_id.clone()) {
                    out.nodes.push(Node {
                        id: gap_id.clone(),
                        label: "Gap".into(),
                        props: props.clone(),
                    });
                }
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

/// Point a reused file's rule payloads at `commit` (AC-0054).
pub(super) fn retarget(props: &mut serde_json::Value, commit: &str) {
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

// ---------------------------------------------------------------------------
// Sanitized capture (SPEC-02, AC-0124): decoded string literals are inspected
// with the shared `core-redact` recognizers, comments are removed, and a
// form outside the supported set withholds the whole display.

struct Replacement {
    start: usize,
    end: usize,
    text: &'static str,
}

fn capture(cx: &FileCx<'_>, node: TsNode<'_>) -> (SourceExpression, Vec<SourceRedaction>) {
    let evidence = source(cx, node);
    let mut result = SourceExpression {
        source: evidence.clone(),
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
                source: evidence,
                reason: RedactionReason::UnsupportedSyntax,
            }],
        );
    }
    let Some(bytes) = cx.source.get(node.byte_range()) else {
        return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
    };
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return withheld(result, evidence, RedactionReason::UnsupportedDecoding);
    };
    if node.has_error() || node.is_missing() {
        return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
    }
    let mut replacements = vec![];
    let mut redactions = vec![];
    let mut stack = vec![node];
    let mut visited = 0;
    while let Some(current) = stack.pop() {
        visited += 1;
        if visited > MAX_EXPRESSION_NODES {
            return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
        }
        match current.kind() {
            "comment" => {
                replacements.push(Replacement {
                    start: current.start_byte(),
                    end: current.end_byte(),
                    text: " ",
                });
                redactions.push(SourceRedaction {
                    source: source(cx, current),
                    reason: RedactionReason::RemovedComment,
                });
                continue;
            }
            "true" | "false" | "nil" => {
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
            "int_literal" | "float_literal" => {
                if current == node
                    && let Ok(value) = decode_number(cx.text(&current))
                {
                    result.literal = Some(LiteralEvidence::Known {
                        value: KnownLiteral::Number(value),
                    });
                }
                continue;
            }
            "iota" | "imaginary_literal" | "rune_literal" => continue,
            "interpreted_string_literal" | "raw_string_literal" => {
                let Ok(context) = sensitive_context(cx, current) else {
                    return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
                };
                let decoded = match string_value(cx, current) {
                    Ok(decoded) => decoded,
                    Err(reason) => {
                        if current == node {
                            result.literal = Some(LiteralEvidence::Withheld {
                                kind: LiteralKind::String,
                                reasons: vec![reason],
                            });
                        }
                        return withheld(result, evidence, reason);
                    }
                };
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
                            source: source(cx, current),
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
                continue;
            }
            "identifier" | "field_identifier" | "package_identifier" | "type_identifier" => {
                let text = cx.text(&current);
                if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
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
                            source: source(cx, current),
                            reason: redaction_reason(reason),
                        });
                    }
                }
                continue;
            }
            kind if current.is_named() && supported_container(kind) => {}
            kind if !current.is_named() && supported_token(kind) => continue,
            _ => return withheld(result, evidence, RedactionReason::UnsupportedSyntax),
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
            return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
        };
        display.push_str(prefix);
        display.push_str(replacement.text);
        copied_to = end;
    }
    let Some(tail) = raw.get(copied_to..) else {
        return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
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
            | "binary_expression"
            | "unary_expression"
            | "selector_expression"
            | "index_expression"
            | "slice_expression"
            | "call_expression"
            | "argument_list"
            | "expression_list"
            | "type_assertion_expression"
            | "type_conversion_expression"
            | "qualified_type"
            | "pointer_type"
            | "slice_type"
            | "composite_literal"
            | "literal_value"
            | "keyed_element"
            | "literal_element"
            | "variadic_argument"
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
            | "..."
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "&"
            | "|"
            | "^"
            | "&^"
            | "<<"
            | ">>"
            | "<"
            | ">"
            | "<="
            | ">="
            | "=="
            | "!="
            | "&&"
            | "||"
            | "!"
            | "<-"
    )
}

/// The decoded value of a Go string literal.
fn string_value(cx: &FileCx<'_>, literal: TsNode<'_>) -> Result<String, RedactionReason> {
    let text = cx.text(&literal);
    if literal.kind() == "raw_string_literal" {
        return text
            .strip_prefix('`')
            .and_then(|t| t.strip_suffix('`'))
            .map(|t| t.replace('\r', ""))
            .ok_or(RedactionReason::UnsupportedSyntax);
    }
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .ok_or(RedactionReason::UnsupportedSyntax)?;
    decode_escapes(inner).map_err(|()| RedactionReason::UnsupportedDecoding)
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
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            'u' => out.push(read_hex(&mut chars, 4)?),
            'U' => out.push(read_hex(&mut chars, 8)?),
            // Byte (`\x`, octal) escapes are not decoded here.
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
    if (raw.starts_with('0') && raw.len() > 1)
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

/// Whether a string literal is the value of a sensitive name: keyed by it,
/// assigned to it, or compared with it.
fn sensitive_context(cx: &FileCx<'_>, mut child: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        let Some(parent) = child.parent() else {
            return Ok(false);
        };
        match parent.kind() {
            "function_declaration" | "method_declaration" | "func_literal" | "block" => {
                return Ok(false);
            }
            "keyed_element" => {
                let mut cursor = parent.walk();
                let parts: Vec<_> = parent.named_children(&mut cursor).collect();
                if parts.len() == 2 && parts[1] == child && sensitive_key(cx, parts[0])? {
                    return Ok(true);
                }
            }
            "binary_expression" => {
                let op = parent
                    .child_by_field_name("operator")
                    .map(|op| cx.text(&op));
                if matches!(op, Some("==" | "!=")) {
                    let mut cursor = parent.walk();
                    for operand in parent.named_children(&mut cursor) {
                        if operand != child && sensitive_key(cx, operand)? {
                            return Ok(true);
                        }
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
            "identifier" | "field_identifier" => return Ok(is_sensitive_name(cx.text(&node))),
            "interpreted_string_literal" | "raw_string_literal" => {
                return Ok(string_value(cx, node).is_ok_and(|value| is_sensitive_name(&value)));
            }
            "literal_element" => node = node.named_child(0).ok_or(())?,
            "selector_expression" => node = node.child_by_field_name("field").ok_or(())?,
            "parenthesized_expression" => node = node.named_child(0).ok_or(())?,
            _ => return Ok(false),
        }
    }
    Err(())
}

#[cfg(test)]
mod tests;
