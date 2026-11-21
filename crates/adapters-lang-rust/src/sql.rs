//! Rust SQL persistence (#2041, AC-1015): `rusqlite`, `sqlx`, `postgres`,
//! `tokio-postgres` and `diesel` statements written as literal SQL, as the
//! tables they read and write (the shared reading in
//! [`adapters_fw::sql`]).
//!
//! A file is read for SQL only when it names one of those crates in a `use`
//! or a qualified path. A statement is then:
//!
//! - a method call `x.M(sql, …)` whose method takes the statement first:
//!   `execute`, `query`, `query_row`, `query_row_and_then`, `query_one` and
//!   `query_opt` with at least one argument after it, or `prepare`,
//!   `prepare_cached`, `execute_batch`, `batch_execute` and `simple_query`
//!   with the statement alone;
//! - a call of `sqlx::query`, `query_as`, `query_scalar`, their `_with`
//!   forms, `sqlx::raw_sql` or `diesel::sql_query`, its path resolved
//!   through the file's `use`s;
//! - a `sqlx` checked macro (`query!`, `query_as!`, `query_scalar!` and
//!   their `_unchecked` forms), whose statement is its first string literal.
//!
//! The receiver's type is not proven, so every fact is InferredStrong with
//! resolution `sql-literal` (or `sql-fragments` when the statement is
//! assembled). The statement is a string literal, a `const` or `static`
//! the file declares with one, a name the function binds with `let` before
//! the call, `+` of those, or `format!` whose placeholders are computed;
//! `&`, `.as_str()`, `.to_string()` and the like are seen through. An
//! argument that is none of these (a parameter list such as
//! `stmt.execute(params![…])`, an unbound name) states nothing. A
//! statement that is assembled and names no table, or names a computed
//! table, is a Gap naming the expression.

use super::*;
use adapters_fw::sql::{Access, COMPUTED, entity_id, parse_with_gaps};
use http::{call_arguments, resolve_external_paths, unquote_string_literal};

/// Crates whose `use` marks a file as one that may run SQL, those whose
/// connections take statements by method first.
const CRATES: &[&str] = &["rusqlite", "postgres", "tokio_postgres", "sqlx", "diesel"];

/// Methods taking the statement first and parameters after it.
const WITH_PARAMS: &[&str] = &[
    "execute",
    "query",
    "query_row",
    "query_row_and_then",
    "query_one",
    "query_opt",
];

/// Methods taking the statement alone.
const ALONE: &[&str] = &[
    "prepare",
    "prepare_cached",
    "execute_batch",
    "batch_execute",
    "simple_query",
];

/// Functions taking the statement first, by resolved path.
const FUNCTIONS: &[&[&str]] = &[
    &["sqlx", "query"],
    &["sqlx", "query_as"],
    &["sqlx", "query_scalar"],
    &["sqlx", "query_with"],
    &["sqlx", "query_as_with"],
    &["sqlx", "query_scalar_with"],
    &["sqlx", "raw_sql"],
    &["diesel", "sql_query"],
    &["diesel", "dsl", "sql_query"],
];

/// `sqlx` macros whose first string literal is the statement.
const MACROS: &[&str] = &[
    "query",
    "query_as",
    "query_scalar",
    "query_unchecked",
    "query_as_unchecked",
    "query_scalar_unchecked",
];

/// Append the tables the file's SQL statements read and write.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &Bindings,
    functions: &HashMap<usize, String>,
    out: &mut Extraction,
) {
    let Some(file_framework) = framework(cx, root, bindings) else {
        return;
    };
    let constants = constants(cx, root);
    // (src, table, label) -> (operations, cited call, from one literal)
    #[allow(clippy::type_complexity)]
    let mut accesses: BTreeMap<
        (String, String, &'static str),
        (BTreeSet<&'static str>, TsNode<'_>, bool),
    > = BTreeMap::new();
    let mut tables: BTreeMap<String, (BTreeSet<String>, TsNode<'_>, &'static str)> =
        BTreeMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        let Some((argument, framework)) = statement(cx, bindings, node) else {
            continue;
        };
        // A test's statements set up its fixture; they are not the system's.
        if target_tests::in_test_code(cx, node) {
            continue;
        }
        let mut statements = Statements::default();
        let Some(computed) = piece(
            cx,
            &constants,
            argument,
            node.start_byte(),
            0,
            &mut statements.texts,
        ) else {
            continue;
        };
        statements.computed = computed;
        let owner = enclosing_function(node, functions).map(|(symbol, _)| symbol);
        let src = owner.unwrap_or_else(|| file_id(cx.id.repo, cx.path));
        let mut found: Vec<Access> = Vec::new();
        let mut unresolved = false;
        for text in &statements.texts {
            let (accesses, computed_table) = parse_with_gaps(text);
            found.extend(accesses);
            unresolved |= computed_table;
        }
        if unresolved || (found.is_empty() && statements.computed) {
            let reason = if unresolved {
                "SQL statement names a computed table"
            } else {
                "SQL statement is computed"
            };
            gap(cx, node, argument, &src, reason, out);
        }
        let exact = !statements.computed && statements.texts.len() == 1;
        for access in found {
            let entry = tables
                .entry(access.table.clone())
                .or_insert_with(|| (BTreeSet::new(), node, framework.unwrap_or(file_framework)));
            entry.0.extend(access.columns.iter().cloned());
            let label = if access.write.is_some() {
                "WRITES"
            } else {
                "READS"
            };
            let entry = accesses
                .entry((src.clone(), access.table, label))
                .or_insert_with(|| (BTreeSet::new(), node, exact));
            entry.0.extend(access.write);
            if exact && !entry.2 {
                entry.1 = node;
                entry.2 = true;
            }
        }
    }
    for (table, (columns, at, framework)) in tables {
        let id = entity_id(cx.id.repo, &table);
        let columns: Vec<serde_json::Value> = columns
            .into_iter()
            .map(|name| serde_json::json!({ "name": name }))
            .collect();
        out.nodes.push(Node {
            props: serde_json::json!({
                "name": table,
                "storage": "sql",
                "store": table,
                "table": table,
                "columns": columns,
                "framework": framework,
                "language": "rust",
                "resolution": "sql-literal",
                "prov": cx.prov_with_confidence(&at, ConfidenceTier::InferredStrong, &format!("DataEntity {id}")),
            }),
            id,
            label: "DataEntity".into(),
        });
    }
    for ((src, table, label), (operations, at, exact)) in accesses {
        let dst = entity_id(cx.id.repo, &table);
        let mut props = serde_json::json!({
            "resolution": if exact { "sql-literal" } else { "sql-fragments" },
            "prov": cx.prov_with_confidence(&at, ConfidenceTier::InferredStrong, &format!("{label} {src} -> {dst}")),
        });
        if let Some(first) = operations.first() {
            props["operation"] = (*first).into();
            props["operations"] = operations.iter().copied().collect::<Vec<_>>().into();
        }
        out.edges.push(Edge {
            src,
            dst,
            label: label.into(),
            props,
        });
    }
}

/// The SQL crate a method-call statement in the file belongs to, the first
/// of [`CRATES`] a `use` or a parsed qualified path names (never a comment
/// or a string); `None` when it names none.
fn framework(cx: &FileCx<'_>, root: TsNode<'_>, bindings: &Bindings) -> Option<&'static str> {
    let mut named: BTreeSet<&str> = bindings
        .map
        .values()
        .filter(|(_, external)| *external)
        .filter_map(|(path, _)| path.first().map(String::as_str))
        .collect();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if !matches!(node.kind(), "scoped_identifier" | "scoped_type_identifier") {
            continue;
        }
        let text = cx.text(&node).trim_start_matches("::");
        let head = text.split("::").next().unwrap_or("").trim();
        if let Some(name) = CRATES.iter().find(|name| **name == head)
            && !cx.workspace_crates.contains(head)
            && !cx.declared_mods.contains(head)
        {
            named.insert(name);
        }
    }
    CRATES
        .iter()
        .find(|name| named.contains(**name))
        .map(|name| crate_label(name))
}

fn crate_label(name: &str) -> &'static str {
    match name {
        "rusqlite" => "rusqlite",
        "postgres" => "postgres",
        "tokio_postgres" => "tokio-postgres",
        "sqlx" => "sqlx",
        _ => "diesel",
    }
}

/// The statement argument of `node` when it is a statement call or macro,
/// with the crate a function or macro statement proves.
fn statement<'t>(
    cx: &FileCx<'_>,
    bindings: &Bindings,
    node: TsNode<'t>,
) -> Option<(TsNode<'t>, Option<&'static str>)> {
    match node.kind() {
        "call_expression" => {
            let mut function = node.child_by_field_name("function")?;
            // `conn.query_row::<T, _, _>(…)`, `sqlx::query_as::<_, T>(…)`.
            if function.kind() == "generic_function" {
                function = function.child_by_field_name("function")?;
            }
            let arguments = call_arguments(node);
            let first = *arguments.first()?;
            match function.kind() {
                "field_expression" => {
                    let method = cx.text(&function.child_by_field_name("field")?);
                    let fits = (WITH_PARAMS.contains(&method) && arguments.len() >= 2)
                        || (ALONE.contains(&method) && arguments.len() == 1);
                    fits.then_some((first, None))
                }
                "identifier" | "scoped_identifier" => {
                    let callee = function;
                    let scopes = visible_scopes(cx, node);
                    let paths =
                        resolve_external_paths(cx, bindings, &scopes, cx.text(&callee), None);
                    let [path] = paths.as_slice() else {
                        return None;
                    };
                    FUNCTIONS
                        .iter()
                        .any(|known| path.iter().map(String::as_str).eq(known.iter().copied()))
                        .then(|| (first, Some(crate_label(&path[0]))))
                }
                _ => None,
            }
        }
        "macro_invocation" => {
            let name = cx.text(&node.child_by_field_name("macro")?);
            let (crate_name, last) = match name.rsplit_once("::") {
                Some((head, last)) => (Some(head.trim()), last.trim()),
                None => (None, name.trim()),
            };
            if !MACROS.contains(&last) {
                return None;
            }
            if let Some(head) = crate_name
                && (head != "sqlx"
                    || cx.workspace_crates.contains(head)
                    || cx.declared_mods.contains(head))
            {
                return None;
            }
            if crate_name.is_none() {
                // An unqualified `query!` is sqlx's only through a `use`.
                let scopes = visible_scopes(cx, node);
                let paths = resolve_external_paths(cx, bindings, &scopes, last, None);
                if !paths
                    .iter()
                    .any(|path| path.first().is_some_and(|first| first == "sqlx"))
                {
                    return None;
                }
            }
            let tree = named_children(node)
                .into_iter()
                .find(|child| child.kind() == "token_tree")?;
            named_children(tree)
                .into_iter()
                .find(|child| matches!(child.kind(), "string_literal" | "raw_string_literal"))
                .map(|literal| (literal, Some("sqlx")))
        }
        _ => None,
    }
}

fn named_children(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The SQL text an expression can hold: one text per value that can reach
/// it, each with [`COMPUTED`] for any piece computed at run time.
#[derive(Default)]
struct Statements {
    texts: Vec<String>,
    computed: bool,
}

/// `const` and `static` items the file declares with one string literal, by
/// name; a name declared twice is left out.
fn constants(cx: &FileCx<'_>, root: TsNode<'_>) -> HashMap<String, String> {
    let mut found: HashMap<String, Option<String>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if !matches!(node.kind(), "const_item" | "static_item") {
            continue;
        }
        let (Some(name), Some(value)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ) else {
            continue;
        };
        let value = unquote_string_literal(cx, value);
        found
            .entry(cx.text(&name).to_string())
            .and_modify(|seen| *seen = None)
            .or_insert(value);
    }
    found
        .into_iter()
        .filter_map(|(name, value)| Some((name, value?)))
        .collect()
}

/// Append the texts `expression` can be. `Some(computed)` when it is a SQL
/// string expression, with whether any part is computed; `None` when it
/// is not one, so the call states nothing.
fn piece(
    cx: &FileCx<'_>,
    constants: &HashMap<String, String>,
    expression: TsNode<'_>,
    before: usize,
    depth: usize,
    texts: &mut Vec<String>,
) -> Option<bool> {
    if depth > 4 {
        return None;
    }
    if let Some(text) = unquote_string_literal(cx, expression) {
        push(texts, text);
        return Some(false);
    }
    match expression.kind() {
        "parenthesized_expression" | "reference_expression" => {
            let last = expression.named_child_count().checked_sub(1)?;
            let inner = expression
                .child_by_field_name("value")
                .or_else(|| expression.named_child(last as _))?;
            piece(cx, constants, inner, before, depth + 1, texts)
        }
        "call_expression" => {
            // `sql.as_str()`, `sql.to_string()`, `String::from(sql)`.
            let function = expression.child_by_field_name("function")?;
            let arguments = call_arguments(expression);
            if function.kind() == "field_expression"
                && arguments.is_empty()
                && matches!(
                    cx.text(&function.child_by_field_name("field")?),
                    "as_str" | "as_ref" | "to_string" | "to_owned" | "into" | "clone"
                )
            {
                let value = function.child_by_field_name("value")?;
                return piece(cx, constants, value, before, depth + 1, texts);
            }
            if matches!(cx.text(&function), "String::from" | "str::to_string")
                && let [value] = arguments[..]
            {
                return piece(cx, constants, value, before, depth + 1, texts);
            }
            None
        }
        "binary_expression"
            if expression
                .child_by_field_name("operator")
                .is_some_and(|op| cx.text(&op) == "+") =>
        {
            let (left, right) = (
                expression.child_by_field_name("left")?,
                expression.child_by_field_name("right")?,
            );
            let mut lefts = Vec::new();
            let mut rights = Vec::new();
            let left_computed = piece(cx, constants, left, before, depth + 1, &mut lefts);
            let right_computed = piece(cx, constants, right, before, depth + 1, &mut rights);
            // One side may be anything that is a string at run time.
            if left_computed.is_none() && right_computed.is_none() {
                return None;
            }
            if lefts.is_empty() {
                lefts.push(COMPUTED.to_string());
            }
            if rights.is_empty() {
                rights.push(COMPUTED.to_string());
            }
            for l in &lefts {
                for r in &rights {
                    push(texts, format!("{l}{r}"));
                }
            }
            Some(left_computed.unwrap_or(true) | right_computed.unwrap_or(true))
        }
        "macro_invocation" => {
            let name = cx.text(&expression.child_by_field_name("macro")?);
            if !matches!(name, "format" | "std::format" | "alloc::format") {
                return None;
            }
            let tree = named_children(expression)
                .into_iter()
                .find(|child| child.kind() == "token_tree")?;
            let format = named_children(tree).into_iter().next()?;
            let text = unquote_string_literal(cx, format)?;
            let (text, computed) = placeholders(&text);
            push(texts, text);
            Some(computed)
        }
        "identifier" => {
            let name = cx.text(&expression);
            let assigned = let_values(cx, expression, name, before);
            if !assigned.is_empty() {
                let mut any = None;
                for value in assigned {
                    if let Some(computed) =
                        piece(cx, constants, value, value.start_byte(), depth + 1, texts)
                    {
                        any = Some(any.unwrap_or(false) | computed);
                    }
                }
                return any;
            }
            let text = constants.get(name)?;
            push(texts, text.clone());
            Some(false)
        }
        _ => None,
    }
}

fn push(texts: &mut Vec<String>, text: String) {
    if !texts.contains(&text) {
        texts.push(text);
    }
}

/// A `format!` string with each `{…}` placeholder as [`COMPUTED`] and `{{`,
/// `}}` as braces, and whether it had a placeholder.
fn placeholders(format: &str) -> (String, bool) {
    let mut out = String::with_capacity(format.len());
    let mut computed = false;
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                }
                out.push(COMPUTED);
                computed = true;
            }
            other => out.push(other),
        }
    }
    (out, computed)
}

/// The value of the nearest `let` binding `name` before `before` in a block
/// that encloses `at` (a binding in a block that already closed is not
/// visible).
fn let_values<'t>(cx: &FileCx<'_>, at: TsNode<'t>, name: &str, before: usize) -> Vec<TsNode<'t>> {
    let mut scope = at;
    while let Some(parent) = scope.parent() {
        scope = parent;
        if matches!(scope.kind(), "function_item" | "closure_expression") {
            break;
        }
    }
    if !matches!(scope.kind(), "function_item" | "closure_expression") {
        return Vec::new();
    }
    let mut values = Vec::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node.start_byte() >= before {
            continue;
        }
        stack.extend(named_children(node));
        if node.kind() != "let_declaration" {
            continue;
        }
        let (Some(pattern), Some(value)) = (
            node.child_by_field_name("pattern"),
            node.child_by_field_name("value"),
        ) else {
            continue;
        };
        let visible = node.parent().is_some_and(|block| {
            block.start_byte() <= at.start_byte() && at.end_byte() <= block.end_byte()
        });
        if visible && pattern.kind() == "identifier" && cx.text(&pattern) == name {
            values.push(value);
        }
    }
    values.sort_by_key(|value| value.start_byte());
    // The nearest binding before the call is the one it reads.
    values.split_off(values.len().saturating_sub(1))
}

fn gap(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    argument: TsNode<'_>,
    src: &str,
    reason: &str,
    out: &mut Extraction,
) {
    let gap = format!("gap:sql:{}@{}@{}", cx.id.repo, cx.path, call.start_byte());
    let mut expression = cx.text(&argument).to_string();
    if expression.len() > 160 {
        let mut cut = 160;
        while !expression.is_char_boundary(cut) {
            cut -= 1;
        }
        expression.truncate(cut);
        expression.push('…');
    }
    out.nodes.push(Node {
        id: gap.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "reason": reason,
            "expression": expression,
            "attempted_tiers": ["T0"],
            "prov": cx.prov_with_confidence(&call, ConfidenceTier::Gap, &format!("Gap {gap}")),
        }),
    });
    out.edges.push(Edge {
        src: src.to_string(),
        dst: gap,
        label: "READS".into(),
        props: serde_json::json!({
            "reason": reason,
            "prov": cx.prov_with_confidence(&call, ConfidenceTier::Gap, &format!("READS {src} -> SQL")),
        }),
    });
}
