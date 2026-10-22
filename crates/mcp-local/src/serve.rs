//! One already-admitted Unix connection. The host owns listener activation and
//! accepts/peer-verifies connections before passing their ownership here.
//!
//! Factory/dispatch futures must yield and offload synchronous application work
//! to a bounded admitted worker. Cancellation stops awaiting a worker; it cannot
//! preempt a blocking thread. The actual worker must retain its work/memory
//! permit until it returns, even if its connection has already closed.

use crate::{connection::Connection, handshake::authenticate_accepted};
use mcp_wire::capability::Capability;
use serde_json::Value;
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::Duration,
};
use tokio::sync::watch;

/// One application response, without a second MCP admission/dispatch layer.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// Serialize this value under the connection's supplied frame cap.
    Respond(Value),
    /// A notification with no response.
    Ignore,
    /// Close the connection and drop the application session.
    Close,
}

/// Application-side sequential adapter; the host implements actual MCP policy.
pub trait Session: Send {
    /// Dispatch one message. Implementations must not block the executor; bounded
    /// synchronous work belongs on a separately admitted blocking worker.
    fn dispatch(
        &mut self,
        message: Value,
    ) -> impl Future<Output = Result<Reply, ServeError>> + Send;
}

/// Fixed failures never contain credentials, paths, source or backend errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServeError {
    /// A zero or unrepresentable whole-session lifetime was configured.
    #[error("invalid local session lifetime")]
    InvalidLifetime,
    /// Local capability authentication did not complete.
    #[error("local authentication failed")]
    Authentication,
    /// A bounded frame or native connection failed.
    #[error("local session transport failed")]
    Transport,
    /// Application/session preparation or dispatch failed.
    #[error("local session application failed")]
    Application,
    /// Host revoked the connection, or dropped its activation sender.
    #[error("local session revoked")]
    Revoked,
    /// Total authentication/preparation/dispatch/transport lifetime expired.
    #[error("local session expired")]
    Expired,
}

/// Authenticate before invoking `factory`, then serve one request at a time.
/// A true initial revocation value, any unseen update, or sender closure revokes
/// fail-closed. Do not reuse/reset a revocation receiver across activations.
/// EOF/Close succeeds; failure, revocation, expiry or external cancellation drops
/// the owned transport and pending application future. Lifetime includes auth.
pub async fn serve_connection<F, Fut, S>(
    connection: Connection,
    capability: &Capability,
    lifetime: Duration,
    revoked: watch::Receiver<bool>,
    factory: F,
) -> Result<(), ServeError>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<S, ServeError>> + Send,
    S: Session,
{
    if lifetime.is_zero() || std::time::Instant::now().checked_add(lifetime).is_none() {
        return Err(ServeError::InvalidLifetime);
    }
    active(&revoked)?;
    let mut updates = revoked.clone();
    let operation = async move {
        let mut connection = authenticate_accepted(connection, capability)
            .await
            .map_err(|_| ServeError::Authentication)?;
        // Authentication may have yielded while activation was revoked. Never
        // acquire application state based solely on the earlier active check.
        active(&revoked)?;
        let mut session = factory().await?;
        active(&revoked)?;
        loop {
            let Some(message) = connection.read().await.map_err(|_| ServeError::Transport)? else {
                return Ok(());
            };
            active(&revoked)?;
            let reply = session.dispatch(message).await?;
            active(&revoked)?;
            match reply {
                Reply::Respond(response) => connection
                    .write(&response)
                    .await
                    .map_err(|_| ServeError::Transport)?,
                Reply::Ignore => {}
                Reply::Close => return Ok(()),
            }
        }
    };
    let expiration = tokio::time::timeout(lifetime, operation);
    let revocation = async move {
        let _ = updates.changed().await;
        Err(ServeError::Revoked)
    };
    let (mut expiration, mut revocation) = (pin!(expiration), pin!(revocation));
    poll_fn(|cx| {
        if let Poll::Ready(result) = revocation.as_mut().poll(cx) {
            return Poll::Ready(result);
        }
        expiration
            .as_mut()
            .poll(cx)
            .map(|result| result.unwrap_or(Err(ServeError::Expired)))
    })
    .await
}

fn active(revoked: &watch::Receiver<bool>) -> Result<(), ServeError> {
    if *revoked.borrow() || revoked.has_changed().unwrap_or(true) {
        Err(ServeError::Revoked)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
