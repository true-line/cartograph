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

// AC-0367: reqwest client calls proven through `use` / qualified paths and
// a proven Client receiver become fetch sites; literal URLs as written,
// anything else computed.
fn reqwest_sites(lib: &str) -> Vec<(String, String, String)> {
    reqwest_sites_with(lib, &[])
}

fn reqwest_sites_with(lib: &str, extra: &[(&str, &str)]) -> Vec<(String, String, String)> {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(dir.path(), "src/lib.rs", lib);
    for (path, text) in extra {
        write(dir.path(), path, text);
    }
    let x = extract_dir(dir.path(), &id()).unwrap();
    x.fetch_sites
        .iter()
        .map(|site| {
            let url = match &site.url {
                adapters_fw::events::IdentityExpr::Literal(url) => format!("lit {url}"),
                adapters_fw::events::IdentityExpr::Computed(text) => format!("computed {text}"),
                adapters_fw::events::IdentityExpr::EnvRef(key) => format!("env {key}"),
            };
            let owner = site.symbol.clone().unwrap_or_default();
            let owner = owner.rsplit('#').next().unwrap().to_string();
            (site.method.clone(), url, owner)
        })
        .collect()
}

fn site(method: &str, url: &str, owner: &str) -> (String, String, String) {
    (method.into(), url.into(), owner.into())
}

#[test]
fn reqwest_calls_on_proven_clients_become_fetch_sites() {
    let sites = reqwest_sites(
        r#"
use reqwest::{Client, Method};
use std::sync::Arc;

pub struct Api { http: Client, base: String }

pub async fn free() { reqwest::get("https://api.example.com/health").await; }
pub fn blocking() { reqwest::blocking::get("http://localhost/ping"); }
pub async fn chained() { Client::new().post("/users").send().await; }
pub async fn built() -> Result<(), reqwest::Error> {
    let client = reqwest::Client::builder().timeout(T).build()?;
    client.put("/users/1").send().await?;
    Ok(())
}
pub async fn param(client: &Client, id: u32) {
    client.delete(format!("/users/{id}")).send().await;
}
pub async fn shared(client: Arc<Client>) { client.request(Method::PATCH, "/users").send().await; }
impl Api {
    pub async fn me(&self) { self.http.get("/me").send().await; }
}
"#,
    );
    assert_eq!(
        sites,
        vec![
            site("GET", "lit https://api.example.com/health", "free"),
            site("GET", "lit http://localhost/ping", "blocking"),
            site("POST", "lit /users", "chained"),
            site("PUT", "lit /users/1", "built"),
            site("DELETE", "computed format!(\"/users/{id}\")", "param"),
            site("PATCH", "lit /users", "shared"),
            site("GET", "lit /me", "Api.me"),
        ]
    );
}

#[test]
fn reqwest_lookalikes_and_unproven_receivers_yield_nothing() {
    let sites = reqwest_sites(
        r#"
mod net { pub struct Client; impl Client { pub fn new() -> Self { Client } pub fn get(&self, _: &str) {} } }
use net::Client;
pub struct A { http: reqwest::Client }
pub struct B { http: Client }

pub fn local_client() { Client::new().get("/not-reqwest"); }
pub fn rebound(other: Client) {
    let client = reqwest::Client::new();
    let client = other;
    client.get("/rebound");
}
pub fn untyped(client: Thing) { client.get("/untyped"); }
pub fn map(m: std::collections::HashMap<u8, u8>) { m.get(&1); }
pub fn shadowed_get(get: fn(&str)) { get("/shadowed"); }
impl B { pub fn lookalike_field(&self) { self.http.get("/b-field"); } }
pub fn closure() { let f = |client: Client| client.get("/closure"); }
"#,
    );
    assert!(sites.is_empty(), "{sites:?}");
}

#[test]
fn reqwest_proof_rejects_local_wrappers_and_foreign_impl_fields() {
    // AC-0367 (#1093 review): a generic parameter named `Client` shadows the
    // import, a local `Arc` is not std's, and `self.http`
    // in `impl other::FakeApi` is not proven by another struct's Client
    // field; std::sync::Arc and the struct's own impl stay proven.
    let sites = reqwest_sites_with(
        r#"
mod other;
mod local {
    use reqwest::Client;
    pub struct Arc<T>(T);
    impl<T> Arc<T> { pub fn get(&self, _: &str) {} }
    pub fn run(client: Arc<Client>) { client.get("/local-arc"); }
}
mod generic {
    use reqwest::Client;
    pub struct S;
    pub fn f<Client: Fake>(client: &Client) { client.get("/generic"); }
    impl<Client: Fake> S { pub fn g(c: Client) { c.get("/generic-impl"); } }
}
pub struct Carrier { pub http: reqwest::Client }
impl other::FakeApi { pub fn f(&self) { self.http.get("/not-http"); } }
impl Carrier { pub fn real(&self) { self.http.get("/carrier"); } }
pub fn std_arc(client: std::sync::Arc<reqwest::Client>) { client.get("/std-arc"); }
"#,
        &[(
            "src/other.rs",
            "pub struct Fake; impl Fake { pub fn get(&self, _: &str) {} }
pub struct FakeApi { pub http: Fake }
",
        )],
    );
    assert_eq!(
        sites,
        vec![
            site("GET", "lit /carrier", "Carrier.real"),
            site("GET", "lit /std-arc", "std_arc"),
        ]
    );
}

#[test]
fn reqwest_proof_rejects_generic_constructors_and_block_local_structs() {
    // AC-0367 (#1093 re-review): `Client::new()` under a generic parameter
    // named `Client` is not reqwest's, and a block-local `Api` with a Client
    // field proves nothing about a module-level `Api`; a module-level
    // struct's genuine Client field and a real constructor stay proven.
    let sites = reqwest_sites_with(
        r#"
mod generic {
    use reqwest::Client;
    pub trait Fake: Sized { fn new() -> Self; fn get(&self, _: &str); }
    pub fn run<Client: Fake>() { Client::new().get("/not-http"); }
    pub fn real() { Client::new().get("/real"); }
}
mod collide {
    fn declarations() { struct Api { http: reqwest::Client } }
    struct Fake;
    impl Fake { fn get(&self, _: &str) {} }
    struct Api { http: Fake }
    impl Api { fn run(&self) { self.http.get("/not-http"); } }
}
pub struct Carrier { pub http: reqwest::Client }
impl Carrier { pub fn real(&self) { self.http.get("/carrier"); } }
"#,
        &[],
    );
    assert_eq!(
        sites,
        vec![
            site("GET", "lit /real", "generic::real"),
            site("GET", "lit /carrier", "Carrier.real"),
        ]
    );
}

// rdkafka records and subscriptions on proven consumers become kafka-topic
// event sites as (role, identity, owner). (T-0469)
fn rdkafka_sites(lib: &str) -> Vec<(String, String, String)> {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(dir.path(), "src/lib.rs", lib);
    let x = extract_dir(dir.path(), &id()).unwrap();
    x.event_sites
        .iter()
        .map(|site| {
            assert_eq!(site.kind, "kafka-topic");
            let identity = match &site.identity {
                adapters_fw::events::IdentityExpr::Literal(t) => format!("lit {t}"),
                adapters_fw::events::IdentityExpr::Computed(t) => format!("computed {t}"),
                adapters_fw::events::IdentityExpr::EnvRef(k) => format!("env {k}"),
            };
            let owner = site.symbol.clone().unwrap_or_default();
            let owner = owner.rsplit('#').next().unwrap().to_string();
            (format!("{:?}", site.role), identity, owner)
        })
        .collect()
}

#[test]
fn rdkafka_records_and_subscriptions_become_event_sites() {
    let sites = rdkafka_sites(
        r#"
use rdkafka::consumer::{BaseConsumer, Consumer, StreamConsumer};
use rdkafka::producer::{BaseRecord, FutureProducer, FutureRecord};
use std::sync::Arc;

pub struct Worker { consumer: StreamConsumer<Ctx> }

pub async fn place(producer: &FutureProducer, topic: &str) {
    producer.send(FutureRecord::to("orders").payload("p").key("k"), T).await;
    producer.send(FutureRecord::to(&topic).payload("p"), T).await;
}
pub fn base(producer: &P) {
    producer.send(BaseRecord::to("audit").payload("p"));
    producer.send(rdkafka::producer::future_producer::FutureRecord::to("full"));
}
pub fn listen(config: &ClientConfig, topics: &[&str]) {
    let consumer: StreamConsumer = config.create().unwrap();
    consumer.subscribe(&["orders", "payments"]).unwrap();
    consumer.subscribe(&["phantom"; 0]).unwrap();
}
pub fn shared(consumer: Arc<BaseConsumer>, topics: &[&str]) {
    consumer.subscribe(topics).unwrap();
}
impl Worker {
    pub fn run(&self) { self.consumer.subscribe(&[TOPIC]).unwrap(); }
}
"#,
    );
    let site = |role: &str, identity: &str, owner: &str| {
        (role.to_string(), identity.to_string(), owner.to_string())
    };
    assert_eq!(
        sites,
        vec![
            site("Produces", "lit orders", "place"),
            site("Produces", "computed topic", "place"),
            site("Produces", "lit audit", "base"),
            site("Produces", "lit full", "base"),
            site("Consumes", "lit orders", "listen"),
            site("Consumes", "lit payments", "listen"),
            site("Consumes", "computed [\"phantom\"; 0]", "listen"),
            site("Consumes", "computed topics", "shared"),
            site("Consumes", "computed TOPIC", "Worker.run"),
        ]
    );
}

// AC-0469 (#1372 review): a reference that names its lifetime still proves
// its consumer or client; an unknown type behind one still proves nothing.
// (T-0469)
#[test]
fn lifetime_references_still_prove_consumers_and_clients() {
    let sites = rdkafka_sites(
        r#"
use rdkafka::consumer::{BaseConsumer, Consumer, StreamConsumer};

pub struct Worker<'a> { consumer: &'a StreamConsumer }

pub fn named<'a>(consumer: &'a StreamConsumer) { consumer.subscribe(&["named"]).unwrap(); }
pub fn placeholder(consumer: &'_ mut BaseConsumer) { consumer.subscribe(&["placeholder"]).unwrap(); }
pub fn forever(consumer: &'static StreamConsumer) { consumer.subscribe(&["forever"]).unwrap(); }
pub fn other<'a>(consumer: &'a Other) { consumer.subscribe(&["other"]).unwrap(); }
impl<'a> Worker<'a> {
    pub fn run(&self) { self.consumer.subscribe(&["field"]).unwrap(); }
}
"#,
    );
    let consumes = |identity: &str, owner: &str| {
        (
            "Consumes".to_string(),
            identity.to_string(),
            owner.to_string(),
        )
    };
    assert_eq!(
        sites,
        vec![
            consumes("lit named", "named"),
            consumes("lit placeholder", "placeholder"),
            consumes("lit forever", "forever"),
            consumes("lit field", "Worker.run"),
        ]
    );
    let fetches = reqwest_sites(
        r#"
use reqwest::Client;

pub async fn param<'a>(client: &'a Client) { client.get("/lifetime").send().await; }
pub async fn other<'a>(client: &'a Other) { client.get("/other").send().await; }
"#,
    );
    assert_eq!(fetches, vec![site("GET", "lit /lifetime", "param")]);
}

// Unimported or lookalike records, a turbofish record, an inferred
// `create()` consumer, other types, a shadowing generic, `assign`, a local
// trait's `unwrap`/`expect` on a consumer and a locally declared
// `FutureRecord` yield no site. (T-0469)
#[test]
fn unproven_rdkafka_calls_are_not_event_sites() {
    let sites = rdkafka_sites(
        r#"
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::FutureRecord;
use other::BaseRecord;

pub fn records() {
    BaseRecord::to("lookalike");
    FutureRecord::<(), str>::to("turbofish");
    Record::to("unimported");
    FutureRecord::new("ctor");
}
pub fn inferred(config: &ClientConfig) {
    let consumer = config.create::<StreamConsumer>().unwrap();
    consumer.subscribe(&["inferred"]).unwrap();
    let other: Other = make();
    other.subscribe(&["other"]).unwrap();
}
pub fn generic<StreamConsumer: Consumer>(consumer: &StreamConsumer) {
    consumer.subscribe(&["generic"]).unwrap();
}
pub fn assigned(consumer: &StreamConsumer) {
    consumer.assign(&list).unwrap();
}
trait Unwrap { fn unwrap(&self) -> Fake; fn expect(&self, _: &str) -> Fake; }
impl Unwrap for StreamConsumer {
    fn unwrap(&self) -> Fake { Fake }
    fn expect(&self, _: &str) -> Fake { Fake }
}
pub fn wrapped(consumer: &StreamConsumer) {
    consumer.unwrap().subscribe(&["not-kafka"]);
    consumer.expect("x").subscribe(&["not-kafka"]);
}
mod local {
    struct FutureRecord;
    impl FutureRecord { fn to(_: &str) {} }
    pub fn own() { FutureRecord::to("own"); }
}
"#,
    );
    assert_eq!(sites, Vec::<(String, String, String)>::new());
}

fn evidence_spans(value: &serde_json::Value) -> Vec<(u64, u64)> {
    value["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["byte_start"].as_u64().unwrap(),
                e["byte_end"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn config_read_ids(x: &Extraction) -> Vec<&str> {
    x.nodes
        .iter()
        .filter(|node| node.label == "ConfigRead")
        .map(|node| node.id.as_str())
        .collect()
}

// AC-0597 (#1789, ADR-0059): one literal `std::env::var` read, one dynamic
// read and one shadowed look-alike (a local function named `var`) yield
// exactly one named ConfigRead, one dynamic ConfigRead and nothing for the
// look-alike. Only the key name and the span of the call are recorded,
// never the fallback value.
#[test]
fn std_env_var_reads_are_config_reads() {
    let text = r#"use std::env;

pub fn load(name: &str) -> (String, Option<String>) {
    let url = env::var("DATABASE_URL").unwrap_or("postgres://user:pw@db/app".into());
    (url, env::var(name).ok())
}

mod fake {
    fn var(key: &str) -> String {
        key.to_string()
    }
    pub fn read() -> String {
        var("SHADOWED")
    }
}
"#;
    let x = extract_source(text.as_bytes(), "src/lib.rs", &id()).unwrap();
    assert_eq!(
        config_read_ids(&x),
        [
            "cfgread:local/rust-ws@src/lib.rs:env:DATABASE_URL",
            "cfgread:local/rust-ws@src/lib.rs:env:?",
        ]
    );
    let named = x
        .nodes
        .iter()
        .find(|n| n.id == "cfgread:local/rust-ws@src/lib.rs:env:DATABASE_URL")
        .unwrap();
    assert_eq!(named.props["source"], "env");
    assert_eq!(named.props["key"], "DATABASE_URL");
    assert_eq!(named.props["language"], "rust");
    assert_eq!(named.props["prov"]["tier"], "Deterministic");
    assert_eq!(named.props["prov"]["confidence_tier"], "Confirmed");
    let read = r#"env::var("DATABASE_URL")"#;
    let at = text.find(read).unwrap() as u64;
    assert_eq!(evidence_spans(&named.props), [(at, at + read.len() as u64)]);
    let dynamic = x
        .nodes
        .iter()
        .find(|n| n.id == "cfgread:local/rust-ws@src/lib.rs:env:?")
        .unwrap();
    assert_eq!(dynamic.props["dynamic"], true);
    assert!(dynamic.props.get("key").is_none());
    let at = text.find("env::var(name)").unwrap() as u64;
    assert_eq!(
        evidence_spans(&dynamic.props),
        [(at, at + "env::var(name)".len() as u64)]
    );
    let reads: Vec<&Edge> = x
        .edges
        .iter()
        .filter(|edge| edge.label == "READS_CONFIG")
        .collect();
    assert_eq!(reads.len(), 2);
    let load = x
        .nodes
        .iter()
        .find(|n| n.label == "Symbol" && n.props["name"] == "load")
        .expect("the reading function is a symbol");
    assert!(reads.iter().all(|edge| edge.src == load.id));
    let all = serde_json::to_string(&(&x.nodes, &x.edges)).unwrap();
    assert!(!all.contains("postgres://") && !all.contains("SHADOWED"));
    // Determinism: a second run yields the same facts.
    let again = extract_source(text.as_bytes(), "src/lib.rs", &id()).unwrap();
    assert_eq!(
        all,
        serde_json::to_string(&(&again.nodes, &again.edges)).unwrap()
    );
}

// AC-0597 (ADR-0059 decisions 1, 4): the qualified path, an imported
// function and `var_os` are the same read and share the key's node; a local
// module named `env`, another crate's `env`, a declared `std` module and a
// closure bound as `var` name nothing.
#[test]
fn std_env_var_needs_the_standard_library_path() {
    let reads = r#"use std::env::var;

static REGION: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| std::env::var("REGION").unwrap());

fn a() -> bool {
    var("REGION").is_ok() && std::env::var_os("REGION").is_some()
}
"#;
    let x = extract_source(reads.as_bytes(), "src/lib.rs", &id()).unwrap();
    assert_eq!(
        config_read_ids(&x),
        ["cfgread:local/rust-ws@src/lib.rs:env:REGION"]
    );
    let node = x.nodes.iter().find(|n| n.label == "ConfigRead").unwrap();
    assert_eq!(evidence_spans(&node.props).len(), 3);
    let sources: Vec<&str> = x
        .edges
        .iter()
        .filter(|edge| edge.label == "READS_CONFIG")
        .map(|edge| edge.src.as_str())
        .collect();
    assert_eq!(sources.len(), 2);
    assert!(sources.contains(&"file:local/rust-ws@src/lib.rs"));

    let lookalikes = [
        "mod env { pub fn var(k: &str) -> String { k.into() } }\nfn a() -> String { env::var(\"LOCAL_MOD\") }\n",
        "use other::env;\nfn a() { let _ = env::var(\"OTHER_CRATE\"); }\n",
        "mod std { pub mod env { pub fn var(k: &str) -> String { k.into() } } }\nfn a() -> String { std::env::var(\"OWN_STD\") }\n",
        "use std::env::var;\nfn a() -> String { let var = |k: &str| k.to_string(); var(\"CLOSURE\") }\n",
        "fn a(env: Env) -> String { env.var(\"METHOD\") }\n",
    ];
    for text in lookalikes {
        let x = extract_source(text.as_bytes(), "src/lib.rs", &id()).unwrap();
        assert!(config_read_ids(&x).is_empty(), "{text}");
    }
}

// AC-1008 (T-1008): a file a `#[path]` attribute wires in is the module the
// declaration names, so `super::` in it resolves to the declaring module.
#[test]
fn path_attribute_modules_take_the_declared_module() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "gamma/Cargo.toml",
        "[package]\nname = \"gamma\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "gamma/src/lib.rs",
        "mod curated;\n#[path = \"../shared/util.rs\"]\nmod util;\n",
    );
    write(
        root,
        "gamma/src/curated.rs",
        "pub fn gather() {}\n\n#[cfg(test)]\n// The tests live beside the module.\n#[path = \"curated_tests.rs\"]\nmod tests;\n",
    );
    write(
        root,
        "gamma/src/curated_tests.rs",
        "use super::gather;\nfn reads() { gather(); }\n",
    );
    write(root, "gamma/shared/util.rs", "pub fn shared() {}\n");
    let x = extract_dir(root, &id()).unwrap();
    let module = |rel: &str| {
        x.nodes
            .iter()
            .find(|n| n.id == format!("file:local/rust-ws@{rel}"))
            .map(|n| n.props["module_path"].as_str().unwrap().to_string())
    };
    assert_eq!(
        module("gamma/src/curated_tests.rs").as_deref(),
        Some("gamma::curated::tests")
    );
    assert_eq!(
        module("gamma/shared/util.rs").as_deref(),
        Some("gamma::util")
    );
    assert_eq!(
        module("gamma/src/curated.rs").as_deref(),
        Some("gamma::curated")
    );
    assert!(has_edge(
        &x,
        "CALLS",
        "sym:local/rust-ws@gamma/src/curated_tests.rs#reads",
        "sym:local/rust-ws@gamma/src/curated.rs#gather",
    ));
}

#[test]
fn path_attributes_resolve_from_the_declaring_directory() {
    assert_eq!(
        module_paths::resolve("a/src/x.rs", "y_tests.rs").as_deref(),
        Some("a/src/y_tests.rs")
    );
    assert_eq!(
        module_paths::resolve("a/src/x.rs", "../gen/./z.rs").as_deref(),
        Some("a/gen/z.rs")
    );
    assert_eq!(module_paths::resolve("x.rs", "../z.rs"), None);
    assert_eq!(module_paths::resolve("a/x.rs", "/abs/z.rs"), None);
    let decls = module_paths::declarations(
        b"#[path = \"t.rs\"] mod tests;\nmod inline { #[path = \"i.rs\"] mod nested; }\n#[cfg_attr(test, path = \"c.rs\")] mod c;\n#[path = \"b.rs\"] mod body {}\n",
    );
    assert_eq!(decls, [("tests".to_string(), "t.rs".to_string())]);
    // A cycle keeps the location's module.
    let declared = BTreeMap::from([
        (
            "src/a.rs".to_string(),
            vec![("b".to_string(), "b.rs".to_string())],
        ),
        (
            "src/b.rs".to_string(),
            vec![("a".to_string(), "a.rs".to_string())],
        ),
    ]);
    assert!(module_paths::modules(&declared, &[]).is_empty());
}

fn sql_facts(x: &Extraction) -> Vec<String> {
    let mut facts: Vec<String> = x
        .edges
        .iter()
        .filter(|edge| {
            matches!(edge.label.as_str(), "READS" | "WRITES")
                && (edge.dst.contains("@sql:") || edge.dst.starts_with("gap:sql:"))
        })
        .map(|edge| {
            let dst = if edge.dst.starts_with("gap:sql:") {
                "gap"
            } else {
                edge.dst.as_str()
            };
            format!(
                "{} {} {} {}",
                edge.src,
                edge.label,
                dst,
                edge.props["operation"].as_str().unwrap_or("-")
            )
        })
        .collect();
    facts.sort();
    facts
}

// AC-1015 (#2041): rusqlite and sqlx statements written as literals, a
// `const`, a `let` binding and a sqlx checked macro name the tables they
// read and write; a `format!` table is a Gap; a parameter list, an unbound
// argument and a look-alike `execute` in a file with no SQL crate name
// nothing.
#[test]
fn rust_sql_statements_are_table_reads_and_writes() {
    let text = r#"use rusqlite::{params, Connection};

const LIST: &str = "SELECT id, email FROM users WHERE active = 1";

pub fn save(conn: &Connection, email: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO users (email, active) VALUES (?1, 1)", params![email])?;
    let mut stmt = conn.prepare(LIST)?;
    let sql = "UPDATE accounts SET balance = balance - ?1 WHERE id = ?2";
    conn.execute(sql, params![1, 2])?;
    stmt.execute(params![email])?;
    Ok(())
}

pub fn dynamic(conn: &Connection, table: &str) -> rusqlite::Result<usize> {
    conn.execute(&format!("DELETE FROM {table} WHERE id = ?1"), params![1])
}

pub async fn orders(pool: &sqlx::PgPool) {
    let _ = sqlx::query("SELECT total FROM orders WHERE id = $1").bind(1).fetch_one(pool).await;
    let _ = sqlx::query!("UPDATE orders SET total = $1", 2).execute(pool).await;
}
"#;
    let x = extract_source(text.as_bytes(), "src/lib.rs", &id()).unwrap();
    let facts = sql_facts(&x);
    assert_eq!(
        facts,
        [
            "sym:local/rust-ws@src/lib.rs#dynamic READS gap -",
            "sym:local/rust-ws@src/lib.rs#orders READS data:local/rust-ws@sql:orders -",
            "sym:local/rust-ws@src/lib.rs#orders WRITES data:local/rust-ws@sql:orders update",
            "sym:local/rust-ws@src/lib.rs#save READS data:local/rust-ws@sql:users -",
            "sym:local/rust-ws@src/lib.rs#save WRITES data:local/rust-ws@sql:accounts update",
            "sym:local/rust-ws@src/lib.rs#save WRITES data:local/rust-ws@sql:users insert",
        ],
        "{facts:#?}"
    );
    let users = x
        .nodes
        .iter()
        .find(|node| node.id == "data:local/rust-ws@sql:users")
        .unwrap();
    assert_eq!(users.label, "DataEntity");
    assert_eq!(users.props["framework"], "rusqlite");
    assert_eq!(users.props["language"], "rust");
    let columns: Vec<&str> = users.props["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert_eq!(columns, ["active", "email"]);
    let gap = x
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.id.starts_with("gap:sql:"))
        .unwrap();
    assert_eq!(gap.props["reason"], "SQL statement names a computed table");

    let lookalike = r#"pub struct Runner;
impl Runner {
    pub fn execute(&self, _: &str, _: u8) {}
}
pub fn run(r: &Runner) {
    r.execute("DELETE FROM users", 1);
}
"#;
    let x = extract_source(lookalike.as_bytes(), "src/lib.rs", &id()).unwrap();
    assert!(sql_facts(&x).is_empty());
    assert!(!x.nodes.iter().any(|node| node.id.contains("@sql:")));

    // A crate named only in a comment or a string proves nothing.
    let mentioned =
        format!("// see sqlx::query\nconst DOC: &str = \"rusqlite::Connection\";\n{lookalike}");
    let x = extract_source(mentioned.as_bytes(), "src/lib.rs", &id()).unwrap();
    assert!(sql_facts(&x).is_empty());

    // A turbofish method call is a statement; a `let` in a block that
    // closed before the call is not the binding it reads.
    let scoped = r#"use rusqlite::Connection;
pub fn count(conn: &Connection) -> rusqlite::Result<i64> {
    let sql = "SELECT count(*) FROM outer_table";
    {
        let sql = "SELECT count(*) FROM inner_table";
        let _ = sql;
    }
    conn.query_row::<i64, _, _>(sql, [], |row| row.get(0))
}
"#;
    let x = extract_source(scoped.as_bytes(), "src/lib.rs", &id()).unwrap();
    assert_eq!(
        sql_facts(&x),
        ["sym:local/rust-ws@src/lib.rs#count READS data:local/rust-ws@sql:outer_table -"]
    );
}

// AC-1016 (#2018): `#[test]` and runtime `#[tokio::test]` functions are
// TestCases implemented by their Symbols; an axum `Request::builder()` and
// an actix `TestRequest` with literal targets REQUESTS the endpoint they
// reach with the statuses the test asserts; a computed target and a path no
// endpoint serves are Gaps; a plain function and `#[cfg(test)]` SQL are not
// facts about the system.
#[test]
fn rust_tests_are_test_cases_requesting_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir.path(),
        "src/lib.rs",
        r#"use axum::{routing, Router};
use axum::http::{Method, Request, StatusCode};
use rusqlite::Connection;

async fn create() {}
async fn show() {}

pub fn app() -> Router {
    Router::new()
        .route("/users", routing::post(create))
        .route("/users/:id", routing::get(show))
}

pub fn helper() {}

pub mod local {
    pub struct Request;
    impl Request {
        pub fn get(_: &str) -> Self {
            Request
        }
    }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_a_user() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/users")
            .body(())
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[test]
    fn shows_and_misses() {
        let id = 7;
        let _ = Request::get("http://localhost/users/42?x=1").body(());
        let _ = Request::get(format!("/users/{id}")).body(());
        let _ = Request::builder().uri("/nowhere").body(());
        // A workspace type named `Request` builds no HTTP request.
        let _ = crate::local::Request::get("/users");
        let response = fake();
        assert!(response.status() == 404);
        helper();
    }

    fn fake() {}

    #[test]
    fn seeds() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("INSERT INTO users (email) VALUES (?1)", ["a"]).unwrap();
    }
}
"#,
    );
    // A `*_tests` module's helpers are test code too, though their
    // `#[cfg(test)]` is on the declaring `mod` in another file.
    write(
        dir.path(),
        "src/store_tests.rs",
        r#"pub fn fixture(conn: &rusqlite::Connection) {
    conn.execute("DELETE FROM users", []).unwrap();
}
"#,
    );
    // So is any module an out-of-line `#[cfg(test)] mod fixtures;` declares.
    write(
        dir.path(),
        "src/fixtures.rs",
        r#"pub fn seed(conn: &rusqlite::Connection) {
    conn.execute("UPDATE accounts SET balance = 0", []).unwrap();
}
"#,
    );
    let x = extract_dir(dir.path(), &id()).unwrap();
    let mut tests: Vec<(&str, &str)> = x
        .nodes
        .iter()
        .filter(|n| n.label == "TestCase")
        .map(|n| (n.id.as_str(), n.props["attribute"].as_str().unwrap()))
        .collect();
    tests.sort();
    assert_eq!(
        tests,
        [
            (
                "test:local/rust-ws@src/lib.rs#tests::creates_a_user",
                "tokio::test"
            ),
            ("test:local/rust-ws@src/lib.rs#tests::seeds", "test"),
            (
                "test:local/rust-ws@src/lib.rs#tests::shows_and_misses",
                "test"
            ),
        ]
    );
    assert!(has_edge(
        &x,
        "IMPLEMENTED_BY",
        "test:local/rust-ws@src/lib.rs#tests::shows_and_misses",
        "sym:local/rust-ws@src/lib.rs#tests::shows_and_misses"
    ));
    let mut requests: Vec<String> = x
        .edges
        .iter()
        .filter(|e| e.label == "REQUESTS")
        .map(|e| {
            let dst = if e.dst.starts_with("gap:") {
                "gap"
            } else {
                &e.dst
            };
            let detail = e.props["statuses"]
                .as_array()
                .map(|s| format!("{s:?}"))
                .or_else(|| e.props["reason"].as_str().map(str::to_string))
                .unwrap_or_default();
            format!("{} -> {dst} {detail}", e.src.rsplit('#').next().unwrap())
        })
        .collect();
    requests.sort();
    assert_eq!(
        requests,
        [
            "tests::creates_a_user -> ep:local/rust-ws@POST:/users [Number(201)]",
            "tests::shows_and_misses -> ep:local/rust-ws@GET:/users/:id [Number(404)]",
            "tests::shows_and_misses -> gap no recovered endpoint serves this path and method",
            "tests::shows_and_misses -> gap test request URL is computed",
        ],
        "{requests:#?}"
    );
    let request = x
        .edges
        .iter()
        .find(|e| e.label == "REQUESTS" && e.dst == "ep:local/rust-ws@POST:/users")
        .unwrap();
    assert_eq!(request.props["link"], "handler_assumed");
    // The test's fixture SQL is not a table the system writes.
    assert!(!x.nodes.iter().any(|n| n.id.contains("@sql:")));
}

// AC-1300 (#2161): a `#[path]` declaration that does not wire its file in
// is a Gap on the declaring file, naming the module, the attribute and why.
#[test]
fn unfollowed_path_attributes_are_gaps_on_the_declaring_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "delta/Cargo.toml",
        "[package]\nname = \"delta\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "delta/src/lib.rs",
        concat!(
            "#[path = \"kept.rs\"]\nmod kept;\n",
            "#[path = \"../../../outside.rs\"]\nmod escaping;\n",
            "#[path = \"/abs/x.rs\"]\nmod absolute;\n",
            "#[path = concat!(\"gen\", \".rs\")]\nmod computed;\n",
            "#[cfg_attr(test, path = \"t.rs\")]\nmod conditional;\n",
            "#[path = \"missing.rs\"]\nmod missing;\n",
            "#[path = \"kept.rs\"]\nmod twice;\n",
            "#[path = \"body\"]\nmod with_body {}\n",
            "mod inline {\n    #[path = \"nested.rs\"]\n    mod nested;\n}\n",
        ),
    );
    write(root, "delta/src/kept.rs", "pub fn k() {}\n");
    write(root, "delta/src/a.rs", "#[path = \"b.rs\"]\nmod b;\n");
    write(root, "delta/src/b.rs", "#[path = \"a.rs\"]\nmod a;\n");
    let x = extract_dir(root, &id()).unwrap();
    let mut gaps: Vec<(String, String, String)> = x
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.id.starts_with("gap:module-path:"))
        .map(|n| {
            assert_eq!(n.props["prov"]["confidence_tier"], "Gap");
            let file = n.props["prov"]["evidence"][0]["path"]
                .as_str()
                .unwrap()
                .to_string();
            (
                file,
                n.props["module"].as_str().unwrap().to_string(),
                n.props["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    gaps.sort();
    let want = |f: &str, m: &str, r: &str| (f.to_string(), m.to_string(), r.to_string());
    assert_eq!(
        gaps,
        [
            want("delta/src/a.rs", "b", "cycle"),
            want("delta/src/b.rs", "a", "cycle"),
            want("delta/src/lib.rs", "absolute", "outside_repository"),
            want("delta/src/lib.rs", "computed", "non_literal"),
            want("delta/src/lib.rs", "conditional", "cfg_attr"),
            want("delta/src/lib.rs", "escaping", "outside_repository"),
            want("delta/src/lib.rs", "missing", "file_not_found"),
            want("delta/src/lib.rs", "nested", "inline_module"),
            want("delta/src/lib.rs", "twice", "already_declared"),
            want("delta/src/lib.rs", "with_body", "module_body"),
        ]
    );
    // Each Gap hangs off its declaring file and cites the attribute.
    let escaping = x
        .nodes
        .iter()
        .find(|n| n.id.starts_with("gap:module-path:") && n.props["module"] == "escaping")
        .unwrap();
    assert_eq!(
        escaping.props["attribute"],
        "#[path = \"../../../outside.rs\"]"
    );
    assert!(has_edge(
        &x,
        "DEPENDS_ON",
        "file:local/rust-ws@delta/src/lib.rs",
        &escaping.id
    ));
    let lib = std::fs::read(root.join("delta/src/lib.rs")).unwrap();
    let evidence = &escaping.props["prov"]["evidence"][0];
    let span = evidence["byte_start"].as_u64().unwrap() as usize
        ..evidence["byte_end"].as_u64().unwrap() as usize;
    assert_eq!(&lib[span], b"#[path = \"../../../outside.rs\"]");
    // The followed declaration still takes the declared module.
    let kept = x
        .nodes
        .iter()
        .find(|n| n.id == "file:local/rust-ws@delta/src/kept.rs")
        .unwrap();
    assert_eq!(kept.props["module_path"], "delta::kept");
}

// AC-1300 (#2161): a file whose only path attribute sits under `cfg_attr` is still
// scanned, and a Gap cites the exact declaration even when a name is reused.
#[test]
fn cfg_attr_only_files_and_reused_names_get_their_own_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "eps/Cargo.toml",
        "[package]\nname = \"eps\"\nversion = \"0.1.0\"\n",
    );
    write(root, "eps/src/lib.rs", "mod a;\nmod b;\nmod c;\n");
    write(
        root,
        "eps/src/a.rs",
        "#[cfg_attr(test, path = \"t.rs\")]\nmod t;\n",
    );
    let b = concat!(
        "#[cfg(unix)]\n#[path = \"unix.rs\"]\nmod imp;\n",
        "#[cfg(windows)]\n#[path = \"windows.rs\"]\nmod imp;\n",
    );
    write(root, "eps/src/b.rs", b);
    // A `path` option inside the predicate sets no path: no Gap.
    write(
        root,
        "eps/src/c.rs",
        "#[cfg_attr(any(unix, path = \"generated\"), derive(Debug))]\nmod c_inner {}\n",
    );
    write(root, "eps/src/unix.rs", "pub fn u() {}\n");
    let x = extract_dir(root, &id()).unwrap();
    let gap = |path: &str| {
        x.nodes
            .iter()
            .filter(|n| {
                n.id.starts_with("gap:module-path:")
                    && n.props["prov"]["evidence"][0]["path"] == path
            })
            .collect::<Vec<_>>()
    };
    assert!(gap("eps/src/c.rs").is_empty());
    let a = gap("eps/src/a.rs");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].props["reason"], "cfg_attr");
    let b_gaps = gap("eps/src/b.rs");
    assert_eq!(b_gaps.len(), 1);
    assert_eq!(b_gaps[0].props["reason"], "file_not_found");
    assert_eq!(b_gaps[0].props["attribute"], "#[path = \"windows.rs\"]");
    let start = b_gaps[0].props["prov"]["evidence"][0]["byte_start"]
        .as_u64()
        .unwrap() as usize;
    assert_eq!(start, b.find("#[path = \"windows.rs\"]").unwrap());
}

// AC-1305 (#2163): auth extractors and axum layers are the endpoint's auth.
#[test]
fn auth_extractors_and_axum_layers_are_endpoint_auth() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "src/extract.rs",
        r#"
use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};

pub struct AuthUser { pub id: i64 }

impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = StatusCode;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts.headers.get("authorization").map(|_| AuthUser { id: 1 }).ok_or(StatusCode::UNAUTHORIZED)
    }
}

pub async fn require_admin(req: axum::extract::Request, next: axum::middleware::Next) -> Result<axum::response::Response, StatusCode> {
    if req.headers().contains_key("x-admin") { Ok(next.run(req).await) } else { Err(StatusCode::FORBIDDEN) }
}

// Inspects 401 and 403 without answering with them.
pub async fn log_it(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let res = next.run(req).await;
    if res.status() == StatusCode::UNAUTHORIZED { tracing::warn!("denied"); }
    match res.status() { StatusCode::FORBIDDEN => tracing::warn!("forbidden"), _ => {} }
    res
}
"#,
    );
    write(
        root,
        "src/lib.rs",
        r#"
mod extract;
use axum::{middleware, routing::get, Router};
use axum_extra::TypedHeader;
use headers::{authorization::Bearer, Authorization};
use tower_http::trace::TraceLayer;
use tower_http::validate_request::ValidateRequestHeaderLayer;
use crate::extract::{log_it, require_admin, AuthUser};

async fn me(user: AuthUser) {}
async fn maybe(user: Option<AuthUser>) {}
async fn token(TypedHeader(auth): TypedHeader<Authorization<Bearer>>) {}
async fn open() {}
async fn admin() {}
async fn traced() {}
async fn weird() {}

pub fn app() -> Router {
    Router::new()
        .route("/me", get(me))
        .route("/maybe", get(maybe))
        .route("/token", get(token))
        .route("/admin", get(admin))
        .route_layer(middleware::from_fn(require_admin))
        .route("/basic", get(open))
        .nest("/api", Router::new().route("/inner", get(open)))
        .layer(ValidateRequestHeaderLayer::basic("u", "p"))
        .route("/open", get(open))
        .route("/traced", get(traced))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(log_it))
        .layer((
            tower_http::decompression::RequestDecompressionLayer::new(),
            // A comment between layers.
            tower_http::timeout::TimeoutLayer::new(std::time::Duration::from_secs(5)),
        ))
        .route("/weird", get(weird))
        .layer(make_layer())
}
"#,
    );
    let x = extract_dir(root, &id()).unwrap();
    let auth = |endpoint: &str| {
        x.nodes
            .iter()
            .find(|n| n.label == "Endpoint" && n.id == format!("ep:local/rust-ws@{endpoint}"))
            .unwrap_or_else(|| panic!("{endpoint}"))
            .props["auth"]
            .clone()
    };

    // A repository extractor that rejects with 401 guards its handlers.
    let me = auth("GET:/me");
    assert_eq!(me["state"], "required", "{me}");
    assert_eq!(me["authenticated"], true);
    assert!(
        me["guards"]
            .as_array()
            .unwrap()
            .contains(&"AuthUser".into()),
        "{me}"
    );
    assert!(
        me["basis"]
            .as_array()
            .unwrap()
            .contains(&"extractor AuthUser rejects with 401".into()),
        "{me}"
    );
    assert_eq!(me["prov"]["confidence_tier"], "InferredStrong");
    let paths: BTreeSet<&str> = me["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, BTreeSet::from(["src/extract.rs", "src/lib.rs"]));

    // `Option<AuthUser>` does not reject: only the layers after it count.
    let maybe = auth("GET:/maybe");
    assert_eq!(
        maybe["guards"],
        serde_json::json!([
            "sym:local/rust-ws@src/extract.rs#require_admin",
            "tower_http.ValidateRequestHeaderLayer::basic"
        ]),
        "{maybe}"
    );

    // A library extractor is a Confirmed guard on its own.
    let token = auth("GET:/token");
    assert!(
        token["guards"]
            .as_array()
            .unwrap()
            .contains(&"TypedHeader<Authorization<Bearer>>".into()),
        "{token}"
    );

    // A layer wraps only the routes added before it.
    let basic = auth("GET:/basic");
    assert_eq!(basic["state"], "required");
    assert_eq!(
        basic["guards"],
        serde_json::json!(["tower_http.ValidateRequestHeaderLayer::basic"])
    );
    assert_eq!(basic["prov"]["confidence_tier"], "Confirmed");
    // A layer over a router reaches the routes nested in it.
    assert_eq!(auth("GET:/api/inner")["guards"], basic["guards"]);
    let admin = auth("GET:/admin");
    assert!(
        admin["basis"]
            .as_array()
            .unwrap()
            .contains(&"middleware require_admin responds 403".into()),
        "{admin}"
    );

    // No guard: unknown, never public; non-auth layers are listed, a
    // from_fn without a deny status is a reason, an unreadable layer a Gap.
    let open = auth("GET:/open");
    assert_eq!(open["state"], "unknown", "{open}");
    assert_eq!(
        open["non_auth_middleware"],
        serde_json::json!([
            "TraceLayer::new_for_http()",
            "tower_http::decompression::RequestDecompressionLayer::new()",
            "tower_http::timeout::TimeoutLayer::new(std::time::Duration::from_secs(5))"
        ])
    );
    assert_eq!(
        open["reasons"],
        serde_json::json!([
            "no_deny_status:middleware::from_fn(log_it)",
            "no_guard_recovered",
            "unreadable_middleware:make_layer()"
        ])
    );
    let weird = auth("GET:/weird");
    assert_eq!(
        weird["reasons"],
        serde_json::json!(["no_guard_recovered", "unreadable_middleware:make_layer()"])
    );
    let gap = x
        .nodes
        .iter()
        .find(|n| n.label == "Gap" && n.props["middleware"] == "make_layer()")
        .expect("layer Gap");
    assert!(has_edge(
        &x,
        "DEPENDS_ON",
        "ep:local/rust-ws@GET:/weird",
        &gap.id
    ));
}
