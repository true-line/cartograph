//! What a condition's local name was assigned (AC-1034, AC-1040): a name
//! whose value at the condition is set by a plain `name = value` statement
//! that runs before it in a block enclosing it, with no other binding of
//! the name able to run in between, is a Binding, and its initializer is
//! recorded as written. Nothing here evaluates or
//! substitutes a value.

use super::{FileCx, RuleBuilder, Scope, capture, source};
use core_graph::rules::*;
use std::collections::BTreeMap;
use tree_sitter::Node as TsNode;

/// The `name = value` assignment whose value `usage` reads, or `None` when
/// the name is a parameter or declared `global`/`nonlocal`, or no plain
/// assignment runs before `usage` in a block enclosing it, or another
/// binding of the name could run between that assignment and `usage`.
///
/// The assignment is the last such one before `usage` (AC-1034; any number
/// of bindings since AC-1040). A binding before it is overwritten by it. One
/// between it and `usage` is admitted only in an earlier clause of an `if`
/// statement whose later clause holds `usage`, which cannot both run, or as
/// the target of the assignment `usage` is the value of. One after `usage`
/// is admitted only when no loop between the assignment's block and
/// `usage` can carry it back.
pub(super) fn sole_assignment<'tree>(
    name: &str,
    usage: TsNode<'tree>,
    scope: &Scope<'_, 'tree>,
) -> Option<TsNode<'tree>> {
    if scope.parameters.contains_key(name) {
        return None;
    }
    let sites = scope.bindings.get(name)?;
    if sites.iter().any(|site| {
        site.parent()
            .is_some_and(|p| matches!(p.kind(), "global_statement" | "nonlocal_statement"))
    }) {
        return None;
    }
    let (site, assignment, block) = sites
        .iter()
        .filter_map(|site| plain(*site, usage).map(|(a, b)| (*site, a, b)))
        .max_by_key(|(site, ..)| site.start_byte())?;
    if within_loop(usage, block) {
        return None;
    }
    let killed = assignment.parent()?.start_byte();
    for other in sites.iter().filter(|other| other.id() != site.id()) {
        if other.end_byte() <= killed || other.start_byte() >= usage.end_byte() {
            continue;
        }
        if target_of(*other, usage) || exclusive(*other, usage) {
            continue;
        }
        return None;
    }
    Some(assignment)
}

/// The plain assignment `site` is the target of, with the block holding
/// its statement, when that statement ends before `usage` and the block
/// encloses `usage`.
fn plain<'tree>(
    site: TsNode<'tree>,
    usage: TsNode<'tree>,
) -> Option<(TsNode<'tree>, TsNode<'tree>)> {
    let assignment = site.parent().filter(|p| p.kind() == "assignment")?;
    if assignment.child_by_field_name("left") != Some(site) {
        return None;
    }
    let value = assignment.child_by_field_name("right")?;
    // `a = b = f()` binds two names with one value; a yield is not a value.
    if matches!(
        value.kind(),
        "assignment" | "yield" | "augmented_assignment"
    ) {
        return None;
    }
    let statement = assignment
        .parent()
        .filter(|p| p.kind() == "expression_statement")?;
    let block = statement.parent().filter(|p| p.kind() == "block")?;
    if statement.end_byte() > usage.start_byte()
        || block.start_byte() > usage.start_byte()
        || usage.end_byte() > block.end_byte()
    {
        return None;
    }
    Some((assignment, block))
}

/// Whether a loop between `block` and `usage` could run a later binding
/// before `usage` again.
fn within_loop(usage: TsNode<'_>, block: TsNode<'_>) -> bool {
    let mut node = usage.parent();
    while let Some(n) = node {
        if n.id() == block.id() {
            return false;
        }
        if matches!(n.kind(), "for_statement" | "while_statement") {
            return true;
        }
        node = n.parent();
    }
    false
}

/// Whether `site` is the target of the assignment whose value holds `usage`
/// (`x = f(x)`): the value is read before the name is bound.
fn target_of(site: TsNode<'_>, usage: TsNode<'_>) -> bool {
    let Some(assignment) = site.parent().filter(|p| p.kind() == "assignment") else {
        return false;
    };
    assignment.child_by_field_name("left") == Some(site)
        && assignment.child_by_field_name("right").is_some_and(|v| {
            v.start_byte() <= usage.start_byte() && usage.end_byte() <= v.end_byte()
        })
}

/// Whether `site` lies in the body of an earlier clause of an `if`
/// statement whose later `elif` or `else` clause holds `usage`.
fn exclusive(site: TsNode<'_>, usage: TsNode<'_>) -> bool {
    let mut node = site.parent();
    while let Some(n) = node {
        if n.kind() == "if_statement"
            && n.start_byte() <= usage.start_byte()
            && usage.end_byte() <= n.end_byte()
        {
            let mut cursor = n.walk();
            let clauses: Vec<_> = n.children(&mut cursor).collect();
            let clause = |x: TsNode<'_>| {
                clauses
                    .iter()
                    .position(|c| c.start_byte() <= x.start_byte() && x.end_byte() <= c.end_byte())
                    .map(|i| (i, clauses[i]))
            };
            let (Some((at, own)), Some((used, later))) = (clause(site), clause(usage)) else {
                return false;
            };
            if at >= used || !matches!(later.kind(), "elif_clause" | "else_clause") {
                return false;
            }
            // The binding must be in a clause's body, not an `elif` test.
            let body = match own.kind() {
                "block" => Some(own),
                "elif_clause" | "else_clause" => own
                    .child_by_field_name("consequence")
                    .or_else(|| own.child_by_field_name("body")),
                _ => None,
            };
            return body.is_some_and(|b| {
                b.start_byte() <= site.start_byte() && site.end_byte() <= b.end_byte()
            });
        }
        node = n.parent();
    }
    false
}

pub(super) fn binding_id(cx: &FileCx<'_>, declaration: TsNode<'_>) -> String {
    format!(
        "binding:{}@{}#declaration@{}",
        cx.id.repo,
        cx.path,
        declaration.start_byte()
    )
}

/// The definitions the condition uses in `seeds` (each a use and the
/// assignment binding it) lead to, with their initializers' redactions.
/// A use whose definition is not recorded is returned in the third place:
/// its dependency must not claim a recorded definition.
pub(super) fn recover<'tree>(
    builder: &mut RuleBuilder<'_, '_, 'tree>,
    scope: &Scope<'_, 'tree>,
    seeds: &[(TsNode<'tree>, TsNode<'tree>)],
) -> (
    Vec<LocalDefinition>,
    Vec<SourceRedaction>,
    Vec<TsNode<'tree>>,
) {
    let mut recovery = Recovery {
        definitions: Vec::new(),
        indices: BTreeMap::new(),
        redactions: Vec::new(),
        nodes: 0,
        dependencies: 0,
    };
    let mut dropped = Vec::new();
    for &(usage, assignment) in seeds {
        if !recovery.definition(builder, scope, usage, assignment, 1) {
            dropped.push(usage);
        }
    }
    for definition in &mut recovery.definitions {
        definition.uses.sort_by_key(|s| (s.byte_start, s.byte_end));
        definition.uses.dedup();
    }
    (recovery.definitions, recovery.redactions, dropped)
}

struct Recovery {
    definitions: Vec<LocalDefinition>,
    indices: BTreeMap<String, usize>,
    redactions: Vec<SourceRedaction>,
    nodes: usize,
    dependencies: usize,
}

/// One initializer's arena under construction: its nodes, its
/// dependencies, and the names it reads that are defined once (the read,
/// its dependency index, and the assignment).
struct Arena<'tree> {
    nodes: Vec<DefinitionExpressionNode>,
    dependencies: Vec<DefinitionDependency>,
    names: Vec<(TsNode<'tree>, usize, TsNode<'tree>)>,
}

impl Recovery {
    /// The longest definition chain from `index`, counting it as one.
    fn height(&self, index: usize, depth: usize) -> usize {
        if depth > MAX_DEFINITION_CHAIN_DEPTH + 1 {
            return depth;
        }
        self.definitions[index]
            .dependencies
            .iter()
            .filter_map(|d| match &d.resolution {
                DependencyResolution::Binding { binding_id, .. } => {
                    self.indices.get(binding_id).copied()
                }
                _ => None,
            })
            .map(|child| 1 + self.height(child, depth + 1))
            .max()
            .unwrap_or(1)
    }

    fn definition<'tree>(
        &mut self,
        builder: &mut RuleBuilder<'_, '_, 'tree>,
        scope: &Scope<'_, 'tree>,
        usage: TsNode<'tree>,
        assignment: TsNode<'tree>,
        chain: usize,
    ) -> bool {
        let cx = builder.cx;
        let id = binding_id(cx, assignment);
        if let Some(&index) = self.indices.get(&id) {
            if chain + self.height(index, 1) - 1 > MAX_DEFINITION_CHAIN_DEPTH {
                builder.gap(RuleGapReason::DefinitionLimit, usage);
                return false;
            }
            self.definitions[index].uses.push(source(cx, usage));
            return true;
        }
        if chain > MAX_DEFINITION_CHAIN_DEPTH
            || self.definitions.len() >= MAX_LOCAL_DEFINITIONS
            || self.nodes >= MAX_DEFINITION_NODES
        {
            builder.gap(RuleGapReason::DefinitionLimit, usage);
            return false;
        }
        let Some(value) = assignment.child_by_field_name("right") else {
            return false;
        };
        let (initializer, mut redactions) = capture(cx, value);
        let mut arena = Arena {
            nodes: Vec::new(),
            dependencies: Vec::new(),
            names: Vec::new(),
        };
        let complete = expression(builder, scope, value, 1, &mut arena).is_some()
            && self.nodes + arena.nodes.len() <= MAX_DEFINITION_NODES
            && self.dependencies + arena.dependencies.len() <= MAX_INITIALIZER_DEPENDENCIES;
        if !complete {
            // An initializer outside the supported shapes is recorded whole,
            // as unsupported, with none of its partial tree.
            let gap_id = builder.gap(RuleGapReason::LocalDefinitionUnsupported, value);
            arena = Arena {
                nodes: vec![DefinitionExpressionNode {
                    expression: initializer.clone(),
                    kind: DefinitionExpressionKind::Unsupported { gap_id },
                }],
                dependencies: Vec::new(),
                names: Vec::new(),
            };
        }
        if initializer.capture != ExpressionCapture::CompleteSyntax {
            builder.control_gap(RuleGapReason::RedactedExpression, value);
        }
        self.redactions.append(&mut redactions);
        self.nodes += arena.nodes.len();
        self.dependencies += arena.dependencies.len();
        let index = self.definitions.len();
        self.indices.insert(id.clone(), index);
        self.definitions.push(LocalDefinition {
            binding_id: id,
            declaration: source(cx, assignment),
            uses: vec![source(cx, usage)],
            initializer,
            expression: DefinitionExpression {
                root: 0,
                nodes: arena.nodes,
            },
            dependencies: arena.dependencies,
        });
        for (read, dependency, inner) in arena.names {
            if !self.definition(builder, scope, read, inner, chain + 1) {
                let gap_id = builder.gap(RuleGapReason::DefinitionLimit, read);
                self.definitions[index].dependencies[dependency].resolution =
                    DependencyResolution::Unresolved { gap_id };
            }
        }
        true
    }
}

/// Pushes `node` and its subtree, root first, and returns its index; `None`
/// for a shape outside the supported ones.
fn expression<'tree>(
    builder: &mut RuleBuilder<'_, '_, 'tree>,
    scope: &Scope<'_, 'tree>,
    node: TsNode<'tree>,
    depth: usize,
    arena: &mut Arena<'tree>,
) -> Option<u32> {
    if depth > MAX_DEFINITION_EXPRESSION_DEPTH || arena.nodes.len() >= MAX_DEFINITION_NODES {
        return None;
    }
    let cx = builder.cx;
    let (captured, _) = capture(cx, node);
    if captured.capture == ExpressionCapture::Unsupported {
        return None;
    }
    let index = arena.nodes.len();
    arena.nodes.push(DefinitionExpressionNode {
        expression: captured,
        kind: DefinitionExpressionKind::PropertyName,
    });
    let dependency = |arena: &mut Arena<'tree>, resolution: DependencyResolution| {
        arena.dependencies.push(DefinitionDependency {
            source: source(cx, node),
            resolution,
        });
        (arena.dependencies.len() - 1) as u32
    };
    let kind = match node.kind() {
        "identifier" => {
            let name = cx.text(&node);
            let resolution = if let Some(assignment) = sole_assignment(name, node, scope) {
                arena
                    .names
                    .push((node, arena.dependencies.len(), assignment));
                DependencyResolution::Binding {
                    binding_id: binding_id(cx, assignment),
                    declaration: source(cx, assignment),
                }
            } else if let Some(declaration) = scope
                .parameters
                .get(name)
                .filter(|_| !scope.rebound.contains(name))
            {
                DependencyResolution::Binding {
                    binding_id: binding_id(cx, *declaration),
                    declaration: source(cx, *declaration),
                }
            } else {
                DependencyResolution::Unresolved {
                    gap_id: builder.gap(RuleGapReason::UnresolvedBinding, node),
                }
            };
            DefinitionExpressionKind::Identifier {
                dependency: dependency(arena, resolution),
            }
        }
        "attribute" => {
            let object = expression(
                builder,
                scope,
                node.child_by_field_name("object")?,
                depth + 1,
                arena,
            )?;
            let property = name_node(cx, node.child_by_field_name("attribute")?, arena)?;
            let gap_id = builder.gap(RuleGapReason::RuntimeValueUnknown, node);
            DefinitionExpressionKind::Member {
                object,
                property,
                computed: false,
                optional: false,
                dependency: dependency(arena, DependencyResolution::Unresolved { gap_id }),
            }
        }
        "call" => {
            let callee = expression(
                builder,
                scope,
                node.child_by_field_name("function")?,
                depth + 1,
                arena,
            )?;
            let list = node
                .child_by_field_name("arguments")
                .filter(|a| a.kind() == "argument_list")?;
            let mut arguments = Vec::new();
            let mut cursor = list.walk();
            for argument in list.named_children(&mut cursor) {
                match argument.kind() {
                    "comment" => {}
                    "keyword_argument" => {
                        let (keyword, _) = capture(cx, argument);
                        if keyword.capture == ExpressionCapture::Unsupported {
                            return None;
                        }
                        let at = arena.nodes.len();
                        arena.nodes.push(DefinitionExpressionNode {
                            expression: keyword,
                            kind: DefinitionExpressionKind::PropertyName,
                        });
                        let name = name_node(cx, argument.child_by_field_name("name")?, arena)?;
                        let value = expression(
                            builder,
                            scope,
                            argument.child_by_field_name("value")?,
                            depth + 2,
                            arena,
                        )?;
                        arena.nodes[at].kind =
                            DefinitionExpressionKind::KeywordArgument { name, value };
                        arguments.push(at as u32);
                    }
                    "list_splat" | "dictionary_splat" | "parenthesized_list_splat" => return None,
                    _ => arguments.push(expression(builder, scope, argument, depth + 1, arena)?),
                }
            }
            let gap_id = builder.gap(RuleGapReason::UnresolvedCall, node);
            DefinitionExpressionKind::Call {
                callee,
                arguments,
                dependency: dependency(arena, DependencyResolution::Unresolved { gap_id }),
            }
        }
        "subscript" => {
            // `suffixes[0]`: one index, read as written.
            let mut cursor = node.walk();
            let indices: Vec<_> = node
                .children_by_field_name("subscript", &mut cursor)
                .collect();
            let [index] = indices[..] else {
                return None;
            };
            let object = expression(
                builder,
                scope,
                node.child_by_field_name("value")?,
                depth + 1,
                arena,
            )?;
            let property = expression(builder, scope, index, depth + 1, arena)?;
            let gap_id = builder.gap(RuleGapReason::RuntimeValueUnknown, node);
            DefinitionExpressionKind::Member {
                object,
                property,
                computed: true,
                optional: false,
                dependency: dependency(arena, DependencyResolution::Unresolved { gap_id }),
            }
        }
        "binary_operator" => {
            let operator = match cx.text(&node.child_by_field_name("operator")?) {
                "+" => BinaryOperator::Add,
                "-" => BinaryOperator::Subtract,
                "*" => BinaryOperator::Multiply,
                "/" => BinaryOperator::Divide,
                "%" => BinaryOperator::Remainder,
                "**" => BinaryOperator::Exponent,
                "<<" => BinaryOperator::LeftShift,
                ">>" => BinaryOperator::RightShift,
                "&" => BinaryOperator::BitwiseAnd,
                "|" => BinaryOperator::BitwiseOr,
                "^" => BinaryOperator::BitwiseXor,
                // `//` and `@` have no closed category.
                _ => return None,
            };
            let left = expression(
                builder,
                scope,
                node.child_by_field_name("left")?,
                depth + 1,
                arena,
            )?;
            let right = expression(
                builder,
                scope,
                node.child_by_field_name("right")?,
                depth + 1,
                arena,
            )?;
            DefinitionExpressionKind::Binary {
                operator,
                left,
                right,
            }
        }
        "parenthesized_expression" => {
            let mut cursor = node.walk();
            let inner: Vec<_> = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() != "comment")
                .collect();
            let [inner] = inner[..] else {
                return None;
            };
            DefinitionExpressionKind::Parenthesized {
                value: expression(builder, scope, inner, depth + 1, arena)?,
            }
        }
        "string" | "integer" | "float" | "true" | "false" | "none" => {
            arena.nodes[index].expression.literal.as_ref()?;
            DefinitionExpressionKind::Literal
        }
        _ => return None,
    };
    arena.nodes[index].kind = kind;
    Some(index as u32)
}

/// A name node (an attribute's or a keyword's), as a property name.
fn name_node(cx: &FileCx<'_>, node: TsNode<'_>, arena: &mut Arena<'_>) -> Option<u32> {
    if node.kind() != "identifier" {
        return None;
    }
    let (captured, _) = capture(cx, node);
    if captured.capture == ExpressionCapture::Unsupported {
        return None;
    }
    arena.nodes.push(DefinitionExpressionNode {
        expression: captured,
        kind: DefinitionExpressionKind::PropertyName,
    });
    Some((arena.nodes.len() - 1) as u32)
}
