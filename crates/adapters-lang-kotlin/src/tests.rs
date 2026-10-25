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

// AC-0098: every Kotlin declaration kind — classes, interfaces, objects,
// data classes, enums, and top-level/member/extension functions — becomes a
// Confirmed T0 Symbol with exact evidence spans.
#[test]
fn declaration_kinds_and_functions_carry_confirmed_provenance() {
    let source = br#"package com.demo

class Greeter {
    fun hello(): String = greet()

    private fun greet(): String = this.suffix()

    private fun suffix(): String = "!"
}

interface Repo
object Config
data class User(val name: String)
enum class Role { ADMIN, USER }

fun topLevel() {
    listOf(1, 2, 3).map { it * 2 }
}

suspend fun refresh() {
    topLevel()
}

fun String.shout(): String = this.uppercase()
"#;
    let path = "src/main/kotlin/com/demo/Greeter.kt";
    let out = extract_source(source, path, &id()).unwrap();

    let sym = |name: &str| format!("sym:local/demo@{path}#{name}");
    assert_eq!(node(&out.nodes, &sym("Greeter")).props["kind"], "class");
    assert_eq!(node(&out.nodes, &sym("Repo")).props["kind"], "interface");
    assert_eq!(node(&out.nodes, &sym("Config")).props["kind"], "object");
    assert_eq!(node(&out.nodes, &sym("User")).props["kind"], "data class");
    assert_eq!(node(&out.nodes, &sym("Role")).props["kind"], "enum class");
    assert_eq!(
        node(&out.nodes, &sym("Greeter.hello")).props["kind"],
        "function"
    );
    assert_eq!(node(&out.nodes, &sym("topLevel")).props["kind"], "function");
    // `suspend` and trailing-lambda syntax parse (grammar verify-at-build);
    // the suspend function is a plain function symbol calling another
    // top-level function of the same file.
    edge(&out.edges, &sym("refresh"), &sym("topLevel"), "CALLS");
    // Extension functions are qualified by their receiver type.
    let shout = node(&out.nodes, &sym("String.shout"));
    assert_eq!(shout.props["kind"], "function");
    assert_eq!(shout.props["receiver"], "String");

    // Unqualified and `this.` calls resolve within the class.
    edge(
        &out.edges,
        &sym("Greeter.hello"),
        &sym("Greeter.greet"),
        "CALLS",
    );
    edge(
        &out.edges,
        &sym("Greeter.greet"),
        &sym("Greeter.suffix"),
        "CALLS",
    );
    edge(
        &out.edges,
        &sym("Greeter.hello"),
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
        assert_eq!(prov["extractor_id"], "t0.adapter-kotlin");
        let end = prov["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end <= source.len() as u64 && end > 0);
    }
}

// AC-0098: import-proven cross-file calls — object/type receivers and
// imported top-level functions — join repo-wide; a declared-package import
// whose target cannot be proven fails closed to an explicit Gap; calls on
// foreign packages assert nothing; unproven receivers (locals, properties)
// assert nothing.
#[test]
fn imported_calls_resolve_across_files_and_missing_targets_gap() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import com.demo.util.Store
import com.demo.util.publish
import com.demo.util.Missing
import org.external.Lib

class App(private val helper: Store) {
    fun run() {
        Store.save()
        publish()
        Missing.run()
        Lib.emit()
        helper.reload()
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/util/Store.kt",
        r#"package com.demo.util

object Store {
    fun save() {}
}

fun publish() {}
"#,
    );
    let (out, stats) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    assert_eq!(stats.recomputed_files, 2);

    let src = "sym:local/demo@app/src/main/kotlin/com/demo/App.kt#App.run";
    let store_save = "sym:local/demo@app/src/main/kotlin/com/demo/util/Store.kt#Store.save";
    let publish = "sym:local/demo@app/src/main/kotlin/com/demo/util/Store.kt#publish";
    // Import-proven receiver call resolves to the declaring file's symbol.
    let resolved = edge(&out.edges, src, store_save, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");
    // An imported top-level function resolves the same way.
    let resolved = edge(&out.edges, src, publish, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");

    // A declared-package import with no such declaration fails closed.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("missing-target import gaps");
    assert_eq!(gap.props["reason"], "unresolved Kotlin import target");
    assert_eq!(gap.props["callee"], "Missing.run");
    edge(&out.edges, src, &gap.id, "CALLS");

    // A call on a foreign-package import asserts nothing, and an unproven
    // receiver (the `helper` property) asserts nothing — no CALLS edge, no
    // extra Gap (the foreign import itself is a Confirmed external, AC-0207).
    assert_eq!(
        out.nodes.iter().filter(|node| node.label == "Gap").count(),
        1
    );
    assert!(
        !out.edges
            .iter()
            .any(|edge| edge.label == "CALLS" && edge.dst.contains("reload"))
    );
}

// AC-0098: duplicate FQNs (the same declaration in two source roots) are
// ambiguous and fail closed to a Gap — never resolved to whichever file
// sorts last (#170 standard).
#[test]
fn duplicate_declarations_are_ambiguous_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import com.demo.util.Store

class App {
    fun run() {
        Store.save()
    }
}
"#,
    );
    for root in ["src/main/kotlin", "src/test/kotlin"] {
        write(
            dir.path(),
            &format!("{root}/com/demo/util/Store.kt"),
            r#"package com.demo.util

object Store {
    fun save() {}
}
"#,
        );
    }
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let src = "sym:local/demo@src/main/kotlin/com/demo/App.kt#App.run";
    assert!(!out.edges.iter().any(|edge| {
        edge.label == "CALLS" && edge.src == src && edge.dst.contains("Store.save")
    }));
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("ambiguous duplicate declarations gap");
    assert_eq!(gap.props["callee"], "Store.save");
    edge(&out.edges, src, &gap.id, "CALLS");
}

// AC-0098: Spring Web endpoints in Kotlin compose class-level
// @RequestMapping with method-level mappings, and lookalike annotations
// without the proving import produce nothing. The controller fixture keeps
// trailing top-level declarations after the class on purpose: that is the
// shape that makes tree-sitter-kotlin-ng resolve the class annotations
// into a preceding `annotated_expression` statement (grammar quirk, #212),
// so this exercises the recovery path, not just the plain modifiers path.
#[test]
fn spring_endpoints_compose_class_and_method_paths_and_fail_closed_without_import() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/UserController.kt",
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/api/users")
class UserController {
    @GetMapping("/{id}")
    fun get(id: String): String = "u"

    @PostMapping
    fun create(): String = "c"
}

data class UserDto(val name: String)
"#,
    );
    // Lookalike annotations with no Spring import prove nothing.
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/FakeController.kt",
        r#"package com.demo.web

import com.other.web.RestController
import com.other.web.GetMapping

@RestController
class FakeController {
    @GetMapping("/nope")
    fun get(): String = "n"
}

object Marker
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let get = node(&out.nodes, "ep:local/demo@GET:/api/users/{id}");
    assert_eq!(get.props["framework"], "spring");
    assert_eq!(get.props["language"], "kotlin");
    assert_eq!(get.props["path"], "/api/users/{id}");
    let post = node(&out.nodes, "ep:local/demo@POST:/api/users");
    assert_eq!(post.props["method"], "POST");
    edge(
        &out.edges,
        "ep:local/demo@GET:/api/users/{id}",
        "sym:local/demo@src/main/kotlin/com/demo/web/UserController.kt#UserController.get",
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

// AC-0098 (per-package proof, the AC-0080 standard): a wildcard of one
// Spring package must not prove an annotation living in another, and a
// named import from the wrong Spring package proves nothing either.
#[test]
fn wildcard_proof_is_per_annotation_package_not_per_vendor() {
    let source = br#"package com.demo.web

import org.springframework.stereotype.*

@Controller
class HomeController {
    @GetMapping("/home")
    fun home(): String = "h"
}
"#;
    let out = extract_source(
        source,
        "src/main/kotlin/com/demo/web/HomeController.kt",
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

    let wrong = br#"package com.demo.web

import org.springframework.stereotype.RestController
import org.springframework.stereotype.GetMapping

@RestController
class WrongController {
    @GetMapping("/wrong")
    fun wrong(): String = "w"
}
"#;
    let out = extract_source(
        wrong,
        "src/main/kotlin/com/demo/web/WrongController.kt",
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

// AC-0098: named Spring imports (including `path =` named arguments) prove
// mappings; a mapping whose path argument is present but not a provable
// literal (a `$` template, a constant reference) is a runtime identity —
// it becomes an explicit route Gap, never a Confirmed endpoint with a
// guessed, interpolated, or defaulted path (#218 review).
#[test]
fn named_spring_imports_prove_mappings_and_dynamic_paths_gap() {
    let source = br#"package com.demo.web

import org.springframework.web.bind.annotation.RestController
import org.springframework.web.bind.annotation.DeleteMapping
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PostMapping

@RestController
class AdminController {
    @DeleteMapping(path = "/admin/cache")
    fun purge() {}

    @GetMapping("/admin/$suffix")
    fun dynamic(): String = "d"

    @PostMapping(path = ROUTE)
    fun constant(): String = "c"
}
"#;
    let path = "src/main/kotlin/com/demo/web/AdminController.kt";
    let out = extract_source(source, path, &id()).unwrap();
    let endpoint = node(&out.nodes, "ep:local/demo@DELETE:/admin/cache");
    assert_eq!(endpoint.props["method"], "DELETE");
    assert_eq!(
        endpoint.props["handler_sym"],
        format!("sym:{}@{path}#AdminController.purge", "local/demo")
    );
    // The literal mapping is the only Confirmed endpoint: the template and
    // constant-reference paths must not surface as endpoints at all — not
    // interpolated, not collapsed to the class base or "/".
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        1
    );
    // Each unprovable mapping is an explicit Gap that still names its
    // handler, so the partial fact is visible rather than silently absent.
    let gaps: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .collect();
    assert_eq!(gaps.len(), 2);
    for (gap, method, handler) in [
        (gaps[0], "GET", "AdminController.dynamic"),
        (gaps[1], "POST", "AdminController.constant"),
    ] {
        assert_eq!(gap.props["reason"], "dynamic Spring mapping path");
        assert_eq!(gap.props["method"], method);
        assert_eq!(
            gap.props["handler_sym"],
            format!("sym:local/demo@{path}#{handler}")
        );
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
        edge(
            &out.edges,
            &gap.id,
            &format!("sym:local/demo@{path}#{handler}"),
            "HANDLES",
        );
    }
}

// AC-0098 (#218 review): a *class-level* @RequestMapping whose path is a
// runtime identity poisons every mapping beneath it — even fully literal
// method paths cannot compose into a Confirmed route, so each one fails
// closed to a route Gap. An argument list with no path-designating
// argument, by contrast, is Spring's own documented default ("") and
// composes normally.
#[test]
fn dynamic_class_base_paths_fail_closed_and_non_path_arguments_default() {
    let source = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping(BASE)
class DynController {
    @GetMapping("/literal")
    fun get(): String = "g"
}
"#;
    let out = extract_source(
        source,
        "src/main/kotlin/com/demo/web/DynController.kt",
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
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("dynamic base gap");
    assert_eq!(gap.props["reason"], "dynamic Spring mapping path");

    // Named non-path arguments (produces, consumes, …) do not designate a
    // path: the mapping keeps Spring's default "" and composes Confirmed.
    let defaulted = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/api")
class JsonController {
    @GetMapping(produces = "application/json")
    fun all(): String = "[]"
}
"#;
    let out = extract_source(
        defaulted,
        "src/main/kotlin/com/demo/web/JsonController.kt",
        &id(),
    )
    .unwrap();
    let endpoint = node(&out.nodes, "ep:local/demo@GET:/api");
    assert_eq!(endpoint.props["path"], "/api");
    assert!(!out.nodes.iter().any(|node| node.label == "Gap"));
}

// AC-0098: content-identical files are reused from the incremental cache
// (with provenance retargeted to the new commit), changed files are
// re-parsed, and deleted files leave the cache.
#[test]
fn incremental_cache_reuses_unchanged_files_and_retargets_commit() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/A.kt",
        "package com.demo\n\nclass A {\n    fun a() {}\n}\n",
    );
    write(
        dir.path(),
        "src/B.kt",
        "package com.demo\n\nclass B {\n    fun b() {}\n}\n",
    );
    let mut cache = IncrementalCache::default();
    let (first, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 2);
    assert_eq!(stats.reused_files, 0);

    let second_id = SourceId {
        repo: "local/demo",
        commit: "abc123",
    };
    let (second, stats) = extract_dir_incremental(dir.path(), &second_id, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    assert_eq!(stats.reused_files, 2);
    assert_eq!(first.nodes.len(), second.nodes.len());
    let a = node(&second.nodes, "sym:local/demo@src/A.kt#A");
    assert_eq!(a.props["prov"]["evidence"][0]["commit_sha"], "abc123");

    // A changed file re-parses; a deleted file leaves the cache.
    write(
        dir.path(),
        "src/A.kt",
        "package com.demo\n\nclass A {\n    fun a2() {}\n}\n",
    );
    std::fs::remove_file(dir.path().join("src/B.kt")).unwrap();
    let (third, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 0);
    assert_eq!(stats.deleted_files, 1);
    node(&third.nodes, "sym:local/demo@src/A.kt#A.a2");
    assert!(!third.nodes.iter().any(|node| node.id.contains("B.kt")));
}

// AC-0098 (#209 hook): the progress callback sees every Kotlin file once,
// as a repo-relative path, in the deterministic sorted walk order — cache
// hits included.
#[test]
fn progress_hook_reports_each_file_in_sorted_order() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/b/Late.kt", "package b\n\nclass Late\n");
    write(dir.path(), "src/a/Early.kt", "package a\n\nclass Early\n");
    write(dir.path(), "build.gradle.kts", "plugins { }\n");
    let mut cache = IncrementalCache::default();
    let mut seen = Vec::new();
    let mut on_file = |path: &str| seen.push(path.to_string());
    extract_dir_incremental_with_progress(dir.path(), &id(), &mut cache, &mut on_file).unwrap();
    assert_eq!(
        seen,
        ["build.gradle.kts", "src/a/Early.kt", "src/b/Late.kt"]
    );

    // Reused files still report — the hook narrates reads, not parses.
    let mut seen_again = Vec::new();
    let mut on_file = |path: &str| seen_again.push(path.to_string());
    extract_dir_incremental_with_progress(dir.path(), &id(), &mut cache, &mut on_file).unwrap();
    assert_eq!(seen, seen_again);
}

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("src/App.kt", ""),
        ("gen/Api.kt", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_kotlin_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["src/App.kt"]);
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
    let slashed_base = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/api/")
class ApiController {
    @GetMapping
    fun root(): String = "r"

    @PostMapping("/items")
    fun create(): String = "c"
}
"#;
    let out = extract_source(
        slashed_base,
        "src/main/kotlin/com/demo/web/ApiController.kt",
        &id(),
    )
    .unwrap();
    assert_eq!(endpoint_routes(&out), ["GET /api/", "POST /api/items"]);

    let slashed_tail = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/v1")
class V1Controller {
    @GetMapping("/")
    fun root(): String = "r"

    @GetMapping("/x")
    fun x(): String = "x"

    @GetMapping("/x/")
    fun xSlash(): String = "xs"
}
"#;
    let out = extract_source(
        slashed_tail,
        "src/main/kotlin/com/demo/web/V1Controller.kt",
        &id(),
    )
    .unwrap();
    assert_eq!(
        endpoint_routes(&out),
        ["GET /v1/", "GET /v1/x", "GET /v1/x/"]
    );
    assert!(!out.nodes.iter().any(|node| node.id.contains("//")));

    // A run of trailing slashes keeps exactly one.
    let doubled = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/v2//")
class V2Controller {
    @GetMapping
    fun root(): String = "r"

    @GetMapping("items//")
    fun items(): String = "i"
}
"#;
    let out = extract_source(
        doubled,
        "src/main/kotlin/com/demo/web/V2Controller.kt",
        &id(),
    )
    .unwrap();
    assert_eq!(endpoint_routes(&out), ["GET /v2/", "GET /v2/items/"]);
}

// AC-0207 (#237): import targets carry provenance; only a package the
// repository (Kotlin or Java sources) provably does not declare confirms as
// an external boundary. In-repo functions resolve to their declaring File.
#[test]
fn import_targets_carry_provenance_and_only_proven_externals_confirm() {
    let dir = tempfile::tempdir().unwrap();
    let app = "package com.demo\n\nimport com.demo.util.helper\nimport com.demo.legacy.JavaThing\nimport kotlinx.coroutines.launch\n\nfun main() {}\n";
    write(dir.path(), "src/main/kotlin/com/demo/App.kt", app);
    write(
        dir.path(),
        "src/main/kotlin/com/demo/util/Util.kt",
        "package com.demo.util\n\nfun helper() {}\n",
    );
    // A Java package in a mixed JVM tree is the repository's own.
    write(
        dir.path(),
        "src/main/java/com/demo/legacy/Other.java",
        "// header\npackage com.demo.legacy;\n\nclass Other {}\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let app_file = "file:local/demo@src/main/kotlin/com/demo/App.kt";
    edge(
        &out.edges,
        app_file,
        "file:local/demo@src/main/kotlin/com/demo/util/Util.kt",
        "IMPORTS",
    );
    let check = |id: &str, confidence: ConfidenceTier, boundary: &str, statement: &str| {
        let node = node(&out.nodes, id);
        let prov: Provenance = serde_json::from_value(node.props["prov"].clone())
            .unwrap_or_else(|_| panic!("{id} carries no provenance"));
        assert_eq!(prov.confidence_tier, confidence, "{id}");
        assert_eq!(node.props["boundary"], boundary, "{id}");
        let span = &prov.evidence[0];
        assert_eq!(
            &app[span.byte_start as usize..span.byte_end as usize],
            statement
        );
    };
    // #464 (ADR-0033): a proven-external import keys on its package, not the
    // imported name — but only when the JVM capitalization convention can
    // find the package/type boundary. A Kotlin top-level function import
    // (`launch`) has no capitalized segment at all, so there is nothing to
    // split on and the id keeps the full imported path unchanged.
    check(
        "mod:kotlinx.coroutines.launch",
        ConfidenceTier::Confirmed,
        "external",
        "import kotlinx.coroutines.launch",
    );
    check(
        "mod:com.demo.legacy.JavaThing",
        ConfidenceTier::Gap,
        "unresolved",
        "import com.demo.legacy.JavaThing",
    );
}

// --- Retrofit client calls (#737) ------------------------------------------

const RETROFIT_API: &str = r#"package io.demo.network

import retrofit2.http.GET
import retrofit2.http.HTTP
import retrofit2.http.POST
import retrofit2.http.Path
import com.other.GET as OtherGet

interface ApiService {
    @GET("api/stats")
    suspend fun stats(): Response<Stats>

    @POST(value = "api/auth/login")
    suspend fun login(@Body body: Login): Response<Token>

    @HTTP(method = "DELETE", path = "api/mailboxes/{id}", hasBody = true)
    suspend fun deleteMailbox(@Path("id") id: Int): Response<Ok>

    @GET(PREFIX + "aliases")
    suspend fun aliases(): Response<Aliases>

    @retrofit2.http.PUT("api/setting")
    suspend fun setting(): Response<Ok>
}

class NotAnInterface {
    @GET("api/never")
    fun never() {}
}
"#;

fn retrofit_sites(files: &[(&str, &str)]) -> Vec<adapters_fw::client::FetchSite> {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        write(dir.path(), rel, content);
    }
    extract_dir(dir.path(), &id()).unwrap().fetch_sites
}

// AC-0315: import-proven Retrofit annotations on interface methods become
// client calls with a literal path, a dynamic path as Computed, the entry
// name and the repo's one literal builder base joined by resolution. A
// class method, an unproven annotation and an `@HTTP` verb all follow the
// same proof. (T-0315)
#[test]
fn retrofit_interface_methods_become_joined_client_calls() {
    let module = r#"package io.demo.network

import retrofit2.Retrofit

fun provide() = Retrofit.Builder()
    .baseUrl("https://app.example.io")
    .build()
"#;
    let other = r#"package io.demo.other

import com.other.GET

interface Unrelated {
    @GET("api/unrelated")
    fun unrelated()
}
"#;
    let sites = retrofit_sites(&[
        ("net/ApiService.kt", RETROFIT_API),
        ("net/Module.kt", module),
        ("other/Unrelated.kt", other),
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
            IdentityExpr::Literal("https://app.example.io".into())
        );
        assert_eq!(base.join, adapters_fw::client::BaseJoin::Resolve);
        assert_eq!(base.path, "net/Module.kt");
        assert_eq!(site.path, "net/ApiService.kt");
    }
    assert_eq!(
        sites[0].symbol.as_deref(),
        Some("sym:local/demo@net/ApiService.kt#ApiService.stats")
    );
}

// AC-0315: a non-literal builder base stays Computed with its text, and two
// distinct bases cannot be chosen between. (T-0315)
#[test]
fn retrofit_bases_must_be_unique_and_keep_their_text() {
    let builder = |url: &str| {
        format!(
            "package io.demo\n\nimport retrofit2.Retrofit\n\nval r = Retrofit.Builder().baseUrl({url}).build()\n"
        )
    };
    let computed = retrofit_sites(&[
        ("ApiService.kt", RETROFIT_API),
        ("Module.kt", &builder("Constants.BASE_URL")),
    ]);
    assert_eq!(
        computed[0].base.as_ref().unwrap().url,
        IdentityExpr::Computed("Constants.BASE_URL".into())
    );
    let two = retrofit_sites(&[
        ("ApiService.kt", RETROFIT_API),
        ("A.kt", &builder("\"https://a.io\"")),
        ("B.kt", &builder("\"https://b.io\"")),
    ]);
    assert_eq!(
        two[0].base.as_ref().unwrap().url,
        IdentityExpr::Computed("2 distinct Retrofit base URL candidates".into())
    );
    // An unimported `Retrofit` builder proves nothing.
    let unproven = retrofit_sites(&[
        ("ApiService.kt", RETROFIT_API),
        (
            "Module.kt",
            "package io.demo\n\nval r = Retrofit.Builder().baseUrl(\"https://a.io\").build()\n",
        ),
    ]);
    assert_eq!(
        unproven[0].base.as_ref().unwrap().url,
        IdentityExpr::Computed("no Retrofit base URL in repo".into())
    );
}

// AC-0098 (#721): imports of extension functions (overloads included),
// top-level properties, type aliases, annotation classes (the grammar's
// expression-shaped parse too) and enum entries name their declaring file;
// a name declared in two files, or not at all, keeps its `mod:` endpoint.
#[test]
fn non_symbol_import_targets_resolve_to_their_declaring_file() {
    let dir = tempfile::tempdir().unwrap();
    let common = "src/main/kotlin/com/demo/common/Common.kt";
    write(
        dir.path(),
        common,
        r#"package com.demo.common

@Qualifier
@Retention(AnnotationRetention.BINARY)
annotation class LoadingState

typealias LoadingFlow = MutableStateFlow<Boolean>

const val PAGE_SIZE = 20

fun String.format(): String = this
fun Int.format(): String = toString()

enum class Theme(val dark: Boolean) {
    @SerializedName("dark")
    DARK(true),
    LIGHT(false);
}

fun String.twice(): String = this
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/common/Other.kt",
        "package com.demo.common\n\nfun String.twice(): String = this\n",
    );
    // A plain function in one file and an extension of that name in another.
    write(
        dir.path(),
        "src/main/kotlin/com/demo/common/Plain.kt",
        "package com.demo.common\n\nfun mixed() = Unit\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/common/Ext.kt",
        "package com.demo.common\n\nfun String.mixed() = this\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import com.demo.common.LoadingState
import com.demo.common.LoadingFlow
import com.demo.common.PAGE_SIZE
import com.demo.common.format
import com.demo.common.Theme.DARK
import com.demo.common.twice
import com.demo.common.missing
import com.demo.common.mixed

class App
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    let app = "file:local/demo@src/main/kotlin/com/demo/App.kt";
    let targets = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS" && edge.src == app)
        .map(|edge| {
            (
                edge.props["specifier"].as_str().unwrap().to_string(),
                edge.dst.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let declaring = format!("file:local/demo@{common}");
    for name in [
        "LoadingState",
        "LoadingFlow",
        "PAGE_SIZE",
        "format",
        "Theme.DARK",
    ] {
        assert_eq!(
            targets[&format!("com.demo.common.{name}")],
            declaring,
            "{name}"
        );
    }
    assert_eq!(
        targets["com.demo.common.twice"],
        "mod:com.demo.common.twice"
    );
    assert_eq!(
        targets["com.demo.common.missing"],
        "mod:com.demo.common.missing"
    );
    assert_eq!(
        targets["com.demo.common.mixed"],
        "mod:com.demo.common.mixed"
    );
}

// AC-0098 (#721): constructing an imported repository class is not an
// unresolved call (constructor calls are not modeled, as for a same-file
// class); an ambiguous class, an object's or interface's `invoke` call, a
// class declaring `invoke` and an undeclared generated member still gap, and
// a factory function of the same name still resolves.
#[test]
fn imported_class_constructor_calls_are_not_unresolved_gaps() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/model/Body.kt",
        "package com.demo.model\n\ndata class LoginBody(val email: String)\n\nclass Session\n\nfun Session(): Session = TODO()\n",
    );
    for root in ["src/main/kotlin", "src/test/kotlin"] {
        write(
            dir.path(),
            &format!("{root}/com/demo/model/Twice.kt"),
            "package com.demo.model\n\nclass Twice\n",
        );
    }
    write(
        dir.path(),
        "src/main/kotlin/com/demo/model/Factory.kt",
        "package com.demo.model\n\nobject Factory {\n    operator fun invoke() = 1\n}\n\ninterface Maker {\n    companion object {\n        operator fun invoke() = 1\n    }\n}\n\nclass Built {\n    companion object {\n        operator fun invoke(n: Int) = n\n    }\n}\n",
    );
    // `invoke` declared as an extension in another file: only the
    // declaration kind keeps these from passing as constructor calls.
    write(
        dir.path(),
        "src/main/kotlin/com/demo/model/Remote.kt",
        "package com.demo.model\n\nobject Thing\n\ninterface Port {\n    companion object\n}\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/model/RemoteInvoke.kt",
        "package com.demo.model\n\noperator fun Thing.invoke() = 1\n\noperator fun Port.Companion.invoke() = 1\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import com.demo.model.Built
import com.demo.model.Factory
import com.demo.model.LoginBody
import com.demo.model.Maker
import com.demo.model.Port
import com.demo.model.Thing
import com.demo.model.Session
import com.demo.model.Twice

fun run() {
    LoginBody("a@b.c")
    Session()
    Twice()
    Factory()
    Maker()
    Built(1)
    Thing()
    Port()
    LoginBody.serializer()
}
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    let gaps = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| node.props["callee"].as_str().unwrap().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        gaps,
        BTreeSet::from([
            "Twice".to_string(),
            "Factory".to_string(),
            "Maker".to_string(),
            "Built".to_string(),
            "Thing".to_string(),
            "Port".to_string(),
            "LoginBody.serializer".to_string(),
        ])
    );
    edge(
        &out.edges,
        "sym:local/demo@src/main/kotlin/com/demo/App.kt#run",
        "sym:local/demo@src/main/kotlin/com/demo/model/Body.kt#Session",
        "CALLS",
    );
}

// AC-0351 (T-0351): import-proven Android Navigation 3 `entryProvider`
// entries become key-based `Screen` nodes with Confirmed `RENDERS` edges to
// resolved composable functions (ignoring callback arguments and failing
// closed on undeclared keys), and back-stack `add` calls — direct, parameter
// helper, and builder helper — become InferredStrong `NAVIGATES_TO` edges or
// explicit `navigation-unresolved` Gaps.
#[test]
fn navigation3_entries_become_screens_and_back_stack_adds_navigate() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/io/simplelogin/android/screens/Screens.kt",
        r#"package io.simplelogin.android.screens

fun LoginMasterScreen() {}
fun HomeTopBar(onSettings: () -> Unit) {
    onSettings()
}
fun HomeScreen(apiKey: String, onCreateAlias: () -> Unit, onShowSettings: () -> Unit) {
    onCreateAlias()
    HomeTopBar(onSettings = onShowSettings)
}
fun CreateAliasScreen(apiKey: String) {}
fun AliasDetailsScreen(apiKey: String, aliasId: Long) {}
fun AliasContactsScreen(apiKey: String, aliasId: Long) {}
fun DeviceSettingsScreen() {}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/io/simplelogin/android/root/AppRoot.kt",
        r#"package io.simplelogin.android.root

import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.ui.NavDisplay
import io.simplelogin.android.screens.AliasContactsScreen
import io.simplelogin.android.screens.AliasDetailsScreen
import io.simplelogin.android.screens.CreateAliasScreen
import io.simplelogin.android.screens.DeviceSettingsScreen
import io.simplelogin.android.screens.HomeScreen
import io.simplelogin.android.screens.LoginMasterScreen

fun AppRoot(viewModel: AppRootViewModel) {
    NavDisplay(
        backStack = viewModel.navBackStack,
        onBack = viewModel::popBackStack,
        entryProvider = entryProvider {
            entry<InitializationDestination> {}
            entry<LogInDestination> {
                LoginMasterScreen()
            }
            entry<HomeDestination>(metadata = listPane()) { destination ->
                HomeScreen(
                    apiKey = destination.apiKey,
                    onCreateAlias = viewModel::showCreateAliasScreen,
                    onShowSettings = viewModel::showDeviceSettingsScreen,
                )
            }
            entry<CreateAliasDestination>(dialogMetadata) { destination ->
                CreateAliasScreen(apiKey = destination.apiKey)
            }
            entry<AliasDetailsDestination> { destination ->
                AliasDetailsScreen(apiKey = destination.apiKey, aliasId = destination.aliasId)
            }
            entry<AliasContactsDestination> { destination ->
                AliasContactsScreen(apiKey = destination.apiKey, aliasId = destination.aliasId)
            }
            entry<DeviceSettingsDestination> {
                DeviceSettingsScreen()
            }
            entry<MissingDestination> {
                DeviceSettingsScreen()
            }
        },
    )
}

data object InitializationDestination : NavKey
data object LogInDestination : NavKey
data class HomeDestination(val apiKey: String) : NavKey
data class CreateAliasDestination(val apiKey: String) : NavKey
data class AliasDetailsDestination(val apiKey: String, val aliasId: Long) : NavKey
data class AliasContactsDestination(val apiKey: String, val aliasId: Long) : NavKey
data object DeviceSettingsDestination : NavKey
data object UnregisteredDestination : NavKey
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/io/simplelogin/android/root/AppRootViewModel.kt",
        r#"package io.simplelogin.android.root

import androidx.compose.runtime.mutableStateListOf
import androidx.navigation3.runtime.NavKey
import kotlinx.coroutines.flow.MutableStateFlow

class ApiKey(val value: String)
class StartupState(val isReady: Boolean, val apiKey: ApiKey?)

class AppRootViewModel {
    private val _navBackStack =
        MutableStateFlow(mutableStateListOf<NavKey>(InitializationDestination))

    init {
        val state: StartupState = StartupState(true, null)
        _navBackStack.value.apply {
            clear()
            if (!state.isReady) {
                add(InitializationDestination)
                return@apply
            }
            state.apiKey?.let { apiKey ->
                add(HomeDestination(apiKey.value))
            } ?: add(LogInDestination)
        }
    }

    fun popBackStack() {
        if (_navBackStack.value.size > 1) {
            _navBackStack.value.removeAt(_navBackStack.value.lastIndex)
        }
    }

    fun showDeviceSettingsScreen() {
        _navBackStack.value.add(DeviceSettingsDestination)
    }

    fun viewAliasDetails(aliasId: Long) = withApiKey { apiKey ->
        if (aliasId > 0) {
            _navBackStack.value.add(AliasDetailsDestination(apiKey.value, aliasId))
        }
    }

    fun viewAliasContacts(aliasId: Long) = withApiKey { apiKey ->
        if (aliasId > 0) {
            uniquelyAddDestination(AliasContactsDestination(apiKey.value, aliasId))
        }
    }

    fun showCreateAliasScreen() = addToBackStackWithApiKey { apiKey ->
        CreateAliasDestination(apiKey.value)
    }

    fun showTaggedSettings() = addTagged("drawer") { DeviceSettingsDestination }

    fun jumpDynamic(pickHome: Boolean) {
        val someVar: NavKey = if (pickHome) HomeDestination("k") else LogInDestination
        _navBackStack.value.add(someVar)
    }

    fun jumpUnregistered() {
        _navBackStack.value.add(UnregisteredDestination)
    }

    private fun withApiKey(block: (ApiKey) -> Unit) {}

    private fun addToBackStackWithApiKey(onCreateDestination: (ApiKey) -> NavKey) =
        withApiKey { apiKey ->
            val destination = onCreateDestination(apiKey)
            _navBackStack.value.add(destination)
        }

    // A builder helper with a leading argument: `h(x) { … }` nests its calls.
    private fun addTagged(tag: String, build: () -> NavKey) {
        _navBackStack.value.add(build())
    }

    private fun uniquelyAddDestination(destination: NavKey) {
        _navBackStack.value.apply {
            removeIf { it::class == destination::class }
            add(destination)
        }
    }
}
"#,
    );
    // A local fake `entryProvider` without the Navigation 3 import proves nothing.
    write(
        dir.path(),
        "src/main/kotlin/io/simplelogin/android/fake/FakeNav.kt",
        r#"package io.simplelogin.android.fake

import io.simplelogin.android.root.DeviceSettingsDestination
import io.simplelogin.android.screens.DeviceSettingsScreen

class NavDisplay
fun entryProvider(block: () -> Unit) = block()
fun <T> entry(block: () -> Unit) = block()

fun FakeRoot() {
    entryProvider {
        entry<DeviceSettingsDestination> {
            DeviceSettingsScreen()
        }
        entry<NoSuchFakeKey> {}
    }
}
"#,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();

    let mut screens: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| {
            let prov = &n.props["prov"];
            assert_eq!(prov["confidence_tier"], "Confirmed");
            assert_eq!(n.props["router"], "navigation3");
            (n.id.clone(), n.props["key"].as_str().unwrap().to_string())
        })
        .collect();
    screens.sort();
    assert_eq!(
        screens,
        [
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.AliasContactsDestination".into(),
                "io.simplelogin.android.root.AliasContactsDestination".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.AliasDetailsDestination".into(),
                "io.simplelogin.android.root.AliasDetailsDestination".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.CreateAliasDestination".into(),
                "io.simplelogin.android.root.CreateAliasDestination".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.DeviceSettingsDestination".into(),
                "io.simplelogin.android.root.DeviceSettingsDestination".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.HomeDestination".into(),
                "io.simplelogin.android.root.HomeDestination".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.InitializationDestination".into(),
                "io.simplelogin.android.root.InitializationDestination".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.LogInDestination".into(),
                "io.simplelogin.android.root.LogInDestination".into(),
            ),
        ]
    );

    let mut renders: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.label == "RENDERS")
        .map(|e| {
            assert_eq!(e.props["prov"]["confidence_tier"], "Confirmed");
            (e.src.clone(), e.dst.clone())
        })
        .collect();
    renders.sort();
    // `InitializationDestination` has an empty entry body and emits no RENDERS;
    // callback references like `viewModel::showCreateAliasScreen` emit no edges.
    assert_eq!(
        renders,
        [
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.AliasContactsDestination".into(),
                "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#AliasContactsScreen".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.AliasDetailsDestination".into(),
                "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#AliasDetailsScreen".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.CreateAliasDestination".into(),
                "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#CreateAliasScreen".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.DeviceSettingsDestination".into(),
                "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#DeviceSettingsScreen".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.HomeDestination".into(),
                "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#HomeScreen".into(),
            ),
            (
                "screen:local/demo@navigation3:io.simplelogin.android.root.LogInDestination".into(),
                "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#LoginMasterScreen".into(),
            ),
        ]
    );

    let vm_file = "src/main/kotlin/io/simplelogin/android/root/AppRootViewModel.kt";
    let vm_sym = format!("sym:local/demo@{vm_file}#AppRootViewModel");

    let init_screen = out
        .nodes
        .iter()
        .find(|n| {
            n.id == "screen:local/demo@navigation3:io.simplelogin.android.root.InitializationDestination"
        })
        .unwrap();
    assert_eq!(init_screen.props["initial"], true);
    assert_eq!(init_screen.props["back_stack_owner"], vm_sym);
    assert!(init_screen.props.get("initial_owners").is_none());

    let home_screen = out
        .nodes
        .iter()
        .find(|n| {
            n.id == "screen:local/demo@navigation3:io.simplelogin.android.root.HomeDestination"
        })
        .unwrap();
    assert!(home_screen.props.get("initial").is_none());
    let home_callbacks = home_screen.props["entry_callbacks"].as_array().unwrap();
    assert_eq!(home_callbacks.len(), 2);
    assert_eq!(home_callbacks[0]["param"], "onCreateAlias");
    assert_eq!(
        home_callbacks[0]["symbol"],
        format!("sym:local/demo@{vm_file}#AppRootViewModel.showCreateAliasScreen")
    );
    assert_eq!(
        home_callbacks[0]["composable"],
        "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#HomeScreen"
    );
    assert_eq!(home_callbacks[0]["read"], true);
    assert_eq!(home_callbacks[0]["forwarded"], serde_json::json!([]));
    assert_eq!(home_callbacks[1]["param"], "onShowSettings");
    assert_eq!(
        home_callbacks[1]["symbol"],
        format!("sym:local/demo@{vm_file}#AppRootViewModel.showDeviceSettingsScreen")
    );
    assert_eq!(
        home_callbacks[1]["composable"],
        "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#HomeScreen"
    );
    assert_eq!(home_callbacks[1]["read"], true);
    assert_eq!(
        home_callbacks[1]["forwarded"],
        serde_json::json!([{
            "composable": "sym:local/demo@src/main/kotlin/io/simplelogin/android/screens/Screens.kt#HomeTopBar",
            "param": "onSettings",
        }])
    );

    let mut navs: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.label == "NAVIGATES_TO" && e.dst.starts_with("screen:"))
        .map(|e| {
            assert_eq!(e.props["prov"]["confidence_tier"], "InferredStrong");
            assert_eq!(e.props["router"], "navigation3");
            assert_eq!(e.props["router_basis"], "back_stack");
            let sites = e.props["sites"].as_array().unwrap();
            assert_eq!(sites.len(), 1);
            assert_eq!(sites[0]["path"], vm_file);
            assert_eq!(
                sites[0].get("phase").and_then(|v| v.as_str()),
                (e.src == vm_sym).then_some("init")
            );
            (
                e.src.clone(),
                e.dst.clone(),
                e.props["conditional"].as_bool().unwrap(),
                sites[0]["via"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    navs.sort();
    assert_eq!(
        navs,
        [
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.HomeDestination".into(),
                true,
                "direct".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.InitializationDestination".into(),
                true,
                "direct".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.LogInDestination".into(),
                true,
                "direct".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel.showCreateAliasScreen"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.CreateAliasDestination".into(),
                false,
                "builder_helper".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel.showDeviceSettingsScreen"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.DeviceSettingsDestination".into(),
                false,
                "direct".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel.showTaggedSettings"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.DeviceSettingsDestination".into(),
                false,
                "builder_helper".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel.viewAliasContacts"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.AliasContactsDestination".into(),
                true,
                "param_helper".into(),
            ),
            (
                format!("sym:local/demo@{vm_file}#AppRootViewModel.viewAliasDetails"),
                "screen:local/demo@navigation3:io.simplelogin.android.root.AliasDetailsDestination".into(),
                true,
                "direct".into(),
            ),
        ]
    );

    let mut gaps: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| {
            n.label == "Gap"
                && matches!(
                    n.props["kind"].as_str(),
                    Some("route-unresolved" | "navigation-unresolved")
                )
        })
        .map(|n| {
            (
                n.props["kind"].as_str().unwrap().to_string(),
                n.props["reason"].as_str().unwrap().to_string(),
                n.props["target"].as_str().map(str::to_string),
            )
        })
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            (
                "navigation-unresolved".into(),
                navigation3::COMPUTED_TARGET_REASON.into(),
                None,
            ),
            (
                "navigation-unresolved".into(),
                navigation3::UNKNOWN_SCREEN_REASON.into(),
                Some("io.simplelogin.android.root.UnregisteredDestination".into()),
            ),
            (
                "route-unresolved".into(),
                navigation3::UNDECLARED_KEY_REASON.into(),
                None,
            ),
        ]
    );
}

// AC-0351: an `entryProvider` property shadows the import, a same-package
// `mutableStateListOf` proves no back stack, an overloaded helper name proves
// no helper, and `add(index, key)` still navigates.
#[test]
fn navigation3_refuses_shadowed_names_and_overloads_but_reads_indexed_adds() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/demo/nav/Nav.kt",
        r#"package demo.nav

import androidx.compose.runtime.mutableStateListOf
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider

data object HomeKey : NavKey
data object SettingsKey : NavKey

fun Root() {
    entryProvider {
        entry<HomeKey> {}
        entry<SettingsKey> {}
    }
}

class Nav {
    private val stack = mutableStateListOf<NavKey>(HomeKey)

    fun indexed() {
        stack.add(0, SettingsKey)
    }

    fun go(key: NavKey) {
        stack.add(key)
    }

    fun go(route: String) {}

    fun viaOverload() {
        go(SettingsKey)
        go("settings")
    }
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/demo/nav/Shadowed.kt",
        r#"package demo.nav

import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider

data object ShadowKey : NavKey

class Shadowed {
    private val entryProvider: (() -> Unit) -> Unit = { it() }

    fun Root() {
        entryProvider {
            entry<ShadowKey> {}
        }
    }
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/demo/nav/FakeStack.kt",
        r#"package demo.nav

import androidx.navigation3.runtime.NavKey

class FakeStack {
    private val stack = mutableStateListOf<NavKey>(HomeKey)

    fun go() {
        stack.add(SettingsKey)
    }
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/demo/nav/Lists.kt",
        r#"package demo.nav

fun <T> mutableStateListOf(vararg items: T): MutableList<T> = mutableListOf(*items)
"#,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();

    let mut screens: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.clone())
        .collect();
    screens.sort();
    assert_eq!(
        screens,
        [
            "screen:local/demo@navigation3:demo.nav.HomeKey",
            "screen:local/demo@navigation3:demo.nav.SettingsKey",
        ]
    );

    let navs: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.label == "NAVIGATES_TO")
        .map(|e| (e.src.clone(), e.dst.clone()))
        .collect();
    assert_eq!(
        navs,
        [(
            "sym:local/demo@src/main/kotlin/demo/nav/Nav.kt#Nav.indexed".to_string(),
            "screen:local/demo@navigation3:demo.nav.SettingsKey".to_string(),
        )]
    );
    assert!(
        !out.nodes
            .iter()
            .any(|n| n.label == "Gap" && n.props["kind"] == "navigation-unresolved"),
        "an overloaded helper's String call is not a computed navigation"
    );
}

// AC-0351: initial destinations across multiple owners, entry-callback
// registrations (direct read, forwarded hops capped at CALLBACK_DEPTH, unread
// parameter, single-call lambda, ignored computed and overloaded expressions),
// and no T0 edges from screens to callback symbols.
#[test]
fn navigation3_records_initial_destinations_and_entry_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/io/simplelogin/android/screens/Screens.kt",
        r#"package io.simplelogin.android.screens

fun HomeTopBar(onSettings: () -> Unit) {
    onSettings()
}

fun HomeScreen(
    apiKey: String,
    onCreateAlias: () -> Unit,
    onShowSettings: () -> Unit,
    onUnread: () -> Unit,
    onLambda: () -> Unit,
    onComputed: () -> Unit,
    onOverloaded: () -> Unit,
) {
    onCreateAlias()
    HomeTopBar(onSettings = onShowSettings)
    // A library callee is not followed, but passing the callback reads it.
    IconButton(onClick = onLambda)
}

fun Hop5(cb: () -> Unit) {
    cb()
}
fun Hop4(cb: () -> Unit) {
    Hop5(cb)
}
fun Hop3(cb: () -> Unit) {
    Hop4(cb)
}
fun Hop2(cb: () -> Unit) {
    Hop3(cb)
}
fun Hop1(cb: () -> Unit) {
    Hop2(cb)
}
fun CreateAliasScreen(onDone: () -> Unit) {
    Hop1(onDone)
}
"#,
    );
    let app_root_src = r#"package io.simplelogin.android.root

import androidx.compose.runtime.mutableStateListOf
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import io.simplelogin.android.screens.CreateAliasScreen
import io.simplelogin.android.screens.HomeScreen
import kotlinx.coroutines.flow.MutableStateFlow

data object InitializationDestination : NavKey
data class HomeDestination(val apiKey: String) : NavKey
data class CreateAliasDestination(val apiKey: String) : NavKey
data object DeviceSettingsDestination : NavKey

fun pick(): () -> Unit = {}
fun topLevelDone() {}

class AppRootViewModel {
    private val _navBackStack =
        MutableStateFlow(mutableStateListOf<NavKey>(InitializationDestination))

    fun showCreateAliasScreen() {
        _navBackStack.value.add(CreateAliasDestination("k"))
    }

    fun showDeviceSettingsScreen() {
        _navBackStack.value.add(DeviceSettingsDestination)
    }

    fun overloaded() {}
    fun overloaded(flag: Boolean) {}
}

class SecondaryNavigator {
    private val stack = rememberNavBackStack(InitializationDestination)
    private val homeStack = mutableStateListOf<NavKey>(HomeDestination("init"))
}

fun AppRoot(viewModel: AppRootViewModel) {
    entryProvider {
        entry<InitializationDestination> {}
        entry<DeviceSettingsDestination> {}
        entry<HomeDestination> { destination ->
            HomeScreen(
                apiKey = destination.apiKey,
                onCreateAlias = viewModel::showCreateAliasScreen,
                onShowSettings = viewModel::showDeviceSettingsScreen,
                onUnread = viewModel::showCreateAliasScreen,
                onLambda = { viewModel.showDeviceSettingsScreen() },
                onComputed = pick(),
                onOverloaded = viewModel::overloaded,
            )
        }
        entry<CreateAliasDestination> {
            CreateAliasScreen(::topLevelDone)
        }
    }
}
"#;
    write(
        dir.path(),
        "src/main/kotlin/io/simplelogin/android/root/AppRoot.kt",
        app_root_src,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let root_file = "src/main/kotlin/io/simplelogin/android/root/AppRoot.kt";
    let screens_file = "src/main/kotlin/io/simplelogin/android/screens/Screens.kt";
    let vm_owner = format!("sym:local/demo@{root_file}#AppRootViewModel");
    let secondary_owner = format!("sym:local/demo@{root_file}#SecondaryNavigator");

    let init_screen = out
        .nodes
        .iter()
        .find(|n| {
            n.id == "screen:local/demo@navigation3:io.simplelogin.android.root.InitializationDestination"
        })
        .unwrap();
    assert_eq!(init_screen.props["initial"], true);
    assert_eq!(init_screen.props["back_stack_owner"], vm_owner);
    assert_eq!(
        init_screen.props["initial_owners"],
        serde_json::json!([vm_owner, secondary_owner])
    );

    let home_screen = out
        .nodes
        .iter()
        .find(|n| {
            n.id == "screen:local/demo@navigation3:io.simplelogin.android.root.HomeDestination"
        })
        .unwrap();
    assert_eq!(home_screen.props["initial"], true);
    assert_eq!(home_screen.props["back_stack_owner"], secondary_owner);
    assert!(home_screen.props.get("initial_owners").is_none());

    let home_callbacks = home_screen.props["entry_callbacks"].as_array().unwrap();
    assert_eq!(
        home_callbacks.len(),
        4,
        "computed and overloaded arguments must not produce callback records"
    );
    let home_composable = format!("sym:local/demo@{screens_file}#HomeScreen");
    let top_bar_composable = format!("sym:local/demo@{screens_file}#HomeTopBar");
    let show_create_sym =
        format!("sym:local/demo@{root_file}#AppRootViewModel.showCreateAliasScreen");
    let show_settings_sym =
        format!("sym:local/demo@{root_file}#AppRootViewModel.showDeviceSettingsScreen");

    assert_eq!(home_callbacks[0]["param"], "onCreateAlias");
    assert_eq!(home_callbacks[0]["symbol"], show_create_sym);
    assert_eq!(home_callbacks[0]["composable"], home_composable);
    assert_eq!(home_callbacks[0]["read"], true);
    assert_eq!(home_callbacks[0]["forwarded"], serde_json::json!([]));
    let ev0 = &home_callbacks[0]["evidence"];
    assert_eq!(ev0["path"], root_file);
    assert_eq!(
        &app_root_src[ev0["byte_start"].as_u64().unwrap() as usize
            ..ev0["byte_end"].as_u64().unwrap() as usize],
        "onCreateAlias = viewModel::showCreateAliasScreen"
    );

    assert_eq!(home_callbacks[1]["param"], "onShowSettings");
    assert_eq!(home_callbacks[1]["symbol"], show_settings_sym);
    assert_eq!(home_callbacks[1]["composable"], home_composable);
    assert_eq!(home_callbacks[1]["read"], true);
    assert_eq!(
        home_callbacks[1]["forwarded"],
        serde_json::json!([{"composable": top_bar_composable, "param": "onSettings"}])
    );

    assert_eq!(home_callbacks[2]["param"], "onUnread");
    assert_eq!(home_callbacks[2]["symbol"], show_create_sym);
    assert_eq!(home_callbacks[2]["composable"], home_composable);
    assert_eq!(home_callbacks[2]["read"], false);
    assert_eq!(home_callbacks[2]["forwarded"], serde_json::json!([]));

    assert_eq!(home_callbacks[3]["param"], "onLambda");
    assert_eq!(home_callbacks[3]["symbol"], show_settings_sym);
    assert_eq!(home_callbacks[3]["composable"], home_composable);
    assert_eq!(home_callbacks[3]["read"], true);
    assert_eq!(home_callbacks[3]["forwarded"], serde_json::json!([]));

    let create_screen = out
        .nodes
        .iter()
        .find(|n| {
            n.id == "screen:local/demo@navigation3:io.simplelogin.android.root.CreateAliasDestination"
        })
        .unwrap();
    let create_callbacks = create_screen.props["entry_callbacks"].as_array().unwrap();
    assert_eq!(create_callbacks.len(), 1);
    assert_eq!(create_callbacks[0]["param"], "onDone");
    assert_eq!(
        create_callbacks[0]["symbol"],
        format!("sym:local/demo@{root_file}#topLevelDone")
    );
    let forwarded = create_callbacks[0]["forwarded"].as_array().unwrap();
    assert_eq!(forwarded.len(), navigation3::CALLBACK_DEPTH);
    // Every hop forwards it, so it is read even though the invoking hop is past the cap.
    assert_eq!(create_callbacks[0]["read"], true);

    assert!(
        !out.edges
            .iter()
            .any(|e| e.src.starts_with("screen:") && e.label != "RENDERS"),
        "screens must never emit edges to callback symbols"
    );
    let mut edge_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for edge in &out.edges {
        *edge_counts.entry(edge.label.clone()).or_default() += 1;
    }
    assert_eq!(
        edge_counts,
        std::collections::BTreeMap::from([
            // Includes `viewModel.showDeviceSettingsScreen()` on the declared
            // `viewModel: AppRootViewModel` parameter (#1048).
            ("CALLS".into(), 10),
            ("DEFINED_IN".into(), 21),
            ("IMPORTS".into(), 7),
            ("NAVIGATES_TO".into(), 2),
            ("RENDERS".into(), 2),
        ])
    );
}

#[test]
fn navigation3_entry_callbacks_resolve_through_with_receivers() {
    // SimpleLogin's `AppRoot(...) = with(viewModel) { … }`: an unqualified
    // reference is a receiver member first, then a top-level function.
    let dir = tempfile::tempdir().unwrap();
    let src = r#"package demo.root

import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider

data object HomeKey : NavKey
data object OtherKey : NavKey

class Vm {
    fun open() {}
    fun close() {}
}

fun close() {}
fun topOnly() {}

fun Screen(onOpen: () -> Unit, onClose: () -> Unit, onTop: () -> Unit) {
    onOpen()
    onClose()
    onTop()
}

fun Root(vm: Vm) = with(vm) {
    entryProvider {
        entry<HomeKey> {
            Screen(onOpen = ::open, onClose = { close() }, onTop = ::topOnly)
        }
    }
}

fun Other(external: ExternalThing) = with(external) {
    entryProvider {
        entry<OtherKey> {
            Screen(onOpen = ::open, onClose = ::close, onTop = ::topOnly)
        }
    }
}
"#;
    write(dir.path(), "src/main/kotlin/demo/root/Root.kt", src);
    let out = extract_dir(dir.path(), &id()).unwrap();
    let file = "src/main/kotlin/demo/root/Root.kt";
    let screen = |key: &str| {
        out.nodes
            .iter()
            .find(|n| n.id == format!("screen:local/demo@navigation3:demo.root.{key}"))
            .unwrap()
    };
    let symbols = |key: &str| -> Vec<String> {
        screen(key).props["entry_callbacks"]
            .as_array()
            .map(|cbs| {
                cbs.iter()
                    .map(|c| c["symbol"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        symbols("HomeKey"),
        vec![
            format!("sym:local/demo@{file}#Vm.open"),
            // The member shadows the same-named top-level `close`.
            format!("sym:local/demo@{file}#Vm.close"),
            format!("sym:local/demo@{file}#topOnly"),
        ]
    );
    // An unproven receiver type could own any name, so nothing is recorded.
    assert!(symbols("OtherKey").is_empty());
}

#[test]
fn navigation3_callbacks_keep_defaulted_params_and_initial_screens_cite_initializers() {
    // Kotlin defaults and parameter modifiers are siblings of `parameter`, so
    // positions and names survive `modifier: Modifier = Modifier`.
    let dir = tempfile::tempdir().unwrap();
    let src = r#"package demo.root

import androidx.compose.runtime.mutableStateListOf
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider

data object HomeKey : NavKey

class Vm {
    private val stack = mutableStateListOf<NavKey>(HomeKey)
    fun open() {}
    fun close() {}
}

fun Screen(modifier: Modifier = Modifier, @Suppress("x") onOpen: () -> Unit = {}, onClose: () -> Unit) {
    onOpen()
    onClose()
}

fun Root(vm: Vm) {
    entryProvider {
        entry<HomeKey> {
            Screen(Modifier, vm::open, onClose = vm::close)
        }
    }
}
"#;
    write(dir.path(), "src/main/kotlin/demo/root/Root.kt", src);
    let out = extract_dir(dir.path(), &id()).unwrap();
    let file = "src/main/kotlin/demo/root/Root.kt";
    let screen = out
        .nodes
        .iter()
        .find(|n| n.id == "screen:local/demo@navigation3:demo.root.HomeKey")
        .unwrap();
    let records: Vec<(String, String, bool)> = screen.props["entry_callbacks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["param"].as_str().unwrap().to_string(),
                c["symbol"].as_str().unwrap().to_string(),
                c["read"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        records,
        vec![
            (
                "onOpen".into(),
                format!("sym:local/demo@{file}#Vm.open"),
                true
            ),
            (
                "onClose".into(),
                format!("sym:local/demo@{file}#Vm.close"),
                true
            ),
        ]
    );

    assert_eq!(screen.props["initial"], true);
    let cited: Vec<&str> = screen.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            &src[e["byte_start"].as_u64().unwrap() as usize
                ..e["byte_end"].as_u64().unwrap() as usize]
        })
        .collect();
    assert_eq!(cited.len(), 2, "entry registration plus the initializer");
    assert!(cited[0].starts_with("entry<HomeKey>"));
    assert_eq!(
        cited[1],
        "private val stack = mutableStateListOf<NavKey>(HomeKey)"
    );
}

// AC-0353 (T-0353): `x.m()` resolves to `T.m` when `x`'s nearest lexical
// binding states its type `T` (#1048): constructor properties, parameters,
// lambda parameters, annotated or `T(…)` locals, `this.x`, nullable and
// generic types, through imports, the same package, or a star import. An
// untyped nearer binding blocks outer ones; undeclared members, duplicate
// FQNs and non-constructible `T(…)` assert nothing.
#[test]
fn declared_type_receivers_resolve_member_calls() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "net/Api.kt",
        "package io.x.net\n\ninterface ApiService {\n    suspend fun getStats(): Int\n    fun other()\n}\n",
    );
    write(
        dir.path(),
        "net/Datasource.kt",
        r#"package io.x.net

interface Datasource {
    fun stats()
}

class DatasourceImpl @Inject constructor(private val apiService: ApiService) : Datasource {
    override fun stats() {
        safe { apiService.getStats() }
    }
}
"#,
    );
    write(
        dir.path(),
        "feature/Vm.kt",
        r#"package io.x.feature

import io.x.net.Datasource

class Box<T> { fun get(): T = TODO() }
class Helper { fun help() {} }
interface Thing {
    fun act()
}

class Vm(private val ds: Datasource, plain: Datasource) {
    private val x: Datasource = ds
    fun direct() { ds.stats() }
    fun viaThis() { this.ds.stats() }
    fun nullable(q: Datasource?) { q?.stats() }
    fun generic(b: Box<Datasource>) { b.get() }
    fun shadowed() { val ds = make(); ds.stats() }
    fun lambda() { run { r: Datasource -> r.stats() } }
    fun constructed() { val h = Helper(); h.help() }
    fun notConstructible() { val t = Thing(); t.act() }
    fun undeclared() { ds.toString() }
    fun looped(xs: List<Datasource>) { for (x in xs) { x.stats() } }
    fun plainHidden() { plain.stats() }
    fun lambdaShadowed() { run { val ds = make(); ds.stats() } }
    fun lambdaLocal() { run { val y: Datasource = make(); y.stats() } }
    class Nested { fun n() { ds.stats() } }
}

fun qualified(a: io.x.net.ApiService) { a.other() }
"#,
    );
    write(
        dir.path(),
        "feature/Generic.kt",
        "package io.x.feature\n\nclass Tp {\n    fun m() {}\n}\n\nfun <Tp> typeParam(x: Tp) {\n    x.m()\n}\n\nclass Holder<Tp>(val t: Tp) {\n    fun h() {\n        t.m()\n    }\n}\n",
    );
    write(
        dir.path(),
        "feature/Local.kt",
        "package io.x.feature\n\nimport io.x.net.Datasource\n\nfun localClass(api: Datasource) {\n    class Local {\n        fun g() {\n            api.stats()\n        }\n    }\n}\n",
    );
    write(
        dir.path(),
        "feature/Loop.kt",
        "package io.x.feature\n\nimport io.x.net.Datasource\n\nfun loopIterable(source: Datasource) {\n    for (source in source.stats()) {\n        println(source)\n    }\n}\n",
    );
    write(
        dir.path(),
        "other/Star.kt",
        "package io.x.other\n\nimport io.x.net.*\n\nfun star(a: ApiService) { a.other() }\n\nfun twin(t: io.x.dup.Twin) { t.go() }\n",
    );
    write(
        dir.path(),
        "dup/A.kt",
        "package io.x.dup\n\nclass Twin { fun go() {} }\n",
    );
    write(
        dir.path(),
        "dup/B.kt",
        "package io.x.dup\n\nclass Twin { fun go() {} }\n",
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut typed: Vec<(String, String)> = out
        .edges
        .iter()
        .filter(|e| e.label == "CALLS" && e.props["resolution"] == "declared-type")
        .map(|e| {
            let short = |s: &str| s.rsplit_once('#').unwrap().1.to_string();
            (short(&e.src), short(&e.dst))
        })
        .collect();
    typed.sort();
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(
        typed,
        vec![
            pair("DatasourceImpl.stats", "ApiService.getStats"),
            pair("Vm.direct", "Datasource.stats"),
            pair("Vm.generic", "Box.get"),
            pair("Vm.lambda", "Datasource.stats"),
            pair("Vm.lambdaLocal", "Datasource.stats"),
            pair("Vm.nullable", "Datasource.stats"),
            pair("Vm.viaThis", "Datasource.stats"),
            pair("Vm.constructed", "Helper.help"),
            pair("star", "ApiService.other"),
            pair("qualified", "ApiService.other"),
            pair("loopIterable", "Datasource.stats"),
            pair("Local.g", "Datasource.stats"),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    let calls = edge(
        &out.edges,
        "sym:local/demo@net/Datasource.kt#DatasourceImpl.stats",
        "sym:local/demo@net/Api.kt#ApiService.getStats",
        "CALLS",
    );
    let prov: Provenance = serde_json::from_value(calls.props["prov"].clone()).unwrap();
    assert_eq!(prov.tier, Tier::Deterministic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
    let span = &prov.evidence[0];
    let text = std::fs::read_to_string(dir.path().join("net/Datasource.kt")).unwrap();
    assert_eq!(
        &text[span.byte_start as usize..span.byte_end as usize],
        "apiService.getStats()"
    );
    // Nothing unproven turns into a Gap either.
    assert!(!out.nodes.iter().any(|n| {
        n.label == "Gap"
            && n.props["callee"]
                .as_str()
                .is_some_and(|c| c.contains("stats") || c.contains("go"))
    }));
}
