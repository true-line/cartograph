//! Read-only registry identity through the caller's active state.db transaction.
//! No registry connection/mutex, OS lock acquisition or checkout access escapes
//! this view. Captured evidence authority remains its original frozen binding.
use super::*;

pub struct RetainedSourceView<'tx> {
    connection: &'tx Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedSourceError {
    Missing,
    Changed,
    Operational,
}

impl<'tx> RetainedSourceView<'tx> {
    /// Borrow the actual writer transaction; the view cannot outlive its commit.
    /// Empty retained-source sets require no source registry initialization.
    pub fn from_transaction(transaction: &'tx rusqlite::Transaction<'_>) -> Self {
        Self {
            connection: transaction,
        }
    }

    pub fn verify(&self, expected: &RegisteredSource) -> Result<(), RetainedSourceError> {
        // Reuse authoritative schema/meta/type/UTF-8 bounds every time. The view
        // caches no validation that could become stale after a same-Tx write.
        check_state(self.connection, Some(&expected.registry_id))
            .map_err(|_| RetainedSourceError::Operational)?;
        let current = read_one(
            self.connection,
            &expected.app_data_dir,
            &expected.registry_id,
            "repo_key",
            &expected.repo_key,
        )
        .map_err(|_| RetainedSourceError::Operational)?
        .ok_or(RetainedSourceError::Missing)?;
        if current.source_id != expected.source_id
            || current.repo_key != expected.repo_key
            || current.root() != expected.root()
            || current.is_managed() != expected.is_managed()
            || current.clone_url() != expected.clone_url()
        {
            return Err(RetainedSourceError::Changed);
        }
        // Display/readiness and checkout existence are not captured identity.
        Ok(())
    }
}

#[cfg(test)]
#[path = "retained_view_tests.rs"]
mod tests;
