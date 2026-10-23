//! Runtime-only host grants; durable principal identity is not authority.
use super::McpClientId;
use std::{
    num::NonZeroUsize,
    sync::{Arc, RwLock},
};

/// Observation only. It cannot be deserialized or used to authorize an operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct McpGrantSnapshot {
    /// Host-selected durable principal.
    pub client_id: McpClientId,
    /// Explicit owner limit for new durable nonce identities.
    pub maximum_identities: NonZeroUsize,
}

struct Identity {
    snapshot: McpGrantSnapshot,
    authorized: bool,
}

/// A revoked, absent or poisoned grant. No private state is disclosed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("MCP task grant unavailable")]
pub struct McpGrantUnavailable;

/// One host activation. Default is read-only; dropping it revokes its sessions.
/// This component neither authenticates peers nor validates durable registration.
#[derive(Default)]
pub struct McpTaskActivation(Arc<RwLock<Option<Identity>>>);

/// An authenticated host session's shared view of one activation, not a credential.
#[derive(Clone)]
pub struct McpTaskSession(Arc<RwLock<Option<Identity>>>);

impl McpTaskActivation {
    /// Host-only construction after owner consent and durable principal validation.
    /// Never call this from untrusted transport arguments or a saved snapshot.
    pub fn granted(client_id: McpClientId, maximum_identities: NonZeroUsize) -> Self {
        Self(Arc::new(RwLock::new(Some(Identity {
            snapshot: McpGrantSnapshot {
                client_id,
                maximum_identities,
            },
            authorized: true,
        }))))
    }

    /// Share revocation state with a host-created session.
    pub fn session(&self) -> McpTaskSession {
        McpTaskSession(Arc::clone(&self.0))
    }

    /// Observe current permission; the returned value is immediately staleable.
    pub fn owner_snapshot(&self) -> Result<Option<McpGrantSnapshot>, McpGrantUnavailable> {
        let state = self.0.read().map_err(|_| McpGrantUnavailable)?;
        Ok(state
            .as_ref()
            .filter(|i| i.authorized)
            .map(|i| i.snapshot.clone()))
    }

    /// Retain only the selected identity for owner reconfirmation, not permission.
    pub fn selected_client(&self) -> Result<Option<McpClientId>, McpGrantUnavailable> {
        let state = self.0.read().map_err(|_| McpGrantUnavailable)?;
        Ok(state.as_ref().map(|i| i.snapshot.client_id.clone()))
    }

    /// Revoke old sessions before creating an independent read-only successor.
    /// Actual endpoint credential rotation remains the host's responsibility.
    pub fn read_only_successor(&self) -> Result<Self, McpGrantUnavailable> {
        let mut state = self.0.write().map_err(|_| McpGrantUnavailable)?;
        let successor = state.as_mut().map(|identity| {
            identity.authorized = false;
            Identity {
                snapshot: identity.snapshot.clone(),
                authorized: false,
            }
        });
        Ok(Self(Arc::new(RwLock::new(successor))))
    }

    /// Stop new admission; already-admitted durable work is not cancelled.
    pub fn revoke(&self) {
        // Poison already denies all readers. Never recover authority from it.
        if let Ok(mut state) = self.0.write()
            && let Some(identity) = state.as_mut()
        {
            identity.authorized = false;
        }
    }
}

impl Drop for McpTaskActivation {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl McpTaskSession {
    /// Hold authority through one bounded synchronous host admission operation.
    /// Revocation waits for this operation; later operations fail closed. Do not
    /// block on network/provider work or reenter activation mutation in `operation`.
    /// The identity/cap are data, not a detachable grant or permission to execute later.
    pub fn with_grant<T>(
        &self,
        operation: impl FnOnce(&McpClientId, NonZeroUsize) -> T,
    ) -> Result<T, McpGrantUnavailable> {
        let state = self.0.read().map_err(|_| McpGrantUnavailable)?;
        let identity = state
            .as_ref()
            .filter(|i| i.authorized)
            .ok_or(McpGrantUnavailable)?;
        Ok(operation(
            &identity.snapshot.client_id,
            identity.snapshot.maximum_identities,
        ))
    }
}

#[cfg(test)]
mod tests;
