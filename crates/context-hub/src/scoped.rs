//! Identities for revision-bound partial reads, never complete graph snapshots.
//!
//! A basis binds host-observed qualifications. It does not prove projection
//! closure, recovery completion, authorization or absence. Only the host may
//! construct it from observations made around its short graph read transaction.
use crate::{ContextError, content_identity};
use core_graph::StoreRevision;
use serde::Serialize;

/// Domain separator deliberately incompatible with complete `context-v1` IDs.
pub const SCOPED_BASIS_DOMAIN: &str = "scoped-read-basis-v1";

/// Explicit export policy used to qualify the partial view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedExportMode {
    /// Excludes weak inference under the shared official projection policy.
    VerifiedOnly,
    /// Includes weak inference with its stored confidence visible.
    BestEffort,
}

/// Fixed-size digests of separately bounded host observations.
/// Callers hash the canonical owner representation, never an unqualified label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopedQualifications {
    /// Official projection mode; this enum does not apply the projection itself.
    pub export_mode: ScopedExportMode,
    /// Source registration and configuration observations.
    pub registration_digest: [u8; 32],
    /// Human rejection-decision observations.
    pub rejection_digest: [u8; 32],
    /// Producer coverage qualifications, including unknown/unavailable states.
    pub coverage_digest: [u8; 32],
}

/// Immutable scoped scan basis. No conversion to a complete snapshot is exposed.
/// Serialization is output-only: clients return a basis ID, which the host checks
/// against freshly observed state, rather than deserializing an asserted proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopedReadBasis {
    id: String,
    revision: StoreRevision,
    matching_policy: &'static str,
    query_digest: [u8; 32],
    qualifications: ScopedQualifications,
}

#[derive(Serialize)]
struct IdentityInput<'a> {
    universe: &'static str,
    revision: &'a StoreRevision,
    matching_policy: &'static str,
    query_digest: &'a [u8; 32],
    qualifications: &'a ScopedQualifications,
}

/// Typed refusal when a scoped basis cannot be used coherently.
#[derive(Debug, thiserror::Error)]
pub enum ScopedBasisError {
    /// Invalid storage token; no identity was minted.
    #[error("invalid scoped store revision")]
    InvalidRevision,
    /// Revision or any bound query/policy/host qualification differs.
    #[error("scoped read basis changed")]
    Changed,
    /// Canonical identity serialization failed.
    #[error(transparent)]
    Identity(#[from] ContextError),
}

impl ScopedReadBasis {
    /// Construct a registered-graph primary-key scan basis. `query_digest` must
    /// come from `DiscoveryMatcher::query_digest`, binding the normalized query
    /// and exact label filter under discovery-v1.
    /// Fixed-size digests keep identity workspace independent of graph size.
    pub fn new(
        revision: StoreRevision,
        query_digest: [u8; 32],
        qualifications: ScopedQualifications,
    ) -> Result<Self, ScopedBasisError> {
        if revision.graph_instance.len() != 32
            || !revision
                .graph_instance
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || revision.generation > i64::MAX as u64
        {
            return Err(ScopedBasisError::InvalidRevision);
        }
        let matching_policy = crate::discovery::DISCOVERY_POLICY;
        let id = content_identity(
            SCOPED_BASIS_DOMAIN,
            &IdentityInput {
                universe: "registered_graph_primary_keys",
                revision: &revision,
                matching_policy,
                query_digest: &query_digest,
                qualifications: &qualifications,
            },
        )?;
        Ok(Self {
            id,
            revision,
            matching_policy,
            query_digest,
            qualifications,
        })
    }

    /// Scoped identity; it is not a recovered graph or artifact content ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Storage token to require inside each subsequent short read transaction.
    pub fn revision(&self) -> &StoreRevision {
        &self.revision
    }

    /// Compare a client continuation with the freshly observed host basis.
    /// This is a consistency check, not an authentication or authorization check.
    pub fn require_current(&self, expected_id: &str) -> Result<(), ScopedBasisError> {
        if self.id != expected_id {
            return Err(ScopedBasisError::Changed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn revision() -> StoreRevision {
        StoreRevision {
            graph_instance: "a".repeat(32),
            generation: 4,
        }
    }
    fn qualifications() -> ScopedQualifications {
        ScopedQualifications {
            export_mode: ScopedExportMode::VerifiedOnly,
            registration_digest: [1; 32],
            rejection_digest: [2; 32],
            coverage_digest: [3; 32],
        }
    }
    #[test]
    fn scoped_basis_binds_every_independent_observation_and_rejects_full_ids() {
        let original = ScopedReadBasis::new(revision(), [4; 32], qualifications()).unwrap();
        assert!(original.id().starts_with("scoped-read-basis-v1:"));
        assert!(
            original
                .require_current(&format!("context-v1:{}", "0".repeat(64)))
                .is_err()
        );
        assert!(original.require_current(original.id()).is_ok());
        for changed in 0..7 {
            let mut revision = revision();
            let mut q = qualifications();
            let mut query = [4; 32];
            match changed {
                0 => revision.generation += 1,
                1 => revision.graph_instance = "b".repeat(32),
                2 => q.export_mode = ScopedExportMode::BestEffort,
                3 => q.registration_digest[0] ^= 1,
                4 => q.rejection_digest[0] ^= 1,
                5 => q.coverage_digest[0] ^= 1,
                _ => query[0] ^= 1,
            }
            let current = ScopedReadBasis::new(revision, query, q).unwrap();
            assert!(matches!(
                current.require_current(original.id()),
                Err(ScopedBasisError::Changed)
            ));
        }
        let same = ScopedReadBasis::new(revision(), [4; 32], qualifications()).unwrap();
        assert_eq!(original, same);
    }
    #[test]
    fn scoped_basis_refuses_invalid_storage_tokens() {
        for bad in [
            StoreRevision {
                graph_instance: "x".repeat(32),
                generation: 0,
            },
            StoreRevision {
                graph_instance: "a".repeat(33),
                generation: 0,
            },
            StoreRevision {
                graph_instance: "a".repeat(32),
                generation: u64::MAX,
            },
        ] {
            assert!(matches!(
                ScopedReadBasis::new(bad, [0; 32], qualifications()),
                Err(ScopedBasisError::InvalidRevision)
            ));
        }
    }
}
