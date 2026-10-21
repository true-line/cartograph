//! Frozen @10 denominator records. These attest structure, never semantic acceptance.
use agents::investigation::{
    INVESTIGATION_V2_POLICY, SpecialistId,
    obligations::{Obligation, StagedObligationRequest},
};
use serde::{Deserialize, Serialize};

pub(super) type RecordResult<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FamilyIdentity {
    pub specialist: SpecialistId,
    pub prompt_fingerprint: String,
    pub request_schema: u32,
    pub prompt_schema: u32,
    pub disclosure_schema: u32,
    pub result_schema: u32,
    pub task_schema: u32,
    pub summary_schema: u32,
    pub intent_schema: u32,
    pub policy: String,
}
impl FamilyIdentity {
    pub(super) fn from_request(request: &StagedObligationRequest) -> RecordResult<Self> {
        request
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .map_err(|e| e.to_string())?;
        let specialist = match request.specialist_id.as_str() {
            "domain-analyst@10" => SpecialistId::DomainAnalystV10,
            "evidence-auditor@10" => SpecialistId::EvidenceAuditorV10,
            _ => return Err("Admission requires the closed @10 family".into()),
        };
        if request.schema_version != 7 {
            return Err("Admission requires request7".into());
        }
        let definition = specialist.definition().map_err(|e| e.to_string())?;
        Ok(Self {
            specialist,
            prompt_fingerprint: definition.prompt_fingerprint,
            request_schema: request.schema_version,
            prompt_schema: 9,
            disclosure_schema: 6,
            result_schema: 7,
            task_schema: 3,
            summary_schema: 7,
            intent_schema: 4,
            policy: INVESTIGATION_V2_POLICY.profile().into(),
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Procedure {
    H4QueryReadFinish,
    H4IndependentSemanticReview,
    H4AuditorActualParent,
    H4NegativeCases,
    H4NativeProcessRestart,
    H4RetainedCitationReopen,
    H4DisposableForgetting,
    H4SharedCoordinator,
    H5CapabilitiesEnvironmentEgress,
    H5CancellationDisconnectUnknown,
    H5DelegationAncestrySharedLimits,
    H5ExternalMcpAcp,
}
const PROCEDURES: [Procedure; 12] = [
    Procedure::H4QueryReadFinish,
    Procedure::H4IndependentSemanticReview,
    Procedure::H4AuditorActualParent,
    Procedure::H4NegativeCases,
    Procedure::H4NativeProcessRestart,
    Procedure::H4RetainedCitationReopen,
    Procedure::H4DisposableForgetting,
    // MT-H4-01 and MT-H5-01..04 remain separate, unexecuted cross-ingress gates.
    Procedure::H4SharedCoordinator,
    Procedure::H5CapabilitiesEnvironmentEgress,
    Procedure::H5CancellationDisconnectUnknown,
    Procedure::H5DelegationAncestrySharedLimits,
    Procedure::H5ExternalMcpAcp,
];
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum LedgerEntry {
    Request { obligation: Obligation },
    Procedure { procedure: Procedure },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrozenLedger {
    pub family: FamilyIdentity,
    pub request_hash: String,
    pub entries: Vec<LedgerEntry>,
    pub count: u32,
    pub digest: String,
}
fn digest<T: Serialize + ?Sized>(value: &T) -> RecordResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    Ok(core_prov::content_hash(&bytes))
}
impl FrozenLedger {
    pub(super) fn freeze(request: &StagedObligationRequest) -> RecordResult<Self> {
        let family = FamilyIdentity::from_request(request)?;
        let normalized = request
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .map_err(|e| e.to_string())?;
        let request_hash = normalized
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .map_err(|e| e.to_string())?;
        let mut entries = normalized
            .obligations
            .obligations
            .into_iter()
            .map(|obligation| LedgerEntry::Request { obligation })
            .collect::<Vec<_>>();
        entries.extend(
            PROCEDURES
                .into_iter()
                .map(|procedure| LedgerEntry::Procedure { procedure }),
        );
        let count = u32::try_from(entries.len()).map_err(|_| "Ledger count overflow")?;
        let digest = digest(&(
            "cartograph:admission-ledger:v1",
            &family,
            &request_hash,
            &entries,
        ))?;
        Ok(Self {
            family,
            request_hash,
            entries,
            count,
            digest,
        })
    }
    pub(super) fn validate_request(&self, request: &StagedObligationRequest) -> RecordResult<()> {
        if self != &Self::freeze(request)? {
            return Err("Frozen obligation ledger differs".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DischargeEvidence {
    /// Must be re-derived from durable worker records by the receipt producer.
    WorkerSavedRecord {
        record_hash: String,
        obligation_id: String,
    },
    /// An independently retained operator procedure artifact, never worker coverage.
    OperatorProcedure {
        artifact_hash: String,
        procedure: Procedure,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Observation {
    Discharged { evidence: DischargeEvidence },
    Insufficient,
    Refused,
    Cancelled,
    Unexecuted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    Preparation,
    Analyst,
    IndependentReview,
    Auditor,
    NativeOperator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StageReceipt {
    pub schema: String,
    pub schema_version: u32,
    pub stage: Stage,
    pub ledger: FrozenLedger,
    pub observations: Vec<Observation>,
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl StageReceipt {
    pub(super) fn unexecuted(ledger: &FrozenLedger, stage: Stage) -> Self {
        Self {
            schema: "cartograph-admission-stage@1".into(),
            schema_version: 1,
            stage,
            ledger: ledger.clone(),
            observations: vec![Observation::Unexecuted; ledger.entries.len()],
        }
    }
    /// Expected ledger must come from validated frozen preparation, not this receipt.
    /// Matching evidence hashes are necessary, not proof of authentic or sufficient evidence.
    pub(super) fn validate_against(&self, expected: &FrozenLedger) -> RecordResult<()> {
        if self.schema != "cartograph-admission-stage@1"
            || self.schema_version != 1
            || &self.ledger != expected
            || self.observations.len() != expected.entries.len()
            || usize::try_from(expected.count).ok() != Some(expected.entries.len())
        {
            return Err("Receipt changed frozen denominator or identity".into());
        }
        for (entry, observation) in expected.entries.iter().zip(&self.observations) {
            if let Observation::Discharged { evidence } = observation {
                let valid = match (entry, evidence) {
                    (
                        LedgerEntry::Request { obligation },
                        DischargeEvidence::WorkerSavedRecord {
                            record_hash,
                            obligation_id,
                        },
                    ) => hash(record_hash) && obligation.key().1 == obligation_id,
                    (
                        LedgerEntry::Procedure { procedure },
                        DischargeEvidence::OperatorProcedure {
                            artifact_hash,
                            procedure: observed,
                        },
                    ) => hash(artifact_hash) && procedure == observed,
                    _ => false,
                };
                if !valid {
                    return Err(
                        "Discharge evidence does not match obligation kind and identity".into(),
                    );
                }
            }
        }
        Ok(())
    }
    pub(super) fn discharged_count(&self) -> usize {
        self.observations
            .iter()
            .filter(|o| matches!(o, Observation::Discharged { .. }))
            .count()
    }
}
#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
