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
    // The rule, its owner, GOVERNS and the file's own File node (AC-1019).
    assert_eq!(receipts.len(), 4);
    for receipt in &receipts {
        match receipt.fact_key() {
            FactKey::Node { id } => {
                let node = captured.nodes.iter().find(|node| &node.id == id).unwrap();
                assert!(node.props.get("via").is_none());
                assert!(matches!(
                    node.label.as_str(),
                    "BusinessRule" | "Component" | "File"
                ));
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
                .to_receipt()
                .unwrap()
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
        "/content/ranges/0/byte_end",
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
        enumerate_with_limits(&root, 0, 64, 8192, MAX_UNFOLLOWED_LINKS),
        Err(CapturedError::Limit("visited entries"))
    ));
    assert!(matches!(
        enumerate_with_limits(&root, 100, 0, 8192, MAX_UNFOLLOWED_LINKS),
        Err(CapturedError::Limit("directory depth"))
    ));
    assert!(matches!(
        enumerate_with_limits(&root, 100, 64, 1, MAX_UNFOLLOWED_LINKS),
        Err(CapturedError::Limit("selected files"))
    ));
}

// AC-1802 (#1653): a project's selection limits bound the walk. A file bound
// above the default selects more files, a visited-entry bound below the tree
// stops it whole, and limits past the hard ceilings are refused.
#[test]
fn selection_limits_bound_the_walk_and_stop_at_the_hard_ceilings() {
    let directory = tempfile::tempdir().unwrap();
    for index in 0..3 {
        fs::write(directory.path().join(format!("f{index}.ts")), b"").unwrap();
    }
    let within = |max_visited_entries, max_files| {
        enumerate_selection_within(
            directory.path(),
            SelectionLimits {
                max_visited_entries,
                max_files,
            },
        )
    };
    assert_eq!(within(3, 3).unwrap().files.len(), 3);
    assert!(matches!(
        within(3, 2),
        Err(CapturedError::Limit("selected files"))
    ));
    assert!(matches!(
        within(2, 3),
        Err(CapturedError::Limit("visited entries"))
    ));
    for invalid in [
        (0, 3),
        (HARD_MAX_VISITED_ENTRIES + 1, 3),
        (3, source_capture::HARD_MAX_FILES + 1),
    ] {
        assert!(matches!(
            within(invalid.0, invalid.1),
            Err(CapturedError::Invalid("selection limits"))
        ));
    }
    assert_eq!(
        SelectionLimits::default().max_files,
        source_capture::MAX_FILES
    );
    within(HARD_MAX_VISITED_ENTRIES, source_capture::HARD_MAX_FILES).unwrap();
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

// AC-1802 (#1653): a raised entry bound never lets one selection retain more
// unfollowed symlink paths than the default walk could; one past the bound
// fails the selection whole.
#[cfg(unix)]
#[test]
fn unfollowed_links_stay_bounded_when_the_walk_is_raised() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["a.ts", "b.ts", "c.ts"] {
        std::os::unix::fs::symlink("missing.ts", directory.path().join(name)).unwrap();
    }
    let root = Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority()).unwrap();
    assert_eq!(
        enumerate_with_limits(&root, 100, 64, 8192, 3)
            .unwrap()
            .unfollowed
            .len(),
        3
    );
    assert!(matches!(
        enumerate_with_limits(&root, 100, 64, 8192, 2),
        Err(CapturedError::Limit("unfollowed links"))
    ));
    assert_eq!(MAX_UNFOLLOWED_LINKS, MAX_VISITED_ENTRIES);
}

#[cfg(unix)]
#[test]
fn captured_enumeration_skips_symlinks_and_rejects_non_utf8_paths() {
    // AC-0148 (#1229): no symlink is followed, opened or resolved, so nothing
    // outside the tree is read; each one is listed instead of failing the
    // whole selection, as the ordinary walk skips it (ADR-0030). Lossy path
    // conversion may never collapse two primary source identities.
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("hidden.ts"), b"external source").unwrap();
    for (path, body) in [
        ("AGENTS.md", "notes"),
        ("src/app.ts", "export function app() { return 1; }"),
        ("lib/util.ts", "export function util() {}"),
        (".gitignore", "ignored-link\n"),
    ] {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    let root = directory.path();
    // A documentation link, a supported-name link outside the tree, a
    // directory link outside it, an in-tree directory link, a dangling link,
    // an ignored link and a link named like an excluded directory.
    symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).unwrap();
    symlink(outside.path().join("hidden.ts"), root.join("src/source.ts")).unwrap();
    symlink(outside.path(), root.join("linked")).unwrap();
    symlink(root.join("lib"), root.join("src/lib")).unwrap();
    symlink(root.join("missing.ts"), root.join("dangling.ts")).unwrap();
    symlink(outside.path(), root.join("ignored-link")).unwrap();
    symlink(outside.path(), root.join("node_modules")).unwrap();
    let selection = enumerate_selection(root).unwrap();
    assert_eq!(selection.files, ["lib/util.ts", "src/app.ts"]);
    assert_eq!(
        selection.unfollowed,
        [
            "CLAUDE.md",
            "dangling.ts",
            "linked",
            "src/lib",
            "src/source.ts"
        ]
    );
    assert_eq!(enumerate_paths(root).unwrap(), selection.files);
    // The ordinary walk selects the same files.
    let mut ordinary = Vec::new();
    crate::collect_ts_files(root, &mut ordinary).unwrap();
    assert_eq!(ordinary, selection.files);
    // A tree without symlinks reports none.
    let plain = tempfile::tempdir().unwrap();
    fs::write(plain.path().join("a.ts"), b"").unwrap();
    assert!(
        enumerate_selection(plain.path())
            .unwrap()
            .unfollowed
            .is_empty()
    );
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
fn captured_walk_returns_its_facts_packed_and_matches_the_list_form() {
    // AC-0863 (#1237): the captured walk packs each file's facts as it merges
    // and completes the directory over the packed store. Its facts, receipts
    // and statistics equal the list form's: every file decoded together,
    // completed over plain lists, receipts matched against the decoded facts.
    let directory = tempfile::tempdir().unwrap();
    for (path, source) in [
        (
            "a.ts",
            "import { b } from './b';\nimport { gone } from './missing';\n\
             export function a(ok: boolean) { if (!ok) throw new Error('no'); gone(); return b(); }\n",
        ),
        (
            "b.ts",
            "export function b(n: number) { for (;;) { if (n > 1) return n; } }\n",
        ),
    ] {
        fs::write(directory.path().join(path), source).unwrap();
    }
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let id = parser_id();
    let (decoded, receipts, stats) = extract_captured_dir(
        directory.path(),
        &id,
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    let (rest, store, packed_receipts, packed_stats) = extract_captured_dir_packed(
        directory.path(),
        &id,
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert!(rest.nodes.is_empty() && rest.edges.is_empty());
    assert_eq!(rest.default_exports, decoded.default_exports);
    assert_eq!(packed_receipts, receipts);
    assert_eq!(packed_stats, stats);
    let packed = store.into_facts().unwrap();
    assert_eq!(packed, (decoded.nodes.clone(), decoded.edges.clone()));

    let mut reference = Extraction::default();
    for path in &paths {
        let (facts, _) = extract_file_with(
            capture.file(path).unwrap(),
            &id,
            Registries::builtin(),
            FILE_DECLARATIONS,
        )
        .unwrap();
        crate::append_extraction(&mut reference, facts);
    }
    let (mut nodes, mut edges) = (
        std::mem::take(&mut reference.nodes),
        std::mem::take(&mut reference.edges),
    );
    crate::complete_directory(
        &mut reference,
        &mut core_graph::facts::FactLists {
            nodes: &mut nodes,
            edges: &mut edges,
        },
        directory.path(),
        &id,
    )
    .unwrap();
    assert_eq!(packed, (nodes.clone(), edges.clone()));
    (reference.nodes, reference.edges) = (nodes, edges);
    let matching = matcher(&reference);
    assert!(!receipts.is_empty() && receipts.iter().all(&matching));
    drop(matching);

    // The fixture exercises the rule path and both pending-call outcomes.
    let labeled = |label: &str| reference.nodes.iter().filter(|n| n.label == label).count();
    assert_eq!(labeled("BusinessRule"), 2);
    assert!(
        reference
            .nodes
            .iter()
            .any(|node| node.props["rule_evidence_gap"] == true)
    );
    let calls = |dst: &str| {
        reference
            .edges
            .iter()
            .any(|edge| edge.label == "CALLS" && edge.dst.starts_with(dst))
    };
    assert!(calls(&format!("sym:{REPO}@b.ts#b")));
    assert!(calls(&format!("gap:call:{REPO}@a.ts@")));
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

/// Whether a receipt is one only the directory lane mints: a File, TestCase
/// or unresolved-call Gap fact decided by completion (AC-1019).
fn completed(receipt: &impl Citable) -> bool {
    matches!(receipt.fact_key(), FactKey::Node { id }
        if id.starts_with("file:") || id.starts_with("test:") || id.starts_with("gap:call:"))
}

/// The directory lane's lexical receipts in full form, in fact-key order,
/// comparable with the single-file lane.
fn by_fact(receipts: Vec<MintedReceipt>) -> Vec<Receipt> {
    let mut receipts: Vec<_> = receipts
        .iter()
        .filter(|receipt| !completed(*receipt))
        .map(|receipt| receipt.to_receipt().unwrap())
        .collect();
    receipts.sort_by(|a, b| a.fact_key().cmp(b.fact_key()));
    receipts
}

/// Single-file `receipts` as the directory lane mints them from a fresh
/// parse: under contract v4, established by each file's own closure
/// (ADR-0072 slice 5).
fn established(receipts: Vec<Receipt>) -> Vec<Receipt> {
    receipts
        .into_iter()
        .map(|receipt| {
            let closure = direct_lexical_closure(receipt.file(), receipt.repo_key()).unwrap();
            receipt
                .with_closure(ReceiptClosure::Established {
                    closure_id: closure.closure_id,
                })
                .unwrap()
        })
        .collect()
}

/// A receipt's fact, digest and cited spans, which a reused parse keeps.
fn cited(
    receipts: &[MintedReceipt],
) -> Vec<(FactKey, String, Vec<source_capture::CaptureSpanRef>)> {
    receipts
        .iter()
        .map(|receipt| {
            (
                receipt.fact_key().clone(),
                receipt.fact_digest().to_owned(),
                receipt.captured_spans().collect(),
            )
        })
        .collect()
}

/// What admitting `receipts` charges, each shared header once (#725).
fn charged<'a>(receipts: impl IntoIterator<Item = &'a MintedReceipt>) -> usize {
    let mut charge = Charge::default();
    let unbounded = ReceiptBudget {
        bytes: usize::MAX,
        receipts: usize::MAX,
    };
    for receipt in receipts {
        assert!(charge.admit(receipt, 0, unbounded));
    }
    charge.bytes
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
    let (facts, receipts) =
        extract_file_with_source_sizes(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
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
    assert_eq!(by_fact(directory_receipts), established(receipts.clone()));
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
            let file = capture.file(path).unwrap();
            let closure = direct_lexical_closure(file.reference(), parser_id().repo).unwrap();
            let receipts =
                extract_file_with(file, &parser_id(), Registries::builtin(), FILE_DECLARATIONS)
                    .unwrap()
                    .1;
            establish(receipts, Some(Arc::new(closure))).unwrap()
        })
        .collect();
    let rules: Vec<_> = per_file.iter().flat_map(|file| &file.rules).collect();
    let rule_bytes = charged(rules.iter().copied());
    let first = &per_file[0].declarations[0];
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
    // The first declaration shares a rule's file header, so it adds only its
    // own bytes.
    let with_first = charged(rules.iter().copied().chain([first]));
    assert_eq!(with_first - rule_bytes, first.retained_bytes());
    let (_, admitted, stats) = run(with_first, usize::MAX).unwrap();
    assert_eq!(admitted.len(), rules.len() + 1);
    assert!(admitted.contains(first));
    assert_eq!(
        stats.omitted_declaration_receipts,
        BTreeMap::from([("b.ts".to_string(), 2)])
    );
    assert!(stats.omitted_receipt_facts_complete);
    let expected_b_decls: Vec<_> = per_file[1]
        .declarations
        .iter()
        .map(|receipt| core_graph::source::SourceOmission {
            fact: receipt.fact_key().clone(),
            emitted_fact_digest: receipt.fact_digest().to_owned(),
            kind: core_graph::source::ReceiptOmissionKind::Declarations,
        })
        .collect();
    assert_eq!(stats.omitted_receipt_facts, expected_b_decls);
    // A receipt-count ceiling equal to the rules admits no declaration.
    let (_, admitted, stats) = run(usize::MAX, rules.len()).unwrap();
    assert_eq!(admitted.len(), rules.len());
    assert_eq!(
        stats.omitted_declaration_receipts,
        BTreeMap::from([("a.ts".to_string(), 1), ("b.ts".to_string(), 2)])
    );
    assert!(stats.omitted_receipt_facts_complete);
    let expected_all_decls: Vec<_> = per_file
        .iter()
        .flat_map(|file| &file.declarations)
        .map(|receipt| core_graph::source::SourceOmission {
            fact: receipt.fact_key().clone(),
            emitted_fact_digest: receipt.fact_digest().to_owned(),
            kind: core_graph::source::ReceiptOmissionKind::Declarations,
        })
        .collect();
    assert_eq!(stats.omitted_receipt_facts, expected_all_decls);
    // AC-0292, AC-0314: a rule receipt that does not fit is omitted and named by
    // file (#722) and by fact key (#725); the extraction succeeds, keeps the
    // fact, and admits no declaration ahead of the rule that lost. (T-0314)
    let (extraction, admitted, stats) = run(rule_bytes - 1, usize::MAX).unwrap();
    let kept = rules.iter().filter(|rule| admitted.contains(rule)).count();
    assert_eq!(kept, rules.len() - 1);
    assert_eq!(
        stats.omitted_rule_receipts,
        BTreeMap::from([("a.ts".to_string(), 1)])
    );
    let lost = rules.iter().find(|rule| !admitted.contains(rule)).unwrap();
    let held = FactStore::from_facts(extraction.nodes.clone(), extraction.edges.clone()).unwrap();
    assert!(surviving_facts(&held)(lost.fact_key()));
    assert!(stats.omitted_receipt_facts_complete);
    let omitted_rule_keys: Vec<_> = stats
        .omitted_receipt_facts
        .iter()
        .filter(|omission| omission.kind == core_graph::source::ReceiptOmissionKind::Rules)
        .map(|omission| omission.fact.clone())
        .collect();
    assert_eq!(omitted_rule_keys, vec![lost.fact_key().clone()]);
    assert!(stats.omitted_receipt_facts.iter().any(|omission| {
        omission.kind == core_graph::source::ReceiptOmissionKind::Rules
            && omission.emitted_fact_digest == lost.fact_digest()
    }));
    let omitted_decl_count = stats
        .omitted_receipt_facts
        .iter()
        .filter(|omission| omission.kind == core_graph::source::ReceiptOmissionKind::Declarations)
        .count() as u64;
    assert_eq!(
        omitted_decl_count,
        stats.omitted_declaration_receipts.values().sum::<u64>()
    );
    // Declarations only fill what the admitted rules left; none is admitted
    // past the byte budget, and every one left out is still named.
    let admitted_bytes = charged(&admitted);
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
    let counted = produce(usize::MAX, 1);
    assert_eq!(
        (counted.declarations.len(), counted.omitted_declarations),
        (1, 2)
    );
    let sized = produce(charged(&all.declarations[..2]), usize::MAX);
    assert_eq!(
        (sized.declarations.len(), sized.omitted_declarations),
        (2, 1)
    );
    // Producer omissions reach the directory stats under the file's name and
    // mark per-fact omission records incomplete because unminted per-file
    // declarations have no FactKey (AC-0314, T-0314).
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
    assert!(!stats.omitted_receipt_facts_complete);
    assert!(stats.omitted_receipt_facts.is_empty());
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
    let (facts, receipts) =
        extract_file_with_source_sizes(capture.file("config.ts").unwrap(), &parser_id()).unwrap();
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
    assert_eq!(by_fact(directory_receipts), established(receipts.clone()));
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
    let (facts, receipts) =
        extract_file_with_source_sizes(capture.file("order.ts").unwrap(), &parser_id()).unwrap();
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
    assert_eq!(by_fact(directory_receipts), established(receipts.clone()));
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
    let local = |receipt: &MintedReceipt| match receipt.fact_key() {
        FactKey::Node { id } => {
            id.starts_with("decl:") && id.contains("#configureDefaultOrderProcess/")
        }
        _ => false,
    };
    let all = produce(usize::MAX);
    let locals = all
        .declarations
        .iter()
        .filter(|receipt| local(receipt))
        .count();
    assert_eq!(locals, 6);
    let rest = produce(all.declarations.len() - locals);
    assert_eq!(rest.omitted_declarations, locals as u64);
    assert!(!rest.declarations.iter().any(local));
}

// AC-0503: directory extraction retains receipts in compact form, one shared
// header per file, and rebuilds the exact receipt on demand (#725).
#[test]
fn compact_receipts_rebuild_exactly_and_share_one_header_per_file() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("a.ts"),
        b"export function guard(ok: boolean) { if (!ok) return false; return true; }\nexport function one() { return 1; }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("b.ts"),
        b"export function two() { return 2; }\n",
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let (_, minted, _) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    let mut full = Vec::new();
    for path in &paths {
        full.extend(
            extract_file_with_source_sizes(capture.file(path).unwrap(), &parser_id())
                .unwrap()
                .1,
        );
    }
    full.sort_by(|a, b| a.fact_key().cmp(b.fact_key()));
    assert_eq!(by_fact(minted.clone()), established(full.clone()));
    for receipt in &minted {
        let rebuilt = receipt.to_receipt().unwrap();
        let json = receipt.to_json().unwrap();
        // Wire bytes and id are those of the full receipt, and decode alone.
        assert_eq!(json, rebuilt.to_json().unwrap());
        assert_eq!(Receipt::from_json(&json).unwrap(), rebuilt);
        assert_eq!(receipt.id(), rebuilt.id());
        assert_eq!(
            receipt.captured_spans().collect::<Vec<_>>(),
            rebuilt
                .ranges()
                .iter()
                .map(|range| range.captured.clone())
                .collect::<Vec<_>>()
        );
        let shares = minted
            .iter()
            .filter(|other| other.file().path == receipt.file().path)
            .all(|other| Arc::ptr_eq(other.header(), receipt.header()));
        assert!(shares, "{} has one header", receipt.file().path);
    }
    // The compact form retains less than the canonical JSON of the same
    // receipts and of the closure records they name, each once.
    let records: BTreeMap<_, _> = minted
        .iter()
        .filter_map(MintedReceipt::input_closure)
        .map(|record| {
            (
                record.closure_id.clone(),
                record.canonical_bytes().unwrap().len(),
            )
        })
        .collect();
    let json_bytes: usize = established(full)
        .iter()
        .map(|r| r.to_json().unwrap().len())
        .sum::<usize>()
        + records.values().sum::<usize>();
    assert!(charged(&minted) < json_bytes);
}

// AC-0503: a file header is charged once, only with the first receipt admitted
// under it; a refused receipt charges nothing and leaves its header uncharged.
#[test]
fn shared_headers_are_charged_once_after_admission() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("a.ts"),
        b"export function one() { return 1; }\nexport function two() { return 2; }\n",
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let declarations = extract_file_with(
        capture.file("a.ts").unwrap(),
        &parser_id(),
        Registries::builtin(),
        FILE_DECLARATIONS,
    )
    .unwrap()
    .1
    .declarations;
    let [first, second] = &declarations[..] else {
        panic!("two declarations");
    };
    let header =
        first.header().retained_bytes() + std::mem::size_of::<(usize, Arc<MintedHeader>)>();
    let exact = header + first.retained_bytes();
    let budget = |bytes| ReceiptBudget {
        bytes,
        receipts: usize::MAX,
    };
    let mut charge = Charge::default();
    // One byte short: refused, and nothing is charged or held.
    assert!(!charge.admit(first, 0, budget(exact - 1)));
    assert_eq!((charge.bytes, charge.headers.len()), (0, 0));
    // Exactly enough: the header is charged with the first admission.
    assert!(charge.admit(first, 0, budget(exact)));
    assert_eq!((charge.bytes, charge.headers.len()), (exact, 1));
    // The second receipt of the file adds only its own bytes.
    let both = exact + second.retained_bytes();
    assert!(!charge.admit(second, 1, budget(both - 1)));
    assert!(charge.admit(second, 1, budget(both)));
    assert_eq!((charge.bytes, charge.headers.len()), (both, 1));
    // The count ceiling still applies on its own.
    assert!(!Charge::default().admit(
        first,
        1,
        ReceiptBudget {
            bytes: usize::MAX,
            receipts: 1,
        }
    ));
}

// AC-0287: fresh app directory captures measure every admitted receipt range.
#[test]
fn captured_directory_mints_measured_receipts() {
    let (directory, capture) = fixture(
        b"export function guard(ok: boolean) { if (!ok) return false; return true; }",
        "source.ts",
    );
    let (_, receipts, stats) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    assert!(!receipts.is_empty());
    assert!(stats.omitted_rule_receipts.is_empty());
    assert!(stats.omitted_declaration_receipts.is_empty());
    for receipt in receipts {
        let receipt = receipt.to_receipt().unwrap();
        for index in 0..receipt.ranges().len() {
            assert!(receipt.source_sizes(index).unwrap().is_some());
        }
    }
}

// AC-0287: authorized local comparison only; never silently ingest real targets.
#[test]
#[ignore = "requires explicitly authorized CARTOGRAPH_CAPTURE_BUDGET_SOURCE"]
fn captured_directory_measurement_budget_diagnostic() {
    let root = std::path::PathBuf::from(
        std::env::var("CARTOGRAPH_CAPTURE_BUDGET_SOURCE").expect("explicit source"),
    );
    let paths = enumerate_paths(&root).unwrap();
    let capture = capture(&root, &paths);
    let mut reports = Vec::new();
    let mut keys = Vec::new();
    for measured in [false, true] {
        let (_, receipts, stats) = extract_captured_dir_with_policy(
            &root,
            &parser_id(),
            &capture,
            Registries::builtin(),
            &mut |_| {},
            DirectoryReceiptPolicy {
                budget: ReceiptBudget {
                    bytes: MAX_EXTRACTION_RECEIPT_BYTES,
                    receipts: MAX_EXTRACTION_RECEIPTS,
                },
                file_declarations: FILE_DECLARATIONS,
                measure_source: measured,
            },
        )
        .unwrap();
        let wire: usize = receipts.iter().map(|r| r.to_json().unwrap().len()).sum();
        let measured_ranges: usize = receipts
            .iter()
            .map(|r| {
                let r = r.to_receipt().unwrap();
                (0..r.ranges().len())
                    .filter(|i| r.source_sizes(*i).unwrap().is_some())
                    .count()
            })
            .sum();
        reports.push(serde_json::json!({"measured":measured,"files":paths.len(),"receipts":receipts.len(),"retained_bytes":charged(receipts.iter()),"wire_bytes":wire,"measured_ranges":measured_ranges,"omitted_rules":stats.omitted_rule_receipts,"omitted_declarations":stats.omitted_declaration_receipts}));
        keys.push(
            receipts
                .iter()
                .map(|r| r.fact_key().clone())
                .collect::<std::collections::BTreeSet<_>>(),
        );
    }
    println!("{}", serde_json::to_string_pretty(&reports).unwrap());
    assert_eq!(
        keys[0], keys[1],
        "measurement changed admitted receipt facts"
    );
    for report in reports {
        assert_eq!(
            report["omitted_rules"],
            serde_json::json!({}),
            "rule receipts dropped"
        );
        assert_eq!(
            report["omitted_declarations"],
            serde_json::json!({}),
            "declarations dropped"
        );
    }
}

// AC-1019: an unresolved relative-import call's Gap, each File node and each
// TestCase cite their spans through receipts the directory lane keeps only
// when the complete fact survives completion; a resolved call leaves no Gap
// receipt, and the single-file lane, with no completion, mints none.
#[test]
fn captured_gap_task_supporting_facts_are_citable_only_when_they_survive() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("caller.ts"),
        br#"import { missing, present } from "./callee";
export function run() {
  present();
  return missing();
}
"#,
    )
    .unwrap();
    fs::write(
        directory.path().join("callee.ts"),
        b"export function present() { return 1; }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("caller.test.ts"),
        b"import { run } from \"./caller\";\nit(\"runs\", () => { run(); });\n",
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let (facts, receipts, _) = extract_captured_dir(
        directory.path(),
        &parser_id(),
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    let gaps: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:call:"))
        .collect();
    assert_eq!(gaps.len(), 1, "only `missing()` stays unresolved");
    let gap = gaps[0];
    let gap_receipts: Vec<_> = receipts
        .iter()
        .filter(|receipt| {
            matches!(receipt.fact_key(), FactKey::Node { id } if id.starts_with("gap:call:"))
        })
        .collect();
    assert_eq!(gap_receipts.len(), 1);
    let receipt = gap_receipts[0].to_receipt().unwrap();
    assert!(receipt.matches_node(gap));
    let range = &receipt.ranges()[0];
    assert_eq!(range.role, RangeRole::Provenance);
    assert_eq!(capture.read_span(&range.captured).unwrap(), b"missing()");
    for label in ["File", "TestCase"] {
        let nodes: Vec<_> = facts.nodes.iter().filter(|n| n.label == label).collect();
        assert!(!nodes.is_empty(), "{label}");
        for node in nodes {
            let receipt = receipts
                .iter()
                .find(|receipt| receipt.fact_key() == &FactKey::from_node(node))
                .unwrap_or_else(|| panic!("{} is not citable", node.id))
                .to_receipt()
                .unwrap();
            assert!(receipt.matches_node(node));
        }
    }
    for path in &paths {
        let (_, single) = extract_file(capture.file(path).unwrap(), &parser_id()).unwrap();
        assert!(!single.iter().any(completed));
    }
}

// AC-1517: an unchanged capture parses nothing and equals a cold walk, at
// the same commit or a later one; a one-byte change parses only that file;
// a file gone from the capture is counted and dropped from the cache.
#[test]
fn attested_parses_are_reused_only_for_unchanged_bytes_and_equal_a_cold_walk() {
    let directory = tempfile::tempdir().unwrap();
    let files = [
        (
            "src/checkout.ts",
            "import { price } from './price';\nexport function checkout(ready: boolean) { if (!ready) throw new Error('not ready'); return price(2); }\n",
        ),
        (
            "src/price.ts",
            "export function price(quantity: number) { if (quantity < 1) return 0; return quantity * 3; }\n",
        ),
        (
            "src/view.tsx",
            "export function View() { return <div>{1}</div>; }\n",
        ),
    ];
    let paths: Vec<String> = files.iter().map(|(path, _)| path.to_string()).collect();
    for (path, body) in files {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    let first_commit = "1111111111111111111111111111111111111111";
    let later_commit = "2222222222222222222222222222222222222222";
    let at = |commit: &'static str| SourceId { repo: REPO, commit };
    let walk = |capture: &Capture, commit: &'static str, cache: &mut CapturedCache| {
        let (_, facts, receipts, stats) = extract_captured_dir_packed_cached(
            directory.path(),
            &at(commit),
            capture,
            Registries::builtin(),
            cache,
            &mut |_| {},
        )
        .unwrap();
        (facts.into_facts().unwrap(), receipts, stats)
    };
    let cold = |capture: &Capture, commit: &'static str| {
        walk(capture, commit, &mut CapturedCache::default())
    };
    let counts = |stats: &IncrementalStats| {
        (
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        )
    };

    let first = capture(directory.path(), &paths);
    let mut cache = CapturedCache::default();
    let (facts, receipts, stats) = walk(&first, first_commit, &mut cache);
    assert_eq!(counts(&stats), (3, 0, 0));
    assert_eq!(cache.len(), 3);
    assert!(!receipts.is_empty());

    let all = |receipts: &[MintedReceipt], expected: &dyn Fn(Option<&ReceiptClosure>) -> bool| {
        receipts
            .iter()
            .filter(|receipt| !completed(*receipt))
            .all(|receipt| expected(receipt.closure()))
    };
    let established = |closure: Option<&ReceiptClosure>| {
        matches!(closure, Some(ReceiptClosure::Established { .. }))
    };
    let unproven = |closure: Option<&ReceiptClosure>| {
        closure
            == Some(&ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![ClosureGapReason::CacheReuseUnproven],
            })
    };
    // A fresh parse establishes each fact's single-file closure, and the walk
    // receipt carries the record it names (ADR-0072 slice 5).
    assert!(all(&receipts, &established));
    assert!(
        receipts
            .iter()
            .filter(|receipt| !completed(*receipt))
            .all(|receipt| receipt.input_closure().is_some())
    );

    // The same capture again: nothing parsed, and each reused parse was made
    // under the closure it now names, so the receipts equal the cold walk's.
    let (again, again_receipts, stats) = walk(&first, first_commit, &mut cache);
    assert_eq!(counts(&stats), (0, 3, 0));
    assert_eq!((again, &again_receipts), (facts, &receipts));

    // Unchanged bytes at a later commit equal a cold walk at that commit.
    let later = capture(directory.path(), &paths);
    let (warm, warm_receipts, stats) = walk(&later, later_commit, &mut cache);
    assert_eq!(counts(&stats), (0, 3, 0));
    let (cold_facts, cold_receipts, _) = cold(&later, later_commit);
    assert!(
        cold_facts
            .0
            .iter()
            .any(|node| node.props.to_string().contains(later_commit))
    );
    assert_eq!((warm, &warm_receipts), (cold_facts, &cold_receipts));

    // One changed byte parses exactly that file.
    fs::write(
        directory.path().join("src/price.ts"),
        files[1].1.replace("* 3", "* 4"),
    )
    .unwrap();
    let changed = capture(directory.path(), &paths);
    let (warm, warm_receipts, stats) = walk(&changed, later_commit, &mut cache);
    assert_eq!(counts(&stats), (1, 2, 0));
    let (cold_facts, cold_receipts, _) = cold(&changed, later_commit);
    assert_eq!(
        (warm, cited(&warm_receipts)),
        (cold_facts, cited(&cold_receipts))
    );
    // The changed bytes recapture every file, so a reused parse no longer
    // proves its closure: only the parsed file's facts establish theirs.
    assert!(
        warm_receipts
            .iter()
            .filter(|receipt| receipt.file().path != "src/price.ts" && !completed(*receipt))
            .all(|receipt| unproven(receipt.closure()) && receipt.input_closure().is_none())
    );
    let parsed: std::collections::BTreeSet<&str> = warm_receipts
        .iter()
        .filter(|receipt| matches!(receipt.closure(), Some(ReceiptClosure::Established { .. })))
        .map(|receipt| receipt.file().path.as_str())
        .collect();
    assert_eq!(parsed, std::collections::BTreeSet::from(["src/price.ts"]));

    // A file left out of the capture is counted and leaves the cache.
    let fewer = capture(directory.path(), &paths[..2]);
    let (_, _, stats) = walk(&fewer, later_commit, &mut cache);
    assert_eq!(counts(&stats), (0, 2, 1));
    assert_eq!(cache.len(), 2);

    // A walk whose directory completion fails, here because its live
    // configuration root cannot be read, leaves no cached parse.
    assert!(
        extract_captured_dir_packed_cached(
            &directory.path().join("missing"),
            &at(later_commit),
            &fewer,
            Registries::builtin(),
            &mut cache,
            &mut |_| {},
        )
        .is_err()
    );
    assert!(cache.is_empty());
}

// AC-1517: an attestation names everything the parse read; any difference
// parses again, and the cached parse never vouches for other bytes.
#[test]
fn attestations_differ_for_any_changed_producer_input() {
    let (directory, first) = fixture(b"export const a = 1;\n", "a.ts");
    let base = Attestation::of(first.file("a.ts").unwrap(), REPO, "registries-1");
    assert_eq!(base.producer, PRODUCER_CONTRACT);
    assert_eq!(
        base,
        Attestation::of(first.file("a.ts").unwrap(), REPO, "registries-1")
    );
    assert_ne!(
        base,
        Attestation::of(first.file("a.ts").unwrap(), REPO, "registries-2")
    );
    assert_ne!(
        base,
        Attestation::of(first.file("a.ts").unwrap(), "local/other", "registries-1")
    );
    fs::write(directory.path().join("a.ts"), b"export const a = 2;\n").unwrap();
    let second = capture(directory.path(), &["a.ts".into()]);
    let other = Attestation::of(second.file("a.ts").unwrap(), REPO, "registries-1");
    assert_eq!(other.byte_len, base.byte_len);
    assert_ne!(other, base);
}
