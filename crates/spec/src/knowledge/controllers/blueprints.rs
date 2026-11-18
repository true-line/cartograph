//! Capability candidates from Flask Blueprints: a route group with a name
//! but no class.
use super::*;

const EXTRACTOR: &str = "t0.adapter-python";

/// Add one Capability candidate per Flask Blueprint and handler file, under
/// the controller rule: every import-proven route the Blueprint serves from
/// that file, citing the endpoint, its handler function and the relation
/// between them. The group is the dotted Blueprint name the adapter recorded
/// before the route's own endpoint name, so a route registered on the
/// application itself, or under a Blueprint whose name is not a literal,
/// belongs to no group. The adapter emits no fact for the Blueprint object,
/// so the handler's file is part of the identity: two Blueprints that share a
/// name are never merged across files, and one Blueprint whose routes sit in
/// two files gives a candidate for each. An endpoint's support may cite the
/// registration chain in other files of the same repository and commit.
pub(super) fn derive(
    nodes: &BTreeMap<&str, Option<&Node>>,
    handles: &BTreeMap<&str, Vec<&core_graph::Edge>>,
    report: &mut CapabilityReport,
) {
    let mut groups: BTreeMap<(String, String, String), Group> = BTreeMap::new();
    for endpoint in nodes.values().filter_map(|n| *n) {
        let props = &endpoint.props;
        if endpoint.label != "Endpoint"
            || props["framework"] != "flask"
            || props["language"] != "python"
        {
            continue;
        }
        let (Some(method), Some(path), Some(endpoint_name)) = (
            props["method"].as_str(),
            props["path"].as_str(),
            props["endpoint_name"].as_str(),
        ) else {
            continue;
        };
        let Some((blueprint, own)) = endpoint_name.rsplit_once('.') else {
            continue; // an application route
        };
        if own.is_empty() || blueprint.split('.').any(str::is_empty) {
            continue;
        }
        let Some(links) = handles.get(endpoint.id.as_str()) else {
            continue;
        };
        let [edge] = links.as_slice() else { continue }; // several handlers are ambiguous
        let Some(Some(handler)) = nodes.get(edge.dst.as_str()) else {
            continue;
        };
        let Some(name) = handler.props["name"].as_str() else {
            continue;
        };
        if handler.label != "Symbol"
            || handler.props["kind"] != "function"
            || handler.props["language"] != "python"
            || props["handler_sym"] != handler.id.as_str()
        {
            continue;
        }
        let (Some(endpoint_prov), Some(edge_prov), Some(handler_prov)) = (
            support_from(props, EXTRACTOR),
            support_from(&edge.props, EXTRACTOR),
            support_from(&handler.props, EXTRACTOR),
        ) else {
            continue;
        };
        let owner = &handler_prov.evidence[0];
        let same_revision = |prov: &Provenance| {
            prov.evidence
                .iter()
                .all(|e| e.repo == owner.repo && e.commit_sha == owner.commit_sha)
        };
        if !same_owner(&handler_prov, owner)
            || !same_revision(&endpoint_prov)
            || !same_revision(&edge_prov)
            || handler.id != format!("sym:{}@{}#{name}", owner.repo, owner.path)
            || endpoint.id != format!("ep:{}@{method}:{path}", owner.repo)
        {
            continue;
        }
        let group = groups
            .entry((owner.repo.clone(), owner.path.clone(), blueprint.to_owned()))
            .or_insert_with(|| Group {
                class: blueprint.to_owned(),
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
    for ((repo, path, blueprint), group) in groups {
        let key = serde_json::to_vec(&(
            CAPABILITY_CONTROLLER_RULE,
            CAPABILITY_CONTROLLER_RULE_VERSION,
            "flask-blueprint",
            &repo,
            &path,
            &blueprint,
        ))
        .expect("string tuple serialization");
        let title = format!("HTTP operations in blueprint {}", group.class);
        let fact = capability_fact(
            (
                CAPABILITY_CONTROLLER_RULE,
                CAPABILITY_CONTROLLER_RULE_VERSION,
            ),
            "http-controller",
            &key,
            title,
            group,
        );
        retain_sized_fact(report, fact);
    }
}

#[cfg(test)]
mod tests;
