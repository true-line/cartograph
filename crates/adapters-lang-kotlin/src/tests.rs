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

// AC-0354 (T-0354): an import-proven `hiltViewModel` call types its binding
// (#1048 W11c-2): `hiltViewModel<VM>()` is `VM`, and
// `hiltViewModel(…) { f: F -> f.create(…) }` is `F.create`'s declared return
// type. Inside `with(x) { m() }` a typed `x`'s member wins over the enclosing
// file's functions, which remain the fallback when `x`'s type lacks `m` or
// proves nothing (a type parameter). A `hiltViewModel` or `with` shadowed by
// a same-file declaration, or a same-package `with` function or callable
// property, types nothing.
#[test]
fn hilt_view_models_type_their_bindings() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "vm/CreateVm.kt",
        r#"package io.x.vm

class CreateVm @AssistedInject constructor(@Assisted val key: String) {
    @AssistedFactory
    interface Factory {
        fun create(key: String): CreateVm
    }

    fun create(body: Int) {}
}
"#,
    );
    write(
        dir.path(),
        "vm/Vms.kt",
        "package io.x.vm\n\nclass LoginVm {\n    fun login() {}\n}\n\nclass OtherVm {\n    fun go() {}\n}\n",
    );
    write(
        dir.path(),
        "ui/Screens.kt",
        r#"package io.x.ui

import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import io.x.vm.*

fun Create(k: String) {
    val viewModel = hiltViewModel(key = k) { factory: CreateVm.Factory ->
        factory.create(k)
    }
    Button(onClick = { viewModel.create(1) })
}

fun Login() = with(hiltViewModel<LoginVm>()) {
    login()
    helper()
}

fun login() {}

fun helper() {}

fun Arg() {
    val vm = hiltViewModel<OtherVm>()
    vm.go()
}

fun Default(viewModel: OtherVm = hiltViewModel()) { viewModel.go() }

fun WrongShape(k: String) {
    val vm = hiltViewModel { f: CreateVm.Factory ->
        val made = f.create(k)
        made
    }
    vm.create(2)
}
"#,
    );
    write(
        dir.path(),
        "fake/Fake.kt",
        r#"package io.x.fake

import io.x.vm.OtherVm

fun <T> hiltViewModel(): T = TODO()

fun Fake() {
    val vm = hiltViewModel<OtherVm>()
    vm.go()
}
"#,
    );
    // The real import, shadowed by a same-file function: no Hilt typing.
    write(
        dir.path(),
        "shadow/Hilt.kt",
        r#"package io.x.shadow

import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import io.x.vm.LoginVm
import io.x.vm.OtherVm

fun <T> hiltViewModel(): OtherVm = OtherVm()

fun Shadowed() {
    val vm = hiltViewModel<LoginVm>()
    vm.login()
}
"#,
    );
    // A member `with` that gives its lambda no receiver.
    write(
        dir.path(),
        "shadow/With.kt",
        r#"package io.x.shadoww

import io.x.vm.LoginVm

class Scope {
    fun with(vm: LoginVm, block: () -> Unit) {
        block()
    }

    fun login() {}

    fun LocalWith(vm: LoginVm) = with(vm) {
        login()
    }
}
"#,
    );
    // A same-package `with` declared in another file shadows `kotlin.with`.
    write(
        dir.path(),
        "pkgwith/With.kt",
        "package io.x.pkgwith\n\nfun with(x: Any, block: () -> Unit) {\n    block()\n}\n",
    );
    write(
        dir.path(),
        "pkgwith/Use.kt",
        r#"package io.x.pkgwith

import io.x.vm.LoginVm

fun login() {}

fun PackageWith(vm: LoginVm) = with(vm) {
    login()
}
"#,
    );
    // A same-package callable property named `with` shadows it too.
    write(
        dir.path(),
        "pkgprop/With.kt",
        "package io.x.pkgprop\n\nimport io.x.vm.LoginVm\n\nval with: (LoginVm, () -> Unit) -> Unit = { _, block -> block() }\n",
    );
    write(
        dir.path(),
        "pkgprop/Use.kt",
        r#"package io.x.pkgprop

import io.x.vm.LoginVm

fun login() {}

fun PropertyWith(vm: LoginVm) {
    with(vm) {
        login()
    }
}
"#,
    );
    // A type parameter receiver proves nothing; the ordinary call stays.
    write(
        dir.path(),
        "generic/Generic.kt",
        r#"package io.x.generic

fun helper() {}

fun <T> Generic(x: T) = with(x) {
    helper()
}
"#,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let short = |s: &str| s.rsplit_once('#').unwrap().1.to_string();
    let mut calls: Vec<(String, String)> = out
        .edges
        .iter()
        .filter(|e| {
            e.label == "CALLS"
                && [
                    "@ui/",
                    "@fake/",
                    "@shadow/",
                    "@pkgwith/",
                    "@pkgprop/",
                    "@generic/",
                ]
                .iter()
                .any(|dir| e.src.contains(dir))
        })
        .filter(|e| !e.dst.starts_with("gap:"))
        .map(|e| (short(&e.src), short(&e.dst)))
        .collect();
    calls.sort();
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    let mut expected = vec![
        pair("Arg", "OtherVm.go"),
        pair("Create", "CreateVm.Factory.create"),
        pair("Create", "CreateVm.create"),
        pair("Default", "OtherVm.go"),
        // The look-alike is an ordinary local call; it types nothing.
        pair("Fake", "hiltViewModel"),
        // Shadowed `hiltViewModel` / `with` keep their ordinary resolution.
        pair("Generic", "helper"),
        pair("Scope.LocalWith", "Scope.login"),
        pair("Scope.LocalWith", "Scope.with"),
        pair("PackageWith", "login"),
        pair("PropertyWith", "login"),
        pair("Shadowed", "hiltViewModel"),
        pair("Login", "LoginVm.login"),
        pair("Login", "helper"),
        pair("WrongShape", "CreateVm.Factory.create"),
    ];
    expected.sort();
    assert_eq!(calls, expected);
    // The view model call carries the declared-type resolution and its span.
    let create = edge(
        &out.edges,
        "sym:local/demo@ui/Screens.kt#Create",
        "sym:local/demo@vm/CreateVm.kt#CreateVm.create",
        "CALLS",
    );
    assert_eq!(create.props["resolution"], "declared-type");
    let prov: Provenance = serde_json::from_value(create.props["prov"].clone()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("ui/Screens.kt")).unwrap();
    let span = &prov.evidence[0];
    assert_eq!(
        &text[span.byte_start as usize..span.byte_end as usize],
        "viewModel.create(1)"
    );
}

// AC-0355 (T-0355): a Hilt `@Binds` method in a `@Module` +
// `@InstallIn(SingletonComponent::class)` module emits `InferredStrong`
// `CALLS` edges (`resolution: "hilt-binds"`, `assumption: "hilt_dispatch"`)
// from each interface member to the matching member on the bound
// implementation, failing closed on non-singleton components, missing imports,
// qualifiers, multibindings, generic keys, missing supertype declarations,
// duplicate bindings, and non-main source sets (W11c-3).
#[test]
fn hilt_binds_modules_dispatch_interface_members_to_implementations() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/io/x/net/Ds.kt",
        r#"package io.x.net

interface Ds {
    fun a()
    fun b()
    fun c()
}

class DsImpl : Ds {
    override fun a() {
    }
    override fun b() {
    }
}
"#,
    );
    let ds_module_path = "src/main/java/io/x/di/RemoteDatasourceModule.kt";
    let ds_module_src = r#"package io.x.di

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import io.x.net.Ds
import io.x.net.DsImpl
import javax.inject.Singleton

@[Module InstallIn(SingletonComponent::class)]
abstract class RemoteDatasourceModule {
    @[Binds Singleton]
    abstract fun bindDs(impl: DsImpl): Ds
}
"#;
    write(dir.path(), ds_module_path, ds_module_src);
    write(
        dir.path(),
        "src/main/java/io/x/repo/Repo.kt",
        r#"package io.x.repo

interface Repo {
    fun load()

    fun refresh() {
        fun inner() {
        }
        inner()
    }

    companion object {
        fun make() {
        }
    }
}

// `make` and `inner` share `Repo.name` symbol shapes but are not members.
class RepoImpl : Repo {
    override fun load() {
    }

    fun make() {
    }

    fun inner() {
    }
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/io/x/di/RepoModule.kt",
        r#"package io.x.di

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import io.x.repo.Repo
import io.x.repo.RepoImpl

@Module
@InstallIn(SingletonComponent::class)
abstract class RepoModule {
    @Binds
    abstract fun bindRepo(
        impl: RepoImpl
    ): Repo
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/io/x/feature/Vm.kt",
        r#"package io.x.feature

import io.x.net.Ds

class Vm(private val ds: Ds) {
    fun x() {
        ds.a()
    }
}
"#,
    );

    // Negative 1: @InstallIn(ActivityComponent::class) module.
    write(
        dir.path(),
        "src/main/java/io/x/neg/ActModule.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.android.components.ActivityComponent

interface ActDs {
    fun m()
}

class ActDsImpl : ActDs {
    override fun m() {
    }
}

@[Module InstallIn(ActivityComponent::class)]
abstract class ActModule {
    @Binds
    abstract fun bind(impl: ActDsImpl): ActDs
}
"#,
    );

    // Negative 2: Missing `import dagger.Binds` (a local `annotation class Binds`).
    write(
        dir.path(),
        "src/main/java/io/x/neg/LocalBindsModule.kt",
        r#"package io.x.neg

import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

annotation class Binds

interface LocalDs {
    fun m()
}

class LocalDsImpl : LocalDs {
    override fun m() {
    }
}

@[Module InstallIn(SingletonComponent::class)]
abstract class LocalBindsModule {
    @Binds
    abstract fun bind(impl: LocalDsImpl): LocalDs
}
"#,
    );

    // Negative 3: `@Binds @Named("x")` qualified binding.
    write(
        dir.path(),
        "src/main/java/io/x/neg/NamedModule.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import javax.inject.Named

interface NamedDs {
    fun m()
}

class NamedDsImpl : NamedDs {
    override fun m() {
    }
}

@[Module InstallIn(SingletonComponent::class)]
abstract class NamedModule {
    @Binds
    @Named("x")
    abstract fun bind(impl: NamedDsImpl): NamedDs
}
"#,
    );

    // Negative 4: `@Binds @IntoSet`.
    write(
        dir.path(),
        "src/main/java/io/x/neg/SetModule.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import dagger.multibindings.IntoSet

interface SetDs {
    fun m()
}

class SetDsImpl : SetDs {
    override fun m() {
    }
}

@[Module InstallIn(SingletonComponent::class)]
abstract class SetModule {
    @Binds
    @IntoSet
    abstract fun bind(impl: SetDsImpl): SetDs
}
"#,
    );

    // Negative 5: Generic key `abstract fun bind(impl: BoxImpl<String>): Box<String>`.
    write(
        dir.path(),
        "src/main/java/io/x/neg/GenericModule.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

interface Box<T> {
    fun m()
}

class BoxImpl<T> : Box<T> {
    override fun m() {
    }
}

@[Module InstallIn(SingletonComponent::class)]
abstract class GenericModule {
    @Binds
    abstract fun bind(impl: BoxImpl<String>): Box<String>
}
"#,
    );

    // Negative 6: Impl that does not declare `: Iface`.
    write(
        dir.path(),
        "src/main/java/io/x/neg/UnrelatedModule.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

interface UnrelatedDs {
    fun m()
}

class UnrelatedDsImpl {
    fun m() {
    }
}

@[Module InstallIn(SingletonComponent::class)]
abstract class UnrelatedModule {
    @Binds
    abstract fun bind(impl: UnrelatedDsImpl): UnrelatedDs
}
"#,
    );

    // Negative 7: Two modules binding the same Iface (both edges suppressed).
    write(
        dir.path(),
        "src/main/java/io/x/neg/DupTypes.kt",
        r#"package io.x.neg

interface DupDs {
    fun m()
}

class DupDsImplA : DupDs {
    override fun m() {
    }
}

class DupDsImplB : DupDs {
    override fun m() {
    }
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/io/x/neg/DupModuleA.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

@[Module InstallIn(SingletonComponent::class)]
abstract class DupModuleA {
    @Binds
    abstract fun bindA(impl: DupDsImplA): DupDs
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/io/x/neg/DupModuleB.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

@[Module InstallIn(SingletonComponent::class)]
abstract class DupModuleB {
    @Binds
    abstract fun bindB(impl: DupDsImplB): DupDs
}
"#,
    );

    // Negative 8: Module under `src/test/java/...`.
    write(
        dir.path(),
        "src/main/java/io/x/neg/TestTypes.kt",
        r#"package io.x.neg

interface TestDs {
    fun m()
}

class TestDsImpl : TestDs {
    override fun m() {
    }
}
"#,
    );
    write(
        dir.path(),
        "src/test/java/io/x/neg/TestModule.kt",
        r#"package io.x.neg

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

@[Module InstallIn(SingletonComponent::class)]
abstract class TestModule {
    @Binds
    abstract fun bind(impl: TestDsImpl): TestDs
}
"#,
    );

    let out = extract_dir(dir.path(), &id()).unwrap();
    let short = |s: &str| s.rsplit_once('#').unwrap().1.to_string();
    let mut hilt_edges: Vec<(String, String)> = out
        .edges
        .iter()
        .filter(|e| e.label == "CALLS" && e.props["resolution"] == "hilt-binds")
        .map(|e| (short(&e.src), short(&e.dst)))
        .collect();
    hilt_edges.sort();
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(
        hilt_edges,
        vec![
            pair("Ds.a", "DsImpl.a"),
            pair("Ds.b", "DsImpl.b"),
            pair("Repo.load", "RepoImpl.load"),
        ]
    );

    // Inspect the exact props and provenance of the `Ds.a -> DsImpl.a` edge.
    let ds_a = edge(
        &out.edges,
        "sym:local/demo@src/main/java/io/x/net/Ds.kt#Ds.a",
        "sym:local/demo@src/main/java/io/x/net/Ds.kt#DsImpl.a",
        "CALLS",
    );
    assert_eq!(ds_a.props["resolution"], "hilt-binds");
    assert_eq!(ds_a.props["assumption"], "hilt_dispatch");
    assert_eq!(
        ds_a.props["binding"],
        format!("sym:local/demo@{ds_module_path}#RemoteDatasourceModule.bindDs")
    );
    let prov: Provenance = serde_json::from_value(ds_a.props["prov"].clone()).unwrap();
    assert_eq!(prov.tier, Tier::Deterministic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::InferredStrong);
    assert_eq!(prov.evidence.len(), 1);
    let span = &prov.evidence[0];
    assert_eq!(span.path, ds_module_path);
    assert_eq!(
        &ds_module_src[span.byte_start as usize..span.byte_end as usize],
        "@[Binds Singleton]\n    abstract fun bindDs(impl: DsImpl): Ds"
    );

    // W11c-1 interface edge `Vm.x -> Ds.a` (Confirmed) is still present.
    let vm_call = edge(
        &out.edges,
        "sym:local/demo@src/main/java/io/x/feature/Vm.kt#Vm.x",
        "sym:local/demo@src/main/java/io/x/net/Ds.kt#Ds.a",
        "CALLS",
    );
    assert_eq!(vm_call.props["resolution"], "declared-type");
    let vm_prov: Provenance = serde_json::from_value(vm_call.props["prov"].clone()).unwrap();
    assert_eq!(vm_prov.confidence_tier, ConfidenceTier::Confirmed);
}

// AC-0431 (#1250): a Spring controller in a Gradle test source set yields no
// product Endpoint; the same controller under `src/main` is unchanged.
#[test]
fn test_source_set_spring_controllers_yield_no_product_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let controller = |name: &str, route: &str| {
        format!(
            r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
class {name} {{
    @GetMapping("{route}")
    fun get(): String = "u"
}}

object Marker{name}
"#
        )
    };
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/Live.kt",
        &controller("Live", "/live"),
    );
    write(
        dir.path(),
        "src/test/kotlin/com/demo/web/TestOnly.kt",
        &controller("TestOnly", "/test-only"),
    );
    write(
        dir.path(),
        "src/testFixtures/kotlin/com/demo/web/Fixture.kt",
        &controller("Fixture", "/fixture"),
    );
    // A package directory named `src/test` below a production root is product.
    write(
        dir.path(),
        "src/main/kotlin/com/acme/src/test/Nested.kt",
        &controller("Nested", "/nested"),
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    let endpoints: std::collections::BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        std::collections::BTreeSet::from(["ep:local/demo@GET:/live", "ep:local/demo@GET:/nested"])
    );
}

/// `(id, reason)` of each route-bearing fact, sorted: endpoints with no
/// reason, Gaps with theirs.
fn route_rows(out: &Extraction) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint" || node.id.starts_with("gap:route:"))
        .map(|node| {
            let reason = node.props["reason"].as_str().unwrap_or("").to_string();
            let id = if node.label == "Endpoint" {
                node.id.clone()
            } else {
                format!("gap {}", node.props["handler_sym"].as_str().unwrap_or(""))
            };
            (id, reason)
        })
        .collect();
    rows.sort();
    rows
}

const SHADOWED: &str =
    "Spring annotation proven only by a star import is shadowed by a same-package declaration";

// AC-0442 (#1288): a same-package declaration obscures a star-imported
// Spring annotation (Kotlin resolves explicit imports, then the package,
// then star imports), whether it is the controller stereotype, the class
// `@RequestMapping` base or the method mapping, so the star-only endpoint
// becomes a route Gap at the directory join. A named import, a fully
// qualified spelling, a directly proven sibling stereotype and other
// packages are unaffected; qualified lookalikes prove nothing.
#[test]
fn spring_star_proof_yields_to_a_same_package_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let web = "src/main/kotlin/com/demo/web";
    write(
        dir.path(),
        &format!("{web}/Local.kt"),
        "package com.demo.web\n\nannotation class GetMapping(val value: String = \"\")\n\ntypealias RequestMapping = Any\n",
    );
    write(
        dir.path(),
        &format!("{web}/Users.kt"),
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
class Users {
    @GetMapping("/users")
    fun list(): String = "u"

    @PostMapping("/users")
    fun create(): String = "c"

    @PostMapping("/users/$suffix")
    fun computed(): String = "d"
}
"#,
    );
    write(
        dir.path(),
        &format!("{web}/Named.kt"),
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*
import org.springframework.web.bind.annotation.GetMapping

@RestController
class Named {
    @GetMapping("/named")
    fun named(): String = "n"

    @org.springframework.web.bind.annotation.PutMapping("/qualified")
    fun qualified(): String = "q"

    @com.other.PostMapping("/lookalike")
    fun lookalike(): String = "l"
}
"#,
    );
    write(
        dir.path(),
        &format!("{web}/Based.kt"),
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/based")
class Based {
    @PostMapping("/x")
    fun x(): String = "x"
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/other/Elsewhere.kt",
        r#"package com.demo.other

import org.springframework.web.bind.annotation.*

@RestController
class Elsewhere {
    @GetMapping("/elsewhere")
    fun elsewhere(): String = "e"
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let sym = |path: &str, member: &str| format!("sym:local/demo@{web}/{path}#{member}");
    assert_eq!(
        route_rows(&out),
        [
            ("ep:local/demo@GET:/elsewhere".to_string(), String::new()),
            ("ep:local/demo@GET:/named".to_string(), String::new()),
            ("ep:local/demo@POST:/users".to_string(), String::new()),
            ("ep:local/demo@PUT:/qualified".to_string(), String::new()),
            (
                format!("gap {}", sym("Based.kt", "Based.x")),
                SHADOWED.to_string()
            ),
            (
                format!("gap {}", sym("Users.kt", "Users.computed")),
                "dynamic Spring mapping path".to_string()
            ),
            (
                format!("gap {}", sym("Users.kt", "Users.list")),
                SHADOWED.to_string()
            ),
        ]
    );
}

// AC-0442: an obscured controller stereotype shadows every star-only route
// of its class, unless a directly proven or unshadowed star-proven sibling
// stereotype still proves the class (even when its own name is declared in
// the package); a same-package type (here an `object`)
// obscures like an annotation class; a shadowed route with a dynamic path
// reports the shadowing.
#[test]
fn spring_shadowed_stereotypes_and_dynamic_paths_settle_at_the_join() {
    let dir = tempfile::tempdir().unwrap();
    let web = "src/main/kotlin/com/demo/web";
    write(
        dir.path(),
        &format!("{web}/Local.kt"),
        "package com.demo.web\n\nannotation class RestController\n\nannotation class PostMapping(val value: String = \"\")\n",
    );
    write(
        dir.path(),
        &format!("{web}/Shadowed.kt"),
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
class Shadowed {
    @GetMapping("/shadowed")
    fun shadowed(): String = "s"
}
"#,
    );
    write(
        dir.path(),
        &format!("{web}/Both.kt"),
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*
import org.springframework.stereotype.Controller

@RestController
@Controller
class Both {
    @GetMapping("/both")
    fun both(): String = "b"

    @PostMapping(ROUTE)
    fun computed(): String = "d"
}
"#,
    );
    write(
        dir.path(),
        &format!("{web}/Stars.kt"),
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*
import org.springframework.stereotype.*

@RestController
@Controller
class Stars {
    @GetMapping("/stars")
    fun stars(): String = "s"
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/api/Api.kt",
        r#"package com.demo.api

import org.springframework.web.bind.annotation.*

object PutMapping

@RestController
class Api {
    @PutMapping("/api")
    fun put(): String = "p"
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/dom/Direct.kt",
        r#"package com.demo.dom

import org.springframework.web.bind.annotation.*
import org.springframework.stereotype.Controller

annotation class RestController

annotation class Controller

@RestController
@Controller
class Direct {
    @PostMapping("/direct")
    fun direct(): String = "d"
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let sym = |path: &str, member: &str| format!("sym:local/demo@{web}/{path}#{member}");
    assert_eq!(
        route_rows(&out),
        [
            ("ep:local/demo@GET:/both".to_string(), String::new()),
            ("ep:local/demo@GET:/stars".to_string(), String::new()),
            ("ep:local/demo@POST:/direct".to_string(), String::new()),
            (
                "gap sym:local/demo@src/main/kotlin/com/demo/api/Api.kt#Api.put".to_string(),
                SHADOWED.to_string()
            ),
            (
                format!("gap {}", sym("Both.kt", "Both.computed")),
                SHADOWED.to_string()
            ),
            (
                format!("gap {}", sym("Shadowed.kt", "Shadowed.shadowed")),
                SHADOWED.to_string()
            ),
        ]
    );
}

// AC-0442: single-file extraction settles star-proven endpoints against the
// file's own declarations, and a reused cached file's pending endpoint is
// retargeted to the new commit.
#[test]
fn single_file_and_cached_star_endpoints_settle() {
    let path = "src/main/kotlin/com/demo/web/Solo.kt";
    let source = r#"package com.demo.web

import org.springframework.web.bind.annotation.*

typealias GetMapping = Any

@RestController
class Solo {
    @GetMapping("/solo")
    fun solo(): String = "s"

    @PostMapping("/solo")
    fun post(): String = "p"
}
"#;
    let out = extract_source(source.as_bytes(), path, &id()).unwrap();
    assert_eq!(
        route_rows(&out),
        [
            ("ep:local/demo@POST:/solo".to_string(), String::new()),
            (
                "gap sym:local/demo@src/main/kotlin/com/demo/web/Solo.kt#Solo.solo".to_string(),
                SHADOWED.to_string()
            ),
        ]
    );

    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), path, source);
    let mut cache = IncrementalCache::default();
    extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    let second = SourceId {
        repo: "local/demo",
        commit: "abc123",
    };
    let (out, stats) = extract_dir_incremental(dir.path(), &second, &mut cache).unwrap();
    assert_eq!(stats.reused_files, 1);
    let commits: BTreeSet<String> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint" || node.id.starts_with("gap:route:"))
        .flat_map(|node| {
            node.props["prov"]["evidence"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .map(|evidence| evidence["commit_sha"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(commits, BTreeSet::from(["abc123".to_string()]));
}

// AC-0442: a same-package product Java type obscures a Kotlin star import
// in a mixed JVM tree; test-source declarations (Java or Kotlin) never
// obscure a product import; a `private` top-level declaration obscures only
// within its own file; default-package declarations obscure too.
#[test]
fn spring_star_shadowing_spans_jvm_languages_visibility_and_source_sets() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/web/GetMapping.java",
        "package com.demo.web;\n\npublic @interface GetMapping { String value(); }\n",
    );
    write(
        dir.path(),
        "src/test/java/com/demo/web/PostMapping.java",
        "package com.demo.web;\n\npublic @interface PostMapping { String value(); }\n",
    );
    write(
        dir.path(),
        "src/test/kotlin/com/demo/web/PutMapping.kt",
        "package com.demo.web\n\nannotation class PutMapping(val value: String = \"\")\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/Hidden.kt",
        "package com.demo.web\n\nprivate annotation class DeleteMapping(val value: String = \"\")\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/Users.kt",
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
class Users {
    @GetMapping("/users")
    fun list(): String = "l"

    @PostMapping("/users")
    fun create(): String = "c"

    @PutMapping("/users")
    fun replace(): String = "r"

    @DeleteMapping("/users")
    fun remove(): String = "d"
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/Own.kt",
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

private annotation class PatchMapping(val value: String = "")

@RestController
class Own {
    @PatchMapping("/own")
    fun patch(): String = "p"
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/Root.kt",
        r#"import org.springframework.web.bind.annotation.*

annotation class GetMapping(val value: String = "")

@RestController
class Root {
    @GetMapping("/root")
    fun root(): String = "r"

    @PostMapping("/root")
    fun post(): String = "p"
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let gap = |path: &str, member: &str| {
        (
            format!("gap sym:local/demo@src/main/kotlin/{path}#{member}"),
            SHADOWED.to_string(),
        )
    };
    assert_eq!(
        route_rows(&out),
        [
            ("ep:local/demo@DELETE:/users".to_string(), String::new()),
            ("ep:local/demo@POST:/root".to_string(), String::new()),
            ("ep:local/demo@POST:/users".to_string(), String::new()),
            ("ep:local/demo@PUT:/users".to_string(), String::new()),
            gap("Root.kt", "Root.root"),
            gap("com/demo/web/Own.kt", "Own.patch"),
            gap("com/demo/web/Users.kt", "Users.list"),
        ]
    );
}

// AC-0443 (#1302): the soft keywords the grammar lexes as keywords still
// name declarations. `dynamic` and `suspend` as function, property and
// parameter names keep their Symbols with exact spans and source text, a
// class under stacked annotations keeps its members and endpoints, and a
// real `suspend` modifier and `dynamic` type still parse.
#[test]
fn soft_keyword_names_keep_their_declarations() {
    let path = "src/main/kotlin/com/demo/web/Both.kt";
    let source = r#"package com.demo.web

import org.springframework.web.bind.annotation.RestController
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.stereotype.Controller

@RestController
@Controller
class Both {
    @GetMapping("/both")
    fun both(): String = "b"

    @GetMapping("/dynamic")
    fun dynamic(): String = "dynamic"

    fun suspend(dynamic: Int): Int = dynamic

    suspend fun load(): dynamic = null

    fun /* named */ suspend(name: String): String = name

    fun ref(): Int = suspend

    suspend /* modifier */ fun later(block: suspend () -> Unit) {}

    /* leading */ suspend fun first(): Int {
        return 1
    }

    /* // = */ suspend fun second(): Int = 2

    val dynamicValue = 1
}
"#;
    let out = extract_source(source.as_bytes(), path, &id()).unwrap();
    let mut endpoints: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    endpoints.sort();
    assert_eq!(
        endpoints,
        ["ep:local/demo@GET:/both", "ep:local/demo@GET:/dynamic"]
    );
    for member in ["Both.both", "Both.dynamic", "Both.suspend", "Both.load"] {
        let symbol = node(&out.nodes, &format!("sym:local/demo@{path}#{member}"));
        let span = &symbol.props["prov"]["evidence"][0];
        let start = span["byte_start"].as_u64().unwrap() as usize;
        let end = span["byte_end"].as_u64().unwrap() as usize;
        let name = member.rsplit('.').next().unwrap();
        assert!(
            source[start..end].contains(&format!("fun {name}(")),
            "{member}: {}",
            &source[start..end]
        );
        assert_eq!(symbol.props["name"], member);
    }
    // A commented name, a reference ending an expression body and a modifier
    // separated by a comment all keep the members around them.
    for member in ["Both.ref", "Both.later", "Both.first", "Both.second"] {
        node(&out.nodes, &format!("sym:local/demo@{path}#{member}"));
    }
    assert!(out.nodes.iter().any(|node| {
        node.props["name"] == "Both.suspend"
            && node.props["prov"]["evidence"][0]["byte_start"]
                .as_u64()
                .is_some_and(|start| {
                    source[start as usize..].starts_with("fun /* named */ suspend(")
                })
    }));
}

// AC-0443: the mask is the same length, respells only whole words, and
// keeps `suspend` wherever it is a modifier or a call.
#[test]
fn soft_keyword_mask_respells_only_names() {
    for (source, masked) in [
        (
            "val dynamic = x.dynamicValue",
            "val dynami_ = x.dynamicValue",
        ),
        (
            "fun f(): dynamic = dynamic(1)",
            "fun f(): dynami_ = dynami_(1)",
        ),
        (
            "val suspend = 1\nvar  suspend = 2",
            "val suspen_ = 1\nvar  suspen_ = 2",
        ),
        ("fun suspend() {}", "fun suspen_() {}"),
        (
            "suspend fun go() = suspend { }",
            "suspend fun go() = suspend { }",
        ),
        (
            "funsuspend() undynamic dynamic2",
            "funsuspend() undynamic dynamic2",
        ),
        ("éval suspend fun x()", "éval suspend fun x()"),
        ("fun f() = suspend", "fun f() = suspen_"),
        ("fun /* c */ suspend() = 1", "fun         suspen_() = 1"),
        (
            "fun f(suspend: Int) = suspend",
            "fun f(suspen_: Int) = suspen_",
        ),
        (
            "val g = suspend\nfun h() = 1",
            "val g = suspen_\nfun h() = 1",
        ),
        (
            "x = suspend.invoke() + suspend(1)",
            "x = suspen_.invoke() + suspen_(1)",
        ),
        (
            "private suspend fun a()\n@A suspend fun b()\nsuspend /* c */ fun c()",
            "private suspend fun a()\n@A suspend fun b()\nsuspend         fun c()",
        ),
        (
            "val t: suspend () -> Unit\nfun r(b: suspend String.() -> Unit)",
            "val t: suspend () -> Unit\nfun r(b: suspend String.() -> Unit)",
        ),
        ("return suspend\nfun z()", "return suspen_\nfun z()"),
        ("x in suspend\ny", "x in suspen_\ny"),
        (
            "/* c */ suspend fun a()\n// note\nsuspend fun b()",
            "        suspend fun a()\n// note\nsuspend fun b()",
        ),
        (
            "val u = \"http://x\"\nsuspend fun c()",
            "val u = \"http://x\"\nsuspend fun c()",
        ),
        (
            "x = // why\n  suspend\nfun d()",
            "x = // why\n  suspen_\nfun d()",
        ),
        (
            "x = /* why */ suspend\nfun e()",
            "x =           suspen_\nfun e()",
        ),
        (
            "/* // note */ suspend fun f()\n/* a /* b */ = */ suspend fun g()",
            "              suspend fun f()\n                  suspend fun g()",
        ),
        (
            "val s = \"= /* (\"\nsuspend fun h()\nval c = '='\nsuspend fun i()",
            "val s = \"= /* (\"\nsuspend fun h()\nval c = '='\nsuspend fun i()",
        ),
        (
            "val r = \"\"\"x = \"\"\"\nsuspend fun j()",
            "val r = \"\"\"x = \"\"\"\nsuspend fun j()",
        ),
        (
            "val t = \"${suspend}\" + \"suspend dynamic\"",
            "val t = \"${suspen_}\" + \"suspen_ dynami_\"",
        ),
    ] {
        let out = mask_soft_keyword_names(source.as_bytes());
        assert_eq!(std::str::from_utf8(&out).unwrap(), masked, "{source}");
    }
}

// AC-0444 (#1306): a block comment on the line before a declaration, even
// after an expression body, keeps that declaration and its class.
#[test]
fn block_comments_keep_the_next_declaration() {
    let path = "src/main/kotlin/com/demo/A.kt";
    let source = "package com.demo\n\nclass A {\n    fun x() = 1\n    /* c */ fun y() = 1\n    /* c */ private fun z() = 1\n    /* a /* nested */ b */ inline fun w() = 1\n    /** doc\n     * lines */\n    val v = \"/* not a comment */\"\n}\n";
    let out = extract_source(source.as_bytes(), path, &id()).unwrap();
    for (member, text) in [
        ("A.y", "fun y() = 1"),
        ("A.z", "private fun z() = 1"),
        ("A.w", "inline fun w() = 1"),
    ] {
        let symbol = node(&out.nodes, &format!("sym:local/demo@{path}#{member}"));
        let span = &symbol.props["prov"]["evidence"][0];
        let start = span["byte_start"].as_u64().unwrap() as usize;
        let end = span["byte_end"].as_u64().unwrap() as usize;
        assert!(
            source[start..end].contains(text),
            "{member}: {}",
            &source[start..end]
        );
    }
    let masked = mask_soft_keyword_names(source.as_bytes());
    assert_eq!(masked.len(), source.len());
    let masked = std::str::from_utf8(&masked).unwrap();
    assert!(masked.contains("val v = \"/* not a comment */\""));
    assert!(masked.contains("    fun y() = 1"));
    assert_eq!(masked.matches('\n').count(), source.matches('\n').count());
}

fn rest_sites(source: &str) -> Vec<(String, String, Option<String>)> {
    retrofit_sites(&[("src/Client.kt", source)])
        .into_iter()
        .map(|site| {
            let url = match site.url {
                IdentityExpr::Literal(url) => format!("literal {url}"),
                IdentityExpr::Computed(text) => format!("computed {text}"),
                other => format!("{other:?}"),
            };
            (site.method, url, site.symbol)
        })
        .collect()
}

// AC-0456 (#1341 review): an untyped local or property initialized by the
// fully qualified constructor is proven, unless a binding of its first
// segment's name is visible where it is declared. (T-0456)
#[test]
fn rest_template_fully_qualified_constructed_bindings_are_proven() {
    let sites = rest_sites(
        r#"package io.demo

class Client {
    private val prop = org.springframework.web.client.RestTemplate()

    fun a() {
        val local = org.springframework.web.client.RestTemplate()
        local.delete("https://a.example/local")
        prop.delete("https://a.example/prop")
    }

    fun obscured(org: Factory) {
        val shadowed = org.springframework.web.client.RestTemplate()
        shadowed.delete("https://a.example/obscured")
    }

    fun other() {
        val wrong = org.example.RestTemplate()
        wrong.delete("https://a.example/wrong")
    }

    fun earlier() {
        val org = Factory()
        val local = org.springframework.web.client.RestTemplate()
        local.delete("https://a.example/earlier")
    }
}

class Holder(private val org: Factory) {
    private val held = org.springframework.web.client.RestTemplate()

    fun b() = held.delete("https://a.example/held")
}

class SelfNamed {
    private val org = org.springframework.web.client.RestTemplate()

    fun c() = org.delete("https://a.example/self")
}
"#,
    );
    let urls: Vec<_> = sites.into_iter().map(|(_, url, _)| url).collect();
    assert_eq!(
        urls,
        [
            "literal https://a.example/local",
            "literal https://a.example/prop"
        ]
    );
}

// AC-0456 (#1341 review): a property named like its own qualified
// constructor's first segment, or properties initialized through each other,
// terminate and prove nothing. (T-0456)
#[test]
fn rest_template_self_and_cyclic_qualified_initializers_terminate() {
    let sites = rest_sites(
        r#"package io.demo

val org = org.springframework.web.client.RestTemplate()
val a = b.springframework.web.client.RestTemplate()
val b = a.springframework.web.client.RestTemplate()

fun f() {
    org.delete("https://a.example/top")
    a.delete("https://a.example/a")
    b.delete("https://a.example/b")
}
"#,
    );
    assert_eq!(sites, []);
}

// AC-0456 (#1341 review): a type-argument call is Spring's reified extension
// only under its import, and a file declaring a verb-named function reads no
// call of that name. (T-0456)
#[test]
fn rest_template_extension_calls_need_spring_extension_proof() {
    let source = |imports: &str, extra: &str| {
        format!(
            r#"package io.demo

import org.springframework.web.client.RestTemplate
{imports}

class Client(private val rest: RestTemplate) {{
    fun typed(): User = rest.getForObject<User>("https://api.example.com/me/{{v}}", 2)

    fun member(): User = rest.getForObject("https://api.example.com/users/1", User::class.java)

    fun remove() = rest.delete("https://api.example.com/users/1")
}}
{extra}
"#
        )
    };
    let methods = |source: String| {
        rest_sites(&source)
            .into_iter()
            .map(|(method, url, _)| format!("{method} {url}"))
            .collect::<Vec<_>>()
    };
    let me = "GET literal https://api.example.com/me/{v}";
    let member = "GET literal https://api.example.com/users/1";
    let remove = "DELETE literal https://api.example.com/users/1";
    assert_eq!(methods(source("", "")), [member, remove]);
    assert_eq!(
        methods(source(
            "import org.springframework.web.client.getForObject",
            ""
        )),
        [me, member, remove]
    );
    assert_eq!(
        methods(source("import org.springframework.web.client.*", "")),
        [me, member, remove]
    );
    // A named import of another extension beats Spring's wildcard.
    assert_eq!(
        methods(source(
            "import org.springframework.web.client.*\nimport custom.getForObject",
            ""
        )),
        [member, remove]
    );
    assert_eq!(
        methods(source(
            "import org.springframework.web.client.getForObject",
            "inline fun <reified T> RestTemplate.getForObject(url: String, marker: Int): T = TODO()",
        )),
        [remove]
    );
}

// AC-0456: calls on a proven Spring `RestTemplate` (a typed constructor or
// class property, `this.x`, a typed or constructed local, or an inline
// `RestTemplate()`) become client calls. Verb methods give their method,
// `exchange` an import-proven `HttpMethod` or `?` with four or more
// arguments, and a non-literal URL is Computed. (T-0456)
#[test]
fn rest_template_calls_on_proven_receivers_become_client_calls() {
    let sites = rest_sites(
        r#"package io.demo

import org.springframework.http.HttpMethod
import org.springframework.web.client.RestTemplate
import org.springframework.web.client.getForObject

class Client(private val rest: RestTemplate) {
    private val other: RestTemplate = RestTemplate()

    fun user(id: Int): User = rest.getForObject("https://api.example.com/users/{id}", User::class.java, id)

    fun reified(): User = rest.getForObject<User>("https://api.example.com/me/{v}", 2)

    // The grammar reads a one-argument `f<T>(x)` as comparisons.
    fun comparison(): User = rest.getForObject<User>("https://api.example.com/me")

    fun remove() = this.rest.delete("https://api.example.com/users/1")

    fun create(base: String, body: User) {
        val local = RestTemplate()
        local.postForEntity("$base/users", body, User::class.java)
        other.put(base + "/users", body)
    }

    fun inline() = RestTemplate().headForHeaders("https://api.example.com/health")

    fun patch(entity: HttpEntity<User>) =
        rest.exchange("https://api.example.com/users/1", HttpMethod.PATCH, entity, User::class.java)

    fun unknown(method: HttpMethod, entity: HttpEntity<User>) =
        rest.exchange("https://api.example.com/users/2", method, entity, User::class.java)
}
"#,
    );
    let symbol = |name: &str| Some(format!("sym:local/demo@src/Client.kt#Client.{name}"));
    assert_eq!(
        sites,
        [
            (
                "GET".into(),
                "literal https://api.example.com/users/{id}".into(),
                symbol("user")
            ),
            (
                "GET".into(),
                "literal https://api.example.com/me/{v}".into(),
                symbol("reified")
            ),
            (
                "DELETE".into(),
                "literal https://api.example.com/users/1".into(),
                symbol("remove")
            ),
            (
                "POST".into(),
                "computed \"$base/users\"".into(),
                symbol("create")
            ),
            (
                "PUT".into(),
                "computed base + \"/users\"".into(),
                symbol("create")
            ),
            (
                "HEAD".into(),
                "literal https://api.example.com/health".into(),
                symbol("inline")
            ),
            (
                "PATCH".into(),
                "literal https://api.example.com/users/1".into(),
                symbol("patch")
            ),
            (
                "?".into(),
                "literal https://api.example.com/users/2".into(),
                symbol("unknown")
            ),
        ]
    );
}

// AC-0456: no call is read without proof: a receiver of another, star-imported
// or locally declared `RestTemplate`, an untyped or shadowing binding, a
// two-argument `exchange(RequestEntity, …)`, named arguments or a trailing
// lambda. (T-0456)
#[test]
fn rest_template_calls_need_a_proven_receiver_and_shape() {
    let unproven = [
        r#"package io.demo
import com.other.RestTemplate
fun f(rest: RestTemplate) = rest.getForObject("https://a.example/x", String::class.java)
"#,
        r#"package io.demo
import org.springframework.web.client.*
fun f(rest: RestTemplate) = rest.getForObject("https://a.example/x", String::class.java)
"#,
        r#"package io.demo
import org.springframework.web.client.RestTemplate
class RestTemplate
fun f(rest: RestTemplate) = rest.getForObject("https://a.example/x", String::class.java)
"#,
        r#"package io.demo
import org.springframework.web.client.RestTemplate
fun f(rest: RestTemplate) {
    val rest = make()
    rest.getForObject("https://a.example/x", String::class.java)
}
"#,
        r#"package io.demo
import org.springframework.web.client.RestTemplate
fun f(rest: RestTemplate, request: RequestEntity<String>) {
    rest.exchange(request, String::class.java)
    rest.getForObject(url = "https://a.example/x", responseType = String::class.java)
    rest.execute("https://a.example/x", method, null) { it }
}
"#,
        // A binding of the qualifier's first segment wins over the package.
        r#"package io.demo
fun f(org: Factory) = org.springframework.web.client.RestTemplate().delete("https://a.example/x")
"#,
        // A type parameter of the imported name wins over the import.
        r#"package io.demo
import org.springframework.web.client.RestTemplate
fun <RestTemplate> f(rest: RestTemplate) = rest.delete("https://a.example/x")
class C<RestTemplate>(private val rest: RestTemplate) {
    fun g() = rest.delete("https://a.example/x")
}
"#,
    ];
    for source in unproven {
        assert_eq!(rest_sites(source), [], "{source}");
    }
    // The fully qualified name needs no import, and an unrelated type
    // parameter or binding obscures nothing.
    let symbol = |name: &str| Some(format!("sym:local/demo@src/Client.kt#{name}"));
    assert_eq!(
        rest_sites(
            r#"package io.demo
import org.springframework.web.client.RestTemplate
fun f(rest: org.springframework.web.client.RestTemplate) = rest.delete("https://a.example/x")
fun g(net: Factory) = org.springframework.web.client.RestTemplate().delete("https://a.example/y")
fun <T> h(rest: RestTemplate, value: T) = rest.delete("https://a.example/z")
class Holder(private val rest: RestTemplate) {
    // A member's own type parameter does not reach the property's type.
    fun <RestTemplate> remove(value: RestTemplate) = rest.delete("https://a.example/w")
}
"#
        ),
        [
            (
                "DELETE".into(),
                "literal https://a.example/x".into(),
                symbol("f")
            ),
            (
                "DELETE".into(),
                "literal https://a.example/y".into(),
                symbol("g")
            ),
            (
                "DELETE".into(),
                "literal https://a.example/z".into(),
                symbol("h")
            ),
            (
                "DELETE".into(),
                "literal https://a.example/w".into(),
                symbol("Holder.remove")
            ),
        ]
    );
}

fn webclient_sites(source: &str) -> Vec<(String, String, Option<String>, Option<String>)> {
    let show = |url: IdentityExpr| match url {
        IdentityExpr::Literal(url) => format!("literal {url}"),
        IdentityExpr::Computed(text) => format!("computed {text}"),
        other => format!("{other:?}"),
    };
    retrofit_sites(&[("src/Client.kt", source)])
        .into_iter()
        .map(|site| {
            (
                site.method,
                show(site.url),
                site.base.map(|base| show(base.url)),
                site.entry,
            )
        })
        .collect()
}

// AC-0456 (#1341 review): a RestTemplate call carries no base, and a repo's
// Retrofit base never joins it. (T-0456)
#[test]
fn rest_template_calls_join_no_retrofit_base() {
    let client = r#"package io.demo
import org.springframework.web.client.RestTemplate
fun f(rest: RestTemplate) = rest.delete("/users/1")
"#;
    let module = r#"package io.demo
import retrofit2.Retrofit
fun provide() = Retrofit.Builder().baseUrl("https://app.example.io").build()
"#;
    for files in [
        vec![("src/Client.kt", client)],
        vec![("src/Client.kt", client), ("src/Net.kt", module)],
    ] {
        let sites = retrofit_sites(&files);
        assert_eq!(sites.len(), 1);
        assert!(sites[0].base.is_none());
    }
    let single = extract_source(client.as_bytes(), "src/Client.kt", &id()).unwrap();
    assert_eq!(single.fetch_sites.len(), 1);
    assert!(single.fetch_sites[0].base.is_none());
}

// AC-0457: `<client>.<verb>().uri(…)` chains on a proven Spring `WebClient`
// (a typed property or parameter, `this.x`, an inline `create` or builder)
// become client calls with their method and base: a literal `create` or last
// `baseUrl`, a runtime base for a held client or an opaque builder, and none
// for an absolute request URL. (T-0457)
#[test]
fn webclient_chains_on_proven_clients_become_client_calls() {
    let sites = webclient_sites(
        r#"package io.demo

import org.springframework.http.HttpMethod
import org.springframework.web.reactive.function.client.WebClient

class Orders(private val client: WebClient) {
    fun get(id: String) = client.get().uri("/orders/{id}", id).retrieve()

    fun this_held() = this.client.delete().uri("/orders/1").retrieve()

    fun created() = WebClient.create("https://orders.example.com/").post().uri("/orders").retrieve()

    fun built(path: String) =
        WebClient.builder().baseUrl("https://a.example").baseUrl("https://b.example").build()
            .put().uri(path).retrieve()

    fun opaque() = WebClient.builder().baseUrl("https://a.example").apply { it }.build()
        .patch().uri("/x").retrieve()

    fun absolute() = client.get().uri("https://other.example/x").retrieve()

    fun method(other: WebClient) = other.method(HttpMethod.OPTIONS).uri("/o").retrieve()

    fun unknown(m: HttpMethod) = client.method(m).uri("/u").retrieve()
}
"#,
    );
    let entry = |name: &str| Some(format!("Orders.{name}"));
    let s = |text: &str| text.to_string();
    assert_eq!(
        sites,
        [
            (
                s("GET"),
                s("literal /orders/{id}"),
                Some(s("computed client")),
                entry("get")
            ),
            (
                s("DELETE"),
                s("literal /orders/1"),
                Some(s("computed this.client")),
                entry("this_held")
            ),
            (
                s("POST"),
                s("literal /orders"),
                Some(s("literal https://orders.example.com")),
                entry("created")
            ),
            (
                s("PUT"),
                s("computed path"),
                Some(s("literal https://b.example")),
                entry("built")
            ),
            (
                s("PATCH"),
                s("literal /x"),
                Some(s(
                    "computed WebClient.builder().baseUrl(\"https://a.example\").apply { it }.build()"
                )),
                entry("opaque")
            ),
            (
                s("GET"),
                s("literal https://other.example/x"),
                None,
                entry("absolute")
            ),
            (
                s("OPTIONS"),
                s("literal /o"),
                Some(s("computed other")),
                entry("method")
            ),
            (
                s("?"),
                s("literal /u"),
                Some(s("computed client")),
                entry("unknown")
            ),
        ]
    );
}

// AC-0457 (#1345 review): a literal base keeps its trailing `/` before a
// relative request path, as Spring joins `/api/` + `orders` to
// `/api/orders`; before a rooted or computed path the doubled `/` collapses,
// and a base without the slash concatenates as Spring does. (T-0457)
#[test]
fn webclient_bases_keep_their_slash_before_relative_paths() {
    let sites = webclient_sites(
        r#"package io.demo

import org.springframework.web.reactive.function.client.WebClient

class Orders {
    fun relative() = WebClient.create("https://host/api/").get().uri("orders").retrieve()

    fun rooted() = WebClient.create("https://host/api/").get().uri("/orders").retrieve()

    fun computed(path: String) =
        WebClient.builder().baseUrl("https://host/api/").build().get().uri(path).retrieve()

    fun bare() = WebClient.create("https://host/api").get().uri("orders").retrieve()
}
"#,
    );
    let bases: Vec<_> = sites
        .into_iter()
        .map(|(_, _, base, entry)| (entry.unwrap(), base.unwrap()))
        .collect();
    let s = |text: &str| text.to_string();
    assert_eq!(
        bases,
        [
            (s("Orders.relative"), s("literal https://host/api/")),
            (s("Orders.rooted"), s("literal https://host/api")),
            (s("Orders.computed"), s("literal https://host/api")),
            (s("Orders.bare"), s("literal https://host/api")),
        ]
    );
}

// AC-0457: no chain is read without a proven client: an untyped property, a
// star import, another or a locally declared `WebClient`, a shadowed type
// name, or named `uri` arguments. (T-0457)
#[test]
fn webclient_chains_need_a_proven_client() {
    let unproven = [
        r#"package io.demo
import org.springframework.web.reactive.function.client.WebClient
class A { private val client = WebClient.create("https://a.example")
fun f() = client.get().uri("/x").retrieve() }
"#,
        r#"package io.demo
import org.springframework.web.reactive.function.client.*
fun f(client: WebClient) = client.get().uri("/x").retrieve()
"#,
        r#"package io.demo
import com.other.WebClient
fun f(client: WebClient) = client.get().uri("/x").retrieve()
"#,
        r#"package io.demo
import org.springframework.web.reactive.function.client.WebClient
interface WebClient
fun f(client: WebClient) = client.get().uri("/x").retrieve()
"#,
        r#"package io.demo
import org.springframework.web.reactive.function.client.WebClient
fun f(WebClient: Factory) = WebClient.create("https://a.example").get().uri("/x").retrieve()
"#,
        r#"package io.demo
import org.springframework.web.reactive.function.client.WebClient
fun f(client: WebClient) = client.get().uri(uri = "/x").retrieve()
"#,
        r#"package io.demo
fun f(org: Factory) =
    org.springframework.web.reactive.function.client.WebClient.create().get().uri("/x").retrieve()
"#,
        r#"package io.demo
import org.springframework.web.reactive.function.client.WebClient
fun <WebClient> f(client: WebClient) = client.get().uri("/x").retrieve()
"#,
    ];
    for source in unproven {
        assert_eq!(webclient_sites(source), [], "{source}");
    }
    // The fully qualified `create` needs no import.
    assert_eq!(
        webclient_sites(
            r#"package io.demo
fun f() = org.springframework.web.reactive.function.client.WebClient.create().get().uri("/x").retrieve()
"#
        ),
        [("GET".into(), "literal /x".into(), None, Some("f".into()))]
    );
}

// AC-0457 (#1345 review): a builder link that is no `WebClient.Builder`
// member may be an extension returning another type, so the chain proves no
// client; a member link taking a lambda keeps the client with a runtime
// base. A `uri` whose lone further argument is not a literal may be a
// `Function<UriBuilder, URI>`, so its URL is computed; template variables
// alongside it keep the literal. (T-0457)
#[test]
fn webclient_builders_need_member_links_and_uri_functions_are_computed() {
    let head =
        "package io.demo\nimport org.springframework.web.reactive.function.client.WebClient\n";
    for chain in [
        r#"WebClient.builder().fake().build()"#,
        r#"WebClient.builder().baseUrl("https://a.example").configure { }.build()"#,
        r#"WebClient.builder().configure().baseUrl("https://a.example").build()"#,
        r#"WebClient.builder().clone("x").build()"#,
        r#"WebClient.builder().clone { }.build()"#,
        r#"WebClient.builder().baseUrl("https://a.example", "b").build()"#,
        r#"WebClient.builder().baseUrl { "https://a.example" }.build()"#,
        r#"WebClient.builder().baseUrl(7).build()"#,
        r#"WebClient.builder().defaultHeader("a", true).build()"#,
    ] {
        let source = format!("{head}fun f() = {chain}.get().uri(\"/x\").retrieve()\n");
        assert_eq!(webclient_sites(&source), [], "{chain}");
    }
    let sites = webclient_sites(&format!(
        "{head}{}",
        r#"fun f() = WebClient.builder().baseUrl("https://a.example").filters { }.build().get().uri("/x").retrieve()
fun g() = WebClient.builder().baseUrl("https://a.example").exchangeFunction(exchange).build().get().uri("/x").retrieve()
fun h(client: WebClient, fn: java.util.function.Function<UriBuilder, java.net.URI>) {
    client.get().uri("/a", fn).retrieve()
    client.get().uri("/b", { builder -> builder.build() }).retrieve()
    client.get().uri("/c/{id}", 7).retrieve()
    client.get().uri("/d/{a}/{b}", a, b).retrieve()
}
fun i(client: WebClient, id: Long, any: Any) {
    client.get().uri("/e/{id}", id).retrieve()
    client.get().uri("/f/{id}", any).retrieve()
}
class String
fun j(client: WebClient, id: String) = client.get().uri("/g/{id}", id).retrieve()
"#
    ));
    let urls: Vec<&str> = sites.iter().map(|(_, url, _, _)| url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "literal /x",
            "literal /x",
            "computed \"/a\"",
            "computed \"/b\"",
            "literal /c/{id}",
            "literal /d/{a}/{b}",
            "literal /e/{id}",
            "computed \"/f/{id}\"",
            "computed \"/g/{id}\"",
        ]
    );
    assert!(sites[..2].iter().all(|(_, _, base, _)| {
        base.as_deref()
            .is_some_and(|base| base.starts_with("computed"))
    }));

    // A function of a member's name that the file declares or imports may be
    // what a link calls, whatever its argument types.
    for shadow in [
        "fun WebClient.Builder.baseUrl(url: String): Fake = Fake()\n",
        "import custom.baseUrl\n",
    ] {
        let source = format!(
            "{head}{shadow}{}",
            r#"fun f() = WebClient.builder().baseUrl("https://a.example").build().get().uri("/x").retrieve()"#
        );
        assert_eq!(webclient_sites(&source), [], "{shadow}");
    }

    // A zero-argument `clone()` is the member and keeps the literal base. A
    // type parameter named `String` where the binding is declared may be a
    // function type, while one declared by another function does not
    // rebind `String` here.
    let sites = webclient_sites(&format!(
        "{head}{}",
        r#"fun k() = WebClient.builder().baseUrl("https://a.example").clone().build().get().uri("/k").retrieve()
fun q(url: String) = WebClient.builder().baseUrl(url).build().get().uri("/q").retrieve()
fun <String : java.util.function.Function<UriBuilder, java.net.URI>> m(client: WebClient, fn: String) =
    client.get().uri("/m", fn).retrieve()
class Holder<String : java.util.function.Function<UriBuilder, java.net.URI>>(val client: WebClient) {
    fun p(fn: String) = client.get().uri("/p", fn).retrieve()
}
fun <String> unrelated() {}
fun o(client: WebClient, id: String) = client.get().uri("/o/{id}", id).retrieve()
"#
    ));
    let rows: Vec<(&str, Option<&str>)> = sites
        .iter()
        .map(|(_, url, base, _)| (url.as_str(), base.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("literal /k", Some("literal https://a.example")),
            ("literal /q", Some("computed url")),
            ("computed \"/m\"", Some("computed client")),
            ("computed \"/p\"", Some("computed client")),
            ("literal /o/{id}", Some("computed client")),
        ]
    );
}

fn kafka_rows(source: &str) -> Vec<String> {
    extract_source(
        source.as_bytes(),
        "src/main/kotlin/io/demo/Orders.kt",
        &id(),
    )
    .unwrap()
    .event_sites
    .iter()
    .map(|site| {
        assert_eq!(site.kind, "kafka-topic");
        let identity = match &site.identity {
            adapters_fw::events::IdentityExpr::Literal(topic) => format!("lit:{topic}"),
            adapters_fw::events::IdentityExpr::Computed(text)
            | adapters_fw::events::IdentityExpr::EnvRef(text) => format!("computed:{text}"),
        };
        let owner = site.symbol.as_deref().unwrap_or("-");
        let owner = owner.rsplit_once('#').map_or(owner, |(_, name)| name);
        format!("{:?} {identity} {owner}", site.role)
    })
    .collect()
}

// AC-0468: `send` on a proven `KafkaTemplate` property, `this.x` or
// constructed local produces its first-argument topic; a one-argument send,
// a placeholder or a non-literal topic is computed. Named-import and fully
// qualified `@KafkaListener` functions consume each `topics` element of a
// collection literal or `arrayOf`, and a `topicPattern` is one computed
// site. (T-0468)
#[test]
fn spring_kafka_templates_and_listeners_become_event_sites() {
    let rows = kafka_rows(
        r##"package io.demo

import org.springframework.kafka.annotation.KafkaListener
import org.springframework.kafka.core.KafkaTemplate

class Orders(private val kafka: KafkaTemplate<String, String>) {
    fun place(id: String, record: ProducerRecord<String, String>) {
        kafka.send("orders", id)
        this.kafka.send("audit", id, "x")
        kafka.send("full", 0, 1L, "k", id)
        kafka.send(record)
        kafka.send("${'$'}{topics.orders}", id)
        kafka.send(topicName(), id)
        kafka.send("#{beans.topic}", id)
    }

    fun local(factory: ProducerFactory<String, String>) {
        val template = KafkaTemplate(factory)
        template.send("local", "v")
    }

    @KafkaListener(topics = ["orders", "returns"], groupId = "billing")
    fun onOrder(message: String) {}

    @KafkaListener(topics = arrayOf("audit"))
    fun onAudit(message: String) {}

    @org.springframework.kafka.annotation.KafkaListener(topicPattern = "orders.*")
    fun onAny(message: String) {}
}
"##,
    );
    assert_eq!(
        rows,
        [
            "Produces lit:orders Orders.place",
            "Produces lit:audit Orders.place",
            "Produces lit:full Orders.place",
            "Produces computed:kafka.send(record) Orders.place",
            "Produces computed:\"${'$'}{topics.orders}\" Orders.place",
            "Produces computed:topicName() Orders.place",
            "Produces computed:\"#{beans.topic}\" Orders.place",
            "Produces lit:local Orders.local",
            "Consumes lit:orders Orders.onOrder",
            "Consumes lit:returns Orders.onOrder",
            "Consumes lit:audit Orders.onAudit",
            "Consumes computed:topicPattern = \"orders.*\" Orders.onAny",
        ]
    );
}

// AC-0468 (#1367 review): an import alias proves `@KafkaListener` through the
// same binding check, and `arrayOf<String>(…)` lists its topics; an alias of
// a foreign annotation proves nothing. (T-0468)
#[test]
fn spring_kafka_listener_aliases_and_typed_arrays_are_read() {
    let rows = kafka_rows(
        r#"package io.demo

import org.springframework.kafka.annotation.KafkaListener as Listener
import com.other.KafkaListener as Foreign

class Orders {
    @Listener(topics = arrayOf<String>("orders", "returns"))
    fun onOrder(message: String) {}

    @Foreign(topics = ["ignored"])
    fun onForeign(message: String) {}
}
"#,
    );
    assert_eq!(
        rows,
        [
            "Consumes lit:orders Orders.onOrder",
            "Consumes lit:returns Orders.onOrder",
        ]
    );
}

// AC-0468: an unimported or foreign `KafkaTemplate`, a shadowed receiver, a
// subclass, `sendDefault`, named arguments, a trailing lambda, a file-declared
// or imported `send`, more arguments than any `send` overload takes, and a
// foreign or unimported `@KafkaListener` yield no site. (T-0468)
#[test]
fn unproven_spring_kafka_calls_are_not_event_sites() {
    for source in [
        r#"package io.demo
class A(private val kafka: KafkaTemplate<String, String>) {
    fun f() = kafka.send("t", "v")
    @KafkaListener(topics = ["t"])
    fun g(m: String) {}
}
"#,
        r#"package io.demo
import com.other.KafkaTemplate
import com.other.KafkaListener
class A(private val kafka: KafkaTemplate<String, String>) {
    fun f() = kafka.send("t", "v")
    @KafkaListener(topics = ["t"])
    fun g(m: String) {}
}
"#,
        r#"package io.demo
import org.springframework.kafka.core.KafkaTemplate
import org.springframework.kafka.requestreply.ReplyingKafkaTemplate
class A(private val kafka: KafkaTemplate<String, String>, private val reply: ReplyingKafkaTemplate<String, String, String>) {
    fun f(kafka: Other) = kafka.send("t", "v")
    fun g() {
        reply.send("t", "v")
        this.kafka.sendDefault("v")
        this.kafka.send(topic = "t", data = "v")
        this.kafka.send("t") { }
    }
}
"#,
        r#"package io.demo
import org.springframework.kafka.core.KafkaTemplate
fun KafkaTemplate<String, String>.send(topic: String, data: String): Unit = Unit
class A(private val kafka: KafkaTemplate<String, String>) {
    fun f() = kafka.send("t", "v")
}
"#,
        r#"package io.demo
import org.springframework.kafka.core.KafkaTemplate
import custom.send
class A(private val kafka: KafkaTemplate<String, String>) {
    fun f() = kafka.send("t", "v")
}
"#,
        r#"package io.demo
import org.springframework.kafka.core.KafkaTemplate
class A(private val kafka: KafkaTemplate<String, String>) {
    fun f() = kafka.send("t", 0, 1L, "k", "v", "extra")
}
"#,
    ] {
        assert_eq!(kafka_rows(source), Vec::<String>::new(), "{source}");
    }
}

fn okhttp_rows(files: &[(&str, &str)]) -> Vec<[String; 3]> {
    retrofit_sites(files)
        .into_iter()
        .map(|site| {
            let url = match site.url {
                IdentityExpr::Literal(url) => format!("literal {url}"),
                IdentityExpr::Computed(text) => format!("computed {text}"),
                other => format!("{other:?}"),
            };
            [site.method, url, site.entry.unwrap_or_default()]
        })
        .collect()
}

// AC-0477 (#1409): `Request`-import, aliased, nested `Builder`-import and
// fully qualified (also unimported and spaced) OkHttp builders; the last
// method link wins (default GET), the last `url` wins, `method` keeps a
// token literal without lowercase letters only, and only a pure string
// `url` argument is a literal. (T-0477)
#[test]
fn okhttp_request_builders_become_fetch_sites() {
    let rows = okhttp_rows(&[
        (
            "src/main/kotlin/io/demo/Client.kt",
            r#"package io.demo

import okhttp3.CacheControl
import okhttp3.HttpUrl
import okhttp3.Request
import okhttp3.RequestBody

class Client {
    fun calls(path: String, target: HttpUrl, body: RequestBody) {
        Request.Builder().url("https://host/a").build()
        Request.Builder().url("https://host/b").delete().build()
        Request.Builder().url("https://host/old").url("https://host/c")
            .header("A", "1").addHeader("B", "2").removeHeader("C").tag("t")
            .tag(String::class.java, "u").cacheControl(CacheControl.FORCE_NETWORK)
            .put(body).post(body).build()
        Request.Builder().url("https://host/$path").get().build()
        Request.Builder().url(target).patch(body).build()
        Request.Builder().url("https://host/d").method("patch", body).build()
        Request.Builder().url("https://host/p").method("PROPFIND", null).build()
        Request.Builder().head().url("https://host/h").delete(body).build()
    }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Nested.kt",
            r#"package io.demo

import okhttp3.Request.Builder
import okhttp3.Request as OkRequest

class Nested {
    fun calls() {
        Builder().url("https://host/e").build()
        okhttp3.Request.Builder().url("https://host/f").head().build()
        OkRequest.Builder().url("https://host/g").build()
    }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Bare.kt",
            r#"package io.demo

class Bare {
    fun calls() = okhttp3 . Request . Builder().url("https://host/i").build()
}
"#,
        ),
    ]);
    let rows: Vec<[&str; 3]> = rows
        .iter()
        .map(|[method, url, entry]| [method.as_str(), url.as_str(), entry.as_str()])
        .collect();
    assert_eq!(
        rows,
        [
            ["GET", "literal https://host/i", "Bare.calls"],
            ["GET", "literal https://host/a", "Client.calls"],
            ["DELETE", "literal https://host/b", "Client.calls"],
            ["POST", "literal https://host/c", "Client.calls"],
            ["GET", "computed \"https://host/$path\"", "Client.calls"],
            ["PATCH", "computed target", "Client.calls"],
            ["?", "literal https://host/d", "Client.calls"],
            ["PROPFIND", "literal https://host/p", "Client.calls"],
            ["DELETE", "literal https://host/h", "Client.calls"],
            ["GET", "literal https://host/e", "Nested.calls"],
            ["HEAD", "literal https://host/f", "Nested.calls"],
            ["GET", "literal https://host/g", "Nested.calls"],
        ]
    );
}

// AC-0477: a foreign, file-declared or unimported `Request`, a binding of
// the first segment, a file-level `okhttp3` object, a wildcard import, a
// file-declared or imported link function, a literal
// the member cannot take, named arguments, a trailing lambda, an unknown or
// wrong-arity link, a held builder and a chain without `url` yield none;
// test sources yield none. (T-0477)
#[test]
fn unproven_okhttp_builders_are_not_fetch_sites() {
    let rows = okhttp_rows(&[
        (
            "src/main/kotlin/io/demo/Client.kt",
            r#"package io.demo

import okhttp3.Request
import okhttp3.RequestBody

class Client {
    fun calls(base: Request.Builder, body: RequestBody) {
        Request.Builder().url("https://host/x").newBuilder().build()
        Request.Builder().url("https://host/x").get(1).build()
        Request.Builder().url("https://host/x").post().build()
        Request.Builder().url("https://host/x").post("text").build()
        Request.Builder().url(42).build()
        Request.Builder().url(url = "https://host/x").build()
        Request.Builder().url("https://host/x").apply { header("A", "1") }.build()
        Request.Builder("seed").url("https://host/x").build()
        Request.Builder().get().build()
        base.url("https://host/x").build()
    }

    fun shadowed(okhttp3: Any) = okhttp3.Request.Builder().url("https://host/x").build()
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Bound.kt",
            r#"package io.demo

import okhttp3.Request

class Bound {
    fun calls(Request: Any) = Request.Builder().url("https://host/x").build()
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Foreign.kt",
            r#"package io.demo

import com.example.Request

class Foreign {
    fun calls() = Request.Builder().url("https://host/x").build()
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Local.kt",
            r#"package io.demo

import okhttp3.Request

class Request

class Local {
    fun calls() = Request.Builder().url("https://host/x").build()
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Extension.kt",
            r#"package io.demo

import okhttp3.Request
import com.example.tag

fun Request.Builder.url(u: Int): Request.Builder = this

class Extension {
    fun calls() {
        Request.Builder().url("https://host/x").build()
        Request.Builder().get().tag("t").url("https://host/y").build()
    }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Captured.kt",
            r#"package io.demo

object okhttp3 {
    object Request {
        class Builder
    }
}

class Captured {
    fun calls() = okhttp3.Request.Builder().url("https://host/x").build()
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Wildcard.kt",
            r#"package io.demo

import com.example.*
import okhttp3.Request

class Wildcard {
    fun calls(endpoint: Endpoint) = Request.Builder().url(endpoint).build()
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Unimported.kt",
            r#"package io.demo

class Unimported {
    fun calls() = Request.Builder().url("https://host/x").build()
}
"#,
        ),
        (
            "src/test/kotlin/io/demo/ClientTest.kt",
            r#"package io.demo

import okhttp3.Request

class ClientTest {
    fun calls() = Request.Builder().url("https://host/x").build()
}
"#,
        ),
    ]);
    assert_eq!(rows, Vec::<[String; 3]>::new());
}

// AC-0471 (#1420): Apache HttpClient 4 and 5 request classes through named
// imports, an alias and fully qualified (also spaced and unimported)
// spellings; the class gives the method and only a pure string argument is
// a literal. (T-0471)
#[test]
fn apache_http_request_constructors_become_fetch_sites() {
    let rows = okhttp_rows(&[
        (
            "src/main/kotlin/io/demo/Client.kt",
            r#"package io.demo

import java.net.URI
import org.apache.http.client.methods.HttpDelete
import org.apache.http.client.methods.HttpGet
import org.apache.http.client.methods.HttpHead
import org.apache.http.client.methods.HttpPost as Post
import org.apache.hc.client5.http.classic.methods.HttpOptions
import org.apache.hc.client5.http.classic.methods.HttpPatch
import org.apache.hc.client5.http.classic.methods.HttpPut
import org.apache.hc.client5.http.classic.methods.HttpTrace

class Client {
    fun calls(path: String, target: URI) {
        HttpGet("https://host/a")
        Post("https://host/b")
        HttpPut("https://host/$path")
        HttpPatch(target)
        HttpDelete("https://host/d")
        HttpHead("https://host/h")
        HttpOptions("https://host/o")
        HttpTrace("https://host/t")
    }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Bare.kt",
            r#"package io.demo

class Bare {
    fun calls() = org . apache . http . client . methods . HttpGet("https://host/q")
}
"#,
        ),
    ]);
    let rows: Vec<[&str; 3]> = rows
        .iter()
        .map(|[method, url, entry]| [method.as_str(), url.as_str(), entry.as_str()])
        .collect();
    assert_eq!(
        rows,
        [
            ["GET", "literal https://host/q", "Bare.calls"],
            ["GET", "literal https://host/a", "Client.calls"],
            ["POST", "literal https://host/b", "Client.calls"],
            ["PUT", "computed \"https://host/$path\"", "Client.calls"],
            ["PATCH", "computed target", "Client.calls"],
            ["DELETE", "literal https://host/d", "Client.calls"],
            ["HEAD", "literal https://host/h", "Client.calls"],
            ["OPTIONS", "literal https://host/o", "Client.calls"],
            ["TRACE", "literal https://host/t", "Client.calls"],
        ]
    );
}

// AC-0471 (#1420): no-argument, named, two-argument and trailing-lambda
// calls, unimported, foreign, file-declared and parameter-bound names, a
// captured or possibly wildcard-captured `org`, and test sources yield no
// site. (T-0471)
#[test]
fn unproven_apache_http_requests_are_not_fetch_sites() {
    let rows = okhttp_rows(&[
        (
            "src/main/kotlin/io/demo/Shapes.kt",
            r#"package io.demo

import org.apache.http.client.methods.HttpGet
import org.apache.http.client.methods.HttpPost

class Shapes {
    fun calls(entity: Any) {
        HttpGet()
        HttpGet(uri = "https://host/x")
        HttpPost("https://host/x", entity)
    }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Trailing.kt",
            r#"package io.demo

import org.apache.http.client.methods.HttpGet

class Trailing {
    fun calls() = HttpGet("https://host/x") { }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Bound.kt",
            r#"package io.demo

import org.apache.http.client.methods.HttpGet

class Bound {
    fun calls(urls: List<String>) = urls.map { HttpGet -> HttpGet("https://host/x") }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Foreign.kt",
            r#"package io.demo

import com.example.HttpGet

class Foreign {
    fun calls() = HttpGet("https://host/x")
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Unimported.kt",
            r#"package io.demo

class Unimported {
    fun calls() = HttpGet("https://host/x")
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Local.kt",
            r#"package io.demo

import org.apache.http.client.methods.HttpGet

class HttpGet(val url: String)

class Local {
    fun calls() = HttpGet("https://host/x")
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Captured.kt",
            r#"package io.demo

object org {
    object apache
}

class Captured {
    fun calls() = org.apache.http.client.methods.HttpGet("https://host/x")
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Wildcard.kt",
            r#"package io.demo

import com.example.*

class Wildcard {
    fun calls() = org.apache.http.client.methods.HttpGet("https://host/x")
}
"#,
        ),
        (
            "src/test/kotlin/io/demo/ClientTest.kt",
            r#"package io.demo

import org.apache.http.client.methods.HttpGet

class ClientTest {
    fun calls() = HttpGet("https://host/x")
}
"#,
        ),
    ]);
    assert!(rows.is_empty(), "{rows:?}");
}

fn ktor_rows(files: &[(&str, &str)]) -> Vec<[String; 4]> {
    let show = |url: IdentityExpr| match url {
        IdentityExpr::Literal(url) => format!("literal {url}"),
        IdentityExpr::Computed(text) => format!("computed {text}"),
        other => format!("{other:?}"),
    };
    retrofit_sites(files)
        .into_iter()
        .map(|site| {
            [
                site.method,
                show(site.url),
                site.base
                    .map(|base| format!("{} {:?}", show(base.url), base.join))
                    .unwrap_or_else(|| "none".to_string()),
                site.entry.unwrap_or_default(),
            ]
        })
        .collect()
}

// AC-0479 (#1416): every verb through named and aliased imports;
// typed, constructed, parameter and `this.x` clients; a block naming
// `method` or `url`; `request`; relative and computed URLs join a runtime
// client base as an RFC 3986 reference, absolute literals keep none.
// (T-0479)
#[test]
fn ktor_client_requests_become_fetch_sites() {
    let rows = ktor_rows(&[(
        "src/main/kotlin/io/demo/Named.kt",
        r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.head
import io.ktor.client.request.options
import io.ktor.client.request.patch
import io.ktor.client.request.post as send
import io.ktor.client.request.put
import io.ktor.client.request.request

class Named(private val client: HttpClient) {
    suspend fun calls(id: String) {
        client.get("https://host/a")
        client.send("https://host/b") { setBody(id) }
        this.client.put("https://host/c")
        client.delete("/orders/$id")
        client.patch(id)
        client.head("https://host/d")
        client.options("https://host/e")
        client.request("https://host/f")
        client.request("https://host/g") { method = HttpMethod.Post }
        client.get("https://host/h") { url { path("x") } }
    }

    suspend fun local() {
        val local = HttpClient(CIO)
        local.get("orders")
    }
}
"#,
    )]);
    let rows: Vec<[&str; 4]> = rows
        .iter()
        .map(|[a, b, c, d]| [a.as_str(), b.as_str(), c.as_str(), d.as_str()])
        .collect();
    assert_eq!(
        rows,
        [
            ["GET", "literal https://host/a", "none", "Named.calls"],
            ["POST", "literal https://host/b", "none", "Named.calls"],
            ["PUT", "literal https://host/c", "none", "Named.calls"],
            [
                "DELETE",
                "computed \"/orders/$id\"",
                "computed client Resolve",
                "Named.calls"
            ],
            [
                "PATCH",
                "computed id",
                "computed client Resolve",
                "Named.calls"
            ],
            ["HEAD", "literal https://host/d", "none", "Named.calls"],
            ["OPTIONS", "literal https://host/e", "none", "Named.calls"],
            ["GET", "literal https://host/f", "none", "Named.calls"],
            ["?", "literal https://host/g", "none", "Named.calls"],
            [
                "GET",
                "computed \"https://host/h\"",
                "computed client Resolve",
                "Named.calls"
            ],
            [
                "GET",
                "literal orders",
                "computed local Resolve",
                "Named.local"
            ],
        ]
    );
    // A request block is part of the cited call.
    let source = "package io.demo\n\nimport io.ktor.client.HttpClient\nimport io.ktor.client.request.request\n\nclass Block(private val client: HttpClient) {\n    suspend fun go() = client.request(\"https://host/z\") { method = HttpMethod.Put }\n}\n";
    let sites = retrofit_sites(&[("src/main/kotlin/io/demo/Block.kt", source)]);
    let [site] = &sites[..] else {
        panic!("{sites:?}");
    };
    assert_eq!(
        &source[site.byte_start as usize..site.byte_end as usize],
        "client.request(\"https://host/z\") { method = HttpMethod.Put }"
    );
}

// AC-0479: an unproven or foreign receiver, another package's `get`, a
// wildcard-only import (a sibling file may declare a winning same-package
// extension), a file-declared `get`, an unimported extension, a
// block-only or named-argument call, two arguments and a test source yield
// none. (T-0479)
#[test]
fn unproven_ktor_client_requests_are_not_fetch_sites() {
    let rows = ktor_rows(&[
        (
            "src/main/kotlin/io/demo/Client.kt",
            r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.get
import com.example.post

class Client(private val client: HttpClient, private val other: Any) {
    suspend fun calls() {
        other.get("https://host/x")
        client.post("https://host/x")
        client.get { url("https://host/x") }
        client.get(urlString = "https://host/x")
        client.get("https://host/x", 1)
        client.put("https://host/x")
    }
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Star.kt",
            r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.*

fun star(client: HttpClient) = client.get("https://host/x")
"#,
        ),
        (
            "src/main/kotlin/io/demo/Shadow.kt",
            r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.get

fun HttpClient.get(path: String): Unit = Unit

class Shadow(private val client: HttpClient) {
    fun calls() = client.get("https://host/x")
}
"#,
        ),
        (
            "src/main/kotlin/io/demo/Foreign.kt",
            r#"package io.demo

import com.example.HttpClient
import io.ktor.client.request.get

class Foreign(private val client: HttpClient) {
    suspend fun calls() = client.get("https://host/x")
}
"#,
        ),
        (
            "src/test/kotlin/io/demo/ClientTest.kt",
            r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.get

class ClientTest(private val client: HttpClient) {
    suspend fun calls() = client.get("https://host/x")
}
"#,
        ),
    ]);
    assert_eq!(rows, Vec::<[String; 4]>::new());
}

// AC-0470 (#1417): a `suspend` declaration right after a star import keeps
// its modifier and its Symbol; a `*` operator still opens an expression.
// (T-0470)
#[test]
fn suspend_after_a_star_import_keeps_its_declaration() {
    for (source, masked) in [
        (
            "import a.b.*\n\nsuspend fun c()",
            "import a.b.*\n\nsuspend fun c()",
        ),
        (
            "import a.b.* // all\nsuspend fun c()",
            "import a.b.* // all\nsuspend fun c()",
        ),
        (
            "import a.b. /* generated */ *\nsuspend fun c()",
            // The block comment is blanked; `suspend` keeps its spelling.
            "import a.b.                 *\nsuspend fun c()",
        ),
        ("x = a * suspend\nfun d()", "x = a * suspen_\nfun d()"),
    ] {
        let out = mask_soft_keyword_names(source.as_bytes());
        assert_eq!(std::str::from_utf8(&out).unwrap(), masked, "{source}");
    }
    let path = "src/main/kotlin/com/demo/Api.kt";
    let source =
        "package com.demo\n\nimport io.ktor.client.request.*\n\nsuspend fun load(): Int = 1\n";
    let out = extract_source(source.as_bytes(), path, &id()).unwrap();
    let symbol = node(&out.nodes, &format!("sym:local/demo@{path}#load"));
    let span = &symbol.props["prov"]["evidence"][0];
    let start = span["byte_start"].as_u64().unwrap() as usize;
    assert!(source[start..].starts_with("suspend fun load("));
}

// AC-0495 (#1426): requests in property and top-level initializers, outside
// any function, are owned by their file in every Kotlin client extractor:
// RestTemplate, WebClient, OkHttp, Ktor and Apache HttpClient. (T-0495)
#[test]
fn client_requests_outside_functions_are_file_owned() {
    let path = "src/main/kotlin/io/demo/Holder.kt";
    let sites = retrofit_sites(&[(
        path,
        r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.get
import okhttp3.Request
import org.apache.http.client.methods.HttpGet
import org.springframework.web.client.RestTemplate
import org.springframework.web.reactive.function.client.WebClient

val top = RestTemplate().getForObject("https://host/r", String::class.java)

class Holder(private val client: HttpClient, private val web: WebClient) {
    val ok = Request.Builder().url("https://host/o").build()
    val apache = HttpGet("https://host/a")
    val ktor = scope.async { client.get("https://host/k") }
    val reactive = web.get().uri("https://host/w").retrieve()
}
"#,
    )]);
    type Row = (String, String, Option<String>, Option<String>, String);
    let rows: Vec<Row> = sites
        .into_iter()
        .map(|site| {
            let url = match site.url {
                IdentityExpr::Literal(url) => url,
                other => format!("{other:?}"),
            };
            (site.method, url, site.symbol, site.entry, site.path)
        })
        .collect();
    let owned = |method: &str, url: &str| {
        (
            method.to_string(),
            url.to_string(),
            None,
            None,
            path.to_string(),
        )
    };
    assert_eq!(
        rows,
        [
            // Grouped by extractor: RestTemplate, WebClient, OkHttp, Apache,
            // Ktor.
            owned("GET", "https://host/r"),
            owned("GET", "https://host/w"),
            owned("GET", "https://host/o"),
            owned("GET", "https://host/a"),
            owned("GET", "https://host/k"),
        ]
    );
}

/// AC-0495: only property and top-level initializers of product sources are
/// file-owned; accessors, `init` blocks, constructor defaults, delegates and
/// test sources own no file-level request, while a test function keeps its
/// own.
#[test]
fn client_requests_outside_initializers_are_not_file_owned() {
    let sites = retrofit_sites(&[
        (
            "src/main/kotlin/io/demo/Lazy.kt",
            r#"package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.get
import okhttp3.Request
import org.apache.http.client.methods.HttpGet
import org.springframework.web.client.RestTemplate
import org.springframework.web.reactive.function.client.WebClient

class Lazy(
    private val client: HttpClient,
    private val web: WebClient,
    val request: Request = Request.Builder().url("https://host/default").build(),
) {
    val reactive get() = web.get().uri("https://host/getter").retrieve()
    val apache by lazy { HttpGet("https://host/delegate") }
    val ktor get() = scope.async { client.get("https://host/ktor-getter") }

    init {
        val local = RestTemplate().getForObject("https://host/init", String::class.java)
    }
}
"#,
        ),
        (
            "src/test/kotlin/io/demo/HolderTest.kt",
            r#"package io.demo

import org.springframework.web.client.RestTemplate
import org.springframework.web.reactive.function.client.WebClient

val top = RestTemplate().getForObject("https://host/test-top", String::class.java)

class HolderTest(private val web: WebClient) {
    val reactive = web.get().uri("https://host/test-w").retrieve()

    fun call() = RestTemplate().getForObject("https://host/test-fn", String::class.java)
}
"#,
        ),
    ]);
    let rows: Vec<(String, Option<String>)> = sites
        .into_iter()
        .map(|site| (format!("{:?}", site.url), site.symbol))
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].0.contains("https://host/test-fn"), "{rows:?}");
    assert!(
        rows[0]
            .1
            .as_deref()
            .is_some_and(|s| s.ends_with("#HolderTest.call")),
        "{rows:?}"
    );
}

fn endpoint_ids(out: &Extraction) -> Vec<String> {
    let mut ids: Vec<String> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.clone())
        .collect();
    ids.sort();
    ids
}

fn route_gaps(out: &Extraction) -> Vec<(String, String)> {
    let mut gaps: Vec<(String, String)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:route:"))
        .map(|node| {
            (
                node.props["method"].as_str().unwrap().to_string(),
                node.props["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    gaps.sort();
    gaps.dedup();
    gaps
}

// AC-0557: function-level @RequestMapping handlers are endpoints or route
// Gaps, never silently dropped; class and function `method =` restrictions
// combine by union (Spring RequestMethodsRequestCondition.combine), and
// literal path arrays are routes (#1605). An empty `[]` argument is not
// parsed by tree-sitter-kotlin-ng, so it is not exercised here.
#[test]
fn spring_function_request_mappings_yield_verbs_or_gaps() {
    let legacy = br#"package com.demo.web

import org.springframework.stereotype.Controller
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RequestMethod
import org.springframework.web.bind.annotation.RequestMethod as Verb

@Controller
@RequestMapping("/legacy")
class LegacyController {
    @RequestMapping("/all")
    fun any(): String = "a"

    @RequestMapping(value = ["/one"], method = [RequestMethod.POST])
    fun one(): String = "o"

    @RequestMapping(path = ["/two"], method = arrayOf(RequestMethod.PUT, Verb.GET))
    fun two(): String = "t"

    @RequestMapping(value = ["/single"], method = [org.springframework.web.bind.annotation.RequestMethod.DELETE])
    fun single(): String = "s"

    @RequestMapping(value = ["/a", "/b"], produces = ["text/plain"])
    fun many(): String = "m"

    @RequestMapping(value = ["/opaque"], method = METHODS)
    fun opaque(): String = "x"

    @RequestMapping(value = ["/spread"], method = arrayOf(*METHODS))
    fun spread(): String = "p"
}
"#;
    let out = extract_source(
        legacy,
        "src/main/kotlin/com/demo/web/LegacyController.kt",
        &id(),
    )
    .unwrap();
    assert_eq!(
        endpoint_ids(&out),
        [
            "ep:local/demo@ALL:/legacy/a",
            "ep:local/demo@ALL:/legacy/all",
            "ep:local/demo@ALL:/legacy/b",
            "ep:local/demo@DELETE:/legacy/single",
            "ep:local/demo@GET:/legacy/two",
            "ep:local/demo@POST:/legacy/one",
            "ep:local/demo@PUT:/legacy/two",
        ]
    );
    assert_eq!(
        route_gaps(&out),
        [(
            "?".to_string(),
            "unresolved Spring request method".to_string()
        )]
    );
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Gap" && node.id.starts_with("gap:route:"))
            .count(),
        2,
        "the constant and the spread each fail closed"
    );

    let inherited = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping(value = ["/api"], method = [RequestMethod.POST])
class PostOnlyController {
    @RequestMapping("/save")
    fun save(): String = "s"

    @RequestMapping(value = ["/both"], method = [RequestMethod.PUT])
    fun both(): String = "b"

    @GetMapping("/read")
    fun read(): String = "r"
}
"#;
    let out = extract_source(
        inherited,
        "src/main/kotlin/com/demo/web/PostOnlyController.kt",
        &id(),
    )
    .unwrap();
    assert_eq!(
        endpoint_ids(&out),
        [
            "ep:local/demo@GET:/api/read",
            "ep:local/demo@POST:/api/both",
            "ep:local/demo@POST:/api/read",
            "ep:local/demo@POST:/api/save",
            "ep:local/demo@PUT:/api/both",
        ]
    );

    let opaque_class = br#"package com.demo.web

import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RestController

@RestController
@RequestMapping(value = ["/opaque"], method = METHODS)
class OpaqueController {
    @GetMapping("/x")
    fun x(): String = "x"
}
"#;
    let out = extract_source(
        opaque_class,
        "src/main/kotlin/com/demo/web/OpaqueController.kt",
        &id(),
    )
    .unwrap();
    assert!(endpoint_ids(&out).is_empty());
    assert_eq!(
        route_gaps(&out),
        [(
            "?".to_string(),
            "unresolved Spring request method".to_string()
        )]
    );

    // A lookalike RequestMapping proves nothing.
    let fake = br#"package com.demo.web

import org.springframework.stereotype.Controller
import com.other.web.RequestMapping

@Controller
class FakeController {
    @RequestMapping("/nope")
    fun get(): String = "n"
}
"#;
    let out = extract_source(
        fake,
        "src/main/kotlin/com/demo/web/FakeController.kt",
        &id(),
    )
    .unwrap();
    assert!(endpoint_ids(&out).is_empty());
    assert!(route_gaps(&out).is_empty());
}

// AC-0557: comments inside an argument never change what it says; a method or
// path argument T0 cannot read fails closed; under a star-only proof an
// unresolved verb settles through shadowing, and one shadowed annotation that
// expands to several verbs and routes yields a single shadow Gap.
#[test]
fn spring_request_mapping_arguments_survive_comments_and_shadowing() {
    let commented = br#"package com.demo.web

import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RequestMethod
import org.springframework.web.bind.annotation.RestController

@RestController
@RequestMapping(value = ["/api"], method /* class */ = [RequestMethod.POST])
class CommentedController {
    @RequestMapping(value = ["/save"], method /* fn */ = [RequestMethod.PUT])
    fun save(): String = "s"

    @RequestMapping(value /* path */ = ["/plain"])
    fun plain(): String = "p"
}
"#;
    let out = extract_source(
        commented,
        "src/main/kotlin/com/demo/web/CommentedController.kt",
        &id(),
    )
    .unwrap();
    assert_eq!(
        endpoint_ids(&out),
        [
            "ep:local/demo@POST:/api/plain",
            "ep:local/demo@POST:/api/save",
            "ep:local/demo@PUT:/api/save",
        ]
    );

    let dir = tempfile::tempdir().unwrap();
    let controller = |package: &str| {
        format!(
            "package {package}\n\nimport org.springframework.web.bind.annotation.*\n\n@RestController\nclass Legacy {{\n    @RequestMapping(value = [\"/x\"], method = METHODS)\n    fun x(): String = \"x\"\n\n    @RequestMapping(value = [\"/a\", \"/b\"], method = [RequestMethod.GET, RequestMethod.POST])\n    fun many(): String = \"m\"\n}}\n"
        )
    };
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/Legacy.kt",
        &controller("com.demo.web"),
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/RequestMapping.kt",
        "package com.demo.web\n\nannotation class RequestMapping(val value: Array<String> = [])\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/plain/Legacy.kt",
        &controller("com.demo.plain"),
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut gaps: Vec<(String, String, String)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.id.starts_with("gap:route:"))
        .map(|node| {
            (
                node.id.split('@').nth(1).unwrap_or_default().to_string(),
                node.props["method"].as_str().unwrap().to_string(),
                node.props["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    gaps.sort();
    let shadowed =
        "Spring annotation proven only by a star import is shadowed by a same-package declaration";
    assert_eq!(
        gaps,
        [
            (
                "src/main/kotlin/com/demo/plain/Legacy.kt".to_string(),
                "?".to_string(),
                "unresolved Spring request method".to_string()
            ),
            (
                "src/main/kotlin/com/demo/web/Legacy.kt".to_string(),
                "?".to_string(),
                shadowed.to_string()
            ),
            (
                "src/main/kotlin/com/demo/web/Legacy.kt".to_string(),
                "GET|POST".to_string(),
                shadowed.to_string()
            ),
        ]
    );
    assert_eq!(
        endpoint_ids(&out),
        [
            "ep:local/demo@GET:/a",
            "ep:local/demo@GET:/b",
            "ep:local/demo@POST:/a",
            "ep:local/demo@POST:/b",
        ]
    );
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

fn config_read_ids(out: &Extraction) -> Vec<&str> {
    out.nodes
        .iter()
        .filter(|node| node.label == "ConfigRead")
        .map(|node| node.id.as_str())
        .collect()
}

// AC-0597 (#1789, ADR-0059): one literal `System.getenv` read, one dynamic
// read and one shadowed look-alike (a local function named `getenv`) yield
// exactly one named ConfigRead, one dynamic ConfigRead and nothing for the
// look-alike. Only the key name and the span of the call are recorded,
// never the fallback value.
#[test]
fn system_getenv_reads_are_config_reads() {
    let text = r#"package com.acme

fun getenv(key: String): String = key

fun load(name: String): List<String?> {
    val url = System.getenv("DATABASE_URL") ?: "postgres://user:pw@db/app"
    return listOf(url, System.getenv(name))
}

fun fake(): String = getenv("SHADOWED")
"#;
    let path = "src/main/kotlin/com/acme/Config.kt";
    let out = extract_source(text.as_bytes(), path, &id()).unwrap();
    assert_eq!(
        config_read_ids(&out),
        [
            "cfgread:local/demo@src/main/kotlin/com/acme/Config.kt:env:DATABASE_URL",
            "cfgread:local/demo@src/main/kotlin/com/acme/Config.kt:env:?",
        ]
    );
    let named = node(
        &out.nodes,
        "cfgread:local/demo@src/main/kotlin/com/acme/Config.kt:env:DATABASE_URL",
    );
    assert_eq!(named.props["source"], "env");
    assert_eq!(named.props["key"], "DATABASE_URL");
    assert_eq!(named.props["language"], "kotlin");
    assert_eq!(named.props["prov"]["tier"], "Deterministic");
    assert_eq!(named.props["prov"]["confidence_tier"], "Confirmed");
    let read = r#"System.getenv("DATABASE_URL")"#;
    let at = text.find(read).unwrap() as u64;
    assert_eq!(evidence_spans(&named.props), [(at, at + read.len() as u64)]);
    let dynamic = node(
        &out.nodes,
        "cfgread:local/demo@src/main/kotlin/com/acme/Config.kt:env:?",
    );
    assert_eq!(dynamic.props["dynamic"], true);
    assert!(dynamic.props.get("key").is_none());
    let at = text.find("System.getenv(name)").unwrap() as u64;
    assert_eq!(
        evidence_spans(&dynamic.props),
        [(at, at + "System.getenv(name)".len() as u64)]
    );
    let reads: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "READS_CONFIG")
        .collect();
    assert_eq!(reads.len(), 2);
    let load = out
        .nodes
        .iter()
        .find(|node| node.label == "Symbol" && node.id.contains("#load"))
        .expect("the reading function is a symbol");
    assert!(reads.iter().all(|edge| edge.src == load.id));
    let all = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    assert!(!all.contains("postgres://") && !all.contains("SHADOWED"));
    // Determinism: a second run yields the same facts.
    let again = extract_source(text.as_bytes(), path, &id()).unwrap();
    assert_eq!(
        all,
        serde_json::to_string(&(&again.nodes, &again.edges)).unwrap()
    );
}

// AC-0597 (ADR-0059 decision 4): a parameter, local, property, import,
// import alias or declared type named `System` voids the file's reads, as
// does a star import of a package the repository does not declare; a
// `System` declared anywhere in the repository, in Kotlin or product Java,
// voids every Kotlin read.
#[test]
fn system_getenv_needs_an_unshadowed_system() {
    let shadowed = [
        r#"fun f(System: Env) = System.getenv("PARAM")"#,
        "fun f(): String { val System = Env(); return System.getenv(\"LOCAL\") }",
        "class A { val System = Env()\n fun f() = System.getenv(\"PROPERTY\") }",
        "import com.fake.System\nfun f() = System.getenv(\"IMPORT\")",
        "import com.fake.Env as System\nfun f() = System.getenv(\"ALIAS\")",
        "import com.fake.*\nfun f() = System.getenv(\"STAR\")",
        "object System { fun getenv(k: String) = k }\nfun f() = System.getenv(\"OBJECT\")",
    ];
    for text in shadowed {
        let out = extract_source(text.as_bytes(), "src/main/kotlin/A.kt", &id()).unwrap();
        assert!(config_read_ids(&out).is_empty(), "{text}");
    }

    let reader =
        "package com.acme\n\nimport com.acme.util.*\n\nval region = System.getenv(\"REGION\")\n";
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/main/kotlin/com/acme/Reader.kt", reader);
    write(
        dir.path(),
        "src/main/kotlin/com/acme/util/Util.kt",
        "package com.acme.util\n\nfun helper() = 1\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    // A star import of a repository package is covered by the repo check.
    assert_eq!(
        config_read_ids(&out),
        ["cfgread:local/demo@src/main/kotlin/com/acme/Reader.kt:env:REGION"]
    );
    // A top-level initializer has no enclosing function: the file reads it.
    edge(
        &out.edges,
        "file:local/demo@src/main/kotlin/com/acme/Reader.kt",
        "cfgread:local/demo@src/main/kotlin/com/acme/Reader.kt:env:REGION",
        "READS_CONFIG",
    );
    write(
        dir.path(),
        "src/main/java/com/acme/util/System.java",
        "package com.acme.util;\npublic class System { public static String getenv(String k) { return k; } }\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(config_read_ids(&out).is_empty());
}

// AC-1023: import-proven Ktor routing builders are Endpoints. `route`
// prefixes compose, a `Route` extension group mounted from another file
// takes its mount's prefix, and the handler lambda is the handler Symbol
// that owns the calls inside it. A computed path or an unmounted group is a
// Gap; a file without the routing import, a same-file `get`, an uncalled
// local function and a test source set assert nothing.
#[test]
fn ktor_routing_dsl_composes_routes_groups_and_handlers() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import io.ktor.server.application.*
import io.ktor.server.routing.routing
import io.ktor.server.routing.route
import io.ktor.server.routing.get
import io.ktor.server.routing.post

fun Application.module() {
    routing {
        get("/health") { call.respondText("ok") }
        route("/api") {
            route("/v1") {
                userRoutes()
                post { audit() }
                fun helper() {
                    get("/hidden") { }
                }
            }
            get(pathFor()) { }
        }
    }
}

fun audit() {}
fun pathFor(): String = "/x"
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/Users.kt",
        r#"package com.demo

import io.ktor.server.routing.*

fun Route.userRoutes() {
    route("/users") {
        get { listUsers() }
        get("/{id}") { }
    }
}

fun Route.orphanRoutes() {
    get("/orphan") { }
}

fun listUsers() {}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/NoImport.kt",
        r#"package com.demo

fun Application.other() {
    routing { get("/no-import") { } }
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/local/Local.kt",
        r#"package com.demo.local

import io.ktor.server.routing.*

fun get(path: String, body: () -> Unit) {}

fun Application.local() {
    routing { get("/local") { } }
}
"#,
    );
    write(
        dir.path(),
        "src/test/kotlin/com/demo/AppTest.kt",
        r#"package com.demo

import io.ktor.server.routing.*

fun Application.testModule() {
    routing { get("/test-only") { } }
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut endpoints: Vec<String> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.clone())
        .collect();
    endpoints.sort();
    assert_eq!(
        endpoints,
        [
            "ep:local/demo@GET:/api/v1/users",
            "ep:local/demo@GET:/api/v1/users/{id}",
            "ep:local/demo@GET:/health",
            "ep:local/demo@POST:/api/v1",
        ]
    );
    let health = node(&out.nodes, "ep:local/demo@GET:/health");
    assert_eq!(health.props["framework"], "ktor");
    let handler = health.props["handler_sym"].as_str().unwrap().to_string();
    assert!(
        handler
            .starts_with("sym:local/demo@src/main/kotlin/com/demo/App.kt#Application.module/get@"),
        "{handler}"
    );
    assert_eq!(node(&out.nodes, &handler).props["kind"], "route handler");
    edge(&out.edges, "ep:local/demo@GET:/health", &handler, "HANDLES");
    let evidence = &health.props["prov"]["evidence"][0];
    let source =
        std::fs::read_to_string(dir.path().join("src/main/kotlin/com/demo/App.kt")).unwrap();
    let span = &source[evidence["byte_start"].as_u64().unwrap() as usize
        ..evidence["byte_end"].as_u64().unwrap() as usize];
    assert!(span.starts_with("get(\"/health\")"), "{span}");

    // Calls inside a handler are the handler's.
    let list = node(&out.nodes, "ep:local/demo@GET:/api/v1/users").props["handler_sym"]
        .as_str()
        .unwrap()
        .to_string();
    edge(
        &out.edges,
        &list,
        "sym:local/demo@src/main/kotlin/com/demo/Users.kt#listUsers",
        "CALLS",
    );
    let post = node(&out.nodes, "ep:local/demo@POST:/api/v1").props["handler_sym"]
        .as_str()
        .unwrap()
        .to_string();
    edge(
        &out.edges,
        &post,
        "sym:local/demo@src/main/kotlin/com/demo/App.kt#audit",
        "CALLS",
    );

    let mut gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "ktor")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            "Ktor route group not mounted in any routing block",
            "dynamic Ktor route path",
        ]
    );
}

// AC-1023: a star-imported routing name that the package also declares
// elsewhere is obscured, so the route is a Gap, not an Endpoint.
#[test]
fn ktor_star_import_obscured_by_same_package_function_is_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import io.ktor.server.routing.*

fun Application.module() {
    routing { get("/x") { } }
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/Get.kt",
        "package com.demo\n\nfun get(path: String, body: () -> Unit) {}\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(!out.nodes.iter().any(|node| node.label == "Endpoint"));
    assert!(out.nodes.iter().any(|node| node.label == "Gap"
        && node.props["reason"] == "Ktor routing name obscured by a same-package declaration"));
}

// AC-1025: import-proven Room entities are SQL tables with their columns,
// a DAO `@Query` literal reads and writes the entity tables it names, and
// `@Insert`/`@Delete`/`@Upsert` write the entity their parameter names,
// across files. A computed query, an undeclared table or type, an
// `entity =` argument and a computed table name are Gaps; lookalike
// annotations without the Room import assert nothing.
#[test]
fn room_entities_and_dao_accesses_join_across_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/data/User.kt",
        r#"package com.demo.data

import androidx.room.ColumnInfo
import androidx.room.Entity
import androidx.room.Ignore
import androidx.room.PrimaryKey

@Entity(tableName = "users")
data class User(
    @PrimaryKey val id: Long,
    @ColumnInfo(name = "e_mail") val email: String,
    val name: String,
) {
    @Ignore var cached: Int = 0
    var nickname: String = ""
        get() = field.trim()
    val display: String
        get() = name.uppercase()
    val shadow: String
        get() = meta.field
}

@Entity(tableName = "")
class Alias(@PrimaryKey val id: Long, val userId: Long)

@Entity(tableName = TABLE)
class Computed(@PrimaryKey val id: Long)

const val TABLE = "computed"
"#,
    );
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/dao/UserDao.kt",
        r#"package com.demo.dao

import androidx.room.*
import com.demo.data.User
import com.demo.data.Alias

@Dao
interface UserDao {
    @Query("SELECT * FROM users WHERE id = :id")
    suspend fun byId(id: Long): User?

    @Query("""
        DELETE FROM alias WHERE userId IN (SELECT id FROM users WHERE name = :name)
    """)
    fun purge(name: String)

    @Query(QUERY)
    fun computed(): List<User>

    @Query("SELECT * FROM audit_log")
    fun audit(): List<String>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    fun insertAll(vararg users: User)

    @Delete
    fun deleteAll(aliases: List<Alias>)

    @Upsert(entity = User::class)
    fun upsertPartial(partial: Any)

    @Update
    fun touch(other: NotAnEntity)
}

const val QUERY = "SELECT * FROM users"
"#,
    );
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/fake/Fake.kt",
        r#"package com.demo.fake

annotation class Entity
annotation class Dao
annotation class Query(val value: String)

@Entity
class Fake(val id: Long)

@Dao
interface FakeDao {
    @Query("SELECT * FROM fake")
    fun all(): List<Fake>
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut entities: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    entities.sort();
    assert_eq!(
        entities,
        ["data:local/demo@sql:alias", "data:local/demo@sql:users"]
    );
    let users = node(&out.nodes, "data:local/demo@sql:users");
    assert_eq!(users.props["framework"], "room");
    assert_eq!(users.props["class"], "com.demo.data.User");
    assert_eq!(
        users.props["columns"],
        serde_json::json!([
            {"name": "id"},
            {"name": "e_mail"},
            {"name": "name"},
            {"name": "nickname"}
        ])
    );

    let dao = "sym:local/demo@app/src/main/kotlin/com/demo/dao/UserDao.kt#UserDao";
    let reads = edge(
        &out.edges,
        &format!("{dao}.byId"),
        "data:local/demo@sql:users",
        "READS",
    );
    assert_eq!(reads.props["resolution"], "sql-literal");
    edge(
        &out.edges,
        &format!("{dao}.purge"),
        "data:local/demo@sql:users",
        "READS",
    );
    let purge = edge(
        &out.edges,
        &format!("{dao}.purge"),
        "data:local/demo@sql:alias",
        "WRITES",
    );
    assert_eq!(purge.props["operation"], "delete");
    let insert = edge(
        &out.edges,
        &format!("{dao}.insertAll"),
        "data:local/demo@sql:users",
        "WRITES",
    );
    assert_eq!(insert.props["operation"], "insert");
    assert_eq!(insert.props["prov"]["confidence_tier"], "Confirmed");
    edge(
        &out.edges,
        &format!("{dao}.deleteAll"),
        "data:local/demo@sql:alias",
        "WRITES",
    );

    let mut gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "room")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            "Room query is not a string literal",
            "Room query table `audit_log` is not a recovered @Entity",
            "Room write names its entity by argument, not read at T0",
            "Room write type `NotAnEntity` is not a recovered @Entity",
            "computed Room entity table name",
        ]
    );
    assert!(
        !out.edges.iter().any(|edge| edge.src.contains("FakeDao")
            && matches!(edge.label.as_str(), "READS" | "WRITES"))
    );
}

// AC-1025: a star-imported `@Entity` that a same-package declaration
// obscures is no entity, so a write naming it is a Gap.
#[test]
fn room_star_import_obscured_by_same_package_type_is_no_entity() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/User.kt",
        r#"package com.demo

import androidx.room.*

@Entity
class User(@PrimaryKey val id: Long)

@Dao
interface UserDao {
    @Insert
    fun add(user: User)
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/Entity.kt",
        "package com.demo\n\nannotation class Entity\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(!out.nodes.iter().any(|node| node.label == "DataEntity"));
    assert!(out.nodes.iter().any(|node| node.label == "Gap"
        && node.props["reason"] == "Room write type `User` is not a recovered @Entity"));
}

// AC-1027: import-proven Exposed table objects are SQL tables with their
// columns, named by the constructor literal or the object name less
// `Table`, and DSL statements on them are reads and writes of the
// enclosing function across files. A computed table name and a statement
// on an unrecovered receiver are Gaps; a lookalike `Table` base and a file
// without an Exposed import assert nothing.
#[test]
fn exposed_tables_and_dsl_statements_join_across_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/db/Tables.kt",
        r#"package com.demo.db

import org.jetbrains.exposed.dao.id.IntIdTable
import org.jetbrains.exposed.sql.Table
import org.jetbrains.exposed.sql.Table as ExposedTable

object Users : Table("users") {
    val id = integer("id").autoIncrement()
    val name = varchar("name", 50).nullable()
    val roles = listOf("admin")
    override val primaryKey = PrimaryKey(id)
}

object CityTable : IntIdTable() {
    val title = text("title")
}

object Tags : ExposedTable("tags")

object Computed : Table(TABLE_NAME)

const val TABLE_NAME = "computed"
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/repo/UserRepo.kt",
        r#"package com.demo.repo

import com.demo.db.CityTable
import com.demo.db.Users
import org.jetbrains.exposed.sql.*

class UserRepo {
    fun all() = Users.selectAll().toList()
    fun add(value: String) {
        Users.insert { it[name] = value }
        CityTable.deleteWhere { title eq value }
    }
    fun rename() = Users.update({ Users.id eq 1 }) { it[name] = "x" }
    fun save(value: String) {
        Users.insert { it[name] = value }
        Users.update { it[name] = value }
    }
    fun orphan() = Orders.selectAll()
    fun local(list: List<String>) = list.select()
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/demo/fake/Fake.kt",
        r#"package com.demo.fake

open class Table(val name: String)

object Lookalike : Table("lookalike")
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut entities: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    entities.sort();
    assert_eq!(
        entities,
        [
            "data:local/demo@sql:city",
            "data:local/demo@sql:tags",
            "data:local/demo@sql:users"
        ]
    );
    let users = node(&out.nodes, "data:local/demo@sql:users");
    assert_eq!(users.props["framework"], "exposed");
    assert_eq!(
        users.props["columns"],
        serde_json::json!([{"name": "id"}, {"name": "name"}])
    );
    assert_eq!(
        node(&out.nodes, "data:local/demo@sql:city").props["columns"],
        serde_json::json!([{"name": "id"}, {"name": "title"}])
    );

    let repo = "sym:local/demo@src/main/kotlin/com/demo/repo/UserRepo.kt#UserRepo";
    edge(
        &out.edges,
        &format!("{repo}.all"),
        "data:local/demo@sql:users",
        "READS",
    );
    let insert = edge(
        &out.edges,
        &format!("{repo}.add"),
        "data:local/demo@sql:users",
        "WRITES",
    );
    assert_eq!(insert.props["operation"], "insert");
    assert_eq!(insert.props["resolution"], "exposed-dsl");
    let delete = edge(
        &out.edges,
        &format!("{repo}.add"),
        "data:local/demo@sql:city",
        "WRITES",
    );
    assert_eq!(delete.props["operation"], "delete");
    assert_eq!(
        edge(
            &out.edges,
            &format!("{repo}.rename"),
            "data:local/demo@sql:users",
            "WRITES",
        )
        .props["operation"],
        "update"
    );
    let save = edge(
        &out.edges,
        &format!("{repo}.save"),
        "data:local/demo@sql:users",
        "WRITES",
    );
    assert_eq!(
        save.props["operations"],
        serde_json::json!(["insert", "update"])
    );
    assert_eq!(
        save.props["statements"],
        serde_json::json!(["insert", "update"])
    );

    let mut gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "exposed")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            "Exposed statement receiver `Orders` is not a recovered table",
            "computed Exposed table name",
        ]
    );
}

// AC-1102: JUnit and kotlin.test functions are TestCases; MockMvc Kotlin
// DSL, `MockMvcRequestBuilders` and Ktor test client requests in their
// bodies link to the Spring and Ktor endpoints they reach, with expected
// statuses. Computed URLs and unserved paths are Gaps; lookalike and
// shadowed annotations are no tests.
#[test]
fn junit_tests_link_mockmvc_and_ktor_requests_to_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/shop/web/OwnerController.kt",
        r#"package com.shop.web

import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PostMapping
import org.springframework.web.bind.annotation.RestController

@RestController
class OwnerController {
    @GetMapping("/owners/{id}")
    fun show(): String = ""

    @GetMapping("/owners/new")
    fun form(): String = ""

    @PostMapping("/owners")
    fun create(): String = ""
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/shop/api/Routes.kt",
        r#"package com.shop.api

import io.ktor.server.routing.*

fun Route.health() {
    get("/health") { }
}

fun Application.module() {
    routing { health() }
}
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/shop/star/Test.kt",
        r#"package com.shop.star

annotation class Test
"#,
    );
    write(
        dir.path(),
        "src/test/kotlin/com/shop/web/OwnerControllerTest.kt",
        r#"package com.shop.web

import org.junit.jupiter.api.Test
import org.springframework.test.web.servlet.MockMvc
import org.springframework.test.web.servlet.get
import org.springframework.test.web.servlet.post
import org.springframework.test.web.servlet.request.MockMvcRequestBuilders
import org.springframework.test.web.servlet.result.MockMvcResultMatchers.status

class OwnerControllerTest(private val mockMvc: MockMvc) {
    @Test
    fun showsOwner() {
        val id = 7
        mockMvc.get("/owners/$id").andExpect { status { isOk() } }
        mockMvc.get("/owners/{id}", 1).andExpect { status { isNotFound() } }
    }

    @Test
    fun formAndCreate() {
        mockMvc.perform(MockMvcRequestBuilders.get("/owners/new")).andExpect(status().isOk())
        mockMvc.post("/owners") { param("name", "x") }.andExpect { status { is3xxRedirection() } }
    }

    @Test
    fun computedAndMissing() {
        mockMvc.get(URL)
        mockMvc.get("/nowhere")
    }

    fun helper() {
        mockMvc.get("/owners/new")
    }

    companion object {
        const val URL = "/owners/new"
    }
}
"#,
    );
    write(
        dir.path(),
        "src/test/kotlin/com/shop/api/HealthTest.kt",
        r#"package com.shop.api

import io.ktor.client.request.get
import io.ktor.server.testing.testApplication
import kotlin.test.Test

class HealthTest {
    @Test
    fun healthy() = testApplication {
        client.get("http://localhost/health?x=1")
    }
}
"#,
    );
    write(
        dir.path(),
        "src/test/kotlin/com/shop/star/StarTest.kt",
        r#"package com.shop.star

import org.junit.jupiter.api.*

class StarTest {
    @Test
    fun shadowed() {}
}
"#,
    );
    write(
        dir.path(),
        "src/test/kotlin/com/shop/fake/FakeTest.kt",
        r#"package com.shop.fake

annotation class Test

class FakeTest {
    @Test
    fun notATest() {}
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut tests: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "TestCase")
        .map(|node| node.props["name"].as_str().unwrap())
        .collect();
    tests.sort();
    assert_eq!(
        tests,
        [
            "HealthTest.healthy",
            "OwnerControllerTest.computedAndMissing",
            "OwnerControllerTest.formAndCreate",
            "OwnerControllerTest.showsOwner",
        ]
    );
    let show = "test:local/demo@src/test/kotlin/com/shop/web/OwnerControllerTest.kt#OwnerControllerTest.showsOwner";
    edge(
        &out.edges,
        show,
        "sym:local/demo@src/test/kotlin/com/shop/web/OwnerControllerTest.kt#OwnerControllerTest.showsOwner",
        "IMPLEMENTED_BY",
    );
    let mut links: Vec<(&str, &str, String)> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "REQUESTS" && edge.dst.starts_with("ep:"))
        .map(|edge| {
            (
                edge.src.rsplit('#').next().unwrap(),
                edge.dst.as_str(),
                edge.props["statuses"].to_string(),
            )
        })
        .collect();
    links.sort();
    assert_eq!(
        links,
        [
            (
                "HealthTest.healthy",
                "ep:local/demo@GET:/health",
                "[]".to_string()
            ),
            (
                "OwnerControllerTest.formAndCreate",
                "ep:local/demo@GET:/owners/new",
                "[200]".to_string()
            ),
            (
                "OwnerControllerTest.formAndCreate",
                "ep:local/demo@POST:/owners",
                "[200]".to_string()
            ),
            (
                "OwnerControllerTest.showsOwner",
                "ep:local/demo@GET:/owners/{id}",
                "[200,404]".to_string()
            ),
        ]
    );
    let shown = edge(
        &out.edges,
        show,
        "ep:local/demo@GET:/owners/{id}",
        "REQUESTS",
    );
    assert_eq!(shown.props["link"], "handler_assumed");
    let mut gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:test-call:"))
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            "no recovered endpoint serves this path and method",
            "test request URL is computed",
        ]
    );
}

/// AC-1102: an aliased JUnit import proves a test; MockK stub and verify
/// blocks perform nothing; a verb call on a receiver that is not a
/// declared test client is no request; a product Java type obscures a star
/// import even when nothing else in the repository waits on Java sources.
#[test]
fn junit_aliases_stubs_receivers_and_java_shadows() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/shop/web/OwnerController.kt",
        r#"package com.shop.web

import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.RestController

@RestController
class OwnerController {
    @GetMapping("/owners")
    fun list(): String = ""
}
"#,
    );
    write(
        dir.path(),
        "src/main/java/com/shop/star/Test.java",
        "package com.shop.star;\n\npublic @interface Test {}\n",
    );
    write(
        dir.path(),
        "src/test/kotlin/com/shop/web/AliasTest.kt",
        r#"package com.shop.web

import io.mockk.every
import io.mockk.verify
import org.junit.jupiter.api.Test as JTest
import org.springframework.test.web.servlet.MockMvc
import org.springframework.test.web.servlet.get

class AliasTest {
    lateinit var mockMvc: MockMvc
    val cache = mutableMapOf<String, String>()

    @JTest
    fun aliased() {
        every { mockMvc.get("/stubbed") } returns null
        verify { mockMvc.get("/verified") }
        cache.get("/cached")
        mockMvc.get("/owners")
    }
}
"#,
    );
    write(
        dir.path(),
        "src/test/kotlin/com/shop/star/StarTest.kt",
        r#"package com.shop.star

import org.junit.jupiter.api.*

class StarTest {
    @Test
    fun shadowed() {}
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let tests: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "TestCase")
        .map(|node| node.props["name"].as_str().unwrap())
        .collect();
    assert_eq!(tests, ["AliasTest.aliased"]);
    let links: Vec<&str> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "REQUESTS")
        .map(|edge| edge.dst.as_str())
        .collect();
    assert_eq!(links, ["ep:local/demo@GET:/owners"]);
    assert!(
        !out.nodes.iter().any(|n| n.id.starts_with("gap:test-call:")),
        "{:?}",
        out.nodes
            .iter()
            .filter(|n| n.id.starts_with("gap:"))
            .collect::<Vec<_>>()
    );
}

// AC-1028: import-proven JPA `@Entity` classes are `jpa` DataEntities with
// their stored properties as fields (`@Id`, `@Column` names, `@Transient`
// skipped), named by `@Entity(name)` or the class and tabled by `@Table`;
// relation properties are REFERENCES to the target entity across files
// (to-many through the collection's element type). An unrecovered target,
// a computed table name and a lookalike `@Entity` assert no relation,
// table or entity.
#[test]
fn jpa_entities_and_relations_join_across_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/shop/model/Customer.kt",
        r#"package com.shop.model

import jakarta.persistence.*

@Entity
@Table(name = "customers", schema = "shop")
class Customer(
    @Id @GeneratedValue val id: Long = 0,
    @Column(name = "full_name") var name: String,
    @OneToMany(mappedBy = "customer") val orders: MutableList<Order> = mutableListOf(),
    @Transient val cached: String = "",
    plain: Int = 0,
) {
    @ManyToMany
    @JoinTable(name = "customer_tags")
    var tags: Set<com.shop.tags.Tag> = emptySet()
    val display: String
        get() = name.uppercase()
}

@Entity(name = "Bad")
@Table(name = TABLE)
class Computed(@Id val id: Long)

const val TABLE = "computed"
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/shop/model/Order.kt",
        r#"package com.shop.model

import javax.persistence.Entity
import javax.persistence.Id
import javax.persistence.JoinColumn
import javax.persistence.ManyToOne
import javax.persistence.OneToOne

@Entity(name = "PurchaseOrder")
data class Order(
    @Id val id: Long,
    @ManyToOne @JoinColumn(name = "customer_id") val customer: Customer?,
    @OneToOne(targetEntity = Invoice::class) val invoice: Any?,
)
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/com/shop/fake/Fake.kt",
        r#"package com.shop.fake

annotation class Entity

@Entity
class NotPersistent(val id: Long)
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut entities: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    entities.sort();
    assert_eq!(
        entities,
        [
            "data:local/demo@jpa:Bad",
            "data:local/demo@jpa:Customer",
            "data:local/demo@jpa:PurchaseOrder"
        ]
    );
    let customer = node(&out.nodes, "data:local/demo@jpa:Customer");
    assert_eq!(customer.props["storage"], "jpa");
    assert_eq!(customer.props["table"], "customers");
    assert_eq!(customer.props["schema"], "shop");
    assert_eq!(customer.props["table_source"], "explicit");
    assert_eq!(customer.props["fqn"], "com.shop.model.Customer");
    assert_eq!(
        customer.props["fields"],
        serde_json::json!([
            {"name": "id", "id": true, "type": "Long"},
            {"name": "name", "id": false, "type": "String", "column": "full_name"},
            {"name": "orders", "id": false, "type": "MutableList<Order>", "relation": "OneToMany"},
            {"name": "tags", "id": false, "type": "Set<com.shop.tags.Tag>", "relation": "ManyToMany"},
        ])
    );
    let order = node(&out.nodes, "data:local/demo@jpa:PurchaseOrder");
    assert_eq!(order.props["table"], "PurchaseOrder");
    assert_eq!(order.props["table_source"], "default");
    let bad = node(&out.nodes, "data:local/demo@jpa:Bad");
    assert!(bad.props.get("table").is_none());
    assert_eq!(bad.props["table_source"], "unresolved");

    let orders = edge(
        &out.edges,
        "data:local/demo@jpa:Customer",
        "data:local/demo@jpa:PurchaseOrder",
        "REFERENCES",
    );
    assert_eq!(orders.props["cardinality"], "one-to-many");
    assert_eq!(orders.props["mapped_by"], "customer");
    assert_eq!(orders.props["owning"], false);
    let customer_ref = edge(
        &out.edges,
        "data:local/demo@jpa:PurchaseOrder",
        "data:local/demo@jpa:Customer",
        "REFERENCES",
    );
    assert_eq!(customer_ref.props["cardinality"], "many-to-one");
    assert_eq!(customer_ref.props["join_column"], "customer_id");
    assert_eq!(customer_ref.props["owning"], true);

    let mut gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] == "jpa")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            "JPA relation target `Invoice` is not a recovered entity",
            "JPA relation target `com.shop.tags.Tag` is not a recovered entity",
            "runtime-computed JPA table name",
        ]
    );
}

// AC-1028: one JPA entity name two classes declare is one duplicate Gap and
// resolves nothing, to or from it; JPA relations resolve among JPA entities
// only (a Room entity class is no target, and a Room query does not read a
// JPA table); two relations to one target are one edge listing both fields;
// a same-package `Transient` keeps the property a field.
#[test]
fn jpa_duplicates_frameworks_and_merged_relations() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/p/Model.kt",
        r#"package p

import jakarta.persistence.Entity
import jakarta.persistence.Id
import jakarta.persistence.ManyToOne
import jakarta.persistence.OneToOne
import jakarta.persistence.Table

annotation class Transient

@Entity
@Table(name = "people")
class Person(
    @Id val id: Long,
    @ManyToOne val referrer: Person?,
    @OneToOne val mentor: Person?,
    @ManyToOne val note: p.room.Note?,
    @Transient val kept: String,
)

@Entity(name = "Twin")
class TwinA(@Id val id: Long, @ManyToOne val person: Person)
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/p/other/Twin.kt",
        r#"package p.other

import jakarta.persistence.Entity
import jakarta.persistence.Id
import jakarta.persistence.OneToOne
import p.Person

@Entity(name = "Twin")
class TwinB(@Id val id: Long)

@Entity
class Holder(@Id val id: Long, @OneToOne val twin: TwinB)
"#,
    );
    write(
        dir.path(),
        "src/main/kotlin/p/room/Note.kt",
        r#"package p.room

import androidx.room.Dao
import androidx.room.Entity
import androidx.room.PrimaryKey
import androidx.room.Query

@Entity
class Note(@PrimaryKey val id: Long)

@Dao
interface NoteDao {
    @Query("SELECT * FROM people")
    fun people(): List<Note>
}
"#,
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let mut entities: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    entities.sort();
    assert_eq!(
        entities,
        [
            "data:local/demo@jpa:Holder",
            "data:local/demo@jpa:Person",
            "data:local/demo@sql:note"
        ]
    );
    let duplicate = node(&out.nodes, "gap:jpa:duplicate:data:local/demo@jpa:Twin");
    assert_eq!(
        duplicate.props["fqns"],
        serde_json::json!(["p.TwinA", "p.other.TwinB"])
    );
    let person = node(&out.nodes, "data:local/demo@jpa:Person");
    let fields: Vec<&str> = person.props["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["id", "referrer", "mentor", "note", "kept"]);
    assert!(person.props["fields"][4].get("unless_obscured").is_none());

    let selfie = edge(
        &out.edges,
        "data:local/demo@jpa:Person",
        "data:local/demo@jpa:Person",
        "REFERENCES",
    );
    assert_eq!(
        selfie.props["relations"],
        serde_json::json!([
            {"field": "mentor", "relation": "OneToOne", "cardinality": "one-to-one", "owning": true},
            {"field": "referrer", "relation": "ManyToOne", "cardinality": "many-to-one", "owning": true},
        ])
    );
    assert_eq!(
        selfie.props["prov"]["evidence"].as_array().unwrap().len(),
        2
    );
    // A relation to a colliding entity, and one from it, fail closed.
    for src in ["data:local/demo@jpa:Holder", "data:local/demo@jpa:Twin"] {
        assert!(out.edges.iter().any(|edge| edge.src == src
            && edge.label == "REFERENCES"
            && edge.dst.starts_with("gap:jpa:local/demo@")));
    }
    assert!(
        !out.edges
            .iter()
            .any(|edge| edge.dst == "data:local/demo@sql:note" && edge.src.contains("jpa"))
    );
    assert!(
        !out.edges
            .iter()
            .any(|edge| edge.dst == "data:local/demo@jpa:Person" && edge.label == "READS")
    );

    let mut gaps: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap" && node.props["framework"] != "kotlin")
        .map(|node| node.props["reason"].as_str().unwrap())
        .collect();
    gaps.sort();
    assert_eq!(
        gaps,
        [
            "JPA entity name is declared by more than one class",
            "JPA relation target `Person` is not a recovered entity",
            "JPA relation target `TwinB` is not a recovered entity",
            "JPA relation target `p.room.Note` is not a recovered entity",
            "Room query table `people` is not a recovered @Entity",
        ]
    );
}
