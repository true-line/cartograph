//! Exact terminal-only definitions retained across development @20 prompt revisions.
use super::*;

// Pinned from retained run 6/7 and run 8 terminal records respectively.
const ANALYST20_HISTORY: [&str; 2] = [
    "25ae99fe3e2a6611db20cc673b27190a59dfe1061aa5a786cbe351b29d9e65e4",
    "9f038354ab93e5734fe38d4481dfa07ec23e08aa6f93a17b0f76db23883887db",
];

pub(super) fn matches(
    saved: &InvestigationSpecialist,
    role: SpecialistId,
    terminal: bool,
) -> Result<bool, InvestigationStoreError> {
    let mut expected = role.definition()?;
    if saved == &expected {
        return Ok(true);
    }
    if !terminal
        || role != SpecialistId::DomainAnalystV20
        || !ANALYST20_HISTORY.contains(&saved.prompt_fingerprint.as_str())
    {
        return Ok(false);
    }
    // Preserve every other field, including identity, operations and trust tier.
    expected
        .prompt_fingerprint
        .clone_from(&saved.prompt_fingerprint);
    Ok(saved == &expected)
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    // AC-0566: old terminal records remain readable without admitting old dispatch.
    #[test]
    fn terminal_analyst20_definitions_preserve_exact_metadata() {
        let role = SpecialistId::DomainAnalystV20;
        let current = role.definition().unwrap();
        assert!(matches(&current, role, false).unwrap());
        for fingerprint in ANALYST20_HISTORY {
            let mut saved = current.clone();
            saved.prompt_fingerprint = fingerprint.into();
            assert!(matches(&saved, role, true).unwrap());
            if saved != current {
                assert!(!matches(&saved, role, false).unwrap());
            }
            saved.operations.pop();
            assert!(!matches(&saved, role, true).unwrap());
            saved = current.clone();
            saved.prompt_fingerprint = fingerprint.into();
            saved.name.push_str(" changed");
            assert!(!matches(&saved, role, true).unwrap());
            assert!(!matches(&saved, SpecialistId::DomainAnalystV19, true).unwrap());
        }
        let mut forged = current;
        forged.prompt_fingerprint = "0".repeat(64);
        assert!(!matches(&forged, role, true).unwrap());
    }
    // AC-0566: opt-in receipt-backed regression on copied development stores.
    #[test]
    #[ignore = "requires isolated retained development fixtures"]
    fn historical_analyst20_terminal_receipts_reopen() {
        let root = std::env::var("CARTOGRAPH_ADVISORY_HISTORY_FIXTURES").unwrap();
        let root = std::path::Path::new(&root);
        assert!(root.ends_with("advisory-history-copies"));
        for name in ["run6", "run7", "run8"] {
            let fixture = root.join(name);
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(fixture.join("terminal.json")).unwrap())
                    .unwrap();
            let expected: InvestigationDetail =
                serde_json::from_value(value["saved"]["detail"].clone()).unwrap();
            let result: Option<InvestigationResult> =
                serde_json::from_value(value["saved"]["result"].clone()).unwrap();
            let store = JobStore::open(fixture.join("profile/state.db")).unwrap();
            let id = &expected.summary.investigation_id;
            assert_eq!(store.investigation(id).unwrap(), expected, "{name}");
            assert_eq!(store.investigation_result(id).unwrap(), result, "{name}");
        }
    }
}
