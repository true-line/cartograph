export function evidence_deployment_history(requested: boolean): string {
  if (requested) {
    return `# Historical deployment evidence
Capture time: 2026-10-01T12:00:00Z
Deployment D7 at revision synthetic-r1 used CourierAlpha.
Deployment record at 2026-10-02T12:00:00Z identifies revision synthetic-r2.
No routing observation or configuration for synthetic-r2 is available in this capture.
The synthetic-r1 routing observation is historical and is not a current-r2 assertion.
`;
  }
  return "";
}
