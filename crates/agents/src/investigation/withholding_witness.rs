//! Text-free host transformation binding. Shape validation is not authentication.
use super::*;
use serde::{Deserialize, Serialize};

pub const FINDINGS_WITHHELD_MESSAGE: &str =
    "Some findings were withheld because their claims could not be verified.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithholdingDisposition {
    FindingsWithheldAfterCorrection,
}

/// The host compares these hashes to independently authenticated invocation,
/// request, disclosure and prior-correction records before publication/reopen.
/// No original rejected prose or model-supplied explanation is persisted here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingWithholdingWitness {
    pub schema_version: u32,
    pub disposition: WithholdingDisposition,
    pub detector_version: u32,
    pub original_finish_hash: String,
    pub original_response_hash: String,
    pub request_hash: String,
    pub obligations_hash: String,
    pub input_ledger_hash: String,
    pub disclosure_hash: String,
    pub prior_correction_witness_hash: String,
    /// Hash of the admitted projected result before attaching this witness,
    /// avoiding a circular final-result hash. The final result binds the witness.
    pub projected_result_hash: String,
    pub original_finding_count: u32,
    pub withheld_indexes: Vec<u32>,
    pub survivor_indexes: Vec<u32>,
}
impl FindingWithholdingWitness {
    pub fn validate_shape(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1
            || self.detector_version != 1
            || self.original_finding_count == 0
            // Same existing finish-shape bound, not a raised policy allowance.
            || self.original_finding_count > 12
            || !self.disclosure_hash.strip_prefix("investigation-disclosure-v6:")
                .is_some_and(crate::source_basis::hash)
            || self.withheld_indexes.is_empty()
            || self.survivor_indexes.is_empty()
            || [
                &self.original_finish_hash, &self.original_response_hash,
                &self.request_hash, &self.obligations_hash, &self.input_ledger_hash,
                &self.prior_correction_witness_hash,
                &self.projected_result_hash,
            ].iter().any(|hash| !crate::source_basis::hash(hash))
        {
            return Err(InvestigationError::InvalidInput);
        }
        for indexes in [&self.withheld_indexes, &self.survivor_indexes] {
            if indexes.windows(2).any(|pair| pair[0] >= pair[1])
                || indexes
                    .iter()
                    .any(|index| *index >= self.original_finding_count)
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        for index in 0..self.original_finding_count {
            if self.withheld_indexes.binary_search(&index).is_ok()
                == self.survivor_indexes.binary_search(&index).is_ok()
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        bounded_bytes(self, MAX_RESULT_BYTES)?;
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.validate_shape()?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:finding-withholding-witness:v1\0",
            &bounded_bytes(self, MAX_RESULT_BYTES)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn witness() -> FindingWithholdingWitness {
        FindingWithholdingWitness {
            schema_version: 1,
            detector_version: 1,
            disposition: WithholdingDisposition::FindingsWithheldAfterCorrection,
            original_finish_hash: "1".repeat(64),
            original_response_hash: "2".repeat(64),
            request_hash: "3".repeat(64),
            obligations_hash: "4".repeat(64),
            input_ledger_hash: "5".repeat(64),
            disclosure_hash: format!("investigation-disclosure-v6:{}", "6".repeat(64)),
            prior_correction_witness_hash: "7".repeat(64),
            projected_result_hash: "8".repeat(64),
            original_finding_count: 3,
            withheld_indexes: vec![1],
            survivor_indexes: vec![0, 2],
        }
    }
    // AC-0287 / #1705: complete, disjoint, ordered current-finish projection.
    #[test]
    fn witness_rejects_missing_overlapping_unsorted_and_empty_partitions() {
        for (withheld, survivors) in [
            (vec![1], vec![0]),
            (vec![1], vec![0, 1, 2]),
            (vec![1], vec![2, 0]),
            (vec![0, 1, 2], vec![]),
            (vec![], vec![0, 1, 2]),
            (vec![1, 1], vec![0, 2]),
        ] {
            let mut bad = witness();
            bad.withheld_indexes = withheld;
            bad.survivor_indexes = survivors;
            assert!(bad.validate_shape().is_err());
        }
    }
    #[test]
    fn witness_hash_binds_each_authority_and_survives_roundtrip() {
        let original = witness();
        let expected = original.fingerprint().unwrap();
        let serialized = serde_json::to_vec(&original).unwrap();
        let reopened: FindingWithholdingWitness = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(reopened.fingerprint().unwrap(), expected);
        for index in 0..8 {
            let mut changed = original.clone();
            let hashes = [
                &mut changed.original_finish_hash,
                &mut changed.original_response_hash,
                &mut changed.request_hash,
                &mut changed.obligations_hash,
                &mut changed.input_ledger_hash,
                &mut changed.disclosure_hash,
                &mut changed.prior_correction_witness_hash,
                &mut changed.projected_result_hash,
            ];
            *hashes.into_iter().nth(index).unwrap() = if index == 5 {
                format!("investigation-disclosure-v6:{}", "9".repeat(64))
            } else {
                "9".repeat(64)
            };
            assert_ne!(changed.fingerprint().unwrap(), expected);
        }
        let mut value = serde_json::to_value(&original).unwrap();
        value["rejected_prose"] = serde_json::json!("not permitted");
        assert!(serde_json::from_value::<FindingWithholdingWitness>(value).is_err());
    }
}
