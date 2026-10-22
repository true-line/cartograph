use super::*;
use core_graph::source::FactKey;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};
use std::fs;

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";

fn parser_id() -> SourceId<'static> {
    SourceId {
        repo: REPO,
        commit: "workdir",
    }
}

fn capture(root: &Path, paths: &[String]) -> Capture {
    capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        paths,
        CaptureLimits::default(),
    )
    .unwrap()
}

fn fixture(source: &[u8], path: &str) -> (tempfile::TempDir, Capture) {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, source).unwrap();
    let captured = capture(directory.path(), &[path.into()]);
    (directory, captured)
}

fn rule_receipt(receipts: &[Receipt]) -> &Receipt {
    receipts
        .iter()
        .find(
            |receipt| matches!(receipt.fact_key(), FactKey::Node { id } if id.starts_with("rule:")),
        )
        .unwrap()
}

#[test]
fn captured_parser_uses_original_bytes_after_target_mutation() {
    // AC-0148: capture is the actual producer input, not a later file lookup.
    let source = b"function ready(enabled: boolean) { if (enabled) return false; }\n// retained-only-canary\n";
    let (directory, capture) = fixture(source, "source.ts");
    fs::write(
        directory.path().join("source.ts"),
        b"function changed() { return true; }",
    )
    .unwrap();
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let ordinary = crate::extract_source(source, "source.ts", &parser_id()).unwrap();
    assert_eq!(facts.nodes, ordinary.nodes);
    assert_eq!(facts.edges, ordinary.edges);
    assert_eq!(receipts.len(), 3, "rule, actual owner, and GOVERNS only");
    fs::remove_file(directory.path().join("source.ts")).unwrap();
    for receipt in &receipts {
        assert_eq!(receipt.source_id().as_str(), SOURCE);
        assert_eq!(receipt.repo_key(), REPO);
        for range in receipt.ranges() {
            assert_eq!(
                capture.read_span(&range.captured).unwrap(),
                &source[range.evidence.byte_start as usize..range.evidence.byte_end as usize]
            );
        }
        let wire = receipt.to_json().unwrap();
        assert!(!wire.contains("retained-only-canary"));
        assert!(!format!("{receipt:?}").contains("retained-only-canary"));
    }
    assert!(
        facts
            .nodes
            .iter()
            .all(|node| node.props.get("receipt_id").is_none())
    );
    let rule = rule_receipt(&receipts);
    assert!(
        capture
            .read_text_span(&rule.ranges()[0].captured)
            .unwrap()
            .contains("return false")
    );
}

#[test]
fn equal_complete_facts_keep_distinct_raw_capture_receipts() {
    // AC-0149: even full fact equality cannot select the producing raw input.
    let before = b"function stable(ready: boolean) { if (ready) return false; }\n// comment-A\n";
    let after = b"function stable(ready: boolean) { if (ready) return false; }\n// comment-B\n";
    assert_eq!(before.len(), after.len());
    let (directory, first) = fixture(before, "source.ts");
    fs::write(directory.path().join("source.ts"), after).unwrap();
    let second = capture(directory.path(), &["source.ts".into()]);
    let (first_facts, first_receipts) =
        extract_file(first.file("source.ts").unwrap(), &parser_id()).unwrap();
    let (second_facts, second_receipts) =
        extract_file(second.file("source.ts").unwrap(), &parser_id()).unwrap();
    assert_eq!(first_facts.nodes, second_facts.nodes);
    assert_eq!(first_facts.edges, second_facts.edges);
    assert_ne!(first.id(), second.id());
    for (a, b) in first_receipts.iter().zip(&second_receipts) {
        assert_eq!(a.fact_key(), b.fact_key());
        assert_eq!(a.fact_digest(), b.fact_digest());
        assert_ne!(a.id(), b.id());
        assert_ne!(a.file().digest, b.file().digest);
    }
}

#[test]
fn captured_rule_inventory_binds_every_nested_source_and_redaction() {
    // AC-0149: duplicate and nested citations retain their structural roles;
    // literals and source text never enter the metadata-only receipt.
    let source = br#"function checkout(enabled: boolean, quantity: number) {
  const minimum = 1;
  if (enabled) {
    if (quantity < minimum) return {password: "tiny-secret-canary"};
    throw new Error("business outcome");
  }
}"#;
    let (_, capture) = fixture(source, "source.ts");
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let mut all_roles = std::collections::BTreeSet::new();
    for receipt in &receipts {
        for range in receipt.ranges() {
            all_roles.insert(range.role);
            assert_eq!(range.captured.file, *receipt.file());
            assert_eq!(
                capture.read_span(&range.captured).unwrap(),
                &source[range.evidence.byte_start as usize..range.evidence.byte_end as usize]
            );
        }
        if let FactKey::Node { id } = receipt.fact_key() {
            let node = facts.nodes.iter().find(|node| &node.id == id).unwrap();
            if let Some(value) = node.props.get("rule") {
                let mut rule =
                    core_graph::rules::GuardedExitEvidence::from_value(value.clone()).unwrap();
                let mut nested = Vec::new();
                rule.visit_sources_mut(|source| nested.push(source.clone()));
                let retained: Vec<_> = receipt
                    .ranges()
                    .iter()
                    .filter(|range| range.role != RangeRole::Provenance)
                    .map(|range| range.evidence.clone())
                    .collect();
                assert_eq!(retained, nested);
            }
        }
        let wire = receipt.to_json().unwrap();
        assert!(!wire.contains("tiny-secret-canary"));
        assert!(!wire.contains("business outcome"));
        assert!(wire.contains("primary_source_only"));
        assert!(wire.contains("input_closure_not_established"));
        assert_eq!(Receipt::from_json(&wire).unwrap(), *receipt);
    }
    for role in [
        RangeRole::ConditionBranch,
        RangeRole::ConditionExpression,
        RangeRole::ReturnValue,
        RangeRole::ThrowValue,
        RangeRole::DependencyDeclaration,
        RangeRole::Redaction,
    ] {
        assert!(all_roles.contains(&role), "missing inventory role {role:?}");
    }
}

#[test]
fn captured_directory_preserves_completion_but_excludes_eval_and_derived_facts() {
    // AC-0148: the common directory pass is reused; participation is granted
    // only by direct emission, never by matching synthetic or derived metadata.
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("pages")).unwrap();
    fs::write(
        directory.path().join("pages/index.tsx"),
        br#"export default function Page(ready: boolean) {
  if (ready) return null;
  eval("function synthetic(ok) { if (ok) return false; }");
  return <div/>;
}"#,
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let ordinary = crate::extract_dir(directory.path(), &parser_id()).unwrap();
    let mut visited = Vec::new();
    let (captured, receipts, stats) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |path| visited.push(path.to_string()),
    )
    .unwrap();
    assert_eq!(ordinary.nodes, captured.nodes);
    assert_eq!(ordinary.edges, captured.edges);
    assert_eq!(visited, paths);
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 0);
    assert!(
        captured
            .nodes
            .iter()
            .any(|node| node.props["via"] == "eval")
    );
    assert!(captured.nodes.iter().any(|node| node.label == "Screen"));
    assert_eq!(receipts.len(), 3);
    for receipt in &receipts {
        match receipt.fact_key() {
            FactKey::Node { id } => {
                let node = captured.nodes.iter().find(|node| &node.id == id).unwrap();
                assert!(node.props.get("via").is_none());
                assert!(matches!(node.label.as_str(), "BusinessRule" | "Component"));
            }
            FactKey::Edge { label, .. } => assert_eq!(label, "GOVERNS"),
        }
    }
    // The retained primary files are parsed on every captured invocation.
    fs::write(
        directory.path().join("pages/index.tsx"),
        b"export default 7;",
    )
    .unwrap();
    let (again, again_receipts, stats) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(captured.nodes, again.nodes);
    assert_eq!(captured.edges, again.edges);
    assert_eq!(receipts, again_receipts);
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 0);
}

#[test]
fn receipt_grammar_and_complete_fact_changes_are_explicit() {
    // AC-0149: package/grammar and every property/provenance field are bound;
    // later enrichment loses the receipt instead of relabeling it in place.
    for (path, grammar) in [
        ("source.ts", Grammar::TypeScript),
        ("source.js", Grammar::Tsx),
        ("source.tsx", Grammar::Tsx),
    ] {
        let (_, capture) = fixture(
            b"function ready(ok: boolean) { if (ok) return false; }",
            path,
        );
        let (mut facts, mut receipts) =
            extract_file(capture.file(path).unwrap(), &parser_id()).unwrap();
        assert!(receipts.iter().all(|receipt| receipt.grammar() == grammar));
        let original = rule_receipt(&receipts).clone();
        let node = facts
            .nodes
            .iter_mut()
            .find(|node| original.fact_key() == &FactKey::from_node(node))
            .unwrap();
        assert!(original.matches_node(node));
        node.props["extra_enrichment"] = serde_json::json!(true);
        assert!(!original.matches_node(node));
        retain_matching(&mut receipts, &facts);
        assert!(!receipts.iter().any(|receipt| receipt.id() == original.id()));
        let mut changed_revision = facts
            .nodes
            .iter()
            .find(|node| node.label == "Symbol")
            .unwrap()
            .clone();
        let owner = receipts
            .iter()
            .find(|receipt| receipt.fact_key() == &FactKey::from_node(&changed_revision))
            .unwrap();
        changed_revision.props["prov"]["evidence"][0]["commit_sha"] =
            serde_json::json!("new-revision");
        assert!(!owner.matches_node(&changed_revision));
    }
}

#[test]
fn live_config_changes_remain_outside_primary_receipt_closure() {
    // AC-0148/0149: directory configuration is intentionally not frozen by a
    // primary-file receipt; changed resolution must not gain that attestation.
    let directory = tempfile::tempdir().unwrap();
    for name in ["one.ts", "two.ts"] {
        fs::write(
            directory.path().join(name),
            b"export function target() {}\n",
        )
        .unwrap();
    }
    fs::write(directory.path().join("main.ts"), b"import { target } from '@lib';\nexport function run(ok: boolean) { if (ok) return false; return target(); }\n").unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let run = |target: &str| {
        fs::write(
            directory.path().join("tsconfig.json"),
            serde_json::json!({"compilerOptions":{"baseUrl":".","paths":{"@lib":[target]}}})
                .to_string(),
        )
        .unwrap();
        extract_captured_dir(
            directory.path(),
            &parser_id(),
            &capture,
            Registries::builtin(),
            &mut |_| {},
        )
        .unwrap()
    };
    let (first, first_receipts, _) = run("one.ts");
    let (second, second_receipts, _) = run("two.ts");
    let first_import = first
        .edges
        .iter()
        .find(|edge| edge.label == "IMPORTS" && edge.src.ends_with("@main.ts"))
        .unwrap();
    let second_import = second
        .edges
        .iter()
        .find(|edge| edge.label == "IMPORTS" && edge.src.ends_with("@main.ts"))
        .unwrap();
    assert!(first_import.dst.ends_with("@one.ts"));
    assert!(second_import.dst.ends_with("@two.ts"));
    assert_eq!(first_receipts, second_receipts);
    assert!(
        !first_receipts
            .iter()
            .any(|receipt| receipt.fact_key() == &FactKey::from_edge(first_import))
    );
    // Outside the rule file, only provenance-only declaration receipts for the
    // directly emitted callables exist (#691); configuration selects neither.
    assert!(first_receipts.iter().all(|receipt| {
        receipt.file().path == "main.ts"
            || receipt
                .ranges()
                .iter()
                .all(|range| range.role == RangeRole::Provenance)
    }));
}

#[test]
fn captured_receipts_reject_tampering_versions_and_bounds() {
    // AC-0149: unsupported contracts and unbounded/malformed records fail with
    // fixed diagnostics; public decoding is integrity validation, not trust.
    let (_, capture) = fixture(
        b"function ready(ok: boolean) { if (ok) return false; }",
        "source.ts",
    );
    let (_, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let receipt = rule_receipt(&receipts);
    let value = serde_json::to_value(receipt).unwrap();
    for pointer in [
        "/content/schema_version",
        "/content/file/digest",
        "/content/repo_key",
        "/content/grammar_package",
        "/content/ranges/0/evidence/byte_end",
        "/receipt_id",
    ] {
        let mut tampered = value.clone();
        *tampered.pointer_mut(pointer).unwrap() =
            serde_json::json!("untrusted-source-value-canary");
        let message = Receipt::from_json(&tampered.to_string())
            .unwrap_err()
            .to_string();
        assert!(!message.contains("untrusted-source-value-canary"));
        assert!(serde_json::from_value::<Receipt>(tampered).is_err());
    }
    let mut extra = value.clone();
    extra["unexpected"] = serde_json::json!(true);
    assert!(Receipt::from_json(&extra.to_string()).is_err());
    let mut nested_extra = value.clone();
    nested_extra["content"]["ranges"][0]["evidence"]["source-canary"] =
        serde_json::json!("raw-source-canary");
    assert!(
        !serde_json::from_value::<Receipt>(nested_extra.clone())
            .unwrap_err()
            .to_string()
            .contains("source-canary")
    );
    assert!(Receipt::from_json(&nested_extra.to_string()).is_err());
    assert!(Receipt::from_json(&" ".repeat(MAX_RECEIPT_BYTES + 1)).is_err());
    let wrong_source = SourceId {
        repo: "local/src_22222222222222222222222222222222",
        commit: "workdir",
    };
    assert!(extract_file(capture.file("source.ts").unwrap(), &wrong_source).is_err());
}

#[test]
fn oversized_primary_ranges_remain_without_truncated_receipts() {
    // AC-0148/0149: a huge owner span remains an ordinary fact, without a
    // shortened citation or receipt. Its bounded local exit still participates.
    let source = format!(
        "function large(ok: boolean) {{ /*{}*/ if (ok) return false; }}",
        "x".repeat(source_capture::MAX_SPAN_BYTES as usize)
    );
    let (_, capture) = fixture(source.as_bytes(), "source.ts");
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let owner = facts
        .nodes
        .iter()
        .find(|node| node.label == "Symbol")
        .unwrap();
    assert!(
        !receipts
            .iter()
            .any(|receipt| receipt.fact_key() == &FactKey::from_node(owner))
    );
    assert!(receipts.iter().any(
        |receipt| matches!(receipt.fact_key(), FactKey::Edge { label, .. } if label == "GOVERNS")
    ));
    assert!(receipts.iter().all(|receipt| {
        receipt.ranges().iter().all(|range| {
            range.evidence.byte_end - range.evidence.byte_start <= source_capture::MAX_SPAN_BYTES
        })
    }));
}

#[test]
fn captured_enumeration_preserves_selection_and_enforces_budgets() {
    // AC-0148: all visited entries count toward the walk budget; supported
    // primary membership is exact, sorted, bounded, and never silently partial.
    let directory = tempfile::tempdir().unwrap();
    for path in [
        "src/z.js",
        "src/a.ts",
        "src/c.tsx",
        "types.d.ts",
        "notes.txt",
        "node_modules/vendor.ts",
        "dist/generated.ts",
        ".git/hidden.ts",
        "src/.hidden/ignored.ts",
        "src/.visible.ts",
    ] {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"").unwrap();
    }
    assert_eq!(
        enumerate_paths(directory.path()).unwrap(),
        ["src/.visible.ts", "src/a.ts", "src/c.tsx", "src/z.js"]
    );
    let root = Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority()).unwrap();
    assert!(matches!(
        enumerate_with_limits(&root, 0, 64, 8192),
        Err(CapturedError::Limit("visited entries"))
    ));
    assert!(matches!(
        enumerate_with_limits(&root, 100, 0, 8192),
        Err(CapturedError::Limit("directory depth"))
    ));
    assert!(matches!(
        enumerate_with_limits(&root, 100, 64, 1),
        Err(CapturedError::Limit("selected files"))
    ));
}

#[test]
fn captured_and_ordinary_walks_select_the_same_gitignore_filtered_files() {
    // AC-0205: a `.gitignore`d build tree is excluded from both lanes, a
    // non-ignored file stays, and the two selections are identical.
    let directory = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "build/\nout/\n*.gen.ts\n"),
        ("src/app.ts", "export function app() { return 1; }"),
        ("src/api.gen.ts", "export function generated() {}"),
        ("build/app.js", "export function bundled() {}"),
        ("out/app.js", "export function emitted() {}"),
        ("packages/web/.gitignore", "vendor/\n!keep.gen.ts\n"),
        ("packages/web/vendor/lib.ts", "export function lib() {}"),
        ("packages/web/keep.gen.ts", "export function kept() {}"),
    ] {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        directory.path().join("src"),
        directory.path().join("out/linked"),
    )
    .unwrap();
    let captured = enumerate_paths(directory.path()).unwrap();
    assert_eq!(captured, ["packages/web/keep.gen.ts", "src/app.ts"]);
    let mut ordinary = Vec::new();
    crate::collect_ts_files(directory.path(), &mut ordinary).unwrap();
    assert_eq!(ordinary, captured);
    let files = crate::extract_dir(directory.path(), &parser_id())
        .unwrap()
        .nodes
        .into_iter()
        .filter(|node| node.label == "File")
        .map(|node| node.id)
        .collect::<Vec<_>>();
    assert!(
        files
            .iter()
            .all(|id| !id.contains("build/") && !id.contains("vendor/"))
    );
    assert!(files.iter().any(|id| id.ends_with("src/app.ts")));
}

#[cfg(unix)]
#[test]
fn captured_enumeration_rejects_symlinks_and_non_utf8_paths() {
    // AC-0148: no-follow handles prevent traversal into a substituted directory;
    // lossy path conversion may never collapse two primary source identities.
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("hidden.ts"), b"external source").unwrap();
    symlink(outside.path(), directory.path().join("linked")).unwrap();
    assert!(enumerate_paths(directory.path()).is_err());
    fs::remove_file(directory.path().join("linked")).unwrap();
    symlink(
        outside.path().join("hidden.ts"),
        directory.path().join("source.ts"),
    )
    .unwrap();
    assert!(enumerate_paths(directory.path()).is_err());
    fs::remove_file(directory.path().join("source.ts")).unwrap();
    // macOS rejects creation of this filename with EILSEQ. Linux permits the
    // raw directory entry, so it exercises the adapter's exact-UTF-8 rejection.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        fs::write(
            directory
                .path()
                .join(std::ffi::OsString::from_vec(b"invalid-\xff.ts".to_vec())),
            b"",
        )
        .unwrap();
        assert!(enumerate_paths(directory.path()).is_err());
    }
    assert_eq!(
        fs::read(outside.path().join("hidden.ts")).unwrap(),
        b"external source"
    );
}

#[test]
fn captured_extraction_is_byte_identical_to_serial() {
    // AC-0217/T-0217 (#482): captured-file parsing runs on the same ordered
    // worker pool as the uncaptured lane (#236) — serial and many-worker runs
    // over the same capture yield byte-identical facts and receipts, and
    // progress still fires in manifest (sorted walk) order.
    let directory = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..24 {
        let next = (i + 1) % 24;
        let rel = format!("src/m{i:02}.ts");
        let file = directory.path().join(&rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            format!(
                "import {{ f{next} }} from './m{next:02}';\n\
                 export function f{i}(enabled: boolean) {{ if (enabled) return false; return f{next}(); }}\n"
            ),
        )
        .unwrap();
        paths.push(rel);
    }
    paths.sort();
    let capture = capture(directory.path(), &paths);
    let run = |workers: usize| {
        source_walk::parallel::with_workers(workers, || {
            let mut visited = Vec::new();
            let (extraction, receipts, stats) = extract_captured_dir(
                directory.path(),
                &parser_id(),
                &capture,
                Registries::builtin(),
                &mut |path| visited.push(path.to_string()),
            )
            .unwrap();
            (
                extraction.nodes,
                extraction.edges,
                receipts,
                stats.recomputed_files,
                visited,
            )
        })
    };
    let serial = run(1);
    assert_eq!(serial.3, 24, "every fixture file is parsed");
    assert_eq!(serial.4, paths, "progress fires in manifest order");
    for workers in [2, 8] {
        let parallel = run(workers);
        assert_eq!(parallel.0, serial.0, "nodes differ with {workers} workers");
        assert_eq!(parallel.1, serial.1, "edges differ with {workers} workers");
        assert_eq!(
            parallel.2, serial.2,
            "receipts differ with {workers} workers"
        );
        assert_eq!(parallel.3, serial.3);
        assert_eq!(
            parallel.4, serial.4,
            "progress order differs with {workers} workers"
        );
    }
}

#[test]
fn captured_parser_emits_the_same_const_unproven_eval_gap() {
    // AC-0201: the captured production lane parses the retained bytes with the
    // same extractor, so the explicit eval Gap and its owner edge are emitted
    // exactly as the ordinary path emits them.
    let source = b"const CODE = load();\nexport function run() {\n  eval(CODE);\n}\n";
    let (_directory, capture) = fixture(source, "src/boot.ts");
    let (facts, _) = extract_file(capture.file("src/boot.ts").unwrap(), &parser_id()).unwrap();
    let ordinary = crate::extract_source(source, "src/boot.ts", &parser_id()).unwrap();
    assert_eq!(facts.nodes, ordinary.nodes);
    assert_eq!(facts.edges, ordinary.edges);
    let gap = facts
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.id.contains("#eval-unproven@"))
        .expect("captured extraction carries the eval Gap");
    assert!(facts.edges.iter().any(|edge| edge.dst == gap.id
        && edge.label == "DEPENDS_ON"
        && edge.src.ends_with("#run")));
}

fn by_fact(mut receipts: Vec<Receipt>) -> Vec<Receipt> {
    receipts.sort_by(|a, b| a.fact_key().cmp(b.fact_key()));
    receipts
}

// AC-0280: every directly emitted callable becomes citable through a
// provenance-only receipt, beside the unchanged rule inventory, and the
// single-file and directory lanes agree on the complete receipt set.
#[test]
fn captured_declarations_bind_every_direct_callable() {
    let source = br#"export function guard(ok: boolean) { if (!ok) return false; return true; }
export function total(a: number, b: number) { return a + b; }
export class Stock {
  level(id: string) { return lookup(id); }
}
export const pick = (items: string[]) => items[0];
"#;
    let (directory, capture) = fixture(source, "source.ts");
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let callables: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .collect();
    assert!(callables.len() >= 4, "guard, total, level and pick");
    for node in &callables {
        let receipt = receipts
            .iter()
            .find(|receipt| receipt.fact_key() == &FactKey::from_node(node))
            .unwrap_or_else(|| panic!("{} is not citable", node.id));
        assert!(receipt.matches_node(node));
        assert_eq!(receipt.ranges().len(), 1);
        assert_eq!(receipt.ranges()[0].role, RangeRole::Provenance);
        let evidence = &receipt.ranges()[0].evidence;
        assert_eq!(
            (evidence.byte_start, evidence.byte_end),
            (
                receipt.ranges()[0].captured.byte_start,
                receipt.ranges()[0].captured.byte_end
            )
        );
    }
    rule_receipt(&receipts);
    let (_, directory_receipts, stats) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(by_fact(directory_receipts), receipts);
    assert!(stats.omitted_declaration_receipts.is_empty());
}

// AC-0280: declarations use only the budget left after every rule receipt, in
// manifest order; a declaration that loses is named by file, and so is a
// rule receipt that loses its own budget (#722).
#[test]
fn captured_declarations_yield_budget_to_rules_and_name_omissions() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("a.ts"),
        b"export function guard(ok: boolean) { if (!ok) return false; return true; }\nexport function one() { return 1; }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("b.ts"),
        b"export function two() { return 2; }\nexport function three() { return 3; }\n",
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let per_file: Vec<_> = paths
        .iter()
        .map(|path| {
            extract_file_with(
                capture.file(path).unwrap(),
                &parser_id(),
                Registries::builtin(),
                FILE_DECLARATIONS,
            )
            .unwrap()
            .1
        })
        .collect();
    let size = |receipt: &Receipt| receipt.to_json().unwrap().len();
    let rules: Vec<_> = per_file.iter().flat_map(|file| &file.rules).collect();
    let rule_bytes: usize = rules.iter().map(|receipt| size(receipt)).sum();
    let first = &per_file[0].declarations[0].0;
    assert_eq!(per_file[0].declarations.len(), 1);
    assert_eq!(per_file[1].declarations.len(), 2);
    let run = |bytes, receipts| {
        extract_captured_dir_bounded(
            directory.path(),
            &parser_id(),
            &capture,
            Registries::builtin(),
            &mut |_| {},
            ReceiptBudget { bytes, receipts },
            FILE_DECLARATIONS,
        )
    };
    // Room for the rules and exactly the first declaration in manifest order.
    let (_, admitted, stats) = run(rule_bytes + size(first), usize::MAX).unwrap();
    assert_eq!(admitted.len(), rules.len() + 1);
    assert!(admitted.contains(first));
    assert_eq!(
        stats.omitted_declaration_receipts,
        BTreeMap::from([("b.ts".to_string(), 2)])
    );
    // A receipt-count ceiling equal to the rules admits no declaration.
    let (_, admitted, stats) = run(usize::MAX, rules.len()).unwrap();
    assert_eq!(admitted.len(), rules.len());
    assert_eq!(
        stats.omitted_declaration_receipts,
        BTreeMap::from([("a.ts".to_string(), 1), ("b.ts".to_string(), 2)])
    );
    // AC-0292: a rule receipt that does not fit is omitted and named by file
    // (#722); the extraction succeeds, keeps the fact, and admits no
    // declaration ahead of the rule that lost.
    let (extraction, admitted, stats) = run(rule_bytes - 1, usize::MAX).unwrap();
    let kept = rules.iter().filter(|rule| admitted.contains(rule)).count();
    assert_eq!(kept, rules.len() - 1);
    assert_eq!(
        stats.omitted_rule_receipts,
        BTreeMap::from([("a.ts".to_string(), 1)])
    );
    let lost = rules.iter().find(|rule| !admitted.contains(rule)).unwrap();
    assert!(surviving_facts(&extraction)(lost.fact_key()));
    // Declarations only fill what the admitted rules left; none is admitted
    // past the byte budget, and every one left out is still named.
    let admitted_bytes: usize = admitted.iter().map(size).sum();
    assert!(admitted_bytes < rule_bytes);
    assert_eq!(
        admitted.len() - kept + stats.omitted_declaration_receipts.values().sum::<u64>() as usize,
        3
    );
}

// AC-0280: the producer itself stops minting at a file's own declaration bound,
// so a file of many tiny callables cannot retain unbounded receipts before
// directory admission; unminted declarations are named for that file.
#[test]
fn captured_declarations_are_bounded_per_file_before_admission() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("many.ts"),
        b"export function one() { return 1; }\nexport function two() { return 2; }\nexport function three() { return 3; }\n",
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let file = capture.file("many.ts").unwrap();
    let produce = |bytes, receipts| {
        extract_file_with(
            file,
            &parser_id(),
            Registries::builtin(),
            ReceiptBudget { bytes, receipts },
        )
        .unwrap()
        .1
    };
    let all = produce(usize::MAX, usize::MAX);
    assert_eq!((all.declarations.len(), all.omitted_declarations), (3, 0));
    for (receipt, bytes) in &all.declarations {
        assert_eq!(*bytes, receipt.to_json().unwrap().len());
    }
    let counted = produce(usize::MAX, 1);
    assert_eq!(
        (counted.declarations.len(), counted.omitted_declarations),
        (1, 2)
    );
    let sized = produce(all.declarations[0].1 + all.declarations[1].1, usize::MAX);
    assert_eq!(
        (sized.declarations.len(), sized.omitted_declarations),
        (2, 1)
    );
    // Producer omissions reach the directory stats under the file's name.
    let (_, receipts, stats) = extract_captured_dir_bounded(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
        ReceiptBudget {
            bytes: usize::MAX,
            receipts: usize::MAX,
        },
        ReceiptBudget {
            bytes: usize::MAX,
            receipts: 1,
        },
    )
    .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        stats.omitted_declaration_receipts,
        BTreeMap::from([("many.ts".to_string(), 2)])
    );
}

// AC-0282: top-level variables, the first-level properties of their object
// initializers, member decorators and an oversized callable's header are each
// citable within one inspection range, without touching Symbol identities.
#[test]
fn captured_declaration_facts_cite_config_decorators_and_headers() {
    let body = "  options.step(1);\n".repeat(600);
    let fields: String = (0..900)
        .map(|index| format!("f{index}: {index}, "))
        .collect();
    let source = format!(
        r#"export const config: Config = {{
  catalogOptions: {{ stock: new DefaultStock() }},
  'quoted-secret': 'value',
  [computed]: 1,
  handler: () => 1,
  wrapped: (() => 2) as Handler,
  orderOptions: {{ small: 1, big: {{ {fields} }} }},
}} as const;
let counter = 0, other = 1;
var twice = 1;
var twice = 2;
export const process = configure({{}});
class Resolver {{
  @Transaction()
  @Allow(Permission.Owner)
  async transition(@Ctx() ctx: RequestContext) {{ return ctx; }}
  @Nest.Get()
  @Namespace /* private-sentinel */ .Guard()
  read() {{ return 1; }}
  @Allow() /* why */ run() {{ return 2; }}
  @First() /* between */ @Second() go() {{ return 3; }}
}}
export function configure(options: Options) {{
{body}  return options;
}}
"#
    );
    let (directory, capture) = fixture(source.as_bytes(), "config.ts");
    let (facts, receipts) = extract_file(capture.file("config.ts").unwrap(), &parser_id()).unwrap();
    let text = |node: &Node| {
        let evidence = &receipts
            .iter()
            .find(|receipt| receipt.fact_key() == &FactKey::from_node(node))
            .unwrap_or_else(|| panic!("{} is not citable", node.id))
            .ranges()[0]
            .evidence;
        source[evidence.byte_start as usize..evidence.byte_end as usize].to_owned()
    };
    let named = |label: &str, name: &str| {
        facts
            .nodes
            .iter()
            .find(|node| node.label == label && node.props["name"] == name)
            .unwrap_or_else(|| panic!("no {label} {name}"))
    };
    let catalog = named("Declaration", "config.catalogOptions");
    assert_eq!(
        catalog.id,
        format!("decl:{REPO}@config.ts#config.catalogOptions")
    );
    assert_eq!(
        text(catalog),
        "catalogOptions: { stock: new DefaultStock() }"
    );
    assert!(text(named("Declaration", "config")).starts_with("config: Config = {"));
    assert_eq!(named("Declaration", "config").props["exported"], true);
    assert_eq!(text(named("Declaration", "counter")), "counter = 0");
    assert_eq!(named("Declaration", "other").props["binding"], "let");
    assert!(text(named("Declaration", "process")).starts_with("process = configure("));
    // Quoted keys are position-named, never persisted; computed keys and
    // callable values (already Symbols) emit no declaration.
    let quoted = source.find("'quoted-secret'").unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|node| node.props["name"] == format!("config.key@{quoted}"))
    );
    assert!(
        !serde_json::to_string(&facts.nodes)
            .unwrap()
            .contains("quoted-secret")
    );
    assert!(
        !facts
            .nodes
            .iter()
            .any(|node| node.props["name"] == "config.computed")
    );
    assert!(
        !facts
            .nodes
            .iter()
            .any(|node| node.props["name"] == "config.handler")
    );
    assert!(
        !facts
            .nodes
            .iter()
            .any(|node| node.props["name"] == "config.wrapped")
    );
    // An oversized section is a flagged gap described one level deeper, until
    // each member fits one inspection range.
    let cap = crate::declarations::MAX_CITABLE_SPAN_BYTES;
    let flagged = |name: &str| {
        named("Declaration", name)
            .props
            .get("span_exceeds_range_cap")
            .is_some()
    };
    assert!(
        flagged("config") && flagged("config.orderOptions") && flagged("config.orderOptions.big")
    );
    assert!(!flagged("config.orderOptions.small") && !flagged("config.catalogOptions"));
    assert_eq!(
        text(named("Declaration", "config.orderOptions.big.f7")),
        "f7: 7"
    );
    for node in facts
        .nodes
        .iter()
        .filter(|node| node.label == "Declaration")
    {
        assert!(text(node).len() <= cap || node.props["span_exceeds_range_cap"] == true);
    }
    // A repeated `var` keeps unique, position-qualified identities.
    let twice: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.label == "Declaration" && node.id.contains("#twice@"))
        .collect();
    assert_eq!(twice.len(), 2);
    // Decorators sit outside the method span and each become citable.
    let method = named("Symbol", "Resolver.transition");
    assert!(text(method).starts_with("async transition("));
    let decorators: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.label == "Decorator" && node.props["target"] == method.id.as_str())
        .collect();
    assert_eq!(
        decorators.iter().map(|node| text(node)).collect::<Vec<_>>(),
        ["@Transaction()", "@Allow(Permission.Owner)"]
    );
    assert_eq!(decorators[1].props["name"], "Allow");
    // Names are rebuilt from identifier tokens, never sliced from source, so a
    // comment inside the callee path stays in captured source alone.
    let read = named("Symbol", "Resolver.read");
    let names: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.label == "Decorator" && node.props["target"] == read.id.as_str())
        .map(|node| (node.props["name"].as_str().unwrap().to_owned(), text(node)))
        .collect();
    assert_eq!(
        names,
        [
            ("Nest.Get".to_string(), "@Nest.Get()".to_string()),
            (
                "Namespace.Guard".to_string(),
                "@Namespace /* private-sentinel */ .Guard()".to_string()
            ),
        ]
    );
    // Comments after or between decorators never hide them.
    let decorated = |method: &str| {
        let target = named("Symbol", method).id.as_str();
        facts
            .nodes
            .iter()
            .filter(|node| node.label == "Decorator" && node.props["target"] == target)
            .map(&text)
            .collect::<Vec<_>>()
    };
    assert_eq!(decorated("Resolver.run"), ["@Allow()"]);
    assert_eq!(decorated("Resolver.go"), ["@First()", "@Second()"]);
    let graph = serde_json::to_string(&(&facts.nodes, &facts.edges)).unwrap();
    assert!(!graph.contains("private-sentinel"));
    assert!(source.contains("private-sentinel"));
    assert!(facts.edges.iter().any(|edge| edge.label == "DECORATES"
        && edge.src == decorators[0].id
        && edge.dst == method.id));
    // The oversized callable keeps its Symbol span; a header fact is citable.
    let configure = named("Symbol", "configure");
    assert!(text(configure).len() > crate::declarations::MAX_CITABLE_SPAN_BYTES);
    let header = named("Signature", "configure");
    assert_eq!(text(header), "function configure(options: Options) ");
    assert_eq!(header.props["symbol"], configure.id.as_str());
    assert_eq!(header.props["header_exceeds_range_cap"], false);
    assert!(
        !facts
            .nodes
            .iter()
            .any(|node| node.label == "Signature" && node.props["name"] == "Resolver.transition")
    );
    for node in facts.nodes.iter().filter(|node| {
        matches!(
            node.label.as_str(),
            "Declaration" | "Decorator" | "Signature"
        )
    }) {
        let receipt = receipts
            .iter()
            .find(|receipt| receipt.fact_key() == &FactKey::from_node(node))
            .unwrap();
        assert!(receipt.matches_node(node));
        assert_eq!(receipt.ranges().len(), 1);
        assert_eq!(receipt.ranges()[0].role, RangeRole::Provenance);
    }
    let (_, directory_receipts, stats) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(by_fact(directory_receipts), receipts);
    assert!(stats.omitted_declaration_receipts.is_empty());
}

// AC-0282: hostile nesting is described to a fixed depth on an ordinary worker
// thread, with the deepest oversized section left as a flagged gap.
#[test]
fn captured_declaration_nesting_is_bounded() {
    let depth = 2000;
    let source = format!(
        "const config = {}'{}'{};\n",
        "{a: ".repeat(depth),
        "x".repeat(9000),
        "}".repeat(depth)
    );
    let facts = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || crate::extract_source(source.as_bytes(), "deep.ts", &parser_id()).unwrap())
        .unwrap()
        .join()
        .expect("bounded nesting never overflows the stack");
    let properties: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.label == "Declaration" && node.props["kind"] == "Property")
        .collect();
    assert_eq!(properties.len(), crate::declarations::MAX_PROPERTY_DEPTH);
    assert!(
        properties
            .iter()
            .all(|node| node.props["span_exceeds_range_cap"] == true)
    );
}

// AC-0283: locals of a callable too large to inspect whole get their own
// citable ranges; nested blocks, callables and in-cap functions are unchanged.
#[test]
fn captured_oversized_callable_locals_are_citable() {
    let filler = "            check(fromState);\n".repeat(600);
    let source = format!(
        r#"export function configureDefaultOrderProcess(options: Options) {{
    let connection: TransactionalConnection;
    let eventBus: EventBus;
    var twice = 1;
    var twice = 2;
    const orderProcess: OrderProcess<OrderState> = {{
        transitions: {{
            Created: {{ to: ['AddingItems', 'Draft'] }},
            Draft: {{ to: ['Cancelled'] }},
        }},
        init(injector) {{ connection = injector.get(TransactionalConnection); }},
        async onTransitionStart(fromState, toState) {{
{filler}        }},
    }};
    const handler = () => 1;
    if (options.strict) {{ const nested = 1; }}
    return orderProcess;
}}
export function small() {{ const inner = 1; return inner; }}
"#
    );
    let (directory, capture) = fixture(source.as_bytes(), "order.ts");
    let (facts, receipts) = extract_file(capture.file("order.ts").unwrap(), &parser_id()).unwrap();
    let text = |node: &Node| {
        let receipt = receipts
            .iter()
            .find(|receipt| receipt.fact_key() == &FactKey::from_node(node))
            .unwrap_or_else(|| panic!("{} is not citable", node.id));
        assert!(receipt.matches_node(node));
        let evidence = &receipt.ranges()[0].evidence;
        source[evidence.byte_start as usize..evidence.byte_end as usize].to_owned()
    };
    let named = |name: &str| {
        facts
            .nodes
            .iter()
            .find(|node| node.label == "Declaration" && node.props["name"] == name)
    };
    let function = facts
        .nodes
        .iter()
        .find(|node| node.label == "Symbol" && node.props["name"] == "configureDefaultOrderProcess")
        .unwrap();
    assert!(text(function).len() > crate::declarations::MAX_CITABLE_SPAN_BYTES);
    let connection = named("configureDefaultOrderProcess/connection").unwrap();
    assert_eq!(
        connection.id,
        format!("decl:{REPO}@order.ts#configureDefaultOrderProcess/connection")
    );
    assert_eq!(text(connection), "connection: TransactionalConnection");
    assert_eq!(connection.props["binding"], "let");
    assert_eq!(connection.props["scope"], "local");
    assert_eq!(connection.props["symbol"], function.id.as_str());
    assert_eq!(connection.props["exported"], false);
    assert_eq!(
        text(named("configureDefaultOrderProcess/eventBus").unwrap()),
        "eventBus: EventBus"
    );
    let twice: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.id.contains("#configureDefaultOrderProcess/twice@"))
        .map(text)
        .collect();
    assert_eq!(twice, ["twice = 1", "twice = 2"]);
    // The oversized object is a flagged gap; its sections are citable.
    let process = named("configureDefaultOrderProcess/orderProcess").unwrap();
    assert_eq!(process.props["span_exceeds_range_cap"], true);
    let transitions = named("configureDefaultOrderProcess/orderProcess.transitions").unwrap();
    assert!(text(transitions).starts_with("transitions: {\n            Created:"));
    assert!(text(transitions).len() <= crate::declarations::MAX_CITABLE_SPAN_BYTES);
    assert_eq!(transitions.props["declaration"], process.id.as_str());
    for absent in [
        "configureDefaultOrderProcess/handler",
        "configureDefaultOrderProcess/nested",
        "configureDefaultOrderProcess/orderProcess.init",
        "configureDefaultOrderProcess/orderProcess.onTransitionStart",
        "small/inner",
        "inner",
    ] {
        assert!(
            named(absent).is_none(),
            "{absent} must not be a Declaration"
        );
    }
    let (_, directory_receipts, stats) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(by_fact(directory_receipts), receipts);
    assert!(stats.omitted_declaration_receipts.is_empty());
    // Locals are minted last: when the file's own facts exhaust the budget,
    // only locals are omitted, and they are counted.
    let file = capture.file("order.ts").unwrap();
    let produce = |receipts| {
        extract_file_with(
            file,
            &parser_id(),
            Registries::builtin(),
            ReceiptBudget {
                bytes: usize::MAX,
                receipts,
            },
        )
        .unwrap()
        .1
    };
    let local = |receipt: &Receipt| match receipt.fact_key() {
        FactKey::Node { id } => {
            id.starts_with("decl:") && id.contains("#configureDefaultOrderProcess/")
        }
        _ => false,
    };
    let all = produce(usize::MAX);
    let locals = all
        .declarations
        .iter()
        .filter(|(receipt, _)| local(receipt))
        .count();
    assert_eq!(locals, 6);
    let rest = produce(all.declarations.len() - locals);
    assert_eq!(rest.omitted_declarations, locals as u64);
    assert!(!rest.declarations.iter().any(|(receipt, _)| local(receipt)));
}
