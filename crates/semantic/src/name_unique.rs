//! T0.5 exact-name uniqueness resolver (#257, AC-0270).
//!
//! A deterministic rule over the published graph, re-run after every load: a
//! call Gap whose callee text equals the name of exactly one callable Symbol
//! of the same language, counted across every repo in the graph, gains an
//! inferred `CALLS` edge from the Gap's caller to that Symbol. No embeddings
//! are consulted.
//!
//! The edge is Deterministic-tier (a named rule produced it) at
//! InferredStrong confidence (what it claims is heuristic). Uniqueness is a
//! fact about the graph, never a binding proof: globals beyond the denylist,
//! externals, shadowing and dynamic dispatch can still bind elsewhere. The
//! Gap, its own edge and its evidence are untouched, so coverage and the
//! register still read the hop as open. Fills are derived, never durable:
//! [`plan`] recomputes them from the current graph and [`live_fills`] honours
//! only a stored edge that equals what the plan derives now.

use crate::evidence_from;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Extractor id and rule name of every fill this resolver writes.
pub const EXTRACTOR_ID: &str = "t0_5.name-unique";
/// Rule version; a changed rule is a different producer.
pub const RULE_VERSION: &str = "1";

/// Why a call Gap was left for T2/T3. Every examined hop is either filled or
/// counted under exactly one reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// No uniqueness rule exists for the Gap's language yet.
    UnsupportedLanguage,
    /// The Gap carries no callee text or no evidence span.
    MissingCalleeOrEvidence,
    /// The callee is a builtin or global name the rule never resolves.
    DenylistedName,
    /// No Symbol of that language has the callee's name.
    NoCandidate,
    /// The only Symbol with the name is not a proven callable (a class, a
    /// plugin-supplied kind, or no kind at all).
    NotCallable,
    /// Two or more Symbols share the name, in any repo.
    Collision,
    /// The only candidate is the caller itself.
    SelfCall,
    /// Another producer already holds a `CALLS` edge between the same pair;
    /// it is never overwritten (R-INT-1).
    EdgeExists,
}

/// The fills one graph derives, and why every other call Gap stayed open.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    /// Inferred `CALLS` edges, sorted by (source, target).
    pub edges: Vec<Edge>,
    /// Call-Gap hops the edges cover.
    pub filled: u64,
    /// Hops left open, by reason.
    pub skipped: BTreeMap<SkipReason, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Language {
    EcmaScript,
}

/// Language with a uniqueness rule, from the first evidence span's path.
/// A TS/JS callee is the imported name (`isObservable`) or `Type.method`,
/// and the TS adapter names Symbols the same way, so exact equality is the
/// whole rule. Other adapters name callees and Symbols differently; they
/// stay unsupported until each gets a rule of its own.
fn language(evidence: &[EvidenceRef]) -> Option<Language> {
    let extension = evidence.first()?.path.rsplit_once('.')?.1;
    matches!(
        extension,
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs"
    )
    .then_some(Language::EcmaScript)
}

fn denylisted(language: Language, name: &str) -> bool {
    match language {
        Language::EcmaScript => matches!(
            name,
            "apply"
                | "bind"
                | "call"
                | "catch"
                | "constructor"
                | "default"
                | "finally"
                | "then"
                | "toString"
                | "valueOf"
        ),
    }
}

/// The Gaps one (source, target) fill covers, and the evidence it cites.
type Fill<'a> = (Vec<&'a str>, Vec<EvidenceRef>);

fn is_fill(edge: &Edge) -> bool {
    edge.label == "CALLS" && edge.props["prov"]["extractor_id"] == EXTRACTOR_ID
}

fn evidence_key(evidence: &EvidenceRef) -> (&str, &str, u64, u64, &str) {
    (
        &evidence.repo,
        &evidence.path,
        evidence.byte_start,
        evidence.byte_end,
        &evidence.commit_sha,
    )
}

/// Kinds the TS/JS extractor emits for declarations a call can reach.
fn callable(node: &Node) -> bool {
    node.label == "Component"
        || matches!(
            node.props["kind"].as_str(),
            Some("Function" | "Method" | "Component")
        )
}

/// Derive every fill from `nodes` and `edges`. Pure and order-independent:
/// the same graph yields byte-identical output however it was read.
pub fn plan(nodes: &[Node], edges: &[Edge]) -> Plan {
    let mut gaps = BTreeMap::new();
    let mut by_name: BTreeMap<(Language, &str), BTreeSet<&str>> = BTreeMap::new();
    let mut targets = BTreeMap::new();
    for node in nodes {
        if node.label == "Gap" {
            gaps.insert(node.id.as_str(), node);
            continue;
        }
        if !matches!(node.label.as_str(), "Symbol" | "Component")
            || node.props["computed_name"] == true
            || !node.props["name_capture"].is_null()
        {
            continue;
        }
        let Some(name) = node.props["name"].as_str() else {
            continue;
        };
        let Some(language) = language(&evidence_from(&node.props)) else {
            continue;
        };
        // Every named Symbol counts toward a collision; only a proven
        // callable can be the target, so `class Foo` beside `function Foo`
        // stays a Gap rather than resolving either way.
        by_name
            .entry((language, name))
            .or_default()
            .insert(node.id.as_str());
        if callable(node) {
            targets.insert(node.id.as_str(), node);
        }
    }
    let held: BTreeSet<(&str, &str)> = edges
        .iter()
        .filter(|edge| edge.label == "CALLS" && !is_fill(edge))
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect();
    let mut hops: Vec<(&Node, &str)> = edges
        .iter()
        .filter(|edge| edge.label == "CALLS")
        .filter_map(|edge| Some((*gaps.get(edge.dst.as_str())?, edge.src.as_str())))
        .collect();
    hops.sort_by(|a, b| (a.0.id.as_str(), a.1).cmp(&(b.0.id.as_str(), b.1)));
    hops.dedup_by(|a, b| a.0.id == b.0.id && a.1 == b.1);

    let mut plan = Plan::default();
    let mut fills: BTreeMap<(&str, &str), Fill<'_>> = BTreeMap::new();
    for (gap, source) in hops {
        let callee = gap.props["callee"].as_str().unwrap_or("").trim();
        let gap_evidence = evidence_from(&gap.props);
        let outcome = if callee.is_empty() || gap_evidence.is_empty() {
            Err(SkipReason::MissingCalleeOrEvidence)
        } else if let Some(language) = language(&gap_evidence) {
            if denylisted(language, callee) {
                Err(SkipReason::DenylistedName)
            } else {
                match by_name.get(&(language, callee)) {
                    None => Err(SkipReason::NoCandidate),
                    Some(ids) if ids.len() > 1 => Err(SkipReason::Collision),
                    Some(ids) => {
                        let target = *ids.first().expect("a name index entry is never empty");
                        if !targets.contains_key(target) {
                            Err(SkipReason::NotCallable)
                        } else if target == source {
                            Err(SkipReason::SelfCall)
                        } else if held.contains(&(source, target)) {
                            Err(SkipReason::EdgeExists)
                        } else {
                            Ok(target)
                        }
                    }
                }
            }
        } else {
            Err(SkipReason::UnsupportedLanguage)
        };
        match outcome {
            Ok(target) => {
                let fill = fills.entry((source, target)).or_default();
                fill.0.push(gap.id.as_str());
                fill.1.extend(gap_evidence);
                fill.1.extend(evidence_from(&targets[target].props));
                plan.filled += 1;
            }
            Err(reason) => *plan.skipped.entry(reason).or_default() += 1,
        }
    }
    for ((source, target), (gap_ids, mut evidence)) in fills {
        evidence.sort_by(|a, b| evidence_key(a).cmp(&evidence_key(b)));
        evidence.dedup();
        let fact = format!(
            "CALLS {source} -> {target} by {EXTRACTOR_ID}@{RULE_VERSION} for {}",
            gap_ids.join(",")
        );
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::InferredStrong,
            evidence,
            EXTRACTOR_ID,
            fact.as_bytes(),
        )
        .expect("InferredStrong is within the Deterministic ceiling")
        .with_method(ProducingMethod::Rule {
            rule: EXTRACTOR_ID.into(),
            version: RULE_VERSION.into(),
        });
        plan.edges.push(Edge {
            src: source.into(),
            dst: target.into(),
            label: "CALLS".into(),
            props: serde_json::json!({
                "resolution": "name-unique",
                "resolves_gaps": gap_ids,
                "prov": prov,
            }),
        });
    }
    plan
}

/// `(gap id, source id)` hops a live fill covers. A stored fill counts only
/// when it equals the edge [`plan`] derives from this same graph now, so a
/// forged, stale, re-sourced or re-evidenced edge leaves its Gap open.
pub fn live_fills(nodes: &[Node], edges: &[Edge]) -> BTreeSet<(String, String)> {
    let stored: BTreeMap<(&str, &str), &Edge> = edges
        .iter()
        .filter(|edge| is_fill(edge))
        .map(|edge| ((edge.src.as_str(), edge.dst.as_str()), edge))
        .collect();
    let mut live = BTreeSet::new();
    for edge in plan(nodes, edges).edges {
        if stored
            .get(&(edge.src.as_str(), edge.dst.as_str()))
            .is_some_and(|held| held.props == edge.props)
        {
            for gap in edge.props["resolves_gaps"].as_array().into_iter().flatten() {
                if let Some(gap) = gap.as_str() {
                    live.insert((gap.to_string(), edge.src.clone()));
                }
            }
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_inputs;

    fn prov(path: &str, at: u64) -> serde_json::Value {
        serde_json::json!({
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [{"repo": "app", "path": path, "byte_start": at, "byte_end": at + 4, "commit_sha": "c"}],
            "extractor_id": "t0.adapter-ts",
            "content_hash": "h",
        })
    }

    fn symbol(id: &str, name: &str, path: &str) -> Node {
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props: serde_json::json!({"name": name, "kind": "Function", "prov": prov(path, 0)}),
        }
    }

    fn gap(id: &str, callee: &str, path: &str, at: u64) -> Node {
        Node {
            id: id.into(),
            label: "Gap".into(),
            props: serde_json::json!({"callee": callee, "reason": "unresolved", "prov": prov(path, at)}),
        }
    }

    fn calls(src: &str, dst: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": prov("src/a.ts", 1)}),
        }
    }

    /// Two unique names, one same-repo collision and one cross-repo
    /// collision: exactly the two unique ones fill.
    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        let nodes = vec![
            symbol("sym:app@src/a.ts#main", "main", "src/a.ts"),
            symbol(
                "sym:app@src/obs.ts#isObservable",
                "isObservable",
                "src/obs.ts",
            ),
            symbol("sym:app@src/s.ts#Store.get", "Store.get", "src/s.ts"),
            symbol("sym:app@src/x.ts#join", "join", "src/x.ts"),
            symbol("sym:app@src/y.ts#join", "join", "src/y.ts"),
            symbol("sym:lib@src/f.ts#format", "format", "src/f.ts"),
            symbol("sym:app@src/g.ts#format", "format", "src/g.ts"),
            gap("gap:call:app@src/a.ts@10", "isObservable", "src/a.ts", 10),
            gap("gap:call:app@src/a.ts@20", "Store.get", "src/a.ts", 20),
            gap("gap:call:app@src/a.ts@30", "join", "src/a.ts", 30),
            gap("gap:call:app@src/a.ts@40", "format", "src/a.ts", 40),
        ];
        let edges = ["10", "20", "30", "40"]
            .iter()
            .map(|at| {
                calls(
                    "sym:app@src/a.ts#main",
                    &format!("gap:call:app@src/a.ts@{at}"),
                )
            })
            .collect();
        (nodes, edges)
    }

    #[test]
    fn unique_names_fill_and_collisions_stay_gaps() {
        let (nodes, edges) = fixture();
        let plan = plan(&nodes, &edges);
        let targets: Vec<&str> = plan.edges.iter().map(|edge| edge.dst.as_str()).collect();
        assert_eq!(
            targets,
            [
                "sym:app@src/obs.ts#isObservable",
                "sym:app@src/s.ts#Store.get"
            ]
        );
        assert_eq!(plan.filled, 2);
        assert_eq!(plan.skipped, BTreeMap::from([(SkipReason::Collision, 2)]));
        assert!(
            plan.edges
                .iter()
                .all(|edge| edge.src == "sym:app@src/a.ts#main")
        );
    }

    #[test]
    fn a_fill_is_an_inferred_claim_from_a_named_rule_citing_both_spans() {
        let (nodes, edges) = fixture();
        let edge = plan(&nodes, &edges).edges.remove(0);
        let prov: Provenance = serde_json::from_value(edge.props["prov"].clone()).unwrap();
        assert_eq!(prov.tier, Tier::Deterministic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::InferredStrong);
        assert_eq!(prov.extractor_id, EXTRACTOR_ID);
        assert_eq!(
            prov.method,
            ProducingMethod::Rule {
                rule: EXTRACTOR_ID.into(),
                version: RULE_VERSION.into()
            }
        );
        let spans: Vec<(&str, u64)> = prov
            .evidence
            .iter()
            .map(|span| (span.path.as_str(), span.byte_start))
            .collect();
        assert_eq!(spans, [("src/a.ts", 10), ("src/obs.ts", 0)]);
        assert_eq!(
            edge.props["resolves_gaps"],
            serde_json::json!(["gap:call:app@src/a.ts@10"])
        );
    }

    #[test]
    fn every_open_hop_is_counted_under_one_reason() {
        let mut nodes = vec![
            symbol("sym:app@src/a.ts#main", "main", "src/a.ts"),
            symbol("sym:app@src/b.ts#held", "held", "src/b.ts"),
            symbol("sym:app@src/c.py#util", "util", "src/c.py"),
            symbol("sym:app@src/d.ts#then", "then", "src/d.ts"),
            gap("gap:py", "util", "src/c.py", 1),
            gap("gap:none", "", "src/a.ts", 2),
            gap("gap:deny", "then", "src/a.ts", 3),
            gap("gap:missing", "absent", "src/a.ts", 4),
            gap("gap:self", "main", "src/a.ts", 5),
            gap("gap:held", "held", "src/a.ts", 6),
        ];
        let mut computed = symbol("sym:app@src/e.ts#computed", "computed", "src/e.ts");
        computed.props["computed_name"] = serde_json::json!(true);
        nodes.push(computed);
        nodes.push(gap("gap:computed", "computed", "src/a.ts", 7));
        let mut edges: Vec<Edge> = [
            "gap:py",
            "gap:none",
            "gap:deny",
            "gap:missing",
            "gap:self",
            "gap:held",
            "gap:computed",
        ]
        .iter()
        .map(|gap| calls("sym:app@src/a.ts#main", gap))
        .collect();
        edges.push(calls("sym:app@src/a.ts#main", "sym:app@src/b.ts#held"));
        let plan = plan(&nodes, &edges);
        assert!(plan.edges.is_empty());
        assert_eq!(
            plan.skipped,
            BTreeMap::from([
                (SkipReason::UnsupportedLanguage, 1),
                (SkipReason::MissingCalleeOrEvidence, 1),
                (SkipReason::DenylistedName, 1),
                (SkipReason::NoCandidate, 2),
                (SkipReason::SelfCall, 1),
                (SkipReason::EdgeExists, 1),
            ])
        );
    }

    #[test]
    fn the_plan_is_byte_identical_whatever_the_read_order() {
        let (nodes, edges) = fixture();
        let forward = serde_json::to_vec(&plan(&nodes, &edges)).unwrap();
        let mut reversed_nodes = nodes.clone();
        reversed_nodes.reverse();
        let mut reversed_edges = edges.clone();
        reversed_edges.reverse();
        assert_eq!(
            forward,
            serde_json::to_vec(&plan(&reversed_nodes, &reversed_edges)).unwrap()
        );
        assert_eq!(forward, serde_json::to_vec(&plan(&nodes, &edges)).unwrap());
    }

    #[test]
    fn several_call_sites_share_one_fill_that_cites_each() {
        let (mut nodes, mut edges) = fixture();
        nodes.push(gap(
            "gap:call:app@src/a.ts@50",
            "isObservable",
            "src/a.ts",
            50,
        ));
        edges.push(calls("sym:app@src/a.ts#main", "gap:call:app@src/a.ts@50"));
        let plan = plan(&nodes, &edges);
        assert_eq!(plan.filled, 3);
        assert_eq!(
            plan.edges[0].props["resolves_gaps"],
            serde_json::json!(["gap:call:app@src/a.ts@10", "gap:call:app@src/a.ts@50"])
        );
        assert_eq!(
            plan.edges[0].props["prov"]["evidence"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn only_callable_symbols_take_a_fill() {
        let mut class = symbol("sym:app@src/k.ts#Store", "Store", "src/k.ts");
        class.props["kind"] = serde_json::json!("Class");
        let mut unknown = symbol("sym:app@src/u.ts#plugged", "plugged", "src/u.ts");
        unknown.props.as_object_mut().unwrap().remove("kind");
        let mut shadow = symbol("sym:app@src/s.ts#Model", "Model", "src/s.ts");
        shadow.props["kind"] = serde_json::json!("Class");
        let nodes = vec![
            symbol("sym:app@src/a.ts#main", "main", "src/a.ts"),
            class,
            unknown,
            shadow,
            symbol("sym:app@src/m.ts#Model", "Model", "src/m.ts"),
            gap("gap:class", "Store", "src/a.ts", 1),
            gap("gap:unknown", "plugged", "src/a.ts", 2),
            gap("gap:shadow", "Model", "src/a.ts", 3),
        ];
        let edges: Vec<Edge> = ["gap:class", "gap:unknown", "gap:shadow"]
            .iter()
            .map(|gap| calls("sym:app@src/a.ts#main", gap))
            .collect();
        let plan = plan(&nodes, &edges);
        assert!(plan.edges.is_empty());
        assert_eq!(
            plan.skipped,
            BTreeMap::from([(SkipReason::NotCallable, 2), (SkipReason::Collision, 1)])
        );
        // No fill exists, so every hop stays T2 work.
        assert_eq!(call_hops(&nodes, &edges).len(), 3);
    }

    fn call_hops(nodes: &[Node], edges: &[Edge]) -> Vec<String> {
        graph_inputs(nodes, edges)
            .0
            .into_iter()
            .map(|hop| hop.gap_id)
            .collect()
    }

    #[test]
    fn only_a_live_current_fill_takes_its_hop_out_of_t2() {
        let (nodes, mut edges) = fixture();
        let fills = plan(&nodes, &edges).edges;
        assert_eq!(call_hops(&nodes, &edges).len(), 4);
        edges.extend(fills.clone());
        assert_eq!(
            call_hops(&nodes, &edges),
            ["gap:call:app@src/a.ts@30", "gap:call:app@src/a.ts@40"]
        );

        let reopened = |nodes: &[Node], edges: &[Edge]| {
            call_hops(nodes, edges).contains(&"gap:call:app@src/a.ts@10".to_string())
        };
        // Forged: right method and pair, different evidence.
        let mut forged = edges.clone();
        let fill = forged
            .iter_mut()
            .find(|edge| edge.dst.ends_with("#isObservable"))
            .unwrap();
        fill.props["prov"]["evidence"][0]["byte_start"] = serde_json::json!(99);
        assert!(reopened(&nodes, &forged));
        // Forged: a bare `resolves_gaps` claim on an edge with no fill provenance.
        let mut claimed = edges.clone();
        claimed.retain(|edge| !is_fill(edge));
        claimed.push(Edge {
            props: serde_json::json!({"resolves_gaps": ["gap:call:app@src/a.ts@10"], "prov": prov("src/a.ts", 10)}),
            ..calls("sym:app@src/a.ts#main", "sym:app@src/obs.ts#isObservable")
        });
        assert!(reopened(&nodes, &claimed));
        // Target removed.
        let without_target: Vec<Node> = nodes
            .iter()
            .filter(|node| !node.id.ends_with("#isObservable"))
            .cloned()
            .collect();
        assert!(reopened(&without_target, &edges));
        // The fill comes from a different source than the Gap's caller.
        let mut moved = edges.clone();
        for edge in moved.iter_mut().filter(|edge| is_fill(edge)) {
            edge.src = "sym:app@src/x.ts#join".into();
        }
        assert!(reopened(&nodes, &moved));
        // A second same-name Symbol arrives: the stored fill is stale.
        let mut colliding = nodes.clone();
        colliding.push(symbol(
            "sym:other@src/o.ts#isObservable",
            "isObservable",
            "src/o.ts",
        ));
        assert!(reopened(&colliding, &edges));
    }
}
