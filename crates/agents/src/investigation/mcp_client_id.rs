//! Host-issued durable client identity; never a credential or caller grant.
use serde::{Deserialize, Deserializer, Serialize};

/// Fixed-size opaque identifier. The host generates its 128 random bits.
/// Deserialization validates representation, not the caller's authority.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct McpClientId(String);

impl McpClientId {
    /// Encodes 16 host-generated random bytes as lowercase hex.
    pub fn from_host_bytes(bytes: [u8; 16]) -> Self {
        let value = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Self(value)
    }

    /// The identifier's hex text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for McpClientId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = McpClientId;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a host-issued 32-character lowercase hexadecimal client identity")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if value.len() != 32
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(E::custom("invalid MCP client identity"));
                }
                Ok(McpClientId(value.to_owned()))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0321: the persisted ID has fixed syntax and contains no credential.
    #[test]
    fn mcp_client_identity_is_fixed_size_and_round_trips() {
        let id = McpClientId::from_host_bytes([0xab; 16]);
        assert_eq!(id.as_str(), "abababababababababababababababab");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<McpClientId>(&json).unwrap(), id);
        for invalid in [
            "",
            "abc",
            "ABABABABABABABABABABABABABABABAB",
            "../private/store",
            &"a".repeat(33),
            &"é".repeat(16),
            &"g".repeat(32),
        ] {
            assert!(
                serde_json::from_str::<McpClientId>(&serde_json::to_string(invalid).unwrap())
                    .is_err()
            );
        }
        for invalid in ["null", "0", "{}", "[]"] {
            assert!(serde_json::from_str::<McpClientId>(invalid).is_err());
        }
    }
}

#[cfg(test)]
mod origin_tests {
    use super::*;
    use crate::investigation::InvestigationOrigin;

    // AC-0323: retain historical App JSON while adding explicit MCP attribution.
    #[test]
    fn task_origin_preserves_app_wire_and_binds_client_namespace() {
        let app: InvestigationOrigin = serde_json::from_str("\"app\"").unwrap();
        assert_eq!(serde_json::to_string(&app).unwrap(), "\"app\"");
        assert_eq!(app.request_namespace(), "app");
        let origin = InvestigationOrigin::Mcp {
            client_id: McpClientId::from_host_bytes([0xab; 16]),
        };
        assert_eq!(
            origin.request_namespace(),
            format!("mcp:{}", "ab".repeat(16))
        );
        let json = serde_json::to_string(&origin).unwrap();
        assert_eq!(
            serde_json::from_str::<InvestigationOrigin>(&json).unwrap(),
            origin
        );
    }
}
