//! Go SQL persistence (#2019): `database/sql` and `sqlx` calls whose
//! statement is literal SQL, as the tables they read and write.
//!
//! A file is read for SQL only when it imports `database/sql` or
//! `github.com/jmoiron/sqlx` (under any alias). A call is a statement when
//! it is a method call `x.M(…)` (never a package function such as
//! `sql.Open`) whose method is a `database/sql` statement method (`Query`,
//! `QueryRow`, `Exec`, `Prepare` and their `Context` forms) or an `sqlx`
//! one (`Get`, `Select`, `NamedExec`, `MustExec`, `Queryx`, `QueryRowx` and
//! their `Context` forms), reading the statement from the argument that
//! method takes it in. The receiver's type is not proven, so every fact is
//! InferredStrong with resolution `sql-literal`.
//!
//! The statement is a string literal or a constant the same file declares
//! with one. `SELECT … FROM t`, `JOIN t` and comma-listed `FROM a, b` are
//! reads; `INSERT INTO t`, `UPDATE t` and `DELETE FROM t` are writes with
//! that operation. A table is a `DataEntity` (`storage: "sql"`) per name,
//! unquoted names folded to lower case, carrying the columns an `INSERT`
//! column list or an `UPDATE … SET` names. A `database/sql` statement
//! method given anything else (a concatenation, `fmt.Sprintf`, a variable)
//! is a Gap naming the expression; a literal that names no table states
//! nothing.

use super::*;

/// `database/sql` statement methods and the argument holding the statement.
const SQL_METHODS: &[(&str, usize)] = &[
    ("Query", 0),
    ("QueryRow", 0),
    ("Exec", 0),
    ("Prepare", 0),
    ("QueryContext", 1),
    ("QueryRowContext", 1),
    ("ExecContext", 1),
    ("PrepareContext", 1),
];

/// `sqlx` statement methods and the argument holding the statement.
const SQLX_METHODS: &[(&str, usize)] = &[
    ("Get", 1),
    ("Select", 1),
    ("GetContext", 2),
    ("SelectContext", 2),
    ("NamedExec", 0),
    ("NamedExecContext", 1),
    ("NamedQuery", 0),
    ("NamedQueryContext", 1),
    ("MustExec", 0),
    ("MustExecContext", 1),
    ("Queryx", 0),
    ("QueryRowx", 0),
    ("QueryxContext", 1),
    ("QueryRowxContext", 1),
];

const IMPORTS: &[(&str, &str)] = &[
    ("database/sql", "database/sql"),
    ("github.com/jmoiron/sqlx", "sqlx"),
];

/// One table a statement touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Access {
    pub(super) table: String,
    /// `None` for a read, else `insert` / `update` / `delete`.
    pub(super) write: Option<&'static str>,
    pub(super) columns: Vec<String>,
}

pub(super) fn entity_id(repo: &str, table: &str) -> String {
    format!("data:{repo}@sql:{table}")
}

/// Append the tables the file's SQL statements read and write.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &HashMap<usize, String>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let framework = IMPORTS
        .iter()
        .filter(|(path, _)| bindings.values().any(|binding| binding.path == *path))
        .map(|(_, framework)| *framework)
        .next_back();
    let Some(framework) = framework else {
        return;
    };
    let sqlx = framework == "sqlx";
    let constants = constants(cx, root);
    let mut stack = vec![root];
    // (src, table, label) -> (operations, cited call, from one literal)
    #[allow(clippy::type_complexity)]
    let mut accesses: BTreeMap<
        (String, String, &'static str),
        (BTreeSet<&'static str>, TsNode<'_>, bool),
    > = BTreeMap::new();
    let mut tables: BTreeMap<String, (BTreeSet<String>, TsNode<'_>)> = BTreeMap::new();
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        if node.kind() != "call_expression" {
            continue;
        }
        let Some((index, core)) = statement_method(cx, bindings, node, sqlx) else {
            continue;
        };
        let Some(argument) = call_arguments(node).get(index).copied() else {
            continue;
        };
        let owner = enclosing_function(node, functions);
        let src = owner.unwrap_or_else(|| file_id(cx.id.repo, cx.path));
        let mut statements = Statements::default();
        fragments(
            cx,
            &constants,
            argument,
            node.start_byte(),
            0,
            &mut statements,
        );
        let found: Vec<Access> = statements
            .texts
            .iter()
            .flat_map(|text| parse(text))
            .collect();
        if found.is_empty() {
            if core && statements.computed {
                gap(cx, node, argument, &src, out);
            }
            continue;
        }
        let exact = !statements.computed && statements.texts.len() == 1;
        for access in found {
            let entry = tables
                .entry(access.table.clone())
                .or_insert_with(|| (BTreeSet::new(), node));
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
    for (table, (columns, at)) in tables {
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
                "language": "go",
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

/// The statement argument index of `call`, and whether its method is a
/// `database/sql` one.
fn statement_method(
    cx: &FileCx<'_>,
    bindings: &HashMap<String, ImportBinding>,
    call: TsNode<'_>,
    sqlx: bool,
) -> Option<(usize, bool)> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "selector_expression" {
        return None;
    }
    let operand = function.child_by_field_name("operand")?;
    // A package function (`sql.Open`, `sqlx.Connect`) is no statement.
    if operand.kind() == "identifier" && bindings.contains_key(cx.text(&operand)) {
        return None;
    }
    let method = cx.text(&function.child_by_field_name("field")?);
    if let Some((_, index)) = SQL_METHODS.iter().find(|(name, _)| *name == method) {
        return Some((*index, true));
    }
    if sqlx {
        return SQLX_METHODS
            .iter()
            .find(|(name, _)| *name == method)
            .map(|(_, index)| (*index, false));
    }
    None
}

/// Top-level and function-level `const name = "…"` declarations with one
/// literal value, by name; a name declared twice is left out.
fn constants(cx: &FileCx<'_>, root: TsNode<'_>) -> HashMap<String, String> {
    let mut found: HashMap<String, Option<String>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        if node.kind() != "const_spec" {
            continue;
        }
        let mut cursor = node.walk();
        let names: Vec<_> = node.children_by_field_name("name", &mut cursor).collect();
        let value = node
            .child_by_field_name("value")
            .and_then(|list| {
                let mut cursor = list.walk();
                let values: Vec<_> = list.named_children(&mut cursor).collect();
                (values.len() == 1).then(|| values[0])
            })
            .and_then(|value| literal_string(cx, value));
        if let [name] = names[..] {
            found
                .entry(cx.text(&name).to_string())
                .and_modify(|seen| *seen = None)
                .or_insert(value);
        }
    }
    found
        .into_iter()
        .filter_map(|(name, value)| Some((name, value?)))
        .collect()
}

/// The SQL text an expression can hold: one text per assignment that can
/// reach it, each with a `?` for any piece computed at run time.
#[derive(Default)]
struct Statements {
    texts: Vec<String>,
    computed: bool,
}

/// The pieces of `expression` as SQL text, followed through `+`,
/// `fmt.Sprintf`'s format and identifiers the enclosing function assigns
/// before `before` (or the file's constants).
fn fragments(
    cx: &FileCx<'_>,
    constants: &HashMap<String, String>,
    expression: TsNode<'_>,
    before: usize,
    depth: usize,
    out: &mut Statements,
) {
    let mut texts = Vec::new();
    let computed = piece(cx, constants, expression, before, depth, &mut texts);
    out.computed |= computed;
    for text in texts {
        if !out.texts.contains(&text) {
            out.texts.push(text);
        }
    }
}

/// Append the texts `expression` can be; whether any part is computed.
fn piece(
    cx: &FileCx<'_>,
    constants: &HashMap<String, String>,
    expression: TsNode<'_>,
    before: usize,
    depth: usize,
    texts: &mut Vec<String>,
) -> bool {
    if depth > 4 {
        texts.push("?".into());
        return true;
    }
    if let Some(text) = literal_string(cx, expression) {
        texts.push(text);
        return false;
    }
    match expression.kind() {
        "parenthesized_expression" => {
            if let Some(inner) = expression.named_child(0) {
                return piece(cx, constants, inner, before, depth + 1, texts);
            }
        }
        "binary_expression"
            if expression
                .child_by_field_name("operator")
                .is_some_and(|op| cx.text(&op) == "+") =>
        {
            let (Some(left), Some(right)) = (
                expression.child_by_field_name("left"),
                expression.child_by_field_name("right"),
            ) else {
                texts.push("?".into());
                return true;
            };
            let mut lefts = Vec::new();
            let mut rights = Vec::new();
            let computed = piece(cx, constants, left, before, depth + 1, &mut lefts)
                | piece(cx, constants, right, before, depth + 1, &mut rights);
            for l in &lefts {
                for r in &rights {
                    texts.push(format!("{l} {r}"));
                }
            }
            return computed;
        }
        "call_expression" => {
            let sprintf = expression
                .child_by_field_name("function")
                .is_some_and(|f| matches!(cx.text(&f), "fmt.Sprintf"));
            if let (true, Some(format)) = (sprintf, call_arguments(expression).first())
                && let Some(text) = literal_string(cx, *format)
            {
                texts.push(text);
                return true;
            }
        }
        "identifier" => {
            let name = cx.text(&expression);
            let assigned = assignments(cx, expression, name, before);
            if !assigned.is_empty() {
                let mut computed = false;
                for value in assigned {
                    computed |= piece(cx, constants, value, value.start_byte(), depth + 1, texts);
                }
                return computed;
            }
            if let Some(text) = constants.get(name) {
                texts.push(text.clone());
                return false;
            }
        }
        _ => {}
    }
    texts.push("?".into());
    true
}

/// The values the function around `at` assigns to `name` before `before`
/// (`name := v`, `var name = v`, `name = v`, `name += v`).
fn assignments<'t>(cx: &FileCx<'_>, at: TsNode<'t>, name: &str, before: usize) -> Vec<TsNode<'t>> {
    let mut scope = at;
    while let Some(parent) = scope.parent() {
        scope = parent;
        if matches!(
            scope.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) {
            break;
        }
    }
    if !matches!(
        scope.kind(),
        "function_declaration" | "method_declaration" | "func_literal"
    ) {
        return Vec::new();
    }
    let mut values = Vec::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node.start_byte() >= before {
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        let (left, right) = match node.kind() {
            "short_var_declaration" | "assignment_statement" => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ),
            "var_spec" => (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ),
            _ => continue,
        };
        let (Some(left), Some(right)) = (left, right) else {
            continue;
        };
        let mut cursor = left.walk();
        let lefts: Vec<_> = if left.kind() == "identifier" {
            vec![left]
        } else {
            left.named_children(&mut cursor).collect()
        };
        let mut cursor = right.walk();
        let rights: Vec<_> = if right.kind() == "expression_list" {
            right.named_children(&mut cursor).collect()
        } else {
            vec![right]
        };
        if lefts.len() != rights.len() {
            continue;
        }
        for (l, r) in lefts.iter().zip(rights) {
            if cx.text(l) == name {
                values.push(r);
            }
        }
    }
    values.sort_by_key(|value| value.start_byte());
    values
}

fn gap(cx: &FileCx<'_>, call: TsNode<'_>, argument: TsNode<'_>, src: &str, out: &mut Extraction) {
    let reason = "SQL statement is computed";
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

/// Merge the `DataEntity` nodes several files recovered for one table: one
/// node, its columns the union, citing the first file.
pub(super) fn merge(out: &mut Extraction) {
    let (sql, rest): (Vec<Node>, Vec<Node>) = std::mem::take(&mut out.nodes)
        .into_iter()
        .partition(|node| node.label == "DataEntity" && node.props["storage"] == "sql");
    out.nodes = rest;
    let mut merged: BTreeMap<String, Node> = BTreeMap::new();
    for node in sql {
        let Some(kept) = merged.get_mut(&node.id) else {
            merged.insert(node.id.clone(), node);
            continue;
        };
        let columns = node.props["columns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(target) = kept.props["columns"].as_array_mut() {
            for column in columns {
                if !target.contains(&column) {
                    target.push(column);
                }
            }
            target.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        }
    }
    out.nodes.extend(merged.into_values());
}

// ---------------------------------------------------------------------------
// SQL reading
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A bare word, lower-cased.
    Word(String),
    /// A quoted identifier, as written.
    Quoted(String),
    Punct(char),
    Other,
}

fn tokens(sql: &str) -> Vec<Token> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push(Token::Other);
        } else if matches!(c, '"' | '`' | '[') {
            let close = if c == '[' { ']' } else { c };
            let start = i + 1;
            i = start;
            while i < chars.len() && chars[i] != close {
                i += 1;
            }
            out.push(Token::Quoted(
                chars[start..i.min(chars.len())].iter().collect(),
            ));
            i += 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$')) {
                i += 1;
            }
            out.push(Token::Word(
                chars[start..i].iter().collect::<String>().to_lowercase(),
            ));
        } else if matches!(c, '(' | ')' | ',' | '.' | ';' | '=') {
            out.push(Token::Punct(c));
            i += 1;
        } else {
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !chars[i].is_alphabetic()
                && !matches!(
                    chars[i],
                    '(' | ')' | ',' | '.' | ';' | '=' | '\'' | '"' | '`' | '[' | '_'
                )
            {
                i += 1;
            }
            out.push(Token::Other);
        }
    }
    out
}

/// Words that never name a table or a function, so a `(` after one opens a
/// subquery or a list, not a call.
const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "join",
    "where",
    "and",
    "or",
    "not",
    "in",
    "exists",
    "as",
    "on",
    "into",
    "values",
    "with",
    "using",
    "lateral",
    "any",
    "all",
    "some",
    "union",
    "intersect",
    "except",
    "set",
    "returning",
    "then",
    "else",
    "when",
    "case",
    "is",
    "by",
    "having",
    "limit",
    "offset",
];

fn word(tokens: &[Token], at: usize) -> Option<&str> {
    match tokens.get(at) {
        Some(Token::Word(word)) => Some(word),
        _ => None,
    }
}

/// The (possibly schema-qualified) table name at `at`, and the index after it.
fn table_name(tokens: &[Token], mut at: usize, columns: bool) -> Option<(String, usize)> {
    let mut parts = Vec::new();
    loop {
        match tokens.get(at)? {
            Token::Word(word) if !KEYWORDS.contains(&word.as_str()) => parts.push(word.clone()),
            Token::Quoted(name) if !name.is_empty() => parts.push(name.clone()),
            _ => return None,
        }
        at += 1;
        if tokens.get(at) == Some(&Token::Punct('.')) {
            at += 1;
            continue;
        }
        break;
    }
    // A table function (`FROM unnest(…)`) is no table; `INSERT INTO t (…)`
    // lists columns.
    if !columns && tokens.get(at) == Some(&Token::Punct('(')) {
        return None;
    }
    Some((parts.join("."), at))
}

/// Skip an optional `[AS] alias` after a table.
fn skip_alias(tokens: &[Token], mut at: usize) -> usize {
    if word(tokens, at) == Some("as") {
        at += 1;
    }
    match tokens.get(at) {
        Some(Token::Word(word))
            if !KEYWORDS.contains(&word.as_str()) && !CLAUSES.contains(&word.as_str()) =>
        {
            at + 1
        }
        Some(Token::Quoted(_)) => at + 1,
        _ => at,
    }
}

/// Words that start the next clause after a table reference.
const CLAUSES: &[&str] = &[
    "left", "right", "inner", "outer", "full", "cross", "natural", "group", "order", "window",
    "for", "default", "select", "fetch", "do", "conflict", "only",
];

/// The names in a parenthesised list at `at` (`(a, b, c)`), and the index
/// after it.
fn name_list(tokens: &[Token], at: usize) -> (Vec<String>, usize) {
    if tokens.get(at) != Some(&Token::Punct('(')) {
        return (Vec::new(), at);
    }
    let mut names = Vec::new();
    let mut i = at + 1;
    while let Some(token) = tokens.get(i) {
        match token {
            Token::Punct(')') => return (names, i + 1),
            Token::Word(name) | Token::Quoted(name) => names.push(name.clone()),
            Token::Punct(',') => {}
            _ => return (Vec::new(), at),
        }
        i += 1;
    }
    (Vec::new(), at)
}

/// The columns an `UPDATE … SET a = …, b = …` assigns.
fn set_columns(tokens: &[Token], at: usize) -> Vec<String> {
    let mut columns = Vec::new();
    let mut depth = 0usize;
    let mut i = at;
    let mut expect_column = true;
    while let Some(token) = tokens.get(i) {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            Token::Punct(',') if depth == 0 => expect_column = true,
            Token::Word(word)
                if depth == 0 && matches!(word.as_str(), "where" | "from" | "returning") =>
            {
                break;
            }
            Token::Word(name) | Token::Quoted(name) if expect_column && depth == 0 => {
                // `t.col = …` names `col`.
                let (name, next) = if tokens.get(i + 1) == Some(&Token::Punct('.')) {
                    match tokens.get(i + 2) {
                        Some(Token::Word(column) | Token::Quoted(column)) => {
                            (column.clone(), i + 3)
                        }
                        _ => (name.clone(), i + 1),
                    }
                } else {
                    (name.clone(), i + 1)
                };
                if tokens.get(next) == Some(&Token::Punct('=')) {
                    columns.push(name);
                }
                expect_column = false;
                i = next;
                continue;
            }
            Token::Punct(';') => break,
            _ => {}
        }
        i += 1;
    }
    columns
}

/// The tables one or more SQL statements read and write.
pub(super) fn parse(sql: &str) -> Vec<Access> {
    let tokens = tokens(sql);
    let mut out: Vec<Access> = Vec::new();
    let mut push = |access: Access| {
        if let Some(existing) = out
            .iter_mut()
            .find(|seen| seen.table == access.table && seen.write == access.write)
        {
            for column in access.columns {
                if !existing.columns.contains(&column) {
                    existing.columns.push(column);
                }
            }
        } else {
            out.push(access);
        }
    };
    // Whether each open `(` is a call's (a function's arguments).
    let mut parens: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let in_call = parens.last().copied().unwrap_or(false);
        match &tokens[i] {
            Token::Punct('(') => {
                let call = match i.checked_sub(1).map(|p| &tokens[p]) {
                    Some(Token::Word(word)) => !KEYWORDS.contains(&word.as_str()),
                    Some(Token::Quoted(_)) => true,
                    _ => false,
                };
                parens.push(call);
            }
            Token::Punct(')') => {
                parens.pop();
            }
            Token::Word(keyword) if !in_call => {
                let previous = i.checked_sub(1).and_then(|p| word(&tokens, p));
                match keyword.as_str() {
                    "insert" | "replace" => {
                        let mut at = i + 1;
                        if word(&tokens, at) == Some("or") {
                            at += 2;
                        }
                        if word(&tokens, at) == Some("ignore") {
                            at += 1;
                        }
                        if word(&tokens, at) == Some("into") {
                            at += 1;
                        }
                        if let Some((table, next)) = table_name(&tokens, at, true) {
                            let next = skip_alias(&tokens, next);
                            let (columns, next) = name_list(&tokens, next);
                            push(Access {
                                table,
                                write: Some("insert"),
                                columns,
                            });
                            i = next;
                            continue;
                        }
                    }
                    // `ON CONFLICT … DO UPDATE`, `FOR UPDATE`, `ON DUPLICATE
                    // KEY UPDATE` update no other table.
                    "update" if !matches!(previous, Some("do" | "for" | "key")) => {
                        let mut at = i + 1;
                        if word(&tokens, at) == Some("only") {
                            at += 1;
                        }
                        if let Some((table, next)) = table_name(&tokens, at, false) {
                            let next = skip_alias(&tokens, next);
                            let columns = if word(&tokens, next) == Some("set") {
                                set_columns(&tokens, next + 1)
                            } else {
                                Vec::new()
                            };
                            push(Access {
                                table,
                                write: Some("update"),
                                columns,
                            });
                            i = next;
                            continue;
                        }
                    }
                    "delete" if word(&tokens, i + 1) == Some("from") => {
                        let mut at = i + 2;
                        if word(&tokens, at) == Some("only") {
                            at += 1;
                        }
                        if let Some((table, next)) = table_name(&tokens, at, false) {
                            push(Access {
                                table,
                                write: Some("delete"),
                                columns: Vec::new(),
                            });
                            i = next;
                            continue;
                        }
                    }
                    // `IS DISTINCT FROM` compares, and `FETCH NEXT FROM c`
                    // reads a cursor; neither names a table.
                    "from" | "join"
                        if !matches!(
                            previous,
                            Some(
                                "distinct"
                                    | "fetch"
                                    | "move"
                                    | "next"
                                    | "prior"
                                    | "first"
                                    | "last"
                                    | "forward"
                                    | "backward"
                                    | "all"
                            )
                        ) =>
                    {
                        let mut at = i + 1;
                        if word(&tokens, at) == Some("only") {
                            at += 1;
                        }
                        while let Some((table, next)) = table_name(&tokens, at, false) {
                            push(Access {
                                table,
                                write: None,
                                columns: Vec::new(),
                            });
                            let next = skip_alias(&tokens, next);
                            if keyword == "from" && tokens.get(next) == Some(&Token::Punct(',')) {
                                at = next + 1;
                                continue;
                            }
                            at = next;
                            break;
                        }
                        i = at.max(i + 1);
                        continue;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    // A CTE or cursor name is the statement's own, not a table.
    let mut ctes = cte_names(&tokens);
    for (i, token) in tokens.iter().enumerate() {
        if *token == Token::Word("declare".into())
            && let Some(Token::Word(name) | Token::Quoted(name)) = tokens.get(i + 1)
        {
            ctes.insert(name.clone());
        }
    }
    out.retain(|access| !ctes.contains(&access.table));
    out
}

/// The names `WITH name AS (…)` defines.
fn cte_names(tokens: &[Token]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (i, token) in tokens.iter().enumerate() {
        let (Token::Word(name) | Token::Quoted(name)) = token else {
            continue;
        };
        let defines = word(tokens, i + 1) == Some("as")
            && tokens.get(i + 2) == Some(&Token::Punct('('))
            && matches!(
                i.checked_sub(1).map(|p| &tokens[p]),
                Some(Token::Punct(',')) | Some(Token::Word(_))
            )
            && (word(tokens, i.wrapping_sub(1)) == Some("with")
                || word(tokens, i.wrapping_sub(1)) == Some("recursive")
                || tokens.get(i.wrapping_sub(1)) == Some(&Token::Punct(',')));
        if defines {
            names.insert(name.clone());
        }
    }
    names
}
