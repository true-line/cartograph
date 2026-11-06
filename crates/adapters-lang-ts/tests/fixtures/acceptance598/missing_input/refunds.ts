export function evidence_refunds(requested: boolean): string {
  if (requested) {
    return `# Refund evidence
Revision: synthetic-r1
The service reads the refund limit from tenant-policy.json at startup.
That file is not included in this capture. The captured handler contains no fallback limit.
The current policy value and deployment environment are not recorded.
`;
  }
  return "";
}
