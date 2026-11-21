use super::*;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/go-service",
        commit: "abc123",
    }
}

fn edge_pairs<'a>(extraction: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    extraction
        .edges
        .iter()
        .filter(|edge| edge.label == label)
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect()
}

#[test]
fn http_chi_and_gin_routes_are_import_proven() {
    // AC-0054/T-0054: registration packages and router factories must be
    // import-proven; matching methods on arbitrary receivers are ignored.
    let source = br#"
package main

import (
    nethttp "net/http"
    "github.com/go-chi/chi/v5"
    web "github.com/gin-gonic/gin"
    fake "example.com/fake"
)

func health(w nethttp.ResponseWriter, r *nethttp.Request) {}
func orders(w nethttp.ResponseWriter, r *nethttp.Request) {}
func create(c *web.Context) {}
func ignored() {}

func routes() {
    nethttp.HandleFunc("GET /health", health)
    router := chi.NewRouter()
    router.Get("/orders", orders)
    api := web.Default()
    api.POST("/orders", create)
    lookalike := fake.NewRouter()
    lookalike.Get("/ignored", ignored)
    dynamic := "/dynamic"
    router.Get(dynamic, ignored)
    api.GET("/wrapped", middleware.Wrap(create))
}
"#;
    let extraction = extract_source(source, "main.go", &id()).unwrap();
    let mut endpoints = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            (
                node.props["method"].as_str().unwrap().to_string(),
                node.props["path"].as_str().unwrap().to_string(),
                node.props["framework"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort();
    assert_eq!(
        endpoints,
        vec![
            ("GET".into(), "/health".into(), "net/http".into()),
            ("GET".into(), "/orders".into(), "chi".into()),
            ("GET".into(), "/wrapped".into(), "gin".into()),
            ("POST".into(), "/orders".into(), "gin".into()),
        ]
    );
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/go-service@GET:/health",
        "sym:local/go-service@main.go#health"
    )));
    let wrapped = extraction
        .edges
        .iter()
        .find(|edge| edge.src == "ep:local/go-service@GET:/wrapped" && edge.label == "HANDLES")
        .expect("a wrapped handler is recorded");
    // #2007: a wrapper call's handler argument is the handler, with the
    // wrapper kept on the edge.
    assert_eq!(wrapped.dst, "sym:local/go-service@main.go#create");
    assert_eq!(wrapped.props["via"], "middleware.Wrap(create)");
}

#[test]
fn calls_resolve_locally_and_across_packages() {
    // AC-0054/T-0054: local calls resolve in the file pass and imported
    // module packages resolve only when their target declaration is present.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cmd/api")).unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/helper")).unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/service\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper/helper.go"),
        "package helper\n\nfunc Imported() int { return 1 }\nfunc Handler() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("cmd/api/main.go"),
        r#"package main

import (
    "net/http"
    "example.com/service/pkg/helper"
)

func local() int { return 2 }
func handler() {
    local()
    helper.Imported()
    http.HandleFunc("POST /imported", helper.Handler)
}
"#,
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let handler = "sym:local/go-service@cmd/api/main.go#handler";
    assert!(
        edge_pairs(&extraction, "CALLS")
            .contains(&(handler, "sym:local/go-service@cmd/api/main.go#local"))
    );
    assert!(edge_pairs(&extraction, "CALLS").contains(&(
        handler,
        "sym:local/go-service@pkg/helper/helper.go#Imported"
    )));
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/go-service@POST:/imported",
        "sym:local/go-service@pkg/helper/helper.go#Handler"
    )));
}

#[test]
fn ignored_go_mod_yields_no_module_path() {
    // AC-0225/T-0225 (#468, ADR-0034): a `go.mod` the root's own `.gitignore`
    // excludes is treated as absent — same as no module declaration at all —
    // rather than read anyway, matching the shared walk's ignore rules for
    // this one direct read outside it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/helper")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "go.mod\n").unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/service\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper/helper.go"),
        "package helper\n\nfunc Imported() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        r#"package main

import "example.com/service/pkg/helper"

func handler() { helper.Imported() }
"#,
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    // With no proven module path, the import cannot resolve to an internal
    // package, so the cross-file CALLS edge into helper.Imported never forms.
    assert!(
        !edge_pairs(&extraction, "CALLS")
            .iter()
            .any(|(_, dst)| dst.contains("pkg/helper/helper.go#Imported"))
    );
}

#[test]
fn unignored_go_mod_still_proves_module_internal_imports() {
    // Control for the above: an untouched `.gitignore` that doesn't name
    // `go.mod` leaves module resolution exactly as it was before #468.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/helper")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/service\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper/helper.go"),
        "package helper\n\nfunc Imported() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        r#"package main

import "example.com/service/pkg/helper"

func handler() { helper.Imported() }
"#,
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        edge_pairs(&extraction, "CALLS")
            .iter()
            .any(|(_, dst)| dst.contains("pkg/helper/helper.go#Imported"))
    );
}

#[test]
fn every_fact_has_confirmed_provenance_and_spans() {
    // AC-0054/T-0054: exact Go source spans are first-class T0 evidence.
    let source = b"package main\n\nimport \"net/http\"\n\nfunc h(w http.ResponseWriter, r *http.Request) {}\nfunc routes() { http.HandleFunc(\"/x\", h) }\n";
    let extraction = extract_source(source, "main.go", &id()).unwrap();
    for props in extraction
        .nodes
        .iter()
        .filter(|node| node.props.get("placeholder").is_none())
        .map(|node| &node.props)
        .chain(extraction.edges.iter().map(|edge| &edge.props))
    {
        assert_eq!(props["prov"]["tier"], "Deterministic");
        assert_eq!(props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(props["prov"]["extractor_id"], EXTRACTOR_ID);
        assert_eq!(props["prov"]["evidence"][0]["path"], "main.go");
        let start = props["prov"]["evidence"][0]["byte_start"].as_u64().unwrap();
        let end = props["prov"]["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end > start);
        assert!(end <= source.len() as u64);
    }
}

#[test]
fn incremental_reingest_is_deterministic_and_skips_noise() {
    // AC-0054/T-0054: unchanged Go contexts are reused, byte changes are
    // isolated, and vendored/test/build sources never enter application facts.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.go"),
        "package app\nfunc A() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.go"),
        "package app\nfunc B() int { return 2 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("a_test.go"),
        "package app\nfunc TestNoise() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("vendor/example.com/noise")).unwrap();
    std::fs::write(
        dir.path().join("vendor/example.com/noise/noise.go"),
        "package noise\nfunc Noise() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("ignored.go"),
        "//go:build ignore\n\npackage app\nfunc Ignored() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("platform_windows.go"),
        "package app\nfunc PlatformOnly() {}\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (first, first_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first_stats.recomputed_files, 2);
    assert!(!first.nodes.iter().any(|node| node.id.contains("Noise")));
    assert!(!first.nodes.iter().any(|node| node.id.contains("Ignored")));
    assert!(
        !first
            .nodes
            .iter()
            .any(|node| node.id.contains("PlatformOnly"))
    );
    let (same, same_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(same_stats.recomputed_files, 0);
    assert_eq!(same_stats.reused_files, 2);
    assert_eq!(first.nodes, same.nodes);
    assert_eq!(first.edges, same.edges);

    std::fs::write(dir.path().join("go.mod"), "module example.com/changed\n").unwrap();
    let (_, module_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(module_stats.recomputed_files, 2);
    assert_eq!(module_stats.reused_files, 0);

    std::fs::write(
        dir.path().join("b.go"),
        "package app\nfunc Changed() int { return 3 }\n",
    )
    .unwrap();
    let (changed, changed_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(changed_stats.recomputed_files, 1);
    assert_eq!(changed_stats.reused_files, 1);
    assert!(
        changed
            .nodes
            .iter()
            .any(|node| node.id.ends_with("#Changed"))
    );
    assert!(!changed.nodes.iter().any(|node| node.id.ends_with("#B")));

    std::fs::remove_file(dir.path().join("b.go")).unwrap();
    let (_, deleted_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(deleted_stats.deleted_files, 1);
}

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("cmd/main.go", ""),
        ("gen/api.go", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_go_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["cmd/main.go"]);
}

fn boundary_of(extraction: &Extraction, id: &str) -> (String, core_prov::ConfidenceTier) {
    let node = extraction
        .nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("missing node {id}"));
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone())
        .unwrap_or_else(|_| panic!("{id} carries no provenance"));
    assert!(!prov.evidence.is_empty(), "{id} cites its import");
    (
        node.props["boundary"].as_str().unwrap().to_string(),
        prov.confidence_tier,
    )
}

#[test]
fn import_targets_carry_provenance_and_only_proven_externals_confirm() {
    // AC-0207 (#237): beneath the repository's own module paths an existing
    // package directory is internal and anything else an explicit Gap;
    // outside them, stdlib and foreign modules are Confirmed external.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/svc\n\nreplace example.com/shared => ./shared\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/store")).unwrap();
    std::fs::write(
        dir.path().join("pkg/store/store_linux.go"),
        "package store\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"github.com/lib/pq\"\n\t\"example.com/svc/pkg/store\"\n\t\"example.com/svc/pkg/missing\"\n\t\"example.com/shared/x\"\n)\n\nfunc main() { fmt.Println(pq.X, store.Y) }\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(expect("mod:fmt"), ("external".into(), Confirmed));
    assert_eq!(
        expect("mod:github.com/lib/pq"),
        ("external".into(), Confirmed)
    );
    // A package directory whose only file is platform-gated still exists.
    assert_eq!(
        expect("mod:example.com/svc/pkg/store"),
        ("internal".into(), Confirmed)
    );
    assert_eq!(
        expect("mod:example.com/svc/pkg/missing"),
        ("unresolved".into(), Gap)
    );
    // A locally replaced module is the repository's own, never external.
    assert_eq!(
        expect("mod:example.com/shared/x"),
        ("unresolved".into(), Gap)
    );
}

#[test]
fn a_local_replace_target_inside_the_repository_is_internal() {
    // AC-0207 (#477): a local `replace` keeps its target directory, resolved
    // against the declaring go.mod, so an existing package beneath it is a
    // Confirmed internal boundary; a missing one, or a target that leaves
    // the repository root (lexically or via a symlink), stays a Gap.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("repo");
    let write = |path: &std::path::Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        &root.join("svc/go.mod"),
        "module example.com/svc\n\nreplace (\n\texample.com/shared => ../shared\n\texample.com/outside => ../../outside\n\texample.com/linked => ./linked\n)\n",
    );
    write(&root.join("shared/go.mod"), "module example.com/shared\n");
    write(&root.join("shared/pkg/x.go"), "package pkg\n");
    // Real packages outside the repository root: never probed.
    write(&outer.path().join("outside/pkg/x.go"), "package pkg\n");
    write(&outer.path().join("linked/pkg/x.go"), "package pkg\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outer.path().join("linked"), root.join("svc/linked")).unwrap();
    write(
        &root.join("svc/main.go"),
        "package main\n\nimport (\n\t\"example.com/shared/pkg\"\n\t\"example.com/shared/missing\"\n\t\"example.com/outside/pkg\"\n\t\"example.com/linked/pkg\"\n)\n",
    );
    let out = extract_dir(&root, &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(
        expect("mod:example.com/shared/pkg"),
        ("internal".into(), Confirmed)
    );
    assert_eq!(
        expect("mod:example.com/shared/missing"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/outside/pkg"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/linked/pkg"),
        ("unresolved".into(), Gap)
    );
}

#[test]
fn a_root_replace_beside_the_target_go_mod_is_internal() {
    // AC-0207 (#477): the issue's shape — a root `replace … => ./shared`
    // listed before `shared/go.mod` — resolves to the package directory.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/svc\n\nreplace example.com/shared => ./shared\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("shared/pkg")).unwrap();
    std::fs::write(
        dir.path().join("shared/go.mod"),
        "module example.com/shared\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("shared/pkg/x.go"), "package pkg\n").unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport (\n\t\"example.com/shared/pkg\"\n\t\"example.com/shared/missing\"\n)\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary_of(&out, "mod:example.com/shared/pkg"),
        ("internal".into(), Confirmed)
    );
    assert_eq!(
        boundary_of(&out, "mod:example.com/shared/missing"),
        ("unresolved".into(), Gap)
    );
}

#[cfg(unix)]
#[test]
fn a_package_directory_symlinked_out_of_the_repository_is_a_gap() {
    // AC-0207 (#477 review): the complete package directory — and the Go
    // file proving it — must resolve inside the repository root, beneath a
    // `replace` target and beneath a discovered `go.mod` alike.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    use std::os::unix::fs::symlink;
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("repo");
    let write = |path: &std::path::Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        &root.join("go.mod"),
        "module example.com/svc\n\nreplace example.com/shared => ./shared\n",
    );
    write(&root.join("shared/go.mod"), "module example.com/shared\n");
    write(&root.join("shared/ok/x.go"), "package ok\n");
    write(&outer.path().join("elsewhere/x.go"), "package elsewhere\n");
    symlink(outer.path().join("elsewhere"), root.join("shared/pkg")).unwrap();
    symlink(outer.path().join("elsewhere"), root.join("local")).unwrap();
    std::fs::create_dir_all(root.join("filelink")).unwrap();
    symlink(
        outer.path().join("elsewhere/x.go"),
        root.join("filelink/x.go"),
    )
    .unwrap();
    write(
        &root.join("main.go"),
        "package main\n\nimport (\n\t\"example.com/shared/ok\"\n\t\"example.com/shared/pkg\"\n\t\"example.com/svc/local\"\n\t\"example.com/svc/filelink\"\n)\n",
    );
    let out = extract_dir(&root, &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(
        expect("mod:example.com/shared/ok"),
        ("internal".into(), Confirmed)
    );
    for escaped in [
        "mod:example.com/shared/pkg",
        "mod:example.com/svc/local",
        "mod:example.com/svc/filelink",
    ] {
        assert_eq!(expect(escaped), ("unresolved".into(), Gap), "{escaped}");
    }
}

#[test]
fn a_version_qualified_replace_never_confirms_internal() {
    // AC-0207 (#477 review): `replace m v1.0.0 => …` applies only to that
    // version and no selected version is known, so a module with a
    // version-qualified replace is never proven internal — not through the
    // replacement target, nor through an in-repository go.mod declaring it.
    // Foreign modules pinned the same way stay Confirmed external.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, body: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        "go.mod",
        "module example.com/svc\n\nreplace (\n\texample.com/shared v1.0.0 => ./shared-v1\n\texample.com/other v1.0.0 => example.com/fork v1.0.0\n\tgolang.org/x/net v0.1.0 => golang.org/x/net v0.2.0\n)\n",
    );
    write("shared-v1/go.mod", "module example.com/shared\n");
    write("shared-v1/pkg/x.go", "package pkg\n");
    write("other/go.mod", "module example.com/other\n");
    write("other/pkg/x.go", "package pkg\n");
    // A nested module beneath a pinned path is no proof either.
    write("shared-v1/sub/go.mod", "module example.com/shared/sub\n");
    write("shared-v1/sub/x.go", "package sub\n");
    write(
        "main.go",
        "package main\n\nimport (\n\t\"example.com/shared/pkg\"\n\t\"example.com/other/pkg\"\n\t\"example.com/shared/sub\"\n\t\"golang.org/x/net/http2\"\n)\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(
        expect("mod:example.com/shared/pkg"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/other/pkg"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/shared/sub"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:golang.org/x/net/http2"),
        ("external".into(), Confirmed)
    );
}

#[test]
fn without_go_mod_no_import_is_proven_external() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println() }\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary_of(&out, "mod:fmt"),
        ("unresolved".into(), core_prov::ConfidenceTier::Gap)
    );
}

#[test]
fn go_mod_comments_and_dot_segments_never_confirm_a_boundary() {
    // AC-0207 (#237 review): a trailing comment on the `module` directive is
    // not part of the path, so in-module imports never read as external; and
    // an import path spelling `..` probes nothing outside the repository.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/svc // service root\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/store")).unwrap();
    std::fs::write(dir.path().join("pkg/store/store.go"), "package store\n").unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport (\n\t\"example.com/svc/pkg/store\"\n\t\"example.com/svc/pkg/gone\"\n\t\"example.com/svc/../svc/pkg/store\"\n)\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        out.nodes
            .iter()
            .all(|node| node.id != "mod:example.com/svc/pkg/store"
                || boundary_of(&out, &node.id) == ("internal".into(), Confirmed))
    );
    assert_eq!(
        boundary_of(&out, "mod:example.com/svc/pkg/gone"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        boundary_of(&out, "mod:example.com/svc/../svc/pkg/store"),
        ("unresolved".into(), Gap)
    );
}

fn fetches(extraction: &Extraction) -> Vec<(String, String, Option<String>)> {
    use adapters_fw::events::IdentityExpr;
    extraction
        .fetch_sites
        .iter()
        .map(|site| {
            let url = match &site.url {
                IdentityExpr::Literal(url) => format!("literal:{url}"),
                IdentityExpr::EnvRef(key) => format!("env:{key}"),
                IdentityExpr::Computed(text) => format!("computed:{text}"),
            };
            (site.method.clone(), url, site.symbol.clone())
        })
        .collect()
}

#[test]
fn net_http_client_calls_become_fetch_sites() {
    // AC-0434: package verbs, `DefaultClient` verbs and `NewRequest` sites on
    // the file's `net/http` import, with their method and exact URL text.
    let source = br#"
package client

import (
    "context"
    web "net/http"
)

type Api struct{}

func (a *Api) Sync(ctx context.Context, base string) {
    web.Get("https://api.example.com/users")
    web.Head(`/health`)
    web.Post("/users", "application/json", nil)
    web.PostForm("/login", nil)
    web.DefaultClient.Get("/me")
    web.NewRequest("", "/a", nil)
    web.NewRequest("PUT", "/b", nil)
    web.NewRequestWithContext(ctx, web.MethodDelete, "/c", nil)
    web.NewRequest(method(), "/d", nil)
    web.NewRequest("GET", base+"/e", nil)
    web.NewRequest("GET", "/f\x2f", nil)
    web.DefaultClient.Do(nil)
    web.HandleFunc("/g", nil)
}

func method() string { return "GET" }
"#;
    let x = extract_source(source, "client.go", &id()).unwrap();
    let sync = Some("sym:local/go-service@client.go#Api.Sync".to_string());
    let want = [
        ("GET", "literal:https://api.example.com/users"),
        ("HEAD", "literal:/health"),
        ("POST", "literal:/users"),
        ("POST", "literal:/login"),
        ("GET", "literal:/me"),
        ("GET", "literal:/a"),
        ("PUT", "literal:/b"),
        ("DELETE", "literal:/c"),
        ("?", "literal:/d"),
        ("GET", "computed:base+\"/e\""),
        ("GET", "computed:\"/f\\x2f\""),
    ]
    .map(|(method, url)| (method.to_string(), url.to_string(), sync.clone()));
    assert_eq!(fetches(&x), want);
}

#[test]
fn shadowed_or_unproven_http_names_yield_no_fetch_sites() {
    // AC-0434: a local value or type named like the import, a lookalike
    // package, and a file without `net/http` are not client calls.
    let source = br#"
package client

import (
    "net/http"
    fake "example.com/http"
)

type client struct{}

func (client) Get(string) {}

func param(http client) { http.Get("/a") }

func local() {
    http := client{}
    http.Get("/b")
}

func closure() {
    run := func() {
        var http client
        http.Get("/c")
    }
    run()
}

func lookalike() { fake.Get("/d") }

func proven() { http.Get("/e") }
"#;
    let x = extract_source(source, "client.go", &id()).unwrap();
    assert_eq!(
        fetches(&x),
        [(
            "GET".to_string(),
            "literal:/e".to_string(),
            Some("sym:local/go-service@client.go#proven".to_string()),
        )]
    );
    let none = extract_source(
        b"package c\nimport http \"example.com/http\"\nfunc f() { http.Get(\"/x\") }\n",
        "c.go",
        &id(),
    )
    .unwrap();
    assert!(none.fetch_sites.is_empty());
}

#[test]
fn directory_extraction_keeps_fetch_sites_across_incremental_reuse() {
    // AC-0434: fetch sites survive the per-file incremental cache.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\nimport \"net/http\"\nfunc main() { http.Get(\"/users\") }\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (first, _) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    let (second, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.reused_files, 1);
    assert_eq!(fetches(&first), fetches(&second));
    assert_eq!(fetches(&second).len(), 1);
}

fn event_rows(source: &[u8]) -> Vec<String> {
    use adapters_fw::events::IdentityExpr;
    extract_source(source, "queue.go", &id())
        .unwrap()
        .event_sites
        .iter()
        .map(|site| {
            assert!(site.registries.is_empty());
            let identity = match &site.identity {
                IdentityExpr::Literal(value) => format!("lit:{value}"),
                IdentityExpr::EnvRef(key) => format!("env:{key}"),
                IdentityExpr::Computed(text) => format!("computed:{text}"),
            };
            let owner = site.symbol.as_deref().unwrap_or("-");
            let owner = owner.rsplit_once('#').map_or(owner, |(_, name)| name);
            format!("{} {:?} {identity} {owner}", site.kind, site.role)
        })
        .collect()
}

// AC-0462: keyed AWS SDK v1/v2 `SendMessageInput`, `ReceiveMessageInput`
// and `PublishInput` literals and kafka-go `Writer`, `WriterConfig` and
// `ReaderConfig` literals on the file's import of the exact path are event
// sites: `aws.String("…")` and string literals are literal identities,
// anything else or a missing field is computed. (T-0462)
#[test]
fn sdk_request_and_config_structs_become_event_sites() {
    let rows = event_rows(
        br#"
package queue

import (
    "os"

    "github.com/aws/aws-sdk-go-v2/aws"
    "github.com/aws/aws-sdk-go-v2/service/sqs"
    snsv1 "github.com/aws/aws-sdk-go/service/sns"
    kafka "github.com/segmentio/kafka-go"
)

func Send(ctx context.Context, client *sqs.Client) {
    client.SendMessage(ctx, &sqs.SendMessageInput{QueueUrl: aws.String("https://sqs.eu-west-1.amazonaws.com/1/orders"), MessageBody: aws.String("b")})
    client.ReceiveMessage(ctx, &sqs.ReceiveMessageInput{QueueUrl: aws.String(os.Getenv("Q"))})
}

func Notify() {
    _ = snsv1.PublishInput{TopicArn: aws.String(`arn:aws:sns:eu-west-1:1:sent`)}
}

func Stream() {
    w := &kafka.Writer{Addr: kafka.TCP("k:9092"), Topic: "orders"}
    perMessage := kafka.Writer{Addr: kafka.TCP("k:9092")}
    r := kafka.NewReader(kafka.ReaderConfig{Brokers: []string{"k:9092"}, Topic: "orders", GroupID: "g"})
    old := kafka.NewWriter(kafka.WriterConfig{Topic: topicName()})
    _, _, _, _ = w, perMessage, r, old
}
"#,
    );
    assert_eq!(
        rows,
        [
            "sqs-queue Produces lit:https://sqs.eu-west-1.amazonaws.com/1/orders Send",
            "sqs-queue Consumes computed:aws.String(os.Getenv(\"Q\")) Send",
            "sns-topic Produces lit:arn:aws:sns:eu-west-1:1:sent Notify",
            "kafka-topic Produces lit:orders Stream",
            "kafka-topic Produces computed:kafka.Writer{Addr: kafka.TCP(\"k:9092\")} Stream",
            "kafka-topic Consumes lit:orders Stream",
            "kafka-topic Produces computed:topicName() Stream",
        ]
    );
}

// AC-0462 (#1361 review): kafka-go declares package `kafka`, so an
// unaliased import binds `kafka`, not the last path segment. (T-0462)
#[test]
fn unaliased_kafka_go_import_binds_its_package_name() {
    let rows = event_rows(
        br#"
package stream

import "github.com/segmentio/kafka-go"

func Stream() {
    _ = kafka.Writer{Topic: "orders"}
}
"#,
    );
    assert_eq!(rows, ["kafka-topic Produces lit:orders Stream"]);
}

// AC-0462 (#1361 review): a comment inside `aws.String(…)` is not an
// argument, so the literal stays a literal identity. (T-0462)
#[test]
fn comments_in_aws_string_keep_the_literal_identity() {
    let rows = event_rows(
        br#"
package queue

import (
    "github.com/aws/aws-sdk-go-v2/aws"
    "github.com/aws/aws-sdk-go-v2/service/sqs"
)

func Send(ctx context.Context, client *sqs.Client) {
    client.SendMessage(ctx, &sqs.SendMessageInput{QueueUrl: aws.String(/* queue URL */ "https://sqs.eu-west-1.amazonaws.com/1/orders")})
}
"#,
    );
    assert_eq!(
        rows,
        ["sqs-queue Produces lit:https://sqs.eu-west-1.amazonaws.com/1/orders Send"]
    );
}

// AC-0462: another package of the same name, a shadowed qualifier, an
// unkeyed literal, a bare string for a `*string` field, a lookalike `aws`
// helper and other types yield no literal site. (T-0462)
#[test]
fn unproven_sdk_structs_are_not_event_sites() {
    let rows = event_rows(
        br#"
package queue

import (
    aws "example.com/fake/aws"
    "example.com/other/sqs"
    kafka "github.com/segmentio/kafka-go"
    realsqs "github.com/aws/aws-sdk-go-v2/service/sqs"
)

func A() {
    _ = sqs.SendMessageInput{QueueUrl: "q"}
    _ = kafka.Message{Topic: "t"}
    _ = kafka.Writer{nil, "t"}
}

func B(kafka Factory) {
    _ = kafka.Writer{Topic: "t"}
}

func C() {
    _ = realsqs.SendMessageInput{QueueUrl: aws.String("q")}
}
"#,
    );
    assert_eq!(rows, ["sqs-queue Produces computed:aws.String(\"q\") C"]);
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

#[test]
fn os_getenv_reads_are_config_reads() {
    // AC-0597 (#1789, ADR-0059): one literal `os.Getenv` read, one dynamic
    // read and one shadowed look-alike (a local function named `getenv`)
    // yield exactly one named ConfigRead, one dynamic ConfigRead and
    // nothing for the look-alike. Only the key name and the span of the
    // call are recorded.
    let text = r#"
package main

import "os"

func getenv(key string) string { return key }

func load(name string) (string, string) {
    url := os.Getenv("DATABASE_URL")
    return url, os.Getenv(name)
}

func fake() string {
    return getenv("SHADOWED")
}
"#;
    let extraction = extract_source(text.as_bytes(), "config.go", &id()).unwrap();
    let reads: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "ConfigRead")
        .collect();
    let ids: Vec<&str> = reads.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "cfgread:local/go-service@config.go:env:DATABASE_URL",
            "cfgread:local/go-service@config.go:env:?",
        ]
    );
    let named = reads[0];
    assert_eq!(named.props["source"], "env");
    assert_eq!(named.props["key"], "DATABASE_URL");
    assert_eq!(named.props["language"], "go");
    assert_eq!(named.props["prov"]["tier"], "Deterministic");
    assert_eq!(named.props["prov"]["confidence_tier"], "Confirmed");
    let read = r#"os.Getenv("DATABASE_URL")"#;
    let at = text.find(read).unwrap() as u64;
    assert_eq!(evidence_spans(&named.props), [(at, at + read.len() as u64)]);
    let dynamic = reads[1];
    assert_eq!(dynamic.props["dynamic"], true);
    assert!(dynamic.props.get("key").is_none());
    let at = text.find("os.Getenv(name)").unwrap() as u64;
    assert_eq!(
        evidence_spans(&dynamic.props),
        [(at, at + "os.Getenv(name)".len() as u64)]
    );
    assert_eq!(
        edge_pairs(&extraction, "READS_CONFIG"),
        [
            (
                "sym:local/go-service@config.go#load",
                "cfgread:local/go-service@config.go:env:DATABASE_URL"
            ),
            (
                "sym:local/go-service@config.go#load",
                "cfgread:local/go-service@config.go:env:?"
            ),
        ]
    );
    assert!(
        !serde_json::to_string(&extraction.nodes)
            .unwrap()
            .contains("SHADOWED")
    );
    // Determinism: a second run yields the same facts.
    let again = extract_source(text.as_bytes(), "config.go", &id()).unwrap();
    assert_eq!(
        serde_json::to_string(&(&extraction.nodes, &extraction.edges)).unwrap(),
        serde_json::to_string(&(&again.nodes, &again.edges)).unwrap()
    );
}

#[test]
fn os_getenv_needs_the_standard_os_import() {
    // AC-0597 (ADR-0059 decisions 1, 4): `LookupEnv` and an aliased import
    // are the same read and share the key's node; a local named like the
    // import, another package aliased `os` and a dot-import yield nothing;
    // a constant key is a dynamic read, never a followed name.
    let reads = r#"
package main

import env "os"

var region = env.Getenv("REGION")

func lookup() bool {
    _, ok := env.LookupEnv("REGION")
    return ok
}

func shadowed(env fakeEnv) string {
    return env.Getenv("PARAM")
}
"#;
    let extraction = extract_source(reads.as_bytes(), "reads.go", &id()).unwrap();
    let nodes: Vec<&Node> = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "ConfigRead")
        .collect();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, "cfgread:local/go-service@reads.go:env:REGION");
    assert_eq!(evidence_spans(&nodes[0].props).len(), 2);
    assert_eq!(
        edge_pairs(&extraction, "READS_CONFIG"),
        [
            (
                "file:local/go-service@reads.go",
                "cfgread:local/go-service@reads.go:env:REGION"
            ),
            (
                "sym:local/go-service@reads.go#lookup",
                "cfgread:local/go-service@reads.go:env:REGION"
            ),
        ]
    );

    let foreign = r#"
package main

import (
    os "example.com/fake/os"
    . "os"
)

const key = "CONST"

func a() string { return os.Getenv("FOREIGN") }
func b() string { return Getenv("DOT") }
"#;
    let extraction = extract_source(foreign.as_bytes(), "foreign.go", &id()).unwrap();
    assert!(!extraction.nodes.iter().any(|n| n.label == "ConfigRead"));

    let constant = r#"
package main

import "os"

const key = "CONST"

func a() string { return os.Getenv(key) }
"#;
    let extraction = extract_source(constant.as_bytes(), "constant.go", &id()).unwrap();
    let ids: Vec<&str> = extraction
        .nodes
        .iter()
        .filter(|n| n.label == "ConfigRead")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(ids, ["cfgread:local/go-service@constant.go:env:?"]);
}

#[test]
fn routes_on_a_mux_parameter_and_their_handler_forms_resolve() {
    // AC-0918 (#2006, #2007): a parameter typed `*http.ServeMux` is a
    // router; a func literal handler is its own callable, a local closure
    // resolves to its literal, a method value on the receiver resolves to
    // the method, and a wrapper resolves to the handler it takes or, when
    // it takes none, to itself. A method value on a field chain stays a Gap.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/device")).unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/bot\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/device/webapi.go"),
        r#"package device

import (
    "context"
    "net/http"
)

type Service struct{ Dev *Dev }

func (s *Service) webAPI(mux *http.ServeMux) {
    mux.HandleFunc("GET /api/web/session", func(w http.ResponseWriter, r *http.Request) {
        s.session(w)
    })
    fleet := func(w http.ResponseWriter, r *http.Request) { s.save(w) }
    mux.HandleFunc("GET /devices", fleet)
    {
        mux := newFake()
        mux.HandleFunc("/shadowed", fleet)
    }
    mux.HandleFunc("GET /api/web/catalog", s.catalog)
    mux.HandleFunc("POST /api/daemon/pull", wrap("POST", s.pull))
    mux.HandleFunc("POST /api/web/relay-rules", relayWrite(true))
    mux.HandleFunc("/.well-known/key", s.Dev.ServerKey)
}

func (s *Service) more(mux *http.ServeMux, fleet http.HandlerFunc) {
    mux.HandleFunc("GET /other", fleet)
    opts := 3
    mux.HandleFunc("GET /opts", options(s.catalog, opts))
}

func (s *Service) session(w http.ResponseWriter) {}
func (s *Service) save(w http.ResponseWriter) {}
func other() {
    mux := newFake()
    mux.HandleFunc("/elsewhere", func(w http.ResponseWriter, r *http.Request) {})
}
func newFake() *Fake { return nil }
func wrap(method string, h func(context.Context) int) http.HandlerFunc { return nil }
func relayWrite(on bool) http.HandlerFunc { return nil }
func options(h http.HandlerFunc, n int) http.HandlerFunc { return nil }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("pkg/device/catalog.go"),
        "package device\n\nimport \"net/http\"\n\nfunc (s *Service) catalog(w http.ResponseWriter, r *http.Request) {}\nfunc (s *Service) pull() int { return 0 }\n",
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let handles: BTreeMap<&str, &str> = edge_pairs(&extraction, "HANDLES").into_iter().collect();
    let ep = |m: &str, p: &str| format!("ep:local/go-service@{m}:{p}");
    let sym = |path: &str, name: &str| format!("sym:local/go-service@pkg/device/{path}#{name}");
    let literal = handles[ep("GET", "/api/web/session").as_str()];
    assert!(
        literal.starts_with(&sym("webapi.go", "Service.webAPI/func@")),
        "{literal}"
    );
    assert!(
        handles[ep("GET", "/devices").as_str()]
            .starts_with(&sym("webapi.go", "Service.webAPI/func@"))
    );
    assert_eq!(
        handles[ep("GET", "/api/web/catalog").as_str()],
        sym("catalog.go", "Service.catalog")
    );
    assert_eq!(
        handles[ep("POST", "/api/daemon/pull").as_str()],
        sym("catalog.go", "Service.pull")
    );
    assert_eq!(
        handles[ep("POST", "/api/web/relay-rules").as_str()],
        sym("webapi.go", "relayWrite")
    );
    assert!(handles[ep("ANY", "/.well-known/key").as_str()].starts_with("gap:go-handler:"));
    // A wrapper's trailing non-handler argument does not hide its handler.
    assert_eq!(
        handles[ep("GET", "/opts").as_str()],
        sym("catalog.go", "Service.catalog")
    );
    // A parameter named like another function's closure is not that closure.
    assert!(handles[ep("GET", "/other").as_str()].starts_with("gap:go-handler:"));
    // The literal's own calls are its calls, not the enclosing method's.
    assert!(
        edge_pairs(&extraction, "CALLS")
            .contains(&(literal, sym("webapi.go", "Service.session").as_str()))
    );
    // So are a bound closure's, though its body precedes its registration.
    let closure = handles[ep("GET", "/devices").as_str()];
    assert!(
        edge_pairs(&extraction, "CALLS")
            .contains(&(closure, sym("webapi.go", "Service.save").as_str()))
    );
    let node = extraction.nodes.iter().find(|n| n.id == literal).unwrap();
    assert_eq!(node.props["package_dir"], "pkg/device");
    // A receiver is proven lexically: a shadowing local or a same-named
    // local in another function is not the router.
    assert!(!handles.contains_key(ep("ANY", "/shadowed").as_str()));
    assert!(!handles.contains_key(ep("ANY", "/elsewhere").as_str()));
}

#[test]
fn a_reused_files_unresolved_handler_fallback_carries_the_new_commit() {
    // AC-0918: the Gap fallback kept for a package handler is retargeted
    // with the rest of a reused file's facts.
    let source = br#"package main

import (
    "net/http"
    "example.com/bot/handlers"
)

func routes(mux *http.ServeMux) {
    mux.HandleFunc("/x", handlers.Missing)
}
"#;
    let mut extraction =
        extract_source_with_module(source, "main.go", &id(), Some("example.com/bot")).unwrap();
    assert_eq!(extraction.pending_endpoints.len(), 1);
    retarget_commit(&mut extraction, "def456");
    let (endpoint, gap, handles) = &extraction.pending_endpoints[0].gap;
    for props in [&endpoint.props, &gap.props, &handles.props] {
        let text = props.to_string();
        assert!(
            text.contains("def456") && !text.contains("abc123"),
            "{text}"
        );
    }
}

/// AC-0946 (#2065): a URL read from a struct field or package constant is
/// joined to the one literal the package binds it to, across files; two
/// literals, an unproven binding or a test file's binding leave it computed.
#[test]
fn a_base_url_held_in_a_field_or_constant_joins_across_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("pkg/gh");
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::write(
        gh.join("client.go"),
        r#"package gh
import "net/http"
type Client struct { API string; OAuth string; Hook string; Other string }
func New() *Client { return &Client{API: "https://api.github.com", OAuth: oauthURL} }
func (c *Client) Get(path string) { http.NewRequest(http.MethodGet, c.API+path, nil) }
func (c *Client) Token() { http.NewRequest(http.MethodPost, c.OAuth, nil) }
func (c *Client) Ping() { http.Get(Status + "/ping") }
func (c *Client) Hooked() { http.NewRequest(http.MethodPost, c.Hook, nil) }
func (c *Client) Mixed() { http.NewRequest(http.MethodPost, c.Other, nil) }
func (c *Client) Local(Status string) { http.Get(Status + "/x") }
func Plain(o *Client) { http.NewRequest(http.MethodGet, o.API, nil) }
type Shadow struct{ URL string }
func NewShadow(Status string) *Shadow { return &Shadow{URL: Status} }
func (s *Shadow) Fetch() { http.Get(s.URL) }
type Assigned struct{ Base string }
func (a *Assigned) Fetch() { http.Get(a.Base) }
func setup(x *Assigned) { x.Base = "https://assigned.example" }
"#,
    )
    .unwrap();
    std::fs::write(
        gh.join("consts.go"),
        r#"package gh
const (
    oauthURL = "https://github.com/login/oauth/access_token"
    Status = "https://www.githubstatus.com"
)
type Other struct{ Other string }
var a = &Client{Hook: hookURL()}
var b = &Client{Other: "https://one.example"}
var c = &Other{Other: "https://two.example"}
func hookURL() string { return "" }
"#,
    )
    .unwrap();
    std::fs::write(
        gh.join("client_test.go"),
        "package gh\nvar t = &Client{API: \"http://127.0.0.1\"}\n",
    )
    .unwrap();
    let x = extract_dir(dir.path(), &id()).unwrap();
    let mut rows: Vec<(String, String)> = x
        .fetch_sites
        .iter()
        .map(|site| {
            let url = match (&site.url, &site.base) {
                (url, Some(base)) => format!("{:?} + {:?}", base.url, url),
                (url, None) => format!("{url:?}"),
            };
            (site.symbol.clone().unwrap_or_default(), url)
        })
        .collect();
    rows.sort();
    let row = |name: &str| {
        rows.iter()
            .find(|(symbol, _)| symbol.ends_with(&format!("Client.{name}")))
            .map(|(_, url)| url.as_str())
            .unwrap()
    };
    assert_eq!(
        row("Get"),
        "Literal(\"https://api.github.com\") + Computed(\"path\")"
    );
    assert_eq!(
        row("Token"),
        "Literal(\"https://github.com/login/oauth/access_token\")"
    );
    assert_eq!(
        row("Ping"),
        "Literal(\"https://www.githubstatus.com\") + Literal(\"/ping\")"
    );
    // A call result is no proven binding.
    assert_eq!(row("Hooked"), "Computed(\"c.Hook\")");
    // The receiver's type picks its own literal out of two.
    assert_eq!(row("Mixed"), "Literal(\"https://one.example\")");
    // A parameter shadows the package constant.
    assert_eq!(row("Local"), "Computed(\"Status + \\\"/x\\\"\")");
    let symbol_row = |suffix: &str| {
        rows.iter()
            .find(|(symbol, _)| symbol.ends_with(suffix))
            .map(|(_, url)| url.as_str())
            .unwrap()
    };
    // An operand that is not the method receiver has no proven type.
    assert_eq!(symbol_row("#Plain"), "Computed(\"o.API\")");
    // A parameter that shadows the constant is not the constant.
    assert_eq!(symbol_row("Shadow.Fetch"), "Computed(\"s.URL\")");
    // An assignment of unknown type cannot supply the value alone.
    assert_eq!(symbol_row("Assigned.Fetch"), "Computed(\"a.Base\")");
}

/// AC-0947 (#2065): a read of a store's conventional URL key is an observed
/// store of that provider, InferredStrong and marked, used by the reading
/// function; a token key and an unlisted key are not.
#[test]
fn a_store_reached_through_its_conventional_url_key_is_observed() {
    let source = br#"
package store
import "os"
func FromEnv() string {
    u := os.Getenv("KV_REST_API_URL")
    _ = os.Getenv("KV_REST_API_TOKEN")
    _ = os.Getenv("BASE_URL")
    return u
}
"#;
    let x = extract_source(source, "store.go", &id()).unwrap();
    let resources: Vec<_> = x.nodes.iter().filter(|n| n.label == "Resource").collect();
    let [store] = resources.as_slice() else {
        panic!("one store: {resources:?}")
    };
    assert_eq!(
        store.id,
        "res:local/go-service@observed:redis:env:KV_REST_API_URL"
    );
    assert_eq!(store.props["provider"], "redis");
    assert_eq!(store.props["kind"], "sdk-client");
    assert_eq!(store.props["env_key"], "KV_REST_API_URL");
    assert_eq!(store.props["store_from_env_key"], true);
    assert_eq!(store.props["prov"]["confidence_tier"], "InferredStrong");
    assert!(
        x.edges
            .iter()
            .any(|e| e.label == "USES" && e.dst == store.id && e.src.ends_with("#FromEnv"))
    );
}
