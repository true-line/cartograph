//! Observed infrastructure from Python (#734, AC-0318): the `pypi` entries
//! of the observed-client registry (boto3, redis, smtplib, SQLAlchemy …)
//! become the same receipt-backed `Resource` / `USES` / Gap shape the
//! TypeScript producer emits ([`adapters_fw::observed`]).
//!
//! A call proves its client only through a module-level import the file
//! never rebinds: `import boto3` → `boto3.client("s3")`, `from redis import
//! Redis as R` → `R(…)`, `import a.b` → `a.b.X(…)`. Any other write of the
//! bound name (assignment, `def`, `class`, a loop, `with`/`except` alias,
//! parameter, another import, `global`/`nonlocal`, `del`, a `case` capture)
//! anywhere in the file voids it: fail closed, never a guess.
//!
//! Identity: a string literal names the instance (a URL keeps scheme and
//! authority only); `os.environ["X"]`, `os.environ.get("X")` and
//! `os.getenv("X")` record the variable name, never a value; a module-level
//! name assigned once is followed one step; anything else is a Gap naming
//! the site.

use adapters_fw::events::{ClientForm, Ecosystem, IdentityArg, IdentityExpr, Registries};
use adapters_fw::observed::{ObservedSite, ObservedSource, assemble};
use core_graph::{Edge, Node};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use crate::{
    ExtractError, FileCx, SourceId, collect_python_files, enclosing_function_node, literal_string,
    qualified_function_name, symbol_id,
};

const EXTRACTOR_ID: &str = "t0.observed-infra-python";

/// What a module-level import binds a name to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Bound {
    /// `import a.b as n` → `a.b`; `import a.b` binds `a` → `a`.
    Module(String),
    /// `from m import x as n` → (`m`, `x`).
    Symbol(String, String),
}

type MemberPath = Vec<Option<String>>;

/// Names a file binds: what each import binds a name to, and how often
/// anything else writes it.
struct Scope {
    /// Every binding imports give a name (`None` = one no registry module
    /// names, e.g. a relative import), and whether one is at module level.
    imports: HashMap<String, (HashSet<Option<Bound>>, bool)>,
    /// Writes other than imports.
    writes: HashMap<String, usize>,
    /// Module-level `name = <expr>` assignments (the value's span),
    /// followed one step.
    assigned: HashMap<String, (usize, usize)>,
    /// Member paths written under a name (`boto3.client = …`,
    /// `redis.Redis.from_url = …`, `ns["k"] = …`); a `None` segment is
    /// any member (`setattr(ns, …)`, a computed key).
    members: HashMap<String, HashSet<MemberPath>>,
}

impl Scope {
    /// The import a name proves: every import of it agrees, one is at
    /// module level, and nothing else in the file writes it.
    fn proven(&self, name: &str) -> Option<&Bound> {
        let (bindings, module_level) = self.imports.get(name)?;
        match (bindings.len(), bindings.iter().next()) {
            (1, Some(Some(bound))) if *module_level && !self.writes.contains_key(name) => {
                Some(bound)
            }
            _ => None,
        }
    }

    /// Whether the file writes the member path a call reads under `name`,
    /// an ancestor of it, or anything beneath it.
    fn overwritten(&self, name: &str, read: &[String]) -> bool {
        self.members.get(name).is_some_and(|written| {
            written.iter().any(|path| {
                path.iter()
                    .zip(read)
                    .all(|(segment, read)| segment.as_ref().is_none_or(|s| s == read))
            })
        })
    }
}

/// The root name and member path of `N.a["k"].b`; `None` when the chain
/// does not start at a name.
fn member_path(cx: &FileCx, mut node: TsNode) -> Option<(String, MemberPath)> {
    let mut path = Vec::new();
    loop {
        match node.kind() {
            "identifier" => {
                path.reverse();
                return Some((cx.text(&node).to_string(), path));
            }
            "attribute" => {
                path.push(Some(
                    cx.text(&node.child_by_field_name("attribute")?).to_string(),
                ));
                node = node.child_by_field_name("object")?;
            }
            "subscript" => {
                let mut walk = node.walk();
                let keys: Vec<_> = node
                    .children_by_field_name("subscript", &mut walk)
                    .collect();
                path.push(match keys.as_slice() {
                    [key] => literal_string(cx, *key),
                    _ => None,
                });
                node = node.child_by_field_name("value")?;
            }
            _ => return None,
        }
    }
}

/// Record every member path a write target (or each target of a tuple,
/// list or `del`) reaches.
fn member_writes(cx: &FileCx, target: TsNode, out: &mut HashMap<String, HashSet<MemberPath>>) {
    match target.kind() {
        "attribute" | "subscript" => {
            if let Some((name, path)) = member_path(cx, target) {
                out.entry(name).or_default().insert(path);
            }
        }
        _ => {
            let mut walk = target.walk();
            for child in target.named_children(&mut walk) {
                member_writes(cx, child, out);
            }
        }
    }
}

fn names_in<'t>(pattern: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    match pattern.kind() {
        "identifier" => out.push(pattern),
        // A member or item write leaves the bound name intact.
        "attribute" | "subscript" => {}
        _ => {
            let mut walk = pattern.walk();
            for child in pattern.named_children(&mut walk) {
                names_in(child, out);
            }
        }
    }
}

/// The names one `import` / `from … import` statement binds, with what
/// each is bound to.
fn import_names(cx: &FileCx, node: TsNode) -> Vec<(String, Option<Bound>)> {
    let mut out = Vec::new();
    let mut walk = node.walk();
    match node.kind() {
        "import_statement" => {
            for child in node.children_by_field_name("name", &mut walk) {
                match child.kind() {
                    "dotted_name" => {
                        let text = cx.text(&child);
                        let head = text.split('.').next().unwrap_or(text).trim();
                        out.push((head.to_string(), Some(Bound::Module(head.to_string()))));
                    }
                    "aliased_import" => {
                        if let (Some(name), Some(alias)) = (
                            child.child_by_field_name("name"),
                            child.child_by_field_name("alias"),
                        ) {
                            out.push((
                                cx.text(&alias).to_string(),
                                Some(Bound::Module(cx.text(&name).to_string())),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        "import_from_statement" => {
            let module = node
                .child_by_field_name("module_name")
                .filter(|module| module.kind() == "dotted_name")
                .map(|module| cx.text(&module).to_string());
            for child in node.children_by_field_name("name", &mut walk) {
                let (name, alias) = match child.kind() {
                    "dotted_name" => (child, child),
                    "aliased_import" => match (
                        child.child_by_field_name("name"),
                        child.child_by_field_name("alias"),
                    ) {
                        (Some(name), Some(alias)) => (name, alias),
                        _ => continue,
                    },
                    _ => continue,
                };
                // A relative import names no registry module.
                let bound = module
                    .as_ref()
                    .map(|module| Bound::Symbol(module.clone(), cx.text(&name).to_string()));
                out.push((cx.text(&alias).to_string(), bound));
            }
            let mut walk = node.walk();
            if node
                .named_children(&mut walk)
                .any(|child| child.kind() == "wildcard_import")
            {
                // `from m import *` may bind any name: every import is void.
                out.push(("*".into(), None));
            }
        }
        _ => {}
    }
    out
}

/// Every name a binding construct writes.
fn written_names<'t>(node: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    let field = |name: &str| node.child_by_field_name(name);
    match node.kind() {
        "assignment" | "augmented_assignment" => {
            if let Some(left) = field("left") {
                names_in(left, out);
            }
        }
        "for_statement" | "for_in_clause" => {
            if let Some(left) = field("left") {
                names_in(left, out);
            }
        }
        "named_expression" => {
            if let Some(name) = field("name") {
                out.push(name);
            }
        }
        "as_pattern_target" | "delete_statement" | "global_statement" | "nonlocal_statement"
        | "case_pattern" => names_in(node, out),
        "function_definition" | "class_definition" => {
            if let Some(name) = field("name") {
                out.push(name);
            }
        }
        "parameters" | "lambda_parameters" => {
            let mut walk = node.walk();
            for parameter in node.named_children(&mut walk) {
                let name = match parameter.kind() {
                    "identifier" => Some(parameter),
                    "default_parameter" | "typed_default_parameter" => {
                        parameter.child_by_field_name("name")
                    }
                    "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {
                        let mut walk = parameter.walk();
                        parameter
                            .named_children(&mut walk)
                            .find(|child| child.kind() == "identifier")
                    }
                    _ => None,
                };
                out.extend(name);
            }
        }
        _ => {}
    }
}

fn collect_scope(cx: &FileCx, root: TsNode) -> Scope {
    let mut scope = Scope {
        imports: HashMap::new(),
        writes: HashMap::new(),
        assigned: HashMap::new(),
        members: HashMap::new(),
    };
    let mut wildcard = false;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            // Only a statement directly under the module proves a binding.
            let module_level = node.parent().is_some_and(|parent| parent.id() == root.id());
            for (name, bound) in import_names(cx, node) {
                if name == "*" {
                    wildcard = true;
                    continue;
                }
                let (bindings, at_module) = scope.imports.entry(name).or_default();
                bindings.insert(bound);
                *at_module |= module_level;
            }
            continue;
        }
        let target = match node.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                node.child_by_field_name("left")
            }
            "delete_statement" => Some(node),
            // `setattr(x, …)` / `delattr(x, …)` may write any member of `x`:
            // a write at `x` overlaps every path beneath it.
            "call" => {
                if let Some(callee) = node.child_by_field_name("function")
                    && matches!(cx.text(&callee), "setattr" | "delattr")
                    && let Some(object) = node
                        .child_by_field_name("arguments")
                        .and_then(|args| positional(args).first().copied())
                    && let Some((name, path)) = member_path(cx, object)
                {
                    scope.members.entry(name).or_default().insert(path);
                }
                None
            }
            _ => None,
        };
        if let Some(target) = target {
            member_writes(cx, target, &mut scope.members);
        }
        let mut names = Vec::new();
        written_names(node, &mut names);
        for name in names {
            *scope.writes.entry(cx.text(&name).to_string()).or_default() += 1;
        }
        if node.kind() == "assignment"
            && node
                .parent()
                .and_then(|statement| statement.parent())
                .is_some_and(|parent| parent.id() == root.id())
            && let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            )
            && left.kind() == "identifier"
        {
            scope.assigned.insert(
                cx.text(&left).to_string(),
                (right.start_byte(), right.end_byte()),
            );
        }
    }
    if wildcard {
        scope.imports.clear();
    }
    scope
}

/// The `(module, attribute)` a callee names through a proven import:
/// `X` → its binding; `n.a.X` → (`<n's module>.a`, `X`).
fn callee_export(cx: &FileCx, callee: TsNode, scope: &Scope) -> Option<(String, String)> {
    match callee.kind() {
        "identifier" => match scope.proven(cx.text(&callee))? {
            Bound::Symbol(module, name) => Some((module.clone(), name.clone())),
            Bound::Module(_) => None,
        },
        "attribute" => {
            let attribute = cx
                .text(&callee.child_by_field_name("attribute")?)
                .to_string();
            let mut object = callee.child_by_field_name("object")?;
            let mut path = Vec::new();
            while object.kind() == "attribute" {
                path.push(
                    cx.text(&object.child_by_field_name("attribute")?)
                        .to_string(),
                );
                object = object.child_by_field_name("object")?;
            }
            // A write of the member path this call reads, an ancestor of
            // it or anything beneath it voids it.
            let read: Vec<String> = path.iter().rev().chain([&attribute]).cloned().collect();
            if object.kind() != "identifier" || scope.overwritten(cx.text(&object), &read) {
                return None;
            }
            let mut module = match scope.proven(cx.text(&object))? {
                Bound::Module(module) => module.clone(),
                // `from a import b` then `b.X` is attribute `X` of `a.b`.
                Bound::Symbol(module, name) => format!("{module}.{name}"),
            };
            for piece in path.iter().rev() {
                module.push('.');
                module.push_str(piece);
            }
            Some((module, attribute))
        }
        _ => None,
    }
}

/// Positional (non-keyword) arguments of a call.
fn positional<'t>(args: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut walk = args.walk();
    args.named_children(&mut walk)
        .filter(|arg| !matches!(arg.kind(), "keyword_argument" | "comment"))
        .collect()
}

fn keyword<'t>(cx: &FileCx, args: TsNode<'t>, key: &str) -> Option<TsNode<'t>> {
    let mut walk = args.walk();
    args.named_children(&mut walk)
        .filter(|arg| arg.kind() == "keyword_argument")
        .find(|arg| {
            arg.child_by_field_name("name")
                .is_some_and(|name| cx.text(&name) == key)
        })
        .and_then(|arg| arg.child_by_field_name("value"))
}

/// The env variable an `os.environ[...]`, `os.environ.get(...)` or
/// `os.getenv(...)` read names, through a proven `os` import.
fn env_key(cx: &FileCx, node: TsNode, scope: &Scope) -> Option<String> {
    let names = |target: TsNode, attribute: &str| {
        callee_export(cx, target, scope).is_some_and(|(m, a)| m == "os" && a == attribute)
    };
    match node.kind() {
        "subscript" => {
            let value = node.child_by_field_name("value")?;
            let mut walk = node.walk();
            let index: Vec<_> = node
                .children_by_field_name("subscript", &mut walk)
                .collect();
            match index.as_slice() {
                [key] if names(value, "environ") => literal_string(cx, *key),
                _ => None,
            }
        }
        "call" => {
            let callee = node.child_by_field_name("function")?;
            // `os.getenv(k)`, or `.get(k)` on a proven `os.environ`.
            let environ_get = callee.kind() == "attribute"
                && callee
                    .child_by_field_name("attribute")
                    .is_some_and(|a| cx.text(&a) == "get")
                && callee
                    .child_by_field_name("object")
                    .is_some_and(|object| names(object, "environ"));
            let reads_env = environ_get || names(callee, "getenv");
            let args = node.child_by_field_name("arguments")?;
            match (reads_env, positional(args).first()) {
                (true, Some(key)) => literal_string(cx, *key),
                _ => None,
            }
        }
        _ => None,
    }
    .filter(|key| !key.is_empty())
}

fn classify(cx: &FileCx, root: TsNode, node: TsNode, scope: &Scope, follow: bool) -> IdentityExpr {
    if let Some(value) = literal_string(cx, node) {
        return IdentityExpr::Literal(value);
    }
    if let Some(key) = env_key(cx, node, scope) {
        return IdentityExpr::EnvRef(key);
    }
    let text = cx.text(&node);
    // A module-level name assigned once, and never written otherwise.
    if follow
        && node.kind() == "identifier"
        && scope.writes.get(text) == Some(&1)
        && !scope.imports.contains_key(text)
        && let Some(&(start, end)) = scope.assigned.get(text)
        && let Some(right) = root.descendant_for_byte_range(start, end)
    {
        return match classify(cx, root, right, scope, false) {
            IdentityExpr::Computed(_) => IdentityExpr::Computed(text.to_string()),
            known => known,
        };
    }
    IdentityExpr::Computed(text.to_string())
}

fn extract_file(
    source: &[u8],
    path: &str,
    id: &SourceId,
    registries: &Registries,
    sites: &mut Vec<ObservedSite>,
) -> Result<(), ExtractError> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let cx = FileCx { source, path, id };
    let scope = collect_scope(&cx, root);
    if scope.imports.is_empty() {
        return Ok(());
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "call" {
            continue;
        }
        let (Some(callee), Some(args)) = (
            node.child_by_field_name("function"),
            node.child_by_field_name("arguments"),
        ) else {
            continue;
        };
        let Some((module, export)) = callee_export(&cx, callee, &scope) else {
            continue;
        };
        let first = positional(args).first().copied();
        let Some(client) = registries.clients.iter().find(|c| {
            c.ecosystem == Ecosystem::Pypi
                && c.form == ClientForm::Call
                && c.module == module
                && c.export == export
                && c.select.as_ref().is_none_or(|select| {
                    first.and_then(|arg| literal_string(&cx, arg)).as_deref()
                        == Some(select.first.as_str())
                })
        }) else {
            continue;
        };
        let identity = match &client.identity {
            None => None,
            Some(IdentityArg::First) => first,
            Some(IdentityArg::Key(key)) => keyword(&cx, args, key),
        }
        .map(|value| classify(&cx, root, value, &scope, true));
        let symbol = enclosing_function_node(node)
            .and_then(|function| qualified_function_name(&cx, function))
            .map(|name| symbol_id(id.repo, path, &name));
        sites.push(ObservedSite {
            family: client.family,
            provider: client.provider.clone(),
            module: Some(client.module.clone()),
            identity,
            registry: Some(client.registry.clone()),
            symbol,
            path: path.to_string(),
            byte_start: node.start_byte() as u64,
            byte_end: node.end_byte() as u64,
        });
    }
    Ok(())
}

/// Observed infrastructure for every Python source under `root`.
pub fn extract_dir(
    root: &Path,
    id: &SourceId,
    registries: &Registries,
) -> Result<(Vec<Node>, Vec<Edge>), ExtractError> {
    let mut files = Vec::new();
    collect_python_files(root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut sites = Vec::new();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, registries, &mut sites)?;
    }
    Ok(assemble(
        &ObservedSource {
            repo: id.repo,
            commit: id.commit,
            extractor: EXTRACTOR_ID,
        },
        &sites,
    ))
}

#[cfg(test)]
#[path = "observed_infra/tests.rs"]
mod tests;
