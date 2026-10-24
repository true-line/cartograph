//! Temporary logical owners of durable record decoding, not a heap/RSS bound.
use super::{MAX_RECORD_BYTES, RECORD_MAX_DEPTH, strict};
use context_hub::logical::{json_bytes, set, slots, sum};
use std::collections::BTreeSet;

/// Scratch for strict preflight and canonical round-trip comparison in
/// `decode_record`. Borrowed input, retained typed DTOs and custom serde
/// implementation internals are excluded and must be admitted by the caller.
/// Collection capacity slack, allocator/native internals and stacks are also
/// excluded. This component alone does not admit a task or enable transport.
pub fn record_decode_scratch_bytes() -> Option<usize> {
    scratch(MAX_RECORD_BYTES, RECORD_MAX_DEPTH)
}

pub(super) fn scratch(bytes: usize, depth: usize) -> Option<usize> {
    let objects = depth.checked_add(1)?;
    let keys = objects
        .checked_mul(strict::MAX_OBJECT_KEYS)?
        .checked_add(1)?;
    // Open objects retain admitted keys; the next key is allocated before its
    // length/count check. Its payload and parser escape scratch can each be as
    // large as the byte-admitted input. Set nodes use logical link currency.
    let preflight = sum(&[
        bytes,
        bytes,
        bytes,
        slots::<BTreeSet<String>>(objects)?,
        set::<String>(keys)?,
    ])?;
    // The typed canonical wire is bounded before either comparison Value is
    // allocated. Both Values coexist with that wire and the second decoder's
    // escape scratch. Typed fields/defaults require separate caller accounting.
    let comparison = sum(&[
        bytes,
        json_bytes(bytes)?,
        json_bytes(bytes)?,
        bytes,
        slots::<Vec<u8>>(2)?,
    ])?;
    Some(preflight.max(comparison))
}

impl super::InvestigationInputLedger {
    /// Closed ledger DTO ownership and decode peak. Borrowed record bytes,
    /// semantic validation and host archive reads are separate; not RSS.
    pub fn decode_workspace_bytes() -> Option<(usize, usize)> {
        use super::*;
        use context_hub::coverage::{CoverageBasis, RepoQualification};
        use std::mem::size_of;
        // Each element needs a distinct admitted JSON value. No nonempty
        // serde defaults synthesize payload; private proof caches are empty.
        let element = [
            size_of::<String>(),
            size_of::<crate::TaskFactSelection>(),
            size_of::<InvestigationReceiptReference>(),
            size_of::<InvestigationCitation>(),
            size_of::<InvestigationQueryManifest>(),
            size_of::<InvestigationCoverageReadManifest>(),
            size_of::<CoverageBasis>(),
            size_of::<RepoQualification>(),
        ]
        .into_iter()
        .max()?;
        let retained = sum(&[
            size_of::<Self>(),
            MAX_RECORD_BYTES,
            element.checked_mul(RECORD_MAX_VALUES)?,
        ])?;
        // Internally tagged fact/origin/scope enums can buffer content during
        // child decoding, before semantic admission. Use the existing logical
        // JSON currency at every admitted nesting level, not serde layout.
        let content =
            json_bytes(MAX_RECORD_BYTES)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?;
        Some((
            retained,
            retained.checked_add(record_decode_scratch_bytes()?.max(content))?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::decode_record;
    use super::*;
    use serde_json::{Value, json};

    // AC-0327: exercise the real decoder's key admission and escaped round trip.
    #[test]
    fn record_workspace_preserves_strict_decoder_boundaries() {
        let mut fields = serde_json::Map::new();
        for i in 0..strict::MAX_OBJECT_KEYS {
            fields.insert(format!("key{i}"), json!(["雪\n\"\\", {"nested": true}]));
        }
        let raw = serde_json::to_string(&fields).unwrap();
        let decoded: Value = decode_record(&raw).unwrap();
        let canonical = serde_json::to_vec(&decoded).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&canonical).unwrap(),
            decoded
        );
        let comparison = sum(&[
            canonical.len(),
            json_bytes(raw.len()).unwrap(),
            json_bytes(canonical.len()).unwrap(),
            canonical.len(),
            slots::<Vec<u8>>(2).unwrap(),
        ])
        .unwrap();
        assert!(record_decode_scratch_bytes().unwrap() >= comparison);
        fields.insert("one_too_many".into(), Value::Null);
        assert!(decode_record::<Value>(&serde_json::to_string(&fields).unwrap()).is_err());
        for len in [strict::MAX_KEY_BYTES, strict::MAX_KEY_BYTES + 1] {
            let body = json!({"x".repeat(len): null}).to_string();
            assert_eq!(
                decode_record::<Value>(&body).is_ok(),
                len == strict::MAX_KEY_BYTES
            );
        }
        // The next key can consume nearly the full admitted wire before its
        // much smaller key-size ceiling is checked.
        let large_key = "x".repeat(MAX_RECORD_BYTES - r#"{"":null}"#.len());
        let body = json!({large_key: null}).to_string();
        assert_eq!(body.len(), MAX_RECORD_BYTES);
        assert!(decode_record::<Value>(&body).is_err());
        assert!(decode_record::<Value>(r#"{"duplicate":1,"duplicate":2}"#).is_err());
    }

    // AC-0327: restore the actual closed ledger and account typed slots, not
    // just serialized bytes. Invalid policy shapes still pass through decoding.
    #[test]
    fn ledger_decode_workspace_covers_real_and_prevalidation_owners() {
        use super::super::*;
        let mut ledger = super::super::tests::ledger();
        let raw = serde_json::to_string(&ledger).unwrap();
        assert_eq!(
            decode_record::<InvestigationInputLedger>(&raw).unwrap(),
            ledger
        );
        let (retained, peak) = InvestigationInputLedger::decode_workspace_bytes().unwrap();
        let typed = std::mem::size_of_val(&ledger)
            + ledger.selected_facts.len() * std::mem::size_of::<crate::TaskFactSelection>()
            + ledger.citations.len() * std::mem::size_of::<InvestigationCitation>();
        assert!(retained >= typed + raw.len());
        assert!(peak >= retained + record_decode_scratch_bytes().unwrap());
        ledger.selected_facts = vec![ledger.selected_facts[0].clone(); RECORD_MAX_ARRAY_ITEMS];
        let raw = serde_json::to_string(&ledger).unwrap();
        let malformed = decode_record::<InvestigationInputLedger>(&raw).unwrap();
        assert!(
            malformed
                .validate_with_policy(&INVESTIGATION_V2_POLICY)
                .is_err()
        );
        assert!(
            retained
                >= std::mem::size_of_val(&malformed)
                    + malformed.selected_facts.len()
                        * std::mem::size_of::<crate::TaskFactSelection>()
                    + raw.len()
        );
    }

    // AC-0327: every intermediate calculation refuses overflow, not saturation.
    #[test]
    fn record_workspace_checked_phases_grow_and_refuse_overflow() {
        let bound = record_decode_scratch_bytes().unwrap();
        assert!(scratch(MAX_RECORD_BYTES * 2, RECORD_MAX_DEPTH).unwrap() > bound);
        assert!(scratch(1, RECORD_MAX_DEPTH).unwrap() > scratch(1, 0).unwrap());
        assert_eq!(scratch(usize::MAX, RECORD_MAX_DEPTH), None);
        assert_eq!(scratch(MAX_RECORD_BYTES, usize::MAX), None);
    }
}
