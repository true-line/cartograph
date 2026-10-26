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
