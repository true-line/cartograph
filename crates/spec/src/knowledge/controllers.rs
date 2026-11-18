//! Capability candidates from controller classes and their HTTP routes.
use super::*;

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
        ("spring", Some("java")) => Some(ControllerShape {
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
/// the adapter emits one. Covered are NestJS controllers (TypeScript) and
/// Spring MVC controllers (Java and Kotlin). The rule reads only what the
/// adapter proved: an endpoint with exactly one handler, a member of a named
/// class, every fact from one file and from that language's own adapter. A
/// route two classes declare, a handler that is not a class member, an
/// interface or object in place of a class and support that is not
/// source-backed are omitted, never guessed. The title names the class and
/// makes no claim about what the operations do or who may call them. The
/// borrowed base is unchanged.
pub fn derive_controller_capabilities(base: &RevisionedSnapshot) -> CapabilityReport {
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
    let mut report = CapabilityReport::default();
    for (controller, group) in groups {
        let key = serde_json::to_vec(&(
            CAPABILITY_CONTROLLER_RULE,
            CAPABILITY_CONTROLLER_RULE_VERSION,
            &controller,
        ))
        .expect("string tuple serialization");
        let title = format!("HTTP operations in {}", group.class);
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

#[cfg(test)]
mod tests;
