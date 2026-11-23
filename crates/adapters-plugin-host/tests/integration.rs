//! Real wasmtime component execution against prebuilt guest fixtures
//! (see `tests/fixtures/README.md`). Proves AC-0070: fuel exhaustion,
//! memory-cap, and deadline bounds all fail the call closed with an
//! explicit `HostError` and no partial facts, and the guest gets no
//! ambient network capability.

use std::time::Duration;

use adapters_plugin_host::{HostError, PluginHost, PluginLimits, RefusalReason, SourceId};

const OK_ADAPTER: &[u8] = include_bytes!("fixtures/compiled/ok-adapter.wasm");
const BUSY_LOOP: &[u8] = include_bytes!("fixtures/compiled/busy-loop.wasm");
const MEMORY_HOG: &[u8] = include_bytes!("fixtures/compiled/memory-hog.wasm");
const NET_PROBE: &[u8] = include_bytes!("fixtures/compiled/net-probe.wasm");
const CLOCK_PROBE: &[u8] = include_bytes!("fixtures/compiled/clock-probe.wasm");

fn source_id() -> SourceId {
    SourceId {
        repo: "owner/repo".to_string(),
        commit: "deadbeef".to_string(),
    }
}

/// T-0070 (AC-0070): a well-behaved plugin's facts round-trip through the
/// host exactly as it emitted them.
#[test]
fn ok_adapter_round_trips_facts() {
    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(OK_ADAPTER, "t0.plugin-fixture")
        .expect("compiles");

    let source = b"hello world";
    let extraction = host
        .call_extract(
            &plugin,
            source,
            "src/lib.rs",
            &source_id(),
            PluginLimits::default(),
        )
        .expect("well-behaved plugin succeeds");

    assert_eq!(extraction.nodes.len(), 1);
    assert_eq!(extraction.edges.len(), 1);
    let node = &extraction.nodes[0];
    assert_eq!(node.id, "owner/repo:src/lib.rs");
    assert_eq!(node.props["len"], source.len());
}

/// T-0070 (AC-0070): a plugin that never returns exhausts its fuel bound
/// and fails closed rather than hanging or partially completing.
#[test]
fn fuel_exhaustion_fails_closed() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(BUSY_LOOP, "t0.plugin-busy").expect("compiles");

    let limits = PluginLimits {
        max_fuel: 100_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(30),
    };
    let err = host
        .call_extract(&plugin, b"x", "loop.rs", &source_id(), limits)
        .expect_err("busy loop must not complete");

    assert!(
        matches!(err, HostError::FuelExhausted(_)),
        "expected FuelExhausted, got {err:?}"
    );
}

/// T-0070 (AC-0070): a plugin given ample fuel but a short wall-clock
/// deadline is interrupted and fails closed.
#[test]
fn deadline_exceeded_fails_closed() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(BUSY_LOOP, "t0.plugin-busy").expect("compiles");

    let limits = PluginLimits {
        max_fuel: u64::MAX,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_millis(60),
    };
    let err = host
        .call_extract(&plugin, b"x", "loop.rs", &source_id(), limits)
        .expect_err("busy loop must not complete");

    assert!(
        matches!(err, HostError::DeadlineExceeded(_)),
        "expected DeadlineExceeded, got {err:?}"
    );
}

/// T-0070 (AC-0070): a plugin that grows memory without bound is denied
/// past the configured cap and fails closed instead of exhausting host
/// memory.
#[test]
fn memory_limit_exceeded_fails_closed() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(MEMORY_HOG, "t0.plugin-memory").expect("compiles");

    let limits = PluginLimits {
        max_fuel: u64::MAX,
        max_memory_bytes: 2 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(30),
    };
    let err = host
        .call_extract(&plugin, b"x", "hog.rs", &source_id(), limits)
        .expect_err("unbounded allocation must not complete");

    assert!(
        matches!(err, HostError::MemoryLimitExceeded(_)),
        "expected MemoryLimitExceeded, got {err:?}"
    );
}

/// T-0070 (AC-0070): the host grants no ambient network capability — a
/// plugin's TCP connect attempt is denied by the default (empty) WasiCtx
/// before any real connection is made.
#[test]
fn no_ambient_network_capability() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(NET_PROBE, "t0.plugin-net").expect("compiles");

    let extraction = host
        .call_extract(
            &plugin,
            b"x",
            "probe.rs",
            &source_id(),
            PluginLimits::default(),
        )
        .expect("probe adapter completes and reports its outcome");

    let outcome = extraction.nodes[0].props["outcome"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        outcome.starts_with("denied:"),
        "expected the connect attempt to be denied, got outcome={outcome:?}"
    );
}

/// T-0070 (AC-0070): the host grants no ambient wall/monotonic clock —
/// ADR-0017's sandboxed determinism requires that a plugin reading the
/// clock still produces byte-identical facts run to run. Two calls
/// separated by a real sleep must report identical fixed clock readings,
/// not the host's actual elapsed wall-clock time.
#[test]
fn no_ambient_clock() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(CLOCK_PROBE, "t0.plugin-clock").expect("compiles");

    let read = |host: &PluginHost| {
        let extraction = host
            .call_extract(
                &plugin,
                b"x",
                "clock.rs",
                &source_id(),
                PluginLimits::default(),
            )
            .expect("clock probe completes");
        extraction.nodes[0].props.clone()
    };

    let first = read(&host);
    std::thread::sleep(Duration::from_millis(50));
    let second = read(&host);

    assert_eq!(
        first, second,
        "clock readings must be fixed, not ambient (first call vs. after a real sleep)"
    );
    assert_eq!(
        first["wall_millis"], 0,
        "wall clock must be fixed at the epoch"
    );
    assert_eq!(
        first["elapsed_nanos"], 0,
        "monotonic clock must be fixed, so elapsed-within-a-call is always zero"
    );
}

/// T-0069 (AC-0069, #199): plugin facts are pinned to the exact artifact —
/// `extractor_id = "{plugin_id}@{hash12}"`, full BLAKE3 artifact hash on
/// every fact — the guest cannot impersonate a compiled-in extractor, and
/// repeat runs of the same artifact over the same source yield an
/// identical whole-extraction hash set (determinism with a plugin active).
#[test]
fn plugin_provenance_pins_artifact_and_repeats_deterministically() {
    use adapters_plugin_host::pin_extraction;
    use core_prov::content_hash;

    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(OK_ADAPTER, "t0.plugin-fixture")
        .expect("compiles");
    let artifact_hash = content_hash(OK_ADAPTER);
    let source = b"hello world";

    // call_extract pins automatically (#204 review): no manual step.
    let run = || {
        host.call_extract(
            &plugin,
            source,
            "src/lib.rs",
            &source_id(),
            PluginLimits::default(),
        )
        .expect("well-behaved plugin succeeds")
    };

    let first = run();
    let expected_id = format!("t0.plugin-fixture@{}", &artifact_hash[..12]);
    // Every fact carries the full artifact hash, prov or not; a fact
    // without provenance is the conformance gate's job to reject (#200).
    for props in first
        .nodes
        .iter()
        .map(|node| &node.props)
        .chain(first.edges.iter().map(|edge| &edge.props))
    {
        assert_eq!(props["plugin_artifact_hash"], artifact_hash.as_str());
    }

    // A guest-supplied extractor_id is overwritten — a plugin cannot
    // impersonate a compiled-in extractor.
    let mut spoofed = adapters_plugin_host::PluginExtraction {
        nodes: vec![core_graph::Node {
            id: "n1".into(),
            label: "File".into(),
            props: serde_json::json!({ "prov": { "extractor_id": "t0.adapter-ts" } }),
        }],
        edges: Vec::new(),
    };
    pin_extraction(&mut spoofed, "t0.plugin-fixture", &artifact_hash);
    assert_eq!(
        spoofed.nodes[0].props["prov"]["extractor_id"],
        expected_id.as_str()
    );
    assert_eq!(
        spoofed.nodes[0].props["plugin_artifact_hash"],
        artifact_hash.as_str()
    );

    // Order-independent whole-extraction hash: identical across runs.
    let canonical = |extraction: &adapters_plugin_host::PluginExtraction| {
        let mut hashes: Vec<String> = extraction
            .nodes
            .iter()
            .map(|node| format!("node:{}:{}", node.id, node.props))
            .chain(extraction.edges.iter().map(|edge| {
                format!(
                    "edge:{}:{}:{}:{}",
                    edge.src, edge.dst, edge.label, edge.props
                )
            }))
            .collect();
        hashes.sort();
        content_hash(hashes.join("\n").as_bytes())
    };
    assert_eq!(canonical(&first), canonical(&run()));

    // A different artifact is a different extractor identity: same id,
    // different bytes can never masquerade as the pinned adapter.
    let other_hash = content_hash(BUSY_LOOP);
    assert_ne!(
        format!("t0.plugin-fixture@{}", &other_hash[..12]),
        expected_id
    );
}

/// T-0068 (AC-0068, #200): the conformance gate passes a well-behaved
/// adapter with a correct golden corpus, and fails closed — with the
/// failing check named — on golden mismatch, an empty corpus, and a
/// bound-violating artifact. Double-run determinism is part of the gate.
#[test]
fn conformance_gate_passes_golden_corpus_and_fails_closed() {
    use adapters_plugin_host::gate::{
        ExpectedEdge, ExpectedNode, GoldenCase, GoldenCorpus, run_gate,
    };

    let host = PluginHost::new().expect("engine");
    let corpus = GoldenCorpus {
        extensions: vec!["rs".to_string()],
        cases: vec![GoldenCase {
            path: "src/lib.rs".to_string(),
            source: "hello world".to_string(),
            nodes: vec![ExpectedNode {
                id: "owner/repo:src/lib.rs".to_string(),
                label: "TestNode".to_string(),
                props: serde_json::json!({ "len": 11 }),
            }],
            edges: vec![ExpectedEdge {
                src: "owner/repo:src/lib.rs".to_string(),
                dst: "owner/repo:src/lib.rs".to_string(),
                label: "SELF".to_string(),
                props: serde_json::json!({}),
            }],
        }],
        coverage: vec![],
        languages: Default::default(),
    };

    let report = run_gate(
        &host,
        "t0.plugin-fixture",
        OK_ADAPTER,
        &corpus,
        PluginLimits::default(),
        &source_id(),
    );
    assert!(report.passed, "checks: {:?}", report.checks);
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"spi-compiles"));
    assert!(names.contains(&"golden:src/lib.rs"));
    assert!(names.contains(&"determinism-double-run"));

    // A wrong expectation fails the golden check, not the whole harness.
    let mut wrong = corpus.clone();
    wrong.cases[0].nodes[0].props = serde_json::json!({ "len": 999 });
    let report = run_gate(
        &host,
        "t0.plugin-fixture",
        OK_ADAPTER,
        &wrong,
        PluginLimits::default(),
        &source_id(),
    );
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "golden:src/lib.rs" && !c.passed)
    );

    // An empty corpus proves nothing and fails closed.
    let empty = GoldenCorpus {
        extensions: vec![],
        cases: vec![],
        coverage: vec![],
        languages: Default::default(),
    };
    let report = run_gate(
        &host,
        "t0.plugin-fixture",
        OK_ADAPTER,
        &empty,
        PluginLimits::default(),
        &source_id(),
    );
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "corpus-nonempty" && !c.passed)
    );

    // A bound-violating artifact fails the contract check by name.
    let tight = PluginLimits {
        max_fuel: 100_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(30),
    };
    let report = run_gate(
        &host,
        "t0.plugin-busy",
        BUSY_LOOP,
        &corpus,
        tight,
        &source_id(),
    );
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "contract:src/lib.rs" && !c.passed)
    );
}

#[test]
fn coverage_declarations_count_only_when_their_cases_pass() {
    // AC-0263: a declared cell names the golden cases proving it; the gate
    // refuses an unproven, unnoted or malformed claim, and a verdict is keyed
    // to the exact corpus bytes so later claim edits are ungated.
    use adapters_plugin_host::gate::{
        DeclaredCell, DeclaredSupport, ExpectedNode, GoldenCase, GoldenCorpus, gate_key, run_gate,
    };
    let host = PluginHost::new().expect("engine");
    let cell = |support, note: &str, cases: &[&str]| DeclaredCell {
        language: "Rust".to_string(),
        framework: "core".to_string(),
        capability: "types".to_string(),
        support,
        note: note.to_string(),
        cases: cases.iter().map(|c| c.to_string()).collect(),
    };
    let corpus = |coverage| GoldenCorpus {
        extensions: vec!["rs".to_string()],
        cases: vec![GoldenCase {
            path: "src/lib.rs".to_string(),
            source: "hello world".to_string(),
            nodes: vec![ExpectedNode {
                id: "owner/repo:src/lib.rs".to_string(),
                label: "TestNode".to_string(),
                props: serde_json::json!({ "len": 11 }),
            }],
            edges: vec![adapters_plugin_host::gate::ExpectedEdge {
                src: "owner/repo:src/lib.rs".to_string(),
                dst: "owner/repo:src/lib.rs".to_string(),
                label: "SELF".to_string(),
                props: serde_json::json!({}),
            }],
        }],
        coverage,
        languages: Default::default(),
    };
    let gate = |corpus: &GoldenCorpus| {
        let report = run_gate(
            &host,
            "t0.plugin-fixture",
            OK_ADAPTER,
            corpus,
            PluginLimits::default(),
            &source_id(),
        );
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "coverage-declarations")
            .expect("coverage check runs")
            .clone();
        (report.passed, check.detail)
    };

    let proven = corpus(vec![
        cell(DeclaredSupport::Supported, "", &["src/lib.rs"]),
        DeclaredCell {
            capability: "http_endpoints".to_string(),
            ..cell(DeclaredSupport::Unsupported, "no routing yet", &[])
        },
    ]);
    assert_eq!(gate(&proven), (true, "2 declared cell(s)".to_string()));

    for (bad, why) in [
        (
            cell(DeclaredSupport::Supported, "", &[]),
            "names no golden case",
        ),
        (
            cell(DeclaredSupport::Supported, "", &["src/missing.rs"]),
            "names unknown case",
        ),
        (
            cell(DeclaredSupport::Supported, "misses macros", &["src/lib.rs"]),
            "must be partial",
        ),
        (
            cell(DeclaredSupport::Partial, "", &["src/lib.rs"]),
            "needs a note",
        ),
        (
            cell(DeclaredSupport::Unsupported, "none", &["src/lib.rs"]),
            "names no cases",
        ),
        (
            DeclaredCell {
                capability: "Types".to_string(),
                ..cell(DeclaredSupport::Supported, "", &["src/lib.rs"])
            },
            "snake_case",
        ),
    ] {
        let (passed, detail) = gate(&corpus(vec![bad]));
        assert!(!passed && detail.contains(why), "{why}: {detail}");
    }
    // A negative (empty-output) case cannot on its own prove a claim, but
    // may accompany a case that asserts facts.
    let with_negative = |coverage| {
        let mut corpus = corpus(coverage);
        corpus.cases.push(GoldenCase {
            path: "src/empty.rs".to_string(),
            source: String::new(),
            nodes: vec![],
            edges: vec![],
        });
        corpus
    };
    let (passed, detail) = gate(&with_negative(vec![cell(
        DeclaredSupport::Supported,
        "",
        &["src/empty.rs"],
    )]));
    assert!(
        !passed && detail.contains("assert no nodes or edges"),
        "{detail}"
    );
    let (_, detail) = gate(&with_negative(vec![cell(
        DeclaredSupport::Supported,
        "",
        &["src/lib.rs", "src/empty.rs"],
    )]));
    assert_eq!(detail, "1 declared cell(s)");

    // Languages match case-insensitively downstream: `Ruby` twice is a
    // duplicate however it is spelled.
    let lower = DeclaredCell {
        language: cell(DeclaredSupport::Supported, "", &[])
            .language
            .to_lowercase(),
        ..cell(DeclaredSupport::Supported, "", &["src/lib.rs"])
    };
    let upper = DeclaredCell {
        language: lower.language.to_uppercase(),
        ..lower.clone()
    };
    let (passed, detail) = gate(&corpus(vec![lower, upper]));
    assert!(!passed && detail.contains("declared twice"), "{detail}");

    // A language name must belong to a claimed extension.
    let mut named = corpus(vec![]);
    named.languages.insert("foo".to_string(), "Foo".to_string());
    let (passed, detail) = gate(&named);
    assert!(!passed && detail.contains("languages.foo"), "{detail}");
    named.extensions.push("foo".to_string());
    assert_eq!(gate(&named).1, "0 declared cell(s)");

    let twice = cell(DeclaredSupport::Supported, "", &["src/lib.rs"]);
    let (passed, detail) = gate(&corpus(vec![twice.clone(), twice]));
    assert!(!passed && detail.contains("declared twice"), "{detail}");

    // The verdict key binds the artifact to the exact corpus bytes.
    let text = serde_json::to_vec(&proven).unwrap();
    let mut edited = proven.clone();
    edited.coverage[0].support = DeclaredSupport::Partial;
    edited.coverage[0].note = "edited after gating".to_string();
    let key = gate_key("artifact", Some(&text));
    assert_eq!(key, gate_key("artifact", Some(&text)));
    assert_ne!(
        key,
        gate_key("artifact", Some(&serde_json::to_vec(&edited).unwrap()))
    );
    assert_ne!(key, gate_key("other-artifact", Some(&text)));
    assert_ne!(gate_key("artifact", None), gate_key("artifact", Some(b"")));
}

#[test]
fn routing_extracts_only_claimed_files_and_is_deterministic() {
    // #201 (AC-0069 extension): a gated plugin covers exactly the files
    // its corpus claims, in sorted order, skipping vendored dirs; the
    // merged facts are pinned and identical across runs.
    use adapters_plugin_host::route::{claimed_files, extract_claimed, extract_claimed_counted};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
    std::fs::create_dir_all(root.join(".cartograph/adapters")).unwrap();
    std::fs::write(root.join("src/b.foo"), "beta").unwrap();
    std::fs::write(root.join("a.foo"), "alpha").unwrap();
    std::fs::write(root.join("readme.md"), "docs").unwrap();
    std::fs::write(root.join("node_modules/dep/c.foo"), "vendored").unwrap();
    // Cartograph's own plugin directory is never project source (#208
    // review): a plugin claiming this extension must not see it.
    std::fs::write(root.join(".cartograph/adapters/plugin.foo"), "artifact").unwrap();

    let claims = vec!["foo".to_string()];
    let files = claimed_files(root, &claims).expect("walk");
    let rels: Vec<String> = files
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    // Sorted, claimed extensions only, vendored dirs skipped.
    assert_eq!(rels, ["a.foo", "src/b.foo"]);

    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(OK_ADAPTER, "t0.plugin-fixture")
        .expect("well-behaved plugin loads");
    let first = extract_claimed(
        &host,
        &plugin,
        root,
        &claims,
        &source_id(),
        PluginLimits::default(),
    )
    .expect("routing succeeds");
    // One node + one SELF edge per claimed file, every fact pinned.
    assert_eq!(first.nodes.len(), 2);
    assert_eq!(first.edges.len(), 2);
    assert!(first.nodes.iter().any(|n| n.id == "owner/repo:a.foo"));
    assert!(first.nodes.iter().any(|n| n.id == "owner/repo:src/b.foo"));
    assert!(!first.nodes.iter().any(|n| n.id.contains("c.foo")));
    assert!(!first.nodes.iter().any(|n| n.id.contains("plugin.foo")));
    let hash = core_prov::content_hash(OK_ADAPTER);
    assert!(
        first
            .nodes
            .iter()
            .map(|n| &n.props)
            .chain(first.edges.iter().map(|e| &e.props))
            .all(|props| props["plugin_artifact_hash"] == serde_json::json!(hash))
    );
    // Host-filled provenance (#208 review): the fixture emits no prov, so
    // every fact carries the host's T0 baseline — pinned extractor id and
    // the mediated source file as the evidence span.
    let short = &hash[..12];
    for props in first
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(first.edges.iter().map(|e| &e.props))
    {
        let prov = props.get("prov").expect("prov filled");
        assert_eq!(prov["tier"], serde_json::json!("Deterministic"));
        assert_eq!(prov["confidence_tier"], serde_json::json!("Confirmed"));
        assert_eq!(
            prov["extractor_id"],
            serde_json::json!(format!("t0.plugin-fixture@{short}"))
        );
        assert_eq!(prov["evidence"][0]["repo"], serde_json::json!("owner/repo"));
        assert!(prov["content_hash"].is_string());
    }

    // The counted route reports every claimed file read, under the same
    // pinned identity the facts carry.
    let counted = extract_claimed_counted(
        &host,
        &plugin,
        root,
        &claims,
        &source_id(),
        PluginLimits::default(),
    )
    .expect("counted routing succeeds");
    assert_eq!(counted.files_read, 2);
    assert!(counted.refused.is_empty());
    assert_eq!(counted.facts.nodes, first.nodes);
    assert_eq!(counted.facts.edges, first.edges);
    assert_eq!(
        adapters_plugin_host::pinned_extractor_id("t0.plugin-fixture", &hash),
        format!("t0.plugin-fixture@{short}")
    );

    let second = extract_claimed(
        &host,
        &plugin,
        root,
        &claims,
        &source_id(),
        PluginLimits::default(),
    )
    .expect("routing succeeds again");
    assert_eq!(
        serde_json::to_string(&first.nodes).unwrap(),
        serde_json::to_string(&second.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&first.edges).unwrap(),
        serde_json::to_string(&second.edges).unwrap()
    );
}

/// A text-form component whose behaviour is chosen by the first byte of the
/// source (see the fixture's header), so one plugin fails on some files of a
/// tree and succeeds on the others.
const REFUSING_ADAPTER: &[u8] = include_bytes!("fixtures/refusing-adapter.wat");

/// A tree of `.foo` files, one per `(name, source)`.
fn foo_tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, source) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    dir
}

/// Route the refusing fixture over `files` under `limits`.
fn route_refusing(
    files: &[(&str, &str)],
    limits: PluginLimits,
) -> Result<adapters_plugin_host::route::ClaimedExtraction, adapters_plugin_host::route::RouteError>
{
    let dir = foo_tree(files);
    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(REFUSING_ADAPTER, "t0.plugin-refusing")
        .expect("text component loads");
    adapters_plugin_host::route::extract_claimed_counted(
        &host,
        &plugin,
        dir.path(),
        &["foo".to_string()],
        &source_id(),
        limits,
    )
}

fn refused(claimed: &adapters_plugin_host::route::ClaimedExtraction) -> Vec<(&str, RefusalReason)> {
    claimed
        .refused
        .iter()
        .map(|file| (file.path.as_str(), file.reason))
        .collect()
}

fn node_ids(claimed: &adapters_plugin_host::route::ClaimedExtraction) -> Vec<&str> {
    claimed
        .facts
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect()
}

/// Bounds a refusing-fixture call reaches quickly, with a deadline far
/// enough away that the fuel bound is always reached first.
fn tight_limits() -> PluginLimits {
    PluginLimits {
        max_fuel: 1_000_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(60),
    }
}

// AC-0658: a plugin that emits a malformed fact for one file of several is
// refused on that file only. The other files' facts are kept, no fact of the
// refused file is, and two runs give the same facts and the same refusal.
#[test]
fn routing_refuses_one_malformed_file_and_keeps_the_others() {
    let files = [
        ("a.foo", "fine"),
        ("src/b.foo", "Malformed"),
        ("src/c.foo", "fine too"),
    ];
    let first = route_refusing(&files, PluginLimits::default()).expect("the pass continues");
    assert_eq!(
        first.files_read, 2,
        "the refused file is not counted as read"
    );
    assert_eq!(
        refused(&first),
        [("src/b.foo", RefusalReason::MalformedFact)]
    );
    assert_eq!(
        node_ids(&first),
        ["owner/repo:a.foo", "owner/repo:src/c.foo"]
    );
    assert!(
        !serde_json::to_string(&first.facts.nodes)
            .unwrap()
            .contains("b.foo"),
        "no fact of the refused file"
    );

    let second = route_refusing(&files, PluginLimits::default()).expect("the pass continues");
    assert_eq!(first.refused, second.refused);
    assert_eq!(
        serde_json::to_string(&first.facts.nodes).unwrap(),
        serde_json::to_string(&second.facts.nodes).unwrap()
    );
    assert_eq!(first.facts.edges, second.facts.edges);

    // The all-or-nothing route still fails on the same file.
    let dir = foo_tree(&files);
    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(REFUSING_ADAPTER, "t0.plugin-refusing")
        .expect("text component loads");
    let error = adapters_plugin_host::route::extract_claimed(
        &host,
        &plugin,
        dir.path(),
        &["foo".to_string()],
        &source_id(),
        PluginLimits::default(),
    )
    .expect_err("all-or-nothing routing fails");
    assert!(
        matches!(
            &error,
            adapters_plugin_host::route::RouteError::Host {
                path,
                error: HostError::MalformedFact(_)
            } if path == "src/b.foo"
        ),
        "{error}"
    );
}

// AC-0658: a plugin that returns an extraction error, or traps, is refused
// on that file as a guest error.
#[test]
fn routing_refuses_a_guest_error_or_trap_as_guest_error() {
    let claimed = route_refusing(
        &[
            ("error.foo", "Error"),
            ("ok.foo", "fine"),
            ("trap.foo", "Trap"),
        ],
        PluginLimits::default(),
    )
    .expect("the pass continues");
    assert_eq!(
        refused(&claimed),
        [
            ("error.foo", RefusalReason::GuestError),
            ("trap.foo", RefusalReason::GuestError)
        ]
    );
    assert_eq!(node_ids(&claimed), ["owner/repo:ok.foo"]);
    assert_eq!(claimed.files_read, 1);
}

// AC-0658: a plugin that exceeds its fuel, memory or table bound is refused
// on that file as bound exceeded.
#[test]
fn routing_refuses_a_fuel_memory_or_table_bound_as_bound_exceeded() {
    let claimed = route_refusing(
        &[
            ("fuel.foo", "Fuel"),
            ("memory.foo", "Grow memory"),
            ("ok.foo", "fine"),
            ("table.foo", "Big table"),
        ],
        tight_limits(),
    )
    .expect("the pass continues");
    assert_eq!(
        refused(&claimed),
        [
            ("fuel.foo", RefusalReason::BoundExceeded),
            ("memory.foo", RefusalReason::BoundExceeded),
            ("table.foo", RefusalReason::BoundExceeded)
        ]
    );
    assert_eq!(node_ids(&claimed), ["owner/repo:ok.foo"]);

    // Each bound is the one the host names.
    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(REFUSING_ADAPTER, "t0.plugin-refusing")
        .expect("text component loads");
    let call = |source: &[u8]| {
        host.call_extract(&plugin, source, "x.foo", &source_id(), tight_limits())
            .expect_err("bound reached")
    };
    assert!(matches!(call(b"F"), HostError::FuelExhausted(_)));
    assert!(matches!(call(b"G"), HostError::MemoryLimitExceeded(_)));
    assert!(matches!(call(b"B"), HostError::TableLimitExceeded(_)));
}

// AC-0658: the wall-clock deadline is not a refusal. It still fails the
// whole pass with no facts, because skipping the file would make the result
// depend on the speed of the machine.
#[test]
fn routing_still_fails_the_whole_pass_on_a_deadline() {
    let error = route_refusing(
        &[("a.foo", "fine"), ("slow.foo", "Forever")],
        PluginLimits {
            max_fuel: u64::MAX,
            deadline: Duration::from_millis(60),
            ..tight_limits()
        },
    )
    .expect_err("a deadline fails the pass");
    assert!(
        matches!(
            &error,
            adapters_plugin_host::route::RouteError::Host {
                path,
                error: HostError::DeadlineExceeded(_)
            } if path == "slow.foo"
        ),
        "{error}"
    );
}

// AC-0658: a call-stack overflow is not a refusal either: the depth at
// which the stack runs out depends on the host, so it fails the whole pass.
#[test]
fn routing_still_fails_the_whole_pass_on_a_stack_overflow() {
    let error = route_refusing(
        &[("a.foo", "fine"), ("deep.foo", "Stack")],
        PluginLimits {
            max_fuel: u64::MAX,
            ..tight_limits()
        },
    )
    .expect_err("a stack overflow fails the pass");
    assert!(
        matches!(
            &error,
            adapters_plugin_host::route::RouteError::Host {
                path,
                error: HostError::StackOverflow
            } if path == "deep.foo"
        ),
        "{error}"
    );
}

const PROV_SOURCE: &[u8] = b"hello world";
const PROV_PATH: &str = "src/lib.rs";

/// One node and one edge carrying `prov` as a guest would have written
/// it, run through the same host step `call_extract` applies to every
/// guest extraction.
fn enforce_guest_prov(
    prov: serde_json::Value,
) -> Result<adapters_plugin_host::PluginExtraction, HostError> {
    let mut extraction = adapters_plugin_host::PluginExtraction {
        nodes: vec![
            core_graph::Node {
                id: "n1".into(),
                label: "Symbol".into(),
                props: serde_json::json!({ "name": "hello", "prov": prov }),
            },
            core_graph::Node {
                id: "n2".into(),
                label: "Symbol".into(),
                props: serde_json::json!({ "name": "world" }),
            },
        ],
        edges: Vec::new(),
    };
    adapters_plugin_host::enforce_provenance(
        &mut extraction,
        "t0.plugin-fixture",
        &core_prov::content_hash(OK_ADAPTER),
        PROV_SOURCE,
        PROV_PATH,
        &source_id(),
    )
    .map(|()| extraction)
}

fn guest_span(path: &str, byte_start: u64, byte_end: u64) -> serde_json::Value {
    serde_json::json!({
        "repo": "owner/repo",
        "path": path,
        "byte_start": byte_start,
        "byte_end": byte_end,
        "commit_sha": "deadbeef",
    })
}

fn guest_prov(tier: &str, confidence: &str, evidence: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "tier": tier,
        "confidence_tier": confidence,
        "evidence": [evidence],
        "extractor_id": "t0.adapter-ts",
        "content_hash": "f".repeat(64),
    })
}

fn assert_malformed(result: Result<adapters_plugin_host::PluginExtraction, HostError>, why: &str) {
    match result {
        Err(HostError::MalformedFact(message)) => assert!(
            message.starts_with("node n1: ") && message.contains(why),
            "expected the rejection to name the fact and {why:?}, got {message:?}"
        ),
        other => panic!("expected MalformedFact ({why}), got {other:?}"),
    }
}

/// T-0594 (AC-0594, #1756): a guest that writes T1 provenance, or any tier
/// or confidence other than T0 Deterministic/Confirmed, is rejected with
/// `MalformedFact`. The extraction is refused whole, so the well-formed
/// fact beside it does not get through either.
#[test]
fn guest_written_t1_provenance_is_rejected() {
    let span = || guest_span(PROV_PATH, 0, 5);
    assert_malformed(
        enforce_guest_prov(guest_prov("Dynamic", "Confirmed", span())),
        "prov.tier is Dynamic",
    );
    for tier in ["Semantic", "Agentic"] {
        assert_malformed(
            enforce_guest_prov(guest_prov(tier, "Confirmed", span())),
            "prov.tier",
        );
    }
    for confidence in ["InferredStrong", "InferredWeak", "Gap"] {
        assert_malformed(
            enforce_guest_prov(guest_prov("Deterministic", confidence, span())),
            "prov.confidence_tier",
        );
    }
    // A prov object that leaves the tier out does not default to T0.
    assert_malformed(
        enforce_guest_prov(serde_json::json!({
            "confidence_tier": "Confirmed",
            "evidence": [span()],
        })),
        "prov.tier is missing",
    );
    // Claims the host cannot check are refused, not carried along.
    let mut observed = guest_prov("Deterministic", "Confirmed", span());
    observed["basis"] = serde_json::json!({ "kind": "observation" });
    assert_malformed(enforce_guest_prov(observed), "prov.basis");
    let mut human = guest_prov("Deterministic", "Confirmed", span());
    human["method"] = serde_json::json!({ "kind": "human", "actor": "owner" });
    assert_malformed(enforce_guest_prov(human), "prov.method");
    assert_malformed(
        enforce_guest_prov(serde_json::json!("Confirmed")),
        "prov is not a JSON object",
    );
}

/// T-0594 (AC-0594, #1756): a guest that cites bytes outside the source it
/// was handed is rejected with `MalformedFact`.
#[test]
fn guest_evidence_span_outside_the_source_is_rejected() {
    let len = PROV_SOURCE.len() as u64;
    let reject = |start, end| {
        assert_malformed(
            enforce_guest_prov(guest_prov(
                "Deterministic",
                "Confirmed",
                guest_span(PROV_PATH, start, end),
            )),
            "outside the 11-byte source",
        );
    };
    reject(0, len + 1);
    reject(len + 5, len + 9);
    reject(7, 3);
    reject(0, u64::MAX);
    // One good reference does not excuse a bad one beside it.
    let mut two = guest_prov("Deterministic", "Confirmed", guest_span(PROV_PATH, 0, 5));
    two["evidence"]
        .as_array_mut()
        .unwrap()
        .push(guest_span(PROV_PATH, 6, len + 1));
    assert_malformed(enforce_guest_prov(two), "prov.evidence[1]");
    // No evidence at all cannot be attributed to the mediated source.
    let mut none = guest_prov("Deterministic", "Confirmed", guest_span(PROV_PATH, 0, 5));
    none["evidence"] = serde_json::json!([]);
    assert_malformed(enforce_guest_prov(none), "prov.evidence is empty");
    // The whole file, and an empty span at its end, are inside the source.
    for (start, end) in [(0, len), (len, len)] {
        enforce_guest_prov(guest_prov(
            "Deterministic",
            "Confirmed",
            guest_span(PROV_PATH, start, end),
        ))
        .expect("an in-range span is accepted");
    }
}

/// T-0594 (AC-0594, #1756): a guest that cites another file, repository or
/// commit than the mediated source is rejected with `MalformedFact`.
#[test]
fn guest_evidence_citing_another_file_is_rejected() {
    for other in [
        "src/other.rs",
        "../secrets.env",
        "/etc/passwd",
        "SRC/lib.rs",
    ] {
        assert_malformed(
            enforce_guest_prov(guest_prov(
                "Deterministic",
                "Confirmed",
                guest_span(other, 0, 5),
            )),
            "not the mediated file",
        );
    }
    let mut other_repo = guest_span(PROV_PATH, 0, 5);
    other_repo["repo"] = serde_json::json!("owner/other");
    assert_malformed(
        enforce_guest_prov(guest_prov("Deterministic", "Confirmed", other_repo)),
        "cites repo",
    );
    let mut other_commit = guest_span(PROV_PATH, 0, 5);
    other_commit["commit_sha"] = serde_json::json!("cafef00d");
    assert_malformed(
        enforce_guest_prov(guest_prov("Deterministic", "Confirmed", other_commit)),
        "cites commit",
    );
}

/// T-0594 (AC-0594, #1756): a guest's valid T0 provenance keeps its tighter
/// evidence span, while the extractor id and content hash are the host's:
/// the hash is the one the host computes for the same fact without guest
/// provenance, whatever the guest wrote.
#[test]
fn host_computes_the_content_hash_and_keeps_valid_guest_spans() {
    let accepted = enforce_guest_prov(guest_prov(
        "Deterministic",
        "Confirmed",
        guest_span(PROV_PATH, 0, 5),
    ))
    .expect("valid T0 guest provenance is accepted");
    let artifact_hash = core_prov::content_hash(OK_ADAPTER);
    let prov: core_prov::Provenance =
        serde_json::from_value(accepted.nodes[0].props["prov"].clone()).expect("host provenance");
    assert_eq!(prov.tier, core_prov::Tier::Deterministic);
    assert_eq!(prov.confidence_tier, core_prov::ConfidenceTier::Confirmed);
    assert_eq!(
        prov.extractor_id,
        adapters_plugin_host::pinned_extractor_id("t0.plugin-fixture", &artifact_hash)
    );
    assert_eq!(prov.evidence.len(), 1);
    assert_eq!(
        (prov.evidence[0].byte_start, prov.evidence[0].byte_end),
        (0, 5)
    );
    assert_eq!(prov.evidence[0].path, PROV_PATH);

    assert_ne!(prov.content_hash, "f".repeat(64));
    let canonical_props = serde_json::json!({
        "name": "hello",
        "plugin_artifact_hash": artifact_hash,
    });
    assert_eq!(
        prov.content_hash,
        core_prov::content_hash(format!("n1:Symbol:{canonical_props}").as_bytes())
    );
    // The same fact without guest provenance gets the same hash, and the
    // whole mediated file as its span.
    let bare = enforce_guest_prov(serde_json::Value::Null).expect("absent prov is host-filled");
    let filled: core_prov::Provenance =
        serde_json::from_value(bare.nodes[0].props["prov"].clone()).expect("host provenance");
    assert_eq!(filled.content_hash, prov.content_hash);
    assert_eq!(
        (filled.evidence[0].byte_start, filled.evidence[0].byte_end),
        (0, PROV_SOURCE.len() as u64)
    );
}

// AC-1069 (T-1069, #385): over a source capture a plugin is routed the
// captured bytes of exactly the captured files it claims, in sorted order,
// skipping vendored directories and other extensions: a claimed file edited
// to one the plugin refuses, deleted or created in the checkout afterwards
// changes no fact and no refusal.
#[test]
fn captured_claimed_files_are_routed_instead_of_the_checkout() {
    use adapters_plugin_host::route::{claims, extract_captured_counted};

    let claims_foo = vec!["foo".to_string()];
    assert!(claims("src/b.foo", &claims_foo));
    assert!(!claims("node_modules/dep/c.foo", &claims_foo));
    assert!(!claims(".cartograph/adapters/plugin.foo", &claims_foo));
    assert!(!claims("readme.md", &claims_foo));

    let dir = foo_tree(&[("a.foo", "fine"), ("src/b.foo", "fine too")]);
    let routed = |claimed: &adapters_plugin_host::route::ClaimedExtraction| {
        (
            node_ids(claimed)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            refused(claimed)
                .into_iter()
                .map(|(path, _)| path.to_string())
                .collect::<Vec<_>>(),
        )
    };
    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(REFUSING_ADAPTER, "t0.plugin-refusing")
        .expect("text component loads");
    let before = adapters_plugin_host::route::extract_claimed_counted(
        &host,
        &plugin,
        dir.path(),
        &claims_foo,
        &source_id(),
        PluginLimits::default(),
    )
    .expect("checkout routing succeeds");
    let captured: std::collections::BTreeMap<String, Vec<u8>> = [
        ("src/b.foo", "fine too"),
        ("a.foo", "fine"),
        ("readme.md", "Malformed"),
        ("node_modules/dep/c.foo", "Malformed"),
    ]
    .into_iter()
    .map(|(rel, source)| (rel.to_string(), source.as_bytes().to_vec()))
    .collect();

    // After capture: a refused edit, a deletion and a new claimed file.
    std::fs::write(dir.path().join("a.foo"), "Malformed").unwrap();
    std::fs::remove_file(dir.path().join("src/b.foo")).unwrap();
    std::fs::write(dir.path().join("c.foo"), "fine").unwrap();
    let paths: Vec<String> = captured.keys().cloned().collect();
    let read = |rel: &str| {
        captured
            .get(rel)
            .cloned()
            .ok_or_else(|| std::io::Error::other("not captured"))
    };
    let after = extract_captured_counted(
        &host,
        &plugin,
        &paths,
        &read,
        &claims_foo,
        &source_id(),
        PluginLimits::default(),
    )
    .expect("captured routing succeeds");
    assert_eq!(after.files_read, 2);
    assert_eq!(routed(&after), routed(&before));
    assert_eq!(after.facts.nodes, before.facts.nodes);
    let live = adapters_plugin_host::route::extract_claimed_counted(
        &host,
        &plugin,
        dir.path(),
        &claims_foo,
        &source_id(),
        PluginLimits::default(),
    )
    .expect("checkout routing succeeds");
    assert_ne!(routed(&live), routed(&before));
}
