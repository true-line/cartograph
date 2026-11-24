//! A captured walk binds directory completion's other live lookups to the
//! capture only when the capture answers each as the live tree did
//! (ADR-0072 slice 6, AC-1092).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};
use std::fs;

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const NEXT: &str = r#"{"name":"web","dependencies":{"next":"14.0.0"}}"#;

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    directory
}

/// Capture the host's selection (sources and configs) and walk it.
fn walk(root: &Path) -> (Capture, IncrementalStats) {
    let selection = enumerate_selection(root).unwrap();
    let mut paths = selection.files.clone();
    paths.extend(selection.configs.iter().cloned());
    paths.sort();
    paths.dedup();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let (_, _, stats) = extract_captured_dir(
        root,
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    (capture, stats)
}

fn absent(binding: &ProbeBinding) -> Vec<&str> {
    match binding {
        ProbeBinding::Bound { absent, .. } => absent.iter().map(String::as_str).collect(),
        ProbeBinding::Unbound => panic!("lookups are unbound"),
    }
}

// AC-1092: a Next router root looked up and not found is bound as absent
// from the capture; one found is covered by the files the capture holds.
#[test]
fn router_lookups_the_capture_answers_are_bound() {
    let directory = tree(&[
        ("package.json", NEXT),
        (
            "src/app/page.tsx",
            "export default function Home() { return null; }\n",
        ),
    ]);
    let (_, stats) = walk(directory.path());
    assert_eq!(absent(&stats.completion_probes), ["app"]);
    let directory = tree(&[
        ("package.json", NEXT),
        (
            "app/page.tsx",
            "export default function Home() { return null; }\n",
        ),
    ]);
    let (_, stats) = walk(directory.path());
    assert_eq!(absent(&stats.completion_probes), Vec::<&str>::new());
}

// AC-1092: a router root that exists but holds no captured file, such as
// one holding only a stylesheet, leaves the lookups unbound.
#[test]
fn a_router_root_the_capture_cannot_show_leaves_them_unbound() {
    let directory = tree(&[
        ("package.json", NEXT),
        ("app/globals.css", "body {}\n"),
        (
            "src/app/page.tsx",
            "export default function Home() { return null; }\n",
        ),
    ]);
    let (_, stats) = walk(directory.path());
    assert_eq!(stats.completion_probes, ProbeBinding::Unbound);
}

// AC-1092: an imported non-source file that is missing is bound as absent;
// one that exists is not retained, so it leaves the lookups unbound.
#[test]
fn asset_lookups_are_bound_only_when_absent() {
    let source = "import './styles.css';\nexport const x = 1;\n";
    let directory = tree(&[("src/app.ts", source)]);
    let (_, stats) = walk(directory.path());
    assert_eq!(absent(&stats.completion_probes), ["src/styles.css"]);
    let directory = tree(&[("src/app.ts", source), ("src/styles.css", "body {}\n")]);
    let (_, stats) = walk(directory.path());
    assert_eq!(stats.completion_probes, ProbeBinding::Unbound);
}

// AC-1092: a dependency type allowlist lets completion read installed
// packages, which no capture holds, so its presence leaves them unbound.
#[test]
fn a_dependency_type_allowlist_leaves_them_unbound() {
    let directory = tree(&[
        ("src/app.ts", "export const x = 1;\n"),
        (".cartograph/types.toml", "read = []\n"),
    ]);
    let (_, stats) = walk(directory.path());
    assert_eq!(stats.completion_probes, ProbeBinding::Unbound);
}

// AC-1092: a root `vercel.json` is selected, and a walk that reads it binds
// it to its retained file.
#[test]
fn a_read_root_file_is_bound_to_its_retained_bytes() {
    let directory = tree(&[
        ("vercel.json", "{}"),
        (
            "api/hello.ts",
            "export default function handler(req, res) { res.send('hi'); }\n",
        ),
    ]);
    let root = directory.path();
    assert!(
        enumerate_selection(root)
            .unwrap()
            .configs
            .contains(&"vercel.json".to_string())
    );
    let (capture, stats) = walk(root);
    let ProbeBinding::Bound { read, .. } = &stats.completion_probes else {
        panic!("lookups are unbound");
    };
    assert_eq!(
        read,
        &[capture.file("vercel.json").unwrap().reference().clone()]
    );
}

// AC-1092: a path looked up as a directory and not found is absent only
// when the capture holds nothing there, not even a file of that name.
#[test]
fn an_absent_lookup_the_capture_holds_leaves_them_unbound() {
    let mut probes = crate::Probes::default();
    probes
        .exists
        .insert("app".into(), (crate::Lookup::Directory, false));
    let directory = tree(&[("app", "not a directory\n"), ("src/a.ts", "")]);
    let root = directory.path();
    let paths = ["app".to_string(), "src/a.ts".to_string()];
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    assert_eq!(bind_probes(&capture, &probes), ProbeBinding::Unbound);
    let mut other = crate::Probes::default();
    other
        .exists
        .insert("pages".into(), (crate::Lookup::Directory, false));
    assert_eq!(
        bind_probes(&capture, &other),
        ProbeBinding::Bound {
            absent: vec!["pages".into()],
            read: Vec::new()
        }
    );
}
