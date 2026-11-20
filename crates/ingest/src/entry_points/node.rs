//! Node scripts (#2008 follow-up): a `.js`/`.mjs`/`.cjs` module behind a
//! `node` shebang is a process start. When the script dispatches on its
//! command line, each subcommand it compares a value read from
//! `process.argv` against (`args.cmd === 'run'`, `case 'status':`) is a
//! command of its own; otherwise the script is one start.
//!
//! A value is read from the command line when it is `process.argv`, or an
//! expression over a name bound to one: a variable, or a parameter of a
//! function in the same file called with one. Names resolve to their
//! nearest declaring function, so an unrelated `args` elsewhere is not
//! the command line. A subcommand is a lowercase word; `--flag` options
//! are not commands.
use super::{Kind, Start, Target, operator_script, span};
use core_prov::ConfidenceTier;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tree_sitter::{Node as TsNode, Parser};

const EXTENSIONS: &[&str] = &[".js", ".mjs", ".cjs"];

/// Whether `rel` is a Node module this reader considers.
pub(super) fn candidate(rel: &str) -> bool {
    EXTENSIONS.iter().any(|ext| rel.ends_with(ext))
}

/// The starts a Node module declares: none without a `node` shebang.
pub(super) fn node_starts(rel: &str, text: &str) -> Vec<Start> {
    let Some(first) = text.lines().next() else {
        return Vec::new();
    };
    if !(first.starts_with("#!") && first.contains("node")) || !system_script(rel) {
        return Vec::new();
    }
    let mut parser = Parser::new();
    let language = if rel.ends_with(".js") {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    parser
        .set_language(&language.into())
        .expect("tree-sitter-typescript grammar");
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let commands = subcommands(text, tree.root_node());
    let file = Target::File {
        rel: rel.to_string(),
        function: None,
        confidence: ConfidenceTier::Confirmed,
        marker: None,
    };
    if commands.is_empty() {
        let kind = if operator_script(rel) {
            "command"
        } else {
            "process"
        };
        return vec![Start {
            declared_in: rel.to_string(),
            span: (0, first.len() as u64),
            key: "main".into(),
            how: "script".into(),
            command: None,
            kind: Kind {
                kind,
                evidence: None,
            },
            port: None,
            target: file,
        }];
    }
    commands
        .into_iter()
        .map(|(name, at)| Start {
            declared_in: rel.to_string(),
            span: at,
            key: name.clone(),
            how: "subcommand".into(),
            command: Some(name),
            kind: Kind {
                kind: "command",
                evidence: None,
            },
            port: None,
            target: file.clone(),
        })
        .collect()
}

/// A script of the running system, not a test or its fixture.
fn system_script(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let in_tests = rel
        .split('/')
        .rev()
        .skip(1)
        .any(|dir| matches!(dir, "tests" | "test" | "__tests__" | "node_modules"));
    !(in_tests || name.contains(".test.") || name.contains(".spec."))
}

fn is_function(node: TsNode) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
    )
}

/// The function (or program) a name declared at `node` belongs to.
fn owner(node: TsNode) -> usize {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if is_function(parent) {
            return parent.start_byte();
        }
        current = parent;
    }
    0
}

fn descendants(root: TsNode) -> Vec<TsNode> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        out.push(node);
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// What the file declares: names per owning function, and the parameters
/// of each named function.
struct Scopes<'t> {
    declared: HashSet<(usize, String)>,
    functions: HashMap<String, TsNode<'t>>,
}

impl<'t> Scopes<'t> {
    fn read(text: &str, nodes: &[TsNode<'t>]) -> Self {
        let mut declared = HashSet::new();
        let mut functions = HashMap::new();
        for node in nodes {
            match node.kind() {
                "variable_declarator" => {
                    if let Some(name) = node.child_by_field_name("name")
                        && name.kind() == "identifier"
                    {
                        let name_text = &text[name.byte_range()];
                        declared.insert((owner(*node), name_text.to_string()));
                        if let Some(value) = node.child_by_field_name("value")
                            && is_function(value)
                        {
                            functions.insert(name_text.to_string(), value);
                        }
                    }
                }
                "function_declaration" | "generator_function_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        functions.insert(text[name.byte_range()].to_string(), *node);
                    }
                }
                "required_parameter" | "optional_parameter" => {
                    if let Some(name) = node.child_by_field_name("pattern")
                        && name.kind() == "identifier"
                    {
                        declared.insert((owner(*node), text[name.byte_range()].to_string()));
                    }
                }
                _ => {}
            }
        }
        Self {
            declared,
            functions,
        }
    }

    /// The owner that declares `name` as seen from `at`, if any.
    fn resolve(&self, name: &str, at: TsNode) -> Option<usize> {
        let mut current = at;
        while let Some(parent) = current.parent() {
            if is_function(parent) && self.declared.contains(&(parent.start_byte(), name.into())) {
                return Some(parent.start_byte());
            }
            current = parent;
        }
        self.declared.contains(&(0, name.into())).then_some(0)
    }
}

/// The parameters of a function node, by position.
fn parameters<'t>(text: &str, function: TsNode<'t>) -> Vec<Option<String>> {
    let Some(list) = function.child_by_field_name("parameters").or_else(|| {
        function
            .child_by_field_name("parameter")
            .and_then(|p| p.parent())
    }) else {
        return Vec::new();
    };
    if list.kind() != "formal_parameters" {
        // `argv => …`: one bare parameter.
        return function
            .child_by_field_name("parameter")
            .map(|p| vec![Some(text[p.byte_range()].to_string())])
            .unwrap_or_default();
    }
    let mut walk = list.walk();
    list.named_children(&mut walk)
        .filter(|p| p.kind() != "comment")
        .map(|p| {
            p.child_by_field_name("pattern")
                .filter(|n| n.kind() == "identifier")
                .map(|n| text[n.byte_range()].to_string())
        })
        .collect()
}

/// The names read from the command line, and the same-file functions that
/// return a value read from it.
#[derive(Default)]
struct Flow {
    names: HashSet<(usize, String)>,
    returning: HashSet<usize>,
}

/// Whether `expr` reads the command line. A call's result does only when
/// it is a same-file function that returns a value read from it
/// (`parse(argv)`) or a method of such a value (`argv.slice(2)`); what
/// another module's function returns is not the command line.
fn derived(text: &str, expr: TsNode, scopes: &Scopes, flow: &Flow) -> bool {
    match expr.kind() {
        "member_expression"
            if text[expr.byte_range()].replace(char::is_whitespace, "") == "process.argv" =>
        {
            true
        }
        "identifier" => {
            let name = &text[expr.byte_range()];
            scopes
                .resolve(name, expr)
                .is_some_and(|owner| flow.names.contains(&(owner, name.to_string())))
        }
        "member_expression" => expr
            .child_by_field_name("object")
            .is_some_and(|object| derived(text, object, scopes, flow)),
        "call_expression" => {
            let Some(callee) = expr.child_by_field_name("function") else {
                return false;
            };
            match callee.kind() {
                "identifier" => scopes
                    .functions
                    .get(&text[callee.byte_range()])
                    .is_some_and(|function| flow.returning.contains(&function.start_byte())),
                "member_expression" => derived(text, callee, scopes, flow),
                _ => false,
            }
        }
        _ if is_function(expr) => false,
        _ => {
            let mut walk = expr.walk();
            expr.named_children(&mut walk)
                .any(|child| derived(text, child, scopes, flow))
        }
    }
}

/// Lowercase-word literals compared against the command line, each with
/// the span of its first comparison.
fn subcommands(text: &str, root: TsNode) -> BTreeMap<String, (u64, u64)> {
    let nodes = descendants(root);
    let scopes = Scopes::read(text, &nodes);
    let mut flow = Flow::default();
    // Fixpoint over bindings and same-file calls; bounded, as each pass
    // only adds names.
    for _ in 0..16 {
        let before = (flow.names.len(), flow.returning.len());
        for node in &nodes {
            match node.kind() {
                "variable_declarator" => {
                    if let (Some(name), Some(value)) = (
                        node.child_by_field_name("name"),
                        node.child_by_field_name("value"),
                    ) && name.kind() == "identifier"
                        && !is_function(value)
                        && derived(text, value, &scopes, &flow)
                    {
                        flow.names
                            .insert((owner(*node), text[name.byte_range()].to_string()));
                    }
                }
                "call_expression" => {
                    let Some(callee) = node.child_by_field_name("function") else {
                        continue;
                    };
                    let Some(function) = scopes.functions.get(&text[callee.byte_range()]) else {
                        continue;
                    };
                    let Some(arguments) = node.child_by_field_name("arguments") else {
                        continue;
                    };
                    let mut walk = arguments.walk();
                    let arguments: Vec<TsNode> = arguments
                        .named_children(&mut walk)
                        .filter(|a| a.kind() != "comment")
                        .collect();
                    for (argument, parameter) in arguments.iter().zip(parameters(text, *function)) {
                        if let Some(parameter) = parameter
                            && derived(text, *argument, &scopes, &flow)
                        {
                            flow.names.insert((function.start_byte(), parameter));
                        }
                    }
                }
                "return_statement" => {
                    if let Some(value) = node.named_child(0)
                        && derived(text, value, &scopes, &flow)
                    {
                        flow.returning.insert(owner(*node));
                    }
                }
                "arrow_function" => {
                    if let Some(body) = node.child_by_field_name("body")
                        && body.kind() != "statement_block"
                        && derived(text, body, &scopes, &flow)
                    {
                        flow.returning.insert(node.start_byte());
                    }
                }
                _ => {}
            }
        }
        if (flow.names.len(), flow.returning.len()) == before {
            break;
        }
    }
    let word = |literal: TsNode| -> Option<String> {
        if literal.kind() != "string" {
            return None;
        }
        let raw = &text[literal.byte_range()];
        let inner = raw.get(1..raw.len().saturating_sub(1))?;
        let mut chars = inner.chars();
        (chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .then(|| inner.to_string())
    };
    let read_from = |subject: TsNode| {
        matches!(
            subject.kind(),
            "identifier" | "member_expression" | "subscript_expression"
        ) && derived(text, subject, &scopes, &flow)
    };
    let mut found: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for node in &nodes {
        let pair = match node.kind() {
            "binary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map(|o| &text[o.byte_range()]);
                if !matches!(operator, Some("===" | "==")) {
                    continue;
                }
                let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) else {
                    continue;
                };
                [(left, right), (right, left)]
                    .into_iter()
                    .find_map(|(subject, literal)| {
                        (read_from(subject)).then(|| word(literal)).flatten()
                    })
            }
            "switch_case" => {
                let subject = node
                    .parent()
                    .and_then(|body| body.parent())
                    .and_then(|switch| switch.child_by_field_name("value"))
                    .map(|v| {
                        if v.kind() == "parenthesized_expression" {
                            v.named_child(0).unwrap_or(v)
                        } else {
                            v
                        }
                    });
                match (subject, node.child_by_field_name("value")) {
                    (Some(subject), Some(literal)) if read_from(subject) => word(literal),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(name) = pair
            && seen.insert((name.clone(), node.start_byte()))
        {
            let at = span(*node);
            found
                .entry(name)
                .and_modify(|first| *first = (*first).min(at))
                .or_insert(at);
        }
    }
    found
}
