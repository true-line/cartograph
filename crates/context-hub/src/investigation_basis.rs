//! Host-observed scoped investigation identity; never a whole-graph claim.
use crate::{ContextError, content_identity};
use core_graph::StoreRevision;
use serde::Serialize;

/// Domain distinct from complete snapshots and discovery scan identities.
pub const INVESTIGATION_BASIS_DOMAIN: &str = "investigation-scoped-basis-v1";

/// The exact scoped universe whose normalized request digest the host binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationScopeExtent {
    /// One node, excluding incident and self-loop edges.
    ExactNode,
    /// Registered graph traversed in bounded pages, never a materialized whole graph.
    AllPaged,
    /// Undirected radius closure and all edges induced by its present nodes.
    InducedNeighborhood,
}

/// Fixed-size observations obtained under one coherent store revision.
/// Digests use canonical host representations; a caller label is not a proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvestigationScopeObservation {
    /// Requested universe, distinguishing materialized scopes from paged All.
    pub extent: InvestigationScopeExtent,
    /// Canonical normalized request, including anchor and radius when applicable.
    pub request_digest: [u8; 32],
    /// Canonical acquired projection, not the complete registered graph.
    /// Must be absent for AllPaged; each page binds its own projection separately.
    pub projection_digest: Option<[u8; 32]>,
    /// Coverage and registration qualifications observed around acquisition.
    pub qualification_digest: [u8; 32],
}

/// Output-only consistency identity. Construction grants no read authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvestigationReadBasis {
    id: String,
    revision: StoreRevision,
    observation: InvestigationScopeObservation,
}

/// Refusal of an invalid or changed investigation acquisition descriptor.
#[derive(Debug, thiserror::Error)]
pub enum InvestigationBasisError {
    /// Storage instance/generation is not a valid revision token.
    #[error("invalid investigation store revision")]
    InvalidRevision,
    /// Paged All cannot claim one materialized projection; concrete scopes must bind one.
    #[error("inconsistent investigation acquisition extent")]
    InvalidExtent,
    /// A frozen acquisition observation no longer agrees with the current one.
    #[error("investigation acquisition basis changed")]
    Changed,
    /// Canonical identity serialization failed.
    #[error(transparent)]
    Identity(#[from] ContextError),
}

impl InvestigationReadBasis {
    /// Bind already bounded host observations, without reading graph or source.
    /// The host must prove acquisition completeness within the requested scope.
    pub fn new(
        revision: StoreRevision,
        observation: InvestigationScopeObservation,
    ) -> Result<Self, InvestigationBasisError> {
        if revision.graph_instance.len() != 32
            || !revision
                .graph_instance
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || revision.generation > i64::MAX as u64
        {
            return Err(InvestigationBasisError::InvalidRevision);
        }
        if (observation.extent == InvestigationScopeExtent::AllPaged)
            != observation.projection_digest.is_none()
        {
            return Err(InvestigationBasisError::InvalidExtent);
        }
        let id = content_identity(
            INVESTIGATION_BASIS_DOMAIN,
            &("raw_stored_facts_acquisition_v1", &revision, &observation),
        )?;
        Ok(Self {
            id,
            revision,
            observation,
        })
    }

    /// Scoped basis identifier, never a complete context snapshot identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Revision required for subsequent evidence/source association acquisition.
    pub fn revision(&self) -> &StoreRevision {
        &self.revision
    }

    /// Require exact continuity; this is not authentication or authorization.
    pub fn require_current(&self, expected: &str) -> Result<(), InvestigationBasisError> {
        if self.id != expected {
            return Err(InvestigationBasisError::Changed);
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
            generation: 7,
        }
    }
    fn observation() -> InvestigationScopeObservation {
        InvestigationScopeObservation {
            extent: InvestigationScopeExtent::InducedNeighborhood,
            request_digest: [1; 32],
            projection_digest: Some([2; 32]),
            qualification_digest: [3; 32],
        }
    }

    // AC-0304: scoped identity binds every observation without whole-graph claims.
    #[test]
    fn every_scoped_observation_and_revision_is_bound() {
        let basis = InvestigationReadBasis::new(revision(), observation()).unwrap();
        assert!(basis.id().starts_with("investigation-scoped-basis-v1:"));
        assert!(!basis.id().starts_with("context-v1:"));
        assert_eq!(
            basis,
            InvestigationReadBasis::new(revision(), observation()).unwrap()
        );
        for field in 0..6 {
            let mut r = revision();
            let mut o = observation();
            match field {
                0 => r.graph_instance = "b".repeat(32),
                1 => r.generation += 1,
                2 => o.extent = InvestigationScopeExtent::ExactNode,
                3 => o.request_digest[0] ^= 1,
                4 => o.projection_digest.as_mut().unwrap()[0] ^= 1,
                _ => o.qualification_digest[0] ^= 1,
            }
            let changed = InvestigationReadBasis::new(r, o).unwrap();
            assert!(matches!(
                changed.require_current(basis.id()),
                Err(InvestigationBasisError::Changed)
            ));
        }
    }

    // AC-0304: malformed storage revisions cannot mint scoped identities.
    #[test]
    fn malformed_revision_mints_no_basis() {
        for r in [
            StoreRevision {
                graph_instance: "A".repeat(32),
                ..revision()
            },
            StoreRevision {
                graph_instance: "a".repeat(31),
                ..revision()
            },
            StoreRevision {
                generation: u64::MAX,
                ..revision()
            },
        ] {
            assert!(matches!(
                InvestigationReadBasis::new(r, observation()),
                Err(InvestigationBasisError::InvalidRevision)
            ));
        }
    }
    // AC-0304: All pages never impersonate one complete graph projection.
    #[test]
    fn paged_all_basis_rejects_materialized_projection_claims() {
        let mut o = observation();
        o.extent = InvestigationScopeExtent::AllPaged;
        assert!(matches!(
            InvestigationReadBasis::new(revision(), o.clone()),
            Err(InvestigationBasisError::InvalidExtent)
        ));
        o.projection_digest = None;
        let basis = InvestigationReadBasis::new(revision(), o.clone()).unwrap();
        basis.require_current(basis.id()).unwrap();
        assert_eq!(basis.revision(), &revision());
        for extent in [
            InvestigationScopeExtent::ExactNode,
            InvestigationScopeExtent::InducedNeighborhood,
        ] {
            o.extent = extent;
            assert!(matches!(
                InvestigationReadBasis::new(revision(), o.clone()),
                Err(InvestigationBasisError::InvalidExtent)
            ));
        }
    }
}
