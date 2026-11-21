//! Proves the capability table is a neutral rewrite (ADR-0071 slice a): each
//! row answers exactly what the `uses_*` match arms answered before the table.
//! The arms below are copied verbatim from `specialists.rs` at `ee9df76`.

use super::*;

#[test]
fn table_rows_follow_enum_order_and_ids() {
    for (index, row) in SPECIALIST_TABLE.iter().enumerate() {
        assert_eq!(row.id as usize, index, "{:?}", row.id);
        let wire = serde_json::to_value(row.id).unwrap();
        assert_eq!(
            wire,
            format!("{}@{}", row.role.slug(), row.version),
            "{:?}",
            row.id
        );
    }
    // Every variant decodes and lands on its own row.
    assert_eq!(SpecialistId::all().count(), SPECIALIST_TABLE.len());
}

#[test]
fn every_capability_matches_the_legacy_predicates() {
    for id in SpecialistId::all() {
        let legacy: [(Capability, bool); 19] = [
            (C::CompactDisclosure, legacy_uses_compact_disclosure(id)),
            (C::TurnProgress, legacy_uses_turn_progress(id)),
            (
                C::InspectionRequirements,
                legacy_uses_inspection_requirements(id),
            ),
            (
                C::CoverageQualification,
                legacy_uses_coverage_qualification(id),
            ),
            (
                C::CompletionObligations,
                legacy_uses_completion_obligations(id),
            ),
            (C::PolicySelection, legacy_uses_policy_selection(id)),
            (
                C::QueryHistoryDisclosure,
                legacy_uses_query_history_disclosure(id),
            ),
            (
                C::NextPromptAdmission,
                legacy_uses_next_prompt_admission(id),
            ),
            (C::CitedNarrative, legacy_uses_cited_narrative(id)),
            (
                C::CombinedFinishFeedback,
                legacy_uses_combined_finish_feedback(id),
            ),
            (C::FinishPatch, legacy_uses_finish_patch(id)),
            (
                C::VisibleActionSchema,
                legacy_uses_visible_action_schema(id),
            ),
            (
                C::ExactSourceReservation,
                legacy_uses_exact_source_reservation(id),
            ),
            (
                C::QueryBudgetRefusals,
                legacy_uses_query_budget_refusals(id),
            ),
            (C::AcquisitionChoices, legacy_uses_acquisition_choices(id)),
            (C::ExactNodeQueries, legacy_uses_exact_node_queries(id)),
            (C::CapacityExclusions, legacy_uses_capacity_exclusions(id)),
            (
                C::FinishFeedbackWitness,
                legacy_uses_finish_feedback_witness(id),
            ),
            (
                C::TypedCitationCorrection,
                legacy_uses_typed_citation_correction(id),
            ),
        ];
        for (capability, expected) in legacy {
            assert_eq!(id.has(capability), expected, "{id:?} {capability:?}");
        }
        assert_eq!(id.row().version, legacy_version(id), "{id:?}");
        assert_eq!(
            id.row().prompt_generation,
            legacy_prompt_generation(id),
            "{id:?}"
        );
    }
}

#[test]
fn every_capability_is_carried_by_some_version() {
    for capability in Capability::ALL {
        assert!(
            SpecialistId::all().any(|id| id.has(capability)),
            "{capability:?} has no row"
        );
    }
}

fn legacy_uses_compact_disclosure(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::SpecWriterV1
            | SpecialistId::SpecWriterV2
            | SpecialistId::GapTriagerV1
            | SpecialistId::GapTriagerV2
            | SpecialistId::SectionWriterV1
            | SpecialistId::DomainAnalystV3
            | SpecialistId::EvidenceAuditorV3
            | SpecialistId::DomainAnalystV4
            | SpecialistId::EvidenceAuditorV4
            | SpecialistId::DomainAnalystV5
            | SpecialistId::EvidenceAuditorV5
            | SpecialistId::DomainAnalystV6
            | SpecialistId::EvidenceAuditorV6
            | SpecialistId::DomainAnalystV7
            | SpecialistId::EvidenceAuditorV7
            | SpecialistId::DomainAnalystV8
            | SpecialistId::EvidenceAuditorV8
            | SpecialistId::DomainAnalystV9
            | SpecialistId::EvidenceAuditorV9
            | SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_turn_progress(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::SpecWriterV1
            | SpecialistId::SpecWriterV2
            | SpecialistId::GapTriagerV1
            | SpecialistId::GapTriagerV2
            | SpecialistId::SectionWriterV1
            | SpecialistId::DomainAnalystV4
            | SpecialistId::EvidenceAuditorV4
            | SpecialistId::DomainAnalystV5
            | SpecialistId::EvidenceAuditorV5
            | SpecialistId::DomainAnalystV6
            | SpecialistId::EvidenceAuditorV6
            | SpecialistId::DomainAnalystV7
            | SpecialistId::EvidenceAuditorV7
            | SpecialistId::DomainAnalystV8
            | SpecialistId::EvidenceAuditorV8
            | SpecialistId::DomainAnalystV9
            | SpecialistId::EvidenceAuditorV9
            | SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_inspection_requirements(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV5 | SpecialistId::EvidenceAuditorV5
    )
}

fn legacy_uses_coverage_qualification(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::SpecWriterV1
            | SpecialistId::SpecWriterV2
            | SpecialistId::GapTriagerV1
            | SpecialistId::GapTriagerV2
            | SpecialistId::SectionWriterV1
            | SpecialistId::DomainAnalystV6
            | SpecialistId::EvidenceAuditorV6
            | SpecialistId::DomainAnalystV7
            | SpecialistId::EvidenceAuditorV7
            | SpecialistId::DomainAnalystV8
            | SpecialistId::EvidenceAuditorV8
            | SpecialistId::DomainAnalystV9
            | SpecialistId::EvidenceAuditorV9
            | SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_completion_obligations(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::SpecWriterV1
            | SpecialistId::SpecWriterV2
            | SpecialistId::GapTriagerV1
            | SpecialistId::GapTriagerV2
            | SpecialistId::SectionWriterV1
            | SpecialistId::DomainAnalystV7
            | SpecialistId::EvidenceAuditorV7
            | SpecialistId::DomainAnalystV8
            | SpecialistId::EvidenceAuditorV8
            | SpecialistId::DomainAnalystV9
            | SpecialistId::EvidenceAuditorV9
            | SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_policy_selection(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV8
            | SpecialistId::EvidenceAuditorV8
            | SpecialistId::DomainAnalystV9
            | SpecialistId::EvidenceAuditorV9
            | SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_query_history_disclosure(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV9
            | SpecialistId::EvidenceAuditorV9
            | SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_next_prompt_admission(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV10
            | SpecialistId::EvidenceAuditorV10
            | SpecialistId::DomainAnalystV11
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_cited_narrative(id: SpecialistId) -> bool {
    matches!(id, SpecialistId::SpecWriterV2 | SpecialistId::GapTriagerV2)
}

fn legacy_version(id: SpecialistId) -> u32 {
    match id {
        SpecialistId::DomainAnalystV1
        | SpecialistId::EvidenceAuditorV1
        | SpecialistId::SpecWriterV1
        | SpecialistId::GapTriagerV1
        | SpecialistId::SectionWriterV1 => 1,
        SpecialistId::DomainAnalyst
        | SpecialistId::EvidenceAuditor
        | SpecialistId::SpecWriterV2
        | SpecialistId::GapTriagerV2 => 2,
        SpecialistId::DomainAnalystV3 | SpecialistId::EvidenceAuditorV3 => 3,
        SpecialistId::DomainAnalystV4 | SpecialistId::EvidenceAuditorV4 => 4,
        SpecialistId::DomainAnalystV5 | SpecialistId::EvidenceAuditorV5 => 5,
        SpecialistId::DomainAnalystV6 | SpecialistId::EvidenceAuditorV6 => 6,
        SpecialistId::DomainAnalystV7 | SpecialistId::EvidenceAuditorV7 => 7,
        SpecialistId::DomainAnalystV8 | SpecialistId::EvidenceAuditorV8 => 8,
        SpecialistId::DomainAnalystV9 | SpecialistId::EvidenceAuditorV9 => 9,
        SpecialistId::DomainAnalystV10 | SpecialistId::EvidenceAuditorV10 => 10,
        SpecialistId::DomainAnalystV11 | SpecialistId::EvidenceAuditorV11 => 11,
        SpecialistId::DomainAnalystV12 | SpecialistId::EvidenceAuditorV12 => 12,
        SpecialistId::DomainAnalystV13 | SpecialistId::EvidenceAuditorV13 => 13,
        SpecialistId::DomainAnalystV14 | SpecialistId::EvidenceAuditorV14 => 14,
        SpecialistId::DomainAnalystV15 => 15,
        SpecialistId::DomainAnalystV17 => 17,
        SpecialistId::DomainAnalystV18 => 18,
        SpecialistId::DomainAnalystV19 => 19,
        SpecialistId::DomainAnalystV20 => 20,
        SpecialistId::DomainAnalystV21 => 21,
        SpecialistId::DomainAnalystV22 => 22,
        SpecialistId::DomainAnalystV23 => 23,
        SpecialistId::DomainAnalystV24 => 24,
        SpecialistId::DomainAnalystV25 => 25,
        SpecialistId::DomainAnalystV26 => 26,
        SpecialistId::DomainAnalystV27 => 27,
        SpecialistId::DomainAnalystV28 => 28,
    }
}

fn legacy_prompt_generation(id: SpecialistId) -> u32 {
    if id == SpecialistId::EvidenceAuditorV14 {
        return 13;
    }
    if matches!(
        id,
        SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    ) {
        return 14;
    }
    if id == SpecialistId::DomainAnalystV23 {
        return 13;
    }
    if matches!(
        id,
        SpecialistId::DomainAnalystV22
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    ) {
        return 12;
    }
    if id == SpecialistId::DomainAnalystV21 {
        return 11;
    }
    if matches!(
        id,
        SpecialistId::DomainAnalystV11
            | SpecialistId::EvidenceAuditorV11
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
            | SpecialistId::DomainAnalystV12
            | SpecialistId::DomainAnalystV13
            | SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    ) {
        return 10;
    }
    if matches!(
        id,
        SpecialistId::SpecWriterV1
            | SpecialistId::SpecWriterV2
            | SpecialistId::GapTriagerV1
            | SpecialistId::GapTriagerV2
            | SpecialistId::SectionWriterV1
    ) {
        7
    } else {
        legacy_version(id)
    }
}

fn legacy_uses_combined_finish_feedback(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    )
}

fn legacy_uses_finish_patch(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV27 | SpecialistId::DomainAnalystV28
    )
}

fn legacy_uses_visible_action_schema(id: SpecialistId) -> bool {
    id == SpecialistId::DomainAnalystV28
}

fn legacy_uses_exact_source_reservation(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    )
}

fn legacy_uses_query_budget_refusals(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_acquisition_choices(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_exact_node_queries(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::DomainAnalystV17
            | SpecialistId::DomainAnalystV18
            | SpecialistId::DomainAnalystV19
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
            | SpecialistId::EvidenceAuditorV12
            | SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
    )
}

fn legacy_uses_capacity_exclusions(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::EvidenceAuditorV14
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    )
}

fn legacy_uses_finish_feedback_witness(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    )
}

fn legacy_uses_typed_citation_correction(id: SpecialistId) -> bool {
    matches!(
        id,
        SpecialistId::EvidenceAuditorV13
            | SpecialistId::EvidenceAuditorV14
            | SpecialistId::DomainAnalystV20
            | SpecialistId::DomainAnalystV21
            | SpecialistId::DomainAnalystV22
            | SpecialistId::DomainAnalystV23
            | SpecialistId::DomainAnalystV24
            | SpecialistId::DomainAnalystV25
            | SpecialistId::DomainAnalystV26
            | SpecialistId::DomainAnalystV27
            | SpecialistId::DomainAnalystV28
    )
}
