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

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
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

fn contains_token(text: &str, token: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(token).any(|(start, matched)| {
        let end = start + matched.len();
        let before = start == 0 || !word(bytes[start - 1]);
        let after = end == bytes.len() || !word(bytes[end]);
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
        ] {
            assert!(names.named_in(statement, &citations), "{statement}");
        }
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
}
