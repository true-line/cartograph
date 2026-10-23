//! Argument dispatch in a scheduled Python entry (#779): a job such as
//! `python cron.py -j stats` runs one branch of the script's `__main__`
//! block, not the whole file. The branch is proven only when, inside
//! `if __name__ == "__main__":`,
//!
//! - one `<parser>.add_argument(...)` declares the option the command
//!   passes (its literal option strings; `dest=` or the first long option
//!   names the attribute);
//! - `<args> = <parser>.parse_args()` binds the parsed arguments;
//! - exactly one `if`/`elif` condition compares `<args>.<dest>` with the
//!   command's literal value; and
//! - that branch's body makes exactly one bare call to a function the file
//!   defines at module level.
//!
//! Anything else proves no dispatch, and the entry stays the file.

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

/// The dispatch the arguments after the script select in `source`, if
/// proven.
pub(super) fn dispatch(source: &str, args: &[&str]) -> Option<Dispatch> {
    let (option, value) = selected(args)?;
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
    let functions: Vec<&str> = children(root)
        .into_iter()
        .filter_map(|node| match node.kind() {
            "function_definition" => Some(node),
            "decorated_definition" => node.child_by_field_name("definition"),
            _ => None,
        })
        .filter(|node| node.kind() == "function_definition")
        .filter_map(|node| node.child_by_field_name("name").map(text))
        .collect();
    let main = children(root).into_iter().find(|node| {
        node.kind() == "if_statement"
            && node
                .child_by_field_name("condition")
                .is_some_and(|condition| is_main_guard(source, condition))
    })?;
    let body = main.child_by_field_name("consequence")?;
    let mut nodes = Vec::new();
    collect(body, &mut nodes);

    // `<parser>.add_argument(<option strings>…, dest=…)`.
    let declared: Vec<(&str, String)> = nodes
        .iter()
        .filter_map(|node| {
            let (receiver, method, arguments) = method_call(source, *node)?;
            (method == "add_argument").then_some(())?;
            let mut options = Vec::new();
            let mut dest = None;
            for argument in children(arguments) {
                match argument.kind() {
                    "string" => options.push(literal(source, argument)?),
                    "keyword_argument" => {
                        let name = text(argument.child_by_field_name("name")?);
                        if name == "dest" {
                            dest = Some(literal(source, argument.child_by_field_name("value")?)?);
                        }
                    }
                    _ => {}
                }
            }
            options.contains(&option).then_some(())?;
            let dest = dest.or_else(|| {
                let long = options.iter().find(|o| o.starts_with("--"))?;
                Some(long.trim_start_matches('-').replace('-', "_"))
            })?;
            Some((receiver, dest))
        })
        .collect();
    let [(parser_name, dest)] = declared.as_slice() else {
        return None;
    };
    // `<args> = <parser>.parse_args()`.
    let bound: Vec<&str> = nodes
        .iter()
        .filter(|node| node.kind() == "assignment")
        .filter_map(|node| {
            let left = node.child_by_field_name("left")?;
            let (receiver, method, arguments) =
                method_call(source, node.child_by_field_name("right")?)?;
            (left.kind() == "identifier"
                && receiver == *parser_name
                && method == "parse_args"
                && arguments.named_child_count() == 0)
                .then(|| text(left))
        })
        .collect();
    let [args_name] = bound.as_slice() else {
        return None;
    };
    // Every `if`/`elif` comparing `<args>.<dest>` with the value.
    let branches: Vec<TsNode> = nodes
        .iter()
        .filter(|node| matches!(node.kind(), "if_statement" | "elif_clause"))
        .filter(|node| {
            node.child_by_field_name("condition")
                .is_some_and(|condition| compares(source, condition, args_name, dest, &value))
        })
        .copied()
        .collect();
    let [branch] = branches.as_slice() else {
        return None;
    };
    let consequence = branch.child_by_field_name("consequence")?;
    let calls: Vec<&str> = children(consequence)
        .into_iter()
        .filter(|statement| statement.kind() == "expression_statement")
        .filter_map(|statement| {
            let [call] = children(statement)[..] else {
                return None;
            };
            let function = call.child_by_field_name("function")?;
            (call.kind() == "call" && function.kind() == "identifier").then(|| text(function))
        })
        .collect();
    let [function] = calls.as_slice() else {
        return None;
    };
    // A name the file defines twice is not one proven function.
    (functions.iter().filter(|name| *name == function).count() == 1).then(|| Dispatch {
        option,
        value,
        function: function.to_string(),
        span: (branch.start_byte() as u64, branch.end_byte() as u64),
    })
}

/// The option and value the command passes: `-j v`, `--job v` or
/// `--job=v`. Several options are ambiguous.
fn selected(args: &[&str]) -> Option<(String, String)> {
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
            _ => (word.to_string(), words.next()?.to_string()),
        };
        if pair.1.is_empty() || pair.1.starts_with('-') || found.replace(pair).is_some() {
            return None;
        }
    }
    found
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

/// `<args>.<dest> == "<value>"`, either way round.
fn compares(source: &str, condition: TsNode, args: &str, dest: &str, value: &str) -> bool {
    comparison(condition).is_some_and(|(left, right)| {
        let attribute = |node: TsNode| {
            node.kind() == "attribute"
                && node
                    .child_by_field_name("object")
                    .is_some_and(|o| o.kind() == "identifier" && &source[o.byte_range()] == args)
                && node
                    .child_by_field_name("attribute")
                    .is_some_and(|a| &source[a.byte_range()] == dest)
        };
        let literal = |node: TsNode| literal(source, node).as_deref() == Some(value);
        (attribute(left) && literal(right)) || (literal(left) && attribute(right))
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
