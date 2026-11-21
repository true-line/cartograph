//! Guarded local exits in Kotlin functions as cited source observations,
//! never business verdicts (SPEC-02, #2021, AC-1106).
//!
//! A `return` with a value or a `throw` under same-function `if` / `else`
//! branches becomes a `BusinessRule` carrying [`GuardedExitEvidence`], owned
//! by the enclosing function Symbol through `GOVERNS`. The branch conditions
//! and values are sanitized displays; the execution predicate and the
//! consumer effect stay unestablished, and every reason they do is a Gap the
//! rule `DEPENDS_ON`. Caps, omission Gaps and the Gap vocabulary follow the
//! Java producer. The trailing lambda of an inline scope function (`let`,
//! `run`, `also`, `apply`, `with`, `use`) runs in the function, so its exits
//! are the function's. Exits inside another lambda, an anonymous function,
//! an object expression, a local function, an `init` block or a property
//! accessor belong to no member Symbol and are not read; a test source set
//! yields no rules.

use super::{Extraction, FileCx, file_id};
use core_graph::rules::*;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use core_redact::{SecretKind, is_sensitive_name, is_token_shaped, recognized_secrets};
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

/// Declarations that end the search for an exit's owner or branches.
const BOUNDARIES: &[&str] = &[
    "function_declaration",
    "lambda_literal",
    "anonymous_function",
    "class_body",
    "enum_class_body",
    "anonymous_initializer",
    "secondary_constructor",
    "getter",
    "setter",
];

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
            "The caller's interpretation of this local return or throw is unknown."
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
        "switch_control" => "`when` control flow in the callable has not been interpreted.",
        "exception_control" => {
            "Exception or finally behavior in the callable has not been interpreted."
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

fn is_comment(node: TsNode<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

/// The Symbol owning `node`: its function declaration when that is a member
/// of a named type (or top level); `None` inside a lambda, an accessor, an
/// initializer or a class body, whose exits are not the function's, and for
/// a local function or a member of an object expression.
fn owner<'f>(
    source: &[u8],
    mut node: TsNode<'_>,
    functions: &'f HashMap<usize, String>,
) -> Option<&'f String> {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "function_declaration" => {
                return member_of_named_type(parent)
                    .then(|| functions.get(&parent.start_byte()))
                    .flatten();
            }
            "lambda_literal" if scope_lambda(source, parent) => node = parent,
            kind if BOUNDARIES.contains(&kind) => return None,
            _ => node = parent,
        }
    }
    None
}

/// Whether a declaration sits at top level or in named types nested only in
/// other named types up to the file: not in an object expression, a local
/// class or another function.
fn member_of_named_type(declaration: TsNode<'_>) -> bool {
    let mut node = declaration;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "source_file" => return true,
            "class_body" | "enum_class_body" | "class_declaration" | "object_declaration"
            | "companion_object" => node = parent,
            _ => return false,
        }
    }
    false
}

/// Standard-library scope functions: inline, so the body of their
/// trailing lambda runs in the calling function and its `return` and
/// `throw` leave that function.
const SCOPE_FUNCTIONS: &[&str] = &["let", "run", "also", "apply", "with", "use"];

/// Whether `lambda` is the trailing lambda of a scope-function call
/// (`x?.let { … }`, `run { … }`), which belongs to the enclosing function.
fn scope_lambda(source: &[u8], lambda: TsNode<'_>) -> bool {
    if lambda.kind() != "lambda_literal" {
        return false;
    }
    let Some(call) = lambda
        .parent()
        .filter(|annotated| annotated.kind() == "annotated_lambda")
        .and_then(|annotated| annotated.parent())
        .filter(|call| call.kind() == "call_expression")
    else {
        return false;
    };
    let Some(head) = call.named_child(0) else {
        return false;
    };
    let name = match head.kind() {
        "identifier" => Some(head),
        "navigation_expression" => head.named_child(1).filter(|n| n.kind() == "identifier"),
        // `with(x) { … }`
        "call_expression" => head.named_child(0).filter(|n| n.kind() == "identifier"),
        _ => None,
    };
    name.is_some_and(|name| {
        name.utf8_text(source)
            .is_ok_and(|text| SCOPE_FUNCTIONS.contains(&text))
    })
}

type Branch<'t> = (TsNode<'t>, TsNode<'t>, BranchPolarity);

/// The expression inside an `if`'s parentheses.
fn unparenthesized(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "parenthesized_expression" {
        let mut cursor = node.walk();
        let Some(inner) = node.named_children(&mut cursor).find(|n| !is_comment(*n)) else {
            break;
        };
        node = inner;
    }
    node
}

/// Which branch of an `if` expression `child` is: the consequence (before
/// `else`) or the alternative (after it); `None` for the condition.
fn if_branch(
    statement: TsNode<'_>,
    condition: TsNode<'_>,
    child: TsNode<'_>,
) -> Option<BranchPolarity> {
    if child == condition || child.start_byte() < condition.end_byte() {
        return None;
    }
    let mut cursor = statement.walk();
    let otherwise = statement
        .children(&mut cursor)
        .find(|c| !c.is_named() && c.kind() == "else");
    Some(match otherwise {
        Some(otherwise) if child.start_byte() >= otherwise.end_byte() => {
            BranchPolarity::FalsyBranch
        }
        _ => BranchPolarity::TruthyBranch,
    })
}

/// The `if` / `else` branches around `node` up to its function, outermost
/// first: the consequence's condition truthy, an `else` (or `else if`) the
/// earlier condition falsy.
fn branches<'t>(source: &[u8], mut node: TsNode<'t>) -> (Vec<Branch<'t>>, bool) {
    let mut conditions: Vec<Branch<'_>> = Vec::new();
    let mut steps = 0;
    while let Some(parent) = node.parent() {
        if BOUNDARIES.contains(&parent.kind()) && !scope_lambda(source, parent) {
            break;
        }
        steps += 1;
        if steps > MAX_ANCESTOR_STEPS || conditions.len() > MAX_CONDITIONS {
            return (conditions, true);
        }
        if parent.kind() == "if_expression"
            && let Some(condition) = parent.child_by_field_name("condition")
            && let Some(polarity) = if_branch(parent, condition, node)
        {
            conditions.push((parent, unparenthesized(condition), polarity));
        }
        node = parent;
    }
    conditions.reverse();
    let limited = conditions.len() > MAX_CONDITIONS;
    conditions.truncate(MAX_CONDITIONS);
    (conditions, limited)
}

fn control_reason(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<RuleGapReason> {
    match node.kind() {
        "for_statement" | "while_statement" | "do_while_statement" => {
            Some(RuleGapReason::LoopDependency)
        }
        "when_expression" => Some(RuleGapReason::SwitchControl),
        "try_expression" => Some(RuleGapReason::ExceptionControl),
        "unary_expression" => {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .any(|c| !c.is_named() && matches!(c.kind(), "++" | "--"))
                .then_some(RuleGapReason::Mutation)
        }
        // Rebinding a local name is not a mutation; writing through a
        // property or an index, compounding a value, or assigning a bare
        // name the function does not declare (an implicit-receiver
        // property), is.
        "assignment" => {
            let left = node.child_by_field_name("left")?;
            let mut cursor = node.walk();
            let compound = node
                .children(&mut cursor)
                .any(|c| !c.is_named() && c.kind() != "=" && c.kind().ends_with('='));
            let field = match left.kind() {
                "navigation_expression" | "index_expression" => true,
                "identifier" => !declares_local(cx, node, cx.text(&left)),
                _ => true,
            };
            (compound || field).then_some(RuleGapReason::Mutation)
        }
        _ => None,
    }
}

/// Whether `name` is a parameter or local in scope at `node` (a variable
/// declared earlier in an enclosing block, or a catch, `for` or function
/// binding). Without one, a bare assignment binds a property, so it counts
/// as a mutation.
fn declares_local(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    let named = |n: TsNode<'_>| cx.text(&n) == name;
    let binds = |declaration: TsNode<'_>| {
        let mut walk = declaration.walk();
        declaration
            .named_children(&mut walk)
            .any(|part| match part.kind() {
                "variable_declaration" => {
                    let mut walk = part.walk();
                    part.named_children(&mut walk)
                        .any(|n| n.kind() == "identifier" && named(n))
                }
                "multi_variable_declaration" => {
                    let mut walk = part.walk();
                    part.named_children(&mut walk).any(|v| {
                        let mut walk = v.walk();
                        v.named_children(&mut walk)
                            .any(|n| n.kind() == "identifier" && named(n))
                    })
                }
                _ => false,
            })
    };
    let mut child = node;
    while let Some(scope) = child.parent() {
        let found = match scope.kind() {
            // Locals declared before the statement on the path.
            "block" | "statements" | "function_body" => {
                let mut walk = scope.walk();
                scope
                    .named_children(&mut walk)
                    .take_while(|s| s.start_byte() < child.start_byte())
                    .any(|s| s.kind() == "property_declaration" && binds(s))
            }
            "for_statement" => binds(scope),
            "lambda_literal" => {
                let mut walk = scope.walk();
                scope
                    .named_children(&mut walk)
                    .filter(|c| c.kind() == "lambda_parameters")
                    .any(binds)
            }
            "catch_block" => {
                let mut walk = scope.walk();
                scope
                    .named_children(&mut walk)
                    .find(|c| c.kind() == "identifier")
                    .is_some_and(named)
            }
            "function_declaration" => {
                let mut walk = scope.walk();
                scope
                    .named_children(&mut walk)
                    .filter(|c| c.kind() == "function_value_parameters")
                    .any(|params| {
                        let mut walk = params.walk();
                        params
                            .named_children(&mut walk)
                            .filter(|p| p.kind() == "parameter")
                            .any(|p| {
                                let mut walk = p.walk();
                                p.named_children(&mut walk)
                                    .find(|n| n.kind() == "identifier")
                                    .is_some_and(named)
                            })
                    })
            }
            _ => false,
        };
        if found {
            return true;
        }
        // A function or class body ends the scopes this one can see.
        if BOUNDARIES.contains(&scope.kind()) && !scope_lambda(cx.source, scope) {
            return false;
        }
        child = scope;
    }
    false
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

/// The expression a `return` or `throw` carries; a labelled `return@x` is
/// no exit of the function.
fn exit_value(exit: TsNode<'_>) -> Option<TsNode<'_>> {
    if exit.child_by_field_name("label").is_some() {
        return None;
    }
    let mut cursor = exit.walk();
    exit.named_children(&mut cursor).find(|n| !is_comment(*n))
}

/// Guarded returns and throws of the file's functions. `functions` maps a
/// declaration's start byte to its Symbol id.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &HashMap<usize, String>,
    out: &mut Extraction,
) {
    // Test code states no product rule.
    if adapters_fw::in_jvm_test_source_set(cx.path) {
        return;
    }
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
        let owner = owner(cx.source, at, functions)
            .cloned()
            .unwrap_or_else(|| file_id(cx.id.repo, cx.path));
        omission_gap(cx, at, &owner, RuleGapReason::UnsupportedRuleSyntax, out);
        return;
    }
    let mut observed: BTreeMap<String, Vec<TsNode<'_>>> = BTreeMap::new();
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
        let Some(owner) = owner(cx.source, node, functions) else {
            continue;
        };
        match node.kind() {
            // A bare `return` is no rule but still precedes later exits.
            "return_expression" | "throw_expression" => {
                if node.child_by_field_name("label").is_none() {
                    observed.entry(owner.clone()).or_default().push(node);
                }
            }
            _ => {
                if let Some(reason) = control_reason(cx, node) {
                    controls
                        .entry(owner.clone())
                        .or_default()
                        .entry(reason_key(reason))
                        .or_insert((reason, node));
                }
            }
        }
    }
    let mut rule_count = 0;
    let mut file_bytes = 0;
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    for (owner_id, exits) in observed {
        let mut prior: Option<TsNode<'_>> = None;
        for (order, exit) in exits.iter().copied().enumerate() {
            let (ancestors, limited) = branches(cx.source, exit);
            let earlier = prior.replace(exit);
            if exit_value(exit).is_none() || (ancestors.is_empty() && !limited) {
                continue;
            }
            if limited || rule_count == MAX_RULES_PER_FILE || file_bytes >= MAX_FILE_RULE_BYTES {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            let rule_id = format!("rule:{}@{}#exit@{}", cx.id.repo, cx.path, exit.start_byte());
            let mut builder = RuleBuilder {
                cx,
                rule_id: rule_id.clone(),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
            };
            builder.control_gap(RuleGapReason::ExecutionPredicateUnknown, exit);
            builder.control_gap(RuleGapReason::ConsumerSemanticsUnknown, exit);
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
            for (branch, condition, polarity) in ancestors {
                let expression =
                    builder.captured(condition, DependencyRole::Condition, &mut redactions);
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let value = exit_value(exit).expect("observed exits carry a value");
            let value = builder.captured(value, DependencyRole::ExitValue, &mut redactions);
            let effect = if exit.kind() == "throw_expression" {
                LocalExit::Throw { value }
            } else {
                LocalExit::Return { value: Some(value) }
            };
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind: RuleKind::GuardedExit,
                owner_id: owner_id.clone(),
                exit_source: source(cx, exit),
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
                omission_gap(cx, exit, &owner_id, reason, out);
                return;
            }
            let payload = serde_json::to_value(&evidence).expect("rule serializes");
            let canonical = serde_json::to_string(&payload).expect("rule serializes");
            if canonical.len() > MAX_RULE_BYTES
                || file_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            file_bytes += canonical.len();
            rule_count += 1;
            out.nodes.push(Node {
                id: rule_id.clone(),
                label: "BusinessRule".into(),
                props: serde_json::json!({
                    "name": format!("Guarded local exit at {}:{}", cx.path, exit.start_position().row + 1),
                    "observation": "guarded_exit",
                    "rule": payload,
                    "prov": cx.prov(&exit, &canonical),
                }),
            });
            out.edges.push(Edge {
                src: rule_id.clone(),
                dst: owner_id.clone(),
                label: "GOVERNS".into(),
                props: serde_json::json!({
                    "observation": "lexical_owner",
                    "prov": cx.prov(&exit, &format!("GOVERNS {rule_id} -> {owner_id}")),
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
// form outside the supported set (a string template, a lambda) withholds the
// whole display.

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
            "line_comment" | "block_comment" => {
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
            "number_literal" | "float_literal" => {
                if current == node
                    && let Ok(value) = decode_number(cx.text(&current))
                {
                    result.literal = Some(LiteralEvidence::Known {
                        value: KnownLiteral::Number(value),
                    });
                }
                continue;
            }
            "character_literal" | "this_expression" | "super_expression" => continue,
            "string_literal" => {
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
            "identifier" => {
                let text = cx.text(&current);
                if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return withheld(result, evidence, RedactionReason::UnsupportedSyntax);
                }
                // The grammar reads `true`, `false` and `null` as names.
                if current == node {
                    result.literal = match text {
                        "true" => Some(KnownLiteral::Boolean(true)),
                        "false" => Some(KnownLiteral::Boolean(false)),
                        "null" => Some(KnownLiteral::Null),
                        _ => None,
                    }
                    .map(|value| LiteralEvidence::Known { value });
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
    if is_token_shaped(value) {
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
            | "navigation_expression"
            | "index_expression"
            | "call_expression"
            | "value_arguments"
            | "value_argument"
            | "as_expression"
            | "is_expression"
            | "in_expression"
            | "range_expression"
            | "callable_reference"
            | "user_type"
            | "nullable_type"
            | "type_arguments"
            | "type_projection"
            | "string_content"
            | "escape_sequence"
    )
}

fn supported_token(kind: &str) -> bool {
    matches!(
        kind,
        "(" | ")"
            | "["
            | "]"
            | "."
            | "?."
            | "::"
            | ","
            | ":"
            | "?"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "<"
            | ">"
            | "<="
            | ">="
            | "=="
            | "!="
            | "==="
            | "!=="
            | "&&"
            | "||"
            | "!"
            | "!!"
            | "?:"
            | "is"
            | "!is"
            | "in"
            | "!in"
            | "as"
            | "as?"
            | ".."
            | "..<"
            | "="
            | "\""
    )
}

/// The decoded value of a Kotlin string literal; a raw string or a
/// template is not read.
fn string_value(cx: &FileCx<'_>, literal: TsNode<'_>) -> Result<String, RedactionReason> {
    let mut cursor = literal.walk();
    if literal
        .named_children(&mut cursor)
        .any(|part| part.kind() == "interpolation")
    {
        return Err(RedactionReason::UnsupportedSyntax);
    }
    let text = cx.text(&literal);
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .ok_or(RedactionReason::UnsupportedSyntax)?;
    decode_escapes(inner)
}

fn decode_escapes(text: &str) -> Result<String, RedactionReason> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A `$name` template.
            '$' if chars
                .peek()
                .is_some_and(|n| n.is_alphabetic() || *n == '_' || *n == '{' || *n == '`') =>
            {
                return Err(RedactionReason::UnsupportedSyntax);
            }
            '\\' => {}
            c => {
                out.push(c);
                continue;
            }
        }
        let decoding = RedactionReason::UnsupportedDecoding;
        match chars.next().ok_or(decoding)? {
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '$' => out.push('$'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'u' => {
                let mut value = 0u32;
                for _ in 0..4 {
                    value =
                        value * 16 + chars.next().and_then(|c| c.to_digit(16)).ok_or(decoding)?;
                }
                out.push(char::from_u32(value).ok_or(decoding)?);
            }
            _ => return Err(decoding),
        }
    }
    Ok(out)
}

fn decode_number(raw: &str) -> Result<serde_json::Number, ()> {
    if (raw.starts_with('0') && raw.len() > 1 && !raw.starts_with("0."))
        || raw.contains('_')
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

/// A call's callee name and receiver: `f(…)` or `x.f(…)`.
fn callee<'t>(cx: &FileCx<'_>, call: TsNode<'t>) -> Option<(Option<TsNode<'t>>, String)> {
    let head = call.named_child(0)?;
    match head.kind() {
        "identifier" => Some((None, cx.text(&head).to_string())),
        "navigation_expression" => {
            let mut cursor = head.walk();
            let parts: Vec<_> = head.named_children(&mut cursor).collect();
            let [receiver, name] = parts.as_slice() else {
                return None;
            };
            (name.kind() == "identifier").then(|| (Some(*receiver), cx.text(name).to_string()))
        }
        _ => None,
    }
}

/// Whether a string literal is the value of a sensitive name: compared with
/// it (`==`, `!=`, `.equals`) or passed beside it (`put("password", …)`).
fn sensitive_context(cx: &FileCx<'_>, mut child: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        let Some(parent) = child.parent() else {
            return Ok(false);
        };
        match parent.kind() {
            kind if BOUNDARIES.contains(&kind) || kind == "block" => return Ok(false),
            "binary_expression" => {
                let mut cursor = parent.walk();
                let equality = parent
                    .children(&mut cursor)
                    .any(|c| !c.is_named() && matches!(c.kind(), "==" | "!=" | "===" | "!=="));
                if equality {
                    let mut cursor = parent.walk();
                    for operand in parent.named_children(&mut cursor) {
                        if operand != child && sensitive_key(cx, operand)? {
                            return Ok(true);
                        }
                    }
                }
            }
            // `secret.equals("…")`, `"…".equals(secret)`, `put("token", "…")`.
            "value_argument" => {}
            "value_arguments" => {
                let mut cursor = parent.walk();
                for argument in parent.named_children(&mut cursor) {
                    let mut walk = argument.walk();
                    let value = argument.named_children(&mut walk).last();
                    if argument != child
                        && let Some(value) = value
                        && sensitive_key(cx, value)?
                    {
                        return Ok(true);
                    }
                }
                if let Some(call) = parent.parent()
                    && call.kind() == "call_expression"
                    && let Some((Some(receiver), name)) = callee(cx, call)
                    && name.starts_with("equals")
                    && sensitive_key(cx, receiver)?
                {
                    return Ok(true);
                }
            }
            "navigation_expression" => {
                // `"…".equals(secret)`
                if parent.named_child(0) == Some(child)
                    && let Some(call) = parent.parent()
                    && call.kind() == "call_expression"
                    && let Some((_, name)) = callee(cx, call)
                    && name.starts_with("equals")
                {
                    let mut cursor = call.walk();
                    for arguments in call
                        .named_children(&mut cursor)
                        .filter(|c| c.kind() == "value_arguments")
                    {
                        let mut walk = arguments.walk();
                        for argument in arguments.named_children(&mut walk) {
                            let mut walk = argument.walk();
                            if let Some(value) = argument.named_children(&mut walk).last()
                                && sensitive_key(cx, value)?
                            {
                                return Ok(true);
                            }
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
            "identifier" => return Ok(is_sensitive_name(cx.text(&node))),
            "string_literal" => {
                return Ok(string_value(cx, node).is_ok_and(|value| is_sensitive_name(&value)));
            }
            "navigation_expression" => node = node.named_child(1).ok_or(())?,
            "call_expression" => {
                // A getter names its value: `getPassword()`.
                let (_, name) = callee(cx, node).ok_or(())?;
                let property = name.strip_prefix("get").unwrap_or(&name);
                return Ok(is_sensitive_name(property)
                    || is_sensitive_name(&property.to_ascii_lowercase()));
            }
            "parenthesized_expression" => node = node.named_child(0).ok_or(())?,
            _ => return Ok(false),
        }
    }
    Err(())
}

#[cfg(test)]
mod tests;
