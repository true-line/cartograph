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
    let source = &publish.props["registry_source"];
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
        publish.props["registry_source"]["name"],
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

use adapters_fw::client::FetchSite;

fn fetch_site(method: &str, url: IdentityExpr) -> FetchSite {
    FetchSite {
        method: method.into(),
        url,
        symbol: Some("sym:test@app.tsx#Orders".into()),
        path: "app.tsx".into(),
        byte_start: 10,
        byte_end: 40,
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
