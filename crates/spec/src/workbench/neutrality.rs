//! The deterministic check for SPEC-16 rule 3 (#1752, AC-0683): a drafted
//! statement does not name the source implementation it cites.
//!
//! A requirement or a rule states behaviour in domain terms. The identifiers
//! of the implementation belong in the citation and the fact index. This
//! module finds the one case a rule can decide without reading meaning: the
//! statement spells out a file path or a code symbol of a fact it cites.
//!
//! **What counts as a source identifier of a cited fact.**
//! - Each path of the fact's evidence, and the file name of that path when
//!   it has an extension. A `Document` or `DocSection` fact is the target's
//!   own documentation, not its implementation, and has none.
//! - The `name` of a cited node whose label is a code declaration
//!   ([`CODE_LABELS`]), and the last segment of a qualified name, when the
//!   text is shaped like code: it holds `_`, `::`, `.`, `#` or `(`, or a
//!   lower-case letter directly followed by an upper-case one.
//!
//! **What is not checked.** A single plain word (`checkout`, `Order`) is
//! not told apart from a domain term and is never a finding. HTTP methods,
//! paths, status codes, field names and message names are part of the
//! observable contract (rule 3) and are not identifiers here: endpoints,
//! data entities and channels are not code labels. Framework terms and
//! identifiers of facts the claim does not cite are left to review.
//!
//! **Matching.** Case-sensitive, on whole tokens: the character before and
//! after the match is not a letter, a digit or `_`.
//!
//! **Cost.** One pass over the facts to index the cited ids; each statement
//! is scanned once per identifier of the facts it cites.

use super::{citation_id, provenance};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};

/// Node labels that declare code. A cited node of another label gives no
/// symbol name.
pub const CODE_LABELS: [&str; 7] = [
    "Symbol",
    "Function",
    "Module",
    "Decorator",
    "Declaration",
    "Component",
    "File",
];

/// Labels of the target's own documentation: their evidence paths are not
/// implementation identifiers.
const DOCUMENT_LABELS: [&str; 2] = ["Document", "DocSection"];

/// Shortest identifier that is matched. A shorter one is too likely to be
/// an ordinary word or abbreviation.
const MIN_IDENTIFIER_BYTES: usize = 3;

/// The source identifiers of the facts a draft cites, by citation id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceIdentifiers {
    by_citation: BTreeMap<String, BTreeSet<String>>,
}

/// Whether `c` continues an identifier: a letter or digit of any script,
/// or `_`. Punctuation such as typographic quotes or dashes does not.
fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `name` is shaped like code and not like a plain word.
fn code_shaped(name: &str) -> bool {
    name.len() >= MIN_IDENTIFIER_BYTES
        && !name.chars().any(char::is_whitespace)
        && (name.contains(['_', '.', '#', '('])
            || name.contains("::")
            || name
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase()))
}

/// The first token of a projected statement that is shaped like code
/// (#1999): the sibling of the AC-0683 check for statements Cartograph
/// renders by rule rather than drafts. A projected statement cites no
/// particular symbol, so any code-shaped token outside a quotation (“…”)
/// is a finding. A sentence's own full stop, a decimal number and an HTTP
/// path are not.
pub(crate) fn projected_code_token(statement: &str) -> Option<&str> {
    let mut quoted = false;
    let mut start = None;
    let mut found = None;
    for (i, c) in statement.char_indices().chain([(statement.len(), ' ')]) {
        match c {
            '“' => quoted = true,
            '”' => quoted = false,
            _ => {}
        }
        let boundary = c.is_whitespace() || matches!(c, ',' | ';' | '“' | '”' | ':');
        if boundary {
            if let Some(from) = start.take() {
                let token = statement[from..i].trim_end_matches(['.', '!', '?']);
                // Parentheses around a token are prose; a call's are not.
                let token = if token.contains('(') {
                    token.trim_start_matches('(')
                } else {
                    token.trim_matches(['(', ')'])
                };
                let token = token.trim_end_matches(['.', '!', '?']);
                if !token.starts_with('/')
                    && !token.chars().all(|c| c.is_ascii_digit() || c == '.')
                    && code_shaped(token)
                {
                    found = Some(token);
                    break;
                }
            }
        } else if start.is_none() && !quoted {
            start = Some(i);
        }
    }
    found
}

/// The file name of `path` when it has an extension.
fn file_name(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty() && !extension.is_empty()).then_some(name)
}

fn paths(props: &serde_json::Value, identity: &str, into: &mut BTreeSet<String>) {
    for evidence in provenance(props, identity).evidence {
        let path = evidence.path;
        if let Some(name) = file_name(&path) {
            if name.len() >= MIN_IDENTIFIER_BYTES {
                into.insert(name.to_owned());
            }
            into.insert(path);
        } else if path.contains('/') && path.len() >= MIN_IDENTIFIER_BYTES {
            into.insert(path);
        }
    }
}

fn symbols(node: &Node, into: &mut BTreeSet<String>) {
    if !CODE_LABELS.contains(&node.label.as_str()) {
        return;
    }
    let Some(name) = node.props["name"].as_str() else {
        return;
    };
    if code_shaped(name) {
        into.insert(name.to_owned());
    }
    // `orders.service.create_order` also names `create_order`.
    let last = name
        .rsplit(['.', '#'])
        .next()
        .and_then(|rest| rest.rsplit("::").next())
        .unwrap_or(name);
    if last != name && code_shaped(last) {
        into.insert(last.to_owned());
    }
}

impl SourceIdentifiers {
    /// Index the identifiers of each fact `cited` names. An id that names
    /// no fact of `nodes` and `edges` has none.
    pub fn of(nodes: &[Node], edges: &[Edge], cited: &BTreeSet<String>) -> Self {
        let mut by_citation = BTreeMap::new();
        if cited.is_empty() {
            return Self { by_citation };
        }
        for node in nodes {
            let id = citation_id(&FactKey::Node {
                id: node.id.clone(),
            });
            if !cited.contains(&id) {
                continue;
            }
            let mut found = BTreeSet::new();
            if !DOCUMENT_LABELS.contains(&node.label.as_str()) {
                paths(&node.props, &format!("node:{}", node.id), &mut found);
            }
            symbols(node, &mut found);
            by_citation.insert(id, found);
        }
        for edge in edges {
            let id = citation_id(&FactKey::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            });
            if !cited.contains(&id) {
                continue;
            }
            let mut found = BTreeSet::new();
            // A link from a documentation section rests on the document.
            if !edge.src.starts_with("docsec:") && !edge.src.starts_with("doc:") {
                let identity = format!("edge:{} {} {}", edge.src, edge.label, edge.dst);
                paths(&edge.props, &identity, &mut found);
            }
            by_citation.insert(id, found);
        }
        Self { by_citation }
    }

    /// Whether `statement` spells out an identifier of a fact in
    /// `citations`, as a whole token.
    pub fn named_in(&self, statement: &str, citations: &[String]) -> bool {
        citations
            .iter()
            .filter_map(|id| self.by_citation.get(id))
            .flatten()
            .any(|identifier| contains_token(statement, identifier))
    }

    /// The position of the first of `claims`, each a statement with its
    /// citation ids, that names a source identifier it cites.
    pub fn first_naming<'a>(
        &self,
        claims: impl IntoIterator<Item = (&'a str, &'a [String])>,
    ) -> Option<usize> {
        claims
            .into_iter()
            .position(|(statement, citations)| self.named_in(statement, citations))
    }
}

/// The text each cited fact shows a reader, for checking the route
/// literals a statement spells out against the facts it cites (#2328): a
/// node's id, label and props without its provenance, or an edge's ends,
/// label and props.
#[derive(Debug, Default)]
pub struct CitedText {
    by_citation: BTreeMap<String, String>,
    /// Every route any fact of the graph shows, for telling a route
    /// fragment (`custom/new`) from prose such as `and/or` (#2335).
    routes: BTreeSet<String>,
}

impl CitedText {
    /// Index the text of each fact of `nodes`, `edges` and the derived
    /// `overlay` nodes that `cited` names.
    pub fn of(nodes: &[Node], edges: &[Edge], overlay: &[Node], cited: &BTreeSet<String>) -> Self {
        let mut by_citation = BTreeMap::new();
        if cited.is_empty() {
            return Self::default();
        }
        let mut routes = BTreeSet::new();
        let shown = |props: &serde_json::Value| {
            let mut props = props.clone();
            if let Some(map) = props.as_object_mut() {
                map.remove("prov");
            }
            props.to_string()
        };
        for node in nodes.iter().chain(overlay) {
            let id = citation_id(&FactKey::Node {
                id: node.id.clone(),
            });
            if cited.contains(&id) {
                let text = format!("{} {} {}", node.id, node.label, shown(&node.props));
                by_citation.insert(id, text);
            }
            routes.extend(route_literals(&node.id).into_iter().map(str::to_owned));
            routes_in(&node.props, &mut routes);
        }
        for edge in edges {
            let id = citation_id(&FactKey::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            });
            if cited.contains(&id) {
                let text = format!(
                    "{} {} {} {}",
                    edge.src,
                    edge.label,
                    edge.dst,
                    shown(&edge.props)
                );
                by_citation.insert(id, text);
            }
        }
        Self {
            by_citation,
            routes,
        }
    }

    /// The first route `statement` spells out that no fact of `citations`
    /// shows, if any: a route literal, or a multi-segment fragment of a
    /// route the graph shows, such as `custom/new` for
    /// `/api/alias/custom/new` (#2335).
    pub fn uncited_route<'a>(&self, statement: &'a str, citations: &[String]) -> Option<&'a str> {
        let cited = || citations.iter().filter_map(|id| self.by_citation.get(id));
        route_literals(statement)
            .into_iter()
            .find(|route| !cited().any(|text| contains_route(text, route)))
            .or_else(|| {
                route_fragments(statement)
                    .into_iter()
                    .filter(|fragment| {
                        self.routes
                            .iter()
                            .any(|route| contains_segments(route, fragment))
                    })
                    .find(|fragment| !cited().any(|text| contains_segments(text, fragment)))
            })
    }
}

/// Collect the route literals in every string of `value`.
fn routes_in(value: &serde_json::Value, routes: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(text) => {
            routes.extend(route_literals(text).into_iter().map(str::to_owned));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| routes_in(item, routes)),
        serde_json::Value::Object(map) => map
            .iter()
            .filter(|(key, _)| key.as_str() != "prov")
            .for_each(|(_, item)| routes_in(item, routes)),
        _ => {}
    }
}

/// The words of `statement` shaped like a route without its leading `/`:
/// two or more non-empty segments joined by `/`, the first starting with
/// a letter or a digit.
fn route_fragments(statement: &str) -> Vec<&str> {
    const TRIM: &[char] = &[
        '"', '\'', '`', '(', ')', '[', ']', ',', '.', ';', ':', '!', '?',
    ];
    statement
        .split_whitespace()
        .map(|word| word.trim_matches(TRIM))
        .filter(|word| {
            word.chars().next().is_some_and(char::is_alphanumeric)
                && word.contains('/')
                && word.split('/').all(|segment| !segment.is_empty())
        })
        .collect()
}

/// Whether `text` holds `fragment` as whole path segments: starting a
/// word or after a `/`, and ending the path or before a `/`.
fn contains_segments(text: &str, fragment: &str) -> bool {
    let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '<' | '{' | '.');
    let more = |c: char| part(c) || matches!(c, ':' | '*');
    text.match_indices(fragment).any(|(start, matched)| {
        let end = start + matched.len();
        let before = !text[..start].chars().next_back().is_some_and(part);
        let after = !text[end..].chars().next().is_some_and(more);
        before && after
    })
}

/// The route literals `statement` spells out: each whitespace-separated
/// word that, without surrounding quotes, brackets and punctuation, starts
/// with `/` followed by a letter, a digit or `<`/`{`/`:`.
pub fn route_literals(statement: &str) -> Vec<&str> {
    const TRIM: &[char] = &[
        '"', '\'', '`', '(', ')', '[', ']', ',', '.', ';', ':', '!', '?',
    ];
    statement
        .split_whitespace()
        .map(|word| word.trim_matches(TRIM))
        .filter(|word| {
            word.strip_prefix('/')
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '<' | '{' | ':'))
        })
        .collect()
}

/// Whether `text` holds `route` as a whole route: not inside a longer path
/// segment, and not followed by more of the path.
fn contains_route(text: &str, route: &str) -> bool {
    let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '<' | '{' | '.');
    // A parameter (`:id`) or a wildcard (`*`) continues a route too.
    let more = |c: char| part(c) || matches!(c, ':' | '*');
    text.match_indices(route).any(|(start, matched)| {
        let end = start + matched.len();
        let before = !text[..start].chars().next_back().is_some_and(part);
        let after = !text[end..].chars().next().is_some_and(more);
        before && after
    })
}

fn contains_token(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(start, matched)| {
        let end = start + matched.len();
        let before = !text[..start].chars().next_back().is_some_and(word);
        let after = !text[end..].chars().next().is_some_and(word);
        before && after
    })
}

#[cfg(test)]
mod tests {
    //! AC-0683 (T-0683).
    use super::*;
    use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
    use serde_json::json;

    fn prov(path: &str, fact: &str) -> Provenance {
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![EvidenceRef {
                repo: "acme/shop".into(),
                path: path.into(),
                byte_start: 1,
                byte_end: 9,
                commit_sha: "abc123".into(),
            }],
            "spec.neutrality.test",
            fact.as_bytes(),
        )
        .unwrap()
    }

    fn node(id: &str, label: &str, name: &str, path: &str) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({ "name": name, "prov": prov(path, id) }),
        }
    }

    fn cite(id: &str) -> String {
        citation_id(&FactKey::Node { id: id.into() })
    }

    fn graph() -> Vec<Node> {
        vec![
            node(
                "sym:create",
                "Symbol",
                "orders.service.create_order",
                "src/orders/service.py",
            ),
            node("sym:plain", "Function", "checkout", "src/checkout.ts"),
            node(
                "sym:class",
                "Symbol",
                "OrderService",
                "src/orders/service.py",
            ),
            node(
                "ep:orders",
                "Endpoint",
                "POST /orders",
                "src/orders/routes.py",
            ),
            node("ent:order", "DataEntity", "order_items", "db/schema.sql"),
            node("docsec:readme", "DocSection", "Orders", "README.md"),
        ]
    }

    fn index(ids: &[&str]) -> (SourceIdentifiers, Vec<String>) {
        let citations: Vec<String> = ids.iter().map(|id| cite(id)).collect();
        let cited = citations.iter().cloned().collect();
        (SourceIdentifiers::of(&graph(), &[], &cited), citations)
    }

    #[test]
    fn a_statement_that_names_a_cited_path_or_symbol_is_found() {
        let (names, citations) = index(&["sym:create", "sym:class"]);
        for statement in [
            "Orders are created by create_order.",
            "The orders.service.create_order function stores an order.",
            "OrderService stores an order.",
            "An order is stored (see src/orders/service.py).",
            "The logic is in service.py.",
            "Orders are created by \u{201c}create_order\u{201d}.",
            "Orders are created by create_order\u{2014}then stored.",
        ] {
            assert!(names.named_in(statement, &citations), "{statement}");
        }
        // A letter of another script still continues the token.
        assert!(!names.named_in("The create_orderé helper.", &citations));
        assert_eq!(
            names.first_naming([
                ("A signed-in customer can place an order.", &citations[..]),
                ("OrderService stores it.", &citations[..]),
            ]),
            Some(1)
        );
    }

    #[test]
    fn domain_terms_and_the_observable_contract_are_not_findings() {
        let (names, citations) = index(&[
            "sym:create",
            "sym:plain",
            "sym:class",
            "ep:orders",
            "ent:order",
            "docsec:readme",
        ]);
        for statement in [
            "A signed-in customer can place an order and gets status 201.",
            // A plain word is not told apart from a domain term.
            "The customer completes checkout.",
            // Method, path and field names are the contract.
            "POST /orders stores one row in order_items.",
            // A documentation file is not the implementation.
            "README.md states that orders cannot be changed after payment.",
            // Part of a longer token is not the identifier.
            "The create_order_draft step is optional.",
            "MyOrderService is another thing.",
        ] {
            assert!(!names.named_in(statement, &citations), "{statement}");
        }
        assert_eq!(
            names.first_naming([("An order is stored.", &citations[..])]),
            None
        );
    }

    #[test]
    fn only_the_facts_a_claim_cites_are_compared() {
        let (names, _) = index(&["sym:create", "ep:orders"]);
        let endpoint = [cite("ep:orders")];
        // The symbol is in the index but this claim does not cite it.
        assert!(!names.named_in("Orders are created by create_order.", &endpoint));
        // The endpoint's own source path is an identifier of it.
        assert!(names.named_in("Handled in src/orders/routes.py.", &endpoint));
        // An id that names no fact has no identifier.
        let unknown = ["F-0000000000000000".to_owned()];
        assert!(!names.named_in("create_order", &unknown));
        assert_eq!(
            SourceIdentifiers::of(&graph(), &[], &BTreeSet::new()),
            SourceIdentifiers::default()
        );
    }

    #[test]
    fn an_edge_gives_its_evidence_paths_and_a_documentation_link_gives_none() {
        let edges = vec![
            Edge {
                src: "ep:orders".into(),
                dst: "sym:create".into(),
                label: "HANDLES".into(),
                props: json!({ "prov": prov("src/orders/routes.py", "e1") }),
            },
            Edge {
                src: "docsec:readme".into(),
                dst: "ep:orders".into(),
                label: "MENTIONS".into(),
                props: json!({ "prov": prov("README.md", "e2") }),
            },
        ];
        let ids: Vec<String> = edges
            .iter()
            .map(|edge| {
                citation_id(&FactKey::Edge {
                    source: edge.src.clone(),
                    label: edge.label.clone(),
                    destination: edge.dst.clone(),
                })
            })
            .collect();
        let names = SourceIdentifiers::of(&graph(), &edges, &ids.iter().cloned().collect());
        assert!(names.named_in("Handled in routes.py.", &ids[..1]));
        assert!(!names.named_in("README.md mentions the route.", &ids[1..]));
    }

    // AC-0907: a projected statement carries no code-shaped token outside
    // a quotation.
    #[test]
    fn a_projected_statement_with_a_code_token_is_found() {
        assert_eq!(
            projected_code_token("When not user.can_create_new_alias(), it is refused."),
            Some("user.can_create_new_alias()")
        );
        assert_eq!(
            projected_code_token("The configured max is 5 unless MAX_ALIAS is set."),
            Some("MAX_ALIAS")
        );
        assert_eq!(
            projected_code_token("An isPremium user."),
            Some("isPremium")
        );
        for clean in [
            "When the user is not active, the request is refused with status 401.",
            "The check “is premium” decides as follows (matched by name, 2 call hops):",
            "It fails with an error (“wrong api_key”).",
            "A transfer at 0.5 is refused for GET /api/aliases.",
        ] {
            assert_eq!(projected_code_token(clean), None, "{clean}");
        }
    }

    /// AC-1314 (T-1314): a route a statement spells out must be shown by a
    /// fact it cites, as a whole route (#2328).
    #[test]
    fn a_route_literal_must_be_shown_by_a_cited_fact() {
        assert_eq!(
            route_literals("Clients call `GET /api/aliases`, then (/api/aliases/<int:id>)."),
            ["/api/aliases", "/api/aliases/<int:id>"]
        );
        assert!(route_literals("Answers 4/5 and/or a / b.").is_empty());
        let endpoint = Node {
            id: "endpoint:GET /api/aliases".into(),
            label: "Endpoint".into(),
            props: json!({"path": "/api/aliases", "prov": prov("app/api/admin/users.py", "e")}),
        };
        let capability = Node {
            id: "cap:alias".into(),
            label: "Capability".into(),
            props: json!({"title": "Manage alias (API)"}),
        };
        let ids = |node: &Node| {
            citation_id(&FactKey::Node {
                id: node.id.clone(),
            })
        };
        let (e, c) = (ids(&endpoint), ids(&capability));
        let cited: BTreeSet<String> = [e.clone(), c.clone()].into();
        let (e, c) = (vec![e], vec![c]);
        let text = CitedText::of(&[endpoint], &[], &[capability], &cited);
        let claim = "Users list aliases through GET /api/aliases.";
        assert_eq!(text.uncited_route(claim, &e), None);
        // A capability title does not show the route.
        assert_eq!(text.uncited_route(claim, &c), Some("/api/aliases"));
        // A longer route, a path segment of a provenance file, or a route the
        // fact holds only as a prefix is not shown.
        assert_eq!(
            text.uncited_route("It serves /api/aliases/<int:id>.", &e),
            Some("/api/aliases/<int:id>")
        );
        assert_eq!(
            text.uncited_route("It lives under /api/admin.", &e),
            Some("/api/admin")
        );
        assert_eq!(text.uncited_route("It is /api.", &e), Some("/api"));
        // An Express-style parameter continues the route (#2328).
        let users = Node {
            id: "endpoint:GET /users/:id".into(),
            label: "Endpoint".into(),
            props: json!({"path": "/users/:id"}),
        };
        let u = vec![ids(&users)];
        let text = CitedText::of(&[users], &[], &[], &u.iter().cloned().collect());
        assert_eq!(text.uncited_route("It lists /users.", &u), Some("/users"));
        assert_eq!(text.uncited_route("It reads /users/:id.", &u), None);
        // A route right after an id's colon still counts.
        let bare = Node {
            id: "endpoint:/health".into(),
            label: "Endpoint".into(),
            props: json!({}),
        };
        let h = vec![ids(&bare)];
        let text = CitedText::of(&[bare], &[], &[], &h.iter().cloned().collect());
        assert_eq!(text.uncited_route("It answers /health.", &h), None);
    }

    /// AC-1316 (T-1316): a fragment of a route the graph shows, spelled
    /// without its leading `/`, must be shown by a fact it cites (#2335).
    #[test]
    fn a_route_fragment_must_be_shown_by_a_cited_fact() {
        let endpoint = |route: &str| Node {
            id: format!("endpoint:POST {route}"),
            label: "Endpoint".into(),
            props: json!({"path": route}),
        };
        let custom = endpoint("/api/alias/custom/new");
        let random = endpoint("/api/alias/random/new");
        let capability = Node {
            id: "cap:alias".into(),
            label: "Capability".into(),
            props: json!({"title": "Create alias"}),
        };
        let ids = |node: &Node| {
            citation_id(&FactKey::Node {
                id: node.id.clone(),
            })
        };
        let (k, c) = (ids(&custom), ids(&capability));
        let cited: BTreeSet<String> = [k.clone(), c.clone()].into();
        let text = CitedText::of(&[custom, random], &[], &[capability], &cited);
        let claim = "Users create aliases via custom/new or random/new.";
        // Citing only the capability shows neither fragment.
        assert_eq!(
            text.uncited_route(claim, std::slice::from_ref(&c)),
            Some("custom/new")
        );
        // Citing the custom endpoint shows its fragment, not the other one,
        // which is not cited here.
        assert_eq!(
            text.uncited_route(claim, std::slice::from_ref(&k)),
            Some("random/new")
        );
        assert_eq!(
            text.uncited_route("It accepts alias/custom/new.", std::slice::from_ref(&k)),
            None
        );
        // A fragment must match whole segments of a route.
        assert_eq!(
            text.uncited_route("It uses stom/new.", std::slice::from_ref(&c)),
            None
        );
        // Prose that matches no route of the graph is not a route.
        assert_eq!(
            text.uncited_route("It reads and/or writes input/output.", &[c]),
            None
        );
    }
}
