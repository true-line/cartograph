use super::*;
use adapters_fw::events::IdentityExpr;
use core_graph::{Edge, Node};

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn id<'a>() -> SourceId<'a> {
    SourceId {
        repo: "local/demo",
        commit: "workdir",
    }
}

fn node<'a>(nodes: &'a [Node], id: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("missing node {id}"))
}

fn edge<'a>(edges: &'a [Edge], src: &str, dst: &str, label: &str) -> &'a Edge {
    edges
        .iter()
        .find(|edge| edge.src == src && edge.dst == dst && edge.label == label)
        .unwrap_or_else(|| panic!("missing edge {src} -{label}-> {dst}"))
}

#[test]
fn classes_methods_and_same_class_calls_carry_confirmed_provenance() {
    let source = br#"package com.demo;

public class Greeter {
    public String hello() {
        return greet();
    }

    private String greet() {
        return this.suffix();
    }

    private String suffix() {
        return "!";
    }
}
"#;
    let path = "src/main/java/com/demo/Greeter.java";
    let out = extract_source(source, path, &id()).unwrap();

    let class_sym = format!("sym:local/demo@{path}#Greeter");
    let hello = format!("sym:local/demo@{path}#Greeter.hello()");
    let greet = format!("sym:local/demo@{path}#Greeter.greet()");
    let suffix = format!("sym:local/demo@{path}#Greeter.suffix()");
    assert_eq!(node(&out.nodes, &class_sym).props["kind"], "class");
    assert_eq!(node(&out.nodes, &hello).props["kind"], "method");

    // Unqualified and `this.` calls resolve within the class.
    edge(&out.edges, &hello, &greet, "CALLS");
    edge(&out.edges, &greet, &suffix, "CALLS");
    edge(
        &out.edges,
        &hello,
        &format!("file:local/demo@{path}"),
        "DEFINED_IN",
    );

    // Every fact is Confirmed T0 with a real span into this file.
    for fact in out
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(out.edges.iter().map(|e| &e.props))
    {
        let prov = &fact["prov"];
        assert_eq!(prov["tier"], "Deterministic");
        assert_eq!(prov["confidence_tier"], "Confirmed");
        assert_eq!(prov["extractor_id"], "t0.adapter-java");
        let end = prov["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end <= source.len() as u64 && end > 0);
    }
}

#[test]
fn same_class_overloads_get_distinct_ids_and_calls_resolve_by_arity() {
    let source = br#"package com.demo;

public class Store {
    String label;

    public void save() {}
    public void save(String name) {}
    public void save(int id) {}
    public void save(String name, int retries) {}

    void run() {
        save();
        save(this.label);
        save("a", 1);
        save(9, 9, 9);
    }
}
"#;
    let path = "src/main/java/com/demo/Store.java";
    let out = extract_source(source, path, &id()).unwrap();

    let run = format!("sym:local/demo@{path}#Store.run()");
    let no_arg = format!("sym:local/demo@{path}#Store.save()");
    let one_string = format!("sym:local/demo@{path}#Store.save(String)");
    let one_int = format!("sym:local/demo@{path}#Store.save(int)");
    let two = format!("sym:local/demo@{path}#Store.save(String,int)");

    // Each overload is its own Confirmed Symbol, not one collapsed fact.
    for overload in [&no_arg, &one_string, &one_int, &two] {
        assert_eq!(node(&out.nodes, overload).props["kind"], "method");
    }

    // A call whose arity matches exactly one overload resolves to it.
    edge(&out.edges, &run, &no_arg, "CALLS");
    edge(&out.edges, &run, &two, "CALLS");

    // `save(this.label)` (arity 1) matches both `save(String)` and
    // `save(int)`, and a field access proves no argument kind, so this is
    // ambiguous, not a guess.
    let gaps: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 2);
    let ambiguous = gaps
        .iter()
        .find(|g| {
            g.props["reason"]
                == "ambiguous overload: multiple local methods accept this argument count"
        })
        .expect("ambiguous-arity call gaps");
    edge(&out.edges, &run, &ambiguous.id, "CALLS");

    // `save(9, 9, 9)` (arity 3) matches no local overload.
    let unmatched = gaps
        .iter()
        .find(|g| g.props["reason"] == "no local overload accepts this call's argument count")
        .expect("unmatched-arity call gaps");
    edge(&out.edges, &run, &unmatched.id, "CALLS");

    // No CALLS edge lands on `save(String)` or `save(int)` directly — the
    // ambiguous call never silently guesses one.
    assert!(
        !out.edges
            .iter()
            .any(|e| e.dst == one_string && e.label == "CALLS")
    );
    assert!(
        !out.edges
            .iter()
            .any(|e| e.dst == one_int && e.label == "CALLS")
    );
}

#[test]
fn imported_class_calls_resolve_across_files_and_missing_targets_gap() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.util.Store;
import com.demo.util.Missing;
import org.external.Lib;

public class App {
    void run() {
        Store.save();
        Missing.run();
        Lib.外部();
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/java/com/demo/util/Store.java",
        r#"package com.demo.util;

public class Store {
    public static void save() {}
}
"#,
    );
    let (out, stats) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    assert_eq!(stats.recomputed_files, 2);

    let src = "sym:local/demo@app/src/main/java/com/demo/App.java#App.run()";
    let store_save = "sym:local/demo@app/src/main/java/com/demo/util/Store.java#Store.save()";
    // Import-proven cross-file call resolves to the declaring file's symbol.
    let resolved = edge(&out.edges, src, store_save, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");

    // A declared-package import with no such class fails closed to a Gap.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("missing-target import gaps");
    assert_eq!(gap.props["reason"], "unresolved Java import target");
    assert_eq!(gap.props["callee"], "Missing.run");
    edge(&out.edges, src, &gap.id, "CALLS");

    // A call on a foreign-package import asserts nothing — no CALLS edge,
    // no extra Gap (the import itself is a Confirmed external, AC-0207).
    assert_eq!(
        out.nodes.iter().filter(|node| node.label == "Gap").count(),
        1
    );
}

#[test]
fn imported_class_overloads_resolve_cross_file_by_arity() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.util.Store;

public class App {
    void run() {
        Store.save();
        Store.save("a", 1);
        Store.save(9, 9, 9);
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/java/com/demo/util/Store.java",
        r#"package com.demo.util;

public class Store {
    public static void save() {}
    public static void save(String name, int retries) {}
}
"#,
    );
    let (out, stats) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    assert_eq!(stats.recomputed_files, 2);

    let src = "sym:local/demo@app/src/main/java/com/demo/App.java#App.run()";
    let no_arg = "sym:local/demo@app/src/main/java/com/demo/util/Store.java#Store.save()";
    let two = "sym:local/demo@app/src/main/java/com/demo/util/Store.java#Store.save(String,int)";

    // Each cross-file call resolves to the overload whose arity it matches.
    edge(&out.edges, src, no_arg, "CALLS");
    edge(&out.edges, src, two, "CALLS");

    // `save(9, 9, 9)` (arity 3) matches no overload of the imported type.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("unmatched cross-file overload gap");
    assert_eq!(
        gap.props["reason"],
        "no unique overload proves this call's argument count"
    );
    edge(&out.edges, src, &gap.id, "CALLS");
    assert_eq!(
        out.nodes.iter().filter(|node| node.label == "Gap").count(),
        1
    );
}

// AC-0433: literal, null and lambda arguments rule out same-arity overloads
// they cannot bind to; a unique survivor resolves, anything else stays a Gap.
#[test]
fn same_arity_overloads_narrow_by_literal_argument_kinds() {
    let source = br#"package com.demo;

import java.util.function.Supplier;

public class Waits<T> {
    Supplier<String> details;

    void await(Runnable condition, long maxWaitMs, String details) {}
    void await(Runnable condition, long maxWaitMs, Supplier<String> details) {}
    void put(T value) {}
    void put(int[] values) {}
    void log(String message) {}
    void log(Object message) {}
    void retry(Integer attempts) {}
    void retry(Object attempts) {}

    void run() {
        await(() -> {}, 10 * 1000, "Failed to poll.");
        await(() -> {}, 10 * 1000, () -> "lazy");
        await(() -> {}, 5, this.details);
        put("x");
        log("both apply");
        retry(() -> 1);
    }
}

// Shadows `java.lang.Integer` in this file: a lambda can bind to it.
interface Integer {
    int get();
}
"#;
    let path = "src/main/java/com/demo/Waits.java";
    let out = extract_source(source, path, &id()).unwrap();
    let run = format!("sym:local/demo@{path}#Waits.run()");
    let eager = format!("sym:local/demo@{path}#Waits.await(Runnable,long,String)");
    let lazy = format!("sym:local/demo@{path}#Waits.await(Runnable,long,Supplier<String>)");
    let custom = format!("sym:local/demo@{path}#Waits.retry(Integer)");
    let put = format!("sym:local/demo@{path}#Waits.put(T)");

    // A string literal cannot bind to `Supplier`, and binds to the class
    // type variable `T` but not to an array.
    for dst in [&eager, &put] {
        let resolved = edge(&out.edges, &run, dst, "CALLS");
        assert_eq!(
            resolved.props["overload_resolution"],
            "literal-argument-kinds"
        );
    }

    // A lambda is never excluded by a class name: here `Integer` is a
    // functional interface, so `retry(() -> 1)` keeps both overloads, and the
    // lazy `await` keeps its `String` overload too. A field access proves
    // nothing, and `log(String)`/`log(Object)` both accept a string literal:
    // no most-specific guess. Four Gaps.
    let gaps: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 4);
    for gap in &gaps {
        assert_eq!(
            gap.props["reason"],
            "ambiguous overload: multiple local methods accept this argument count"
        );
        edge(&out.edges, &run, &gap.id, "CALLS");
    }
    for overload in ["log(String)", "log(Object)", "retry(Object)"] {
        let dst = format!("sym:local/demo@{path}#Waits.{overload}");
        assert!(!out.edges.iter().any(|e| e.dst == dst && e.label == "CALLS"));
    }
    for dst in [&lazy, &custom] {
        assert!(
            !out.edges
                .iter()
                .any(|e| &e.dst == dst && e.label == "CALLS")
        );
    }
}

// AC-0433: the same narrowing applies to import-proven cross-file calls.
#[test]
fn imported_same_arity_overloads_narrow_by_literal_argument_kinds() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.util.Store;

public class App {
    void run() {
        Store.save("a", 1);
        Store.save(null, 1);
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/java/com/demo/util/Store.java",
        r#"package com.demo.util;

import java.util.function.Supplier;

public class Store {
    public static void save(String name, int retries) {}
    public static void save(Supplier<String> name, int retries) {}
}
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    let src = "sym:local/demo@app/src/main/java/com/demo/App.java#App.run()";
    let eager = "sym:local/demo@app/src/main/java/com/demo/util/Store.java#Store.save(String,int)";
    let resolved = edge(&out.edges, src, eager, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");
    assert_eq!(
        resolved.props["overload_resolution"],
        "literal-argument-kinds"
    );

    // `null` binds to either reference type: still a Gap.
    let gaps: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        gaps[0].props["reason"],
        "no unique overload proves this call's argument count"
    );
    edge(&out.edges, src, &gaps[0].id, "CALLS");
}

#[test]
fn same_named_types_in_different_packages_keep_separate_overload_sets() {
    // Two packages each declare a `Store` with a zero-arg `save()`. The
    // import resolves uniquely to `com.demo.a.Store` (#170's duplicate-FQN
    // rule), so the overload index must scope its candidates to that exact
    // package — not merge them with `com.demo.b.Store`'s same-named,
    // same-arity method just because both types share the simple name
    // "Store" (#435 review).
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.a.Store;

public class App {
    void run() {
        Store.save();
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/java/com/demo/a/Store.java",
        r#"package com.demo.a;

public class Store {
    public static void save() {}
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/java/com/demo/b/Store.java",
        r#"package com.demo.b;

public class Store {
    public static void save() {}
}
"#,
    );
    let (out, stats) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    assert_eq!(stats.recomputed_files, 3);

    let src = "sym:local/demo@app/src/main/java/com/demo/App.java#App.run()";
    let a_save = "sym:local/demo@app/src/main/java/com/demo/a/Store.java#Store.save()";
    let b_save = "sym:local/demo@app/src/main/java/com/demo/b/Store.java#Store.save()";

    // Resolves uniquely to the imported package's method...
    edge(&out.edges, src, a_save, "CALLS");
    // ...never to the unrelated same-named type in the other package, and
    // the call is not left ambiguous either (exactly one CALLS edge from
    // this call site, no overload Gap).
    assert!(
        !out.edges
            .iter()
            .any(|edge| edge.src == src && edge.dst == b_save),
        "call must not resolve into an unrelated package's same-named type"
    );
    assert_eq!(
        out.edges
            .iter()
            .filter(|edge| edge.src == src && edge.label == "CALLS")
            .count(),
        1,
        "call must resolve unambiguously, not fail closed to a spurious overload Gap"
    );
    assert!(
        !out.nodes.iter().any(|node| node.label == "Gap"),
        "the call is resolvable and must not produce an overload Gap"
    );
}

#[test]
fn spring_endpoints_compose_class_and_method_paths_and_fail_closed_without_import() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/web/UserController.java",
        r#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/users")
public class UserController {
    @GetMapping("/{id}")
    public String get() { return "u"; }

    @PostMapping
    public String create() { return "c"; }
}
"#,
    );
    // Lookalike annotations with no Spring import prove nothing.
    write(
        dir.path(),
        "src/main/java/com/demo/web/FakeController.java",
        r#"package com.demo.web;

import com.other.web.RestController;
import com.other.web.GetMapping;

@RestController
public class FakeController {
    @GetMapping("/nope")
    public String get() { return "n"; }
}
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let get = node(&out.nodes, "ep:local/demo@GET:/api/users/{id}");
    assert_eq!(get.props["framework"], "spring");
    assert_eq!(get.props["path"], "/api/users/{id}");
    let post = node(&out.nodes, "ep:local/demo@POST:/api/users");
    assert_eq!(post.props["method"], "POST");
    edge(
        &out.edges,
        "ep:local/demo@GET:/api/users/{id}",
        "sym:local/demo@src/main/java/com/demo/web/UserController.java#UserController.get()",
        "HANDLES",
    );

    // The unproven controller contributed no endpoints at all.
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        2
    );
}

#[test]
fn wildcard_proof_is_per_annotation_package_not_per_vendor() {
    // #170 review: a wildcard of one Spring package must not prove an
    // annotation living in another. Here `stereotype.*` proves @Controller,
    // but the unimported (custom/lookalike) @GetMapping stays unproven —
    // no endpoint is asserted.
    let source = br#"package com.demo.web;

import org.springframework.stereotype.*;

@Controller
public class HomeController {
    @GetMapping("/home")
    public String home() { return "h"; }
}
"#;
    let out = extract_source(
        source,
        "src/main/java/com/demo/web/HomeController.java",
        &id(),
    )
    .unwrap();
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        0
    );

    // And a named import from the wrong Spring package proves nothing either.
    let wrong = br#"package com.demo.web;

import org.springframework.stereotype.RestController;
import org.springframework.stereotype.GetMapping;

@RestController
public class WrongController {
    @GetMapping("/wrong")
    public String wrong() { return "w"; }
}
"#;
    let out = extract_source(
        wrong,
        "src/main/java/com/demo/web/WrongController.java",
        &id(),
    )
    .unwrap();
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        0
    );
}

#[test]
fn duplicate_type_declarations_are_ambiguous_and_fail_closed() {
    // #170 review: the same FQN declared in two source roots (main + test)
    // must never resolve a Confirmed call to whichever file sorts last —
    // the import is ambiguous, so the call fails closed to a Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.util.Store;

public class App {
    void run() {
        Store.save();
    }
}
"#,
    );
    for root in ["src/main/java", "src/test/java"] {
        write(
            dir.path(),
            &format!("{root}/com/demo/util/Store.java"),
            r#"package com.demo.util;

public class Store {
    public static void save() {}
}
"#,
        );
    }
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let src = "sym:local/demo@src/main/java/com/demo/App.java#App.run()";
    // No Confirmed CALLS edge to either declaration…
    assert!(!out.edges.iter().any(|edge| {
        edge.label == "CALLS" && edge.src == src && edge.dst.contains("Store.save")
    }));
    // …only an explicit Gap.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("ambiguous duplicate declarations gap");
    assert_eq!(gap.props["callee"], "Store.save");
    edge(&out.edges, src, &gap.id, "CALLS");
}

#[test]
fn named_spring_imports_prove_mappings_too() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.bind.annotation.DeleteMapping;

@RestController
public class AdminController {
    @DeleteMapping(path = "/admin/cache")
    public void purge() {}
}
"#;
    let path = "src/main/java/com/demo/web/AdminController.java";
    let out = extract_source(source, path, &id()).unwrap();
    let endpoint = node(&out.nodes, "ep:local/demo@DELETE:/admin/cache");
    assert_eq!(endpoint.props["method"], "DELETE");
    assert_eq!(
        endpoint.props["handler_sym"],
        format!("sym:{}@{path}#AdminController.purge()", "local/demo")
    );
}

/// AC-0192 (#234): the petclinic shape. Array-initializer paths are Spring's
/// documented multi-path form; each literal is a real route, and a single
/// element must not collapse to `/`.
#[test]
fn array_initializer_mapping_paths_are_literal_routes_not_the_root() {
    let source = br#"package com.demo.web;

import org.springframework.stereotype.Controller;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;

@Controller
@RequestMapping({ "/clinic" })
public class VetController {
    @GetMapping({ "/vets" })
    public String showResourcesVetList() { return "v"; }

    @GetMapping(value = { "/vets.html", "/vets/all" })
    public String showVetList() { return "l"; }

    @GetMapping({})
    public String home() { return "h"; }
}
"#;
    let out = extract_source(source, "src/VetController.java", &id()).unwrap();
    let mut routes: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.props["path"].as_str().unwrap())
        .collect();
    routes.sort_unstable();
    // `{}` is an empty path list — Spring's default, the class base itself.
    assert_eq!(
        routes,
        [
            "/clinic",
            "/clinic/vets",
            "/clinic/vets.html",
            "/clinic/vets/all"
        ]
    );
    edge(
        &out.edges,
        "ep:local/demo@GET:/clinic/vets",
        "sym:local/demo@src/VetController.java#VetController.showResourcesVetList()",
        "HANDLES",
    );
    for route in ["/clinic/vets.html", "/clinic/vets/all"] {
        edge(
            &out.edges,
            &format!("ep:local/demo@GET:{route}"),
            "sym:local/demo@src/VetController.java#VetController.showVetList()",
            "HANDLES",
        );
    }
    // A composed route cites the class base and the method mapping, on both
    // the Endpoint and its HANDLES edge.
    let endpoint = out
        .nodes
        .iter()
        .find(|node| node.id == "ep:local/demo@GET:/clinic/vets")
        .unwrap();
    let handles = out
        .edges
        .iter()
        .find(|e| e.src == endpoint.id && e.label == "HANDLES")
        .unwrap();
    for prov in [&endpoint.props["prov"], &handles.props["prov"]] {
        assert_eq!(
            cited(source, prov),
            [
                "@RequestMapping({ \"/clinic\" })",
                "@GetMapping({ \"/vets\" })"
            ]
        );
    }
}

/// Source text of every evidence span in `prov`.
fn cited<'a>(source: &'a [u8], prov: &serde_json::Value) -> Vec<&'a str> {
    prov["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|span| {
            let start = span["byte_start"].as_u64().unwrap() as usize;
            let end = span["byte_end"].as_u64().unwrap() as usize;
            std::str::from_utf8(&source[start..end]).unwrap()
        })
        .collect()
}

/// AC-0192 (#234): a mapping whose path is present but not a provable
/// literal is a runtime identity. It becomes an explicit Gap bound to its
/// handler — never a Confirmed endpoint at the `/` default.
#[test]
fn non_literal_mapping_paths_fail_closed_to_route_gaps() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
public class DynamicController {
    @GetMapping(Paths.VETS)
    public String constant() { return "c"; }

    @GetMapping("/api" + "/owners")
    public String concatenated() { return "o"; }

    @PostMapping(path = { "/ok", Paths.OTHER })
    public String mixed() { return "m"; }

    @GetMapping
    public String root() { return "r"; }
}
"#;
    let out = extract_source(source, "src/DynamicController.java", &id()).unwrap();
    let endpoints: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    // Only the path-less mapping is provable (Spring's documented default).
    assert_eq!(endpoints, ["ep:local/demo@GET:/"]);

    let gaps: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:route:"))
        .collect();
    assert_eq!(gaps.len(), 3);
    for gap in &gaps {
        assert_eq!(gap.label, "Gap");
        assert_eq!(gap.props["reason"], "dynamic Spring mapping path");
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    }
    for handler in ["constant", "concatenated", "mixed"] {
        let handler =
            format!("sym:local/demo@src/DynamicController.java#DynamicController.{handler}()");
        assert!(
            out.edges.iter().any(|edge| edge.label == "HANDLES"
                && edge.dst == handler
                && edge.src.starts_with("gap:route:")),
            "{handler} must be handled by a route Gap"
        );
    }
}

/// AC-0192 (#234): a non-literal class-level base poisons every mapping
/// under it — composing a literal tail onto an unknown base is still a guess.
#[test]
fn non_literal_class_base_fails_every_mapping_closed() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping(Api.BASE)
public class BasedController {
    @GetMapping("/items")
    public String items() { return "i"; }
}
"#;
    let out = extract_source(source, "src/BasedController.java", &id()).unwrap();
    assert!(out.nodes.iter().all(|node| node.label != "Endpoint"));
    let gaps: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:route:"))
        .collect();
    assert_eq!(gaps.len(), 1);
    // The evidence must point at the expression that made the route
    // unprovable (the class base), not only at the literal method mapping.
    assert_eq!(
        cited(source, &gaps[0].props["prov"]),
        ["@RequestMapping(Api.BASE)", "@GetMapping(\"/items\")"]
    );
}

/// AC-0192 (#432 review): an escaped literal's source spelling is not its
/// runtime value, so it must not become a Confirmed route.
#[test]
fn escaped_literal_mapping_paths_fail_closed() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
public class EscapedController {
    @GetMapping({ "/foo\u002Dbar" })
    public String unicode() { return "u"; }

    @GetMapping("/tab\there")
    public String tab() { return "t"; }
}
"#;
    let out = extract_source(source, "src/EscapedController.java", &id()).unwrap();
    assert!(out.nodes.iter().all(|node| node.label != "Endpoint"));
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.id.starts_with("gap:route:"))
            .count(),
        2
    );
}

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("src/App.java", ""),
        ("gen/Api.java", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_java_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["src/App.java"]);
}

/// Sorted `METHOD path` of every Endpoint in `out`.
fn endpoint_routes(out: &Extraction) -> Vec<String> {
    let mut routes: Vec<String> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            format!(
                "{} {}",
                node.props["method"].as_str().unwrap(),
                node.props["path"].as_str().unwrap()
            )
        })
        .collect();
    routes.sort_unstable();
    routes
}

// AC-0206 (#445): a trailing slash spelled in the source survives
// composition — Spring 6 matches `/a` and `/a/` as distinct routes — while
// segments still meet at exactly one `/`.
#[test]
fn spring_routes_keep_literal_trailing_slashes() {
    let slashed_base = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/")
public class ApiController {
    @GetMapping
    public String root() { return "r"; }

    @PostMapping("/items")
    public String create() { return "c"; }

    @PutMapping({ "/x", "/x/" })
    public String put() { return "p"; }
}
"#;
    let out = extract_source(slashed_base, "src/ApiController.java", &id()).unwrap();
    assert_eq!(
        endpoint_routes(&out),
        ["GET /api/", "POST /api/items", "PUT /api/x", "PUT /api/x/"]
    );
    edge(
        &out.edges,
        "ep:local/demo@GET:/api/",
        "sym:local/demo@src/ApiController.java#ApiController.root()",
        "HANDLES",
    );

    let slashed_tail = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/v1")
public class V1Controller {
    @GetMapping("/")
    public String root() { return "r"; }

    @GetMapping("items/")
    public String items() { return "i"; }
}
"#;
    let out = extract_source(slashed_tail, "src/V1Controller.java", &id()).unwrap();
    assert_eq!(endpoint_routes(&out), ["GET /v1/", "GET /v1/items/"]);
    assert!(!out.nodes.iter().any(|node| node.id.contains("//")));

    // A run of trailing slashes keeps exactly one.
    let doubled = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/v2//")
public class V2Controller {
    @GetMapping
    public String root() { return "r"; }

    @GetMapping("items//")
    public String items() { return "i"; }
}
"#;
    let out = extract_source(doubled, "src/V2Controller.java", &id()).unwrap();
    assert_eq!(endpoint_routes(&out), ["GET /v2/", "GET /v2/items/"]);
}

fn prov_of(node: &Node) -> Provenance {
    serde_json::from_value(node.props["prov"].clone())
        .unwrap_or_else(|_| panic!("{} carries no provenance", node.id))
}

#[test]
fn import_targets_carry_provenance_and_only_proven_externals_confirm() {
    // AC-0207 (#237): every closed-over endpoint cites the import that named
    // it; only a package the repository provably does not declare is a
    // Confirmed external boundary, in-repo types resolve to their File, and
    // an in-system target that does not resolve stays an explicit Gap.
    let dir = tempfile::tempdir().unwrap();
    let app = r#"package com.demo;

import com.demo.util.Store;
import com.demo.util.Missing;
import com.demo.util.*;
import com.demo.kt.Widget;
import com.Stray;
import jakarta.persistence.Entity;
import static org.junit.Assert.assertEquals;

public class App {}
"#;
    write(dir.path(), "src/main/java/com/demo/App.java", app);
    write(
        dir.path(),
        "src/main/java/com/demo/util/Store.java",
        "package com.demo.util;\n\npublic class Store {}\n",
    );
    // A Kotlin package in a mixed JVM tree is the repository's own.
    write(
        dir.path(),
        "src/main/kotlin/com/demo/kt/Widget.kt",
        "/* header */\n@file:JvmName(\"W\")\npackage com.demo.kt\n\nclass Widget\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let app_file = "file:local/demo@src/main/java/com/demo/App.java";

    // The in-repo type import resolves to the declaring File, Confirmed.
    let store = edge(
        &out.edges,
        app_file,
        "file:local/demo@src/main/java/com/demo/util/Store.java",
        "IMPORTS",
    );
    assert_eq!(store.props["resolution"], "import-proven");
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id == "mod:com.demo.util.Store")
    );

    let expect = |id: &str, boundary: &str, confidence: ConfidenceTier, statement: &str| {
        let node = node(&out.nodes, id);
        assert_eq!(node.label, "Module", "{id}");
        assert_eq!(node.props["placeholder"], true, "{id}");
        assert_eq!(node.props["boundary"], boundary, "{id}");
        assert!(node.props["reason"].as_str().is_some_and(|r| !r.is_empty()));
        let prov = prov_of(node);
        assert_eq!(prov.confidence_tier, confidence, "{id}");
        assert_eq!(prov.extractor_id, EXTRACTOR_ID);
        // The evidence is the exact import statement that named it.
        let span = &prov.evidence[0];
        assert_eq!(span.path, "src/main/java/com/demo/App.java");
        assert_eq!(
            &app[span.byte_start as usize..span.byte_end as usize],
            statement,
            "{id}"
        );
    };
    // #464 (ADR-0033): a proven-external import keys on its package, not the
    // imported type/member.
    expect(
        "mod:jakarta.persistence",
        "external",
        ConfidenceTier::Confirmed,
        "import jakarta.persistence.Entity;",
    );
    expect(
        "mod:org.junit",
        "external",
        ConfidenceTier::Confirmed,
        "import static org.junit.Assert.assertEquals;",
    );
    expect(
        "mod:com.demo.util",
        "internal",
        ConfidenceTier::Confirmed,
        "import com.demo.util.*;",
    );
    // Declared package, no such type: an unresolved in-system hop.
    expect(
        "mod:com.demo.util.Missing",
        "unresolved",
        ConfidenceTier::Gap,
        "import com.demo.util.Missing;",
    );
    // The Kotlin-declared package is in-system, never external.
    expect(
        "mod:com.demo.kt.Widget",
        "unresolved",
        ConfidenceTier::Gap,
        "import com.demo.kt.Widget;",
    );
    // An enclosing package of a declared one cannot be proven external.
    expect(
        "mod:com.Stray",
        "unresolved",
        ConfidenceTier::Gap,
        "import com.Stray;",
    );
    // No fact of any kind is left without valid provenance.
    for node in &out.nodes {
        prov_of(node).validate().unwrap();
    }
}

#[test]
fn foreign_headers_the_scan_cannot_read_never_confirm_externals() {
    // AC-0207 (#237 review): a multiline Kotlin file annotation does not
    // hide its package, and a header the scan cannot parse leaves every
    // undeclared import an explicit Gap rather than Confirmed external.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/App.java",
        "package com.demo;\n\nimport com.other.kt.Widget;\nimport jakarta.persistence.Entity;\n\npublic class App {}\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/other/kt/Widget.kt",
        "@file:JvmName(\n    \"Widgets\"\n)\n\npackage com.other.kt\n\nclass Widget\n",
    );
    let boundary = |out: &Extraction, id: &str| {
        let node = node(&out.nodes, id);
        (
            node.props["boundary"].as_str().unwrap().to_string(),
            prov_of(node).confidence_tier,
        )
    };
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary(&out, "mod:com.other.kt.Widget"),
        ("unresolved".into(), ConfidenceTier::Gap)
    );
    assert_eq!(
        boundary(&out, "mod:jakarta.persistence"),
        ("external".into(), ConfidenceTier::Confirmed)
    );

    write(
        dir.path(),
        "src/main/kotlin/com/other/Broken.kt",
        "@file:JvmName(\n    \"never closed\"\n\npackage com.hidden\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary(&out, "mod:jakarta.persistence.Entity"),
        ("unresolved".into(), ConfidenceTier::Gap)
    );
}

#[test]
fn only_a_complete_import_target_retargets_to_its_file() {
    // AC-0207 (#237 review): `a.Store.Missing` is not proven by `a.Store`;
    // a declared member is. An unproven member import stays a Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/a/App.java",
        "package a;\n\nimport static a.Store.save;\nimport static a.Store.missing;\nimport a.Store.Missing;\n\npublic class App {}\n",
    );
    write(
        dir.path(),
        "src/a/Store.java",
        "package a;\n\npublic class Store {\n    public static void save() {}\n}\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let store = "file:local/demo@src/a/Store.java";
    let into_store = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS" && edge.dst == store)
        .count();
    assert_eq!(into_store, 1, "only the declared member retargets");
    for unproven in ["mod:a.Store.missing", "mod:a.Store.Missing"] {
        let node = node(&out.nodes, unproven);
        assert_eq!(node.props["boundary"], "unresolved", "{unproven}");
        assert_eq!(prov_of(node).confidence_tier, ConfidenceTier::Gap);
    }
}

#[test]
fn external_imports_from_one_package_share_its_module_id_and_keep_their_specifier() {
    // AC-0226 (#464, ADR-0033): two types proven external from the same
    // package key their `IMPORTS` edge on that shared package id rather than
    // the imported type, and each edge still names exactly what it imported.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/App.java",
        "package com.demo;\n\nimport jakarta.persistence.Entity;\nimport jakarta.persistence.Table;\n\npublic class App {}\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let app_file = "file:local/demo@src/main/java/com/demo/App.java";
    let module_edges: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS" && edge.src == app_file)
        .collect();
    assert_eq!(module_edges.len(), 2);
    assert!(
        module_edges
            .iter()
            .all(|edge| edge.dst == "mod:jakarta.persistence"),
        "{module_edges:?}"
    );
    let specifiers: BTreeSet<&str> = module_edges
        .iter()
        .map(|edge| edge.props["specifier"].as_str().unwrap())
        .collect();
    assert_eq!(
        specifiers,
        BTreeSet::from(["jakarta.persistence.Entity", "jakarta.persistence.Table"])
    );
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.id == "mod:jakarta.persistence")
            .count(),
        1,
        "one shared Module node, not one per imported type"
    );
}

/// A small petclinic-like JPA project: Owner/Pet in one package, Visit and
/// the repositories in others, so relation targets and repository bindings
/// resolve through named imports, the file's package and wildcards.
fn petclinic(dir: &Path) {
    write(
        dir,
        "src/main/java/com/demo/owner/Owner.java",
        r#"package com.demo.owner;

import jakarta.persistence.*;
import java.util.List;

@Entity
@Table(name = "owners", schema = "clinic")
public class Owner {
    private static final long serialVersionUID = 1L;

    @Id
    @GeneratedValue
    private Integer id;

    @Column(name = "last_name")
    private String lastName;

    @Transient
    private String scratch;

    @OneToMany(cascade = CascadeType.ALL, fetch = FetchType.EAGER)
    @JoinColumn(name = "owner_id")
    private List<Pet> pets;
}
"#,
    );
    write(
        dir,
        "src/main/java/com/demo/owner/Pet.java",
        r#"package com.demo.owner;

import com.demo.visit.Visit;
import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.JoinColumn;
import jakarta.persistence.ManyToOne;
import jakarta.persistence.OneToMany;
import jakarta.persistence.Table;
import java.util.Set;

@Entity
@Table(name = "pets")
public class Pet {
    @Id
    private Integer id;

    private String name;

    @ManyToOne
    @JoinColumn(name = "owner_id")
    private Owner owner;

    @OneToMany(mappedBy = "pet")
    private Set<Visit> visits;
}
"#,
    );
    write(
        dir,
        "src/main/java/com/demo/visit/Visit.java",
        r#"package com.demo.visit;

import com.demo.owner.Pet;
import jakarta.persistence.*;

@Entity
public class Visit {
    @Id
    private Integer id;

    @ManyToOne
    @JoinColumn(name = "pet_id")
    private Pet pet;
}
"#,
    );
    write(
        dir,
        "src/main/java/com/demo/owner/OwnerRepository.java",
        r#"package com.demo.owner;

import java.util.Optional;
import org.springframework.data.repository.Repository;

public interface OwnerRepository extends Repository<Owner, Integer> {
    Optional<Owner> findById(Integer id);
    void save(Owner owner);
}
"#,
    );
    write(
        dir,
        "src/main/java/com/demo/pet/PetRepository.java",
        r#"package com.demo.pet;

import com.demo.owner.Pet;
import org.springframework.data.jpa.repository.JpaRepository;

public interface PetRepository extends JpaRepository<Pet, Integer> {
}
"#,
    );
    write(
        dir,
        "src/main/java/com/demo/owner/OwnerController.java",
        r#"package com.demo.owner;

import com.demo.pet.PetRepository;
import java.util.Map;

public class OwnerController {
    private final OwnerRepository owners;
    private final PetRepository pets;
    private final Map<String, String> cache;

    public OwnerController(OwnerRepository owners, PetRepository pets, Map<String, String> cache) {
        this.owners = owners;
        this.pets = pets;
        this.cache = cache;
    }

    public Owner show(Integer id) {
        cache.get("k");
        return this.owners.findById(id).orElseThrow();
    }

    public void create(Owner owner) {
        owners.save(owner);
    }

    public void forget(PetRepository repo, Integer id) {
        repo.deleteById(id);
        this.pets.findAll();
    }
}
"#,
    );
}

fn extract_petclinic() -> (tempfile::TempDir, Extraction) {
    let dir = tempfile::tempdir().unwrap();
    petclinic(dir.path());
    let out = extract_dir(dir.path(), &id()).unwrap();
    (dir, out)
}

/// AC-0284 (#239): import-proven JPA classes become DataEntity nodes with
/// their entity name, table (explicit, else the entity-name default) and
/// fields, the `@Id` field marked and static/transient state left out —
/// each cited to the annotations that prove it.
#[test]
fn jpa_entities_carry_table_fields_and_id_with_cited_evidence() {
    let (dir, out) = extract_petclinic();
    let entities: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        entities,
        BTreeSet::from([
            "data:local/demo@jpa:Owner",
            "data:local/demo@jpa:Pet",
            "data:local/demo@jpa:Visit",
        ])
    );

    let owner = node(&out.nodes, "data:local/demo@jpa:Owner");
    assert_eq!(owner.props["storage"], "jpa");
    assert_eq!(owner.props["name"], "Owner");
    assert_eq!(owner.props["table"], "owners");
    assert_eq!(owner.props["store"], "owners");
    assert_eq!(owner.props["table_source"], "explicit");
    assert_eq!(owner.props["schema"], "clinic");
    assert_eq!(owner.props["fqn"], "com.demo.owner.Owner");
    assert_eq!(
        owner.props["fields"],
        serde_json::json!([
            { "name": "id", "type": "Integer", "id": true },
            { "name": "lastName", "type": "String", "id": false, "column": "last_name" },
            {
                "name": "pets",
                "type": "List<Pet>",
                "id": false,
                "relation": "OneToMany"
            },
        ])
    );
    assert_eq!(owner.props["prov"]["confidence_tier"], "Confirmed");
    assert_eq!(owner.props["prov"]["tier"], "Deterministic");
    let source = std::fs::read(dir.path().join("src/main/java/com/demo/owner/Owner.java")).unwrap();
    assert_eq!(
        cited(&source, &owner.props["prov"]),
        ["@Entity", r#"@Table(name = "owners", schema = "clinic")"#]
    );

    // No @Table: JPA's default table is the entity name.
    let visit = node(&out.nodes, "data:local/demo@jpa:Visit");
    assert_eq!(visit.props["table"], "Visit");
    assert_eq!(visit.props["table_source"], "default");
}

/// AC-0284 (#239): `@OneToMany`/`@ManyToOne` fields become REFERENCES
/// edges between the resolved entities, carrying cardinality, owning side
/// and `@JoinColumn`; targets resolve across packages through imports.
#[test]
fn jpa_relations_become_references_edges_with_cardinality() {
    let (dir, out) = extract_petclinic();
    let owner = "data:local/demo@jpa:Owner";
    let pet = "data:local/demo@jpa:Pet";
    let visit = "data:local/demo@jpa:Visit";

    let pets = edge(&out.edges, owner, pet, "REFERENCES");
    assert_eq!(pets.props["relation"], "OneToMany");
    assert_eq!(pets.props["cardinality"], "one-to-many");
    assert_eq!(pets.props["field"], "pets");
    assert_eq!(pets.props["owning"], true);
    assert_eq!(pets.props["join_column"], "owner_id");

    let pet_owner = edge(&out.edges, pet, owner, "REFERENCES");
    assert_eq!(pet_owner.props["cardinality"], "many-to-one");
    assert_eq!(pet_owner.props["join_column"], "owner_id");

    let visits = edge(&out.edges, pet, visit, "REFERENCES");
    assert_eq!(visits.props["cardinality"], "one-to-many");
    assert_eq!(visits.props["mapped_by"], "pet");
    assert_eq!(visits.props["owning"], false);

    let visit_pet = edge(&out.edges, visit, pet, "REFERENCES");
    assert_eq!(visit_pet.props["cardinality"], "many-to-one");
    assert_eq!(visit_pet.props["join_column"], "pet_id");

    // A relation cites the annotated field declaration.
    let source = std::fs::read(dir.path().join("src/main/java/com/demo/visit/Visit.java")).unwrap();
    let cited = cited(&source, &visit_pet.props["prov"]);
    assert_eq!(cited.len(), 1);
    assert!(cited[0].starts_with("@ManyToOne") && cited[0].ends_with("Pet pet;"));
    assert!(
        out.nodes
            .iter()
            .all(|node| node.label != "Gap" || node.props["framework"] != "jpa"),
        "every relation target here is a recovered entity"
    );
}

/// AC-0284 (#239): Spring Data repositories bind to their entity (`Repository`
/// and `JpaRepository`), and calls on a typed receiver — field, `this.field`
/// or parameter — become symbol-attributed READS/WRITES on that entity.
#[test]
fn spring_data_repositories_bind_entities_and_call_sites_read_and_write() {
    let (_dir, out) = extract_petclinic();
    let owner = "data:local/demo@jpa:Owner";
    let pet = "data:local/demo@jpa:Pet";
    let owner_repo =
        "sym:local/demo@src/main/java/com/demo/owner/OwnerRepository.java#OwnerRepository";
    let pet_repo = "sym:local/demo@src/main/java/com/demo/pet/PetRepository.java#PetRepository";

    let binding = edge(&out.edges, owner_repo, owner, "REFERENCES");
    assert_eq!(binding.props["relation"], "repository");
    assert_eq!(binding.props["framework"], "spring-data");
    assert_eq!(binding.props["prov"]["confidence_tier"], "Confirmed");
    edge(&out.edges, pet_repo, pet, "REFERENCES");

    let controller =
        "sym:local/demo@src/main/java/com/demo/owner/OwnerController.java#OwnerController";
    let show = format!("{controller}.show(Integer)");
    let create = format!("{controller}.create(Owner)");
    let forget = format!("{controller}.forget(PetRepository,Integer)");
    // `this.owners.findById(…)` reads, `owners.save(…)` writes.
    assert_eq!(
        edge(&out.edges, &show, owner, "READS").props["method"],
        "findById"
    );
    assert_eq!(
        edge(&out.edges, &create, owner, "WRITES").props["method"],
        "save"
    );
    // A parameter-typed receiver and `this.pets` resolve to Pet.
    edge(&out.edges, &forget, pet, "WRITES");
    edge(&out.edges, &forget, pet, "READS");
    // `cache.get("k")` is a Map, not a repository: nothing is asserted.
    assert!(
        !out.edges
            .iter()
            .any(|e| matches!(e.label.as_str(), "READS" | "WRITES")
                && e.src == show
                && e.dst != owner),
        "{:?}",
        out.edges
    );
    let access = out
        .edges
        .iter()
        .filter(|e| matches!(e.label.as_str(), "READS" | "WRITES"))
        .count();
    assert_eq!(access, 4);
}

/// AC-0284 (#239): nothing is asserted without proof. A lookalike `@Entity`
/// (no JPA import) is not an entity; a computed entity or table name, a
/// relation to a non-entity and a repository of an unknown type are explicit
/// Gaps, never a guessed Confirmed fact (R-INT-4).
#[test]
fn unproven_jpa_facts_fail_closed_to_gaps() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/Lookalike.java",
        r#"package com.demo;

import com.other.Entity;

@Entity
public class Lookalike {
    private Integer id;
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/Dynamic.java",
        r#"package com.demo;

import jakarta.persistence.*;

@Entity
@Table(name = Names.TABLE)
public class Dynamic {
    @Id
    private Integer id;

    @ManyToOne
    private Elsewhere elsewhere;

    @OneToMany(targetEntity = Dynamic.class)
    private java.util.List raw;
}

interface UnknownRepository extends org.springframework.data.repository.CrudRepository<Missing, Integer> {
}

interface GenericRepository<T> extends org.springframework.data.repository.CrudRepository<T, Integer> {
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let entities: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .collect();
    assert_eq!(entities.len(), 1, "{entities:?}");
    let dynamic = entities[0];
    assert_eq!(dynamic.id, "data:local/demo@jpa:Dynamic");
    assert_eq!(dynamic.props["table_source"], "unresolved");
    assert!(dynamic.props.get("table").is_none());

    let reasons: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "jpa")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        BTreeSet::from([
            "JPA relation target is not a recovered entity",
            "Spring Data repository entity is not a recovered entity",
            "runtime-computed JPA table name",
        ])
    );
    // The `targetEntity = Dynamic.class` relation resolves to a self edge;
    // the unknown `Elsewhere` target is the Gap edge, not a Confirmed one.
    let self_edge = edge(
        &out.edges,
        "data:local/demo@jpa:Dynamic",
        "data:local/demo@jpa:Dynamic",
        "REFERENCES",
    );
    assert_eq!(self_edge.props["field"], "raw");
    assert!(out.edges.iter().any(|e| e.label == "REFERENCES"
        && e.dst.starts_with("gap:jpa:")
        && e.props["prov"]["confidence_tier"] == "Gap"));
    // A generic base repository names no entity (a type variable).
    assert!(
        !out.edges
            .iter()
            .any(|e| { e.label == "REFERENCES" && e.src.ends_with("#GenericRepository") })
    );
}

/// AC-0284 (#239): re-ingest with the incremental cache reuses parsed files
/// and still joins entities, relations and repository access identically,
/// re-targeting evidence to the new commit.
#[test]
fn jpa_facts_survive_incremental_reuse_and_retarget_commits() {
    let dir = tempfile::tempdir().unwrap();
    petclinic(dir.path());
    let mut cache = IncrementalCache::default();
    let first_id = id();
    let (first, _) = extract_dir_incremental(dir.path(), &first_id, &mut cache).unwrap();
    let next = SourceId {
        repo: "local/demo",
        commit: "abc123",
    };
    let (second, stats) = extract_dir_incremental(dir.path(), &next, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);

    let shape = |out: &Extraction| {
        let mut facts: Vec<String> = out
            .nodes
            .iter()
            .filter(|n| n.label == "DataEntity")
            .map(|n| format!("N {} {}", n.id, n.props["fields"]))
            .chain(
                out.edges
                    .iter()
                    .filter(|e| matches!(e.label.as_str(), "REFERENCES" | "READS" | "WRITES"))
                    .map(|e| {
                        format!(
                            "E {} {} {} {}",
                            e.label, e.src, e.dst, e.props["cardinality"]
                        )
                    }),
            )
            .collect();
        facts.sort();
        facts
    };
    assert_eq!(shape(&first), shape(&second));
    // 3 entities, 4 relations, 2 repository bindings, 4 repository accesses.
    assert_eq!(shape(&second).len(), 13);
    // The reuse path holds one entity declaration per file: no id is
    // claimed twice, so no duplicate-entity Gap is produced (#702 review).
    assert!(
        !second
            .nodes
            .iter()
            .any(|node| node.id.starts_with("gap:jpa:duplicate:")),
        "{:?}",
        second.nodes.iter().map(|node| &node.id).collect::<Vec<_>>()
    );
    let access = second
        .edges
        .iter()
        .find(|e| e.label == "WRITES")
        .expect("repository write");
    assert_eq!(access.props["prov"]["evidence"][0]["commit_sha"], "abc123");
}

/// AC-0284 (#239): an entity name is not unique across packages. Two
/// `@Entity` classes that share it would claim one DataEntity id and merge
/// into a fact neither class proves. The join replaces the merged node with
/// one explicit Gap naming both classes, and the relation that targeted it
/// fails closed to its own Gap (R-INT-4).
#[test]
fn jpa_duplicate_entity_names_across_packages_fail_closed_to_one_gap() {
    let dir = tempfile::tempdir().unwrap();
    for (package, path) in [
        ("com.demo.owner", "src/main/java/com/demo/owner/Owner.java"),
        ("com.demo.other", "src/main/java/com/demo/other/Owner.java"),
    ] {
        write(
            dir.path(),
            path,
            &format!(
                r#"package {package};

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class Owner {{
    @Id
    private Integer id;
}}
"#
            ),
        );
    }
    write(
        dir.path(),
        "src/main/java/com/demo/pet/Pet.java",
        r#"package com.demo.pet;

import com.demo.owner.Owner;
import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.ManyToOne;

@Entity
public class Pet {
    @Id
    private Integer id;

    @ManyToOne
    private Owner owner;
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();

    // The merged DataEntity is gone: no node claims that id any more.
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id == "data:local/demo@jpa:Owner"),
        "{:?}",
        out.nodes.iter().map(|node| &node.id).collect::<Vec<_>>()
    );
    let entities: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(entities, BTreeSet::from(["data:local/demo@jpa:Pet"]));

    // One Gap stands in for the ambiguous entity, naming both classes.
    let gap = node(&out.nodes, "gap:jpa:duplicate:data:local/demo@jpa:Owner");
    assert_eq!(gap.label, "Gap");
    assert_eq!(
        gap.props["reason"],
        "JPA entity name is declared by more than one class"
    );
    assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    assert_eq!(gap.props["framework"], "jpa");
    assert_eq!(gap.props["language"], "java");
    assert_eq!(
        gap.props["fqns"],
        serde_json::json!(["com.demo.other.Owner", "com.demo.owner.Owner"])
    );
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(gap.props["prov"]["tier"], "Deterministic");
    // The Gap cites the first class in sorted-FQN order.
    assert_eq!(
        gap.props["prov"]["evidence"][0]["path"],
        "src/main/java/com/demo/other/Owner.java"
    );

    // The relation to the ambiguous entity fails closed to its own Gap.
    let relation_gap = out
        .nodes
        .iter()
        .find(|node| {
            node.label == "Gap"
                && node.props["reason"] == "JPA relation target is not a recovered entity"
        })
        .expect("relation gap");
    edge(
        &out.edges,
        "data:local/demo@jpa:Pet",
        &relation_gap.id,
        "REFERENCES",
    );
    assert!(
        !out.edges
            .iter()
            .any(|edge| edge.dst == "data:local/demo@jpa:Owner"),
        "the ambiguous entity id resolves to nothing"
    );
}

/// AC-0284 (#239): a `@Table` name computed at runtime is not a table
/// identity T0 can confirm, so it fails closed to an explicit Gap — and that
/// Gap stays connected to the entity that declares it (R-INT-4).
#[test]
fn jpa_dynamic_table_name_gap_is_connected_to_its_entity() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/Dynamic.java",
        r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.Table;

@Entity
@Table(name = Names.TABLE)
public class Dynamic {
    @Id
    private Integer id;
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let gap = out
        .nodes
        .iter()
        .find(|node| {
            node.label == "Gap" && node.props["reason"] == "runtime-computed JPA table name"
        })
        .expect("dynamic table gap");
    assert_eq!(gap.props["framework"], "jpa");
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");

    // The Gap is connected to its source, not orphaned in the graph.
    let edge = edge(
        &out.edges,
        "data:local/demo@jpa:Dynamic",
        &gap.id,
        "REFERENCES",
    );
    assert_eq!(edge.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(edge.props["attempted_resolution"], "entity-type");

    // The entity itself is still recovered, with no table asserted.
    let entity = node(&out.nodes, "data:local/demo@jpa:Dynamic");
    assert_eq!(entity.props["table_source"], "unresolved");
    assert!(entity.props.get("table").is_none());
}

/// AC-0284 (#239): `this.x` names a field of the immediately enclosing type
/// only. An inner class that declares no such field asserts nothing, even
/// when an enclosing type declares a field of the same name.
#[test]
fn jpa_this_receiver_resolves_only_in_the_immediately_enclosing_type() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/Pet.java",
        r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class Pet {
    @Id
    private Integer id;
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/PetRepository.java",
        r#"package com.demo;

import org.springframework.data.jpa.repository.JpaRepository;

public interface PetRepository extends JpaRepository<Pet, Integer> {
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/Service.java",
        r#"package com.demo;

public class Service {
    private final PetRepository repo;

    public void outerWrite(Pet pet) {
        this.repo.save(pet);
    }

    public class Inner {
        public void innerWrite(Pet pet) {
            this.repo.save(pet);
        }
    }
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let outer = "sym:local/demo@src/main/java/com/demo/Service.java#Service.outerWrite(Pet)";
    let inner = "sym:local/demo@src/main/java/com/demo/Service.java#Service.Inner.innerWrite(Pet)";

    // The outer class declares the field: its `this.repo` is that field.
    edge(&out.edges, outer, "data:local/demo@jpa:Pet", "WRITES");

    // The inner class declares no `repo` field: `this.repo` names nothing
    // there, and the enclosing type's field is a different receiver.
    assert!(
        !out.edges
            .iter()
            .any(|edge| { edge.src == inner && matches!(edge.label.as_str(), "READS" | "WRITES") }),
        "{:?}",
        out.edges
            .iter()
            .filter(|edge| edge.src == inner)
            .collect::<Vec<_>>()
    );
}

/// AC-0284 (#239, #702 review): two relation fields to the same target
/// entity are one fact, not two edges the reconciler collapses into
/// whichever field came last. The published edge carries every field's
/// metadata in `relations` and cites every field's evidence.
#[test]
fn jpa_relations_to_the_same_target_aggregate_into_one_edge_with_per_field_metadata() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/Ticket.java",
        r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.ManyToOne;
import jakarta.persistence.OneToOne;

@Entity
public class Ticket {
    @Id
    private Integer id;

    @ManyToOne
    private User createdBy;

    @OneToOne
    private User approvedBy;
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/User.java",
        r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class User {
    @Id
    private Integer id;
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();

    let ticket = "data:local/demo@jpa:Ticket";
    let user = "data:local/demo@jpa:User";
    let relations: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "REFERENCES" && edge.src == ticket && edge.dst == user)
        .collect();
    assert_eq!(relations.len(), 1, "{relations:?}");

    // Both fields, sorted by name, with their own relation metadata.
    let published = relations[0];
    assert_eq!(
        published.props["relations"],
        serde_json::json!([
            {
                "field": "approvedBy",
                "relation": "OneToOne",
                "cardinality": "one-to-one",
                "owning": true,
            },
            {
                "field": "createdBy",
                "relation": "ManyToOne",
                "cardinality": "many-to-one",
                "owning": true,
            },
        ])
    );
    // The edge cites both fields' evidence.
    let source = std::fs::read(dir.path().join("src/main/java/com/demo/Ticket.java")).unwrap();
    let cited = cited(&source, &published.props["prov"]);
    assert_eq!(cited.len(), 2, "{cited:?}");
    assert!(cited[0].contains("approvedBy"), "{cited:?}");
    assert!(cited[1].contains("createdBy"), "{cited:?}");
}

/// AC-0284 (#239, #702 review): the receiver of a repository call is the
/// declaration visible at the call site — not every declaration in the
/// method. A local declared later or in a sibling block does not capture
/// an earlier call (the field wins there); a local in an inner block and a
/// lambda parameter shadow the field inside their scope; a name bound by a
/// pattern variable asserts nothing, because flow scoping is not modelled.
#[test]
fn jpa_repository_receiver_resolves_against_the_scope_visible_at_the_call_site() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        (
            "Pet",
            r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class Pet {
    @Id
    private Integer id;
}
"#,
        ),
        (
            "Owner",
            r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class Owner {
    @Id
    private Integer id;
}
"#,
        ),
    ] {
        write(
            dir.path(),
            &format!("src/main/java/com/demo/{name}.java"),
            body,
        );
    }
    write(
        dir.path(),
        "src/main/java/com/demo/PetRepository.java",
        r#"package com.demo;

import org.springframework.data.jpa.repository.JpaRepository;

public interface PetRepository extends JpaRepository<Pet, Integer> {
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/OwnerRepository.java",
        r#"package com.demo;

import org.springframework.data.repository.Repository;

public interface OwnerRepository extends Repository<Owner, Integer> {
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/Service.java",
        r#"package com.demo;

public class Service {
    private final PetRepository repo;
    private final OwnerRepository owners;

    void laterLocal(Pet pet) {
        repo.save(pet);
        {
            OwnerRepository repo = owners;
            repo.save(pet);
        }
    }

    void siblingBlock(Pet pet) {
        {
            OwnerRepository repo = owners;
            repo.save(pet);
        }
        {
            repo.save(pet);
        }
    }

    void shadowingLocal(Pet pet) {
        {
            OwnerRepository repo = owners;
            repo.save(pet);
        }
    }

    void lambdaParameter(Pet pet) {
        java.util.function.Function<OwnerRepository, Pet> f = (OwnerRepository repo) -> {
            repo.save(pet);
            return null;
        };
    }

    void patternBinding(Pet pet, Object value) {
        if (value instanceof OwnerRepository repo) {
            repo.save(pet);
        }
    }

    void switchPattern(Pet pet, Object value) {
        switch (value) {
            case OwnerRepository repo -> repo.save(pet);
            default -> { }
        }
    }

    Service(OwnerRepository owners, Pet pet) {
        this.owners = owners;
        this.repo = null;
        OwnerRepository repo = owners;
        repo.save(pet);
    }

    void switchGroupLocal(Pet pet, int kind) {
        switch (kind) {
            case 1:
                OwnerRepository repo = owners;
                repo.save(pet);
                break;
            default:
                break;
        }
    }
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();

    let pet = "data:local/demo@jpa:Pet";
    let owner = "data:local/demo@jpa:Owner";
    let service = "sym:local/demo@src/main/java/com/demo/Service.java#Service";
    let writes = |method: &str, entity: &str| -> usize {
        out.edges
            .iter()
            .filter(|edge| {
                edge.label == "WRITES"
                    && edge.src == format!("{service}.{method}")
                    && edge.dst == entity
            })
            .count()
    };
    let accesses = |method: &str| -> usize {
        out.edges
            .iter()
            .filter(|edge| {
                matches!(edge.label.as_str(), "READS" | "WRITES")
                    && edge.src == format!("{service}.{method}")
            })
            .count()
    };

    // The local in the later block does not capture the earlier call: that
    // one is the field, the later one is the local.
    assert_eq!(writes("laterLocal(Pet)", pet), 1, "{:?}", out.edges);
    assert_eq!(writes("laterLocal(Pet)", owner), 1, "{:?}", out.edges);
    // The sibling block's local is out of scope in the second block: the
    // field wins there.
    assert_eq!(writes("siblingBlock(Pet)", owner), 1, "{:?}", out.edges);
    assert_eq!(writes("siblingBlock(Pet)", pet), 1, "{:?}", out.edges);
    // An inner-block local shadows the field inside its block.
    assert_eq!(writes("shadowingLocal(Pet)", owner), 1, "{:?}", out.edges);
    assert_eq!(writes("shadowingLocal(Pet)", pet), 0, "{:?}", out.edges);
    // A lambda parameter shadows the field too.
    assert_eq!(writes("lambdaParameter(Pet)", owner), 1, "{:?}", out.edges);
    assert_eq!(writes("lambdaParameter(Pet)", pet), 0, "{:?}", out.edges);
    // A name bound by a pattern variable asserts nothing — neither the
    // pattern's type nor the field of the same name.
    assert_eq!(accesses("patternBinding(Pet,Object)"), 0, "{:?}", out.edges);
    assert_eq!(accesses("switchPattern(Pet,Object)"), 0, "{:?}", out.edges);
    // A local declared directly in a constructor body or a switch group
    // shadows the field just as a block local does.
    let constructor_writes = |entity: &str| -> usize {
        out.edges
            .iter()
            .filter(|edge| {
                edge.label == "WRITES"
                    && edge.src.starts_with(&format!("{service}.Service("))
                    && edge.dst == entity
            })
            .count()
    };
    assert_eq!(constructor_writes(owner), 1, "{:?}", out.edges);
    assert_eq!(constructor_writes(pet), 0, "{:?}", out.edges);
    assert_eq!(
        writes("switchGroupLocal(Pet,int)", owner),
        1,
        "{:?}",
        out.edges
    );
    assert_eq!(
        writes("switchGroupLocal(Pet,int)", pet),
        0,
        "{:?}",
        out.edges
    );
}

/// AC-0284 (#239, #702 review): every call site is its own occurrence. The
/// reconciler unions the evidence of repeated `(src, dst, label)` edges, so
/// dropping the later ones would lose their citations — and their fact
/// hash.
#[test]
fn jpa_repeated_repository_calls_each_cite_their_own_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/Pet.java",
        r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class Pet {
    @Id
    private Integer id;
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/PetRepository.java",
        r#"package com.demo;

import org.springframework.data.jpa.repository.JpaRepository;

public interface PetRepository extends JpaRepository<Pet, Integer> {
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/demo/Service.java",
        r#"package com.demo;

public class Service {
    private final PetRepository repo;

    void twice(Pet pet) {
        repo.save(pet);
        repo.save(pet);
    }
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();

    let src = "sym:local/demo@src/main/java/com/demo/Service.java#Service.twice(Pet)";
    let writes: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "WRITES" && edge.src == src)
        .collect();
    assert_eq!(writes.len(), 2, "{writes:?}");
    for occurrence in &writes {
        assert_eq!(occurrence.dst, "data:local/demo@jpa:Pet");
        assert_eq!(occurrence.props["method"], "save");
    }
    // The two occurrences cite their own call site.
    let starts: BTreeSet<u64> = writes
        .iter()
        .map(|edge| {
            edge.props["prov"]["evidence"][0]["byte_start"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(starts.len(), 2, "{starts:?}");
}

/// AC-0284 (#239, #702 review): a collision is counted per declaration, not
/// per distinct FQN. Two source roots that declare the same class claim one
/// DataEntity id just as two packages that declare the same entity name do:
/// the merged node would assert facts no single declaration proves, so one
/// explicit Gap replaces it and names every claiming FQN.
#[test]
fn jpa_same_fqn_entity_declarations_collide_in_two_source_roots() {
    let dir = tempfile::tempdir().unwrap();
    for root in ["src/main/java", "src/test/java"] {
        write(
            dir.path(),
            &format!("{root}/com/demo/Owner.java"),
            r#"package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
public class Owner {
    @Id
    private Integer id;
}
"#,
        );
    }
    let out = extract_dir(dir.path(), &id()).unwrap();

    // The merged DataEntity is gone: no node claims that id any more.
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id == "data:local/demo@jpa:Owner"),
        "{:?}",
        out.nodes.iter().map(|node| &node.id).collect::<Vec<_>>()
    );
    let gap = node(&out.nodes, "gap:jpa:duplicate:data:local/demo@jpa:Owner");
    assert_eq!(gap.label, "Gap");
    assert_eq!(
        gap.props["reason"],
        "JPA entity name is declared by more than one class"
    );
    // Every claiming FQN, duplicates included.
    assert_eq!(
        gap.props["fqns"],
        serde_json::json!(["com.demo.Owner", "com.demo.Owner"])
    );
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    // The Gap cites one of the two declarations.
    assert_eq!(
        gap.props["prov"]["evidence"][0]["path"],
        "src/main/java/com/demo/Owner.java"
    );
}

// --- Retrofit client calls (#737) ------------------------------------------

fn retrofit_sites(files: &[(&str, &str)]) -> Vec<adapters_fw::client::FetchSite> {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        write(dir.path(), rel, content);
    }
    extract_dir(dir.path(), &id()).unwrap().fetch_sites
}

const JAVA_API: &str = r#"package io.demo;

import retrofit2.http.GET;
import retrofit2.http.HTTP;
import retrofit2.http.POST;

public interface ApiService {
    @GET("api/stats")
    Call<Stats> stats();

    @POST(value = "api/auth/login")
    Call<Token> login(@Body Login body);

    @HTTP(method = "DELETE", path = "api/mailboxes/{id}", hasBody = true)
    Call<Ok> deleteMailbox(@Path("id") int id);

    @GET(PREFIX + "aliases")
    Call<Aliases> aliases();

    @retrofit2.http.PUT("api/setting")
    Call<Ok> setting();
}
"#;

// AC-0315: import-proven Retrofit annotations on Java interface methods
// become client calls joined to the repo's one literal builder base; a class
// method and a same-named annotation from another package do not. (T-0315)
#[test]
fn retrofit_interface_methods_become_joined_client_calls() {
    let module = r#"package io.demo;

import retrofit2.Retrofit;

class Module {
    Retrofit provide() {
        return new Retrofit.Builder().baseUrl("https://app.example.io/").build();
    }
}
"#;
    let other = r#"package io.other;

import com.other.GET;

interface Unrelated {
    @GET("api/unrelated")
    void unrelated();
}

class NotAnInterface {
    @retrofit2.http.GET("api/never")
    void never() {}
}
"#;
    let sites = retrofit_sites(&[
        ("src/ApiService.java", JAVA_API),
        ("src/Module.java", module),
        ("src/Other.java", other),
    ]);
    let summary: Vec<_> = sites
        .iter()
        .map(|s| {
            (
                s.method.as_str(),
                s.url.clone(),
                s.entry.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "GET",
                IdentityExpr::Literal("api/stats".into()),
                "ApiService.stats"
            ),
            (
                "POST",
                IdentityExpr::Literal("api/auth/login".into()),
                "ApiService.login"
            ),
            (
                "DELETE",
                IdentityExpr::Literal("api/mailboxes/{id}".into()),
                "ApiService.deleteMailbox"
            ),
            (
                "GET",
                IdentityExpr::Computed("@GET(PREFIX + \"aliases\")".into()),
                "ApiService.aliases"
            ),
            (
                "PUT",
                IdentityExpr::Literal("api/setting".into()),
                "ApiService.setting"
            ),
        ]
    );
    for site in &sites {
        let base = site.base.as_ref().unwrap();
        assert_eq!(
            base.url,
            IdentityExpr::Literal("https://app.example.io/".into())
        );
        assert_eq!(base.path, "src/Module.java");
    }
}

// AC-0315: two distinct Java builder bases cannot be chosen between, and an
// unimported builder proves nothing. (T-0315)
#[test]
fn retrofit_bases_must_be_unique_and_proven() {
    let builder = |import: &str, url: &str| {
        format!(
            "package io.demo;\n{import}\nclass M {{ Object r = new Retrofit.Builder().baseUrl(\"{url}\").build(); }}\n"
        )
    };
    let two = retrofit_sites(&[
        ("ApiService.java", JAVA_API),
        (
            "A.java",
            &builder("import retrofit2.Retrofit;", "https://a.io/"),
        ),
        (
            "B.java",
            &builder("import retrofit2.Retrofit;", "https://b.io/"),
        ),
    ]);
    assert_eq!(
        two[0].base.as_ref().unwrap().url,
        IdentityExpr::Computed("2 distinct Retrofit base URL candidates".into())
    );
    let unproven = retrofit_sites(&[
        ("ApiService.java", JAVA_API),
        ("A.java", &builder("", "https://a.io/")),
    ]);
    assert_eq!(
        unproven[0].base.as_ref().unwrap().url,
        IdentityExpr::Computed("no Retrofit base URL in repo".into())
    );
}

// AC-0428 (#1257): JAX-RS resource classes become endpoints joining the
// class @Path with each method's own @Path, proven by the jakarta or javax
// package; what T0 cannot place is a route Gap naming its handler.
#[test]
fn jaxrs_resources_compose_paths_and_unplaceable_methods_gap() {
    let dir = tempfile::tempdir().unwrap();
    let base = "src/main/java/com/demo/rest";
    write(
        dir.path(),
        &format!("{base}/Connectors.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.GET;
import jakarta.ws.rs.POST;
import jakarta.ws.rs.Path;
import jakarta.ws.rs.PathParam;

@Path("/connectors")
public class Connectors {
    @GET
    public String list() { return ""; }

    @POST
    @Path("{connector}/restart")
    public String restart(@PathParam("connector") String connector) { return ""; }

    @Path("/{connector}/tasks")
    public Object tasks() { return null; }

    static final String DYNAMIC = "/x";

    @GET
    @Path(DYNAMIC)
    public String dynamic() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{base}/Root.java"),
        r#"package com.demo.rest;

import javax.ws.rs.*;

@Path("/")
public class Root {
    @GET
    public String root() { return ""; }

    @jakarta.ws.rs.DELETE
    @Path("/tasks/")
    public String clear() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{base}/Internal.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.PUT;
import jakarta.ws.rs.Path;

public abstract class Internal {
    @PUT
    @Path("/{connector}/fence")
    public String fence() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{base}/Api.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;

@Path("/api")
public interface Api {
    @GET
    String ping();
}
"#,
    );
    // Same-named annotations from another package prove nothing.
    write(
        dir.path(),
        &format!("{base}/Fake.java"),
        r#"package com.demo.rest;

import com.other.GET;
import com.other.Path;

@Path("/fake")
public class Fake {
    @GET
    public String get() { return ""; }
}
"#,
    );
    // A wildcard of any other package proves nothing either.
    write(
        dir.path(),
        &format!("{base}/Wild.java"),
        r#"package com.demo.rest;

import com.other.rs.*;

@Path("/wild")
public class Wild {
    @GET
    public String get() { return ""; }
}
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let endpoints: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        BTreeSet::from([
            "ep:local/demo@DELETE:/tasks/",
            "ep:local/demo@GET:/",
            "ep:local/demo@GET:/connectors",
            "ep:local/demo@POST:/connectors/{connector}/restart",
        ])
    );
    let restart = node(
        &out.nodes,
        "ep:local/demo@POST:/connectors/{connector}/restart",
    );
    assert_eq!(restart.props["framework"], "jax-rs");
    assert_eq!(restart.props["method"], "POST");
    edge(
        &out.edges,
        "ep:local/demo@POST:/connectors/{connector}/restart",
        &format!("sym:local/demo@{base}/Connectors.java#Connectors.restart(String)"),
        "HANDLES",
    );

    let mut gaps: Vec<(String, &str)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "jax-rs")
        .map(|node| {
            (
                node.props["handler_sym"].as_str().unwrap().to_string(),
                node.props["reason"].as_str().unwrap(),
            )
        })
        .collect();
    gaps.sort();
    let sym = |file: &str, method: &str| format!("sym:local/demo@{base}/{file}.java#{method}");
    assert_eq!(
        gaps,
        vec![
            (
                sym("Api", "Api.ping()"),
                "JAX-RS resource method on an interface: served only by an implementing resource class"
            ),
            (
                sym("Connectors", "Connectors.dynamic()"),
                "dynamic JAX-RS path"
            ),
            (
                sym("Connectors", "Connectors.tasks()"),
                "JAX-RS sub-resource locator: routes are resolved at runtime"
            ),
            (
                sym("Internal", "Internal.fence()"),
                "JAX-RS resource method without a class-level @Path (inherited or sub-resource)"
            ),
        ]
    );
    for (handler, _) in &gaps {
        assert!(out.edges.iter().any(|edge| edge.label == "HANDLES"
            && &edge.dst == handler
            && edge.src.starts_with("gap:route:")));
    }
}

// AC-0428 (#1264 review): only public methods are resource methods, a type
// the file declares shadows an imported simple annotation name, a method
// path of `/` keeps its trailing slash, and an anonymous class's methods are
// never composed with the enclosing resource's @Path.
#[test]
fn jaxrs_skips_non_public_shadowed_and_anonymous_methods() {
    let dir = tempfile::tempdir().unwrap();
    let base = "src/main/java/com/demo/rest";
    write(
        dir.path(),
        &format!("{base}/Hidden.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.*;

@Path("/hidden")
public class Hidden {
    @GET
    @Path("/")
    public String index() { return ""; }

    @GET
    @Path("/private")
    private String secret() { return ""; }

    @GET
    @Path("/protected")
    protected String guarded() { return ""; }

    @GET
    @Path("/package")
    String local() { return ""; }

    @Path("/sub")
    public Object sub() {
        return new Object() {
            @GET
            public String inner() { return ""; }
        };
    }
}
"#,
    );
    write(
        dir.path(),
        &format!("{base}/Shadow.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.*;

@Path("/shadow")
public class Shadow {
    @interface GET {}

    @GET
    public String get() { return ""; }

    @jakarta.ws.rs.POST
    public String post() { return ""; }
}
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let endpoints: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        BTreeSet::from(["ep:local/demo@GET:/hidden/", "ep:local/demo@POST:/shadow"])
    );
    let gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "jax-rs")
        .map(|node| node.props["handler_sym"].as_str().unwrap())
        .collect();
    assert_eq!(
        gaps,
        vec![format!("sym:local/demo@{base}/Hidden.java#Hidden.sub()").as_str()]
    );
}

// AC-0428 (#1264 review): a same-package type in another file shadows a
// wildcard-imported annotation (JLS §6.4.1), so the wildcard-only endpoint
// becomes a route Gap at the directory join; a named import, a qualified
// spelling and a wildcard in another package are unaffected.
#[test]
fn jaxrs_wildcard_proof_yields_to_a_same_package_type() {
    let dir = tempfile::tempdir().unwrap();
    let rest = "src/main/java/com/demo/rest";
    let other = "src/main/java/com/demo/other";
    write(
        dir.path(),
        &format!("{rest}/GET.java"),
        "package com.demo.rest;\n\npublic @interface GET {}\n",
    );
    write(
        dir.path(),
        &format!("{rest}/Shadowed.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.*;

@Path("/shadowed")
public class Shadowed {
    @GET
    public String get() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{rest}/Named.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;

@Path("/named")
public class Named {
    @GET
    public String get() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{rest}/Qualified.java"),
        r#"package com.demo.rest;

import jakarta.ws.rs.*;

@Path("/qualified")
public class Qualified {
    @jakarta.ws.rs.GET
    public String get() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{other}/Elsewhere.java"),
        r#"package com.demo.other;

import jakarta.ws.rs.*;

@Path("/elsewhere")
public class Elsewhere {
    @GET
    public String get() { return ""; }
}
"#,
    );
    let mut cache = IncrementalCache::default();
    let (out, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();

    let endpoints: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        BTreeSet::from([
            "ep:local/demo@GET:/elsewhere",
            "ep:local/demo@GET:/named",
            "ep:local/demo@GET:/qualified",
        ])
    );
    let gaps: Vec<(&str, &str)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "jax-rs")
        .map(|node| {
            (
                node.props["handler_sym"].as_str().unwrap(),
                node.props["reason"].as_str().unwrap(),
            )
        })
        .collect();
    let handler = format!("sym:local/demo@{rest}/Shadowed.java#Shadowed.get()");
    assert_eq!(
        gaps,
        vec![(
            handler.as_str(),
            "JAX-RS annotation proven only by a wildcard import is shadowed by a same-package type"
        )]
    );
    assert!(out.edges.iter().any(|edge| edge.label == "HANDLES"
        && edge.dst == handler
        && edge.src.starts_with("gap:route:")));

    // Reused files carry deferred evidence to the new commit: the pending
    // Endpoint and Gap, and both HANDLES edges, cite it.
    let next = SourceId {
        repo: "local/demo",
        commit: "abc123",
    };
    let (moved, stats) = extract_dir_incremental(dir.path(), &next, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    let commit = |props: &serde_json::Value| props["prov"]["evidence"][0]["commit_sha"].clone();
    let shadow_gap = moved
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.props["framework"] == "jax-rs")
        .unwrap();
    let elsewhere = node(&moved.nodes, "ep:local/demo@GET:/elsewhere");
    let handles = |src: &str| {
        moved
            .edges
            .iter()
            .find(|edge| edge.label == "HANDLES" && edge.src == src)
            .unwrap()
    };
    for props in [
        &shadow_gap.props,
        &handles(&shadow_gap.id).props,
        &elsewhere.props,
        &handles(&elsewhere.id).props,
    ] {
        assert_eq!(commit(props), "abc123");
    }

    // Shadowing is settled at the join, never baked into a cached file: once
    // the shadowing type is gone, the reused Shadowed.java is an Endpoint.
    std::fs::remove_file(dir.path().join(format!("{rest}/GET.java"))).unwrap();
    let (out, stats) = extract_dir_incremental(dir.path(), &next, &mut cache).unwrap();
    assert_eq!((stats.recomputed_files, stats.deleted_files), (0, 1));
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id == "ep:local/demo@GET:/shadowed")
    );
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["framework"] == "jax-rs")
    );
}

// AC-0431 (#1250): endpoints are product facts. A Spring controller or a
// JAX-RS resource in a Maven/Gradle test source set yields no Endpoint and
// no route Gap; the same declarations under `src/main` are unchanged.
#[test]
fn test_source_set_controllers_yield_no_product_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let spring = |name: &str, route: &str| {
        format!(
            r#"package com.demo;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class {name} {{
    @GetMapping("{route}")
    public String get() {{ return ""; }}
}}
"#
        )
    };
    let jaxrs = |name: &str, route: &str| {
        format!(
            r#"package com.demo;

import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;

@Path("{route}")
public class {name} {{
    @GET
    public String get() {{ return ""; }}

    @Path("/sub")
    public Object sub() {{ return null; }}
}}
"#
        )
    };
    write(
        dir.path(),
        "web/src/main/java/com/demo/Live.java",
        &spring("Live", "/live"),
    );
    write(
        dir.path(),
        "web/src/main/java/com/demo/LiveRes.java",
        &jaxrs("LiveRes", "/live-res"),
    );
    write(
        dir.path(),
        "web/src/test/java/com/demo/TestCtl.java",
        &spring("TestCtl", "/test-only"),
    );
    write(
        dir.path(),
        "web/src/integrationTest/java/com/demo/ItRes.java",
        &jaxrs("ItRes", "/it-res"),
    );
    // A package directory named `src/test` below a production root is product.
    write(
        dir.path(),
        "web/src/main/java/com/acme/src/test/Nested.java",
        &spring("Nested", "/nested"),
    );
    // A test-only type never shadows a product wildcard import.
    write(
        dir.path(),
        "web/src/main/java/com/demo/Wild.java",
        r#"package com.demo;

import jakarta.ws.rs.*;

@Path("/wild")
public class Wild {
    @GET
    public String get() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        "web/src/test/java/com/demo/GET.java",
        "package com.demo;\n\npublic @interface GET {}\n",
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let endpoints: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        BTreeSet::from([
            "ep:local/demo@GET:/live",
            "ep:local/demo@GET:/live-res",
            "ep:local/demo@GET:/nested",
            "ep:local/demo@GET:/wild",
        ])
    );
    let route_gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:route:"))
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(route_gaps.len(), 1, "{route_gaps:?}");
    assert!(route_gaps[0].contains("/src/main/"), "{route_gaps:?}");
    // Test sources are still recovered as code: only the endpoint is withheld.
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id.ends_with("TestCtl.java#TestCtl.get()"))
    );
}

fn site_rows(sites: &[adapters_fw::client::FetchSite]) -> Vec<(String, String, String)> {
    sites
        .iter()
        .map(|site| {
            let url = match &site.url {
                IdentityExpr::Literal(url) => format!("literal:{url}"),
                IdentityExpr::EnvRef(key) => format!("env:{key}"),
                IdentityExpr::Computed(text) => format!("computed:{text}"),
            };
            (
                site.method.clone(),
                url,
                site.symbol.clone().unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn rest_template_calls_on_proven_receivers_become_fetch_sites() {
    // AC-0435: verb methods and `exchange` on a field, `this.` field,
    // parameter, local and `new` receiver typed `RestTemplate`.
    let sites = retrofit_sites(&[(
        "src/main/java/io/demo/Client.java",
        r#"package io.demo;

import org.springframework.http.HttpMethod;
import org.springframework.web.client.RestTemplate;

class Client {
    private final RestTemplate rest = new RestTemplate();

    void sync(RestTemplate given, String base) {
        rest.getForObject("/users/{id}", String.class, 1);
        this.rest.postForEntity("/users", null, String.class);
        given.put("/users/1", null);
        RestTemplate local = rest;
        local.delete(base + "/users/1");
        new RestTemplate().headForHeaders("/health");
        rest.exchange("/orders", HttpMethod.PATCH, null, String.class);
        rest.exchange("/orders", method(), null, String.class);
        rest.patchForObject("/a\\u0062", null, String.class);
        rest.optionsForAllow("/users");
    }

    HttpMethod method() { return HttpMethod.GET; }
}
"#,
    )]);
    let owner = "sym:local/demo@src/main/java/io/demo/Client.java#Client.sync(RestTemplate,String)";
    let want = [
        ("GET", "literal:/users/{id}"),
        ("POST", "literal:/users"),
        ("PUT", "literal:/users/1"),
        ("DELETE", "computed:base + \"/users/1\""),
        ("HEAD", "literal:/health"),
        ("PATCH", "literal:/orders"),
        ("?", "literal:/orders"),
        ("PATCH", "computed:\"/a\\\\u0062\""),
        ("OPTIONS", "literal:/users"),
    ]
    .map(|(method, url)| (method.to_string(), url.to_string(), owner.to_string()));
    assert_eq!(site_rows(&sites), want);
}

#[test]
fn obscured_http_method_constants_give_an_unknown_method() {
    // AC-0435: a local, parameter or field named `org`, or a type named
    // `org` anywhere in the file, obscures the package in
    // `org.springframework.http.HttpMethod.GET`, so only the unobscured
    // spelling proves the method.
    let sites = retrofit_sites(&[
        (
            "src/main/java/io/demo/Client.java",
            r#"package io.demo;

import org.springframework.web.client.RestTemplate;

class Client {
    private final RestTemplate rest = new RestTemplate();

    void plain() {
        rest.exchange("/plain", org.springframework.http.HttpMethod.GET, null, String.class);
    }

    void local(Holder holder) {
        Holder org = holder;
        rest.exchange("/local", org.springframework.http.HttpMethod.GET, null, String.class);
    }

    void param(Holder org) {
        rest.exchange("/param", org.springframework.http.HttpMethod.GET, null, String.class);
    }
}

class Fielded {
    private final RestTemplate rest = new RestTemplate();
    private Holder org;

    void field() {
        rest.exchange("/field", org.springframework.http.HttpMethod.GET, null, String.class);
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Typed.java",
            r#"package io.demo;

import org.springframework.web.client.RestTemplate;

class Typed {
    static class org {}
    private final RestTemplate rest = new RestTemplate();

    void typed() {
        rest.exchange("/typed", org.springframework.http.HttpMethod.GET, null, String.class);
    }
}
"#,
        ),
    ]);
    let rows = site_rows(&sites)
        .into_iter()
        .map(|(method, url, _)| (method, url))
        .collect::<Vec<_>>();
    let want = [
        ("GET", "literal:/plain"),
        ("?", "literal:/local"),
        ("?", "literal:/param"),
        ("?", "literal:/field"),
        ("?", "literal:/typed"),
    ]
    .map(|(method, url)| (method.to_string(), url.to_string()));
    assert_eq!(rows, want);
}

#[test]
fn unproven_rest_template_receivers_yield_no_fetch_sites() {
    // AC-0435: wildcard imports, lookalike types, a same-named nested type or
    // type parameter, untyped and pattern bindings, anonymous subclasses,
    // outer and inherited fields, and test source sets prove nothing.
    let sites = retrofit_sites(&[
        (
            "src/main/java/io/demo/Wild.java",
            r#"package io.demo;
import org.springframework.web.client.*;
class Wild {
    RestTemplate rest;
    void a() { rest.getForObject("/wild", String.class); }
}
"#,
        ),
        (
            "src/main/java/io/demo/Look.java",
            r#"package io.demo;
import com.acme.RestTemplate;
class Look {
    RestTemplate rest;
    void a() { rest.getForObject("/look", String.class); }
}
"#,
        ),
        (
            "src/main/java/io/demo/Shadow.java",
            r#"package io.demo;
import org.springframework.web.client.RestTemplate;
class Shadow<T> {
    static class RestTemplate { void getForObject(String u, Class<?> c) {} }
    RestTemplate rest;
    void a() { rest.getForObject("/nested", String.class); }
}
"#,
        ),
        (
            "src/main/java/io/demo/Param.java",
            r#"package io.demo;
import org.springframework.web.client.RestTemplate;
class Param {
    <RestTemplate> void a(RestTemplate rest) { rest.getForObject("/generic", String.class); }
}
"#,
        ),
        (
            "src/main/java/io/demo/Scopes.java",
            r#"package io.demo;
import java.util.function.Consumer;
import org.springframework.web.client.RestTemplate;
class Scopes extends Base {
    RestTemplate outer;
    void a(Object o) {
        Consumer<Object> c = rest -> ((RestTemplate) rest).getForObject("/cast", String.class);
        if (o instanceof RestTemplate t) { t.getForObject("/pattern", String.class); }
        new RestTemplate() {}.getForObject("/anonymous", String.class);
        inherited.getForObject("/inherited", String.class);
        Runnable r = new Runnable() {
            public void run() { outer.getForObject("/anon-outer", String.class); }
        };
    }
    class Inner {
        void b() { outer.getForObject("/inner-outer", String.class); }
    }
}
"#,
        ),
        (
            "src/test/java/io/demo/ClientTest.java",
            r#"package io.demo;
import org.springframework.web.client.RestTemplate;
class ClientTest {
    RestTemplate rest;
    void a() { rest.getForObject("/test", String.class); }
}
"#,
        ),
    ]);
    assert!(sites.is_empty(), "{:?}", site_rows(&sites));
}

#[test]
fn single_file_extraction_exposes_rest_template_sites() {
    // AC-0435: the public single-file API returns RestTemplate sites as
    // fetch sites, never holding them back for a directory join.
    let out = extract_source(
        b"package io.demo;\n\nimport org.springframework.web.client.RestTemplate;\n\n\
          class Client {\n    void sync(RestTemplate rest) { rest.getForObject(\"/users\", String.class); }\n}\n",
        "src/main/java/io/demo/Client.java",
        &id(),
    )
    .unwrap();
    assert_eq!(
        site_rows(&out.fetch_sites),
        [(
            "GET".to_string(),
            "literal:/users".to_string(),
            "sym:local/demo@src/main/java/io/demo/Client.java#Client.sync(RestTemplate)"
                .to_string()
        )]
    );
}

// AC-0438 (#1288): a same-package type obscures a wildcard-imported Spring
// annotation (JLS §6.4.1), whether it is the controller stereotype, the class
// `@RequestMapping` base or the method mapping, so the wildcard-only endpoint
// becomes a route Gap at the directory join. A named import, the exact
// qualified spelling, a wildcard in another package and a test-only type are
// unaffected; a qualified lookalike proves nothing.
#[test]
fn spring_wildcard_proof_yields_to_a_same_package_type() {
    let dir = tempfile::tempdir().unwrap();
    let web = "src/main/java/com/demo/web";
    let ctl = "src/main/java/com/demo/ctl";
    let base = "src/main/java/com/demo/base";
    let other = "src/main/java/com/demo/other";
    let wildcard_controller = |package: &str, class: &str, route: &str| {
        format!(
            "package {package};\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\npublic class {class} {{\n    @GetMapping(\"{route}\")\n    public String get() {{ return \"\"; }}\n}}\n"
        )
    };
    write(
        dir.path(),
        &format!("{web}/GetMapping.java"),
        "package com.demo.web;\n\npublic @interface GetMapping { String value() default \"\"; }\n",
    );
    write(
        dir.path(),
        &format!("{web}/Shadowed.java"),
        &wildcard_controller("com.demo.web", "Shadowed", "/shadowed"),
    );
    write(
        dir.path(),
        &format!("{web}/Named.java"),
        r#"package com.demo.web;

import org.springframework.web.bind.annotation.*;
import org.springframework.web.bind.annotation.GetMapping;

@RestController
public class Named {
    @GetMapping("/named")
    public String get() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{web}/Qualified.java"),
        r#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
public class Qualified {
    @org.springframework.web.bind.annotation.GetMapping("/qualified")
    public String get() { return ""; }

    @com.acme.GetMapping("/lookalike")
    public String lookalike() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{ctl}/RestController.java"),
        "package com.demo.ctl;\n\npublic @interface RestController {}\n",
    );
    write(
        dir.path(),
        &format!("{ctl}/Stereotype.java"),
        &wildcard_controller("com.demo.ctl", "Stereotype", "/stereotype"),
    );
    write(
        dir.path(),
        &format!("{base}/RequestMapping.java"),
        "package com.demo.base;\n\npublic @interface RequestMapping { String value() default \"\"; }\n",
    );
    write(
        dir.path(),
        &format!("{base}/Based.java"),
        r#"package com.demo.base;

import org.springframework.web.bind.annotation.*;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
@RequestMapping("/api")
public class Based {
    @GetMapping("/based")
    public String get() { return ""; }
}
"#,
    );
    write(
        dir.path(),
        &format!("{other}/Elsewhere.java"),
        &wildcard_controller("com.demo.other", "Elsewhere", "/elsewhere"),
    );
    write(
        dir.path(),
        "src/test/java/com/demo/other/GetMapping.java",
        "package com.demo.other;\n\npublic @interface GetMapping { String value() default \"\"; }\n",
    );
    let mut cache = IncrementalCache::default();
    let (out, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();

    let endpoints: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint" && node.props["framework"] == "spring")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        BTreeSet::from([
            "ep:local/demo@GET:/elsewhere",
            "ep:local/demo@GET:/named",
            "ep:local/demo@GET:/qualified",
        ])
    );
    let gaps: BTreeSet<(&str, &str)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "spring")
        .map(|node| {
            (
                node.props["handler_sym"].as_str().unwrap(),
                node.props["reason"].as_str().unwrap(),
            )
        })
        .collect();
    let reason =
        "Spring annotation proven only by a wildcard import is shadowed by a same-package type";
    let shadowed = format!("sym:local/demo@{web}/Shadowed.java#Shadowed.get()");
    let stereotype = format!("sym:local/demo@{ctl}/Stereotype.java#Stereotype.get()");
    let based = format!("sym:local/demo@{base}/Based.java#Based.get()");
    assert_eq!(
        gaps,
        BTreeSet::from([
            (based.as_str(), reason),
            (shadowed.as_str(), reason),
            (stereotype.as_str(), reason),
        ])
    );
    for handler in [&shadowed, &stereotype, &based] {
        assert!(out.edges.iter().any(|edge| edge.label == "HANDLES"
            && &edge.dst == handler
            && edge.src.starts_with("gap:route:")));
    }

    // A reused file's deferred endpoint settles the same way at the next commit.
    let next = SourceId {
        repo: "local/demo",
        commit: "abc123",
    };
    let (moved, _) = extract_dir_incremental(dir.path(), &next, &mut cache).unwrap();
    let moved_gaps = moved
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["reason"] == reason)
        .count();
    assert_eq!(moved_gaps, 3);
}

// AC-0438 (#1288): single-file extraction settles wildcard-proven Spring and
// JAX-RS endpoints against the file's own types instead of dropping them.
#[test]
fn single_file_extraction_settles_wildcard_endpoints() {
    let spring = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
public class Users {
    @GetMapping("/users")
    public String list() { return ""; }
}
"#;
    let out = extract_source(spring, "src/main/java/com/demo/web/Users.java", &id()).unwrap();
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id == "ep:local/demo@GET:/users")
    );
    let jaxrs = br#"package com.demo.rest;

import jakarta.ws.rs.*;

@Path("/orders")
public class Orders {
    @GET
    public String list() { return ""; }
}
"#;
    let out = extract_source(jaxrs, "src/main/java/com/demo/rest/Orders.java", &id()).unwrap();
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id == "ep:local/demo@GET:/orders")
    );
}

// AC-0438 (#1296 review): any one proven stereotype makes a controller, so a
// directly imported `@Controller` keeps the endpoint whatever the annotation
// order, and wildcard-only stereotypes fail closed only when every one of
// them is shadowed by a same-package type.
#[test]
fn spring_direct_controller_proof_dominates_a_shadowed_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let one = "src/main/java/com/demo/one";
    let half = "src/main/java/com/demo/half";
    let both = "src/main/java/com/demo/both";
    let class = |package: &str, imports: &str, annotations: &str, class: &str| {
        format!(
            "package {package};\n\n{imports}\n{annotations}\npublic class {class} {{\n    @GetMapping(\"/{route}\")\n    public String get() {{ return \"\"; }}\n}}\n",
            route = class.to_lowercase()
        )
    };
    let direct = "import org.springframework.web.bind.annotation.*;\nimport org.springframework.web.bind.annotation.GetMapping;\nimport org.springframework.stereotype.Controller;\n";
    let wildcards = "import org.springframework.web.bind.annotation.*;\nimport org.springframework.web.bind.annotation.GetMapping;\nimport org.springframework.stereotype.*;\n";
    // Both stereotypes are declared here, but the named `Controller` import
    // wins over its same-package type, so the endpoints stand.
    for (package, shadows) in [
        (one, &["RestController", "Controller"][..]),
        (half, &["RestController"][..]),
        (both, &["RestController", "Controller"][..]),
    ] {
        let name = package.rsplit('/').next().unwrap();
        for shadow in shadows {
            write(
                dir.path(),
                &format!("{package}/{shadow}.java"),
                &format!("package com.demo.{name};\n\npublic @interface {shadow} {{}}\n"),
            );
        }
    }
    for (name, imports, annotations) in [
        ("RestFirst", direct, "@RestController\n@Controller"),
        ("ControllerFirst", direct, "@Controller\n@RestController"),
    ] {
        write(
            dir.path(),
            &format!("{one}/{name}.java"),
            &class("com.demo.one", imports, annotations, name),
        );
    }
    write(
        dir.path(),
        &format!("{half}/OneShadowed.java"),
        &class(
            "com.demo.half",
            wildcards,
            "@RestController\n@Controller",
            "OneShadowed",
        ),
    );
    write(
        dir.path(),
        &format!("{both}/AllShadowed.java"),
        &class(
            "com.demo.both",
            wildcards,
            "@RestController\n@Controller",
            "AllShadowed",
        ),
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let endpoints: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint" && node.props["framework"] == "spring")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        BTreeSet::from([
            "ep:local/demo@GET:/controllerfirst",
            "ep:local/demo@GET:/oneshadowed",
            "ep:local/demo@GET:/restfirst",
        ])
    );
    let gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "spring")
        .map(|node| node.props["handler_sym"].as_str().unwrap())
        .collect();
    assert_eq!(
        gaps,
        vec![format!("sym:local/demo@{both}/AllShadowed.java#AllShadowed.get()").as_str()]
    );
}

// AC-0438 (#1296 review): a shadowed annotation, not its dynamic path, blocks
// the route, so a shadowed dynamic mapping settles to the shadowing Gap at the
// directory join while an unshadowed one keeps the dynamic-path Gap.
#[test]
fn spring_shadowed_dynamic_mappings_report_the_shadowing() {
    let dir = tempfile::tempdir().unwrap();
    let controller = |package: &str| {
        format!(
            "package {package};\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\npublic class Paths {{\n    static final String PATH = \"/p\";\n    @GetMapping(PATH)\n    public String get() {{ return \"\"; }}\n}}\n"
        )
    };
    write(
        dir.path(),
        "src/main/java/com/demo/shadow/GetMapping.java",
        "package com.demo.shadow;\n\npublic @interface GetMapping { String value() default \"\"; }\n",
    );
    write(
        dir.path(),
        "src/main/java/com/demo/shadow/Paths.java",
        &controller("com.demo.shadow"),
    );
    write(
        dir.path(),
        "src/main/java/com/demo/plain/Paths.java",
        &controller("com.demo.plain"),
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let gaps: BTreeSet<(&str, &str)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "spring")
        .map(|node| {
            (
                node.props["handler_sym"].as_str().unwrap(),
                node.props["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        gaps,
        BTreeSet::from([
            (
                "sym:local/demo@src/main/java/com/demo/plain/Paths.java#Paths.get()",
                "dynamic Spring mapping path",
            ),
            (
                "sym:local/demo@src/main/java/com/demo/shadow/Paths.java#Paths.get()",
                "Spring annotation proven only by a wildcard import is shadowed by a same-package type",
            ),
        ])
    );
    assert_eq!(
        out.edges
            .iter()
            .filter(|edge| edge.label == "HANDLES" && edge.src.starts_with("gap:route:"))
            .count(),
        2
    );
}

/// `(method, url, base, entry)` rows of Feign sites, sorted.
fn feign_rows(sites: &[adapters_fw::client::FetchSite]) -> Vec<[String; 4]> {
    let expr = |expr: &IdentityExpr| match expr {
        IdentityExpr::Literal(url) => format!("literal:{url}"),
        IdentityExpr::EnvRef(key) => format!("env:{key}"),
        IdentityExpr::Computed(text) => format!("computed:{text}"),
    };
    let mut rows = sites
        .iter()
        .map(|site| {
            [
                site.method.clone(),
                expr(&site.url),
                site.base
                    .as_ref()
                    .map(|base| expr(&base.url))
                    .unwrap_or_default(),
                site.entry.clone().unwrap_or_default(),
            ]
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

#[test]
fn feign_client_methods_become_fetch_sites_with_their_base() {
    // AC-0437: abstract mapped methods of proven @FeignClient interfaces,
    // with the client's path prefix and its url, service or placeholder base.
    let sites = retrofit_sites(&[
        (
            "src/main/java/io/demo/UsersClient.java",
            r#"package io.demo;

import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.*;

@FeignClient(name = "users", url = "users.example:8080/", path = "api/")
interface UsersClient {
    @GetMapping("/users/{id}")
    User get(@PathVariable long id);

    @PostMapping("users")
    User create(User user);

    @RequestMapping(value = "/users/{id}", method = RequestMethod.DELETE)
    void delete(long id);

    @RequestMapping("/health")
    String health();

    @RequestMapping(value = "/both", method = {RequestMethod.GET, RequestMethod.POST})
    String both();

    @PutMapping
    String root();

    @GetMapping(PATH)
    String constant();

    @GetMapping("/local")
    default String local() { return ""; }
}
"#,
        ),
        (
            "src/main/java/io/demo/OrdersClient.java",
            r#"package io.demo;

import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.GetMapping;

@FeignClient("orders")
interface OrdersClient {
    @GetMapping("/orders")
    List<Order> all();
}
"#,
        ),
        (
            "src/main/java/io/demo/PayClient.java",
            r#"package io.demo;

@org.springframework.cloud.openfeign.FeignClient(name = "pay", url = "${pay.url}")
interface PayClient {
    @org.springframework.web.bind.annotation.GetMapping("/charges")
    List<Charge> charges();
}
"#,
        ),
    ]);
    let users = "computed:@GetMapping(PATH)";
    let base = "literal:http://users.example:8080";
    let mut want = [
        ["GET", "literal:/api/users/{id}", base, "UsersClient.get"],
        ["POST", "literal:/api/users", base, "UsersClient.create"],
        [
            "DELETE",
            "literal:/api/users/{id}",
            base,
            "UsersClient.delete",
        ],
        ["GET", "literal:/api/health", base, "UsersClient.health"],
        ["?", "literal:/api/both", base, "UsersClient.both"],
        ["PUT", "literal:/api", base, "UsersClient.root"],
        ["GET", users, base, "UsersClient.constant"],
        [
            "GET",
            "literal:/orders",
            "computed:orders",
            "OrdersClient.all",
        ],
        [
            "GET",
            "literal:/charges",
            "computed:${pay.url}",
            "PayClient.charges",
        ],
    ]
    .map(|row| row.map(str::to_string));
    want.sort();
    assert_eq!(feign_rows(&sites), want);
}

#[test]
fn unproven_feign_clients_yield_no_fetch_sites() {
    // AC-0437: wildcard and lookalike imports, a file-declared FeignClient,
    // a class, unproven mappings and a test source set prove nothing; a
    // class-level @RequestMapping, which OpenFeign rejects, makes the
    // client's sites computed.
    let client = |import: &str, annotation: &str| {
        format!(
            "package io.demo;\n\n{import}\nimport org.springframework.web.bind.annotation.GetMapping;\n\n\
             {annotation}\ninterface Client {{\n    @GetMapping(\"/x\")\n    String x();\n}}\n"
        )
    };
    let wildcard = client(
        "import org.springframework.cloud.openfeign.*;",
        "@FeignClient(\"a\")",
    );
    let lookalike = client("import com.acme.FeignClient;", "@FeignClient(\"a\")");
    let declared = format!(
        "{}\n@interface FeignClient {{ String value(); }}\n",
        client(
            "import org.springframework.cloud.openfeign.FeignClient;",
            "@FeignClient(\"a\")"
        )
    );
    let class = "package io.demo;\n\nimport org.springframework.cloud.openfeign.FeignClient;\n\
                 import org.springframework.web.bind.annotation.GetMapping;\n\n\
                 @FeignClient(\"a\")\nabstract class Client {\n    @GetMapping(\"/x\")\n    abstract String x();\n}\n";
    let unmapped = "package io.demo;\n\nimport org.springframework.cloud.openfeign.FeignClient;\n\n\
                    @FeignClient(\"a\")\ninterface Client {\n    @GetMapping(\"/x\")\n    String x();\n}\n";
    let proven = client(
        "import org.springframework.cloud.openfeign.FeignClient;",
        "@FeignClient(\"a\")",
    );
    let class_mapping = client(
        "import org.springframework.cloud.openfeign.FeignClient;\n\
         import org.springframework.web.bind.annotation.RequestMapping;",
        "@FeignClient(\"a\")\n@RequestMapping(\"/api\")",
    );
    let shadowed = format!(
        "{}\n@interface GetMapping {{ String value(); }}\n",
        client(
            "import org.springframework.cloud.openfeign.FeignClient;",
            "@FeignClient(\"a\")"
        )
    );
    let lookalike_mapping = "package io.demo;\n\nimport org.springframework.cloud.openfeign.FeignClient;\n\
                             import org.springframework.web.bind.annotation.GetMapping;\n\n\
                             @FeignClient(\"a\")\ninterface Client {\n    @com.acme.GetMapping(\"/x\")\n    String x();\n}\n";
    for (rel, source) in [
        ("src/main/java/io/demo/Client.java", shadowed.as_str()),
        ("src/main/java/io/demo/Client.java", lookalike_mapping),
        ("src/main/java/io/demo/Client.java", wildcard.as_str()),
        ("src/main/java/io/demo/Client.java", lookalike.as_str()),
        ("src/main/java/io/demo/Client.java", declared.as_str()),
        ("src/main/java/io/demo/Client.java", class),
        ("src/main/java/io/demo/Client.java", unmapped),
        ("src/test/java/io/demo/Client.java", proven.as_str()),
    ] {
        assert_eq!(
            feign_rows(&retrofit_sites(&[(rel, source)])),
            Vec::<[String; 4]>::new(),
            "{source}"
        );
    }
    let rows = feign_rows(&retrofit_sites(&[(
        "src/main/java/io/demo/Client.java",
        class_mapping.as_str(),
    )]));
    assert_eq!(
        rows,
        [[
            "GET",
            "computed:@GetMapping(\"/x\")",
            "computed:a",
            "Client.x"
        ]
        .map(str::to_string)]
    );
}

#[test]
fn same_package_types_obscure_wildcard_feign_mapping_proof() {
    // AC-0437: a same-package type obscures an on-demand import (JLS
    // §6.4.1), so a mapping proven only by a wildcard yields no site, and a
    // `RequestMethod` proven only by one leaves the method `?`. A named
    // import, a package without the type and a test-only type are controls.
    let wildcard = "import org.springframework.cloud.openfeign.FeignClient;\n\
                    import org.springframework.web.bind.annotation.*;";
    let client = |package: &str, imports: &str, name: &str, body: &str| {
        format!(
            "package {package};\n\n{imports}\n\n@FeignClient(name = \"c\", url = \"https://x\")\n\
             interface {name} {{\n{body}}}\n"
        )
    };
    let shadowed = client(
        "p",
        wildcard,
        "C",
        "    @GetMapping(\"/x\")\n    void call();\n\n    \
         @RequestMapping(value = \"/y\", method = RequestMethod.POST)\n    void post();\n",
    );
    let named = client(
        "p",
        "import org.springframework.cloud.openfeign.FeignClient;\n\
         import org.springframework.web.bind.annotation.GetMapping;",
        "Named",
        "    @GetMapping(\"/z\")\n    void z();\n",
    );
    let other = client(
        "q",
        wildcard,
        "Q",
        "    @GetMapping(\"/q\")\n    void q();\n",
    );
    let sites = retrofit_sites(&[
        (
            "src/main/java/p/GetMapping.java",
            "package p;\n\npublic @interface GetMapping { String value(); }\n",
        ),
        (
            "src/main/java/p/RequestMethod.java",
            "package p;\n\npublic enum RequestMethod { GET, POST }\n",
        ),
        ("src/main/java/p/C.java", shadowed.as_str()),
        ("src/main/java/p/Named.java", named.as_str()),
        ("src/main/java/q/Q.java", other.as_str()),
        (
            "src/test/java/q/GetMapping.java",
            "package q;\n\npublic @interface GetMapping { String value(); }\n",
        ),
    ]);
    let mut want = [
        ["?", "literal:/y", "literal:https://x", "C.post"],
        ["GET", "literal:/z", "literal:https://x", "Named.z"],
        ["GET", "literal:/q", "literal:https://x", "Q.q"],
    ]
    .map(|row| row.map(str::to_string));
    want.sort();
    assert_eq!(feign_rows(&sites), want);
}

#[test]
fn single_file_extraction_exposes_feign_sites() {
    // AC-0437: the public single-file API returns Feign sites as fetch sites,
    // settled against the file's own declared types.
    let out = extract_source(
        b"package io.demo;\n\nimport org.springframework.cloud.openfeign.FeignClient;\n\
          import org.springframework.web.bind.annotation.*;\n\n\
          @FeignClient(\"users\")\ninterface Users {\n    @GetMapping(\"/users\")\n    String all();\n}\n",
        "src/main/java/io/demo/Users.java",
        &id(),
    )
    .unwrap();
    assert_eq!(
        feign_rows(&out.fetch_sites),
        [["GET", "literal:/users", "computed:users", "Users.all"].map(str::to_string)]
    );
}

// AC-0437 (#1289 review): a class-level `@RequestMapping` proven only by a
// wildcard makes the Feign paths computed unless a same-package type obscures
// it, in which case the method's own literal path stands; a directly imported
// class mapping keeps them computed whatever the package declares.
#[test]
fn feign_wildcard_class_mapping_settles_at_the_join() {
    let dir = tempfile::tempdir().unwrap();
    let client = |package: &str, imports: &str, name: &str| {
        format!(
            "package {package};\n\nimport org.springframework.cloud.openfeign.FeignClient;\nimport org.springframework.web.bind.annotation.GetMapping;\n{imports}\n@FeignClient(name = \"users\", url = \"https://users.example.com\")\n@RequestMapping(\"/api\")\npublic interface {name} {{\n    @GetMapping(\"/x\")\n    String get();\n}}\n"
        )
    };
    let wildcard = "import org.springframework.web.bind.annotation.*;\n";
    let named = "import org.springframework.web.bind.annotation.RequestMapping;\n";
    write(
        dir.path(),
        "src/main/java/com/demo/shadow/RequestMapping.java",
        "package com.demo.shadow;\n\npublic @interface RequestMapping { String value() default \"\"; }\n",
    );
    write(
        dir.path(),
        "src/main/java/com/demo/shadow/Shadowed.java",
        &client("com.demo.shadow", wildcard, "Shadowed"),
    );
    write(
        dir.path(),
        "src/main/java/com/demo/shadow/Named.java",
        &client("com.demo.shadow", named, "Named"),
    );
    write(
        dir.path(),
        "src/main/java/com/demo/plain/Plain.java",
        &client("com.demo.plain", wildcard, "Plain"),
    );
    let rows: Vec<[String; 2]> = feign_rows(&extract_dir(dir.path(), &id()).unwrap().fetch_sites)
        .into_iter()
        .map(|[_, url, _, entry]| [entry, url])
        .collect();
    assert_eq!(
        rows,
        vec![
            [
                "Named.get".to_string(),
                "computed:@GetMapping(\"/x\")".to_string()
            ],
            [
                "Plain.get".to_string(),
                "computed:@GetMapping(\"/x\")".to_string()
            ],
            ["Shadowed.get".to_string(), "literal:/x".to_string()],
        ]
    );
}

#[test]
fn webclient_bases_keep_their_slash_before_relative_paths() {
    // AC-0441 (#1380): a literal base keeps its trailing `/` before a
    // relative request path, as Spring joins `/api/` + `orders` to
    // `/api/orders`; before a rooted or computed path the doubled `/`
    // collapses, and a base without the slash concatenates as Spring does.
    let sites = retrofit_sites(&[(
        "src/main/java/io/demo/Client.java",
        r#"package io.demo;

import org.springframework.web.reactive.function.client.WebClient;

class Client {
    void calls(String path) {
        WebClient.create("https://host/api/").get().uri("orders").retrieve();
        WebClient.create("https://host/api/").get().uri("/orders").retrieve();
        WebClient.builder().baseUrl("https://host/api/").build().get().uri(path).retrieve();
        WebClient.create("https://host/api").get().uri("orders").retrieve();
    }
}
"#,
    )]);
    assert_eq!(
        webclient_rows(&sites),
        [
            ["GET", "literal:orders", "literal:https://host/api/"],
            ["GET", "literal:/orders", "literal:https://host/api"],
            ["GET", "computed:path", "literal:https://host/api"],
            ["GET", "literal:orders", "literal:https://host/api"],
        ]
    );
}

fn webclient_rows(sites: &[adapters_fw::client::FetchSite]) -> Vec<[String; 3]> {
    let expr = |expr: &IdentityExpr| match expr {
        IdentityExpr::Literal(url) => format!("literal:{url}"),
        IdentityExpr::EnvRef(key) => format!("env:{key}"),
        IdentityExpr::Computed(text) => format!("computed:{text}"),
    };
    sites
        .iter()
        .map(|site| {
            [
                site.method.clone(),
                expr(&site.url),
                site.base
                    .as_ref()
                    .map(|base| expr(&base.url))
                    .unwrap_or_else(|| "none".into()),
            ]
        })
        .collect()
}

#[test]
fn webclient_request_chains_on_proven_clients_become_fetch_sites() {
    // AC-0441: every verb and `method(HttpMethod.X)` on a field, `this.`
    // field, parameter and local typed `WebClient`, plus inline `create` and
    // `builder()…build()` clients with literal, computed and absent bases; a
    // URI builder factory (either order) or `apply` makes the base runtime,
    // and an absolute request URL drops the base.
    let sites = retrofit_sites(&[(
        "src/main/java/io/demo/Client.java",
        r#"package io.demo;

import org.springframework.http.HttpMethod;
import org.springframework.web.reactive.function.client.WebClient;

class Client {
    private final WebClient users;

    Client(WebClient users) { this.users = users; }

    void calls(WebClient param, String id) {
        users.get().uri("/users/{id}", id).retrieve();
        this.users.post().uri("/users").retrieve();
        param.put().uri("/users/" + id).retrieve();
        WebClient local = param;
        local.delete().uri("/users/{id}", id).retrieve();
        users.patch().uri("/a").retrieve();
        users.head().uri("/b").retrieve();
        users.options().uri("/c").retrieve();
        users.method(HttpMethod.POST).uri("/d").retrieve();
        users.method(verb()).uri("/e").retrieve();
        WebClient.create("https://orders.example.com/").get().uri("/orders").retrieve();
        WebClient.create().get().uri("https://absolute.example.com/x").retrieve();
        WebClient.builder().baseUrl("https://old.example.com").baseUrl("https://stock.example.com").build().get().uri("/stock").retrieve();
        WebClient.builder().baseUrl(host()).build().get().uri("/f").retrieve();
        WebClient.builder().build().get().uri("/g").retrieve();
        WebClient.builder().baseUrl("https://a.example").uriBuilderFactory(factory).build().get().uri("/h").retrieve();
        WebClient.builder().uriBuilderFactory(factory).baseUrl("https://a.example").build().get().uri("/i").retrieve();
        WebClient.builder().uriBuilderFactory(factory).build().get().uri("/j").retrieve();
        WebClient.builder().apply(b -> b.baseUrl(host())).build().get().uri("/k").retrieve();
        WebClient.create("https://orders.example.com").get().uri("https://other.example.com/l").retrieve();
        users.get().uri("http://other.example.com/m").retrieve();
        users.get().uri("/n?next=https://x").retrieve();
        users.get().uri("9p://not-a-scheme").retrieve();
    }
}
"#,
    )]);
    assert_eq!(
        webclient_rows(&sites),
        [
            ["GET", "literal:/users/{id}", "computed:users"],
            ["POST", "literal:/users", "computed:this.users"],
            ["PUT", "computed:\"/users/\" + id", "computed:param"],
            ["DELETE", "literal:/users/{id}", "computed:local"],
            ["PATCH", "literal:/a", "computed:users"],
            ["HEAD", "literal:/b", "computed:users"],
            ["OPTIONS", "literal:/c", "computed:users"],
            ["POST", "literal:/d", "computed:users"],
            ["?", "literal:/e", "computed:users"],
            [
                "GET",
                "literal:/orders",
                "literal:https://orders.example.com"
            ],
            ["GET", "literal:https://absolute.example.com/x", "none"],
            ["GET", "literal:/stock", "literal:https://stock.example.com"],
            ["GET", "literal:/f", "computed:host()"],
            ["GET", "literal:/g", "none"],
            [
                "GET",
                "literal:/h",
                "computed:WebClient.builder().baseUrl(\"https://a.example\").uriBuilderFactory(factory).build()"
            ],
            [
                "GET",
                "literal:/i",
                "computed:WebClient.builder().uriBuilderFactory(factory).baseUrl(\"https://a.example\").build()"
            ],
            [
                "GET",
                "literal:/j",
                "computed:WebClient.builder().uriBuilderFactory(factory).build()"
            ],
            [
                "GET",
                "literal:/k",
                "computed:WebClient.builder().apply(b -> b.baseUrl(host())).build()"
            ],
            ["GET", "literal:https://other.example.com/l", "none"],
            ["GET", "literal:http://other.example.com/m", "none"],
            ["GET", "literal:/n?next=https://x", "computed:users"],
            ["GET", "literal:9p://not-a-scheme", "computed:users"],
        ]
        .map(|row| row.map(String::from))
    );
}

#[test]
fn unproven_webclient_chains_yield_no_fetch_sites() {
    // AC-0441: a wildcard import, a lookalike type, a file-local `WebClient`,
    // a variable obscuring `WebClient.create`, an untyped receiver, verb
    // calls of the wrong arity and a test source yield nothing.
    let source = |imports: &str, body: &str| {
        format!(
            "package io.demo;\n\n{imports}\nclass C {{\n    void calls(WebClient client) {{\n        {body}\n    }}\n}}\n"
        )
    };
    let proven = "import org.springframework.web.reactive.function.client.WebClient;\n";
    for (path, text) in [
        (
            "src/main/java/io/demo/Wild.java",
            source(
                "import org.springframework.web.reactive.function.client.*;\n",
                "client.get().uri(\"/w\").retrieve();",
            ),
        ),
        (
            "src/main/java/io/demo/Lookalike.java",
            source(
                "import com.acme.WebClient;\n",
                "client.get().uri(\"/l\").retrieve();",
            ),
        ),
        (
            "src/main/java/io/demo/Local.java",
            format!(
                "{}\ninterface WebClient {{}}\n",
                source(proven, "client.get().uri(\"/d\").retrieve();")
            ),
        ),
        (
            "src/main/java/io/demo/Obscured.java",
            source(
                proven,
                "Object WebClient = null; WebClient.create(\"http://x\").get().uri(\"/o\");",
            ),
        ),
        (
            "src/main/java/io/demo/Untyped.java",
            source(
                proven,
                "var other = make(); other.get().uri(\"/u\").retrieve();",
            ),
        ),
        (
            "src/main/java/io/demo/Arity.java",
            source(
                proven,
                "client.get(\"x\").uri(\"/g\"); client.method().uri(\"/m\");",
            ),
        ),
        (
            "src/test/java/io/demo/Test.java",
            source(proven, "client.get().uri(\"/t\").retrieve();"),
        ),
    ] {
        let sites = retrofit_sites(&[(path, text.as_str())]);
        assert!(sites.is_empty(), "{path}: {:?}", webclient_rows(&sites));
    }
}

fn kafka_rows(files: &[(&str, &str)]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        write(dir.path(), rel, content);
    }
    extract_dir(dir.path(), &id())
        .unwrap()
        .event_sites
        .iter()
        .map(|site| {
            assert_eq!(site.kind, "kafka-topic");
            assert!(site.registries.is_empty());
            let identity = match &site.identity {
                IdentityExpr::Literal(topic) => format!("lit:{topic}"),
                IdentityExpr::EnvRef(key) => format!("env:{key}"),
                IdentityExpr::Computed(text) => format!("computed:{text}"),
            };
            let owner = site.symbol.as_deref().unwrap_or("-");
            // The method name, without owner or signature.
            let owner = owner.split('(').next().unwrap_or(owner);
            let owner = owner.rsplit('.').next().unwrap_or(owner);
            format!("{:?} {identity} {owner}", site.role)
        })
        .collect()
}

// AC-0459: `send` on a proven `KafkaTemplate` field, `this.` field,
// parameter or local is a produces site whose topic is the first of two or
// more arguments, and each `topics` element of a proven `@KafkaListener`
// method is a consumes site; placeholders, expressions, non-literals, a
// record or message argument, a pattern and partitions are computed.
// (T-0459)
#[test]
fn spring_kafka_templates_and_listeners_become_event_sites() {
    let rows = kafka_rows(&[(
        "src/main/java/io/demo/Orders.java",
        r##"package io.demo;

import org.springframework.kafka.annotation.KafkaListener;
import org.springframework.kafka.core.KafkaTemplate;

public class Orders {
    static final String AUDIT = "audit";
    private final KafkaTemplate<String, String> template;

    void publish(String body, ProducerRecord<String, String> record) {
        template.send("orders", body);
        this.template.send("orders", 0, "k", body);
        template.send(AUDIT, body);
        template.send(record);
    }

    void relay(org.springframework.kafka.core.KafkaTemplate<String, String> given) {
        KafkaTemplate<String, String> local = given;
        local.send("relayed", "x");
    }

    @KafkaListener(topics = "orders", groupId = "billing")
    void onOrder(String body) {}

    @KafkaListener(topics = {"a", "${topic.b}", "#{'c'}"})
    void onMany(String body) {}

    @org.springframework.kafka.annotation.KafkaListener(topicPattern = "audit.*")
    void onPattern(String body) {}
}
"##,
    )]);
    assert_eq!(
        rows,
        [
            "Produces lit:orders publish",
            "Produces lit:orders publish",
            "Produces computed:AUDIT publish",
            "Produces computed:template.send(record) publish",
            "Produces lit:relayed relay",
            "Consumes lit:orders onOrder",
            "Consumes lit:a onMany",
            "Consumes computed:\"${topic.b}\" onMany",
            "Consumes computed:\"#{'c'}\" onMany",
            "Consumes computed:topicPattern = \"audit.*\" onPattern",
        ]
    );
}

// AC-0459: an unproven receiver (untyped, other type, wildcard-imported,
// shadowed by a declared type, an inherited field, `var`), a lookalike or
// wildcard-imported listener annotation, `sendDefault`, a class-level
// listener and test sources yield no event site. (T-0459)
#[test]
fn spring_kafka_sites_need_a_proven_template_or_listener() {
    let quiet = |rel: &str, source: &str| {
        let rows = kafka_rows(&[(rel, source)]);
        assert!(rows.is_empty(), "{rel}: {rows:?}");
    };
    quiet(
        "src/main/java/io/demo/A.java",
        r#"package io.demo;

import org.springframework.kafka.core.*;
import org.springframework.kafka.annotation.*;

class A {
    private final KafkaTemplate<String, String> template;
    void f() { template.send("t", "x"); }
    @KafkaListener(topics = "t")
    void g(String body) {}
}
"#,
    );
    quiet(
        "src/main/java/io/demo/B.java",
        r#"package io.demo;

import org.springframework.kafka.annotation.KafkaListener;
import org.springframework.kafka.core.KafkaTemplate;

@KafkaListener(topics = "class-level")
class B extends Base {
    private final Other template;
    void f(Object raw, KafkaProducer producer) {
        template.send("t", "x");
        raw.send("t", "x");
        producer.send("t", "x");
        inherited.send("t", "x");
        var local = new KafkaTemplate<String, String>(factory);
        local.send("t", "x");
        Base.this.template.send("t", "x");
    }
    void g(KafkaTemplate<String, String> given) { given.sendDefault("x"); }
    @KafkaHandler
    void h(String body) {}
    @io.other.KafkaListener(topics = "t")
    void i(String body) {}
}
"#,
    );
    quiet(
        "src/main/java/io/demo/C.java",
        r#"package io.demo;

import org.springframework.kafka.annotation.KafkaListener;
import org.springframework.kafka.core.KafkaTemplate;

class C {
    class KafkaTemplate {}
    @interface KafkaListener { String[] topics(); }
    void f(KafkaTemplate template) { template.send("t", "x"); }
    @KafkaListener(topics = "t")
    void g(String body) {}
}
"#,
    );
    let product = r#"package io.demo;

import org.springframework.kafka.core.KafkaTemplate;

class D {
    void f(KafkaTemplate<String, String> template) { template.send("t", "x"); }
}
"#;
    quiet("src/test/java/io/demo/D.java", product);
    // Control: the same source in a product source set is a site.
    assert_eq!(
        kafka_rows(&[("src/main/java/io/demo/D.java", product)]).len(),
        1
    );
}

#[test]
fn java_net_http_request_builders_become_fetch_sites() {
    // AC-0475 (#1395): import-proven and fully qualified `HttpRequest`
    // builders; the last method link wins (default GET), the last `uri`
    // wins over the `newBuilder` argument, `method` keeps a token literal
    // without lowercase letters only (#1396: `M-SEARCH`), and a URL is literal only through a proven `URI`.
    let sites = retrofit_sites(&[(
        "src/main/java/io/demo/Client.java",
        r#"package io.demo;

import java.net.URI;
import java.net.http.HttpRequest;
import java.time.Duration;

class Client {
    void calls(String path, URI target) throws Exception {
        HttpRequest.newBuilder(URI.create("https://host/a")).build();
        HttpRequest.newBuilder().uri(new URI("https://host/b")).DELETE().build();
        HttpRequest.newBuilder(URI.create("https://host/old")).uri(URI.create("https://host/c"))
            .header("A", "1").headers("B", "2", "C", "3").setHeader("D", "4")
            .timeout(Duration.ofSeconds(5)).PUT(null).POST(null).build();
        HttpRequest.newBuilder().uri(URI.create("https://host/" + path)).GET().build();
        HttpRequest.newBuilder().uri(target).method("PATCH", null).build();
        HttpRequest.newBuilder().uri(URI.create("https://host/d")).method("patch", null).build();
        HttpRequest.newBuilder().uri(URI.create("https://host/s")).method("M-SEARCH", null).build();
        HttpRequest.newBuilder().uri(URI.create("https://host/s")).method("GET /", null).build();
        java.net.http.HttpRequest.newBuilder(java.net.URI.create("https://host/e")).HEAD().build();
    }
}
"#,
    )]);
    assert_eq!(
        webclient_rows(&sites),
        [
            ["GET", "literal:https://host/a", "none"],
            ["DELETE", "literal:https://host/b", "none"],
            ["POST", "literal:https://host/c", "none"],
            ["GET", "computed:\"https://host/\" + path", "none"],
            ["PATCH", "computed:target", "none"],
            ["?", "literal:https://host/d", "none"],
            ["M-SEARCH", "literal:https://host/s", "none"],
            ["?", "literal:https://host/s", "none"],
            ["HEAD", "literal:https://host/e", "none"],
        ]
    );
}

#[test]
fn unproven_java_net_http_builders_are_not_fetch_sites() {
    // AC-0475: a foreign or obscured `HttpRequest`, a builder with a link
    // that is no builder member or the wrong arity, a two-argument
    // `newBuilder`, a held builder and a chain without a URI yield none;
    // an unproven `URI.create` gives a computed URL; test sources yield none.
    let sites = retrofit_sites(&[
        (
            "src/main/java/io/demo/Client.java",
            r#"package io.demo;

import java.net.http.HttpRequest;

class Client {
    void calls(HttpRequest base, Object HttpRequest2) throws Exception {
        HttpRequest.newBuilder(URI.create("https://host/unproven-uri")).build();
        HttpRequest.newBuilder(URI.create("https://host/x")).copy().build();
        HttpRequest.newBuilder(URI.create("https://host/x")).GET(1).build();
        HttpRequest.newBuilder(URI.create("https://host/x")).headers("A").build();
        HttpRequest.newBuilder(base, (k, v) -> true).build();
        HttpRequest.Builder held = HttpRequest.newBuilder(URI.create("https://host/x"));
        held.build();
        HttpRequest.newBuilder().GET().build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Shadow.java",
            r#"package io.demo;

import java.net.URI;
import java.net.http.HttpRequest;

class Shadow {
    void calls(Object HttpRequest) {
        HttpRequest.newBuilder(URI.create("https://host/y")).build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Foreign.java",
            r#"package io.demo;

import java.net.URI;
import com.acme.HttpRequest;

class Foreign {
    void calls() {
        HttpRequest.newBuilder(URI.create("https://host/z")).build();
    }
}
"#,
        ),
        (
            "src/test/java/io/demo/ClientTest.java",
            r#"package io.demo;

import java.net.URI;
import java.net.http.HttpRequest;

class ClientTest {
    void calls() {
        HttpRequest.newBuilder(URI.create("https://host/t")).build();
    }
}
"#,
        ),
    ]);
    assert_eq!(
        webclient_rows(&sites),
        [[
            "GET",
            "computed:URI.create(\"https://host/unproven-uri\")",
            "none"
        ]]
    );
}

#[test]
fn okhttp_request_builders_become_fetch_sites() {
    // AC-0476 (#1405): `Request`-import, nested `Builder`-import and fully
    // qualified (also unimported and spaced) OkHttp builders; the last method link wins (default GET),
    // the last `url` wins, `method` keeps a token literal without lowercase
    // letters only, and only a string `url` argument is a literal.
    let sites = retrofit_sites(&[
        (
            "src/main/java/io/demo/Client.java",
            r#"package io.demo;

import okhttp3.CacheControl;
import okhttp3.Request;
import okhttp3.RequestBody;

class Client {
    void calls(String path, okhttp3.HttpUrl target, RequestBody body) {
        new Request.Builder().url("https://host/a").build();
        new Request.Builder().url("https://host/b").delete().build();
        new Request.Builder().url("https://host/old").url("https://host/c")
            .header("A", "1").addHeader("B", "2").removeHeader("C").tag("t")
            .tag(String.class, "u").cacheControl(CacheControl.FORCE_NETWORK)
            .put(body).post(body).build();
        new Request.Builder().url("https://host/" + path).get().build();
        new Request.Builder().url(target).patch(body).build();
        new Request.Builder().url("https://host/d").method("patch", body).build();
        new Request.Builder().url("https://host/p").method("PROPFIND", null).build();
        new Request.Builder().head().url("https://host/h").delete(body).build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Nested.java",
            r#"package io.demo;

import okhttp3.Request.Builder;

class Nested {
    void calls() {
        new Builder().url("https://host/e").build();
        new okhttp3.Request.Builder().url("https://host/f").head().build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Bare.java",
            r#"package io.demo;

class Bare {
    void calls() {
        new okhttp3 . Request . Builder().url("https://host/g").build();
    }
}
"#,
        ),
    ]);
    assert_eq!(
        webclient_rows(&sites),
        [
            ["GET", "literal:https://host/g", "none"],
            ["GET", "literal:https://host/a", "none"],
            ["DELETE", "literal:https://host/b", "none"],
            ["POST", "literal:https://host/c", "none"],
            ["GET", "computed:\"https://host/\" + path", "none"],
            ["PATCH", "computed:target", "none"],
            ["?", "literal:https://host/d", "none"],
            ["PROPFIND", "literal:https://host/p", "none"],
            ["DELETE", "literal:https://host/h", "none"],
            ["GET", "literal:https://host/e", "none"],
            ["HEAD", "literal:https://host/f", "none"],
        ]
    );
}

#[test]
fn unproven_okhttp_builders_are_not_fetch_sites() {
    // AC-0476: a foreign, file-declared or unimported `Request`, a declared
    // or imported `okhttp3` lookalike (connector P2 on #1406), an
    // anonymous builder subclass, an unknown or wrong-arity link, a held
    // builder and a chain without `url` yield none; test sources yield none.
    let sites = retrofit_sites(&[
        (
            "src/main/java/io/demo/Client.java",
            r#"package io.demo;

import okhttp3.Request;

class Client {
    void calls(Request.Builder base) {
        new Request.Builder().url("https://host/x").newBuilder().build();
        new Request.Builder().url("https://host/x").get(1).build();
        new Request.Builder().url("https://host/x").post().build();
        new Request.Builder().url("https://host/x").header("A").build();
        new Request.Builder() {}.url("https://host/x").build();
        new Request.Builder("seed").url("https://host/x").build();
        Request.Builder held = new Request.Builder().url("https://host/x");
        held.build();
        base.url("https://host/x").build();
        new Request.Builder().get().build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Foreign.java",
            r#"package io.demo;

import com.acme.Request;

class Foreign {
    void calls() {
        new Request.Builder().url("https://host/z").build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Local.java",
            r#"package io.demo;

import okhttp3.Request;

class Local {
    static class Request { static class Builder {} }
    void calls() {
        new Request.Builder().url("https://host/l").build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Lookalike.java",
            r#"package io.demo;

class Lookalike {
    static class okhttp3 { static class Request { static class Builder {} } }
    void calls() {
        new okhttp3.Request.Builder().url("https://host/k").build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/ImportedLookalike.java",
            r#"package io.demo;

import com.acme.okhttp3;

class ImportedLookalike {
    void calls() {
        new okhttp3.Request.Builder().url("https://host/m").build();
    }
}
"#,
        ),
        (
            "src/main/java/io/demo/Unimported.java",
            r#"package io.demo;

class Unimported {
    void calls() {
        new Request.Builder().url("https://host/u").build();
    }
}
"#,
        ),
        (
            "src/test/java/io/demo/ClientTest.java",
            r#"package io.demo;

import okhttp3.Request;

class ClientTest {
    void calls() {
        new Request.Builder().url("https://host/t").build();
    }
}
"#,
        ),
    ]);
    assert_eq!(webclient_rows(&sites), Vec::<[&str; 3]>::new());
}
