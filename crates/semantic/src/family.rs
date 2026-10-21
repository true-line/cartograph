//! Data-only family identity shared with the native job ownership harness.
use serde::{Deserialize, Serialize};

/// Supported unresolved flow-hop families at the M7 exit gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HopKind {
    /// Producer/consumer to a channel whose runtime identity was unresolved.
    Channel,
    /// Caller to a callee that the deterministic type/import graph could not
    /// establish.
    Call,
}

/// Exact candidate-family identity shared by grouping and staged proposals.
/// A missing subtype is distinct from every named subtype, never a wildcard.
/// This identifies a proposal's origin; it grants no evaluation authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyKey {
    kind: HopKind,
    subtype: Option<String>,
}

impl FamilyKey {
    /// Preserve the existing hop kind and subtype without normalization.
    pub fn new(kind: HopKind, subtype: Option<String>) -> Self {
        Self { kind, subtype }
    }

    pub fn kind(&self) -> HopKind {
        self.kind
    }

    pub fn subtype(&self) -> Option<&str> {
        self.subtype.as_deref()
    }
}
