use super::*;
use agents::investigation::McpClientId;
use mcp_local::record::ConnectionRecord;
use std::{
    future::Future,
    num::NonZeroUsize,
    task::{Context, Poll, Waker},
};

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}
fn app() -> crate::test_app::TestApp {
    crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    )
}
fn policy() -> Policy {
    super::super::tests::policy()
}

fn task_fixture() -> (crate::test_app::TestApp, Policy) {
    use tauri::Manager;
    let root = tempfile::tempdir().unwrap();
    let app = app();
    app.manage(crate::registered_source_tests::app_state(root.path()));
    let mut policy = policy();
    policy.worker = crate::mcp_reads::worker::tests::task_limits(&app.state::<crate::AppState>());
    let transport =
        crate::mcp_reads::reservation::TransportReservation::for_limits(policy.transport)
            .unwrap()
            .activation_bytes;
    policy.aggregate.max_reserved_bytes = nz(transport
        .checked_add(policy.worker.reserved_bytes.get())
        .unwrap());
    app.manage(root);
    (app, policy)
}

// AC-0320/0321: host validation precedes endpoint creation; no task is dispatched.
#[test]
fn host_activation_validates_principal_and_revokes_before_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let mut store = crate::jobs::JobStore::open(dir.path().join("jobs.sqlite")).unwrap();
    let client = store.create_mcp_client(nz(1)).unwrap();
    let (app, task_policy) = task_fixture();
    let service = Service::default();
    assert_eq!(
        service
            .enable_selected(
                app.handle().clone(),
                parent.path(),
                task_policy,
                Some((&store, McpClientId::from_host_bytes([0; 16]), nz(1)))
            )
            .unwrap_err(),
        ReadError::InvalidRequest
    );
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    tauri::async_runtime::block_on(async {
        service
            .enable_selected(
                app.handle().clone(),
                parent.path(),
                task_policy,
                Some((&store, client.clone(), nz(1))),
            )
            .unwrap();
        let session = service
            .0
            .lock()
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .tasks
            .session();
        assert_eq!(session.with_grant(|id, _| id.clone()).unwrap(), client);
        let observed = service.owner_status().unwrap();
        assert_eq!(observed["selected_client"], client.as_str());
        assert_eq!(observed["service"], "running");
        assert_eq!(observed["task_dispatch_available"], true);
        assert_eq!(observed["task_grant"]["client_id"], client.as_str());
        let cleanup = service.begin_stop(false).unwrap();
        assert!(session.with_grant(|_, _| ()).is_err());
        Service::await_cleanup(cleanup).await.unwrap();
        service
            .enable(app.handle().clone(), parent.path(), policy())
            .unwrap();
        assert!(
            service
                .0
                .lock()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .tasks
                .session()
                .with_grant(|_, _| ())
                .is_err()
        );
        assert!(session.with_grant(|_, _| ()).is_err());
        service.disable().await.unwrap();
    });
}

// AC-0321: actual private endpoint rotation invalidates old credentials and grants.
#[test]
fn host_rotation_preserves_selection_without_restoring_authority() {
    let dir = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let mut store = crate::jobs::JobStore::open(dir.path().join("jobs.sqlite")).unwrap();
    let client = store.create_mcp_client(nz(1)).unwrap();
    let (app, task_policy) = task_fixture();
    let service = Service::default();
    tauri::async_runtime::block_on(async {
        let old = service
            .enable_selected(
                app.handle().clone(),
                parent.path(),
                task_policy,
                Some((&store, client.clone(), nz(1))),
            )
            .unwrap();
        let old_record = ConnectionRecord::read(&old).unwrap();
        let session = service
            .0
            .lock()
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .tasks
            .session();
        let new = service
            .rotate(app.handle().clone(), parent.path())
            .await
            .unwrap();
        let observed = service.owner_status().unwrap();
        assert_eq!(observed["selected_client"], client.as_str());
        assert_eq!(observed["task_dispatch_available"], true);
        assert!(observed["task_grant"].is_null());
        assert_ne!(old, new);
        assert!(!old.exists());
        assert!(session.with_grant(|_, _| ()).is_err());
        {
            let state = service.0.lock().unwrap();
            let tasks = &state.active.as_ref().unwrap().tasks;
            assert_eq!(tasks.selected_client().unwrap(), Some(client));
            assert!(tasks.owner_snapshot().unwrap().is_none());
            assert!(tasks.session().with_grant(|_, _| ()).is_err());
        }
        let limits = task_policy.transport;
        assert!(
            old_record
                .connect(
                    limits.read_frame_bytes,
                    limits.write_frame_bytes,
                    limits.frame_deadline
                )
                .await
                .is_err()
        );
        let fresh = ConnectionRecord::read(&new).unwrap();
        let mut stale = fresh
            .connect(
                limits.read_frame_bytes,
                limits.write_frame_bytes,
                limits.frame_deadline,
            )
            .await
            .unwrap();
        stale
            .write(&old_record.authentication_preamble())
            .await
            .unwrap();
        assert!(matches!(stale.read().await, Ok(None) | Err(_)));
        service.disable().await.unwrap();
        assert!(!new.exists());
    });
}

// AC-0321/0325: cancellation releases the fence, and stop invalidates its token.
#[test]
fn host_rotation_cancellation_and_stop_cannot_resurrect_activation() {
    let parent = tempfile::tempdir().unwrap();
    let app = app();
    tauri::async_runtime::block_on(async {
        for shutdown in [false, true] {
            let service = Service::default();
            service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            let rotation = service.begin_rotation().unwrap();
            assert!(
                service
                    .enable(app.handle().clone(), parent.path(), policy())
                    .is_err()
            );
            assert!(service.begin_rotation().is_err());
            service.stop(shutdown).await.unwrap();
            assert_eq!(
                service
                    .finish_rotation(app.handle().clone(), parent.path(), rotation)
                    .await
                    .unwrap_err(),
                ReadError::InvalidRequest
            );
            assert!(service.0.lock().unwrap().active.is_none());
        }
        let service = Service::default();
        service
            .enable(app.handle().clone(), parent.path(), policy())
            .unwrap();
        let mut rotation = service.begin_rotation().unwrap();
        let (_complete, barrier) = tokio::sync::watch::channel(None);
        // Control only this waiter's completion; actual cleanup keeps its owner.
        rotation.cleanup = Some(barrier);
        let mut waiting =
            Box::pin(service.finish_rotation(app.handle().clone(), parent.path(), rotation));
        assert_eq!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        );
        drop(waiting);
        assert!(service.0.lock().unwrap().rotation.is_none());
        service.disable().await.unwrap();
        service
            .enable(app.handle().clone(), parent.path(), policy())
            .unwrap();
        service.disable().await.unwrap();
    });
}

#[test]
fn host_rotation_cleanup_failure_never_starts_successor() {
    let parent = tempfile::tempdir().unwrap();
    let app = app();
    let service = Service::default();
    tauri::async_runtime::block_on(async {
        let record = service
            .enable(app.handle().clone(), parent.path(), policy())
            .unwrap();
        std::fs::rename(&record, record.with_file_name("saved-record")).unwrap();
        std::fs::write(&record, b"fixture replacement").unwrap();
        assert_eq!(
            service
                .rotate(app.handle().clone(), parent.path())
                .await
                .unwrap_err(),
            ReadError::Storage
        );
        assert_eq!(service.status().unwrap(), Status::CleanupFailed);
        assert!(service.0.lock().unwrap().rotation.is_none());
        assert!(
            service
                .enable(app.handle().clone(), parent.path(), policy())
                .is_err()
        );
    });
}
