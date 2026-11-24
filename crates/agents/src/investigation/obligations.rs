//! Immutable generation-4 requests and mixed completion-obligation identities.
//! Acquisition, execution and public availability remain host responsibilities.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// Share the existing request/inspection admission bounds without changing old encodings.
/// Generation-4 whole-request, envelope and individual target encoding cap.
pub const COMPLETION_REQUEST_BYTES: usize = INVESTIGATION_V1_POLICY.request_bytes();
/// Raw mixed-obligation admission cap, applied before ordering or deduplication.
pub const MAX_COMPLETION_OBLIGATIONS: usize = INVESTIGATION_V1_POLICY.max_completion_obligations();
const CONTRACT: &str = "investigation-completion-obligations@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryTarget {
    pub context_id: String,
    pub repo: String,
    pub inventory_hash: String,
}
impl InventoryTarget {
    fn validate(&self) -> Result<(), InvestigationError> {
        if !self
            .context_id
            .strip_prefix("context-v2:")
            .is_some_and(crate::source_basis::hash)
            || (self.repo.is_empty()
                || self.repo.len() > core_graph::coverage::MAX_IDENTITY_BYTES
                || self.repo.contains('\0'))
            || !crate::source_basis::hash(&self.inventory_hash)
        {
            return Err(InvestigationError::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Obligation {
    Source {
        obligation_id: String,
        target: Box<InvestigationInspectionTarget>,
    },
    Inventory {
        obligation_id: String,
        target: InventoryTarget,
    },
}
impl Obligation {
    pub fn content_id(&self) -> Result<String, InvestigationError> {
        let (domain, bytes) = match self {
            Self::Source { target, .. } => {
                target.validate()?;
                (
                    b"cartograph:source-obligation:v1\0".as_slice(),
                    bounded_bytes(target, COMPLETION_REQUEST_BYTES)?,
                )
            }
            Self::Inventory { target, .. } => {
                target.validate()?;
                (
                    b"cartograph:inventory-obligation:v1\0".as_slice(),
                    bounded_bytes(target, COMPLETION_REQUEST_BYTES)?,
                )
            }
        };
        Ok(crate::source_basis::domain_hash(domain, &bytes))
    }
    pub fn key(&self) -> (&str, &str) {
        match self {
            Self::Source { obligation_id, .. } => ("source", obligation_id),
            Self::Inventory { obligation_id, .. } => ("inventory", obligation_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationEnvelope {
    pub schema_version: u32,
    pub contract: String,
    pub obligations: Vec<Obligation>,
}
impl ObligationEnvelope {
    pub fn normalized(&self) -> Result<Self, InvestigationError> {
        self.normalized_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn normalized_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        let (schema, contract, _) = envelope_family(policy)?;
        if self.schema_version != schema
            || self.contract != contract
            || !(1..=policy.max_completion_obligations()).contains(&self.obligations.len())
        {
            return Err(InvestigationError::InvalidRequest);
        }
        bounded_bytes(self, policy.envelope_bytes())?;
        // Admit the complete raw list before any target/partition ownership.
        let mut entries = BTreeMap::new();
        let mut source_keys = BTreeMap::new();
        let mut inventory_keys = BTreeMap::new();
        let mut snapshot = None;
        for item in &self.obligations {
            if item.key().1 != item.content_id()? {
                return Err(InvestigationError::InvalidRequest);
            }
            if let Obligation::Source { target, .. } = item {
                if snapshot.is_some_and(|old| old != &target.graph_snapshot_id) {
                    return Err(InvestigationError::InvalidRequest);
                }
                snapshot = Some(&target.graph_snapshot_id);
                let key = bounded_bytes(&(&target.fact, target.role, target.index), 4096)?;
                if source_keys
                    .insert(key, &target.target_id)
                    .is_some_and(|old| old != &target.target_id)
                {
                    return Err(InvestigationError::InvalidRequest);
                }
            }
            if let Obligation::Inventory { target, .. } = item
                && inventory_keys
                    .insert((&target.context_id, &target.repo), &target.inventory_hash)
                    .is_some_and(|old| old != &target.inventory_hash)
            {
                return Err(InvestigationError::InvalidRequest);
            }
            if entries
                .insert(item.key(), item)
                .is_some_and(|old| old != item)
            {
                return Err(InvestigationError::InvalidRequest);
            }
        }
        Ok(Self {
            schema_version: schema,
            contract: contract.into(),
            obligations: entries.into_values().cloned().collect(),
        })
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.fingerprint_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn fingerprint_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        let (_, _, domain) = envelope_family(policy)?;
        Ok(crate::source_basis::domain_hash(
            domain,
            &bounded_bytes(
                &self.normalized_with_policy(policy)?,
                policy.envelope_bytes(),
            )?,
        ))
    }
    /// Projection is for future authoritative host validation, not another authority.
    pub fn sources(&self) -> Result<Vec<InvestigationInspectionTarget>, InvestigationError> {
        self.sources_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn sources_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<Vec<InvestigationInspectionTarget>, InvestigationError> {
        Ok(self
            .normalized_with_policy(policy)?
            .obligations
            .into_iter()
            .filter_map(|item| match item {
                Obligation::Source { target, .. } => Some(*target),
                _ => None,
            })
            .collect())
    }
}

fn envelope_family(
    policy: &InvestigationPolicy,
) -> Result<(u32, &'static str, &'static [u8]), InvestigationError> {
    match policy.profile() {
        "investigation-v1" => Ok((1, CONTRACT, b"cartograph:completion-obligations:v1\0")),
        "investigation-v2" => Ok((
            2,
            "investigation-completion-obligations@2",
            b"cartograph:completion-obligations:v2\0",
        )),
        _ => Err(InvestigationError::InvalidRequest),
    }
}

/// Generation4 is a distinct closed request family. The transport selects this
/// arm explicitly; legacy StartInvestigationRequest never accepts V7 roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedObligationRequest {
    pub schema_version: u32,
    pub request_nonce: String,
    pub specialist_id: String,
    pub obligations: ObligationEnvelope,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_legacy"
    )]
    pub inspection_requirements: Option<Vec<InvestigationInspectionTarget>>,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub limit_profile: String,
    pub expected_graph_revision: Option<String>,
    pub expected_context_id: String,
    pub conversation_id: Option<String>,
    pub parent_id: Option<String>,
}
// Presence-only decoder: a present array is rejected later, while null must
// never collapse to the absent legacy field and bypass the authority check.
fn present_legacy<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<InvestigationInspectionTarget>>, D::Error> {
    Vec::deserialize(d).map(Some)
}
impl StagedObligationRequest {
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        Self::decode_with_policy(raw, &INVESTIGATION_V1_POLICY)
    }
    /// The transport must select a registered policy before decoding this family.
    pub fn decode_with_policy(
        raw: &str,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        super::strict::preflight(raw, policy.request_bytes(), 32, 4096, 4096)?;
        let value: Self =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidRequest)?;
        // Only registered completion generations enter normalization; schema,
        // specialist, envelope and immutable policy must still match exactly.
        if !matches!(value.schema_version, 4..=7) {
            return Err(InvestigationError::InvalidRequest);
        }
        value.normalized_with_policy(policy)
    }
    pub fn normalized(&self) -> Result<Self, InvestigationError> {
        self.normalized_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn normalized_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        // Whole request admission must precede envelope partition clones.
        bounded_bytes(self, policy.request_bytes())?;
        let (schema, roles, _) = request_family(policy, self.schema_version, &self.specialist_id)?;
        if self.schema_version != schema
            || self.inspection_requirements.is_some()
            || !roles.contains(&self.specialist_id.as_str())
        {
            return Err(InvestigationError::InvalidRequest);
        }
        super::records::CommonRequestFields {
            request_nonce: &self.request_nonce,
            question: &self.question,
            scope: &self.scope,
            limit_profile: &self.limit_profile,
            expected_graph_revision: self.expected_graph_revision.as_deref(),
            expected_context_id: Some(&self.expected_context_id),
            conversation_id: self.conversation_id.as_deref(),
            parent_id: self.parent_id.as_deref(),
        }
        .validate_with_policy(policy)?;
        // The former V6 projection's 8 KiB bound was redundant: the admitted
        // whole V7 request retains the same common-field encodings, a role at
        // least as long as domain-analyst@6, and the additional obligations.
        // V7 also retains null optional IDs that the projection omitted. No
        // projection allocation or legacy-family validation is needed here.
        for item in &self.obligations.obligations {
            match item {
                Obligation::Source { target, .. }
                    if self.expected_graph_revision.as_deref()
                        != Some(target.graph_snapshot_id.as_str()) =>
                {
                    return Err(InvestigationError::InvalidRequest);
                }
                Obligation::Inventory { target, .. }
                    if target.context_id != self.expected_context_id =>
                {
                    return Err(InvestigationError::InvalidRequest);
                }
                _ => (),
            }
        }
        let obligations = self.obligations.normalized_with_policy(policy)?;
        Ok(Self {
            obligations,
            ..self.clone()
        })
    }
    /// Bind a normalized V7 intent to its retained qualified acquisition basis.
    pub fn validate_ledger(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<(), InvestigationError> {
        self.validate_ledger_with_policy(ledger, &INVESTIGATION_V1_POLICY)
    }
    pub fn validate_ledger_with_policy(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        let request = self.normalized_with_policy(policy)?;
        let role = serde_json::from_value(serde_json::Value::String(request.specialist_id.clone()))
            .map_err(|_| InvestigationError::InvalidRequest)?;
        ledger.validate_for_specialist(role, policy)?;
        let proof = ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if proof.context_id != request.expected_context_id
            || request
                .expected_graph_revision
                .as_deref()
                .is_some_and(|graph| graph != ledger.graph_snapshot_id)
            || request
                .obligations
                .obligations
                .iter()
                .any(|item| match item {
                    Obligation::Inventory { target, .. } => {
                        !proof.qualification.repos.iter().any(|repo| {
                            repo.repo == target.repo
                                && repo.status == context_hub::coverage::CoverageStatus::Recorded
                                && repo.inventory_hash.as_deref()
                                    == Some(target.inventory_hash.as_str())
                        })
                    }
                    Obligation::Source { target, .. } => {
                        target.graph_snapshot_id != ledger.graph_snapshot_id
                    }
                })
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
    pub fn intent_hash(&self) -> Result<String, InvestigationError> {
        self.intent_hash_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn intent_hash_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        // Preserve the existing whole-request capacity error before family
        // rejection, even if an oversized request also has a mismatched role.
        let request = self.normalized_with_policy(policy)?;
        let (_, _, domain) =
            request_family(policy, request.schema_version, &request.specialist_id)?;
        Ok(crate::source_basis::domain_hash(
            domain,
            &bounded_bytes(&request, policy.request_bytes())?,
        ))
    }
}

fn request_family(
    policy: &InvestigationPolicy,
    schema_version: u32,
    specialist_id: &str,
) -> Result<(u32, &'static [&'static str], &'static [u8]), InvestigationError> {
    // Policy alone cannot select an immutable request/prompt family, including
    // when hashing saved requests. Public request7 and historical requests
    // retain their exact schema, specialist and policy tuple.
    match (policy.profile(), schema_version, specialist_id) {
        (
            "investigation-v1",
            4,
            "domain-analyst@7" | "evidence-auditor@7" | "spec-writer@1" | "spec-writer@2"
            | "gap-triager@1" | "gap-triager@2" | "section-writer@1",
        ) => Ok((
            4,
            &[
                "domain-analyst@7",
                "evidence-auditor@7",
                "spec-writer@1",
                "spec-writer@2",
                "gap-triager@1",
                "gap-triager@2",
                "section-writer@1",
            ],
            b"cartograph:investigation-client-intent:v4\0",
        )),
        ("investigation-v2", 5, "domain-analyst@8" | "evidence-auditor@8") => Ok((
            5,
            &["domain-analyst@8", "evidence-auditor@8"],
            b"cartograph:investigation-client-intent:v5\0",
        )),
        ("investigation-v2", 6, "domain-analyst@9" | "evidence-auditor@9") => Ok((
            6,
            &["domain-analyst@9", "evidence-auditor@9"],
            b"cartograph:investigation-client-intent:v6\0",
        )),
        ("investigation-v2", 7, "domain-analyst@10" | "evidence-auditor@10") => Ok((
            7,
            &["domain-analyst@10", "evidence-auditor@10"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@11" | "evidence-auditor@11") => Ok((
            7,
            &["domain-analyst@11", "evidence-auditor@11"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        (
            "investigation-v2",
            7,
            "domain-analyst@15" | "domain-analyst@17" | "domain-analyst@18",
        ) => Ok((
            7,
            &[
                "domain-analyst@15",
                "domain-analyst@17",
                "domain-analyst@18",
            ],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "evidence-auditor@12") => Ok((
            7,
            &["evidence-auditor@12"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "evidence-auditor@13") => Ok((
            7,
            &["evidence-auditor@13"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "evidence-auditor@14") => Ok((
            7,
            &["evidence-auditor@14"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@19") => Ok((
            7,
            &["domain-analyst@19"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@20") => Ok((
            7,
            &["domain-analyst@20"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@21") => Ok((
            7,
            &["domain-analyst@21"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@22") => Ok((
            7,
            &["domain-analyst@22"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@23") => Ok((
            7,
            &["domain-analyst@23"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@24") => Ok((
            7,
            &["domain-analyst@24"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@25") => Ok((
            7,
            &["domain-analyst@25"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@26") => Ok((
            7,
            &["domain-analyst@26"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@27") => Ok((
            7,
            &["domain-analyst@27"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@28") => Ok((
            7,
            &["domain-analyst@28"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@29") => Ok((
            7,
            &["domain-analyst@29"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@30") => Ok((
            7,
            &["domain-analyst@30"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@31") => Ok((
            7,
            &["domain-analyst@31"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@32") => Ok((
            7,
            &["domain-analyst@32"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@14") => Ok((
            7,
            &["domain-analyst@14"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@13") => Ok((
            7,
            &["domain-analyst@13"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        ("investigation-v2", 7, "domain-analyst@12") => Ok((
            7,
            &["domain-analyst@12"],
            b"cartograph:investigation-client-intent:v7\0",
        )),
        _ => Err(InvestigationError::InvalidRequest),
    }
}

#[cfg(test)]
#[path = "admission_family_tests.rs"]
mod admission_family_tests;
#[cfg(test)]
#[path = "query_history_family_tests.rs"]
mod query_history_family_tests;
#[cfg(test)]
#[path = "obligations_tests.rs"]
mod tests;
