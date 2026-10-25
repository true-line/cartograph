//! Vitest/Jest target tests and the GraphQL operations they request (#968,
//! AC-0348).
//!
//! A `describe`/`it`/`test` call with a literal name, proven through a test
//! framework import or as an unshadowed global, becomes a `TestCase` named by
//! its `describe` chain. A test client is proven only through a registered
//! factory (`createTestEnvironment` from `@vendure/testing`) whose destructured
//! client names its API. Each `client.query(document)` in a test's own
//! callback whose document is a literal `graphql(…)`/`gql…` constant — in the
//! file or imported from another — yields `REQUESTS` to the recovered
//! operations its root fields name. Every request that cannot be proven is a
//! Gap naming why. Static references only: nothing here executes a test.

use crate::{FileCx, callable, literal_string};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

/// Modules whose `describe`/`it`/`test` exports prove a test call.
const TEST_MODULES: &[&str] = &["vitest", "@jest/globals", "bun:test"];
/// Modifiers a test call may carry (`it.only`, `describe.skip`).
const MODIFIERS: &[&str] = &["only", "skip", "concurrent", "sequential", "todo", "fails"];
/// A registered test client factory.
struct ClientFactory {
    /// Package that exports it.
    module: &'static str,
    /// Its own source file, for monorepos that import it relatively.
    file_suffix: &'static str,
    /// Exported factory name.
    factory: &'static str,
    /// Destructured client name → the API it addresses.
    clients: &'static [(&'static str, &'static str)],
}

const CLIENT_FACTORIES: &[ClientFactory] = &[ClientFactory {
    module: "@vendure/testing",
    file_suffix: "testing/lib/create-test-environment.ts",
    factory: "createTestEnvironment",
    clients: &[("adminClient", "admin"), ("shopClient", "shop")],
}];
/// Exported names of GraphQL document tags (`graphql(…)`, `gql…`).
const DOCUMENT_TAGS: &[&str] = &["graphql", "gql"];
/// The API a resolver outside `resolvers/<api>/` is scoped to (W10c-1).
const DEFAULT_API: &str = crate::graphql_resolvers::DEFAULT_API;
/// Marks an interpolated `${…}` in a document's text.
const SUBSTITUTION: char = '\u{1}';

/// Gap reason: the test's name, or an enclosing `describe`'s, is computed.
pub const COMPUTED_NAME_REASON: &str = "test name is computed";
/// Gap reason: the document is neither a literal nor a recovered constant.
pub const COMPUTED_DOCUMENT_REASON: &str =
    "GraphQL document is not a literal or a recovered constant";
/// Gap reason: no recovered operation serves a root field.
pub const UNSERVED_FIELD_REASON: &str = "no recovered resolver operation serves this root field";

/// A GraphQL document's literal text and where it is written.
#[derive(Debug, Clone)]
pub(crate) struct Document {
    text: String,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
enum DocumentRef {
    Literal(Document),
    Imported { file: String, name: String },
}

/// One proven client `query` call awaiting directory-wide documents.
#[derive(Debug, Clone)]
pub(crate) struct PendingRequest {
    test: String,
    api: &'static str,
    document: DocumentRef,
    /// The call, then the client's proving declaration.
    evidence: Vec<EvidenceRef>,
}

/// Per-file facts carried to directory completion.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    /// Exported document constants: `(path, name)` → document.
    pub documents: HashMap<(String, String), Document>,
    pub requests: Vec<PendingRequest>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Pending) {
        self.documents.extend(other.documents);
        self.requests.extend(other.requests);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for document in self.documents.values_mut() {
            document.evidence.commit_sha = commit.into();
        }
        for request in &mut self.requests {
            if let DocumentRef::Literal(document) = &mut request.document {
                document.evidence.commit_sha = commit.into();
            }
            for evidence in &mut request.evidence {
                evidence.commit_sha = commit.into();
            }
        }
    }
}

fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

fn prov(evidence: Vec<EvidenceRef>, confidence: ConfidenceTier, fact: &str) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        crate::EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("test provenance has evidence");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// Whether `path` names a test file (`x.test.ts`, `x.spec.ts`, `x.e2e-spec.ts`).
pub(crate) fn is_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [".test.", ".spec.", ".e2e-spec."]
        .iter()
        .any(|marker| name.contains(marker))
}

/// What the file's imports and bindings prove at a site.
pub(crate) struct Proofs<'a, 'tree> {
    pub bindings: &'a callable::Bindings<'tree>,
    pub import_modules: &'a HashMap<String, String>,
    pub imported_names: &'a HashMap<String, String>,
    /// Relative imports: local → (resolved file, exported name).
    pub imported_files: &'a HashMap<String, (String, String)>,
}

impl Proofs<'_, '_> {
    /// `(module, exported name)` of an import-proven local at `site`.
    fn import<'s>(&'s self, site: TsNode<'_>, local: &'s str) -> Option<(&'s str, &'s str)> {
        if !self.bindings.imported(site, local) {
            return None;
        }
        if let Some((file, exported)) = self.imported_files.get(local) {
            return Some((file.as_str(), exported.as_str()));
        }
        let module = self.import_modules.get(local)?;
        let exported = self.imported_names.get(local).map_or(local, String::as_str);
        Some((module.as_str(), exported))
    }
}

/// The literal text of a `graphql(…)` call or a `gql…` tagged template whose
/// tag is import-proven, with each `${…}` replaced by [`SUBSTITUTION`].
fn document_text(cx: &FileCx<'_>, proofs: &Proofs<'_, '_>, value: TsNode<'_>) -> Option<String> {
    if value.kind() != "call_expression" {
        return None;
    }
    let tag = value.child_by_field_name("function")?;
    if tag.kind() != "identifier" {
        return None;
    }
    let (_, exported) = proofs.import(value, cx.text(&tag))?;
    if !DOCUMENT_TAGS.contains(&exported) {
        return None;
    }
    let arguments = value.child_by_field_name("arguments")?;
    let literal = if arguments.kind() == "template_string" {
        arguments
    } else {
        let mut walk = arguments.walk();
        arguments.named_children(&mut walk).next()?
    };
    match literal.kind() {
        "string" => literal_string(cx, literal),
        "template_string" => {
            let mut text = String::new();
            let mut walk = literal.walk();
            for part in literal.named_children(&mut walk) {
                match part.kind() {
                    "template_substitution" => text.push(SUBSTITUTION),
                    _ => text.push_str(cx.text(&part)),
                }
            }
            Some(text)
        }
        _ => None,
    }
}

/// A `const` declarator's value, when the declarator is `const`.
fn const_value(declarator: TsNode<'_>) -> Option<TsNode<'_>> {
    let is_const = declarator
        .parent()
        .filter(|p| p.kind() == "lexical_declaration")
        .and_then(|p| p.child(0))
        .is_some_and(|keyword| keyword.kind() == "const");
    (declarator.kind() == "variable_declarator" && is_const)
        .then(|| declarator.child_by_field_name("value"))
        .flatten()
}

enum TestCall<'tree> {
    Describe(Option<String>),
    Test {
        name: Option<String>,
        /// Every argument after the name: the test's own code, including a
        /// callback a wrapper such as `assertThrows(async () => …)` takes.
        body: Vec<TsNode<'tree>>,
        modifier: Option<String>,
    },
}

/// Classify a proven `describe`/`it`/`test` call.
fn test_call<'tree>(
    cx: &FileCx<'_>,
    proofs: &Proofs<'_, 'tree>,
    call: TsNode<'tree>,
) -> Option<TestCall<'tree>> {
    let function = call.child_by_field_name("function")?;
    let (base, modifier, each) = match function.kind() {
        "identifier" => (function, None, false),
        "member_expression" => {
            let object = function.child_by_field_name("object")?;
            let property = cx
                .text(&function.child_by_field_name("property")?)
                .to_string();
            (object.kind() == "identifier" && MODIFIERS.contains(&property.as_str()))
                .then_some((object, Some(property), false))?
        }
        // `it.each(table)('name %s', fn)`: the name is computed per row.
        "call_expression" => {
            let inner = function.child_by_field_name("function")?;
            let object = inner.child_by_field_name("object")?;
            let property = inner.child_by_field_name("property")?;
            (inner.kind() == "member_expression" && cx.text(&property) == "each")
                .then_some((object, None, true))?
        }
        _ => return None,
    };
    let local = cx.text(&base);
    let exported = match proofs.import(call, local) {
        Some((module, exported)) if TEST_MODULES.contains(&module) => exported,
        Some(_) => return None,
        None if proofs.bindings.global_unshadowed(call, local) => local,
        None => return None,
    };
    let arguments = call.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    let arguments: Vec<_> = arguments
        .named_children(&mut walk)
        .filter(|n| n.kind() != "comment")
        .collect();
    let name = arguments
        .first()
        .filter(|_| !each)
        .and_then(|first| literal_string(cx, *first));
    match exported {
        "describe" => Some(TestCall::Describe(name)),
        "it" | "test" => Some(TestCall::Test {
            name,
            body: arguments.iter().skip(1).copied().collect(),
            modifier,
        }),
        _ => None,
    }
}

/// The names of the `describe` calls enclosing `node`, outermost first;
/// `None` when any of them is computed.
fn describe_chain(
    cx: &FileCx<'_>,
    proofs: &Proofs<'_, '_>,
    node: TsNode<'_>,
) -> Option<Vec<String>> {
    let mut chain = Vec::new();
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(current.kind(), "arrow_function" | "function_expression")
            && parent.kind() == "arguments"
            && let Some(call) = parent.parent()
            && let Some(TestCall::Describe(name)) = test_call(cx, proofs, call)
        {
            chain.push(name?);
        }
        current = parent;
    }
    chain.reverse();
    Some(chain)
}

/// The API a proven test client names at `site`, with its declaration.
fn client_api<'tree>(
    cx: &FileCx<'_>,
    proofs: &Proofs<'_, 'tree>,
    site: TsNode<'_>,
    local: &str,
) -> Option<(&'static str, TsNode<'tree>)> {
    let declarator = proofs.bindings.stable_declaration(site, local)?;
    let value = const_value(declarator)?;
    let factory = value.child_by_field_name("function")?;
    if value.kind() != "call_expression" || factory.kind() != "identifier" {
        return None;
    }
    let (module, exported) = proofs.import(value, cx.text(&factory))?;
    let factory = CLIENT_FACTORIES.iter().find(|f| {
        f.factory == exported && (f.module == module || module.ends_with(f.file_suffix))
    })?;
    let pattern = declarator.child_by_field_name("name")?;
    if pattern.kind() != "object_pattern" {
        return None;
    }
    let mut walk = pattern.walk();
    let key = pattern
        .named_children(&mut walk)
        .find_map(|entry| match entry.kind() {
            "shorthand_property_identifier_pattern" if cx.text(&entry) == local => {
                Some(local.to_string())
            }
            "pair_pattern" => {
                let key = entry.child_by_field_name("key")?;
                let value = entry.child_by_field_name("value")?;
                (value.kind() == "identifier" && cx.text(&value) == local)
                    .then(|| cx.text(&key).to_string())
            }
            _ => None,
        })?;
    let (_, api) = factory.clients.iter().find(|(name, _)| *name == key)?;
    Some((api, declarator))
}

fn calls_within<'tree>(node: TsNode<'tree>, out: &mut Vec<TsNode<'tree>>) {
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        if child.kind() == "call_expression" {
            out.push(child);
        }
        calls_within(child, out);
    }
}

/// Index the file's exported document constants and, in a test file, its
/// tests and their proven client requests.
pub(crate) fn extract<'tree>(
    cx: &FileCx<'_>,
    root: TsNode<'tree>,
    proofs: &Proofs<'_, 'tree>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    pending: &mut Pending,
) {
    let mut walk = root.walk();
    for statement in root.named_children(&mut walk) {
        if statement.kind() != "export_statement" {
            continue;
        }
        let Some(declaration) = statement.child_by_field_name("declaration") else {
            continue;
        };
        let mut inner = declaration.walk();
        for declarator in declaration.named_children(&mut inner) {
            let Some(value) = const_value(declarator) else {
                continue;
            };
            let Some(name) = declarator.child_by_field_name("name") else {
                continue;
            };
            if let Some(text) = document_text(cx, proofs, value) {
                pending.documents.insert(
                    (cx.path.to_string(), cx.text(&name).to_string()),
                    Document {
                        text,
                        evidence: evidence(cx, value),
                    },
                );
            }
        }
    }
    if !is_test_file(cx.path) {
        return;
    }
    let mut calls = Vec::new();
    calls_within(root, &mut calls);
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for call in calls {
        let Some(TestCall::Test {
            name,
            body: arguments,
            modifier,
        }) = test_call(cx, proofs, call)
        else {
            continue;
        };
        let chain = describe_chain(cx, proofs, call);
        let (Some(name), Some(mut chain)) = (name, chain) else {
            let id = format!(
                "gap:test-name:{}@{}@{}",
                cx.id.repo,
                cx.path,
                call.start_byte()
            );
            nodes.push(Node {
                id: id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "test-name-computed",
                    "reason": COMPUTED_NAME_REASON,
                    "attempted_tiers": ["T0"],
                    "prov": prov(vec![evidence(cx, call)], ConfidenceTier::Gap, &format!("Gap {id}")),
                }),
            });
            continue;
        };
        chain.push(name.clone());
        let mut qualified = chain.join(" > ");
        let count = seen.entry(qualified.clone()).or_default();
        *count += 1;
        if *count > 1 {
            qualified = format!("{qualified} [{count}]");
        }
        let test = format!("test:{}@{}#{qualified}", cx.id.repo, cx.path);
        let mut props = serde_json::json!({
            "name": name,
            "qualified_name": qualified,
            "framework": "vitest-jest",
            "language": "typescript",
            "path": cx.path,
            "prov": prov(vec![evidence(cx, call)], ConfidenceTier::Confirmed, &format!("TestCase {test}")),
        });
        if let Some(modifier) = modifier {
            props["modifier"] = modifier.into();
        }
        nodes.push(Node {
            id: test.clone(),
            label: "TestCase".into(),
            props,
        });
        edges.push(Edge {
            src: test.clone(),
            dst: crate::file_id(cx.id.repo, cx.path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": prov(vec![evidence(cx, call)], ConfidenceTier::Confirmed, &format!("DEFINED_IN {test}")),
            }),
        });
        let mut body = Vec::new();
        for argument in arguments {
            if argument.kind() == "call_expression" {
                body.push(argument);
            }
            calls_within(argument, &mut body);
        }
        for site in body {
            let Some(function) = site.child_by_field_name("function") else {
                continue;
            };
            let (Some(object), Some(property)) = (
                function.child_by_field_name("object"),
                function.child_by_field_name("property"),
            ) else {
                continue;
            };
            if function.kind() != "member_expression"
                || object.kind() != "identifier"
                || cx.text(&property) != "query"
            {
                continue;
            }
            let Some((api, declaration)) = client_api(cx, proofs, site, cx.text(&object)) else {
                continue;
            };
            let Some(arguments) = site.child_by_field_name("arguments") else {
                continue;
            };
            let mut walk = arguments.walk();
            let document = arguments
                .named_children(&mut walk)
                .next()
                .and_then(|argument| {
                    if let Some(text) = document_text(cx, proofs, argument) {
                        return Some(DocumentRef::Literal(Document {
                            text,
                            evidence: evidence(cx, argument),
                        }));
                    }
                    if argument.kind() != "identifier" {
                        return None;
                    }
                    let local = cx.text(&argument);
                    if proofs.bindings.imported(site, local) {
                        // Only a relative import names a file whose constants
                        // this walk recovers.
                        let (file, name) = proofs.imported_files.get(local)?;
                        return Some(DocumentRef::Imported {
                            file: file.clone(),
                            name: name.clone(),
                        });
                    }
                    let declarator = proofs.bindings.stable_declaration(site, local)?;
                    let value = const_value(declarator)?;
                    Some(DocumentRef::Literal(Document {
                        text: document_text(cx, proofs, value)?,
                        evidence: evidence(cx, value),
                    }))
                });
            let call_evidence = vec![evidence(cx, site), evidence(cx, declaration)];
            match document {
                Some(document) => pending.requests.push(PendingRequest {
                    test: test.clone(),
                    api,
                    document,
                    evidence: call_evidence,
                }),
                None => push_gap(
                    nodes,
                    edges,
                    &test,
                    call_evidence,
                    COMPUTED_DOCUMENT_REASON,
                    None,
                ),
            }
        }
    }
}

fn push_gap(
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    test: &str,
    evidence: Vec<EvidenceRef>,
    reason: &str,
    field: Option<&str>,
) {
    let call = &evidence[0];
    let mut id = format!(
        "gap:test-call:{}@{}@{}",
        call.repo, call.path, call.byte_start
    );
    if let Some(field) = field {
        id = format!("{id}:{field}");
    }
    let mut props = serde_json::json!({
        "reason": reason,
        "attempted_tiers": ["T0"],
        "prov": prov(evidence.clone(), ConfidenceTier::Gap, &format!("Gap {id}")),
    });
    if let Some(field) = field {
        props["field"] = field.into();
    }
    nodes.push(Node {
        id: id.clone(),
        label: "Gap".into(),
        props,
    });
    edges.push(Edge {
        src: test.into(),
        dst: id.clone(),
        label: "REQUESTS".into(),
        props: serde_json::json!({
            "reason": reason,
            "prov": prov(evidence, ConfidenceTier::Gap, &format!("REQUESTS {test} -> {id}")),
        }),
    });
}

/// The candidate files an extension-guessing relative import may name.
fn file_candidates(file: &str) -> Vec<String> {
    let mut out = vec![file.to_string()];
    if let Some(stem) = file.strip_suffix(".ts") {
        for ext in [".tsx", ".js", ".mjs", ".cjs", ".jsx"] {
            out.push(format!("{stem}{ext}"));
        }
        for ext in [".ts", ".tsx", ".js"] {
            out.push(format!("{stem}/index{ext}"));
        }
    }
    out
}

/// Resolve every pending request against the directory's documents and
/// recovered GraphQL operations.
pub(crate) fn complete(pending: Pending, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>) {
    let operations: BTreeMap<(String, String, String), String> = nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && n.props["protocol"] == "graphql")
        .filter_map(|n| {
            let key = |k: &str| n.props[k].as_str().map(str::to_string);
            Some((
                (key("api")?, key("operation")?, key("field")?),
                n.id.clone(),
            ))
        })
        .collect();
    let operation = |api: &str, kind: &str, field: &str| {
        operations.get(&(api.to_string(), kind.to_string(), field.to_string()))
    };
    #[derive(Default)]
    struct Link {
        evidence: Vec<EvidenceRef>,
        fallback: bool,
    }
    let mut links: BTreeMap<(String, String), Link> = BTreeMap::new();
    let (mut gap_nodes, mut gap_edges) = (Vec::new(), Vec::new());
    for request in pending.requests {
        let document = match &request.document {
            DocumentRef::Literal(document) => Some(document.clone()),
            DocumentRef::Imported { file, name } => file_candidates(file)
                .into_iter()
                .find_map(|file| pending.documents.get(&(file, name.clone())).cloned()),
        };
        let Some(document) = document else {
            push_gap(
                &mut gap_nodes,
                &mut gap_edges,
                &request.test,
                request.evidence,
                COMPUTED_DOCUMENT_REASON,
                None,
            );
            continue;
        };
        let (kind, fields) = match root_fields(&document.text) {
            Ok(parsed) => parsed,
            Err(reason) => {
                push_gap(
                    &mut gap_nodes,
                    &mut gap_edges,
                    &request.test,
                    request.evidence,
                    reason,
                    None,
                );
                continue;
            }
        };
        let mut evidence = request.evidence.clone();
        evidence.push(document.evidence.clone());
        for field in fields {
            let exact = operation(request.api, kind, &field);
            let fallback = operation(DEFAULT_API, kind, &field);
            let Some((endpoint, fallback)) = exact
                .map(|id| (id, false))
                .or(fallback.map(|id| (id, true)))
            else {
                push_gap(
                    &mut gap_nodes,
                    &mut gap_edges,
                    &request.test,
                    evidence.clone(),
                    UNSERVED_FIELD_REASON,
                    Some(&field),
                );
                continue;
            };
            let link = links
                .entry((request.test.clone(), endpoint.clone()))
                .or_default();
            link.fallback |= fallback;
            for item in &evidence {
                if !link.evidence.contains(item) {
                    link.evidence.push(item.clone());
                }
            }
        }
    }
    nodes.extend(gap_nodes);
    edges.extend(gap_edges);
    for ((test, endpoint), link) in links {
        // A default-scoped operation was assumed to serve this client's API.
        let (confidence, assumption) = if link.fallback {
            (ConfidenceTier::InferredWeak, "client_api_default_fallback")
        } else {
            (ConfidenceTier::InferredStrong, "client_api_assumed")
        };
        edges.push(Edge {
            src: test.clone(),
            dst: endpoint.clone(),
            label: "REQUESTS".into(),
            props: serde_json::json!({
                "via": ["graphql"],
                "statuses": [],
                "keys": [],
                "link": assumption,
                "prov": prov(link.evidence, confidence, &format!("REQUESTS {test} -> {endpoint}")),
            }),
        });
    }
}

/// One lexical token of a GraphQL document.
#[derive(Debug, PartialEq)]
enum Token<'a> {
    Name(&'a str),
    Punct(char),
    Spread,
    Substitution,
    Value,
}

fn tokens(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                if text[i..].starts_with("\"\"\"") {
                    let end = text[i + 3..]
                        .find("\"\"\"")
                        .map_or(bytes.len(), |e| i + 3 + e + 3);
                    i = end;
                } else {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                }
                out.push(Token::Value);
            }
            b'.' if text[i..].starts_with("...") => {
                out.push(Token::Spread);
                i += 3;
            }
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b':' | b'@' | b'$' | b'=' | b'!' | b'|'
            | b'&' => {
                out.push(Token::Punct(c as char));
                i += 1;
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.push(Token::Name(&text[start..i]));
            }
            _ if c.is_ascii_digit() || c == b'-' => {
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'-' | b'+'))
                {
                    i += 1;
                }
                out.push(Token::Value);
            }
            _ if text[i..].starts_with(SUBSTITUTION) => {
                out.push(Token::Substitution);
                i += SUBSTITUTION.len_utf8();
            }
            _ => i += text[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out
}

/// Skip one balanced group starting at the opener at `at`; returns the index
/// after its closer.
fn skip_group(tokens: &[Token<'_>], at: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, token) in tokens[at..].iter().enumerate() {
        match token {
            Token::Punct(c) if *c == open => depth += 1,
            Token::Punct(c) if *c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The operation kind and root field names (aliases resolved, `__typename`
/// ignored) of a document holding exactly one operation.
pub(crate) fn root_fields(text: &str) -> Result<(&'static str, Vec<String>), &'static str> {
    let tokens = tokens(text);
    let mut operations = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            Token::Name("fragment") => {
                let open = tokens[i..]
                    .iter()
                    .position(|t| *t == Token::Punct('{'))
                    .ok_or("GraphQL document is malformed")?;
                i = skip_group(&tokens, i + open, '{', '}')
                    .ok_or("GraphQL document is malformed")?;
            }
            Token::Name(kind @ ("query" | "mutation" | "subscription")) => {
                let open = tokens[i..]
                    .iter()
                    .position(|t| *t == Token::Punct('{'))
                    .ok_or("GraphQL document is malformed")?;
                // Variable definitions may hold `{` only inside a default
                // object value, which `(` … `)` encloses.
                let mut j = i + 1;
                while j < i + open {
                    if tokens[j] == Token::Punct('(') {
                        j = skip_group(&tokens, j, '(', ')')
                            .ok_or("GraphQL document is malformed")?;
                    } else {
                        j += 1;
                    }
                }
                let open = (j..tokens.len())
                    .find(|&k| tokens[k] == Token::Punct('{'))
                    .ok_or("GraphQL document is malformed")?;
                let end =
                    skip_group(&tokens, open, '{', '}').ok_or("GraphQL document is malformed")?;
                operations.push((
                    match kind {
                        "query" => "query",
                        "mutation" => "mutation",
                        _ => "subscription",
                    },
                    open,
                    end,
                ));
                i = end;
            }
            Token::Punct('{') => {
                let end =
                    skip_group(&tokens, i, '{', '}').ok_or("GraphQL document is malformed")?;
                operations.push(("query", i, end));
                i = end;
            }
            Token::Substitution => i += 1,
            _ => return Err("GraphQL document is malformed"),
        }
    }
    let [(kind, open, end)] = operations[..] else {
        return Err(if operations.is_empty() {
            "GraphQL document has no operation"
        } else {
            "GraphQL document has more than one operation"
        });
    };
    let mut fields = BTreeSet::new();
    let mut i = open + 1;
    while i < end - 1 {
        match tokens[i] {
            Token::Name(first) => {
                let name = if tokens.get(i + 1) == Some(&Token::Punct(':')) {
                    i += 2;
                    match tokens.get(i) {
                        Some(Token::Name(underlying)) => *underlying,
                        _ => return Err("GraphQL document is malformed"),
                    }
                } else {
                    first
                };
                i += 1;
                if tokens.get(i) == Some(&Token::Punct('(')) {
                    i = skip_group(&tokens, i, '(', ')').ok_or("GraphQL document is malformed")?;
                }
                while tokens.get(i) == Some(&Token::Punct('@')) {
                    i += 2;
                    if tokens.get(i) == Some(&Token::Punct('(')) {
                        i = skip_group(&tokens, i, '(', ')')
                            .ok_or("GraphQL document is malformed")?;
                    }
                }
                if tokens.get(i) == Some(&Token::Punct('{')) {
                    i = skip_group(&tokens, i, '{', '}').ok_or("GraphQL document is malformed")?;
                }
                if name != "__typename" {
                    fields.insert(name.to_string());
                }
            }
            Token::Spread => return Err("GraphQL operation selects a fragment at its root"),
            Token::Substitution => return Err("GraphQL document is interpolated at its root"),
            _ => return Err("GraphQL document is malformed"),
        }
    }
    if fields.is_empty() {
        return Err("GraphQL operation selects no root field");
    }
    Ok((kind, fields.into_iter().collect()))
}
