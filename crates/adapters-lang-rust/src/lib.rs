//! Rust deterministic (T0) language adapter (#999).
//!
//! Files, symbols, `use` imports and deterministic call edges become the
//! server graph, with Cargo-workspace-aware module resolution. A call
//! T0 cannot place is never guessed: a workspace path that should resolve
//! and does not is an explicit Gap, while calls T0 has no claim on
//! (methods on other receivers, trait objects, closures, prelude and
//! external items) are left out, as the Go and Python adapters do.

use core_graph::placeholder::Boundary;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use http::{ActixAttrRoute, ActixServiceMount, AxumRouterDef, PendingDirectEndpoint};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-rust";

/// Rust extraction errors.
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

/// Rust graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    pending_calls: Vec<PendingCall>,
    axum_routers: Vec<AxumRouterDef>,
    actix_attr_routes: Vec<ActixAttrRoute>,
    actix_service_mounts: Vec<ActixServiceMount>,
    pending_endpoints: Vec<PendingDirectEndpoint>,
    /// reqwest client calls, stitched to endpoints by the host (#999).
    pub fetch_sites: Vec<adapters_fw::client::FetchSite>,
    /// rdkafka producer and consumer sites (#1371).
    pub event_sites: Vec<adapters_fw::events::EventSite>,
    /// Requests tests build, joined to endpoints once every file is read.
    test_requests: Vec<target_tests::TestRequest>,
    handler_params: Vec<auth::HandlerParams>,
    extractors: Vec<auth::Extractor>,
    deny_fns: Vec<auth::DenyFn>,
}

/// How a pending call resolves once every file is known.
#[derive(Debug, Clone)]
enum Target {
    /// Absolute logical path, e.g. `alpha::a::helper` or
    /// `alpha::a::Widget::new` (type segment before the method).
    Path(Vec<String>),
    /// `Type::method` / `self.method()` in the caller's own crate.
    Method {
        crate_name: String,
        type_name: String,
        method: String,
    },
    /// Absolute paths a relative or glob-imported name could mean. It
    /// resolves only when exactly one names a symbol. A miss is a Gap only
    /// when `gap_on_miss` holds and a candidate's parent is a module of this
    /// workspace (`sub::f()` with a `sub` module), never for `Type::new()`
    /// on a type T0 cannot see or an external crate path.
    Candidates {
        paths: Vec<Vec<String>>,
        gap_on_miss: bool,
    },
}

#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    target: Target,
    props: serde_json::Value,
    gap_node: Node,
    gap_edge: Edge,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Rust parse cache.
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Rust source work performed by an incremental extraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// New or byte-changed files parsed.
    pub recomputed_files: u64,
    /// Content-identical files reused.
    pub reused_files: u64,
    /// Cached files removed because the source disappeared.
    pub deleted_files: u64,
}

fn language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

/// Process-wide compiled-query cache (#897): each static pattern is
/// compiled once, never per file (`Query` is `Send + Sync`).
fn query(pattern: &'static str) -> &'static Query {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static Query>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
    cache.entry(pattern).or_insert_with(|| {
        Box::leak(Box::new(
            Query::new(&language(), pattern).expect("static query"),
        ))
    })
}

struct FileCx<'a> {
    source: &'a [u8],
    path: &'a str,
    id: &'a SourceId<'a>,
    /// Module segments of this file, crate name first.
    module: Vec<String>,
    crate_name: String,
    workspace_crates: BTreeSet<String>,
    /// Names of modules this file declares (`mod foo;` / `mod foo {}`), so
    /// a 2018-edition `use foo::x` resolves locally rather than as a crate.
    declared_mods: BTreeSet<String>,
    /// Whether the file is a module an out-of-line `#[cfg(test)] mod x;`
    /// declares, or one nested in it (#2018).
    test_module: bool,
}

impl FileCx<'_> {
    fn text(&self, node: &TsNode<'_>) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }
    fn evidence(&self, node: &TsNode<'_>) -> EvidenceRef {
        EvidenceRef {
            repo: self.id.repo.into(),
            path: self.path.into(),
            byte_start: node.start_byte() as u64,
            byte_end: node.end_byte() as u64,
            commit_sha: self.id.commit.into(),
        }
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
        prov_from_evidence(&self.evidence(node), confidence, fact)
    }
}

fn prov_from_evidence(
    evidence: &EvidenceRef,
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        vec![evidence.clone()],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic confidence is within its ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
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
        retarget_props_commit(&mut pending.gap_node.props, commit);
        retarget_props_commit(&mut pending.gap_edge.props, commit);
    }
    http::retarget_commit(extraction, commit);
    auth::retarget(extraction, commit);
    target_tests::retarget(&mut extraction.test_requests, commit);
}

fn segs(text: &str) -> Vec<String> {
    text.split("::")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Resolve leading `crate`/`self`/`super`/workspace-crate segments to an
/// absolute logical path from `scope`, the module (inline modules
/// included) the path is written in. Returns `(segments, is_external)`.
fn resolve_path(cx: &FileCx<'_>, scope: &[String], raw: &[String]) -> (Vec<String>, bool) {
    if raw.is_empty() {
        return (Vec::new(), false);
    }
    match raw[0].as_str() {
        "crate" => {
            let mut out = vec![cx.crate_name.clone()];
            out.extend(raw[1..].iter().cloned());
            (out, false)
        }
        "self" => {
            let mut out = scope.to_vec();
            out.extend(raw[1..].iter().cloned());
            (out, false)
        }
        "super" => {
            let mut base = scope.to_vec();
            let mut rest = raw;
            while rest.first().map(String::as_str) == Some("super") {
                base.pop();
                rest = &rest[1..];
            }
            if rest.first().map(String::as_str) == Some("self") {
                rest = &rest[1..];
            }
            base.extend(rest.iter().cloned());
            (base, false)
        }
        first if cx.declared_mods.contains(first) => {
            let mut out = scope.to_vec();
            out.extend(raw.iter().cloned());
            (out, false)
        }
        first if cx.workspace_crates.contains(first) => (raw.to_vec(), false),
        _ => (raw.to_vec(), true),
    }
}

#[derive(Debug, Clone)]
struct ImportItem {
    /// Path segments exactly as written (expanded from groups).
    written: Vec<String>,
    alias: Option<String>,
    glob: bool,
}

fn expand_use(node: TsNode<'_>, cx: &FileCx<'_>, prefix: &[String], out: &mut Vec<ImportItem>) {
    match node.kind() {
        "use_list" => {
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                expand_use(child, cx, prefix, out);
            }
        }
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .unwrap_or_else(|| node.named_child(0).expect("scoped use path"));
            let mut next = prefix.to_vec();
            next.extend(segs(cx.text(&path)));
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                if child.id() != path.id() {
                    expand_use(child, cx, &next, out);
                }
            }
        }
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .unwrap_or_else(|| node.named_child(0).expect("use path"));
            let alias = node
                .child_by_field_name("alias")
                .map(|n| cx.text(&n).to_string());
            let mut written = prefix.to_vec();
            written.extend(segs(cx.text(&path)));
            out.push(ImportItem {
                written,
                alias,
                glob: false,
            });
        }
        "use_wildcard" => {
            let mut written = prefix.to_vec();
            if let Some(child) = node.named_child(0) {
                written.extend(segs(cx.text(&child)));
            }
            out.push(ImportItem {
                written,
                alias: None,
                glob: true,
            });
        }
        _ => {
            let mut written = prefix.to_vec();
            written.extend(segs(cx.text(&node)));
            if !written.is_empty() {
                out.push(ImportItem {
                    written,
                    alias: None,
                    glob: false,
                });
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Bindings {
    /// (module scope, local name) -> (absolute segments, is_external). A
    /// `use` is visible only in the module that declares it.
    map: HashMap<(String, String), (Vec<String>, bool)>,
    /// Module scope -> workspace glob import prefixes, tried for names no
    /// `use` binds.
    globs: HashMap<String, Vec<Vec<String>>>,
    /// Module scope -> external glob import prefixes (`use axum::routing::*`).
    external_globs: HashMap<String, Vec<Vec<String>>>,
    /// (visible scope, name) of every item declared there, any kind: a
    /// nearer item shadows outer `use`s and globs of the same name.
    items: BTreeSet<(String, String)>,
}

impl Bindings {
    /// The binding visible from `scopes` (innermost first).
    fn get(&self, scopes: &[String], name: &str) -> Option<&(Vec<String>, bool)> {
        scopes
            .iter()
            .find_map(|scope| self.map.get(&(scope.clone(), name.to_string())))
    }
    /// Every workspace glob prefix visible from `scopes`.
    fn globs(&self, scopes: &[String]) -> Vec<Vec<String>> {
        scopes
            .iter()
            .filter_map(|scope| self.globs.get(scope))
            .flatten()
            .cloned()
            .collect()
    }
}

/// The scopes a name written at `node` can see, innermost first: each
/// enclosing block (`<module>::<block@byte>`) and function body
/// (`<module>::<fn@byte>`), never a module path, so function- and
/// block-local items and `use`s stay out of module and re-export lookups;
/// then the module.
fn visible_scopes(cx: &FileCx<'_>, node: TsNode<'_>) -> Vec<String> {
    let module = scope_of(cx, node).join("::");
    let mut scopes = Vec::new();
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "function_item" => scopes.push(format!("{module}::<fn@{}>", parent.start_byte())),
            // A function body is the function scope itself.
            "block" if parent.parent().is_none_or(|p| p.kind() != "function_item") => {
                scopes.push(format!("{module}::<block@{}>", parent.start_byte()));
            }
            _ => {}
        }
        current = parent;
    }
    scopes.push(module);
    scopes
}

/// How a bare name resolves from `scopes`, searched innermost first with
/// items and `use`s of one scope considered together.
enum NameResolution<'a> {
    Local(&'a String),
    Import(&'a (Vec<String>, bool)),
    /// Only workspace glob imports can bring the name in.
    Globs(Vec<Vec<String>>),
    /// An inner glob may or may not shadow an outer binding: unprovable.
    Ambiguous,
    None,
}

fn resolve_name<'a>(
    scopes: &[String],
    name: &str,
    locals: &'a HashMap<(String, String), String>,
    bindings: &'a Bindings,
) -> NameResolution<'a> {
    let mut globs = Vec::new();
    for scope in scopes {
        let key = (scope.clone(), name.to_string());
        let found = match (locals.get(&key), bindings.map.get(&key)) {
            (Some(local), _) => Some(NameResolution::Local(local)),
            (None, Some(import)) => Some(NameResolution::Import(import)),
            (None, None) => None,
        };
        if let Some(found) = found {
            return if globs.is_empty() {
                found
            } else {
                NameResolution::Ambiguous
            };
        }
        if let Some(prefixes) = bindings.globs.get(scope) {
            globs.extend(prefixes.iter().cloned());
        }
    }
    if globs.is_empty() {
        NameResolution::None
    } else {
        NameResolution::Globs(globs)
    }
}

fn parse_imports(cx: &FileCx<'_>, root: TsNode<'_>, out: &mut Extraction) -> Bindings {
    let q = query("(use_declaration) @use");
    let mut bindings = Bindings::default();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(q, root, cx.source);
    while let Some(found) = matches.next() {
        let decl = found.captures()[0].node;
        // `argument`, not the first child: `pub use` starts with a
        // visibility modifier.
        let Some(clause) = decl.child_by_field_name("argument") else {
            continue;
        };
        let mut items = Vec::new();
        expand_use(clause, cx, &[], &mut items);
        let scope = scope_of(cx, decl);
        // A `use` inside a function body binds only within that function.
        let scope_key = visible_scopes(cx, decl).swap_remove(0);
        for item in items {
            let (resolved, external) = resolve_path(cx, &scope, &item.written);
            let specifier = if item.glob {
                format!("{}::*", item.written.join("::"))
            } else {
                item.written.join("::")
            };
            let mut props = serde_json::json!({
                "specifier": specifier,
                "resolved": resolved.join("::"),
                "external": external,
                "scope": scope_key,
                "prov": cx.prov(&decl, &format!("IMPORTS {}", cx.text(&decl))),
            });
            // Workspace imports are keyed by the absolute path, so
            // `super::a` from two files does not collapse into one module.
            let target = if external {
                specifier.clone()
            } else if item.glob {
                format!("{}::*", resolved.join("::"))
            } else {
                resolved.join("::")
            };
            if item.glob {
                props["glob"] = serde_json::json!(true);
                if external {
                    bindings
                        .external_globs
                        .entry(scope_key.clone())
                        .or_default()
                        .push(resolved);
                } else {
                    bindings
                        .globs
                        .entry(scope_key.clone())
                        .or_default()
                        .push(resolved);
                }
            } else {
                let local = item
                    .alias
                    .clone()
                    .or_else(|| item.written.last().cloned())
                    .unwrap_or_default();
                if !local.is_empty() {
                    props["binds"] = serde_json::json!(local);
                    bindings
                        .map
                        .insert((scope_key.clone(), local), (resolved, external));
                }
            }
            out.edges.push(Edge {
                src: file_id(cx.id.repo, cx.path),
                dst: format!("mod:{target}"),
                label: "IMPORTS".into(),
                props,
            });
        }
    }
    let q = query(
        "[(function_item) (struct_item) (enum_item) (union_item) (trait_item) (type_item) \
          (const_item) (static_item) (mod_item) (macro_definition)] @item",
    );
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(q, root, cx.source);
    while let Some(found) = matches.next() {
        let item = found.captures()[0].node;
        if let Some(name) = item.child_by_field_name("name") {
            let scope = visible_scopes(cx, item).swap_remove(0);
            bindings.items.insert((scope, cx.text(&name).to_string()));
        }
    }
    bindings
}

/// The module `node` is written in: the file's module plus its inline ones.
fn scope_of(cx: &FileCx<'_>, node: TsNode<'_>) -> Vec<String> {
    let mut scope = cx.module.clone();
    scope.extend(inline_modules(cx, node));
    scope
}

/// Inline `mod foo {}` segments enclosing `node`, outermost first.
fn inline_modules(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Vec<String> {
    let mut mods = Vec::new();
    while let Some(parent) = node.parent() {
        if parent.kind() == "mod_item"
            && let Some(name) = parent.child_by_field_name("name")
        {
            mods.push(cx.text(&name).to_string());
        }
        node = parent;
    }
    mods.reverse();
    mods
}

fn impl_type(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "impl_item" {
            let ty = parent.child_by_field_name("type")?;
            let text = cx.text(&ty);
            let base = text.split('<').next().unwrap_or(text).trim();
            return base.rsplit("::").next().map(|s| s.trim().to_string());
        }
        node = parent;
    }
    None
}

/// The trait of the `impl Trait for Type` enclosing `node`, whitespace
/// removed (`From<io::Error>`), so two trait impls on one type keep
/// distinct method identities.
fn impl_trait(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "impl_item" {
            let tr = parent.child_by_field_name("trait")?;
            return Some(cx.text(&tr).split_whitespace().collect());
        }
        node = parent;
    }
    None
}

/// The enclosing functions and blocks of `node`, outermost first, as symbol
/// id segments: each function's name, and `<blockN>` for a block that is not
/// a function body, numbered in source order within its function so sibling
/// blocks declaring the same name keep distinct identities.
fn enclosing_function_names(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Vec<String> {
    let mut names = Vec::new();
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "function_item" => {
                if let Some(name) = parent.child_by_field_name("name") {
                    names.push(cx.text(&name).to_string());
                }
            }
            "block" if parent.parent().is_none_or(|p| p.kind() != "function_item") => {
                names.push(format!("<block{}>", block_ordinal(parent)));
            }
            _ => {}
        }
        node = parent;
    }
    names.reverse();
    names
}

/// 1-based source-order position of `block` among the non-body blocks of its
/// nearest enclosing function (or of the file outside any function).
fn block_ordinal(block: TsNode<'_>) -> usize {
    let mut owner = block;
    while let Some(parent) = owner.parent() {
        owner = parent;
        if owner.kind() == "function_item" {
            break;
        }
    }
    let mut count = 0;
    let mut stack = vec![owner];
    while let Some(node) = stack.pop() {
        if node.kind() == "block"
            && node.start_byte() <= block.start_byte()
            && node.parent().is_none_or(|p| p.kind() != "function_item")
        {
            count += 1;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    count
}

fn trait_type(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "trait_item" {
            return parent
                .child_by_field_name("name")
                .map(|n| cx.text(&n).to_string());
        }
        node = parent;
    }
    None
}

fn gap_pair(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    src: &str,
    callee: &str,
    reason: &str,
) -> (Node, Edge) {
    let gap_id = format!("gap:call:{}@{}@{}", cx.id.repo, cx.path, call.start_byte());
    (
        Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "callee": callee,
                "reason": reason,
                "attempted_tiers": ["T0"],
                "prov": cx.prov_with_confidence(&call, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
            }),
        },
        Edge {
            src: src.to_string(),
            dst: gap_id.clone(),
            label: "CALLS".into(),
            props: serde_json::json!({
                "reason": reason,
                "attempted_resolution": "rust-module-resolution",
                "prov": cx.prov_with_confidence(&call, ConfidenceTier::Gap, &format!("CALLS -> {gap_id}")),
            }),
        },
    )
}

fn push_pending(
    cx: &FileCx<'_>,
    out: &mut Extraction,
    call: TsNode<'_>,
    src: String,
    target: Target,
    callee: &str,
    reason: &str,
) {
    let (gap_node, gap_edge) = gap_pair(cx, call, &src, callee, reason);
    out.pending_calls.push(PendingCall {
        src,
        target,
        props: serde_json::json!({
            "resolution": "rust-module-proven",
            "prov": cx.prov(&call, &format!("CALLS {callee}")),
        }),
        gap_node,
        gap_edge,
    });
}

fn enclosing_function<'t>(
    mut node: TsNode<'t>,
    functions: &HashMap<usize, String>,
) -> Option<(String, TsNode<'t>)> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "function_item"
            && let Some(id) = functions.get(&parent.start_byte())
        {
            return Some((id.clone(), parent));
        }
        node = parent;
    }
    None
}

/// Names `function` binds as values: parameters, `let`, `for`, `if let` /
/// `while let`, match arms and closure parameters, nested functions
/// excluded. Computed once per function; block scope is not tracked, so a
/// shadowed name is left out for the whole function (fail closed).
fn value_bindings(cx: &FileCx<'_>, function: TsNode<'_>) -> BTreeSet<String> {
    fn identifiers(cx: &FileCx<'_>, node: TsNode<'_>, out: &mut BTreeSet<String>) {
        if node.kind() == "identifier" {
            out.insert(cx.text(&node).to_string());
        }
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            identifiers(cx, child, out);
        }
    }
    let mut names = BTreeSet::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "parameter" | "let_declaration" | "for_expression" | "let_condition"
            | "match_pattern" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    identifiers(cx, pattern, &mut names);
                } else if node.kind() == "match_pattern" {
                    identifiers(cx, node, &mut names);
                }
            }
            "closure_parameters" => identifiers(cx, node, &mut names),
            _ => {}
        }
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            if child.kind() != "function_item" {
                stack.push(child);
            }
        }
    }
    names
}

fn extract_source_inner(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
    module: Vec<String>,
    crate_name: String,
    workspace_crates: BTreeSet<String>,
    test_module: bool,
) -> Result<Extraction, ExtractError> {
    let mut parser = Parser::new();
    parser.set_language(&language())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let mut declared_mods = BTreeSet::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query("(mod_item name: (identifier) @name)"), root, source);
    while let Some(found) = matches.next() {
        if let Ok(name) = found.captures()[0].node.utf8_text(source) {
            declared_mods.insert(name.to_string());
        }
    }
    let cx = FileCx {
        source,
        path,
        id,
        module,
        crate_name,
        workspace_crates,
        declared_mods,
        test_module,
    };
    let mut out = Extraction::default();
    out.nodes.push(Node {
        id: file_id(id.repo, path),
        label: "File".into(),
        props: serde_json::json!({
            "path": path,
            "language": "rust",
            "module_path": cx.module.join("::"),
            "crate_name": cx.crate_name,
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });
    let bindings = parse_imports(&cx, root, &mut out);

    // Symbols: free/trait/impl functions, structs, enums, traits.
    let sym_query = query(
        "[(function_item) (function_signature_item) (struct_item) (enum_item) (trait_item)] @sym",
    );
    let mut functions_by_start: HashMap<usize, String> = HashMap::new();
    let mut function_items: Vec<(TsNode<'_>, String)> = Vec::new();
    // (module scope, name) -> free function in that inline or file module.
    let mut locals: HashMap<(String, String), String> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(sym_query, root, source);
    while let Some(found) = matches.next() {
        let node = found.captures()[0].node;
        let Some(name_node) = node.child_by_field_name("name") else {
            continue;
        };
        let simple = cx.text(&name_node).to_string();
        let (qualified, kind, type_name) = match node.kind() {
            "struct_item" => (simple.clone(), "struct", None),
            "enum_item" => (simple.clone(), "enum", None),
            "trait_item" => (simple.clone(), "trait", None),
            _ => {
                if let Some(ty) = impl_type(&cx, node) {
                    (format!("{ty}.{simple}"), "function", Some(ty))
                } else if let Some(ty) = trait_type(&cx, node) {
                    (format!("{ty}.{simple}"), "function", Some(ty))
                } else {
                    (simple.clone(), "function", None)
                }
            }
        };
        // Sibling inline modules may reuse a name, so their path is part of
        // the identity (`#a::run`, `#b::run`).
        // Function-local items and trait-impl methods are disambiguated the
        // same way: `outer::Visitor`, `Failure.From<io::Error>.from`.
        let mut prefix = inline_modules(&cx, node);
        prefix.extend(enclosing_function_names(&cx, node));
        let identity = match (&type_name, impl_trait(&cx, node)) {
            (Some(ty), Some(tr)) => format!("{ty}.{tr}.{simple}"),
            _ => qualified.clone(),
        };
        let local_name = if prefix.is_empty() {
            identity
        } else {
            format!("{}::{identity}", prefix.join("::"))
        };
        let symbol = symbol_id(id.repo, path, &local_name);
        let module = scope_of(&cx, node);
        if node.kind() == "function_item" {
            functions_by_start.insert(node.start_byte(), symbol.clone());
            function_items.push((node, symbol.clone()));
        }
        // A nested `fn` is visible only inside its enclosing function.
        let visible = visible_scopes(&cx, node);
        let fn_local = visible.len() > 1;
        if type_name.is_none() && kind == "function" {
            locals.insert((visible[0].clone(), simple.clone()), symbol.clone());
        }
        let mut props = serde_json::json!({
            "name": qualified,
            "simple_name": simple,
            "module_path": module.join("::"),
            "crate_name": cx.crate_name,
            "kind": kind,
            "language": "rust",
            "prov": cx.prov(&node, &format!("Symbol {symbol}")),
        });
        if let Some(ty) = &type_name {
            props["type_name"] = serde_json::json!(ty);
        }
        if fn_local {
            props["fn_local"] = serde_json::json!(true);
        }
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props,
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&node, &format!("DEFINED_IN {symbol}")),
            }),
        });
    }

    // Configuration reads (#1789, AC-0597).
    let functions_by_start: HashMap<usize, String> = function_items
        .iter()
        .map(|(function, symbol)| (function.start_byte(), symbol.clone()))
        .collect();
    config_read::extract(&cx, root, &bindings, &functions_by_start, &mut out);
    // SQL statements as table reads and writes (#2041, AC-1015).
    sql::extract(&cx, root, &bindings, &functions_by_start, &mut out);
    // Guarded returns as business rules (#2018, AC-1017).
    rule_evidence::extract(&cx, root, &functions_by_start, &mut out);
    // Target tests and the requests they build (#2018, AC-1016).
    let mut test_requests = Vec::new();
    target_tests::extract(
        &cx,
        &bindings,
        &function_items,
        &mut out,
        &mut test_requests,
    );
    out.test_requests = test_requests;

    let client_fields = reqwest::client_fields(&cx, root, &bindings);
    let consumer_fields = rdkafka::consumer_fields(&cx, root, &bindings);
    auth::extract(&cx, root, &function_items, &mut out);
    for (fn_node, fn_sym) in &function_items {
        out.handler_params
            .extend(auth::handler_params(&cx, *fn_node, fn_sym, &bindings));
        http::extract_actix_fn_attrs(&cx, *fn_node, fn_sym, &bindings, &mut out);
        http::extract_function_routes(&cx, *fn_node, fn_sym, &bindings, &locals, &mut out);
        reqwest::extract_function_calls(&cx, *fn_node, fn_sym, &bindings, &client_fields, &mut out);
        rdkafka::extract_function_sites(
            &cx,
            *fn_node,
            fn_sym,
            &bindings,
            &consumer_fields,
            &mut out,
        );
    }

    // Calls.
    let call_query = query("(call_expression) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(call_query, root, source);
    let mut shadowing: HashMap<usize, BTreeSet<String>> = HashMap::new();
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let Some((src, function)) = enclosing_function(call, &functions_by_start) else {
            continue;
        };
        let Some(mut callee_node) = call.child_by_field_name("function") else {
            continue;
        };
        // `parse::<u32>()`: resolve the base of a turbofish callee.
        if callee_node.kind() == "generic_function"
            && let Some(base) = callee_node.child_by_field_name("function")
        {
            callee_node = base;
        }
        let callee = cx.text(&callee_node).to_string();
        let scope = scope_of(&cx, call);
        let scopes = visible_scopes(&cx, call);
        match callee_node.kind() {
            "identifier" => {
                let name = callee.as_str();
                // A parameter or `let` binding of the same name (a closure,
                // a `fn` pointer) shadows any item: left out, never guessed.
                let values = shadowing
                    .entry(function.start_byte())
                    .or_insert_with(|| value_bindings(&cx, function));
                if values.contains(name) {
                    continue;
                }
                match resolve_name(&scopes, name, &locals, &bindings) {
                    NameResolution::Local(dst) => {
                        if src != *dst {
                            out.edges.push(Edge {
                                src: src.clone(),
                                dst: dst.clone(),
                                label: "CALLS".into(),
                                props: serde_json::json!({
                                    "prov": cx.prov(&call, &format!("CALLS {callee}")),
                                }),
                            });
                        }
                    }
                    NameResolution::Import((resolved, external)) => {
                        if !external {
                            push_pending(
                                &cx,
                                &mut out,
                                call,
                                src,
                                Target::Path(resolved.clone()),
                                &callee,
                                "unresolved Rust call target",
                            );
                        }
                    }
                    NameResolution::Globs(globs) => {
                        // Only a workspace glob import can bring the name in
                        // scope; a prelude item, constructor or local closure
                        // matches no candidate and is left out.
                        let paths = globs
                            .into_iter()
                            .map(|mut path| {
                                path.push(name.to_string());
                                path
                            })
                            .collect();
                        push_pending(
                            &cx,
                            &mut out,
                            call,
                            src,
                            Target::Candidates {
                                paths,
                                gap_on_miss: false,
                            },
                            &callee,
                            "unresolved Rust call target",
                        );
                    }
                    NameResolution::Ambiguous | NameResolution::None => {}
                }
            }
            "scoped_identifier" => {
                let parts = segs(&callee);
                if parts.len() < 2 {
                    continue;
                }
                if parts[0] == "Self" {
                    if let Some(ty) = impl_type(&cx, call) {
                        push_pending(
                            &cx,
                            &mut out,
                            call,
                            src,
                            Target::Method {
                                crate_name: cx.crate_name.clone(),
                                type_name: ty,
                                method: parts[parts.len() - 1].clone(),
                            },
                            &callee,
                            "unresolved Rust call target",
                        );
                    }
                    continue;
                }
                if let Some((base, external)) = bindings.get(&scopes, &parts[0]) {
                    if *external {
                        continue;
                    }
                    let mut target = base.clone();
                    target.extend(parts[1..].iter().cloned());
                    push_pending(
                        &cx,
                        &mut out,
                        call,
                        src,
                        Target::Path(target),
                        &callee,
                        "unresolved Rust call target",
                    );
                    continue;
                }
                let (resolved, external) = resolve_path(&cx, &scope, &parts);
                let target = if external {
                    // A path relative to this module, through a workspace
                    // glob import, or into an external crate: the
                    // directory-wide index decides which.
                    let mut paths = vec![{
                        let mut path = scope.clone();
                        path.extend(parts.iter().cloned());
                        path
                    }];
                    for glob in bindings.globs(&scopes) {
                        let mut path = glob.clone();
                        path.extend(parts.iter().cloned());
                        paths.push(path);
                    }
                    Target::Candidates {
                        paths,
                        gap_on_miss: true,
                    }
                } else {
                    Target::Path(resolved)
                };
                push_pending(
                    &cx,
                    &mut out,
                    call,
                    src,
                    target,
                    &callee,
                    "unresolved Rust call target",
                );
            }
            "field_expression" => {
                // Only `self.method()` has a receiver type T0 can prove.
                let receiver = callee_node
                    .child_by_field_name("value")
                    .map(|n| cx.text(&n).to_string())
                    .unwrap_or_default();
                if receiver == "self"
                    && let Some(ty) = impl_type(&cx, call)
                {
                    let method = callee_node
                        .child_by_field_name("field")
                        .map(|n| cx.text(&n).to_string())
                        .unwrap_or_default();
                    push_pending(
                        &cx,
                        &mut out,
                        call,
                        src,
                        Target::Method {
                            crate_name: cx.crate_name.clone(),
                            type_name: ty,
                            method,
                        },
                        &callee,
                        "unresolved Rust call target",
                    );
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// One Cargo package found under the root.
#[derive(Debug, Clone)]
struct CrateInfo {
    name: String,
    /// Repo-relative directory of the `Cargo.toml` ("" at the root).
    dir: String,
}

/// Tiny line parser for `[package] name` (no toml dependency).
fn parse_crate_name(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package && let Some(rest) = line.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.replace('-', "_"));
                }
            }
        }
    }
    None
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "target" | "vendor" | "node_modules" | "dist" | "build"
        )
}

fn discover_crates(root: &Path) -> Result<Vec<CrateInfo>, ExtractError> {
    let mut crates = Vec::new();
    for file in source_walk::files(root, &skip_dir, source_walk::Gitignores::Honor)? {
        if file.name != "Cargo.toml" {
            continue;
        }
        let text = std::fs::read_to_string(&file.path)?;
        if let Some(name) = parse_crate_name(&text) {
            let dir = file
                .rel
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_default();
            crates.push(CrateInfo { name, dir });
        }
    }
    crates.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(crates)
}

/// Module segments for `rel`: crate name, then the path under `src/`
/// (`foo.rs`/`foo/mod.rs` both name module `foo`; roots name the crate).
fn module_for(rel: &str, crates: &[CrateInfo]) -> (Vec<String>, String) {
    let owner = crates
        .iter()
        .filter(|c| c.dir.is_empty() || rel.starts_with(&format!("{}/", c.dir)))
        .max_by_key(|c| c.dir.len());
    let (crate_name, rest) = match owner {
        Some(k) => {
            let rest = if k.dir.is_empty() {
                rel.to_string()
            } else {
                rel[k.dir.len() + 1..].to_string()
            };
            (k.name.clone(), rest)
        }
        None => ("crate".to_string(), rel.to_string()),
    };
    let rest = rest.strip_prefix("src/").unwrap_or(&rest).to_string();
    let mut parts: Vec<String> = rest.split('/').map(str::to_string).collect();
    let file = parts.pop().unwrap_or_default();
    let mut module = vec![crate_name.clone()];
    module.extend(parts);
    if !matches!(file.as_str(), "mod.rs" | "lib.rs" | "main.rs") {
        module.push(file.trim_end_matches(".rs").to_string());
    }
    (module, crate_name)
}

/// Recover deterministic facts from one Rust source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let (module, crate_name) = module_for(path, &[]);
    let mut out = extract_source_inner(
        source,
        path,
        id,
        module,
        crate_name.clone(),
        BTreeSet::from([crate_name]),
        false,
    )?;
    finalize_single(&mut out);
    Ok(out)
}

/// Single-file resolution: workspace targets outside the file fail closed to Gaps.
fn finalize_single(out: &mut Extraction) {
    let index = SymbolIndex::build(out);
    for pending in std::mem::take(&mut out.pending_calls) {
        resolve_pending(pending, &index, out);
    }
    http::finalize_endpoints(out, &index);
    let requests = std::mem::take(&mut out.test_requests);
    target_tests::resolve(out, requests);
    close_imports(out, &index);
    adapters_fw::sql::merge(&mut out.nodes);
}

#[derive(Debug, Default)]
struct SymbolIndex {
    by_path: BTreeMap<String, String>,
    by_method: BTreeMap<(String, String, String), String>,
    ambiguous_methods: BTreeSet<(String, String, String)>,
    modules: BTreeSet<String>,
    /// `module::local` bound by a `use` -> (absolute target, is_external),
    /// so a call through a re-export (`use super::HashMap`) follows it.
    reexports: BTreeMap<String, (Vec<String>, bool)>,
}

impl SymbolIndex {
    fn build(out: &Extraction) -> Self {
        let mut index = SymbolIndex::default();
        let file_modules: BTreeMap<&str, &str> = out
            .nodes
            .iter()
            .filter(|n| n.label == "File")
            .filter_map(|n| Some((n.id.as_str(), n.props.get("module_path")?.as_str()?)))
            .collect();
        for edge in out.edges.iter().filter(|e| e.label == "IMPORTS") {
            let (Some(module), Some(local), Some(resolved)) = (
                edge.props
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .or_else(|| file_modules.get(edge.src.as_str()).copied()),
                edge.props.get("binds").and_then(|v| v.as_str()),
                edge.props.get("resolved").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let external = edge.props.get("external").and_then(|v| v.as_bool()) == Some(true);
            index
                .reexports
                .entry(format!("{module}::{local}"))
                .or_insert((segs(resolved), external));
        }
        for node in &out.nodes {
            if node.label == "File"
                && let Some(m) = node.props.get("module_path").and_then(|v| v.as_str())
            {
                index.modules.insert(m.to_string());
            }
            if node.label != "Symbol" {
                continue;
            }
            let Some(module) = node.props.get("module_path").and_then(|v| v.as_str()) else {
                continue;
            };
            index.modules.insert(module.to_string());
            let Some(simple) = node.props.get("simple_name").and_then(|v| v.as_str()) else {
                continue;
            };
            // Function-local items are not reachable by module path.
            if node.props.get("fn_local").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            if let Some(ty) = node.props.get("type_name").and_then(|v| v.as_str()) {
                index
                    .by_path
                    .insert(format!("{module}::{ty}::{simple}"), node.id.clone());
                let crate_name = node
                    .props
                    .get("crate_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let key = (crate_name, ty.to_string(), simple.to_string());
                match index.by_method.entry(key.clone()) {
                    std::collections::btree_map::Entry::Occupied(_) => {
                        index.ambiguous_methods.insert(key);
                    }
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(node.id.clone());
                    }
                }
            } else {
                index
                    .by_path
                    .insert(format!("{module}::{simple}"), node.id.clone());
            }
        }
        index
    }
}

/// Look `parts` up, following up to four re-export hops. `Err(true)` is a
/// miss inside a workspace module, the only miss worth a Gap; a path that
/// leaves the workspace or names an item of a type (an enum variant, a
/// derived or trait associated function) is `Err(false)`.
fn lookup(index: &SymbolIndex, parts: &[String]) -> Result<String, bool> {
    let mut parts = parts.to_vec();
    for _ in 0..4 {
        if let Some(id) = index.by_path.get(&parts.join("::")) {
            return Ok(id.clone());
        }
        let hop = (2..=parts.len())
            .rev()
            .find_map(|i| Some((i, index.reexports.get(&parts[..i].join("::"))?)));
        match hop {
            Some((_, (_, true))) => return Err(false),
            Some((i, (target, false))) if target.as_slice() != &parts[..i] => {
                let mut next = target.clone();
                next.extend(parts[i..].iter().cloned());
                parts = next;
            }
            _ => break,
        }
    }
    Err(parts.len() > 1 && index.modules.contains(&parts[..parts.len() - 1].join("::")))
}

fn resolve_pending(mut pending: PendingCall, index: &SymbolIndex, out: &mut Extraction) {
    let resolved: Result<String, bool> = match &pending.target {
        Target::Path(parts) => lookup(index, parts),
        Target::Method {
            crate_name,
            type_name,
            method,
        } => {
            // A miss is usually a derived or std trait method
            // (`self.clone()`), which T0 has no claim on.
            let key = (crate_name.clone(), type_name.clone(), method.clone());
            if index.ambiguous_methods.contains(&key) {
                Err(false)
            } else {
                index.by_method.get(&key).cloned().ok_or(false)
            }
        }
        Target::Candidates { paths, gap_on_miss } => {
            let found: Vec<Result<String, bool>> =
                paths.iter().map(|path| lookup(index, path)).collect();
            let hits: BTreeSet<&String> = found.iter().filter_map(|r| r.as_ref().ok()).collect();
            match hits.len() {
                1 => Ok(hits.into_iter().next().expect("one hit").clone()),
                0 => Err(*gap_on_miss && found.contains(&Err(true))),
                _ => {
                    pending.gap_node.props["reason"] =
                        "unresolved Rust call target: ambiguous glob imports".into();
                    pending.gap_edge.props["reason"] =
                        "unresolved Rust call target: ambiguous glob imports".into();
                    Err(true)
                }
            }
        }
    };
    match resolved {
        Ok(dst) => {
            if dst != pending.src {
                out.edges.push(Edge {
                    src: pending.src,
                    dst,
                    label: "CALLS".into(),
                    props: pending.props,
                });
            }
        }
        Err(true) => {
            out.nodes.push(pending.gap_node);
            out.edges.push(pending.gap_edge);
        }
        Err(false) => {}
    }
}

fn close_imports(out: &mut Extraction, index: &SymbolIndex) {
    let modules = index.modules.clone();
    let by_path = index.by_path.clone();
    let Extraction { nodes, edges, .. } = out;
    core_graph::placeholder::close_over_endpoints(
        nodes,
        edges,
        EXTRACTOR_ID,
        core_graph::placeholder::label_for_id,
        |endpoint, edge| {
            let spec = endpoint.strip_prefix("mod:")?;
            let external = edge.props.get("external").and_then(|v| v.as_bool());
            if external == Some(true) {
                return Some(Boundary::External {
                    reason: "crate outside this workspace".into(),
                    evidence: vec![],
                });
            }
            let resolved = edge
                .props
                .get("resolved")
                .and_then(|v| v.as_str())
                .unwrap_or(spec)
                .trim_end_matches("::*");
            if modules.contains(resolved) || by_path.contains_key(resolved) {
                Some(Boundary::Internal {
                    reason: "module in this workspace".into(),
                    evidence: vec![],
                })
            } else {
                Some(Boundary::Unresolved {
                    reason: "import of a workspace path with no module".into(),
                })
            }
        },
    );
}

/// Recover a Rust directory with content-addressed per-file parse reuse.
pub fn extract_dir_incremental(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    extract_dir_incremental_with_progress(root, id, cache, &mut |_| {})
}

/// Same as [`extract_dir_incremental`], calling `on_file` with each file's
/// repo-relative path as it's read.
pub fn extract_dir_incremental_with_progress(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let files = rust_paths(root)?;
    extract_dir_with(
        root,
        id,
        &files,
        &|path| std::fs::read(root.join(path)),
        cache,
        on_file,
        &mut |_, _| {},
    )
}

/// The `.rs` files the ordinary walk selects, sorted, without parsing them.
pub fn rust_paths(root: &Path) -> Result<Vec<String>, ExtractError> {
    let mut files: Vec<String> = Vec::new();
    for file in source_walk::files(root, &skip_dir, source_walk::Gitignores::Honor)? {
        if file.path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(file.rel);
        }
    }
    files.sort();
    Ok(files)
}

/// Recover a Rust directory whose `.rs` sources are the retained bytes `read`
/// returns for each of `files`, instead of the checkout (#385). Each file's
/// own extraction is handed to `on_parsed` before directory resolution, so a
/// caller can bind receipts to it; crate discovery still reads `Cargo.toml`
/// files under `root`, outside any receipt.
pub fn extract_dir_from_sources(
    root: &Path,
    id: &SourceId<'_>,
    files: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    on_file: &mut dyn FnMut(&str),
    on_parsed: &mut dyn FnMut(&str, &Extraction),
) -> Result<Extraction, ExtractError> {
    extract_dir_with(
        root,
        id,
        files,
        read,
        &mut IncrementalCache::default(),
        on_file,
        on_parsed,
    )
    .map(|(extraction, _)| extraction)
}

fn extract_dir_with(
    root: &Path,
    id: &SourceId<'_>,
    files: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
    on_parsed: &mut dyn FnMut(&str, &Extraction),
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let crates = discover_crates(root)?;
    let workspace_crates: BTreeSet<String> = crates.iter().map(|c| c.name.clone()).collect();
    let active: BTreeSet<String> = files.iter().cloned().collect();
    let mut stats = IncrementalStats {
        deleted_files: cache.files.keys().filter(|p| !active.contains(*p)).count() as u64,
        ..IncrementalStats::default()
    };
    cache.files.retain(|p, _| active.contains(p));
    // Files a `#[path]` attribute wires in take the declared module (#2041).
    let mut declared = BTreeMap::new();
    let mut scanned = BTreeMap::new();
    for path in files {
        let source = read(path)?;
        if module_paths::mentions_path(&source) {
            let scan = module_paths::scan(&source);
            let decls: Vec<(String, String)> = scan
                .iter()
                .filter_map(|decl| decl.path.clone().ok().map(|p| (decl.name.clone(), p)))
                .collect();
            if !decls.is_empty() {
                declared.insert(path.clone(), decls);
            }
            if !scan.is_empty() {
                scanned.insert(path.clone(), scan);
            }
        }
    }
    let wired = module_paths::modules(&declared, &crates);
    // Modules an out-of-line `#[cfg(test)] mod x;` declares: the files at
    // or under them are test code (#2018).
    let mut test_prefixes: BTreeSet<Vec<String>> = BTreeSet::new();
    for path in files {
        let source = read(path)?;
        let names = target_tests::cfg_test_declarations(&source);
        if names.is_empty() {
            continue;
        }
        let (module, _) = wired
            .get(path)
            .cloned()
            .unwrap_or_else(|| module_for(path, &crates));
        for name in names {
            let mut prefix = module.clone();
            prefix.push(name);
            test_prefixes.insert(prefix);
        }
    }
    let mut out = Extraction::default();
    for path in files {
        on_file(path);
        let source = read(path)?;
        let (module, crate_name) = wired
            .get(path)
            .cloned()
            .unwrap_or_else(|| module_for(path, &crates));
        let test_module = test_prefixes
            .iter()
            .any(|prefix| module.starts_with(prefix));
        let source_hash = core_prov::content_hash(&source);
        let context = format!(
            "{source_hash}\0{}\0{}\0{test_module}",
            module.join("::"),
            workspace_crates
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        );
        let context_hash = core_prov::content_hash(context.as_bytes());
        let extraction = match cache.files.get(path) {
            Some(cached) if cached.source_hash == context_hash => {
                stats.reused_files += 1;
                let mut reused = cached.extraction.clone();
                retarget_commit(&mut reused, id.commit);
                reused
            }
            _ => {
                stats.recomputed_files += 1;
                extract_source_inner(
                    &source,
                    path,
                    id,
                    module,
                    crate_name,
                    workspace_crates.clone(),
                    test_module,
                )?
            }
        };
        on_parsed(path, &extraction);
        cache.files.insert(
            path.clone(),
            CachedFile {
                source_hash: context_hash,
                extraction: extraction.clone(),
            },
        );
        out.nodes.extend(extraction.nodes);
        out.edges.extend(extraction.edges);
        out.pending_calls.extend(extraction.pending_calls);
        out.axum_routers.extend(extraction.axum_routers);
        out.actix_attr_routes.extend(extraction.actix_attr_routes);
        out.actix_service_mounts
            .extend(extraction.actix_service_mounts);
        out.pending_endpoints.extend(extraction.pending_endpoints);
        out.fetch_sites.extend(extraction.fetch_sites);
        out.event_sites.extend(extraction.event_sites);
        out.test_requests.extend(extraction.test_requests);
        out.handler_params.extend(extraction.handler_params);
        out.extractors.extend(extraction.extractors);
        out.deny_fns.extend(extraction.deny_fns);
    }
    path_gaps(id, &scanned, &declared, &active, &mut out);
    let index = SymbolIndex::build(&out);
    for pending in std::mem::take(&mut out.pending_calls) {
        resolve_pending(pending, &index, &mut out);
    }
    http::finalize_endpoints(&mut out, &index);
    let requests = std::mem::take(&mut out.test_requests);
    target_tests::resolve(&mut out, requests);
    close_imports(&mut out, &index);
    adapters_fw::sql::merge(&mut out.nodes);
    Ok((out, stats))
}

/// A Gap on the declaring file for each `#[path]` declaration that does not
/// wire its file in (#2161, AC-1300): the file keeps its location's module,
/// and the Gap says which declaration and why. Computed on every run from
/// the declarations, so it never depends on the per-file cache.
fn path_gaps(
    id: &SourceId<'_>,
    scanned: &BTreeMap<String, Vec<module_paths::Declaration>>,
    declared: &BTreeMap<String, Vec<(String, String)>>,
    files: &BTreeSet<String>,
    out: &mut Extraction,
) {
    let mut gaps: Vec<(&str, &module_paths::Declaration, &'static str)> = Vec::new();
    for (path, decls) in scanned {
        for decl in decls {
            if let Err(reason) = decl.path {
                gaps.push((path, decl, reason));
            }
        }
    }
    for (declarer, index, reason) in module_paths::unresolved(declared, files) {
        let Some((path, decls)) = scanned.get_key_value(&declarer) else {
            continue;
        };
        // `declared` holds a file's followed declarations in scan order, so
        // the index names the exact declaration, even under a reused name.
        if let Some(decl) = decls.iter().filter(|decl| decl.path.is_ok()).nth(index) {
            gaps.push((path, decl, reason));
        }
    }
    for (path, decl, reason) in gaps {
        let gap = format!("gap:module-path:{}@{}@{}", id.repo, path, decl.span.0);
        let evidence = EvidenceRef {
            repo: id.repo.into(),
            path: path.into(),
            byte_start: decl.span.0 as u64,
            byte_end: decl.span.1 as u64,
            commit_sha: id.commit.into(),
        };
        let props = serde_json::json!({
            "reason": reason,
            "module": decl.name,
            "attribute": decl.attribute,
            "attempted_tiers": ["T0"],
            "prov": prov_from_evidence(&evidence, ConfidenceTier::Gap, &format!("Gap {gap}")),
        });
        out.nodes.push(Node {
            id: gap.clone(),
            label: "Gap".into(),
            props,
        });
        out.edges.push(Edge {
            src: file_id(id.repo, path),
            dst: gap.clone(),
            label: "DEPENDS_ON".into(),
            props: serde_json::json!({
                "reason": reason,
                "prov": prov_from_evidence(&evidence, ConfidenceTier::Gap, &format!("DEPENDS_ON -> {gap}")),
            }),
        });
    }
}

/// Recover a Rust directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

mod auth;
mod config_read;
mod http;
mod module_paths;
mod rdkafka;
mod reqwest;
mod rule_evidence;
mod sql;
mod target_tests;

#[cfg(test)]
mod tests;
