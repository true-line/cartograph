export function evidence_observation_b(requested: boolean): string {
  if (requested) {
    return `# Deployment observation B
Deployment: D7
Observed time: 2026-10-01T12:00:00Z
Source: synthetic inspection B
Refund approval required: false.
No ordering or authority over other observations is recorded.
`;
  }
  return "";
}
