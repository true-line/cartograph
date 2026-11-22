use std::path::Path;

use crate::{Extraction, SourceId, extract_dir};

fn id() -> SourceId<'static> {
    SourceId {
        repo: "acme/shop",
        commit: "abc123",
    }
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn extract(files: &[(&str, &str)]) -> Extraction {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), files);
    extract_dir(dir.path(), &id()).unwrap()
}

fn dependency_calls(out: &Extraction) -> Vec<&core_graph::Edge> {
    out.edges
        .iter()
        .filter(|edge| {
            edge.label == "CALLS" && edge.props["resolution"].as_str() == Some("dependency-types")
        })
        .collect()
}

fn gaps(out: &Extraction) -> usize {
    out.nodes.iter().filter(|node| node.label == "Gap").count()
}

const MANIFEST: &str = r#"{
  "name": "shop",
  "dependencies": {
    "@acme/storage": "^3.1.0",
    "billing": "^2.0.0",
    "legacy": "^1.0.0",
    "untyped": "^1.0.0"
  }
}
"#;

const SERVICE: &str = "import { upload, Bucket, Options, missing } from '@acme/storage';
import * as billing from 'billing';
import { legacy } from 'legacy';
import { other } from 'undeclared';
import { untypedCall } from 'untyped';
export function checkout() {
    upload('a');
    new Bucket();
    missing();
    billing.charge(10);
    legacy();
    other();
    untypedCall();
}
export function settle() {
    billing.refund(5);
    billing.charge(1);
}
";

fn storage_package() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "node_modules/@acme/storage/package.json",
            r#"{"name": "@acme/storage", "version": "3.1.4", "types": "./dist/index.d.ts"}"#,
        ),
        (
            "node_modules/@acme/storage/dist/index.d.ts",
            "export * from './upload';\nexport { Bucket } from './bucket';\nexport interface Options { size: number }\n",
        ),
        (
            "node_modules/@acme/storage/dist/upload.d.ts",
            "export declare function upload(key: string): Promise<void>;\n",
        ),
        (
            "node_modules/@acme/storage/dist/bucket.d.ts",
            "export declare class Bucket {}\n",
        ),
        // Implementation code is never read: a parse would fail loudly.
        (
            "node_modules/@acme/storage/dist/index.js",
            "<<not javascript>>",
        ),
        (
            "node_modules/billing/package.json",
            r#"{"name": "billing", "version": "2.0.1", "exports": {".": {"import": "./index.mjs", "types": {"import": "./types/main.d.ts", "require": "./types/main.d.cts"}}}}"#,
        ),
        (
            "node_modules/billing/types/main.d.ts",
            "declare function charge(amount: number): void;\nexport { charge };\nexport declare const refund: (amount: number) => void;\nexport declare namespace Refunds { function create(): void; }\n",
        ),
        (
            "node_modules/legacy/package.json",
            r#"{"name": "legacy", "version": "1.0.0", "types": "index.d.ts"}"#,
        ),
        (
            "node_modules/legacy/index.d.ts",
            "declare function legacy(): void;\nexport = legacy;\n",
        ),
        (
            "node_modules/untyped/package.json",
            r#"{"name": "untyped", "version": "1.0.0", "main": "index.js"}"#,
        ),
        ("node_modules/untyped/index.js", "module.exports = {};\n"),
    ]
}

fn project(allow: Option<&str>) -> Vec<(&'static str, String)> {
    let mut files: Vec<(&str, String)> = vec![
        ("package.json", MANIFEST.into()),
        ("src/checkout.ts", SERVICE.into()),
    ];
    files.extend(
        storage_package()
            .into_iter()
            .map(|(p, t)| (p, t.to_string())),
    );
    if let Some(allow) = allow {
        files.push((".cartograph/types.toml", allow.into()));
    }
    files
}

fn extract_project(allow: Option<&str>) -> Extraction {
    let files = project(allow);
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    extract(&refs)
}

const ALLOW_ALL: &str =
    "read = [\"@acme/storage\", \"billing\", \"legacy\", \"untyped\", \"undeclared\"]\n";

// AC-1506: a named-import call and a namespace-member call on allowlisted
// packages become CALLS edges to the package's external boundary, naming
// the member, its declaration kind and file, and the installed version,
// citing the call site and the manifest's dependency line.
#[test]
fn allowlisted_package_calls_reach_their_boundary() {
    let out = extract_project(Some(ALLOW_ALL));
    let calls = dependency_calls(&out);
    let summary: Vec<_> = calls
        .iter()
        .map(|edge| {
            (
                edge.src.as_str(),
                edge.dst.as_str(),
                edge.props["members"].clone(),
                edge.props["declarations"].clone(),
                edge.props["declared_in"].clone(),
                edge.props["package_version"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "sym:acme/shop@src/checkout.ts#checkout",
                "mod:@acme/storage",
                serde_json::json!(["upload"]),
                serde_json::json!(["function"]),
                serde_json::json!(["dist/upload.d.ts"]),
                "3.1.4",
            ),
            (
                "sym:acme/shop@src/checkout.ts#checkout",
                "mod:billing",
                serde_json::json!(["charge"]),
                serde_json::json!(["function"]),
                serde_json::json!(["types/main.d.ts"]),
                "2.0.1",
            ),
            (
                // Both members one caller reaches stay on its one edge.
                "sym:acme/shop@src/checkout.ts#settle",
                "mod:billing",
                serde_json::json!(["charge", "refund"]),
                serde_json::json!(["function", "variable"]),
                serde_json::json!(["types/main.d.ts", "types/main.d.ts"]),
                "2.0.1",
            ),
        ]
    );
    let evidence: Vec<_> = serde_json::from_value::<Vec<serde_json::Value>>(
        calls[0].props["prov"]["evidence"].clone(),
    )
    .unwrap()
    .into_iter()
    .map(|cited| cited["path"].as_str().unwrap().to_string())
    .collect();
    assert_eq!(evidence, ["src/checkout.ts", "package.json"]);
    // The settle edge cites both call sites and the manifest line once.
    let cited = calls[2].props["prov"]["evidence"].as_array().unwrap();
    let paths: Vec<_> = cited.iter().map(|c| c["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        ["src/checkout.ts", "package.json", "src/checkout.ts"]
    );
    // The boundary the calls reach is still the import's external module.
    let module = out
        .nodes
        .iter()
        .find(|node| node.id == "mod:billing")
        .unwrap();
    assert_eq!(module.props["boundary"], "external");
    assert_eq!(
        out.dependency_types,
        crate::DependencyTypesReport {
            packages_read: vec!["@acme/storage".into(), "billing".into()],
            packages_unavailable: vec![
                ("legacy".into(), "`export =` (CommonJS) declarations".into()),
                ("untyped".into(), "no type declarations".into()),
            ],
            calls_proven: 4,
        }
    );
}

// AC-1506: a warm-cache walk proves the same calls, under the new commit.
#[test]
fn warm_cache_walks_prove_the_same_calls() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(Some(ALLOW_ALL));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    write(dir.path(), &refs);
    let mut cache = crate::IncrementalCache::default();
    let (cold, _) = crate::extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    let next = SourceId {
        repo: "acme/shop",
        commit: "def456",
    };
    let (warm, stats) = crate::extract_dir_incremental(dir.path(), &next, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    let cold_calls = dependency_calls(&cold);
    let warm_calls = dependency_calls(&warm);
    assert_eq!(cold_calls.len(), 3);
    assert_eq!(warm_calls.len(), 3);
    for edge in warm_calls {
        for cited in edge.props["prov"]["evidence"].as_array().unwrap() {
            assert_eq!(cited["commit_sha"], "def456");
        }
    }
}

// AC-1506: with no allowlist nothing changes, byte for byte, whether or not
// `node_modules` is installed; and the reader never adds a Gap.
#[test]
fn without_an_allowlist_facts_are_unchanged_and_no_gap_is_added() {
    let without_modules = extract(&[("package.json", MANIFEST), ("src/checkout.ts", SERVICE)]);
    let installed = extract_project(None);
    assert_eq!(installed.nodes, without_modules.nodes);
    assert_eq!(installed.edges, without_modules.edges);
    assert_eq!(
        installed.dependency_types,
        crate::DependencyTypesReport::default()
    );
    assert!(dependency_calls(&installed).is_empty());

    let allowed = extract_project(Some(ALLOW_ALL));
    assert_eq!(gaps(&allowed), gaps(&installed));
    // Only the proven edges are new.
    let mut base = installed.edges.clone();
    base.extend(dependency_calls(&allowed).into_iter().cloned());
    base.sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
    let mut after = allowed.edges.clone();
    after.sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
    assert_eq!(after.len(), base.len());
}

// AC-1506: a package the allowlist omits proves nothing even when installed.
#[test]
fn unlisted_packages_are_never_read() {
    let out = extract_project(Some("read = [\"billing\"]\n"));
    let members: Vec<_> = dependency_calls(&out)
        .iter()
        .map(|edge| edge.props["members"].clone())
        .collect();
    assert_eq!(
        members,
        [
            serde_json::json!(["charge"]),
            serde_json::json!(["charge", "refund"])
        ]
    );
    assert_eq!(out.dependency_types.packages_read, ["billing"]);
}

// AC-1506: `export *` reaching two different declarations is ambiguous and
// proves nothing; a named re-export shadows it.
#[test]
fn ambiguous_star_exports_prove_nothing() {
    let out = extract(&[
        (
            "package.json",
            r#"{"name": "shop", "dependencies": {"kit": "1.0.0"}}"#,
        ),
        (
            "src/use.ts",
            "import { make, pick } from 'kit';\nexport function run() { make(); pick(); }\n",
        ),
        (".cartograph/types.toml", "read = [\"kit\"]\n"),
        (
            "node_modules/kit/package.json",
            r#"{"name": "kit", "typings": "index.d.ts"}"#,
        ),
        (
            "node_modules/kit/index.d.ts",
            "export * from './a';\nexport * from './b';\nexport { pick } from './a';\n",
        ),
        (
            "node_modules/kit/a.d.ts",
            "export declare function make(): void;\nexport declare function pick(): void;\n",
        ),
        (
            "node_modules/kit/b.d.ts",
            "export declare class make {}\nexport declare class pick {}\n",
        ),
    ]);
    let members: Vec<_> = dependency_calls(&out)
        .iter()
        .map(|edge| {
            (
                edge.props["members"].clone(),
                edge.props["declared_in"].clone(),
            )
        })
        .collect();
    assert_eq!(
        members,
        [(serde_json::json!(["pick"]), serde_json::json!(["a.d.ts"]))]
    );
}

// AC-1506: a package without its own types falls back to `@types/<name>`,
// scoped names folded the DefinitelyTyped way.
#[test]
fn types_packages_answer_for_untyped_packages() {
    let out = extract(&[
        (
            "package.json",
            r#"{"name": "shop", "dependencies": {"@acme/queue": "1.0.0"}}"#,
        ),
        (
            "src/use.ts",
            "import { send } from '@acme/queue';\nexport function run() { send(); }\n",
        ),
        (".cartograph/types.toml", "read = [\"@acme/queue\"]\n"),
        (
            "node_modules/@acme/queue/package.json",
            r#"{"name": "@acme/queue", "main": "q.js"}"#,
        ),
        (
            "node_modules/@types/acme__queue/package.json",
            r#"{"name": "@types/acme__queue", "version": "1.0.3"}"#,
        ),
        (
            "node_modules/@types/acme__queue/index.d.ts",
            "export function send(): void;\n",
        ),
    ]);
    let calls = dependency_calls(&out);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].props["package_version"], "1.0.3");
}

// AC-1506: a malformed allowlist refuses the walk instead of reading nothing.
#[test]
fn malformed_allowlists_refuse() {
    for config in [
        "read = [\"@acme/*\"]\n",
        "read = [\"billing/sub\"]\n",
        "read = [\"billing\", \"billing\"]\n",
        "read = [\"billing\"]\nextra = true\n",
        "read = \"billing\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &[(".cartograph/types.toml", config)]);
        let error = crate::dependency_types_allowlist(dir.path()).unwrap_err();
        assert!(
            error.to_string().contains(".cartograph/types.toml"),
            "{config}: {error}"
        );
        assert!(extract_dir(dir.path(), &id()).is_err(), "{config}");
    }
}

// AC-1506: an allowlist reached through a symlink is refused, and a package
// directory that resolves outside the repository is never read.
#[cfg(unix)]
#[test]
fn symlinks_never_reach_outside_the_repository() {
    let outside = tempfile::tempdir().unwrap();
    write(
        outside.path(),
        &[
            ("types.toml", "read = [\"billing\"]\n"),
            (
                "billing/package.json",
                r#"{"name": "billing", "types": "index.d.ts"}"#,
            ),
            (
                "billing/index.d.ts",
                "export declare function charge(): void;\n",
            ),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[(".cartograph/.keep", "")]);
    std::os::unix::fs::symlink(
        outside.path().join("types.toml"),
        dir.path().join(".cartograph/types.toml"),
    )
    .unwrap();
    assert!(crate::dependency_types_allowlist(dir.path()).is_err());

    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (
                "package.json",
                r#"{"name": "shop", "dependencies": {"billing": "1"}}"#,
            ),
            (
                "src/use.ts",
                "import { charge } from 'billing';\nexport function run() { charge(); }\n",
            ),
            (".cartograph/types.toml", "read = [\"billing\"]\n"),
        ],
    );
    std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("billing"),
        dir.path().join("node_modules/billing"),
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(dependency_calls(&out).is_empty());
    assert_eq!(
        out.dependency_types.packages_unavailable,
        [(
            "billing".to_string(),
            "not installed under node_modules".to_string()
        )]
    );
}
