use super::*;
use core_graph::{Edge, Node};

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/p",
        commit: "abc123",
    }
}

fn extract(files: &[(&str, &str)]) -> Extraction {
    extract_with(files, Registries::builtin())
}

fn extract_with(files: &[(&str, &str)], registries: &Registries) -> Extraction {
    let dir = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    extract_dir(dir.path(), &id(), registries).unwrap()
}

fn resource<'a>(ex: &'a Extraction, logical_id: &str) -> &'a Node {
    let id = format!("res:local/p@{logical_id}");
    ex.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| {
            panic!(
                "no {id} in {:?}",
                ex.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
            )
        })
}

fn uses(ex: &Extraction) -> Vec<(&str, &str)> {
    let mut pairs: Vec<_> = ex
        .edges
        .iter()
        .filter(|edge| edge.label == "USES")
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect();
    pairs.sort();
    pairs
}

fn span(edge: &Edge) -> (u64, u64) {
    let evidence = &edge.props["prov"]["evidence"][0];
    (
        evidence["byte_start"].as_u64().unwrap(),
        evidence["byte_end"].as_u64().unwrap(),
    )
}

// AC-0316: browser storage is observed infrastructure. `indexedDB.open`
// names a database, Web Storage is one resource per global, and every use
// is a USES edge citing its own call. (T-0316)
#[test]
fn browser_storage_globals_become_observed_resources() {
    let src = r#"
export function openDb() {
  return indexedDB.open("inventory", 2);
}
export function remember(key: string) {
  window.localStorage.setItem(key, "1");
  return localStorage.getItem(key);
}
export async function warm() {
  const cache = await caches.open("assets-v1");
}
"#;
    let ex = extract(&[("src/db.ts", src)]);
    let idb = resource(&ex, "observed:indexeddb:inventory");
    assert_eq!(idb.label, "Resource");
    assert_eq!(idb.props["observed"], true);
    assert_eq!(idb.props["kind"], "browser-storage");
    assert_eq!(idb.props["identity"], "inventory");
    resource(&ex, "observed:web-storage:localStorage");
    resource(&ex, "observed:cache-storage:assets-v1");
    let open = ex
        .edges
        .iter()
        .find(|edge| edge.dst.ends_with("indexeddb:inventory"))
        .unwrap();
    let (start, end) = span(open);
    assert_eq!(
        &src[start as usize..end as usize],
        r#"indexedDB.open("inventory", 2)"#
    );
    let storage: Vec<_> = ex
        .edges
        .iter()
        .filter(|edge| edge.dst.ends_with("localStorage"))
        .collect();
    assert_eq!(
        storage.len(),
        2,
        "each access is one edge, a chain counts once"
    );
}

// AC-0316: a file that binds a storage global's name itself shadows it, so
// nothing is observed for it there. (T-0316)
#[test]
fn shadowed_storage_globals_are_not_observed() {
    let ex = extract(&[(
        "src/fake.ts",
        r#"
import { localStorage } from "./memory";
export function f(indexedDB: { open(n: string): void }) {
  indexedDB.open("x");
  localStorage.getItem("k");
}
"#,
    )]);
    assert!(ex.nodes.is_empty(), "{:?}", ex.nodes);
    assert!(ex.edges.is_empty());
}

// AC-0316: registry clients are observed only through a binding proven from
// their module: ES imports (named, aliased, default, namespace) and
// CommonJS require. The same constructor name from elsewhere, or a `let`
// or `var` require binding the file writes (assignment, destructuring,
// `for…of`), proves nothing; a member write leaves it intact unless it
// may overwrite the export the call selects. (T-0316)
#[test]
fn registry_clients_need_a_proven_binding() {
    let ex = extract(&[
        (
            "src/aws.ts",
            r#"
import { S3Client as Storage } from "@aws-sdk/client-s3";
import * as dynamo from "@aws-sdk/client-dynamodb";
import Stripe from "stripe";
export const s3 = new Storage({ region: "eu-west-1" });
export const ddb = new dynamo.DynamoDBClient({});
export const stripe = new Stripe(process.env.STRIPE_KEY!);
"#,
        ),
        (
            "src/db.js",
            r#"
const { Pool } = require("pg");
const { createClient: redis } = require("redis");
const pool = new Pool({ connectionString: process.env.DATABASE_URL });
const cache = redis({ url: "redis://user:secret@cache.internal:6379/0" });
"#,
        ),
        (
            "src/pg.ts",
            r#"
import pg from "pg";
export const client = new pg.Client({ connectionString: "postgres://app:pw@db.example:5432/app" });
"#,
        ),
        (
            "src/rebound.js",
            r#"
const { Pool } = require("pg");
function local() {
  const Pool = require("./my-pool");
  return new Pool();
}
"#,
        ),
        (
            "src/reassigned.js",
            r#"
let { S3Client } = require("@aws-sdk/client-s3");
let { SQSClient } = require("@aws-sdk/client-sqs");
var { SNSClient } = require("@aws-sdk/client-sns");
let { LambdaClient } = require("@aws-sdk/client-lambda");
let { SecretsManagerClient } = require("@aws-sdk/client-secrets-manager");
let Stripe = require("stripe");
S3Client = Fake;
Stripe ||= Fake;
({ SQSClient } = fakes);
function later() { SNSClient = Fake; }
for (LambdaClient of fakes) {}
SecretsManagerClient.prototype.extra = 1;
new S3Client({});
new SQSClient({});
new SNSClient({});
new LambdaClient({});
new SecretsManagerClient({});
new Stripe("sk");
"#,
        ),
        (
            "src/overwritten.js",
            r#"
const A = require("@aws-sdk/client-sqs");
const B = require("@aws-sdk/client-sns");
const C = require("@aws-sdk/client-lambda");
const D = require("@aws-sdk/client-s3");
const E = require("@aws-sdk/client-dynamodb");
const F = require("@aws-sdk/client-secrets-manager");
const G = require("@aws-sdk/client-secrets-manager");
A.SQSClient = Fake;
B["SNSClient"] = Fake;
C[key] = Fake;
Object.assign(D, fakes);
({ x: E.DynamoDBClient } = fakes);
delete F.SecretsManagerClient;
G.config.region = "eu-west-1";
G.SecretsManagerClient.prototype.extra = 1;
new A.SQSClient({});
new B.SNSClient({});
new C.LambdaClient({});
new D.S3Client({});
new E.DynamoDBClient({});
new F.SecretsManagerClient({});
new G.SecretsManagerClient({});
"#,
        ),
        (
            "src/cast.ts",
            r#"
import * as H from "@aws-sdk/client-sqs";
import * as I from "@aws-sdk/client-sns";
import * as J from "@aws-sdk/client-lambda";
import * as K from "@aws-sdk/client-s3";
Object.assign(H as any, { SQSClient: Fake });
((I as any)).SNSClient = Fake;
(J!)["LambdaClient"] = Fake;
Reflect.set((K satisfies object), "S3Client", Fake);
new H.SQSClient({});
new I.SNSClient({});
new J.LambdaClient({});
new K.S3Client({});
"#,
        ),
        (
            "src/local.ts",
            r#"
import { S3Client } from "./fake-s3";
class Pool {}
export const a = new S3Client();
export const b = new Pool();
"#,
        ),
    ]);
    let s3 = resource(&ex, "observed:aws-s3");
    assert_eq!(s3.props["kind"], "sdk-client");
    assert_eq!(s3.props["module"], "@aws-sdk/client-s3");
    resource(&ex, "observed:aws-dynamodb");
    resource(&ex, "observed:stripe");
    let pg = resource(&ex, "observed:postgres:env:DATABASE_URL");
    assert_eq!(pg.props["env_key"], "DATABASE_URL");
    // A URL keeps scheme and authority only: never credentials or paths.
    let redis = resource(&ex, "observed:redis:redis://cache.internal:6379");
    assert!(!redis.props.to_string().contains("secret"));
    assert!(
        !ex.edges
            .iter()
            .any(|edge| edge.props.to_string().contains("secret"))
    );
    resource(&ex, "observed:postgres:postgres://db.example:5432");
    // A `let` binding no expression writes still proves its module.
    resource(&ex, "observed:aws-secrets-manager");
    let reassigned: Vec<_> = uses(&ex)
        .into_iter()
        .filter(|(src, _)| src.contains("reassigned.js"))
        .collect();
    assert_eq!(reassigned.len(), 1, "{reassigned:?}");
    assert!(reassigned[0].1.ends_with("observed:aws-secrets-manager"));
    // Overwriting the selected export, under any key the file may name,
    // voids it; config and prototype writes do not.
    let overwritten: Vec<_> = uses(&ex)
        .into_iter()
        .filter(|(src, _)| src.contains("overwritten.js"))
        .collect();
    assert_eq!(overwritten.len(), 1, "{overwritten:?}");
    assert!(overwritten[0].1.ends_with("observed:aws-secrets-manager"));
    // Casts, `!` and parentheses still write the namespace they wrap.
    let cast: Vec<_> = uses(&ex)
        .into_iter()
        .filter(|(src, _)| src.contains("cast.ts"))
        .collect();
    assert!(cast.is_empty(), "{cast:?}");
    let registry: Vec<_> = ex
        .edges
        .iter()
        .map(|edge| edge.props["registry"].as_str().unwrap())
        .collect();
    assert!(
        registry
            .iter()
            .all(|name| *name == "builtin:observed-clients.json")
    );
    assert_eq!(
        ex.nodes.iter().filter(|n| n.label == "Resource").count(),
        7,
        "the local S3Client, a rebound Pool, a local Pool and reassigned \
         require bindings prove nothing: {:?}",
        ex.nodes
    );
}

// AC-0316: an identity the code computes at runtime is a Gap naming the
// site, never a resource; a client called without its identity still
// proves the provider. (T-0316)
#[test]
fn computed_identities_are_gaps_and_absent_ones_name_the_provider() {
    let src = r#"
import { MongoClient } from "mongodb";
import { openDB } from "idb";
export function connect(uri: string) {
  return new MongoClient(uri);
}
export const local = new MongoClient();
export const db = openDB(`cart-${1}`);
"#;
    let ex = extract(&[("src/mongo.ts", src)]);
    resource(&ex, "observed:mongodb");
    let gaps: Vec<_> = ex.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert_eq!(gaps[0].props["reason"], "runtime-computed mongodb identity");
    assert_eq!(gaps[0].props["raw"], "uri");
    assert_eq!(
        gaps[1].props["reason"],
        "runtime-computed indexeddb identity"
    );
    let to_gaps: Vec<_> = uses(&ex)
        .into_iter()
        .filter(|(_, dst)| dst.starts_with("gap:observed:"))
        .collect();
    assert_eq!(to_gaps.len(), 2);
    assert!(
        to_gaps.iter().any(|(src, _)| src.ends_with("#connect")),
        "{to_gaps:?}"
    );
}

// AC-0316: an extension registry file adds a client without code, and the
// edges it proves cite that file. (T-0316)
#[test]
fn an_extension_registry_adds_observed_clients() {
    let extension = br#"{"schema":"cartograph-observed-client-registry@1","version":"x-1",
        "clients":[{"family":"sdk-client","provider":"algolia","module":"algoliasearch",
          "export":"default","form":"call","identity":"first"}]}"#;
    let registries = Registries::layered(&[("algolia.json".into(), extension.to_vec())]);
    let src = r#"
import algoliasearch from "algoliasearch";
export const search = algoliasearch("APP123", "key");
"#;
    let ex = extract_with(&[("src/search.ts", src)], &registries);
    resource(&ex, "observed:algolia:APP123");
    assert_eq!(ex.edges[0].props["registry"], "extension:algolia.json");
    let builtin = extract(&[("src/search.ts", src)]);
    assert!(builtin.nodes.is_empty());
}

// #786 review: only the platform `fetch` drops a computed URL (the client-call
// pass records it). A registry client that happens to be named `fetch`, in
// any family, keeps its runtime-computed identity Gap. (T-0316)
#[test]
fn a_registry_client_named_fetch_keeps_its_computed_identity_gap() {
    let extension = br#"{"schema":"cartograph-observed-client-registry@1","version":"x-1",
        "clients":[{"family":"sdk-client","provider":"fetch","module":"fetch-sdk",
          "export":"default","form":"call","identity":"first"}]}"#;
    let registries = Registries::layered(&[("fetch.json".into(), extension.to_vec())]);
    let src = r#"
import connect from "fetch-sdk";
export const a = connect(region + ".example.com");
export const b = fetch(base + "/items");
"#;
    let ex = extract_with(&[("src/client.ts", src)], &registries);
    let gaps: Vec<_> = ex.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].props["reason"], "runtime-computed fetch identity");
    assert_eq!(gaps[0].props["kind"], "sdk-client");
}

// AC-0316 (#758 review): an identity that cannot be recorded safely is
// withheld whole, never kept as Gap raw text: a credential URL whose origin
// exceeds the bound, a URL with no host, a token-shaped literal. A computed
// identity keeps its source text only with URL userinfo and recognized
// secrets removed. (T-0316)
#[test]
fn unrecordable_and_computed_identities_never_persist_credentials() {
    let long_host = format!("{}.cache.internal", "a".repeat(140));
    let src = format!(
        r#"
import {{ createClient }} from "redis";
import {{ MongoClient }} from "mongodb";
export const a = createClient({{ url: "redis://user:hunter2@{long_host}:6379/0?x=1" }});
export const b = createClient({{ url: "redis://user:hunter2@" }});
export const c = new MongoClient("{token}");
export const d = new MongoClient("mongodb://admin:hunter2@db.internal/app?authSource=" + env);
"#,
        token = "Z".repeat(48),
    );
    let ex = extract(&[("src/clients.ts", &src)]);
    let gaps: Vec<_> = ex.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 4, "{gaps:?}");
    for gap in &gaps[..3] {
        assert_eq!(gap.props["raw"], serde_json::Value::Null, "{gap:?}");
        assert!(gap.props["reason"].as_str().unwrap().contains("withheld"));
    }
    assert_eq!(
        gaps[3].props["raw"],
        r#""mongodb://db.internal/app?authSource=" + env"#
    );
    let all = format!("{:?}{:?}", ex.nodes, ex.edges);
    assert!(!all.contains("hunter2") && !all.contains("user:"), "{all}");
    assert!(!all.contains(&"Z".repeat(48)));
}

// AC-0316 (#758 review): binding proof is lexical. A parameter, a local
// or a block shadows an imported client, a `require` in one function
// proves nothing in another, a literal const does not reach past a
// shadowing parameter, a parameter named `process` is no env reference,
// and a file's own `require` proves no module. (T-0316)
#[test]
fn client_bindings_and_constants_are_proven_at_each_site() {
    let src = r#"
import { S3Client } from "@aws-sdk/client-s3";
import { MongoClient } from "mongodb";
const NAME = "orders";
export function param(S3Client) { return new S3Client({}); }
export function local() { const S3Client = Fake; return new S3Client({}); }
export function block() { { class S3Client {} new S3Client({}); } }
export function a() { const { Pool } = require("pg"); return Pool; }
export function b() { return new Pool({}); }
export function shadowed(NAME) { return indexedDB.open(NAME); }
export function visible() { return indexedDB.open(NAME); }
export function outer(Pool) {
  return () => { const { Pool } = require("pg"); return new Pool({}); };
}
export function env(process) { return new MongoClient(process.env.URL); }
"#;
    let own_require = r#"
function require(m) { return fake; }
const { Pool } = require("pg");
export const pool = new Pool({ connectionString: "postgres://own.example" });
"#;
    let ex = extract(&[("src/scope.ts", src), ("src/require.js", own_require)]);
    let resources: Vec<_> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Resource")
        .map(|n| n.id.as_str())
        .collect();
    // The innermost binding wins: the nested `require` proves `Pool` past
    // the outer parameter of the same name. The two Gap sites still prove
    // their providers, with the instance unresolved (AC-0376).
    assert_eq!(
        resources,
        [
            "res:local/p@observed:indexeddb",
            "res:local/p@observed:indexeddb:orders",
            "res:local/p@observed:mongodb",
            "res:local/p@observed:postgres",
        ]
    );
    let raws: Vec<_> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .map(|n| n.props["raw"].as_str().unwrap())
        .collect();
    assert_eq!(raws, ["NAME", "process.env.URL"]);
}

// AC-0316 (#758 review): a qualified storage global counts only when its
// receiver (`window`, `self`, `globalThis`) is the platform global at the
// site. (T-0316)
#[test]
fn qualified_storage_globals_need_a_global_receiver() {
    let src = r#"
export function w() { return window.localStorage.getItem("k"); }
export function s() { return self.indexedDB.open("idb"); }
export function g() { return globalThis.caches.open("c"); }
export function pw(window) { return window.localStorage.getItem("k"); }
export function ps(self) { return self.indexedDB.open("other"); }
export function pg() { const globalThis = fake; return globalThis.caches.open("other"); }
"#;
    let ex = extract(&[("src/globals.ts", src)]);
    assert_eq!(
        uses(&ex),
        [
            (
                "sym:local/p@src/globals.ts#g",
                "res:local/p@observed:cache-storage:c"
            ),
            (
                "sym:local/p@src/globals.ts#s",
                "res:local/p@observed:indexeddb:idb"
            ),
            (
                "sym:local/p@src/globals.ts#w",
                "res:local/p@observed:web-storage:localStorage"
            ),
        ]
    );
}

// AC-0316 (#758 review): with a retained capture, observed facts come from
// the captured bytes. An edit to the checkout after capture, or a file the
// capture did not select, never reaches the recovery. (T-0316)
#[test]
fn captured_recovery_reads_only_the_retained_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, source: &str| std::fs::write(dir.path().join(path), source).unwrap();
    write("app.ts", "localStorage.setItem('k', 'v');\n");
    write("other.ts", "sessionStorage.setItem('k', 'v');\n");
    // A declaration file is not source, captured or not.
    write("types.d.ts", "sessionStorage.setItem('k', 'v');\n");
    let source = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
    let capture = source_capture::capture_working_tree(
        dir.path(),
        &source,
        &["app.ts".into(), "types.d.ts".into()],
        source_capture::CaptureLimits::default(),
    )
    .unwrap();
    // The checkout changes after capture.
    write(
        "app.ts",
        "import { S3Client } from '@aws-sdk/client-s3';\nnew S3Client({});\n",
    );
    let ex = extract_captured(&capture, &id(), Registries::builtin()).unwrap();
    resource(&ex, "observed:web-storage:localStorage");
    let ids: Vec<_> = ex.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids.len(), 1, "only the captured app.ts counts: {ids:?}");
    // The live tree, by contrast, sees today's bytes.
    let live = extract_dir(dir.path(), &id(), Registries::builtin()).unwrap();
    resource(&live, "observed:aws-s3");
}

// AC-0316: an HTTP API is one `http-api` resource per origin. Every call
// whose URL starts with a proven absolute origin is a USES edge citing it,
// whichever client made it; relative URLs and computed `fetch` URLs make
// none, a host the head leaves open is never truncated to a wrong origin,
// and a computed client URL is a Gap. (T-0316)
#[test]
fn http_calls_group_into_one_resource_per_origin() {
    let src = r#"
import axios from "axios";
import ky from "ky";
const API = "https://api.example.com";
export async function load(id: string) {
  await fetch("https://api.example.com/items");
  await fetch(`${API}/items/${id}`);
  await window.fetch(API + "/stats");
  await axios.get("https://api.example.com/users?x=1");
  await ky.post(`https://hooks.example.org/v1/${id}`);
  const client = axios.create({ baseURL: "https://billing.example.net/api" });
  await fetch("/relative/path");
  await fetch(`https://${id}.example.com/x`);
  await fetch(dynamicUrl(id));
  await axios.get(dynamicUrl(id));
  await axios.get("/relative");
  await axios({ url: "https://HOOKS.example.org:443/v2", method: "post" });
  await fetch("HTTPS://API.EXAMPLE.COM/upper");
}
function dynamicUrl(id: string) { return id; }
"#;
    let ex = extract(&[("src/api.ts", src)]);
    let api = resource(&ex, "observed:http-api:https://api.example.com");
    assert_eq!(api.props["kind"], "http-api");
    assert_eq!(api.props["type"], "http-api");
    assert_eq!(api.props["identity"], "https://api.example.com");
    assert!(api.props.get("module").is_none());
    // A resource shared by every client records none of their modules.
    let hooks = resource(&ex, "observed:http-api:https://hooks.example.org");
    assert!(hooks.props.get("module").is_none());
    // `axios({ url })` names its URL, and an origin is canonical: one
    // resource for any case or default port spelling.
    let to_hooks = ex
        .edges
        .iter()
        .filter(|edge| {
            edge.dst
                .ends_with("@observed:http-api:https://hooks.example.org")
        })
        .count();
    assert_eq!(to_hooks, 2);
    resource(&ex, "observed:http-api:https://billing.example.net");
    let resources: Vec<_> = ex
        .nodes
        .iter()
        .filter(|node| node.label == "Resource")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(resources.len(), 3, "{resources:?}");
    let to_api: Vec<_> = ex
        .edges
        .iter()
        .filter(|edge| {
            edge.dst
                .ends_with("@observed:http-api:https://api.example.com")
        })
        .map(|edge| {
            let (start, end) = span(edge);
            (
                edge.props["client"].as_str().unwrap(),
                &src[start as usize..end as usize],
            )
        })
        .collect();
    assert_eq!(
        to_api,
        [
            ("fetch", r#"fetch("https://api.example.com/items")"#),
            ("fetch", "fetch(`${API}/items/${id}`)"),
            ("fetch", r#"window.fetch(API + "/stats")"#),
            ("axios", r#"axios.get("https://api.example.com/users?x=1")"#),
            ("fetch", r#"fetch("HTTPS://API.EXAMPLE.COM/upper")"#),
        ]
    );
    // Only the computed axios URL is a Gap; the open host and the
    // computed fetch are not guessed and not duplicated.
    let gaps: Vec<_> = ex
        .nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(gaps, ["runtime-computed http-api identity"]);
}

// AC-0316: `fetch` is the platform's only while the file binds neither it
// nor the global receiver's property. (T-0316)
#[test]
fn shadowed_or_rewritten_fetch_is_not_observed() {
    let ex = extract(&[
        (
            "src/a.ts",
            r#"
import fetch from "node-fetch-lookalike";
export const a = () => fetch("https://api.example.com/a");
"#,
        ),
        (
            "src/b.ts",
            r#"
window.fetch = mock;
export const b = () => window.fetch("https://api.example.com/b");
"#,
        ),
        (
            "src/d.ts",
            r#"
window.fetch = mock;
export const d = () => fetch("https://api.example.com/d");
"#,
        ),
        (
            "src/c.ts",
            r#"
export function c(fetch: (u: string) => void) { fetch("https://api.example.com/c"); }
"#,
        ),
    ]);
    assert!(
        ex.nodes.iter().all(|node| node.label != "Resource"),
        "{:?}",
        ex.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
}

// AC-0317: a URL led by an env reference names that variable, never a
// value; a relative or empty head names nothing. (T-0317)
#[test]
fn env_led_http_urls_name_their_variable() {
    let ex = extract(&[(
        "src/env.ts",
        r#"
export const a = () => fetch(`${import.meta.env.VITE_API}/items`);
export const b = () => fetch(process.env.API_URL + "/stats");
"#,
    )]);
    let vite = resource(&ex, "observed:http-api:env:VITE_API");
    assert_eq!(vite.props["env_key"], "VITE_API");
    resource(&ex, "observed:http-api:env:API_URL");
    assert_eq!(uses(&ex).len(), 2);
}

// AC-0317: an HTTP config object names its URL only by a top-level
// property it provably ends with: the last one wins, a nested `url` is not
// the call's, and a later spread, computed key or method that may
// override it leaves the origin unproven (#765 review). (T-0317)
#[test]
fn http_config_urls_need_the_effective_top_level_property() {
    let src = r#"
import axios from "axios";
export async function load(config: object, k: string) {
  await axios({ metadata: { url: "https://nested.example" }, url: "https://top.example/a" });
  await axios({ url: "https://first.example", url: "https://last.example/b" });
  await axios({ ...config, url: "https://explicit.example/c" });
  await axios({ url: "https://spread.example", ...config });
  await axios({ url: "https://computed.example", [k]: "x" });
  await axios({ url: "https://getter.example", get url() { return k; } });
  axios.create({ baseURL: "https://base.example/v1", ...config });
  axios.create({ headers: { baseURL: "https://inner.example" } });
}
"#;
    let ex = extract(&[("src/config.ts", src)]);
    let mut resources: Vec<_> = ex
        .nodes
        .iter()
        .filter(|node| node.label == "Resource")
        .map(|node| node.id.rsplit('@').next().unwrap())
        .collect();
    resources.sort();
    assert_eq!(
        resources,
        [
            "observed:http-api:https://explicit.example",
            "observed:http-api:https://last.example",
            "observed:http-api:https://top.example",
        ]
    );
    let gaps = ex.nodes.iter().filter(|node| node.label == "Gap").count();
    assert_eq!(gaps, 4, "{:?}", ex.nodes);
}

// AC-0316: WebExtension storage is observed infrastructure, one resource per
// area, through the unbound `chrome`/`browser` global or any name an import of
// `webextension-polyfill` binds; any other binding or a write of `<api>.storage`
// proves nothing. (T-0316)
#[test]
fn webextension_storage_areas_become_observed_resources() {
    let polyfill = r#"
import browser from "webextension-polyfill";
export async function save(key, value) {
  await browser.storage.sync.set({ [key]: value });
  return (await browser.storage.sync.get(key))[key];
}
browser.storage.onChanged.addListener(() => {});
"#;
    let global = "chrome.storage.local.get('k', () => {});\n";
    let ex = extract(&[("src/SLStorage.js", polyfill), ("src/bg.js", global)]);
    let sync = resource(&ex, "observed:webextension-storage:sync");
    assert_eq!(sync.props["kind"], "browser-storage");
    assert_eq!(sync.props["module"], "webextension-polyfill");
    let local = resource(&ex, "observed:webextension-storage:local");
    assert!(local.props.get("module").is_none());
    let edges: Vec<_> = ex
        .edges
        .iter()
        .filter(|edge| edge.dst.contains("webextension-storage"))
        .collect();
    assert_eq!(edges.len(), 3, "each access chain is one edge");
    let (start, end) = span(
        edges
            .iter()
            .find(|edge| edge.dst.ends_with(":local"))
            .unwrap(),
    );
    assert_eq!(
        &global[start as usize..end as usize],
        "chrome.storage.local"
    );

    // A polyfill import under any local name proves the API, and a global
    // access sorted first does not drop the polyfill module (#789 review).
    for alias in [
        "import ext from \"webextension-polyfill\";\next.storage.sync.get('k');\n",
        "import * as ext from \"webextension-polyfill\";\next.storage.sync.get('k');\n",
        "const ext = require(\"webextension-polyfill\");\next.storage.sync.get('k');\n",
    ] {
        let ex = extract(&[
            ("src/a.js", "chrome.storage.sync.get('k');\n"),
            ("src/b.js", alias),
        ]);
        let sync = resource(&ex, "observed:webextension-storage:sync");
        assert_eq!(sync.props["module"], "webextension-polyfill", "{alias}");
        let edges = ex
            .edges
            .iter()
            .filter(|edge| edge.dst.ends_with(":sync"))
            .count();
        assert_eq!(edges, 2, "{alias}");
    }

    for (case, source) in [
        (
            "other module",
            "import browser from './fake';\nbrowser.storage.sync.get('k');\n",
        ),
        (
            "local binding",
            "const chrome = makeMock();\nchrome.storage.local.get('k');\n",
        ),
        (
            "parameter",
            "export function f(browser) { return browser.storage.sync.get('k'); }\n",
        ),
        (
            "storage written",
            "chrome.storage = fake;\nchrome.storage.local.get('k');\n",
        ),
        ("other global", "ext.storage.sync.get('k');\n"),
        (
            "alias storage written",
            "import ext from \"webextension-polyfill\";\next.storage = fake;\next.storage.sync.get('k');\n",
        ),
        ("unknown area", "chrome.storage.cloud.get('k');\n"),
        ("computed area", "chrome.storage[area].get('k');\n"),
    ] {
        let ex = extract(&[("src/x.js", source)]);
        assert!(
            ex.nodes
                .iter()
                .all(|n| !n.id.contains("webextension-storage")),
            "{case}"
        );
    }
}
