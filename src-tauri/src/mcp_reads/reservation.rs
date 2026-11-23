//! Versioned logical transport payload accounting, independent of worker lifetime.
//!
//! These units cover named buffers and JSON/header roles, not allocator slack,
//! SQLite caches, executor tasks, native stacks, kernel buffers or process RSS.
//! The worker's phase minimum is composed separately in worker_bound.
pub(crate) mod capture;
pub(crate) mod evidence;
pub(crate) mod receipt;
pub(crate) mod schema;
pub(crate) mod scoped_bound;
pub(crate) mod session;
pub(crate) mod worker_bound;

// The transport accounting sizes the Unix-socket MCP service's connections,
// which exist only on Unix; the worker and session bounds are portable.
#[cfg(unix)]
use mcp_local::activation::ActivationLimits;
use serde_json::Value;
use std::mem::size_of;

pub(crate) const TRANSPORT_MODEL: &str = "mcp-transport-payload-v1";

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TransportReservation {
    pub model: &'static str,
    pub per_connection_bytes: usize,
    pub activation_bytes: usize,
}

/// Every legal compact JSON value needs at least one input byte. Charging
/// bytes+1 structural entries also covers the root/container boundary. Every
/// entry receives Value, String and logical map-slot headers, even when absent.
/// The three usize map links are accounting units, not a BTree layout promise.
fn json_payload(bytes: usize) -> Option<usize> {
    let entries = bytes.checked_add(1)?;
    let headers = size_of::<Value>()
        .checked_add(size_of::<String>())?
        .checked_add(size_of::<usize>().checked_mul(3)?)?;
    bytes.checked_add(entries.checked_mul(headers)?)
}

#[cfg(unix)]
impl TransportReservation {
    pub(crate) fn for_limits(limits: ActivationLimits) -> Option<Self> {
        let read = limits.read_frame_bytes.get();
        let write = limits.write_frame_bytes.get();
        // Match Connection::from_stream's retained read-ahead capacity. Reject
        // arithmetic overflow rather than reproducing its saturating fallback.
        let read_ahead = read.checked_add(1)?.min(8192);
        let roles = [
            read_ahead,
            read,                  // raw incoming frame during parsing
            json_payload(read)?,   // parsed message, including pre-authentication
            json_payload(read)?,   // admitted argument clone while input is live
            json_payload(write)?,  // returned response can outlive worker permit
            write,                 // write_frame's bounded serialization buffer
            write.checked_add(1)?, // Connection::write output plus newline
            size_of::<mcp_local::connection::Connection>(),
        ];
        let per_connection_bytes = roles.into_iter().try_fold(0usize, usize::checked_add)?;
        let activation_bytes = per_connection_bytes.checked_mul(limits.max_connections.get())?;
        Some(Self {
            model: TRANSPORT_MODEL,
            per_connection_bytes,
            activation_bytes,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{num::NonZeroUsize, time::Duration};
    fn limits(read: usize, write: usize, connections: usize) -> ActivationLimits {
        ActivationLimits {
            max_connections: NonZeroUsize::new(connections).unwrap(),
            read_frame_bytes: NonZeroUsize::new(read).unwrap(),
            write_frame_bytes: NonZeroUsize::new(write).unwrap(),
            frame_deadline: Duration::from_secs(1),
            session_lifetime: Duration::from_secs(2),
        }
    }
    #[test]
    fn transport_accounting_reserves_unauthenticated_connections_and_terminal_response() {
        // AC-0288: admission must include transport even without a worker.
        let one = TransportReservation::for_limits(limits(100, 200, 1)).unwrap();
        let two = TransportReservation::for_limits(limits(100, 200, 2)).unwrap();
        assert_eq!(two.activation_bytes, one.activation_bytes * 2);
        assert_eq!(two.per_connection_bytes, one.per_connection_bytes);
        // A response still has its own DOM and two wire buffers when the worker
        // exits. The reservation depends on write capacity independently of read.
        let larger = TransportReservation::for_limits(limits(100, 400, 1)).unwrap();
        assert!(larger.activation_bytes >= one.activation_bytes + 3 * 200);
        assert!(one.activation_bytes > 100 + 200);
    }
    #[test]
    fn transport_accounting_rejects_unrepresentable_caps_and_connection_products() {
        assert!(TransportReservation::for_limits(limits(usize::MAX, 1, 1)).is_none());
        assert!(TransportReservation::for_limits(limits(1, usize::MAX, 1)).is_none());
        assert!(TransportReservation::for_limits(limits(1, 1, usize::MAX)).is_none());
    }
    #[test]
    fn json_payload_charges_structural_expansion_of_small_scalar_arrays() {
        let json = "[0,0,0,0,0]";
        let value: Value = serde_json::from_str(json).unwrap();
        let known_headers = (value.as_array().unwrap().len() + 1) * size_of::<Value>();
        assert!(json_payload(json.len()).unwrap() >= json.len() + known_headers);
    }
}
