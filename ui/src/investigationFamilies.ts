/** Exact public family identities; saved generations never inherit catalog upgrades. */
export const isCompletionSpecialist = (id: string) => /^(domain-analyst|evidence-auditor)@(7|8|9|10)$/.test(id) || id === 'spec-writer@1' || id === 'spec-writer@2' || id === 'gap-triager@1';
export const isExpandedSpecialist = (id: string) => /^(domain-analyst|evidence-auditor)@(8|9|10)$/.test(id);
const families = {
  4: { specialistIds: ['domain-analyst@7', 'evidence-auditor@7', 'spec-writer@1', 'spec-writer@2', 'gap-triager@1'], roleVersion: 7, requestVersion: 4, resultVersion: 5, expanded: false },
  5: { specialistIds: ['domain-analyst@8', 'evidence-auditor@8'], roleVersion: 8, requestVersion: 5, resultVersion: 5, expanded: true },
  6: { specialistIds: ['domain-analyst@9', 'evidence-auditor@9'], roleVersion: 9, requestVersion: 6, resultVersion: 6, expanded: true },
  7: { specialistIds: ['domain-analyst@10', 'evidence-auditor@10'], roleVersion: 10, requestVersion: 7, resultVersion: 7, expanded: true },
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
