import { SPECIALIST_TABLE, type SpecialistCapability, type SpecialistId } from './generated/specialistTable';

/** Reads the host capability table (ADR-0071); the client keeps no version list of its own. */
export const hasCapability = (id: string, capability: SpecialistCapability) =>
  Object.hasOwn(SPECIALIST_TABLE, id) && SPECIALIST_TABLE[id as SpecialistId].capabilities.includes(capability);
/** Exact public family identities; saved generations never inherit catalog upgrades. */
export const isCompletionSpecialist = (id: string) => hasCapability(id, 'completion_obligations');
export const isExpandedSpecialist = (id: string) => hasCapability(id, 'policy_selection');
export const usesCitedNarrative = (id: string) => hasCapability(id, 'cited_narrative');

/** The completion request family a version belongs to, keyed by its summary schema version. */
function familyVersion(id: string): 4 | 5 | 6 | 7 | undefined {
  if (!isCompletionSpecialist(id)) return undefined;
  if (!isExpandedSpecialist(id)) return 4;
  if (!hasCapability(id, 'query_history_disclosure')) return 5;
  return hasCapability(id, 'next_prompt_admission') ? 7 : 6;
}
const resultVersions = { 4: 5, 5: 5, 6: 6, 7: 7 } as const;
const families = Object.fromEntries(([4, 5, 6, 7] as const).map((version) => {
  const specialistIds = (Object.keys(SPECIALIST_TABLE) as SpecialistId[]).filter((id) => familyVersion(id) === version);
  return [version, { specialistIds, roleVersions: [...new Set(specialistIds.map((id) => SPECIALIST_TABLE[id].version))],
    requestVersion: version, resultVersion: resultVersions[version], expanded: version !== 4 }];
})) as Record<4 | 5 | 6 | 7, { specialistIds: SpecialistId[]; roleVersions: number[]; requestVersion: 4 | 5 | 6 | 7; resultVersion: 5 | 6 | 7; expanded: boolean }>;
export function completionFamily(summaryVersion: number) {
  return Number.isInteger(summaryVersion) ? families[summaryVersion as keyof typeof families] : undefined;
}
const hash = /^[0-9a-f]{64}(?![\s\S])/;
export function isCompletionDisclosure(value: unknown, resultVersion: number): value is string {
  const version = resultVersion === 5 || resultVersion === 8 ? 4 : resultVersion === 6 ? 5 : resultVersion === 7 ? 6 : undefined;
  const prefix = `investigation-disclosure-v${version}:`;
  return version !== undefined && typeof value === 'string' && value.startsWith(prefix) && hash.test(value.slice(prefix.length));
}
export function isCompletionResultId(value: unknown, resultVersion: number): value is string {
  const prefix = `investigation-result-v${resultVersion}:`;
  return [5, 6, 7, 8].includes(resultVersion) && typeof value === 'string' && value.startsWith(prefix) && hash.test(value.slice(prefix.length));
}

/** Saved partial results, not the set of creatable desktop specialists. */
export const usesFindingWithholding = (id: string) => hasCapability(id, 'finding_withholding');
