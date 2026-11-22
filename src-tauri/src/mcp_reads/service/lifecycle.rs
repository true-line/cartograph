//! Rotation is a fenced stop/start, never a grant renewal.
use super::*;
use agents::investigation::McpTaskActivation;

struct Rotation {
    service: Arc<Mutex<State>>,
    token: Arc<()>,
    policy: Policy,
    tasks: McpTaskActivation,
    cleanup: Option<CleanupCompletion>,
}
impl Drop for Rotation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.service.lock()
            && state
                .rotation
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.token))
        {
            state.rotation = None;
        }
    }
}

impl Service {
    /// One owner observation. Selection alone is never authority.
    pub(crate) fn owner_status(&self) -> Result<serde_json::Value, ReadError> {
        let state = self.0.lock().map_err(|_| ReadError::Storage)?;
        let selected = state
            .active
            .as_ref()
            .map(|active| active.tasks.selected_client())
            .transpose()
            .map_err(|_| ReadError::Storage)?
            .flatten();
        let grant = state
            .active
            .as_ref()
            .map(|active| active.tasks.owner_snapshot())
            .transpose()
            .map_err(|_| ReadError::Storage)?
            .flatten();
        Ok(serde_json::json!({
            "service": state.status()?,
            "selected_client": selected,
            "task_dispatch_available": true,
            "task_grant": grant,
        }))
    }

    /// Replace endpoint credentials; retain selection, never task authority.
    /// The old activation owns cleanup even if this waiter is cancelled.
    pub(crate) async fn rotate<R: tauri::Runtime>(
        &self,
        app: tauri::AppHandle<R>,
        parent: &Path,
    ) -> Result<PathBuf, ReadError> {
        self.finish_rotation(app, parent, self.begin_rotation()?)
            .await
    }

    async fn finish_rotation<R: tauri::Runtime>(
        &self,
        app: tauri::AppHandle<R>,
        parent: &Path,
        mut rotation: Rotation,
    ) -> Result<PathBuf, ReadError> {
        Self::await_cleanup(rotation.cleanup.take()).await?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ReadError::Storage)?;
        let shared = self.0.clone();
        let parent = parent.to_path_buf();
        let policy = rotation.policy;
        let tasks = std::mem::take(&mut rotation.tasks);
        let token = rotation.token.clone();
        // Keep the Rotation guard in this future: cancellation invalidates the
        // token even if the blocking job has not started. Startup checks it
        // under the same lock as disable/shutdown before creating an endpoint.
        tauri::async_runtime::spawn_blocking(move || {
            let _runtime = runtime.enter();
            Self::enable_transition(&shared, app, &parent, policy, tasks, Some(&token))
        })
        .await
        .map_err(|_| ReadError::Storage)?
    }

    fn begin_rotation(&self) -> Result<Rotation, ReadError> {
        let mut state = self.0.lock().map_err(|_| ReadError::Storage)?;
        if state.rotation.is_some() || state.stopping || state.shutdown || state.cleanup_failed {
            return Err(ReadError::InvalidRequest);
        }
        let active = state.active.as_ref().ok_or(ReadError::InvalidRequest)?;
        if active.activation.status() != ActivationStatus::Running {
            return Err(ReadError::InvalidRequest);
        }
        let policy = active.policy;
        let tasks = active
            .tasks
            .read_only_successor()
            .map_err(|_| ReadError::Storage)?;
        let token = Arc::new(());
        state.rotation = Some(token.clone());
        let cleanup = match self.begin_stop_locked(&mut state) {
            Ok(cleanup) => cleanup,
            Err(error) => {
                state.rotation = None;
                return Err(error);
            }
        };
        Ok(Rotation {
            service: self.0.clone(),
            token,
            policy,
            tasks,
            cleanup,
        })
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests;
