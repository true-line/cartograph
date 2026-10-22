//! App/relay connection authentication; no discovery, store reads or app launch.
use crate::{connection::Connection, record::ConnectionRecord};
use std::{num::NonZeroUsize, time::Duration};

/// Source-free handshake error. Credentials are never reflected to the client.
#[derive(Debug, thiserror::Error)]
#[error("local authentication failed or app unavailable")]
pub struct HandshakeError;

/// Verify the recorded endpoint, connect, then complete the internal handshake.
/// On failure the connection is dropped; no MCP input has been forwarded.
pub async fn connect_authenticated(
    record: &ConnectionRecord,
    read_cap: NonZeroUsize,
    write_cap: NonZeroUsize,
    deadline: Duration,
) -> Result<Connection, HandshakeError> {
    let mut connection = record
        .connect(read_cap, write_cap, deadline)
        .await
        .map_err(|_| HandshakeError)?;
    connection
        .write(&record.authentication_preamble())
        .await
        .map_err(|_| HandshakeError)?;
    let response = connection
        .read()
        .await
        .map_err(|_| HandshakeError)?
        .ok_or(HandshakeError)?;
    if !mcp_wire::capability::is_acknowledgement(&response) {
        return Err(HandshakeError);
    }
    Ok(connection)
}

/// Authenticate a peer-verified accepted connection before acquiring app context.
/// Failure or cancellation owns and drops the connection; no retry on that stream
/// and no protocol/application frame is dispatched before the exact ACK completes.
pub async fn authenticate_accepted(
    mut connection: Connection,
    capability: &mcp_wire::capability::Capability,
) -> Result<Connection, HandshakeError> {
    let preamble = connection
        .read()
        .await
        .map_err(|_| HandshakeError)?
        .ok_or(HandshakeError)?;
    if !capability.authenticates_preamble(&preamble) {
        return Err(HandshakeError);
    }
    connection
        .write(&mcp_wire::capability::acknowledgement())
        .await
        .map_err(|_| HandshakeError)?;
    Ok(connection)
}
