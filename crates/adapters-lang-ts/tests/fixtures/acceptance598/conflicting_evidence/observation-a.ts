export function evidence_observation_a(requested: boolean): string {
  if (requested) {
    return `# Deployment observation A
Deployment: D7
Observed time: 2026-10-01T12:00:00Z
Source: synthetic inspection A
Refund approval required: true.
No ordering or authority over other observations is recorded.
`;
  }
  return "";
}
