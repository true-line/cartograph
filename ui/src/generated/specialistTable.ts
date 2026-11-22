// GENERATED from crates/agents/src/investigation/capabilities.rs (ADR-0071) — do not edit.
// Regenerate: CARTOGRAPH_WRITE_GENERATED=1 cargo test -p agents ui_specialist_table_is_generated_from_the_capability_table

export type SpecialistId =
  | 'domain-analyst@1'
  | 'evidence-auditor@1'
  | 'domain-analyst@2'
  | 'evidence-auditor@2'
  | 'domain-analyst@3'
  | 'evidence-auditor@3'
  | 'domain-analyst@4'
  | 'evidence-auditor@4'
  | 'domain-analyst@5'
  | 'evidence-auditor@5'
  | 'domain-analyst@6'
  | 'evidence-auditor@6'
  | 'domain-analyst@7'
  | 'evidence-auditor@7'
  | 'domain-analyst@8'
  | 'evidence-auditor@8'
  | 'domain-analyst@9'
  | 'evidence-auditor@9'
  | 'domain-analyst@10'
  | 'evidence-auditor@10'
  | 'domain-analyst@11'
  | 'domain-analyst@12'
  | 'domain-analyst@13'
  | 'domain-analyst@14'
  | 'domain-analyst@15'
  | 'domain-analyst@17'
  | 'domain-analyst@18'
  | 'domain-analyst@19'
  | 'domain-analyst@20'
  | 'domain-analyst@21'
  | 'domain-analyst@22'
  | 'domain-analyst@23'
  | 'domain-analyst@24'
  | 'domain-analyst@25'
  | 'domain-analyst@26'
  | 'domain-analyst@27'
  | 'domain-analyst@28'
  | 'domain-analyst@29'
  | 'evidence-auditor@11'
  | 'evidence-auditor@12'
  | 'evidence-auditor@13'
  | 'evidence-auditor@14'
  | 'spec-writer@1'
  | 'spec-writer@2'
  | 'gap-triager@1'
  | 'gap-triager@2'
  | 'section-writer@1';
export type SpecialistRole =
  | 'domain-analyst'
  | 'evidence-auditor'
  | 'spec-writer'
  | 'gap-triager'
  | 'section-writer';
export type SpecialistAvailability =
  | 'current'
  | 'offered'
  | 'retired';
export type SpecialistCapability =
  | 'compact_disclosure'
  | 'turn_progress'
  | 'inspection_requirements'
  | 'coverage_qualification'
  | 'completion_obligations'
  | 'policy_selection'
  | 'query_history_disclosure'
  | 'next_prompt_admission'
  | 'cited_narrative'
  | 'combined_finish_feedback'
  | 'finish_patch'
  | 'visible_action_schema'
  | 'exact_source_reservation'
  | 'query_budget_refusals'
  | 'acquisition_choices'
  | 'exact_node_queries'
  | 'capacity_exclusions'
  | 'finish_feedback_witness'
  | 'typed_citation_correction'
  | 'finding_withholding'
  | 'local_finish_generation'
  | 'native_tool_runtime';

export interface SpecialistTableRow {
  readonly role: SpecialistRole;
  readonly version: number;
  readonly availability: SpecialistAvailability;
  readonly capabilities: readonly SpecialistCapability[];
}

/** One row per specialist version, in host declaration order. */
export const SPECIALIST_TABLE: Readonly<Record<SpecialistId, SpecialistTableRow>> = {
  'domain-analyst@1': { role: 'domain-analyst', version: 1, availability: 'retired', capabilities: [] },
  'evidence-auditor@1': { role: 'evidence-auditor', version: 1, availability: 'retired', capabilities: [] },
  'domain-analyst@2': { role: 'domain-analyst', version: 2, availability: 'retired', capabilities: [] },
  'evidence-auditor@2': { role: 'evidence-auditor', version: 2, availability: 'retired', capabilities: [] },
  'domain-analyst@3': { role: 'domain-analyst', version: 3, availability: 'retired', capabilities: ['compact_disclosure'] },
  'evidence-auditor@3': { role: 'evidence-auditor', version: 3, availability: 'retired', capabilities: ['compact_disclosure'] },
  'domain-analyst@4': { role: 'domain-analyst', version: 4, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress'] },
  'evidence-auditor@4': { role: 'evidence-auditor', version: 4, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress'] },
  'domain-analyst@5': { role: 'domain-analyst', version: 5, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'inspection_requirements'] },
  'evidence-auditor@5': { role: 'evidence-auditor', version: 5, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'inspection_requirements'] },
  'domain-analyst@6': { role: 'domain-analyst', version: 6, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification'] },
  'evidence-auditor@6': { role: 'evidence-auditor', version: 6, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification'] },
  'domain-analyst@7': { role: 'domain-analyst', version: 7, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations'] },
  'evidence-auditor@7': { role: 'evidence-auditor', version: 7, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations'] },
  'domain-analyst@8': { role: 'domain-analyst', version: 8, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection'] },
  'evidence-auditor@8': { role: 'evidence-auditor', version: 8, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection'] },
  'domain-analyst@9': { role: 'domain-analyst', version: 9, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure'] },
  'evidence-auditor@9': { role: 'evidence-auditor', version: 9, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure'] },
  'domain-analyst@10': { role: 'domain-analyst', version: 10, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'evidence-auditor@10': { role: 'evidence-auditor', version: 10, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'domain-analyst@11': { role: 'domain-analyst', version: 11, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'domain-analyst@12': { role: 'domain-analyst', version: 12, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'domain-analyst@13': { role: 'domain-analyst', version: 13, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'domain-analyst@14': { role: 'domain-analyst', version: 14, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'domain-analyst@15': { role: 'domain-analyst', version: 15, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'domain-analyst@17': { role: 'domain-analyst', version: 17, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'exact_node_queries'] },
  'domain-analyst@18': { role: 'domain-analyst', version: 18, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'acquisition_choices', 'exact_node_queries'] },
  'domain-analyst@19': { role: 'domain-analyst', version: 19, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'acquisition_choices', 'exact_node_queries'] },
  'domain-analyst@20': { role: 'domain-analyst', version: 20, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'acquisition_choices', 'exact_node_queries', 'typed_citation_correction'] },
  'domain-analyst@21': { role: 'domain-analyst', version: 21, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'typed_citation_correction'] },
  'domain-analyst@22': { role: 'domain-analyst', version: 22, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction'] },
  'domain-analyst@23': { role: 'domain-analyst', version: 23, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction'] },
  'domain-analyst@24': { role: 'domain-analyst', version: 24, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'combined_finish_feedback', 'exact_source_reservation', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction'] },
  'domain-analyst@25': { role: 'domain-analyst', version: 25, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'combined_finish_feedback', 'exact_source_reservation', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction', 'finding_withholding'] },
  'domain-analyst@26': { role: 'domain-analyst', version: 26, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'combined_finish_feedback', 'exact_source_reservation', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction', 'finding_withholding', 'local_finish_generation'] },
  'domain-analyst@27': { role: 'domain-analyst', version: 27, availability: 'offered', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'combined_finish_feedback', 'finish_patch', 'exact_source_reservation', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction', 'finding_withholding', 'local_finish_generation'] },
  'domain-analyst@28': { role: 'domain-analyst', version: 28, availability: 'current', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'combined_finish_feedback', 'finish_patch', 'visible_action_schema', 'exact_source_reservation', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction', 'finding_withholding', 'local_finish_generation'] },
  'domain-analyst@29': { role: 'domain-analyst', version: 29, availability: 'offered', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'combined_finish_feedback', 'finish_patch', 'visible_action_schema', 'exact_source_reservation', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction', 'finding_withholding', 'local_finish_generation', 'native_tool_runtime'] },
  'evidence-auditor@11': { role: 'evidence-auditor', version: 11, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission'] },
  'evidence-auditor@12': { role: 'evidence-auditor', version: 12, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'query_budget_refusals', 'acquisition_choices', 'exact_node_queries'] },
  'evidence-auditor@13': { role: 'evidence-auditor', version: 13, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'query_budget_refusals', 'acquisition_choices', 'exact_node_queries', 'finish_feedback_witness', 'typed_citation_correction'] },
  'evidence-auditor@14': { role: 'evidence-auditor', version: 14, availability: 'current', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'policy_selection', 'query_history_disclosure', 'next_prompt_admission', 'query_budget_refusals', 'acquisition_choices', 'exact_node_queries', 'capacity_exclusions', 'finish_feedback_witness', 'typed_citation_correction'] },
  'spec-writer@1': { role: 'spec-writer', version: 1, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations'] },
  'spec-writer@2': { role: 'spec-writer', version: 2, availability: 'current', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'cited_narrative'] },
  'gap-triager@1': { role: 'gap-triager', version: 1, availability: 'retired', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations'] },
  'gap-triager@2': { role: 'gap-triager', version: 2, availability: 'current', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations', 'cited_narrative'] },
  'section-writer@1': { role: 'section-writer', version: 1, availability: 'current', capabilities: ['compact_disclosure', 'turn_progress', 'coverage_qualification', 'completion_obligations'] },
};
