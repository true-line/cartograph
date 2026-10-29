/** Exact public family identities; saved generations never inherit catalog upgrades. */
export const isCompletionSpecialist = (id: string) => (id === 'domain-analyst@12' || id === 'domain-analyst@13' || id === 'domain-analyst@14' || id === 'domain-analyst@15' || id === 'domain-analyst@17') || /^(domain-analyst|evidence-auditor)@(7|8|9|10|11)$/.test(id) || id === 'spec-writer@1' || id === 'spec-writer@2' || id === 'gap-triager@1' || id === 'gap-triager@2';
export const isExpandedSpecialist = (id: string) => (id === 'domain-analyst@12' || id === 'domain-analyst@13' || id === 'domain-analyst@14' || id === 'domain-analyst@15' || id === 'domain-analyst@17') || /^(domain-analyst|evidence-auditor)@(8|9|10|11)$/.test(id);
export const usesCitedNarrative = (id: string) => id === 'spec-writer@2' || id === 'gap-triager@2';
const families = {
  4: { specialistIds: ['domain-analyst@7', 'evidence-auditor@7', 'spec-writer@1', 'spec-writer@2', 'gap-triager@1', 'gap-triager@2'], roleVersions: [7], requestVersion: 4, resultVersion: 5, expanded: false },
  5: { specialistIds: ['domain-analyst@8', 'evidence-auditor@8'], roleVersions: [8], requestVersion: 5, resultVersion: 5, expanded: true },
  6: { specialistIds: ['domain-analyst@9', 'evidence-auditor@9'], roleVersions: [9], requestVersion: 6, resultVersion: 6, expanded: true },
  7: { specialistIds: ['domain-analyst@10', 'evidence-auditor@10', 'domain-analyst@11', 'evidence-auditor@11', 'domain-analyst@12', 'domain-analyst@13', 'domain-analyst@14', 'domain-analyst@15', 'domain-analyst@17'], roleVersions: [10, 11, 12, 13, 14, 15, 17], requestVersion: 7, resultVersion: 7, expanded: true },
} as const;
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
