//! Go deterministic (T0) language adapter (SPEC-00 §3.3, M10).
//!
//! Import-proven `net/http`, chi, and gin registrations become
//! Endpoint/HANDLES facts. Functions, imports, and direct calls become the
//! server graph, with Go-module-aware directory joins for local packages.
//! Import-proven `net/http` client calls become fetch sites (AC-0434).

use core_graph::placeholder::Boundary;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::ControlFlow;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-go";

/// Go extraction errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// tree-sitter grammar/version mismatch.
    #[error("language: {0}")]
    Language(#[from] tree_sitter::LanguageError),
    /// The parser returned no tree.
    #[error("parse produced no tree for {0}")]
    NoTree(String),
    /// Filesystem failure while walking a directory.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Identity of the source being recovered.
pub struct SourceId<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for an unversioned tree.
    pub commit: &'a str,
}

/// Go graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    /// `net/http` client calls, stitched to endpoints by the caller.
    pub fetch_sites: Vec<adapters_fw::client::FetchSite>,
    /// AWS SDK and kafka-go event sites (AC-0462), stitched by the
    /// `events` crate.
    pub event_sites: Vec<adapters_fw::events::EventSite>,
    pending_calls: Vec<PendingCall>,
    pending_endpoints: Vec<PendingEndpoint>,
    handler_functions: Vec<vercel::HandlerFunction>,
    pending_bases: Vec<base::Pending>,
    base_bindings: Vec<base::Binding>,
    test_requests: Vec<target_tests::Request>,
    pending_gorm: Vec<gorm::Pending>,
    gorm_table_names: Vec<gorm::TableName>,
}

#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    package_dir: String,
    exported: String,
    props: serde_json::Value,
}

#[derive(Debug, Clone)]
struct PendingEndpoint {
    node: Node,
    edge: Edge,
    /// The endpoint, its Gap and the HANDLES edge to it, kept when no
    /// Symbol answers the package handler (#2007): the route stays listed.
    gap: (Node, Node, Edge),
    package_dir: String,
    exported: String,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Go parse cache (AC-0054).
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Go source work performed by an incremental extraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// New or byte-changed files parsed.
    pub recomputed_files: u64,
    /// Content-identical files reused.
    pub reused_files: u64,
    /// Cached files removed because the source disappeared.
    pub deleted_files: u64,
}

struct FileCx<'a> {
    source: &'a [u8],
    path: &'a str,
    id: &'a SourceId<'a>,
}

impl FileCx<'_> {
    fn text(&self, node: &TsNode<'_>) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn prov(&self, node: &TsNode<'_>, fact: &str) -> serde_json::Value {
        self.prov_with_confidence(node, ConfidenceTier::Confirmed, fact)
    }

    fn prov_with_confidence(
        &self,
        node: &TsNode<'_>,
        confidence: ConfidenceTier,
        fact: &str,
    ) -> serde_json::Value {
        let provenance = Provenance::new(
            Tier::Deterministic,
            confidence,
            vec![EvidenceRef {
                repo: self.id.repo.into(),
                path: self.path.into(),
                byte_start: node.start_byte() as u64,
                byte_end: node.end_byte() as u64,
                commit_sha: self.id.commit.into(),
            }],
            EXTRACTOR_ID,
            fact.as_bytes(),
        )
        .expect("Deterministic confidence is within its ceiling");
        serde_json::to_value(provenance).expect("provenance serializes")
    }
}

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

/// Whether a Symbol id names a function of a test file.
fn in_test_file(id: &str) -> bool {
    id.split_once('#')
        .is_some_and(|(file, _)| file.ends_with("_test.go"))
}

fn symbol_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

fn retarget_props_commit(props: &mut serde_json::Value, commit: &str) {
    let Ok(mut provenance) =
        serde_json::from_value::<Provenance>(props.get("prov").cloned().unwrap_or_default())
    else {
        return;
    };
    for evidence in &mut provenance.evidence {
        evidence.commit_sha = commit.into();
    }
    props["prov"] = serde_json::to_value(provenance).expect("provenance serializes");
}

fn retarget_commit(extraction: &mut Extraction, commit: &str) {
    for node in &mut extraction.nodes {
        retarget_props_commit(&mut node.props, commit);
        if node.label == "BusinessRule" {
            rule_evidence::retarget(&mut node.props, commit);
        }
    }
    for edge in &mut extraction.edges {
        retarget_props_commit(&mut edge.props, commit);
    }
    for pending in &mut extraction.pending_calls {
        retarget_props_commit(&mut pending.props, commit);
    }
    for function in &mut extraction.handler_functions {
        let mut props = serde_json::json!({ "prov": function.prov.take() });
        retarget_props_commit(&mut props, commit);
        function.prov = props["prov"].take();
    }
    for pending in &mut extraction.pending_gorm {
        retarget_props_commit(&mut pending.props, commit);
    }
    for request in &mut extraction.test_requests {
        request.retarget(commit);
    }
    for pending in &mut extraction.pending_endpoints {
        retarget_props_commit(&mut pending.node.props, commit);
        retarget_props_commit(&mut pending.edge.props, commit);
        let (endpoint, gap, handles) = &mut pending.gap;
        retarget_props_commit(&mut endpoint.props, commit);
        retarget_props_commit(&mut gap.props, commit);
        retarget_props_commit(&mut handles.props, commit);
    }
}

#[derive(Debug, Clone)]
struct ImportBinding {
    path: String,
    package_dir: Option<String>,
}

fn local_package_dir(import_path: &str, module_path: Option<&str>) -> Option<String> {
    let module_path = module_path?;
    if import_path == module_path {
        return Some(String::new());
    }
    import_path
        .strip_prefix(module_path)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .map(str::to_string)
}

fn unquote(text: &str) -> Option<String> {
    let text = text.trim();
    ((text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('`') && text.ends_with('`')))
    .then(|| text[1..text.len() - 1].to_string())
}

fn default_import_name(import_path: &str) -> &str {
    match import_path {
        "net/http" => "http",
        "github.com/go-chi/chi" | "github.com/go-chi/chi/v5" => "chi",
        "github.com/gin-gonic/gin" => "gin",
        "github.com/segmentio/kafka-go" => "kafka",
        _ => import_path.rsplit('/').next().unwrap_or(import_path),
    }
}

fn parse_imports(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    module_path: Option<&str>,
    out: &mut Extraction,
) -> HashMap<String, ImportBinding> {
    let query = crate::queries::query("(import_spec) @spec");
    let mut bindings = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    while let Some(found) = matches.next() {
        let spec = found.captures()[0].node;
        let Some(path_node) = spec.child_by_field_name("path") else {
            continue;
        };
        let Some(import_path) = unquote(cx.text(&path_node)) else {
            continue;
        };
        let explicit = spec
            .child_by_field_name("name")
            .map(|name| cx.text(&name).to_string());
        let local = explicit.unwrap_or_else(|| default_import_name(&import_path).to_string());
        if local != "_" && local != "." {
            bindings.insert(
                local,
                ImportBinding {
                    package_dir: local_package_dir(&import_path, module_path),
                    path: import_path.clone(),
                },
            );
        }
        let mut props = serde_json::json!({
            "specifier": import_path,
            "prov": cx.prov(&spec, &format!("IMPORTS {}", cx.text(&spec))),
        });
        // A package of this module: its directory in the repository, so a
        // consumer need not guess it from the import path (#2010).
        if let Some(dir) = local_package_dir(&import_path, module_path) {
            props["package_dir"] = dir.into();
        }
        out.edges.push(Edge {
            src: file_id(cx.id.repo, cx.path),
            dst: format!("mod:{import_path}"),
            label: "IMPORTS".into(),
            props,
        });
    }
    bindings
}

fn package_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .filter(|parent| parent != ".")
        .unwrap_or_default()
}

fn receiver_type(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    let receiver = function.child_by_field_name("receiver")?;
    cx.text(&receiver)
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .rfind(|piece| !piece.is_empty())
        .map(str::to_string)
}

fn function_name(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<(String, String)> {
    let simple = cx.text(&function.child_by_field_name("name")?).to_string();
    let qualified = receiver_type(cx, function)
        .map(|receiver| format!("{receiver}.{simple}"))
        .unwrap_or_else(|| simple.clone());
    Some((simple, qualified))
}

fn enclosing_function(mut node: TsNode<'_>, functions: &HashMap<usize, String>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) && let Some(id) = functions.get(&parent.start_byte())
        {
            return Some(id.clone());
        }
        node = parent;
    }
    None
}

/// The call's arguments; comments between them are not arguments.
fn call_arguments(call: TsNode<'_>) -> Vec<TsNode<'_>> {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = arguments.walk();
    arguments
        .named_children(&mut walk)
        .filter(|argument| argument.kind() != "comment")
        .collect()
}

fn literal_string(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    )
    .then(|| unquote(cx.text(&node)))
    .flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framework {
    HttpMux,
    Chi,
    Gin,
}

enum HandlerTarget {
    Resolved(String),
    Package {
        package_dir: String,
        exported: String,
    },
    Gap {
        expression: String,
    },
}

fn framework_factory(callee: &str, bindings: &HashMap<String, ImportBinding>) -> Option<Framework> {
    let (base, member) = callee.split_once('.')?;
    let path = &bindings.get(base)?.path;
    match (path.as_str(), member) {
        ("net/http", "NewServeMux") => Some(Framework::HttpMux),
        ("github.com/go-chi/chi", "NewRouter") | ("github.com/go-chi/chi/v5", "NewRouter") => {
            Some(Framework::Chi)
        }
        ("github.com/gin-gonic/gin", "Default" | "New") => Some(Framework::Gin),
        _ => None,
    }
}

/// The router a declared type proves (#2006): `*http.ServeMux`,
/// `chi.Router`/`*chi.Mux`, `*gin.Engine`/`*gin.RouterGroup`, each through
/// its import binding.
fn framework_type(text: &str, bindings: &HashMap<String, ImportBinding>) -> Option<Framework> {
    let (base, name) = text.trim().trim_start_matches('*').split_once('.')?;
    let path = &bindings.get(base)?.path;
    match (path.as_str(), name) {
        ("net/http", "ServeMux") => Some(Framework::HttpMux),
        ("github.com/go-chi/chi" | "github.com/go-chi/chi/v5", "Router" | "Mux") => {
            Some(Framework::Chi)
        }
        ("github.com/gin-gonic/gin", "Engine" | "RouterGroup" | "IRouter" | "IRoutes") => {
            Some(Framework::Gin)
        }
        _ => None,
    }
}

/// One declaration of a local name: the scope it is visible in, where it
/// is declared, and the router its type or factory proves, if any.
#[derive(Debug, Clone, Copy)]
struct Declaration {
    scope: (usize, usize),
    at: usize,
    file_scope: bool,
    framework: Option<Framework>,
}

/// Router receivers, resolved lexically (#2006): a name proves a router only
/// where its innermost visible declaration does, so a same-named local of
/// another type in another function, or a shadowing one, proves nothing.
#[derive(Debug, Default)]
struct Receivers {
    declarations: HashMap<String, Vec<Declaration>>,
}

impl Receivers {
    fn visible(&self, name: &str, at: usize) -> Option<(usize, &Declaration)> {
        self.declarations
            .get(name)?
            .iter()
            .enumerate()
            .filter(|(_, d)| d.scope.0 <= at && at < d.scope.1 && (d.file_scope || d.at <= at))
            .min_by_key(|(_, d)| (d.scope.1 - d.scope.0, std::cmp::Reverse(d.at)))
    }

    fn get(&self, name: &str, at: usize) -> Option<Framework> {
        self.visible(name, at)?.1.framework
    }
}

/// The scope a declaration at `node` is visible in: its function for a
/// parameter, else the nearest enclosing block, statement header or case.
fn declaration_scope(node: TsNode<'_>, parameter: bool) -> (usize, usize, bool) {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let found = if parameter {
            matches!(
                parent.kind(),
                "function_declaration" | "method_declaration" | "func_literal"
            )
        } else {
            matches!(
                parent.kind(),
                "block"
                    | "if_statement"
                    | "for_statement"
                    | "expression_switch_statement"
                    | "type_switch_statement"
                    | "expression_case"
                    | "type_case"
                    | "default_case"
                    | "communication_case"
                    | "source_file"
            )
        };
        if found {
            return (
                parent.start_byte(),
                parent.end_byte(),
                parent.kind() == "source_file",
            );
        }
        current = parent;
    }
    (node.start_byte(), node.end_byte(), true)
}

/// Router receivers proven by a parameter's or variable's declared type
/// (`func routes(mux *http.ServeMux)`, `var mux *http.ServeMux`, #2006), by a
/// `mux := http.NewServeMux()` / `var mux = …` binding, or by a later
/// `mux = http.NewServeMux()` assignment to an untyped declaration. Every
/// other declaration is recorded too, so it shadows.
fn assignment_receivers(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Receivers {
    let factory_of = |value: Option<TsNode<'_>>| {
        value
            .filter(|v| v.kind() == "call_expression")
            .and_then(|v| v.child_by_field_name("function"))
            .and_then(|f| framework_factory(cx.text(&f), bindings))
    };
    let mut receivers = Receivers::default();
    let mut assignments = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        let mut declare = |name: TsNode<'_>, parameter: bool, framework: Option<Framework>| {
            let (start, end, file_scope) = declaration_scope(node, parameter);
            receivers
                .declarations
                .entry(cx.text(&name).to_string())
                .or_default()
                .push(Declaration {
                    scope: (start, end),
                    at: name.start_byte(),
                    file_scope,
                    framework,
                });
        };
        match node.kind() {
            "parameter_declaration" | "var_spec" => {
                let typed = node
                    .child_by_field_name("type")
                    .and_then(|t| framework_type(cx.text(&t), bindings));
                let values = node.child_by_field_name("value");
                let single = values.filter(|v| v.named_child_count() == 1);
                let framework = typed.or_else(|| factory_of(single.and_then(|v| v.named_child(0))));
                let mut walk = node.walk();
                let names: Vec<_> = node.children_by_field_name("name", &mut walk).collect();
                let framework = if names.len() == 1 || typed.is_some() {
                    framework
                } else {
                    None
                };
                for name in names {
                    declare(name, node.kind() == "parameter_declaration", framework);
                }
            }
            "short_var_declaration" => {
                let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) else {
                    continue;
                };
                let framework = (left.named_child_count() == 1 && right.named_child_count() == 1)
                    .then(|| factory_of(right.named_child(0)))
                    .flatten();
                let mut walk = left.walk();
                for name in left.named_children(&mut walk) {
                    if name.kind() == "identifier" {
                        declare(name, false, framework);
                    }
                }
            }
            "assignment_statement" => {
                let (Some(left), Some(right)) = (
                    node.child_by_field_name("left")
                        .and_then(|l| l.named_child(0)),
                    node.child_by_field_name("right")
                        .and_then(|r| r.named_child(0)),
                ) else {
                    continue;
                };
                if left.kind() == "identifier"
                    && let Some(framework) = factory_of(Some(right))
                {
                    assignments.push((cx.text(&left).to_string(), left.start_byte(), framework));
                }
            }
            _ => {}
        }
    }
    for (name, at, framework) in assignments {
        let Some((index, declaration)) = receivers.visible(&name, at) else {
            continue;
        };
        if declaration.framework.is_none() {
            receivers.declarations.get_mut(&name).expect("visible")[index].framework =
                Some(framework);
        }
    }
    receivers
}

fn route_pattern(pattern: String) -> (String, String) {
    if let Some((method, path)) = pattern.split_once(' ')
        && !method.is_empty()
        && method
            .chars()
            .all(|character| character.is_ascii_uppercase())
        && path.starts_with('/')
    {
        return (method.into(), path.into());
    }
    ("ANY".into(), pattern)
}

fn endpoint_registration<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    callee: &str,
    bindings: &HashMap<String, ImportBinding>,
    receivers: &Receivers,
) -> Option<(String, String, TsNode<'t>, &'static str)> {
    let (base, member) = callee.split_once('.')?;
    let arguments = call_arguments(call);
    let route = arguments
        .first()
        .copied()
        .and_then(|argument| literal_string(cx, argument))?;
    let handler = arguments.get(1).copied()?;
    if bindings.get(base).map(|binding| binding.path.as_str()) == Some("net/http")
        && matches!(member, "Handle" | "HandleFunc")
    {
        let (method, path) = route_pattern(route);
        return Some((method, path, handler, "net/http"));
    }
    let framework = receivers.get(base, call.start_byte())?;
    match &framework {
        Framework::HttpMux if matches!(member, "Handle" | "HandleFunc") => {
            let (method, path) = route_pattern(route);
            Some((method, path, handler, "net/http"))
        }
        Framework::Chi => {
            const METHODS: &[&str] = &["Get", "Post", "Put", "Delete", "Patch", "Options", "Head"];
            METHODS
                .contains(&member)
                .then(|| (member.to_ascii_uppercase(), route, handler, "chi"))
        }
        Framework::Gin => {
            const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"];
            METHODS
                .contains(&member)
                .then(|| (member.into(), route, handler, "gin"))
        }
        _ => None,
    }
}

/// What resolves a route's handler expression within one file (#2007).
struct HandlerScope<'a, 't> {
    bindings: &'a HashMap<String, ImportBinding>,
    locals: &'a HashMap<String, String>,
    /// Local variables bound once in the file to a func literal
    /// (`fleet := func(w, r) {…}`).
    closures: &'a HashMap<String, TsNode<'t>>,
}

/// The receiver variable and type of the method enclosing `node`
/// (`func (s *Service) routes()` gives `("s", "Service")`).
fn enclosing_receiver(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<(String, String)> {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "method_declaration" => {
                let receiver = parent.child_by_field_name("receiver")?;
                let declaration = receiver.named_child(0)?;
                let name = cx
                    .text(&declaration.child_by_field_name("name")?)
                    .to_string();
                return Some((name, receiver_type(cx, parent)?));
            }
            "function_declaration" => return None,
            _ => node = parent,
        }
    }
    None
}

/// The handler a route registers (#2007): a function name, an imported
/// package's function, a func literal (its own callable), a local bound
/// once to a func literal, a method value on the enclosing method's
/// receiver, or the handler a wrapper call takes (`wrap("GET", s.list)`)
/// or returns (`relayWrite(true)`, the wrapper itself). Anything else is a
/// Gap.
fn handler_target<'t>(
    cx: &FileCx<'_>,
    handler: TsNode<'t>,
    scope: &HandlerScope<'_, 't>,
    depth: usize,
) -> (HandlerTarget, Option<TsNode<'t>>) {
    let text = cx.text(&handler);
    let gap = || {
        (
            HandlerTarget::Gap {
                expression: text.to_string(),
            },
            None,
        )
    };
    match handler.kind() {
        "func_literal" => (HandlerTarget::Resolved(String::new()), Some(handler)),
        "identifier" => {
            if let Some(symbol) = scope.locals.get(text) {
                (HandlerTarget::Resolved(symbol.clone()), None)
            } else if let Some(literal) = scope
                .closures
                .get(text)
                .filter(|literal| closure_visible(cx, **literal, handler, text))
            {
                (HandlerTarget::Resolved(String::new()), Some(*literal))
            } else {
                gap()
            }
        }
        "selector_expression" => {
            let Some((base, member)) = text.split_once('.') else {
                return gap();
            };
            if member.contains('.') {
                return gap();
            }
            if let Some(package_dir) = scope
                .bindings
                .get(base)
                .and_then(|binding| binding.package_dir.clone())
            {
                return (
                    HandlerTarget::Package {
                        package_dir,
                        exported: member.to_string(),
                    },
                    None,
                );
            }
            match enclosing_receiver(cx, handler) {
                Some((receiver, ty)) if receiver == base => (
                    HandlerTarget::Package {
                        package_dir: package_dir(cx.path),
                        exported: format!("{ty}.{member}"),
                    },
                    None,
                ),
                _ => gap(),
            }
        }
        "call_expression" if depth == 0 => {
            // A wrapper: the handler it takes, else the wrapper itself.
            // The last argument that resolves: `wrap(h, opts)` skips an
            // `opts` that names no handler and takes `h`.
            let taken = call_arguments(handler)
                .into_iter()
                .rev()
                .filter(|a| {
                    matches!(
                        a.kind(),
                        "func_literal" | "identifier" | "selector_expression"
                    )
                })
                .map(|a| handler_target(cx, a, scope, depth + 1))
                .find(|(t, _)| !matches!(t, HandlerTarget::Gap { .. }));
            if let Some(found) = taken {
                return found;
            }
            match handler.child_by_field_name("function") {
                Some(callee) => {
                    let found = handler_target(cx, callee, scope, depth + 1);
                    if matches!(found.0, HandlerTarget::Gap { .. }) {
                        gap()
                    } else {
                        found
                    }
                }
                None => gap(),
            }
        }
        _ => gap(),
    }
}

/// A func literal registered as a route handler is its own callable
/// (#2007): `routes/func@1234` inside `routes`, in the same package.
fn literal_symbol(
    cx: &FileCx<'_>,
    literal: TsNode<'_>,
    functions: &HashMap<usize, String>,
    out: &mut Extraction,
) -> String {
    let enclosing = enclosing_function(literal, functions)
        .and_then(|id| id.rsplit_once('#').map(|(_, name)| name.to_string()))
        .unwrap_or_else(|| "file".to_string());
    let qualified = format!("{enclosing}/func@{}", literal.start_byte());
    let symbol = symbol_id(cx.id.repo, cx.path, &qualified);
    out.nodes.push(Node {
        id: symbol.clone(),
        label: "Symbol".into(),
        props: serde_json::json!({
            "name": qualified,
            "simple_name": qualified,
            "package_dir": package_dir(cx.path),
            "kind": "function",
            "language": "go",
            "func_literal": true,
            "prov": cx.prov(&literal, &format!("Symbol {symbol}")),
        }),
    });
    out.edges.push(Edge {
        src: symbol.clone(),
        dst: file_id(cx.id.repo, cx.path),
        label: "DEFINED_IN".into(),
        props: serde_json::json!({
            "prov": cx.prov(&literal, &format!("DEFINED_IN {symbol}")),
        }),
    });
    symbol
}

/// Whether the local `name`, bound once to `literal` by `name := func…`,
/// is the binding visible at `usage`: the usage follows the declaration
/// inside the block that declares it, and no function between them takes a
/// parameter of that name. A same-named binding in another function is not
/// visible (#2007).
fn closure_visible(cx: &FileCx<'_>, literal: TsNode<'_>, usage: TsNode<'_>, name: &str) -> bool {
    let mut declaration = literal;
    while declaration.kind() != "short_var_declaration" {
        let Some(parent) = declaration.parent() else {
            return false;
        };
        declaration = parent;
    }
    let Some(block) = declaration.parent() else {
        return false;
    };
    if declaration.end_byte() > usage.start_byte()
        || usage.start_byte() < block.start_byte()
        || usage.end_byte() > block.end_byte()
    {
        return false;
    }
    let mut node = usage;
    while let Some(parent) = node.parent() {
        if parent.id() == block.id() {
            return true;
        }
        if matches!(
            parent.kind(),
            "func_literal" | "function_declaration" | "method_declaration"
        ) && parent
            .child_by_field_name("parameters")
            .is_some_and(|parameters| declares(cx, parameters, name))
        {
            return false;
        }
        node = parent;
    }
    false
}

/// Whether a parameter list declares `name`.
fn declares(cx: &FileCx<'_>, parameters: TsNode<'_>, name: &str) -> bool {
    let mut walk = parameters.walk();
    parameters.named_children(&mut walk).any(|parameter| {
        let mut walk = parameter.walk();
        parameter
            .children_by_field_name("name", &mut walk)
            .any(|n| cx.text(&n) == name)
    })
}

/// Locals bound exactly once in the file to a func literal.
fn closures<'t>(cx: &FileCx<'_>, root: TsNode<'t>) -> HashMap<String, TsNode<'t>> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut found = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "short_var_declaration" {
            continue;
        }
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            continue;
        };
        if left.named_child_count() != 1 || right.named_child_count() != 1 {
            continue;
        }
        let (Some(name), Some(value)) = (left.named_child(0), right.named_child(0)) else {
            continue;
        };
        let name = cx.text(&name).to_string();
        *seen.entry(name.clone()).or_default() += 1;
        if value.kind() == "func_literal" {
            found.insert(name, value);
        }
    }
    found.retain(|name, _| seen.get(name) == Some(&1));
    found
}

/// The repo-relative directory a local `replace` target names, resolved
/// against the declaring `go.mod`'s directory `from`. `None` (fail closed:
/// no package directory is provable) when the target leaves the repository
/// root, lexically or through a symlink, or does not exist.
fn replace_target_dir(root: &Path, from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    for part in target.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    let dir = parts.join("/");
    let canonical_root = root.canonicalize().ok()?;
    root.join(&dir)
        .canonicalize()
        .ok()
        .filter(|resolved| resolved.starts_with(&canonical_root))
        .map(|_| dir)
}

/// The modules the repository itself provides (#237, #477).
#[derive(Debug, Default)]
struct RepoModules {
    /// `(module path, repo-relative directory)`: each `go.mod` (nested
    /// modules included) plus each `replace … => ./local` target, whose
    /// directory is resolved against its declaring `go.mod` (`None` when it
    /// leaves the repository or the replace is version-qualified). An import
    /// beneath one of these is inside the system.
    modules: Vec<(String, Option<String>)>,
    /// Module paths some `replace` qualifies with a version
    /// (`replace m v1.2.3 => …`). Go applies such a replacement only to that
    /// version, and no selected version is known here, so nothing — neither
    /// the replacement target nor an in-repository `go.mod` declaring the
    /// path — proves which directory provides the module: it is never
    /// Confirmed internal.
    version_qualified: BTreeSet<String>,
}

fn repo_modules(root: &Path) -> Result<RepoModules, ExtractError> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(name, "vendor" | "node_modules" | "dist" | "build" | "bin")
    };
    let mut out = RepoModules::default();
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if file.name != "go.mod" {
            continue;
        }
        let raw = std::fs::read_to_string(&file.path)?;
        let dir = file
            .rel
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default();
        for line in raw.lines().map(directive) {
            if let Some(module) = line.strip_prefix("module ") {
                out.modules.push((
                    module.trim().trim_matches('"').to_string(),
                    Some(dir.clone()),
                ));
            } else if let Some((from, to)) = line.split_once("=>") {
                let to = to.trim();
                let from = from.trim().trim_start_matches("replace").trim();
                let mut from = from.split_whitespace();
                let module = from.next().unwrap_or("").trim_matches('"');
                if module.is_empty() {
                    continue;
                }
                let version_qualified = from.next().is_some();
                if version_qualified {
                    out.version_qualified.insert(module.to_string());
                }
                if to.starts_with("./") || to.starts_with("../") {
                    let target = to.split_whitespace().next().unwrap_or(to).trim_matches('"');
                    out.modules.push((
                        module.to_string(),
                        (!version_qualified)
                            .then(|| replace_target_dir(root, &dir, target))
                            .flatten(),
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Classify an import no declaration resolved (#237, ADR-0031). Beneath a
/// repository module: an existing package directory is an internal
/// boundary, anything else an explicit Gap. Outside every repository
/// module, in module mode, the import is external — the standard library
/// when its first element has no dot (Go's reserved stdlib namespace).
/// Without a root `go.mod` nothing is provable and the import stays a Gap.
/// A package directory proves nothing unless it, and the Go file found in
/// it, resolve (symlinks followed) inside the canonical repository root.
fn classify_import(
    import_path: &str,
    root: &Path,
    module_path: Option<&str>,
    repo: &RepoModules,
) -> Boundary {
    let beneath = |module: &str| {
        import_path == module
            || import_path
                .strip_prefix(module)
                .is_some_and(|suffix| suffix.starts_with('/'))
    };
    // The most specific module wins (Go resolves an import to its longest
    // matching module path); among equal paths an entry carrying a proven
    // directory beats one that does not, so a `go.mod` or an in-repository
    // `replace` target is probed rather than an escaped replacement.
    if let Some((module, dir)) = repo
        .modules
        .iter()
        .filter(|(module, _)| beneath(module))
        .max_by_key(|(module, dir)| (module.len(), dir.is_some()))
    {
        // Beneath any version-qualified module path, which module provides
        // the package depends on the unknown selected version: fail closed.
        let dir = dir
            .as_ref()
            .filter(|_| !repo.version_qualified.iter().any(|pinned| beneath(pinned)));
        // Only plain path elements are probed: an import path spelling
        // `..`, `.`, or an absolute component never leaves the repository
        // and proves no package directory (it stays a Gap).
        let canonical_root = root.canonicalize().ok();
        let inside = |path: &Path| {
            path.canonicalize().ok().filter(|resolved| {
                canonical_root
                    .as_ref()
                    .is_some_and(|root| resolved.starts_with(root))
            })
        };
        let package_dir = dir.and_then(|dir| {
            let suffix = import_path[module.len()..].trim_start_matches('/');
            Path::new(suffix)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                .then(|| root.join(dir).join(suffix))
        });
        let has_go_source = package_dir
            .and_then(|dir| inside(&dir))
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .is_some_and(|entries| {
                entries.flatten().any(|entry| {
                    let path = entry.path();
                    path.extension().and_then(|ext| ext.to_str()) == Some("go")
                        && inside(&path).is_some_and(|file| file.is_file())
                })
            });
        return if has_go_source {
            Boundary::Internal {
                reason: "package directory in this repository".into(),
                evidence: vec![],
            }
        } else {
            Boundary::Unresolved {
                reason: "import of a repository module path with no package directory".into(),
            }
        };
    }
    if module_path.is_none() {
        return Boundary::Unresolved {
            reason: "import outside module mode (no go.mod) cannot be proven external".into(),
        };
    }
    let first = import_path.split('/').next().unwrap_or(import_path);
    Boundary::External {
        reason: if first.contains('.') {
            "module outside this repository".into()
        } else {
            "Go standard library package".into()
        },
        evidence: vec![],
    }
}

fn extract_source_with_module(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
    module_path: Option<&str>,
) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let cx = FileCx { source, path, id };
    let mut out = Extraction::default();
    out.nodes.push(Node {
        id: file_id(id.repo, path),
        label: "File".into(),
        props: serde_json::json!({
            "path": path,
            "language": "go",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });
    let bindings = parse_imports(&cx, root, module_path, &mut out);
    out.base_bindings = base::bindings(&cx, root, &package_dir(path));
    let receivers = assignment_receivers(&cx, root, &bindings);
    let closures = closures(&cx, root);
    let client = client::Proof::new(&cx, &bindings);

    let function_query =
        crate::queries::query("[(function_declaration) (method_declaration)] @function");
    let mut functions_by_start = HashMap::new();
    let mut locals = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(function_query, root, source);
    while let Some(found) = matches.next() {
        let function = found.captures()[0].node;
        let Some((simple, qualified)) = function_name(&cx, function) else {
            continue;
        };
        let symbol = symbol_id(id.repo, path, &qualified);
        functions_by_start.insert(function.start_byte(), symbol.clone());
        out.handler_functions.extend(vercel::handler_function(
            &cx, function, &symbol, &simple, &bindings,
        ));
        if qualified == simple {
            locals.insert(simple.clone(), symbol.clone());
        }
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "simple_name": simple,
                "package_dir": package_dir(path),
                "kind": "function",
                "language": "go",
                "prov": cx.prov(&function, &format!("Symbol {symbol}")),
            }),
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("DEFINED_IN {symbol}")),
            }),
        });
    }

    // Func literals that route handlers resolve to are callables of their
    // own (#2007). Register them before any call or literal is attributed,
    // so the calls inside a closure are owned by it, not by its encloser.
    {
        let scope = HandlerScope {
            bindings: &bindings,
            locals: &locals,
            closures: &closures,
        };
        let call_query = crate::queries::query("(call_expression) @call");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(call_query, root, source);
        let mut literals = Vec::new();
        while let Some(found) = matches.next() {
            let call = found.captures()[0].node;
            let Some(function) = call.child_by_field_name("function") else {
                continue;
            };
            if let Some((_, _, handler_node, _)) =
                endpoint_registration(&cx, call, cx.text(&function), &bindings, &receivers)
                && let (_, Some(literal)) = handler_target(&cx, handler_node, &scope, 0)
            {
                literals.push(literal);
            }
        }
        // Outer literals first, so a nested one is named inside its encloser.
        literals.sort_by_key(|literal| literal.start_byte());
        literals.dedup_by_key(|literal| literal.start_byte());
        for literal in literals {
            let symbol = literal_symbol(&cx, literal, &functions_by_start, &mut out);
            functions_by_start.insert(literal.start_byte(), symbol);
        }
    }

    // SDK request and config structs (AC-0462).
    let literal_query = crate::queries::query("(composite_literal) @literal");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(literal_query, root, source);
    while let Some(found) = matches.next() {
        let literal = found.captures()[0].node;
        let owner = enclosing_function(literal, &functions_by_start);
        out.event_sites
            .extend(events::site(&cx, &bindings, literal, owner));
    }

    let mut config_reads = Vec::new();
    let call_query = crate::queries::query("(call_expression) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(call_query, root, source);
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let Some(function) = call.child_by_field_name("function") else {
            continue;
        };
        let callee = cx.text(&function);
        if let Some((method, route, handler_node, framework)) =
            endpoint_registration(&cx, call, callee, &bindings, &receivers)
        {
            let scope = HandlerScope {
                bindings: &bindings,
                locals: &locals,
                closures: &closures,
            };
            let (mut handler, literal) = handler_target(&cx, handler_node, &scope, 0);
            if let Some(literal) = literal {
                let symbol = functions_by_start
                    .get(&literal.start_byte())
                    .expect("handler literals are registered before calls")
                    .clone();
                handler = HandlerTarget::Resolved(symbol);
            }
            let endpoint = format!("ep:{}@{method}:{route}", id.repo);
            let mut endpoint_node = Node {
                id: endpoint.clone(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": method,
                    "path": route,
                    "framework": framework,
                    "language": "go",
                    "prov": cx.prov(&call, &format!("Endpoint {endpoint}")),
                }),
            };
            let mut handles = Edge {
                src: endpoint.clone(),
                dst: String::new(),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": cx.prov(&call, &format!("HANDLES {endpoint}")),
                }),
            };
            // The Gap form of this registration: what an unresolved handler,
            // or a package handler no Symbol answers, is recorded as.
            let gap_form = |mut endpoint_node: Node, mut handles: Edge, expression: &str| {
                let gap_id = format!("gap:go-handler:{}@{}@{}", id.repo, path, call.start_byte());
                endpoint_node.props["handler_sym"] = gap_id.clone().into();
                let gap = Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "handler_expression": expression,
                        "reason": "unresolved Go route handler expression",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                };
                handles.dst = gap_id;
                handles.props = serde_json::json!({
                    "reason": "unresolved Go route handler expression",
                    "attempted_resolution": "go-handler-expression",
                    "prov": cx.prov_with_confidence(
                        &call,
                        ConfidenceTier::Gap,
                        &format!("HANDLES {} -> {}", endpoint, handles.dst),
                    ),
                });
                (endpoint_node, gap, handles)
            };
            match handler {
                HandlerTarget::Resolved(handler) => {
                    endpoint_node.props["handler_sym"] = handler.clone().into();
                    handles.dst = handler;
                    if handler_node.kind() == "call_expression" {
                        handles.props["via"] = cx.text(&handler_node).into();
                    }
                    out.nodes.push(endpoint_node);
                    out.edges.push(handles);
                }
                HandlerTarget::Package {
                    package_dir,
                    exported,
                } => out.pending_endpoints.push(PendingEndpoint {
                    edge: {
                        if handler_node.kind() == "call_expression" {
                            handles.props["via"] = cx.text(&handler_node).into();
                        }
                        handles.clone()
                    },
                    gap: gap_form(
                        endpoint_node.clone(),
                        handles.clone(),
                        cx.text(&handler_node),
                    ),
                    node: endpoint_node,
                    package_dir,
                    exported,
                }),
                HandlerTarget::Gap { expression } => {
                    let (endpoint_node, gap, handles) =
                        gap_form(endpoint_node, handles, &expression);
                    out.nodes.push(endpoint_node);
                    out.nodes.push(gap);
                    out.edges.push(handles);
                }
            }
        }

        let owner = enclosing_function(call, &functions_by_start);
        if let Some((site, url)) = client.fetch_site_and_url(call, owner.clone()) {
            // A base held in a field or constant waits for the package (#2065).
            match base::pending(&cx, site, url, call, &package_dir(path), |n| {
                client.classify(n)
            }) {
                Ok(held) => out.pending_bases.push(held),
                Err(site) => out.fetch_sites.push(*site),
            }
        }
        config_reads.extend(config_read::site(&cx, &bindings, call, owner.clone()));

        let Some(src) = owner else {
            continue;
        };
        if !callee.contains('.') {
            if let Some(dst) = locals.get(callee) {
                if src != *dst {
                    out.edges.push(Edge {
                        src,
                        dst: dst.clone(),
                        label: "CALLS".into(),
                        props: serde_json::json!({
                            "prov": cx.prov(&call, &format!("CALLS {callee}")),
                        }),
                    });
                }
            } else if callee.chars().all(|c| c.is_alphanumeric() || c == '_') {
                // A function of the same package in another file (#2019).
                out.pending_calls.push(PendingCall {
                    src,
                    package_dir: package_dir(path),
                    exported: callee.to_string(),
                    props: serde_json::json!({
                        "resolution": "go-package-directory-proven",
                        "prov": cx.prov(&call, &format!("CALLS {callee}")),
                    }),
                });
            }
            continue;
        }
        let Some((base, exported)) = callee.split_once('.') else {
            continue;
        };
        // An imported package's function, or a method on the enclosing
        // method's receiver (`s.save()` in `func (s *Service)`, #2007).
        let (package_dir, exported) = match bindings
            .get(base)
            .and_then(|binding| binding.package_dir.clone())
        {
            Some(package_dir) => (package_dir, exported.to_string()),
            None => match enclosing_receiver(&cx, call) {
                Some((receiver, ty)) if receiver == base && !exported.contains('.') => {
                    (package_dir(path), format!("{ty}.{exported}"))
                }
                _ => continue,
            },
        };
        out.pending_calls.push(PendingCall {
            src,
            package_dir,
            exported,
            props: serde_json::json!({
                "resolution": "go-module-directory-proven",
                "prov": cx.prov(&call, &format!("CALLS {callee}")),
            }),
        });
    }
    config_read::emit(&cx, &config_reads, &mut out);
    // Guarded exits and refusal responses (#2009); test code states no
    // business rules.
    if path.ends_with("_test.go") {
        // A test file states its tests, not the system (#2019).
        target_tests::extract(&cx, root, &bindings, &mut out);
        target_tests::keep_test_facts(&mut out);
    } else {
        rule_evidence::extract(&cx, root, &functions_by_start, &bindings, &mut out);
        sql::extract(&cx, root, &functions_by_start, &bindings, &mut out);
        gorm::extract(&cx, root, &functions_by_start, &bindings, &mut out);
    }
    Ok(out)
}

/// Recover deterministic facts from one Go source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let mut out = extract_source_with_module(source, path, id, None)?;
    // One file is its own package here (#2065).
    let held = std::mem::take(&mut out.pending_bases);
    let joined = base::join(held, &std::mem::take(&mut out.base_bindings));
    out.fetch_sites.extend(joined);
    Ok(out)
}

fn skips_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "vendor" | "node_modules" | "dist" | "build" | "bin")
}

/// Why recovery skips an in-scope, non-test Go file (#1258).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// A `_GOOS` or `_GOARCH` file-name suffix.
    PlatformSuffix,
    /// A `//go:build` or `// +build` line before the package clause.
    BuildConstraint,
}

/// A Go file's repo-relative path, with why recovery skips it (`None`: read).
type WalkedGoFile = (String, Option<SkipReason>);

/// Walk `root` exactly as recovery does: each non-test Go file in walk order,
/// with the reason recovery skips it, or `None` when recovery reads it.
/// `on_step` runs before each directory and each Go file is read, with its
/// repo-relative path; a break stops the walk and returns `None`.
fn walk_go_files(
    root: &Path,
    tests: bool,
    on_step: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Option<Vec<WalkedGoFile>>, ExtractError> {
    let Some(files) = source_walk::files_until(
        root,
        &skips_dir,
        source_walk::Gitignores::Honor,
        &mut |step| on_step(step.dir),
    )?
    else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for file in files {
        if file
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("go")
            || (!tests && file.name.ends_with("_test.go"))
        {
            continue;
        }
        if on_step(&file.rel).is_break() {
            return Ok(None);
        }
        let reason = if has_platform_suffix(&file.name) {
            Some(SkipReason::PlatformSuffix)
        } else if has_explicit_build_constraint(&std::fs::read(&file.path)?) {
            Some(SkipReason::BuildConstraint)
        } else {
            None
        };
        out.push((file.rel, reason));
    }
    Ok(Some(out))
}

/// The Go files recovery skips under `root`, with their reasons, in walk
/// order, so preflight can disclose them (#1258). `on_step` is as for the
/// walk: a break returns `None`.
pub fn skipped_files_until(
    root: &Path,
    on_step: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> Result<Option<Vec<(String, SkipReason)>>, ExtractError> {
    Ok(walk_go_files(root, false, on_step)?.map(|files| {
        files
            .into_iter()
            .filter_map(|(rel, reason)| Some((rel, reason?)))
            .collect()
    }))
}

fn collect_go_files(root: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
    out.extend(
        walk_go_files(root, true, &mut |_| ControlFlow::Continue(()))?
            .expect("a hook that never breaks walks the whole tree")
            .into_iter()
            .filter(|(_, reason)| reason.is_none())
            .map(|(rel, _)| rel),
    );
    Ok(())
}

fn has_explicit_build_constraint(source: &[u8]) -> bool {
    let text = String::from_utf8_lossy(source);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("//go:build") || line.starts_with("// +build") {
            return true;
        }
        if line.starts_with("package ") || (!line.is_empty() && !line.starts_with("//")) {
            break;
        }
    }
    false
}

fn has_platform_suffix(name: &str) -> bool {
    const GOOS: &[&str] = &[
        "aix",
        "android",
        "darwin",
        "dragonfly",
        "freebsd",
        "illumos",
        "ios",
        "js",
        "linux",
        "netbsd",
        "openbsd",
        "plan9",
        "solaris",
        "wasip1",
        "windows",
    ];
    const GOARCH: &[&str] = &[
        "386", "amd64", "arm", "arm64", "loong64", "mips", "mips64", "mips64le", "mipsle", "ppc64",
        "ppc64le", "riscv64", "s390x", "wasm",
    ];
    let stem = name.strip_suffix(".go").unwrap_or(name);
    let mut parts = stem.rsplit('_');
    let last = parts.next().unwrap_or("");
    GOOS.contains(&last) || GOARCH.contains(&last)
}

/// One `go.mod` line without its trailing `//` comment (module paths never
/// contain `//`), trimmed.
fn directive(line: &str) -> &str {
    line.split("//").next().unwrap_or(line).trim()
}

/// The ignore rules from `root`'s own `.gitignore`, matching the shared walk
/// (ADR-0030): only the root's own file counts, never an ancestor's.
fn root_gitignore(root: &Path) -> Result<source_walk::IgnoreRules, ExtractError> {
    let path = root.join(source_walk::GITIGNORE);
    // Match source_walk::files: only a regular-file .gitignore is loaded. A
    // symlinked .gitignore is skipped without following it, so retargeting
    // the symlink can't change which facts this recovers for a given commit.
    let bytes = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => match std::fs::File::open(&path) {
            Ok(file) => Some(source_walk::read_gitignore(file)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        },
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(source_walk::IgnoreRules::default().descend("", bytes.as_deref()))
}

fn module_path(root: &Path) -> Result<Option<String>, ExtractError> {
    // An ignored `go.mod` is treated as absent, not read: a direct read is
    // otherwise the one thing in this adapter that bypasses the shared walk's
    // `.gitignore` rules (#468, ADR-0034).
    if root_gitignore(root)?.is_ignored("go.mod", false) {
        return Ok(None);
    }
    let path = root.join("go.mod");
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(raw.lines().find_map(|line| {
        directive(line)
            .strip_prefix("module ")
            .map(|module| module.trim().trim_matches('"'))
            .filter(|module| !module.is_empty())
            .map(str::to_string)
    }))
}

/// Recover a Go directory with content-addressed per-file parse reuse.
pub fn extract_dir_incremental(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    extract_dir_incremental_with_progress(root, id, cache, &mut |_| {})
}

/// Same as [`extract_dir_incremental`], calling `on_file` with each file's
/// repo-relative path as it's read (#209 live progress hook).
pub fn extract_dir_incremental_with_progress(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let module_path = module_path(root)?;
    let mut files = Vec::new();
    collect_go_files(root, &mut files)?;
    files.sort();
    let active = files.iter().cloned().collect::<BTreeSet<_>>();
    let mut stats = IncrementalStats {
        deleted_files: cache
            .files
            .keys()
            .filter(|path| !active.contains(*path))
            .count() as u64,
        ..IncrementalStats::default()
    };
    cache.files.retain(|path, _| active.contains(path));
    let mut out = Extraction::default();
    // Files parse on parallel workers (#236); results merge here in sorted
    // order, so the output is byte-identical to a serial run.
    // Workers see only each cached file's hash; the merge owns the cache and
    // replaces entries one at a time, exactly as the serial loop did, so a
    // re-ingest never holds a second copy of the cache.
    let previous: std::collections::BTreeMap<String, String> = cache
        .files
        .iter()
        .map(|(path, cached)| (path.clone(), cached.source_hash.clone()))
        .collect();
    let merged = source_walk::parallel::map_ordered(
        &files,
        |path| {
            let source = std::fs::read(root.join(path))?;
            let source_hash = core_prov::content_hash(&source);
            let context_hash = core_prov::content_hash(
                format!("{source_hash}\0{}", module_path.as_deref().unwrap_or("")).as_bytes(),
            );
            let reusable = previous.get(path).is_some_and(|hash| *hash == context_hash);
            let fresh = if reusable {
                None
            } else {
                Some(extract_source_with_module(
                    &source,
                    path,
                    id,
                    module_path.as_deref(),
                )?)
            };
            Ok::<_, ExtractError>((context_hash, fresh))
        },
        |path, (context_hash, fresh)| {
            on_file(path);
            let extraction = match fresh {
                Some(extraction) => {
                    stats.recomputed_files += 1;
                    extraction
                }
                None => {
                    stats.reused_files += 1;
                    let mut extraction = cache.files[path].extraction.clone();
                    retarget_commit(&mut extraction, id.commit);
                    extraction
                }
            };
            cache.files.insert(
                path.to_string(),
                CachedFile {
                    source_hash: context_hash,
                    extraction: extraction.clone(),
                },
            );
            out.nodes.extend(extraction.nodes);
            out.edges.extend(extraction.edges);
            out.fetch_sites.extend(extraction.fetch_sites);
            out.event_sites.extend(extraction.event_sites);
            out.pending_calls.extend(extraction.pending_calls);
            out.pending_endpoints.extend(extraction.pending_endpoints);
            out.handler_functions.extend(extraction.handler_functions);
            out.pending_bases.extend(extraction.pending_bases);
            out.base_bindings.extend(extraction.base_bindings);
            out.test_requests.extend(extraction.test_requests);
            out.pending_gorm.extend(extraction.pending_gorm);
            out.gorm_table_names.extend(extraction.gorm_table_names);
            Ok(())
        },
    );
    merged?;
    // By simple name (`pkg.Func`) and by qualified name (`Type.Method`, a
    // method value on a receiver, #2007).
    // A test file's functions answer only calls from test files, and never
    // shadow the package's own (#2019).
    // An unqualified call names a package-level function, never a method,
    // so `functions` leaves out `Type.Method` symbols.
    let index_of = |tests: bool, functions: bool| {
        out.nodes
            .iter()
            .filter(|node| node.label == "Symbol")
            .filter(move |node| tests || !in_test_file(&node.id))
            .filter(move |node| {
                !functions || !node.id.rsplit('#').next().unwrap_or("").contains('.')
            })
            .flat_map(|node| {
                let dir = node.props.get("package_dir").and_then(|d| d.as_str());
                let test = in_test_file(&node.id);
                ["simple_name", "name"].into_iter().filter_map(move |key| {
                    Some((
                        (dir?.to_string(), node.props.get(key)?.as_str()?.to_string()),
                        (test, node.id.clone()),
                    ))
                })
            })
            .fold(
                BTreeMap::new(),
                |mut index: BTreeMap<_, (bool, String)>, (key, value)| {
                    match index.get(&key) {
                        Some((false, _)) if value.0 => {}
                        _ => {
                            index.insert(key, value);
                        }
                    }
                    index
                },
            )
            .into_iter()
            .map(|(key, (_, id))| (key, id))
            .collect::<BTreeMap<_, _>>()
    };
    let test_index = index_of(true, false);
    let symbol_index = index_of(false, false);
    let test_functions = index_of(true, true);
    let package_functions = index_of(false, true);
    for pending in std::mem::take(&mut out.pending_calls) {
        let unqualified = pending.props["resolution"] == "go-package-directory-proven";
        let index = match (in_test_file(&pending.src), unqualified) {
            (true, true) => &test_functions,
            (true, false) => &test_index,
            (false, true) => &package_functions,
            (false, false) => &symbol_index,
        };
        if let Some(dst) = index.get(&(pending.package_dir, pending.exported)) {
            out.edges.push(Edge {
                src: pending.src,
                dst: dst.clone(),
                label: "CALLS".into(),
                props: pending.props,
            });
        }
    }
    for mut pending in std::mem::take(&mut out.pending_endpoints) {
        if let Some(dst) = symbol_index.get(&(pending.package_dir, pending.exported)) {
            pending.node.props["handler_sym"] = dst.clone().into();
            pending.edge.dst = dst.clone();
            out.nodes.push(pending.node);
            out.edges.push(pending.edge);
        } else {
            let (endpoint, gap, handles) = pending.gap;
            out.nodes.push(endpoint);
            out.nodes.push(gap);
            out.edges.push(handles);
        }
    }
    let held = std::mem::take(&mut out.pending_bases);
    let joined = base::join(held, &std::mem::take(&mut out.base_bindings));
    out.fetch_sites.extend(joined);
    let functions = std::mem::take(&mut out.handler_functions);
    vercel::routes(root, id.repo, id.commit, &functions, &mut out);
    target_tests::resolve(&mut out, id.repo);
    sql::merge(&mut out);
    gorm::resolve(&mut out);
    let modules = repo_modules(root)?;
    let Extraction { nodes, edges, .. } = &mut out;
    core_graph::placeholder::close_over_endpoints(
        nodes,
        edges,
        EXTRACTOR_ID,
        core_graph::placeholder::label_for_id,
        |endpoint, _| {
            let import_path = endpoint.strip_prefix("mod:")?;
            Some(classify_import(
                import_path,
                root,
                module_path.as_deref(),
                &modules,
            ))
        },
    );
    Ok((out, stats))
}

/// Recover a Go directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

mod base;
mod client;
mod config_read;
mod events;
mod gorm;
mod queries;
mod rule_evidence;
mod sql;
mod target_tests;
mod vercel;

#[cfg(test)]
mod tests;
