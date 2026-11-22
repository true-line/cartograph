//! Capability candidates from controller classes and their HTTP routes.
use super::*;

mod route_groups;

/// The base-fact shape one adapter gives a controller class and its handlers.
struct ControllerShape {
    /// The adapter whose facts this shape reads; no other extractor counts.
    extractor: &'static str,
    /// The `kind` the adapter gives a handler that is a member of a class.
    handler_kind: &'static str,
    /// The adapter emits a Symbol for the class itself; the rule then requires
    /// and cites it. Without one, the handler's own `class` property names it.
    class_symbol: bool,
}

/// The shape for an HTTP endpoint's framework and language, if the rule
/// covers it. Anything else is another rule's work or nobody's yet.
fn controller_shape(endpoint: &serde_json::Value) -> Option<ControllerShape> {
    match (
        endpoint["framework"].as_str()?,
        endpoint["language"].as_str(),
    ) {
        ("nestjs", _) => Some(ControllerShape {
            extractor: "t0.adapter-ts",
            handler_kind: "Method",
            class_symbol: false,
        }),
        ("spring" | "jax-rs", Some("java")) => Some(ControllerShape {
            extractor: "t0.adapter-java",
            handler_kind: "method",
            class_symbol: true,
        }),
        ("spring", Some("kotlin")) => Some(ControllerShape {
            extractor: "t0.adapter-kotlin",
            handler_kind: "function",
            class_symbol: true,
        }),
        _ => None,
    }
}

/// The class a handler Symbol is a member of, from its qualified name: the
/// part before the member, with a Java parameter list set aside first. A
/// Kotlin extension function has a receiver, not an owning class.
fn handler_class<'a>(handler: &'a Node, shape: &ControllerShape) -> Option<&'a str> {
    let name = handler.props["name"].as_str()?;
    let qualified = match shape.extractor {
        "t0.adapter-java" => name.split_once('(')?.0,
        _ if name.contains('(') => return None,
        _ => name,
    };
    let (class, member) = qualified.rsplit_once('.')?;
    if class.is_empty()
        || member.is_empty()
        || class.contains('@') // an adapter-made name for an anonymous class
        || handler.props.get("receiver").is_some()
    {
        return None;
    }
    // Without a class Symbol to check against, the handler names its class.
    (shape.class_symbol || handler.props["class"].as_str() == Some(class)).then_some(class)
}

/// Derive Capability candidates from import-proven controller routes: one per
/// controller class, citing each of its HTTP endpoints, the method that
/// handles it and the relation between them, and the class's own Symbol where
/// the adapter emits one. Covered are NestJS controllers (TypeScript), Spring
/// MVC controllers (Java and Kotlin) and JAX-RS resource classes (Java). The
/// rule reads only what the
/// adapter proved: an endpoint with exactly one handler, a member of a named
/// class, every fact from one file and from that language's own adapter. A
/// route two classes declare, a handler that is not a class member, an
/// interface or object in place of a class and support that is not
/// source-backed are omitted, never guessed. The title names the class and
/// makes no claim about what the operations do or who may call them. Route
/// groups with a name and no class (Flask Blueprints, Go packages, Rust
/// modules) give candidates of their own (see `route_groups`). The
/// borrowed base is unchanged.
pub fn derive_controller_capabilities(base: &RevisionedSnapshot) -> CapabilityReport {
    // Every derivation under test reads a compacted base, as the knowledge
    // phase reads it (#2215).
    #[cfg(test)]
    let compacted = crate::workbench::compacted_snapshot_for_tests(base);
    #[cfg(test)]
    let base = &compacted;
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
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for endpoint in nodes.values().filter_map(|n| *n) {
        let props = &endpoint.props;
        if endpoint.label != "Endpoint" {
            continue;
        }
        let Some(shape) = controller_shape(props) else {
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
        if handler.label != "Symbol" || handler.props["kind"] != shape.handler_kind {
            continue;
        }
        let (Some(class), Some(name)) = (
            handler_class(handler, &shape),
            handler.props["name"].as_str(),
        ) else {
            continue;
        };
        let (Some(endpoint_prov), Some(edge_prov), Some(handler_prov)) = (
            support_from(props, shape.extractor),
            support_from(&edge.props, shape.extractor),
            support_from(&handler.props, shape.extractor),
        ) else {
            continue;
        };
        let owner = &handler_prov.evidence[0];
        if !same_owner(&handler_prov, owner)
            || !same_owner(&endpoint_prov, owner)
            || !same_owner(&edge_prov, owner)
            || handler.id != format!("sym:{}@{}#{name}", owner.repo, owner.path)
            || endpoint.id != format!("ep:{}@{method}:{path}", owner.repo)
        {
            continue;
        }
        let controller = format!("sym:{}@{}#{class}", owner.repo, owner.path);
        let mut supporting = vec![endpoint_prov, edge_prov, handler_prov.clone()];
        let mut cites = vec![
            BaseRef::Node(endpoint.id.clone()),
            BaseRef::Node(handler.id.clone()),
            BaseRef::Edge(edge.src.clone(), edge.dst.clone(), edge.label.clone()),
        ];
        if shape.class_symbol {
            // The class is a fact of its own: a plain class the same adapter
            // declared in the handler's file, under the name the handler has.
            let Some(Some(declared)) = nodes.get(controller.as_str()) else {
                continue;
            };
            let Some(class_prov) = support_from(&declared.props, shape.extractor) else {
                continue;
            };
            if declared.label != "Symbol"
                || declared.props["kind"] != "class"
                || declared.props["name"] != class
                || declared.props["language"] != props["language"]
                || handler.props["language"] != props["language"]
                || props["handler_sym"] != handler.id.as_str()
                || !same_owner(&class_prov, owner)
            {
                continue;
            }
            cites.push(BaseRef::Node(declared.id.clone()));
            supporting.push(class_prov);
        }
        let group = groups.entry(controller).or_insert_with(|| Group {
            class: class.to_owned(),
            ..Group::default()
        });
        group.mutates |= naming::mutating(method);
        group.cites.extend(cites);
        for prov in &supporting {
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
    let mut pending = Vec::new();
    for (controller, group) in groups {
        let key = serde_json::to_vec(&(
            CAPABILITY_CONTROLLER_RULE,
            CAPABILITY_CONTROLLER_RULE_VERSION,
            &controller,
        ))
        .expect("string tuple serialization");
        let title = naming::title(&naming::subject(&group.class), None, group.mutates);
        let (path, class) = controller
            .split_once('@')
            .and_then(|(_, rest)| rest.rsplit_once('#'))
            .map_or((String::new(), String::new()), |(path, class)| {
                (path.to_owned(), class.to_owned())
            });
        pending.push(Pending {
            title,
            places: [path, class],
            key,
            group,
        });
    }
    route_groups::derive(&nodes, &handles, &base.graph.1, &mut pending);
    let mut report = CapabilityReport::default();
    for (title, key, group) in disambiguated(pending) {
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
        retain_sized_fact(&mut report, fact);
    }
    report
}

/// A controller or route group named but not yet titled apart from the
/// others.
struct Pending {
    title: String,
    /// What tells it apart when titles collide: its file, then its class,
    /// Blueprint, package or module.
    places: [String; 2],
    key: Vec<u8>,
    group: Group,
}

/// The URL route prefix every cited HTTP endpoint of `group` shares, up to
/// its first parameter segment (`/admin/orders`), or none when the group
/// cites no HTTP route or its routes share no literal segment. Endpoint ids
/// read `ep:<repo>@<method>:<path>`.
fn route_prefix(group: &Group) -> Option<String> {
    let mut common: Option<Vec<&str>> = None;
    for cite in &group.cites {
        let BaseRef::Node(id) = cite else { continue };
        let Some(route) = id
            .strip_prefix("ep:")
            .and_then(|rest| rest.split_once('@'))
            .and_then(|(_, rest)| rest.split_once(':'))
            .map(|(_, path)| path)
        else {
            continue;
        };
        if !route.starts_with('/') {
            return None;
        }
        let segments: Vec<&str> = route
            .split('/')
            .filter(|s| !s.is_empty())
            .take_while(|s| !s.contains(['<', '{', ':', '*']))
            .collect();
        common = Some(match common {
            None => segments,
            Some(prior) => prior
                .into_iter()
                .zip(segments)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    common
        .filter(|segments| !segments.is_empty())
        .map(|segments| format!("/{}", segments.join("/")))
}

/// Titles that would collide across every controller and route group name
/// the URL route prefix their routes share after the title when that tells
/// each of them apart (`View orders (/admin/orders)`): a route is the
/// observable contract, a file path is not. Otherwise they name their file
/// in words, and their class or group too when the file does not tell them
/// apart (AC-0851, #1999).
fn disambiguated(pending: Vec<Pending>) -> Vec<(String, Vec<u8>, Group)> {
    let mut by_title: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_file: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let prefixes: Vec<Option<String>> = pending.iter().map(|p| route_prefix(&p.group)).collect();
    // Per colliding title, how many candidates name each route prefix, and
    // whether every one has a prefix.
    let mut by_prefix: BTreeMap<(&str, Option<&str>), usize> = BTreeMap::new();
    for (p, prefix) in pending.iter().zip(&prefixes) {
        *by_title.entry(&p.title).or_default() += 1;
        *by_file.entry((&p.title, &p.places[0])).or_default() += 1;
        *by_prefix.entry((&p.title, prefix.as_deref())).or_default() += 1;
    }
    // Titles whose candidates each have a route prefix no other shares.
    let mut routes_fail: BTreeSet<&str> = BTreeSet::new();
    for ((title, prefix), n) in &by_prefix {
        if prefix.is_none() || *n > 1 {
            routes_fail.insert(title);
        }
    }
    let routes_tell_apart = |title: &str| !routes_fail.contains(title);
    // A file is named in words, never by its path (#1999).
    let mut files_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for p in &pending {
        files_of.entry(&p.title).or_default().push(&p.places[0]);
    }
    let places: BTreeMap<&str, BTreeMap<&str, String>> = files_of
        .into_iter()
        .map(|(title, files)| (title, naming::places_apart(files)))
        .collect();
    let titles: Vec<String> = pending
        .iter()
        .zip(&prefixes)
        .map(|(p, prefix)| {
            if by_title[p.title.as_str()] < 2 {
                p.title.clone()
            } else if let Some(prefix) = prefix.as_deref().filter(|_| routes_tell_apart(&p.title)) {
                format!("{} ({prefix})", p.title)
            } else if by_file[&(p.title.as_str(), p.places[0].as_str())] < 2 {
                format!(
                    "{} ({})",
                    p.title,
                    places[p.title.as_str()][p.places[0].as_str()]
                )
            } else {
                format!(
                    "{} ({}, {})",
                    p.title,
                    places[p.title.as_str()][p.places[0].as_str()],
                    naming::spelled_words(&p.places[1])
                )
            }
        })
        .collect();
    pending
        .into_iter()
        .zip(titles)
        .map(|(p, title)| (title, p.key, p.group))
        .collect()
}

#[cfg(test)]
mod tests;
