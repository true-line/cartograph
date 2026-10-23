//! Argument dispatch in a scheduled Python entry (#779): a job such as
//! `python cron.py -j stats` runs one branch of the script's `__main__`
//! block, not the whole file. The branch is proven only when, inside
//! `if __name__ == "__main__":`,
//!
//! - `<parser> = argparse.ArgumentParser(...)`, with `import argparse` the
//!   module's only binding of `argparse`, is the module's only binding of
//!   `<parser>`, which the block then only reads as attributes;
//! - after it, one `<parser>.add_argument(...)` declares the option the
//!   command passes (literal option strings; `dest=`, else argparse's
//!   default, names the attribute), with only value-preserving keywords;
//! - after it, `<args> = <parser>.parse_args()` binds the parsed arguments,
//!   which the block then only reads as attributes;
//! - after that, one `if`/`elif` chain tests `<args>.<dest>`, every clause
//!   before the selected one comparing it with another literal; all three
//!   run in order on the block's straight-line path (entering `with`
//!   bodies only); and
//! - the selected branch makes exactly one bare call to a name bound once
//!   in module scope, by an undecorated top-level `def` before the guard.
//!
//! Anything else proves no dispatch: the entry stays the file, and the
//! `ENTRY` names why (`dispatch_unresolved`).

use tree_sitter::{Node as TsNode, Parser};

/// The function a command's argument selects, and where that is proven.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Dispatch {
    /// The option the command passed (`-j`, `--job`).
    pub option: String,
    /// The literal value it selected.
    pub value: String,
    /// The module-level function the branch calls.
    pub function: String,
    /// Byte span of the selected branch.
    pub span: (u64, u64),
}

/// Why a command's options select no proven function.
pub(super) const UNPROVEN: &str = "argument dispatch not proven in __main__";
/// Several options, or one without a value, select no single branch.
pub(super) const AMBIGUOUS: &str = "schedule command passes several options or a valueless one";

/// The dispatch the arguments after the script select in `source`: `None`
/// when they pass no option, otherwise the dispatch or why it is unproven.
pub(super) fn dispatch(source: &str, args: &[&str]) -> Option<Result<Dispatch, &'static str>> {
    let (option, value) = match selected(args) {
        Ok(Some(pair)) => pair,
        Ok(None) => return None,
        Err(reason) => return Some(Err(reason)),
    };
    Some(proven(source, option, value).ok_or(UNPROVEN))
}

fn proven(source: &str, option: String, value: String) -> Option<Dispatch> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let text = |node: TsNode| &source[node.byte_range()];
    let main = children(root).into_iter().find(|node| {
        node.kind() == "if_statement"
            && node
                .child_by_field_name("condition")
                .is_some_and(|condition| is_main_guard(source, condition))
    })?;
    let body = main.child_by_field_name("consequence")?;
    // Every node of the block, and the statements it runs in order (entering
    // `with` bodies only): a declaration, binding or dispatch elsewhere —
    // under another condition, in a loop — is not on the proven path.
    let mut nodes = Vec::new();
    collect(body, &mut nodes);
    let mut path = Vec::new();
    straight(body, &mut path);

    // The one `<parser>.add_argument(...)` naming the option, on the path,
    // with only value-preserving keywords.
    let declarations: Vec<TsNode> = nodes
        .iter()
        .copied()
        .filter(|node| {
            method_call(source, *node).is_some_and(|(_, method, arguments)| {
                method == "add_argument"
                    && children(arguments)
                        .into_iter()
                        .any(|argument| literal(source, argument).as_deref() == Some(&option))
            })
        })
        .collect();
    let [declaration] = declarations.as_slice() else {
        return None;
    };
    let declared_at = path
        .iter()
        .position(|statement| expression(*statement) == Some(*declaration))?;
    let (parser_name, _, arguments) = method_call(source, *declaration)?;
    let dest = declared_dest(source, root, arguments, &value)?;
    // The receiver is the module's one parser, built from the real argparse
    // on the path before the declaration and only read after it: a
    // replacement or a parser passed on may parse differently (#796 review).
    let [constructed] = binders(source, root, parser_name)?[..] else {
        return None;
    };
    // On the path; a use before it would fail, so it precedes the
    // declaration.
    path.iter()
        .position(|statement| expression(*statement) == Some(constructed))?;
    let constructor = constructed.child_by_field_name("right")?;
    // Only a call has a `function`.
    if constructor
        .child_by_field_name("function")
        .is_none_or(|function| text(function) != "argparse.ArgumentParser")
        || !imported_module(source, root, main, "argparse")
        || !read_as_attribute(
            source,
            &nodes,
            parser_name,
            constructed.child_by_field_name("left")?,
        )
    {
        return None;
    }
    // `<args> = <parser>.parse_args()` after it, on the path.
    let (_, args_name, binding) =
        path.iter()
            .enumerate()
            .skip(declared_at + 1)
            .find_map(|(at, statement)| {
                let assignment =
                    expression(*statement).filter(|node| node.kind() == "assignment")?;
                let left = assignment.child_by_field_name("left")?;
                let (receiver, method, arguments) =
                    method_call(source, assignment.child_by_field_name("right")?)?;
                (left.kind() == "identifier"
                    && receiver == parser_name
                    && method == "parse_args"
                    && arguments.named_child_count() == 0)
                    .then(|| (at, text(left), left))
            })?;
    // The parsed arguments are only read, as attributes, after the binding:
    // any other use (rebinding, `args.job = …`, passing `args` on) may
    // change the value.
    if !read_as_attribute(source, &nodes, args_name, binding) {
        return None;
    }
    // The one `if`/`elif` chain on the path that tests `<args>.<dest>` (so
    // after the binding, as every read is); every earlier clause compares
    // it with another literal.
    let tests_dest = |condition: TsNode| {
        comparison(condition).is_some_and(|(left, right)| {
            is_args_attribute(source, left, args_name, &dest)
                || is_args_attribute(source, right, args_name, &dest)
        })
    };
    let chains: Vec<TsNode> = path
        .iter()
        .copied()
        .filter(|statement| {
            statement.kind() == "if_statement"
                && clauses(*statement).iter().any(|clause| {
                    clause
                        .child_by_field_name("condition")
                        .is_some_and(tests_dest)
                })
        })
        .collect();
    let [chain] = chains.as_slice() else {
        return None;
    };
    let mut branch = None;
    for clause in clauses(*chain) {
        let condition = clause.child_by_field_name("condition")?;
        let compared = compared_literal(source, condition, args_name, &dest)?;
        if compared == value {
            branch = Some(clause);
            break;
        }
    }
    let branch = branch?;
    let consequence = branch.child_by_field_name("consequence")?;
    let calls: Vec<&str> = children(consequence)
        .into_iter()
        .filter_map(expression)
        .filter_map(|call| {
            let function = call.child_by_field_name("function")?;
            (call.kind() == "call" && function.kind() == "identifier").then(|| text(function))
        })
        .collect();
    let [function] = calls.as_slice() else {
        return None;
    };
    // The function is bound once, by an undecorated top-level `def` that
    // has run before the guard: a conditional or later `def` may not exist
    // when the branch calls it (#796 review).
    let [def] = binders(source, root, function)?[..] else {
        return None;
    };
    (def.kind() == "function_definition"
        && def.parent() == Some(root)
        && def.end_byte() <= main.start_byte())
    .then(|| Dispatch {
        option,
        value,
        function: function.to_string(),
        span: (branch.start_byte() as u64, branch.end_byte() as u64),
    })
}

/// The attribute an `add_argument` call stores, when its keywords keep the
/// passed string as is: `help`, `metavar`, `required`, `default`, `dest`,
/// `type=str` (the builtin, which the module never binds), `action="store"`,
/// and `choices` listing `value`. Anything else (a converting `type`,
/// `nargs`, other actions, splats) proves no stored value.
fn declared_dest(source: &str, root: TsNode, arguments: TsNode, value: &str) -> Option<String> {
    let mut options = Vec::new();
    let mut dest = None;
    for argument in children(arguments) {
        match argument.kind() {
            "string" => options.push(literal(source, argument).filter(|o| o.starts_with('-'))?),
            "keyword_argument" => {
                let name = &source[argument.child_by_field_name("name")?.byte_range()];
                let keyword = argument.child_by_field_name("value")?;
                match name {
                    "help" | "metavar" | "required" | "default" => {}
                    "dest" => dest = Some(literal(source, keyword)?),
                    "type"
                        if &source[keyword.byte_range()] == "str"
                            && binders(source, root, "str")?.is_empty() => {}
                    "action" if literal(source, keyword).as_deref() == Some("store") => {}
                    "choices" => {
                        let listed = matches!(keyword.kind(), "list" | "tuple")
                            && children(keyword)
                                .into_iter()
                                .any(|choice| literal(source, choice).as_deref() == Some(value));
                        if !listed {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    // argparse's default: the first long option, else the first short one.
    dest.or_else(|| {
        let option = options
            .iter()
            .find(|o| o.starts_with("--"))
            .or(options.first())?;
        Some(option.trim_start_matches('-').replace('-', "_"))
    })
}

/// The statements a block runs in order, entering `with` bodies.
fn straight<'t>(block: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    for statement in children(block) {
        out.push(statement);
        if statement.kind() == "with_statement"
            && let Some(body) = statement.child_by_field_name("body")
        {
            straight(body, out);
        }
    }
}

/// An `if` statement's condition clauses in order: the `if`, then `elif`s.
fn clauses(statement: TsNode) -> Vec<TsNode> {
    std::iter::once(statement)
        .chain(
            children(statement)
                .into_iter()
                .filter(|clause| clause.kind() == "elif_clause"),
        )
        .collect()
}

/// The literal `<args>.<dest>` is compared with by `==`, either way round.
fn compared_literal(source: &str, condition: TsNode, args: &str, dest: &str) -> Option<String> {
    let (left, right) = comparison(condition)?;
    if is_args_attribute(source, left, args, dest) {
        literal(source, right)
    } else if is_args_attribute(source, right, args, dest) {
        literal(source, left)
    } else {
        None
    }
}

fn is_args_attribute(source: &str, node: TsNode, args: &str, dest: &str) -> bool {
    node.kind() == "attribute"
        && node
            .child_by_field_name("object")
            .is_some_and(|o| o.kind() == "identifier" && &source[o.byte_range()] == args)
        && node
            .child_by_field_name("attribute")
            .is_some_and(|a| &source[a.byte_range()] == dest)
}

/// Whether `node` is (inside) the target of an assignment or deletion.
fn is_write_target(node: TsNode) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        let target = match parent.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                parent.child_by_field_name("left")
            }
            "named_expression" => parent.child_by_field_name("name"),
            "delete_statement" | "as_pattern_target" => return true,
            _ => None,
        };
        if target == Some(child) {
            return true;
        }
        child = parent;
    }
    false
}

/// Whether every use of `name` in `nodes` but `binding` is an attribute
/// read after `binding`: a read before it would fail at run time, and any
/// other use (rebinding, `x.a = …`, passing `x` on) may change the value.
fn read_as_attribute(source: &str, nodes: &[TsNode], name: &str, binding: TsNode) -> bool {
    nodes
        .iter()
        .filter(|node| {
            node.kind() == "identifier" && &source[node.byte_range()] == name && **node != binding
        })
        .all(|node| {
            node.start_byte() >= binding.end_byte()
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "attribute"
                        && parent.child_by_field_name("object") == Some(*node)
                        && !is_write_target(parent)
                })
        })
}

/// Whether `module` is bound only by a top-level `import <module>` before
/// the guard.
fn imported_module(source: &str, root: TsNode, main: TsNode, module: &str) -> bool {
    binders(source, root, module).is_some_and(|binders| {
        matches!(binders[..], [import]
        if import.kind() == "import_statement"
            && import.parent() == Some(root)
            && import.end_byte() <= main.start_byte()
            && children(import).iter().any(|name| {
                name.kind() == "dotted_name" && &source[name.byte_range()] == module
            }))
    })
}

/// The module-scope nodes that bind `name`: `def`s and classes (the
/// definition node), imports and deletions mentioning it, and assignment,
/// loop, walrus and `as` targets (the assignment for a plain one). `None`
/// when a binding cannot be placed: a wildcard import, or `global` naming
/// it in any function.
fn binders<'t>(source: &str, root: TsNode<'t>, name: &str) -> Option<Vec<TsNode<'t>>> {
    let mut found = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((node, in_function)) = stack.pop() {
        let named = |field: &str| {
            node.child_by_field_name(field)
                .is_some_and(|n| &source[n.byte_range()] == name)
        };
        let mentions = |node: TsNode| {
            let mut nodes = Vec::new();
            collect(node, &mut nodes);
            nodes
                .iter()
                .any(|n| n.kind() == "identifier" && &source[n.byte_range()] == name)
        };
        let binds = match node.kind() {
            "global_statement" if mentions(node) => return None,
            "wildcard_import" => return None,
            "function_definition" | "class_definition" => named("name"),
            "import_statement"
            | "import_from_statement"
            | "delete_statement"
            | "as_pattern_target" => mentions(node),
            "assignment" | "augmented_assignment" | "for_statement" => {
                node.child_by_field_name("left").is_some_and(mentions)
            }
            "named_expression" => named("name"),
            _ => false,
        };
        if binds && !in_function {
            // A plain assignment is its statement's expression.
            found.push(node);
        }
        let nested =
            in_function || matches!(node.kind(), "function_definition" | "class_definition");
        for child in children(node) {
            // A def's name and decorators are in the enclosing scope; only
            // its parameters and body are nested.
            let inner = nested
                && !(matches!(node.kind(), "function_definition" | "class_definition")
                    && !in_function
                    && node.child_by_field_name("name") == Some(child));
            stack.push((child, inner));
        }
    }
    Some(found)
}

/// The option and value the command passes: `-j v`, `--job v` or
/// `--job=v`; `None` when it passes no option. Several options, or one
/// without a value, are ambiguous.
fn selected(args: &[&str]) -> Result<Option<(String, String)>, &'static str> {
    let mut found = None;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if !word.starts_with('-') {
            continue;
        }
        let pair = match word.split_once('=') {
            Some((option, value)) if word.starts_with("--") => {
                (option.to_string(), value.to_string())
            }
            _ => (word.to_string(), words.next().ok_or(AMBIGUOUS)?.to_string()),
        };
        if pair.1.is_empty() || pair.1.starts_with('-') || found.replace(pair).is_some() {
            return Err(AMBIGUOUS);
        }
    }
    Ok(found)
}

/// The expression an expression statement holds.
fn expression(statement: TsNode<'_>) -> Option<TsNode<'_>> {
    match children(statement)[..] {
        [only] if statement.kind() == "expression_statement" => Some(only),
        _ => None,
    }
}

/// A node's named children.
fn children<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut walk = node.walk();
    node.named_children(&mut walk).collect()
}

fn collect<'t>(node: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    out.push(node);
    let mut walk = node.walk();
    let children: Vec<TsNode<'t>> = node.named_children(&mut walk).collect();
    for child in children {
        // A nested function or class body is not the `__main__` block's code.
        if !matches!(child.kind(), "function_definition" | "class_definition") {
            collect(child, out);
        }
    }
}

/// `<receiver>.<method>(<arguments>)` with an identifier receiver.
fn method_call<'t>(source: &'t str, node: TsNode<'t>) -> Option<(&'t str, &'t str, TsNode<'t>)> {
    (node.kind() == "call").then_some(())?;
    let function = node.child_by_field_name("function")?;
    (function.kind() == "attribute").then_some(())?;
    let object = function.child_by_field_name("object")?;
    (object.kind() == "identifier").then_some(())?;
    let attribute = function.child_by_field_name("attribute")?;
    Some((
        &source[object.byte_range()],
        &source[attribute.byte_range()],
        node.child_by_field_name("arguments")?,
    ))
}

/// A plain string literal's value: no prefix, escape or interpolation.
fn literal(source: &str, node: TsNode) -> Option<String> {
    (node.kind() == "string").then_some(())?;
    let mut walk = node.walk();
    let parts: Vec<TsNode> = node.named_children(&mut walk).collect();
    let raw = &source[node.byte_range()];
    if !(raw.starts_with('"') || raw.starts_with('\'')) {
        return None;
    }
    match parts.as_slice() {
        [start, content, end]
            if start.kind() == "string_start"
                && content.kind() == "string_content"
                && end.kind() == "string_end" =>
        {
            let value = &source[content.byte_range()];
            (!value.contains('\\')).then(|| value.to_string())
        }
        _ => None,
    }
}

/// `__name__ == "__main__"`, either way round.
fn is_main_guard(source: &str, condition: TsNode) -> bool {
    comparison(condition).is_some_and(|(left, right)| {
        let name =
            |node: TsNode| node.kind() == "identifier" && &source[node.byte_range()] == "__name__";
        let main = |node: TsNode| literal(source, node).as_deref() == Some("__main__");
        (name(left) && main(right)) || (main(left) && name(right))
    })
}

/// The operands of a single `==` comparison.
fn comparison(node: TsNode) -> Option<(TsNode, TsNode)> {
    (node.kind() == "comparison_operator").then_some(())?;
    let mut walk = node.walk();
    let operands: Vec<TsNode> = node.named_children(&mut walk).collect();
    let [left, right] = operands.as_slice() else {
        return None;
    };
    let mut walk = node.walk();
    let operators: Vec<&str> = node
        .children(&mut walk)
        .filter(|child| !child.is_named())
        .map(|child| child.kind())
        .collect();
    (operators == ["=="]).then_some((*left, *right))
}
