//! Pure knowledge-layer candidates; publication remains an overlay operation.
mod business_flows;
pub use business_flows::{
    BUSINESS_FLOW_RULE, BUSINESS_FLOW_RULE_VERSION, BusinessFlowOmission, BusinessFlowReport,
    derive_business_flows,
};
mod controllers;
pub use controllers::derive_controller_capabilities;
mod entry_points;
pub use entry_points::{
    CAPABILITY_ENTRY_POINT_RULE, CAPABILITY_ENTRY_POINT_RULE_VERSION,
    derive_entry_point_capabilities,
};
pub(crate) mod actors;
mod naming;
mod subsystems;
pub use actors::{
    ACTOR_ALLOW_ASSUMPTION, ACTOR_ALLOW_RULE, ACTOR_ALLOW_RULE_VERSION, ActorReport,
    derive_permission_actors,
};
use core_graph::{
    Node, RevisionedSnapshot,
    overlay::{BaseRef, MAX_FACT_BYTES, OverlayFact, OverlayItem},
};
use core_prov::{ConfidenceTier, EvidenceBasis, EvidenceRef, ProducingMethod, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
pub use subsystems::{
    MAX_SUBSYSTEM_FILES, SUBSYSTEM_FILE_CAP, SUBSYSTEM_RULE, SUBSYSTEM_RULE_VERSION,
    SubsystemReport, derive_subsystems,
};

/// Deterministic recipe naming a semantic resolver-group capability.
pub const CAPABILITY_GRAPHQL_RULE: &str = "knowledge.capability.graphql-resolver-group";
/// Changed grouping, support or naming semantics require a new recipe version.
pub const CAPABILITY_GRAPHQL_RULE_VERSION: &str = "3";

/// Deterministic recipe naming a controller class's HTTP operations.
pub const CAPABILITY_CONTROLLER_RULE: &str = "knowledge.capability.http-controller";
/// Changed grouping, support or naming semantics require a new recipe version.
pub const CAPABILITY_CONTROLLER_RULE_VERSION: &str = "7";

/// Deterministic recipe projecting a persistence model to a weak domain candidate.
pub const DOMAIN_ENTITY_ORM_RULE: &str = "knowledge.domain-entity.orm-model";
/// Changed eligibility, identity or projection semantics require a new version.
pub const DOMAIN_ENTITY_ORM_RULE_VERSION: &str = "2";

/// Candidates and mappings admitted under the overlay's per-fact byte cap.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DomainEntityReport {
    /// Candidate/mapping pairs small enough for overlay admission.
    pub facts: Vec<OverlayFact>,
    /// Models omitted because either exact serialized fact exceeded the cap.
    pub oversized_models: usize,
}

fn orm_model_support(node: &Node) -> Option<(Provenance, String, String)> {
    let (storage, expected_extractor, columns_key) = match node.props["storage"].as_str()? {
        "jpa" => ("jpa", "t0.adapter-java", "fields"),
        "sqlalchemy" => ("sqlalchemy", "t0.adapter-python", "columns"),
        "gorm" => ("gorm", "t0.adapter-go", "columns"),
        _ => return None,
    };
    let name = node.props["name"].as_str()?.trim();
    let table = node.props["table"].as_str()?.trim();
    let store = node.props["store"].as_str()?.trim();
    let table_source = node.props.get("table_source").and_then(|v| v.as_str());
    if name.is_empty()
        || table.is_empty()
        || table != store
        // The JPA adapter always classifies the source of the table name.
        // Missing and unresolved values cannot prove a stable physical table.
        || (storage == "jpa" && !matches!(table_source, Some("explicit" | "default")))
        // Preserve fail-closed validation if another extractor supplies this field.
        || (node.props.get("table_source").is_some()
            && !matches!(table_source, Some("explicit" | "default")))
        || node
            .props
            .get("schema")
            .is_some_and(|v| v.as_str().is_none_or(str::is_empty))
    {
        return None;
    }
    let columns = node.props[columns_key].as_array()?;
    if columns.is_empty()
        || columns.iter().any(|column| {
            column["name"]
                .as_str()
                .is_none_or(|field| field.trim().is_empty())
        })
    {
        return None;
    }
    node.props.get("prov")?;
    let prov: Provenance =
        serde_json::from_value(crate::stored_prov(&node.props).into_owned()).ok()?;
    if prov.validate().is_err()
        || prov.tier != Tier::Deterministic
        || prov.confidence_tier != ConfidenceTier::Confirmed
        || prov.extractor_id != expected_extractor
        || !matches!(prov.basis, None | Some(EvidenceBasis::Source))
        || prov.evidence.is_empty()
        || prov.evidence.iter().any(|e| {
            e.repo.trim().is_empty()
                || e.path.trim().is_empty()
                || e.commit_sha.trim().is_empty()
                || e.byte_start >= e.byte_end
        })
    {
        return None;
    }
    Some((prov, name.to_owned(), storage.to_owned()))
}

fn domain_provenance(evidence: Vec<EvidenceRef>, identity: &[u8]) -> Provenance {
    Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
        evidence,
        "t2.knowledge.domain-entity.orm-model",
        identity,
    )
    .expect("semantic weak is within the confidence ceiling")
    .with_method(ProducingMethod::Rule {
        rule: DOMAIN_ENTITY_ORM_RULE.into(),
        version: DOMAIN_ENTITY_ORM_RULE_VERSION.into(),
    })
}

/// Derive weak DomainEntity candidates only from structurally validated JPA
/// and SQLAlchemy DataEntity facts. The class name is display text, not the
/// eligibility signal; each candidate maps explicitly to its persistence row.
pub fn derive_orm_domain_entities(base: &RevisionedSnapshot) -> DomainEntityReport {
    let mut by_id: BTreeMap<&str, Option<&Node>> = BTreeMap::new();
    for node in &base.graph.0 {
        by_id
            .entry(&node.id)
            .and_modify(|value| *value = None)
            .or_insert(Some(node));
    }
    let mut report = DomainEntityReport::default();
    for node in by_id.values().filter_map(|node| *node) {
        if node.label != "DataEntity" {
            continue;
        }
        let Some((source_prov, name, storage)) = orm_model_support(node) else {
            continue;
        };
        let key = serde_json::to_vec(&(
            DOMAIN_ENTITY_ORM_RULE,
            DOMAIN_ENTITY_ORM_RULE_VERSION,
            &node.id,
        ))
        .expect("rule identity serializes");
        let id = format!("domain-entity:orm-model:{}", core_prov::content_hash(&key));
        let mut props = serde_json::json!({
            "name": name,
            "backing_data_entity_id": node.id,
            "storage": storage,
            "table": node.props["table"],
        });
        if let Some(schema) = node.props.get("schema") {
            props["schema"] = schema.clone();
        }
        let cite = BaseRef::Node(node.id.clone());
        let candidate_bytes = serde_json::to_vec(&(
            DOMAIN_ENTITY_ORM_RULE,
            DOMAIN_ENTITY_ORM_RULE_VERSION,
            &props,
            &cite,
        ))
        .expect("candidate content serializes");
        props["prov"] = serde_json::json!(domain_provenance(
            source_prov.evidence.clone(),
            &candidate_bytes,
        ));
        let candidate = OverlayFact {
            item: OverlayItem::Node(Node {
                id: id.clone(),
                label: "DomainEntity".into(),
                props,
            }),
            cites: BTreeSet::from([cite.clone()]),
        };
        let edge_content = serde_json::to_vec(&(
            DOMAIN_ENTITY_ORM_RULE,
            DOMAIN_ENTITY_ORM_RULE_VERSION,
            &id,
            &node.id,
            "MAPS_TO",
            &cite,
        ))
        .expect("mapping content serializes");
        let mapping = OverlayFact {
            item: OverlayItem::Edge(core_graph::Edge {
                src: id,
                dst: node.id.clone(),
                label: "MAPS_TO".into(),
                props: serde_json::json!({
                    "prov": domain_provenance(source_prov.evidence, &edge_content)
                }),
            }),
            cites: BTreeSet::from([cite]),
        };
        if [&candidate, &mapping].into_iter().any(|fact| {
            serde_json::to_string(fact)
                .expect("overlay fact serialization")
                .len()
                > MAX_FACT_BYTES
        }) {
            report.oversized_models += 1;
        } else {
            report.facts.extend([candidate, mapping]);
        }
    }
    report
}

// GraphQL field names use ASCII Name grammar; TS class/member identities do not.
fn identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn support(props: &serde_json::Value) -> Option<Provenance> {
    support_from(props, "t0.adapter-ts")
}
/// Source-backed, confident deterministic support from one named adapter.
fn support_from(props: &serde_json::Value, extractor: &str) -> Option<Provenance> {
    props.get("prov")?;
    let prov: Provenance = serde_json::from_value(crate::stored_prov(props).into_owned()).ok()?;
    (prov.validate().is_ok()
        && prov.tier == Tier::Deterministic
        && prov.extractor_id == extractor
        && matches!(
            prov.confidence_tier,
            ConfidenceTier::Confirmed | ConfidenceTier::InferredStrong
        )
        && matches!(prov.basis, None | Some(EvidenceBasis::Source))
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
    /// Whether any grouped operation changes state; names the title's verb.
    mutates: bool,
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
            || class.is_empty()
            || member.is_empty()
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
        group.mutates |= naming::mutating(operation);
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
        let title = naming::title(
            &naming::subject(&group.class),
            Some(&naming::api_audience(&api)),
            group.mutates,
        );
        let fact = capability_fact(
            (CAPABILITY_GRAPHQL_RULE, CAPABILITY_GRAPHQL_RULE_VERSION),
            "graphql-resolver-group",
            &key,
            title,
            group,
        );
        retain_sized_fact(&mut report, fact);
    }
    report
}

/// One Capability candidate for a group: weak semantic, named by its rule,
/// carrying every evidence span of the facts it cites. `kind` names the
/// candidate family in the node id and extractor id; `key` is its identity.
fn capability_fact(
    (rule, version): (&str, &str),
    kind: &str,
    key: &[u8],
    title: String,
    group: Group,
) -> OverlayFact {
    let id = format!("capability:{kind}:{}", core_prov::content_hash(key));
    let content = serde_json::to_vec(&serde_json::json!({"title":&title,"cites":&group.cites}))
        .expect("string and citation serialization");
    let prov = Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
        group.evidence.into_values().collect(),
        format!("t2.knowledge.capability.{kind}"),
        &content,
    )
    .expect("semantic weak is within the confidence ceiling")
    .with_method(ProducingMethod::Rule {
        rule: rule.into(),
        version: version.into(),
    });
    OverlayFact {
        item: OverlayItem::Node(Node {
            id,
            label: "Capability".into(),
            props: serde_json::json!({"title":title,"prov":prov}),
        }),
        cites: group.cites,
    }
}

#[cfg(test)]
#[path = "knowledge_tests.rs"]
mod tests;
