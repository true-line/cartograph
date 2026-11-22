// AC-0499: knowledge-layer embedding phase (#1441).
use super::*;
use llm::{Embedding, EmbeddingIdentity, Locality, ProviderCaps};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Recording {
    embeddings: bool,
    calls: AtomicUsize,
    sent: Mutex<Vec<String>>,
}
impl Recording {
    fn new(embeddings: bool) -> Self {
        Self {
            embeddings,
            calls: AtomicUsize::new(0),
            sent: Mutex::new(vec![]),
        }
    }
}
impl LlmProvider for Recording {
    fn id(&self) -> &str {
        "recording"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: self.embeddings,
            chat: false,
            tool_use: false,
        }
    }
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        Ok(Some(EmbeddingIdentity {
            endpoint: "http://127.0.0.1:11434/".into(),
            model: "fixture".into(),
            digest: format!("sha256:{:064x}", 1),
            recipe: "fixture@1".into(),
        }))
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sent.lock().unwrap().extend_from_slice(texts);
        Ok(texts.iter().map(|t| vec![1.0, t.len() as f32]).collect())
    }
}

fn node(id: &str, label: &str, props: serde_json::Value) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn base() -> Vec<Node> {
    vec![
        node(
            "sym:b",
            "Symbol",
            serde_json::json!({"name": "pay", "kind": "function", "arity": 2, "prov": {"tier": "T0"}}),
        ),
        node(
            "zz:ep",
            "Endpoint",
            serde_json::json!({"method": "POST", "path": "/pay", "tags": ["x"]}),
        ),
        node("gap:z", "Gap", serde_json::json!({"name": "unresolved"})),
        node(
            "sym:a",
            "Symbol",
            serde_json::json!({"name": "charge", "pure": true}),
        ),
    ]
}

fn cache() -> (tempfile::TempDir, Arc<Mutex<EmbeddingCache>>) {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("cache.db")).unwrap(),
    ));
    cache
        .lock()
        .unwrap()
        .bind_instance(&"a".repeat(32))
        .unwrap();
    (dir, cache)
}

fn run_with(
    nodes: &[Node],
    provider: Option<&dyn LlmProvider>,
    cache: &Arc<Mutex<EmbeddingCache>>,
) -> PhaseOutcome {
    let lease = cache.lock().unwrap().lease().unwrap();
    run(
        nodes,
        &[],
        &SectionText::new(),
        provider,
        Arc::clone(cache),
        lease,
    )
    .unwrap()
}

#[test]
fn units_follow_the_recipe_order_and_render_scalars_only() {
    let (units, omitted) = units(&base());
    assert_eq!(omitted, 0);
    let ids: Vec<_> = units.iter().map(|u| u.node_id.as_str()).collect();
    assert_eq!(ids, ["zz:ep", "sym:a", "sym:b"]);
    assert_eq!(units[0].text, "Endpoint\nmethod: POST\npath: /pay");
    assert_eq!(units[1].text, "Symbol\nname: charge\npure: true");
    assert_eq!(units[2].text, "Symbol\narity: 2\nkind: function\nname: pay");
    assert_eq!(units[2].key().len(), 64);
}

// AC-1209 (T-1209): a base whose `prov` is held as its text renders the
// units, keys and all, that the stored base renders.
#[test]
fn a_compacted_prov_renders_the_units_of_the_stored_base() {
    let stored = base();
    let mut held = base();
    for node in &mut held {
        if let Some(prov) = node.props.get_mut("prov") {
            core_graph::stored::compact_borrowable(prov);
        }
    }
    assert!(core_graph::stored::compact_text(&held[0].props["prov"]).is_some());
    let text = |nodes: &[Node]| {
        units(nodes)
            .0
            .iter()
            .map(|unit| (unit.text.clone(), unit.key()))
            .collect::<Vec<_>>()
    };
    assert_eq!(text(&held), text(&stored));
}

#[test]
fn units_past_the_cap_are_counted_and_long_text_is_bounded() {
    let mut nodes: Vec<_> = (0..MAX_UNITS + 2)
        .map(|i| node(&format!("sym:{i:06}"), "Symbol", serde_json::json!({})))
        .collect();
    nodes.push(node(
        "zz:late",
        "Endpoint",
        serde_json::json!({"path": "é".repeat(MAX_UNIT_BYTES)}),
    ));
    nodes.push(node(
        "sym:000000",
        "Symbol",
        serde_json::json!({"dup": true}),
    ));
    let (units, omitted) = units(&nodes);
    assert_eq!(units.len(), MAX_UNITS);
    assert_eq!(omitted, 3);
    assert_eq!(units[0].node_id, "zz:late");
    assert!(units[0].text.len() <= MAX_UNIT_BYTES);
    assert!(units[0].text.len() > MAX_UNIT_BYTES - 2);
    assert_eq!(units[1].node_id, "sym:000000");
    assert_eq!(
        units.last().unwrap().node_id,
        format!("sym:{:06}", MAX_UNITS - 2)
    );
}

#[test]
fn no_provider_is_a_named_omission_with_the_base_unchanged() {
    let (_dir, cache) = cache();
    let nodes = base();
    let before = nodes.clone();
    let without_embeddings = Recording::new(false);
    for provider in [None, Some(&without_embeddings as &dyn LlmProvider)] {
        let outcome = run_with(&nodes, provider, &cache);
        assert!(matches!(
            outcome,
            PhaseOutcome::Skipped {
                omission: NO_PROVIDER
            }
        ));
    }
    assert_eq!(nodes, before);
    assert_eq!(without_embeddings.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unchanged_units_reuse_cached_vectors_without_provider_calls() {
    let (_dir, cache) = cache();
    let provider = Recording::new(true);
    let first = run_with(&base(), Some(&provider), &cache);
    let PhaseOutcome::Embedded {
        binding,
        units,
        omitted,
        stats,
    } = first
    else {
        panic!("phase skipped");
    };
    assert_eq!(omitted, 0);
    assert_eq!(stats.unique_misses, 3);
    assert_eq!(binding.as_ref().unwrap().dimensions(), 2);
    let expected: Vec<_> = super::units(&base())
        .0
        .iter()
        .map(|unit| (unit.node_id.clone(), unit.key()))
        .collect();
    assert_eq!(units, expected);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let again = run_with(&base(), Some(&provider), &cache);
    let PhaseOutcome::Embedded {
        binding: second,
        stats,
        units: same,
        ..
    } = again
    else {
        panic!("phase skipped");
    };
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.calls, stats.unique_misses, stats.hits), (0, 0, 3));
    assert_eq!(second, binding);
    assert_eq!(same, units);

    let mut changed = base();
    changed[0].props["name"] = "refund".into();
    let PhaseOutcome::Embedded { stats, .. } = run_with(&changed, Some(&provider), &cache) else {
        panic!("phase skipped");
    };
    assert_eq!((stats.calls, stats.unique_misses, stats.hits), (1, 1, 2));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn secrets_are_redacted_before_text_reaches_the_provider() {
    let canary = "AKIAIOSFODNN7EXAMPLE";
    let (_dir, cache) = cache();
    let provider = Recording::new(true);
    let nodes = vec![node(
        "res:k",
        "Resource",
        serde_json::json!({"name": "bucket", "access_key": canary, "note": format!("password={canary}")}),
    )];
    let PhaseOutcome::Embedded { .. } = run_with(&nodes, Some(&provider), &cache) else {
        panic!("phase skipped");
    };
    let sent = provider.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].contains(canary));
    assert!(sent[0].contains("[REDACTED"));
}

#[test]
fn redaction_precedes_truncation_so_a_cut_secret_never_leaks() {
    let canary = "AKIAIOSFODNN7EXAMPLE";
    let prefix = "Resource\nnote: ";
    let filler = "x ".repeat((MAX_UNIT_BYTES - prefix.len() - 8) / 2);
    let nodes = vec![node(
        "res:k",
        "Resource",
        serde_json::json!({"note": format!("{filler}{canary}")}),
    )];
    let (units, _) = units(&nodes);
    assert!(units[0].text.len() <= MAX_UNIT_BYTES);
    assert!(!units[0].text.contains("AKIA"));
    assert!(units[0].text.contains("[REDACTED"));
}

#[test]
fn unit_keys_hash_the_exact_text_the_cache_embeds() {
    // An access-key-shaped run that continues past 16 characters is not a
    // match; truncation can cut it to exactly 16 at the end of the unit,
    // which the cached provider's own redaction pass would then change.
    let (_dir, cache) = cache();
    let provider = Recording::new(true);
    let prefix = "Resource\nnote: ";
    let filler = format!("{} ", "x ".repeat((MAX_UNIT_BYTES - prefix.len() - 21) / 2));
    let note = format!("{filler}AKIA{}", "ABCDEFGHIJKLMNOPQRSTU");
    assert_eq!(prefix.len() + filler.len() + 20, MAX_UNIT_BYTES);
    let nodes = vec![node(
        "res:k",
        "Resource",
        serde_json::json!({ "note": note }),
    )];
    let PhaseOutcome::Embedded { units, .. } = run_with(&nodes, Some(&provider), &cache) else {
        panic!("phase skipped");
    };
    let sent = provider.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].contains("AKIA"));
    assert_eq!(core_redact::redact_text(&sent[0]).0, sent[0]);
    let key = KnowledgeUnit {
        node_id: "res:k".into(),
        text: sent[0].clone(),
    }
    .key();
    assert_eq!(units, [("res:k".to_owned(), key)]);
}

fn flow(id: &str, trigger: &str, hops: &[(&str, &str, &str)]) -> Node {
    let hops: Vec<_> = hops
        .iter()
        .map(|(src, label, dst)| {
            serde_json::json!({"label": label, "src": "s", "dst": "d", "src_name": src, "dst_name": dst})
        })
        .collect();
    node(
        id,
        "BusinessFlow",
        serde_json::json!({
            "name": format!("Traced flow from {trigger}"),
            "trace": {"trigger": "ep", "trigger_kind": "Endpoint", "trigger_name": trigger, "hops": hops, "status": "Verified", "score": 1.0},
            "prov": {"tier": "T2"},
        }),
    )
}

// AC-0499: a derived flow is a unit after the base's rules and before its
// modules and symbols, rendered by its trigger, status and hops.
#[test]
fn derived_flows_are_units_rendered_by_their_hops() {
    let mut nodes = base();
    nodes.push(node("mod:m", "Module", serde_json::json!({"name": "m"})));
    // A base node cannot pose as a flow, and only flows count as flows.
    nodes.push(flow("business-flow:base", "GET /base", &[]));
    let pay = flow(
        "business-flow:b",
        "POST /pay",
        &[
            ("POST /pay", "HANDLES", "PayController.pay"),
            ("PayController.pay", "CALLS", "charge"),
        ],
    );
    let refund = flow("business-flow:a", "POST /refund", &[]);
    let stray = node("sym:stray", "Symbol", serde_json::json!({}));
    // A flow under a base node's id is not a second unit of that id.
    let shadow = flow("sym:a", "GET /shadow", &[]);
    let (units, omitted) = units_with_flows(&nodes, &[&pay, &refund, &stray, &pay, &shadow]);
    assert_eq!(omitted, 0);
    let ids: Vec<_> = units.iter().map(|u| u.node_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "zz:ep",
            "business-flow:a",
            "business-flow:b",
            "mod:m",
            "sym:a",
            "sym:b"
        ]
    );
    assert_eq!(
        units[2].text,
        "BusinessFlow\nname: Traced flow from POST /pay\ntrigger: Endpoint POST /pay\nstatus: Verified\nhop: POST /pay HANDLES PayController.pay\nhop: PayController.pay CALLS charge"
    );
    // Without flows the units are the base units, as retrieval builds them.
    assert_eq!(super::units(&nodes).0.len(), 4);
}

// AC-0499: flows past their share are counted and take no symbol's place
// beyond it.
#[test]
fn flows_past_their_share_are_counted_as_omitted() {
    let flows: Vec<_> = (0..MAX_FLOW_UNITS + 3)
        .map(|i| flow(&format!("business-flow:{i:06}"), "GET /x", &[]))
        .collect();
    let refs: Vec<_> = flows.iter().collect();
    let symbols: Vec<_> = (0..MAX_UNITS)
        .map(|i| node(&format!("sym:{i:06}"), "Symbol", serde_json::json!({})))
        .collect();
    let (units, omitted) = units_with_flows(&symbols, &refs);
    assert_eq!(units.len(), MAX_UNITS);
    assert_eq!(omitted, 3 + MAX_FLOW_UNITS);
    assert_eq!(
        units
            .iter()
            .filter(|u| u.node_id.starts_with("business-flow:"))
            .count(),
        MAX_FLOW_UNITS
    );
    assert_eq!(
        units[MAX_FLOW_UNITS - 1].node_id,
        format!("business-flow:{:06}", MAX_FLOW_UNITS - 1)
    );
    assert_eq!(units[MAX_FLOW_UNITS].node_id, "sym:000000");
}

// AC-0499: a flow's text is redacted before the provider sees it, and an
// unchanged flow is a cache hit.
#[test]
fn flow_units_are_redacted_and_reuse_cached_vectors() {
    let canary = "AKIAIOSFODNN7EXAMPLE";
    let (_dir, cache) = cache();
    let provider = Recording::new(true);
    let leaky = flow(
        "business-flow:a",
        "GET /keys",
        &[("GET /keys", "CALLS", &format!("load password={canary}"))],
    );
    let embed = |flows: &[&Node]| {
        let lease = cache.lock().unwrap().lease().unwrap();
        run(
            &base(),
            flows,
            &SectionText::new(),
            Some(&provider),
            Arc::clone(&cache),
            lease,
        )
        .unwrap()
    };
    let PhaseOutcome::Embedded { units, stats, .. } = embed(&[&leaky]) else {
        panic!("phase skipped");
    };
    assert_eq!((units.len(), stats.unique_misses), (4, 4));
    assert_eq!(units[1].0, "business-flow:a");
    {
        let sent = provider.sent.lock().unwrap();
        assert!(sent.iter().all(|text| !text.contains(canary)));
        assert!(
            sent.iter()
                .any(|text| text.starts_with("BusinessFlow\n") && text.contains("[REDACTED"))
        );
    }
    let PhaseOutcome::Embedded { stats, .. } = embed(&[&leaky]) else {
        panic!("phase skipped");
    };
    assert_eq!((stats.calls, stats.unique_misses, stats.hits), (0, 0, 4));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

fn section(id: &str, heading: &str) -> Node {
    node(
        id,
        SECTION_LABEL,
        serde_json::json!({"path": "README.md", "ordinal": 1, "level": 2, "heading": heading}),
    )
}

// AC-1505: a documentation section is a unit after the traced flows and
// before modules, rendered by its heading and the text the host read from
// its source (whitespace collapsed, redacted before it reaches the
// provider); without host text it is its heading alone.
#[test]
fn sections_render_their_heading_and_the_host_read_text() {
    let canary = "AKIAIOSFODNN7EXAMPLE";
    let mut nodes = base();
    nodes.push(section("docsec:r@README.md#1", "Refunds"));
    nodes.push(section("docsec:r@README.md#2", "Deploy"));
    nodes.push(node("mod:m", "Module", serde_json::json!({"name": "m"})));
    let texts = SectionText::from([(
        "docsec:r@README.md#1".to_string(),
        format!("## Refunds\n\nRefunds settle   nightly.\nkey={canary}\n"),
    )]);
    let (units, omitted) = units_with_sources(&nodes, &[], &texts);
    assert_eq!(omitted, 0);
    let order: Vec<_> = units.iter().map(|unit| unit.node_id.as_str()).collect();
    let refunds = order
        .iter()
        .position(|id| *id == "docsec:r@README.md#1")
        .unwrap();
    let module = order.iter().position(|id| *id == "mod:m").unwrap();
    assert!(refunds < module, "{order:?}");
    let refunds = &units[refunds].text;
    assert!(refunds.contains("heading: Refunds"), "{refunds}");
    assert!(
        refunds.contains("text: ## Refunds Refunds settle nightly."),
        "{refunds}"
    );
    assert!(!refunds.contains(canary), "{refunds}");
    let deploy = units
        .iter()
        .find(|unit| unit.node_id == "docsec:r@README.md#2")
        .unwrap();
    assert!(!deploy.text.contains("text:"), "{}", deploy.text);
    // Without host text the recipe is the one `units_with_flows` gives.
    assert_eq!(
        units_with_sources(&nodes, &[], &SectionText::new()),
        units_with_flows(&nodes, &[])
    );
}

// AC-1505: sections past their share are counted, never embedded, and the
// host is asked for exactly the sections the phase admits.
#[test]
fn sections_past_their_share_are_counted_and_admission_matches() {
    let nodes: Vec<Node> = (0..MAX_SECTION_UNITS + 3)
        .map(|n| section(&format!("docsec:r@README.md#{n:05}"), "Part"))
        .collect();
    let (units, omitted) = units(&nodes);
    assert_eq!(units.len(), MAX_SECTION_UNITS);
    assert_eq!(omitted, 3);
    let admitted: Vec<&str> = admitted_sections(&nodes, &[])
        .into_iter()
        .map(|node| node.id.as_str())
        .collect();
    let embedded: Vec<&str> = units.iter().map(|unit| unit.node_id.as_str()).collect();
    assert_eq!(admitted, embedded);
    // When higher-ranked units fill the global cap, no section is admitted,
    // so the host reads none it could not embed.
    let mut full: Vec<Node> = (0..MAX_UNITS)
        .map(|n| {
            node(
                &format!("ep:r@GET /{n:05}"),
                "Endpoint",
                serde_json::json!({}),
            )
        })
        .collect();
    full.push(section("docsec:r@README.md#1", "Part"));
    assert!(admitted_sections(&full, &[]).is_empty());
    let mut room = full[1..].to_vec();
    room.push(section("docsec:r@README.md#0", "Part"));
    let admitted: Vec<&str> = admitted_sections(&room, &[])
        .into_iter()
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(admitted, ["docsec:r@README.md#0"]);
}
