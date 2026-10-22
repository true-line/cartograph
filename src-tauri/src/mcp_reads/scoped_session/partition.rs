//! Closed partition compilation in one pinned graph transaction per request.
use super::*;

#[derive(Debug, Serialize)]
pub(crate) struct ScopedArtifact {
    pub session_basis_id: String,
    /// Entry point is response metadata, not part of canonical component content.
    pub requested_root: String,
    pub complete_system: bool,
    pub absence_claim: bool,
    pub artifact: spec::PartitionArtifact,
}
fn partition_error(error: spec::PartitionError) -> ReadError {
    match error {
        spec::PartitionError::Graph(error) => graph::<()>(Err(error)).unwrap_err(),
        spec::PartitionError::Limit => ReadError::Limit,
        spec::PartitionError::Unavailable => ReadError::ArtifactUnavailable,
        spec::PartitionError::InvalidRequest => ReadError::InvalidRequest,
    }
}
impl ScopedReadSession {
    fn check_external_qualifications(
        &self,
        ctx: &mut ScopedReadContext<'_>,
    ) -> Result<(), ReadError> {
        let scope = normalized_scope((ctx.scope)()?)?;
        let decisions = rejected(ctx.decisions)?;
        if hash(&scope)? != self.qualifications.registration_digest
            || hash(&decisions)? != self.qualifications.rejection_digest
        {
            return Err(ReadError::StaleBasis);
        }
        Ok(())
    }
    pub(crate) fn partition(
        &self,
        ctx: &mut ScopedReadContext<'_>,
        expected: &str,
        kind: spec::PartitionArtifactKind,
        root: &str,
        max_bytes: usize,
    ) -> Result<ScopedArtifact, ReadError> {
        self.require_basis(expected)?;
        if root.is_empty() || root.len() > self.graph_limits.max_bytes {
            return Err(ReadError::InvalidRequest);
        }
        #[derive(Serialize)]
        struct Envelope<'a> {
            session_basis_id: &'a str,
            requested_root: &'a str,
            complete_system: bool,
            absence_claim: bool,
            artifact: Option<()>,
        }
        let overhead = context_hub::logical::count(&Envelope {
            session_basis_id: &self.basis_id,
            requested_root: root,
            complete_system: false,
            absence_claim: false,
            artifact: None,
        })
        .and_then(|bytes| bytes.checked_sub(4))
        .ok_or(ReadError::Limit)?;
        let artifact_bytes = max_bytes.checked_sub(overhead).ok_or(ReadError::Limit)?;
        self.check_external_qualifications(ctx)?;
        // The scoped reader borrows the store connection and cannot escape its
        // transaction callback. Keep the graph guard through compilation so
        // acquisition and rendering share that pinned revision. This bounded
        // critical section can delay publication on this store handle; releasing
        // it earlier requires an owned acquired-partition boundary.
        let artifact = ctx
            .graph
            .lock()
            .map_err(|_| ReadError::Storage)?
            .with_scoped_reader(&self.revision, self.graph_limits, |reader| {
                spec::compile_partition_selected(
                    reader,
                    kind,
                    root,
                    &self.basis_id,
                    self.mode,
                    &self.rejected,
                    artifact_bytes,
                )
            })
            .map_err(partition_error)?;
        // A concurrent graph commit does not invalidate this request's pinned
        // snapshot. The next request will fail its expected-revision check.
        // Registration/rejection decisions live outside that SQLite snapshot.
        self.check_external_qualifications(ctx)?;
        let response = ScopedArtifact {
            session_basis_id: self.basis_id.clone(),
            requested_root: root.to_owned(),
            complete_system: false,
            absence_claim: false,
            artifact,
        };
        admission::fits(&response, max_bytes)?;
        Ok(response)
    }
}
