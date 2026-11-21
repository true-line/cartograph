//! AC-1012 (T-1012): a new project removes the loaded system and keeps the
//! application's settings.
use super::*;
use core_graph::{GraphStore, Node};

fn open(dir: &Path) -> crate::AppState {
    crate::open_app_state(dir).unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// Every entry a launch makes is either removed by a new project or kept, so
// a new store cannot be left out of it.
#[test]
fn every_entry_a_launch_makes_is_removed_or_kept() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "a".into(),
            label: "Resource".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    let made = entries(dir.path());
    drop(state);
    let unlisted: Vec<&String> = made
        .iter()
        .filter(|name| !REMOVED.contains(&name.as_str()) && !KEPT.contains(&name.as_str()))
        .collect();
    assert!(unlisted.is_empty(), "not removed or kept: {unlisted:?}");
}

#[test]
fn a_new_project_starts_empty_and_keeps_the_settings() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "a".into(),
            label: "Resource".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    state.jobs.lock().unwrap().enqueue("ingest").unwrap();
    let chosen = state
        .settings
        .lock()
        .unwrap()
        .set_local_model("qwen3:8b")
        .unwrap();
    std::fs::create_dir_all(dir.path().join("adapters")).unwrap();
    std::fs::write(dir.path().join("adapters/kept.txt"), b"kept").unwrap();
    drop(state);

    // Nothing changes until the next launch.
    request(dir.path()).unwrap();
    assert!(pending(dir.path()));
    let state = open(dir.path());
    assert!(!pending(dir.path()));
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert!(nodes.is_empty() && edges.is_empty());
    assert!(state.jobs.lock().unwrap().list().unwrap().is_empty());
    assert_eq!(
        state.settings.lock().unwrap().local_model().unwrap(),
        chosen
    );
    assert_eq!(
        std::fs::read(dir.path().join("adapters/kept.txt")).unwrap(),
        b"kept"
    );
    drop(state);

    // A launch with nothing pending changes nothing.
    let state = open(dir.path());
    assert_eq!(
        state.settings.lock().unwrap().local_model().unwrap(),
        chosen
    );
}

// A launch that stopped after setting the old state aside finishes the work.
#[test]
fn a_new_project_interrupted_part_way_is_finished_by_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let chosen = state
        .settings
        .lock()
        .unwrap()
        .set_local_model("qwen3:8b")
        .unwrap();
    state.jobs.lock().unwrap().enqueue("ingest").unwrap();
    drop(state);
    request(dir.path()).unwrap();
    std::fs::rename(dir.path().join("state.db"), dir.path().join(OLD_STATE)).unwrap();
    std::fs::write(dir.path().join("state.db"), b"").unwrap();

    let state = open(dir.path());
    assert!(!pending(dir.path()) && !dir.path().join(OLD_STATE).exists());
    assert!(state.jobs.lock().unwrap().list().unwrap().is_empty());
    assert_eq!(
        state.settings.lock().unwrap().local_model().unwrap(),
        chosen
    );
}

// A launch that stopped after the new state was complete but before the old
// one was removed keeps the new state and removes the old.
#[test]
fn a_new_project_interrupted_after_finishing_keeps_the_new_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    let chosen = state
        .settings
        .lock()
        .unwrap()
        .set_local_model("qwen3:8b")
        .unwrap();
    drop(state);
    request(dir.path()).unwrap();
    drop(open(dir.path()));
    // As if the old state's removal had not happened.
    std::fs::write(dir.path().join(OLD_STATE), b"stale").unwrap();

    let state = open(dir.path());
    assert!(!dir.path().join(OLD_STATE).exists());
    assert_eq!(
        state.settings.lock().unwrap().local_model().unwrap(),
        chosen
    );
}
