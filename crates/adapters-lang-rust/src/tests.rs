use super::*;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/rust-ws",
        commit: "abc123",
    }
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "beta/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nalpha = { path = \"../alpha\" }\n",
    );
    write(
        root,
        "alpha/src/lib.rs",
        r#"
pub mod a;
use crate::a::helper;
use self::a::Widget;
use serde::Serialize;
pub fn root_fn() {}
pub fn root_call() { helper(); }
pub fn make() { Widget::new(); }
"#,
    );
    write(
        root,
        "alpha/src/a.rs",
        r#"
pub mod b;
use crate::a::b::deep;
use super::root_fn;
use alpha::a::b::deep as deep_alias;
use crate::a::{helper2, b::deep2};
use crate::a::b::*;
pub use std::collections::HashMap;
pub fn helper() {}
pub fn helper2() {}
pub struct Widget;
impl Widget {
    pub fn new() -> Self { Widget }
    pub fn run(&self) { self.step(); Self::new(); helper(); deep(); }
    pub fn step(&self) {}
}
pub trait Speak {
    fn speak(&self) { helper(); }
}
pub fn call_deep() { deep(); deep_alias(); helper2(); deep2(); b::deep(); crate::a::b::deep(); root_fn(); }
"#,
    );
    write(
        root,
        "alpha/src/a/b.rs",
        r#"
use super::helper;
use self::local;
use crate::root_fn;
pub fn deep() {}
pub fn deep2() {}
pub fn local() {}
pub fn go() { helper(); local(); root_fn(); }
pub struct Other;
impl Other {
    pub fn poke(&self) {}
}
pub trait Speaker {
    fn speak(&self);
}
pub fn via_other(o: &Other) { o.poke(); }
pub fn dispatch(s: &dyn Speaker) { s.speak(); }
pub fn with_closure() { let f = || 1; f(); }
pub fn mac() { println!("hi"); some_macro!(); }
"#,
    );
    write(
        root,
        "beta/src/lib.rs",
        r#"
use alpha::a::helper;
use alpha::a::Widget;
pub fn beta_fn() { helper(); Widget::new(); alpha::a::helper2(); }
"#,
    );
    dir
}

fn edge_pairs<'a>(x: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    x.edges
        .iter()
        .filter(|e| e.label == label)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect()
}

fn has_edge(x: &Extraction, label: &str, src: &str, dst: &str) -> bool {
    edge_pairs(x, label).contains(&(src, dst))
}

#[test]
fn symbols_and_defined_in() {
    let dir = fixture();
    let x = extract_dir(dir.path(), &id()).unwrap();
    for sym in [
        "sym:local/rust-ws@alpha/src/lib.rs#root_fn",
        "sym:local/rust-ws@alpha/src/a.rs#helper",
        "sym:local/rust-ws@alpha/src/a.rs#Widget",
        "sym:local/rust-ws@alpha/src/a.rs#Widget.run",
        "sym:local/rust-ws@alpha/src/a.rs#Speak",
        "sym:local/rust-ws@alpha/src/a.rs#Speak.speak",
        "sym:local/rust-ws@alpha/src/a/b.rs#deep",
        "sym:local/rust-ws@beta/src/lib.rs#beta_fn",
    ] {
        assert!(
            x.nodes.iter().any(|n| n.id == sym && n.label == "Symbol"),
            "missing symbol {sym}"
        );
        let file = sym.split('#').next().unwrap().replacen("sym:", "file:", 1);
        assert!(has_edge(&x, "DEFINED_IN", sym, &file), "DEFINED_IN {sym}");
    }
    // target/ noise and module files: lib.rs is a File node.
    assert!(
        x.nodes
            .iter()
            .any(|n| n.id == "file:local/rust-ws@alpha/src/a/b.rs" && n.label == "File")
    );
}

#[test]
fn every_import_form() {
    let dir = fixture();
    let x = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<&str> = x
        .edges
        .iter()
        .filter(|e| e.label == "IMPORTS")
        .map(|e| e.dst.as_str())
        .collect();
    for dst in [
        "mod:alpha::a::helper",
        "mod:alpha::a::Widget",
        "mod:alpha::root_fn",
        "mod:alpha::a::b::local",
        "mod:alpha::a::b::deep",
        "mod:alpha::a::helper2",
        "mod:alpha::a::b::deep2",
        "mod:alpha::a::b::*",
        "mod:serde::Serialize",
        "mod:std::collections::HashMap",
    ] {
        assert!(imports.contains(&dst), "missing import {dst}");
    }
    let glob = x
        .edges
        .iter()
        .find(|e| e.dst == "mod:alpha::a::b::*")
        .unwrap();
    assert_eq!(glob.props["glob"], true);
    // `pub use` is read through its argument, not its visibility.
    assert!(
        !imports.iter().any(|dst| dst.contains("pub")),
        "{imports:?}"
    );
    // External imports are Confirmed external placeholders, not Gaps.
    let ext = x
        .nodes
        .iter()
        .find(|n| n.id == "mod:serde::Serialize")
        .unwrap();
    assert_eq!(ext.props["boundary"], "external");
    assert_eq!(ext.props["prov"]["confidence_tier"], "Confirmed");
    let internal = x
        .nodes
        .iter()
        .find(|n| n.id == "mod:alpha::a::helper")
        .unwrap();
    assert_eq!(internal.props["boundary"], "internal");
}

#[test]
fn resolved_calls() {
    let dir = fixture();
    let x = extract_dir(dir.path(), &id()).unwrap();
    let a = "sym:local/rust-ws@alpha/src/a.rs";
    let b = "sym:local/rust-ws@alpha/src/a/b.rs";
    let lib = "sym:local/rust-ws@alpha/src/lib.rs";
    // same-module
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{a}#Widget.run"),
        &format!("{a}#helper")
    ));
    // imported via use
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{b}#go"),
        &format!("{a}#helper")
    ));
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{lib}#root_call"),
        &format!("{a}#helper")
    ));
    // path calls: b::deep(), crate::a::b::deep(), alpha::a::helper2()
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{a}#call_deep"),
        &format!("{b}#deep")
    ));
    assert!(has_edge(
        &x,
        "CALLS",
        "sym:local/rust-ws@beta/src/lib.rs#beta_fn",
        &format!("{a}#helper2")
    ));
    // Self:: and self.method() within impl
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{a}#Widget.run"),
        &format!("{a}#Widget.new")
    ));
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{a}#Widget.run"),
        &format!("{a}#Widget.step")
    ));
    // Type::assoc() through an imported type
    assert!(has_edge(
        &x,
        "CALLS",
        "sym:local/rust-ws@beta/src/lib.rs#beta_fn",
        &format!("{a}#Widget.new")
    ));
}

#[test]
fn only_unresolvable_workspace_paths_are_gaps() {
    let dir = fixture();
    write(dir.path(), "alpha/src/a/b/c.rs", "pub fn present() {}\n");
    write(
        dir.path(),
        "alpha/src/a/b.rs",
        &format!(
            "{}\npub mod c;\npub fn broken() {{ crate::a::missing(); c::absent(); c::present(); }}\n\
             pub fn quiet() {{ Some(1); Ok::<(), ()>(()); drop(1); String::new(); serde_json::json(); }}\n",
            std::fs::read_to_string(dir.path().join("alpha/src/a/b.rs")).unwrap()
        ),
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    let b = "sym:local/rust-ws@alpha/src/a/b.rs";
    let gaps: Vec<&str> = x
        .edges
        .iter()
        .filter(|e| e.label == "CALLS" && e.dst.starts_with("gap:"))
        .map(|e| e.src.as_str())
        .collect();
    // `crate::a::missing()` and `c::absent()` name workspace modules but
    // no symbol: two Gaps from `broken`, and nothing else in the fixture.
    assert_eq!(gaps, vec![format!("{b}#broken"); 2], "{gaps:?}");
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{b}#broken"),
        "sym:local/rust-ws@alpha/src/a/b/c.rs#present"
    ));
    // Receivers, trait objects, closures, prelude items and external
    // crates are left out rather than guessed or flagged.
    for quiet in ["via_other", "dispatch", "with_closure", "quiet"] {
        assert!(
            !x.edges
                .iter()
                .any(|e| e.label == "CALLS" && e.src == format!("{b}#{quiet}")),
            "{quiet}"
        );
    }
    for gap in x.nodes.iter().filter(|n| n.label == "Gap") {
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
        assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    }
}

#[test]
fn glob_imports_resolve_only_unambiguous_names() {
    let dir = fixture();
    write(
        dir.path(),
        "beta/src/lib.rs",
        "pub mod x;\npub mod y;\nuse crate::x::*;\nuse crate::y::*;\n\
         pub fn caller() { only_x(); both(); Ok::<(), ()>(()); }\n",
    );
    write(
        dir.path(),
        "beta/src/x.rs",
        "pub fn only_x() {}\npub fn both() {}\n",
    );
    write(dir.path(), "beta/src/y.rs", "pub fn both() {}\n");
    let x = extract_dir(dir.path(), &id()).unwrap();
    let caller = "sym:local/rust-ws@beta/src/lib.rs#caller";
    assert!(has_edge(
        &x,
        "CALLS",
        caller,
        "sym:local/rust-ws@beta/src/x.rs#only_x"
    ));
    let gaps: Vec<&Node> = x
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.props["callee"] == "both")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        gaps[0].props["reason"],
        "unresolved Rust call target: ambiguous glob imports"
    );
    assert!(
        !x.nodes
            .iter()
            .any(|n| n.label == "Gap" && n.props["callee"] == "Ok::<(), ()>")
    );
}

#[test]
fn macros_are_not_calls() {
    let dir = fixture();
    let x = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        !x.edges
            .iter()
            .any(|e| { e.label == "CALLS" && e.src == "sym:local/rust-ws@alpha/src/a/b.rs#mac" })
    );
    assert!(!x.nodes.iter().any(|n| {
        n.label == "Gap"
            && n.props["callee"]
                .as_str()
                .is_some_and(|c| c.contains("println") || c.contains("some_macro"))
    }));
}

#[test]
fn determinism_and_provenance() {
    let dir = fixture();
    let first = extract_dir(dir.path(), &id()).unwrap();
    let second = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(first.nodes, second.nodes);
    assert_eq!(first.edges, second.edges);
    for props in first
        .nodes
        .iter()
        .filter(|n| n.props.get("placeholder").is_none() && n.label != "Gap")
        .map(|n| &n.props)
    {
        assert_eq!(props["prov"]["tier"], "Deterministic");
        assert_eq!(props["prov"]["extractor_id"], EXTRACTOR_ID);
    }
}

#[test]
fn incremental_cache_reuses_unchanged_files() {
    let dir = fixture();
    let mut cache = IncrementalCache::default();
    let (first, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 4);
    assert_eq!(stats.reused_files, 0);
    let (same, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    assert_eq!(stats.reused_files, 4);
    assert_eq!(first.nodes, same.nodes);
    assert_eq!(first.edges, same.edges);
    write(dir.path(), "beta/src/lib.rs", "pub fn changed() {}\n");
    let (_, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 3);
    std::fs::remove_file(dir.path().join("beta/src/lib.rs")).unwrap();
    let (_, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.deleted_files, 1);
}

#[test]
fn single_file_extraction_resolves_local_calls() {
    let x = extract_source(b"pub fn a() {}\npub fn b() { a(); }\n", "src/lib.rs", &id()).unwrap();
    assert!(has_edge(
        &x,
        "CALLS",
        "sym:local/rust-ws@src/lib.rs#b",
        "sym:local/rust-ws@src/lib.rs#a"
    ));
}

#[test]
fn inline_modules_scope_identity_bindings_and_qualified_calls() {
    // #1075 review: sibling inline modules keep distinct identities, each
    // `use` binds only in its module, and a qualified call reaches the
    // inline module's item.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    write(dir.path(), "src/x.rs", "pub fn f() {}\n");
    write(dir.path(), "src/y.rs", "pub fn f() {}\n");
    write(
        dir.path(),
        "src/lib.rs",
        r#"
pub mod x;
pub mod y;
mod a { use crate::x::f; pub fn run() { f(); } }
mod b { use crate::y::f; pub fn run() { f(); } }
mod inner { pub fn target() {} }
pub fn caller() { crate::inner::target(); inner::target(); }
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    let lib = "sym:local/rust-ws@src/lib.rs";
    for sym in [format!("{lib}#a::run"), format!("{lib}#b::run")] {
        assert!(x.nodes.iter().any(|n| n.id == sym), "missing {sym}");
    }
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{lib}#a::run"),
        "sym:local/rust-ws@src/x.rs#f"
    ));
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{lib}#b::run"),
        "sym:local/rust-ws@src/y.rs#f"
    ));
    assert!(!has_edge(
        &x,
        "CALLS",
        &format!("{lib}#a::run"),
        "sym:local/rust-ws@src/y.rs#f"
    ));
    let caller_to_target: Vec<_> = edge_pairs(&x, "CALLS")
        .into_iter()
        .filter(|(s, d)| *s == format!("{lib}#caller") && *d == format!("{lib}#inner::target"))
        .collect();
    assert_eq!(caller_to_target.len(), 2, "{caller_to_target:?}");
}

#[test]
fn value_bindings_shadow_items_and_turbofish_calls_resolve() {
    let x = extract_source(
        br#"
pub fn target() {}
pub fn parse<T>() {}
pub fn param(target: fn()) { target(); }
pub fn closure() { let target = || {}; target(); }
pub fn turbofish() { parse::<u32>(); }
pub fn plain() { target(); }
"#,
        "src/lib.rs",
        &id(),
    )
    .unwrap();
    let lib = "sym:local/rust-ws@src/lib.rs";
    for shadowed in ["param", "closure"] {
        assert!(
            !has_edge(
                &x,
                "CALLS",
                &format!("{lib}#{shadowed}"),
                &format!("{lib}#target")
            ),
            "{shadowed}"
        );
    }
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{lib}#plain"),
        &format!("{lib}#target")
    ));
    assert!(has_edge(
        &x,
        "CALLS",
        &format!("{lib}#turbofish"),
        &format!("{lib}#parse")
    ));
}

#[test]
fn trait_impls_and_function_local_items_keep_distinct_identities() {
    let x = extract_source(
        br#"
pub struct Failure;
impl From<std::io::Error> for Failure { fn from(_: std::io::Error) -> Self { Failure } }
impl From<std::fmt::Error> for Failure { fn from(_: std::fmt::Error) -> Self { Failure } }
pub struct Visitor;
pub fn outer() { struct Visitor; }
"#,
        "src/lib.rs",
        &id(),
    )
    .unwrap();
    let ids: Vec<&str> = x.nodes.iter().map(|n| n.id.as_str()).collect();
    let unique: BTreeSet<&str> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "{ids:?}");
    let lib = "sym:local/rust-ws@src/lib.rs";
    for sym in [
        format!("{lib}#Failure.From<std::io::Error>.from"),
        format!("{lib}#Failure.From<std::fmt::Error>.from"),
        format!("{lib}#Visitor"),
        format!("{lib}#outer::Visitor"),
    ] {
        assert!(unique.contains(sym.as_str()), "missing {sym}");
    }
}

#[test]
fn function_local_uses_and_items_stay_inside_their_function() {
    // #1075 review: a `use` or nested `fn` inside a function body is not
    // visible to sibling functions or by module path.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
mod x { pub fn f() {} }
mod y { pub fn f() {} }
use crate::x::f;
pub fn scoped() { use crate::y::f; f(); }
pub fn outer() { f(); }
pub fn target() {}
pub fn other() { fn target() {} target(); }
pub fn caller() { target(); crate::target(); }
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    let lib = "sym:local/rust-ws@src/lib.rs";
    let calls = |src: &str| -> Vec<String> {
        edge_pairs(&x, "CALLS")
            .into_iter()
            .filter(|(s, _)| *s == format!("{lib}#{src}"))
            .map(|(_, d)| d.to_string())
            .collect()
    };
    assert_eq!(calls("scoped"), vec![format!("{lib}#y::f")]);
    assert_eq!(calls("outer"), vec![format!("{lib}#x::f")]);
    assert_eq!(calls("other"), vec![format!("{lib}#other::target")]);
    assert_eq!(
        calls("caller"),
        vec![format!("{lib}#target"), format!("{lib}#target")]
    );
}

#[test]
fn inner_uses_shadow_outer_items_and_block_uses_stay_in_their_block() {
    // #1075 review: items and `use`s are searched together per scope,
    // innermost first, and a block is its own scope.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
fn f() {}
mod x { pub fn g() {} }
mod y { pub fn f() {} pub fn g() {} }
use crate::x::g;
pub fn shadowed() { use crate::y::f; f(); }
pub fn blocks() { { use crate::y::g; g(); } g(); }
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    let lib = "sym:local/rust-ws@src/lib.rs";
    let calls = |src: &str| -> Vec<String> {
        let mut dst: Vec<String> = edge_pairs(&x, "CALLS")
            .into_iter()
            .filter(|(s, _)| *s == format!("{lib}#{src}"))
            .map(|(_, d)| d.to_string())
            .collect();
        dst.sort();
        dst
    };
    assert_eq!(calls("shadowed"), vec![format!("{lib}#y::f")]);
    assert_eq!(
        calls("blocks"),
        vec![format!("{lib}#x::g"), format!("{lib}#y::g")]
    );
}

#[test]
fn sibling_block_items_keep_distinct_identities() {
    // #1075 review: same-name items in sibling blocks are distinct symbols,
    // and each call reaches the one in its own block.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        "pub fn caller() { { fn target() {} target(); } { fn target() {} target(); } }\n",
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    let lib = "sym:local/rust-ws@src/lib.rs";
    let first = format!("{lib}#caller::<block1>::target");
    let second = format!("{lib}#caller::<block2>::target");
    let ids: Vec<&str> = x
        .nodes
        .iter()
        .filter(|n| n.label == "Symbol")
        .map(|n| n.id.as_str())
        .collect();
    assert!(
        ids.contains(&first.as_str()) && ids.contains(&second.as_str()),
        "{ids:?}"
    );
    let mut calls: Vec<String> = edge_pairs(&x, "CALLS")
        .into_iter()
        .map(|(_, d)| d.to_string())
        .collect();
    calls.sort();
    assert_eq!(calls, vec![first, second]);
}

#[test]
fn axum_endpoints_chained_nest_merge_params_and_cross_file_handlers() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/handlers.rs",
        r#"
pub async fn list_users() {}
pub async fn create_user() {}
pub async fn get_user_v7() {}
pub async fn get_user_v8() {}
pub async fn delete_user() {}
pub async fn patch_item() {}
pub async fn any_ping() {}
"#,
    );
    write(
        dir.path(),
        "src/sub.rs",
        r#"
use axum::{routing::{delete, patch}, Router};
use crate::handlers::{delete_user, patch_item};

pub fn nested_router() -> Router {
    Router::new()
        .route("/members/{id}", delete(delete_user))
}

pub fn merged_router() -> Router {
    axum::Router::new()
        .route("/items/:id", patch(patch_item))
}
"#,
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
pub mod handlers;
pub mod sub;

use axum::{
    routing::{any, get, post},
    Router,
};
use crate::handlers::{any_ping, create_user, get_user_v8, list_users};

pub fn app() -> Router {
    let inline_sub = Router::new()
        .route("/legacy/:id", get(handlers::get_user_v7));
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/users/{id}", get(get_user_v8))
        .route("/ping", any(any_ping))
        .nest("/v1", inline_sub)
        .nest("/api", sub::nested_router())
        .merge(sub::merged_router())
}
"#,
    );

    let x = extract_dir(dir.path(), &id()).unwrap();
    let expected = [
        (
            "ep:local/rust-ws@GET:/users",
            "GET",
            "/users",
            "sym:local/rust-ws@src/handlers.rs#list_users",
        ),
        (
            "ep:local/rust-ws@POST:/users",
            "POST",
            "/users",
            "sym:local/rust-ws@src/handlers.rs#create_user",
        ),
        (
            "ep:local/rust-ws@GET:/users/{id}",
            "GET",
            "/users/{id}",
            "sym:local/rust-ws@src/handlers.rs#get_user_v8",
        ),
        (
            "ep:local/rust-ws@ANY:/ping",
            "ANY",
            "/ping",
            "sym:local/rust-ws@src/handlers.rs#any_ping",
        ),
        (
            "ep:local/rust-ws@GET:/v1/legacy/:id",
            "GET",
            "/v1/legacy/:id",
            "sym:local/rust-ws@src/handlers.rs#get_user_v7",
        ),
        (
            "ep:local/rust-ws@DELETE:/api/members/{id}",
            "DELETE",
            "/api/members/{id}",
            "sym:local/rust-ws@src/handlers.rs#delete_user",
        ),
        (
            "ep:local/rust-ws@PATCH:/items/:id",
            "PATCH",
            "/items/:id",
            "sym:local/rust-ws@src/handlers.rs#patch_item",
        ),
    ];

    for (ep_id, method, path, sym) in expected {
        let ep = x
            .nodes
            .iter()
            .find(|n| n.id == ep_id && n.label == "Endpoint")
            .unwrap_or_else(|| panic!("missing endpoint {ep_id}; nodes: {:#?}", x.nodes));
        assert_eq!(ep.props["method"], method);
        assert_eq!(ep.props["path"], path);
        assert_eq!(ep.props["framework"], "axum");
        assert_eq!(ep.props["language"], "rust");
        assert_eq!(ep.props["handler_sym"], sym);
        assert!(
            has_edge(&x, "HANDLES", ep_id, sym),
            "missing HANDLES {ep_id} -> {sym}"
        );
    }
    // Nested sub-routers are not also emitted unprefixed at root.
    assert!(
        !x.nodes
            .iter()
            .any(|n| n.id == "ep:local/rust-ws@GET:/legacy/:id")
    );
    assert!(
        !x.nodes
            .iter()
            .any(|n| n.id == "ep:local/rust-ws@DELETE:/members/{id}")
    );
}

#[test]
fn actix_web_attributes_route_resource_scope_and_app() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"actix_svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/handlers.rs",
        r#"
use actix_web::{delete, get, post, route};

#[get("/standalone")]
pub async fn standalone() {}

#[post("/items")]
pub async fn create_item() {}

#[route("/multi", method = "PUT", method = "PATCH")]
pub async fn multi_method() {}

#[actix_web::head("/head-check")]
pub async fn head_check() {}

#[delete("/scoped/{id}")]
pub async fn delete_scoped() {}

pub async fn res_get() {}
pub async fn res_all() {}
pub async fn app_direct() {}
"#,
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
pub mod handlers;

use actix_web::{web, App};
use crate::handlers::{
    app_direct, create_item, delete_scoped, multi_method, res_all, res_get,
};

pub fn configure() {
    let _app = App::new()
        .route("/direct", web::get().to(app_direct))
        .service(
            web::resource("/res")
                .route(web::get().to(res_get))
                .to(res_all),
        )
        .service(
            web::scope("/api")
                .service(create_item)
                .service(delete_scoped)
                .service(
                    web::resource("/nested-res")
                        .route(web::post().to(handlers::res_get)),
                ),
        )
        .service(multi_method);
}
"#,
    );

    let x = extract_dir(dir.path(), &id()).unwrap();
    let expected = [
        (
            "ep:local/rust-ws@GET:/standalone",
            "GET",
            "/standalone",
            "sym:local/rust-ws@src/handlers.rs#standalone",
        ),
        (
            "ep:local/rust-ws@POST:/api/items",
            "POST",
            "/api/items",
            "sym:local/rust-ws@src/handlers.rs#create_item",
        ),
        (
            "ep:local/rust-ws@DELETE:/api/scoped/{id}",
            "DELETE",
            "/api/scoped/{id}",
            "sym:local/rust-ws@src/handlers.rs#delete_scoped",
        ),
        (
            "ep:local/rust-ws@PUT:/multi",
            "PUT",
            "/multi",
            "sym:local/rust-ws@src/handlers.rs#multi_method",
        ),
        (
            "ep:local/rust-ws@PATCH:/multi",
            "PATCH",
            "/multi",
            "sym:local/rust-ws@src/handlers.rs#multi_method",
        ),
        (
            "ep:local/rust-ws@HEAD:/head-check",
            "HEAD",
            "/head-check",
            "sym:local/rust-ws@src/handlers.rs#head_check",
        ),
        (
            "ep:local/rust-ws@GET:/direct",
            "GET",
            "/direct",
            "sym:local/rust-ws@src/handlers.rs#app_direct",
        ),
        (
            "ep:local/rust-ws@GET:/res",
            "GET",
            "/res",
            "sym:local/rust-ws@src/handlers.rs#res_get",
        ),
        (
            "ep:local/rust-ws@ANY:/res",
            "ANY",
            "/res",
            "sym:local/rust-ws@src/handlers.rs#res_all",
        ),
        (
            "ep:local/rust-ws@POST:/api/nested-res",
            "POST",
            "/api/nested-res",
            "sym:local/rust-ws@src/handlers.rs#res_get",
        ),
    ];

    for (ep_id, method, path, sym) in expected {
        let ep = x
            .nodes
            .iter()
            .find(|n| n.id == ep_id && n.label == "Endpoint")
            .unwrap_or_else(|| panic!("missing endpoint {ep_id}; nodes: {:#?}", x.nodes));
        assert_eq!(ep.props["method"], method);
        assert_eq!(ep.props["path"], path);
        assert_eq!(ep.props["framework"], "actix-web");
        assert_eq!(ep.props["language"], "rust");
        assert_eq!(ep.props["handler_sym"], sym);
        assert!(
            has_edge(&x, "HANDLES", ep_id, sym),
            "missing HANDLES {ep_id} -> {sym}"
        );
    }
    // Attribute routes mounted under `/api` do not also emit unprefixed.
    assert!(
        !x.nodes
            .iter()
            .any(|n| n.id == "ep:local/rust-ws@POST:/items")
    );
    assert!(
        !x.nodes
            .iter()
            .any(|n| n.id == "ep:local/rust-ws@DELETE:/scoped/{id}")
    );
}

#[test]
fn unproven_lookalikes_yield_nothing() {
    let x = extract_source(
        br#"
struct Router;
impl Router {
    fn new() -> Self { Router }
    fn route(self, _p: &str, _h: ()) -> Self { self }
    fn nest(self, _p: &str, _s: Self) -> Self { self }
}
fn get<F>(_f: F) {}
pub fn handler() {}

#[get("/unproven-attr")]
pub fn attr_handler() {}

pub fn main() {
    let _ = Router::new()
        .route("/unproven", get(handler))
        .nest("/api", Router::new().route("/sub", get(handler)));
}
"#,
        "src/lib.rs",
        &id(),
    )
    .unwrap();

    assert!(!x.nodes.iter().any(|n| n.label == "Endpoint"));
    assert!(!x.edges.iter().any(|e| e.label == "HANDLES"));
    assert!(!x.nodes.iter().any(|n| n.label == "Gap"));
}

#[test]
fn closure_handler_and_computed_path_yield_cited_gaps() {
    let x = extract_source(
        br#"
use axum::{routing::get, Router};
use actix_web::{post, web, App};

const DYN_PATH: &str = "/dynamic";

pub async fn ok_handler() {}

#[post(DYN_PATH)]
pub async fn actix_dyn_attr() {}

pub fn build() {
    let _axum = Router::new()
        .route("/closure", get(|| async { "hi" }))
        .route(DYN_PATH, get(ok_handler));
    let _actix = App::new()
        .route("/actix-closure", web::get().to(|| async { "yo" }))
        .route(DYN_PATH, web::get().to(ok_handler));
}
"#,
        "src/lib.rs",
        &id(),
    )
    .unwrap();

    for ep_id in [
        "ep:local/rust-ws@GET:/closure",
        "ep:local/rust-ws@GET:/actix-closure",
    ] {
        let ep = x
            .nodes
            .iter()
            .find(|n| n.id == ep_id && n.label == "Endpoint")
            .unwrap_or_else(|| panic!("missing {ep_id}"));
        let gap_id = ep.props["handler_sym"].as_str().unwrap();
        assert!(gap_id.starts_with("gap:rust-handler:"), "{gap_id}");
        let gap = x.nodes.iter().find(|n| n.id == gap_id).unwrap();
        assert_eq!(gap.label, "Gap");
        assert_eq!(
            gap.props["reason"],
            "unresolved Rust route handler expression"
        );
        assert!(has_edge(&x, "HANDLES", ep_id, gap_id));
    }

    let route_gaps: Vec<_> = x
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.id.starts_with("gap:route:"))
        .collect();
    assert_eq!(route_gaps.len(), 3, "{route_gaps:#?}");
    for gap in &route_gaps {
        assert_eq!(gap.props["reason"], "computed Rust route path");
        assert_eq!(gap.props["expression"], "DYN_PATH");
    }
    assert!(route_gaps.iter().any(|g| has_edge(
        &x,
        "HANDLES",
        &g.id,
        "sym:local/rust-ws@src/lib.rs#actix_dyn_attr"
    )));
    assert!(route_gaps.iter().any(|g| has_edge(
        &x,
        "HANDLES",
        &g.id,
        "sym:local/rust-ws@src/lib.rs#ok_handler"
    )));
}

#[test]
fn endpoint_determinism_across_runs_and_cache() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"det\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/b.rs",
        r#"
use actix_web::{get, web, App};

#[get("/b")]
pub async fn b_attr() {}

pub async fn b_direct() {}

pub fn b_app() {
    let _ = App::new()
        .service(web::scope("/v1").service(b_attr))
        .route("/b-direct", web::post().to(b_direct));
}
"#,
    );
    write(
        dir.path(),
        "src/a.rs",
        r#"
use axum::{routing::{get, post}, Router};

pub async fn a_get() {}
pub async fn a_post() {}

pub fn a_router() -> Router {
    Router::new()
        .route("/a", get(a_get).post(a_post))
}
"#,
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
pub mod a;
pub mod b;

use axum::Router;

pub fn root() -> Router {
    Router::new().nest("/api", a::a_router())
}
"#,
    );

    let mut cache = IncrementalCache::default();
    let (first, stats1) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats1.recomputed_files, 3);
    let (second, stats2) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats2.reused_files, 3);
    let third = extract_dir(dir.path(), &id()).unwrap();

    assert_eq!(first.nodes, second.nodes);
    assert_eq!(first.edges, second.edges);
    assert_eq!(first.nodes, third.nodes);
    assert_eq!(first.edges, third.edges);
}

#[test]
fn attribute_routes_under_a_computed_scope_are_gaps_not_root_endpoints() {
    // A handler mounted only under `web::scope(prefix)` with a computed
    // prefix is served at an unknown path: never claim its bare path.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"actix_svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
use actix_web::{get, web, App};

#[get("/x")]
pub async fn x() {}

pub fn configure(prefix: &str) {
    let _app = App::new().service(web::scope(prefix).service(x));
}
"#,
    );
    let ex = extract_dir(dir.path(), &id()).unwrap();
    let endpoints: Vec<&str> = ex
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| n.id.as_str())
        .collect();
    assert!(endpoints.is_empty(), "{endpoints:?}");
    let x_sym = "sym:local/rust-ws@src/lib.rs#x";
    assert!(
        ex.edges
            .iter()
            .any(|e| e.label == "HANDLES" && e.src.starts_with("gap:route:") && e.dst == x_sym),
        "the handler is cited by a computed-route Gap"
    );
}

fn endpoint_ids(x: &Extraction) -> Vec<&str> {
    let mut ids: Vec<&str> = x
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| n.id.as_str())
        .collect();
    ids.sort();
    ids
}

#[test]
fn nearer_local_items_shadow_framework_imports_and_globs() {
    // AC-0364 (#1089): a function-local `fn get` shadows the module's
    // `use axum::routing::get`, and a same-scope `fn post` shadows a
    // `routing::*` glob: neither is claimed as an axum route.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
use axum::{Router, routing::get};

pub async fn advertised() {}
pub async fn actual() {}

pub fn shadowed() -> Router {
    fn get<H>(_: H) -> axum::routing::MethodRouter {
        axum::routing::post(actual)
    }
    Router::new().route("/x", get(advertised))
}

pub fn real() -> Router {
    Router::new().route("/real", get(actual))
}

pub mod globbed {
    use axum::routing::*;
    use axum::Router;
    fn post<H>(_: H) -> axum::routing::MethodRouter { todo!() }
    pub async fn h() {}
    pub fn app() -> Router {
        Router::new().route("/g", post(h)).route("/p", put(h))
    }
}
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        endpoint_ids(&x),
        ["ep:local/rust-ws@GET:/real", "ep:local/rust-ws@PUT:/p",]
    );
}

#[test]
fn nested_routers_from_another_workspace_crate_keep_their_prefix() {
    // AC-0364 (#1089): routers are keyed by the defining crate's symbol.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "api/Cargo.toml",
        "[package]\nname = \"api\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "api/src/lib.rs",
        r#"
use axum::{Router, routing::get};
pub async fn health() {}
pub fn router() -> Router {
    Router::new().route("/health", get(health))
}
"#,
    );
    write(
        dir.path(),
        "web/Cargo.toml",
        "[package]\nname = \"web\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "web/src/lib.rs",
        r#"
use axum::Router;
pub fn app() -> Router {
    Router::new().nest("/api", api::router())
}
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(endpoint_ids(&x), ["ep:local/rust-ws@GET:/api/health"]);
}

#[test]
fn actix_root_and_scoped_mounts_and_root_route_slashes() {
    // AC-0364 (#1089): a handler served at the root and under a scope keeps
    // both paths; actix keeps a scoped `"/"` route's slash and treats `""`
    // as the scope itself, while an axum nested root route is the prefix.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"
use actix_web::{get, web, App};
use axum::{Router, routing};

#[get("/status")]
pub async fn status() {}
pub async fn index() {}
pub async fn bare() {}
pub async fn nested() {}

pub fn actix() {
    let _app = App::new()
        .service(status)
        .service(
            web::scope("/api")
                .service(status)
                .route("/", web::get().to(index))
                .route("", web::post().to(bare)),
        );
}

pub fn axum() -> Router {
    Router::new().nest("/v1", Router::new().route("/", routing::get(nested)))
}
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        endpoint_ids(&x),
        [
            "ep:local/rust-ws@GET:/api/",
            "ep:local/rust-ws@GET:/api/status",
            "ep:local/rust-ws@GET:/status",
            "ep:local/rust-ws@GET:/v1",
            "ep:local/rust-ws@POST:/api",
        ]
    );
}

#[test]
fn route_literals_decode_rust_escapes_or_decline() {
    // AC-0364 (#1089): `"\u{2f}"` is `/` at runtime, raw strings keep their
    // backslashes, and an escape Rust rejects is not claimed as a path.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r###"
use axum::{Router, routing::get};
pub async fn h() {}
pub fn app() -> Router {
    Router::new()
        .route("/a\u{2f}b", get(h))
        .route(r"/raw\d", get(h))
        .route("/bad\q", get(h))
        .nest("/n\x2Fm", Router::new().route("/c", get(h)))
}
"###,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        endpoint_ids(&x),
        [
            "ep:local/rust-ws@GET:/a/b",
            "ep:local/rust-ws@GET:/n/m/c",
            r"ep:local/rust-ws@GET:/raw\d",
        ]
    );
}
