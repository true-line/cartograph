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
//! name assigned once is followed one step, also into another repository
//! module (`config.DB_URI`, `from app.config import DB_URI`) under the
//! rules `follow_module` documents; a parameter of a module-level function
//! is followed to the argument of that function's one caller under the
//! rules `follow_parameter` documents; anything else is a Gap naming the
//! site.

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
    /// A `from m import *` anywhere: it may rebind any name.
    wildcard: bool,
    /// Each name a relative import imports, whatever its local alias.
    relative: HashSet<String>,
    /// Dotted targets patched by name (`monkeypatch.setattr("a.b.X", …)`,
    /// `patch("a.b.X")`).
    patches: Vec<MemberPath>,
    /// Roots whose members `setattr`/`delattr`/`patch.object` write by a
    /// computed name, other than a method's own receiver, and whether the
    /// write is at the root itself (not beneath a member chain).
    computed_setattr: Vec<(String, bool)>,
    /// A computed-name write on a method's receiver: harmless only if the
    /// method is never called unbound with a module.
    receiver_setattr: bool,
    /// Namespaces written by computed key (`globals()[k] = …`,
    /// `vars(x)[k] = …`, `x.__dict__[k] = …`).
    namespaces: Vec<Namespace>,
}

/// A namespace a computed-key write reaches.
#[derive(Debug, Clone, PartialEq)]
enum Namespace {
    /// `globals()`: the writing module's own.
    Own,
    /// `vars(x)` / `x.__dict__`: the one `x` names.
    Of(String),
    /// `vars(<expression>)`: unknown.
    Unknown,
}

/// The namespace a write target reaches by computed key, if any.
fn namespace_write(cx: &FileCx, target: TsNode, out: &mut Vec<Namespace>) {
    match target.kind() {
        "subscript" | "attribute" => {
            let mut node = target;
            let mut dict = false;
            loop {
                match node.kind() {
                    "attribute" => {
                        dict |= node
                            .child_by_field_name("attribute")
                            .is_some_and(|a| cx.text(&a) == "__dict__");
                        let Some(object) = node.child_by_field_name("object") else {
                            return;
                        };
                        node = object;
                    }
                    "subscript" => {
                        let Some(value) = node.child_by_field_name("value") else {
                            return;
                        };
                        node = value;
                    }
                    _ => break,
                }
            }
            match node.kind() {
                "identifier" if dict => out.push(Namespace::Of(cx.text(&node).to_string())),
                "call" => {
                    let callee = node.child_by_field_name("function");
                    match callee.map(|callee| cx.text(&callee)) {
                        Some("globals") => out.push(Namespace::Own),
                        Some("vars") => out.push(
                            node.child_by_field_name("arguments")
                                .and_then(|args| positional(args).first().copied())
                                .and_then(|arg| member_path(cx, arg))
                                .map_or(Namespace::Unknown, |(root, _)| Namespace::Of(root)),
                        ),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        _ => {
            let mut walk = target.walk();
            for child in target.named_children(&mut walk) {
                namespace_write(cx, child, out);
            }
        }
    }
}

/// Whether `object` is the first parameter of a method defined directly
/// in a class body, undecorated or `@classmethod` (`self`, `cls`). By
/// convention a receiver, not proof: an unbound call may pass anything.
fn is_receiver(cx: &FileCx, object: TsNode) -> bool {
    if object.kind() != "identifier" {
        return false;
    }
    let mut node = object;
    let function = loop {
        match node.parent() {
            Some(parent) if parent.kind() == "function_definition" => break parent,
            Some(parent) => node = parent,
            None => return false,
        }
    };
    let mut owner = function.parent();
    if let Some(decorated) = owner.filter(|o| o.kind() == "decorated_definition") {
        // `@staticmethod` or an unknown decorator: any first argument.
        let mut walk = decorated.walk();
        let only_classmethod = decorated
            .named_children(&mut walk)
            .filter(|child| child.kind() == "decorator")
            .all(|decorator| cx.text(&decorator).trim() == "@classmethod");
        if !only_classmethod {
            return false;
        }
        owner = decorated.parent();
    }
    let in_class = owner
        .filter(|block| block.kind() == "block")
        .and_then(|block| block.parent())
        .is_some_and(|class| class.kind() == "class_definition");
    let first = function
        .child_by_field_name("parameters")
        .and_then(|params| {
            let mut walk = params.walk();
            params.named_children(&mut walk).next()
        });
    in_class
        && first.is_some_and(|first| {
            first.kind() == "identifier" && cx.text(&first) == cx.text(&object)
        })
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

    /// What a name denotes wherever the file uses it: every binding is an
    /// import of one target (at any level) and nothing else writes it.
    /// Weaker than [`Scope::proven`]; enough to say which module a write
    /// through the name reaches.
    fn imported_only(&self, name: &str) -> Option<&Bound> {
        let (bindings, _) = self.imports.get(name)?;
        match (bindings.len(), bindings.iter().next()) {
            (1, Some(Some(bound))) if !self.writes.contains_key(name) => Some(bound),
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

/// The names a relative `from … import` imports, unaliased.
fn relative_targets(cx: &FileCx, node: TsNode) -> Vec<String> {
    if node
        .child_by_field_name("module_name")
        .is_none_or(|module| module.kind() != "relative_import")
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut walk = node.walk();
    for child in node.children_by_field_name("name", &mut walk) {
        let name = match child.kind() {
            "aliased_import" => child.child_by_field_name("name"),
            _ => Some(child),
        };
        if let Some(name) = name {
            let text = cx.text(&name);
            out.push(text.rsplit('.').next().unwrap_or(text).trim().to_string());
        }
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
        wildcard: false,
        relative: HashSet::new(),
        patches: Vec::new(),
        computed_setattr: Vec::new(),
        receiver_setattr: false,
        namespaces: Vec::new(),
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            // Only a statement directly under the module proves a binding.
            let module_level = node.parent().is_some_and(|parent| parent.id() == root.id());
            scope.relative.extend(relative_targets(cx, node));
            for (name, bound) in import_names(cx, node) {
                if name == "*" {
                    scope.wildcard = true;
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
                if let (Some(callee), Some(args)) = (
                    node.child_by_field_name("function"),
                    node.child_by_field_name("arguments"),
                ) {
                    let verb = match callee.kind() {
                        "identifier" => Some(cx.text(&callee)),
                        "attribute" => callee
                            .child_by_field_name("attribute")
                            .map(|attribute| cx.text(&attribute)),
                        _ => None,
                    };
                    // Keyword forms (`patch.object(target=x, attribute="a")`,
                    // `monkeypatch.setattr(target=…, name=…)`) name the same
                    // operands; a splat may name any of them.
                    let listed = positional(args);
                    let splat = listed
                        .iter()
                        .any(|arg| matches!(arg.kind(), "list_splat" | "dictionary_splat"));
                    if splat && matches!(verb, Some("setattr" | "delattr" | "patch" | "object")) {
                        scope.namespaces.push(Namespace::Unknown);
                    }
                    let first = keyword(cx, args, "target").or(listed.first().copied());
                    let attr_arg = keyword(cx, args, "attribute")
                        .or(keyword(cx, args, "name"))
                        .or(listed.get(1).copied());
                    match (verb, first.and_then(|first| literal_string(cx, first))) {
                        // `monkeypatch.setattr("a.b.X", v)`, `patch("a.b.X")`.
                        (Some("setattr" | "delattr" | "patch"), Some(target)) => scope
                            .patches
                            .push(target.split('.').map(|s| Some(s.to_string())).collect()),
                        // `setattr(x, "a", v)`, `monkeypatch.setattr(x, …)`,
                        // `patch.object(x, "a")`: member `a` of `x`, or any
                        // member when the name is computed.
                        (Some("setattr" | "delattr" | "object"), None) => {
                            if let Some((name, mut path)) =
                                first.and_then(|object| member_path(cx, object))
                            {
                                let attr = attr_arg.and_then(|attr| literal_string(cx, attr));
                                if attr.is_none() {
                                    if first.is_some_and(|object| is_receiver(cx, object)) {
                                        scope.receiver_setattr = true;
                                    } else {
                                        scope
                                            .computed_setattr
                                            .push((name.clone(), path.is_empty()));
                                    }
                                }
                                path.push(attr);
                                scope.members.entry(name).or_default().insert(path);
                            } else if first.is_some() {
                                // `setattr(f(), k, v)`: an unknown object.
                                scope.namespaces.push(Namespace::Unknown);
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(target) = target {
            member_writes(cx, target, &mut scope.members);
            namespace_write(cx, target, &mut scope.namespaces);
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
    if scope.wildcard {
        scope.imports.clear();
    }
    scope
}

/// The dotted segments a proven binding names: `Module(a.b)` → `[a, b]`,
/// `Symbol(m, x)` → `[m…, x]`.
fn bound_segments(bound: &Bound) -> MemberPath {
    let (module, name) = match bound {
        Bound::Module(module) => (module.as_str(), None),
        Bound::Symbol(module, name) => (module.as_str(), Some(name.clone())),
    };
    module
        .split('.')
        .map(|segment| Some(segment.to_string()))
        .chain(name.map(Some))
        .collect()
}

/// A site whose identity names a constant of another module, resolved
/// once every file has been read.
struct Pending {
    site: usize,
    module: String,
    name: String,
    /// The reading module's own binding the site reads through
    /// (`app.db.config`): patching it replaces what the site sees.
    local: MemberPath,
}

/// A site whose identity is a parameter of the module-level function it
/// is in, resolved through that function's one caller once every file has
/// been read.
struct ParamPending {
    site: usize,
    module: String,
    function: String,
    /// The parameter's position, and its name for a keyword argument.
    index: usize,
    name: String,
}

/// A function export, `(module, name)`.
type Export = (String, String);

/// What cross-module follows need from every file.
#[derive(Default)]
struct Follows {
    pending: Vec<Pending>,
    params: Vec<ParamPending>,
    /// Calls through a proven import, by `(module, export)`: the caller's
    /// file and the call's span.
    calls: HashMap<Export, Vec<(String, usize, usize)>>,
    /// `(module, export)`s some file reads other than by calling them
    /// (a value, a re-export, an unproven import): a caller may hide.
    escaped: HashSet<(String, String)>,
    /// Names bound by an import naming no module (relative, `*`), and
    /// modules imported other than provably: calls through them are unseen.
    unseen_names: HashSet<String>,
    unseen_modules: HashSet<String>,
    /// Dotted member paths written through a proven binding or patched by
    /// name; a `None` segment is any member.
    module_writes: Vec<MemberPath>,
    /// Member names written through any other root (an alias, a
    /// parameter, `self`): such a write may rebind a module's constant.
    rebound: HashSet<String>,
    /// A computed-name member write through a root that may be any module
    /// (an alias, a parameter): no follow is proven.
    any_member: bool,
    /// A computed-name write on a method receiver somewhere: follows rest
    /// on the receiver convention.
    receiver_assumed: bool,
}

/// `a.b.c` from module segments.
fn dotted(path: &MemberPath) -> String {
    path.iter().flatten().cloned().collect::<Vec<_>>().join(".")
}

/// The dotted module a repository-relative Python file is.
fn module_of(path: &str) -> MemberPath {
    let stem = path.strip_suffix(".py").unwrap_or(path);
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    stem.split('/').map(|s| Some(s.to_string())).collect()
}

/// The `(module, attribute)` a callee names through a proven import:
/// `X` → its binding; `n.a.X` → (`<n's module>.a`, `X`).
fn callee_export(cx: &FileCx, callee: TsNode, scope: &Scope) -> Option<(String, String)> {
    match callee.kind() {
        // Any member write under a called name (`R.__new__ = …`) voids it.
        "identifier" if scope.overwritten(cx.text(&callee), &[]) => None,
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

/// The module-level, undecorated function whose plain positional parameter
/// `value` is, never written in the function's body at any depth: the
/// function's name and the parameter's position. A default, a splat or a
/// separator anywhere in the list refuses it.
fn param_of(cx: &FileCx, root: TsNode, call: TsNode, value: TsNode) -> Option<(String, usize)> {
    if value.kind() != "identifier" {
        return None;
    }
    let name = cx.text(&value);
    let function = enclosing_function_node(call)?;
    if function.parent().map(|parent| parent.id()) != Some(root.id()) {
        return None;
    }
    let parameters = function.child_by_field_name("parameters")?;
    let mut walk = parameters.walk();
    let mut index = None;
    for (at, parameter) in parameters
        .named_children(&mut walk)
        .filter(|parameter| parameter.kind() != "comment")
        .enumerate()
    {
        let ident = match parameter.kind() {
            "identifier" => parameter,
            "typed_parameter" => {
                let mut walk = parameter.walk();
                parameter
                    .named_children(&mut walk)
                    .find(|child| child.kind() == "identifier")?
            }
            _ => return None,
        };
        if cx.text(&ident) == name {
            index = Some(at);
        }
    }
    let index = index?;
    let mut written = Vec::new();
    let mut stack = vec![function.child_by_field_name("body")?];
    while let Some(node) = stack.pop() {
        written_names(node, &mut written);
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    if written.iter().any(|node| cx.text(node) == name) {
        return None;
    }
    let function_name = cx.text(&function.child_by_field_name("name")?);
    Some((function_name.to_string(), index))
}

/// Every proven-import reference in the file: calls through `(module,
/// export)` are recorded; any other read of an import-bound function (a
/// value, an attribute of it, an import never called) escapes; imports a
/// call could be made through unseen are recorded too.
fn collect_references(cx: &FileCx, root: TsNode, scope: &Scope, follows: &mut Follows) {
    for (name, (bindings, _)) in &scope.imports {
        for bound in bindings {
            match bound {
                None => {
                    follows.unseen_names.insert(name.clone());
                }
                Some(Bound::Symbol(module, export)) if scope.proven(name).is_none() => {
                    follows.escaped.insert((module.clone(), export.clone()));
                }
                Some(Bound::Module(module)) if scope.proven(name).is_none() => {
                    follows.unseen_modules.insert(module.clone());
                }
                _ => {}
            }
        }
    }
    let mut called = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            continue;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        let parent = node.parent();
        let field_of = |field: &str| {
            parent
                .and_then(|parent| parent.child_by_field_name(field))
                .is_some_and(|child| child.id() == node.id())
        };
        let resolved = match node.kind() {
            "identifier" => {
                // A member name, or a keyword's name, is not a read of it.
                let kind = parent.map(|parent| parent.kind());
                if (kind == Some("attribute") && field_of("attribute"))
                    || (kind == Some("keyword_argument") && field_of("name"))
                {
                    continue;
                }
                match scope.proven(cx.text(&node)) {
                    Some(Bound::Symbol(module, export)) => Some((module.clone(), export.clone())),
                    _ => None,
                }
            }
            "attribute" => callee_export(cx, node, scope),
            _ => None,
        };
        let Some(target) = resolved else {
            continue;
        };
        let callee = parent.is_some_and(|parent| parent.kind() == "call") && field_of("function");
        if callee && let Some(call) = parent {
            called.insert(target.clone());
            follows.calls.entry(target).or_default().push((
                cx.path.to_string(),
                call.start_byte(),
                call.end_byte(),
            ));
        } else {
            follows.escaped.insert(target);
        }
    }
    // An import the file never calls may be a re-export.
    for name in scope.imports.keys() {
        if let Some(Bound::Symbol(module, export)) = scope.proven(name)
            && !called.contains(&(module.clone(), export.clone()))
        {
            follows.escaped.insert((module.clone(), export.clone()));
        }
    }
}

fn extract_file(
    source: &[u8],
    path: &str,
    id: &SourceId,
    registries: &Registries,
    sites: &mut Vec<ObservedSite>,
    follows: &mut Follows,
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
    // Member writes through an import-bound name (`config.X = …`) may
    // replace another module's constant: no follow may read past them.
    for (name, paths) in &scope.members {
        if let Some(bound) = scope.imported_only(name) {
            let base = bound_segments(bound);
            follows.module_writes.extend(
                paths
                    .iter()
                    .map(|path| [base.clone(), path.clone()].concat()),
            );
        }
        // Through any root, a named member write may rebind a constant of
        // whatever module the chain reaches (`holder.cfg.DB_URI = …`).
        follows.rebound.extend(
            paths
                .iter()
                .filter_map(|path| path.last().cloned().flatten()),
        );
    }
    follows.module_writes.extend(scope.patches.iter().cloned());
    // A computed-name write is attributable only at an import-bound root
    // itself; beneath a member chain (`holder.target`) it may be anything.
    for (root, at_root) in &scope.computed_setattr {
        if !at_root || scope.imported_only(root).is_none() {
            follows.any_member = true;
        }
    }
    follows.receiver_assumed |= scope.receiver_setattr;
    for namespace in &scope.namespaces {
        match namespace {
            Namespace::Own => follows
                .module_writes
                .push([module_of(path), vec![None]].concat()),
            Namespace::Of(root) => match scope.imported_only(root) {
                Some(bound) => follows
                    .module_writes
                    .push([bound_segments(bound), vec![None]].concat()),
                None => follows.any_member = true,
            },
            Namespace::Unknown => follows.any_member = true,
        }
    }
    // A wildcard import clears the file's proven imports, so a call through
    // it is unseen.
    if scope.wildcard {
        follows.unseen_names.insert("*".into());
    }
    // A call through a relative import is unseen under its target's name.
    follows.unseen_names.extend(scope.relative.iter().cloned());
    if scope.imports.is_empty() {
        return Ok(());
    }
    collect_references(&cx, root, &scope, follows);
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
        let value = match &client.identity {
            None => None,
            Some(IdentityArg::First) => first.or_else(|| {
                client
                    .keyword
                    .as_deref()
                    .and_then(|key| keyword(&cx, args, key))
            }),
            Some(IdentityArg::Key(key)) => keyword(&cx, args, key),
        };
        let identity = value.map(|value| classify(&cx, root, value, &scope, true));
        // A parameter: the argument of the function's one caller.
        if let (Some(IdentityExpr::Computed(_)), Some(value)) = (&identity, value)
            && let Some((function, index)) = param_of(&cx, root, node, value)
        {
            follows.params.push(ParamPending {
                site: sites.len(),
                module: dotted(&module_of(path)),
                function,
                index,
                name: cx.text(&value).to_string(),
            });
        }
        // `config.X` / an imported `X`: another module's constant.
        if let (Some(IdentityExpr::Computed(_)), Some(value)) = (&identity, value)
            && let Some((module, name)) = callee_export(&cx, value, &scope)
            && let Some((root_name, _)) = member_path(&cx, value)
        {
            let mut local = module_of(path);
            local.push(Some(root_name));
            follows.pending.push(Pending {
                site: sites.len(),
                module,
                name,
                local,
            });
        }
        let symbol = enclosing_function_node(node)
            .and_then(|function| qualified_function_name(&cx, function))
            .map(|name| symbol_id(id.repo, path, &name));
        sites.push(ObservedSite {
            family: client.family,
            provider: client.provider.clone(),
            module: Some(client.module.clone()),
            identity,
            identity_source: None,
            identity_assumptions: Vec::new(),
            registry: Some(client.registry.clone()),
            symbol,
            path: path.to_string(),
            byte_start: node.start_byte() as u64,
            byte_end: node.end_byte() as u64,
        });
    }
    Ok(())
}

/// A parsed repository module a follow reads.
struct ModuleFile {
    rel: String,
    source: Vec<u8>,
    tree: tree_sitter::Tree,
}

/// The repository file a dotted module is, when exactly one candidate
/// exists and no package on the way binds the next segment itself (`from
/// app import config` may then name `app/__init__.py`'s attribute).
fn module_file(
    root: &Path,
    id: &SourceId,
    known: &HashSet<&str>,
    module: &str,
) -> Result<Option<ModuleFile>, ExtractError> {
    let base = module.replace('.', "/");
    let candidates = [format!("{base}.py"), format!("{base}/__init__.py")];
    let [rel] = candidates
        .iter()
        .filter(|rel| known.contains(rel.as_str()))
        .collect::<Vec<_>>()[..]
    else {
        return Ok(None);
    };
    let segments: Vec<&str> = module.split('.').collect();
    for depth in 1..segments.len() {
        let package = format!("{}/__init__.py", segments[..depth].join("/"));
        if !known.contains(package.as_str()) {
            continue;
        }
        let Some(init) = parse(root, &package)? else {
            return Ok(None);
        };
        let cx = FileCx {
            source: &init.source,
            path: &init.rel,
            id,
        };
        let scope = collect_scope(&cx, init.tree.root_node());
        let next = segments[depth];
        if scope.wildcard || scope.writes.contains_key(next) || scope.imports.contains_key(next) {
            return Ok(None);
        }
    }
    parse(root, rel)
}

fn parse(root: &Path, rel: &str) -> Result<Option<ModuleFile>, ExtractError> {
    let source = std::fs::read(root.join(rel))?;
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    Ok(parser.parse(&source, None).map(|tree| ModuleFile {
        rel: rel.to_string(),
        source,
        tree,
    }))
}

/// A followed identity and the assignment it rests on (path and span).
type Followed = (IdentityExpr, (String, u64, u64));

/// Another module's constant as an identity, one step: the module is one
/// repository file, it assigns the name once at module level and never
/// otherwise writes or imports it (nor star-imports), no file writes that
/// member through a proven binding, and the value is itself a literal or
/// env read. Returns the identity and the assignment it rests on.
fn follow_module(
    root: &Path,
    id: &SourceId,
    known: &HashSet<&str>,
    follows: &Follows,
    modules: &mut HashMap<String, Option<ModuleFile>>,
    pending: &Pending,
) -> Result<Option<Followed>, ExtractError> {
    let (module, name) = (pending.module.as_str(), pending.name.as_str());
    let read: MemberPath = module
        .split('.')
        .chain([name])
        .map(|s| Some(s.to_string()))
        .collect();
    let overlaps = |read: &MemberPath| {
        follows.module_writes.iter().any(|path| {
            path.iter()
                .zip(read)
                .all(|(segment, read)| segment.is_none() || segment == read)
        })
    };
    let local_name = pending.local.last().cloned().flatten();
    if follows.any_member
        || overlaps(&read)
        || overlaps(&pending.local)
        || follows.rebound.contains(name)
        || local_name.is_some_and(|local| follows.rebound.contains(&local))
    {
        return Ok(None);
    }
    if !modules.contains_key(module) {
        let file = module_file(root, id, known, module)?;
        modules.insert(module.to_string(), file);
    }
    let Some(Some(file)) = modules.get(module) else {
        return Ok(None);
    };
    let cx = FileCx {
        source: &file.source,
        path: &file.rel,
        id,
    };
    let root_node = file.tree.root_node();
    let scope = collect_scope(&cx, root_node);
    if scope.wildcard || scope.writes.get(name) != Some(&1) || scope.imports.contains_key(name) {
        return Ok(None);
    }
    let Some(&(start, end)) = scope.assigned.get(name) else {
        return Ok(None);
    };
    let Some(value) = root_node.descendant_for_byte_range(start, end) else {
        return Ok(None);
    };
    let identity = classify(&cx, root_node, value, &scope, false);
    if matches!(identity, IdentityExpr::Computed(_)) {
        return Ok(None);
    }
    let statement = value.parent().unwrap_or(value);
    Ok(Some((
        identity,
        (
            file.rel.clone(),
            statement.start_byte() as u64,
            statement.end_byte() as u64,
        ),
    )))
}

/// A parameter's identity, one step through its function's caller: no
/// wildcard, relative or unproven import could call the function unseen,
/// nothing writes it as a member or reads it other than by calling it, its
/// module defines it once and names it nowhere else, and exactly one call
/// reaches it, without splats, passing the parameter positionally or by
/// keyword (not both). The argument is classified in the caller, and a
/// `config.X` argument followed as `follow_module` does. Dynamic lookups
/// (`getattr(module, "f")`) are not seen, so the result carries
/// `identity_assumes_static_callers`.
fn follow_parameter(
    root: &Path,
    id: &SourceId,
    known: &HashSet<&str>,
    follows: &Follows,
    modules: &mut HashMap<String, Option<ModuleFile>>,
    param: &ParamPending,
) -> Result<Option<Followed>, ExtractError> {
    let (module, function) = (param.module.as_str(), param.function.as_str());
    let target = (module.to_string(), function.to_string());
    let last = module.rsplit('.').next().unwrap_or(module);
    let read: MemberPath = module
        .split('.')
        .chain([function])
        .map(|s| Some(s.to_string()))
        .collect();
    let written = follows.module_writes.iter().any(|path| {
        path.iter()
            .zip(&read)
            .all(|(segment, read)| segment.is_none() || segment == read)
    });
    let unseen = ["*", function, last]
        .iter()
        .any(|name| follows.unseen_names.contains(*name))
        || follows
            .unseen_modules
            .iter()
            .any(|m| module == m || module.starts_with(&format!("{m}.")));
    if unseen
        || written
        || follows.any_member
        || follows.rebound.contains(function)
        || follows.escaped.contains(&target)
    {
        return Ok(None);
    }
    let Some([(caller, start, end)]) = follows.calls.get(&target).map(Vec::as_slice) else {
        return Ok(None);
    };
    // Defined once, and named nowhere else in its own module.
    let Some(defining) = module_file(root, id, known, module)? else {
        return Ok(None);
    };
    let cx = FileCx {
        source: &defining.source,
        path: &defining.rel,
        id,
    };
    let mut mentions = 0;
    let mut stack = vec![defining.tree.root_node()];
    while let Some(node) = stack.pop() {
        mentions += usize::from(node.kind() == "identifier" && cx.text(&node) == function);
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    // One mention is the definition: any write or import of the name in
    // its module is another (a wildcard anywhere is refused above).
    if mentions != 1 {
        return Ok(None);
    }
    let Some(file) = parse(root, caller)? else {
        return Ok(None);
    };
    let cx = FileCx {
        source: &file.source,
        path: &file.rel,
        id,
    };
    let caller_root = file.tree.root_node();
    let Some(args) = caller_root
        .descendant_for_byte_range(*start, *end)
        .filter(|call| call.kind() == "call")
        .and_then(|call| call.child_by_field_name("arguments"))
    else {
        return Ok(None);
    };
    let mut walk = args.walk();
    if args
        .named_children(&mut walk)
        .any(|arg| matches!(arg.kind(), "list_splat" | "dictionary_splat"))
    {
        return Ok(None);
    }
    let value = match (
        positional(args).get(param.index).copied(),
        keyword(&cx, args, &param.name),
    ) {
        (Some(value), None) | (None, Some(value)) => value,
        _ => return Ok(None),
    };
    let scope = collect_scope(&cx, caller_root);
    let span = (
        file.rel.clone(),
        value.start_byte() as u64,
        value.end_byte() as u64,
    );
    match classify(&cx, caller_root, value, &scope, true) {
        IdentityExpr::Computed(_) => {}
        identity => return Ok(Some((identity, span))),
    }
    let (Some((module, name)), Some((root_name, _))) =
        (callee_export(&cx, value, &scope), member_path(&cx, value))
    else {
        return Ok(None);
    };
    let mut local = module_of(&file.rel);
    local.push(Some(root_name));
    let pending = Pending {
        site: param.site,
        module,
        name,
        local,
    };
    follow_module(root, id, known, follows, modules, &pending)
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
    let mut follows = Follows::default();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, registries, &mut sites, &mut follows)?;
    }
    let known: HashSet<&str> = files.iter().map(String::as_str).collect();
    let mut modules: HashMap<String, Option<ModuleFile>> = HashMap::new();
    let receiver_assumption: Vec<&'static str> = follows
        .receiver_assumed
        .then_some("identity_assumes_method_receivers")
        .into_iter()
        .collect();
    for pending in &follows.pending {
        if let Some((identity, source)) =
            follow_module(root, id, &known, &follows, &mut modules, pending)?
        {
            sites[pending.site].identity = Some(identity);
            sites[pending.site].identity_source = Some(source);
            sites[pending.site].identity_assumptions = receiver_assumption.clone();
        }
    }
    for param in &follows.params {
        if let Some((identity, source)) =
            follow_parameter(root, id, &known, &follows, &mut modules, param)?
        {
            sites[param.site].identity = Some(identity);
            sites[param.site].identity_source = Some(source);
            // The caller's argument may itself rest on the receiver
            // convention, and a receiver write could rebind the function.
            sites[param.site].identity_assumptions = [
                receiver_assumption.as_slice(),
                &["identity_assumes_static_callers"],
            ]
            .concat();
        }
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
