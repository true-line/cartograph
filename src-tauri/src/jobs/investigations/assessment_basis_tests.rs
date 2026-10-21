use super::*;

#[test]
fn completion_assessment_basis_requires_saved_disclosure_family() {
    // AC-0286: shared V2 limits cannot relabel a saved @8 or @9 assessment,
    // including the nested refusal basis retained after the terminal transition.
    let policy = &INVESTIGATION_V2_POLICY;
    for progressive in [false, true] {
        let mut fixture =
            Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        for refusal_count in 1..=2 {
            fixture.refuse();
            let tx = fixture.store.conn.unchecked_transaction().unwrap();
            let mut task = load_task_base(&tx, &fixture.id).unwrap();
            let intent = task.completion_intent.clone().unwrap();
            intent.validate(&task).unwrap();
            let completion = task.detail.completion.as_mut().unwrap();
            let hash = match &mut completion.assessment_basis {
                CompletionAssessmentBasis::FinishingDisclosure {
                    disclosure_hash, ..
                } => {
                    assert_eq!(refusal_count, 1);
                    disclosure_hash
                }
                CompletionAssessmentBasis::TerminalObservation {
                    last_refusal_basis, ..
                } => {
                    assert_eq!(refusal_count, 2);
                    &mut last_refusal_basis.as_mut().unwrap().disclosure_hash
                }
                CompletionAssessmentBasis::NotAssessed => panic!("finish must have a basis"),
            };
            let expected = if progressive { 5 } else { 4 };
            let original_prefix = format!("investigation-disclosure-v{expected}:");
            let digest = hash.strip_prefix(&original_prefix).unwrap().to_owned();
            *hash = format!(
                "investigation-disclosure-v{}:{digest}",
                if progressive { 4 } else { 5 }
            );
            // Both domains satisfy the generic V2 metadata shape. The saved
            // family must reject the relabel before replaying the journal.
            completion.validate_with_policy(policy).unwrap();
            assert!(matches!(
                intent.validate(&task),
                Err(InvestigationStoreError::Invalid)
            ));
            let raw = serde_json::to_vec(&task).unwrap();
            tx.execute(
                "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                params![raw, core_prov::content_hash(&raw), fixture.id],
            )
            .unwrap();
            assert!(matches!(
                load_task_base(&tx, &fixture.id),
                Err(InvestigationStoreError::Invalid)
            ));
            tx.rollback().unwrap();
            fixture.store.investigation(&fixture.id).unwrap();
        }
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        reopened.investigation(&fixture.id).unwrap();
    }
}
