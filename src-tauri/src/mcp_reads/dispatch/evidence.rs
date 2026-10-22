//! Concrete receipt-bound evidence route; no callbacks or generic commands.
use super::{Dispatcher, ReadError, inner_cap, object_schema, parse, session_properties, success};
use crate::mcp_reads::evidence::EvidenceLimits;
use agents::{TaskFactKey, investigation::InvestigationInspectionTarget};
use serde::{Deserialize, Deserializer, de::Error};
use serde_json::{Map, Value, json};
use std::num::{NonZeroU64, NonZeroUsize};

#[derive(Clone, Copy)]
pub(crate) struct EvidenceWorkLimits {
    pub max_file_validation_bytes: NonZeroU64,
    pub max_span_bytes: NonZeroU64,
}
pub(super) struct Backend<'state> {
    pub state: &'state crate::AppState,
    pub limits: EvidenceWorkLimits,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceKind {
    RetainedPrimarySource,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceArgs {
    Describe {
        session_id: String,
        basis_id: String,
        #[serde(rename = "kind")]
        _kind: EvidenceKind,
        fact: TaskFactKey,
        ordinal: usize,
        max_bytes: NonZeroUsize,
    },
    Read {
        session_id: String,
        basis_id: String,
        #[serde(rename = "kind")]
        _kind: EvidenceKind,
        target: Box<StrictTarget>,
        max_bytes: NonZeroUsize,
    },
}

struct StrictTarget(InvestigationInspectionTarget);
impl<'de> Deserialize<'de> for StrictTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        // All other target structures already reject unknown fields. EvidenceRef
        // is deliberately a wider public type, so close this ingress explicitly.
        let source = value
            .get("source")
            .and_then(Value::as_object)
            .ok_or_else(|| D::Error::custom("invalid evidence target"))?;
        let keys = ["repo", "path", "byte_start", "byte_end", "commit_sha"];
        if source.len() != keys.len() || !keys.iter().all(|key| source.contains_key(*key)) {
            return Err(D::Error::custom("invalid evidence target"));
        }
        serde_json::from_value(value)
            .map(Self)
            .map_err(|_| D::Error::custom("invalid evidence target"))
    }
}

impl Dispatcher<'_> {
    pub(super) fn call_evidence(
        &self,
        id: &Value,
        arguments: Map<String, Value>,
    ) -> Result<Value, ReadError> {
        let backend = self.evidence.as_ref().ok_or(ReadError::InvalidRequest)?;
        match parse::<EvidenceArgs>(arguments)? {
            EvidenceArgs::Describe {
                session_id,
                basis_id,
                _kind: _,
                fact,
                ordinal,
                max_bytes,
            } => {
                self.check_session(&session_id, &basis_id)?;
                let total_cap = max_bytes.get().min(self.limits.max_response_bytes.get());
                let template = success(
                    id,
                    "Retained primary-source description.",
                    json!({"session_id":self.session_id,"kind":"retained_primary_source","description":Value::Null}),
                );
                let description = self.session.describe_evidence(
                    backend.state,
                    &basis_id,
                    &fact,
                    ordinal,
                    inner_cap(&template, total_cap)?,
                )?;
                Ok(success(
                    id,
                    "Retained primary-source description.",
                    json!({"session_id":self.session_id,"kind":"retained_primary_source","description":description}),
                ))
            }
            EvidenceArgs::Read {
                session_id,
                basis_id,
                _kind: _,
                target,
                max_bytes,
            } => {
                self.check_session(&session_id, &basis_id)?;
                let total_cap = max_bytes.get().min(self.limits.max_response_bytes.get());
                let template = success(
                    id,
                    "Retained primary-source span.",
                    json!({"session_id":self.session_id,"kind":"retained_primary_source","span":Value::Null}),
                );
                let span = self.session.read_evidence(
                    backend.state,
                    &basis_id,
                    &target.0,
                    EvidenceLimits {
                        max_file_validation_bytes: backend.limits.max_file_validation_bytes.get(),
                        max_span_bytes: backend.limits.max_span_bytes.get(),
                        max_response_bytes: inner_cap(&template, total_cap)?,
                    },
                )?;
                Ok(success(
                    id,
                    "Retained primary-source span.",
                    json!({"session_id":self.session_id,"kind":"retained_primary_source","span":span}),
                ))
            }
        }
    }
}

pub(super) fn descriptor() -> (&'static str, Value) {
    let fact = fact_schema();
    let mut describe = session_properties();
    describe["mode"] = json!({"const":"describe"});
    describe["kind"] = json!({"const":"retained_primary_source"});
    describe["fact"] = fact;
    describe["ordinal"] = json!({"type":"integer","minimum":0});
    describe["max_bytes"] =
        json!({"type":"integer","minimum":1,"description":"Maximum complete MCP response bytes."});
    let mut read = session_properties();
    read["mode"] = json!({"const":"read"});
    read["kind"] = json!({"const":"retained_primary_source"});
    read["target"] = target_schema();
    read["max_bytes"] = describe["max_bytes"].clone();
    (
        "Describe one receipt-bound retained range or read its exact captured text; never reads a current checkout.",
        json!({"type":"object","oneOf":[object_schema(describe,&["mode","kind","session_id","basis_id","fact","ordinal","max_bytes"]),object_schema(read,&["mode","kind","session_id","basis_id","target","max_bytes"])]}),
    )
}

fn fact_schema() -> Value {
    json!({"oneOf":[object_schema(json!({"kind":{"const":"node"},"id":{"type":"string"}}),&["kind","id"]),object_schema(json!({"kind":{"const":"edge"},"source":{"type":"string"},"label":{"type":"string"},"destination":{"type":"string"}}),&["kind","source","label","destination"])]})
}

fn target_schema() -> Value {
    let source = object_schema(
        json!({"repo":{"type":"string"},"path":{"type":"string"},"byte_start":{"type":"integer","minimum":0},"byte_end":{"type":"integer","minimum":0},"commit_sha":{"type":"string"}}),
        &["repo", "path", "byte_start", "byte_end", "commit_sha"],
    );
    let file = object_schema(
        json!({"source_id":{"type":"string"},"capture_id":{"type":"string"},"path":{"type":"string"},"digest":{"type":"string"},"byte_len":{"type":"integer","minimum":0}}),
        &["source_id", "capture_id", "path", "digest", "byte_len"],
    );
    let captured = object_schema(
        json!({"file":file,"byte_start":{"type":"integer","minimum":0},"byte_end":{"type":"integer","minimum":0}}),
        &["file", "byte_start", "byte_end"],
    );
    let roles = [
        agents::TaskRangeRole::Provenance,
        agents::TaskRangeRole::RuleExit,
        agents::TaskRangeRole::ConditionBranch,
        agents::TaskRangeRole::ConditionExpression,
        agents::TaskRangeRole::ReturnValue,
        agents::TaskRangeRole::ThrowValue,
        agents::TaskRangeRole::Dependency,
        agents::TaskRangeRole::DependencyDeclaration,
        agents::TaskRangeRole::Redaction,
        agents::TaskRangeRole::DefinitionDeclaration,
        agents::TaskRangeRole::DefinitionUse,
        agents::TaskRangeRole::DefinitionInitializer,
        agents::TaskRangeRole::DefinitionExpression,
        agents::TaskRangeRole::DefinitionDependency,
        agents::TaskRangeRole::DefinitionDependencyDeclaration,
    ];
    object_schema(
        json!({"schema_version":{"type":"integer","minimum":0},"target_id":{"type":"string"},"graph_snapshot_id":{"type":"string"},"fact":fact_schema(),"fact_digest":{"type":"string"},"registered_source_id":{"type":"string"},"receipt_id":{"type":"string"},"receipt_inventory_index":{"type":"integer","minimum":0,"maximum":u32::MAX},"role":{"enum":roles},"index":{"type":"integer","minimum":0,"maximum":u32::MAX},"source":source,"captured":captured}),
        &[
            "schema_version",
            "target_id",
            "graph_snapshot_id",
            "fact",
            "fact_digest",
            "registered_source_id",
            "receipt_id",
            "receipt_inventory_index",
            "role",
            "index",
            "source",
            "captured",
        ],
    )
}

#[cfg(test)]
mod tests;
