use super::*;

const APP_TS: &str = r#"
import express from 'express';
import { listUsers } from './users';

const app = express();

app.get('/users', listUsers);
app.post('/users', (req, res) => {
  createUser();
});

function createUser() {
  validate();
}

function validate() {}

app.listen(3000);
"#;

const USERS_TS: &str = r#"
export function listUsers() {}
"#;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "qwtm/example",
        commit: "abc123",
    }
}

fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("app.ts"), APP_TS).unwrap();
    std::fs::write(src.join("users.ts"), USERS_TS).unwrap();
    // Noise that must be skipped:
    std::fs::create_dir_all(dir.path().join("node_modules/junk")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/junk/index.ts"),
        "export const x = 1;",
    )
    .unwrap();
    std::fs::write(src.join("types.d.ts"), "declare const y: number;").unwrap();
    dir
}

fn edge_pairs<'a>(ex: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    ex.edges
        .iter()
        .filter(|e| e.label == label)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect()
}

#[test]
fn extracts_express_endpoints_not_arbitrary_calls() {
    // AC-0004: endpoints via the framework adapter, marked Confirmed.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let endpoints: Vec<_> = ex.nodes.iter().filter(|n| n.label == "Endpoint").collect();
    let ids: Vec<_> = endpoints.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["ep:qwtm/example@GET:/users", "ep:qwtm/example@POST:/users"]
    );
    // `app.listen(3000)` and non-router receivers must not become endpoints.
    assert!(!ids.iter().any(|i| i.contains("listen")));
}

#[test]
fn endpoint_receiver_must_come_from_framework_factory() {
    // A `get` call on an untracked receiver is NOT a T0 endpoint.
    let src = r#"
const map = new Map();
map.get('/users');
"#;
    let ex = extract_source(src.as_bytes(), "src/other.ts", &id()).unwrap();
    assert!(ex.nodes.iter().all(|n| n.label != "Endpoint"));
}

#[test]
fn handles_edges_bind_named_and_anonymous_handlers() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let handles = edge_pairs(&ex, "HANDLES");
    // Named handler resolves through the import — cross-file, deterministically.
    assert!(handles.contains(&(
        "ep:qwtm/example@GET:/users",
        "sym:qwtm/example@src/users.ts#listUsers"
    )));
    // Anonymous handler gets a stable offset-keyed symbol in this file.
    let (_, anon) = handles
        .iter()
        .find(|(src, _)| *src == "ep:qwtm/example@POST:/users")
        .expect("POST handler edge");
    assert!(anon.starts_with("sym:qwtm/example@src/app.ts#anon@"));
}

#[test]
fn call_edges_are_symbol_to_symbol() {
    // AC-0005: intra-procedural call edges.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    // Anonymous POST handler calls createUser; createUser calls validate.
    assert!(calls.iter().any(
        |(src, dst)| src.starts_with("sym:qwtm/example@src/app.ts#anon@")
            && *dst == "sym:qwtm/example@src/app.ts#createUser"
    ));
    assert!(calls.contains(&(
        "sym:qwtm/example@src/app.ts#createUser",
        "sym:qwtm/example@src/app.ts#validate"
    )));
}

#[test]
fn typed_member_calls_resolve_local_and_imported_methods() {
    // AC-0005: explicit TS receiver types and constructor types make member
    // calls deterministic across method and file boundaries.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("service.ts"),
        r#"
export class UserService {
  list() {}
}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("controller.ts"),
        r#"
import { UserService } from './service';
import { MissingService as MS } from './missing';

export class UserController {
  constructor(private readonly users: UserService) {}
  index() { this.users.list(); }
}

export function run(users: UserService) {
  users.list();
}

export function inferred() {
  const users = new UserService();
  users.list();
}

export function unresolved(missing: MS) {
  missing.run();
}
"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let target = "sym:qwtm/example@src/service.ts#UserService.list";
    assert!(calls.contains(&(
        "sym:qwtm/example@src/controller.ts#UserController.index",
        target
    )));
    assert!(calls.contains(&("sym:qwtm/example@src/controller.ts#run", target)));
    assert!(calls.contains(&("sym:qwtm/example@src/controller.ts#inferred", target)));
    assert!(
        !calls
            .iter()
            .any(|(_, dst)| *dst == "sym:qwtm/example@src/missing.ts#MissingService.run")
    );
    let gap = ex
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.props["callee"] == "MissingService.run")
        .expect("unresolved typed call remains an explicit semantic slot");
    assert!(calls.contains(&(
        "sym:qwtm/example@src/controller.ts#unresolved",
        gap.id.as_str()
    )));
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    let target_node = ex.nodes.iter().find(|node| node.id == target).unwrap();
    assert_eq!(target_node.props["kind"], "Method");
}

#[test]
fn unresolved_relative_import_calls_emit_gaps_without_global_noise() {
    // AC-0021: a call with a relative-import target that directory-wide T0
    // cannot prove becomes a CALLS Gap. Package/global calls are not local
    // semantic candidates and must not flood the graph with false slots.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("caller.ts"),
        r#"
import { processOrder as po } from './missing';
import { randomUUID } from 'node:crypto';
export function run() {
  po();
  randomUUID();
  console.log('done');
}
"#,
    )
    .unwrap();
    let ex = extract_dir(dir.path(), &id()).unwrap();
    let gaps: Vec<_> = ex.nodes.iter().filter(|node| node.label == "Gap").collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].props["callee"], "processOrder");
    let calls = edge_pairs(&ex, "CALLS");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, gaps[0].id);
    assert_eq!(
        ex.edges
            .iter()
            .find(|edge| edge.label == "CALLS")
            .unwrap()
            .props["prov"]["confidence_tier"],
        "Gap"
    );
}

#[test]
fn fastify_and_nest_endpoints_are_import_proven() {
    // AC-0004: framework registry coverage includes Fastify factory routes and
    // Nest controller/method decorators, without name-based guessing.
    let fastify = extract_source(
        br#"
import Fastify from 'fastify';
const app = Fastify();
function listUsers() {}
app.get('/users', listUsers);
"#,
        "src/fastify.ts",
        &id(),
    )
    .unwrap();
    assert!(
        fastify
            .nodes
            .iter()
            .any(|node| node.id == "ep:qwtm/example@GET:/users")
    );

    let nest = extract_source(
        br#"
import { Controller as Api, Get, Post } from '@nestjs/common';

@Api('users')
export class UsersController {
  @Get()
  list() {}

  @Post(':id')
  update() {}
}
"#,
        "src/users.controller.ts",
        &id(),
    )
    .unwrap();
    let endpoints: Vec<_> = nest
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        [
            "ep:qwtm/example@GET:/users",
            "ep:qwtm/example@POST:/users/:id"
        ]
    );
    let handles = edge_pairs(&nest, "HANDLES");
    assert!(handles.contains(&(
        "ep:qwtm/example@GET:/users",
        "sym:qwtm/example@src/users.controller.ts#UsersController.list"
    )));

    let lookalike = extract_source(
        br#"
function Controller(_: string) { return (_: unknown) => {}; }
function Get() { return (_: unknown) => {}; }
@Controller('fake')
class FakeController { @Get() list() {} }
"#,
        "src/fake.ts",
        &id(),
    )
    .unwrap();
    assert!(lookalike.nodes.iter().all(|node| node.label != "Endpoint"));
}

#[test]
fn imports_resolve_relative_files_and_modules() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let imports = edge_pairs(&ex, "IMPORTS");
    assert!(imports.contains(&(
        "file:qwtm/example@src/app.ts",
        "file:qwtm/example@src/users.ts"
    )));
    assert!(imports.contains(&("file:qwtm/example@src/app.ts", "mod:express")));
}

#[test]
fn every_fact_carries_confirmed_t0_provenance() {
    // AC-0006: provenance {tier, confidence, evidence span, extractor} on all facts.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let all_props = ex
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(ex.edges.iter().map(|e| &e.props));
    for props in all_props {
        let prov = props.get("prov").expect("prov present");
        assert_eq!(prov["tier"], "Deterministic");
        assert_eq!(prov["confidence_tier"], "Confirmed");
        assert_eq!(prov["extractor_id"], "t0.adapter-ts");
        let ev = &prov["evidence"][0];
        assert_eq!(ev["repo"], "qwtm/example");
        assert_eq!(ev["path"], "src/app.ts");
        assert_eq!(ev["commit_sha"], "abc123");
        assert!(ev["byte_end"].as_u64().unwrap() > ev["byte_start"].as_u64().unwrap());
        assert!(prov["content_hash"].as_str().unwrap().len() == 64);
    }
}

#[test]
fn evidence_spans_point_at_the_actual_source() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let ep = ex
        .nodes
        .iter()
        .find(|n| n.id == "ep:qwtm/example@GET:/users")
        .unwrap();
    let ev = &ep.props["prov"]["evidence"][0];
    let span = &APP_TS.as_bytes()
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    // Jump-to-source lands on the registration call (M1 exit-gate groundwork).
    assert_eq!(
        std::str::from_utf8(span).unwrap(),
        "app.get('/users', listUsers)"
    );
}

#[test]
fn dir_walk_skips_noise_and_is_deterministic() {
    let dir = fixture_dir();
    let a = extract_dir(dir.path(), &id()).unwrap();
    let b = extract_dir(dir.path(), &id()).unwrap();
    // node_modules and .d.ts excluded: only our two files produce File nodes
    // (placeholders are labeled but flagged).
    let files: Vec<_> = a
        .nodes
        .iter()
        .filter(|n| n.label == "File" && n.props.get("placeholder").is_none())
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(
        files,
        vec![
            "file:qwtm/example@src/app.ts",
            "file:qwtm/example@src/users.ts"
        ]
    );
    // US-0014 groundwork: identical input -> identical facts (incl. hashes).
    assert_eq!(
        serde_json::to_string(&a.nodes).unwrap(),
        serde_json::to_string(&b.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&a.edges).unwrap(),
        serde_json::to_string(&b.edges).unwrap()
    );
}

#[test]
fn delta_reingest_reuses_unchanged_files_and_recomputes_only_changes() {
    // AC-0040 (T-0040): source parsing is content-addressed per physical file.
    let dir = fixture_dir();
    let mut cache = IncrementalCache::default();
    let (_, first) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first.recomputed_files, 2);
    assert_eq!(first.reused_files, 0);

    let (same, second) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(second.recomputed_files, 0);
    assert_eq!(second.reused_files, 2);

    std::fs::write(
        dir.path().join("src/users.ts"),
        "export function listUsers() { return []; }\n",
    )
    .unwrap();
    let (changed, third) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(third.recomputed_files, 1);
    assert_eq!(third.reused_files, 1);
    assert_ne!(
        serde_json::to_string(&same.nodes).unwrap(),
        serde_json::to_string(&changed.nodes).unwrap()
    );

    std::fs::remove_file(dir.path().join("src/users.ts")).unwrap();
    let (_, fourth) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(fourth.deleted_files, 1);
}

#[test]
fn progress_hook_fires_once_per_file_in_deterministic_order() {
    // AC-0094 (T-0094): the shell's live "what it's doing right now" ping
    // rides this callback, one call per file, in the same sorted order
    // extraction itself uses (US-0014's determinism).
    let dir = fixture_dir();
    let mut cache = IncrementalCache::default();
    let mut seen = Vec::new();
    let (_, stats) = extract_dir_incremental_with_progress(
        dir.path(),
        &id(),
        &mut cache,
        Registries::builtin(),
        &mut |path| seen.push(path.to_string()),
    )
    .unwrap();
    assert_eq!(stats.recomputed_files, 2);
    assert_eq!(
        seen,
        vec!["src/app.ts".to_string(), "src/users.ts".to_string()]
    );

    // The plain entry point (used everywhere that doesn't care) stays silent.
    let mut cache = IncrementalCache::default();
    extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
}

#[test]
fn plain_javascript_and_jsx_files_are_collected_and_parsed() {
    // AC-0095: no separate JS adapter exists — this crate's grammar parses
    // plain JS/JSX directly, so .js/.jsx/.mjs/.cjs are collected right
    // alongside .ts/.tsx.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.js"),
        "export function add(a, b) { return a + b; }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("util.mjs"), "export const x = 1;\n").unwrap();
    std::fs::write(dir.path().join("legacy.cjs"), "module.exports = {};\n").unwrap();
    std::fs::write(
        dir.path().join("Widget.jsx"),
        "export function Widget() { return <div>hi</div>; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let files: std::collections::BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "File" && n.props.get("placeholder").is_none())
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(
        files,
        std::collections::BTreeSet::from([
            "file:qwtm/example@Widget.jsx",
            "file:qwtm/example@index.js",
            "file:qwtm/example@legacy.cjs",
            "file:qwtm/example@util.mjs",
        ])
    );
    // .jsx gets the same JSX-aware Component treatment as .tsx (capitalized,
    // parsed under the TSX grammar).
    let widget = out
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@Widget.jsx#Widget")
        .expect("Widget symbol");
    assert_eq!(widget.label, "Component");
}

#[test]
fn extensionless_import_reconciles_to_the_real_non_ts_extension() {
    // The real extension of an extensionless relative import isn't knowable
    // per file (`resolve_relative` guesses `.ts`); once the whole directory
    // is known, both the IMPORTS edge and the cross-file CALLS edge must
    // join the real `.jsx` file's nodes instead of a phantom `.ts`
    // placeholder next to an orphaned real File/Symbol.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.js"),
        "import { helper } from './helper';\nexport function run() { helper(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("helper.jsx"),
        "export function helper() { return null; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let real_file = "file:qwtm/example@helper.jsx";
    let real_symbol = "sym:qwtm/example@helper.jsx#helper";
    let guessed_file = "file:qwtm/example@helper.ts";
    let guessed_symbol = "sym:qwtm/example@helper.ts#helper";

    assert!(out.nodes.iter().any(|n| n.id == real_file));
    assert!(!out.nodes.iter().any(|n| n.id == guessed_file));
    assert!(edge_pairs(&out, "IMPORTS").contains(&("file:qwtm/example@app.js", real_file)));

    assert!(out.nodes.iter().any(|n| n.id == real_symbol));
    assert!(!out.nodes.iter().any(|n| n.id == guessed_symbol));
    assert!(edge_pairs(&out, "CALLS").contains(&("sym:qwtm/example@app.js#run", real_symbol)));
    // No Gap: the call resolved for real, not by falling through to one.
    assert!(!out.nodes.iter().any(|n| n.label == "Gap"));
}

#[test]
fn imported_jsx_component_reconciles_to_the_real_non_ts_extension() {
    // RENDERS edges for an imported component are built eagerly from the
    // same `imported` map as cross-file CALLS (both direct JSX usage and a
    // React Router `element={<Comp/>}`) — they need the same directory-wide
    // extension correction, not just IMPORTS/CALLS.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("App.jsx"),
        r#"
import { Header } from './Header';
export function App() {
  return <Header />;
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Header.jsx"),
        "export function Header() { return null; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let real_symbol = "sym:qwtm/example@Header.jsx#Header";
    let guessed_symbol = "sym:qwtm/example@Header.ts#Header";
    assert!(!out.nodes.iter().any(|n| n.id == guessed_symbol));
    assert!(edge_pairs(&out, "RENDERS").contains(&("sym:qwtm/example@App.jsx#App", real_symbol)));
    assert!(!out.nodes.iter().any(|n| n.label == "Gap"));
}

#[test]
fn close_over_creates_placeholders_for_unresolved_targets() {
    let src = "import { helper } from './missing';\nexport function run() { helper(); }";
    let mut ex = extract_source(src.as_bytes(), "src/run.ts", &id()).unwrap();
    ex.close_over_endpoints();
    // The missing file and its symbol exist as flagged placeholders.
    let placeholder = ex
        .nodes
        .iter()
        .find(|n| n.id == "file:qwtm/example@src/missing.ts")
        .expect("placeholder file node");
    assert_eq!(placeholder.props["placeholder"], true);
}

// --- Event-site detection (US-0004; stitching tested in `events`) ----------

use adapters_fw::events::{ChannelRole, IdentityExpr};

fn sites_for(source: &str) -> Vec<adapters_fw::events::EventSite> {
    extract_source(source.as_bytes(), "app.ts", &id())
        .unwrap()
        .event_sites
}

// AC-0010 groundwork: receivers must be proven from the SDK, mirroring the
// endpoint extractor — a look-alike `emit` on an unproven object is not a
// producer site.
#[test]
fn event_receiver_must_come_from_sdk_constructor() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
const impostor = getBus();
function a() { bus.emit('real.event'); }
function b() { impostor.emit('fake.event'); }
function c() { bus.emit('also.real'); }
"#,
    );
    let ids: Vec<_> = sites
        .iter()
        .map(|s| match &s.identity {
            IdentityExpr::Literal(l) => l.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(ids, ["real.event", "also.real"]);
}

// A constructor imported from the wrong module is not an SDK command.
#[test]
fn command_constructor_must_be_imported_from_sdk_module() {
    let sites = sites_for(
        r#"
import { SendMessageCommand } from './local-fake';
new SendMessageCommand({ QueueUrl: 'https://q' });
"#,
    );
    assert!(sites.is_empty(), "look-alike ctor from a relative module");
}

// AWS SDK v2 method style: receiver proven from `new AWS.SQS()`.
#[test]
fn aws_v2_send_message_is_detected_with_key_identity() {
    let sites = sites_for(
        r#"
import AWS from 'aws-sdk';
const sqs = new AWS.SQS();
export function push() {
  sqs.sendMessage({ QueueUrl: 'https://sqs.example/q', MessageBody: 'x' });
}
"#,
    );
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].kind, "sqs-queue");
    assert_eq!(sites[0].role, ChannelRole::Produces);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::Literal("https://sqs.example/q".into())
    );
    assert_eq!(
        sites[0].symbol.as_deref(),
        Some("sym:qwtm/example@app.ts#push")
    );
}

// EventBridge: the identity key is found nested inside Entries[].
#[test]
fn eventbridge_detail_type_is_found_nested() {
    let sites = sites_for(
        r#"
import { PutEventsCommand } from '@aws-sdk/client-eventbridge';
new PutEventsCommand({
  Entries: [{ Source: 'app', DetailType: 'order.placed', Detail: '{}' }],
});
"#,
    );
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::Literal("order.placed".into())
    );
    assert_eq!(sites[0].kind, "eventbridge-detail-type");
}

// A local `const` bound to a string literal resolves at T0 (same file,
// deterministic); an unbound identifier stays computed (escalates).
#[test]
fn local_const_identity_is_literal_unknown_identifier_is_computed() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
const TOPIC = 'orders.created';
function a(dynamic: string) {
  bus.emit(TOPIC);
  bus.emit(dynamic);
}
"#,
    );
    assert_eq!(sites.len(), 2);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::Literal("orders.created".into())
    );
    assert_eq!(sites[1].identity, IdentityExpr::Computed("dynamic".into()));
}

// A registry key that is not statically present is a computed identity —
// the site is kept (it escalates), never silently dropped (AC-0012).
#[test]
fn missing_identity_key_yields_computed_not_dropped() {
    let sites = sites_for(
        r#"
import { SendMessageCommand } from '@aws-sdk/client-sqs';
declare const params: any;
new SendMessageCommand(params);
"#,
    );
    assert_eq!(sites.len(), 1);
    assert!(matches!(sites[0].identity, IdentityExpr::Computed(_)));
}

// Only immutable `const` bindings promote to literal identities: a `let`
// can be reassigned before the emit, so stamping Confirmed on it would
// fabricate a channel for a runtime value (AC-0012 boundary).
#[test]
fn let_bound_identity_stays_computed() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
let topic = 'orders';
function a() { bus.emit(topic); }
"#,
    );
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].identity, IdentityExpr::Computed("topic".into()));
}

// Bracketed env access is the same deterministic config ref as dotted
// access (AC-0011): `process.env['KEY']` resolves, computed keys do not.
#[test]
fn bracketed_env_access_is_an_env_ref() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
function a(k: string) {
  bus.emit(process.env['ORDERS_TOPIC']);
  bus.emit(process.env[k]);
}
"#,
    );
    assert_eq!(sites.len(), 2);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::EnvRef("ORDERS_TOPIC".into())
    );
    assert!(matches!(sites[1].identity, IdentityExpr::Computed(_)));
}

// --- Client-side extraction (US-0005) --------------------------------------

use adapters_fw::client::FetchSite;

fn client_extract(path: &str, source: &str) -> Extraction {
    extract_source(source.as_bytes(), path, &id()).unwrap()
}

// AC-0013: a React Router repo yields Screen nodes, Component nodes, and
// RENDERS edges — routes import-proven, components capitalized-in-tsx.
// (T-0013)
#[test]
fn react_router_routes_become_screens_with_renders() {
    let out = client_extract(
        "app.tsx",
        r#"
import { Routes, Route } from 'react-router-dom';
import { Orders } from './orders';

export function App() {
  return (
    <Routes>
      <Route path="/orders" element={<Orders />} />
    </Routes>
  );
}
"#,
    );
    let screen = out
        .nodes
        .iter()
        .find(|n| n.label == "Screen")
        .expect("screen node");
    assert_eq!(screen.id, "screen:qwtm/example@/orders");
    assert_eq!(screen.props["route"], "/orders");
    // The screen renders the routed (imported) component.
    // Import binding resolves to .ts by default (extension-aware
    // resolution rides typed inter-proc work, #2); close-over placeholders
    // keep the edge valid either way.
    assert!(out.edges.iter().any(|e| e.label == "RENDERS"
        && e.src == "screen:qwtm/example@/orders"
        && e.dst == "sym:qwtm/example@orders.ts#Orders"));
    // App itself is a Component (capitalized, tsx).
    let app = out
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@app.tsx#App")
        .unwrap();
    assert_eq!(app.label, "Component");
}

// A Route look-alike without the react-router import is not a screen.
#[test]
fn route_component_must_be_import_proven() {
    let out = client_extract(
        "app.tsx",
        r#"
import { Route } from './my-own-router';
export function App() {
  return <Route path="/fake" element={<div />} />;
}
"#,
    );
    assert!(out.nodes.iter().all(|n| n.label != "Screen"));
}

// Component-to-component JSX usage becomes RENDERS; HTML tags do not.
#[test]
fn jsx_usage_becomes_renders_edges() {
    let out = client_extract(
        "page.tsx",
        r#"
import { Header } from './header';
function Body() { return <div>content</div>; }
export function Page() {
  return (
    <main>
      <Header />
      <Body />
    </main>
  );
}
"#,
    );
    let renders: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|e| e.label == "RENDERS")
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert!(renders.contains(&(
        "sym:qwtm/example@page.tsx#Page",
        "sym:qwtm/example@header.ts#Header"
    )));
    assert!(renders.contains(&(
        "sym:qwtm/example@page.tsx#Page",
        "sym:qwtm/example@page.tsx#Body"
    )));
    // <main>/<div> never produce edges.
    assert_eq!(renders.len(), 2);
}

// AC-0013 (Next.js half): pages/ files are screens by convention; index
// collapses; _app is chrome. (T-0013)
#[test]
fn next_pages_convention_yields_screens() {
    let dir = tempfile::tempdir().unwrap();
    let pages = dir.path().join("pages");
    std::fs::create_dir_all(pages.join("users")).unwrap();
    std::fs::write(
        pages.join("index.tsx"),
        "export default function Home() { return <div/>; }\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("users").join("[id].tsx"),
        "export default function UserDetail() { return <div/>; }\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("_app.tsx"),
        "export default function MyApp() { return <div/>; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let screens: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.as_str())
        .collect();
    assert!(screens.contains(&"screen:qwtm/example@/"));
    assert!(screens.contains(&"screen:qwtm/example@/users/[id]"));
    assert!(!screens.iter().any(|s| s.contains("_app")));
    // The screen renders the page's default export.
    assert!(out.edges.iter().any(|e| e.label == "RENDERS"
        && e.src == "screen:qwtm/example@/users/[id]"
        && e.dst == "sym:qwtm/example@pages/users/[id].tsx#UserDetail"));
}

// AC-0542: pages-router screens decode only File nodes and append the same
// Screens and RENDERS edges over plain lists and the column store.
#[test]
fn next_pages_completion_is_identical_over_lists_and_store() {
    use core_graph::facts::{FactLists, FactStore};
    let file = |path: &str| Node {
        id: format!("file:qwtm/example@{path}"),
        label: "File".into(),
        props: serde_json::json!({ "path": path }),
    };
    let nodes = vec![
        file("pages/index.tsx"),
        file("web/pages/users/[id].tsx"),
        file("pages/_app.tsx"),
        file("pages/api/hello.js"),
        file("lib/util.ts"),
        Node {
            id: "sym:qwtm/example@pages/index.tsx#Home".into(),
            label: "Function".into(),
            props: serde_json::json!({}),
        },
    ];
    let edges = vec![Edge {
        src: "file:qwtm/example@pages/index.tsx".into(),
        dst: "sym:qwtm/example@pages/index.tsx#Home".into(),
        label: "DEFINES".into(),
        props: serde_json::json!({}),
    }];
    let exports = HashMap::from([(
        "pages/index.tsx".to_string(),
        "sym:qwtm/example@pages/index.tsx#Home".to_string(),
    )]);
    let (mut list_nodes, mut list_edges) = (nodes.clone(), edges.clone());
    next_pages_screens(
        &exports,
        &id(),
        &mut FactLists {
            nodes: &mut list_nodes,
            edges: &mut list_edges,
        },
    )
    .unwrap();
    let mut store = FactStore::from_facts(nodes, edges).unwrap();
    next_pages_screens(&exports, &id(), &mut store).unwrap();
    assert_eq!(
        store.into_facts().unwrap(),
        (list_nodes.clone(), list_edges.clone())
    );
    let screens: Vec<&str> = list_nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(
        screens,
        ["screen:qwtm/example@/", "screen:qwtm/example@/users/[id]"]
    );
    assert_eq!(list_edges.last().unwrap().label, "RENDERS");
}

// AC-0543: guessed IMPORTS/RENDERS targets are reconciled against the known
// file and symbol sets identically over plain lists and the column store,
// rewriting only proven targets in their slots.
#[test]
fn guessed_target_reconciliation_is_identical_over_lists_and_store() {
    use core_graph::facts::{FactLists, FactStore};
    let node = |id: &str, label: &str| Node {
        id: id.into(),
        label: label.into(),
        props: serde_json::json!({}),
    };
    let edge = |src: &str, dst: &str, label: &str| Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: serde_json::json!({}),
    };
    let (a, b) = ("file:qwtm/example@src/a.ts", "file:qwtm/example@src/b.tsx");
    let comp = "sym:qwtm/example@src/b.tsx#Comp";
    let nodes = vec![node(a, "File"), node(b, "File"), node(comp, "Function")];
    let edges = vec![
        edge(a, "file:qwtm/example@src/b.ts", "IMPORTS"),
        edge(a, "sym:qwtm/example@src/b.ts#Comp", "RENDERS"),
        edge(a, "file:qwtm/example@src/missing.ts", "IMPORTS"),
        edge(a, "mod:react", "IMPORTS"),
        edge(a, comp, "CALLS"),
    ];
    let known_symbols: std::collections::HashSet<String> =
        nodes.iter().map(|n| n.id.clone()).collect();
    let known_files: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| n.label == "File")
        .map(|n| n.id.clone())
        .collect();
    let (mut list_nodes, mut list_edges) = (nodes.clone(), edges.clone());
    reconcile_guessed_edge_targets(
        &mut FactLists {
            nodes: &mut list_nodes,
            edges: &mut list_edges,
        },
        &known_files,
        &known_symbols,
    )
    .unwrap();
    let mut store = FactStore::from_facts(nodes, edges).unwrap();
    reconcile_guessed_edge_targets(&mut store, &known_files, &known_symbols).unwrap();
    assert_eq!(
        store.into_facts().unwrap(),
        (list_nodes, list_edges.clone())
    );
    let targets: Vec<&str> = list_edges.iter().map(|e| e.dst.as_str()).collect();
    assert_eq!(
        targets,
        [
            b,
            comp,
            "file:qwtm/example@src/missing.ts",
            "mod:react",
            comp
        ]
    );
}

#[test]
fn next_pages_convention_covers_plain_js_and_excludes_api_routes() {
    // Real-world Next.js apps commonly write JSX pages in plain .js without
    // renaming to .jsx (AC-0095) — a page under pages/ qualifies the same as
    // .tsx/.jsx. pages/api/* shares the extension but is never a screen.
    let dir = tempfile::tempdir().unwrap();
    let pages = dir.path().join("pages");
    std::fs::create_dir_all(pages.join("api")).unwrap();
    std::fs::write(
        pages.join("about.js"),
        "export default function About() { return <div/>; }\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("api").join("hello.js"),
        "export default function handler(req, res) { res.status(200).end(); }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let screens: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(screens, vec!["screen:qwtm/example@/about"]);
    assert!(out.edges.iter().any(|e| e.label == "RENDERS"
        && e.src == "screen:qwtm/example@/about"
        && e.dst == "sym:qwtm/example@pages/about.js#About"));
}

// Fetch sites classify their URL like channel identities: literal, env
// ref, or computed — and the method comes from the call shape. (T-0014
// groundwork; resolution against endpoints is tested in `events`.)
#[test]
fn fetch_and_axios_sites_are_detected_and_classified() {
    let out = client_extract(
        "api.tsx",
        r#"
import axios from 'axios';
export function Orders() {
  fetch('/api/orders');
  fetch('/api/orders', { method: 'POST', body: '{}' });
  fetch(process.env.API_BASE);
  axios.get('/api/users?page=2');
  axios({ url: '/api/ping', method: 'delete' });
  fetch(buildUrl());
  return <div/>;
}
function buildUrl(): string { return '/x'; }
"#,
    );
    let sites: Vec<(&str, &FetchSite)> = out
        .fetch_sites
        .iter()
        .map(|s| (s.method.as_str(), s))
        .collect();
    assert_eq!(sites.len(), 6);
    // Call-form pass first (fetch + axios object form, document order),
    // then the member-form pass.
    assert!(matches!(&sites[0].1.url, IdentityExpr::Literal(u) if u == "/api/orders"));
    assert_eq!(sites[0].0, "GET");
    assert_eq!(sites[1].0, "POST");
    assert!(matches!(&sites[2].1.url, IdentityExpr::EnvRef(k) if k == "API_BASE"));
    assert_eq!(sites[3].0, "DELETE");
    assert!(matches!(&sites[3].1.url, IdentityExpr::Literal(u) if u == "/api/ping"));
    assert!(matches!(&sites[4].1.url, IdentityExpr::Computed(_)));
    assert_eq!(sites[5].0, "GET");
    assert!(matches!(&sites[5].1.url, IdentityExpr::Literal(u) if u == "/api/users?page=2"));
    // All sites anchor at the enclosing component.
    assert!(
        out.fetch_sites
            .iter()
            .all(|s| s.symbol.as_deref() == Some("sym:qwtm/example@api.tsx#Orders"))
    );
}

// AC-0352 (#1012): a fetch site records whether every pass through its
// Symbol's body that gets past the call makes it. A call under a branch, a
// loop, a `try`, a short-circuit, an optional chain, a parameter default or
// a callback does not, and neither does a call anchored at a component from
// inside one of its closures.
#[test]
fn fetch_sites_record_whether_the_call_is_straight_line() {
    let out = client_extract(
        "flow.tsx",
        r#"
export async function submit(flag, items, hook, fallback = fetch('/default')) {
  const first = await fetch('/first', { method: 'POST' });
  if (flag) await fetch('/if');
  else fetch('/else');
  if (await fetch('/condition')) {}
  flag && fetch('/and');
  const pick = flag ? fetch('/then') : null;
  for (const item of items) fetch('/loop');
  try { fetch('/try'); } catch (e) { fetch('/catch'); } finally { fetch('/finally'); }
  items.forEach(() => fetch('/callback'));
  hook?.run(fetch('/optional'));
  hook?.deep.run(fetch('/optional-deep'));
  fetch?.('/optional-call');
  await hook?.client.fetch('/optional-receiver');
  switch (flag) { case 1: fetch('/case'); }
  return fetch('/last');
}
export const save = async () => {
  await fetch('/arrow');
};
"#,
    );
    let mut lines: Vec<(String, Option<bool>)> = out
        .fetch_sites
        .iter()
        .map(|site| {
            let IdentityExpr::Literal(url) = &site.url else {
                panic!("{site:?}");
            };
            (url.clone(), site.straight_line)
        })
        .collect();
    lines.sort();
    let straight: Vec<&str> = lines
        .iter()
        .filter(|(_, straight_line)| *straight_line == Some(true))
        .map(|(url, _)| url.as_str())
        .collect();
    // The callback's call is its own Symbol's, and straight-line there: a
    // navigation in `submit` is in another Symbol and proves nothing by it.
    assert_eq!(
        straight,
        ["/arrow", "/callback", "/condition", "/first", "/last"]
    );
    let callback = out
        .fetch_sites
        .iter()
        .find(|site| matches!(&site.url, IdentityExpr::Literal(url) if url == "/callback"))
        .unwrap();
    assert_ne!(callback.symbol, out.fetch_sites[0].symbol);
    assert_eq!(lines.len(), 18);
    assert!(
        lines
            .iter()
            .all(|(_, straight_line)| straight_line.is_some())
    );

    // A site anchored at its component from inside a closure is not
    // straight-line in the component, whatever it is in the closure.
    let component = client_extract(
        "page.tsx",
        r#"
export function Page() {
  fetch('/render');
  const submit = () => { fetch('/closure'); };
  return <button onClick={submit}/>;
}
"#,
    );
    let by_url = |url: &str| {
        component
            .fetch_sites
            .iter()
            .find(|site| matches!(&site.url, IdentityExpr::Literal(u) if u == url))
            .unwrap()
    };
    assert_eq!(by_url("/render").symbol, by_url("/closure").symbol);
    assert_eq!(by_url("/render").straight_line, Some(true));
    assert_eq!(by_url("/closure").straight_line, Some(false));
}

// A shadowed `fetch` (imported or locally defined) is application code —
// treating it as the browser API would confirm FETCHES against the wrong
// target and corrupt the graph.
#[test]
fn shadowed_fetch_is_not_a_fetch_site() {
    let imported_shadow = client_extract(
        "a.tsx",
        r#"
import { fetch } from './cache';
export function A() { fetch('/orders'); return <div/>; }
"#,
    );
    assert!(imported_shadow.fetch_sites.is_empty());

    let local_shadow = client_extract(
        "b.tsx",
        r#"
function fetch(url: string) { return url; }
export function B() { fetch('/orders'); return <div/>; }
"#,
    );
    assert!(local_shadow.fetch_sites.is_empty());
}

// Only a direct property of the options object sets the HTTP method — a
// `method` key nested under headers/data does not.
#[test]
fn nested_method_key_does_not_set_the_http_method() {
    let out = client_extract(
        "c.tsx",
        r#"
export function C() {
  fetch('/orders', { headers: { method: 'POST' } });
  return <div/>;
}
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    assert_eq!(out.fetch_sites[0].method, "GET");
}

// A fetch inside an event-handler closure anchors at the component, not
// the closure (SPEC-00 §3.5: FETCHES(component → Endpoint)) — otherwise
// the screen's RENDERS chain can never reach it.
#[test]
fn fetch_in_handler_closure_anchors_at_the_component() {
    let out = client_extract(
        "checkout.tsx",
        r#"
export function Checkout() {
  const submit = () => fetch('/orders', { method: 'POST' });
  return <button onClick={submit}>Order</button>;
}
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    assert_eq!(
        out.fetch_sites[0].symbol.as_deref(),
        Some("sym:qwtm/example@checkout.tsx#Checkout")
    );
}

// A fetch inside a *nested* component belongs to that nested component —
// attributing it to the outer one would confirm FETCHES onto screens that
// never render the child.
#[test]
fn fetch_in_nested_component_anchors_at_the_nearest_component() {
    let out = client_extract(
        "checkout.tsx",
        r#"
export function Checkout() {
  const CouponLookup = () => {
    fetch('/coupons');
    return <input />;
  };
  return <div>checkout</div>;
}
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    let nested = out
        .nodes
        .iter()
        .find(|node| {
            node.label == "Component"
                && node.props["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Checkout/CouponLookup@"))
        })
        .expect("nested component retains its actual lexical owner");
    assert_eq!(
        out.fetch_sites[0].symbol.as_deref(),
        Some(nested.id.as_str())
    );
}

#[test]
fn object_methods_and_property_callbacks_have_cited_distinct_owners() {
    // AC-0119: every source callable is real, cited and distinct across objects.
    let source = r#"
function helper() {}
const first = { check() { helper(); }, arrow: () => helper(), fn: function named() { helper(); } };
const second = { check() { helper(); }, [key]() { helper(); } };
class Box {
  check() { helper(); }
  create() { return { check() { helper(); }, callback: () => helper() }; }
}

"#;
    let out = extract_source(source.as_bytes(), "owners.ts", &id()).unwrap();
    let symbols: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .collect();
    let ids: BTreeSet<_> = symbols.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids.len(), symbols.len(), "no callable identities merge");
    let helper = "sym:qwtm/example@owners.ts#helper";
    let calls: Vec<_> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "CALLS" && edge.dst == helper)
        .collect();
    assert_eq!(calls.len(), 8);
    assert_eq!(
        calls
            .iter()
            .map(|edge| &edge.src)
            .collect::<BTreeSet<_>>()
            .len(),
        8
    );
    for edge in calls {
        let owner = symbols
            .iter()
            .find(|node| node.id == edge.src)
            .expect("a call owner must be emitted");
        assert!(out.edges.iter().any(|edge| edge.label == "DEFINED_IN"
            && edge.src == owner.id
            && edge.dst == "file:qwtm/example@owners.ts"));
        let prov: Provenance = serde_json::from_value(owner.props["prov"].clone()).unwrap();
        assert_eq!(prov.tier, Tier::Deterministic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
        assert_eq!(prov.evidence.len(), 1);
        let span = &prov.evidence[0];
        assert_eq!(span.path, "owners.ts");
        assert_eq!(span.commit_sha, "abc123");
        let text = &source[span.byte_start as usize..span.byte_end as usize];
        assert!(
            text.contains("helper()"),
            "the evidence is the owner, not another object: {text}"
        );
        if owner.id.contains("first@")
            || owner.id.contains("second@")
            || owner.id.contains("Box.create/")
        {
            assert!(
                owner.props.get("class").is_none(),
                "object method is not a class member"
            );
        }
    }
    assert!(
        symbols
            .iter()
            .any(|node| node.id.contains("first@") && node.id.contains(".check@"))
    );
    assert!(
        symbols
            .iter()
            .any(|node| node.id.contains("second@") && node.id.contains(".check@"))
    );
    assert!(
        symbols
            .iter()
            .any(|node| node.id.contains("Box.create/") && node.id.contains(".check@"))
    );
    let computed = symbols
        .iter()
        .find(|node| node.props["computed_name"] == true)
        .unwrap();
    assert!(computed.id.contains(".computed@"));
    assert!(
        !computed.id.contains("[key]"),
        "computed syntax does not prove a runtime key"
    );
}

#[test]
fn class_expression_owners_never_merge_by_anonymous_or_private_name() {
    // AC-0119 / AC-0120 / AC-0121: expression-local class names need identity.
    let source = r#"
function first() {}
function second() {}
const A = class { check() { first(); } };
const B = class { check() { second(); } };
const C = class Private { check() { first(); } };
const D = class Private { check() { second(); } };
class Existing { check() { first(); } }
"#;
    let out = extract_source(source.as_bytes(), "class-expressions.ts", &id()).unwrap();
    let methods: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.props["kind"] == "Method")
        .collect();
    assert_eq!(methods.len(), 5);
    assert_eq!(
        methods
            .iter()
            .map(|node| &node.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    assert!(
        methods
            .iter()
            .any(|node| node.id == "sym:qwtm/example@class-expressions.ts#Existing.check")
    );
    let calls = edge_pairs(&out, "CALLS");
    assert_eq!(calls.len(), 5);
    for method in methods {
        let prov: Provenance = serde_json::from_value(method.props["prov"].clone()).unwrap();
        let evidence = &prov.evidence[0];
        let text = &source[evidence.byte_start as usize..evidence.byte_end as usize];
        let target = if text.contains("first()") {
            "first"
        } else {
            "second"
        };
        assert!(calls.contains(&(
            method.id.as_str(),
            sym_id(id().repo, "class-expressions.ts", target).as_str()
        )));
        assert_eq!(calls.iter().filter(|(src, _)| *src == method.id).count(), 1);
    }
}

#[test]
fn nested_calls_resolve_only_lexical_callable_bindings() {
    // AC-0120: nearest owners, lexical shadowing, and no unrelated name matches.
    let source = r#"
function shared() {}
function reassigned() {}
function wrapper(shared: () => void) {
  shared();
  function local() {}
  local();
  { const local = () => {}; local(); }
  local();
  [1].map(() => { local(); });
}
function other() { function local() {} local(); }
function unrelated() { local(); shared(); }
function destructured({ shared }: { shared: () => void }) { shared(); }
function looped() { for (const shared of callbacks) { shared(); } }
function caught() { try {} catch (shared) { shared(); } }
function changed() { reassigned = () => {}; reassigned(); }
const named = function privateName() { shared(); privateName(); };
function outside() { privateName(); }
"#;
    let out = extract_source(source.as_bytes(), "scope.ts", &id()).unwrap();
    let symbol = |prefix: &str| {
        out.nodes
            .iter()
            .find(|node| {
                node.label == "Symbol"
                    && node.props["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with(prefix))
            })
            .unwrap()
    };
    let outer_local = symbol("wrapper/local@");
    let other_local = symbol("other/local@");
    assert_ne!(outer_local.id, other_local.id);
    let calls = edge_pairs(&out, "CALLS");
    assert!(calls.contains(&("sym:qwtm/example@scope.ts#wrapper", outer_local.id.as_str())));
    assert!(calls.contains(&("sym:qwtm/example@scope.ts#other", other_local.id.as_str())));
    let anonymous = calls
        .iter()
        .find(|(src, dst)| *dst == outer_local.id && *src != "sym:qwtm/example@scope.ts#wrapper")
        .expect("map callback owns its call")
        .0;
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id == anonymous && node.label == "Symbol")
    );
    assert!(
        !calls
            .iter()
            .any(|(src, dst)| *src == "sym:qwtm/example@scope.ts#wrapper"
                && dst.ends_with("#shared"))
    );
    assert!(!calls.iter().any(|(src, _)| src.ends_with("#destructured")
        || src.ends_with("#changed")
        || src.ends_with("#looped")
        || src.ends_with("#caught")
        || src.ends_with("#outside")));
    assert_eq!(
        calls
            .iter()
            .filter(|(src, _)| src.ends_with("#unrelated"))
            .count(),
        1
    );
    assert!(calls.contains(&(
        "sym:qwtm/example@scope.ts#unrelated",
        "sym:qwtm/example@scope.ts#shared"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@scope.ts#named",
        "sym:qwtm/example@scope.ts#shared"
    )));
    for (source, target) in calls {
        assert!(out.nodes.iter().any(|node| node.id == source));
        assert!(out.nodes.iter().any(|node| node.id == target));
    }
}

#[test]
fn shadowed_imports_and_receivers_do_not_borrow_file_wide_proof() {
    // AC-0120: imported bindings and typed receivers obey the call's own scope.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("helper.ts"),
        "export function imported() {} export class Remote { check() {} }",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("scope.ts"),
        r#"
import { imported, Remote } from './helper';
class Local { check() {} }
function proven(receiver: Remote) { imported(); receiver.check(); }
function shadowed(imported: () => void, receiver: unknown) { imported(); receiver.check(); }
function local() { const receiver = new Local(); receiver.check(); }
function unknown() { receiver.check(); }
class Container {
  check() {}
  build() {
    const object = { check() { this.check(); } };
    const callback = function () { this.check(); };
    const arrow = () => this.check();
    return object;
  }
}
"#,
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let calls = edge_pairs(&out, "CALLS");
    assert!(calls.contains(&(
        "sym:qwtm/example@scope.ts#proven",
        "sym:qwtm/example@helper.ts#imported"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@scope.ts#proven",
        "sym:qwtm/example@helper.ts#Remote.check"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@scope.ts#local",
        "sym:qwtm/example@scope.ts#Local.check"
    )));
    assert!(!calls.iter().any(|(src, _)| src.ends_with("#shadowed")
        || src.ends_with("#unknown")
        || src.contains("/callback@")));
    let class_calls: Vec<_> = calls
        .iter()
        .filter(|(_, dst)| dst.ends_with("#Container.check"))
        .collect();
    assert_eq!(class_calls.len(), 1);
    assert!(class_calls[0].0.contains("Container.build/arrow@"));
}

#[test]
fn callable_ownership_preserves_existing_ids_and_determinism() {
    // AC-0121: existing top-level/class/route identities agree with call owners.
    let source = r#"
import express from 'express';
const router = express();
function helper() {}
const arrow = () => helper();
class Service { run() { helper(); } }
router.get('/check', () => { helper(); });
function register() { router.post('/check', function () { helper(); }); }
const config = { test() { helper(); }, before: () => helper() };
"#;
    let first = extract_source(source.as_bytes(), "stable.ts", &id()).unwrap();
    let second = extract_source(source.as_bytes(), "stable.ts", &id()).unwrap();
    assert_eq!(first.nodes, second.nodes);
    assert_eq!(first.edges, second.edges);
    for name in ["helper", "arrow", "Service.run", "register"] {
        let expected = sym_id(id().repo, "stable.ts", name);
        assert_eq!(
            first
                .nodes
                .iter()
                .filter(|node| node.id == expected)
                .count(),
            1
        );
    }
    for handles in first.edges.iter().filter(|edge| edge.label == "HANDLES") {
        assert!(handles.dst.starts_with("sym:qwtm/example@stable.ts#anon@"));
        assert_eq!(
            first
                .nodes
                .iter()
                .filter(|node| node.id == handles.dst)
                .count(),
            1
        );
        assert!(first.edges.iter().any(|edge| edge.label == "CALLS"
            && edge.src == handles.dst
            && edge.dst.ends_with("#helper")));
    }
    assert_eq!(
        first
            .edges
            .iter()
            .filter(|edge| edge.label == "HANDLES")
            .count(),
        2
    );
}

#[test]
fn pulumi_aws_constructors_emit_resources_dependencies_and_capabilities() {
    // AC-0051/T-0051: Pulumi remains a T0 language-adapter parse, while AWS
    // relationship semantics come from the same registry as Terraform.
    let source = r#"
import * as aws from '@pulumi/aws';
import * as pulumi from '@pulumi/pulumi';
import { Bucket as LogsBucket } from '@pulumi/aws/s3';

const queue = new aws.sqs.Queue('orders', {});
const logs = new LogsBucket('logs', {});
new aws.s3.Bucket('audit', {});
const archive = new pulumi.asset.FileAsset('archive.zip');
const worker = new aws.lambda.Function('worker', {});
const mapping = new aws.lambda.EventSourceMapping('orders-worker', {
  eventSourceArn: queue.arn,
  functionName: worker.arn,
}, { parent: worker, dependsOn: [queue] });
"#;
    let ex = extract_source(source.as_bytes(), "infra.ts", &id()).unwrap();
    let resources = ex
        .nodes
        .iter()
        .filter(|node| node.label == "Resource")
        .collect::<Vec<_>>();
    assert_eq!(resources.len(), 5);
    assert!(resources.iter().all(|node| {
        node.props["source"] == "pulumi"
            && node.props["prov"]["tier"] == "Deterministic"
            && node.props["prov"]["confidence_tier"] == "Confirmed"
    }));
    let queue = "res:qwtm/example@pulumi:aws:sqs/queue:Queue:orders";
    let logs = "res:qwtm/example@pulumi:aws:s3/bucket:Bucket:logs";
    let audit = "res:qwtm/example@pulumi:aws:s3/bucket:Bucket:audit";
    let worker = "res:qwtm/example@pulumi:aws:lambda/function:Function:worker";
    let mapping =
        "res:qwtm/example@pulumi:aws:lambda/eventSourceMapping:EventSourceMapping:orders-worker";
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "TRIGGERS").contains(&(queue, worker)));
    assert!(resources.iter().any(|node| node.id == logs));
    assert!(resources.iter().any(|node| node.id == audit));
    assert!(
        resources
            .iter()
            .all(|node| node.props["logical_id"] != "archive.zip")
    );
    let trigger = ex
        .edges
        .iter()
        .find(|edge| edge.label == "TRIGGERS")
        .unwrap();
    assert_eq!(trigger.props["via"], mapping);
    assert_eq!(trigger.props["registry"], iac::registry::REGISTRY_VERSION);
}

#[test]
fn pulumi_imported_resources_resolve_directory_relationships() {
    // AC-0051/T-0051: relative imports are proven only after the whole
    // directory is recovered, then REFERENCES and registry edges target the
    // imported resources rather than disappearing at the file boundary.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("queue.ts"),
        "import * as aws from '@pulumi/aws';\nexport const queue = new aws.sqs.Queue('orders', {});\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("worker.ts"),
        "import * as aws from '@pulumi/aws';\nexport const worker = new aws.lambda.Function('worker', {});\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("mapping.ts"),
        r#"
import * as aws from '@pulumi/aws';
import { queue } from './queue';
import { worker } from './worker';
new aws.lambda.EventSourceMapping('orders-worker', {
  eventSourceArn: queue.arn,
  functionName: worker.arn,
}, { dependsOn: [queue, worker] });
"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let queue = "res:qwtm/example@pulumi:aws:sqs/queue:Queue:orders";
    let worker = "res:qwtm/example@pulumi:aws:lambda/function:Function:worker";
    let mapping =
        "res:qwtm/example@pulumi:aws:lambda/eventSourceMapping:EventSourceMapping:orders-worker";
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "TRIGGERS").contains(&(queue, worker)));
}

#[test]
fn pulumi_lookalikes_without_import_proof_are_ignored() {
    // AC-0051/T-0051: constructor spelling is never enough to assert IaC.
    let source = r#"
const aws = makeTestDouble();
const queue = new aws.sqs.Queue('orders', {});
"#;
    let ex = extract_source(source.as_bytes(), "lookalike.ts", &id()).unwrap();
    assert!(ex.nodes.iter().all(|node| node.label != "Resource"));
}

// --- Literal eval()/new Function() extraction (#214, AC-0099) ---------------

// A string-literal eval() is compile-time-known: its Symbols and CALLS are
// Confirmed T0, cite the string's span at the eval site, carry `via: "eval"`,
// and the enclosing symbol CALLS the extracted code's entry symbol so the
// flow tracer walks through the eval boundary. (T-0099)
#[test]
fn literal_eval_extracts_facts_with_eval_site_provenance() {
    let src = r#"
export function boot() {
  eval("function setup() { registerLegacy(); } function registerLegacy() {} setup();");
}
"#;
    let ex = extract_source(src.as_bytes(), "src/boot.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let entry = calls
        .iter()
        .find(|(s, d)| {
            *s == "sym:qwtm/example@src/boot.ts#boot"
                && d.starts_with("sym:qwtm/example@src/boot.ts#eval@")
        })
        .expect("containment CALLS into the eval entry symbol")
        .1;
    // Eval-recovered symbols are namespaced under the entry (#217 review).
    let setup_id = format!("{entry}.setup");
    let register_id = format!("{entry}.registerLegacy");
    let setup = ex
        .nodes
        .iter()
        .find(|n| n.id == setup_id)
        .expect("eval-defined symbol");
    assert_eq!(setup.props["via"], "eval");
    assert_eq!(setup.props["prov"]["tier"], "Deterministic");
    assert_eq!(setup.props["prov"]["confidence_tier"], "Confirmed");
    // Evidence cites the string literal's span at the eval site — the
    // code's true source location.
    let ev = &setup.props["prov"]["evidence"][0];
    let span = &src
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    assert!(
        span.starts_with("\"function setup()"),
        "cites the string at the eval site: {span}"
    );

    assert!(calls.contains(&(entry, setup_id.as_str())));
    assert!(calls.contains(&(setup_id.as_str(), register_id.as_str())));
    let entry_node = ex.nodes.iter().find(|n| n.id == entry).expect("entry node");
    assert_eq!(entry_node.props["via"], "eval");
    assert!(
        entry_node.props["name"]
            .as_str()
            .unwrap()
            .starts_with("<eval@")
    );
    // The site is classified covered — preflight closes its finding.
    assert_eq!(
        ex.eval_sites,
        vec![EvalSite {
            path: "src/boot.ts".into(),
            line: 3,
            proof: EvalProof::Covered,
        }]
    );
}

// #217 review: an eval-defined symbol that shares its name with a real
// outer symbol is a different declaration — the store upserts nodes by id,
// so the eval-recovered fact must live under the site's namespace instead
// of merging with (and clobbering) the outer one. (T-0099)
#[test]
fn eval_defined_symbols_never_clobber_same_named_outer_symbols() {
    let src = r#"
function setup() {}
export function boot() {
  setup();
  eval("function setup() {} setup();");
}
"#;
    let ex = extract_source(src.as_bytes(), "src/app.ts", &id()).unwrap();
    let outer = ex
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@src/app.ts#setup")
        .expect("outer symbol keeps its id");
    assert!(outer.props.get("via").is_none(), "outer fact untouched");
    let calls = edge_pairs(&ex, "CALLS");
    let entry = calls
        .iter()
        .find(|(s, d)| {
            *s == "sym:qwtm/example@src/app.ts#boot"
                && d.starts_with("sym:qwtm/example@src/app.ts#eval@")
        })
        .expect("containment CALLS into the eval entry symbol")
        .1;
    let inner_id = format!("{entry}.setup");
    let inner = ex
        .nodes
        .iter()
        .find(|n| n.id == inner_id)
        .expect("eval-defined symbol is namespaced under its site");
    assert_eq!(inner.props["via"], "eval");
    // Each call targets its own declaration: boot's direct call the outer
    // symbol, the eval body's call the eval-scoped one.
    assert!(calls.contains(&(
        "sym:qwtm/example@src/app.ts#boot",
        "sym:qwtm/example@src/app.ts#setup"
    )));
    assert!(calls.contains(&(entry, inner_id.as_str())));
}

// #475: the same code at two eval sites yields distinct facts (the ids carry
// the site), so every eval-recovered fact's content hash must be derived
// from its final namespaced form — node id, or edge (src, label, dst) — not
// from the pre-namespacing inner fact, or distinct edges share one hash.
// Re-extraction stays byte-identical. (AC-0099, T-0099)
#[test]
fn identical_eval_code_at_two_sites_yields_distinct_fact_hashes() {
    let code = "function a() { b(); } function b() {} a();";
    let src = format!("export function run() {{\n  eval(\"{code}\");\n  eval(\"{code}\");\n}}\n");
    let ex = extract_source(src.as_bytes(), "src/twice.ts", &id()).unwrap();
    let sites: Vec<usize> = src.match_indices("eval(\"").map(|(at, _)| at).collect();
    assert_eq!(sites.len(), 2);
    // Same digit count, so the synthetic wrapper buffers are the same length
    // and the pre-namespacing inner facts are byte-identical across sites.
    assert_eq!(sites[0].to_string().len(), sites[1].to_string().len());
    let hash =
        |props: &serde_json::Value| props["prov"]["content_hash"].as_str().unwrap().to_string();
    let sym = |suffix: String| format!("sym:qwtm/example@src/twice.ts#{suffix}");
    let calls_hash = |src_id: String, dst_id: String| -> String {
        let edge = ex
            .edges
            .iter()
            .find(|e| e.label == "CALLS" && e.src == src_id && e.dst == dst_id)
            .unwrap_or_else(|| panic!("missing CALLS {src_id} -> {dst_id}"));
        hash(&edge.props)
    };
    let [s0, s1] = [sites[0], sites[1]];
    assert_ne!(
        calls_hash(sym(format!("eval@{s0}.a")), sym(format!("eval@{s0}.b"))),
        calls_hash(sym(format!("eval@{s1}.a")), sym(format!("eval@{s1}.b"))),
    );
    assert_ne!(
        calls_hash(sym(format!("eval@{s0}")), sym(format!("eval@{s0}.a"))),
        calls_hash(sym(format!("eval@{s1}")), sym(format!("eval@{s1}.a"))),
    );
    // Sweep: no two eval-recovered facts with distinct identities share a
    // hash, and each hash is the one derived from the fact's final form.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for node in ex.nodes.iter().filter(|n| n.props["via"] == "eval") {
        let identity = format!("{} {}", node.label, node.id);
        assert_eq!(
            hash(&node.props),
            core_prov::content_hash(identity.as_bytes())
        );
        if let Some(prior) = seen.insert(hash(&node.props), identity.clone()) {
            panic!("{identity} shares a hash with {prior}");
        }
    }
    for edge in ex.edges.iter().filter(|e| e.props["via"] == "eval") {
        let identity = format!("{} {} -> {}", edge.label, edge.src, edge.dst);
        assert_eq!(
            hash(&edge.props),
            core_prov::content_hash(identity.as_bytes())
        );
        if let Some(prior) = seen.insert(hash(&edge.props), identity.clone()) {
            panic!("{identity} shares a hash with {prior}");
        }
    }
    // Deterministic: re-ingesting the same source yields the same facts.
    let again = extract_source(src.as_bytes(), "src/twice.ts", &id()).unwrap();
    assert_eq!(ex.nodes, again.nodes);
    assert_eq!(ex.edges, again.edges);
}

// #498: the `eval-rules` Gap and its DEPENDS_ON edge are distinct facts, so
// each carries the hash of its own final identity — the node's
// `Gap <id>`, the edge's `DEPENDS_ON <src> -> <dst>` — never a shared one.
// (AC-0099, T-0099)
#[test]
fn eval_rules_gap_and_its_edge_hash_their_own_identities() {
    let src = "export function run() {\n  eval(\"function f(x) { if (!x) { return; } g(); } function g() {}\");\n}\n";
    let ex = extract_source(src.as_bytes(), "src/rules.ts", &id()).unwrap();
    let hash =
        |props: &serde_json::Value| props["prov"]["content_hash"].as_str().unwrap().to_string();
    let gap = ex
        .nodes
        .iter()
        .find(|n| n.label == "Gap" && n.id.contains("#eval-rules@"))
        .expect("guarded return in decoded eval code defers to an eval-rules Gap");
    let edge = ex
        .edges
        .iter()
        .find(|e| e.label == "DEPENDS_ON" && e.dst == gap.id)
        .expect("the eval entry DEPENDS_ON its eval-rules Gap");
    assert_eq!(
        hash(&gap.props),
        core_prov::content_hash(format!("Gap {}", gap.id).as_bytes())
    );
    assert_eq!(
        hash(&edge.props),
        core_prov::content_hash(format!("DEPENDS_ON {} -> {}", edge.src, edge.dst).as_bytes())
    );
    assert_ne!(hash(&gap.props), hash(&edge.props));
}

#[test]
fn eval_wrapper_projection_preserves_nested_lexical_owners() {
    // AC-0099 / AC-0120 / AC-0121: remove only the synthetic wrapper scope.
    let source = r#"
function boot() {
  eval("function shared() {} function first() { function shared() {} shared(); } function second() { shared(); } first(); second();");
}
"#;
    let out = extract_source(source.as_bytes(), "eval-scopes.ts", &id()).unwrap();
    let calls = edge_pairs(&out, "CALLS");
    let entry = calls
        .iter()
        .find(|(src, dst)| src.ends_with("#boot") && dst.contains("#eval@"))
        .unwrap()
        .1;
    let root_shared = format!("{entry}.shared");
    let first = format!("{entry}.first");
    let second = format!("{entry}.second");
    let nested = out
        .nodes
        .iter()
        .find(|node| node.id.starts_with(&format!("{first}/shared@")))
        .expect("nested declaration retains its lexical owner");
    assert!(calls.contains(&(first.as_str(), nested.id.as_str())));
    assert!(calls.contains(&(second.as_str(), root_shared.as_str())));
    assert!(!calls.contains(&(first.as_str(), root_shared.as_str())));
    for (src, dst) in calls {
        assert!(out.nodes.iter().any(|node| node.id == src));
        assert!(out.nodes.iter().any(|node| node.id == dst));
        assert!(!src.contains("__cgeval") && !dst.contains("__cgeval"));
    }
    assert_eq!(
        out.nodes
            .iter()
            .map(|node| &node.id)
            .collect::<BTreeSet<_>>()
            .len(),
        out.nodes.len()
    );
}

// A same-file `const CODE = '…'` and a const-object member proven through
// `const_resolution` are as compile-time-known as an inline literal. (T-0099)
#[test]
fn const_resolved_eval_extracts_through_binding_proof() {
    let src = r#"
const BOOT = "function legacyBoot() {}";
const Scripts = { report: "function legacyReport() {}" } as const;
export function run() {
  eval(BOOT);
  eval(Scripts.report);
}
"#;
    let ex = extract_source(src.as_bytes(), "src/legacy.ts", &id()).unwrap();
    for name in ["legacyBoot", "legacyReport"] {
        let node = ex
            .nodes
            .iter()
            .find(|n| {
                n.id.starts_with("sym:qwtm/example@src/legacy.ts#eval@")
                    && n.id.ends_with(&format!(".{name}"))
            })
            .unwrap_or_else(|| panic!("{name} extracted under its eval namespace"));
        assert_eq!(node.props["name"], *name);
        assert_eq!(node.props["via"], "eval");
        assert_eq!(node.props["prov"]["confidence_tier"], "Confirmed");
    }
    // Both sites prove to literals; `run` calls both entry symbols.
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![(5, EvalProof::Covered), (6, EvalProof::Covered)]
    );
    let entries = edge_pairs(&ex, "CALLS")
        .into_iter()
        .filter(|(s, d)| {
            *s == "sym:qwtm/example@src/legacy.ts#run"
                && d.starts_with("sym:qwtm/example@src/legacy.ts#eval@")
        })
        .count();
    assert_eq!(entries, 2);
}

// Unprovable arguments emit no recovered facts, ever — a const-shaped binding
// that cannot be proven becomes an explicit Gap (AC-0201, T-0201), and interpolated
// templates or computed expressions stay dynamic (Unsupported). (T-0099)
#[test]
fn unproven_and_interpolated_eval_yield_classification_but_no_facts() {
    let src = r#"
const code = build();
export function run(name: string) {
  eval(code);
  eval(`register('${name}')`);
  eval(name + '()');
}
"#;
    let ex = extract_source(src.as_bytes(), "src/dynamic.ts", &id()).unwrap();
    assert!(ex.nodes.iter().all(|n| n.props.get("via").is_none()));
    assert!(ex.edges.iter().all(|e| e.props.get("via").is_none()));
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![
            (4, EvalProof::ConstUnproven),
            (5, EvalProof::Dynamic),
            (6, EvalProof::Dynamic),
        ]
    );
    // Only the const-shaped site surfaces as a Gap; dynamic ones stay findings.
    let gaps: Vec<_> = ex.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(
        gaps.len(),
        1,
        "one Gap, for the const-shaped site: {gaps:?}"
    );
}

fn eval_gaps(ex: &Extraction) -> Vec<&Node> {
    ex.nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.id.contains("eval-unproven@"))
        .collect()
}

fn evidence_texts<'a>(src: &'a str, props: &serde_json::Value) -> Vec<&'a str> {
    props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ev| {
            &src[ev["byte_start"].as_u64().unwrap() as usize
                ..ev["byte_end"].as_u64().unwrap() as usize]
        })
        .collect()
}

// #444: a const-shaped eval() argument that cannot be proven to a literal is
// an explicit Gap node — never Confirmed, never silently dropped — owned by
// its enclosing symbol through DEPENDS_ON, citing the call and the argument,
// with an offset-keyed id that is identical across runs. (AC-0201, T-0201)
#[test]
fn const_unproven_eval_emits_an_explicit_gap_owned_by_its_symbol() {
    let src = "const CODE = load();\nexport function run() {\n  eval(CODE);\n}\n";
    let ex = extract_source(src.as_bytes(), "src/boot.ts", &id()).unwrap();
    let gaps = eval_gaps(&ex);
    assert_eq!(gaps.len(), 1, "exactly one Gap for the site: {gaps:?}");
    let gap = gaps[0];
    let offset = src.find("eval(CODE)").unwrap();
    assert_eq!(
        gap.id,
        format!("gap:qwtm/example@src/boot.ts#eval-unproven@{offset}")
    );
    assert_eq!(gap.props["construct"], "eval");
    assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    assert!(
        gap.props["reason"]
            .as_str()
            .unwrap()
            .contains("could not be proven to a literal")
    );
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(
        evidence_texts(src, &gap.props),
        vec!["eval(CODE)", "CODE"],
        "cites the call site and the unproven argument"
    );
    let owners: Vec<_> = ex
        .edges
        .iter()
        .filter(|e| e.dst == gap.id)
        .map(|e| (e.src.as_str(), e.label.as_str()))
        .collect();
    assert_eq!(
        owners,
        vec![("sym:qwtm/example@src/boot.ts#run", "DEPENDS_ON")]
    );
    // Nothing about the site is asserted as Confirmed.
    assert!(ex.nodes.iter().all(|n| n.props.get("via").is_none()));

    // Deterministic: the same source yields the same facts.
    let again = extract_source(src.as_bytes(), "src/boot.ts", &id()).unwrap();
    assert_eq!(ex.nodes, again.nodes);
    assert_eq!(ex.edges, again.edges);

    // Inside proven eval code the Gap is namespaced under that eval site, so a
    // synthetic-buffer offset can never collide with an outer-file Gap id.
    let nested_src = "export function run() {\n  eval(\"eval(CODE)\");\n}\n";
    let nested = extract_source(nested_src.as_bytes(), "src/nested.ts", &id()).unwrap();
    let gaps = eval_gaps(&nested);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    let outer = nested_src.find("eval(\"").unwrap();
    assert!(
        gaps[0].id.starts_with(&format!(
            "gap:qwtm/example@src/nested.ts#eval@{outer}.eval-unproven@"
        )),
        "{}",
        gaps[0].id
    );
    assert_eq!(gaps[0].props["via"], "eval");
    assert!(nested.edges.iter().any(|e| e.dst == gaps[0].id
        && e.src == format!("sym:qwtm/example@src/nested.ts#eval@{outer}")
        && e.label == "DEPENDS_ON"));
}

// #474: decoded offsets are not mapped back into the outer literal, so a
// nested Gap that cites its inner call and unproven argument collapses to ONE
// outer-literal span (never N identical ones) and says how many inner spans
// stayed unmapped; single-span eval facts carry no marker. (AC-0213, T-0213)
#[test]
fn nested_eval_gap_cites_one_outer_span_and_marks_inner_spans_unmapped() {
    let src = "export function run() {\n  eval(\"eval(CODE)\");\n}\n";
    let ex = extract_source(src.as_bytes(), "src/nested.ts", &id()).unwrap();
    let gaps = eval_gaps(&ex);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    let owner = ex
        .edges
        .iter()
        .find(|e| e.dst == gaps[0].id && e.label == "DEPENDS_ON")
        .expect("nested Gap keeps its owner edge");
    for props in [&gaps[0].props, &owner.props] {
        assert_eq!(evidence_texts(src, props), vec!["\"eval(CODE)\""]);
        // The inner `eval(CODE)` call and its `CODE` argument.
        assert_eq!(props["unmapped_inner_spans"], 2);
    }
    for node in ex.nodes.iter().filter(|n| n.props["via"] == "eval") {
        if node.id != gaps[0].id {
            assert!(
                node.props.get("unmapped_inner_spans").is_none(),
                "{}",
                node.id
            );
        }
    }
}

// #451 review: a proven outer eval whose code holds an unproven eval is not
// Covered — its claim downgrades so preflight keeps the potential-Gap finding
// the recovery emitted — and the same nested code at two outer sites yields
// distinct content hashes derived from each Gap's final, namespaced id.
// (AC-0201, T-0201)
#[test]
fn nested_const_unproven_eval_downgrades_the_outer_claim_and_rehashes() {
    let src = "export function run() {\n  eval(\"eval(CODE)\");\n  eval(\"eval(CODE)\");\n  eval(\"function ok() {}\");\n}\n";
    let ex = extract_source(src.as_bytes(), "src/nested.ts", &id()).unwrap();
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![
            (2, EvalProof::ConstUnproven),
            (3, EvalProof::ConstUnproven),
            (4, EvalProof::Covered),
        ]
    );
    let gaps = eval_gaps(&ex);
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    let hash =
        |props: &serde_json::Value| props["prov"]["content_hash"].as_str().unwrap().to_string();
    assert_ne!(hash(&gaps[0].props), hash(&gaps[1].props));
    for gap in &gaps {
        assert_eq!(
            hash(&gap.props),
            core_prov::content_hash(format!("Gap {}", gap.id).as_bytes())
        );
        let owner = ex
            .edges
            .iter()
            .find(|e| e.dst == gap.id && e.label == "DEPENDS_ON")
            .expect("nested Gap keeps its owner edge");
        assert_eq!(
            hash(&owner.props),
            core_prov::content_hash(
                format!("DEPENDS_ON {} -> {}", owner.src, owner.dst).as_bytes()
            )
        );
    }
}

// #476: claims combine worst-wins across nesting. A proven outer eval whose
// code holds a dynamic eval is claimed Dynamic, so preflight keeps the line's
// Unsupported finding — also when a const-unproven site sits beside it — while
// the facts recovered from the proven outer code stay Confirmed T0.
// (AC-0212, T-0212)
#[test]
fn nested_dynamic_eval_makes_the_outer_claim_dynamic() {
    let src = "export function run(x: string) {\n  eval(\"function inner() {} eval(x + 1);\");\n  eval(\"eval(CODE); eval(build());\");\n  eval(\"eval(CODE);\");\n}\n";
    let ex = extract_source(src.as_bytes(), "src/dyn.ts", &id()).unwrap();
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![
            (2, EvalProof::Dynamic),
            (3, EvalProof::Dynamic),
            (4, EvalProof::ConstUnproven),
        ]
    );
    let outer = src.find("eval(\"").unwrap();
    let inner = ex
        .nodes
        .iter()
        .find(|n| n.id == format!("sym:qwtm/example@src/dyn.ts#eval@{outer}.inner"))
        .expect("the proven outer code's facts are still recovered");
    assert_eq!(inner.props["via"], "eval");
    assert_eq!(inner.props["prov"]["confidence_tier"], "Confirmed");
}

// A top-level site has no enclosing symbol: the File owns the Gap. A
// `new Function` with several unproven arguments cites every one of them,
// while proven literal arguments are not cited. (AC-0201, T-0201)
#[test]
fn const_unproven_sites_bind_to_file_and_cite_every_unproven_argument() {
    let src = "import { PARAM, BODY } from './parts';\neval(BODY);\nexport function make() {\n  return new Function(PARAM, \"b\", BODY);\n}\n";
    let ex = extract_source(src.as_bytes(), "src/factory.ts", &id()).unwrap();
    let gaps = eval_gaps(&ex);
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    let top = gaps
        .iter()
        .find(|g| g.props["construct"] == "eval")
        .unwrap();
    assert!(
        ex.edges
            .iter()
            .any(|e| e.src == "file:qwtm/example@src/factory.ts"
                && e.dst == top.id
                && e.label == "DEPENDS_ON"),
        "top-level Gap is owned by its File"
    );
    let ctor = gaps
        .iter()
        .find(|g| g.props["construct"] == "new Function")
        .unwrap();
    assert_eq!(
        evidence_texts(src, &ctor.props),
        vec!["new Function(PARAM, \"b\", BODY)", "PARAM", "BODY"]
    );
    assert!(
        ex.edges
            .iter()
            .any(|e| e.src == "sym:qwtm/example@src/factory.ts#make"
                && e.dst == ctor.id
                && e.label == "DEPENDS_ON")
    );
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![(2, EvalProof::ConstUnproven), (4, EvalProof::ConstUnproven)]
    );

    // Proven and dynamic sites never produce an eval Gap.
    let other = extract_source(
        b"const OK = 'run()';\nexport function go(x: string) {\n  eval(OK);\n  eval(x + '');\n  new Function(build());\n}\n",
        "src/other.ts",
        &id(),
    )
    .unwrap();
    assert!(eval_gaps(&other).is_empty());
}

// A local binding named `eval` is NOT the global — no facts and no claim,
// following the `shadowed_fetch_is_not_a_fetch_site` precedent. (T-0099)
#[test]
fn shadowed_eval_is_not_the_global_and_yields_no_facts() {
    let local_shadow = extract_source(
        br#"
function eval(code) { return code; }
export function run() { eval("function fake() {}"); }
"#,
        "src/sandbox.js",
        &id(),
    )
    .unwrap();
    assert!(
        local_shadow
            .nodes
            .iter()
            .all(|n| n.id != "sym:qwtm/example@src/sandbox.js#fake")
    );
    assert!(local_shadow.eval_sites.is_empty());

    let imported_shadow = extract_source(
        br#"
import { eval } from './interpreter';
export function run() { eval("function fake() {}"); }
"#,
        "src/host.ts",
        &id(),
    )
    .unwrap();
    assert!(
        imported_shadow
            .nodes
            .iter()
            .all(|n| n.id != "sym:qwtm/example@src/host.ts#fake")
    );
    assert!(imported_shadow.eval_sites.is_empty());

    // #217 review: a NON-function local binding shadows too — a const bound
    // to anything, or a parameter — as does a `Function` binding for the
    // constructor form. None of these are the global evaluator.
    for (name, path, source) in [
        (
            "const-bound variable",
            "src/vm.js",
            "const eval = interpret;\nexport function run() { eval(\"function fake() {}\"); }\n",
        ),
        (
            "parameter",
            "src/exec.js",
            "export function run(eval) { eval(\"function fake() {}\"); }\n",
        ),
        (
            "Function constructor binding",
            "src/ctor.js",
            "const Function = makeCtor();\nexport function run() { return new Function(\"return 1;\"); }\n",
        ),
    ] {
        let shadowed = extract_source(source.as_bytes(), path, &id()).unwrap();
        assert!(
            shadowed.nodes.iter().all(|n| n.props.get("via").is_none()),
            "{name} shadow must not extract: {path}"
        );
        assert!(
            shadowed.eval_sites.is_empty(),
            "{name} shadow must not claim: {path}"
        );
    }
}

// new Function("a", "b", "body"): the LAST string argument is the body,
// earlier ones are parameter names; a computed body emits nothing. (T-0099)
#[test]
fn new_function_extracts_the_last_string_argument_as_body() {
    let src = r#"
export function make() {
  return new Function("a", "b", "function helper(x) { return x; } return helper(a) + helper(b);");
}
"#;
    let ex = extract_source(src.as_bytes(), "src/factory.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let entry = calls
        .iter()
        .find(|(s, d)| {
            *s == "sym:qwtm/example@src/factory.ts#make"
                && d.starts_with("sym:qwtm/example@src/factory.ts#eval@")
        })
        .expect("containment CALLS into the body entry symbol")
        .1;
    let helper_id = format!("{entry}.helper");
    let helper = ex
        .nodes
        .iter()
        .find(|n| n.id == helper_id)
        .expect("body-defined symbol");
    assert_eq!(helper.props["via"], "eval");
    // Evidence cites the BODY string (the last argument), not a parameter.
    let ev = &helper.props["prov"]["evidence"][0];
    let span = &src
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    assert!(span.starts_with("\"function helper"), "body span: {span}");
    assert!(calls.contains(&(entry, helper_id.as_str())));
    assert_eq!(
        ex.eval_sites,
        vec![EvalSite {
            path: "src/factory.ts".into(),
            line: 3,
            proof: EvalProof::Covered,
        }]
    );

    // Runtime-computed body: no facts, stays dynamic.
    let dynamic = extract_source(
        br#"
export function make() {
  return new Function(buildBody());
}
"#,
        "src/dyn.ts",
        &id(),
    )
    .unwrap();
    assert!(dynamic.nodes.iter().all(|n| n.props.get("via").is_none()));
    assert_eq!(
        dynamic.eval_sites,
        vec![EvalSite {
            path: "src/dyn.ts".into(),
            line: 3,
            proof: EvalProof::Dynamic,
        }]
    );
}

#[test]
fn directory_index_import_resolves_through_index_files() {
    // #213 (AC-0100): `./utils` meaning `./utils/index.ts` is the Node
    // directory-index convention; the extensionless guess must try the
    // index file before giving up, for IMPORTS and cross-file CALLS alike.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { helper } from './utils';\nexport function run() { helper(); }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("utils")).unwrap();
    std::fs::write(
        dir.path().join("utils/index.ts"),
        "export function helper() {}\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let index_file = "file:qwtm/example@utils/index.ts";
    assert!(edge_pairs(&out, "IMPORTS").contains(&("file:qwtm/example@app.ts", index_file)));
    assert!(
        !out.nodes
            .iter()
            .any(|n| n.id == "file:qwtm/example@utils.ts")
    );
    assert!(edge_pairs(&out, "CALLS").contains(&(
        "sym:qwtm/example@app.ts#run",
        "sym:qwtm/example@utils/index.ts#helper"
    )));
}

#[test]
fn nodenext_js_extension_import_resolves_to_the_ts_source() {
    // #213 (AC-0100): `moduleResolution: nodenext` requires imports to
    // spell the *emitted* `.js` extension — `import './foo.js'` written in
    // a tree that holds `foo.ts`. The spelled file doesn't exist; its TS
    // sibling does, and both the IMPORTS and CALLS edges must join it. A
    // real `.js` file spelled `.js` is left exactly as spelled.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { fetchUsers } from './api.js';\nimport { legacy } from './old.js';\nexport function run() { fetchUsers(); legacy(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("api.ts"),
        "export function fetchUsers() {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("old.js"), "export function legacy() {}\n").unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&("file:qwtm/example@app.ts", "file:qwtm/example@api.ts")));
    assert!(imports.contains(&("file:qwtm/example@app.ts", "file:qwtm/example@old.js")));
    // No phantom api.js placeholder survives.
    assert!(!out.nodes.iter().any(|n| n.id == "file:qwtm/example@api.js"));
    let calls = edge_pairs(&out, "CALLS");
    assert!(calls.contains(&(
        "sym:qwtm/example@app.ts#run",
        "sym:qwtm/example@api.ts#fetchUsers"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@app.ts#run",
        "sym:qwtm/example@old.js#legacy"
    )));
}

// AC-0544: config-driven bare-import resolution selects only a file's `mod:`
// IMPORTS and rewrites them identically over plain lists and the column store.
#[test]
fn bare_import_resolution_is_identical_over_lists_and_store() {
    use core_graph::facts::{FactLists, FactStore};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
    std::fs::write(
        dir.path().join("src/components/Button.tsx"),
        "export function Button() { return null; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/app.tsx"),
        "import react from 'react';\nexport const a = 1;\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let react = out
        .edges
        .iter()
        .find(|e| e.label == "IMPORTS" && e.dst == "mod:react")
        .unwrap()
        .clone();
    let mut alias = react.clone();
    alias.dst = "mod:@/components/Button".into();
    let mut foreign = alias.clone();
    foreign.src = "sym:qwtm/example@src/app.tsx#a".into();
    let nodes = out.nodes.clone();
    let edges = vec![alias, react.clone(), foreign.clone()];
    let known_files: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| n.label == "File")
        .map(|n| n.id.clone())
        .collect();
    let index = resolution::ResolutionIndex::load(dir.path()).unwrap();
    let (mut list_nodes, mut list_edges) = (nodes.clone(), edges.clone());
    resolution::resolve_bare_imports(
        &mut FactLists {
            nodes: &mut list_nodes,
            edges: &mut list_edges,
        },
        &index,
        &id(),
        &known_files,
    )
    .unwrap();
    let mut store = FactStore::from_facts(nodes, edges).unwrap();
    resolution::resolve_bare_imports(&mut store, &index, &id(), &known_files).unwrap();
    assert_eq!(
        store.into_facts().unwrap(),
        (list_nodes, list_edges.clone())
    );
    assert_eq!(
        list_edges[0].dst,
        "file:qwtm/example@src/components/Button.tsx"
    );
    assert_eq!(list_edges[0].props["resolved_via"], "tsconfig-paths");
    let prov: Provenance = serde_json::from_value(list_edges[0].props["prov"].clone()).unwrap();
    assert_eq!(prov.evidence.last().unwrap().path, "tsconfig.json");
    assert_eq!(list_edges[1], react);
    assert_eq!(list_edges[2], foreign);
}

// AC-0545: the directory closure decodes only edges with an open endpoint and
// appends the same classified placeholders over plain lists and the store.
#[test]
fn directory_closure_is_identical_over_lists_and_store() {
    use core_graph::facts::{FactLists, FactStore};
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/style.css"), "a {}\n").unwrap();
    std::fs::write(
        dir.path().join("src/app.tsx"),
        "import react from 'react';\nexport const a = 1;\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let react = out
        .edges
        .iter()
        .find(|e| e.label == "IMPORTS" && e.dst == "mod:react")
        .unwrap()
        .clone();
    let relative = |path: &str, spec: &str| {
        let mut edge = react.clone();
        edge.dst = format!("file:qwtm/example@src/{path}");
        edge.props["specifier"] = serde_json::json!(spec);
        edge
    };
    let nodes: Vec<Node> = out
        .nodes
        .iter()
        .filter(|n| n.label == "File")
        .cloned()
        .collect();
    let edges = vec![
        relative("style.css", "./style.css"),
        react.clone(),
        relative("missing.ts", "./missing"),
    ];
    let known_files: std::collections::HashSet<String> =
        nodes.iter().map(|n| n.id.clone()).collect();
    let index = resolution::ResolutionIndex::load(dir.path()).unwrap();
    let (mut list_nodes, mut list_edges) = (nodes.clone(), edges.clone());
    close_directory(
        &mut FactLists {
            nodes: &mut list_nodes,
            edges: &mut list_edges,
        },
        dir.path(),
        &id(),
        &index,
        &known_files,
    )
    .unwrap();
    let mut store = FactStore::from_facts(nodes.clone(), edges).unwrap();
    close_directory(&mut store, dir.path(), &id(), &index, &known_files).unwrap();
    assert_eq!(
        store.into_facts().unwrap(),
        (list_nodes.clone(), list_edges)
    );
    let tier = |id: &str| {
        let node = list_nodes[nodes.len()..]
            .iter()
            .find(|n| n.id == id)
            .unwrap();
        serde_json::from_value::<Provenance>(node.props["prov"].clone())
            .unwrap()
            .confidence_tier
    };
    assert_eq!(list_nodes.len(), nodes.len() + 3);
    assert_eq!(tier("file:qwtm/example@src/style.css"), Confirmed);
    assert_eq!(tier("mod:react"), Confirmed);
    assert_eq!(tier("file:qwtm/example@src/missing.ts"), Gap);
}

// AC-0546: the directory walk packs each file's facts as it merges and the
// incremental cache holds them packed; a re-ingest under a new commit that
// reuses one file and reparses another equals a fresh walk. AC-0863: the
// reused file's pending call is held packed and is retargeted to the new
// commit before it links.
#[test]
fn packed_walk_and_cache_reuse_match_a_fresh_walk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.ts"),
        "import { b } from './b';\nexport function a() { return b(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.ts"),
        "export function b() { return 1; }\n",
    )
    .unwrap();
    let at = |commit| SourceId {
        repo: "qwtm/example",
        commit,
    };
    let mut cache = IncrementalCache::default();
    let facts = |ex: &Extraction| (ex.nodes.clone(), ex.edges.clone());
    let (first, _) = extract_dir_incremental(dir.path(), &at("c1"), &mut cache).unwrap();
    let fresh = extract_dir(dir.path(), &at("c1")).unwrap();
    assert_eq!(facts(&first), facts(&fresh));
    assert_eq!(first.default_exports, fresh.default_exports);
    std::fs::write(
        dir.path().join("b.ts"),
        "export function b() { return 2; }\nexport function c() {}\n",
    )
    .unwrap();
    let (again, stats) = extract_dir_incremental(dir.path(), &at("c2"), &mut cache).unwrap();
    assert_eq!((stats.reused_files, stats.recomputed_files), (1, 1));
    let fresh = extract_dir(dir.path(), &at("c2")).unwrap();
    assert_eq!(facts(&again), facts(&fresh));
    assert!(
        !serde_json::to_string(&again.nodes)
            .unwrap()
            .contains("\"c1\"")
    );
    assert!(
        again
            .nodes
            .iter()
            .any(|n| n.id == "sym:qwtm/example@b.ts#c")
    );
}

#[test]
fn packed_walk_returns_the_decoded_walks_facts_packed() {
    // AC-0547 (#1574): the packed walk hands back the same facts, in order,
    // without decoding them, and leaves the extraction's lists empty.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.ts"),
        "import { b } from './b';\nimport x from 'left-pad';\nexport function a() { return b(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.ts"),
        "export function b() { return 1; }\n",
    )
    .unwrap();
    let id = SourceId {
        repo: "qwtm/example",
        commit: "c1",
    };
    let (decoded, _) =
        extract_dir_incremental(dir.path(), &id, &mut IncrementalCache::default()).unwrap();
    let (rest, store, stats) = extract_dir_incremental_packed(
        dir.path(),
        &id,
        &mut IncrementalCache::default(),
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(stats.recomputed_files, 2);
    assert!(rest.nodes.is_empty() && rest.edges.is_empty());
    assert_eq!(rest.default_exports, decoded.default_exports);
    assert_eq!(store.into_facts().unwrap(), (decoded.nodes, decoded.edges));
}

#[test]
fn tsconfig_paths_alias_resolves_to_a_real_file_with_the_config_cited() {
    // #213 (AC-0100): a `paths` alias is applied before the specifier is
    // classified as an opaque package, and the tsconfig that decided the
    // resolution is cited in the edge's provenance evidence. Unmatched
    // bare specifiers stay `mod:` nodes exactly as before.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{
  // JSONC on purpose — real tsconfigs carry comments.
  "compilerOptions": {
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"] },
  },
}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
    std::fs::write(
        dir.path().join("src/components/Button.tsx"),
        "export function Button() { return null; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/app.tsx"),
        "import { Button } from '@/components/Button';\nimport react from 'react';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<_> = out.edges.iter().filter(|e| e.label == "IMPORTS").collect();
    let resolved = imports
        .iter()
        .find(|e| e.dst == "file:qwtm/example@src/components/Button.tsx")
        .expect("alias resolves to the real file");
    assert_eq!(resolved.props["resolved_via"], "tsconfig-paths");
    let prov: Provenance = serde_json::from_value(resolved.props["prov"].clone()).unwrap();
    assert!(
        prov.evidence
            .iter()
            .any(|evidence| evidence.path == "tsconfig.json"),
        "the deciding config is citable evidence"
    );
    // External packages stay opaque — resolution never reaches node_modules.
    assert!(imports.iter().any(|e| e.dst == "mod:react"));
    assert!(!imports.iter().any(|e| e.dst == "mod:@/components/Button"));
}

#[test]
fn baseurl_bare_specifier_resolves_without_paths() {
    // #213: an explicit baseUrl alone lets bare specifiers name files
    // relative to it (classic pre-NodeNext TS).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/lib")).unwrap();
    std::fs::write(dir.path().join("src/lib/db.ts"), "export function q() {}\n").unwrap();
    std::fs::write(
        dir.path().join("src/main.ts"),
        "import { q } from 'lib/db';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&(
        "file:qwtm/example@src/main.ts",
        "file:qwtm/example@src/lib/db.ts"
    )));
}

#[test]
fn workspace_package_bare_specifier_resolves_through_its_exports_map() {
    // #213 (AC-0100): a bare specifier naming a sibling workspace package
    // resolves through that package's exports map to its real source, with
    // the package.json cited; anything not in the tree stays `mod:`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("packages/shared/src")).unwrap();
    std::fs::write(
        dir.path().join("packages/shared/package.json"),
        r#"{ "name": "@acme/shared", "exports": { ".": { "import": "./src/index.ts" } } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("packages/shared/src/index.ts"),
        "export function api() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("packages/app")).unwrap();
    std::fs::write(
        dir.path().join("packages/app/main.ts"),
        "import { api } from '@acme/shared';\nimport lodash from 'lodash';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<_> = out.edges.iter().filter(|e| e.label == "IMPORTS").collect();
    let resolved = imports
        .iter()
        .find(|e| e.dst == "file:qwtm/example@packages/shared/src/index.ts")
        .expect("workspace package resolves to its entry source");
    assert_eq!(resolved.props["resolved_via"], "workspace-package");
    let prov: Provenance = serde_json::from_value(resolved.props["prov"].clone()).unwrap();
    assert!(
        prov.evidence
            .iter()
            .any(|evidence| evidence.path == "packages/shared/package.json")
    );
    assert!(imports.iter().any(|e| e.dst == "mod:lodash"));

    // Deterministic: a second walk produces identical edges (US-0014).
    let again = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(out.edges, again.edges);
}

#[test]
fn unproven_alias_and_missing_index_fail_closed() {
    // #213: resolution is a proof against real files — an alias mapping to
    // nothing and an import with no file and no index stay explicit
    // (`mod:` node / placeholder), never a guessed rewrite.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { x } from '@/nothing';\nimport { y } from './missing';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<_> = out.edges.iter().filter(|e| e.label == "IMPORTS").collect();
    assert!(imports.iter().any(|e| e.dst == "mod:@/nothing"));
    // The relative miss keeps the guessed placeholder id (made an explicit
    // placeholder node by close_over_endpoints) — no invented resolution.
    assert!(
        imports
            .iter()
            .any(|e| e.dst == "file:qwtm/example@missing.ts")
    );
    let placeholder = out
        .nodes
        .iter()
        .find(|n| n.id == "file:qwtm/example@missing.ts")
        .expect("placeholder node exists");
    assert_eq!(placeholder.props["placeholder"], true);
}

#[test]
fn nested_tsconfig_shadows_parent_aliases_even_when_empty() {
    // #220 review: the nearest tsconfig governs its files outright — a
    // nested package with its own (alias-free) tsconfig must NOT have the
    // root config's aliases reach into it; the import stays an explicit
    // `mod:` node. Files governed by the root config keep resolving.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/thing.ts"), "export function t() {}\n").unwrap();
    std::fs::write(
        dir.path().join("root.ts"),
        "import { t } from '@/thing';\nexport const a = 1;\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("packages/app")).unwrap();
    std::fs::write(dir.path().join("packages/app/tsconfig.json"), "{}").unwrap();
    std::fs::write(
        dir.path().join("packages/app/main.ts"),
        "import { t } from '@/thing';\nexport const b = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    // Root-governed file resolves through the root alias…
    assert!(imports.contains(&(
        "file:qwtm/example@root.ts",
        "file:qwtm/example@src/thing.ts"
    )));
    // …but the nested package's identical import is shadowed by its own
    // config and stays unresolved rather than borrowing the root alias.
    assert!(imports.contains(&("file:qwtm/example@packages/app/main.ts", "mod:@/thing")));
}

#[test]
fn most_specific_paths_pattern_wins() {
    // #220 review: TypeScript picks the pattern with the longest prefix
    // before `*` — `@/foo/*` must beat `@/*` even when both targets exist.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"], "@/foo/*": ["special/*"] } } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/foo")).unwrap();
    std::fs::write(
        dir.path().join("src/foo/bar.ts"),
        "export const general = 1;\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("special")).unwrap();
    std::fs::write(
        dir.path().join("special/bar.ts"),
        "export const specific = 1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { specific } from '@/foo/bar';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&(
        "file:qwtm/example@app.ts",
        "file:qwtm/example@special/bar.ts"
    )));
    assert!(!imports.contains(&(
        "file:qwtm/example@app.ts",
        "file:qwtm/example@src/foo/bar.ts"
    )));
}

#[test]
fn computed_keys_and_decorators_use_the_creation_scope() {
    // AC-0120: a computed key cannot see the method's parameters or own calls.
    let source = r#"
function key() { return 'run'; }
function decorate() { return () => {}; }
function build() { return { [key()](key: () => void) { key(); } }; }
function buildClass() { class Example { @decorate() run() {} } return Example; }
"#;
    let out = extract_source(source.as_bytes(), "creation.ts", &id()).unwrap();
    let calls = edge_pairs(&out, "CALLS");
    assert_eq!(
        calls
            .iter()
            .filter(|(_, dst)| dst.ends_with("#key"))
            .count(),
        1
    );
    assert!(calls.contains(&(
        "sym:qwtm/example@creation.ts#build",
        "sym:qwtm/example@creation.ts#key"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@creation.ts#buildClass",
        "sym:qwtm/example@creation.ts#decorate"
    )));
    assert!(
        !calls
            .iter()
            .any(|(src, _)| src.contains(".computed@") || src.contains("Example"))
    );
}

#[test]
fn nested_class_initializers_never_borrow_outer_this() {
    // AC-0120: nested class fields/static blocks have their own runtime this.
    let source = r#"
class Outer {
  run() {}
  make() {
    const valid = () => this.run();
    return class Inner {
      run() {}
      callback = () => this.run();
      value = this.run();
      static { this.run(); }
    };
  }
}
"#;
    let out = extract_source(source.as_bytes(), "class-this.ts", &id()).unwrap();
    let calls = edge_pairs(&out, "CALLS");
    let outer_calls: Vec<_> = calls
        .iter()
        .filter(|(_, dst)| dst.ends_with("#Outer.run"))
        .collect();
    assert_eq!(outer_calls.len(), 1);
    assert!(outer_calls[0].0.contains("Outer.make/valid@"));
    assert!(!calls.iter().any(|(src, _)| src.ends_with("#Outer.make")));
}

#[test]
fn receiver_identity_is_bound_at_its_declaration_and_invalidated_by_assignment() {
    // AC-0120: declaration-site type proof cannot migrate to a shadowing class.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remote.ts"),
        "export class Remote { run() {} }",
    )
    .unwrap();
    std::fs::write(dir.path().join("receiver.ts"), r#"
import { Remote } from './remote';
class Service { run() {} }
class Replacement { run() {} }
function typed(receiver: Service) { class Service { run() {} } receiver.run(); }
function imported(receiver: Remote) { class Remote { run() {} } receiver.run(); }
function constructed() { const receiver = new Service(); { class Service { run() {} } receiver.run(); } }
function reassigned() { let receiver = new Service(); receiver = new Replacement(); receiver.run(); }
function changedLater() { const receiver = new Service(); function change() { receiver = new Replacement(); } receiver.run(); }
function qualified(receiver: Namespace.Service) { receiver.run(); }
function array(receiver: Service[]) { receiver.run(); }
"#).unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let calls = edge_pairs(&out, "CALLS");
    let local = "sym:qwtm/example@receiver.ts#Service.run";
    assert!(calls.contains(&("sym:qwtm/example@receiver.ts#typed", local)));
    assert!(calls.contains(&("sym:qwtm/example@receiver.ts#constructed", local)));
    assert!(calls.contains(&(
        "sym:qwtm/example@receiver.ts#imported",
        "sym:qwtm/example@remote.ts#Remote.run"
    )));
    for name in ["reassigned", "changedLater", "qualified", "array"] {
        assert!(
            !calls
                .iter()
                .any(|(src, _)| src.ends_with(&format!("#{name}"))),
            "{name} must not reuse invalid receiver proof"
        );
    }
}

#[test]
fn ambiguous_static_and_accessor_members_have_distinct_owners_without_guessed_dispatch() {
    // AC-0119 / AC-0120 / AC-0121: own every method, but prove its dispatch kind.
    let source = r#"
function first() {}
function second() {}
class Mixed {
  static run() { first(); }
  run() { second(); }
  get value() { first(); return 1; }
  set value(input: number) { second(); }
  static staticOnly() { first(); }
  plain() { second(); }
}
function use(receiver: Mixed) { receiver.run(); receiver.value(); receiver.staticOnly(); receiver.plain(); }
"#;
    let out = extract_source(source.as_bytes(), "members.ts", &id()).unwrap();
    let methods: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.props["kind"] == "Method")
        .collect();
    assert_eq!(methods.len(), 6);
    assert_eq!(
        methods
            .iter()
            .map(|node| &node.id)
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    let calls = edge_pairs(&out, "CALLS");
    for method in methods {
        let prov: Provenance = serde_json::from_value(method.props["prov"].clone()).unwrap();
        let evidence = &prov.evidence[0];
        let text = &source[evidence.byte_start as usize..evidence.byte_end as usize];
        let expected = if text.contains("first()") {
            "first"
        } else {
            "second"
        };
        assert!(calls.contains(&(
            method.id.as_str(),
            sym_id(id().repo, "members.ts", expected).as_str()
        )));
    }
    let use_calls: Vec<_> = calls
        .iter()
        .filter(|(src, _)| src.ends_with("#use"))
        .collect();
    assert_eq!(
        use_calls,
        vec![&(
            "sym:qwtm/example@members.ts#use",
            "sym:qwtm/example@members.ts#Mixed.plain"
        )]
    );
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id.ends_with("#Mixed.staticOnly")),
        "unique old member identity stays stable"
    );
}

#[test]
fn imported_static_and_accessor_methods_cannot_satisfy_instance_calls() {
    // AC-0120: directory target existence alone does not prove instance dispatch.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remote.ts"),
        "export class Remote { static run() {} get task() { return () => {}; } }",
    )
    .unwrap();
    std::fs::write(dir.path().join("caller.ts"), "import { Remote } from './remote'; function use(receiver: Remote) { receiver.run(); receiver.task(); }").unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let calls: Vec<_> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "CALLS" && edge.src.ends_with("#use"))
        .collect();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert!(
            out.nodes
                .iter()
                .any(|node| node.id == call.dst && node.label == "Gap")
        );
        assert_eq!(call.props["prov"]["confidence_tier"], "Gap");
    }
}

#[test]
fn with_environment_cannot_prove_global_fetch() {
    // AC-0120: an unresolvable dynamic scope is not proof of an unshadowed global.
    let out = client_extract(
        "dynamic-scope.tsx",
        r#"
function Dynamic() { with (unknownObject) { fetch('/unproven'); } }
function Genuine() { fetch('/known'); }
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    assert_eq!(
        out.fetch_sites[0].symbol.as_deref(),
        Some("sym:qwtm/example@dynamic-scope.tsx#Genuine")
    );
}

#[test]
fn named_class_expression_self_name_shadows_outer_callable_only_inside_class() {
    // AC-0120: the private class-expression name is not the outer function.
    let source = r#"
function C() {}
const Inner = class C { run() { C(); } };
function outside() { C(); }
"#;
    let out = extract_source(source.as_bytes(), "class-self.ts", &id()).unwrap();
    assert_eq!(
        edge_pairs(&out, "CALLS"),
        vec![(
            "sym:qwtm/example@class-self.ts#outside",
            "sym:qwtm/example@class-self.ts#C"
        )]
    );
}

#[test]
fn named_class_expression_type_does_not_borrow_outer_class_proof() {
    // AC-0120: same-spelled types inside a named class expression cannot resolve
    // to the outer class. Existing this dispatch and outside type proof survive.
    let source = r#"
class C { run() {} }
const Inner = class C {
  run() {}
  use(receiver: C) { receiver.run(); this.run(); }
};
function outside(receiver: C) { receiver.run(); }
"#;
    let out = extract_source(source.as_bytes(), "class-self-type.ts", &id()).unwrap();
    let inner_method = |suffix: &str| {
        out.nodes
            .iter()
            .find(|node| {
                node.props["class"]
                    .as_str()
                    .is_some_and(|class| class.starts_with("C@"))
                    && node.id.ends_with(suffix)
            })
            .unwrap()
    };
    let inner_use = inner_method(".use");
    let inner_run = inner_method(".run");
    let calls = edge_pairs(&out, "CALLS");
    assert!(calls.contains(&(inner_use.id.as_str(), inner_run.id.as_str())));
    assert!(!calls.contains(&(
        inner_use.id.as_str(),
        "sym:qwtm/example@class-self-type.ts#C.run"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@class-self-type.ts#outside",
        "sym:qwtm/example@class-self-type.ts#C.run"
    )));
}

#[test]
fn parameter_decorator_this_does_not_borrow_decorated_instance() {
    // AC-0120: parameter decorators execute at creation, while the method body
    // still has its own instance receiver. Unsupported ambient this stays open.
    let source = r#"
function deco(value: any) { return () => {}; }
class Outer {
  run() {}
  build() {
    class Inner {
      run() {}
      method(@deco(this.run()) value: any) { this.run(); }
    }
    return Inner;
  }
}
"#;
    let out = extract_source(source.as_bytes(), "decorator-this.ts", &id()).unwrap();
    let calls = edge_pairs(&out, "CALLS");
    let creation_calls: Vec<_> = calls
        .iter()
        .filter(|(src, _)| src.ends_with("#Outer.build"))
        .collect();
    assert_eq!(
        creation_calls,
        vec![&(
            "sym:qwtm/example@decorator-this.ts#Outer.build",
            "sym:qwtm/example@decorator-this.ts#deco"
        )]
    );
    let inner_method = out
        .nodes
        .iter()
        .find(|node| {
            node.props["class"]
                .as_str()
                .is_some_and(|class| class.starts_with("Outer.build/Inner@"))
                && node.id.ends_with(".method")
        })
        .unwrap();
    let inner_run = format!(
        "sym:qwtm/example@decorator-this.ts#{}.run",
        inner_method.props["class"].as_str().unwrap()
    );
    assert!(calls.contains(&(inner_method.id.as_str(), inner_run.as_str())));
}

#[test]
fn nested_nest_controllers_bind_their_actual_qualified_handlers() {
    // AC-0121: decorators bind the same actual method as symbol extraction,
    // including distinct same-spelled controllers in non-module scopes.
    let source = br#"
import { Controller, Get } from '@nestjs/common';
@Controller('top')
class SharedController { @Get() list() {} }
function create() {
  @Controller('function')
  class SharedController { @Get() list() {} }
  return SharedController;
}
{
  @Controller('block')
  class SharedController { @Get() list() {} }
}
namespace Nested {
  @Controller('namespace')
  export class SharedController { @Get() list() {} }
}
"#;
    let out = extract_source(source, "src/controllers.ts", &id()).unwrap();
    let handlers: Vec<_> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "HANDLES")
        .collect();
    assert_eq!(handlers.len(), 4);
    let mut targets = std::collections::BTreeSet::new();
    for scope in ["top", "function", "block", "namespace"] {
        let endpoint = format!("ep:qwtm/example@GET:/{scope}");
        let handler = handlers.iter().find(|edge| edge.src == endpoint).unwrap();
        let symbol = out
            .nodes
            .iter()
            .find(|node| node.id == handler.dst)
            .expect("HANDLES target must be a real emitted symbol");
        assert_eq!(symbol.label, "Symbol");
        assert_eq!(symbol.props["kind"], "Method");
        let prov: Provenance = serde_json::from_value(symbol.props["prov"].clone()).unwrap();
        assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
        let evidence = &prov.evidence[0];
        let method = &source[evidence.byte_start as usize..evidence.byte_end as usize];
        assert!(std::str::from_utf8(method).unwrap().contains("list() {}"));
        assert!(
            out.edges
                .iter()
                .any(|edge| edge.src == symbol.id && edge.label == "DEFINED_IN")
        );
        assert!(targets.insert(symbol.id.clone()));
        if scope == "top" {
            assert_eq!(
                symbol.id,
                "sym:qwtm/example@src/controllers.ts#SharedController.list"
            );
        } else {
            assert_ne!(
                symbol.id,
                "sym:qwtm/example@src/controllers.ts#SharedController.list"
            );
        }
    }
    let repeated = extract_source(source, "src/controllers.ts", &id()).unwrap();
    assert_eq!(out.nodes, repeated.nodes);
    assert_eq!(out.edges, repeated.edges);
}

#[test]
fn shadowed_nest_decorators_never_borrow_file_import_proof() {
    // AC-0120: class/method decorators execute at their creation site and
    // cannot borrow file-wide import proof through unrelated local bindings.
    for parameters in ["Controller: any", "Get: any", "Controller: any, Get: any"] {
        let source = format!(
            r#"
import {{ Controller, Get }} from '@nestjs/common';
function make({parameters}) {{
  @Controller('fake')
  class NestedController {{ @Get() run() {{}} }}
}}
"#
        );
        let out = extract_source(source.as_bytes(), "src/shadowed.ts", &id()).unwrap();
        assert!(out.nodes.iter().any(|node| node.props["kind"] == "Method"));
        assert!(
            !out.nodes.iter().any(|node| node.label == "Endpoint"),
            "{parameters}"
        );
        assert!(
            !out.edges.iter().any(|edge| edge.label == "HANDLES"),
            "{parameters}"
        );
    }
}

#[test]
fn bare_import_targets_carry_provenance_and_only_proven_externals_confirm() {
    // AC-0207 (#237): a bare specifier left after config-driven resolution
    // is Confirmed external only when nothing in the repository could answer
    // it; aliases, workspace packages, subpath imports, source directories,
    // and relative misses stay explicit Gaps. Every placeholder cites the
    // import statement that named it.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "package.json",
        r#"{ "name": "app", "dependencies": { "react": "^18.0.0" } }"#,
    );
    write(
        "tsconfig.json",
        r#"{ "compilerOptions": { "paths": { "@app/*": ["src/*"] } } }"#,
    );
    write(
        "packages/shared/package.json",
        r#"{ "name": "@acme/shared" }"#,
    );
    write("src/utils/format.ts", "export const f = 1;\n");
    write("src/styles.css", "body {}\n");
    let main = "import './styles.css';\nimport './gone.css';\nimport React from 'react';\nimport fs from 'node:fs';\nimport path from 'path';\nimport left from 'left-pad';\nimport { gone } from '@app/gone';\nimport { s } from '@acme/shared/deep';\nimport { t } from '@acme/shared';\nimport { h } from '#internal/h';\nimport { u } from 'utils/format';\nimport { a } from '~/alias';\nimport { m } from './missing';\n";
    write("src/main.ts", main);
    let out = extract_dir(dir.path(), &id()).unwrap();
    let boundary = |id: &str| {
        let node = out
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("missing node {id}"));
        assert_eq!(node.props["placeholder"], true, "{id}");
        let prov: Provenance = serde_json::from_value(node.props["prov"].clone())
            .unwrap_or_else(|_| panic!("{id} carries no provenance"));
        let span = &prov.evidence[0];
        assert_eq!(span.path, "src/main.ts", "{id}");
        assert!(main[span.byte_start as usize..span.byte_end as usize].starts_with("import "));
        (
            node.props["boundary"].as_str().unwrap().to_string(),
            prov.confidence_tier,
            prov.evidence.len(),
        )
    };
    // A declared dependency also cites its package.json declaration.
    assert_eq!(boundary("mod:react"), ("external".into(), Confirmed, 2));
    assert_eq!(boundary("mod:node:fs"), ("external".into(), Confirmed, 1));
    assert_eq!(boundary("mod:path"), ("external".into(), Confirmed, 1));
    assert_eq!(boundary("mod:left-pad"), ("external".into(), Confirmed, 1));
    // The workspace package itself is the repository's own, cited by its
    // manifest; a subpath no file answers stays a Gap (below).
    assert_eq!(
        boundary("mod:@acme/shared"),
        ("internal".into(), Confirmed, 2)
    );
    for in_system in [
        "mod:@app/gone",
        "mod:@acme/shared/deep",
        "mod:#internal/h",
        "mod:utils/format",
        "mod:~/alias",
        "file:qwtm/example@src/missing.ts",
        "file:qwtm/example@src/gone.css",
    ] {
        assert_eq!(
            boundary(in_system),
            ("unresolved".into(), Gap, 1),
            "{in_system}"
        );
    }
}

#[test]
fn an_extending_tsconfig_never_confirms_an_undeclared_bare_specifier() {
    // AC-0207 (#237 review): inherited `paths`/`baseUrl` are not loaded, so
    // under a tsconfig that `extends` another, an inherited alias such as
    // `@app/foo` stays an explicit Gap; a manifest-declared dependency is
    // still proven external by its declaration.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "package.json",
        r#"{ "name": "app", "dependencies": { "react": "^18.0.0" } }"#,
    );
    write(
        "tsconfig.base.json",
        r#"{ "compilerOptions": { "paths": { "@app/*": ["lib/*"] } } }"#,
    );
    write("tsconfig.json", r#"{ "extends": "./tsconfig.base.json" }"#);
    write(
        "src/main.ts",
        "import React from 'react';\nimport { foo } from '@app/foo';\nimport fs from 'fs';\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let boundary = |id: &str| {
        let node = out
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("missing node {id}"));
        let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
        (
            node.props["boundary"].as_str().unwrap().to_string(),
            prov.confidence_tier,
        )
    };
    assert_eq!(boundary("mod:react"), ("external".into(), Confirmed));
    assert_eq!(boundary("mod:@app/foo"), ("unresolved".into(), Gap));
    assert_eq!(boundary("mod:fs"), ("unresolved".into(), Gap));
}

#[test]
fn an_asset_import_escaping_the_repository_is_never_internal() {
    // AC-0207 (#237 review): `../../outside.css` from `src/main.ts` climbs
    // above the root; the clamped id names an in-repo `outside.css` that
    // happens to exist, which proves nothing about the imported file.
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("outside.css", "body {}\n");
    write(
        "src/main.ts",
        "import '../../outside.css';\nimport '../outside.css';\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let edges: Vec<_> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS")
        .collect();
    assert_eq!(edges.len(), 2);
    let node = out
        .nodes
        .iter()
        .find(|node| node.id == "file:qwtm/example@outside.css")
        .unwrap();
    // Both specifiers clamp to the same id; the escaping one keeps it a Gap.
    assert_eq!(node.props["boundary"], "unresolved");
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert_eq!(prov.confidence_tier, core_prov::ConfidenceTier::Gap);
}

#[test]
fn literal_bundler_aliases_keep_package_shaped_specifiers_in_system() {
    // AC-0220 (#463): a Vite/webpack `resolve.alias` key spelled like a
    // package name is read as data (never executed). A literal path target
    // resolves to the real file citing the config; an unproven one stays an
    // explicit Gap naming the config; a key aliased to another package or to
    // a non-literal value keeps today's external classification.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let vite = r#"import { defineConfig } from 'vite';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const dyn = process.env.X;
export default defineConfig({
  resolve: {
    alias: {
      components: path.resolve(__dirname, 'app/components'),
      'widgets': './app/widgets',
      assets: fileURLToPath(new URL('./app/assets', import.meta.url)),
      lodash: 'lodash-es',
      dynamic: dyn,
    },
  },
});
"#;
    write("vite.config.ts", vite);
    write(
        "web/webpack.config.js",
        "module.exports = { resolve: { alias: { store$: '/abs/store.js', [key]: './x' } } };\n",
    );
    write("app/components/Button.tsx", "export const Button = 1;\n");
    write("app/widgets/index.ts", "export const w = 1;\n");
    write(
        "src/main.ts",
        "import { Button } from 'components/Button';\nimport { w } from 'widgets';\nimport { gone } from 'components/Gone';\nimport logo from 'assets/logo';\nimport _ from 'lodash';\nimport d from 'dynamic';\n",
    );
    write(
        "web/app.ts",
        "import s from 'store';\nimport sub from 'store/sub';\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let edge = |dst: &str| {
        out.edges
            .iter()
            .find(|edge| edge.label == "IMPORTS" && edge.dst == dst)
            .unwrap_or_else(|| panic!("no IMPORTS edge to {dst}"))
    };
    let button = edge("file:qwtm/example@app/components/Button.tsx");
    assert_eq!(button.props["resolved_via"], "bundler-alias");
    let prov: Provenance = serde_json::from_value(button.props["prov"].clone()).unwrap();
    let cited = prov
        .evidence
        .iter()
        .find(|evidence| evidence.path == "vite.config.ts")
        .expect("the deciding bundler config is citable evidence");
    assert_eq!(
        &vite[cited.byte_start as usize..cited.byte_end as usize],
        "components"
    );
    assert_eq!(
        edge("file:qwtm/example@app/widgets/index.ts").props["resolved_via"],
        "bundler-alias"
    );
    let boundary = |id: &str| {
        let node = out
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("missing node {id}"));
        let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
        (
            node.props["boundary"].as_str().unwrap().to_string(),
            prov.confidence_tier,
            node.props["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    };
    for (gap, config) in [
        ("mod:components/Gone", "vite.config.ts"),
        ("mod:assets/logo", "vite.config.ts"),
        ("mod:store", "web/webpack.config.js"),
    ] {
        let (kind, tier, reason) = boundary(gap);
        assert_eq!((kind.as_str(), tier), ("unresolved", Gap), "{gap}");
        assert!(reason.contains(config), "{gap}: {reason}");
    }
    // Package-to-package and non-literal aliases keep the old behavior.
    assert_eq!(boundary("mod:lodash").0, "external");
    assert_eq!(boundary("mod:lodash").1, Confirmed);
    assert_eq!(boundary("mod:dynamic").0, "external");
    // webpack `store$` is exact: it does not address `store/sub`.
    assert_eq!(boundary("mod:store/sub").0, "external");
}

/// Extract a tree of `(path, text)` files and return the extraction.
fn extract_tree(files: &[(&str, &str)]) -> Extraction {
    let dir = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    extract_dir(dir.path(), &id()).unwrap()
}

// AC-0848: test code yields no product Endpoint (#1250's rule for TS):
// Vendure's `e2e/fixtures/test-plugins/*.ts` controllers and
// `*.e2e-spec.ts` files were documented as the system's own routes.
#[test]
fn test_trees_yield_no_product_endpoints() {
    const CONTROLLER: &[u8] = br#"
import { Controller, Get } from '@nestjs/common';
@Controller('run-job')
export class TestController {
  @Get()
  runJob() {}
}
"#;
    let endpoints = |path: &str| {
        let out = extract_source(CONTROLLER, path, &id()).unwrap();
        let ids: Vec<String> = out
            .nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .map(|node| node.id.clone())
            .collect();
        assert!(
            out.edges
                .iter()
                .all(|edge| !edge.src.starts_with("ep:") || ids.contains(&edge.src)),
            "no edge outlives its endpoint"
        );
        ids
    };
    for path in [
        "e2e/fixtures/test-plugins/with-job-queue.ts",
        "e2e/error-handler-strategy.e2e-spec.ts",
        "src/__tests__/job.controller.ts",
        "packages/core/test/fixtures/job.controller.ts",
        "src/job.controller.spec.ts",
    ] {
        assert!(endpoints(path).is_empty(), "{path}");
    }
    // Product code, including names that only contain "test", keeps its routes.
    for path in [
        "src/job.controller.ts",
        "src/testing/job.controller.ts",
        "src/latest/job.controller.ts",
    ] {
        assert_eq!(endpoints(path), ["ep:qwtm/example@GET:/run-job"], "{path}");
    }
}

// AC-0848: a fixture that is itself a Next.js package routes nothing; its
// deferred route sites are dropped, not completed at the walk's end.
#[test]
fn a_next_fixture_in_a_test_tree_routes_nothing() {
    let package = r#"{ "dependencies": { "next": "15.0.0" } }"#;
    let route = "export default function ping(req, res) { res.json({}); }\n";
    let out = extract_tree(&[
        ("__fixtures__/site/package.json", package),
        ("__fixtures__/site/pages/api/ping.ts", route),
        ("web/package.json", package),
        ("web/pages/api/ping.ts", route),
    ]);
    let next: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && n.props["framework"] == "next")
        .filter_map(|n| n.props["prov"]["evidence"][0]["path"].as_str())
        .collect();
    assert_eq!(next, ["web/pages/api/ping.ts"]);
}

/// `(boundary, tier, reason)` of placeholder `id`.
fn placeholder(out: &Extraction, id: &str) -> (String, core_prov::ConfidenceTier, String) {
    let node = out
        .nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("missing node {id}"));
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    (
        node.props["boundary"].as_str().unwrap().to_string(),
        prov.confidence_tier,
        node.props["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

fn imports_file(out: &Extraction, rel: &str) -> bool {
    let dst = format!("file:qwtm/example@{rel}");
    out.edges
        .iter()
        .any(|edge| edge.label == "IMPORTS" && edge.dst == dst)
}

#[test]
fn a_vite_alias_key_keeps_a_trailing_dollar_literally() {
    // AC-0220 (#507 review): `key$` is webpack's exact-match syntax only; a
    // Vite key ending in `$` is matched as spelled.
    let out = extract_tree(&[
        (
            "vite.config.ts",
            "export default { resolve: { alias: { 'ui$': './app/ui' } } };\n",
        ),
        ("app/ui/Button.tsx", "export const Button = 1;\n"),
        (
            "src/main.ts",
            "import b from 'ui$/Button';\nimport u from 'ui';\n",
        ),
    ]);
    assert!(imports_file(&out, "app/ui/Button.tsx"));
    // `ui` is not the Vite key `ui$`, so nothing in-system addresses it.
    assert_eq!(placeholder(&out, "mod:ui").0, "external");
}

#[test]
fn only_an_alias_in_the_exported_config_resolves() {
    // AC-0220 (#507 review): an alias object not provably in the exported
    // config — a nested helper, or one of several alias objects in a file —
    // never resolves a specifier; the specifier stays an in-system Gap
    // citing the config, never Confirmed and never external.
    use core_prov::ConfidenceTier::Gap;
    let out = extract_tree(&[
        (
            "vite.config.ts",
            "export default defineConfig(({ mode }) => ({ resolve: { alias: { kit: './app/kit' } } }));\n",
        ),
        (
            "helper/vite.config.ts",
            "const unused = { resolve: { alias: { comps: './comps' } } };\nexport default defineConfig({});\n",
        ),
        (
            "multi/webpack.config.js",
            "module.exports = {\n  resolve: { alias: { parts: './parts' } },\n  plugins: [new P({ resolve: { alias: { other: './other' } } })],\n};\n",
        ),
        ("helper/comps/Button.tsx", "export const Button = 1;\n"),
        ("helper/main.ts", "import { Button } from 'comps/Button';\n"),
        ("app/kit/Card.tsx", "export const Card = 1;\n"),
        ("multi/parts/Wheel.ts", "export const Wheel = 1;\n"),
        ("src/main.ts", "import { Card } from 'kit/Card';\n"),
        ("multi/app.ts", "import { Wheel } from 'parts/Wheel';\n"),
    ]);
    // `defineConfig(env => ({ … }))` is the exported config.
    assert!(imports_file(&out, "app/kit/Card.tsx"));
    assert!(!imports_file(&out, "helper/comps/Button.tsx"));
    assert!(!imports_file(&out, "multi/parts/Wheel.ts"));
    for (gap, config) in [
        ("mod:comps/Button", "helper/vite.config.ts"),
        ("mod:parts/Wheel", "multi/webpack.config.js"),
    ] {
        let (kind, tier, reason) = placeholder(&out, gap);
        assert_eq!((kind.as_str(), tier), ("unresolved", Gap), "{gap}");
        assert!(reason.contains(config), "{gap}: {reason}");
    }
}

#[test]
fn equal_scope_configs_that_disagree_on_an_alias_never_resolve_it() {
    // AC-0220 (#507 review): a Vite and a webpack config in one directory
    // that map the same key to different targets leave the specifier a
    // Gap; configs that agree still resolve it.
    use core_prov::ConfidenceTier::Gap;
    let out = extract_tree(&[
        (
            "vite.config.ts",
            "export default { resolve: { alias: { shared: './a', same: './a' } } };\n",
        ),
        (
            "webpack.config.js",
            "module.exports = { resolve: { alias: { shared: './b', same: './a' } } };\n",
        ),
        ("a/x.ts", "export const x = 1;\n"),
        ("b/x.ts", "export const x = 2;\n"),
        (
            "src/main.ts",
            "import { x } from 'shared/x';\nimport { x as y } from 'same/x';\n",
        ),
    ]);
    assert!(!imports_file(&out, "b/x.ts"));
    let (kind, tier, _) = placeholder(&out, "mod:shared/x");
    assert_eq!((kind.as_str(), tier), ("unresolved", Gap));
    assert!(imports_file(&out, "a/x.ts"), "agreeing configs resolve");
}

#[test]
fn vue_script_blocks_are_parsed_with_spans_in_the_component() {
    // AC-0268 (#595): `<script>` and `<script setup>` parse with this
    // crate's extractor; template text is never read as code, and every
    // span names its place in the `.vue` file itself.
    const COMPONENT: &str = "<template>\n  <button @click=\"save()\">{{ label }}</button>\n</template>\n\
        <script>\nexport default { name: 'Editor' }\n</script>\n\
        <script setup lang=\"ts\">\nimport { load } from './api'\n\
        function save(): Promise<Response> { return fetch('/api/notes', { method: 'POST' }) }\n\
        function refresh() { return load() }\n</script>\n\
        <style>\n.save { color: red }\n</style>\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Editor.vue"), COMPONENT).unwrap();
    std::fs::write(
        dir.path().join("api.ts"),
        "export function load() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.ts"),
        "import Editor from './Editor.vue';\nexport const root = Editor;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let vue = "file:qwtm/example@Editor.vue";
    assert!(
        out.nodes
            .iter()
            .any(|n| n.id == vue && n.props.get("placeholder").is_none())
    );
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&(vue, "file:qwtm/example@api.ts")));
    assert!(imports.contains(&("file:qwtm/example@main.ts", vue)));
    assert!(edge_pairs(&out, "CALLS").contains(&(
        "sym:qwtm/example@Editor.vue#refresh",
        "sym:qwtm/example@api.ts#load"
    )));
    // The template's `save()` handler is not a call site.
    assert!(
        !edge_pairs(&out, "CALLS")
            .iter()
            .any(|(_, dst)| dst.ends_with("#save"))
    );
    let [site] = out.fetch_sites.as_slice() else {
        panic!("one fetch site, got {:?}", out.fetch_sites);
    };
    assert_eq!(site.method, "POST");
    assert_eq!(site.path, "Editor.vue");
    let start = COMPONENT.find("fetch('/api/notes'").unwrap() as u64;
    assert_eq!(site.byte_start, start);
    let save = out
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@Editor.vue#save")
        .expect("save symbol");
    let span = &save.props["prov"]["evidence"][0];
    let start = span["byte_start"].as_u64().unwrap() as usize;
    let end = span["byte_end"].as_u64().unwrap() as usize;
    assert!(COMPONENT[start..end].starts_with("function save()"));

    // Re-extraction of unchanged bytes is identical.
    let again = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        serde_json::to_string(&out.nodes).unwrap(),
        serde_json::to_string(&again.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&out.edges).unwrap(),
        serde_json::to_string(&again.edges).unwrap()
    );
}

#[test]
fn vue_typescript_casts_and_same_line_blocks_keep_their_facts() {
    // AC-0268 (#655 review): `lang="ts"` keeps the TypeScript grammar, so an
    // angle-bracket cast cannot swallow the declarations after it; and a
    // trailing line comment in one block cannot hide the next block.
    let cast = "<script setup lang=\"ts\">\nconst n = <number>raw;\n\
                function helper() { return n; }\nfunction run() { return helper(); }\n</script>\n";
    let out = extract_source(cast.as_bytes(), "Cast.vue", &id()).unwrap();
    assert!(edge_pairs(&out, "CALLS").contains(&(
        "sym:qwtm/example@Cast.vue#run",
        "sym:qwtm/example@Cast.vue#helper"
    )));

    let same_line = "<script>const a = 1 // normal script</script><script setup>\
                     function save() { return fetch('/api/notes') }</script>\n";
    let out = extract_source(same_line.as_bytes(), "Same.vue", &id()).unwrap();
    assert!(
        out.nodes
            .iter()
            .any(|n| n.id == "sym:qwtm/example@Same.vue#save")
    );
    let [site] = out.fetch_sites.as_slice() else {
        panic!("one fetch site, got {:?}", out.fetch_sites);
    };
    assert_eq!(site.byte_start, same_line.find("fetch(").unwrap() as u64);
}

const NATS_REGISTRY: &str = r#"{"schema":"cartograph-event-sdk-registry@1","version":"nats-1",
  "sdks":[{"kind":"nats-subject","role":"produces",
    "pattern":{"method":{"module":"nats","ctor":"Client","method":"publish"}},
    "identity":"first"}]}"#;
const NATS_SOURCE: &str = "import { Client } from 'nats';\n\
const nc = new Client();\n\
export function notify() { nc.publish('orders.placed', 'x'); }\n";

#[test]
fn an_extension_registry_entry_produces_sites_that_name_its_file() {
    let builtin = extract_source(NATS_SOURCE.as_bytes(), "src/nats.ts", &id()).unwrap();
    assert!(builtin.event_sites.is_empty());
    let layered = Registries::layered(&[("nats.json".into(), NATS_REGISTRY.as_bytes().to_vec())]);
    let ex = extract_source_with(NATS_SOURCE.as_bytes(), "src/nats.ts", &id(), &layered).unwrap();
    assert_eq!(ex.event_sites.len(), 1);
    let site = &ex.event_sites[0];
    assert_eq!(site.kind, "nats-subject");
    assert_eq!(site.identity, IdentityExpr::Literal("orders.placed".into()));
    assert_eq!(site.registries.len(), 1);
    let registry = &site.registries[0];
    assert_eq!(registry.name, "extension:nats.json");
    assert_eq!(registry.version, "nats-1");
    // Built-in sites name the built-in file.
    let sqs = extract_source(
        b"import { SendMessageCommand } from '@aws-sdk/client-sqs';\nnew SendMessageCommand({ QueueUrl: 'q' });\n",
        "src/sqs.ts",
        &id(),
    )
    .unwrap();
    assert_eq!(
        sqs.event_sites[0].registries[0].name,
        "builtin:event-sdks.json"
    );
}

#[test]
fn a_rejected_extension_registry_changes_no_fact() {
    let broken = NATS_REGISTRY.replace("produces", "observes");
    let layered = Registries::layered(&[("nats.json".into(), broken.into_bytes())]);
    assert_eq!(layered.rejected.len(), 1);
    let builtin = extract_source(NATS_SOURCE.as_bytes(), "src/nats.ts", &id()).unwrap();
    let with = extract_source_with(NATS_SOURCE.as_bytes(), "src/nats.ts", &id(), &layered).unwrap();
    assert_eq!(format!("{with:?}"), format!("{builtin:?}"));
}

#[test]
fn a_registry_change_invalidates_the_incremental_cache() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("nats.ts"), NATS_SOURCE).unwrap();
    let layered = Registries::layered(&[("nats.json".into(), NATS_REGISTRY.as_bytes().to_vec())]);
    let mut cache = IncrementalCache::default();
    let run = |cache: &mut IncrementalCache, registries: &Registries| {
        extract_dir_incremental_with_progress(dir.path(), &id(), cache, registries, &mut |_| {})
            .unwrap()
    };
    let (first, _) = run(&mut cache, Registries::builtin());
    assert!(first.event_sites.is_empty());
    let (same, stats) = run(&mut cache, Registries::builtin());
    assert_eq!((stats.reused_files, same.event_sites.len()), (1, 0));
    let (extended, stats) = run(&mut cache, &layered);
    assert_eq!((stats.recomputed_files, stats.reused_files), (1, 0));
    assert_eq!(extended.event_sites.len(), 1);
    let (back, stats) = run(&mut cache, Registries::builtin());
    assert_eq!((stats.recomputed_files, back.event_sites.len()), (1, 0));
}

const REGION_QUALIFIER: &str = r#"{"schema":"cartograph-event-sdk-registry@1","version":"region-1",
  "qualifiers":[{"kind":"sqs-queue","component":"region","key":"Region"}]}"#;

#[test]
fn a_qualifier_only_extension_is_named_on_the_sites_it_shapes() {
    let layered =
        Registries::layered(&[("region.json".into(), REGION_QUALIFIER.as_bytes().to_vec())]);
    assert!(layered.rejected.is_empty(), "{:?}", layered.rejected);
    let source = b"import { SendMessageCommand } from '@aws-sdk/client-sqs';\n\
new SendMessageCommand({ QueueUrl: 'q', Region: 'eu-west-1' });\n\
new SendMessageCommand({ QueueUrl: 'q' });\n";
    let ex = extract_source_with(source, "src/sqs.ts", &id(), &layered).unwrap();
    assert_eq!(ex.event_sites.len(), 2);
    let shaped = &ex.event_sites[0];
    assert_eq!(
        shaped.components.get("region").map(String::as_str),
        Some("eu-west-1")
    );
    let names: Vec<&str> = shaped.registries.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["builtin:event-sdks.json", "extension:region.json"]);
    // A qualifier that proved nothing contributed nothing.
    let plain = &ex.event_sites[1];
    assert!(plain.components.is_empty());
    let names: Vec<&str> = plain.registries.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["builtin:event-sdks.json"]);
}

// AC-0347 (T-0347).
#[test]
fn nest_graphql_resolvers_emit_scoped_operations_and_gap_computed_fields() {
    let ex = extract_source(
        br#"
import { Args, Mutation as Mut, Query, ResolveField, Resolver } from '@nestjs/graphql';
import { Allow } from '../../decorators/allow.decorator';
import { Permission } from '@vendure/common';

const NAME = 'dynamic';

@Resolver()
export class CustomerResolver {
  @Query()
  @Allow(Permission.ReadCustomer, Permission.Owner)
  async customers(@Args() args: unknown) {}

  @Query('customerByEmail')
  @Allow(Permission.ReadCustomer, someOther)
  byEmail() {}

  @Query(() => String, { name: 'customerCount' })
  count() {}

  @Mut({ description: 'x' })
  createCustomer() {}

  @Mut(NAME)
  computed() {}

  @ResolveField()
  orders() {}
}
"#,
        "packages/core/src/api/resolvers/admin/customer.resolver.ts",
        &id(),
    )
    .unwrap();
    let endpoints: BTreeMap<_, _> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| (n.id.as_str(), n))
        .collect();
    assert_eq!(
        endpoints.keys().copied().collect::<Vec<_>>(),
        [
            "ep:qwtm/example@graphql:admin:mutation:createCustomer",
            "ep:qwtm/example@graphql:admin:query:customerByEmail",
            "ep:qwtm/example@graphql:admin:query:customerCount",
            "ep:qwtm/example@graphql:admin:query:customers",
        ]
    );
    let customers = endpoints["ep:qwtm/example@graphql:admin:query:customers"];
    assert_eq!(customers.props["method"], "QUERY");
    assert_eq!(customers.props["path"], "admin.customers");
    assert_eq!(customers.props["protocol"], "graphql");
    assert_eq!(customers.props["scope_basis"], "resolver_directory");
    assert_eq!(customers.props["prov"]["confidence_tier"], "InferredStrong");
    assert_eq!(
        customers.props["permissions"],
        serde_json::json!(["ReadCustomer", "Owner"])
    );
    // A non-`Permission.X` argument records nothing rather than a subset.
    assert!(
        endpoints["ep:qwtm/example@graphql:admin:query:customerByEmail"].props["permissions"]
            .is_null()
    );
    assert!(edge_pairs(&ex, "HANDLES").contains(&(
        "ep:qwtm/example@graphql:admin:query:customers",
        "sym:qwtm/example@packages/core/src/api/resolvers/admin/customer.resolver.ts#CustomerResolver.customers"
    )));
    let gap = ex
        .nodes
        .iter()
        .find(|n| n.label == "Gap" && n.props["kind"] == "graphql-field-computed")
        .unwrap();
    assert_eq!(
        gap.props["reason"],
        graphql_resolvers::COMPUTED_FIELD_REASON
    );
    assert!(
        !endpoints
            .keys()
            .any(|id| id.contains("orders") || id.contains("computed"))
    );

    // Outside `resolvers/<api>/` the API is `default`; a look-alike
    // `Resolver` not imported from @nestjs/graphql proves nothing.
    let plugin = extract_source(
        br#"
import { Query, Resolver } from '@nestjs/graphql';
@Resolver()
export class PluginResolver { @Query() pluginThing() {} }
"#,
        "src/plugin/plugin.resolver.ts",
        &id(),
    )
    .unwrap();
    let ep = plugin.nodes.iter().find(|n| n.label == "Endpoint").unwrap();
    assert_eq!(ep.id, "ep:qwtm/example@graphql:default:query:pluginThing");
    assert_eq!(ep.props["scope_basis"], "none");
    let lookalike = extract_source(
        br#"
import { Query } from '@nestjs/graphql';
function Resolver() { return (_: unknown) => {}; }
@Resolver()
class Fake { @Query() thing() {} }
"#,
        "src/api/resolvers/admin/fake.ts",
        &id(),
    )
    .unwrap();
    assert!(lookalike.nodes.iter().all(|n| n.label != "Endpoint"));
    // A class that is both a resolver and a controller keeps its HTTP routes.
    let hybrid = extract_source(
        br#"
import { Controller, Get } from '@nestjs/common';
import { Query, Resolver } from '@nestjs/graphql';
@Resolver()
@Controller('health')
export class HealthResolver {
  @Query() health() {}
  @Get('live') live() {}
}
"#,
        "src/api/resolvers/admin/health.resolver.ts",
        &id(),
    )
    .unwrap();
    let mut methods: Vec<_> = hybrid
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| {
            (
                n.props["method"].as_str().unwrap(),
                n.props["path"].as_str().unwrap(),
            )
        })
        .collect();
    methods.sort();
    assert_eq!(
        methods,
        [("GET", "/health/live"), ("QUERY", "admin.health")]
    );
}

// AC-0348 (T-0348): the GraphQL root-field reader.
#[test]
fn graphql_root_fields_read_one_operation_and_refuse_ambiguity() {
    use target_tests::root_fields;
    let (kind, fields) = root_fields(
        "# list\nquery GetCustomers($options: CustomerListOptions = { take: 2 }) {\n  list: customers(options: $options) @include(if: true) { items { id } }\n  __typename\n  activeChannel { id }\n}\nfragment F on Customer { id }",
    )
    .unwrap();
    assert_eq!(kind, "query");
    assert_eq!(fields, ["activeChannel", "customers"]);
    assert_eq!(
        root_fields("mutation { createCustomer(input: { name: \"a{b\" }) { id } }").unwrap(),
        ("mutation", vec!["createCustomer".to_string()])
    );
    // A shorthand query and a nested interpolation are fine.
    assert_eq!(root_fields("{ me { \u{1} } }").unwrap().1, ["me"]);
    for (text, reason) in [
        (
            "query A { a } query B { b }",
            "GraphQL document has more than one operation",
        ),
        (
            "query { ...Root }",
            "GraphQL operation selects a fragment at its root",
        ),
        (
            "query { \u{1} }",
            "GraphQL document is interpolated at its root",
        ),
        (
            "fragment F on X { id }",
            "GraphQL document has no operation",
        ),
        ("query { a(", "GraphQL document is malformed"),
    ] {
        assert_eq!(root_fields(text).unwrap_err(), reason, "{text}");
    }
}

// AC-0348 (T-0348).
#[test]
fn vitest_tests_request_graphql_operations_through_proven_clients() {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "src/api/resolvers/admin/customer.resolver.ts",
        "import { Mutation, Query, Resolver } from '@nestjs/graphql';\n@Resolver()\nexport class CustomerResolver {\n  @Query() customers() {}\n  @Mutation() createCustomer() {}\n}\n",
    );
    write(
        "src/api/resolvers/shop/shop.resolver.ts",
        "import { Query, Resolver } from '@nestjs/graphql';\n@Resolver()\nexport class ShopResolver { @Query() activeCustomer() {} }\n",
    );
    write(
        "src/plugin/plugin.resolver.ts",
        "import { Query, Resolver } from '@nestjs/graphql';\n@Resolver()\nexport class PluginResolver { @Query() pluginThing() {} }\n",
    );
    write(
        "e2e/graphql/shared.ts",
        "import { graphql } from './graphql-admin';\nexport const getCustomersDocument = graphql(`query GetCustomers { customers { items { id } } }`);\nexport const notADocument = `query { customers { id } }`;\n",
    );
    write(
        "e2e/customer.e2e-spec.ts",
        r#"import { createTestEnvironment } from '@vendure/testing';
import { describe, it, expect } from 'vitest';
import gql from 'graphql-tag';
import { getCustomersDocument, notADocument, missingDocument } from './graphql/shared';

const { server, adminClient, shopClient: shop } = createTestEnvironment({});
const CREATE = gql`mutation { createCustomer(input: {}) { id } }`;

describe('Customer resolver', () => {
  it('lists customers', async () => {
    const r = await adminClient.query(getCustomersDocument);
    await adminClient.query(CREATE, {});
    await shop.query(gql`query { activeCustomer { id } pluginThing unknownField }`);
    expect(r).toBeDefined();
  });
  describe('nested', () => {
    it.only('lists customers', async () => {
      await shop.query(getCustomersDocument);
    });
    it('lists customers', async () => {
      await adminClient.query(notADocument);
      await adminClient.query(missingDocument);
      await adminClient.query(`query { customers { id } }`);
    });
  });
  it('wrapped', assertThrows(async () => {
    await adminClient.query(getCustomersDocument);
  }, 'message'));
  it(`computed ${1}`, () => {});
  it('shadowed', async () => {
    const adminClient = { query: (_: unknown) => {} };
    await adminClient.query(CREATE);
  });
});
"#,
    );
    // A monorepo spec importing the factory's own source file.
    write(
        "testing/lib/create-test-environment.ts",
        "export function createTestEnvironment(_: unknown) { return {}; }\n",
    );
    write(
        "e2e/monorepo.e2e-spec.ts",
        "import { createTestEnvironment } from '../testing/lib/create-test-environment';\nimport { it } from 'vitest';\nimport { getCustomersDocument } from './graphql/shared';\nconst { adminClient } = createTestEnvironment({});\nit('monorepo', async () => { await adminClient.query(getCustomersDocument); });\n",
    );
    let ex = extract_dir(dir.path(), &id()).unwrap();
    assert!(ex.edges.iter().any(|e| e.label == "REQUESTS"
        && e.src == "test:qwtm/example@e2e/monorepo.e2e-spec.ts#monorepo"
        && e.dst == "ep:qwtm/example@graphql:admin:query:customers"));
    let tests: Vec<_> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "TestCase" && n.props["path"] == "e2e/customer.e2e-spec.ts")
        .map(|n| n.id.as_str())
        .collect();
    let base = "test:qwtm/example@e2e/customer.e2e-spec.ts#Customer resolver";
    assert_eq!(
        tests,
        [
            format!("{base} > lists customers"),
            format!("{base} > nested > lists customers"),
            format!("{base} > nested > lists customers [2]"),
            format!("{base} > wrapped"),
            format!("{base} > shadowed"),
        ]
    );
    let requests: BTreeSet<_> = ex
        .edges
        .iter()
        .filter(|e| e.label == "REQUESTS" && !e.dst.starts_with("gap:") && e.src.starts_with(base))
        .map(|e| {
            (
                e.src.trim_start_matches(base),
                e.dst.as_str(),
                e.props["prov"]["confidence_tier"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        requests,
        BTreeSet::from([
            (
                " > lists customers",
                "ep:qwtm/example@graphql:admin:query:customers",
                "InferredStrong"
            ),
            (
                " > lists customers",
                "ep:qwtm/example@graphql:admin:mutation:createCustomer",
                "InferredStrong"
            ),
            (
                " > lists customers",
                "ep:qwtm/example@graphql:shop:query:activeCustomer",
                "InferredStrong"
            ),
            // Only a default-scoped resolver serves it: assumed, so weak.
            (
                " > lists customers",
                "ep:qwtm/example@graphql:default:query:pluginThing",
                "InferredWeak"
            ),
            // A callback passed through a wrapper is still the test's code.
            (
                " > wrapped",
                "ep:qwtm/example@graphql:admin:query:customers",
                "InferredStrong"
            ),
        ])
    );
    let gaps: BTreeMap<_, _> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.id.starts_with("gap:test-"))
        .map(|n| {
            (
                n.props["field"].as_str().unwrap_or(""),
                n.props["reason"].as_str().unwrap(),
            )
        })
        .fold(BTreeMap::new(), |mut map, (field, reason)| {
            *map.entry((field, reason)).or_insert(0) += 1;
            map
        });
    assert_eq!(
        gaps,
        BTreeMap::from([
            // The shop client's `customers` is admin-only; `unknownField` is unserved.
            (("customers", target_tests::UNSERVED_FIELD_REASON), 1),
            (("unknownField", target_tests::UNSERVED_FIELD_REASON), 1),
            // A plain template, an unexported import and an untagged literal.
            (("", target_tests::COMPUTED_DOCUMENT_REASON), 3),
            (("", target_tests::COMPUTED_NAME_REASON), 1),
        ])
    );
    // The shadowed local `adminClient` is not the test client.
    assert!(
        !ex.edges
            .iter()
            .any(|e| e.src.ends_with("> shadowed") && e.label == "REQUESTS")
    );
    let only = ex
        .nodes
        .iter()
        .find(|n| n.id.ends_with("nested > lists customers"))
        .unwrap();
    assert_eq!(only.props["modifier"], "only");
}

// AC-0348 (T-0348): only the test's own code is read, and an imported
// document comes from the file the import resolves to.
#[test]
fn vitest_requests_skip_declared_helpers_and_resolve_the_imported_file() {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "src/api/resolvers/admin/product.resolver.ts",
        "import { Query, Resolver } from '@nestjs/graphql';\n@Resolver()\nexport class ProductResolver { @Query() product() {} @Query() products() {} }\n",
    );
    // `explicit.ts` exists but its export is not a recovered document; the
    // directory index beside it exports a literal of the same name.
    write(
        "e2e/docs/explicit.ts",
        "export const doc = buildDocument();\n",
    );
    write(
        "e2e/docs/explicit/index.ts",
        "import { graphql } from './graphql';\nexport const doc = graphql(`query { products { id } }`);\n",
    );
    // NodeNext: `./next.js` names the TypeScript source it is emitted from.
    write(
        "e2e/docs/next.ts",
        "import { graphql } from './graphql';\nexport const doc = graphql(`query { products { id } }`);\n",
    );
    // `missing.ts` does not exist: its siblings are not what it names.
    for path in ["e2e/docs/missing/index.ts", "e2e/docs/missing.tsx"] {
        write(
            path,
            "import { graphql } from './graphql';\nexport const doc = graphql(`query { products { id } }`);\n",
        );
    }
    write(
        "e2e/docs/listed/index.ts",
        "import { graphql } from './graphql';\nexport const doc = graphql(`query { products { id } }`);\n",
    );
    write(
        "e2e/product.e2e-spec.ts",
        r#"import { createTestEnvironment } from '@vendure/testing';
import { it } from 'vitest';
import { graphql } from './graphql';
import { doc as explicitDoc } from './docs/explicit.ts';
import { doc as extensionless } from './docs/explicit';
import { doc as indexed } from './docs/listed';
import { doc as missing } from './docs/missing.ts';
import { doc as nodenext } from './docs/next.js';
const { adminClient } = createTestEnvironment({});
const PRODUCT = graphql(`query { product { id } }`);
it('no request', async () => {
  function unused() { adminClient.query(PRODUCT); }
  const alsoUnused = async () => { await adminClient.query(PRODUCT); };
  const wrapped = (() => adminClient.query(PRODUCT));
  const typed = (async () => { await adminClient.query(PRODUCT); }) as () => Promise<void>;
  const checked = (() => adminClient.query(PRODUCT)) satisfies () => unknown;
});
it('iife', async () => { await (async () => adminClient.query(PRODUCT))(); });
it('direct', async () => {
  await adminClient.query(PRODUCT);
  await Promise.all([1].map(async () => adminClient.query(PRODUCT)));
});
it('explicit file', async () => { await adminClient.query(explicitDoc); });
it('existing file', async () => { await adminClient.query(extensionless); });
it('directory index', async () => { await adminClient.query(indexed); });
it('missing explicit file', async () => { await adminClient.query(missing); });
it('nodenext', async () => { await adminClient.query(nodenext); });
"#,
    );
    let ex = extract_dir(dir.path(), &id()).unwrap();
    let base = "test:qwtm/example@e2e/product.e2e-spec.ts#";
    let requests = |test: &str| -> BTreeSet<&str> {
        ex.edges
            .iter()
            .filter(|e| e.label == "REQUESTS" && e.src == format!("{base}{test}"))
            .map(|e| {
                if e.dst.starts_with("gap:") {
                    "gap"
                } else {
                    e.dst.as_str()
                }
            })
            .collect()
    };
    // An uncalled helper, declared or bound to a name, is not the test's code.
    assert!(requests("no request").is_empty());
    // An immediately invoked function is.
    assert_eq!(
        requests("iife"),
        BTreeSet::from(["ep:qwtm/example@graphql:admin:query:product"])
    );
    // A direct call and a callback passed as an argument are.
    assert_eq!(
        requests("direct"),
        BTreeSet::from(["ep:qwtm/example@graphql:admin:query:product"])
    );
    // The imported file exists and its export is not recovered: a Gap, not
    // the directory index's same-named export.
    assert_eq!(requests("explicit file"), BTreeSet::from(["gap"]));
    assert_eq!(requests("existing file"), BTreeSet::from(["gap"]));
    // An explicit `.ts` that does not exist is a Gap, never a sibling.
    assert_eq!(requests("missing explicit file"), BTreeSet::from(["gap"]));
    assert_eq!(
        requests("nodenext"),
        BTreeSet::from(["ep:qwtm/example@graphql:admin:query:products"])
    );
    // Each test is implemented by its own callback, a recovered Symbol.
    let symbols: BTreeSet<&str> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Symbol")
        .map(|n| n.id.as_str())
        .collect();
    for test in ex.nodes.iter().filter(|n| n.label == "TestCase") {
        let [implemented] = ex
            .edges
            .iter()
            .filter(|e| e.label == "IMPLEMENTED_BY" && e.src == test.id)
            .collect::<Vec<_>>()[..]
        else {
            panic!("{} has one IMPLEMENTED_BY", test.id)
        };
        assert!(
            symbols.contains(implemented.dst.as_str()),
            "{}",
            implemented.dst
        );
    }
    // With no `listed.ts`, the import resolves to the directory index.
    assert_eq!(
        requests("directory index"),
        BTreeSet::from(["ep:qwtm/example@graphql:admin:query:products"])
    );
}

#[test]
fn vue_router_tables_become_screens_rendering_their_components() {
    // AC-0349 (#989): SimpleLogin's extension shape. The table is a static
    // method of an imported class, its paths come through `static PATH =
    // PATH`, and its components come through a `const` object argument.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "popup/Navigation.js",
        "const PATH = { ROOT: '/', MAIN: '/main', RESULT: '/new-alias-result/' };\n\
         class Navigation {\n  static PATH = PATH;\n\
         \x20 static getRoutes(components) {\n    return [\n\
         \x20     { path: Navigation.PATH.ROOT, component: components.SplashScreen },\n\
         \x20     { path: Navigation.PATH.MAIN, component: components.Main },\n\
         \x20     { path: Navigation.PATH.RESULT, component: components.NewAliasResult },\n\
         \x20     { path: computePath(), component: components.Main },\n\
         \x20     { path: '/lost', component: components.Missing },\n\
         \x20   ];\n  }\n}\nexport default Navigation;\n",
    );
    for name in ["SplashScreen", "Main", "NewAliasResult"] {
        write(
            &format!("popup/components/{name}.vue"),
            "<template><div/></template>\n<script>\nexport default {}\n</script>\n",
        );
    }
    write(
        "popup/App.vue",
        "<template><router-view/></template>\n<script>\n\
         import VueRouter from 'vue-router';\nimport Navigation from './Navigation';\n\
         import SplashScreen from './components/SplashScreen';\n\
         import Main from './components/Main';\n\
         import NewAliasResult from './components/NewAliasResult.vue';\n\
         const components = { SplashScreen, Main, NewAliasResult };\n\
         const routes = Navigation.getRoutes(components);\n\
         const router = new VueRouter({ mode: 'abstract', routes });\n\
         export default { router };\n</script>\n",
    );
    // Not proven: a local `VueRouter` and an unrecovered table.
    write(
        "other/fake.js",
        "class VueRouter {}\nnew VueRouter({ routes: [{ path: '/fake', component: X }] });\n",
    );
    write(
        "other/router.ts",
        "import { createRouter } from 'vue-router';\nimport Home from './Home.vue';\n\
         createRouter({ routes: [{ path: '/', redirect: '/home' }, { path: '/home', component: Home, \
         children: [{ path: 'tab', component: Home }] }] });\n\
         createRouter({ routes: load() });\n",
    );
    write(
        "other/Home.vue",
        "<template><div/></template>\n<script>\nexport default {}\n</script>\n",
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let screens: BTreeSet<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| {
            assert_eq!(n.props["router"], "vue-router");
            n.id.as_str()
        })
        .collect();
    let screen = |route: &str| format!("screen:qwtm/example@{route}");
    let expected: BTreeSet<String> = [
        "/",
        "/main",
        "/new-alias-result/",
        "/lost",
        "/home",
        "/home/tab",
    ]
    .into_iter()
    .map(screen)
    .collect();
    assert_eq!(
        screens,
        expected.iter().map(String::as_str).collect::<BTreeSet<_>>()
    );
    let renders = edge_pairs(&out, "RENDERS");
    for (route, file) in [
        ("/", "popup/components/SplashScreen.vue"),
        ("/main", "popup/components/Main.vue"),
        ("/new-alias-result/", "popup/components/NewAliasResult.vue"),
        ("/home", "other/Home.vue"),
        ("/home/tab", "other/Home.vue"),
    ] {
        let (src, dst) = (screen(route), format!("file:qwtm/example@{file}"));
        assert!(renders.contains(&(src.as_str(), dst.as_str())), "{route}");
    }
    // `Main` is imported without an extension: the `.vue` file is assumed,
    // while `NewAliasResult.vue` is named exactly.
    let resolution = |route: &str| {
        out.edges
            .iter()
            .find(|e| e.label == "RENDERS" && e.src == screen(route))
            .map(|e| {
                (
                    e.props["resolution"].clone(),
                    e.props["prov"]["confidence_tier"].clone(),
                )
            })
            .unwrap()
    };
    assert_eq!(
        resolution("/main"),
        (
            client_routes::VUE_EXTENSION_ASSUMED.into(),
            "InferredStrong".into()
        )
    );
    assert_eq!(
        resolution("/new-alias-result/"),
        (serde_json::Value::Null, "Confirmed".into())
    );
    // `other/router.ts` redirects `/` too: the rendering route wins.
    let root = out.nodes.iter().find(|n| n.id == screen("/")).unwrap();
    assert!(root.props.get("redirect").is_none());
    let gaps: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.props["kind"] == "route-unresolved")
        .map(|n| n.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        gaps.iter()
            .filter(|r| **r == client_routes::COMPUTED_PATH_REASON)
            .count(),
        1
    );
    assert_eq!(
        gaps.iter()
            .filter(|r| **r == client_routes::UNRESOLVED_COMPONENT_REASON)
            .count(),
        1
    );
    assert_eq!(
        gaps.iter()
            .filter(|r| **r == client_routes::UNRESOLVED_TABLE_REASON)
            .count(),
        1
    );
    assert!(
        renders.contains(&(
            screen("/lost").as_str(),
            out.nodes
                .iter()
                .find(|n| n.props["reason"] == client_routes::UNRESOLVED_COMPONENT_REASON)
                .unwrap()
                .id
                .as_str()
        ))
    );
}

#[test]
fn vue_router_components_honor_an_explicit_extension() {
    // AC-0349 (#989): an explicit extension names only that file and its
    // NodeNext siblings; only an extensionless import may fall back to `.vue`.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let sfc = "<template><div/></template>\n<script>\nexport default {}\n</script>\n";
    write("src/Missing.vue", sfc);
    write("src/Panels.vue", sfc);
    write("src/Built.ts", "export default {};\n");
    write(
        "src/router.ts",
        "import { createRouter } from 'vue-router';\n\
         import Missing from './Missing.js';\nimport Built from './Built.js';\n\
         import { Panel } from './Panels';\n\
         createRouter({ routes: [{ path: '/missing', component: Missing }, \
         { path: '/built', component: Built }, { path: '/panel', component: Panel }] });\n",
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let renders = |route: &str| {
        let src = format!("screen:qwtm/example@{route}");
        out.edges
            .iter()
            .filter(|e| e.label == "RENDERS" && e.src == src)
            .map(|e| {
                (
                    e.dst.clone(),
                    e.props["resolution"].as_str().map(str::to_string),
                )
            })
            .collect::<Vec<_>>()
    };
    // `./Missing.js` never guesses its way to `Missing.vue`: the screen
    // renders an unresolved-component Gap instead.
    let missing = renders("/missing");
    assert_eq!(missing.len(), 1);
    let gap = out.nodes.iter().find(|n| n.id == missing[0].0).unwrap();
    assert_eq!(
        gap.props["reason"],
        client_routes::UNRESOLVED_COMPONENT_REASON
    );
    // `./Built.js` is the NodeNext spelling of `Built.ts`: confirmed.
    assert_eq!(
        renders("/built"),
        [("file:qwtm/example@src/Built.ts".to_string(), None)]
    );
    // An extensionless named import may still resolve to `.vue`, assumed.
    assert_eq!(
        renders("/panel"),
        [(
            "file:qwtm/example@src/Panels.vue".to_string(),
            Some(client_routes::VUE_EXTENSION_ASSUMED.to_string())
        )]
    );
}

#[test]
fn vue_navigations_link_symbols_to_screens_through_proven_wrappers() {
    // W11a-2: SimpleLogin's shape. `Navigation.setRouter(this.$router)`
    // proves the module's `router`; `navigateTo(path)` pushes its parameter.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "popup/Navigation.js",
        "let router = null;\nlet other = null;\n\
         const PATH = { ROOT: '/', MAIN: '/main', RESULT: '/result', GONE: '/gone' };\n\
         class Navigation {\n  static PATH = PATH;\n\
         \x20 static getRoutes(c) { return [{ path: PATH.ROOT, component: c.Root }, \
         { path: PATH.MAIN, component: c.Main }, { path: PATH.RESULT, component: c.Main }]; }\n\
         \x20 static setRouter($router) { router = $router; }\n\
         \x20 static navigateTo(path, canGoBack) { if (canGoBack) { router.push(path); } else { router.replace(path); } }\n\
         \x20 static later(path) { setTimeout(() => router.push(path), 10); }\n\
         \x20 static shadow(path) { [1].map((path) => router.push(path)); }\n\
         \x20 static setOther($r) { other = $r; }\n\
         \x20 static viaOther(path) { other.push(path); }\n\
         }\nother = window.fake;\nexport default Navigation;\n",
    );
    for name in ["Root", "Main"] {
        write(
            &format!("popup/{name}.vue"),
            "<template><div/></template>\n<script>\nexport default {}\n</script>\n",
        );
    }
    write(
        "popup/App.vue",
        "<template><router-view/></template>\n<script>\n\
         import VueRouter from 'vue-router';\nimport Navigation from './Navigation';\n\
         import Root from './Root.vue';\nimport Main from './Main.vue';\n\
         const router = new VueRouter({ routes: Navigation.getRoutes({ Root, Main }) });\n\
         export default {\n  router,\n  mounted() {\n    Navigation.setRouter(this.$router);\n\
         \x20   Navigation.setOther(this.$router);\n    Navigation.navigateTo(Navigation.PATH.ROOT);\n  },\n};\n</script>\n",
    );
    write(
        "popup/Flow.vue",
        "<template><div/></template>\n<script>\nimport Navigation from './Navigation';\n\
         export default {\n  methods: {\n\
         \x20   async save(res) {\n      if (res.status === 201) { Navigation.navigateTo(Navigation.PATH.RESULT); }\n      Navigation.later('/main');\n      if (res.retry) { Navigation.navigateTo(Navigation.PATH.MAIN, true); }\n    },\n\
         \x20   go(where) { Navigation.navigateTo(where); Navigation.navigateTo(Navigation.PATH.GONE); },\n\
         \x20   skipped() { Navigation.shadow('/main'); Navigation.viaOther('/main'); },\n\
         \x20   direct() { this.$router.push('/main'); },\n  },\n};\n</script>\n",
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let sym = |file: &str, name: &str| {
        out.nodes
            .iter()
            .find(|n| {
                n.label == "Symbol"
                    && n.id.starts_with(&format!("sym:qwtm/example@{file}#"))
                    && n.id.contains(name)
            })
            .unwrap_or_else(|| panic!("{file} {name}"))
            .id
            .clone()
    };
    let navigations: BTreeSet<(String, String, bool)> = out
        .edges
        .iter()
        .filter(|e| e.label == "NAVIGATES_TO" && e.dst.starts_with("screen:"))
        .map(|e| {
            assert_eq!(e.props["prov"]["confidence_tier"], "InferredStrong");
            (e.src.clone(), e.dst.clone(), e.props["conditional"] == true)
        })
        .collect();
    let screen = |route: &str| format!("screen:qwtm/example@{route}");
    let save = sym("popup/Flow.vue", ".save@");
    assert_eq!(
        navigations,
        BTreeSet::from([
            (sym("popup/App.vue", ".mounted@"), screen("/"), false),
            (save.clone(), screen("/result"), true),
            // A parameter captured by a nested callback still flows.
            (save, screen("/main"), false),
            (sym("popup/Flow.vue", ".direct@"), screen("/main"), false),
        ])
    );
    // Each call is kept on the one edge with its own flag and route.
    let main = out
        .edges
        .iter()
        .find(|e| e.label == "NAVIGATES_TO" && e.src.contains(".save@") && e.dst == screen("/main"))
        .unwrap();
    let sites = main.props["sites"].as_array().unwrap();
    let flags: Vec<_> = sites
        .iter()
        .map(|site| (site["conditional"] == true, site["via"].as_str().unwrap()))
        .collect();
    assert_eq!(flags, [(false, "wrapper"), (true, "wrapper")]);
    // A shadowed parameter, and a router variable assigned outside its
    // setter, prove no wrapper; `skipped` navigates nowhere.
    let skipped = sym("popup/Flow.vue", ".skipped@");
    assert!(
        !out.edges
            .iter()
            .any(|e| e.label == "NAVIGATES_TO" && e.src == skipped)
    );
    // A computed target and a constant naming no screen are Gaps.
    let go = sym("popup/Flow.vue", ".go@");
    let reasons: BTreeSet<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "NAVIGATES_TO" && e.src == go)
        .map(|e| {
            out.nodes.iter().find(|n| n.id == e.dst).unwrap().props["reason"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(
        reasons,
        BTreeSet::from([
            client_navigation::COMPUTED_TARGET_REASON,
            client_navigation::UNKNOWN_SCREEN_REASON
        ])
    );
}

#[test]
fn vue_navigations_record_a_branch_on_a_bound_call_result() {
    // AC-1309 (T-1309): SimpleLogin's `const res = await callAPI(…)` then
    // `if (res.status === 201) navigateTo(RESULT)`, inside `try`, records the
    // call the branch tests. A `let`, a branch on another name, and a branch
    // inside a callback record nothing.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "popup/Navigation.js",
        "let router = null;\nconst PATH = { ROOT: '/', RESULT: '/result' };\n\
         class Navigation {\n  static PATH = PATH;\n\
         \x20 static getRoutes(c) { return [{ path: PATH.ROOT, component: c.Root }, \
         { path: PATH.RESULT, component: c.Root }]; }\n\
         \x20 static setRouter($router) { router = $router; }\n\
         \x20 static navigateTo(path) { router.push(path); }\n\
         }\nexport default Navigation;\n",
    );
    write(
        "popup/Root.vue",
        "<template><div/></template>\n<script>\nexport default {}\n</script>\n",
    );
    write(
        "popup/App.vue",
        "<template><router-view/></template>\n<script>\n\
         import VueRouter from 'vue-router';\nimport Navigation from './Navigation';\n\
         import Root from './Root.vue';\n\
         const router = new VueRouter({ routes: Navigation.getRoutes({ Root }) });\n\
         export default {\n  router,\n  mounted() {\n    Navigation.setRouter(this.$router);\n  },\n};\n</script>\n",
    );
    let flow = "<template><div/></template>\n<script>\nimport Navigation from './Navigation';\n\
         export default {\n  methods: {\n\
         \x20   async proven() {\n      try {\n        const res = await callAPI(1);\n        \
         if (res.status === 201) { Navigation.navigateTo(Navigation.PATH.RESULT); }\n      } catch (e) {}\n    },\n\
         \x20   async reassigned() {\n      let res = await callAPI(2);\n      \
         if (res.ok) { Navigation.navigateTo(Navigation.PATH.RESULT); }\n    },\n\
         \x20   async other(flag) {\n      const res = await callAPI(3);\n      \
         if (flag) { Navigation.navigateTo(Navigation.PATH.RESULT); }\n    },\n\
         \x20   async later() {\n      const res = await callAPI(4);\n      \
         setTimeout(() => { if (res.ok) { Navigation.navigateTo(Navigation.PATH.RESULT); } }, 1);\n    },\n\
         \x20   async both() {\n      const first = await callAPI(5);\n      const second = await callAPI(6);\n      \
         if (first.ok && second.ok) { Navigation.navigateTo(Navigation.PATH.RESULT); }\n    },\n\
         \x20   async caught() {\n      const res = await callAPI(7);\n      \
         try { run(); } catch (res) { if (res.ok) { Navigation.navigateTo(Navigation.PATH.RESULT); } }\n    },\n\
         \x20   async looped(values) {\n      const res = await callAPI(8);\n      \
         for (const res of values) { if (res.ok) { Navigation.navigateTo(Navigation.PATH.RESULT); } }\n    },\n\
         \x20   async destructured(values) {\n      const res = await callAPI(9);\n      \
         for (const { res } of values) { if (res) { Navigation.navigateTo(Navigation.PATH.RESULT); } }\n    },\n\
         \x20 },\n};\n</script>\n";
    write("popup/Flow.vue", flow);

    let out = extract_dir(dir.path(), &id()).unwrap();
    // `source` is part of the navigating Symbol's id; the callback in
    // `later` is its own `anon` Symbol.
    let result_of = |source: &str| {
        let edge = out
            .edges
            .iter()
            .find(|e| {
                e.label == "NAVIGATES_TO"
                    && e.src.contains(source)
                    && e.dst == "screen:qwtm/example@/result"
            })
            .unwrap_or_else(|| panic!("{source}"));
        let sites = edge.props["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0]["conditional"], true);
        sites[0].get("result_of").cloned()
    };
    let start = flow.find("callAPI(1)").unwrap() as u64;
    assert_eq!(
        result_of(".proven@"),
        Some(serde_json::json!([
            {"byte_start": start, "byte_end": start + "callAPI(1)".len() as u64}
        ]))
    );
    assert_eq!(result_of(".reassigned@"), None);
    assert_eq!(result_of(".other@"), None);
    assert_eq!(result_of("#anon@"), None);
    // Every bound call the condition reads is listed, in source order.
    let range = |call: &str| {
        let start = flow.find(call).unwrap() as u64;
        serde_json::json!({"byte_start": start, "byte_end": start + call.len() as u64})
    };
    assert_eq!(
        result_of(".both@"),
        Some(serde_json::json!([
            range("callAPI(5)"),
            range("callAPI(6)")
        ]))
    );
    // A `catch` parameter or loop binding shadows the outer `const`.
    assert_eq!(result_of(".caught@"), None);
    assert_eq!(result_of(".looped@"), None);
    assert_eq!(result_of(".destructured@"), None);
}

#[test]
fn vue_router_tables_follow_export_clauses_default_children_and_bindings() {
    // AC-0349 (#990 review): a table exported through `export { a as b }`
    // resolves, a default child (`path: ''`) is its parent's URL, and a
    // parameter named like a module table is a computed path, not the table.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let sfc = "<template><div/></template>\n<script>\nexport default {}\n</script>\n";
    write("src/Settings.vue", sfc);
    write("src/Overview.vue", sfc);
    write(
        "src/routes.js",
        "const PATH = { SETTINGS: '/settings', HOME: '/home' };\n\
         function buildRoutes(c) {\n  return [\n\
         \x20   { path: PATH.SETTINGS, component: c.Settings, children: [{ path: '', component: c.Overview }] },\n\
         \x20 ];\n}\n\
         function shadowRoutes(PATH, c) {\n  return [{ path: PATH.HOME, component: c.Overview }];\n}\n\
         export { buildRoutes as getRoutes, shadowRoutes };\n",
    );
    write(
        "src/router.js",
        "import VueRouter from 'vue-router';\nimport { getRoutes, shadowRoutes } from './routes';\n\
         import Settings from './Settings.vue';\nimport Overview from './Overview.vue';\n\
         new VueRouter({ routes: getRoutes({ Settings, Overview }) });\n\
         new VueRouter({ routes: shadowRoutes({ HOME: '/elsewhere' }, { Settings, Overview }) });\n",
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let screens: BTreeSet<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(screens, BTreeSet::from(["screen:qwtm/example@/settings"]));
    let renders: BTreeSet<_> = edge_pairs(&out, "RENDERS")
        .into_iter()
        .filter(|(src, _)| src.starts_with("screen:"))
        .collect();
    assert_eq!(
        renders,
        BTreeSet::from([
            (
                "screen:qwtm/example@/settings",
                "file:qwtm/example@src/Settings.vue"
            ),
            (
                "screen:qwtm/example@/settings",
                "file:qwtm/example@src/Overview.vue"
            ),
        ])
    );
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "Gap" && n.props["reason"] == client_routes::COMPUTED_PATH_REASON)
    );
}

#[test]
fn vue_navigations_refuse_locally_shadowed_bindings() {
    // AC-0350 (#997 review): a local `router`, a module router with its own
    // initializer, and a parameter named like a constant table prove nothing.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "popup/Navigation.js",
        "let router = null;\nlet preset = window.fake;\n\
         const PATH = { ROOT: '/', MAIN: '/main' };\n\
         class Navigation {\n\
         \x20 static getRoutes(c) { return [{ path: PATH.ROOT, component: c.Root }, \
         { path: PATH.MAIN, component: c.Main }]; }\n\
         \x20 static setRouter($router) { router = $router; }\n\
         \x20 static setPreset($r) { preset = $r; }\n\
         \x20 static go(path) { router.push(path); }\n\
         \x20 static local(path) { const router = window.other; router.push(path); }\n\
         \x20 static viaPreset(path) { preset.push(path); }\n\
         }\nexport default Navigation;\n",
    );
    for name in ["Root", "Main"] {
        write(
            &format!("popup/{name}.vue"),
            "<template><div/></template>\n<script>\nexport default {}\n</script>\n",
        );
    }
    write(
        "popup/App.vue",
        "<template><router-view/></template>\n<script>\n\
         import VueRouter from 'vue-router';\nimport Navigation from './Navigation';\n\
         import Root from './Root.vue';\nimport Main from './Main.vue';\n\
         const router = new VueRouter({ routes: Navigation.getRoutes({ Root, Main }) });\n\
         export default {\n  router,\n  mounted() {\n    Navigation.setRouter(this.$router);\n\
         \x20   Navigation.setPreset(this.$router);\n  },\n};\n</script>\n",
    );
    write(
        "popup/Flow.vue",
        "<template><div/></template>\n<script>\nimport Navigation from './Navigation';\n\
         const PATH = { MAIN: '/main' };\n\
         export default {\n  methods: {\n\
         \x20   ok() { Navigation.go('/main'); },\n\
         \x20   tableOk() { this.$router.push(PATH.MAIN); },\n\
         \x20   tableParam(PATH) { this.$router.push(PATH.MAIN); },\n\
         \x20   shadowLocal() { Navigation.local('/main'); },\n\
         \x20   preset() { Navigation.viaPreset('/main'); },\n  },\n};\n</script>\n",
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let sym = |name: &str| {
        out.nodes
            .iter()
            .find(|n| {
                n.label == "Symbol"
                    && n.id.starts_with("sym:qwtm/example@popup/Flow.vue#")
                    && n.id.contains(name)
            })
            .unwrap_or_else(|| panic!("{name}"))
            .id
            .clone()
    };
    let navigations: BTreeSet<(String, String)> = out
        .edges
        .iter()
        .filter(|e| e.label == "NAVIGATES_TO")
        .map(|e| {
            let dst = match out.nodes.iter().find(|n| n.id == e.dst) {
                Some(n) if n.label == "Gap" => n.props["reason"].as_str().unwrap().to_string(),
                _ => e.dst.clone(),
            };
            (e.src.clone(), dst)
        })
        .collect();
    let main = "screen:qwtm/example@/main".to_string();
    assert_eq!(
        navigations,
        BTreeSet::from([
            (sym(".ok@"), main.clone()),
            (sym(".tableOk@"), main),
            (
                sym(".tableParam@"),
                client_navigation::COMPUTED_TARGET_REASON.to_string()
            ),
        ])
    );
}

// --- Raw Node http.createServer routes (#1077) --------------------------------

const NODE_HTTP_DAEMON: &str = r#"
import { createServer } from 'node:http';
createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/ui' || url.pathname.startsWith('/ui/')) { return; }
  if (req.method === 'POST' && url.pathname === '/v0/vouch') { return; }
  if (req.method === 'POST' && /^\/v0\/soul\/revisions\/(approve|reject|adopt|edit)$/.exec(url.pathname)) { return; }
  if (['GET /v0/binding', 'POST /v0/credential'].includes(`${req.method} ${url.pathname}`)) { return; }
  if (url.pathname === '/health') { return; }
  const name = ownerRouteName(req.method, url.pathname);
});
"#;

// AC-0363 (T-0363): raw node:http `createServer` routes. Every
// `node_http_*` test below cites this acceptance criterion.
fn node_http_endpoints(ex: &Extraction) -> Vec<(String, String)> {
    ex.nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && n.props["framework"] == "node-http")
        .map(|n| {
            (
                n.props["method"].as_str().unwrap().to_string(),
                n.props["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect()
}

#[test]
fn node_http_recovers_every_predicate_form_in_source_order() {
    let ex = extract_source(NODE_HTTP_DAEMON.as_bytes(), "agent-daemon.mjs", &id()).unwrap();
    assert_eq!(
        node_http_endpoints(&ex),
        pairs(&[
            ("ALL", "/ui"),
            ("ALL", "/ui/*"),
            ("POST", "/v0/vouch"),
            ("POST", "/v0/soul/revisions/approve"),
            ("POST", "/v0/soul/revisions/reject"),
            ("POST", "/v0/soul/revisions/adopt"),
            ("POST", "/v0/soul/revisions/edit"),
            ("GET", "/v0/binding"),
            ("POST", "/v0/credential"),
            ("ALL", "/health"),
        ])
    );
}

#[test]
fn node_http_endpoint_shape_mirrors_express() {
    let ex = extract_source(NODE_HTTP_DAEMON.as_bytes(), "agent-daemon.mjs", &id()).unwrap();
    let vouch = ex
        .nodes
        .iter()
        .find(|n| n.id == "ep:qwtm/example@POST:/v0/vouch")
        .expect("vouch endpoint");
    assert_eq!(vouch.label, "Endpoint");
    assert_eq!(vouch.props["framework"], "node-http");
    // The citation spans the method guard and the path comparison.
    let source = NODE_HTTP_DAEMON;
    let ev = &vouch.props["prov"]["evidence"][0];
    let (start, end) = (
        ev["byte_start"].as_u64().unwrap() as usize,
        ev["byte_end"].as_u64().unwrap() as usize,
    );
    assert_eq!(&source[start..end], "req.method === 'POST'");
    let prefix = ex
        .nodes
        .iter()
        .find(|n| n.id == "ep:qwtm/example@ALL:/ui/*")
        .unwrap();
    assert_eq!(prefix.props["match"], "prefix");
    assert_eq!(prefix.props["prefix_kind"], "raw");
    let handles = edge_pairs(&ex, "HANDLES");
    assert!(
        handles
            .iter()
            .any(|(src, dst)| *src == "ep:qwtm/example@POST:/v0/vouch"
                && dst.starts_with("sym:qwtm/example@agent-daemon.mjs#anon@"))
    );
}

#[test]
fn node_http_computed_route_is_a_cited_gap() {
    let ex = extract_source(NODE_HTTP_DAEMON.as_bytes(), "agent-daemon.mjs", &id()).unwrap();
    let gaps: Vec<_> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.props["kind"] == "node-http-route-computed")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].props["reason"], "computed node:http route");
    let ev = &gaps[0].props["prov"]["evidence"][0];
    let (start, end) = (
        ev["byte_start"].as_u64().unwrap() as usize,
        ev["byte_end"].as_u64().unwrap() as usize,
    );
    assert_eq!(
        &NODE_HTTP_DAEMON[start..end],
        "ownerRouteName(req.method, url.pathname)"
    );
    assert!(
        edge_pairs(&ex, "DEPENDS_ON")
            .iter()
            .any(|(_, dst)| *dst == gaps[0].id)
    );
}

#[test]
fn node_http_other_regex_is_a_gap_not_endpoints() {
    let src = r#"
import http from 'http';
http.createServer(function (req, res) {
  if (/^\/v0\/items\/\d+$/.test(req.url)) { return; }
});
"#;
    let ex = extract_source(src.as_bytes(), "s.js", &id()).unwrap();
    assert!(node_http_endpoints(&ex).is_empty());
    let gap = ex
        .nodes
        .iter()
        .find(|n| n.props["kind"] == "node-http-route-computed")
        .expect("gap");
    assert_eq!(
        gap.props["reason"],
        "node:http route regex is not a literal alternation"
    );
}

#[test]
fn node_http_req_url_and_same_file_handler_and_require() {
    let src = r#"
const { createServer } = require('node:https');
function handle(req, res) {
  if (req.url === '/ping') { res.end(); }
}
createServer({}, handle);
"#;
    let ex = extract_source(src.as_bytes(), "s.cjs", &id()).unwrap();
    assert_eq!(node_http_endpoints(&ex), pairs(&[("ALL", "/ping")]));
    assert!(
        edge_pairs(&ex, "HANDLES")
            .contains(&("ep:qwtm/example@ALL:/ping", "sym:qwtm/example@s.cjs#handle"))
    );
    let ns = r#"
const http = require('http');
http.createServer((req, res) => { req.url === '/a' && res.end(); });
"#;
    let ex = extract_source(ns.as_bytes(), "n.js", &id()).unwrap();
    assert_eq!(node_http_endpoints(&ex), pairs(&[("ALL", "/a")]));
}

#[test]
fn node_http_namespace_import_and_ternary_in_ts() {
    let src = r#"
import * as http from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
http.createServer((req: IncomingMessage, res: ServerResponse) => {
  const { pathname } = new URL(req.url!, "http://x");
  const p = new URL(req.url!, "http://x").pathname;
  const kind = p === "/a" ? 1 : 2;
  if (req.method === "GET" && p.startsWith("/files/")) {}
});
"#;
    let ex = extract_source(src.as_bytes(), "s.ts", &id()).unwrap();
    assert_eq!(
        node_http_endpoints(&ex),
        pairs(&[("ALL", "/a"), ("GET", "/files/*")])
    );
}

#[test]
fn node_http_unproven_create_server_yields_nothing() {
    let local = r#"
function createServer(h) { return h; }
createServer((req, res) => { if (req.url === '/x') {} });
"#;
    let ex = extract_source(local.as_bytes(), "a.mjs", &id()).unwrap();
    assert!(node_http_endpoints(&ex).is_empty());
    let other = r#"
import { createServer } from 'some-lib';
createServer((req, res) => { if (req.url === '/x') {} });
"#;
    let ex = extract_source(other.as_bytes(), "b.mjs", &id()).unwrap();
    assert!(node_http_endpoints(&ex).is_empty());
    let shadowed = r#"
import { createServer } from 'node:http';
function f(createServer) { createServer((req, res) => { if (req.url === '/x') {} }); }
"#;
    let ex = extract_source(shadowed.as_bytes(), "c.mjs", &id()).unwrap();
    assert!(node_http_endpoints(&ex).is_empty());
}

#[test]
fn node_http_extraction_is_deterministic() {
    let a = extract_source(NODE_HTTP_DAEMON.as_bytes(), "agent-daemon.mjs", &id()).unwrap();
    let b = extract_source(NODE_HTTP_DAEMON.as_bytes(), "agent-daemon.mjs", &id()).unwrap();
    let ids = |ex: &Extraction| -> Vec<String> { ex.nodes.iter().map(|n| n.id.clone()).collect() };
    assert_eq!(ids(&a), ids(&b));
    assert_eq!(
        serde_json::to_string(&a.nodes.iter().map(|n| &n.props).collect::<Vec<_>>()).unwrap(),
        serde_json::to_string(&b.nodes.iter().map(|n| &n.props).collect::<Vec<_>>()).unwrap()
    );
}

#[test]
fn node_http_enclosing_method_guard_scopes_inner_routes() {
    let src = r#"
import http from 'node:http';
http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  if (req.method === 'POST') {
    if (url.pathname === '/a') {}
    if (req.method === 'GET' && url.pathname === '/never') {}
  }
  if (url.pathname === '/b') {}
});
"#;
    let ex = extract_source(src.as_bytes(), "s.mjs", &id()).unwrap();
    assert_eq!(
        node_http_endpoints(&ex),
        vec![
            ("POST".to_string(), "/a".to_string()),
            ("ALL".to_string(), "/b".to_string()),
        ]
    );
}

#[test]
fn node_http_request_members_must_be_the_handler_parameter() {
    // AC-0363: a nested parameter or a shadowing binding named `req` is not
    // the server request.
    let src = r#"
import { createServer } from 'node:http';
createServer((req, res) => {
  function helper(req) { if (req.url === '/internal') {} }
  { const req = { url: '/fake' }; if (req.url === '/fake') {} }
  [1].forEach(() => { if (req.url === '/captured') {} });
  if (req.url === '/real') {}
});
"#;
    let ex = extract_source(src.as_bytes(), "s.mjs", &id()).unwrap();
    assert_eq!(
        node_http_endpoints(&ex),
        vec![
            ("ALL".to_string(), "/captured".to_string()),
            ("ALL".to_string(), "/real".to_string()),
        ]
    );
}

#[test]
fn node_http_else_of_a_method_guard_declines_inner_claims() {
    // AC-0363: an `else` (or negated) method constraint cannot be stated as
    // one method, so routes under it are not claimed.
    let src = r#"
import { createServer } from "node:http";
createServer((req, res) => {
  if (req.method === "POST") {} else { if (req.method === "POST" && req.url === "/never") {} }
  if (req.method !== "GET") { if (req.url === "/not-get") {} }
});
"#;
    let ex = extract_source(src.as_bytes(), "s.mjs", &id()).unwrap();
    assert!(
        node_http_endpoints(&ex).is_empty(),
        "{:?}",
        node_http_endpoints(&ex)
    );
}

#[test]
fn node_http_negative_method_in_the_route_conjunction_declines_the_claim() {
    // AC-0363: a method constraint one method cannot state, written in the
    // route's own condition (`!==`, a negation, a computed read), declines
    // the route instead of widening it to ALL.
    let src = r#"
import { createServer } from "node:http";
createServer((req, res) => {
  if (req.method !== 'GET' && req.url === '/not-get') {}
  if (!(req.method === 'GET') && req.url === '/negated') {}
  if (req.method.toUpperCase() === 'GET' && req.url === '/computed') {}
  if (req.method === 'POST' && req.url === '/post') {}
  if (req.method !== 'GET' || req.url === '/either') {}
  if (req.method === 'GET' && req.method.length > 9) { if (req.url === '/narrowed') {} }
});
"#;
    let ex = extract_source(src.as_bytes(), "s.mjs", &id()).unwrap();
    let mut endpoints = node_http_endpoints(&ex);
    endpoints.sort();
    assert_eq!(
        endpoints,
        [
            ("ALL".to_string(), "/either".to_string()),
            ("POST".to_string(), "/post".to_string()),
        ]
    );
}

#[test]
fn node_http_shared_endpoint_keeps_every_handler() {
    // AC-0363: two servers in one file serving the same route share the
    // endpoint node and each keep a HANDLES edge.
    let src = r#"
import http from 'node:http';
http.createServer((req, res) => { if (req.url === '/health') {} });
http.createServer((req, res) => { if (req.url === '/health') {} });
"#;
    let ex = extract_source(src.as_bytes(), "s.mjs", &id()).unwrap();
    assert_eq!(node_http_endpoints(&ex).len(), 1);
    let handles = ex
        .edges
        .iter()
        .filter(|e| e.label == "HANDLES" && e.src.ends_with("ALL:/health"))
        .count();
    assert_eq!(handles, 2);
}

const SERVICE_WORKER_JS: &str = r#"
const CACHE = 'shell-v1';
self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(CACHE));
});
self.addEventListener('fetch', (event) => {
  event.respondWith(
    fetch(event.request).then((response) => {
      return response;
    }),
  );
});
function helper() {}
"#;

/// Runtime prop of every File, Symbol and Component node, by id.
fn runtimes(out: &Extraction) -> BTreeMap<String, Option<String>> {
    out.nodes
        .iter()
        .filter(|node| matches!(node.label.as_str(), "File" | "Symbol" | "Component"))
        .map(|node| {
            (
                node.id.clone(),
                node.props["runtime"].as_str().map(str::to_string),
            )
        })
        .collect()
}

// AC-0365: `self.addEventListener('install' | 'fetch', …)` on the unshadowed
// global proves a service worker; the File and every Symbol in it carry
// `runtime: "service-worker"`.
#[test]
fn a_self_lifecycle_listener_marks_the_file_and_its_symbols_as_a_service_worker() {
    let out = extract_tree(&[("web/sw.js", SERVICE_WORKER_JS)]);
    let runtimes = runtimes(&out);
    assert!(runtimes.len() >= 4, "{runtimes:?}");
    assert!(
        runtimes
            .values()
            .all(|runtime| runtime.as_deref() == Some("service-worker")),
        "{runtimes:?}"
    );
}

// AC-0365: a file with no worker proof, and a `self` the file rebinds, stay
// unmarked, as does a non-lifecycle event on the global.
#[test]
fn non_worker_and_shadowed_self_files_stay_unmarked() {
    let out = extract_tree(&[
        (
            "app.js",
            "function run() { window.addEventListener('install', () => {}); }",
        ),
        (
            "shadow.js",
            "function run(self) { self.addEventListener('install', () => {}); }\n\
             function other() { const self = makeBus(); self.addEventListener('fetch', () => {}); }",
        ),
        ("message.js", "self.addEventListener('message', () => {});"),
    ]);
    let runtimes = runtimes(&out);
    assert!(!runtimes.is_empty());
    assert!(runtimes.values().all(Option::is_none), "{runtimes:?}");
}

// AC-0365: a file whose path is the literal argument of
// `navigator.serviceWorker.register` is a worker even without a lifecycle
// listener; an ambiguous or unresolvable literal marks nothing.
#[test]
fn a_registered_path_marks_exactly_one_resolved_file() {
    let out = extract_tree(&[
        (
            "web/app.js",
            "navigator.serviceWorker.register('/worker.js');",
        ),
        ("web/worker.js", "function boot() {}"),
        (
            "web/other.js",
            "navigator.serviceWorker.register('/dup.js');\nnavigator.serviceWorker.register('../up.js');",
        ),
        ("a/dup.js", "function a() {}"),
        ("b/dup.js", "function b() {}"),
        ("up.js", "function up() {}"),
    ]);
    let runtimes = runtimes(&out);
    for (id, runtime) in &runtimes {
        let worker = id.contains("worker.js");
        assert_eq!(runtime.is_some(), worker, "{id}: {runtime:?}");
    }
    assert!(runtimes.contains_key("sym:qwtm/example@web/worker.js#boot"));
}

// AC-0365 (#1085 review): a relative or protocol-relative literal resolves
// against a base URL the extractor does not know, so it proves nothing.
#[test]
fn relative_registration_literals_prove_nothing() {
    let out = extract_tree(&[
        (
            "web/app.js",
            "navigator.serviceWorker.register('sw.js');\nnavigator.serviceWorker.register('./push.js');\nnavigator.serviceWorker.register('//cdn/x.js');",
        ),
        ("web/sw.js", "function a() {}"),
        ("web/push.js", "function b() {}"),
        ("cdn/x.js", "function c() {}"),
    ]);
    assert!(runtimes(&out).values().all(Option::is_none));
}

// AC-0365 (#1085 review): the registration call that proves a worker is
// cited by every node it marks, and the content hash covers it.
#[test]
fn registration_evidence_is_cited_by_marked_nodes() {
    let tree = [
        (
            "web/app.js",
            "navigator.serviceWorker.register('/worker.js');",
        ),
        ("web/worker.js", "function boot() {}"),
    ];
    let marked = extract_tree(&tree);
    let unmarked = extract_tree(&[tree[1]]);
    for id in [
        "file:qwtm/example@web/worker.js",
        "sym:qwtm/example@web/worker.js#boot",
    ] {
        let node = |out: &Extraction| out.nodes.iter().find(|n| n.id == id).unwrap().props.clone();
        let (with, without) = (node(&marked), node(&unmarked));
        let evidence = with["prov"]["evidence"].as_array().unwrap();
        assert!(
            evidence.iter().any(|e| e["path"] == "web/app.js"),
            "{id}: {evidence:?}"
        );
        assert_ne!(
            with["prov"]["content_hash"],
            without["prov"]["content_hash"]
        );
    }
}

// AC-0365: a shadowed `navigator` registers nothing.
#[test]
fn a_shadowed_navigator_register_proves_nothing() {
    let out = extract_tree(&[
        (
            "web/app.js",
            "function f(navigator) { navigator.serviceWorker.register('/worker.js'); }",
        ),
        ("web/worker.js", "function boot() {}"),
    ]);
    assert!(runtimes(&out).values().all(Option::is_none));
}

// AC-0365: anonymous callbacks keep their `anon@<byte>` ids and gain a
// deterministic `display_name` from the call they are passed to.
#[test]
fn anonymous_callbacks_get_display_names_without_changing_ids() {
    let out = client_extract("web/sw.js", SERVICE_WORKER_JS);
    let named: BTreeMap<String, Option<String>> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| {
            (
                node.props["name"].as_str().unwrap().to_string(),
                node.props["display_name"].as_str().map(str::to_string),
            )
        })
        .collect();
    let by_display: BTreeSet<_> = named
        .iter()
        .filter(|(name, _)| name.starts_with("anon@"))
        .map(|(_, display)| display.clone())
        .collect();
    assert_eq!(
        by_display,
        BTreeSet::from([
            Some("sw.js · install listener".to_string()),
            Some("sw.js · fetch listener".to_string()),
            Some("sw.js · fetch().then callback".to_string()),
        ])
    );
    assert_eq!(named["helper"], None);
    for node in out.nodes.iter().filter(|node| node.label == "Symbol") {
        let name = node.props["name"].as_str().unwrap();
        assert_eq!(node.id, format!("sym:qwtm/example@web/sw.js#{name}"));
    }
}

// AC-0365: an anonymous function that is not a method-call argument has no
// display name, and neither does a named one.
#[test]
fn display_names_are_absent_without_a_method_call_argument() {
    let out = client_extract(
        "app.js",
        "export default () => {};\nwrap(() => {});\nconst named = () => {};",
    );
    assert!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Symbol")
            .all(|node| node.props.get("display_name").is_none())
    );
}

#[test]
fn worker_proofs_reused_at_a_new_commit_match_a_fresh_extraction() {
    // AC-0365 (#1078): a cache filled at commit A and reused at commit B
    // cites B in registration and self-listener proofs, and hashes them as
    // a fresh extraction at B does.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("web")).unwrap();
    for (path, text) in [
        (
            "web/app.js",
            "navigator.serviceWorker.register('/worker.js');\n",
        ),
        ("web/worker.js", "function boot() {}\n"),
        (
            "web/sw.js",
            "self.addEventListener('install', function onInstall() {});\n",
        ),
    ] {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    let at = |commit| SourceId {
        repo: "qwtm/example",
        commit,
    };
    let mut cache = IncrementalCache::default();
    extract_dir_incremental(dir.path(), &at("aaaa"), &mut cache).unwrap();
    let (reused, stats) = extract_dir_incremental(dir.path(), &at("bbbb"), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0, "the second run reuses the cache");
    let fresh = extract_dir(dir.path(), &at("bbbb")).unwrap();
    let facts = |out: &Extraction| {
        let mut facts: Vec<String> = out
            .nodes
            .iter()
            .map(|n| format!("{} {}", n.id, n.props))
            .chain(
                out.edges
                    .iter()
                    .map(|e| format!("{} {} {} {}", e.src, e.label, e.dst, e.props)),
            )
            .collect();
        facts.sort();
        facts
    };
    assert_eq!(facts(&reused), facts(&fresh));
    for id in [
        "file:qwtm/example@web/worker.js",
        "file:qwtm/example@web/sw.js",
    ] {
        let node = reused.nodes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(node.props["runtime"], "service-worker", "{id}");
        let evidence = node.props["prov"]["evidence"].as_array().unwrap();
        assert!(
            evidence.iter().all(|e| e["commit_sha"] == "bbbb"),
            "{id}: {evidence:?}"
        );
    }
}

// AC-0541: pending-call and Pulumi completion over the packed store leave
// exactly the facts they leave over plain lists: a call to a known symbol
// links, a guessed `.ts` target is reconciled, an instance call links only
// to a method marked for instance dispatch, an unknown target records its
// Gap, and only Pulumi relationships whose endpoints resolve are appended
// with their content hash recomputed. AC-0863: each call's candidates are
// held packed and only the selected outcome is decoded.
#[test]
fn call_and_pulumi_completion_is_identical_over_lists_and_store() {
    use core_graph::facts::{FactLists, FactStore};

    let node = |id: &str, props: serde_json::Value| Node {
        id: id.into(),
        label: "Symbol".into(),
        props,
    };
    let edge = |src: &str, dst: &str, label: &str| Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: serde_json::json!({}),
    };
    let call = |dst: &str, instance: bool, gap: &str| {
        let mut call = PendingCall::new(
            edge("sym:caller", dst, "CALLS"),
            Node {
                id: gap.into(),
                label: "Gap".into(),
                props: serde_json::json!({}),
            },
            edge("sym:caller", gap, "CALLS"),
        );
        call.requires_instance_method = instance;
        call
    };
    let nodes = vec![
        node("sym:caller", serde_json::json!({})),
        node("sym:r@a.tsx#f", serde_json::json!({})),
        node(
            "sym:r@a.ts#plain",
            serde_json::json!({"instance_dispatch": false}),
        ),
        node(
            "sym:r@a.ts#method",
            serde_json::json!({"instance_dispatch": true}),
        ),
    ];
    let known: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let calls = || {
        vec![
            call("sym:r@a.ts#f", false, "gap:1"),
            call("sym:r@a.ts#plain", true, "gap:2"),
            call("sym:r@a.ts#method", true, "gap:3"),
            call("sym:r@missing.ts#g", false, "gap:4"),
        ]
    };
    let prov = serde_json::to_value(
        core_prov::Provenance::new(
            core_prov::Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![],
            EXTRACTOR_ID,
            b"stale",
        )
        .unwrap(),
    )
    .unwrap();
    let pulumi = || {
        vec![
            PendingPulumiEdge {
                src: PulumiEndpoint::Resource("res:a".into()),
                dst: PulumiEndpoint::ImportedSymbol("bucket".into()),
                label: "DEPENDS_ON".into(),
                props: serde_json::json!({"prov": prov.clone(), "via": "bucket.arn"}),
            },
            PendingPulumiEdge {
                src: PulumiEndpoint::Resource("res:a".into()),
                dst: PulumiEndpoint::ImportedSymbol("unbound".into()),
                label: "DEPENDS_ON".into(),
                props: serde_json::json!({}),
            },
        ]
    };
    let bindings = HashMap::from([("bucket".to_string(), "res:bucket".to_string())]);
    let (mut listed, mut listed_edges) = (nodes.clone(), Vec::new());
    let mut lists = FactLists {
        nodes: &mut listed,
        edges: &mut listed_edges,
    };
    complete_pending_calls(
        calls(),
        &known,
        &crate::reexports::Index::default(),
        Vec::new(),
        &id(),
        &mut lists,
    )
    .unwrap();
    complete_pulumi_edges(pulumi(), &bindings, &mut lists).unwrap();
    let mut store = FactStore::from_facts(nodes.clone(), Vec::new()).unwrap();
    complete_pending_calls(
        calls(),
        &known,
        &crate::reexports::Index::default(),
        Vec::new(),
        &id(),
        &mut store,
    )
    .unwrap();
    complete_pulumi_edges(pulumi(), &bindings, &mut store).unwrap();
    assert_eq!(
        store.into_facts().unwrap(),
        (listed.clone(), listed_edges.clone())
    );
    let gaps: Vec<_> = listed[4..].iter().map(|n| n.id.as_str()).collect();
    assert_eq!(gaps, ["gap:2", "gap:4"]);
    let links: Vec<_> = listed_edges
        .iter()
        .map(|e| (e.dst.as_str(), e.label.as_str()))
        .collect();
    assert_eq!(
        links,
        [
            ("sym:r@a.tsx#f", "CALLS"),
            ("gap:2", "CALLS"),
            ("sym:r@a.ts#method", "CALLS"),
            ("gap:4", "CALLS"),
            ("res:bucket", "DEPENDS_ON"),
        ]
    );
    let hash = listed_edges[4].props["prov"]["content_hash"].clone();
    assert_eq!(
        hash,
        serde_json::json!(core_prov::content_hash(
            b"DEPENDS_ON res:a -> res:bucket via bucket.arn"
        ))
    );
}

// AC-0847: TypeORM entities are the data model: an import-proven `@Entity` class is a
// DataEntity with its own columns, and relation decorators are REFERENCES
// edges to the entity the relative import (or the same file) names.
#[test]
fn typeorm_entities_and_relations_are_recovered() {
    let out = extract_tree(&[
        (
            "src/entity/product.entity.ts",
            r#"
import { Column, Entity, JoinTable, ManyToMany, ManyToOne, OneToMany, PrimaryGeneratedColumn } from 'typeorm';
import { VendureEntity } from './base.entity';
import { Asset } from './asset.entity';
@Entity('product')
export class Product extends VendureEntity {
  @PrimaryGeneratedColumn() id: number;
  @Column({ nullable: true }) deletedAt: Date | null;
  @Column({ name: 'is_enabled', default: true }) enabled: boolean;
  @ManyToOne(type => Asset, { onDelete: 'SET NULL' }) featuredAsset: Asset;
  @ManyToMany(() => Asset) @JoinTable() assets: Asset[];
  @OneToMany(type => ProductVariant, variant => variant.product) variants: ProductVariant[];
  notPersisted: string;
}
@Entity()
export class ProductVariant {
  @Column() sku: string;
  @ManyToOne(type => Product, product => product.variants) product: Product;
}
"#,
        ),
        (
            "src/entity/asset.entity.ts",
            r#"
import { Column, Entity } from 'typeorm';
@Entity()
export class Asset { @Column() name: string; }
"#,
        ),
        (
            "src/fake.ts",
            r#"
function Entity() { return (_: unknown) => {}; }
@Entity()
export class NotAnEntity {}
"#,
        ),
    ]);
    let entity = |id: &str| {
        out.nodes
            .iter()
            .find(|node| node.label == "DataEntity" && node.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
    };
    let product = entity("data:qwtm/example@typeorm:src/entity/product.entity.ts#Product");
    entity("data:qwtm/example@typeorm:src/entity/product.entity.ts#ProductVariant");
    entity("data:qwtm/example@typeorm:src/entity/asset.entity.ts#Asset");
    assert_eq!(
        out.nodes.iter().filter(|n| n.label == "DataEntity").count(),
        3,
        "a lookalike @Entity proves nothing"
    );
    assert_eq!(product.props["table"], "product");
    assert_eq!(product.props["extends"], "VendureEntity");
    let columns = product.props["columns"].as_array().unwrap();
    assert_eq!(columns.len(), 3, "{columns:?}");
    assert_eq!(columns[0]["primary_key"], true);
    assert_eq!(columns[1]["name"], "deletedAt");
    assert_eq!(columns[1]["type"], "Date | null");
    assert_eq!(columns[1]["nullable"], true);
    assert_eq!(columns[2]["column"], "is_enabled");
    let refs = |src: &str, dst: &str| {
        out.edges
            .iter()
            .find(|e| e.label == "REFERENCES" && e.src.ends_with(src) && e.dst.ends_with(dst))
            .unwrap_or_else(|| panic!("missing {src} -> {dst}"))
    };
    // Two relations to Asset are one edge with both fields.
    let to_asset = refs("#Product", "asset.entity.ts#Asset");
    let fields = to_asset.props["relations"].as_array().unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0]["cardinality"], "many-to-one");
    assert_eq!(fields[1]["relation"], "ManyToMany");
    assert_eq!(fields[1]["owning"], true);
    let to_variant = refs("#Product", "#ProductVariant");
    assert_eq!(to_variant.props["relations"][0]["owning"], false);
    refs("#ProductVariant", "product.entity.ts#Product");
}

// AC-0847: an unproven relation target is a REFERENCES edge to an explicit
// Gap, a `@ChildEntity` argument is a discriminator and never a table, and
// `@Column({ primary: true })` is a primary key.
#[test]
fn typeorm_unresolved_targets_child_entities_and_primary_options() {
    let out = extract_tree(&[(
        "src/entity/photo.entity.ts",
        r#"
import { ChildEntity, Column, ManyToOne } from 'typeorm';
import { Album } from '@app/album';
@ChildEntity('photo')
export class Photo {
  @Column({ primary: true }) code: string;
  @ManyToOne(() => Album) album: Album;
  @ManyToOne(() => pick()) other: unknown;
}
"#,
    )]);
    let photo = out
        .nodes
        .iter()
        .find(|n| n.id == "data:qwtm/example@typeorm:src/entity/photo.entity.ts#Photo")
        .expect("child entity");
    assert!(photo.props.get("table").is_none(), "{}", photo.props);
    assert_eq!(photo.props["discriminator"], "photo");
    assert_eq!(photo.props["columns"][0]["primary_key"], true);
    let gaps: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.label == "REFERENCES" && e.src == photo.id)
        .map(|e| {
            let gap = out.nodes.iter().find(|n| n.id == e.dst).unwrap();
            assert_eq!(gap.label, "Gap");
            (
                e.props["field"].as_str().unwrap(),
                gap.props["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        gaps,
        [
            ("album", "TypeORM relation target is not a recovered entity"),
            ("other", "TypeORM relation target is not a literal class"),
        ]
    );
}

#[test]
fn node_test_runner_calls_are_tests() {
    // AC-0920 (#2008): `node:test` proves `test`/`it`/`describe`/`suite`,
    // named or as its default export; a lookalike module does not. The framework follows the binding that proved the call.
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "tests/hq.test.mjs",
        "import { test, describe } from 'node:test';\nimport assert from 'node:assert/strict';\ndescribe('hq', () => { test('admits a soul', () => { assert.ok(true); }); });\ntest.skip('later', () => {});\n",
    );
    write(
        "tests/default.test.mjs",
        "import t from 'node:test';\nimport { suite, it } from 'node:test';\nt('default export', () => {});\nsuite('group', () => { it('inside', () => {}); });\n",
    );
    write(
        "tests/fake.test.mjs",
        "import { test } from './not-node-test.mjs';\ntest('ignored', () => {});\n",
    );
    write(
        "tests/mixed.test.ts",
        "import { test } from 'vitest';\nimport { mock } from 'node:test';\ntest('vitest case', () => { mock.fn(); });\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut names: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "TestCase")
        .filter_map(|n| n.props["qualified_name"].as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "default export",
            "group > inside",
            "hq > admits a soul",
            "later",
            "vitest case"
        ],
        "{names:?}"
    );
    // The framework follows the binding that proved the call, not the
    // file's other imports.
    let framework = |name: &str| {
        out.nodes
            .iter()
            .find(|n| n.label == "TestCase" && n.props["qualified_name"] == name)
            .map(|n| n.props["framework"].as_str().unwrap().to_owned())
            .unwrap()
    };
    assert_eq!(framework("vitest case"), "vitest-jest");
    assert_eq!(framework("hq > admits a soul"), "node-test");
}

#[test]
fn typed_call_on_an_inheriting_class_reaches_the_nearest_base_method() {
    // AC-1514: `T.m` that `T` only inherits resolves through `extends`.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("base.ts"),
        r#"
export class Disposable { dispose() {} }
export class Store extends Disposable { get(key: string) { return key; } }
export class Loop extends Knot {}
class Knot extends Loop {}
export abstract class Shape { area() { return 0; } abstract label(): string; }
export class Square extends Shape { label() { return 'sq'; } }
export function measure(square: Square) { square.area(); }
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("derived.ts"),
        r#"
import { Store } from './base.js';
import { Remote } from 'remote-pkg';
export class ResourceStore extends Store {}
export class OwnStore extends Store { get(key: string) { return key + '!'; } }
export class Proxy extends Remote {}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("use.ts"),
        r#"
import { ResourceStore, OwnStore, Proxy } from './derived.js';
import { Loop } from './base';
export function read(store: ResourceStore) { store.get('a'); }
export function close(store: ResourceStore) { store.dispose(); }
export function own(store: OwnStore) { store.get('b'); }
export function missing(store: ResourceStore) { store.absent(); }
export function remote(proxy: Proxy) { proxy.send(); }
export function cycle(loop: Loop) { loop.spin(); }
"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let sym = |name: &str| format!("sym:qwtm/example@src/{name}");
    for (from, to) in [
        ("use.ts#read", "base.ts#Store.get"),
        ("use.ts#close", "base.ts#Disposable.dispose"),
        ("use.ts#own", "derived.ts#OwnStore.get"),
        ("base.ts#measure", "base.ts#Shape.area"),
    ] {
        assert!(
            calls.contains(&(sym(from).as_str(), sym(to).as_str())),
            "{from} -> {to} in {calls:?}"
        );
    }
    let close = ex
        .edges
        .iter()
        .find(|e| e.src == sym("use.ts#close") && e.label == "CALLS")
        .unwrap();
    assert_eq!(close.props["resolution"], "inherited-member");
    assert_eq!(
        close.props["inherits_via"],
        serde_json::json!([
            sym("derived.ts#ResourceStore"),
            sym("base.ts#Store"),
            sym("base.ts#Disposable")
        ])
    );
    assert_eq!(close.props["prov"]["confidence_tier"], "Confirmed");
    let cited: Vec<&str> = close.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(cited, ["src/use.ts", "src/derived.ts", "src/base.ts"]);
    let gaps: BTreeSet<&str> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .filter_map(|n| n.props["callee"].as_str())
        .collect();
    assert_eq!(
        gaps,
        BTreeSet::from(["Loop.spin", "Proxy.send", "ResourceStore.absent"])
    );
}
