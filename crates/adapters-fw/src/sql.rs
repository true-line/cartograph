//! SQL statement text as the tables it reads and writes (#2019, #2041):
//! shared by the language adapters that read literal SQL (Go
//! `database/sql` and `sqlx`; Rust `rusqlite`, `sqlx`, `postgres` and
//! `diesel`).
//!
//! `SELECT … FROM t`, `JOIN t` and comma-listed `FROM a, b` are reads;
//! `INSERT INTO t`, `UPDATE t` and `DELETE FROM t` are writes with that
//! operation, carrying the columns an `INSERT` column list or an
//! `UPDATE … SET` names. Unquoted names fold to lower case. A CTE or cursor
//! name is the statement's own, not a table. A piece computed at run time is
//! written as [`COMPUTED`]; a table reference that is or touches one is
//! reported as computed rather than named.

use core_graph::Node;
use std::collections::{BTreeMap, BTreeSet};

/// One table a statement touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    /// The table, schema-qualified as written.
    pub table: String,
    /// `None` for a read, else `insert` / `update` / `delete`.
    pub write: Option<&'static str>,
    /// The columns an `INSERT` column list or an `UPDATE … SET` names.
    pub columns: Vec<String>,
}

/// The id of the `DataEntity` (`storage: "sql"`) for `table` in `repo`.
pub fn entity_id(repo: &str, table: &str) -> String {
    format!("data:{repo}@sql:{table}")
}

/// Merge the `DataEntity` nodes several files recovered for one table: one
/// node, its columns the union, citing the first file.
pub fn merge(nodes: &mut Vec<Node>) {
    let (sql, rest): (Vec<Node>, Vec<Node>) = std::mem::take(nodes)
        .into_iter()
        .partition(|node| node.label == "DataEntity" && node.props["storage"] == "sql");
    *nodes = rest;
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
    nodes.extend(merged.into_values());
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A bare word, lower-cased.
    Word(String),
    /// A quoted identifier, as written.
    Quoted(String),
    Punct(char),
    /// A piece computed at run time, or a word it touches.
    Computed,
    Other,
}

/// Stands for a piece of a statement computed at run time.
pub const COMPUTED: char = '\u{1}';

fn tokens(sql: &str) -> Vec<Token> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == COMPUTED {
            out.push(Token::Computed);
            i += 1;
        } else if c.is_whitespace() {
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
            // A word joined to a computed piece is not a whole name.
            let joined = chars.get(i) == Some(&COMPUTED)
                || start.checked_sub(1).map(|p| chars[p]) == Some(COMPUTED);
            out.push(if joined {
                Token::Computed
            } else {
                Token::Word(chars[start..i].iter().collect::<String>().to_lowercase())
            });
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
    "materialized",
    "recursive",
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
            Token::Quoted(name) if !name.is_empty() && !name.contains(COMPUTED) => {
                parts.push(name.clone())
            }
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

/// Whether the table reference at `at` is computed: a computed piece, a
/// quoted name holding one, or a qualified name ending in one.
fn computed_slot(tokens: &[Token], mut at: usize) -> bool {
    loop {
        match tokens.get(at) {
            Some(Token::Computed) => return true,
            Some(Token::Quoted(name)) if name.contains(COMPUTED) => return true,
            Some(Token::Word(_) | Token::Quoted(_))
                if tokens.get(at + 1) == Some(&Token::Punct('.')) =>
            {
                at += 2;
            }
            _ => return false,
        }
    }
}

/// The tables one or more SQL statements read and write, and whether a
/// table reference in them is computed.
pub fn parse_with_gaps(sql: &str) -> (Vec<Access>, bool) {
    let tokens = tokens(sql);
    let unresolved = std::cell::Cell::new(false);
    let slot = |at: usize, columns: bool| {
        if computed_slot(&tokens, at) {
            unresolved.set(true);
        }
        table_name(&tokens, at, columns)
    };
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
                        if let Some((table, next)) = slot(at, true) {
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
                        if let Some((table, next)) = slot(at, false) {
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
                        if let Some((table, next)) = slot(at, false) {
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
                        while let Some((table, next)) = slot(at, false) {
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
    (out, unresolved.get())
}

/// The names `WITH name [(columns)] AS [[NOT] MATERIALIZED] (…)` defines.
fn cte_names(tokens: &[Token]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (i, token) in tokens.iter().enumerate() {
        let (Token::Word(name) | Token::Quoted(name)) = token else {
            continue;
        };
        let after_list = matches!(
            i.checked_sub(1).map(|p| &tokens[p]),
            Some(Token::Punct(','))
        ) || matches!(
            i.checked_sub(1).and_then(|p| word(tokens, p)),
            Some("with" | "recursive")
        );
        if !after_list {
            continue;
        }
        let mut at = i + 1;
        if tokens.get(at) == Some(&Token::Punct('(')) {
            let (_, next) = name_list(tokens, at);
            if next == at {
                continue;
            }
            at = next;
        }
        if word(tokens, at) != Some("as") {
            continue;
        }
        at += 1;
        if word(tokens, at) == Some("not") {
            at += 1;
        }
        if word(tokens, at) == Some("materialized") {
            at += 1;
        }
        if tokens.get(at) == Some(&Token::Punct('(')) {
            names.insert(name.clone());
        }
    }
    names
}
