//! `external.md` (#1788, AC-0660, ADR-0063, SPEC-13).
use super::*;
use crate::{
    FactIndex, SelectedArtifactOutcome, SelectedCompileLimits, SpecArtifact, check_citations,
    citation_id, compile_spec, compile_spec_selected, try_compile_spec_with_coverage,
};
use core_graph::FactKey;
use core_prov::{EvidenceRef, Tier};
use flowtracer::FlowStatus;
use serde_json::{Value, json};

const ID: &str = "external";
const WEB: &str = "acme/web";
const API: &str = "acme/api";
const STRIPE: &str = "https://api.stripe.com";
const ORDERS: &str = "https://orders.internal";

fn prov(
    tier: Tier,
    confidence: ConfidenceTier,
    (repo, path): (&str, &str),
    spans: &[(u64, u64)],
    fact: &str,
) -> Value {
    let evidence = spans
        .iter()
        .map(|&(byte_start, byte_end)| EvidenceRef {
            repo: repo.into(),
            path: path.into(),
            byte_start,
            byte_end,
            commit_sha: "0123abc".into(),
        })
        .collect();
    serde_json::to_value(
        Provenance::new(tier, confidence, evidence, "t0.fixture", fact.as_bytes()).unwrap(),
    )
    .unwrap()
}

fn t0(confidence: ConfidenceTier, at: (&str, &str), span: (u64, u64), fact: &str) -> Value {
    prov(Tier::Deterministic, confidence, at, &[span], fact)
}

fn symbol(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

fn gap_id(repo: &str, path: &str, byte: u64) -> String {
    format!("gap:fetch:{repo}@{path}@{byte}")
}

/// One outbound call as `events::stitch_fetches` leaves its Gap.
struct Call<'a> {
    at: (&'a str, &'a str),
    byte: u64,
    caller: &'a str,
    /// `(method, url, url_source)`, when the URL resolved.
    request: Option<(&'a str, &'a str, &'a str)>,
    reason: &'a str,
}

/// Facts in the shapes `events`, `adapters_fw::observed` and `dynamic` emit
/// them, and the flows the tracer finds in them.
#[derive(Clone, Default)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn node(&mut self, id: &str, label: &str, mut props: Value, at: (&str, &str)) {
        props["prov"] = t0(ConfidenceTier::Confirmed, at, (1, 9), id);
        self.nodes.push(Node {
            id: id.into(),
            label: label.into(),
            props,
        });
    }

    fn edge(&mut self, src: &str, label: &str, dst: &str, at: (&str, &str), span: (u64, u64)) {
        let confidence = if dst.starts_with("gap:") {
            ConfidenceTier::Gap
        } else {
            ConfidenceTier::Confirmed
        };
        self.edges.push(Edge {
            props: json!({"prov": t0(confidence, at, span, &format!("{src} {label} {dst}"))}),
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
        });
    }

    /// A Symbol in `path`, by name.
    fn symbol(&mut self, (repo, path): (&str, &str), name: &str) -> String {
        let id = symbol(repo, path, name);
        if self.nodes.iter().all(|node| node.id != id) {
            self.node(&id, "Symbol", json!({"name": name}), (repo, path));
        }
        id
    }

    /// An endpoint and the Symbol that handles it: one flow.
    fn entry(&mut self, (repo, path): (&str, &str), route: &str, handler: &str) -> String {
        let id = format!("ep:{repo}@GET:{route}");
        self.node(
            &id,
            "Endpoint",
            json!({"method": "GET", "path": route}),
            (repo, path),
        );
        let handler = self.symbol((repo, path), handler);
        self.edge(&id, "HANDLES", &handler, (repo, path), (10, 20));
        id
    }

    /// The Gap of an outbound call and the `FETCHES` relation into it.
    fn call(&mut self, call: Call<'_>) -> String {
        let (repo, path) = call.at;
        let id = gap_id(repo, path, call.byte);
        let span = (call.byte, call.byte + 30);
        let mut props = json!({
            "reason": call.reason,
            "attempted_tiers": ["T0"],
            "prov": t0(ConfidenceTier::Gap, call.at, span, &id),
        });
        if let Some((method, url, source)) = call.request {
            props["request"] = json!({"method": method, "url": url, "url_source": source});
        }
        self.nodes.push(Node {
            id: id.clone(),
            label: "Gap".into(),
            props,
        });
        let caller = self.symbol(call.at, call.caller);
        self.edge(&caller, "FETCHES", &id, call.at, span);
        id
    }

    /// A literal third-party call: no binding pass has seen it.
    fn outbound(&mut self, at: (&str, &str), byte: u64, caller: &str, url: &str) -> String {
        self.call(Call {
            at,
            byte,
            caller,
            request: Some(("POST", url, "literal")),
            reason: "no recovered endpoint matches POST /v1/charges",
        })
    }

    /// What `events::http_binding` leaves on a Gap it keeps.
    fn kept(&mut self, gap: &str, reason: &str, candidates: Value) {
        let node = self.fact(gap);
        node.props["t0_reason"] = node.props["reason"].clone();
        node.props["reason"] = reason.into();
        if candidates.as_array().is_some_and(|list| !list.is_empty()) {
            node.props["candidates"] = candidates;
        }
    }

    /// What `events::http_binding` records on a Gap it keeps since #1843,
    /// and on its Gap edge: the binding state of the call's origin, and on
    /// the Gap the citations of the bindings that name that origin.
    fn recorded(&mut self, gap: &str, state: &str, cited: Option<Value>) {
        self.fact(gap).props[BINDING_STATE] = state.into();
        if let Some(cited) = cited {
            self.fact(gap).props["binding"] = cited;
        }
        self.relation("FETCHES", gap).props[BINDING_STATE] = state.into();
    }

    /// An observed client resource and one use of it.
    fn observed(
        &mut self,
        at: (&str, &str),
        (kind, provider, instance): (&str, &str, Option<Value>),
        (caller, byte): (&str, u64),
    ) -> String {
        let (repo, _) = at;
        let named = instance.as_ref().map(|instance| {
            instance["identity"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("env:{}", instance["env_key"].as_str().unwrap()))
        });
        let logical = match &named {
            Some(name) => format!("observed:{provider}:{name}"),
            None => format!("observed:{provider}"),
        };
        let id = format!("res:{repo}@{logical}");
        if self.nodes.iter().all(|node| node.id != id) {
            let mut props = json!({
                "type": provider, "provider": provider, "kind": kind,
                "observed": true, "logical_id": logical,
            });
            if let Some(instance) = instance {
                props
                    .as_object_mut()
                    .unwrap()
                    .extend(instance.as_object().unwrap().clone());
            }
            self.node(&id, "Resource", props, at);
        }
        let caller = self.symbol(at, caller);
        self.edge(&caller, "USES", &id, at, (byte, byte + 30));
        if kind == HTTP_API {
            self.edges.last_mut().unwrap().props["client"] = "fetch".into();
        }
        id
    }

    fn http(&mut self, at: (&str, &str), origin: &str, use_at: (&str, u64)) -> String {
        self.observed(
            at,
            (HTTP_API, HTTP_API, Some(json!({"identity": origin}))),
            use_at,
        )
    }

    /// A channel and its publisher.
    fn published(&mut self, at: (&str, &str), kind: &str, identity: &str, by: &str) -> String {
        let id = format!("chan:{kind}:{identity}");
        self.node(
            &id,
            "Channel",
            json!({"kind": kind, "identity": identity}),
            at,
        );
        let by = self.symbol(at, by);
        self.edge(&by, "PUBLISHES", &id, at, (40, 60));
        id
    }

    fn entity(&mut self, at: (&str, &str), name: &str, label: &str, by: &str) -> String {
        let id = format!("data:{}@{name}", at.0);
        if self.nodes.iter().all(|node| node.id != id) {
            self.node(&id, "DataEntity", json!({"name": name}), at);
        }
        let by = self.symbol(at, by);
        self.edge(&by, label, &id, at, (70, 90));
        id
    }

    fn fact(&mut self, id: &str) -> &mut Node {
        self.nodes
            .iter_mut()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("no fact {id}"))
    }

    fn relation(&mut self, label: &str, dst: &str) -> &mut Edge {
        self.edges
            .iter_mut()
            .find(|edge| edge.label == label && edge.dst == dst)
            .unwrap_or_else(|| panic!("no {label} into {dst}"))
    }

    fn flows(&self) -> Vec<Flow> {
        flowtracer::trace(&self.nodes, &self.edges)
    }

    fn artifact(&self, mode: ExportMode) -> SpecArtifact {
        compile_spec(
            &self.nodes,
            &self.edges,
            &self.flows(),
            mode,
            &BTreeSet::new(),
        )
        .artifacts
        .into_iter()
        .find(|artifact| artifact.id == ID)
        .expect("the external artifact is exported")
    }

    fn content(&self) -> String {
        self.artifact(ExportMode::VerifiedOnly).content
    }
}

/// The rows of the table under `## heading`.
fn table<'a>(content: &'a str, heading: &str) -> Vec<&'a str> {
    let start = content
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("no section {heading} in:\n{content}"));
    content[start..]
        .lines()
        .skip(1)
        .skip_while(|line| !line.starts_with("|---"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .collect()
}

fn cells(row: &str) -> Vec<&str> {
    row.trim_matches('|').split(" | ").map(str::trim).collect()
}

fn counts(content: &str) -> &str {
    content
        .lines()
        .find(|line| line.starts_with("Export mode:"))
        .expect("the counts line")
}

fn binding(service: &str, repo: &str, base_url: &str) -> Value {
    json!([{
        "kind": "declared-service", "service": service, "repo": repo,
        "base_url": base_url, "declared_in": "cartograph.system.toml",
    }])
}

fn fetch(props: Value) -> Node {
    Node {
        id: gap_id(WEB, "src/a.ts", 5),
        label: "Gap".into(),
        props,
    }
}

fn request(method: &str, url: &str, source: &str) -> Value {
    json!({"request": {"method": method, "url": url, "url_source": source}})
}

fn resource(props: Value) -> Node {
    Node {
        id: "res:acme/web@observed:x".into(),
        label: "Resource".into(),
        props,
    }
}

// AC-0660 (SPEC-13 classification, ADR-0063 decision 5): the one predicate
// decides every row of the table from the target's own fact and the origins
// binding citations name. Two cases are narrower than the table: a call the
// binding pass kept without candidates, and a call with a computed method,
// are unclassified, because the fact does not record the pass's outcome.
#[test]
fn the_predicate_decides_every_row_of_the_classification_table() {
    use Boundary::{Inside, NotACrossing, Outside, Unclassified};
    let none = BTreeSet::new();
    let bound: BTreeSet<String> = [ORDERS.to_string()].into();
    let decide = |node: &Node, bound: &BTreeSet<String>| boundary(&Target::Fetch(node), bound);

    // Fetch Gap: binding evidence names a registered repository. Ambiguous:
    // the candidates say so, whatever else the graph cites.
    let mut ambiguous = fetch(request("GET", "https://orders.internal/v1/7", "literal"));
    ambiguous.props["candidates"] = json!([{"endpoint": "ep:acme/api@GET:/v1/:id"}]);
    assert_eq!(
        decide(&ambiguous, &none),
        NotACrossing(NotCrossing::InsideUnresolved)
    );
    // No endpoint matched, and a citation elsewhere names the origin. Host
    // and scheme compare in lower case; the port is part of the origin.
    let unmatched = fetch(request("DELETE", "HTTPS://Orders.Internal/v1/7", "literal"));
    assert_eq!(
        decide(&unmatched, &bound),
        NotACrossing(NotCrossing::InsideUnresolved)
    );
    let other_port = fetch(request(
        "DELETE",
        "https://orders.internal:8443/v1",
        "literal",
    ));
    assert_eq!(decide(&other_port, &bound), Outside);

    // Fetch Gap: a literal scheme and host, and no declared binding.
    for source in ["literal", "base:literal"] {
        let literal = fetch(request("POST", "https://api.stripe.com/v1/charges", source));
        assert_eq!(decide(&literal, &none), Outside, "{source}");
        assert_eq!(decide(&literal, &bound), Outside, "{source}");
    }
    // Credentials in a URL are not part of the origin.
    assert_eq!(
        target_origin(&Target::Fetch(&fetch(request(
            "GET",
            "https://user:secret@api.stripe.com/v1",
            "literal"
        )))),
        Some(STRIPE.to_string())
    );

    // Fetch Gap: a URL without a host is the caller's own origin, even when
    // a query carries another URL.
    for url in ["/api/orders", "/next?to=https://evil.example/x", "orders"] {
        let relative = fetch(request("GET", url, "literal"));
        assert_eq!(
            decide(&relative, &none),
            NotACrossing(NotCrossing::SameOrigin),
            "{url}"
        );
    }

    // Fetch Gap: no request, or a host that is not literal.
    let computed = fetch(json!({"reason": "runtime-computed fetch URL"}));
    assert_eq!(
        decide(&computed, &none),
        Unclassified(Unknown::HostNotInSource)
    );
    for source in ["config:.env", "base:config:.env"] {
        let configured = fetch(request("GET", "https://api.stripe.com/v1", source));
        assert_eq!(
            decide(&configured, &none),
            Unclassified(Unknown::HostNotInSource),
            "{source}"
        );
    }
    assert_eq!(
        decide(
            &fetch(request("GET", "//cdn.example/x.js", "literal")),
            &none
        ),
        Unclassified(Unknown::SchemeNotInSource)
    );

    // Narrower than the table. The binding pass kept the Gap: "no binding
    // covers it" and "a binding names a repository with no such endpoint"
    // differ only in the sentence, which is not parsed.
    let mut kept = fetch(request(
        "POST",
        "https://api.stripe.com/v1/charges",
        "literal",
    ));
    kept.props["t0_reason"] = "no recovered endpoint matches POST /v1/charges".into();
    kept.props["reason"] = "no binding evidence: no declared service or gateway route covers \
                            https://api.stripe.com/v1/charges"
        .into();
    assert_eq!(
        decide(&kept, &none),
        Unclassified(Unknown::BindingNotRecorded)
    );
    kept.props["reason"] =
        "binding evidence names orders (acme/api), but no recovered endpoint there matches \
         POST /v1/charges"
            .into();
    assert_eq!(
        decide(&kept, &none),
        Unclassified(Unknown::BindingNotRecorded)
    );
    // The binding pass skips a call whose method is computed.
    let any_method = fetch(request("?", "https://api.stripe.com/v1/charges", "literal"));
    assert_eq!(
        decide(&any_method, &none),
        Unclassified(Unknown::MethodNotKnown)
    );

    // #1843: the binding pass records what the declared bindings say about
    // the origin of every Gap it keeps, and the predicate reads that field,
    // never the sentence. `none` is outside only from a literal host.
    for mut evaluated in [kept.clone(), any_method.clone()] {
        evaluated.props[BINDING_STATE] = STATE_NONE.into();
        assert_eq!(decide(&evaluated, &none), Outside);
        for inside in STATES_INSIDE {
            evaluated.props[BINDING_STATE] = inside.into();
            assert_eq!(
                decide(&evaluated, &none),
                NotACrossing(NotCrossing::InsideUnresolved),
                "{inside}"
            );
        }
    }
    let mut configured = fetch(request("GET", "https://api.stripe.com/v1", "config:.env"));
    configured.props[BINDING_STATE] = STATE_NONE.into();
    assert_eq!(
        decide(&configured, &none),
        Unclassified(Unknown::HostNotInSource)
    );
    // The words `events::http_binding` records (pinned there as well).
    assert_eq!(
        (BINDING_STATE, STATE_NONE, STATES_INSIDE),
        (
            "binding_state",
            "none",
            ["origin_declared", "names_repository", "ambiguous"]
        )
    );
    assert_eq!(
        Unknown::HostNotInSource.reason(),
        "host not known from source"
    );

    // Observed clients.
    let observed = |props: Value, bound: &BTreeSet<String>| {
        boundary(&Target::Observed(&resource(props)), bound)
    };
    let api = json!({"observed": true, "kind": "http-api", "identity": ORDERS});
    assert_eq!(observed(api.clone(), &bound), Inside);
    assert_eq!(observed(api, &none), Outside);
    assert_eq!(
        observed(
            json!({"observed": true, "kind": "http-api", "env_key": "API_URL"}),
            &none
        ),
        Unclassified(Unknown::HostNotInSource)
    );
    let run_time = Node {
        id: "gap:observed:acme/web@src/a.ts@5".into(),
        label: "Gap".into(),
        props: json!({"kind": "http-api", "reason": "runtime-computed http-api identity"}),
    };
    assert_eq!(
        boundary(&Target::ObservedGap(&run_time), &none),
        Unclassified(Unknown::HostNotInSource)
    );
    for kind in ["sdk-client", "auth-provider"] {
        assert_eq!(
            observed(json!({"observed": true, "kind": kind}), &bound),
            Unclassified(Unknown::ProviderOperator),
            "{kind}"
        );
    }
    assert_eq!(
        Unknown::ProviderOperator.reason(),
        "the registries do not record who operates this provider"
    );
    assert_eq!(
        observed(json!({"observed": true, "kind": "browser-storage"}), &none),
        NotACrossing(NotCrossing::BrowserStorage)
    );

    // Channels.
    let channel = |kind: &str, publishers, subscribers, backing| {
        let node = Node {
            id: format!("chan:{kind}:orders"),
            label: "Channel".into(),
            props: json!({"kind": kind, "identity": "orders"}),
        };
        boundary(
            &Target::Channel {
                node: &node,
                publishers,
                subscribers,
                backing,
            },
            &none,
        )
    };
    assert_eq!(
        channel("inproc-event", 1, 0, 0),
        NotACrossing(NotCrossing::InProcess)
    );
    assert_eq!(channel("sqs-queue", 1, 1, 0), Inside);
    assert_eq!(channel("sqs-queue", 1, 0, 1), Inside);
    assert_eq!(
        channel("sqs-queue", 1, 0, 0),
        Unclassified(Unknown::NoSubscriber)
    );
    assert_eq!(
        Unknown::NoSubscriber.reason(),
        "no subscriber or backing resource recovered"
    );
    assert_eq!(
        channel("sqs-queue", 0, 2, 0),
        NotACrossing(NotCrossing::Inbound)
    );
}

/// `acme/web` calls `acme/api` and a payment provider. Without a system
/// manifest, so no binding pass has touched a Gap.
fn shop() -> Fixture {
    let mut f = Fixture::default();
    let pay = (WEB, "src/pay.ts");
    // GET /checkout -> checkout -> charge, which calls the provider and
    // writes an order; checkout reads the customer.
    f.entry(pay, "/checkout", "checkout");
    let checkout = f.symbol(pay, "checkout");
    let charge = f.symbol(pay, "charge");
    f.edge(&checkout, "CALLS", &charge, pay, (30, 38));
    f.outbound(pay, 120, "charge", "https://api.stripe.com/v1/charges");
    f.entity(pay, "Order", "WRITES", "charge");
    f.entity(pay, "Customer", "READS", "checkout");
    f.entity(pay, "Order", "READS", "checkout");
    // An entity no flow to the provider touches.
    f.entity((WEB, "src/audit.ts"), "AuditLog", "WRITES", "record");

    // GET /refund -> refund, which stops at an unresolved call before the
    // provider call in `settle`.
    let refund = (WEB, "src/refund.ts");
    f.entry(refund, "/refund", "refund");
    let stop = "gap:call:acme/web@src/refund.ts@44";
    f.nodes.push(Node {
        id: stop.into(),
        label: "Gap".into(),
        props: json!({
            "reason": "dynamic dispatch",
            "attempted_tiers": ["T0"],
            "prov": t0(ConfidenceTier::Gap, refund, (44, 60), stop),
        }),
    });
    let from = f.symbol(refund, "refund");
    f.edge(&from, "CALLS", stop, refund, (44, 60));
    f.outbound(refund, 300, "settle", "https://api.stripe.com/v1/refunds");
    f.entity(refund, "Refund", "WRITES", "settle");
    f
}

// AC-0660: a literal third-party origin that no binding covers is listed as
// outside with its sites, spans, the flow that reaches it and the entities
// read or written on that flow. A flow that breaks before the outbound call
// is not listed as reaching the target and stays partial (R-INT-4).
#[test]
fn a_third_party_origin_is_outside_with_its_sites_flows_and_entities() {
    let f = shop();
    let content = f.content();
    assert!(counts(&content).contains("Outside targets: 1. Unclassified targets: 0."));

    let services = table(&content, "External services");
    assert_eq!(services.len(), 1, "{content}");
    assert_eq!(
        cells(services[0]),
        [
            STRIPE,
            "outside",
            "2",
            "1",
            "Customer (read); Order (read, written)",
            "—"
        ]
    );

    let sites = table(&content, "Sites of each external service");
    assert_eq!(sites.len(), 2, "{content}");
    let charge = cells(sites[0]);
    assert_eq!(charge[..5], [STRIPE, "charge", "POST", "/v1/charges", "—"]);
    // The outbound call is a Gap fact, and the row says so.
    assert_eq!(charge[5..7], ["Deterministic", "Gap"]);
    assert_eq!(charge[7], "acme/web:src/pay.ts bytes 120..150 @ 0123abc");
    assert_eq!(charge[8], "GET /checkout");
    // The site the broken flow never reached.
    let settle = cells(sites[1]);
    assert_eq!(settle[1..4], ["settle", "POST", "/v1/refunds"]);
    assert_eq!(settle[8], "no flow recovered through this site");

    // One row per flow and target. The column does not say "sent".
    assert!(content.contains("| Entities read or written on these flows |"));
    assert!(content.contains("does not say that an entity is sent"));
    let data = table(&content, "External data flows");
    assert_eq!(data.len(), 1, "{content}");
    assert_eq!(
        cells(data[0]),
        [
            STRIPE,
            "GET /checkout",
            "Endpoint",
            "Partial",
            "Customer (read); Order (read, written)"
        ]
    );
    // Neither the refund flow nor what `settle` writes is listed.
    assert!(!data.iter().any(|row| row.contains("/refund")));
    assert!(!content.contains("Refund (written)"));
    assert!(!content.contains("AuditLog"));
    let flows = f.flows();
    let refund = flows
        .iter()
        .find(|flow| flow.trigger_name == "GET /refund")
        .unwrap();
    assert_eq!(refund.status, FlowStatus::Partial);
    assert!(
        refund
            .hops
            .iter()
            .all(|hop| !hop.dst.starts_with(FETCH_GAP))
    );

    // The limits are stated.
    assert!(content.contains("**The outside list is a lower bound.**"));
    assert!(content.contains(
        "**Without declared bindings, a call to another registered repository by absolute URL \
         is listed as outside.**"
    ));
}

/// Two registered repositories and a declared binding for `acme/api`, as the
/// binding pass leaves the graph: the bound call is a `FETCHES` relation
/// that cites the binding, and every other absolute call it evaluated keeps
/// its Gap with `t0_reason`.
fn system() -> Fixture {
    let mut f = shop();
    let orders = (WEB, "src/orders.ts");
    // Bound and resolved: a relation into the other repository.
    f.entry(orders, "/orders", "listOrders");
    f.node(
        "ep:acme/api@GET:/v1/orders",
        "Endpoint",
        json!({"method": "GET", "path": "/v1/orders"}),
        (API, "app.py"),
    );
    let list = f.symbol(orders, "listOrders");
    f.edge(
        &list,
        "FETCHES",
        "ep:acme/api@GET:/v1/orders",
        orders,
        (200, 230),
    );
    f.relation("FETCHES", "ep:acme/api@GET:/v1/orders").props["binding"] =
        binding("orders", API, ORDERS);
    // The same call, as the observed-client pass sees it.
    f.http(orders, ORDERS, ("listOrders", 200));

    // The binding pass evaluated both provider calls and found no binding.
    for (path, byte, tail) in [
        ("src/pay.ts", 120, "charges"),
        ("src/refund.ts", 300, "refunds"),
    ] {
        f.kept(
            &gap_id(WEB, path, byte),
            &format!(
                "no binding evidence: no declared service or gateway route covers \
                 {STRIPE}/v1/{tail}"
            ),
            json!([]),
        );
    }
    // Bound, and no endpoint there matches.
    let cancel = f.call(Call {
        at: orders,
        byte: 400,
        caller: "cancel",
        request: Some(("DELETE", "https://orders.internal/v1/orders/7", "literal")),
        reason: "deferred to system binding: https://orders.internal is bound in the system \
                 manifest",
    });
    f.kept(
        &cancel,
        "binding evidence names orders (acme/api), but no recovered endpoint there matches \
         DELETE /v1/orders/7",
        json!([]),
    );

    // A host computed at run time, and one read from configuration.
    f.call(Call {
        at: orders,
        byte: 500,
        caller: "report",
        request: None,
        reason: "runtime-computed fetch URL",
    });
    f.observed(
        orders,
        (HTTP_API, HTTP_API, Some(json!({"env_key": "REPORTS_URL"}))),
        ("export", 600),
    );
    // An SDK client and an auth-provider client.
    f.observed(orders, ("sdk-client", "stripe", None), ("charge", 700));
    f.observed(orders, ("auth-provider", "auth0", None), ("login", 800));
    // Browser storage.
    f.observed(
        orders,
        ("browser-storage", "indexeddb", None),
        ("cache", 900),
    );

    // Channels: one consumed in the other repository, one backed by a
    // deployed queue, one nobody is seen to consume, one in process and one
    // that is only consumed.
    let worker = (API, "worker.py");
    let placed = f.published(orders, "sqs-queue", "orders-placed", "listOrders");
    let consumer = f.symbol(worker, "consume");
    f.edge(&consumer, "SUBSCRIBES", &placed, worker, (5, 25));
    let backed = f.published(orders, "sns-topic", "orders-audit", "listOrders");
    f.node(
        "res:acme/api@aws_sns_topic.audit",
        "Resource",
        json!({"type": "aws_sns_topic"}),
        (API, "infra/main.tf"),
    );
    f.edge(
        "res:acme/api@aws_sns_topic.audit",
        "BACKS",
        &backed,
        (API, "infra/main.tf"),
        (1, 9),
    );
    f.published(orders, "kafka-topic", "orders-shipped", "listOrders");
    f.published(orders, "inproc-event", "cart-updated", "listOrders");
    let inbound = "chan:sqs-queue:payments-settled";
    f.node(
        inbound,
        "Channel",
        json!({"kind": "sqs-queue", "identity": "payments-settled"}),
        worker,
    );
    f.edge(&consumer, "SUBSCRIBES", inbound, worker, (30, 50));
    f
}

// AC-0660 (SPEC-13 acceptance evidence), over two registered repositories
// with a declared binding, in the shapes the binding pass leaves: the call
// into the other repository is absent; a run-time host, a configured host,
// an SDK client and an auth-provider client are unclassified with their
// reasons; a channel with a subscriber or a backing resource is absent and
// one without is unclassified; a call binding evidence places in the other
// repository is counted and not listed; an in-process channel, browser
// storage and an inbound channel are not listed.
//
// This graph is in the shape stored before the binding pass recorded its
// outcome as a field (#1843): the third-party calls are unclassified, not
// outside, because whether the pass found no binding is a sentence only.
// On a graph with the recorded state they are outside
// (`a_recorded_binding_state_lists_third_party_calls_as_outside`), as on a
// graph no binding pass has touched
// (`a_third_party_origin_is_outside_with_its_sites_flows_and_entities`).
#[test]
fn two_repositories_with_a_declared_binding_are_classified_by_the_one_predicate() {
    let f = system();
    let content = f.content();
    assert_eq!(
        counts(&content),
        "Export mode: `verified-only`. Outside targets: 0. Unclassified targets: 7. Calls that \
         binding evidence places in a registered repository and that matched no one endpoint \
         there (inside, unresolved): 1. Calls to the caller's own origin that matched no \
         endpoint: 0. Published channels with a recovered subscriber or backing resource: 2."
    );

    // The bound origin is not a target, as a call or as an observed client.
    for heading in [
        "External services",
        "Sites of each external service",
        "External data flows",
        "Unclassified",
    ] {
        for row in table(&content, heading) {
            assert!(!row.contains("orders.internal"), "{heading}: {row}");
        }
    }
    assert_eq!(
        table(&content, "External services"),
        ["| — | None in this export | 0 | 0 | — | — |"]
    );

    let rows: Vec<Vec<&str>> = table(&content, "Unclassified")
        .into_iter()
        .map(cells)
        .collect();
    let listed: Vec<(&str, &str, &str)> = rows.iter().map(|row| (row[0], row[1], row[4])).collect();
    let unrecorded = Unknown::BindingNotRecorded.reason();
    assert_eq!(
        listed,
        [
            (
                "URL not recovered",
                "Outbound HTTP call",
                "host not known from source"
            ),
            (
                "POST https://api.stripe.com/v1/charges",
                "Outbound HTTP call",
                unrecorded
            ),
            (
                "POST https://api.stripe.com/v1/refunds",
                "Outbound HTTP call",
                unrecorded
            ),
            (
                "http-api: configuration key REPORTS\\_URL",
                "HTTP client",
                "host not known from source"
            ),
            (
                "stripe",
                "SDK client",
                "the registries do not record who operates this provider"
            ),
            (
                "auth0",
                "Auth-provider client",
                "the registries do not record who operates this provider"
            ),
            (
                "kafka-topic: orders-shipped",
                "Published channel",
                "no subscriber or backing resource recovered"
            ),
        ]
    );
    assert!(rows.iter().all(|row| row[2] == "unclassified"));
    // The sentence the binding pass recorded is shown, not interpreted.
    assert!(rows[1][5].starts_with("no binding evidence: no declared service"));
    assert_eq!(rows[0][5], "runtime-computed fetch URL");
    // Each row carries its repository, tier, confidence and span.
    assert_eq!(rows[1][3], WEB);
    assert_eq!(rows[1][7..9], ["Deterministic", "Gap"]);
    assert_eq!(rows[1][9], "acme/web:src/pay.ts bytes 120..150 @ 0123abc");
    assert_eq!(rows[4][7..9], ["Deterministic", "Confirmed"]);

    for absent in [
        "orders-placed",
        "orders-audit",
        "cart-updated",
        "payments-settled",
        "indexeddb",
    ] {
        for heading in ["External services", "Unclassified"] {
            for row in table(&content, heading) {
                assert!(!row.contains(absent), "{absent} in {heading}: {row}");
            }
        }
    }
    // The broken flow is still partial and reaches nothing listed.
    let flows = f.flows();
    let refund = flows
        .iter()
        .find(|flow| flow.trigger_name == "GET /refund")
        .unwrap();
    assert_eq!(refund.status, FlowStatus::Partial);
    assert!(content.contains("**A declared binding is known here only through the graph.**"));
}

// AC-0660 (#1843, SPEC-13 "Binding outcome"): the same two repositories on
// a graph whose binding pass recorded its outcome. The literal third-party
// calls are outside, each with its flow and span; the call a binding places
// in the other repository is still counted as inside, unresolved; a call
// with a computed method is classified by its origin; and an observed
// client of a declared host that no call names is not listed as outside.
#[test]
fn a_recorded_binding_state_lists_third_party_calls_as_outside() {
    let mut f = system();
    let orders = (WEB, "src/orders.ts");
    for (path, byte) in [("src/pay.ts", 120), ("src/refund.ts", 300)] {
        f.recorded(&gap_id(WEB, path, byte), "none", None);
    }
    f.recorded(
        &gap_id(WEB, "src/orders.ts", 400),
        "names_repository",
        Some(binding("orders", API, "https://orders.internal")),
    );
    // Computed methods: one to the provider, one to a declared gateway.
    for (byte, caller, url, state, cited) in [
        (
            1000,
            "send",
            "https://api.stripe.com/v1/events",
            "none",
            None,
        ),
        (
            1100,
            "proxy",
            "https://gw.internal/any",
            "origin_declared",
            Some(binding("orders", API, "https://gw.internal")),
        ),
    ] {
        let gap = f.call(Call {
            at: orders,
            byte,
            caller,
            request: Some(("?", url, "literal")),
            reason: "runtime-computed HTTP method",
        });
        f.recorded(&gap, state, cited);
    }
    // An observed client of a declared host, at no outbound call's site.
    let admin = f.http(orders, "https://admin.internal", ("audit", 1200));
    f.fact(&admin).props["binding"] = binding("admin", API, "https://admin.internal");
    // And one of an undeclared host: still outside.
    f.http(orders, "https://hooks.example.com", ("notify", 1300));

    let content = f.content();
    assert_eq!(
        counts(&content),
        "Export mode: `verified-only`. Outside targets: 2. Unclassified targets: 5. Calls that \
         binding evidence places in a registered repository and that matched no one endpoint \
         there (inside, unresolved): 2. Calls to the caller's own origin that matched no \
         endpoint: 0. Published channels with a recovered subscriber or backing resource: 2."
    );
    let services: Vec<Vec<&str>> = table(&content, "External services")
        .into_iter()
        .map(cells)
        .collect();
    let origins: Vec<&str> = services.iter().map(|row| row[0]).collect();
    assert_eq!(
        origins,
        ["https://api.stripe.com", "https://hooks.example.com"]
    );
    // Both recorded calls and the computed-method call are sites of the one
    // provider target, each with its span.
    let sites = table(&content, "Sites of each external service");
    let stripe: Vec<&&str> = sites
        .iter()
        .filter(|row| row.contains("api.stripe.com"))
        .collect();
    assert_eq!(stripe.len(), 3, "{sites:?}");
    for span in [
        "acme/web:src/pay.ts bytes 120..150 @ 0123abc",
        "acme/web:src/refund.ts bytes 300..330 @ 0123abc",
        "acme/web:src/orders.ts bytes 1000..1030 @ 0123abc",
    ] {
        assert!(
            stripe.iter().any(|row| row.contains(span)),
            "{span}: {stripe:?}"
        );
    }
    // The flow that reaches the charge is listed with the target.
    let flows = table(&content, "External data flows");
    assert!(
        flows
            .iter()
            .any(|row| row.contains("api.stripe.com") && row.contains("GET /checkout")),
        "{flows:?}"
    );
    // No declared host is a target or an unclassified row.
    for heading in [
        "External services",
        "Sites of each external service",
        "External data flows",
        "Unclassified",
    ] {
        for row in table(&content, heading) {
            for inside in ["orders.internal", "gw.internal", "admin.internal"] {
                assert!(!row.contains(inside), "{inside} in {heading}: {row}");
            }
        }
    }
    let unclassified = table(&content, "Unclassified");
    assert_eq!(unclassified.len(), 5);
    assert!(
        unclassified
            .iter()
            .all(|row| !row.contains("api.stripe.com")),
        "{unclassified:?}"
    );

    // A graph stored before the state was recorded keeps its result: the
    // same facts without the field are unclassified, with the reason.
    let before = system().content();
    assert!(before.contains(Unknown::BindingNotRecorded.reason()));
    assert!(!content.contains(Unknown::BindingNotRecorded.reason()));
}

// AC-0660: one origin is one target. Two call sites of it are two sites; an
// observed client use at the site of a call is that call, not a third site;
// a use with no outbound call of its own is a site, with the flows through
// the using symbol; a use of the origin from another repository joins the
// same target.
#[test]
fn one_origin_called_from_two_sites_and_observed_is_one_target() {
    let mut f = shop();
    let pay = (WEB, "src/pay.ts");
    // The observed-client pass sees the call in `charge` at the same span,
    // and the other call in `settle`.
    f.http(pay, STRIPE, ("charge", 120));
    f.http((WEB, "src/refund.ts"), STRIPE, ("settle", 300));
    // The same origin, written in another case.
    let upper = f.outbound(pay, 500, "charge", "HTTPS://API.Stripe.com/v1/tokens");
    f.fact(&upper).props["reason"] = "no recovered endpoint matches POST /v1/tokens".into();
    // A client the outbound-call pass does not read, in a symbol on the
    // checkout flow, and one at file level in another repository.
    f.http(pay, STRIPE, ("checkout", 640));
    let other = (API, "billing.py");
    f.http(other, STRIPE, ("reconcile", 50));

    let content = f.content();
    let services = table(&content, "External services");
    assert_eq!(services.len(), 1, "{content}");
    assert_eq!(cells(services[0])[..4], [STRIPE, "outside", "5", "1"]);

    let sites: Vec<Vec<&str>> = table(&content, "Sites of each external service")
        .into_iter()
        .map(cells)
        .collect();
    let listed: Vec<(&str, &str, &str, &str)> = sites
        .iter()
        .map(|row| (row[1], row[3], row[4], row[7]))
        .collect();
    assert_eq!(
        listed,
        [
            (
                "reconcile",
                "—",
                "fetch",
                "acme/api:billing.py bytes 50..80 @ 0123abc"
            ),
            (
                "charge",
                "/v1/charges",
                "fetch",
                "acme/web:src/pay.ts bytes 120..150 @ 0123abc"
            ),
            (
                "charge",
                "/v1/tokens",
                "—",
                "acme/web:src/pay.ts bytes 500..530 @ 0123abc"
            ),
            (
                "checkout",
                "—",
                "fetch",
                "acme/web:src/pay.ts bytes 640..670 @ 0123abc"
            ),
            (
                "settle",
                "/v1/refunds",
                "fetch",
                "acme/web:src/refund.ts bytes 300..330 @ 0123abc"
            ),
        ]
    );
    // A call that is both keeps the Gap's confidence; a use alone has its own.
    assert_eq!(sites[1][5..7], ["Deterministic", "Gap"]);
    assert_eq!(sites[3][5..7], ["Deterministic", "Confirmed"]);
    // An observed use is not a flow hop: its flows are those through the
    // using symbol, and there are none through `reconcile`.
    assert_eq!(sites[3][8], "GET /checkout");
    assert_eq!(sites[0][8], "no flow recovered through this site");
    assert!(table(&content, "Unclassified")[0].contains("None in this export"));

    // A use at the site of a call that matched an endpoint is inside.
    let mut resolved = Fixture::default();
    let at = (WEB, "src/a.ts");
    resolved.entry(at, "/v1/charges", "serve");
    let caller = resolved.symbol(at, "load");
    resolved.edge(
        &caller,
        "FETCHES",
        "ep:acme/web@GET:/v1/charges",
        at,
        (90, 120),
    );
    resolved.http(at, STRIPE, ("load", 90));
    assert!(
        table(&resolved.content(), "External services")[0].contains("None in this export"),
        "{}",
        resolved.content()
    );
}

// AC-0660 (R-INT-5): a verified-only export omits a row whose fact is
// InferredWeak and states how many facts it left out; a best-effort export
// includes it, marked.
#[test]
fn verified_only_omits_weak_rows_and_counts_them_and_best_effort_marks_them() {
    let mut f = shop();
    let at = (WEB, "src/mail.ts");
    let weak = |fact: &str, span| {
        prov(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            at,
            &[span],
            fact,
        )
    };
    // A proposed client of another origin, and a proposed channel.
    let mail = f.http(at, "https://api.mail.example", ("send", 10));
    f.fact(&mail).props["prov"] = weak(&mail, (10, 40));
    f.relation("USES", &mail).props["prov"] = weak("uses mail", (10, 40));
    let topic = f.published(at, "kafka-topic", "mail-sent", "send");
    f.fact(&topic).props["prov"] = weak(&topic, (40, 60));
    f.relation("PUBLISHES", &topic).props["prov"] = weak("publishes mail-sent", (40, 60));

    let verified = f.artifact(ExportMode::VerifiedOnly);
    assert!(!verified.content.contains("api.mail.example"));
    assert!(!verified.content.contains("mail-sent"));
    assert!(counts(&verified.content).contains("Outside targets: 1. Unclassified targets: 0."));
    assert!(verified.content.contains(
        "Boundary facts left out of this export because they are InferredWeak or rejected: 4."
    ));
    assert!(
        verified
            .assertions
            .iter()
            .all(|a| a.provenance.confidence_tier != ConfidenceTier::InferredWeak)
    );

    let best = f.artifact(ExportMode::BestEffort).content;
    assert!(counts(&best).contains("Outside targets: 2. Unclassified targets: 1."));
    assert!(best.contains("Boundary facts left out of this export because they are rejected: 0."));
    const MARK: &str = "rests on an InferredWeak fact; a verified-only export omits this row";
    let service = table(&best, "External services")
        .into_iter()
        .find(|row| row.contains("api.mail.example"))
        .unwrap();
    assert_eq!(cells(service)[5], MARK);
    let site = table(&best, "Sites of each external service")
        .into_iter()
        .find(|row| row.contains("api.mail.example"))
        .unwrap();
    assert_eq!(cells(site)[5..7], ["Agentic", "InferredWeak"]);
    assert_eq!(cells(site)[9], MARK);
    let channel = cells(table(&best, "Unclassified")[0]);
    assert_eq!(channel[0], "kafka-topic: mail-sent");
    assert_eq!(channel[7..9], ["Agentic", "InferredWeak"]);
    assert_eq!(channel[10], MARK);
    // The confirmed target is not marked.
    let stripe = table(&best, "External services")
        .into_iter()
        .find(|row| row.contains(STRIPE))
        .unwrap();
    assert_eq!(cells(stripe)[5], "—");
}

// AC-0660: a row carries the tier and confidence of the fact that holds its
// literal and never a higher one, whatever the other facts of the row say.
#[test]
fn a_row_never_states_a_higher_confidence_than_its_fact() {
    let mut f = system();
    let pay = (WEB, "src/pay.ts");
    // An observed use at the Gap's site is Confirmed; the row stays a Gap.
    f.http(pay, STRIPE, ("charge", 120));
    // A use whose identity rests on an assumption, on a Confirmed resource.
    let strong = f.http(pay, "https://api.maps.example", ("locate", 900));
    f.relation("USES", &strong).props["prov"] = prov(
        Tier::Deterministic,
        ConfidenceTier::InferredStrong,
        pay,
        &[(900, 930)],
        "uses maps",
    );
    let artifact = f.artifact(ExportMode::BestEffort);
    let rank_of = |name: &str| match name {
        "Confirmed" => 3,
        "InferredStrong" => 2,
        "InferredWeak" => 1,
        "Gap" => 0,
        other => panic!("unknown confidence {other}"),
    };
    // The lowest confidence any assertion of the artifact records for a
    // span, by the span's text as the rows print it.
    let mut floor: BTreeMap<String, u8> = BTreeMap::new();
    for assertion in &artifact.assertions {
        for span in &assertion.provenance.evidence {
            let key = format!(
                "{}:{} bytes {}..{}",
                span.repo, span.path, span.byte_start, span.byte_end
            );
            let held = rank(assertion.provenance.confidence_tier);
            floor
                .entry(key)
                .and_modify(|lowest| *lowest = (*lowest).min(held))
                .or_insert(held);
        }
    }
    let mut checked = 0;
    for (heading, confidence, span) in [
        ("Sites of each external service", 6, 7),
        ("Unclassified", 8, 9),
    ] {
        for row in table(&artifact.content, heading) {
            let row = cells(row);
            let at = row[span].split(" @ ").next().unwrap();
            let lowest = floor
                .get(at)
                .unwrap_or_else(|| panic!("{at} is cited by no assertion"));
            assert!(rank_of(row[confidence]) <= *lowest, "{heading}: {row:?}");
            checked += 1;
        }
    }
    assert!(checked >= 8, "{checked}");
    let sites = table(&artifact.content, "Sites of each external service");
    let maps = cells(
        sites
            .iter()
            .find(|row| row.contains("api.maps.example"))
            .unwrap(),
    );
    assert_eq!(maps[5..7], ["Deterministic", "InferredStrong"]);
    // No row of any table states Confirmed for an outbound-call Gap.
    for row in table(&artifact.content, "Unclassified") {
        let row = cells(row);
        if row[1] == "Outbound HTTP call" {
            assert_eq!(row[8], "Gap", "{row:?}");
        }
    }
}

// AC-0660: the same graph yields the same bytes whatever order the store or
// the caller hands the facts and the flows over in, and the selected
// compiler agrees.
#[test]
fn two_exports_are_byte_identical_whatever_the_input_order() {
    let mut f = system();
    f.http(
        (WEB, "src/pay.ts"),
        "https://api.maps.example",
        ("checkout", 900),
    );
    f.outbound(
        (API, "billing.py"),
        10,
        "reconcile",
        "https://api.tax.example/v2/rates",
    );
    let flows = f.flows();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let first = f.artifact(mode);
        assert_eq!(f.artifact(mode), first);
        assert!(first.verify_identity());
        let mut nodes: Vec<Node> = f.nodes.iter().rev().cloned().collect();
        // A fact handed over twice is listed once.
        nodes.extend(f.nodes.iter().take(5).cloned());
        let mut edges: Vec<Edge> = f.edges.iter().rev().cloned().collect();
        edges.rotate_left(7);
        let reversed: Vec<Flow> = flows.iter().rev().cloned().collect();
        let shuffled = compile_spec(&nodes, &edges, &reversed, mode, &BTreeSet::new())
            .artifacts
            .into_iter()
            .find(|artifact| artifact.id == ID)
            .unwrap();
        assert_eq!(shuffled, first);

        let selected = compile_spec_selected(
            &nodes,
            &edges,
            &reversed,
            mode,
            &BTreeSet::new(),
            &[ID],
            SelectedCompileLimits {
                max_bytes: 1 << 20,
                adr: crate::AdrLimits {
                    max_rows: 1000,
                    max_output_bytes: 1 << 20,
                },
            },
        )
        .unwrap();
        assert_eq!(selected, [SelectedArtifactOutcome::Produced(first.clone())]);

        // An output bound too small yields no partial document.
        let limited = compile_spec_selected(
            &nodes,
            &edges,
            &reversed,
            mode,
            &BTreeSet::new(),
            &[ID],
            SelectedCompileLimits {
                max_bytes: first.content.len() / 2,
                adr: crate::AdrLimits {
                    max_rows: 1000,
                    max_output_bytes: 1 << 20,
                },
            },
        )
        .unwrap();
        assert!(matches!(
            limited.as_slice(),
            [SelectedArtifactOutcome::Limited { artifact_id, .. }] if artifact_id == ID
        ));
    }
}

// AC-0660 (ADR-0058): every assertion of the artifact has a row in the
// export's fact index that names this artifact, each fact is cited once, and
// the ids it cites pass the export's citation check.
#[test]
fn every_row_cites_a_fact_that_resolves_in_the_fact_index() {
    let mut f = system();
    f.http(
        (WEB, "src/pay.ts"),
        "https://api.maps.example",
        ("checkout", 900),
    );
    let flows = f.flows();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle =
            try_compile_spec_with_coverage(&f.nodes, &f.edges, &flows, mode, &BTreeSet::new(), &[])
                .expect("the citations resolve");
        let artifact = bundle.artifacts.iter().find(|a| a.id == ID).unwrap();
        let index = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "fact-index-json")
            .unwrap();
        let index: FactIndex = serde_json::from_str(&index.content).unwrap();
        let subjects: BTreeSet<&str> = artifact
            .assertions
            .iter()
            .map(|assertion| assertion.subject_id.as_str())
            .collect();
        assert_eq!(subjects.len(), artifact.assertions.len(), "each fact once");
        // The outside use with its resource, and the entities of its flow
        // with the relations that read and write them.
        for subject in [
            "res:acme/web@observed:http-api:https://api.maps.example",
            "data:acme/web@Customer",
            "data:acme/web@Order",
            "sym:acme/web@src/pay.ts#charge WRITES data:acme/web@Order",
            "gap:fetch:acme/web@src/pay.ts@120",
            "chan:kafka-topic:orders-shipped",
        ] {
            assert!(subjects.contains(subject), "{subject} in {subjects:?}");
        }
        let mut cited = Vec::new();
        for assertion in &artifact.assertions {
            let subject = |key: &FactKey| match key {
                FactKey::Node { id } => id.clone(),
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => format!("{source} {label} {destination}"),
            };
            let row = index
                .rows
                .iter()
                .find(|row| subject(&row.fact) == assertion.subject_id)
                .unwrap_or_else(|| panic!("{} has a row", assertion.subject_id));
            assert_eq!(row.citation_id, citation_id(&row.fact));
            assert!(row.cited_by.iter().any(|by| by == ID));
            cited.push(row.citation_id.clone());
        }
        check_citations(&index, cited.iter().map(String::as_str)).expect("every id resolves");
    }
}
