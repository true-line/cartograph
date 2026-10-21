//! Durable consistency proofs, separate from the unchanged public qualification.
use super::*;
use core_graph::coverage::{CoverageReadLimits, MAX_INVENTORY_BYTES};

/// Maximum UTF-8 bytes in a recorded identity, matching the coverage store.
pub const MAX_COVERAGE_IDENTITY_BYTES: usize = 256;
/// Maximum repos/basis entries accepted by a durable proof.
pub const MAX_COVERAGE_REPOS: usize = CoverageReadLimits::MAX_RECORDS;

/// Recorded identities used by the original coverage identity algorithm.
/// This is host metadata; it is not part of `coverage-qualification@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageBasis {
    /// Repo owning the recorded inventory.
    pub repo: String,
    /// Registration recorded by the ingest, if any.
    pub registered_source_id: Option<String>,
    /// Ingest identity.
    pub ingest_id: String,
    /// Recorded plugin basis.
    pub config_hash: String,
    /// Exact inventory body hash.
    pub inventory_hash: String,
}

/// A self-consistent historical qualification and its scoped context identity.
///
/// Validation recomputes identities and checks the complete status shape. It
/// does not attest that a registration was current: that comparison belongs to
/// the host that prepared the qualification. No source or inventory cache is
/// restored by deserializing this proof. Callers must bound decoding separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageProof {
    /// Durable proof format, currently one.
    pub schema_version: u32,
    /// Exact public qualification supplied to the model.
    pub qualification: CoverageQualification,
    /// Recorded identity basis, in repo order.
    pub basis: Vec<CoverageBasis>,
    /// Graph-only identity of the authorized scoped snapshot.
    pub snapshot_id: String,
    /// Shared `context-v2` identity of snapshot and qualification.
    pub context_id: String,
}

impl CoverageQualification {
    /// Preserve a prepared qualification's identity basis without adding it to
    /// the public wire. A public-only deserialized block with recorded repos
    /// lacks this basis and cannot produce a valid proof.
    pub fn durable_proof(&self, snapshot_id: &str) -> Result<CoverageProof, ContextError> {
        let qualification = CoverageQualification {
            contract: self.contract.clone(),
            exhaustive: self.exhaustive,
            coverage_id: self.coverage_id.clone(),
            repos: self.repos.clone(),
            inventories: BTreeMap::new(),
            basis: Vec::new(),
        };
        let proof = CoverageProof {
            schema_version: 1,
            qualification,
            basis: self.basis.clone(),
            snapshot_id: snapshot_id.to_string(),
            context_id: context_identity(snapshot_id, &self.coverage_id)?,
        };
        proof.validate()?;
        Ok(proof)
    }
}

impl CoverageContext {
    /// Historical consistency proof for this prepared context.
    pub fn durable_proof(&self) -> Result<CoverageProof, ContextError> {
        let proof = self.coverage.durable_proof(self.snapshot_id())?;
        if proof.context_id != self.context_id {
            return Err(invalid());
        }
        Ok(proof)
    }
}

impl CoverageProof {
    /// Validate durable metadata using the same identity algorithms as reads.
    /// This establishes internal consistency, not external registration truth.
    pub fn validate(&self) -> Result<(), ContextError> {
        let qualification = &self.qualification;
        if self.schema_version != 1
            || qualification.contract != COVERAGE_CONTRACT
            || qualification.exhaustive
            || !identity_hash(&self.snapshot_id, "context-v1")
            || qualification.repos.len() > MAX_COVERAGE_REPOS
            || self.basis.len() > MAX_COVERAGE_REPOS
        {
            return Err(invalid());
        }
        let mut previous_repo: Option<&str> = None;
        let mut basis = self.basis.iter();
        for repo in &qualification.repos {
            if !identity(&repo.repo)
                || previous_repo.is_some_and(|previous| previous >= repo.repo.as_str())
            {
                return Err(invalid());
            }
            previous_repo = Some(&repo.repo);
            validate_repo(repo)?;
            if repo.status == CoverageStatus::Unrecorded {
                continue;
            }
            let recorded = basis.next().ok_or_else(invalid)?;
            if recorded.repo != repo.repo
                || !identity(&recorded.repo)
                || !recorded
                    .registered_source_id
                    .as_deref()
                    .is_none_or(identity)
                || !identity(&recorded.ingest_id)
                || !identity(&recorded.config_hash)
                || !hash(&recorded.inventory_hash)
                || repo.inventory_hash.as_deref() != Some(recorded.inventory_hash.as_str())
                || (repo.status == CoverageStatus::Recorded
                    && recorded.registered_source_id.is_none())
            {
                return Err(invalid());
            }
        }
        if basis.next().is_some()
            || qualification.coverage_id
                != content_identity(
                    "coverage-context-v1",
                    &QualificationIdentity {
                        contract: &qualification.contract,
                        exhaustive: qualification.exhaustive,
                        repos: &qualification.repos,
                        basis: &self.basis,
                    },
                )?
            || self.context_id != context_identity(&self.snapshot_id, &qualification.coverage_id)?
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Validate one separately retained original record. Only Recorded entries
    /// grant detail reads; stale hashes never authorize an inventory body.
    pub fn validate_inventory(&self, stored: &StoredCoverage) -> Result<(), ContextError> {
        self.validate()?;
        let record = &stored.record;
        let repo = self
            .qualification
            .repos
            .iter()
            .find(|repo| repo.repo == record.repo_key && repo.status == CoverageStatus::Recorded)
            .ok_or_else(|| ContextError::InventoryUnavailable(record.repo_key.clone()))?;
        let basis = self
            .basis
            .iter()
            .find(|basis| basis.repo == record.repo_key)
            .ok_or_else(invalid)?;
        validate_original(stored, repo, basis)
    }

    /// Validate exactly the Recorded inventories, in any order, checking the
    /// proof once. Missing, duplicate, stale or unrelated entries are refused.
    pub fn validate_inventories(&self, inventories: &[StoredCoverage]) -> Result<(), ContextError> {
        self.validate()?;
        let mut expected: BTreeMap<_, _> = self
            .qualification
            .repos
            .iter()
            .filter(|repo| repo.status != CoverageStatus::Unrecorded)
            .zip(&self.basis)
            .filter(|(repo, _)| repo.status == CoverageStatus::Recorded)
            .map(|(repo, basis)| (repo.repo.as_str(), (repo, basis)))
            .collect();
        if expected.len() != inventories.len() {
            return Err(invalid());
        }
        for stored in inventories {
            let (repo, basis) = expected
                .remove(stored.record.repo_key.as_str())
                .ok_or_else(invalid)?;
            validate_original(stored, repo, basis)?;
        }
        Ok(())
    }
}

fn validate_original(
    stored: &StoredCoverage,
    repo: &RepoQualification,
    basis: &CoverageBasis,
) -> Result<(), ContextError> {
    let record = &stored.record;
    if record.registered_source_id != basis.registered_source_id
        || record.ingest_id != basis.ingest_id
        || record.config_hash != basis.config_hash
        || stored.inventory_hash != basis.inventory_hash
        || record.inventory_json.len() > MAX_INVENTORY_BYTES
        || record.inventory_hash() != stored.inventory_hash
    {
        return Err(invalid());
    }
    let (commit, hints, counts) = derive(&record.repo_key, &record.inventory_json)?;
    if repo.commit.as_deref() != Some(commit.as_str())
        || repo.layer_hints.as_ref() != Some(&hints)
        || repo.counts != Some(counts)
        || repo.limited != Some(counts.limited())
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate_repo(repo: &RepoQualification) -> Result<(), ContextError> {
    let derived_absent = repo.commit.is_none()
        && repo.limited.is_none()
        && repo.counts.is_none()
        && repo.layer_hints.is_none();
    if repo.status == CoverageStatus::Unrecorded {
        return if derived_absent && repo.inventory_hash.is_none() && repo.compared.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    if !repo.inventory_hash.as_deref().is_some_and(hash)
        || !(repo.compared == [COMPARED_REGISTRATION, COMPARED_PLUGINS]
            || repo.compared
                == [
                    COMPARED_REGISTRATION,
                    COMPARED_PLUGINS,
                    COMPARED_LAYER_HINTS,
                ])
    {
        return Err(invalid());
    }
    if repo.status == CoverageStatus::Stale {
        return if derived_absent {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    let counts = repo.counts.ok_or_else(invalid)?;
    let hints = repo.layer_hints.as_ref().ok_or_else(invalid)?;
    if !repo.commit.as_deref().is_some_and(identity)
        || repo.limited != Some(counts.limited())
        || [
            counts.not_selected,
            counts.missing_input,
            counts.not_applicable,
            counts.unsupported,
        ]
        .into_iter()
        .any(|count| count > MAX_INVENTORY_BYTES)
        || hints.len() > MAX_INVENTORY_BYTES
        || hints.iter().any(|hint| !identity(hint))
        || hints.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(invalid());
    }
    Ok(())
}

fn context_identity(snapshot_id: &str, coverage_id: &str) -> Result<String, ContextError> {
    content_identity(
        "context-v2",
        &ContextIdentity {
            snapshot_id,
            coverage_id,
        },
    )
}

fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_COVERAGE_IDENTITY_BYTES && !value.contains('\0')
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn identity_hash(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix(':'))
        .is_some_and(hash)
}

fn invalid() -> ContextError {
    ContextError::InvalidCoverage("invalid durable qualification".to_string())
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
