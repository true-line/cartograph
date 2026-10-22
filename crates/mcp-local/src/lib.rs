//! Private local endpoints and authenticated connections for the app and relay.
//! These transport primitives do not implement app read handlers or an MCP server.
//! Windows transport and native security verification are unfinished.

#[cfg(unix)]
pub mod connection;

#[cfg(unix)]
pub mod endpoint;

#[cfg(unix)]
pub mod record;

#[cfg(unix)]
pub mod handshake;

#[cfg(unix)]
pub mod serve;

/// Shared admission for actual synchronous workers across local connections.
pub mod work_budget;

#[cfg(unix)]
pub mod activation;
