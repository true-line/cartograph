//! D1 §1.3 and §1.5 beyond refusals (#1998, AC-0882): what each operation of
//! a capability takes, returns on success and changes, and the rate limits
//! that bound it.
//!
//! For each derived Capability, each Endpoint it cites is one operation. Its
//! row states, from facts already recovered and nothing read from source:
//!
//! - **Takes**: the literal input names its handler reads (`request_inputs`);
//! - **On success**: the 2xx statuses of `responses`, each with the JSON
//!   keys the adapter tied to it (#1998, AC-0902). The recorded body kinds
//!   are not tied to a status, so they are left to `endpoint-inventory.md`;
//! - **Changes**: the entities written, with the operation (insert, update,
//!   delete), by the handler and the Symbols it reaches along `CALLS` within
//!   [`rules::CALL_DEPTH`] hops, from `WRITES` edges;
//! - **Not recovered**: each of these classes the graph holds nothing for,
//!   named for the operation, so an absent statement always has a row.
//!
//! §1.5 lists the flask-limiter rate limits on each capability's operations
//! (`rate_limits`) as limits, beside the rejection exits.
//!
//! The rows are observations of what the code is recorded to do, with the
//! tier and confidence of each cited fact; the conditions under which a
//! write happens are not established here (`flow_dossiers.md` holds the
//! traced flows).

use super::rules::CALL_DEPTH;
use crate::workbench::capabilities::{Stories, Story, Support};
use crate::workbench::endpoint_inventory::{INPUTS_KIND, RESPONSES_KIND};
use crate::workbench::endpoint_inventory::{rate_limits, reads, shape, status_keys, strings};
use crate::workbench::render::{Assertions, Literal, NodeName, RenderLimit, Text};
use crate::workbench::{ExportMode, citation_id};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The graph indexes one D1 compile reads.
struct Index<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    calls: BTreeMap<&'a str, Vec<&'a str>>,
    writes: BTreeMap<&'a str, Vec<&'a Edge>>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut calls: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut writes: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for edge in edges {
            let (Some(src), Some(dst)) =
                (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
            else {
                continue;
            };
            match edge.label.as_str() {
                "CALLS" if src.label == "Symbol" && dst.label == "Symbol" => {
                    calls.entry(&src.id).or_default().push(&dst.id);
                }
                "WRITES" if src.label == "Symbol" && dst.label == "DataEntity" => {
                    writes.entry(&src.id).or_default().push(edge);
                }
                _ => {}
            }
        }
        Self {
            by_id,
            calls,
            writes,
        }
    }

    /// The `WRITES` edges of `handler` and the Symbols it reaches, nearest
    /// first, each once.
    fn writes_from(&self, handler: &'a str) -> Vec<&'a Edge> {
        let mut seen: BTreeSet<&str> = BTreeSet::from([handler]);
        let mut front = vec![handler];
        let mut out: Vec<&Edge> = Vec::new();
        for depth in 0..=CALL_DEPTH {
            let mut next = Vec::new();
            for id in &front {
                out.extend(self.writes.get(id).into_iter().flatten());
                if depth < CALL_DEPTH {
                    for callee in self.calls.get(id).into_iter().flatten() {
                        if seen.insert(callee) {
                            next.push(*callee);
                        }
                    }
                }
            }
            front = next;
        }
        out
    }
}

/// `inserts Alias; updates AliasUsedOn` from the cited writes.
fn changes(index: &Index<'_>, writes: &[&Edge]) -> String {
    let mut by_entity: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for edge in writes {
        let entity = index
            .by_id
            .get(edge.dst.as_str())
            .map_or_else(|| edge.dst.clone(), |n| NodeName(n).to_string());
        let operations = strings(&edge.props["operations"]);
        let set = by_entity.entry(entity).or_default();
        if operations.is_empty() {
            set.insert(edge.props["operation"].as_str().unwrap_or("writes"));
        } else {
            set.extend(operations);
        }
    }
    let mut verbs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (entity, operations) in by_entity {
        for operation in operations {
            let verb = match operation {
                "insert" => "inserts",
                "update" => "updates",
                "delete" => "deletes",
                _ => "writes",
            };
            verbs.entry(verb).or_default().push(entity.clone());
        }
    }
    ["inserts", "updates", "deletes", "writes"]
        .into_iter()
        .filter_map(|verb| {
            verbs
                .get(verb)
                .map(|entities| format!("{verb} {}", entities.join(", ")))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The operations a capability cites: its Endpoint facts and the handler
/// each one's `HANDLES` edge names.
fn operations<'a>(story: &Story<'a>) -> Vec<(&'a Node, Option<&'a str>)> {
    let handlers: BTreeMap<&str, &str> = story
        .support
        .iter()
        .filter_map(|(_, fact)| match fact {
            Support::Edge(edge) if edge.label == "HANDLES" => {
                Some((edge.src.as_str(), edge.dst.as_str()))
            }
            _ => None,
        })
        .collect();
    story
        .support
        .iter()
        .filter_map(|(_, fact)| match fact {
            Support::Node(node) if node.label == "Endpoint" => Some(*node),
            _ => None,
        })
        .map(|endpoint| {
            let handler = handlers
                .get(endpoint.id.as_str())
                .copied()
                .or_else(|| endpoint.props["handler_sym"].as_str());
            (endpoint, handler)
        })
        .collect()
}

fn capabilities<'s, 'a>(knowledge: &'s Stories<'a>) -> impl Iterator<Item = &'s Story<'a>> {
    knowledge
        .included
        .iter()
        .filter(|story| story.node.label == "Capability")
}

/// Cite a derived shape fact once; its citation id.
fn cite(
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    id: String,
    kind: &str,
    summary: &str,
    prov: core_prov::Provenance,
) -> Result<String, RenderLimit> {
    let citation = citation_id(&FactKey::Node { id: id.clone() });
    if asserted.insert(citation.clone()) {
        assertions.derived(&id, kind, &summary, prov)?;
    }
    Ok(citation)
}

/// §1.3: what each operation of each capability takes, returns on success
/// and changes. Returns the number of facts cited.
pub(super) fn operations_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: &Stories<'_>,
    mode: ExportMode,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let index = Index::new(nodes, edges);
    let total: usize = capabilities(knowledge).map(|s| operations(s).len()).sum();
    if total == 0 {
        return Ok(0);
    }
    writeln!(
        out,
        "\nWhat each operation takes, returns on success and changes: {total} operations. \
         \"Takes\" is the literal input names the handler reads; \"On success\" the 2xx \
         statuses its returns prove (`endpoint-inventory.md` lists the body kinds and JSON keys \
         across all its responses); \"Changes\" the entities the handler, and what it calls within {CALL_DEPTH} \
         hops, inserts, updates or deletes. When each write happens is not established here. \
         \"Not recovered\" names each of these the graph holds nothing for, which is not \
         evidence that the operation has none.\n"
    )?;
    let mut cited = 0usize;
    let mut headings = super::AreaHeadings::new(knowledge);
    for (area, story) in knowledge.capabilities() {
        let ops = operations(story);
        if ops.is_empty() {
            continue;
        }
        let marks = headings.before(area, out)?;
        writeln!(out, "{marks} {}\n", Literal(NodeName(story.node)))?;
        out.push_str(
            "| Operation | Takes | On success | Changes | Not recovered | Citation |\n\
             |---|---|---|---|---|---|\n",
        )?;
        for (endpoint, handler) in ops {
            let mut missing: Vec<&str> = Vec::new();
            let mut cites: Vec<String> = Vec::new();
            let takes = match shape(endpoint, "request_inputs", mode) {
                Some((raw, prov)) => {
                    let text = reads(raw);
                    cites.push(cite(
                        (assertions, asserted),
                        format!("request-inputs:{}", endpoint.id),
                        INPUTS_KIND,
                        &format!("Request inputs of {}: {text}", endpoint.id),
                        prov,
                    )?);
                    text
                }
                None => {
                    missing.push("inputs");
                    "—".into()
                }
            };
            let success = match shape(endpoint, "responses", mode) {
                Some((raw, prov)) => {
                    let ok: Vec<String> = raw["statuses"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_u64)
                        .filter(|status| (200..300).contains(status))
                        .map(|status| match status_keys(raw, status)[..] {
                            [] => status.to_string(),
                            ref keys => format!("{status} with JSON keys {}", keys.join(", ")),
                        })
                        .collect();
                    if ok.is_empty() {
                        missing.push("success response");
                        "—".into()
                    } else {
                        // Body kinds are not tied to statuses in the fact, so a
                        // kind could belong to an error return: not stated here.
                        // Keys are, when the adapter recorded them per status.
                        let mut text = ok.join("; ");
                        if strings(&raw["partial"]).contains(&"response_built_elsewhere") {
                            text.push_str("; body built outside the handler");
                        }
                        cites.push(cite(
                            (assertions, asserted),
                            format!("responses:{}", endpoint.id),
                            RESPONSES_KIND,
                            &format!("Responses of {}: {text}", endpoint.id),
                            prov,
                        )?);
                        text
                    }
                }
                None => {
                    missing.push("success response");
                    "—".into()
                }
            };
            let writes = handler.map(|h| index.writes_from(h)).unwrap_or_default();
            let changed = if writes.is_empty() {
                missing.push("writes");
                "none traced".into()
            } else {
                for edge in &writes {
                    let key = FactKey::Edge {
                        source: edge.src.clone(),
                        label: edge.label.clone(),
                        destination: edge.dst.clone(),
                    };
                    let citation = citation_id(&key);
                    if asserted.insert(citation.clone()) {
                        assertions.edge(edge)?;
                        cited += 1;
                    }
                    if !cites.contains(&citation) {
                        cites.push(citation);
                    }
                }
                changes(&index, &writes)
            };
            cited += cites.len();
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} |",
                Literal(NodeName(endpoint)),
                Literal(takes),
                Literal(success),
                Literal(changed),
                if missing.is_empty() {
                    "—".to_owned()
                } else {
                    missing.join(", ")
                },
                if cites.is_empty() {
                    "—".to_owned()
                } else {
                    cites.join(", ")
                },
            )?;
        }
        out.push_str("\n")?;
    }
    Ok(cited)
}

/// §1.5 under one capability (#2539): the operations it serves, each
/// with its own Endpoint fact, so a rule can be stated for its operation
/// and cite it. Written right under the capability's heading; the rate
/// limits follow its rule tables ([`capability_limits`]). Returns the
/// number of facts cited.
pub(super) fn capability_operations(
    story: &Story<'_>,
    mode: ExportMode,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let (cited, _) = operations_and_limits(story, mode, (assertions, asserted), out, true)?;
    Ok(cited)
}

/// §1.5 after one capability's rule tables (#2539, AC-0882): each
/// operation's flask-limiter rate limit as a rule row citing the
/// operation's Endpoint, whose recorded `rate_limits` hold the figures.
pub(super) fn capability_limits(
    story: &Story<'_>,
    mode: ExportMode,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let (_, limits) = operations_and_limits(story, mode, (assertions, asserted), out, false)?;
    Ok(limits)
}

/// The operations line (`line`) or the rate-limit table of one
/// capability; the facts cited and the limit rows written.
fn operations_and_limits(
    story: &Story<'_>,
    mode: ExportMode,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
    line: bool,
) -> Result<(usize, usize), RenderLimit> {
    let mut endpoints: Vec<&Node> = operations(story)
        .into_iter()
        .map(|(endpoint, _)| endpoint)
        .collect();
    endpoints.sort_by_cached_key(|endpoint| (NodeName(endpoint).to_string(), endpoint.id.clone()));
    endpoints.dedup_by(|a, b| a.id == b.id);
    if endpoints.is_empty() {
        return Ok((0, 0));
    }
    let mut listed = Vec::with_capacity(endpoints.len());
    let mut limits = Vec::new();
    for endpoint in &endpoints {
        let citation = citation_id(&FactKey::Node {
            id: endpoint.id.clone(),
        });
        if asserted.insert(citation.clone()) {
            assertions.node(endpoint, None)?;
        }
        listed.push(format!("{} ({citation})", Literal(NodeName(endpoint))));
        if let Some((text, unresolved, _)) = rate_limits(endpoint, mode) {
            // The rule names its operation: two operations sharing one limit
            // state two rules, which the drafter's repeated-rule check keeps.
            // The refusal is the recorded status; a breach callback's
            // response replaces it, and its status is not recorded. A graph
            // recovered before the status was recorded marks neither, and
            // flask-limiter's own refusal is 429.
            let recorded = &endpoint.props["rate_limits"];
            let on_breach = recorded["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| entry["on_breach"] == true));
            let refusal = match recorded["exceeded_status"].as_u64() {
                Some(status) => format!("requests past it are refused with {status}"),
                None if on_breach => {
                    "requests past it get the response of a breach callback".to_owned()
                }
                None => "requests past it are refused with 429".to_owned(),
            };
            let mut rule = format!("{} accepts {text}; {refusal}.", NodeName(endpoint));
            if unresolved {
                rule.push_str(" A declared limit's value is not resolved.");
            }
            limits.push(format!(
                "| {} | {} | rate limit declared on the handler | {citation} |",
                Literal(NodeName(endpoint)),
                Literal(&rule),
            ));
        }
    }
    if line {
        writeln!(out, "Operations: {}.\n", listed.join("; "))?;
    } else if !limits.is_empty() {
        writeln!(
            out,
            "Rate limits:\n\n| Where | Rule | Source | Citation |\n|---|---|---|---|"
        )?;
        for row in &limits {
            writeln!(out, "{row}")?;
        }
        out.push_str("\n")?;
    }
    Ok((endpoints.len(), limits.len()))
}

/// Whether an operation of `story` has a recorded rate limit `mode`
/// admits: §1.5 then lists the capability even when no other rule of it is
/// recovered (#2539).
pub(super) fn has_limits(story: &Story<'_>, mode: ExportMode) -> bool {
    operations(story)
        .into_iter()
        .any(|(endpoint, _)| rate_limits(endpoint, mode).is_some())
}

/// Logical workspace of [`operations_per_capability`] and
/// [`limits_per_capability`] beyond the borrowed graph, stories and output:
/// the id, call and write indexes, one operation's traversal and writes,
/// its per-entity grouping and one row's text, and the retained limit rows.
/// Charged at the graph's row, byte and value ceilings; `None` on overflow.
pub(crate) fn operations_per_capability_workspace(
    graph: flowtracer::TraceGraphBound,
) -> Option<usize> {
    use crate::workbench::accounting::{map_slots, set_slots, slots, sum};
    let rows = graph.rows;
    let index = sum(&[
        map_slots::<&str, &Node>(rows)?,
        map_slots::<&str, Vec<&str>>(rows)?,
        slots::<&str>(rows)?,
        map_slots::<&str, Vec<&Edge>>(rows)?,
        slots::<&Edge>(rows)?,
    ])?;
    let traversal = sum(&[
        set_slots::<&str>(rows)?,
        slots::<&str>(rows)?.checked_mul(2)?,
        slots::<&Edge>(rows)?,
        map_slots::<String, BTreeSet<&str>>(rows)?,
        map_slots::<&str, Vec<String>>(rows)?,
        graph.bytes.checked_mul(2)?,
    ])?;
    // Rows of text: each operation's cells and citation ids, at most the
    // graph's bytes several times over, and the retained limit rows.
    let text = graph.bytes.checked_mul(6)?;
    let limits = sum(&[
        slots::<(String, String, String, String)>(rows)?,
        graph.bytes.checked_mul(4)?,
    ])?;
    sum(&[index, traversal, text, limits])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::render::Budget;
    use core_graph::overlay::BaseRef;

    /// §1.3 and §1.5 for one capability over a recovered Flask repository.
    fn render(files: &[(&str, &str)], mode: ExportMode) -> (String, String) {
        render_with(files, mode, |_| {})
    }

    /// [`render`], with `edit` applied to the recovered nodes first.
    fn render_with(
        files: &[(&str, &str)],
        mode: ExportMode,
        edit: impl Fn(&mut Vec<Node>),
    ) -> (String, String) {
        let dir = tempfile::tempdir().unwrap();
        for (path, source) in files {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, source).unwrap();
        }
        let facts = adapters_lang_python::extract_dir(
            dir.path(),
            &adapters_lang_python::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let mut facts = facts;
        edit(&mut facts.nodes);
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let endpoints: Vec<&Node> = nodes
            .iter()
            .copied()
            .filter(|n| n.label == "Endpoint")
            .collect();
        let handles: Vec<&Edge> = edges
            .iter()
            .copied()
            .filter(|e| e.label == "HANDLES")
            .collect();
        let cites: Vec<BaseRef> = endpoints
            .iter()
            .map(|n| BaseRef::Node(n.id.clone()))
            .chain(
                handles
                    .iter()
                    .map(|e| BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone())),
            )
            .collect();
        let mut support: Vec<(&BaseRef, Support)> = Vec::new();
        for (cite, endpoint) in cites.iter().zip(&endpoints) {
            support.push((cite, Support::Node(endpoint)));
        }
        for (cite, edge) in cites[endpoints.len()..].iter().zip(&handles) {
            support.push((cite, Support::Edge(edge)));
        }
        let cap = Node {
            id: "capability:test:alias".into(),
            label: "Capability".into(),
            props: serde_json::json!({ "title": "Manage alias (API)" }),
        };
        let knowledge = Stories {
            included: vec![Story {
                node: &cap,
                support,
            }],
            filtered: 0,
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
        };
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut operations = Text::new(&budget, "").unwrap();
        operations_per_capability(
            (&nodes, &edges),
            &knowledge,
            mode,
            (&mut assertions, &mut asserted),
            &mut operations,
        )
        .unwrap();
        let mut limits = Text::new(&budget, "").unwrap();
        capability_operations(
            &knowledge.included[0],
            mode,
            (&mut assertions, &mut asserted),
            &mut limits,
        )
        .unwrap();
        capability_limits(
            &knowledge.included[0],
            mode,
            (&mut assertions, &mut asserted),
            &mut limits,
        )
        .unwrap();
        (operations.finish(), limits.finish())
    }

    const FILES: &[(&str, &str)] = &[
        ("app/__init__.py", ""),
        (
            "app/extensions.py",
            "from flask_limiter import Limiter\n\nlimiter = Limiter()\n",
        ),
        (
            "app/config.py",
            "import os\n\nALIAS_LIMIT = os.environ.get(\"ALIAS_LIMIT\") or \"100/day;5/minute\"\n",
        ),
        (
            "app/models.py",
            "from sqlalchemy import Column, Integer\nfrom sqlalchemy.orm import declarative_base\n\
             \nBase = declarative_base()\n\n\nclass Alias(Base):\n    __tablename__ = \"alias\"\n    id = Column(Integer, primary_key=True)\n",
        ),
        (
            "app/alias_utils.py",
            "from app.db import Session\nfrom app.models import Alias\n\n\ndef create(note):\n    alias = Alias()\n    Session.add(alias)\n    return alias\n",
        ),
        (
            "app/db.py",
            "from sqlalchemy.orm import scoped_session, sessionmaker\n\nSession = scoped_session(sessionmaker())\n",
        ),
        (
            "app/views.py",
            r#"from flask import Flask, jsonify, request
from app import alias_utils
from app.config import ALIAS_LIMIT
from app.extensions import limiter

app = Flask(__name__)


@app.route("/api/alias/new", methods=["POST"])
@limiter.limit(ALIAS_LIMIT)
def new_alias():
    data = request.get_json()
    note = data.get("note")
    if not note:
        return jsonify(error="note"), 400
    alias_utils.create(note)
    return jsonify(alias="a"), 201


@app.route("/api/ping")
def ping():
    return "pong"
"#,
        ),
    ];

    // AC-0882: an operation states what it takes, its success statuses and
    // the entities it writes through the functions it calls, and names each
    // class the graph holds nothing for.
    #[test]
    fn operations_state_inputs_success_and_writes_and_name_what_is_missing() {
        let (operations, _) = render(FILES, ExportMode::BestEffort);
        assert!(
            operations.contains("### Manage alias (API)"),
            "{operations}"
        );
        let row = |name: &str| {
            operations
                .lines()
                .find(|line| line.starts_with(&format!("| {name} |")))
                .unwrap_or_else(|| panic!("{name}: {operations}"))
                .to_owned()
        };
        let created = row("POST /api/alias/new");
        assert!(
            // AC-0902: the success status carries the JSON keys tied to it.
            created.starts_with(
                "| POST /api/alias/new | body note | 201 with JSON keys alias | inserts Alias | — | "
            ),
            "{created}"
        );
        let ping = row("GET /api/ping");
        assert!(
            ping.starts_with("| GET /api/ping | — | 200 | none traced | inputs, writes | "),
            "{ping}"
        );
    }

    // AC-0882: the workspace bound grows with the graph and refuses overflow.
    #[test]
    fn operations_workspace_is_checked() {
        let graph = |rows, bytes, values| flowtracer::TraceGraphBound {
            rows,
            bytes,
            values,
        };
        let small = operations_per_capability_workspace(graph(1 << 10, 16, 64)).unwrap();
        let large = operations_per_capability_workspace(graph(1 << 20, 1 << 10, 1 << 12)).unwrap();
        assert!(small < large);
        assert!(operations_per_capability_workspace(graph(usize::MAX, 1, 1)).is_none());
        assert!(operations_per_capability_workspace(graph(1, usize::MAX, 1)).is_none());
    }

    // AC-0882, AC-1608 (#2539): under its capability, §1.5 lists each
    // operation with its own Endpoint fact, and each rate limit as a rule
    // row citing that Endpoint, with its value read through configuration
    // and what refuses the requests past it.
    #[test]
    fn rate_limits_are_listed_per_capability_with_their_configuration_default() {
        let (_, limits) = render(FILES, ExportMode::BestEffort);
        assert!(
            limits.starts_with("Operations: GET /api/ping (F-"),
            "{limits}"
        );
        // The operations line and the limit row cite the same Endpoint fact.
        let listed = limits
            .split("POST /api/alias/new (")
            .nth(1)
            .and_then(|rest| rest.get(..18))
            .expect("the operation is listed with its id");
        assert!(
            limits.contains(&format!(
                "| POST /api/alias/new | POST /api/alias/new accepts at most 100 a day, 5 a minute (the \
                 default; the environment variable ALIAS\\_LIMIT replaces it); requests past it \
                 are refused with 429. | rate limit declared on the handler | {listed} |"
            )),
            "{limits}"
        );
        // An operation with no recorded limit has no limit row.
        assert_eq!(
            limits
                .matches(" | rate limit declared on the handler | ")
                .count(),
            1
        );
        // A graph recovered before the status was recorded still reads
        // flask-limiter's 429; a breach callback's response is named.
        let strip = |nodes: &mut Vec<Node>, on_breach: bool| {
            for node in nodes.iter_mut().filter(|node| node.label == "Endpoint") {
                let limits = &mut node.props["rate_limits"];
                if let Some(limits) = limits.as_object_mut() {
                    limits.remove("exceeded_status");
                    if on_breach {
                        limits["entries"][0]["on_breach"] = true.into();
                    }
                }
            }
        };
        let (_, older) = render_with(FILES, ExportMode::BestEffort, |nodes| strip(nodes, false));
        assert!(
            older.contains("requests past it are refused with 429."),
            "{older}"
        );
        let (_, breach) = render_with(FILES, ExportMode::BestEffort, |nodes| strip(nodes, true));
        assert!(
            breach.contains("requests past it get the response of a breach callback."),
            "{breach}"
        );
        // An environment default is InferredStrong, which verified-only keeps.
        let (_, verified) = render(FILES, ExportMode::VerifiedOnly);
        assert!(verified.contains("ALIAS\\_LIMIT"), "{verified}");
    }
}
