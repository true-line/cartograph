//! Channel-identity resolution tests (US-0004). Fixtures run the real
//! chain: TS source → adapter event-site detection → stitch.

use super::*;

fn extract_sites(dir: &Path) -> Vec<EventSite> {
    let id = adapters_lang_ts::SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    adapters_lang_ts::extract_dir(dir, &id)
        .expect("extract")
        .event_sites
}

fn stitch_dir(dir: &Path) -> Extraction {
    let sites = extract_sites(dir);
    let cfg = ConfigIndex::from_dir(dir).expect("config scan");
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    stitch(&sites, &cfg, &id)
}

fn confidence(props: &serde_json::Value) -> &str {
    props["prov"]["confidence_tier"]
        .as_str()
        .expect("prov on every fact")
}

// AC-0010: literal channel ids on both sides match into one Channel with
// Confirmed PUBLISHES/SUBSCRIBES edges. (T-0010)
#[test]
fn literal_channel_ids_link_producer_and_consumer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("bus.ts"),
        r#"
import { EventEmitter } from 'node:events';
const bus = new EventEmitter();
export function placeOrder() {
  bus.emit('order.placed', { id: 1 });
}
export function onOrderPlaced() {
  bus.on('order.placed', handle);
}
function handle() {}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let channels: Vec<_> = out.nodes.iter().filter(|n| n.label == "Channel").collect();
    assert_eq!(channels.len(), 1, "both sides resolve to the same channel");
    let chan = channels[0];
    assert_eq!(chan.id, "chan:inproc-event:order.placed");
    assert_eq!(chan.props["identity"], "order.placed");
    assert_eq!(confidence(&chan.props), "Confirmed");

    let publish = out
        .edges
        .iter()
        .find(|e| e.label == "PUBLISHES")
        .expect("publish edge");
    let subscribe = out
        .edges
        .iter()
        .find(|e| e.label == "SUBSCRIBES")
        .expect("subscribe edge");
    assert_eq!(publish.src, "sym:test@bus.ts#placeOrder");
    assert_eq!(publish.dst, chan.id);
    assert_eq!(subscribe.src, "sym:test@bus.ts#onOrderPlaced");
    assert_eq!(subscribe.dst, chan.id);
    assert_eq!(confidence(&publish.props), "Confirmed");
    assert_eq!(publish.props["resolver"], "literal");
    // The registry file that recognized the site is named in full (#594).
    assert_eq!(publish.props["registry"], "ts-2026.10");
    let source = &publish.props["registry_sources"][0];
    assert_eq!(source["name"], "builtin:event-sdks.json");
    assert_eq!(source["schema"], "cartograph-event-sdk-registry@1");
    assert_eq!(source["content_hash"].as_str().map(str::len), Some(64));
    // No Gap emitted for resolved sites.
    assert!(out.nodes.iter().all(|n| n.label != "Gap"));
}

// AC-0011: a channel id from an env file present in the repo resolves via
// the config resolver, still Confirmed. (T-0011)
#[test]
fn config_resolved_channel_is_confirmed_via_env_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "ORDERS_QUEUE=https://sqs.example/orders\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("send.ts"),
        r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const client = new SQSClient({});
export function enqueue(body: string) {
  return client.send(new SendMessageCommand({
    QueueUrl: process.env.ORDERS_QUEUE,
    MessageBody: body,
  }));
}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let chan = out
        .nodes
        .iter()
        .find(|n| n.label == "Channel")
        .expect("env ref resolved to a channel");
    assert_eq!(chan.id, "chan:sqs-queue:https://sqs.example/orders");
    assert_eq!(confidence(&chan.props), "Confirmed");
    let publish = out
        .edges
        .iter()
        .find(|e| e.label == "PUBLISHES")
        .expect("publish edge");
    assert_eq!(publish.props["resolver"], "config:.env");
    assert_eq!(publish.src, "sym:test@send.ts#enqueue");
}

// AC-0012: a runtime-computed channel id cannot resolve at T0; the hop
// escalates (ladder above T0 is empty until M6/M7) and emits an explicit
// Gap with a reason — never a silent completion (R-INT-4). (T-0012)
#[test]
fn computed_channel_emits_gap_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dynamic.ts"),
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
export function notify(tenant: string) {
  bus.emit(channelFor(tenant), {});
}
function channelFor(tenant: string): string { return 'tenant.' + tenant; }
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    assert!(
        out.nodes.iter().all(|n| n.label != "Channel"),
        "computed identity must not fabricate a channel"
    );
    let gap = out
        .nodes
        .iter()
        .find(|n| n.label == "Gap")
        .expect("explicit Gap node");
    assert_eq!(gap.props["reason"], "runtime-computed channel identity");
    assert_eq!(gap.props["raw"], "channelFor(tenant)");
    assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    assert_eq!(confidence(&gap.props), "Gap");
    // The branch is truncated at the Gap, not dropped: the edge still exists.
    let publish = out
        .edges
        .iter()
        .find(|e| e.label == "PUBLISHES")
        .expect("edge to the Gap");
    assert_eq!(publish.dst, gap.id);
    assert_eq!(confidence(&publish.props), "Gap");
    assert_eq!(
        publish.props["registry_sources"][0]["name"],
        "builtin:event-sdks.json"
    );
}

// AC-0012 (env-ref miss): an env ref with no matching key in any env file
// is unresolved — Gap with the key named in the reason, not a guess.
#[test]
fn missing_env_key_emits_gap_naming_the_key() {
    let dir = tempfile::tempdir().unwrap();
    // No .env file at all.
    std::fs::write(
        dir.path().join("send.ts"),
        r#"
import { SNSClient, PublishCommand } from '@aws-sdk/client-sns';
const sns = new SNSClient({});
export function announce() {
  return sns.send(new PublishCommand({ TopicArn: process.env.ANNOUNCE_TOPIC }));
}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let gap = out
        .nodes
        .iter()
        .find(|n| n.label == "Gap")
        .expect("gap node");
    assert_eq!(
        gap.props["reason"],
        "env key ANNOUNCE_TOPIC not found in any env file in the repo"
    );
}

// Config resolver mechanics: parsing, precedence, determinism (US-0014).
#[test]
fn config_index_parses_env_files_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "# comment\nORDERS_TOPIC=orders\nexport QUOTED='q-topic'\nBAD LINE\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".env.local"),
        "ORDERS_TOPIC=local-override\nEXTRA=x\n",
    )
    .unwrap();

    let cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    // Sorted file order, first definition wins: `.env` < `.env.local`.
    assert_eq!(cfg.resolve("ORDERS_TOPIC"), Some(("orders", ".env")));
    assert_eq!(cfg.resolve("QUOTED"), Some(("q-topic", ".env")));
    assert_eq!(cfg.resolve("EXTRA"), Some(("x", ".env.local")));
    assert_eq!(cfg.resolve("MISSING"), None);
}

// Kafka topics stitch across files: producer and consumer in different
// modules, both literal, one channel.
#[test]
fn kafka_producer_and_consumer_stitch_across_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("producer.ts"),
        r#"
import { Kafka } from 'kafkajs';
const kafka = new Kafka({ clientId: 'a' });
const producer = kafka.producer();
export async function publishOrder() {
  await producer.send({ topic: 'orders', messages: [] });
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("consumer.ts"),
        r#"
import { Kafka } from 'kafkajs';
const kafka = new Kafka({ clientId: 'b' });
const consumer = kafka.consumer({ groupId: 'g' });
export async function listen() {
  await consumer.subscribe({ topic: 'orders' });
}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let channels: Vec<_> = out.nodes.iter().filter(|n| n.label == "Channel").collect();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].id, "chan:kafka-topic:orders");
    assert!(
        out.edges
            .iter()
            .any(|e| e.label == "PUBLISHES" && e.src == "sym:test@producer.ts#publishOrder")
    );
    assert!(
        out.edges
            .iter()
            .any(|e| e.label == "SUBSCRIBES" && e.src == "sym:test@consumer.ts#listen")
    );
}

// --- FETCHES resolution (US-0005, AC-0014) ---------------------------------

use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};

fn fetch_site(method: &str, url: IdentityExpr) -> FetchSite {
    FetchSite {
        method: method.into(),
        url,
        symbol: Some("sym:test@app.tsx#Orders".into()),
        path: "app.tsx".into(),
        byte_start: 10,
        byte_end: 40,
        base: None,
        entry: None,
    }
}

fn eps(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

// AC-0014: a resolvable fetch URL matching one endpoint is a Confirmed
// FETCHES edge — exact and :param-template routes both match. (T-0014)
#[test]
fn resolvable_fetch_urls_confirm_against_endpoints() {
    let endpoints = eps(&["ep:test@GET:/api/users/:id", "ep:test@POST:/api/orders"]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("POST", IdentityExpr::Literal("/api/orders".into())),
            // Query string stripped; :id template matches a concrete segment.
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://api.example.com/api/users/42?full=1".into()),
            ),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let fetches: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES")
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert_eq!(
        fetches,
        [
            ("sym:test@app.tsx#Orders", "ep:test@POST:/api/orders"),
            ("sym:test@app.tsx#Orders", "ep:test@GET:/api/users/:id"),
        ]
    );
    for e in out.edges.iter().filter(|e| e.label == "FETCHES") {
        assert_eq!(confidence(&e.props), "Confirmed");
    }
    assert!(out.nodes.iter().all(|n| n.label != "Gap"));
}

// AC-0434: a method-less route (`ANY`, Go's `HandleFunc("/x")` or actix
// `web::route()`) serves every method; a method route beside it on the same
// path makes the match ambiguous rather than silently picking one.
#[test]
fn method_less_routes_match_every_fetch_method() {
    let endpoints = eps(&[
        "ep:test@ANY:/users",
        "ep:test@ANY:/orders",
        "ep:test@POST:/orders",
    ]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("GET", IdentityExpr::Literal("/users".into())),
            fetch_site("DELETE", IdentityExpr::Literal("/users".into())),
            fetch_site("POST", IdentityExpr::Literal("/orders".into())),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let bound: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES")
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(bound[..2], ["ep:test@ANY:/users", "ep:test@ANY:/users"]);
    let reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .filter_map(|n| n.props["reason"].as_str())
        .collect();
    assert_eq!(
        reasons,
        ["ambiguous endpoint match for POST /orders (2 candidates)"]
    );
}

// AC-0434: a HEAD fetch binds a GET route only where the caller names that
// route as one its framework also serves for HEAD (Go's `ServeMux`).
#[test]
fn head_fetches_bind_get_routes_only_where_the_framework_serves_head() {
    let endpoints = eps(&["ep:test@GET:/health", "ep:test@GET:/ready"]);
    let serves_head = ["ep:test@GET:/health".to_string()].into_iter().collect();
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches_with(
        &[
            fetch_site("HEAD", IdentityExpr::Literal("/health".into())),
            fetch_site("HEAD", IdentityExpr::Literal("/ready".into())),
        ],
        &endpoints,
        &serves_head,
        &BTreeMap::new(),
        &cfg,
        &id,
    );
    let bound: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES")
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(bound[0], "ep:test@GET:/health");
    let reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .filter_map(|n| n.props["reason"].as_str())
        .collect();
    assert_eq!(reasons, ["no recovered endpoint matches HEAD /ready"]);
}

// AC-0014: everything unresolvable escalates to an explicit Gap with a
// reason — computed URL, no matching endpoint, ambiguous match. (T-0014)
#[test]
fn unresolvable_fetches_emit_gaps_with_reasons() {
    let endpoints = eps(&["ep:test@GET:/api/users/:id", "ep:test@GET:/api/:section/42"]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("GET", IdentityExpr::Computed("buildUrl()".into())),
            fetch_site("DELETE", IdentityExpr::Literal("/api/nothing".into())),
            // Matches both template endpoints — ambiguous is not Confirmed.
            fetch_site("GET", IdentityExpr::Literal("/api/users/42".into())),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .map(|n| n.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons.len(), 3);
    assert_eq!(reasons[0], "runtime-computed fetch URL");
    assert_eq!(
        reasons[1],
        "no recovered endpoint matches DELETE /api/nothing"
    );
    assert!(reasons[2].starts_with("ambiguous endpoint match for GET /api/users/42"));
    // Every gap edge is Gap-confidence and the site is never dropped.
    let gap_edges = out.edges.iter().filter(|e| e.label == "FETCHES").count();
    assert_eq!(gap_edges, 3);
}

// AC-0206 (#445): endpoint routes keep a literal trailing slash, so a fetch
// that differs from a route only by that slash is not the same route — it
// never confirms against it and escalates to a Gap instead. (T-0206)
#[test]
fn trailing_slash_only_differences_never_confirm() {
    let endpoints = eps(&["ep:test@GET:/api/", "ep:test@GET:/v1/items"]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("GET", IdentityExpr::Literal("/api".into())),
            fetch_site("GET", IdentityExpr::Literal("/v1/items/".into())),
            fetch_site("GET", IdentityExpr::Literal("/api/".into())),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let fetches: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES")
        .map(|e| (e.dst.as_str(), confidence(&e.props)))
        .collect();
    assert_eq!(
        fetches,
        [
            ("gap:fetch:test@app.tsx@10", "Gap"),
            ("gap:fetch:test@app.tsx@10", "Gap"),
            ("ep:test@GET:/api/", "Confirmed"),
        ]
    );
    let reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .map(|n| n.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        [
            "no recovered endpoint matches GET /api",
            "no recovered endpoint matches GET /v1/items/"
        ]
    );
}

// An env-resolved base URL confirms through the config resolver, same as
// channel identities (AC-0011 discipline applied to AC-0014).
#[test]
fn env_resolved_fetch_url_confirms() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "ORDERS_API=https://api.example.com/api/orders\n",
    )
    .unwrap();
    let cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[fetch_site(
            "POST",
            IdentityExpr::EnvRef("ORDERS_API".into()),
        )],
        &eps(&["ep:test@POST:/api/orders"]),
        &cfg,
        &id,
    );
    let edge = out.edges.iter().find(|e| e.label == "FETCHES").unwrap();
    assert_eq!(edge.dst, "ep:test@POST:/api/orders");
    assert_eq!(edge.props["resolver"], "config:.env");
    assert_eq!(confidence(&edge.props), "Confirmed");
}

// AC-0002: manifest-declared identities override env files and resolve
// with the manifest as the named source. (T-0002)
#[test]
fn manifest_identities_override_env_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "ORDERS_TOPIC=from-dotenv\n").unwrap();
    let mut cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    let mut manifest = std::collections::BTreeMap::new();
    manifest.insert("ORDERS_TOPIC".to_string(), "from-manifest".to_string());
    manifest.insert("EXTRA_QUEUE".to_string(), "q".to_string());
    cfg.apply_manifest(&manifest, "cartograph.system.toml");

    assert_eq!(
        cfg.resolve("ORDERS_TOPIC"),
        Some(("from-manifest", "cartograph.system.toml")),
        "author declaration beats checked-in env files"
    );
    assert_eq!(
        cfg.resolve("EXTRA_QUEUE"),
        Some(("q", "cartograph.system.toml"))
    );
}

// --- EventBridge composite identity at T1 (#573, AC-0256, T-0256) --------
// Each case runs the real chain: TS PutEvents site → adapter → stitch →
// OTLP trace → `dynamic::apply_trace`.

const EB_SITE: &str = r#"
import { PutEventsCommand } from '@aws-sdk/client-eventbridge';
export function publish(kind: string) {
  return new PutEventsCommand({
    Entries: [{ EventBusName: 'orders', Source: 'shop', DetailType: `order.${kind}`, Detail: '{}' }],
  });
}
"#;

/// One OTLP JSON line; each span is `(span_id, [(attribute, value)])`.
fn otlp(spans: &[(&str, &[(&str, &str)])]) -> String {
    let spans: Vec<serde_json::Value> = spans
        .iter()
        .map(|(span_id, attrs)| {
            serde_json::json!({
                "traceId": "trace-eb",
                "spanId": span_id,
                "name": "PutEvents",
                "attributes": attrs
                    .iter()
                    .map(|(k, v)| serde_json::json!({"key": k, "value": {"stringValue": v}}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    format!(
        "{}\n",
        serde_json::json!({"resourceSpans": [{"resource": {"attributes": []},
            "scopeSpans": [{"spans": spans}]}]})
    )
}

const EB: (&str, &str) = ("messaging.system", "aws_eventbridge");
const AT_SITE: (&str, &str) = ("code.file.path", "/srv/shop/pub.ts");

fn eb_t1(source: &str, trace: &str) -> (Extraction, dynamic::TraceEnrichment) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pub.ts"), source).unwrap();
    let mut out = stitch_dir(dir.path());
    let trace = dynamic::parse_otlp_jsonl(trace).expect("trace parses");
    let report = dynamic::apply_trace(&mut out.nodes, &mut out.edges, &trace, "trace.jsonl");
    (out, report)
}

fn eb_gap(out: &Extraction) -> &Node {
    out.nodes
        .iter()
        .find(|n| n.label == "Gap" && n.props["kind"] == "eventbridge-detail-type")
        .expect("EventBridge Gap kept")
}

#[test]
fn eventbridge_static_site_records_bus_and_keeps_detail_type_gap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pub.ts"), EB_SITE).unwrap();
    let out = stitch_dir(dir.path());
    let gap = eb_gap(&out);
    assert_eq!(gap.props["components"]["bus"], "orders");
    assert_eq!(gap.props["raw"], "`order.${kind}`");
    assert!(gap.props.get("batch").is_none());
}

#[test]
fn eventbridge_attributed_complete_observation_fills_gap_with_one_identity() {
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
        ("cloud.account.id", "111122223333"),
        ("cloud.region", "us-east-1"),
    ];
    let (out, report) = eb_t1(EB_SITE, &otlp(&[("s1", span)]));
    assert_eq!(report.channel_gaps_resolved, 1);
    assert!(out.nodes.iter().all(|n| n.label != "Gap"));
    let id =
        "chan:eventbridge-scoped:detail=order.placed|bus=orders|scope=111122223333:us-east-1:?";
    let channel = out.nodes.iter().find(|n| n.id == id).expect("one identity");
    assert_eq!(channel.props["identity"], "order.placed");
    assert_eq!(channel.props["components"]["bus"], "orders");
    assert_eq!(channel.props["components"]["region"], "us-east-1");
    assert_eq!(channel.props["bus_source"], "trace");
    assert_eq!(channel.props["prov"]["tier"], "Dynamic");
    assert_eq!(channel.props["observed"]["span_id"], "s1");
    let publish = out.edges.iter().find(|e| e.label == "PUBLISHES").unwrap();
    assert_eq!(publish.dst, id);
    assert_eq!(publish.props["resolver"], dynamic::OTEL_EXTRACTOR_ID);
}

#[test]
fn eventbridge_static_bus_completes_detail_only_observation() {
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let (out, report) = eb_t1(EB_SITE, &otlp(&[("s1", span)]));
    assert_eq!(report.channel_gaps_resolved, 1);
    let channel = out
        .nodes
        .iter()
        .find(|n| n.id == "chan:eventbridge-scoped:detail=order.placed|bus=orders|scope=?:?:?")
        .expect("static bus plus observed detail type");
    assert_eq!(channel.props["bus_source"], "static");
}

#[test]
fn eventbridge_bus_only_observation_keeps_gap_and_preserves_bus() {
    let span: &[(&str, &str)] = &[EB, AT_SITE, ("messaging.destination.name", "orders")];
    let (out, report) = eb_t1(EB_SITE, &otlp(&[("s1", span), ("s2", span)]));
    assert_eq!(report.channel_gaps_resolved, 0);
    let gap = eb_gap(&out);
    assert_eq!(gap.props["t1_reason"], "detail type unobserved");
    assert_eq!(
        gap.props["attempted_tiers"],
        serde_json::json!(["T0", "T1"])
    );
    let bus_id = "chan:eventbridge-scoped:bus=orders|scope=?:?:?";
    assert_eq!(gap.props["t1_observed_bus"], serde_json::json!([bus_id]));
    let bus = out
        .nodes
        .iter()
        .find(|n| n.id == bus_id)
        .expect("bus kept as its own fact");
    assert_eq!(bus.props["kind"], "eventbridge-bus");
    assert!(
        out.edges.iter().all(|e| e.dst != bus_id),
        "no edge claimed to the bus"
    );
}

#[test]
fn eventbridge_incompatible_observation_keeps_gap_with_contradiction() {
    let detail: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "invoice.paid"),
    ];
    let (out, _) = eb_t1(EB_SITE, &otlp(&[("s1", detail)]));
    assert_eq!(
        eb_gap(&out).props["t1_reason"],
        "contradiction: observed detail type invoice.paid does not fit static `order.${kind}`"
    );
    let bus: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "billing"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let (out, _) = eb_t1(EB_SITE, &otlp(&[("s1", bus)]));
    assert_eq!(
        eb_gap(&out).props["t1_reason"],
        "contradiction: observed bus billing contradicts static bus orders"
    );
}

#[test]
fn eventbridge_same_detail_type_on_distinct_buses_or_scopes_keeps_gap() {
    let unbussed = EB_SITE.replace("EventBusName: 'orders', ", "");
    let on = |bus: &'static str, region: &'static str| -> Vec<(&'static str, &'static str)> {
        vec![
            EB,
            AT_SITE,
            ("messaging.destination.name", bus),
            (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
            ("cloud.region", region),
        ]
    };
    let (a, b) = (on("orders", "us-east-1"), on("audit", "us-east-1"));
    let (out, _) = eb_t1(&unbussed, &otlp(&[("s1", &a), ("s2", &b)]));
    assert_eq!(eb_gap(&out).props["t1_reason"], "2 distinct identities");
    let c = on("orders", "eu-west-1");
    let (out, _) = eb_t1(&unbussed, &otlp(&[("s1", &a), ("s3", &c)]));
    assert_eq!(eb_gap(&out).props["t1_reason"], "2 distinct identities");
}

#[test]
fn eventbridge_observation_from_unrelated_site_keeps_gap() {
    let span: &[(&str, &str)] = &[
        EB,
        ("code.file.path", "/srv/billing/invoice.ts"),
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let (out, report) = eb_t1(EB_SITE, &otlp(&[("s1", span)]));
    assert_eq!(report.channel_gaps_unresolved, 1);
    assert_eq!(
        eb_gap(&out).props["t1_reason"],
        "source attribution missing"
    );
    // Unlocated spans are not attributed either: no unique-gap fallback.
    let unlocated: &[(&str, &str)] = &[EB, (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed")];
    let (out, _) = eb_t1(EB_SITE, &otlp(&[("s1", unlocated)]));
    assert_eq!(
        eb_gap(&out).props["t1_reason"],
        "source attribution missing"
    );
}

#[test]
fn eventbridge_repeated_identical_spans_count_as_one_identity() {
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let (out, report) = eb_t1(EB_SITE, &otlp(&[("s1", span), ("s2", span), ("s3", span)]));
    assert_eq!(report.channel_gaps_resolved, 1);
    let channels: Vec<_> = out.nodes.iter().filter(|n| n.label == "Channel").collect();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].props["observed"]["span_id"], "s1");
}

#[test]
fn eventbridge_several_distinct_detail_types_keep_gap() {
    let placed: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let shipped: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.shipped"),
    ];
    let (out, report) = eb_t1(EB_SITE, &otlp(&[("s1", placed), ("s2", shipped)]));
    assert_eq!(report.channel_gaps_resolved, 0);
    assert_eq!(eb_gap(&out).props["t1_reason"], "2 distinct identities");
    assert!(
        out.nodes
            .iter()
            .all(|n| n.props["kind"] != "eventbridge-detail-type" || n.label == "Gap")
    );
}

#[test]
fn eventbridge_batched_entries_keep_explicit_batch_gap() {
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let batched = EB_SITE.replace(
        "Detail: '{}' }],",
        "Detail: '{}' }, { Source: 'shop', DetailType: 'order.audit', Detail: '{}' }],",
    );
    let (out, _) = eb_t1(&batched, &otlp(&[("s1", span)]));
    let gap = eb_gap(&out);
    assert_eq!(gap.props["batch"], "2 entries");
    assert_eq!(
        gap.props["t1_reason"],
        "batch correlation unsupported: 2 entries in one PutEvents call"
    );
    let mapped = r#"
import { PutEventsCommand } from '@aws-sdk/client-eventbridge';
export function publishAll(events: { kind: string }[]) {
  return new PutEventsCommand({ Entries: events.map((e) => ({ DetailType: e.kind })) });
}
"#;
    let (out, _) = eb_t1(mapped, &otlp(&[("s1", span)]));
    assert_eq!(
        eb_gap(&out).props["t1_reason"],
        "batch correlation unsupported: computed entries in one PutEvents call"
    );
}

#[test]
fn eventbridge_literal_site_is_confirmed_at_t0_and_untouched_by_trace() {
    let literal = EB_SITE.replace("`order.${kind}`", "'order.placed'");
    let span: &[(&str, &str)] = &[EB, AT_SITE, ("messaging.destination.name", "orders")];
    let (out, report) = eb_t1(&literal, &otlp(&[("s1", span)]));
    assert_eq!(report, dynamic::TraceEnrichment::default());
    let channel = out
        .nodes
        .iter()
        .find(|n| n.id == "chan:eventbridge-detail-type:order.placed")
        .expect("stored T0 id unchanged");
    assert_eq!(confidence(&channel.props), "Confirmed");
    let publish = out.edges.iter().find(|e| e.label == "PUBLISHES").unwrap();
    assert_eq!(publish.props["components"]["bus"], "orders");
}

// Review regressions on #599 (Codex cross-review and automated review).

#[test]
fn eventbridge_literal_neighbour_in_same_file_blocks_file_only_attribution() {
    let source = format!(
        "{EB_SITE}\nexport function other() {{ return new PutEventsCommand({{ Entries: [{{ EventBusName: 'orders', DetailType: 'order.placed', Detail: '{{}}' }}] }}); }}"
    );
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let (out, report) = eb_t1(&source, &otlp(&[("literal-site-span", span)]));
    assert_eq!(report.channel_gaps_resolved, 0);
    assert_eq!(
        eb_gap(&out).props["t1_reason"],
        "source attribution ambiguous: an observed code path matches 2 EventBridge sites"
    );
}

#[test]
fn eventbridge_composite_id_never_aliases_a_legacy_literal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pub.ts"), EB_SITE).unwrap();
    let colliding = EB_SITE.replace("`order.${kind}`", "'order.placed|bus=orders|scope=?:?:?'");
    std::fs::write(dir.path().join("other.ts"), colliding).unwrap();
    let mut out = stitch_dir(dir.path());
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let trace = dynamic::parse_otlp_jsonl(&otlp(&[("s1", span)])).unwrap();
    dynamic::apply_trace(&mut out.nodes, &mut out.edges, &trace, "trace.jsonl");
    let publish = out
        .edges
        .iter()
        .find(|e| e.src.contains("pub.ts") && e.label == "PUBLISHES")
        .expect("site edge");
    let channel = out.nodes.iter().find(|n| n.id == publish.dst).unwrap();
    assert_eq!(channel.props["identity"], "order.placed");
    assert!(
        out.nodes
            .iter()
            .any(|n| n.id == "chan:eventbridge-detail-type:order.placed|bus=orders|scope=?:?:?")
    );
}

#[test]
fn eventbridge_conflict_split_across_trace_files_keeps_gap_in_any_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pub.ts"), EB_SITE).unwrap();
    let placed: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let shipped: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.shipped"),
    ];
    let a = dynamic::parse_otlp_jsonl(&otlp(&[("a1", placed)])).unwrap();
    let b = dynamic::parse_otlp_jsonl(&otlp(&[("b1", shipped)])).unwrap();
    for order in [
        [(&a, "a.jsonl"), (&b, "b.jsonl")],
        [(&b, "b.jsonl"), (&a, "a.jsonl")],
    ] {
        let mut out = stitch_dir(dir.path());
        let report = dynamic::apply_traces(&mut out.nodes, &mut out.edges, &order);
        assert_eq!(report.channel_gaps_resolved, 0);
        assert_eq!(eb_gap(&out).props["t1_reason"], "2 distinct identities");
    }
    // Agreeing files fill the Gap and cite the same evidence in any order.
    let again = dynamic::parse_otlp_jsonl(&otlp(&[("b2", placed)])).unwrap();
    let mut cited = Vec::new();
    for order in [
        [(&a, "a.jsonl"), (&again, "b.jsonl")],
        [(&again, "b.jsonl"), (&a, "a.jsonl")],
    ] {
        let mut out = stitch_dir(dir.path());
        let report = dynamic::apply_traces(&mut out.nodes, &mut out.edges, &order);
        assert_eq!(report.channel_gaps_resolved, 1);
        let channel = out
            .nodes
            .iter()
            .find(|n| n.id.starts_with("chan:eventbridge-scoped:detail="))
            .unwrap();
        cited.push(channel.props["observed"]["span_id"].clone());
    }
    assert_eq!(
        cited,
        vec![serde_json::json!("a1"), serde_json::json!("a1")]
    );
}

#[test]
fn eventbridge_batched_site_still_preserves_observed_bus() {
    let batched = EB_SITE.replace(
        "Detail: '{}' }],",
        "Detail: '{}' }, { Source: 'shop', DetailType: 'order.audit', Detail: '{}' }],",
    );
    let span: &[(&str, &str)] = &[EB, AT_SITE, ("messaging.destination.name", "orders")];
    let (out, _) = eb_t1(&batched, &otlp(&[("s1", span)]));
    let bus_id = "chan:eventbridge-scoped:bus=orders|scope=?:?:?";
    assert_eq!(
        eb_gap(&out).props["t1_observed_bus"],
        serde_json::json!([bus_id])
    );
    assert!(out.nodes.iter().any(|n| n.id == bus_id));
}

#[test]
fn eventbridge_suffix_aliasing_files_keep_both_gaps() {
    // Codex re-review on #599: `publish.ts` and `src/publish.ts` both match
    // `/srv/shop/src/publish.ts` by suffix, so one span proves neither.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("publish.ts"), EB_SITE).unwrap();
    std::fs::write(dir.path().join("src/publish.ts"), EB_SITE).unwrap();
    let mut out = stitch_dir(dir.path());
    let span: &[(&str, &str)] = &[
        EB,
        ("code.file.path", "/srv/shop/src/publish.ts"),
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let trace = dynamic::parse_otlp_jsonl(&otlp(&[("s1", span)])).unwrap();
    let report = dynamic::apply_trace(&mut out.nodes, &mut out.edges, &trace, "trace.jsonl");
    assert_eq!(report.channel_gaps_resolved, 0);
    let gaps: Vec<_> = out.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 2);
    for gap in gaps {
        assert_eq!(
            gap.props["t1_reason"],
            "source attribution ambiguous: an observed code path matches 2 EventBridge sites"
        );
    }
}

#[test]
fn eventbridge_same_path_in_two_repos_is_not_unique_attribution() {
    // Codex re-review on #599: repository identity is part of the site, so
    // identical paths and offsets in two repos never collapse into one site.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pub.ts"), EB_SITE).unwrap();
    let sites = extract_sites(dir.path());
    let cfg = ConfigIndex::default();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for repo in ["shop", "billing"] {
        let out = stitch(
            &sites,
            &cfg,
            &SourceId {
                repo,
                commit: "deadbeef",
            },
        );
        nodes.extend(out.nodes);
        edges.extend(out.edges);
    }
    let span: &[(&str, &str)] = &[
        EB,
        AT_SITE,
        ("messaging.destination.name", "orders"),
        (dynamic::EVENTBRIDGE_DETAIL_TYPE_ATTR, "order.placed"),
    ];
    let trace = dynamic::parse_otlp_jsonl(&otlp(&[("s1", span)])).unwrap();
    let report = dynamic::apply_trace(&mut nodes, &mut edges, &trace, "trace.jsonl");
    assert_eq!(report.channel_gaps_resolved, 0);
    assert_eq!(report.channel_gaps_unresolved, 2);
}

// --- Cross-repo HTTP binding (#589) ---------------------------------------

/// Fetch Gaps a `web` repo with no endpoints of its own produces.
fn web_gaps(sites: &[FetchSite], cfg: &ConfigIndex) -> (Vec<Node>, Vec<Edge>) {
    let id = SourceId {
        repo: "web",
        commit: "deadbeef",
    };
    let out = stitch_fetches(sites, &[], cfg, &id);
    let gaps = out.nodes.into_iter().filter(|n| n.label == "Gap").collect();
    let edges = out
        .edges
        .into_iter()
        .filter(|e| e.label == "FETCHES")
        .collect();
    (gaps, edges)
}

fn service(name: &str, repo: &str, base_url: &str) -> HttpBinding {
    HttpBinding {
        service: name.into(),
        repo: repo.into(),
        base_url: base_url.into(),
        kind: BindingKind::DeclaredService,
        declared_in: "cartograph.system.toml".into(),
    }
}

fn route(name: &str, repo: &str, base_url: &str, prefix: &str, strip: bool) -> HttpBinding {
    HttpBinding {
        kind: BindingKind::GatewayRoute {
            prefix: prefix.into(),
            strip_prefix: strip,
        },
        ..service(name, repo, base_url)
    }
}

// AC-0240: a declared service's base URL joins the call to the one endpoint
// it reaches in the serving repo, citing the binding. (T-0240)
#[test]
fn declared_service_binding_joins_a_cross_repo_fetch() {
    let (gaps, edges) = web_gaps(
        &[fetch_site(
            "GET",
            IdentityExpr::Literal("https://Orders.example.com/v1/orders/7?full=1".into()),
        )],
        &ConfigIndex::default(),
    );
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@GET:/v1/orders/:id", "ep:api@POST:/v1/orders"]),
        &[service("orders", "api", "https://orders.example.com/")],
    );
    assert!(out.kept_nodes.is_empty() && out.kept_edges.is_empty());
    let [joined] = out.joined.as_slice() else {
        panic!("one join: {:?}", out.joined)
    };
    assert_eq!(joined.gap_id, gaps[0].id);
    assert_eq!(joined.edge.src, "sym:test@app.tsx#Orders");
    assert_eq!(joined.edge.dst, "ep:api@GET:/v1/orders/:id");
    assert_eq!(confidence(&joined.edge.props), "Confirmed");
    assert_eq!(joined.edge.props["prov"]["tier"], "Deterministic");
    assert_eq!(
        joined.edge.props["prov"]["extractor_id"],
        http_binding::EXTRACTOR_ID
    );
    assert_eq!(joined.edge.props["resolver"], "literal");
    assert_eq!(
        joined.edge.props["binding"],
        serde_json::json!([{
            "kind": "declared-service",
            "service": "orders",
            "repo": "api",
            "base_url": "https://orders.example.com/",
            "declared_in": "cartograph.system.toml",
        }])
    );
    // The source citation survives from the call site.
    assert_eq!(joined.edge.props["prov"]["evidence"][0]["path"], "app.tsx");
}

// AC-0240: a base URL read from config binds the same way and keeps the
// config file as the resolver. (T-0240)
#[test]
fn config_base_url_binding_cites_its_config_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "ORDERS_URL=https://orders.example.com/v1/orders\n",
    )
    .unwrap();
    let cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    let (gaps, edges) = web_gaps(
        &[fetch_site(
            "POST",
            IdentityExpr::EnvRef("ORDERS_URL".into()),
        )],
        &cfg,
    );
    assert_eq!(gaps[0].props["request"]["url_source"], "config:.env");
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@POST:/v1/orders"]),
        &[service("orders", "api", "https://orders.example.com")],
    );
    assert_eq!(out.joined.len(), 1);
    assert_eq!(out.joined[0].edge.dst, "ep:api@POST:/v1/orders");
    assert_eq!(out.joined[0].edge.props["resolver"], "config:.env");
}

// AC-0240: a gateway route forwards its prefix, stripped or kept, and never
// matches a longer segment that merely starts with it. (T-0240)
#[test]
fn gateway_routes_forward_on_whole_prefix_segments() {
    let (gaps, edges) = web_gaps(
        &[
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://gw.example.com/orders/v1/orders/7".into()),
            ),
            FetchSite {
                byte_start: 50,
                ..fetch_site(
                    "GET",
                    IdentityExpr::Literal("https://gw.example.com/api/items".into()),
                )
            },
            FetchSite {
                byte_start: 90,
                ..fetch_site(
                    "GET",
                    IdentityExpr::Literal("https://gw.example.com/ordersx/v1/orders/7".into()),
                )
            },
        ],
        &ConfigIndex::default(),
    );
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@GET:/v1/orders/:id", "ep:catalog@GET:/api/items"]),
        &[
            route("orders", "api", "https://gw.example.com", "/orders", true),
            route(
                "catalog",
                "catalog",
                "https://gw.example.com",
                "api/",
                false,
            ),
        ],
    );
    let joined: Vec<&str> = out.joined.iter().map(|j| j.edge.dst.as_str()).collect();
    assert_eq!(
        joined,
        ["ep:api@GET:/v1/orders/:id", "ep:catalog@GET:/api/items"]
    );
    assert_eq!(
        out.joined[0].edge.props["binding"][0]["kind"],
        "gateway-route"
    );
    assert_eq!(out.joined[0].edge.props["binding"][0]["strip_prefix"], true);
    let [kept] = out.kept_nodes.as_slice() else {
        panic!("one kept gap: {:?}", out.kept_nodes)
    };
    assert_eq!(
        kept.props["reason"],
        "no binding evidence: no declared service or gateway route covers https://gw.example.com/ordersx/v1/orders/7"
    );
}

// AC-0240 (#606 review): a root gateway prefix forwards every path under
// its base URL, stripped or kept, including the root itself. (T-0240)
#[test]
fn root_gateway_prefix_forwards_child_and_root_paths() {
    let (gaps, edges) = web_gaps(
        &[
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://gw.example.com/users".into()),
            ),
            FetchSite {
                byte_start: 50,
                ..fetch_site(
                    "GET",
                    IdentityExpr::Literal("https://gw.example.com".into()),
                )
            },
        ],
        &ConfigIndex::default(),
    );
    let endpoints = eps(&["ep:api@GET:/users", "ep:api@GET:/"]);
    for (prefix, strip) in [("/", true), ("/", false), ("", true)] {
        let out = stitch_cross_repo(
            &gaps,
            &edges,
            &endpoints,
            &[route("api", "api", "https://gw.example.com", prefix, strip)],
        );
        let joined: Vec<&str> = out.joined.iter().map(|j| j.edge.dst.as_str()).collect();
        assert_eq!(
            joined,
            ["ep:api@GET:/users", "ep:api@GET:/"],
            "prefix {prefix:?}, strip {strip}"
        );
    }
}

// AC-0240 (#606 review): an origin the manifest binds is deferred by the
// per-repo pass, so a same-repo route that merely shares the path never
// confirms the call; the system pass then binds it to the declared service.
// Undeclared origins still match in-repo as before. (T-0240)
#[test]
fn bound_origins_defer_same_repo_path_coincidences_to_the_binding() {
    let mut cfg = ConfigIndex::default();
    cfg.defer_origins(&["https://orders.example.com".to_string()].into());
    let id = SourceId {
        repo: "web",
        commit: "deadbeef",
    };
    let own = eps(&["ep:web@GET:/v1/orders"]);
    let out = stitch_fetches(
        &[
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://ORDERS.example.com/v1/orders".into()),
            ),
            FetchSite {
                byte_start: 50,
                ..fetch_site(
                    "GET",
                    IdentityExpr::Literal("https://bff.example.com/v1/orders".into()),
                )
            },
        ],
        &own,
        &cfg,
        &id,
    );
    let local: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES" && e.dst.starts_with("ep:"))
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(
        local,
        ["ep:web@GET:/v1/orders"],
        "only the undeclared origin"
    );
    let gaps: Vec<Node> = out.nodes.into_iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        gaps[0].props["reason"],
        "deferred to system binding: https://orders.example.com is bound in the system manifest"
    );
    let edges: Vec<Edge> = out
        .edges
        .into_iter()
        .filter(|e| e.dst.starts_with("gap:"))
        .collect();
    let mut endpoints = own.clone();
    endpoints.push("ep:api@GET:/v1/orders".into());
    let bound = stitch_cross_repo(
        &gaps,
        &edges,
        &endpoints,
        &[service("orders", "api", "https://orders.example.com")],
    );
    assert_eq!(bound.joined.len(), 1);
    assert_eq!(bound.joined[0].edge.dst, "ep:api@GET:/v1/orders");
}

// AC-0240/AC-0241 (#606 review): a declared base URL with a path offers both
// the full path and the path relative to the base. One match joins; matches
// under both readings that differ stay an ambiguous Gap. (T-0240, T-0241)
#[test]
fn declared_base_path_offers_both_readings_and_never_guesses_between_them() {
    let (gaps, edges) = web_gaps(
        &[fetch_site(
            "GET",
            IdentityExpr::Literal("https://example.com/api/orders".into()),
        )],
        &ConfigIndex::default(),
    );
    let bindings = [service("orders", "api", "https://example.com/api")];
    for routes in [["ep:api@GET:/orders"], ["ep:api@GET:/api/orders"]] {
        let out = stitch_cross_repo(&gaps, &edges, &eps(&routes), &bindings);
        assert_eq!(out.joined.len(), 1, "{routes:?}");
        assert_eq!(out.joined[0].edge.dst, routes[0]);
    }
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@GET:/orders", "ep:api@GET:/api/orders"]),
        &bindings,
    );
    assert!(out.joined.is_empty());
    assert_eq!(
        out.kept_nodes[0].props["reason"],
        "ambiguous cross-repo binding for GET https://example.com/api/orders: 2 equally supported targets"
    );
}

// AC-0241: without binding evidence the call stays a Gap whose reason says
// so, and the original same-repo reason is kept beside it. (T-0241)
#[test]
fn fetch_without_binding_evidence_stays_a_gap_with_its_reason() {
    let (gaps, edges) = web_gaps(
        &[fetch_site(
            "GET",
            IdentityExpr::Literal("https://unknown.example.com/x".into()),
        )],
        &ConfigIndex::default(),
    );
    let bindings = [service("orders", "api", "https://orders.example.com")];
    let endpoints = eps(&["ep:api@GET:/x"]);
    let out = stitch_cross_repo(&gaps, &edges, &endpoints, &bindings);
    assert!(out.joined.is_empty());
    assert_eq!(out.kept_nodes.len(), 1);
    let node = &out.kept_nodes[0];
    let edge = &out.kept_edges[0];
    for props in [&node.props, &edge.props] {
        assert_eq!(
            props["reason"],
            "no binding evidence: no declared service or gateway route covers https://unknown.example.com/x"
        );
        assert_eq!(props["t0_reason"], "no recovered endpoint matches GET /x");
    }
    assert_eq!(confidence(&edge.props), "Gap");
    // A second pass over its own output keeps the first reason, unchanged.
    let again = stitch_cross_repo(&out.kept_nodes, &out.kept_edges, &endpoints, &bindings);
    assert_eq!(again.kept_nodes[0].props, node.props);
    // No bindings at all, a relative URL or a computed URL: untouched.
    assert!(
        stitch_cross_repo(&gaps, &edges, &endpoints, &[])
            .kept_nodes
            .is_empty()
    );
    let (local, local_edges) = web_gaps(
        &[
            fetch_site("GET", IdentityExpr::Literal("/x".into())),
            FetchSite {
                byte_start: 50,
                ..fetch_site("GET", IdentityExpr::Computed("build()".into()))
            },
        ],
        &ConfigIndex::default(),
    );
    let out = stitch_cross_repo(&local, &local_edges, &endpoints, &bindings);
    assert!(out.joined.is_empty() && out.kept_nodes.is_empty());
}

// AC-0660 (#1843): every Gap the system pass keeps records what the declared
// bindings say about its origin, on the node and its Gap edge, and cites the
// bindings that name that origin. The projection never parses the sentence.
#[test]
fn kept_gaps_record_the_binding_state_of_their_origin() {
    let sites = [
        // No declared binding names this origin.
        fetch_site(
            "GET",
            IdentityExpr::Literal("https://third.example.com/x".into()),
        ),
        // A gateway origin, and no route with this prefix.
        FetchSite {
            byte_start: 50,
            ..fetch_site(
                "GET",
                IdentityExpr::Literal("https://gw.example.com/other/1".into()),
            )
        },
        // A binding covers it and no endpoint there matches.
        FetchSite {
            byte_start: 90,
            ..fetch_site(
                "DELETE",
                IdentityExpr::Literal("https://orders.example.com/v1/orders/7".into()),
            )
        },
        // Two equally supported endpoints.
        FetchSite {
            byte_start: 130,
            ..fetch_site(
                "GET",
                IdentityExpr::Literal("https://both.example.com/v1/orders/7".into()),
            )
        },
        // Computed methods: no endpoint is matched, the origin is still known.
        FetchSite {
            byte_start: 170,
            ..fetch_site(
                "?",
                IdentityExpr::Literal("https://third.example.com/y".into()),
            )
        },
        FetchSite {
            byte_start: 210,
            ..fetch_site(
                "?",
                IdentityExpr::Literal("https://ORDERS.example.com/v1/orders".into()),
            )
        },
    ];
    let (gaps, edges) = web_gaps(&sites, &ConfigIndex::default());
    assert_eq!(gaps.len(), sites.len());
    let endpoints = eps(&[
        "ep:api@GET:/v1/orders/:id",
        "ep:b1@GET:/v1/orders/:id",
        "ep:b2@GET:/v1/orders/:id",
    ]);
    let bindings = [
        service("orders", "api", "https://orders.example.com"),
        route("orders", "api", "https://gw.example.com", "/orders", true),
        service("one", "b1", "https://both.example.com"),
        service("two", "b2", "https://both.example.com"),
    ];
    let out = stitch_cross_repo(&gaps, &edges, &endpoints, &bindings);
    assert!(out.joined.is_empty());
    assert_eq!(out.kept_nodes.len(), sites.len());
    let kept = |url: &str| {
        let node = out
            .kept_nodes
            .iter()
            .find(|node| node.props["request"]["url"] == url)
            .unwrap_or_else(|| panic!("kept {url}"));
        let edge = out
            .kept_edges
            .iter()
            .find(|edge| edge.dst == node.id)
            .unwrap();
        assert_eq!(edge.props[BINDING_STATE], node.props[BINDING_STATE]);
        assert!(edge.props.get("binding").is_none());
        node
    };
    let services = |node: &Node| -> Vec<String> {
        node.props["binding"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|citation| {
                format!(
                    "{}:{}",
                    citation["kind"].as_str().unwrap(),
                    citation["service"].as_str().unwrap()
                )
            })
            .collect()
    };

    let none = kept("https://third.example.com/x");
    assert_eq!(none.props[BINDING_STATE], BINDING_STATE_NONE);
    assert!(none.props.get("binding").is_none());

    let declared = kept("https://gw.example.com/other/1");
    assert_eq!(declared.props[BINDING_STATE], BINDING_STATE_ORIGIN_DECLARED);
    assert_eq!(services(declared), ["gateway-route:orders"]);

    let names = kept("https://orders.example.com/v1/orders/7");
    assert_eq!(names.props[BINDING_STATE], BINDING_STATE_NAMES_REPOSITORY);
    assert_eq!(services(names), ["declared-service:orders"]);

    let ambiguous = kept("https://both.example.com/v1/orders/7");
    assert_eq!(ambiguous.props[BINDING_STATE], BINDING_STATE_AMBIGUOUS);
    assert!(ambiguous.props.get("binding").is_none());
    assert_eq!(ambiguous.props["candidates"].as_array().unwrap().len(), 2);

    // A computed method keeps its own reason and gains no cross-repo one.
    let computed = kept("https://third.example.com/y");
    assert_eq!(computed.props[BINDING_STATE], BINDING_STATE_NONE);
    assert_eq!(computed.props["reason"], "runtime-computed HTTP method");
    assert!(computed.props.get("t0_reason").is_none());
    let computed = kept("https://ORDERS.example.com/v1/orders");
    assert_eq!(computed.props[BINDING_STATE], BINDING_STATE_ORIGIN_DECLARED);
    assert_eq!(services(computed), ["declared-service:orders"]);
    assert_eq!(computed.props["reason"], "runtime-computed HTTP method");

    // A second pass over its own output changes nothing, and a pass with
    // other bindings replaces what the first recorded.
    let again = stitch_cross_repo(&out.kept_nodes, &out.kept_edges, &endpoints, &bindings);
    let props = |nodes: &[Node]| -> Vec<serde_json::Value> {
        let mut nodes: Vec<&Node> = nodes.iter().collect();
        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        nodes.iter().map(|node| node.props.clone()).collect()
    };
    assert_eq!(props(&again.kept_nodes), props(&out.kept_nodes));
    let other = stitch_cross_repo(
        &out.kept_nodes,
        &out.kept_edges,
        &endpoints,
        &[service("third", "api", "https://third.example.com")],
    );
    // The recorded outcome is part of each fact's content hash, derived from
    // the extractor's own hash: the graph hash follows the outcome, and a
    // later pass replaces the hash instead of chaining it.
    let hash =
        |props: &serde_json::Value| props["prov"]["content_hash"].as_str().unwrap().to_string();
    for (kept, changed) in [(&out, &other), (&again, &other)] {
        for node in &kept.kept_nodes {
            let source = gaps.iter().find(|gap| gap.id == node.id).unwrap();
            assert_eq!(node.props["t0_content_hash"], hash(&source.props));
            assert_ne!(hash(&node.props), hash(&source.props));
            let after = changed.kept_nodes.iter().find(|n| n.id == node.id).unwrap();
            assert_eq!(after.props["t0_content_hash"], hash(&source.props));
            assert_eq!(
                hash(&after.props) == hash(&node.props),
                after.props[BINDING_STATE] == node.props[BINDING_STATE]
                    && after.props.get("binding") == node.props.get("binding")
                    && after.props["reason"] == node.props["reason"]
                    && after.props.get("candidates") == node.props.get("candidates"),
                "{}",
                node.id
            );
        }
        for edge in &kept.kept_edges {
            let source = edges.iter().find(|gap| gap.dst == edge.dst).unwrap();
            assert_eq!(edge.props["t0_content_hash"], hash(&source.props));
            assert_ne!(hash(&edge.props), hash(&source.props));
        }
    }
    assert!(
        out.kept_nodes
            .iter()
            .zip(&other.kept_nodes)
            .any(|(first, second)| hash(&first.props) != hash(&second.props))
    );
    for node in &other.kept_nodes {
        let third = node.props["request"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://third.example.com/");
        if third {
            assert_ne!(node.props[BINDING_STATE], BINDING_STATE_NONE);
            assert_eq!(services(node), ["declared-service:third"]);
        } else {
            assert_eq!(node.props[BINDING_STATE], BINDING_STATE_NONE);
            assert!(node.props.get("binding").is_none(), "{node:?}");
        }
    }
}

// AC-0660 (#1843): an observed HTTP client of a declared host cites the
// bindings that name its origin, so it is not listed as leaving the system.
#[test]
fn observed_clients_of_a_declared_origin_cite_its_bindings() {
    let resource = |id: &str, kind: &str, identity: &str, observed: bool| Node {
        id: id.into(),
        label: "Resource".into(),
        props: serde_json::json!({
            "kind": kind,
            "identity": identity,
            "observed": observed,
            "prov": {"content_hash": format!("hash of {id}")},
        }),
    };
    let resources = [
        resource("res:b", "http-api", "https://Orders.example.com/v1", true),
        resource("res:a", "http-api", "https://gw.example.com", true),
        resource("res:c", "http-api", "https://third.example.com", true),
        resource("res:d", "sdk-client", "https://orders.example.com", true),
        resource("res:e", "http-api", "https://orders.example.com", false),
        resource("res:f", "http-api", "orders", true),
    ];
    let bindings = [
        service("orders", "api", "https://orders.example.com"),
        route("orders", "api", "https://gw.example.com", "/orders", true),
        route(
            "catalog",
            "catalog",
            "https://gw.example.com",
            "/items",
            false,
        ),
    ];
    let bound = bind_observed_clients(&resources, &bindings);
    let ids: Vec<&str> = bound.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids, ["res:a", "res:b"]);
    let gateway = bound[0].props["binding"].as_array().unwrap();
    assert_eq!(gateway.len(), 2);
    assert_eq!(gateway[0]["service"], "catalog");
    assert_eq!(gateway[1]["service"], "orders");
    assert_eq!(bound[1].props["binding"][0]["kind"], "declared-service");
    assert_eq!(bound[1].props["identity"], "https://Orders.example.com/v1");
    // The citations are part of the client's content hash.
    for node in &bound {
        let own = format!("hash of {}", node.id);
        assert_eq!(node.props["t0_content_hash"], own);
        assert_ne!(node.props["prov"]["content_hash"], own);
    }

    // Unchanged citations are not written again; a binding that is no
    // longer declared takes its citation with it.
    let mut stored = resources.to_vec();
    for node in &bound {
        *stored.iter_mut().find(|n| n.id == node.id).unwrap() = node.clone();
    }
    assert!(bind_observed_clients(&stored, &bindings).is_empty());
    let dropped = bind_observed_clients(&stored, &bindings[1..]);
    let ids: Vec<&str> = dropped.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids, ["res:b"]);
    assert!(dropped[0].props.get("binding").is_none());
    assert!(dropped[0].props.get("t0_content_hash").is_none());
    assert_eq!(dropped[0].props["prov"]["content_hash"], "hash of res:b");
    // Other citations give another hash, from the same own hash.
    let fewer = bind_observed_clients(&stored, &bindings[..2]);
    let ids: Vec<&str> = fewer.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids, ["res:a"]);
    assert_eq!(fewer[0].props["t0_content_hash"], "hash of res:a");
    assert_ne!(
        fewer[0].props["prov"]["content_hash"],
        bound[0].props["prov"]["content_hash"]
    );
    assert_ne!(fewer[0].props["prov"]["content_hash"], "hash of res:a");
    assert!(bind_observed_clients(&resources, &[]).is_empty());
}

// AC-0241: binding evidence that names a service with no matching endpoint
// keeps the Gap and names the service. (T-0241)
#[test]
fn binding_without_a_matching_endpoint_names_the_service() {
    let (gaps, edges) = web_gaps(
        &[fetch_site(
            "DELETE",
            IdentityExpr::Literal("https://orders.example.com/v1/orders/7".into()),
        )],
        &ConfigIndex::default(),
    );
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@GET:/v1/orders/:id"]),
        &[service("orders", "api", "https://orders.example.com")],
    );
    assert!(out.joined.is_empty());
    assert_eq!(
        out.kept_nodes[0].props["reason"],
        "binding evidence names orders (api), but no recovered endpoint there matches DELETE /v1/orders/7"
    );
}

// AC-0241: two equally supported targets are never picked between; the Gap
// lists both with their bindings. (T-0241)
#[test]
fn equally_supported_targets_stay_an_ambiguous_gap() {
    let (gaps, edges) = web_gaps(
        &[fetch_site(
            "GET",
            IdentityExpr::Literal("https://orders.example.com/v1/orders/7".into()),
        )],
        &ConfigIndex::default(),
    );
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@GET:/v1/orders/:id", "ep:api2@GET:/v1/orders/:id"]),
        &[
            service("orders", "api", "https://orders.example.com"),
            service("orders-next", "api2", "https://orders.example.com"),
        ],
    );
    assert!(out.joined.is_empty());
    let node = &out.kept_nodes[0];
    assert_eq!(
        node.props["reason"],
        "ambiguous cross-repo binding for GET https://orders.example.com/v1/orders/7: 2 equally supported targets"
    );
    let candidates: Vec<&str> = node.props["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["endpoint"].as_str().unwrap())
        .collect();
    assert_eq!(
        candidates,
        ["ep:api2@GET:/v1/orders/:id", "ep:api@GET:/v1/orders/:id"]
    );
}

// #594: a channel names every registry file that shaped any of its sites,
// once each, in name order; a Gap names its site's files.
#[test]
fn channel_and_gap_nodes_name_every_contributing_registry_file() {
    use adapters_fw::events::Registries;
    let qualifier = r#"{"schema":"cartograph-event-sdk-registry@1","version":"region-1",
      "qualifiers":[{"kind":"sqs-queue","component":"region","key":"Region"}]}"#;
    let layered = Registries::layered(&[("region.json".into(), qualifier.as_bytes().to_vec())]);
    let source = r#"
import { SendMessageCommand } from '@aws-sdk/client-sqs';
import { Consumer } from 'sqs-consumer';
export function send() { return new SendMessageCommand({ QueueUrl: 'q', Region: 'eu-west-1' }); }
export function listen() { return new Consumer({ queueUrl: 'q' }); }
export function lost(url: string) { return new SendMessageCommand({ QueueUrl: url, Region: 'eu-west-1' }); }
"#;
    let ts = adapters_lang_ts::SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let sites = adapters_lang_ts::extract_source_with(source.as_bytes(), "q.ts", &ts, &layered)
        .unwrap()
        .event_sites;
    let dir = tempfile::tempdir().unwrap();
    let cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch(&sites, &cfg, &id);
    let names = |props: &serde_json::Value| -> Vec<String> {
        props["registry_sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect()
    };
    let channel = out.nodes.iter().find(|n| n.label == "Channel").unwrap();
    assert_eq!(
        names(&channel.props),
        ["builtin:event-sdks.json", "extension:region.json"]
    );
    let consume = out.edges.iter().find(|e| e.label == "SUBSCRIBES").unwrap();
    assert_eq!(names(&consume.props), ["builtin:event-sdks.json"]);
    let gap = out.nodes.iter().find(|n| n.label == "Gap").unwrap();
    assert_eq!(
        names(&gap.props),
        ["builtin:event-sdks.json", "extension:region.json"]
    );
}

// --- Joined client calls (#737) --------------------------------------------

fn joined_site(
    method: &str,
    path: &str,
    base: IdentityExpr,
    join: BaseJoin,
    entry: &str,
) -> FetchSite {
    FetchSite {
        base: Some(FetchBase {
            url: base,
            join,
            path: "api.js".into(),
            byte_start: 7,
        }),
        entry: Some(entry.into()),
        ..fetch_site(method, IdentityExpr::Literal(path.into()))
    }
}

// AC-0315: a Retrofit path resolves against its literal base URL and binds
// cross-repo on the declared service, citing the client entry and join.
// (T-0315)
#[test]
fn retrofit_path_joins_its_base_url_and_binds_cross_repo() {
    let site = joined_site(
        "GET",
        "api/stats",
        IdentityExpr::Literal("https://app.example.io".into()),
        BaseJoin::Resolve,
        "ApiService.stats",
    );
    let (gaps, edges) = web_gaps(&[site], &ConfigIndex::default());
    assert_eq!(
        gaps[0].props["request"]["url"],
        "https://app.example.io/api/stats"
    );
    assert_eq!(gaps[0].props["request"]["url_source"], "base:literal");
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@GET:/api/stats", "ep:api@POST:/api/stats"]),
        &[service("app", "api", "https://app.example.io")],
    );
    let [joined] = out.joined.as_slice() else {
        panic!("one join: {:?}", out.joined)
    };
    assert_eq!(joined.edge.dst, "ep:api@GET:/api/stats");
    assert_eq!(joined.edge.props["resolver"], "base:literal");
    assert_eq!(
        joined.edge.props["join"],
        serde_json::json!({"entry": "ApiService.stats", "path": "api.js", "byte_start": 7})
    );
}

// AC-0315: a runtime base URL resolves only through a system-manifest
// declaration of its source text; an env file defining the same text does
// not count, and the Gap names the table entry. (T-0315)
#[test]
fn runtime_table_base_resolves_only_from_the_manifest() {
    let site = || {
        joined_site(
            "POST",
            "/api/auth/login",
            IdentityExpr::Computed("SETTINGS.apiUrl".into()),
            BaseJoin::Concat,
            "API_ROUTE.LOGIN",
        )
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "SETTINGS.apiUrl=https://env.example.io\n",
    )
    .unwrap();
    let mut cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    let (gaps, _) = web_gaps(&[site()], &cfg);
    assert_eq!(
        gaps[0].props["reason"],
        "runtime base URL `SETTINGS.apiUrl` of API_ROUTE.LOGIN is not declared in the system manifest"
    );
    assert!(gaps[0].props["request"].is_null());
    assert_eq!(gaps[0].props["join"]["entry"], "API_ROUTE.LOGIN");

    let declared = BTreeMap::from([(
        "SETTINGS.apiUrl".to_string(),
        "https://app.example.io".to_string(),
    )]);
    cfg.apply_manifest(&declared, "cartograph.system.toml");
    let (gaps, edges) = web_gaps(&[site()], &cfg);
    assert_eq!(
        gaps[0].props["request"]["url"],
        "https://app.example.io/api/auth/login"
    );
    assert_eq!(
        gaps[0].props["request"]["url_source"],
        "base:config:cartograph.system.toml"
    );
    let out = stitch_cross_repo(
        &gaps,
        &edges,
        &eps(&["ep:api@POST:/api/auth/login"]),
        &[service("app", "api", "https://app.example.io")],
    );
    assert_eq!(out.joined.len(), 1);
    assert_eq!(out.joined[0].edge.props["join"]["entry"], "API_ROUTE.LOGIN");
}

// AC-0315: a joined call whose path is not literal stays a Gap naming the
// entry, and a base env key that no file defines names it too. (T-0315)
#[test]
fn joined_calls_with_unresolved_halves_stay_named_gaps() {
    let computed_path = FetchSite {
        url: IdentityExpr::Computed("route()".into()),
        ..joined_site(
            "GET",
            "",
            IdentityExpr::Literal("https://app.example.io".into()),
            BaseJoin::Resolve,
            "ApiService.dynamic",
        )
    };
    let env_base = FetchSite {
        byte_start: 90,
        ..joined_site(
            "GET",
            "/api/x",
            IdentityExpr::EnvRef("API_URL".into()),
            BaseJoin::Concat,
            "API_ROUTE.X",
        )
    };
    let (gaps, _) = web_gaps(&[computed_path, env_base], &ConfigIndex::default());
    let reasons: Vec<_> = gaps
        .iter()
        .map(|g| g.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        [
            "runtime-computed path of ApiService.dynamic",
            "env key API_URL (base URL of API_ROUTE.X) not found in any env file in the repo",
        ]
    );
}

// AC-0315: Flask `<int:id>` and FastAPI/Spring `{id}` route parameters match
// one concrete segment like Express `:id`; literal segments still must agree
// and an empty segment never fills a parameter. (T-0315)
#[test]
fn route_parameters_match_in_every_framework_spelling() {
    for pattern in [
        "/api/aliases/:id/toggle",
        "/api/aliases/<int:id>/toggle",
        "/api/aliases/<id>/toggle",
        "/api/aliases/{id}/toggle",
    ] {
        assert!(
            route_matches(pattern, "/api/aliases/:alias_id/toggle"),
            "{pattern}"
        );
        assert!(route_matches(pattern, "/api/aliases/7/toggle"), "{pattern}");
        assert!(!route_matches(pattern, "/api/aliases//toggle"), "{pattern}");
        assert!(
            !route_matches(pattern, "/api/aliases/7/contacts"),
            "{pattern}"
        );
    }
    assert!(!route_matches("/api/<>/x", "/api/a/x"));
}

// AC-0452: an Express-style `ALL` route serves every method, as `ANY` does;
// a method route beside it on the same path stays ambiguous, and prefix
// routes (`/x/*`) are not matched (#1327).
#[test]
fn all_routes_match_every_fetch_method() {
    let endpoints = eps(&[
        "ep:test@ALL:/api/users/:id",
        "ep:test@ALL:/api/orders",
        "ep:test@POST:/api/orders",
        "ep:test@ALL:/api/files/*",
    ]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("DELETE", IdentityExpr::Literal("/api/users/7".into())),
            fetch_site("GET", IdentityExpr::Literal("/api/users/8".into())),
            fetch_site("POST", IdentityExpr::Literal("/api/orders".into())),
            fetch_site("GET", IdentityExpr::Literal("/api/files/a".into())),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let bound: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES" && e.dst.starts_with("ep:"))
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(
        bound,
        ["ep:test@ALL:/api/users/:id", "ep:test@ALL:/api/users/:id"]
    );
    let mut reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .filter_map(|n| n.props["reason"].as_str())
        .collect();
    reasons.sort();
    assert_eq!(
        reasons,
        [
            "ambiguous endpoint match for POST /api/orders (2 candidates)",
            "no recovered endpoint matches GET /api/files/a",
        ]
    );
}

fn prefix_endpoint(id: &str, kind: &str) -> Node {
    Node {
        id: id.into(),
        label: "Endpoint".into(),
        props: serde_json::json!({"match": "prefix", "prefix_kind": kind}),
    }
}

// AC-0453: a prefix route matches by the kind its producer declares: a raw
// `startsWith` string, or a segment catch-all of one or more (Next
// `[...x]`) or zero or more (`[[...x]]`) segments.
#[test]
fn prefix_routes_match_by_their_producer_kind() {
    use PrefixKind::*;
    let cases = [
        (Raw, "/x/*", "/x/", true),
        (Raw, "/x/*", "/x/a/", true),
        (Raw, "/x/*", "/x", false),
        (Raw, "/api*", "/apix", true),
        (Raw, "/api*", "/api/v1", true),
        (Raw, "/api", "/api", false),
        (SegmentsPlus, "/api/files/*", "/api/files/a", true),
        (SegmentsPlus, "/api/files/*", "/api/files/a/b", true),
        (SegmentsPlus, "/api/files/*", "/api/files/a/", true),
        (SegmentsPlus, "/api/files/*", "/api/files", false),
        (SegmentsPlus, "/api/files/*", "/api/files/", false),
        (SegmentsPlus, "/api/files/*", "/api/files//a", false),
        (SegmentsPlus, "/api/files/*", "/api/filesx/a", false),
        (SegmentsPlus, "/u/:id/*", "/u/7/a", true),
        (SegmentsPlus, "/u/:id/*", "/u//a", false),
        (SegmentsPlus, "/*", "/a", true),
        (SegmentsPlus, "/*", "/", false),
        (SegmentsStar, "/docs/*", "/docs", true),
        (SegmentsStar, "/docs/*", "/docs/", true),
        (SegmentsStar, "/docs/*", "/docs/a/b", true),
        (SegmentsStar, "/docs/*", "/docsx", false),
        (SegmentsStar, "/*", "/", true),
        (SegmentsStar, "/docs", "/docs", false),
        (Unsupported, "/u/:id/*", "/u/7/*", false),
        (Unsupported, "/u/:id/*", "/u/7/a", false),
    ];
    for (kind, pattern, path, expected) in cases {
        assert_eq!(
            route_matches_kind(Some(kind), pattern, path),
            expected,
            "{kind:?} {pattern} {path}"
        );
    }

    let nodes = [
        prefix_endpoint("ep:test@ALL:/ui/*", "raw"),
        prefix_endpoint("ep:test@ALL:/api/files/*", "segments+"),
        prefix_endpoint("ep:test@GET:/docs/*", "segments*"),
        prefix_endpoint("ep:test@GET:/odd/*", "regex"),
        Node {
            id: "ep:test@GET:/bare/*".into(),
            label: "Endpoint".into(),
            props: serde_json::json!({"match": "prefix"}),
        },
    ];
    let kinds = prefix_kinds(&nodes);
    // A missing or unknown kind is never guessed.
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        [
            ("ep:test@ALL:/api/files/*".to_string(), SegmentsPlus),
            ("ep:test@ALL:/ui/*".to_string(), Raw),
            ("ep:test@GET:/bare/*".to_string(), Unsupported),
            ("ep:test@GET:/docs/*".to_string(), SegmentsStar),
            ("ep:test@GET:/odd/*".to_string(), Unsupported),
        ]
    );
    // Producers that disagree on one id's kind leave it unsupported, in
    // either order.
    let raw = prefix_endpoint("ep:test@GET:/both/*", "raw");
    let plus = prefix_endpoint("ep:test@GET:/both/*", "segments+");
    for pair in [[raw.clone(), plus.clone()], [plus, raw]] {
        assert_eq!(prefix_kinds(&pair)["ep:test@GET:/both/*"], Unsupported);
    }
    let mut ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    ids.push("ep:test@GET:/api/files/readme".into());
    // An exact route whose literal path ends in `*` is not a prefix route.
    ids.push("ep:test@GET:/lit/*".into());
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches_with(
        &[
            fetch_site("GET", IdentityExpr::Literal("/ui/app.js".into())),
            fetch_site("PUT", IdentityExpr::Literal("/api/files/a/b".into())),
            fetch_site("GET", IdentityExpr::Literal("/docs".into())),
            // An exact route and a catch-all both match: no precedence is
            // assumed (#1329).
            fetch_site("GET", IdentityExpr::Literal("/api/files/readme".into())),
            fetch_site("GET", IdentityExpr::Literal("/odd/a".into())),
            fetch_site("GET", IdentityExpr::Literal("/bare/a".into())),
            // A literal `*` request does not match an unsupported prefix
            // route as an exact one.
            fetch_site("GET", IdentityExpr::Literal("/bare/*".into())),
            fetch_site("GET", IdentityExpr::Literal("/lit/*".into())),
        ],
        &ids,
        &BTreeSet::new(),
        &prefix_kinds(&nodes),
        &ConfigIndex::default(),
        &id,
    );
    let bound: Vec<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES" && e.dst.starts_with("ep:"))
        .map(|e| e.dst.as_str())
        .collect();
    assert_eq!(
        bound,
        [
            "ep:test@ALL:/ui/*",
            "ep:test@ALL:/api/files/*",
            "ep:test@GET:/docs/*",
            "ep:test@GET:/lit/*",
        ]
    );
    let mut reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .filter_map(|n| n.props["reason"].as_str())
        .collect();
    reasons.sort();
    assert_eq!(
        reasons,
        [
            "ambiguous endpoint match for GET /api/files/readme (2 candidates)",
            "no recovered endpoint matches GET /bare/*",
            "no recovered endpoint matches GET /bare/a",
            "no recovered endpoint matches GET /odd/a",
        ]
    );
}

// AC-0453: the cross-repo pass matches method-less and prefix routes the
// same way as the same-repo pass.
#[test]
fn cross_repo_fetches_join_all_and_prefix_routes() {
    let (gaps, edges) = web_gaps(
        &[
            fetch_site(
                "DELETE",
                IdentityExpr::Literal("https://orders.example.com/v1/orders/7".into()),
            ),
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://orders.example.com/assets/app.js".into()),
            ),
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://orders.example.com/blob/a".into()),
            ),
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://orders.example.com/blob/*".into()),
            ),
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://orders.example.com/lit/*".into()),
            ),
        ],
        &ConfigIndex::default(),
    );
    let nodes = [
        prefix_endpoint("ep:api@GET:/assets/*", "segments+"),
        prefix_endpoint("ep:api@GET:/blob/*", "bogus"),
    ];
    let out = stitch_cross_repo_with(
        &gaps,
        &edges,
        &eps(&[
            "ep:api@ALL:/v1/orders/:id",
            "ep:api@GET:/assets/*",
            "ep:api@GET:/blob/*",
            "ep:api@GET:/lit/*",
        ]),
        &prefix_kinds(&nodes),
        &[service("orders", "api", "https://orders.example.com/")],
    );
    let mut joined: Vec<&str> = out.joined.iter().map(|j| j.edge.dst.as_str()).collect();
    joined.sort();
    assert_eq!(
        joined,
        [
            "ep:api@ALL:/v1/orders/:id",
            "ep:api@GET:/assets/*",
            "ep:api@GET:/lit/*"
        ]
    );
    // Both `/blob/` requests stay gaps.
    assert_eq!(out.kept_nodes.len(), 2, "{:?}", out.kept_nodes);
}
