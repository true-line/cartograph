//! Local bridge capability preamble. Native endpoint permissions, peer checks,
//! deadlines, disable/restart revocation and framing bounds belong to the host.
use crate::admission::{Action, Admission};
use serde_json::Value;
use subtle::ConstantTimeEq;

/// Internal bridge protocol, separate from negotiated MCP protocol versions.
pub const LOCAL_PROTOCOL: &str = "cartograph-local@1";

/// Fixed internal acknowledgement; it exposes no credentials or app metadata.
pub fn acknowledgement() -> Value {
    serde_json::json!({"protocol": LOCAL_PROTOCOL, "authenticated": true})
}

/// Require the exact acknowledgement before a relay emits any MCP traffic.
pub fn is_acknowledgement(value: &Value) -> bool {
    value == &acknowledgement()
}

/// A fresh OS-random local capability. Deliberately neither Debug nor Serialize.
/// Possession grants the same account-local access as the private endpoint file.
pub struct Capability([u8; 32]);

/// OS entropy failed; no predictable fallback capability may be used.
#[derive(Debug, thiserror::Error)]
#[error("local capability generation failed")]
pub struct EntropyUnavailable;

impl Capability {
    /// Generate a new capability at explicit service activation.
    pub fn generate() -> Result<Self, EntropyUnavailable> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| EntropyUnavailable)?;
        Ok(Self(bytes))
    }

    /// Expose solely for a private endpoint record or authentication preamble.
    /// Never place this value in logs, client config examples, argv or errors.
    pub fn expose_for_local_transport(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(64);
        for byte in self.0 {
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 15) as usize] as char);
        }
        encoded
    }

    /// Validate the exact internal preamble after native peer verification.
    /// This pure check does not establish a reusable connection or authorize
    /// app reads; the owner must close on failure and enforce activation lifetime.
    pub fn authenticates_preamble(&self, message: &Value) -> bool {
        message.as_object().is_some_and(|object| {
            object.len() == 2
                && object.get("protocol").and_then(Value::as_str) == Some(LOCAL_PROTOCOL)
                && object
                    .get("capability")
                    .and_then(Value::as_str)
                    .is_some_and(|value| self.matches(value))
        })
    }

    fn matches(&self, candidate: &str) -> bool {
        // Length is public; equal-length comparisons do not branch on contents.
        bool::from(
            self.expose_for_local_transport()
                .as_bytes()
                .ct_eq(candidate.as_bytes()),
        )
    }
}

/// Authentication stage outcome; rejection has no secret-bearing explanation.
#[derive(Debug, PartialEq)]
pub enum BridgeAction {
    /// Send the fixed local acknowledgement before processing MCP frames.
    Authenticated,
    /// Close permanently without any protocol or context dispatch.
    Reject,
    /// Already authenticated: handle this MCP admission outcome.
    Protocol(Action),
}

#[derive(PartialEq)]
enum State {
    Pending,
    Authenticated,
    Rejected,
}

/// One connection's handshake gate. The caller owns the activation capability,
/// supplies bounded input and must close connections when that activation ends.
pub struct BridgeAdmission {
    state: State,
    admission: Admission,
}

impl BridgeAdmission {
    /// Begin unauthenticated; no MCP message is admitted before the preamble.
    pub fn new(admission: Admission) -> Self {
        Self {
            state: State::Pending,
            admission,
        }
    }

    /// Permanently revoke this connection before closing its native transport.
    /// Host disable/restart must revoke every live connection and drop sessions.
    pub fn revoke(&mut self) {
        self.state = State::Rejected;
    }

    /// Authenticate exactly one internal preamble, then delegate MCP admission.
    /// Native peer identity must be verified before calling this method.
    pub fn receive(&mut self, message: Value, capability: &Capability) -> BridgeAction {
        match self.state {
            State::Rejected => BridgeAction::Reject,
            State::Authenticated => BridgeAction::Protocol(self.admission.receive(message)),
            State::Pending => {
                let valid = capability.authenticates_preamble(&message);
                self.state = if valid {
                    State::Authenticated
                } else {
                    State::Rejected
                };
                if valid {
                    BridgeAction::Authenticated
                } else {
                    BridgeAction::Reject
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::num::NonZeroUsize;

    fn gate() -> BridgeAdmission {
        BridgeAdmission::new(Admission::new([], NonZeroUsize::new(8).unwrap()))
    }
    fn preamble(capability: &Capability) -> Value {
        json!({"protocol": LOCAL_PROTOCOL, "capability":capability.expose_for_local_transport()})
    }

    #[test]
    fn preamble_is_required_and_rejection_is_terminal() {
        // #718 / AC-0289: handshake unit coverage, not native authentication acceptance.
        let secret = Capability([0x71; 32]);
        let mut gate = gate();
        assert_eq!(
            gate.receive(json!({"jsonrpc":"2.0","id":1,"method":"ping"}), &secret),
            BridgeAction::Reject
        );
        assert_eq!(
            gate.receive(preamble(&secret), &secret),
            BridgeAction::Reject
        );
    }

    #[test]
    fn credential_matches_exactly_and_never_enters_responses() {
        let secret = Capability([0xab; 32]);
        assert_eq!(secret.expose_for_local_transport(), "ab".repeat(32));
        let mut gate = gate();
        assert_eq!(
            gate.receive(preamble(&secret), &secret),
            BridgeAction::Authenticated
        );
        let BridgeAction::Protocol(Action::Respond(response)) =
            gate.receive(json!({"jsonrpc":"2.0","id":1,"method":"ping"}), &secret)
        else {
            panic!()
        };
        assert_eq!(response, json!({"jsonrpc":"2.0","id":1,"result":{}}));
    }

    #[test]
    fn malformed_or_old_capabilities_cannot_authenticate_new_connection() {
        let secret = Capability([0xab; 32]);
        let old = Capability([0x72; 32]);
        for candidate in [
            preamble(&old),
            json!({"protocol":LOCAL_PROTOCOL,"capability":"AB".repeat(32)}),
            json!({"protocol":LOCAL_PROTOCOL,"capability":secret.expose_for_local_transport(),"method":"ping"}),
            json!({"protocol":LOCAL_PROTOCOL,"capability":"short"}),
            json!(null),
        ] {
            assert_eq!(gate().receive(candidate, &secret), BridgeAction::Reject);
        }
    }

    #[test]
    fn revocation_ends_authenticated_admission() {
        let secret = Capability([0x71; 32]);
        let mut gate = gate();
        assert_eq!(
            gate.receive(preamble(&secret), &secret),
            BridgeAction::Authenticated
        );
        gate.revoke();
        assert_eq!(
            gate.receive(json!({"jsonrpc":"2.0","id":1,"method":"ping"}), &secret),
            BridgeAction::Reject
        );
        assert_eq!(
            gate.receive(preamble(&secret), &secret),
            BridgeAction::Reject
        );
    }

    #[test]
    fn acknowledgement_is_exact_and_contains_no_capability() {
        assert!(is_acknowledgement(&acknowledgement()));
        assert!(!is_acknowledgement(
            &json!({"protocol":LOCAL_PROTOCOL,"authenticated":false})
        ));
        assert!(!is_acknowledgement(
            &json!({"protocol":LOCAL_PROTOCOL,"authenticated":true,"capability":"unexpected"})
        ));
        assert!(!is_acknowledgement(&json!({"jsonrpc":"2.0","result":{}})));
    }

    #[test]
    fn operating_system_capability_can_authenticate_without_logging_it() {
        let secret = Capability::generate().unwrap();
        assert_eq!(
            gate().receive(preamble(&secret), &secret),
            BridgeAction::Authenticated
        );
    }
}
