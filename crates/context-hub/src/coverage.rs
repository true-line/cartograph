//! Coverage-qualified context reads (AC-0239).
//!
//! A [`CoverageContext`] pairs an immutable snapshot with the recorded
//! inventories of the repos in scope, read in the same graph transaction.
//! Every response it serves carries the qualification and never claims
//! exhaustive coverage: recording no limitation is not evidence that every
//! language or framework is supported. Each repo states what was compared at
//! read time; everything else describes the recorded ingest only. Existing
//! [`ContextSnapshot`] reads and their identities are unchanged.

use super::{
    ContextError, ContextSnapshot, DiscoveryRequest, DiscoveryResponse, QueryRequest,
    QueryResponse, content_identity,
};
use core_graph::coverage::StoredCoverage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod validation;
pub use validation::{
    CoverageBasis, CoverageProof, MAX_COVERAGE_IDENTITY_BYTES, MAX_COVERAGE_REPOS,
};

/// Version of the qualification block.
pub const COVERAGE_CONTRACT: &str = "coverage-qualification@1";

/// The only inventory body version this contract derives from; any other
/// version is refused rather than read under these semantics.
pub const INVENTORY_VERSION: &str = "coverage@1";

/// How a scoped repo's inventory relates to its current registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// The inventory was recorded under the repo's current registration and
    /// plugin configuration.
    Recorded,
    /// An inventory exists but its registration or configuration changed.
    Stale,
    /// No inventory was recorded for this repo.
    Unrecorded,
}

/// Compared at read time: the repo's current registered source.
pub const COMPARED_REGISTRATION: &str = "registration";
/// Compared at read time: the repo's current plugin basis.
pub const COMPARED_PLUGINS: &str = "plugins";
/// Compared at read time: the layer selection the caller requested.
pub const COMPARED_LAYER_HINTS: &str = "layer_hints";

/// What the caller currently knows about one repo in scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedRepo {
    /// Repo identity the facts are namespaced by.
    pub repo_key: String,
    /// Its current registered source, if it is registered.
    pub registered_source_id: Option<String>,
    /// Identity of its current plugin basis only. Layer selection and declared
    /// inputs are per-ingest options, recorded in the inventory.
    pub config_hash: String,
    /// The layer selection the caller now wants qualified, if any. Without
    /// one, a recorded inventory qualifies exactly its own prior ingest.
    #[serde(default)]
    pub layer_hints: Option<Vec<String>>,
}

/// Producer states counted from a recorded inventory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageCounts {
    /// Producers the layer hints excluded.
    pub not_selected: usize,
    /// Producers whose input was not declared.
    pub missing_input: usize,
    /// Producers with nothing to read in this repo.
    pub not_applicable: usize,
    /// Capabilities no installed producer covers.
    pub unsupported: usize,
}

impl CoverageCounts {
    /// Exclusions, missing inputs and unsupported capabilities limit coverage;
    /// a producer with nothing to read does not.
    pub fn limited(&self) -> bool {
        self.not_selected != 0 || self.missing_input != 0 || self.unsupported != 0
    }
}

/// One repo's qualification. Fields derived from an inventory are `None`
/// unless it is [`CoverageStatus::Recorded`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoQualification {
    /// Repo identity.
    pub repo: String,
    /// Inventory status.
    pub status: CoverageStatus,
    /// Commit the inventory describes.
    pub commit: Option<String>,
    /// Content hash of the recorded inventory, for a detail read.
    pub inventory_hash: Option<String>,
    /// Whether anything was excluded, missing or unsupported.
    pub limited: Option<bool>,
    /// Producer states, counted from the inventory.
    pub counts: Option<CoverageCounts>,
    /// The layer selection the inventory was recorded under; empty selects
    /// every layer.
    pub layer_hints: Option<Vec<String>>,
    /// What was compared with the current state at read time. Anything else
    /// (declared inputs included) is as recorded at ingest, not revalidated.
    pub compared: Vec<String>,
}

/// The block every coverage-qualified response carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageQualification {
    /// [`COVERAGE_CONTRACT`].
    pub contract: String,
    /// Always `false`.
    pub exhaustive: bool,
    /// Identity of this block and the recorded basis behind it.
    pub coverage_id: String,
    /// Every repo in scope, by repo.
    pub repos: Vec<RepoQualification>,
    #[serde(skip)]
    inventories: BTreeMap<String, (String, String)>,
    #[serde(skip)]
    basis: Vec<CoverageBasis>,
}

#[derive(Serialize)]
struct QualificationIdentity<'a> {
    contract: &'a str,
    exhaustive: bool,
    repos: &'a [RepoQualification],
    basis: &'a [CoverageBasis],
}

/// Count producer states from an inventory body and check the fields the
/// block derives from it.
type Derived = (String, Vec<String>, CoverageCounts);

fn derive(repo: &str, inventory_json: &str) -> Result<Derived, ContextError> {
    let invalid = || ContextError::InvalidCoverage(repo.to_string());
    let inventory: Value = serde_json::from_str(inventory_json).map_err(|_| invalid())?;
    if inventory["inventory"].as_str() != Some(INVENTORY_VERSION)
        || inventory["repo"].as_str() != Some(repo)
    {
        return Err(invalid());
    }
    let commit = inventory["commit"]
        .as_str()
        .ok_or_else(invalid)?
        .to_string();
    if let Some(note) = inventory.get("commit_note")
        && !note.is_null()
        && !note
            .as_str()
            .is_some_and(|text| text.len() <= 256 && !text.contains('\0'))
    {
        return Err(invalid());
    }
    let layer_hints = inventory["layer_hints"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|hint| hint.as_str().map(str::to_string).ok_or_else(invalid))
        .collect::<Result<_, _>>()?;
    let mut counts = CoverageCounts::default();
    for producer in inventory["producers"].as_array().ok_or_else(invalid)? {
        match producer["state"].as_str() {
            Some("ran") => {}
            Some("not_selected") => counts.not_selected += 1,
            Some("missing_input") => counts.missing_input += 1,
            Some("not_applicable") => counts.not_applicable += 1,
            _ => return Err(invalid()),
        }
    }
    counts.unsupported = inventory["unsupported"]
        .as_array()
        .ok_or_else(invalid)?
        .len();
    // The recorded flag must agree with the counts it summarizes.
    if inventory["limited"].as_bool() != Some(counts.limited()) {
        return Err(invalid());
    }
    Ok((commit, normalized(layer_hints), counts))
}

/// Layer selections compare as sets.
fn normalized(mut hints: Vec<String>) -> Vec<String> {
    hints.sort();
    hints.dedup();
    hints
}

impl CoverageQualification {
    /// Qualify `scope` with the `recorded` inventories. Every scoped repo and
    /// every recorded repo appears, including ones that published no facts.
    pub fn new(scope: &[ScopedRepo], recorded: &[StoredCoverage]) -> Result<Self, ContextError> {
        let current: BTreeMap<&str, &ScopedRepo> = scope
            .iter()
            .map(|repo| (repo.repo_key.as_str(), repo))
            .collect();
        let stored: BTreeMap<&str, &StoredCoverage> = recorded
            .iter()
            .map(|stored| (stored.record.repo_key.as_str(), stored))
            .collect();
        if current.len() != scope.len() || stored.len() != recorded.len() {
            return Err(ContextError::InvalidCoverage("duplicate repo".into()));
        }
        let repos: BTreeSet<&str> = current.keys().chain(stored.keys()).copied().collect();
        let mut qualified = Vec::with_capacity(repos.len());
        let mut basis = Vec::new();
        let mut inventories = BTreeMap::new();
        for repo in repos {
            let Some(stored) = stored.get(repo) else {
                qualified.push(RepoQualification {
                    repo: repo.to_string(),
                    status: CoverageStatus::Unrecorded,
                    commit: None,
                    inventory_hash: None,
                    limited: None,
                    counts: None,
                    layer_hints: None,
                    compared: Vec::new(),
                });
                continue;
            };
            let record = &stored.record;
            basis.push(CoverageBasis {
                repo: repo.to_string(),
                registered_source_id: record.registered_source_id.clone(),
                ingest_id: record.ingest_id.clone(),
                config_hash: record.config_hash.clone(),
                inventory_hash: stored.inventory_hash.clone(),
            });
            // Fail closed: an unregistered repo, a different registration, a
            // changed plugin basis or a different requested layer selection
            // is stale, never recorded.
            let now = current.get(repo);
            let mut compared = vec![
                COMPARED_REGISTRATION.to_string(),
                COMPARED_PLUGINS.to_string(),
            ];
            let requested = now.and_then(|now| now.layer_hints.clone()).map(normalized);
            if requested.is_some() {
                compared.push(COMPARED_LAYER_HINTS.to_string());
            }
            // A registration must be present on both sides: two absent
            // registrations do not make an inventory current.
            let current_basis = now.is_some_and(|now| {
                now.registered_source_id.is_some()
                    && now.registered_source_id == record.registered_source_id
                    && now.config_hash == record.config_hash
            });
            let derived = if current_basis {
                Some(derive(repo, &record.inventory_json)?)
            } else {
                None
            };
            let Some((commit, layer_hints, counts)) = derived.filter(|(_, hints, _)| {
                requested
                    .as_ref()
                    .is_none_or(|requested| requested == hints)
            }) else {
                qualified.push(RepoQualification {
                    repo: repo.to_string(),
                    status: CoverageStatus::Stale,
                    commit: None,
                    inventory_hash: Some(stored.inventory_hash.clone()),
                    limited: None,
                    counts: None,
                    layer_hints: None,
                    compared,
                });
                continue;
            };
            inventories.insert(
                repo.to_string(),
                (stored.inventory_hash.clone(), record.inventory_json.clone()),
            );
            qualified.push(RepoQualification {
                repo: repo.to_string(),
                status: CoverageStatus::Recorded,
                commit: Some(commit),
                inventory_hash: Some(stored.inventory_hash.clone()),
                limited: Some(counts.limited()),
                counts: Some(counts),
                layer_hints: Some(layer_hints),
                compared,
            });
        }
        let coverage_id = content_identity(
            "coverage-context-v1",
            &QualificationIdentity {
                contract: COVERAGE_CONTRACT,
                exhaustive: false,
                repos: &qualified,
                basis: &basis,
            },
        )?;
        Ok(Self {
            contract: COVERAGE_CONTRACT.to_string(),
            exhaustive: false,
            coverage_id,
            repos: qualified,
            inventories,
            basis,
        })
    }

    /// The recorded inventory behind `inventory_hash`, refusing a repo whose
    /// inventory is not recorded or has changed. Never substitutes another.
    pub fn inventory(&self, repo: &str, inventory_hash: &str) -> Result<&str, ContextError> {
        let qualified = self
            .repos
            .iter()
            .find(|qualified| qualified.repo == repo)
            .ok_or_else(|| ContextError::InventoryUnavailable(repo.to_string()))?;
        if qualified.inventory_hash.as_deref() != Some(inventory_hash) {
            return Err(ContextError::InventoryChanged(repo.to_string()));
        }
        self.inventories
            .get(repo)
            .filter(|(hash, _)| hash == inventory_hash)
            .map(|(_, body)| body.as_str())
            .ok_or_else(|| ContextError::InventoryUnavailable(repo.to_string()))
    }
}

/// A snapshot read under recorded coverage limits.
#[derive(Debug)]
pub struct CoverageContext {
    /// The held projection; `None` for a context whose facts are read from a
    /// store page by page (#1602).
    snapshot: Option<Arc<ContextSnapshot>>,
    snapshot_id: String,
    coverage: CoverageQualification,
    context_id: String,
}

/// A request continued only within one complete context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedRequest<T> {
    /// The inner request; its byte budget covers the whole qualified response.
    pub request: T,
    /// The context a continuation was issued in. Required with a cursor.
    #[serde(default)]
    pub context_id: Option<String>,
}

/// A page and the qualification it was read under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedResponse<T> {
    /// Identity of the graph snapshot and coverage block together.
    pub context_id: String,
    /// What the read could and could not see.
    pub coverage: CoverageQualification,
    /// The page.
    pub response: T,
}

#[derive(Serialize)]
struct ContextIdentity<'a> {
    snapshot_id: &'a str,
    coverage_id: &'a str,
}

impl ContextSnapshot {
    /// Pair this snapshot with coverage read in the same graph transaction.
    pub fn with_coverage(
        self,
        coverage: CoverageQualification,
    ) -> Result<CoverageContext, ContextError> {
        CoverageContext::from_shared_snapshot(Arc::new(self), coverage)
    }
}

impl CoverageContext {
    /// Qualify an existing immutable projection without rebuilding its facts,
    /// adjacency or identity. Coverage must originate from the same graph read,
    /// exactly as required by [`ContextSnapshot::with_coverage`].
    pub fn from_shared_snapshot(
        snapshot: Arc<ContextSnapshot>,
        coverage: CoverageQualification,
    ) -> Result<Self, ContextError> {
        let mut context = Self::for_identity(snapshot.id().to_owned(), coverage)?;
        context.snapshot = Some(snapshot);
        Ok(context)
    }

    /// Qualify a complete graph known only by its identity, whose facts a
    /// caller reads page by page through [`Self::query_source`] (#1602). The
    /// context id is the one [`Self::from_shared_snapshot`] gives for a
    /// snapshot with that identity. It holds no facts, so [`Self::query`] and
    /// [`Self::discover`] refuse with [`ContextError::FactsNotHeld`].
    pub fn for_identity(
        snapshot_id: String,
        coverage: CoverageQualification,
    ) -> Result<Self, ContextError> {
        let context_id = content_identity(
            "context-v2",
            &ContextIdentity {
                snapshot_id: &snapshot_id,
                coverage_id: &coverage.coverage_id,
            },
        )?;
        Ok(Self {
            snapshot: None,
            snapshot_id,
            coverage,
            context_id,
        })
    }

    fn held(&self) -> Result<&ContextSnapshot, ContextError> {
        self.snapshot.as_deref().ok_or(ContextError::FactsNotHeld)
    }

    /// Test whether this context shares the supplied graph projection allocation.
    /// This exposes no graph facts or unqualified query access.
    pub fn shares_snapshot(&self, snapshot: &Arc<ContextSnapshot>) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, snapshot))
    }

    /// Graph-only snapshot identity, unchanged from [`ContextSnapshot::id`].
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    /// Identity of the coverage block.
    pub fn coverage_id(&self) -> &str {
        &self.coverage.coverage_id
    }

    /// Identity of the snapshot and coverage together.
    pub fn context_id(&self) -> &str {
        &self.context_id
    }

    /// The qualification every response carries.
    pub fn coverage(&self) -> &CoverageQualification {
        &self.coverage
    }

    /// Query under the qualification.
    pub fn query(
        &self,
        request: QualifiedRequest<QueryRequest>,
    ) -> Result<QualifiedResponse<QueryResponse>, ContextError> {
        let mut held = self.held()?;
        self.query_source(request, &mut held)
    }

    /// [`Self::query`] over `source`, whose facts are this context's complete
    /// graph (#1602).
    pub fn query_source<S: crate::FactSource>(
        &self,
        request: QualifiedRequest<QueryRequest>,
        source: &mut S,
    ) -> Result<QualifiedResponse<QueryResponse>, S::Error> {
        request.request.validate_limits()?;
        let continued = request.request.cursor.is_some();
        let mut inner = request.request;
        inner.max_bytes = self.inner_budget(inner.max_bytes, continued, &request.context_id)?;
        let response = crate::query_source(&self.snapshot_id, inner, source)?;
        Ok(self.respond(response)?)
    }

    /// Discover candidates from the shared snapshot under its recorded coverage.
    /// The byte cap includes the complete qualification envelope, and a cursor
    /// continues only under the context that issued it. Discovery's own coverage
    /// describes matching work; the outer coverage qualifies producer inputs.
    pub fn discover(
        &self,
        request: QualifiedRequest<DiscoveryRequest>,
    ) -> Result<QualifiedResponse<DiscoveryResponse>, ContextError> {
        super::validate_limit(
            "max_candidates",
            request.request.max_candidates,
            super::discovery::MAX_CANDIDATES,
        )?;
        super::validate_limit(
            "max_bytes",
            request.request.max_bytes,
            super::MAX_RESPONSE_BYTES,
        )?;
        let continued = request.request.cursor.is_some();
        let mut inner = request.request;
        inner.max_bytes = self.inner_budget(inner.max_bytes, continued, &request.context_id)?;
        self.respond(self.held()?.discover(inner)?)
    }

    /// The inner page's share of `max_bytes`, after the qualification
    /// envelope. A qualification that cannot fit fails; it is never dropped.
    fn inner_budget(
        &self,
        max_bytes: usize,
        continued: bool,
        context_id: &Option<String>,
    ) -> Result<usize, ContextError> {
        if (continued || context_id.is_some())
            && context_id.as_deref() != Some(self.context_id.as_str())
        {
            return Err(ContextError::CursorContextMismatch);
        }
        #[derive(Serialize)]
        struct Envelope<'a> {
            context_id: &'a str,
            coverage: &'a CoverageQualification,
            response: (),
        }
        let envelope = crate::logical::encoded_size(&Envelope {
            context_id: &self.context_id,
            coverage: &self.coverage,
            response: (),
        })? - "null".len();
        max_bytes
            .checked_sub(envelope)
            .filter(|inner| *inner > 0)
            .ok_or(ContextError::ResponseBudgetExceeded {
                max_bytes,
                required_bytes: envelope + 1,
            })
    }

    fn respond<T>(&self, response: T) -> Result<QualifiedResponse<T>, ContextError> {
        Ok(QualifiedResponse {
            context_id: self.context_id.clone(),
            coverage: self.coverage.clone(),
            response,
        })
    }
}

#[cfg(test)]
mod tests;
