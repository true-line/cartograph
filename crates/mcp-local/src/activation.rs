//! Explicit Unix activation. Drop requests cancellation immediately; filesystem
//! cleanup runs when the owner task is polled. Await `disable` to verify cleanup.
//! Blocking application workers remain the host's separately admitted ownership.
use crate::{
    connection::Connection,
    endpoint::Endpoint,
    serve::{ServeError, Session, serve_connection},
};
use mcp_wire::capability::Capability;
use std::{
    future::{Future, poll_fn},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::pin,
    sync::Arc,
    task::Poll,
    time::Duration,
};
use tokio::{
    sync::watch,
    task::{JoinHandle, JoinSet},
};

/// Every resource policy is supplied by the host; there are no policy defaults.
#[derive(Clone, Copy)]
pub struct ActivationLimits {
    /// Maximum simultaneous connections, including unauthenticated peers.
    pub max_connections: NonZeroUsize,
    /// Maximum incoming serialized frame bytes.
    pub read_frame_bytes: NonZeroUsize,
    /// Maximum outgoing serialized frame bytes.
    pub write_frame_bytes: NonZeroUsize,
    /// Nonzero deadline for an entire frame.
    pub frame_deadline: Duration,
    /// Nonzero total lifetime including authentication and application work.
    pub session_lifetime: Duration,
}
/// Fixed failures contain no endpoint paths or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ActivationError {
    /// Invalid duration or unavailable runtime.
    #[error("invalid local activation limits or runtime")]
    InvalidLimits,
    /// Endpoint creation or acceptance failed.
    #[error("local activation endpoint failed")]
    Endpoint,
    /// Owner task failed or was cancelled.
    #[error("local activation task failed")]
    Task,
    /// Endpoint record removal could not be verified.
    #[error("local activation cleanup failed")]
    Cleanup,
}
/// The owner publishes failures before cleanup awaits; stopped never means enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStatus {
    /// Owner is accepting connections.
    Running,
    /// Owner stopped accepting and is cleaning up.
    Stopped,
    /// Owner failed and stopped accepting.
    Failed(ActivationError),
}

/// One fresh capability and endpoint, disabled by default until explicitly created.
pub struct Activation {
    record: PathBuf,
    revoked: watch::Sender<bool>,
    status: watch::Receiver<ActivationStatus>,
    task: Option<JoinHandle<Result<(), ActivationError>>>,
}
impl Activation {
    /// Requires a running Tokio runtime. Factory execution starts only after auth.
    pub fn start<F, Fut, S>(
        parent: &Path,
        limits: ActivationLimits,
        factory: F,
    ) -> Result<Self, ActivationError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<S, ServeError>> + Send + 'static,
        S: Session + 'static,
    {
        if tokio::runtime::Handle::try_current().is_err()
            || [limits.frame_deadline, limits.session_lifetime]
                .into_iter()
                .any(|d| d.is_zero() || std::time::Instant::now().checked_add(d).is_none())
        {
            return Err(ActivationError::InvalidLimits);
        }
        let capability = Arc::new(Capability::generate().map_err(|_| ActivationError::Endpoint)?);
        let mut endpoint = Endpoint::bind(parent).map_err(|_| ActivationError::Endpoint)?;
        let record = endpoint
            .publish(&capability)
            .map_err(|_| ActivationError::Endpoint)?;
        let (revoked, mut updates) = watch::channel(false);
        let child_revoke = revoked.clone();
        let (state, status) = watch::channel(ActivationStatus::Running);
        let factory = Arc::new(factory);
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            let result = loop {
                while tasks.try_join_next().is_some() {}
                if *updates.borrow() {
                    break Ok(());
                }
                let event = {
                    let accept = endpoint.accept();
                    let changed = updates.changed();
                    let mut accept = pin!(accept);
                    let mut changed = pin!(changed);
                    poll_fn(|cx| {
                        if changed.as_mut().poll(cx).is_ready() {
                            return Poll::Ready(None);
                        }
                        if !tasks.is_empty() && tasks.poll_join_next(cx).is_ready() {
                            return Poll::Ready(Some(None));
                        }
                        accept.as_mut().poll(cx).map(|r| Some(Some(r)))
                    })
                    .await
                };
                let Some(event) = event else {
                    break Ok(());
                };
                let Some(accepted) = event else {
                    continue;
                };
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(_) => break Err(ActivationError::Endpoint),
                };
                // Finished tasks cannot consume capacity merely awaiting reaping.
                while tasks.try_join_next().is_some() {}
                if tasks.len() >= limits.max_connections.get() {
                    drop(stream);
                    continue;
                }
                let connection = match Connection::from_stream(
                    stream,
                    limits.read_frame_bytes,
                    limits.write_frame_bytes,
                    limits.frame_deadline,
                ) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let capability = capability.clone();
                let factory = factory.clone();
                let receiver = child_revoke.subscribe();
                tasks.spawn(async move {
                    serve_connection(
                        connection,
                        &capability,
                        limits.session_lifetime,
                        receiver,
                        move || async move { factory().await },
                    )
                    .await
                });
            };
            state.send_replace(match result {
                Ok(()) => ActivationStatus::Stopped,
                Err(error) => ActivationStatus::Failed(error),
            });
            child_revoke.send_replace(true);
            drop(endpoint);
            tasks.shutdown().await;
            result
        });
        Ok(Self {
            record,
            revoked,
            status,
            task: Some(task),
        })
    }
    /// Explicit private record path for a relay.
    pub fn record_path(&self) -> &Path {
        &self.record
    }
    /// Current owner state, including unexpected task completion.
    pub fn status(&self) -> ActivationStatus {
        let state = *self.status.borrow();
        if state == ActivationStatus::Running
            && self.task.as_ref().is_some_and(JoinHandle::is_finished)
        {
            ActivationStatus::Failed(ActivationError::Task)
        } else {
            state
        }
    }
    /// Stop acceptance, revoke sessions, drain task ownership and verify removal.
    pub async fn disable(mut self) -> Result<(), ActivationError> {
        self.revoked.send_replace(true);
        let result = self
            .task
            .take()
            .ok_or(ActivationError::Task)?
            .await
            .map_err(|_| ActivationError::Task)?;
        match std::fs::symlink_metadata(&self.record) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(ActivationError::Cleanup),
        }
        result
    }
}
impl Drop for Activation {
    fn drop(&mut self) {
        self.revoked.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
#[cfg(test)]
mod tests;
