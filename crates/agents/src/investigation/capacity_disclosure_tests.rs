//! AC-1430 (T-1430): a capacity-excluded obligation named in a top-level
//! limitation is disclosed, not met, and only a supported result carries it.
use super::*;

fn excluded(id: &str) -> ObligationObservation {
    ObligationObservation {
        obligation_id: id.into(),
        kind: "source".into(),
        attempted: false,
        acquired: false,
        exactly_supplied: false,
        unmet: Some(RefusalReason::NotAttempted),
        last_attempt_outcome: None,
        capacity_disclosed: false,
    }
}

fn met(id: &str) -> ObligationObservation {
    ObligationObservation {
        attempted: true,
        acquired: true,
        exactly_supplied: true,
        unmet: None,
        last_attempt_outcome: Some(CompletionAttemptOutcome::Acquired),
        ..excluded(id)
    }
}

#[test]
fn a_named_capacity_exclusion_is_disclosed_and_nothing_else_is() {
    let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
    let named = vec![format!(
        "The {} read was excluded for capacity.",
        capacity_name(&a)
    )];
    let mut assessment = vec![excluded(&a), excluded(&b), met(&c)];
    let before = assessment.clone();
    disclose_named(&mut assessment, &named, &[]);
    assert_eq!(assessment, before, "only bound exclusions qualify");
    disclose_named(&mut assessment, &named, &[a.clone(), b.clone()]);
    assert!(assessment[0].capacity_disclosed && assessment[0].unmet.is_none());
    assert_eq!(assessment[1], before[1], "an unnamed exclusion stays unmet");
    assert_eq!(assessment[2], before[2]);
    let mut attempted = vec![ObligationObservation {
        attempted: true,
        last_attempt_outcome: Some(CompletionAttemptOutcome::Unavailable),
        ..excluded(&a)
    }];
    let kept = attempted.clone();
    disclose_named(&mut attempted, &named, std::slice::from_ref(&a));
    assert_eq!(
        attempted, kept,
        "an attempted obligation is not a capacity gap"
    );
    // Old records without the flag keep their exact bytes.
    assert!(
        serde_json::to_value(&before[0])
            .unwrap()
            .get("capacity_disclosed")
            .is_none()
    );
}

#[test]
fn only_a_supported_result_that_names_the_gap_carries_a_disclosure() {
    let (a, c) = ("a".repeat(64), "c".repeat(64));
    let mut disclosed = excluded(&a);
    disclosed.unmet = None;
    disclosed.capacity_disclosed = true;
    let completion = |outcome, assessment| InvestigationCompletionResult {
        withholding: None,
        schema_version: 1,
        outcome,
        request_hash: "d".repeat(64),
        obligations_hash: "e".repeat(64),
        finishing_attempts_hash: "f".repeat(64),
        assessment,
    };
    let finding = InvestigationFinding {
        finding_id: "finding-1".into(),
        claim_kind: InvestigationClaimKind::ImplementedBehavior,
        title: "Stock is checked".into(),
        statement: "The threshold is read before sale.".into(),
        citation_ids: vec!["fact-1".into()],
        limitations: vec!["Other paths were not read.".into()],
        tier: core_prov::Tier::Agentic,
        confidence_tier: core_prov::ConfidenceTier::InferredWeak,
    };
    let named = vec![format!("{} was not read (capacity).", capacity_name(&a))];
    let supported = completion(
        CompletionOutcome::Supported,
        vec![disclosed.clone(), met(&c)],
    );
    supported
        .validate(
            std::slice::from_ref(&finding),
            KnowledgeCompleteness::Partial,
            &named,
        )
        .unwrap();
    assert!(
        supported
            .validate(
                std::slice::from_ref(&finding),
                KnowledgeCompleteness::Partial,
                &["Nothing named.".to_string()],
            )
            .is_err()
    );
    let mut attempted = disclosed.clone();
    attempted.attempted = true;
    attempted.last_attempt_outcome = Some(CompletionAttemptOutcome::Unavailable);
    assert!(
        completion(CompletionOutcome::Supported, vec![attempted, met(&c)])
            .validate(
                std::slice::from_ref(&finding),
                KnowledgeCompleteness::Partial,
                &named,
            )
            .is_err()
    );
    assert!(
        completion(CompletionOutcome::InsufficientEvidence, vec![disclosed])
            .validate(&[], KnowledgeCompleteness::InsufficientEvidence, &named)
            .is_err()
    );
}

// AC-1444: the host names each capacity gap the finish left unnamed, within
// the limitation bound, and the named gaps then count as disclosed.
#[test]
fn the_host_discloses_unnamed_capacity_gaps_within_the_limitation_bound() {
    let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
    let entry = |id: &str, reason| FeedbackEntry {
        obligation_id: id.into(),
        kind: "source".into(),
        reason,
    };
    let unmet = vec![
        entry(&a, RefusalReason::ExcludedByCapacity),
        entry(&b, RefusalReason::ExcludedByCapacity),
        entry(&c, RefusalReason::NotAttempted),
    ];
    let mut limitations = vec![
        "Other paths were not read.".to_string(),
        format!("{} was not read (capacity).", capacity_name(&b)),
    ];
    host_capacity_disclosures(&mut limitations, &unmet);
    assert_eq!(
        limitations,
        vec![
            "Other paths were not read.".to_string(),
            format!("{} was not read (capacity).", capacity_name(&b)),
            format!("{}: {HOST_CAPACITY_DISCLOSURE}", capacity_name(&a)),
        ],
        "one line for the unnamed gap; a named gap and another reason add none"
    );
    assert!(limitations[2].len() <= 512);
    let mut assessment = vec![excluded(&a), excluded(&b), excluded(&c)];
    disclose_named(&mut assessment, &limitations, &[a.clone(), b.clone()]);
    assert!(assessment[0].capacity_disclosed && assessment[1].capacity_disclosed);
    assert!(!assessment[2].capacity_disclosed);
    // A full limitation list takes no host line; the gap stays unmet.
    let mut full = vec!["x".to_string(); finish_patch::MAX_LIMITATIONS];
    host_capacity_disclosures(&mut full, &unmet);
    assert_eq!(full.len(), finish_patch::MAX_LIMITATIONS);
    assert!(full.iter().all(|text| text == "x"));
}
