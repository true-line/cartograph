//! Guarded local exits in Rust functions as cited source observations,
//! never business verdicts (SPEC-02, #2018).
//!
//! A `return` with a value under same-function `if` / `else` branches
//! becomes a `BusinessRule` carrying [`GuardedExitEvidence`], owned by the
//! enclosing `fn` Symbol through `GOVERNS`. The branch conditions and values
//! are sanitized displays; the execution predicate and the consumer effect
//! stay unestablished, and every reason they do is a Gap the rule
//! `DEPENDS_ON`. Caps, omission Gaps and the Gap vocabulary follow the
//! Python, TypeScript, Go and Java producers. Returns inside a closure or an
//! `async` block return from it, not from the function, and are not read;
//! test code (AC-1016) yields no rules.

use super::{Extraction, FileCx, file_id, target_tests};
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

/// Items and expressions that end the search for an exit's owner or
/// branches: a `return` in a closure or `async` block leaves only it.
const BOUNDARIES: &[&str] = &[
    "function_item",
    "closure_expression",
    "async_block",
    "const_item",
    "static_item",
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
            "The caller's interpretation of this local return is unknown."
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
        "exception_control" => "Error propagation (`?`) in the callable has not been interpreted.",
        "redacted_expression" => "An expression was withheld or incompletely captured.",
        "analysis_limit" => {
            "A source-rule capture limit was reached; additional analysis remains incomplete."
        }
        "unsupported_rule_syntax" => {
            "Incomplete source syntax prevented supported rule analysis; remaining file analysis was omitted."
        }
        other => RuleGapReason::describe_key(other),
    }
}

/// The Symbol owning `node`: its nearest enclosing `fn`; `None` inside a
/// closure, an `async` block or a `const`/`static` initializer, whose
/// returns are not the function's.
fn owner<'f>(mut node: TsNode<'_>, functions: &'f HashMap<usize, String>) -> Option<&'f String> {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "function_item" => return functions.get(&parent.start_byte()),
            kind if BOUNDARIES.contains(&kind) => return None,
            _ => node = parent,
        }
    }
    None
}

type Branch<'t> = (TsNode<'t>, TsNode<'t>, BranchPolarity);

/// The expression inside parentheses (`if (a) {}` is legal, if unusual).
fn unparenthesized(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "parenthesized_expression" {
        let mut cursor = node.walk();
        let Some(inner) = node
            .named_children(&mut cursor)
            .find(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        else {
            break;
        };
        node = inner;
    }
    node
}

/// The `if` / `else` branches around `node` up to its function, outermost
/// first: the consequence's condition truthy, an `else` (or `else if`) the
/// earlier condition falsy. An `if let` condition is captured as written.
fn branches(mut node: TsNode<'_>) -> (Vec<Branch<'_>>, bool) {
    let mut conditions: Vec<Branch<'_>> = Vec::new();
    let mut steps = 0;
    while let Some(parent) = node.parent() {
        if BOUNDARIES.contains(&parent.kind()) {
            break;
        }
        steps += 1;
        if steps > MAX_ANCESTOR_STEPS || conditions.len() > MAX_CONDITIONS {
            return (conditions, true);
        }
        if parent.kind() == "if_expression"
            && let Some(condition) = parent.child_by_field_name("condition")
        {
            let condition = unparenthesized(condition);
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

fn control_reason(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<RuleGapReason> {
    match node.kind() {
        "loop_expression" | "while_expression" | "for_expression" => {
            Some(RuleGapReason::LoopDependency)
        }
        "match_expression" => Some(RuleGapReason::SwitchControl),
        // `?` returns early with the error it propagates.
        "try_expression" => Some(RuleGapReason::ExceptionControl),
        "compound_assignment_expr" => Some(RuleGapReason::Mutation),
        // Rebinding a local name is not a mutation; writing through a field,
        // an index or a dereference is.
        "assignment_expression" => {
            let left = node.child_by_field_name("left")?;
            let through = match left.kind() {
                "field_expression" | "index_expression" => true,
                "unary_expression" => cx.text(&left).starts_with('*'),
                _ => false,
            };
            through.then_some(RuleGapReason::Mutation)
        }
        _ => None,
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

/// The expression a `return` carries.
fn exit_value(exit: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut cursor = exit.walk();
    exit.named_children(&mut cursor)
        .find(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
}

/// Guarded returns of the file's functions. `functions` maps a
/// `function_item`'s start byte to its Symbol id.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &HashMap<usize, String>,
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
        let Some(owner) = owner(node, functions) else {
            continue;
        };
        match node.kind() {
            // A bare `return` is no rule but still precedes later exits.
            // Test code states no product rule (AC-1016).
            "return_expression" => {
                if !target_tests::in_test_code(cx, node) {
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
            let (ancestors, limited) = branches(exit);
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
            let effect = LocalExit::Return { value: Some(value) };
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
            "boolean_literal" => {
                if current == node {
                    result.literal = Some(LiteralEvidence::Known {
                        value: KnownLiteral::Boolean(cx.text(&current) == "true"),
                    });
                }
                continue;
            }
            "integer_literal" | "float_literal" => {
                if current == node
                    && let Ok(value) = decode_number(cx.text(&current))
                {
                    result.literal = Some(LiteralEvidence::Known {
                        value: KnownLiteral::Number(value),
                    });
                }
                continue;
            }
            "char_literal" | "self" | "crate" | "super" | "primitive_type"
            | "mutable_specifier" => continue,
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
            "identifier"
            | "type_identifier"
            | "field_identifier"
            | "shorthand_field_identifier" => {
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
            | "reference_expression"
            | "field_expression"
            | "index_expression"
            | "call_expression"
            | "arguments"
            | "generic_function"
            | "scoped_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "reference_type"
            | "type_arguments"
            | "type_cast_expression"
            | "try_expression"
            | "await_expression"
            | "struct_expression"
            | "field_initializer_list"
            | "field_initializer"
            | "shorthand_field_initializer"
            | "base_field_initializer"
            | "tuple_expression"
            | "array_expression"
            | "unit_expression"
            | "range_expression"
            | "macro_invocation"
            | "token_tree"
            | "let_condition"
            | "let_chain"
            | "tuple_struct_pattern"
            | "tuple_pattern"
            | "struct_pattern"
            | "field_pattern"
            | "ref_pattern"
            | "or_pattern"
            | "slice_pattern"
            | "mut_pattern"
            | "captured_pattern"
            | "remaining_field_pattern"
            | "closure_expression"
            | "closure_parameters"
            | "parameter"
            | "block"
            | "expression_statement"
            | "let_declaration"
            | "if_expression"
            | "else_clause"
            | "match_expression"
            | "match_block"
            | "match_arm"
            | "match_pattern"
            | "return_expression"
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
            | "{"
            | "}"
            | "."
            | ","
            | ":"
            | "::"
            | "?"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
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
            | "&&"
            | "||"
            | "!"
            | "="
            | ".."
            | "..="
            | "_"
            | "let"
            | "as"
            | "ref"
            | "mut"
            | "await"
            | "=>"
            | "@"
            | "->"
            | ";"
            | "move"
            | "if"
            | "else"
            | "match"
            | "return"
            | "\""
    )
}

/// The decoded value of a Rust string or byte string literal; a raw string
/// is not read.
fn string_value(cx: &FileCx<'_>, literal: TsNode<'_>) -> Result<String, RedactionReason> {
    let text = cx.text(&literal);
    let text = text.strip_prefix('b').unwrap_or(text);
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
            '0' => out.push('\0'),
            'x' => {
                let hi = chars.next().and_then(|c| c.to_digit(8)).ok_or(())?;
                let lo = chars.next().and_then(|c| c.to_digit(16)).ok_or(())?;
                out.push(char::from_u32(hi * 16 + lo).ok_or(())?);
            }
            'u' => {
                if chars.next() != Some('{') {
                    return Err(());
                }
                let mut value = 0u32;
                let mut digits = 0;
                loop {
                    match chars.next().ok_or(())? {
                        '}' if digits > 0 => break,
                        '_' => {}
                        c => {
                            value = value * 16 + c.to_digit(16).ok_or(())?;
                            digits += 1;
                            if digits > 6 {
                                return Err(());
                            }
                        }
                    }
                }
                out.push(char::from_u32(value).ok_or(())?);
            }
            // A line continuation is not decoded here.
            _ => return Err(()),
        }
    }
    Ok(out)
}

/// A plain decimal literal without a type suffix or separators.
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

/// Whether a string literal is the value of a sensitive name: compared with
/// it (`==`, `!=`, `.eq(…)` either way round) or passed beside it
/// (`insert("password", …)`).
fn sensitive_context(cx: &FileCx<'_>, mut child: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        let Some(parent) = child.parent() else {
            return Ok(false);
        };
        match parent.kind() {
            kind if BOUNDARIES.contains(&kind) || kind == "block" => return Ok(false),
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
            // `"…".eq(secret)`: the literal is the receiver.
            "field_expression" if parent.child_by_field_name("value") == Some(child) => {
                if let Some(call) = parent.parent()
                    && call.kind() == "call_expression"
                    && call.child_by_field_name("function") == Some(parent)
                    && let Some(arguments) = call.child_by_field_name("arguments")
                {
                    let mut cursor = arguments.walk();
                    for argument in arguments.named_children(&mut cursor) {
                        if sensitive_key(cx, argument)? {
                            return Ok(true);
                        }
                    }
                }
            }
            // `secret.eq("…")`, `insert("token", "…")`, `ct_eq(secret, "…")`.
            "arguments" => {
                let mut cursor = parent.walk();
                for argument in parent.named_children(&mut cursor) {
                    if argument != child && sensitive_key(cx, argument)? {
                        return Ok(true);
                    }
                }
                if let Some(call) = parent.parent()
                    && call.kind() == "call_expression"
                    && let Some(function) = call.child_by_field_name("function")
                    && function.kind() == "field_expression"
                    && let Some(value) = function.child_by_field_name("value")
                    && sensitive_key(cx, value)?
                {
                    return Ok(true);
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
            "string_literal" => {
                return Ok(string_value(cx, node).is_ok_and(|value| is_sensitive_name(&value)));
            }
            "field_expression" => node = node.child_by_field_name("field").ok_or(())?,
            "reference_expression" | "unary_expression" => {
                node = node
                    .child_by_field_name("value")
                    .or_else(|| node.named_child(0))
                    .ok_or(())?
            }
            // A getter names its value: `password()`, `get_password()`.
            "call_expression" => {
                let function = node.child_by_field_name("function").ok_or(())?;
                if function.kind() != "field_expression" {
                    return Ok(false);
                }
                let name = function.child_by_field_name("field").ok_or(())?;
                let text = cx.text(&name);
                return Ok(is_sensitive_name(text.strip_prefix("get_").unwrap_or(text)));
            }
            "parenthesized_expression" => node = node.named_child(0).ok_or(())?,
            _ => return Ok(false),
        }
    }
    Err(())
}

#[cfg(test)]
mod tests;
