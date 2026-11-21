//! AC-1013 (T-1013) and AC-1014 (T-1014): a new project starts empty and
//! keeps the application's settings; switching loads a parked project whole.
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

fn put(state: &crate::AppState, id: &str) {
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: id.into(),
            label: "Resource".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
}

fn node_ids(state: &crate::AppState) -> Vec<String> {
    let (nodes, _) = state.graph.lock().unwrap().read_snapshot().unwrap();
    nodes.into_iter().map(|node| node.id).collect()
}

fn local_model(state: &crate::AppState) -> crate::settings::LocalModelChoice {
    state.settings.lock().unwrap().local_model().unwrap()
}

// Every entry a launch makes is either the project's or the application's,
// so a new store cannot be left out of a project.
#[test]
fn every_entry_a_launch_makes_is_the_projects_or_kept() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    put(&state, "a");
    active(dir.path()).unwrap();
    let made = entries(dir.path());
    drop(state);
    let unlisted: Vec<&String> = made
        .iter()
        .filter(|name| !PROJECT_ENTRIES.contains(&name.as_str()) && !KEPT.contains(&name.as_str()))
        .collect();
    assert!(unlisted.is_empty(), "not listed: {unlisted:?}");
}

#[test]
fn a_new_project_starts_empty_and_keeps_the_settings() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    put(&state, "a");
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
    let first = active(dir.path()).unwrap();

    // Nothing changes until the next launch.
    request(dir.path(), Change::New).unwrap();
    assert_eq!(pending_change(dir.path()).unwrap(), Some(Change::New));
    let state = open(dir.path());
    assert_eq!(pending_change(dir.path()).unwrap(), None);
    assert!(node_ids(&state).is_empty());
    assert!(state.jobs.lock().unwrap().list().unwrap().is_empty());
    assert_eq!(local_model(&state), chosen);
    assert_eq!(
        std::fs::read(dir.path().join("adapters/kept.txt")).unwrap(),
        b"kept"
    );
    // The first project is parked, not deleted.
    let listed = list(dir.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[0].active && listed[0].meta.id != first.id);
    assert_eq!((listed[1].active, &listed[1].meta.id), (false, &first.id));
    drop(state);

    // A launch with nothing pending changes nothing.
    let state = open(dir.path());
    assert_eq!(local_model(&state), chosen);
}

#[test]
fn switching_loads_a_parked_project_whole_and_settings_follow_the_application() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    put(&state, "simplelogin");
    drop(state);
    let first = rename(dir.path(), &active(dir.path()).unwrap().id, "SimpleLogin").unwrap();
    request(dir.path(), Change::New).unwrap();
    let state = open(dir.path());
    put(&state, "vendure");
    // A setting changed in the second project stays when switching back.
    let chosen = state
        .settings
        .lock()
        .unwrap()
        .set_local_model("qwen3:8b")
        .unwrap();
    drop(state);
    let second = active(dir.path()).unwrap();

    request(
        dir.path(),
        Change::Switch {
            to: first.id.clone(),
        },
    )
    .unwrap();
    let state = open(dir.path());
    assert_eq!(node_ids(&state), vec!["simplelogin".to_owned()]);
    assert_eq!(local_model(&state), chosen);
    assert_eq!(
        active(dir.path()).unwrap().name.as_deref(),
        Some("SimpleLogin")
    );
    drop(state);

    request(
        dir.path(),
        Change::Switch {
            to: second.id.clone(),
        },
    )
    .unwrap();
    let state = open(dir.path());
    assert_eq!(node_ids(&state), vec!["vendure".to_owned()]);
    drop(state);

    // A parked project can be renamed and deleted; the loaded one cannot be
    // deleted, and a missing one cannot be loaded.
    rename(dir.path(), &first.id, "Login").unwrap();
    assert_eq!(
        list(dir.path()).unwrap()[1].meta.name.as_deref(),
        Some("Login")
    );
    assert!(delete(dir.path(), &second.id).is_err());
    delete(dir.path(), &first.id).unwrap();
    assert_eq!(list(dir.path()).unwrap().len(), 1);
    assert!(request(dir.path(), Change::Switch { to: first.id }).is_err());
}

// A launch that stopped part way through a switch is finished by the next
// one, and no step runs twice.
#[test]
fn a_change_interrupted_part_way_is_finished_by_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path());
    put(&state, "first");
    drop(state);
    let first = active(dir.path()).unwrap();
    request(dir.path(), Change::New).unwrap();
    let state = open(dir.path());
    put(&state, "second");
    drop(state);

    // Park ran, then the launch stopped before loading the chosen project.
    request(
        dir.path(),
        Change::Switch {
            to: first.id.clone(),
        },
    )
    .unwrap();
    let parked = park(dir.path()).unwrap();
    write_json(
        &dir.path().join(MARKER),
        &Pending {
            change: Change::Switch {
                to: first.id.clone(),
            },
            parked: Some(parked.clone()),
        },
    )
    .unwrap();
    // Half the chosen project was already moved back.
    std::fs::rename(
        parked_dir(dir.path(), &first.id).join("graph.db"),
        dir.path().join("graph.db"),
    )
    .unwrap();

    let state = open(dir.path());
    assert_eq!(node_ids(&state), vec!["first".to_owned()]);
    assert_eq!(pending_change(dir.path()).unwrap(), None);
    let listed = list(dir.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].meta.id, parked);
    drop(state);
    request(dir.path(), Change::Switch { to: parked }).unwrap();
    assert_eq!(node_ids(&open(dir.path())), vec!["second".to_owned()]);
}

#[test]
fn a_project_name_is_one_short_line() {
    let dir = tempfile::tempdir().unwrap();
    let id = active(dir.path()).unwrap().id;
    assert!(rename(dir.path(), &id, "two\nlines").is_err());
    assert!(rename(dir.path(), &id, &"x".repeat(121)).is_err());
    assert_eq!(rename(dir.path(), &id, "  ").unwrap().name, None);
    assert!(rename(dir.path(), "0123456789abcdef", "x").is_err());
}
