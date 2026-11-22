//! Shared native artifact renderers with per-occurrence output admission.
use super::render::{Assertions, Budget, Join, Json, NodeName, RenderLimit, Replace, Safe, Text};
use super::*;
use core_graph::FactKey;
use std::fmt::{self, Write};

pub(super) struct Output {
    pub content: String,
    pub assertions: Vec<SpecAssertion>,
    /// The fact-key shape of each assertion's subject (ADR-0058).
    pub subjects: Vec<fact_index::Subject>,
}
fn finish(mut content: Text, assertions: Assertions) -> Result<Output, RenderLimit> {
    render::inline(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
    })
}
fn nongap(node: &&Node) -> bool {
    provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap
}
fn edge_nongap(edge: &&Edge) -> bool {
    provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
}
fn strings<'a>(node: &'a Node, key: &str) -> Vec<&'a str> {
    node.props[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

/// A relation field's metadata as text: a string as it is, a boolean (the
/// owning side) as its JSON spelling, an absent value as nothing.
fn relation_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `createProduct` -> `create product`; `products` -> `products`.
fn words(identifier: &str) -> String {
    let mut out = String::new();
    let mut previous_lower = false;
    for c in identifier.chars() {
        if c == '_' || c == '-' {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower {
            out.push(' ');
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        out.extend(c.to_lowercase());
    }
    out.trim().to_owned()
}

fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    }
}

/// What a caller of `endpoint` does, in words: a GraphQL field, or the
/// handler method that serves an HTTP route.
fn operation_phrase(endpoint: &Node, handler: Option<&Node>) -> Option<String> {
    if let Some(field) = endpoint.props["field"].as_str() {
        let field = words(field);
        return Some(match endpoint.props["operation"].as_str() {
            Some("query") => format!("view {field}"),
            _ => field,
        });
    }
    let name = handler?.props["name"].as_str()?;
    let member = name.rsplit(['.', '#']).next().unwrap_or(name);
    let mut member = words(member);
    // A handler named for its subject (`coupon_route`, `lifetime_licence`)
    // reads with its method's verb (#1979): the trailing framework word
    // goes, and an action word that starts the name stays the verb.
    for tail in [" route", " view", " handler", " endpoint", " page"] {
        if let Some(head) = member.strip_suffix(tail)
            && !head.is_empty()
        {
            member = head.to_owned();
            break;
        }
    }
    if member.is_empty() {
        return None;
    }
    let first = member.split(' ').next().unwrap_or_default();
    if HANDLER_ACTIONS.contains(&first) {
        return Some(member);
    }
    let verb = match endpoint.props["method"]
        .as_str()
        .map(str::to_ascii_uppercase)
    {
        Some(m) if m == "GET" || m == "HEAD" => "view",
        Some(m) if m == "POST" => "submit",
        Some(m) if m == "PUT" || m == "PATCH" => "update",
        Some(m) if m == "DELETE" => "delete",
        _ => return Some(member),
    };
    Some(format!("{verb} {member}"))
}

/// Words a handler name starts with when it already says what the caller
/// does (`delete_alias`, `reset_password`, `get_settings`).
const HANDLER_ACTIONS: &[&str] = &[
    "accept",
    "activate",
    "add",
    "apply",
    "approve",
    "authorize",
    "block",
    "buy",
    "cancel",
    "change",
    "check",
    "claim",
    "clone",
    "close",
    "confirm",
    "connect",
    "create",
    "delete",
    "disable",
    "disconnect",
    "download",
    "edit",
    "enable",
    "enter",
    "exit",
    "export",
    "fetch",
    "forgot",
    "generate",
    "get",
    "handle",
    "import",
    "invite",
    "join",
    "link",
    "list",
    "login",
    "logout",
    "manage",
    "mark",
    "open",
    "pay",
    "process",
    "publish",
    "redeem",
    "refresh",
    "register",
    "reject",
    "remove",
    "rename",
    "request",
    "resend",
    "reset",
    "restore",
    "retry",
    "revoke",
    "save",
    "search",
    "send",
    "set",
    "setup",
    "show",
    "sign",
    "start",
    "stop",
    "submit",
    "subscribe",
    "sync",
    "toggle",
    "transfer",
    "unblock",
    "unlink",
    "unsubscribe",
    "update",
    "upload",
    "validate",
    "verify",
    "view",
];

/// "As <caller>, I can <operation> (<METHOD path>)." for each Endpoint a
/// derived Capability cites. The caller is the Actor caller set derived for
/// that endpoint when one is included, else the API it belongs to. Nothing
/// here is a new fact: it restates the cited facts and included Actors.
fn readable_stories(
    story: &capabilities::Story<'_>,
    included: &[capabilities::Story<'_>],
    (mode, rejected): (ExportMode, &BTreeSet<String>),
) -> Vec<String> {
    use core_graph::overlay::BaseRef;
    let mut endpoints = Vec::new();
    let mut handlers = BTreeMap::new();
    for (cite, fact) in &story.support {
        if let (BaseRef::Node(_), capabilities::Support::Node(node)) = (cite, fact) {
            if node.label == "Endpoint" {
                endpoints.push(*node);
            } else if node.label == "Symbol" {
                handlers.insert(node.id.as_str(), *node);
            }
        }
    }
    let mut handled_by = BTreeMap::new();
    for (cite, _) in &story.support {
        if let BaseRef::Edge(src, dst, label) = cite
            && label == "HANDLES"
            && let Some(handler) = handlers.get(dst.as_str())
        {
            handled_by.insert(src.as_str(), *handler);
        }
    }
    let callers: BTreeMap<&str, Vec<&str>> = included
        .iter()
        .filter(|s| s.node.label == "Actor")
        .filter_map(|s| {
            Some((
                s.node.props["endpoint_id"].as_str()?,
                strings(s.node, "any_of"),
            ))
        })
        .collect();
    endpoints
        .into_iter()
        .filter_map(|endpoint| {
            let phrase = operation_phrase(endpoint, handled_by.get(endpoint.id.as_str()).copied())?;
            let caller = match (
                callers.get(endpoint.id.as_str()),
                endpoint.props["api"].as_str(),
            ) {
                (Some(any_of), api) if !any_of.is_empty() => format!(
                    "{} caller holding {}",
                    api.map_or_else(
                        || "a".to_owned(),
                        |api| format!("{} {api} API", article(api))
                    ),
                    any_of.join(" or ")
                ),
                (_, Some(api)) => format!("{} {api} API caller", article(api)),
                // A Python endpoint's caller as its guard reading states (#1979).
                (_, None) => super::endpoint_permissions::caller(endpoint, mode, rejected)
                    .map_or_else(|| "an API caller".to_owned(), |c| c.subject()),
            };
            Some(format!(
                "- As {}, I can {} ({}).",
                render::Literal(caller),
                render::Literal(phrase),
                render::Literal(NodeName(endpoint))
            ))
        })
        .collect()
}

/// The source file of each handler a derived Capability cites, with the
/// handler members in that file, in path order: only the Symbols a cited
/// `HANDLES` edge ends at, never a cited class or other support. Read from
/// each handler's own evidence; nothing new is inferred.
fn handler_files<'a>(story: &capabilities::Story<'a>) -> BTreeMap<String, Vec<String>> {
    let handlers: BTreeSet<&str> = story
        .support
        .iter()
        .filter_map(|(_, fact)| match fact {
            capabilities::Support::Edge(edge) if edge.label == "HANDLES" => Some(edge.dst.as_str()),
            _ => None,
        })
        .collect();
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, fact) in &story.support {
        if let capabilities::Support::Node(node) = fact
            && node.label == "Symbol"
            && handlers.contains(node.id.as_str())
            && let Some(evidence) = provenance(&node.props, &node.id).evidence.first()
        {
            let member = node.props["name"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| node.id.rsplit('#').next().map(str::to_owned))
                .unwrap_or_default();
            let members = files.entry(evidence.path.clone()).or_default();
            if !member.is_empty() && !members.contains(&member) {
                members.push(member);
            }
        }
    }
    files
}

/// The Actor caller sets, as one table: who may call each operation, by the
/// permissions its guard lists. Assumed from the guard's convention, never
/// effective runtime authorization (Semantic / InferredWeak).
fn caller_sets(
    content: &mut Text,
    included: &[capabilities::Story<'_>],
) -> Result<(), RenderLimit> {
    let mut endpoints: BTreeMap<&str, &Node> = BTreeMap::new();
    for story in included {
        for (_, fact) in &story.support {
            if let capabilities::Support::Node(node) = fact
                && node.label == "Endpoint"
            {
                endpoints.insert(node.id.as_str(), node);
            }
        }
    }
    let mut rows: Vec<(Vec<&str>, String, String)> = included
        .iter()
        .filter(|s| s.node.label == "Actor")
        .map(|s| {
            let endpoint = s.node.props["endpoint_id"].as_str().unwrap_or_default();
            let operation = endpoints
                .get(endpoint)
                .map_or_else(|| endpoint.to_owned(), |n| NodeName(n).to_string());
            let file = s.node.props["permission_site"]["prov"]["evidence"][0]["path"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            (strings(s.node, "any_of"), operation, file)
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort();
    rows.dedup();
    content.push_str("## Caller permission sets\n\n")?;
    content.push_str(
        "Who may call each operation, by the permissions its guard lists: a caller holding any \
         one of them is admitted. Assumed caller sets (Semantic / InferredWeak), read from the \
         guard's convention; not effective runtime authorization or an identified user, role \
         or principal.\n\n",
    )?;
    content.push_str("| Any of | Operation | Guard in |\n|---|---|---|\n")?;
    for (any_of, operation, file) in &rows {
        writeln!(
            content,
            "| {} | {} | {} |",
            render::Literal(any_of.join(", ")),
            render::Literal(operation),
            render::Literal(file)
        )?;
    }
    content.push_str("\n")?;
    Ok(())
}

pub(super) fn stories(nodes: &[&Node], max: Option<usize>) -> Result<Output, RenderLimit> {
    stories_with_support(nodes, None, (ExportMode::BestEffort, &BTreeSet::new()), max)
}
pub(super) fn stories_with_support(
    nodes: &[&Node],
    stories: Option<&capabilities::Stories<'_>>,
    policy: (ExportMode, &BTreeSet<String>),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    let mut capabilities: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Capability")
        .filter(nongap)
        .collect();
    let base_count = capabilities.len();
    if let Some(stories) = stories {
        capabilities.extend(stories.included.iter().map(|s| s.node));
    }
    for node in &capabilities {
        assertions.node(node, None)?;
    }
    for story in stories.into_iter().flat_map(|s| &s.included) {
        for (cite, fact) in &story.support {
            assertions.support(&capabilities::support_id(&story.node.id, cite), fact)?;
        }
    }
    let mut content = Text::new(&budget, "# Recovered user stories\n\n")?;
    if capabilities.is_empty() && !stories.is_some_and(|s| s.filtered > 0) {
        content.push_str("No Capability facts have been recovered yet.\n")?;
    }
    // Derived stories read as prose: the readable stories and the source
    // files that hold each operation (#1443). Each story's exact identity
    // and support stay JSON text, in the appendix below, never in the body.
    let included = stories.map_or(&[][..], |s| &s.included[..]);
    for (index, node) in capabilities.iter().enumerate() {
        match index.checked_sub(base_count).and_then(|i| included.get(i)) {
            // Actors render as one table after the stories.
            Some(_) if node.label == "Actor" => {}
            Some(story) => {
                // Legacy headings are unchanged.
                writeln!(
                    content,
                    "## US-R-{:04} — Derived Capability: {}\n",
                    index + 1,
                    render::Literal(NodeName(node))
                )?;
                // Every recovered name is literal text.
                for line in readable_stories(story, included, policy) {
                    writeln!(content, "{line}")?;
                }
                let files = handler_files(story);
                if !files.is_empty() {
                    content.push_str("\nImplemented in:\n\n")?;
                    for (file, members) in &files {
                        writeln!(
                            content,
                            "- {} ({})",
                            render::Literal(file),
                            render::Literal(members.join(", "))
                        )?;
                    }
                }
                writeln!(
                    content,
                    "\nIdentity and support: `US-R-{:04}` under Story identities and support.\n",
                    index + 1
                )?;
            }
            // Recovered names and ids are literal text, never Markdown/HTML (#1453).
            None => writeln!(
                content,
                "## US-R-{:04} — {}\n\n- Recovered capability assertion: {}\n",
                index + 1,
                render::Literal(NodeName(node)),
                render::Literal(&node.id)
            )?,
        }
    }
    caller_sets(&mut content, included)?;
    if !included.is_empty() {
        content.push_str("## Story identities and support\n\n")?;
        content.push_str(
            "The exact identity of each derived story and every fact it cites, as JSON text.\n\n",
        )?;
    }
    for (index, node) in capabilities.iter().enumerate() {
        let Some(story) = index.checked_sub(base_count).and_then(|i| included.get(i)) else {
            continue;
        };
        if node.label == "Actor" {
            writeln!(
                content,
                "### Actor caller set — Semantic / InferredWeak convention\n\n    {}\n",
                render::Serialized(&(
                    &node.id,
                    &node.props["any_of"],
                    &node.props["assumption"],
                    &node.props["permission_site"]["prov"]["evidence"]
                ))
            )?;
        } else {
            writeln!(
                content,
                "### US-R-{:04}\n\n    {}\n",
                index + 1,
                render::Serialized(&(&node.id, render::DisplayValue(&NodeName(node))))
            )?;
        }
        for (cite, _) in &story.support {
            writeln!(
                content,
                "    {}",
                render::Serialized(&(capabilities::support_id(&node.id, cite), cite))
            )?;
        }
        content.push_str("\n")?;
    }
    if let Some(stories) = stories.filter(|s| s.filtered > 0) {
        writeln!(
            content,
            "{} derived stories omitted because candidate facts or their supporting facts are filtered.\n",
            stories.filtered
        )?;
    }
    if stories.is_some_and(|s| !s.included.is_empty()) {
        // Base output retains its existing formatting. Sidecar prose and
        // provenance are literal JSON, including strings containing HTML,
        // Markdown fences or headings. The same Text/Budget admits it all.
        if base_count > 0 {
            render::inline_literal(&mut content, &assertions.values[..base_count])?;
        }
        content.push_str("\n## Derived assertions and provenance\n\n")?;
        for assertion in &assertions.values[base_count..] {
            writeln!(content, "    {}", render::Serialized(assertion))?;
        }
        Ok(Output {
            content: content.finish(),
            assertions: assertions.values,
            subjects: assertions.subjects,
        })
    } else {
        render::inline_literal(&mut content, &assertions.values)?;
        Ok(Output {
            content: content.finish(),
            assertions: assertions.values,
            subjects: assertions.subjects,
        })
    }
}
pub(super) fn flow(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let origins = render::Origins::new(nodes, edges);
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    for flow in flows {
        for (index, hop) in flow.hops.iter().enumerate() {
            assertions.hop(flow, hop, index, &origins)?;
        }
    }
    let tests = diagrams::TestReferences::new(nodes, edges, |e| edge_nongap(&e));
    if let Some(tests) = &tests {
        // Each rendered reference cites every edge proving it, once.
        let mut cited = BTreeSet::new();
        for flow in flows {
            // A zero-hop flow's trigger is its only participant.
            let ids = std::iter::once(flow.trigger.as_str()).chain(
                flow.hops
                    .iter()
                    .flat_map(|h| [h.src.as_str(), h.dst.as_str()]),
            );
            for id in ids {
                for (_, proof) in tests.by_participant.get(id).into_iter().flatten() {
                    for edge in proof {
                        if cited.insert(edge_identity(edge)) {
                            assertions.edge(edge)?;
                        }
                    }
                }
            }
        }
    }
    let mut content = Text::new(&budget, "")?;
    diagrams::flows_with_tests(&mut content, flows, tests.as_ref())?;
    finish(content, assertions)
}
pub(super) fn topology(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut selected_edges: Vec<Edge> = edges
        .iter()
        .copied()
        .filter(|e| TOPOLOGY_EDGE_LABELS.contains(&e.label.as_str()))
        .filter(edge_nongap)
        .cloned()
        .collect();
    let backed: BTreeSet<_> = selected_edges
        .iter()
        .filter(|e| e.label == "BACKS")
        .map(|e| e.dst.as_str())
        .collect();
    let configured: BTreeSet<_> = selected_edges
        .iter()
        .filter(|e| e.label == "CONFIGURED_IN")
        .map(|e| e.dst.as_str())
        .collect();
    let selected_nodes: Vec<Node> = nodes
        .iter()
        .copied()
        .filter(|n| {
            n.label == "Resource"
                || (n.label == "Channel" && backed.contains(n.id.as_str()))
                || (n.label == "File" && configured.contains(n.id.as_str()))
        })
        .filter(nongap)
        .cloned()
        .collect();
    let ids: BTreeSet<_> = selected_nodes.iter().map(|n| n.id.as_str()).collect();
    selected_edges.retain(|e| ids.contains(e.src.as_str()) && ids.contains(e.dst.as_str()));
    let mut assertions = Assertions::new(&budget);
    for n in &selected_nodes {
        assertions.node(n, None)?;
    }
    for e in &selected_edges {
        assertions.edge(e)?;
    }
    let mut content = Text::new(&budget, "# Resource topology\n\n```mermaid\n")?;
    diagrams::topology(&mut content, &selected_nodes, &selected_edges)?;
    content.push_str("```\n")?;
    finish(content, assertions)
}
struct ModelName<'a>(&'a Node);
impl fmt::Display for ModelName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.label != "DataEntity" {
            write!(f, "{}: ", self.0.label)?;
        }
        write!(f, "{}", NodeName(self.0))
    }
}
/// Producer-owned incident labels used by both rendering and partition closure.
pub const DATA_PARTITION_EDGE_LABELS: &[&str] = DATA_EDGE_LABELS;

/// The citation id of a node fact (ADR-0058).
fn node_citation(node: &Node) -> String {
    citation_id(&FactKey::Node {
        id: node.id.clone(),
    })
}
/// The citation id of an edge fact (ADR-0058).
fn edge_citation(edge: &Edge) -> String {
    citation_id(&FactKey::Edge {
        source: edge.src.clone(),
        label: edge.label.clone(),
        destination: edge.dst.clone(),
    })
}
/// An edge's confidence, decoded as its assertion decodes it.
fn edge_confidence(edge: &Edge) -> ConfidenceTier {
    provenance(&edge.props, &edge_identity(edge)).confidence_tier
}
const DATA_CITATIONS: &str = "\nEvery statement below prints its citation id. `fact-index.md` \
lists the tier, confidence, method and evidence spans of each id. The diagram draws the same \
facts and carries no ids.\n";

pub(super) fn data(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let by_id: BTreeMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
    let entities: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "DataEntity")
        .filter(nongap)
        .collect();
    let entity_ids: BTreeSet<_> = entities.iter().map(|n| n.id.as_str()).collect();
    let mappings: Vec<_> = edges
        .iter()
        .copied()
        .filter(|e| DATA_PARTITION_EDGE_LABELS.contains(&e.label.as_str()))
        .filter(edge_nongap)
        .filter(|e| entity_ids.contains(e.src.as_str()) || entity_ids.contains(e.dst.as_str()))
        .filter(|e| by_id.contains_key(e.src.as_str()) && by_id.contains_key(e.dst.as_str()))
        .collect();
    let ids: BTreeSet<_> = entity_ids
        .iter()
        .copied()
        .chain(
            mappings
                .iter()
                .flat_map(|e| [e.src.as_str(), e.dst.as_str()]),
        )
        .collect();
    let model_nodes: Vec<_> = ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
    let aliases: BTreeMap<_, _> = model_nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let mut assertions = Assertions::new(&budget);
    for n in &model_nodes {
        assertions.node(n, None)?;
    }
    for e in &mappings {
        assertions.edge(e)?;
    }
    let mut content = Text::new(
        &budget,
        "# Recovered data model\n\n```mermaid\nflowchart LR\n",
    )?;
    // The diagram draws the entities and the relations between them; the
    // callables that read or write each one are listed under it (#1983).
    for n in model_nodes.iter().filter(|n| n.label == "DataEntity") {
        writeln!(
            content,
            "    d{}[\"{}\"]",
            aliases[n.id.as_str()],
            Replace(ModelName(n), &[('\r', " "), ('\n', " "), ('"', "'")])
        )?;
    }
    for e in mappings
        .iter()
        .filter(|e| entity_ids.contains(e.src.as_str()) && entity_ids.contains(e.dst.as_str()))
    {
        writeln!(
            content,
            "    d{} -->|{}| d{}",
            aliases[e.src.as_str()],
            e.label,
            aliases[e.dst.as_str()]
        )?;
    }
    content.push_str("```\n")?;
    if entities.is_empty() {
        content.push_str("\nNo DataEntity facts have been recovered yet.\n")?;
    }
    if !entities.is_empty() {
        entity_sections(&entities, &mappings, &by_id, &mut content)?;
    }
    if !model_nodes.is_empty() {
        content.push_str(DATA_CITATIONS)?;
        content.push_str(
            "\n## Entities and related nodes\n\n| Node | Kind | Tier | Confidence | Citation |\n|---|---|---|---|---|\n",
        )?;
        for n in &model_nodes {
            let prov = provenance(&n.props, &n.id);
            writeln!(
                content,
                "| `{}` | {} | {:?} | {:?} | {} |",
                Safe(&n.id),
                Safe(&n.label),
                prov.tier,
                prov.confidence_tier,
                node_citation(n)
            )?;
        }
    }
    // Each entity's own columns, as its adapter recorded them: `columns`
    // (SQLAlchemy, TypeORM) or `fields` (JPA). A column is a property of
    // its entity, so it cites that entity.
    let column_rows = |n: &Node| {
        n.props
            .get("columns")
            .or_else(|| n.props.get("fields"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len)
    };
    if entities.iter().any(|n| column_rows(n) > 0) {
        content.push_str(
            "\n## Entity columns\n\n| Entity | Column | Type | Key | Nullable | Stored as | Citation |\n|---|---|---|---|---|---|---|\n",
        )?;
        for n in entities.iter().filter(|n| column_rows(n) > 0) {
            let cite = node_citation(n);
            let columns = n
                .props
                .get("columns")
                .or_else(|| n.props.get("fields"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten();
            for column in columns {
                let key = column["primary_key"]
                    .as_bool()
                    .or_else(|| column["id"].as_bool())
                    .unwrap_or(false);
                writeln!(
                    content,
                    "| {} | `{}` | {} | {} | {} | {} | {cite} |",
                    Safe(NodeName(n)),
                    Safe(relation_value(&column["name"])),
                    Safe(relation_value(&column["type"])),
                    if key { "primary" } else { "" },
                    Safe(relation_value(&column["nullable"])),
                    Safe(relation_value(&column["column"])),
                )?;
            }
        }
    }
    if !mappings.is_empty() {
        content.push_str(
            "\n## Access and mapping relations\n\n| Source | Relation | Target | Confidence | Citation |\n|---|---|---|---|---|\n",
        )?;
        for e in &mappings {
            writeln!(
                content,
                "| `{}` | {} | `{}` | {:?} | {} |",
                Safe(&e.src),
                e.label,
                Safe(&e.dst),
                edge_confidence(e),
                edge_citation(e)
            )?;
        }
    }
    // One relation edge can carry several fields — several JPA relations to
    // the same target are one fact — so every field it declares is listed.
    let relation_fields: Vec<_> = mappings
        .iter()
        .filter_map(|e| {
            let relations = e.props.get("relations")?.as_array()?;
            Some((e, relations))
        })
        .collect();
    if !relation_fields.is_empty() {
        content.push_str(
            "\n## Relation fields\n\n| Source | Field | Relation | Cardinality | Owning | Target | Confidence | Citation |\n|---|---|---|---|---|---|---|---|\n",
        )?;
        for (e, relations) in relation_fields {
            // A field is a property of its relation edge, so it cites that edge.
            let (confidence, cite) = (edge_confidence(e), edge_citation(e));
            for relation in relations {
                writeln!(
                    content,
                    "| `{}` | `{}` | {} | {} | {} | `{}` | {confidence:?} | {cite} |",
                    Safe(&e.src),
                    Safe(relation_value(&relation["field"])),
                    Safe(relation_value(&relation["relation"])),
                    Safe(relation_value(&relation["cardinality"])),
                    Safe(relation_value(&relation["owning"])),
                    Safe(&e.dst)
                )?;
            }
        }
    }
    finish(content, assertions)
}
/// One section per entity (#1983): its table, its columns, the entities it
/// references and is referenced by, and the callables that write and read
/// it, by name and operation. The entity and its relations to other
/// entities cite their facts; each writer and reader relation is cited in
/// the relations table after it, which keeps every fact with its ids.
fn entity_sections(
    entities: &[&Node],
    mappings: &[&Edge],
    by_id: &BTreeMap<&str, &Node>,
    out: &mut Text,
) -> Result<(), RenderLimit> {
    /// Most callables listed per operation before "and N more".
    const LISTED: usize = 8;
    let name = |id: &str| -> String {
        match by_id.get(id) {
            Some(n) if n.label == "Symbol" => id
                .rsplit_once('#')
                .map_or_else(|| NodeName(n).to_string(), |(_, q)| q.to_owned()),
            Some(n) => NodeName(n).to_string(),
            None => id.to_owned(),
        }
    };
    let cites = |edges: &[&&Edge]| -> String {
        edges
            .iter()
            .map(|e| edge_citation(e))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ")
    };
    out.push_str("\n## Entities\n")?;
    for entity in entities {
        writeln!(out, "\n### {}\n", Safe(NodeName(entity)))?;
        let store = text_prop(entity, &["table", "store", "collection"]);
        writeln!(
            out,
            "{}Cites {}.\n",
            store.map_or_else(String::new, |t| format!("Stored in `{}`. ", Safe(t))),
            node_citation(entity)
        )?;
        let columns: Vec<&serde_json::Value> = entity
            .props
            .get("columns")
            .or_else(|| entity.props.get("fields"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        if !columns.is_empty() {
            out.push_str("| Column | Type | Key | Nullable |\n|---|---|---|---|\n")?;
            for column in columns {
                let key = column["primary_key"]
                    .as_bool()
                    .or_else(|| column["id"].as_bool())
                    .unwrap_or(false);
                writeln!(
                    out,
                    "| `{}` | {} | {} | {} |",
                    Safe(relation_value(&column["name"])),
                    Safe(relation_value(&column["type"])),
                    if key { "primary" } else { "" },
                    Safe(relation_value(&column["nullable"])),
                )?;
            }
            out.push_str("\n")?;
        }
        let id = entity.id.as_str();
        let related = |outgoing: bool| -> Vec<&&Edge> {
            mappings
                .iter()
                .filter(|e| e.label == "REFERENCES" || e.label == "MAPS_TO")
                .filter(|e| if outgoing { e.src == id } else { e.dst == id })
                // Entity to entity only: a repository's binding to its
                // entity is not a relation between entities.
                .filter(|e| {
                    let other = if outgoing { &e.dst } else { &e.src };
                    by_id
                        .get(other.as_str())
                        .is_some_and(|n| n.label == "DataEntity")
                })
                .collect()
        };
        for (title, outgoing) in [("References", true), ("Referenced by", false)] {
            let edges = related(outgoing);
            if edges.is_empty() {
                continue;
            }
            let others: BTreeSet<String> = edges
                .iter()
                .map(|e| name(if outgoing { &e.dst } else { &e.src }))
                .collect();
            writeln!(
                out,
                "- **{title}:** {} ({})",
                Safe(others.into_iter().collect::<Vec<_>>().join(", ")),
                cites(&edges)
            )?;
        }
        for (title, label) in [("Written by", "WRITES"), ("Read by", "READS")] {
            let edges: Vec<&&Edge> = mappings
                .iter()
                .filter(|e| e.label == label && e.dst == id)
                .collect();
            if edges.is_empty() {
                continue;
            }
            // Callables grouped by the operations their relation records.
            let mut by_operation: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for e in &edges {
                let operations = e.props["operations"]
                    .as_array()
                    .map(|ops| {
                        ops.iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .filter(|ops| !ops.is_empty())
                    .or_else(|| e.props["operation"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unspecified".to_owned());
                by_operation
                    .entry(operations)
                    .or_default()
                    .insert(format!("`{}`", name(&e.src)));
            }
            let groups: Vec<String> = by_operation
                .into_iter()
                .map(|(operation, callers)| {
                    let total = callers.len();
                    let mut shown: Vec<String> = callers.into_iter().take(LISTED).collect();
                    if total > LISTED {
                        shown.push(format!("and {} more", total - LISTED));
                    }
                    format!("{operation}: {}", shown.join(", "))
                })
                .collect();
            writeln!(
                out,
                "- **{title}** ({} callables): {}",
                edges.len(),
                Safe(groups.join("; "))
            )?;
        }
    }
    Ok(())
}
pub(super) fn adrs(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let adrs: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "ADR")
        .filter(nongap)
        .collect();
    let ids: BTreeSet<_> = adrs.iter().map(|n| n.id.as_str()).collect();
    let decisions: Vec<_> = edges
        .iter()
        .copied()
        .filter(|e| e.label == "DECIDES")
        .filter(edge_nongap)
        .filter(|e| ids.contains(e.src.as_str()))
        .collect();
    let mut assertions = Assertions::new(&budget);
    for n in &adrs {
        assertions.node(n, None)?;
    }
    for e in &decisions {
        assertions.edge(e)?;
    }
    let mut content = Text::new(&budget, "# Found and recovered ADRs\n\n")?;
    for n in &adrs {
        writeln!(content, "## {}\n", NodeName(n))?;
        if let Some(v) = n.props["origin"].as_str() {
            writeln!(content, "**Origin:** {v}\n")?;
        }
        if let Some(v) = n.props["status"].as_str() {
            writeln!(content, "**Status:** {v}\n")?;
        }
        if let Some(v) = text_prop(n, &["body", "content", "decision"]) {
            writeln!(content, "{v}\n")?;
        }
    }
    if adrs.is_empty() {
        content.push_str("No found or recovered ADR facts have been recovered yet.\n")?;
    }
    if !decisions.is_empty() {
        content.push_str("\n## Decision links\n\n| ADR | Relation | Subject |\n|---|---|---|\n")?;
        for e in &decisions {
            writeln!(
                content,
                "| `{}` | {} | `{}` |",
                Safe(&e.src),
                e.label,
                Safe(&e.dst)
            )?;
        }
    }
    finish(content, assertions)
}
pub(super) fn drift(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let findings: Vec<_> = nodes.iter().copied().filter(|n| is_drift_node(n)).collect();
    let mut assertions = Assertions::new(&budget);
    for n in &findings {
        assertions.node(n, None)?;
    }
    for e in edges.iter().copied().filter(|e| is_drift_edge(e)) {
        assertions.edge(e)?;
    }
    let adr_findings: Vec<_> = findings
        .iter()
        .copied()
        .filter(|n| {
            !matches!(
                n.props["kind"].as_str(),
                Some("contract-drift" | "schema-drift" | "test-drift" | "documentation-drift")
            )
        })
        .collect();
    let mut contract_findings: Vec<_> = findings
        .iter()
        .copied()
        .filter(|n| n.props["kind"].as_str() == Some("contract-drift"))
        .collect();
    let mut contract_gaps: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| is_gap_node(n) && n.props["kind"].as_str() == Some("contract-doc-gap"))
        .collect();
    let has_declared_docs = !contract_findings.is_empty()
        || !contract_gaps.is_empty()
        || nodes.iter().any(|n| n.label == "DocumentedRoute");

    let mut content = Text::new(
        &budget,
        "# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n",
    )?;
    for n in &adr_findings {
        let triggers = strings(n, "flow_triggers");
        let p = provenance(&n.props, &n.id);
        write!(
            content,
            "| {} | `{}` | `{}` | ",
            Safe(NodeName(n)),
            Safe(n.props["adr_id"].as_str().unwrap_or("—")),
            Safe(n.props["offending_edge"].as_str().unwrap_or("—"))
        )?;
        if triggers.iter().all(|s| s.is_empty()) && triggers.len() < 2 {
            write!(content, "—")?;
        } else {
            write!(content, "{}", Safe(Join(&triggers, ", ")))?;
        }
        writeln!(content, " | {:?} |", p.confidence_tier)?;
    }
    if adr_findings.is_empty() {
        content.push_str("| — | No ADR/code conflicts recovered |\n")?;
    }
    if has_declared_docs {
        let by_id: BTreeMap<&str, &Node> =
            nodes.iter().copied().map(|n| (n.id.as_str(), n)).collect();
        content.push_str(
            "\n## API contract drift\n\n| Finding | Endpoint | Documented | Served | Confidence |\n|---|---|---|---|---|\n",
        )?;
        contract_findings.sort_by(|a, b| {
            let ak = a.props["drift_kind"].as_str().unwrap_or("");
            let bk = b.props["drift_kind"].as_str().unwrap_or("");
            let ae = a.props["key"]
                .as_str()
                .or_else(|| a.props["canonical"].as_str())
                .unwrap_or(&a.id);
            let be = b.props["key"]
                .as_str()
                .or_else(|| b.props["canonical"].as_str())
                .unwrap_or(&b.id);
            (ak, ae, a.id.as_str()).cmp(&(bk, be, b.id.as_str()))
        });
        for n in &contract_findings {
            let drift_kind = n.props["drift_kind"].as_str().unwrap_or("contract-drift");
            let endpoint = n.props["key"]
                .as_str()
                .or_else(|| n.props["canonical"].as_str())
                .unwrap_or(&n.id);
            let documented = contract_side_cell(n, "documented", "DOCUMENTED_AS", edges, &by_id);
            let served = contract_side_cell(n, "served", "SERVED_AS", edges, &by_id);
            let p = provenance(&n.props, &n.id);
            writeln!(
                content,
                "| {} | `{}` | {} | {} | {:?} |",
                Safe(drift_kind),
                Safe(endpoint),
                Safe(&documented),
                Safe(&served),
                p.confidence_tier,
            )?;
        }
        contract_gaps.sort_by(|a, b| {
            let ad = a.props["doc_path"].as_str().unwrap_or("");
            let bd = b.props["doc_path"].as_str().unwrap_or("");
            let ar = a.props["reason"].as_str().unwrap_or("");
            let br = b.props["reason"].as_str().unwrap_or("");
            (ad, ar, a.id.as_str()).cmp(&(bd, br, b.id.as_str()))
        });
        for g in &contract_gaps {
            let doc_path = g.props["doc_path"].as_str().unwrap_or("—");
            let reason = g.props["reason"].as_str().unwrap_or(&g.id);
            writeln!(
                content,
                "| gap | `{}` | {} | — | Gap |",
                Safe(doc_path),
                Safe(reason),
            )?;
        }
        if contract_findings.is_empty() && contract_gaps.is_empty() {
            content.push_str("| — | No API contract drift recovered | — | — | — |\n")?;
        }
    }
    schema_drift(nodes, &findings, &mut content, &mut assertions)?;
    test_drift(nodes, &findings, &mut content)?;
    documentation_drift(&findings, &mut content)?;
    finish(content, assertions)
}

/// `## Documentation drift` (ADR-0065, AC-0679): route and file references
/// in the target's own documentation that no longer match, each with what
/// it was compared with. Written only when there is such a finding, so a
/// register without one keeps its bytes.
fn documentation_drift(findings: &[&Node], content: &mut Text) -> fmt::Result {
    let mut drift: Vec<&Node> = findings
        .iter()
        .copied()
        .filter(|n| n.props["kind"].as_str() == Some("documentation-drift"))
        .collect();
    if drift.is_empty() {
        return Ok(());
    }
    let text = |n: &Node, key: &str| n.props[key].as_str().unwrap_or("—").to_string();
    drift.sort_by_key(|n| {
        (
            text(n, "document"),
            text(n, "drift_kind"),
            text(n, "key"),
            n.id.clone(),
        )
    });
    content.push_str(
        "\n## Documentation drift\n\nExact route and file references in the target's own documentation that no longer match what was recovered. A reference is stated intent: it changes no recovered fact.\n\n| Finding | Document | Reference | Compared with | Confidence |\n|---|---|---|---|---|\n",
    )?;
    for n in &drift {
        let p = provenance(&n.props, &n.id);
        writeln!(
            content,
            "| {} | `{}` | `{}` | {} | {:?} |",
            Safe(text(n, "drift_kind")),
            Safe(text(n, "document")),
            Safe(text(n, "key")),
            Safe(text(n, "compared")),
            p.confidence_tier,
        )?;
    }
    Ok(())
}

/// `## Test and contract drift` (#953): what target tests expect of an
/// endpoint against what its recovered responses return, and the endpoints
/// whose evidence is incomplete. Rendered once the repository has tests.
fn test_drift(nodes: &[&Node], findings: &[&Node], content: &mut Text) -> fmt::Result {
    if !nodes.iter().any(|n| n.label == "TestCase") {
        return Ok(());
    }
    content.push_str(
        "\n## Test and contract drift\n\nStatic comparison of the statuses tests assert and the response keys they read with what each endpoint's recovered responses return, within the responses' recorded scope.\n\n| Finding | Endpoint | Statuses | Keys | Tests | Confidence |\n|---|---|---|---|---|---|\n",
    )?;
    let mut rows: Vec<&Node> = findings
        .iter()
        .copied()
        .filter(|n| n.props["kind"].as_str() == Some("test-drift"))
        .collect();
    rows.sort_by_key(|n| {
        (
            n.props["drift_kind"].as_str().unwrap_or(""),
            n.props["endpoint"].as_str().unwrap_or(""),
            n.id.as_str(),
        )
    });
    let numbers = |n: &Node| -> Vec<String> {
        n.props["statuses"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.to_string())
            .collect()
    };
    let dash = |values: Vec<String>| {
        if values.is_empty() {
            "—".to_owned()
        } else {
            values.join(", ")
        }
    };
    for n in &rows {
        let keys: Vec<String> = strings(n, "keys").into_iter().map(str::to_owned).collect();
        let tests = strings(n, "tests");
        writeln!(
            content,
            "| {} | `{}` | {} | {} | {} | {:?} |",
            Safe(n.props["drift_kind"].as_str().unwrap_or("test-drift")),
            Safe(n.props["endpoint"].as_str().unwrap_or(&n.id)),
            dash(numbers(n)),
            Safe(dash(keys)),
            tests.len(),
            provenance(&n.props, &n.id).confidence_tier
        )?;
    }
    let mut gaps: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|n| is_gap_node(n) && n.props["kind"].as_str() == Some("test-drift-gap"))
        .collect();
    gaps.sort_by_key(|n| (n.props["endpoint"].as_str().unwrap_or(""), n.id.as_str()));
    for g in &gaps {
        writeln!(
            content,
            "| incomplete evidence | `{}` | — | — | — | {} |",
            Safe(g.props["endpoint"].as_str().unwrap_or(&g.id)),
            Safe(g.props["reason"].as_str().unwrap_or("Gap"))
        )?;
    }
    if rows.is_empty() && gaps.is_empty() {
        content.push_str("| — | No test and contract drift recovered | — | — | — | — |\n")?;
    }
    Ok(())
}

/// `## Schema drift`: each replayed migration history, then its findings
/// and the tables it could not check (#833).
fn schema_drift(
    nodes: &[&Node],
    findings: &[&Node],
    content: &mut Text,
    assertions: &mut Assertions,
) -> Result<(), RenderLimit> {
    let histories: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "MigrationHistory")
        .collect();
    let mut drift: Vec<&Node> = findings
        .iter()
        .copied()
        .filter(|n| n.props["kind"].as_str() == Some("schema-drift"))
        .collect();
    let mut gaps: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|n| is_gap_node(n) && n.props["kind"].as_str() == Some("migration-gap"))
        .collect();
    if histories.is_empty() && drift.is_empty() && gaps.is_empty() {
        return Ok(());
    }
    content.push_str("\n## Schema drift\n\n")?;
    for history in &histories {
        assertions.node(history, None)?;
        let count = |key: &str| history.props[key].as_u64().unwrap_or(0);
        writeln!(
            content,
            "Migrations in `{}` replayed to head `{}` ({} revisions): {} modeled tables and {} columns compared.\n",
            Safe(history.props["location"].as_str().unwrap_or("—")),
            Safe(history.props["head"].as_str().unwrap_or("—")),
            count("revisions"),
            count("modeled_tables_compared"),
            count("modeled_columns_compared"),
        )?;
    }
    content.push_str(
        "| Finding | Table / column | Modeled | Migrated | Confidence |\n|---|---|---|---|---|\n",
    )?;
    let key = |n: &&Node| {
        (
            n.props["drift_kind"].as_str().unwrap_or("").to_string(),
            n.props["key"].as_str().unwrap_or(&n.id).to_string(),
            n.id.clone(),
        )
    };
    drift.sort_by_key(key);
    for n in &drift {
        let p = provenance(&n.props, &n.id);
        writeln!(
            content,
            "| {} | `{}` | {} | {} | {:?} |",
            Safe(n.props["drift_kind"].as_str().unwrap_or("schema-drift")),
            Safe(n.props["key"].as_str().unwrap_or(&n.id)),
            Safe(n.props["modeled"].as_str().unwrap_or("—")),
            Safe(n.props["migrated"].as_str().unwrap_or("—")),
            p.confidence_tier,
        )?;
    }
    gaps.sort_by(|a, b| a.id.cmp(&b.id));
    for g in &gaps {
        assertions.node(g, None)?;
        writeln!(
            content,
            "| gap | `{}` | {} | — | Gap |",
            Safe(g.props["table"].as_str().unwrap_or("—")),
            Safe(g.props["reason"].as_str().unwrap_or(&g.id)),
        )?;
    }
    if drift.is_empty() && gaps.is_empty() {
        content.push_str("| — | No schema drift recovered | — | — | — |\n")?;
    }
    Ok(())
}
fn contract_side_cell(
    drift: &Node,
    prop_name: &str,
    edge_label: &str,
    edges: &[&Edge],
    by_id: &BTreeMap<&str, &Node>,
) -> String {
    if let Some(s) = drift.props[prop_name].as_str().filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    let mut parts = Vec::new();
    for edge in edges
        .iter()
        .copied()
        .filter(|e| e.src == drift.id && e.label == edge_label)
    {
        if let Some(target) = by_id.get(edge.dst.as_str()) {
            parts.push(format!("{} ({})", NodeName(target), target.id));
        } else {
            parts.push(edge.dst.clone());
        }
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(", ")
    }
}
fn list(out: &mut Text, values: &[&str]) -> fmt::Result {
    if values.iter().all(|s| s.is_empty()) && values.len() < 2 {
        out.push_str("—")
    } else {
        write!(out, "{}", Safe(Join(values, ", ")))
    }
}
/// `security.md`. An endpoint finding states what was recovered, in the words
/// `endpoint-permissions.md` uses for the same fact (#1820, AC-0656).
pub(super) fn security(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: Option<&super::capabilities::Stories<'_>>,
    (mode, rejected): (ExportMode, &BTreeSet<String>),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let findings: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Finding" && n.props["kind"].as_str() == Some("security"))
        .collect();
    let mut assertions = Assertions::new(&budget);
    for n in &findings {
        assertions.node(n, None)?;
    }
    let mut content = Text::new(
        &budget,
        "# Security findings\n\n\"No authentication check recovered\" means none recovered in the source that was read: a check outside it, such as a gateway or middleware the adapters do not read, is not visible to this finding.\n\n| Finding | Type | Subject | Resource scope | Actions | US / AC | Confidence |\n|---|---|---|---|---|---|---|\n",
    )?;
    for n in &findings {
        write!(
            content,
            "| {} | `{}` | `{}` | ",
            Safe(NodeName(n)),
            Safe(security_type(
                n.props["category"].as_str().unwrap_or("security")
            )),
            Safe(n.props["subject_id"].as_str().unwrap_or("—"))
        )?;
        list(&mut content, &strings(n, "resource_scope"))?;
        content.push_str(" | ")?;
        list(&mut content, &strings(n, "actions"))?;
        writeln!(
            content,
            " | {} / {} | {:?} |",
            Safe(n.props["us_id"].as_str().unwrap_or("US-0015")),
            Safe(n.props["ac_id"].as_str().unwrap_or("—")),
            provenance(&n.props, &n.id).confidence_tier
        )?;
    }
    if findings.is_empty() {
        content.push_str("| — | — | — | — | — | No explicit security findings | — |\n")?;
    }
    if let Some(knowledge) = knowledge {
        access_per_capability(
            (nodes, edges),
            knowledge,
            (mode, rejected),
            &mut assertions,
            &mut content,
        )?;
    }
    finish(content, assertions)
}

/// `security.md` "Access control per capability" (AC-0853): under each
/// derived Capability, every operation it cites through `HANDLES` with who
/// may call it as its recovered permission site states. Operations with no
/// recovered site are counted in the lead and listed like the others.
fn access_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: &super::capabilities::Stories<'_>,
    (mode, rejected): (ExportMode, &BTreeSet<String>),
    assertions: &mut Assertions,
    content: &mut Text,
) -> Result<(), RenderLimit> {
    use super::capabilities::Support;
    use super::endpoint_permissions::{Access, AccessIndex};
    let by_id: BTreeMap<&str, &Node> = nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| (n.id.as_str(), *n))
        .collect();
    let mut sections: Vec<(&Node, Vec<&Node>)> = Vec::new();
    for story in knowledge
        .included
        .iter()
        .filter(|s| s.node.label == "Capability")
    {
        let mut operations: Vec<&Node> = story
            .support
            .iter()
            .filter_map(|(_, fact)| match fact {
                Support::Edge(e) if e.label == "HANDLES" => by_id.get(e.src.as_str()).copied(),
                _ => None,
            })
            .collect();
        operations.sort_by(|a, b| a.id.cmp(&b.id));
        operations.dedup_by(|a, b| a.id == b.id);
        if !operations.is_empty() {
            sections.push((story.node, operations));
        }
    }
    if sections.is_empty() {
        return Ok(());
    }
    let index = AccessIndex::new(nodes, edges);
    let mut rows: Vec<(&Node, Vec<(&Node, Access)>)> = Vec::new();
    let (mut guarded, mut none, mut open) = (0usize, 0usize, 0usize);
    for (capability, operations) in sections {
        let mut cells = Vec::new();
        for endpoint in operations {
            let access = index.access(endpoint, mode, rejected);
            match &access {
                Access::Guarded(_) => guarded += 1,
                Access::NoneRecovered { .. } => none += 1,
                Access::NotEstablished(_) => open += 1,
            }
            cells.push((endpoint, access));
        }
        rows.push((capability, cells));
    }
    write!(
        content,
        "\n## Access control per capability\n\nWho may call each operation of a derived \
         Capability, as the permission site recovered on its endpoint states: {guarded} with a \
         recovered guard or permission set, {none} with none recovered, {open} not established. \
         A permission site records decorator or guard syntax (Deterministic), not effective \
         runtime authorization; a check inside a handler body, in middleware or at a gateway is \
         not visible here. `endpoint-permissions.md` gives each site's span and handler path.\n"
    )?;
    let mut cited: BTreeSet<&str> = BTreeSet::new();
    for (capability, cells) in rows {
        let title = capability.props["title"].as_str().unwrap_or(&capability.id);
        writeln!(
            content,
            "\n### {}\n\n| Operation | Who may call | Citation |\n|---|---|---|",
            Safe(title)
        )?;
        for (endpoint, access) in cells {
            let who = match access {
                Access::Guarded(text) => text,
                Access::NoneRecovered { partial: false } => "none recovered".to_owned(),
                Access::NoneRecovered { partial: true } => {
                    "none recovered (handler path partial)".to_owned()
                }
                Access::NotEstablished(why) => format!("not established: {why}"),
            };
            let id = super::citation_id(&core_graph::FactKey::Node {
                id: endpoint.id.clone(),
            });
            writeln!(
                content,
                "| {} | {} | {id} |",
                Safe(NodeName(endpoint)),
                Safe(&who)
            )?;
            if cited.insert(&endpoint.id) {
                assertions.node(endpoint, None)?;
            }
        }
    }
    Ok(())
}
/// The Type cell of a `security.md` row. The stored category of an endpoint
/// finding keeps its identity (`unauthenticated_endpoint`); the document names
/// what was recovered, so it never calls the endpoint unauthenticated (#1820).
fn security_type(category: &str) -> &str {
    match category {
        "unauthenticated_endpoint" => "no_auth_check_recovered",
        other => other,
    }
}
struct Settings<'a>(&'a Node);
impl fmt::Display for Settings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.props["settings_behind_code"].as_bool() == Some(true) {
            return f.write_str("settings live in code — detected by presence only");
        }
        let Some(values) = self.0.props["settings"]
            .as_object()
            .filter(|v| !v.is_empty())
        else {
            return f.write_str("—");
        };
        for (i, (k, v)) in values.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{k}={}", Json(v))?;
        }
        Ok(())
    }
}
pub(super) fn toolchain(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let tools: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Tool")
        .collect();
    let paths: Vec<Vec<&str>> = tools
        .iter()
        .map(|n| {
            let mut paths: Vec<_> = edges
                .iter()
                .filter(|e| e.label == "DEFINED_IN" && e.src == n.id)
                .map(|e| e.dst.split_once('@').map(|(_, p)| p).unwrap_or(&e.dst))
                .collect();
            paths.sort();
            paths.dedup();
            paths
        })
        .collect();
    let mut assertions = Assertions::new(&budget);
    for (n, p) in tools.iter().zip(&paths) {
        assertions.tool(n, p)?;
    }
    let mut content = Text::new(
        &budget,
        "# Toolchain\n\n| Tool | Category | Defined in | Settings | Confidence |\n|---|---|---|---|---|\n",
    )?;
    for (n, paths) in tools.iter().zip(&paths) {
        writeln!(
            content,
            "| {} | {} | {} | {} | {:?} |",
            Safe(n.props["display"].as_str().unwrap_or(&n.id)),
            Safe(n.props["category"].as_str().unwrap_or("—")),
            Safe(Join(paths, ", ")),
            Safe(Settings(n)),
            provenance(&n.props, &n.id).confidence_tier
        )?;
    }
    if tools.is_empty() {
        content.push_str("| — | — | — | No toolchain facts recovered | — |\n")?;
    }
    finish(content, assertions)
}

#[derive(Default)]
struct Reference<'a> {
    tests: Vec<&'a str>,
    statuses: BTreeSet<u64>,
    keys: BTreeSet<&'a str>,
}

/// Static test-to-endpoint references (#946). A reference says a test issues
/// a request to an endpoint; it is not evidence that the test runs or passes.
pub(super) fn tests(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let test_ids: BTreeSet<&str> = nodes
        .iter()
        .filter(|n| n.label == "TestCase")
        .map(|n| n.id.as_str())
        .collect();
    let endpoints: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Endpoint")
        .filter(nongap)
        .collect();
    let endpoint_ids: BTreeSet<&str> = endpoints.iter().map(|n| n.id.as_str()).collect();
    let requests: Vec<&Edge> = edges
        .iter()
        .copied()
        .filter(|e| e.label == "REQUESTS" && test_ids.contains(e.src.as_str()))
        .collect();
    let mut linked: BTreeMap<&str, Reference> = BTreeMap::new();
    let mut unresolved = 0;
    for edge in &requests {
        if !edge_nongap(edge) {
            unresolved += 1;
        } else if endpoint_ids.contains(edge.dst.as_str()) {
            let reference = linked.entry(edge.dst.as_str()).or_default();
            // A test id is `test:{repo}@{path}#{qualname}`; the row names
            // the test by its path and qualified name.
            reference.tests.push(
                edge.src
                    .split_once('@')
                    .map_or(edge.src.as_str(), |(_, t)| t),
            );
            let values = |key| edge.props[key].as_array().into_iter().flatten();
            reference
                .statuses
                .extend(values("statuses").filter_map(serde_json::Value::as_u64));
            reference
                .keys
                .extend(values("keys").filter_map(serde_json::Value::as_str));
        }
    }
    let mut assertions = Assertions::new(&budget);
    for edge in requests.iter().copied().filter(edge_nongap) {
        if endpoint_ids.contains(edge.dst.as_str()) {
            assertions.edge(edge)?;
        }
    }
    for node in endpoints
        .iter()
        .filter(|n| !linked.contains_key(n.id.as_str()))
    {
        assertions.node(node, None)?;
    }
    let mut content = Text::new(
        &budget,
        "# Test arrangements\n\nStatic references from test code to recovered endpoints. \
         A reference means a test issues a request to the endpoint; it is not \
         evidence that the test runs or passes.\n\n| Measure | Count |\n|---|---|\n",
    )?;
    writeln!(
        content,
        "| Recovered tests | {} |\n| Test → endpoint references | {} |\n\
         | Unresolved test requests (see gap_register.md) | {unresolved} |\n\
         | Endpoints referenced by a test | {} of {} |",
        test_ids.len(),
        linked.values().map(|r| r.tests.len()).sum::<usize>(),
        linked.len(),
        endpoints.len()
    )?;
    content.push_str(
        "\n## Referenced endpoints\n\n| Endpoint | Tests | Asserted statuses | Response keys read |\n|---|---|---|---|\n",
    )?;
    for node in &endpoints {
        let Some(reference) = linked.get(node.id.as_str()) else {
            continue;
        };
        let statuses: Vec<String> = reference.statuses.iter().map(u64::to_string).collect();
        let keys: Vec<&str> = reference.keys.iter().copied().collect();
        let or_dash = |values: String| {
            if values.is_empty() {
                "—".to_owned()
            } else {
                values
            }
        };
        writeln!(
            content,
            "| {} | {} | {} | {} |",
            Safe(NodeName(node)),
            Safe(Join(&reference.tests, ", ")),
            or_dash(statuses.join(", ")),
            Safe(or_dash(keys.join(", ")))
        )?;
    }
    if linked.is_empty() {
        content.push_str("| — | — | — | No test references recovered |\n")?;
    }
    content.push_str("\n## Endpoints without a recovered test reference\n\n")?;
    let mut any = false;
    for node in endpoints
        .iter()
        .filter(|n| !linked.contains_key(n.id.as_str()))
    {
        any = true;
        writeln!(content, "- {}", Safe(NodeName(node)))?;
    }
    if !any {
        content.push_str("None.\n")?;
    }
    finish(content, assertions)
}

/// Bounded path-router and key-router journey candidates (`journey-candidate@2`).
pub(super) fn journeys(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    (mode, rejected): (ExportMode, &BTreeSet<String>),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let search = crate::rules::journey::search(
        nodes,
        edges,
        flows,
        crate::rules::journey::JourneyBudget::default(),
    );
    let candidates: Vec<_> = search
        .candidates
        .iter()
        .filter(|c| super::included(&c.provenance, mode, rejected))
        .cloned()
        .collect();
    let mut assertions = Assertions::new(&budget);
    for candidate in &candidates {
        assertions.journey(candidate)?;
    }
    let mut content = Text::new(&budget, "# Journey candidates\n\n")?;
    // Pages, handlers, operations and tables by name (#1981); the ids stay
    // on each journey's `Id` line, in the fact index and in `bundle.json`.
    let names = JourneyNames(nodes.iter().map(|n| (n.id.as_str(), *n)).collect());
    if mode == ExportMode::BestEffort && !search.entries.is_empty() {
        content.push_str(
            "## Search\n\n| Entry | Expanded | Candidates | Truncated | Assumed navigations | Unread callbacks |\n|---|---|---|---|---|---|\n",
        )?;
        for entry in &search.entries {
            let trunc_str = entry.truncated.map_or("none", |t| t.as_str());
            writeln!(
                content,
                "| {} | {} | {} | `{}` | {} | {} |",
                Safe(names.screen(&entry.entry)),
                Safe(entry.expanded_states),
                Safe(entry.candidates),
                Safe(trunc_str),
                Safe(entry.assumed_navigations),
                Safe(entry.unread_callbacks)
            )?;
        }
        content.push_str("\n")?;
    }
    if candidates.is_empty() {
        writeln!(
            content,
            "Interpretation: rule journey-candidate@{}, InferredWeak, never a confirmed business journey\n",
            crate::rules::journey::JOURNEY_RULE_VERSION
        )?;
        content.push_str(if mode == ExportMode::BestEffort {
            "No journey candidates recovered.\n"
        } else {
            "Journey candidates are InferredWeak, so a verified-only export withholds them.\n"
        })?;
        return finish(content, assertions);
    }
    for (i, candidate) in candidates.iter().enumerate() {
        if i > 0 {
            content.push_str("\n")?;
        }
        // An alternative (ADR-0084) names the screens its goal's shortest
        // path skips, so two journeys to one goal never share a heading.
        // From the unfiltered search, so an alternative whose primary was
        // rejected or withheld still names it and is never shown as a primary.
        let primary = candidate
            .alternative_of
            .as_ref()
            .and_then(|id| search.candidates.iter().find(|c| &c.id == id));
        match primary {
            Some(primary) => {
                let via: Vec<String> = candidate
                    .steps
                    .iter()
                    .filter(|step| !primary.steps.iter().any(|p| p.screen == step.screen))
                    .map(|step| names.screen(&step.screen))
                    .collect();
                writeln!(
                    content,
                    "## {} (via {})\n",
                    Safe(&candidate.label),
                    Safe(via.join(", "))
                )?;
            }
            None => writeln!(content, "## {}\n", Safe(&candidate.label))?,
        }
        writeln!(content, "{}\n", Safe(names.path(candidate)))?;
        writeln!(
            content,
            "- Id: `{}`\n- Support fingerprint: `{}`\n- Entry: {}\n\
             - Interpretation: rule journey-candidate@{}, InferredWeak, never a confirmed business journey\n",
            Safe(&candidate.id),
            Safe(&candidate.support_fingerprint),
            Safe(names.screen(&candidate.entry)),
            crate::rules::journey::JOURNEY_RULE_VERSION
        )?;
        content.push_str("### Steps\n\n| # | Screen | Navigation | Client calls | Unresolved |\n|---|---|---|---|---|\n")?;
        for (idx, step) in candidate.steps.iter().enumerate() {
            let nav_str = match &step.navigation {
                None => "—".to_owned(),
                Some(nav) => names.navigation(nav),
            };
            let calls_str = if step.calls.is_empty() {
                "—".to_owned()
            } else {
                step.calls
                    .iter()
                    .map(|c| names.operation(&c.endpoint))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mut unresolved_items: Vec<String> = step
                .unresolved
                .iter()
                .map(|u| {
                    format!(
                        "{}: `{}` → {}",
                        u.kind.as_str(),
                        names.symbol(&u.source),
                        names.name(&u.gap_id)
                    )
                })
                .collect();
            unresolved_items.extend(step.unread_callbacks.iter().map(|u| {
                format!(
                    "unread callback: {} (`{}`)",
                    u.param,
                    names.symbol(&u.composable)
                )
            }));
            let unresolved_str = if unresolved_items.is_empty() {
                "—".to_owned()
            } else {
                unresolved_items.join(", ")
            };
            writeln!(
                content,
                "| {idx} | {} | {} | {} | {} |",
                Safe(names.screen(&step.screen)),
                Safe(&nav_str),
                Safe(&calls_str),
                Safe(&unresolved_str)
            )?;
        }
        if !candidate.assumptions.is_empty() {
            content.push_str(
                "\n### Assumptions\n\n| Step | Kind | Screen | Symbol | Param | Composable | Registration | Forwarded | Limitation |\n|---|---|---|---|---|---|---|---|---|\n",
            )?;
            for assumption in &candidate.assumptions {
                // A Hilt dispatch names the interface member it crossed from.
                let symbol_str = match &assumption.interface {
                    Some(interface) => format!(
                        "{} → {}",
                        names.symbol(interface),
                        names.symbol(&assumption.symbol)
                    ),
                    None => names.symbol(&assumption.symbol),
                };
                let param_str = assumption.param.as_deref().unwrap_or("—");
                let composable_str = assumption
                    .composable
                    .as_deref()
                    .map_or_else(|| "—".to_owned(), |c| names.symbol(c));
                let registration_str = assumption.registration.as_ref().map_or_else(
                    || "—".to_owned(),
                    |r| format!("{}:{}-{}", r.path, r.byte_start, r.byte_end),
                );
                let forwarded_str = if assumption.forwarded.is_empty() {
                    "—".to_owned()
                } else {
                    assumption
                        .forwarded
                        .iter()
                        .map(|h| format!("{}.{}", names.symbol(&h.composable), h.param))
                        .collect::<Vec<_>>()
                        .join(" → ")
                };
                writeln!(
                    content,
                    "| {} | `{}` | {} | `{}` | {} | {} | {} | {} | {} |",
                    Safe(assumption.step),
                    Safe(assumption.kind.as_str()),
                    Safe(names.screen(&assumption.screen)),
                    Safe(&symbol_str),
                    Safe(param_str),
                    Safe(&composable_str),
                    Safe(&registration_str),
                    Safe(&forwarded_str),
                    Safe(&assumption.limitation)
                )?;
            }
        }
        let writes_str = if candidate.goal.writes.is_empty() {
            "—".to_owned()
        } else {
            candidate
                .goal
                .writes
                .iter()
                .map(|w| {
                    format!(
                        "`{}` → {}",
                        names.symbol(&w.symbol),
                        names.table(&w.data_entity)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let result_str = match &candidate.result_step {
            Some(res) => format!(
                "{} ({})",
                candidate.result.as_str(),
                names.screen(&res.screen)
            ),
            None => candidate.result.as_str().to_owned(),
        };
        writeln!(
            content,
            "\n### Goal and result\n\n\
             - Endpoint: {} ({})\n\
             - Goal basis: `{}`\n\
             - Write coverage: `{}`\n\
             - Writes: {}\n\
             - Result: {}\n",
            Safe(names.operation(&candidate.goal.endpoint)),
            Safe(&candidate.goal.method),
            candidate.goal.goal_basis.as_str(),
            candidate
                .goal
                .write_coverage
                .as_deref()
                .unwrap_or("observed"),
            Safe(&writes_str),
            Safe(&result_str)
        )?;
        let flows_str = if candidate.flows.is_empty() {
            "—".to_owned()
        } else {
            names.list(&candidate.flows, |id| names.operation(id))
        };
        let tests_str = if candidate.tests.is_empty() {
            "—".to_owned()
        } else {
            names.list(&candidate.tests, |id| format!("`{}`", names.symbol(id)))
        };
        writeln!(
            content,
            "### Flows and tests\n\n- Flows: {}\n- Tests: {}\n",
            Safe(&flows_str),
            Safe(&tests_str)
        )?;
        let assumed_str = if candidate.earlier_assumed_goals.is_empty() {
            "—".to_owned()
        } else {
            candidate
                .earlier_assumed_goals
                .iter()
                .map(|g| format!("{} ({})", names.operation(&g.endpoint), g.write_coverage))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let cond_str = if candidate.conditional_steps.is_empty() {
            "—".to_owned()
        } else {
            candidate
                .conditional_steps
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let trunc_str = candidate.truncated.map_or("none", |t| t.as_str());
        writeln!(
            content,
            "### Assumptions, truncation and alternatives\n\n\
             - Earlier assumed goals: {}\n\
             - Conditional steps: {}\n\
             - Truncated: `{trunc_str}`\n\
             - Alternatives: {} (complete: {})",
            Safe(&assumed_str),
            Safe(&cond_str),
            candidate.alternatives,
            candidate.alternatives_complete
        )?;
        if let Some(primary) = primary {
            writeln!(
                content,
                "- Alternative of: {} (`{}`)",
                Safe(&primary.label),
                Safe(&primary.id)
            )?;
        }
    }
    finish(content, assertions)
}

/// The names a journey prints for the facts it walks (#1981): a page by its
/// route and view name, a handler by its name, an operation as `METHOD
/// path`, a table by its name, and an id that resolves to no node as itself.
struct JourneyNames<'a>(BTreeMap<&'a str, &'a Node>);

impl JourneyNames<'_> {
    /// Most tests or flows a journey lists before "and N more".
    const LISTED: usize = 5;

    fn name(&self, id: &str) -> String {
        self.0
            .get(id)
            .map_or_else(|| id.to_owned(), |n| NodeName(n).to_string())
    }
    /// "`/auth/login` (auth.login)": the route a reader opens, then the
    /// view's name when it says something the route does not.
    fn screen(&self, id: &str) -> String {
        let Some(node) = self.0.get(id) else {
            return format!("`{id}`");
        };
        let route = node.props["route"].as_str();
        let name = node.props["name"].as_str();
        match (route, name) {
            (Some(route), Some(name)) if name != route => format!("`{route}` ({name})"),
            (Some(route), _) => format!("`{route}`"),
            _ => format!("`{}`", NodeName(node)),
        }
    }
    fn operation(&self, id: &str) -> String {
        self.name(id)
    }
    fn table(&self, id: &str) -> String {
        format!("`{}`", self.name(id))
    }
    fn navigation(&self, nav: &crate::rules::journey::StepNavigation) -> String {
        let conditional = nav.sites_taken.iter().any(|s| s.conditional);
        let sites = nav
            .sites_taken
            .iter()
            .map(|s| format!("{} (byte {})", s.path, s.byte_start))
            .collect::<Vec<_>>();
        format!(
            "from {}{} at {}",
            self.via(&nav.source),
            if conditional { ", conditionally," } else { "" },
            if sites.is_empty() {
                "no recorded site".to_owned()
            } else {
                sites.join(", ")
            }
        )
    }
    /// Where a navigation starts: a link in a template or file, or a handler.
    fn via(&self, id: &str) -> String {
        match self.0.get(id) {
            Some(node) if matches!(node.label.as_str(), "File" | "Template") => {
                format!("a link in `{}`", NodeName(node))
            }
            _ => format!("`{}`", self.symbol(id)),
        }
    }
    /// A callable or test by its qualified name (`CreationRemoteDatasource.create`),
    /// which its id carries after `#`; another fact by its name.
    fn symbol(&self, id: &str) -> String {
        match id.rsplit_once('#') {
            Some((_, qualified)) if id.starts_with("sym:") || id.starts_with("test:") => {
                qualified.to_owned()
            }
            _ => self.name(id),
        }
    }
    fn list(&self, ids: &[String], show: impl Fn(&str) -> String) -> String {
        let mut shown: Vec<String> = ids.iter().take(Self::LISTED).map(|id| show(id)).collect();
        if ids.len() > Self::LISTED {
            shown.push(format!("and {} more", ids.len() - Self::LISTED));
        }
        shown.join(", ")
    }
    /// One sentence a reader follows: each page in order, how it was
    /// reached, then the goal call and what it writes.
    fn path(&self, candidate: &crate::rules::journey::JourneyCandidate) -> String {
        let mut text = String::from("**Path:** ");
        for (i, step) in candidate.steps.iter().enumerate() {
            if i > 0 {
                text.push_str(" → ");
            }
            text.push_str(&self.screen(&step.screen));
            if let Some(nav) = &step.navigation {
                let conditional = nav.sites_taken.iter().any(|s| s.conditional);
                let _ = write!(
                    text,
                    " (via {}{})",
                    self.via(&nav.source),
                    if conditional { ", conditional" } else { "" }
                );
            }
        }
        let _ = write!(text, ", then {}", self.operation(&candidate.goal.endpoint));
        let tables: BTreeSet<String> = candidate
            .goal
            .writes
            .iter()
            .map(|w| self.table(&w.data_entity))
            .collect();
        if tables.is_empty() {
            text.push('.');
        } else {
            let _ = write!(
                text,
                ", which writes {}.",
                tables.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        text
    }
}

/// Base Capability references retained by stories_with_support. Appended
/// sidecar references belong to the combined bound, not this base-only role.
pub(super) fn logical_story_index_bytes(rows: usize) -> Option<usize> {
    super::accounting::sum(&[
        super::accounting::slots::<&Node>(rows)?,
        std::mem::size_of::<Vec<&Node>>(),
        std::mem::size_of::<BTreeSet<&str>>(), // common gap-count set header
    ])
}

/// Explicit borrowed index roles; input properties are not cloned by these
/// vectors/maps. Topology's owned selected graph is charged by the caller.
pub(super) fn logical_index_bytes(rows: usize, values: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    let data = sum(&[
        map_slots::<&str, &Node>(rows)?, // by_id
        slots::<&Node>(rows)?,
        set_slots::<&str>(rows)?, // entities/entity_ids
        slots::<&Edge>(rows)?,
        set_slots::<&str>(rows)?, // mappings/ids
        slots::<&Node>(rows)?,
        map_slots::<&str, usize>(rows)?, // model_nodes/aliases
        slots::<(&&Edge, &Vec<serde_json::Value>)>(rows)?, // relation_fields
    ])?;
    let tools = sum(&[
        slots::<&Node>(rows)?,
        slots::<Vec<&str>>(rows)?,
        slots::<&str>(rows)?,
        slots::<&str>(rows)?,
    ])?; // paths + one sort scratch
    let tests = sum(&[
        set_slots::<&str>(rows)?, // test_ids + endpoint_ids
        set_slots::<&str>(rows)?,
        slots::<&Node>(rows)?,
        slots::<&Edge>(rows)?,
        map_slots::<&str, Reference<'static>>(rows)?,
        slots::<&str>(rows)?,       // referencing tests
        set_slots::<u64>(values)?,  // asserted statuses
        set_slots::<&str>(values)?, // read keys
        slots::<String>(values)?,
        slots::<&str>(values)?,
    ])?;
    let simple = sum(&[
        slots::<&Node>(rows)?,
        slots::<&Edge>(rows)?,
        set_slots::<&str>(rows)?,
        set_slots::<&str>(rows)?, // topology backed + configured
        slots::<&str>(values)?,
    ])?;
    let endpoints = super::endpoint_permissions::logical_index_bytes(rows)?;
    let pipelines = super::pipelines::logical_index_bytes(rows)?;
    let configuration = super::configuration::logical_index_bytes(rows, values)?;
    let external = super::external::logical_index_bytes(rows, values)?;
    let environments = super::environments::logical_index_bytes(rows, values)?;
    let curated = super::curated::logical_index_bytes(rows)?;
    let inventory = super::endpoint_inventory::logical_index_bytes(rows)?;
    Some(
        data.max(tools)
            .max(tests)
            .max(simple)
            .max(endpoints)
            .max(pipelines)
            .max(configuration)
            .max(external)
            .max(environments)
            .max(curated)
            .max(inventory),
    )
}
