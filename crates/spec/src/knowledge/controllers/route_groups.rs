//! Capability candidates from route groups that have a name but no class:
//! Flask Blueprints, Go packages and Rust modules.
use super::*;

/// What names the group a route belongs to, for the frameworks whose
/// handlers are plain functions.
#[derive(Clone, Copy)]
enum GroupBy {
    /// Flask: the dotted Blueprint name before the route's endpoint name.
    FlaskBlueprint,
    /// Go: the package directory of the handler function.
    GoPackage,
    /// Rust: the crate and module the handler function is declared in.
    RustModule,
}

/// The adapter and grouping for an endpoint's language, if this rule reads it.
fn group_shape(endpoint: &serde_json::Value) -> Option<(&'static str, GroupBy)> {
    match (
        endpoint["language"].as_str()?,
        endpoint["framework"].as_str()?,
    ) {
        ("python", "flask") => Some(("t0.adapter-python", GroupBy::FlaskBlueprint)),
        ("go", "net/http" | "chi" | "gin") => Some(("t0.adapter-go", GroupBy::GoPackage)),
        ("rust", "axum" | "actix-web") => Some(("t0.adapter-rust", GroupBy::RustModule)),
        _ => None,
    }
}

/// The group's identity tag, its name and its title, from what the adapter
/// recorded on the endpoint and its handler in `path`.
fn group_name(
    by: GroupBy,
    endpoint: &serde_json::Value,
    handler: &serde_json::Value,
    path: &str,
) -> Option<(&'static str, String, String)> {
    let part = |name: &str| !name.is_empty() && !name.chars().any(char::is_whitespace);
    match by {
        GroupBy::FlaskBlueprint => {
            // The adapter records `mount` only on a route a Blueprint serves:
            // an application route may spell a dotted endpoint name itself.
            endpoint["mount"].as_str()?;
            let (blueprint, own) = endpoint["endpoint_name"].as_str()?.rsplit_once('.')?;
            // Flask allows any non-empty name, spaces included.
            (!own.is_empty() && blueprint.split('.').all(|name| !name.is_empty())).then(|| {
                (
                    "flask-blueprint",
                    blueprint.to_owned(),
                    format!("HTTP operations in blueprint {blueprint}"),
                )
            })
        }
        GroupBy::GoPackage => {
            // A method is never a resolved handler; its name would carry the
            // receiver type.
            let dir = handler["package_dir"].as_str()?;
            let file_dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            (handler["name"] == handler["simple_name"] && dir == file_dir).then(|| {
                let shown = if dir.is_empty() { "." } else { dir };
                (
                    "go-package",
                    dir.to_owned(),
                    format!("HTTP operations in package {shown}"),
                )
            })
        }
        GroupBy::RustModule => {
            // The adapter's module path starts with the crate's name.
            let (crate_name, module) = (
                handler["crate_name"].as_str()?,
                handler["module_path"].as_str()?,
            );
            if handler.get("fn_local").is_some()
                || handler.get("type_name").is_some()
                || handler["name"] != handler["simple_name"]
                || !module.split("::").all(part)
                || module.split("::").next() != Some(crate_name)
            {
                return None;
            }
            let name = module.to_owned();
            let title = format!("HTTP operations in module {name}");
            Some(("rust-module", name, title))
        }
    }
}

/// Add Capability candidates for route groups that have a name but no class,
/// under the controller rule: one per group and handler file, citing every
/// import-proven endpoint in it, its handler function and the relation
/// between them.
///
/// - Flask: the group is the dotted Blueprint name the adapter recorded
///   before the route's own endpoint name. A route registered on the
///   application itself, whatever its endpoint name, or under a Blueprint
///   whose name is not a literal, belongs to no group.
/// - Go (`net/http`, chi, gin): the group is the package directory of the
///   handler function. These frameworks give a group of routes a path prefix
///   and no name, so the package is the nearest named unit the adapter proves.
/// - Rust (axum, actix-web): the group is the crate and module of
///   the handler function, for the same reason. A method on a type and a
///   function nested in another are left out.
///
/// The adapters emit no fact for the Blueprint, package or module itself, so
/// the handler's file is part of the identity: two groups that share a name
/// are never merged across files, and one group whose handlers sit in two
/// files gives a candidate for each. An endpoint's support may cite its
/// registration in other files of the same repository and commit.
pub(super) fn derive(
    nodes: &BTreeMap<&str, Option<&Node>>,
    handles: &BTreeMap<&str, Vec<&core_graph::Edge>>,
    report: &mut CapabilityReport,
) {
    type Key = (&'static str, String, String, String);
    let mut groups: BTreeMap<Key, (String, Group)> = BTreeMap::new();
    for endpoint in nodes.values().filter_map(|n| *n) {
        let props = &endpoint.props;
        if endpoint.label != "Endpoint" {
            continue;
        }
        let Some((extractor, by)) = group_shape(props) else {
            continue;
        };
        let (Some(method), Some(path)) = (props["method"].as_str(), props["path"].as_str()) else {
            continue;
        };
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
            || handler.props["language"] != props["language"]
            || props["handler_sym"] != handler.id.as_str()
        {
            continue;
        }
        let (Some(endpoint_prov), Some(edge_prov), Some(handler_prov)) = (
            support_from(props, extractor),
            support_from(&edge.props, extractor),
            support_from(&handler.props, extractor),
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
        let Some((tag, group_name, title)) = group_name(by, props, &handler.props, &owner.path)
        else {
            continue;
        };
        let (_, group) = groups
            .entry((tag, owner.repo.clone(), owner.path.clone(), group_name))
            .or_insert_with(|| (title, Group::default()));
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
    for ((tag, repo, path, name), (title, group)) in groups {
        let key = serde_json::to_vec(&(
            CAPABILITY_CONTROLLER_RULE,
            CAPABILITY_CONTROLLER_RULE_VERSION,
            tag,
            &repo,
            &path,
            &name,
        ))
        .expect("string tuple serialization");
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
