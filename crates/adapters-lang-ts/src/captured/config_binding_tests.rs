//! A captured walk binds directory completion's resolution configs to the
//! capture only when it retains exactly the bytes read (ADR-0072 slice 6,
//! AC-1090).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};
use std::fs;

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";

fn tree() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (path, body) in [
        ("package.json", r#"{"name":"app"}"#),
        ("tsconfig.json", r#"{"compilerOptions":{"baseUrl":"."}}"#),
        (
            "packages/lib/package.json",
            r#"{"name":"lib","main":"index.ts"}"#,
        ),
        (
            "packages/lib/index.ts",
            "export function helper() { return 1; }\n",
        ),
        (
            "src/app.ts",
            "import { helper } from 'lib';\nexport const x = helper();\n",
        ),
        ("vite.config.ts", "export default {};\n"),
        ("ignored/package.json", r#"{"name":"ignored"}"#),
        ("node_modules/dep/package.json", r#"{"name":"dep"}"#),
        (".gitignore", "ignored/\n"),
    ] {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    directory
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

fn walk(root: &Path, capture: &Capture) -> (Extraction, IncrementalStats) {
    let (extraction, _, stats) = extract_captured_dir(
        root,
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    (extraction, stats)
}

fn with_configs(selection: &Selection) -> Vec<String> {
    let mut paths = selection.files.clone();
    paths.extend(selection.configs.iter().cloned());
    paths.sort();
    paths.dedup();
    paths
}

fn bound_paths(binding: &ConfigBinding) -> Vec<&str> {
    match binding {
        ConfigBinding::Bound(files) => files.iter().map(|file| file.path.as_str()).collect(),
        ConfigBinding::Unbound => panic!("configs are unbound"),
    }
}

// AC-1090: the selection lists every resolution config the ordinary
// `.gitignore`-aware walk reads, and none it skips; a bundler config is a
// source and is listed with the files too.
#[test]
fn the_selection_lists_the_configs_completion_reads() {
    let directory = tree();
    let selection = enumerate_selection(directory.path()).unwrap();
    assert_eq!(
        selection.configs,
        [
            "package.json",
            "packages/lib/package.json",
            "tsconfig.json",
            "vite.config.ts"
        ]
    );
    assert!(selection.files.contains(&"vite.config.ts".to_string()));
    let (_, reads) = crate::resolution::ResolutionIndex::load_reading(directory.path()).unwrap();
    let read: Vec<&str> = reads.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(read, selection.configs);
}

// AC-1090: a capture retaining the configs binds each to its retained file,
// and the walk's facts equal those of a capture without them.
#[test]
fn retained_configs_are_bound_to_the_capture() {
    let directory = tree();
    let root = directory.path();
    let selection = enumerate_selection(root).unwrap();
    let held = capture(root, &with_configs(&selection));
    let (bound, stats) = walk(root, &held);
    assert_eq!(bound_paths(&stats.resolution_configs), selection.configs);
    let ConfigBinding::Bound(files) = &stats.resolution_configs else {
        unreachable!()
    };
    for file in files {
        assert_eq!(file, held.file(&file.path).unwrap().reference());
    }
    let plain = capture(root, &selection.files);
    let (unbound, stats) = walk(root, &plain);
    assert_eq!(stats.resolution_configs, ConfigBinding::Unbound);
    assert_eq!(bound.nodes, unbound.nodes);
    assert_eq!(bound.edges, unbound.edges);
}

// AC-1090: a config changed, added or removed after capture leaves the
// configs unbound, since completion read bytes the capture does not hold.
#[test]
fn a_config_the_capture_does_not_hold_leaves_them_unbound() {
    for change in ["edit", "add", "remove"] {
        let directory = tree();
        let root = directory.path();
        let selection = enumerate_selection(root).unwrap();
        let held = capture(root, &with_configs(&selection));
        match change {
            "edit" => fs::write(root.join("tsconfig.json"), "{}").unwrap(),
            "add" => fs::write(root.join("packages/lib/tsconfig.json"), "{}").unwrap(),
            _ => fs::remove_file(root.join("packages/lib/package.json")).unwrap(),
        }
        let (_, stats) = walk(root, &held);
        assert_eq!(stats.resolution_configs, ConfigBinding::Unbound, "{change}");
    }
}
