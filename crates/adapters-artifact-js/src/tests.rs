use super::*;
use base64::Engine as _;

const REPO: &str = "acme/shop";

fn id() -> SourceId<'static> {
    SourceId {
        repo: REPO,
        commit: "workdir",
    }
}

const API: &str =
    "export async function loadCart(id: string) {\n  return fetch('/api/cart/' + id);\n}\n";
const CART: &str = "import { loadCart } from './api';\nimport axios from 'axios';\nexport function showCart() {\n  return loadCart('1');\n}\n";
const LEGACY: &str =
    "function boot() {\n  return start();\n}\nfunction start() {\n  return 1;\n}\n";

fn app_map() -> String {
    serde_json::json!({
        "version": 3,
        "file": "app.js",
        "sources": [
            "webpack://shop/./src/api.ts",
            "webpack://shop/./src/cart.ts?abcd",
            "webpack://shop/node_modules/lodash/x.js",
            "webpack/bootstrap",
            "webpack://shop/(webpack)/buildin/global.js",
            "webpack://shop/./src/missing.ts",
            "webpack://shop/./src/present.ts",
            null
        ],
        "sourcesContent": [API, CART, "module.exports = 1;", "boot", "module.exports = g;", null, "export const x = 1;", null],
        "mappings": "AAAA"
    })
    .to_string()
}

fn legacy_bundle() -> String {
    let map = serde_json::json!({
        "version": 3,
        "sources": ["../src/legacy.js"],
        "sourcesContent": [LEGACY],
        "mappings": ""
    })
    .to_string();
    let inline = base64::engine::general_purpose::STANDARD.encode(map);
    format!(
        "function a(){{return b()}}\n//# sourceMappingURL=data:application/json;charset=utf-8;base64,{inline}\n"
    )
}

/// A checkout shipping built bundles: one with a sibling map, one with an
/// inline map, three whose maps cannot be read, and one in `dist/`.
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<(&str, String)> = vec![
        (
            "public/js/app.js",
            "console.log(1);\n//# sourceMappingURL=app.js.map\n".into(),
        ),
        ("public/js/app.js.map", app_map()),
        ("src/present.ts", "export const x = 1;\n".into()),
        ("static/legacy.js", legacy_bundle()),
        (
            "static/broken.js",
            "x();\n//# sourceMappingURL=nope.js.map\n".into(),
        ),
        (
            "static/escape.js",
            "x();\n//# sourceMappingURL=../../../x.map\n".into(),
        ),
        (
            "static/bad.js",
            "x();\n//# sourceMappingURL=bad.js.map\n".into(),
        ),
        ("static/bad.js.map", "{".into()),
        ("static/plain.js", "export function plain() {}\n".into()),
        (
            "dist/app.js",
            "x();\n//# sourceMappingURL=../public/js/app.js.map\n".into(),
        ),
    ];
    for (rel, text) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn node<'a>(out: &'a Extraction, id: &str) -> &'a Node {
    out.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| {
            panic!(
                "no node {id}: {:#?}",
                out.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
            )
        })
}

fn provenance(props: &serde_json::Value) -> Provenance {
    serde_json::from_value(props["prov"].clone()).unwrap()
}

/// AC-1216: the original sources a bundle's map carries are read as T0
/// facts placed under the bundle, each citing the map's digest and the
/// source as its member; calls between recovered sources resolve.
#[test]
fn mapped_sources_are_read_under_their_bundle_with_artifact_evidence() {
    let dir = checkout();
    let mut seen = Vec::new();
    let out = extract_dir(dir.path(), &id(), &mut |rel| seen.push(rel.to_string())).unwrap();
    let map_digest = digest(app_map().as_bytes());

    let cart = node(&out, &format!("file:{REPO}@public/js/app.js!/src/cart.ts"));
    assert_eq!(cart.label, "File");
    let prov = provenance(&cart.props);
    assert_eq!(prov.extractor_id, EXTRACTOR_ID);
    assert_eq!(prov.tier, Tier::Deterministic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
    assert!(prov.evidence.is_empty());
    assert_eq!(
        prov.basis,
        Some(EvidenceBasis::Artifact {
            refs: vec![ArtifactRef {
                digest: map_digest.clone(),
                kind: MAP_KIND.into(),
                at: ArtifactLocator::Member {
                    path: "src/cart.ts".into()
                },
            }]
        })
    );
    node(&out, &format!("file:{REPO}@public/js/app.js!/src/api.ts"));

    // The import call from cart.ts into api.ts resolves across recovered files.
    let call = out
        .edges
        .iter()
        .find(|edge| {
            edge.label == "CALLS" && edge.src.contains("cart.ts") && edge.dst.contains("loadCart")
        })
        .unwrap_or_else(|| panic!("no call: {:#?}", out.edges));
    assert!(call.dst.contains("public/js/app.js!/src/api.ts"));
    assert_eq!(provenance(&call.props).extractor_id, EXTRACTOR_ID);

    // The inline map is cited from the bundle that carries it.
    let legacy = node(
        &out,
        &format!("file:{REPO}@static/legacy.js!/src/legacy.js"),
    );
    let EvidenceBasis::Artifact { refs } = provenance(&legacy.props).basis.unwrap() else {
        panic!("artifact basis");
    };
    assert_eq!(refs[0].kind, BUNDLE_KIND);
    assert_eq!(refs[0].digest, digest(legacy_bundle().as_bytes()));

    // Dependencies, the bundler runtime, sources the checkout holds and
    // bundles in build output are not recovered.
    for skipped in [
        "node_modules",
        "bootstrap",
        "buildin",
        "present.ts",
        "@dist/",
        "plain.js",
    ] {
        assert!(
            out.nodes.iter().all(|node| !node.id.contains(skipped)),
            "{skipped} was read"
        );
    }
    assert_eq!(
        seen,
        [
            "public/js/app.js",
            "static/bad.js",
            "static/broken.js",
            "static/escape.js",
            "static/legacy.js"
        ]
    );

    // Every fact is attributed to the artifact reader, with valid evidence.
    for prov in out
        .nodes
        .iter()
        .map(|node| &node.props)
        .chain(out.edges.iter().map(|edge| &edge.props))
        .map(provenance)
    {
        assert_eq!(prov.extractor_id, EXTRACTOR_ID);
        prov.validate().unwrap();
        prov.basis().validate().unwrap();
        assert!(matches!(prov.basis(), EvidenceBasis::Artifact { .. }));
    }
}

/// AC-1216: a map that cannot be read is one Gap on its bundle naming why,
/// and mapped sources without content are counted in one Gap.
#[test]
fn unreadable_maps_are_gaps_naming_why() {
    let dir = checkout();
    let out = extract_dir(dir.path(), &id(), &mut |_| {}).unwrap();
    let code = |bundle: &str, kind: &str| {
        node(&out, &format!("gap:{REPO}@{bundle}#{kind}")).props["reason_code"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(code("static/broken.js", "js-bundle"), "map_missing");
    assert_eq!(code("static/escape.js", "js-bundle"), "map_outside");
    assert_eq!(code("static/bad.js", "js-bundle"), "bad_map");
    assert_eq!(
        code("public/js/app.js", "js-sources-content"),
        "no_sources_content"
    );
    let missing = node(
        &out,
        &format!("gap:{REPO}@public/js/app.js#js-sources-content"),
    );
    assert!(
        missing.props["reason"]
            .as_str()
            .unwrap()
            .contains("1 mapped sources")
    );
    assert_eq!(
        provenance(&missing.props).confidence_tier,
        ConfidenceTier::Gap
    );
}

/// AC-1216: reading the same files from a capture, in any order, gives the
/// same extraction as reading the checkout.
#[test]
fn a_capture_read_in_any_order_matches_the_checkout() {
    let dir = checkout();
    let from_checkout = extract_dir(dir.path(), &id(), &mut |_| {}).unwrap();
    let mut paths = walk(dir.path()).unwrap();
    paths.reverse();
    let read = |rel: &str| std::fs::read(dir.path().join(rel));
    let from_capture = extract_dir_from(&id(), &paths, &read, &mut |_| {}).unwrap();
    assert_eq!(from_checkout, from_capture);
    assert_eq!(
        artifact_paths(dir.path()).unwrap(),
        [
            "public/js/app.js",
            "public/js/app.js.map",
            "static/bad.js",
            "static/bad.js.map"
        ]
    );
}

/// AC-1216: a map over the limit is a `too_large` Gap citing the whole
/// file's digest, and is never read whole.
#[test]
fn an_oversized_map_is_a_gap_not_a_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.js"),
        "x();\n//# sourceMappingURL=app.js.map\n",
    )
    .unwrap();
    // A sparse file: the length is over the limit without the disk use.
    let map = std::fs::File::create(dir.path().join("app.js.map")).unwrap();
    map.set_len(MAX_ARTIFACT_BYTES as u64 + 10).unwrap();
    drop(map);
    let out = extract_dir(dir.path(), &id(), &mut |_| {}).unwrap();
    let gap = node(&out, &format!("gap:{REPO}@app.js#js-bundle"));
    assert_eq!(gap.props["reason_code"], "too_large");
    let EvidenceBasis::Artifact { refs } = provenance(&gap.props).basis.unwrap() else {
        panic!("artifact basis");
    };
    assert_eq!(refs[0].kind, MAP_KIND);
    assert_eq!(
        refs[0].at,
        ArtifactLocator::Offset {
            start: 0,
            end: MAX_ARTIFACT_BYTES as u64 + 10
        }
    );
    assert_eq!(refs[0].digest, digest(&vec![0; MAX_ARTIFACT_BYTES + 10]));
}

/// AC-1216: a capture is read only when it holds every map file its
/// bundles name, whatever the file is called; inline maps need none.
#[test]
fn a_capture_is_read_when_it_holds_the_named_maps() {
    let files = std::collections::BTreeMap::from([
        ("a.js", "x();\n//# sourceMappingURL=maps/a.json\n"),
        ("maps/a.json", "{}"),
        (
            "b.js",
            "x();\n//# sourceMappingURL=data:application/json,%7B%7D\n",
        ),
        ("build/c.js", "x();\n//# sourceMappingURL=c.js.map\n"),
    ]);
    let read = |rel: &str| {
        files
            .get(rel)
            .map(|text| text.as_bytes().to_vec())
            .ok_or_else(|| std::io::Error::other("not captured"))
    };
    let paths = |names: &[&str]| {
        names
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>()
    };
    assert!(capture_holds_maps(
        &paths(&["a.js", "maps/a.json", "b.js", "build/c.js"]),
        &read
    ));
    assert!(capture_holds_maps(&paths(&["b.js"]), &read));
    assert!(!capture_holds_maps(&paths(&["a.js", "b.js"]), &read));
}

/// AC-1216: a business rule read from a recovered source is supporting
/// evidence, never confirmed from the artifact alone.
#[test]
fn business_rules_from_an_artifact_are_not_confirmed() {
    let source = Recovered {
        path: "public/app.js!/src/rules.ts".into(),
        member: "src/rules.ts".into(),
        content: String::new(),
        cited: Cited::of(b"{}", MAP_KIND),
    };
    let by_path = BTreeMap::from([(source.path.as_str(), &source)]);
    let fact = |label: &str| {
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![core_prov::EvidenceRef {
                repo: REPO.into(),
                path: source.path.clone(),
                byte_start: 0,
                byte_end: 4,
                commit_sha: "workdir".into(),
            }],
            "t0.adapter-ts",
            label.as_bytes(),
        )
        .unwrap();
        let mut props = serde_json::json!({ "prov": prov });
        assert!(restamp(&mut props, label, "x", None, &by_path));
        provenance(&props).confidence_tier
    };
    assert_eq!(fact("BusinessRule"), ConfidenceTier::InferredStrong);
    assert_eq!(fact("GOVERNS"), ConfidenceTier::InferredStrong);
    assert_eq!(fact("Symbol"), ConfidenceTier::Confirmed);
}

#[test]
fn source_map_comments_urls_and_sections() {
    use source_map::*;
    assert_eq!(
        mapping_url(b"a()\n//# sourceMappingURL=app.js.map\n\n").as_deref(),
        Some("app.js.map")
    );
    assert_eq!(
        mapping_url(b"a()\n//@ sourceMappingURL=old.map").as_deref(),
        Some("old.map")
    );
    // Only a trailing comment names the map.
    assert_eq!(mapping_url(b"//# sourceMappingURL=x.map\na()\n"), None);
    assert_eq!(
        data_url("data:application/json,%7B%7D"),
        Some(Ok(b"{}".to_vec()))
    );
    assert_eq!(data_url("data:application/json;base64,!!"), Some(Err(())));
    assert_eq!(data_url("app.js.map"), None);

    let sectioned = serde_json::json!({
        "version": 3,
        "sections": [
            {"offset": {"line": 0, "column": 0}, "map": {
                "version": 3, "sourceRoot": "lib", "sources": ["a.ts"], "sourcesContent": ["a"], "mappings": ""
            }},
            {"offset": {"line": 9, "column": 0}, "map": {
                "version": 3, "sources": ["webpack:///b.ts"], "mappings": ""
            }}
        ]
    });
    assert_eq!(
        sources(sectioned.to_string().as_bytes()).unwrap(),
        [
            MappedSource {
                url: "lib/a.ts".into(),
                content: Some("a".into())
            },
            MappedSource {
                url: "webpack:///b.ts".into(),
                content: None
            }
        ]
    );
    assert_eq!(
        sources(br#"{"version":2,"sources":[]}"#),
        Err(MapRefusal::Version)
    );
    assert_eq!(
        sources(br#"{"version":3,"sections":[{"url":"x.map"}]}"#),
        Err(MapRefusal::SectionUrl)
    );
}

#[test]
fn mapped_source_paths_read_from_the_root_or_the_map() {
    assert_eq!(
        source_path("webpack://app/./src/a.ts", "public").as_deref(),
        Some("src/a.ts")
    );
    assert_eq!(
        source_path("webpack:///src/a.ts?x#y", "public").as_deref(),
        Some("src/a.ts")
    );
    assert_eq!(
        source_path("../src/a.ts", "public/js").as_deref(),
        Some("public/src/a.ts")
    );
    assert_eq!(
        source_path("../../../a.ts", "public").as_deref(),
        Some("a.ts")
    );
    assert_eq!(
        source_path("/src/a.ts", "public").as_deref(),
        Some("src/a.ts")
    );
    assert_eq!(source_path("C:\\src\\a.ts", "").as_deref(), None);
    assert_eq!(source_path("webpack://app", "").as_deref(), None);
    assert_eq!(source_path("src/a:b.ts", "").as_deref(), None);
}
