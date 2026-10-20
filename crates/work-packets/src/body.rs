//! Typed, immutable seven-group packet body. No type grants execution authority.
#![allow(missing_docs)]

use crate::{
    ArtifactSelection, CanonicalLimits, JsonInputLimits, JsonPathPattern, canonical_json,
    preflight_json_scoped,
};
use agents::AssertionDecisionRecord;
use core_graph::{
    Edge, Node,
    omission::{FillSeam, Omission, OmissionClass},
    source::{FactKey, SourceBinding, edge_digest, node_digest},
};
use core_prov::Provenance;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use source_capture::CaptureSpanRef;
use spec::SpecArtifact;
use std::{collections::BTreeSet, io::Write};

pub const MAX_BODY_BYTES: usize = 896 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PacketError {
    #[error("invalid packet: {0}")]
    InvalidInput(&'static str),
    #[error("packet limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("conflicting packet entries: {0}")]
    SelectionConflict(&'static str),
    #[error("packet not found")]
    NotFound,
    #[error("review conflict at revision {revision} ({status})")]
    ReviewConflict { revision: u64, status: String },
    #[error("packet storage: {0}")]
    Storage(#[from] rusqlite::Error),
}
pub type PacketResult<T> = Result<T, PacketError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    RequestedIntent,
    Evidence { evidence_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Known<T> {
    Supplied {
        value: T,
        origin: Origin,
    },
    Missing {
        #[serde(deserialize_with = "StrictOmission::deserialize")]
        omission: Omission,
    },
}
// Keep packet protocol decoding strict without changing legacy graph readers.
#[derive(Deserialize)]
#[serde(remote = "Omission", deny_unknown_fields)]
struct StrictOmission {
    class: OmissionClass,
    seam: FillSeam,
    detail: String,
}

impl<T> Known<T> {
    pub fn requested(value: T) -> Self {
        Self::Supplied {
            value,
            origin: Origin::RequestedIntent,
        }
    }
    pub fn missing(detail: impl Into<String>) -> Self {
        Self::Missing {
            omission: missing_omission(detail),
        }
    }
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Supplied { value, .. } => Some(value),
            Self::Missing { .. } => None,
        }
    }
}
pub fn missing_omission(detail: impl Into<String>) -> Omission {
    Omission {
        class: OmissionClass::MissingInput,
        seam: FillSeam::Interpretation,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketBody {
    pub schema_version: u32,
    pub identity: PacketIdentity,
    pub intent: PacketIntent,
    pub evidence: PacketEvidence,
    pub work: PacketWork,
    pub acceptance: PacketAcceptance,
    pub handoff: PacketHandoff,
    #[serde(rename = "return")]
    pub returns: PacketReturn,
}

macro_rules! protocol {
    ($name:ident {$($field:ident : $ty:ty),* $(,)?}) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $(pub $field: $ty),* }
    };
}
protocol!(ContentBinding {
    id: String,
    schema: String,
    content_id: String
});
protocol!(RecoveredBasis {
    view: String,
    snapshot_id: String
});
protocol!(SourceIdentity { registration_id: String, repo_key: String, revision: Known<String> });
protocol!(ConfigurationIdentity { name: String, binding: Known<ContentBinding> });
protocol!(PacketIdentity {
    generator_version: String, template_version: String, application: Known<ContentBinding>,
    recovered_basis: RecoveredBasis, sources: Vec<SourceIdentity>, configuration: Vec<ConfigurationIdentity>,
    input_closure: Known<Vec<String>>, curated_context: Known<String>
});
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketIntent {
    pub objective: Known<String>,
    pub expected_outcome: Known<String>,
    pub current_behavior: Known<String>,
    pub intended_behavior: Known<String>,
    pub scope: Known<String>,
    pub non_goals: Known<String>,
    pub business_rationale: Known<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_journey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
}
impl Default for PacketIntent {
    fn default() -> Self {
        Self {
            objective: Known::missing("intent.objective was not supplied"),
            expected_outcome: Known::missing("intent.expected_outcome was not supplied"),
            current_behavior: Known::missing("intent.current_behavior was not supplied"),
            intended_behavior: Known::missing("intent.intended_behavior was not supplied"),
            scope: Known::missing("intent.scope was not supplied"),
            non_goals: Known::missing("intent.non_goals was not supplied"),
            business_rationale: Known::missing("intent.business_rationale was not supplied"),
            requested_actor: None,
            requested_journey: None,
            requested_by: None,
        }
    }
}
protocol!(Component { repo_key: String, path: Option<String>, fact_references: Vec<FactKey> });
protocol!(Dependency { description: String, evidence_ids: Vec<String> });
protocol!(PacketWork { affected_components: Known<Vec<Component>>, proposed_approach: Known<Vec<String>>, dependencies: Known<Vec<Dependency>>, open_decisions: Known<Vec<String>> });
impl Default for PacketWork {
    fn default() -> Self {
        Self {
            affected_components: Known::missing("work.affected_components was not supplied"),
            proposed_approach: Known::missing("work.proposed_approach was not supplied"),
            dependencies: Known::missing("work.dependencies was not supplied"),
            open_decisions: Known::missing("work.open_decisions was not supplied"),
        }
    }
}
protocol!(AcceptanceProcedure { id: String, description: String, steps: Vec<String>, expected_evidence: Vec<String>, evidence_ids: Vec<String> });
protocol!(EvidenceObservation { id: String, description: String, evidence_ids: Vec<String> });
protocol!(PacketAcceptance {
    positive_cases: Known<Vec<AcceptanceProcedure>>, negative_cases: Known<Vec<AcceptanceProcedure>>, proposed_verification: Known<Vec<AcceptanceProcedure>>,
    discovered_tests: Vec<EvidenceObservation>, structural_test_associations: Vec<EvidenceObservation>, execution_evidence: Vec<EvidenceObservation>,
    coverage_evidence: Vec<EvidenceObservation>, assertion_support: Vec<EvidenceObservation>
});
impl Default for PacketAcceptance {
    fn default() -> Self {
        Self {
            positive_cases: Known::missing("acceptance.positive_cases was not supplied"),
            negative_cases: Known::missing("acceptance.negative_cases was not supplied"),
            proposed_verification: Known::missing(
                "acceptance.proposed_verification was not supplied",
            ),
            discovered_tests: vec![],
            structural_test_associations: vec![],
            execution_evidence: vec![],
            coverage_evidence: vec![],
            assertion_support: vec![],
        }
    }
}
protocol!(BudgetRequest {
    resource: String,
    limit: String
});
protocol!(PacketHandoff { requested_capabilities: Known<Vec<String>>, checkout_boundaries: Known<Vec<String>>, tool_and_egress_requests: Known<Vec<String>>,
    budgets: Known<Vec<BudgetRequest>>, proposed_role: Known<String>, authorization_references: Known<Vec<String>> });
impl Default for PacketHandoff {
    fn default() -> Self {
        Self {
            requested_capabilities: Known::missing(
                "handoff.requested_capabilities was not supplied",
            ),
            checkout_boundaries: Known::missing("handoff.checkout_boundaries was not supplied"),
            tool_and_egress_requests: Known::missing(
                "handoff.tool_and_egress_requests was not supplied",
            ),
            budgets: Known::missing("handoff.budgets was not supplied"),
            proposed_role: Known::missing("handoff.proposed_role was not supplied"),
            authorization_references: Known::missing(
                "handoff.authorization_references was not supplied",
            ),
        }
    }
}
protocol!(PacketReturn { implementation_revision: Known<String>, pull_request: Known<String>, execution_receipts: Known<Vec<String>>,
    source_configuration_changes: Known<Vec<String>>, reingest_comparison: Known<String>, residual_gaps: Known<Vec<String>> });
impl Default for PacketReturn {
    fn default() -> Self {
        Self {
            implementation_revision: Known::missing(
                "return.implementation_revision was not supplied",
            ),
            pull_request: Known::missing("return.pull_request was not supplied"),
            execution_receipts: Known::missing("return.execution_receipts was not supplied"),
            source_configuration_changes: Known::missing(
                "return.source_configuration_changes was not supplied",
            ),
            reingest_comparison: Known::missing("return.reingest_comparison was not supplied"),
            residual_gaps: Known::missing("return.residual_gaps was not supplied"),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PacketFact {
    Node(Node),
    Edge(Edge),
}
impl PacketFact {
    pub fn reference(&self) -> FactKey {
        match self {
            Self::Node(n) => FactKey::Node { id: n.id.clone() },
            Self::Edge(e) => FactKey::Edge {
                source: e.src.clone(),
                label: e.label.clone(),
                destination: e.dst.clone(),
            },
        }
    }
    pub fn digest(&self) -> PacketResult<String> {
        match self {
            Self::Node(n) => node_digest(n),
            Self::Edge(e) => edge_digest(e),
        }
        .map_err(|_| PacketError::InvalidInput("fact digest"))
    }
    pub fn props(&self) -> &Value {
        match self {
            Self::Node(n) => &n.props,
            Self::Edge(e) => &e.props,
        }
    }
}
protocol!(FactRelationship {
    reference: FactKey,
    stale: bool
});
protocol!(ReviewAnnotation { id: String, record_hash: String, record: AssertionDecisionRecord, relationship: Known<FactRelationship> });
protocol!(FactEvidence {
    id: String, reference: FactKey, expected_digest: String, fact: PacketFact, provenance: Option<Provenance>, provenance_problem: Option<String>,
    source_association: Known<SourceBinding>, review_annotations: Vec<ReviewAnnotation>
});
protocol!(ArtifactEvidence { id: String, selection: ArtifactSelection, artifact: Known<SpecArtifact> });
protocol!(SourceExcerpt {
    id: String,
    fact: FactKey,
    receipt_id: String,
    range_role: String,
    range_index: usize,
    span: CaptureSpanRef,
    text: String
});
protocol!(InterpretationEvidence { id: String, text: String, origin: Origin, supporting_evidence_ids: Vec<String>, review: Known<String>, freshness: Known<String>, conflict: Known<String>, historical_decision: Option<ReviewAnnotation> });
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLimitation {
    pub id: String,
    pub pointers: Vec<String>,
    #[serde(deserialize_with = "StrictOmission::deserialize")]
    pub omission: Omission,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PacketEvidence {
    pub facts: Vec<FactEvidence>,
    pub artifacts: Vec<ArtifactEvidence>,
    pub source_excerpts: Vec<SourceExcerpt>,
    pub interpretations: Vec<InterpretationEvidence>,
    pub limitations: Vec<EvidenceLimitation>,
}

/// A writer which rejects each chunk before extending its allocation.
pub(crate) struct BoundedBytes {
    pub bytes: Vec<u8>,
    maximum: usize,
}
impl BoundedBytes {
    pub fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("packet byte limit"));
        }
        let required = self.bytes.len() + bytes.len();
        if required > self.bytes.capacity() {
            let capacity = required.max(
                self.bytes
                    .capacity()
                    .saturating_mul(2)
                    .max(64)
                    .min(self.maximum),
            );
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn bounded_json<T: Serialize>(value: &T, maximum: usize) -> PacketResult<Vec<u8>> {
    let mut out = BoundedBytes::new(maximum);
    serde_json::to_writer(&mut out, value)
        .map_err(|_| PacketError::LimitExceeded("serialized bytes"))?;
    Ok(out.bytes)
}
fn canonical<T: Serialize>(value: &T, maximum: usize) -> PacketResult<Vec<u8>> {
    let bytes = bounded_json(value, maximum)?;
    preflight_json_scoped(
        std::str::from_utf8(&bytes).map_err(|_| PacketError::InvalidInput("UTF-8"))?,
        JsonInputLimits {
            bytes: maximum,
            ..JsonInputLimits::PACKET_BODY
        },
        |path| {
            !path.is_within(&[
                JsonPathPattern::Key("evidence"),
                JsonPathPattern::Key("facts"),
                JsonPathPattern::AnyIndex,
                JsonPathPattern::Key("fact"),
                JsonPathPattern::Key("value"),
                JsonPathPattern::Key("props"),
            ])
        },
    )
    .map_err(|_| PacketError::LimitExceeded("JSON structure"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| PacketError::InvalidInput("JSON"))?;
    canonical_json(
        &value,
        CanonicalLimits {
            bytes: maximum,
            depth: 40,
            values: 65_536,
            object_keys: 65_536,
        },
    )
    .map_err(|_| PacketError::LimitExceeded("canonical body"))
}
/// Stable local identity derived from a typed/content key, independent of arrival order.
pub fn evidence_id<T: Serialize>(kind: &str, key: &T) -> PacketResult<String> {
    text(kind)?;
    let bytes = canonical(key, MAX_BODY_BYTES)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"cartograph:work-packet:evidence:v1\0");
    hash.update(kind.as_bytes());
    hash.update(b"\0");
    hash.update(&bytes);
    Ok(format!("{kind}:{}", hash.finalize().to_hex()))
}
pub fn decision_record_hash(record: &AssertionDecisionRecord) -> PacketResult<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"cartograph:assertion-decision-record:v1\0");
    hash.update(&canonical(record, 16 * 1024)?);
    Ok(hash.finalize().to_hex().to_string())
}
pub fn packet_id(canonical_body: &[u8]) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"cartograph:work-packet:v1\0");
    hash.update(canonical_body);
    format!("work-packet-v1:{}", hash.finalize().to_hex())
}
pub fn valid_packet_id(id: &str) -> bool {
    id.strip_prefix("work-packet-v1:").is_some_and(hex64)
}
fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn text(value: &str) -> PacketResult<()> {
    if value.trim().is_empty() {
        Err(PacketError::InvalidInput("empty required text"))
    } else if value.len() > 4096 {
        Err(PacketError::LimitExceeded("narrative text"))
    } else {
        Ok(())
    }
}
fn count(len: usize, max: usize, name: &'static str) -> PacketResult<()> {
    if len > max {
        Err(PacketError::LimitExceeded(name))
    } else {
        Ok(())
    }
}
fn unique<T: PartialEq, K: Ord>(values: &mut Vec<T>, key: impl Fn(&T) -> K) -> PacketResult<()> {
    values.sort_by_cached_key(|v| key(v));
    for pair in values.windows(2) {
        if key(&pair[0]) == key(&pair[1]) && pair[0] != pair[1] {
            return Err(PacketError::SelectionConflict("duplicate key"));
        }
    }
    values.dedup();
    Ok(())
}
fn json_key<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("typed packet serializes")
}

impl PacketBody {
    pub fn draft(snapshot_id: String) -> Self {
        Self {
            schema_version: 1,
            identity: PacketIdentity {
                generator_version: env!("CARGO_PKG_VERSION").into(),
                template_version: "work-packet@1".into(),
                application: Known::missing("target application identity was not supplied"),
                recovered_basis: RecoveredBasis {
                    view: "recovered_graph".into(),
                    snapshot_id,
                },
                sources: vec![],
                configuration: vec![],
                input_closure: Known::missing(
                    "producer input/configuration closure is not established",
                ),
                curated_context: Known::missing(
                    "H3: curated context is unavailable in this product slice",
                ),
            },
            intent: PacketIntent::default(),
            evidence: PacketEvidence::default(),
            work: PacketWork::default(),
            acceptance: PacketAcceptance::default(),
            handoff: PacketHandoff::default(),
            returns: PacketReturn::default(),
        }
    }
    /// Normalize only collections declared unordered by SPEC-11, then validate.
    pub fn normalize(&mut self) -> PacketResult<()> {
        self.validate_counts()?;
        bounded_json(self, MAX_BODY_BYTES)?;
        unique(&mut self.evidence.facts, |v| v.reference.clone())?;
        unique(&mut self.evidence.artifacts, |v| {
            json_key(&(&v.selection.id, v.selection.mode))
        })?;
        unique(&mut self.identity.sources, |v| v.registration_id.clone())?;
        unique(&mut self.identity.configuration, |v| v.name.clone())?;
        unique(&mut self.evidence.source_excerpts, |v| {
            json_key(&(
                &v.fact,
                &v.receipt_id,
                &v.range_role,
                v.range_index,
                &v.span,
            ))
        })?;
        for fact in &mut self.evidence.facts {
            unique(&mut fact.review_annotations, |v| {
                json_key(&(&v.record_hash, &v.record.assertion.provenance.content_hash))
            })?;
        }
        if let Known::Supplied { value, .. } = &mut self.work.dependencies {
            unique(value, json_key)?;
        }
        self.validate()
    }
    fn validate_counts(&self) -> PacketResult<()> {
        count(self.evidence.facts.len(), 64, "facts")?;
        count(self.evidence.artifacts.len(), 4, "artifacts")?;
        count(self.identity.sources.len(), 16, "sources")?;
        count(self.identity.configuration.len(), 16, "configuration")?;
        count(self.evidence.source_excerpts.len(), 12, "excerpts")?;
        count(self.evidence.interpretations.len(), 64, "interpretations")?;
        count(self.evidence.limitations.len(), 64, "limitations")?;
        for fact in &self.evidence.facts {
            count(fact.review_annotations.len(), 64, "fact annotations")?;
        }
        let mut records = BTreeSet::new();
        for annotation in self
            .evidence
            .facts
            .iter()
            .flat_map(|f| &f.review_annotations)
            .chain(
                self.evidence
                    .interpretations
                    .iter()
                    .filter_map(|i| i.historical_decision.as_ref()),
            )
        {
            records.insert(annotation.record_hash.as_str());
            count(records.len(), 64, "decisions")?;
        }
        Ok(())
    }
    pub fn validate(&self) -> PacketResult<()> {
        self.validate_counts()?;
        // Admit the complete representation before provenance mirrors, digest
        // helpers or semantic-validation JSON can allocate owned copies.
        let bytes = canonical(self, MAX_BODY_BYTES)?;
        if self.schema_version != 1
            || self.identity.recovered_basis.view != "recovered_graph"
            || !self
                .identity
                .recovered_basis
                .snapshot_id
                .strip_prefix("context-v1:")
                .is_some_and(hex64)
        {
            return Err(PacketError::InvalidInput("schema or recovered basis"));
        }
        text(&self.identity.generator_version)?;
        text(&self.identity.template_version)?;
        if self.identity.curated_context.value().is_some() {
            return Err(PacketError::InvalidInput("curated context is unavailable"));
        }
        for source in &self.identity.sources {
            text(&source.registration_id)?;
            text(&source.repo_key)?;
        }
        for configuration in &self.identity.configuration {
            text(&configuration.name)?;
        }
        for label in [
            &self.intent.requested_actor,
            &self.intent.requested_journey,
            &self.intent.requested_by,
        ]
        .into_iter()
        .flatten()
        {
            text(label)?;
        }
        if self.evidence.facts.is_empty() && self.evidence.artifacts.is_empty() {
            return Err(PacketError::InvalidInput("empty evidence selection"));
        }
        let mut ids = BTreeSet::new();
        for id in self
            .evidence
            .facts
            .iter()
            .map(|v| &v.id)
            .chain(self.evidence.artifacts.iter().map(|v| &v.id))
            .chain(self.evidence.source_excerpts.iter().map(|v| &v.id))
            .chain(self.evidence.interpretations.iter().map(|v| &v.id))
            .chain(self.evidence.limitations.iter().map(|v| &v.id))
        {
            text(id)?;
            if !ids.insert(id.as_str()) {
                return Err(PacketError::SelectionConflict("local evidence id"));
            }
        }
        for annotation in self
            .evidence
            .facts
            .iter()
            .flat_map(|f| &f.review_annotations)
            .chain(
                self.evidence
                    .interpretations
                    .iter()
                    .filter_map(|i| i.historical_decision.as_ref()),
            )
        {
            text(&annotation.id)?;
            ids.insert(annotation.id.as_str());
        }
        for fact in &self.evidence.facts {
            if fact.reference != fact.fact.reference()
                || fact.expected_digest != fact.fact.digest()?
                || fact.id != evidence_id("fact", &(&fact.reference, &fact.expected_digest))?
            {
                return Err(PacketError::InvalidInput("fact identity"));
            }
            let original = fact
                .fact
                .props()
                .get("prov")
                .cloned()
                .and_then(|v| serde_json::from_value::<Provenance>(v).ok())
                .filter(|p| p.validate().is_ok());
            if original != fact.provenance
                || (fact.provenance.is_none()
                    && fact
                        .provenance_problem
                        .as_ref()
                        .is_none_or(|p| p.trim().is_empty()))
                || (fact.provenance.is_some() && fact.provenance_problem.is_some())
            {
                return Err(PacketError::InvalidInput("provenance mirror"));
            }
            if let Some(binding) = fact.source_association.value()
                && (binding.fact != fact.reference
                    || binding.emitted_fact_digest != fact.expected_digest)
            {
                return Err(PacketError::InvalidInput("source association"));
            }
            for annotation in &fact.review_annotations {
                validate_annotation(annotation)?;
                if let Some(r) = annotation.relationship.value() {
                    if r.reference != fact.reference {
                        return Err(PacketError::InvalidInput("annotation association"));
                    }
                    if r.stale
                        != fact.provenance.as_ref().is_none_or(|current| {
                            current.content_hash
                                != annotation.record.assertion.provenance.content_hash
                        })
                    {
                        return Err(PacketError::InvalidInput("historical decision freshness"));
                    }
                }
            }
        }
        let mut artifact_bytes = 0usize;
        for item in &self.evidence.artifacts {
            match &item.artifact {
                Known::Supplied { origin, .. } => validate_origin(origin, &ids)?,
                Known::Missing { omission } => text(&omission.detail)?,
            }
            if item.selection.id.is_empty()
                || item.selection.id.len() > 128
                || item.selection.schema != SpecArtifact::expected_schema(&item.selection.id)
                || !item
                    .selection
                    .content_id
                    .strip_prefix("spec-artifact-v1:")
                    .is_some_and(hex64)
            {
                return Err(PacketError::InvalidInput("artifact selection"));
            }
            if item.id != evidence_id("artifact", &item.selection)? {
                return Err(PacketError::InvalidInput("artifact evidence id"));
            }
            if let Some(artifact) = item.artifact.value() {
                if !artifact.verify_identity()
                    || artifact.id != item.selection.id
                    || artifact.schema != item.selection.schema
                    || artifact.content_id != item.selection.content_id
                {
                    return Err(PacketError::SelectionConflict("artifact identity"));
                }
                artifact_bytes = artifact_bytes
                    .checked_add(artifact.content.len())
                    .ok_or(PacketError::LimitExceeded("artifact bytes"))?;
                let remaining = MAX_ARTIFACT_BYTES
                    .checked_sub(artifact_bytes)
                    .ok_or(PacketError::LimitExceeded("artifact bytes"))?;
                artifact_bytes += bounded_json(&artifact.assertions, remaining)?.len();
            } else if let Known::Missing { omission } = &item.artifact
                && (omission.class != OmissionClass::MissingInput
                    || omission.seam != FillSeam::Interpretation)
            {
                return Err(PacketError::InvalidInput("omitted artifact classification"));
            }
        }
        let mut excerpt_bytes = 0usize;
        for excerpt in &self.evidence.source_excerpts {
            count(excerpt.text.len(), 8192, "excerpt bytes")?;
            excerpt_bytes += excerpt.text.len();
            if excerpt.text.is_empty()
                || excerpt.span.byte_end <= excerpt.span.byte_start
                || excerpt.span.byte_end > excerpt.span.file.byte_len
                || excerpt.span.byte_end - excerpt.span.byte_start != excerpt.text.len() as u64
                || !self.evidence.facts.iter().any(|f| {
                    f.reference == excerpt.fact
                        && f.source_association
                            .value()
                            .is_some_and(|b| b.receipt_id == excerpt.receipt_id)
                })
            {
                return Err(PacketError::InvalidInput("excerpt binding"));
            }
            if excerpt.id
                != evidence_id(
                    "excerpt",
                    &(
                        &excerpt.fact,
                        &excerpt.receipt_id,
                        &excerpt.range_role,
                        excerpt.range_index,
                        &excerpt.span,
                        &excerpt.text,
                    ),
                )?
            {
                return Err(PacketError::InvalidInput("excerpt id"));
            }
        }
        count(excerpt_bytes, 48 * 1024, "total excerpt bytes")?;
        for interpretation in &self.evidence.interpretations {
            text(&interpretation.text)?;
            if let Some(record) = &interpretation.historical_decision {
                validate_annotation(record)?;
            }
        }
        for limitation in &self.evidence.limitations {
            text(&limitation.omission.detail)?;
            if limitation.pointers.is_empty() {
                return Err(PacketError::InvalidInput("limitation without pointer"));
            }
            for pointer in &limitation.pointers {
                text(pointer)?;
            }
        }
        for observations in [
            &self.acceptance.discovered_tests,
            &self.acceptance.structural_test_associations,
            &self.acceptance.execution_evidence,
            &self.acceptance.coverage_evidence,
            &self.acceptance.assertion_support,
        ] {
            count(observations.len(), 64, "acceptance observations")?;
            for item in observations {
                text(&item.id)?;
                text(&item.description)?;
                if item.evidence_ids.is_empty() {
                    return Err(PacketError::InvalidInput(
                        "observation without exact evidence",
                    ));
                }
            }
        }
        // First serialize through a capped writer. The temporary JSON tree below
        // therefore cannot grow from an unadmitted full body.
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| PacketError::InvalidInput("body JSON"))?;
        validate_knowledge(&value, &ids, false)?;
        Ok(())
    }
    pub fn canonical_bytes(&self) -> PacketResult<Vec<u8>> {
        self.validate()?;
        canonical(self, MAX_BODY_BYTES)
    }
    pub fn content_id(&self) -> PacketResult<String> {
        Ok(packet_id(&self.canonical_bytes()?))
    }
    pub fn from_json(bytes: &[u8]) -> PacketResult<Self> {
        preflight_json_scoped(
            std::str::from_utf8(bytes).map_err(|_| PacketError::InvalidInput("UTF-8"))?,
            JsonInputLimits::PACKET_BODY,
            |path| {
                !path.is_within(&[
                    JsonPathPattern::Key("evidence"),
                    JsonPathPattern::Key("facts"),
                    JsonPathPattern::AnyIndex,
                    JsonPathPattern::Key("fact"),
                    JsonPathPattern::Key("value"),
                    JsonPathPattern::Key("props"),
                ])
            },
        )
        .map_err(|error| match error {
            crate::JsonInputError::InvalidInput => PacketError::InvalidInput("body JSON admission"),
            crate::JsonInputError::LimitExceeded { .. } => {
                PacketError::LimitExceeded("body JSON admission")
            }
        })?;
        let body: Self =
            serde_json::from_slice(bytes).map_err(|_| PacketError::InvalidInput("body schema"))?;
        body.validate()?;
        // Upstream source records have legacy permissive serde implementations;
        // reject dropped protocol fields instead of accepting a lossy parse.
        let original: Value =
            serde_json::from_slice(bytes).map_err(|_| PacketError::InvalidInput("body JSON"))?;
        if serde_json::to_value(&body).map_err(|_| PacketError::InvalidInput("body JSON"))?
            != original
        {
            return Err(PacketError::InvalidInput(
                "unknown or noncanonical protocol fields",
            ));
        }
        let mut normalized = body.clone();
        normalized.normalize()?;
        if normalized != body {
            return Err(PacketError::InvalidInput("unnormalized saved body"));
        }
        Ok(body)
    }
}
fn validate_annotation(annotation: &ReviewAnnotation) -> PacketResult<()> {
    let p = &annotation.record.assertion.provenance;
    if p.validate().is_err()
        || p.evidence.is_empty()
        || !matches!(p.tier, core_prov::Tier::Semantic | core_prov::Tier::Agentic)
        || !matches!(
            p.confidence_tier,
            core_prov::ConfidenceTier::InferredStrong | core_prov::ConfidenceTier::InferredWeak
        )
        || annotation.record_hash != decision_record_hash(&annotation.record)?
        || annotation.id != evidence_id("decision", &(&p.content_hash, &annotation.record_hash))?
    {
        return Err(PacketError::InvalidInput("historical decision"));
    }
    text(&annotation.record.assertion.subject_id)?;
    text(&annotation.record.assertion.summary)?;
    if annotation.record.decision == agents::AssertionDecision::Annotated
        && annotation
            .record
            .note
            .as_ref()
            .is_none_or(|n| n.trim().is_empty())
    {
        return Err(PacketError::InvalidInput("annotated decision note"));
    }
    bounded_json(&annotation.record, 16 * 1024)?;
    Ok(())
}
fn validate_knowledge(
    value: &Value,
    evidence_ids: &BTreeSet<&str>,
    narrative: bool,
) -> PacketResult<()> {
    match value {
        Value::Object(object) => {
            if let Some(state) = object.get("state").and_then(Value::as_str) {
                if state == "missing" {
                    text(
                        object
                            .get("omission")
                            .and_then(|o| o.get("detail"))
                            .and_then(Value::as_str)
                            .ok_or(PacketError::InvalidInput("missing omission"))?,
                    )?;
                    return Ok(());
                }
                if state == "supplied" {
                    let supplied = &object["value"];
                    if supplied.is_null()
                        || supplied.as_str().is_some_and(|s| s.trim().is_empty())
                        || supplied.as_array().is_some_and(Vec::is_empty)
                    {
                        return Err(PacketError::InvalidInput("empty supplied knowledge"));
                    }
                    validate_knowledge(supplied, evidence_ids, true)?;
                    validate_knowledge(&object["origin"], evidence_ids, false)?;
                    return Ok(());
                }
            }
            for (key, child) in object {
                if key == "props"
                    || key == "artifact"
                    || key == "record"
                    || key == "provenance"
                    || key == "fact"
                    || key == "reference"
                {
                    continue;
                }
                if (key == "evidence_ids" || key == "supporting_evidence_ids")
                    && let Some(ids) = child.as_array()
                {
                    for id in ids {
                        if !id.as_str().is_some_and(|s| evidence_ids.contains(s)) {
                            return Err(PacketError::InvalidInput("dangling evidence reference"));
                        }
                    }
                }
                if key == "kind"
                    && child.as_str() == Some("evidence")
                    && object
                        .get("evidence_ids")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty)
                {
                    return Err(PacketError::InvalidInput(
                        "evidence origin without references",
                    ));
                }
                validate_knowledge(child, evidence_ids, narrative)?;
            }
        }
        Value::Array(items) => {
            if narrative {
                count(items.len(), 64, "knowledge collection")?;
            }
            for item in items {
                validate_knowledge(item, evidence_ids, narrative)?;
            }
        }
        Value::String(s) if narrative => text(s)?,
        _ => {}
    }
    Ok(())
}

fn validate_origin(origin: &Origin, ids: &BTreeSet<&str>) -> PacketResult<()> {
    if let Origin::Evidence { evidence_ids } = origin {
        if evidence_ids.is_empty() {
            return Err(PacketError::InvalidInput(
                "evidence origin without references",
            ));
        }
        count(evidence_ids.len(), 64, "origin evidence references")?;
        if evidence_ids.iter().any(|id| !ids.contains(id.as_str())) {
            return Err(PacketError::InvalidInput("dangling evidence reference"));
        }
    }
    Ok(())
}
