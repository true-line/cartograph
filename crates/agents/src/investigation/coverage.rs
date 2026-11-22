//! Immutable inventory detail admission for qualified investigations (AC-0239).
//! This value owns preparation-time data; it never reads the current graph.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{
    INVESTIGATION_V1_POLICY, InvestigationError, InvestigationPolicy, MAX_QUERY_BYTES,
    bounded_bytes,
};
use context_hub::coverage::CoverageProof;
use core_graph::coverage::StoredCoverage;
use serde::{Deserialize, Serialize};

/// Separate archive bound; these bodies are not eagerly included in prompts.
pub const MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
const ARCHIVE_DEPTH: usize = 32;
const ARCHIVE_VALUES: usize = 262144;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCoverageArchive {
    schema_version: u32,
    proof: CoverageProof,
    inventories: Vec<StoredCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCoverageRead {
    pub context_id: String,
    pub repo: String,
    pub inventory_hash: String,
    pub max_bytes: usize,
}

/// One acquisition of exact retained inventory detail. Repeated reads still
/// consume the shared raw acquisition budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCoverageReadManifest {
    pub request: InvestigationCoverageRead,
    pub response_hash: String,
    pub response_bytes: usize,
}

impl InvestigationCoverageReadManifest {
    pub fn validate(&self, proof: &CoverageProof) -> Result<(), InvestigationError> {
        if self.request.context_id != proof.context_id
            || !(1..=MAX_QUERY_BYTES).contains(&self.request.max_bytes)
            || self.response_bytes == 0
            || self.response_bytes > self.request.max_bytes
            || !crate::source_basis::hash(&self.response_hash)
            || !proof.qualification.repos.iter().any(|repo| {
                repo.repo == self.request.repo
                    && repo.status == context_hub::coverage::CoverageStatus::Recorded
                    && repo.inventory_hash.as_deref() == Some(self.request.inventory_hash.as_str())
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }

    pub fn admit_response(
        &self,
        proof: &CoverageProof,
        raw: &str,
    ) -> Result<InvestigationCoverageDetail, InvestigationError> {
        self.validate(proof)?;
        if raw.len() != self.response_bytes
            || core_prov::content_hash(raw.as_bytes()) != self.response_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        super::strict::preflight(raw, MAX_QUERY_BYTES, 16, 128, 32)?;
        let detail: InvestigationCoverageDetail =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)?;
        if detail.schema_version != 1
            || detail.context_id != self.request.context_id
            || detail.repo != self.request.repo
            || detail.inventory_hash != self.request.inventory_hash
            || core_prov::content_hash(detail.inventory_json.as_bytes()) != detail.inventory_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(detail)
    }
}

/// Untrusted inventory content, not graph evidence or a source citation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCoverageDetail {
    pub schema_version: u32,
    pub context_id: String,
    pub repo: String,
    pub inventory_hash: String,
    pub inventory_json: String,
}

impl InvestigationCoverageArchive {
    pub fn new(
        proof: CoverageProof,
        mut inventories: Vec<StoredCoverage>,
    ) -> Result<Self, InvestigationError> {
        inventories.sort_by(|a, b| a.record.repo_key.cmp(&b.record.repo_key));
        let archive = Self {
            schema_version: 1,
            proof,
            inventories,
        };
        archive.validate()?;
        Ok(archive)
    }

    /// SQL callers must preflight stored type/byte count before retrieving a body.
    /// This decoder also rejects unknown nested fields and duplicate JSON keys.
    pub fn decode(body: &str) -> Result<Self, InvestigationError> {
        super::strict::preflight(
            body,
            MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
            ARCHIVE_DEPTH,
            ARCHIVE_VALUES,
            context_hub::coverage::MAX_COVERAGE_REPOS,
        )?;
        let archive: Self =
            serde_json::from_str(body).map_err(|_| InvestigationError::InvalidInput)?;
        // Missing optional fields can expand during typed serialization. Bound
        // that encoding before allocating either comparison tree.
        let canonical = bounded_bytes(&archive, MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES)?;
        let original: serde_json::Value =
            serde_json::from_str(body).map_err(|_| InvestigationError::InvalidInput)?;
        let round_trip: serde_json::Value =
            serde_json::from_slice(&canonical).map_err(|_| InvestigationError::InvalidInput)?;
        if round_trip != original {
            return Err(InvestigationError::InvalidInput);
        }
        archive.validate()?;
        Ok(archive)
    }

    /// Logical (retained archive, decode peak), excluding the borrowed wire,
    /// SQL/schema owners, caller task, capacity slack and allocator/native RSS.
    /// Shape admission precedes typed decode; semantic field limits do not.
    pub fn decode_workspace_bytes() -> Option<(usize, usize)> {
        use context_hub::coverage::{CoverageBasis, MAX_COVERAGE_REPOS, RepoQualification};
        use context_hub::logical::{slots, sum};
        let bytes = MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES;
        let retained = sum(&[
            slots::<Self>(1)?,
            bytes, // aggregate decoded string payload cannot exceed admitted JSON
            slots::<StoredCoverage>(MAX_COVERAGE_REPOS)?,
            slots::<CoverageBasis>(MAX_COVERAGE_REPOS)?,
            slots::<RepoQualification>(MAX_COVERAGE_REPOS)?,
            // compared/layer_hints vectors can be populated before validation.
            // Charge every admitted value as a String slot, including malformed
            // input; per-repo semantic limits are not yet established.
            slots::<String>(ARCHIVE_VALUES)?,
        ])?;
        // decode retains canonical bytes and both comparison Values while
        // validate runs. The scratch helper includes parser escape storage too;
        // summing validation is deliberately conservative for that overlap.
        let peak = sum(&[
            retained,
            super::record_workspace::scratch(bytes, ARCHIVE_DEPTH)?,
            super::coverage_archive_validation_workspace()?,
        ])?;
        Some((retained, peak))
    }

    /// Reconcile saved acquisition metadata with this archive, never live data.
    pub fn validate_reads(
        &self,
        reads: &[InvestigationCoverageReadManifest],
    ) -> Result<(), InvestigationError> {
        self.validate_reads_with_policy(reads, &INVESTIGATION_V1_POLICY)
    }

    /// Reconcile under the exact task policy while retaining the archive's
    /// structural and individual response limits.
    pub fn validate_reads_with_policy(
        &self,
        reads: &[InvestigationCoverageReadManifest],
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if reads.len() > policy.limits().tool_actions as usize {
            return Err(InvestigationError::LimitExceeded);
        }
        for read in reads {
            let detail = self.read(&read.request)?;
            let raw = bounded_bytes(&detail, read.request.max_bytes)?;
            if raw.len() != read.response_bytes
                || core_prov::content_hash(&raw) != read.response_hash
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        Ok(())
    }

    pub fn proof(&self) -> &CoverageProof {
        &self.proof
    }

    /// Validate again after durable decoding; deserialization grants no access.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1 || self.inventories.len() > 4096 {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES)?;
        if self
            .inventories
            .windows(2)
            .any(|pair| pair[0].record.repo_key >= pair[1].record.repo_key)
        {
            return Err(InvestigationError::InvalidInput);
        }
        self.proof
            .validate_inventories(&self.inventories)
            .map_err(|_| InvestigationError::InvalidInput)?;
        Ok(())
    }

    /// Logical (retained detail, peak) for read(), excluding the borrowed
    /// archive/request. Successful lookup bounds identities before cloning;
    /// the body can fit while the later serialized envelope is refused.
    pub fn read_workspace_bytes() -> Option<(usize, usize)> {
        use context_hub::logical::{slots, sum};
        let retained = sum(&[
            slots::<InvestigationCoverageDetail>(1)?,
            "context-v2:".len().checked_add(64)?,
            context_hub::coverage::MAX_COVERAGE_IDENTITY_BYTES,
            64,
            MAX_QUERY_BYTES,
        ])?;
        let encoding = sum(&[retained, MAX_QUERY_BYTES, slots::<Vec<u8>>(1)?])?;
        Some((
            retained,
            encoding.max(super::coverage_archive_validation_workspace()?),
        ))
    }

    /// The response envelope consumes the caller's budget along with the body.
    pub fn read(
        &self,
        request: &InvestigationCoverageRead,
    ) -> Result<InvestigationCoverageDetail, InvestigationError> {
        self.validate()?;
        if !(1..=MAX_QUERY_BYTES).contains(&request.max_bytes) {
            return Err(InvestigationError::LimitExceeded);
        }
        if request.context_id != self.proof.context_id {
            return Err(InvestigationError::InvalidInput);
        }
        let inventory = self
            .inventories
            .iter()
            .find(|inventory| {
                inventory.record.repo_key == request.repo
                    && inventory.inventory_hash == request.inventory_hash
            })
            .ok_or(InvestigationError::InvalidInput)?;
        if inventory.record.inventory_json.len() > request.max_bytes {
            return Err(InvestigationError::LimitExceeded);
        }
        let detail = InvestigationCoverageDetail {
            schema_version: 1,
            context_id: self.proof.context_id.clone(),
            repo: request.repo.clone(),
            inventory_hash: inventory.inventory_hash.clone(),
            inventory_json: inventory.record.inventory_json.clone(),
        };
        bounded_bytes(&detail, request.max_bytes)?;
        Ok(detail)
    }
}

#[cfg(test)]
#[path = "ledger_policy_tests.rs"]
mod ledger_policy_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use context_hub::{
        ContextSnapshot,
        coverage::{CoverageQualification, ScopedRepo},
    };
    use core_graph::coverage::CoverageRecord;
    use serde_json::json;

    pub(super) fn fixture() -> (InvestigationCoverageArchive, InvestigationCoverageRead) {
        fixture_with_inventory_padding(0)
    }

    fn fixture_with_inventory_padding(
        padding: usize,
    ) -> (InvestigationCoverageArchive, InvestigationCoverageRead) {
        let record = CoverageRecord {
            repo_key: "acme/api".into(),
            registered_source_id: Some("src:api".into()),
            ingest_id: "job:1".into(),
            config_hash: "plugins:1".into(),
            inventory_json: json!({"inventory":"coverage@1", "repo":"acme/api",
                "commit":"a".repeat(40), "layer_hints":[], "producers":[],
                "unsupported":[], "limited":false})
            .to_string()
                + &" ".repeat(padding),
        };
        let stored = StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        };
        let qualification = CoverageQualification::new(
            &[ScopedRepo {
                repo_key: "acme/api".into(),
                registered_source_id: Some("src:api".into()),
                config_hash: "plugins:1".into(),
                layer_hints: None,
            }],
            std::slice::from_ref(&stored),
        )
        .unwrap();
        let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
        let proof = qualification.durable_proof(snapshot.id()).unwrap();
        let request = InvestigationCoverageRead {
            context_id: proof.context_id.clone(),
            repo: "acme/api".into(),
            inventory_hash: stored.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        };
        (
            InvestigationCoverageArchive::new(proof, vec![stored]).unwrap(),
            request,
        )
    }

    // AC-0327: actual archive serialization and sequential original validation.
    #[test]
    fn archive_validation_workspace_covers_real_retained_inventory() {
        let (archive, _) = fixture_with_inventory_padding(8192);
        archive.validate().unwrap();
        let wire = bounded_bytes(&archive, MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES).unwrap();
        let bound = super::super::coverage_archive_validation_workspace().unwrap();
        assert!(bound >= wire.len());
        assert!(
            bound
                >= context_hub::logical::coverage_batch_validation_workspace(
                    context_hub::logical::count(archive.proof()).unwrap(),
                    archive.inventories[0].record.inventory_json.len(),
                    archive.proof.qualification.repos.len(),
                )
                .unwrap()
        );
        let mut stale = archive;
        stale.inventories[0].record.ingest_id.push_str(":changed");
        assert!(matches!(
            stale.validate(),
            Err(InvestigationError::InvalidInput)
        ));
    }

    // AC-0239: progressive detail binds the retained preparation-time identity,
    // not a current graph lookup; exact UTF-8 response bytes include its envelope.
    #[test]
    fn retained_detail_round_trips_and_accounts_for_envelope() {
        let (archive, mut request) = fixture();
        let detail = archive.read(&request).unwrap();
        let encoded = serde_json::to_string(&archive).unwrap();
        let restored = InvestigationCoverageArchive::decode(&encoded).unwrap();
        let (archive_owner, decode_peak) =
            InvestigationCoverageArchive::decode_workspace_bytes().unwrap();
        let actual_slots = std::mem::size_of::<InvestigationCoverageArchive>()
            + archive.inventories.len() * std::mem::size_of::<StoredCoverage>()
            + archive.proof.basis.len()
                * std::mem::size_of::<context_hub::coverage::CoverageBasis>()
            + archive.proof.qualification.repos.len()
                * std::mem::size_of::<context_hub::coverage::RepoQualification>()
            + archive
                .proof
                .qualification
                .repos
                .iter()
                .map(|repo| {
                    (repo.compared.len() + repo.layer_hints.as_ref().map_or(0, Vec::len))
                        * std::mem::size_of::<String>()
                })
                .sum::<usize>();
        // Wire length is used only as a conservative payload charge; DTO and
        // nested vector element owners are counted separately above.
        assert!(archive_owner >= actual_slots + encoded.len());
        assert!(
            decode_peak
                >= archive_owner + super::super::coverage_archive_validation_workspace().unwrap()
        );
        assert_eq!(restored.read(&request).unwrap(), detail);
        request.max_bytes = serde_json::to_vec(&detail).unwrap().len();
        assert_eq!(restored.read(&request).unwrap(), detail);
        let (retained, peak) = InvestigationCoverageArchive::read_workspace_bytes().unwrap();
        let actual = std::mem::size_of::<InvestigationCoverageDetail>()
            + detail.context_id.len()
            + detail.repo.len()
            + detail.inventory_hash.len()
            + detail.inventory_json.len();
        assert!(retained >= actual);
        assert!(peak >= retained + request.max_bytes);
        assert!(detail.inventory_json.len() < request.max_bytes - 1);
        request.max_bytes -= 1;
        assert_eq!(
            restored.read(&request),
            Err(InvestigationError::LimitExceeded)
        );
    }

    // AC-0327: the archive keeps its distinct ceiling, including escaped
    // inventory JSON, and accepts the exact input boundary but not one byte more.
    #[test]
    fn archive_decode_preserves_distinct_byte_ceiling() {
        let (archive, _) = fixture_with_inventory_padding(super::super::MAX_RECORD_BYTES);
        let mut encoded = serde_json::to_string(&archive).unwrap();
        assert!(encoded.len() > super::super::MAX_RECORD_BYTES);
        assert_eq!(
            InvestigationCoverageArchive::decode(&encoded).unwrap(),
            archive
        );
        encoded.push_str(&" ".repeat(MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES - encoded.len()));
        assert_eq!(
            InvestigationCoverageArchive::decode(&encoded).unwrap(),
            archive
        );
        encoded.push(' ');
        assert_eq!(
            InvestigationCoverageArchive::decode(&encoded),
            Err(InvestigationError::LimitExceeded)
        );
    }

    // AC-0327: a missing Option expands to null. Refuse canonical bytes before
    // comparing Values or running semantic validation of this untrusted archive.
    #[test]
    fn archive_decode_bounds_canonical_expansion_before_comparison() {
        let (archive, _) = fixture();
        let mut value = serde_json::to_value(archive).unwrap();
        let record = value["inventories"][0]["record"].as_object_mut().unwrap();
        record.remove("registered_source_id");
        record.insert("inventory_json".into(), "".into());
        let overhead = serde_json::to_vec(&value).unwrap().len();
        value["inventories"][0]["record"]["inventory_json"] = "x"
            .repeat(MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES - overhead)
            .into();
        let body = serde_json::to_string(&value).unwrap();
        assert_eq!(body.len(), MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES);
        assert_eq!(
            InvestigationCoverageArchive::decode(&body),
            Err(InvestigationError::LimitExceeded)
        );
    }

    // AC-0239: a new context, wrong hash, or edited body never substitutes detail.
    #[test]
    fn retained_detail_rejects_context_hash_and_body_substitution() {
        let (archive, request) = fixture();
        let mut changed = request.clone();
        changed.context_id.push('0');
        assert!(archive.read(&changed).is_err());
        changed = request.clone();
        changed.inventory_hash.push('0');
        assert!(archive.read(&changed).is_err());
        let mut changed = archive.clone();
        changed.inventories[0].record.inventory_json.push(' ');
        assert!(changed.read(&request).is_err());
        let mut missing = archive.clone();
        missing.inventories.clear();
        assert!(missing.validate().is_err());
        let mut duplicate = archive.clone();
        duplicate.inventories.push(archive.inventories[0].clone());
        assert!(duplicate.validate().is_err());
    }
    // AC-0239: nested archive metadata is a closed durable contract.
    #[test]
    fn retained_detail_decode_rejects_unknown_nested_metadata() {
        let (archive, _) = fixture();
        let mut encoded = serde_json::to_value(&archive).unwrap();
        encoded["inventories"][0]["record"]["latest"] = true.into();
        assert!(InvestigationCoverageArchive::decode(&encoded.to_string()).is_err());
        let duplicate = serde_json::to_string(&archive).unwrap().replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        );
        assert!(InvestigationCoverageArchive::decode(&duplicate).is_err());
        let nested_duplicate = serde_json::to_string(&archive).unwrap().replacen(
            "\"repo_key\":\"acme/api\"",
            "\"repo_key\":\"acme/api\",\"repo_key\":\"acme/api\"",
            1,
        );
        assert!(InvestigationCoverageArchive::decode(&nested_duplicate).is_err());
    }
    // AC-0239: stale and unrecorded repos stay qualified but grant no detail.
    #[test]
    fn stale_and_unrecorded_qualification_never_authorizes_inventory() {
        let (original, request) = fixture();
        let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
        let qualification = CoverageQualification::new(
            &[ScopedRepo {
                repo_key: "acme/new".into(),
                registered_source_id: Some("src:new".into()),
                config_hash: "plugins:1".into(),
                layer_hints: None,
            }],
            &original.inventories,
        )
        .unwrap();
        let proof = qualification.durable_proof(snapshot.id()).unwrap();
        let archive = InvestigationCoverageArchive::new(proof.clone(), vec![]).unwrap();
        let mut stale_request = request;
        stale_request.context_id = proof.context_id.clone();
        assert!(archive.read(&stale_request).is_err());
        stale_request.repo = "acme/new".into();
        assert!(archive.read(&stale_request).is_err());
        assert!(InvestigationCoverageArchive::new(proof, original.inventories).is_err());
    }
    // AC-0239: legacy ledgers retain their wire/domain and cannot carry a
    // qualification that an old prompt would omit.
    #[test]
    fn qualified_ledger_binds_proof_without_changing_legacy_wire() {
        use crate::investigation::*;
        let (archive, _) = fixture();
        let mut ledger = InvestigationInputLedger {
            coverage_reads: vec![],
            artifact_reads: vec![],
            schema_version: 1,
            coverage: None,
            graph_snapshot_id: archive.proof.snapshot_id.clone(),
            scope_snapshot_id: archive.proof.snapshot_id.clone(),
            revision: 1,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: None,
            supplied_history_bytes: 0,
        };
        let original = serde_json::to_string(&ledger).unwrap();
        assert!(!original.contains("coverage"));
        assert_eq!(
            ledger.fingerprint().unwrap(),
            crate::source_basis::domain_hash(
                b"cartograph:investigation-input-ledger:v1\0",
                original.as_bytes()
            )
        );
        ledger.coverage = Some(archive.proof.clone());
        assert!(ledger.validate().is_err());
        ledger.schema_version = 2;
        ledger.validate().unwrap();
        let fingerprint = ledger.fingerprint().unwrap();
        assert_ne!(
            fingerprint,
            crate::source_basis::domain_hash(
                b"cartograph:investigation-input-ledger:v1\0",
                &serde_json::to_vec(&ledger).unwrap()
            )
        );
        let restored: InvestigationInputLedger =
            decode_record(&serde_json::to_string(&ledger).unwrap()).unwrap();
        assert_eq!(restored.fingerprint().unwrap(), fingerprint);
        assert!(
            InvestigationInput::prepare(
                "Inspect coverage",
                SpecialistId::DomainAnalystV3,
                ledger.clone(),
                &[],
                &[],
                &[],
                &InvestigationUsage::default()
            )
            .is_err()
        );
        ledger.scope_snapshot_id = format!("context-v1:{}", "a".repeat(64));
        assert!(ledger.validate().is_err());
        ledger.coverage = None;
        assert!(ledger.validate().is_err());
    }
    // AC-0239: equal graph bytes do not permit a later ingest basis to replace
    // the qualification of an already running investigation.
    #[test]
    fn qualified_turn_rejects_new_coverage_over_identical_graph() {
        use crate::investigation::*;
        let (archive, _) = fixture();
        let previous = InvestigationInputLedger {
            coverage_reads: vec![],
            artifact_reads: vec![],
            schema_version: 2,
            coverage: Some(archive.proof.clone()),
            graph_snapshot_id: archive.proof.snapshot_id.clone(),
            scope_snapshot_id: archive.proof.snapshot_id.clone(),
            revision: 0,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: None,
            supplied_history_bytes: 0,
        };
        let mut current = previous.clone();
        current.revision = 1;
        current.queries.push(InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: MAX_QUERY_BYTES,
                cursor: None,
            },
            response_hash: core_prov::content_hash(b"{}"),
            response_bytes: 2,
            returned_facts: 0,
            total_selected: 0,
            has_more: false,
        });
        let mut basis = InvestigationTurnBasis {
            input_ledger_hash: current.fingerprint().unwrap(),
            previous_ledger: Some(previous),
            completed_tool: Some(InvestigationTool::QueryContext),
        };
        basis.validate(&current).unwrap();
        let mut replaced = archive.inventories;
        replaced[0].record.ingest_id = "job:2".into();
        current.coverage = Some(
            CoverageQualification::new(
                &[ScopedRepo {
                    repo_key: "acme/api".into(),
                    registered_source_id: Some("src:api".into()),
                    config_hash: "plugins:1".into(),
                    layer_hints: None,
                }],
                &replaced,
            )
            .unwrap()
            .durable_proof(&current.scope_snapshot_id)
            .unwrap(),
        );
        current.validate().unwrap();
        basis.input_ledger_hash = current.fingerprint().unwrap();
        assert!(basis.validate(&current).is_err());
    }
}

#[cfg(test)]
#[path = "coverage_detail_tests.rs"]
mod detail_tests;

#[cfg(test)]
#[path = "artifact_read_ledger_tests.rs"]
mod artifact_read_tests;
