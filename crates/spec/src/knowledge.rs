//! Pure knowledge-layer candidates; publication remains an overlay operation.
use core_graph::{
    Node, RevisionedSnapshot,
    overlay::{BaseRef, MAX_FACT_BYTES, OverlayFact, OverlayItem},
};
use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};

/// Deterministic recipe naming a semantic resolver-group capability.
pub const CAPABILITY_GRAPHQL_RULE: &str = "knowledge.capability.graphql-resolver-group";
/// Changed grouping, support or naming semantics require a new recipe version.
pub const CAPABILITY_GRAPHQL_RULE_VERSION: &str = "1";

fn identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn support(props: &serde_json::Value) -> Option<Provenance> {
    let prov: Provenance = serde_json::from_value(props.get("prov")?.clone()).ok()?;
    (prov.validate().is_ok()
        && prov.tier == Tier::Deterministic
        && prov.extractor_id == "t0.adapter-ts"
        && matches!(
            prov.confidence_tier,
            ConfidenceTier::Confirmed | ConfidenceTier::InferredStrong
        )
        && !prov.evidence.is_empty()
        && prov.evidence.iter().all(|e| {
            !e.repo.is_empty()
                && !e.path.is_empty()
                && !e.commit_sha.is_empty()
                && e.byte_start < e.byte_end
        }))
    .then_some(prov)
}
fn same_owner(prov: &Provenance, owner: &EvidenceRef) -> bool {
    prov.evidence
        .iter()
        .all(|e| e.repo == owner.repo && e.path == owner.path && e.commit_sha == owner.commit_sha)
}
fn scoped_path(path: &str, api: &str) -> bool {
    let segments: Vec<_> = path.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || matches!(*s, "." | "..") || s.contains('\\'))
    {
        return false;
    }
    segments
        .iter()
        .rposition(|s| *s == "resolvers")
        .is_some_and(|at| at + 2 < segments.len() && segments[at + 1] == api)
}

/// Candidates and named omissions from this producer, before aggregate admission.
/// The coordinator still enforces total fact/byte caps across all producers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CapabilityReport {
    /// Individually serializable facts within the overlay per-fact cap.
    pub facts: Vec<OverlayFact>,
    /// Resolver groups omitted because exact OverlayFact JSON exceeded the cap.
    pub oversized_groups: usize,
}
fn retain_sized_fact(report: &mut CapabilityReport, fact: OverlayFact) {
    // Exactly the serialization/currency used by overlay::admit, including
    // sidecar citations, UTF-8 and JSON escaping; no parallel size estimator.
    let bytes = serde_json::to_string(&fact)
        .expect("overlay fact serialization")
        .len();
    if bytes > MAX_FACT_BYTES {
        report.oversized_groups += 1;
    } else {
        report.facts.push(fact);
    }
}

type EvidenceKey = (String, String, u64, u64, String);
#[derive(Default)]
struct Group {
    class: String,
    cites: BTreeSet<BaseRef>,
    evidence: BTreeMap<EvidenceKey, EvidenceRef>,
}

/// Derive Capability candidates from the existing import-proven TS resolver
/// facts. The borrowed base is unchanged. This checks structural consistency,
/// not database authenticity or publication currency; OverlayStore owns those.
/// Unsupported/ambiguous pairs are omitted, never converted to business claims.
pub fn derive_graphql_resolver_capabilities(base: &RevisionedSnapshot) -> CapabilityReport {
    let mut nodes: BTreeMap<&str, Option<&Node>> = BTreeMap::new();
    for node in &base.graph.0 {
        nodes
            .entry(&node.id)
            .and_modify(|v| *v = None)
            .or_insert(Some(node));
    }
    let mut handles: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    for edge in base.graph.1.iter().filter(|e| e.label == "HANDLES") {
        handles.entry(&edge.src).or_default().push(edge);
    }
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    for endpoint in nodes.values().filter_map(|n| *n) {
        let props = &endpoint.props;
        if endpoint.label != "Endpoint"
            || props["protocol"] != "graphql"
            || props["framework"] != "nestjs-graphql"
            || props["scope_basis"] != "resolver_directory"
        {
            continue;
        }
        let (Some(api), Some(operation), Some(field)) = (
            props["api"].as_str(),
            props["operation"].as_str(),
            props["field"].as_str(),
        ) else {
            continue;
        };
        if api == "default"
            || !identifier(api)
            || !identifier(field)
            || !matches!(operation, "query" | "mutation")
            || props["method"] != operation.to_ascii_uppercase()
            || props["path"] != format!("{api}.{field}")
        {
            continue;
        }
        let Some(links) = handles.get(endpoint.id.as_str()) else {
            continue;
        };
        let [edge] = links.as_slice() else { continue }; // duplicate or multiple ownership is ambiguous
        let Some(Some(handler)) = nodes.get(edge.dst.as_str()) else {
            continue;
        };
        let (Some(class), Some(name)) = (
            handler.props["class"].as_str(),
            handler.props["name"].as_str(),
        ) else {
            continue;
        };
        let Some((owner_class, member)) = name.rsplit_once('.') else {
            continue;
        };
        if handler.label != "Symbol"
            || handler.props["kind"] != "Method"
            || owner_class != class
            || !identifier(class)
            || !identifier(member)
        {
            continue;
        }
        let (Some(endpoint_prov), Some(edge_prov), Some(handler_prov)) = (
            support(props),
            support(&edge.props),
            support(&handler.props),
        ) else {
            continue;
        };
        let owner = &handler_prov.evidence[0];
        if !same_owner(&handler_prov, owner)
            || !same_owner(&endpoint_prov, owner)
            || !same_owner(&edge_prov, owner)
            || !scoped_path(&owner.path, api)
            || handler.id != format!("sym:{}@{}#{name}", owner.repo, owner.path)
            || endpoint.id != format!("ep:{}@graphql:{api}:{operation}:{field}", owner.repo)
        {
            continue;
        }
        let resolver = format!("sym:{}@{}#{class}", owner.repo, owner.path);
        let group = groups
            .entry((api.to_owned(), resolver))
            .or_insert_with(|| Group {
                class: class.to_owned(),
                ..Group::default()
            });
        group.cites.extend([
            BaseRef::Node(endpoint.id.clone()),
            BaseRef::Node(handler.id.clone()),
            BaseRef::Edge(edge.src.clone(), edge.dst.clone(), edge.label.clone()),
        ]);
        for prov in [&endpoint_prov, &edge_prov, &handler_prov] {
            for e in &prov.evidence {
                group
                    .evidence
                    .entry((
                        e.repo.clone(),
                        e.path.clone(),
                        e.byte_start,
                        e.byte_end,
                        e.commit_sha.clone(),
                    ))
                    .or_insert_with(|| e.clone());
            }
        }
    }
    let mut report = CapabilityReport::default();
    for ((api, resolver), group) in groups {
        let key = serde_json::to_vec(&(
            CAPABILITY_GRAPHQL_RULE,
            CAPABILITY_GRAPHQL_RULE_VERSION,
            &api,
            &resolver,
        ))
        .expect("string tuple serialization");
        let id = format!(
            "capability:graphql-resolver-group:{}",
            core_prov::content_hash(&key)
        );
        let title = format!("{api} GraphQL operations in {}", group.class);
        let content = serde_json::to_vec(&serde_json::json!({"title":&title,"cites":&group.cites}))
            .expect("string and citation serialization");
        let prov = Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredWeak,
            group.evidence.into_values().collect(),
            "t2.knowledge.capability.graphql-resolver-group",
            &content,
        )
        .expect("semantic weak is within the confidence ceiling")
        .with_method(ProducingMethod::Rule {
            rule: CAPABILITY_GRAPHQL_RULE.into(),
            version: CAPABILITY_GRAPHQL_RULE_VERSION.into(),
        });
        let fact = OverlayFact {
            item: OverlayItem::Node(Node {
                id,
                label: "Capability".into(),
                props: serde_json::json!({"title":title,"prov":prov}),
            }),
            cites: group.cites,
        };
        retain_sized_fact(&mut report, fact);
    }
    report
}

#[cfg(test)]
#[path = "knowledge_tests.rs"]
mod tests;
