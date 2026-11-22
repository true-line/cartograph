// AC-0501: hybrid graph-first retrieval (#1442).
use super::*;
use llm::{Embedding, EmbeddingIdentity, Locality, ProviderCaps};
use std::sync::Mutex;

/// Pinned vectors: billing-like text points one way, everything else the
/// other. Records every text it is sent.
struct Pinned {
    sent: Mutex<Vec<String>>,
    model: &'static str,
    broken: bool,
    cloud: bool,
    failing: bool,
}
impl Pinned {
    fn new() -> Self {
        Self {
            sent: Mutex::new(vec![]),
            model: "pinned",
            broken: false,
            cloud: false,
            failing: false,
        }
    }
}
impl LlmProvider for Pinned {
    fn id(&self) -> &str {
        "pinned"
    }
    fn locality(&self) -> Locality {
        if self.cloud {
            Locality::Cloud
        } else {
            Locality::Local
        }
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: false,
            tool_use: false,
        }
    }
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        Ok(None)
    }
    fn embedding_model_id(&self) -> String {
        self.model.into()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.sent.lock().unwrap().extend_from_slice(texts);
        if self.failing {
            return Err(ProviderError::InvalidUrl("failing".into()));
        }
        Ok(texts
            .iter()
            .map(
                |t| match (self.broken, t.contains("billing") || t.contains("invoice")) {
                    (true, _) => vec![f32::NAN, 1.0],
                    (false, true) => vec![1.0, 0.0],
                    (false, false) => vec![0.0, 1.0],
                },
            )
            .collect())
    }
}

fn node(id: &str, label: &str, props: serde_json::Value) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn edge(src: &str, dst: &str, label: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: serde_json::json!({}),
    }
}

fn base() -> (Vec<Node>, Vec<Edge>) {
    (
        vec![
            node("sym:pay", "Symbol", serde_json::json!({"name": "pay"})),
            node(
                "sym:noise",
                "Symbol",
                serde_json::json!({"name": "pay_pay_helper", "doc": "pay pay pay pay pay"}),
            ),
            node(
                "ep:pay",
                "Endpoint",
                serde_json::json!({"method": "POST", "path": "/pay"}),
            ),
            node(
                "sym:charge",
                "Symbol",
                serde_json::json!({"name": "charge", "doc": "charge the card"}),
            ),
            node(
                "sym:settle",
                "Symbol",
                serde_json::json!({"name": "settle", "doc": "invoice settlement"}),
            ),
            node("gap:x", "Gap", serde_json::json!({"name": "unindexed"})),
        ],
        vec![
            edge("ep:pay", "sym:pay", "HANDLES"),
            edge("sym:pay", "sym:charge", "CALLS"),
        ],
    )
}

fn ids(hits: &[Hit]) -> Vec<(&str, Reason)> {
    hits.iter()
        .map(|h| (h.node_id.as_str(), h.reason))
        .collect()
}

#[test]
fn an_exact_name_ranks_its_anchor_first() {
    let (nodes, edges) = base();
    let index = Retrieval::build(&nodes, &edges).unwrap();
    let hits = index.search("pay", None, 10).unwrap();
    assert_eq!(
        ids(&hits)[..3],
        [
            ("sym:pay", Reason::Anchor),
            ("ep:pay", Reason::Neighbour),
            ("sym:charge", Reason::Neighbour),
        ]
    );
    assert_eq!(
        hits[1].cites,
        vec![
            BaseRef::Node("ep:pay".into()),
            BaseRef::Edge("ep:pay".into(), "sym:pay".into(), "HANDLES".into()),
        ]
    );
    // The heavily repeated lexical match still ranks after the anchor.
    assert!(ids(&hits)[3..].contains(&("sym:noise", Reason::Ranked)));
    for query in ["POST /pay", "/pay", "ep:pay"] {
        let hits = index.search(query, None, 10).unwrap();
        assert_eq!(ids(&hits)[0], ("ep:pay", Reason::Anchor), "{query}");
    }
    assert!(hits.iter().all(|h| h.tier == Tier::Semantic));
}

#[test]
fn a_lexical_query_works_without_a_provider() {
    let (nodes, edges) = base();
    let before = (nodes.clone(), edges.clone());
    let index = Retrieval::build(&nodes, &edges).unwrap();
    let hits = index.search("card charge", None, 10).unwrap();
    assert_eq!(ids(&hits), [("sym:charge", Reason::Ranked)]);
    assert!(hits[0].score > 0.0);
    assert_eq!(hits[0].cites, vec![BaseRef::Node("sym:charge".into())]);
    // Text rank orders by relevance: the unit matching both terms, many
    // times, precedes the single-term matches.
    let ranked = index.search("helper pay", None, 10).unwrap();
    assert_eq!(ids(&ranked)[0], ("sym:noise", Reason::Ranked));
    assert!(ranked.len() >= 3);
    // Labels outside the unit recipe still anchor by name but are not
    // ranked by text; the base is unchanged.
    let gap = index.search("unindexed", None, 10).unwrap();
    assert_eq!(ids(&gap), [("gap:x", Reason::Anchor)]);
    assert!(index.search("unindexed gap", None, 10).unwrap().is_empty());
    assert_eq!((nodes, edges), before);
}

#[test]
fn pinned_vectors_fuse_with_text_deterministically() {
    let (nodes, edges) = base();
    let provider = Pinned::new();
    let run = || {
        let mut index = Retrieval::build(&nodes, &edges).unwrap();
        index.embed_units(&provider).unwrap();
        index.search("billing", Some(&provider), 10).unwrap()
    };
    let first = run();
    // No unit contains "billing"; only the vector leg finds the invoice.
    assert_eq!(ids(&first)[0], ("sym:settle", Reason::Ranked));
    assert_eq!(first, run());
    // Fusion: a unit found by both legs outranks one found by either alone.
    let mut index = Retrieval::build(&nodes, &edges).unwrap();
    index.embed_units(&provider).unwrap();
    let both = index.search("invoice charge", Some(&provider), 10).unwrap();
    assert_eq!(ids(&both)[0], ("sym:settle", Reason::Ranked));
    // Without a provider at query time the lexical leg alone answers.
    assert!(index.search("billing", None, 10).unwrap().is_empty());
}

#[test]
fn queries_are_bounded_redacted_and_never_fts_syntax() {
    let (nodes, edges) = base();
    let provider = Pinned::new();
    let mut index = Retrieval::build(&nodes, &edges).unwrap();
    index.embed_units(&provider).unwrap();
    assert!(matches!(
        index.search(&"a".repeat(MAX_QUERY_BYTES + 1), None, 10),
        Err(RetrievalError::Query(_))
    ));
    let secret = format!("AKIA{}", "ABCDEFGHIJKLMNOP");
    index
        .search(&format!("charge {secret}"), Some(&provider), 10)
        .unwrap();
    assert!(
        provider
            .sent
            .lock()
            .unwrap()
            .iter()
            .all(|t| !t.contains(&secret))
    );
    for query in ["\"", "charge*", "NEAR(charge", "charge OR", "-", ""] {
        index.search(query, None, 10).unwrap();
    }
    assert_eq!(index.search("pay", None, 2).unwrap().len(), 2);
    assert!(index.search("pay", None, 0).unwrap().is_empty());
}

#[test]
fn invalid_unit_vectors_are_refused() {
    let (nodes, edges) = base();
    let mut provider = Pinned::new();
    provider.broken = true;
    let mut index = Retrieval::build(&nodes, &edges).unwrap();
    assert!(matches!(
        index.embed_units(&provider),
        Err(RetrievalError::Query("invalid unit vector"))
    ));
    // The index still answers lexically.
    assert_eq!(index.search("settle", Some(&provider), 5).unwrap().len(), 1);
}

#[test]
fn only_a_local_provider_receives_unit_or_query_text() {
    let (nodes, edges) = base();
    let mut cloud = Pinned::new();
    cloud.cloud = true;
    let mut index = Retrieval::build(&nodes, &edges).unwrap();
    assert!(matches!(
        index.embed_units(&cloud),
        Err(RetrievalError::Query("provider is not local"))
    ));
    index.embed_units(&Pinned::new()).unwrap();
    assert!(matches!(
        index.search("billing", Some(&cloud), 10),
        Err(RetrievalError::Query("provider is not local"))
    ));
    assert!(cloud.sent.lock().unwrap().is_empty());
}

#[test]
fn graph_hits_that_fill_the_limit_skip_ranking_and_the_provider() {
    let (nodes, edges) = base();
    let mut failing = Pinned::new();
    let mut index = Retrieval::build(&nodes, &edges).unwrap();
    index.embed_units(&failing).unwrap();
    failing.sent.lock().unwrap().clear();
    failing.failing = true;
    assert!(index.search("pay", Some(&failing), 0).unwrap().is_empty());
    assert_eq!(
        ids(&index.search("pay", Some(&failing), 1).unwrap()),
        [("sym:pay", Reason::Anchor)]
    );
    assert_eq!(
        ids(&index.search("pay", Some(&failing), 3).unwrap()).len(),
        3
    );
    assert!(failing.sent.lock().unwrap().is_empty());
    // Past the graph hits, ranking runs and the provider error surfaces.
    assert!(matches!(
        index.search("pay", Some(&failing), 4),
        Err(RetrievalError::Provider(_))
    ));
}

#[test]
fn queries_embed_only_in_the_indexed_vector_space() {
    let (nodes, edges) = base();
    let mut index = Retrieval::build(&nodes, &edges).unwrap();
    index.embed_units(&Pinned::new()).unwrap();
    let mut other = Pinned::new();
    other.model = "other";
    assert!(matches!(
        index.search("billing", Some(&other), 10),
        Err(RetrievalError::Query(
            "provider is not the indexed embedding model"
        ))
    ));
    assert!(other.sent.lock().unwrap().is_empty());
}

#[test]
fn equal_text_scores_break_ties_by_node_id_before_the_row_limit() {
    // Far more equal-scoring units than any row cut: unit order puts the
    // larger ids (Endpoints) first, so the smallest ids lie past every
    // cut-off taken in unit order, yet they are the ones kept.
    let total = MAX_HITS * 5 + 50;
    let nodes: Vec<Node> = (0..total)
        .map(|i| {
            node(
                &format!("sym:{i:04}"),
                if i < MAX_HITS { "Symbol" } else { "Endpoint" },
                serde_json::json!({"name": format!("n{i}"), "doc": "same"}),
            )
        })
        .collect();
    let index = Retrieval::build(&nodes, &[]).unwrap();
    let hits = index.search("same", None, MAX_HITS).unwrap();
    let expected: Vec<String> = (0..MAX_HITS).map(|i| format!("sym:{i:04}")).collect();
    let got: Vec<&str> = hits.iter().map(|h| h.node_id.as_str()).collect();
    assert_eq!(got, expected);
}

/// Vectors spread over a quarter circle by the number in the unit text; a
/// query gets the direction its own number names.
struct Spread;
impl Spread {
    fn angle(text: &str) -> f32 {
        let digits: String = text
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse::<f32>().unwrap_or(0.0) / 10_000.0 * std::f32::consts::FRAC_PI_2
    }
}
impl LlmProvider for Spread {
    fn id(&self) -> &str {
        "spread"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: false,
            tool_use: false,
        }
    }
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        Ok(None)
    }
    fn embedding_model_id(&self) -> String {
        "spread".into()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Ok(texts
            .iter()
            .map(|text| {
                let angle = Self::angle(text);
                vec![angle.cos(), angle.sin()]
            })
            .collect())
    }
}

// AC-0734: the vector index is rebuilt from the unit vectors, proposes
// candidates only, and leaves a small base's answer to the full scan.
#[test]
fn the_vector_index_is_rebuilt_with_the_units_and_only_proposes_candidates() {
    let (nodes, edges) = base();
    let mut small = Retrieval::build(&nodes, &edges).unwrap();
    assert_eq!(small.indexed_vectors(), None);
    small.embed_units(&Pinned::new()).unwrap();
    assert_eq!(small.indexed_vectors(), Some(small.units.len()));
    // Rebuilt, not appended to, when the units are embedded again.
    small.embed_units(&Pinned::new()).unwrap();
    assert_eq!(small.indexed_vectors(), Some(small.units.len()));
    // At or under the candidate count every vector is scored: dropping the
    // index changes nothing.
    let with_index = small.search("billing", Some(&Pinned::new()), 10).unwrap();
    small.vector_index = None;
    let without = small.search("billing", Some(&Pinned::new()), 10).unwrap();
    assert_eq!(with_index, without);

    // Past the candidate count the index proposes rows and the stored
    // vectors score them: the answer is the full scan's, in its order, and
    // the same on a second build. Neighbouring units differ by less than an
    // f32 can tell apart, so the nearest few tie and order by node id.
    let total = VECTOR_CANDIDATES * 4;
    let nodes: Vec<Node> = (0..total)
        .map(|i| {
            node(
                &format!("sym:{i:05}"),
                "Symbol",
                serde_json::json!({"name": format!("unit{i}")}),
            )
        })
        .collect();
    let mut large = Retrieval::build(&nodes, &[]).unwrap();
    large.embed_units(&Spread).unwrap();
    assert_eq!(large.indexed_vectors(), Some(total));
    // "q 1600" matches no unit text, so only the vector leg ranks.
    let hits = large.search("q 1600", Some(&Spread), 5).unwrap();
    assert_eq!(hits.len(), 5);
    for hit in &hits {
        let row: usize = hit.node_id["sym:".len()..].parse().unwrap();
        assert!(
            row.abs_diff(1600) <= 4,
            "{} is not near the query",
            hit.node_id
        );
    }
    let mut again = Retrieval::build(&nodes, &[]).unwrap();
    again.embed_units(&Spread).unwrap();
    assert_eq!(again.search("q 1600", Some(&Spread), 5).unwrap(), hits);
    // The index is a cache: without it the full scan gives the same answer.
    large.vector_index = None;
    assert_eq!(large.search("q 1600", Some(&Spread), 5).unwrap(), hits);
}

// AC-0501: a derived flow is a ranked unit whose hit cites the base facts
// the flow was traced from, by text and by vector.
#[test]
fn a_derived_flow_is_ranked_and_cites_its_base_facts() {
    let (nodes, edges) = base();
    let cites: BTreeSet<BaseRef> = [
        BaseRef::Node("ep:pay".into()),
        BaseRef::Node("sym:pay".into()),
        BaseRef::Edge("ep:pay".into(), "sym:pay".into(), "HANDLES".into()),
    ]
    .into_iter()
    .collect();
    let flow = |id: &str, hop: &str| OverlayFact {
        item: OverlayItem::Node(node(
            id,
            "BusinessFlow",
            serde_json::json!({
                "name": "Traced flow from POST /pay",
                "trace": {"trigger_kind": "Endpoint", "trigger_name": "POST /pay", "status": "Verified",
                    "hops": [{"label": "HANDLES", "src_name": "POST /pay", "dst_name": hop}]},
            }),
        )),
        cites: cites.clone(),
    };
    let derived = vec![
        flow("business-flow:a", "refund ledger"),
        // Not a flow, and a flow that takes a base node's id: neither is a
        // flow unit.
        OverlayFact {
            item: OverlayItem::Node(node(
                "capability:x",
                "Capability",
                serde_json::json!({"title": "refund ledger"}),
            )),
            cites: cites.clone(),
        },
        flow("sym:charge", "refund ledger"),
    ];
    let mut index = Retrieval::build_with_flows(&nodes, &edges, &derived).unwrap();
    let hits = index.search("refund ledger", None, 10).unwrap();
    assert_eq!(ids(&hits), [("business-flow:a", Reason::Ranked)]);
    assert_eq!(hits[0].cites, cites.iter().cloned().collect::<Vec<_>>());
    assert_eq!(hits[0].tier, Tier::Semantic);
    // A flow is never an anchor: its name ranks it, and the base node of
    // the same id keeps its own citation.
    let named = index
        .search("Traced flow from POST /pay", None, 10)
        .unwrap();
    assert!(named.iter().all(|h| h.reason == Reason::Ranked));
    let charge = index.search("card charge", None, 10).unwrap();
    assert_eq!(charge[0].cites, vec![BaseRef::Node("sym:charge".into())]);
    // The vector leg holds the flow as well.
    index.embed_units(&Pinned::new()).unwrap();
    assert_eq!(index.indexed_vectors(), Some(6));
    // Without derived facts the index is the base index.
    let plain = Retrieval::build(&nodes, &edges).unwrap();
    assert!(plain.search("refund ledger", None, 10).unwrap().is_empty());
}

// AC-1516: a commit the phase embedded is a ranked hit by its message and
// cites the base files it touched, never one the base lacks.
#[test]
fn a_commit_is_ranked_by_its_message_and_cites_the_files_it_touched() {
    let nodes = vec![
        Node {
            id: "file:r@src/refunds.ts".into(),
            label: "File".into(),
            props: serde_json::json!({"path": "src/refunds.ts"}),
        },
        Node {
            id: "mod:m".into(),
            label: "Module".into(),
            props: serde_json::json!({"name": "m"}),
        },
    ];
    let commits = [knowledge_ingest::CommitText {
        repo: "r".into(),
        sha: "b2".into(),
        message: "Settle refunds nightly".into(),
        paths: vec!["src/gone.ts".into(), "src/refunds.ts".into()],
    }];
    let index = Retrieval::build_with_history(
        &nodes,
        &[],
        &[],
        &knowledge_ingest::SectionText::new(),
        &commits,
    )
    .unwrap();
    let hits = index.search("nightly", None, 10).unwrap();
    assert_eq!(ids(&hits), [("commit:r@b2", Reason::Ranked)]);
    assert_eq!(
        hits[0].cites,
        vec![BaseRef::Node("file:r@src/refunds.ts".into())]
    );
}
