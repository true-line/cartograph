export function evidence_routing(requested: boolean): string {
  if (requested) {
    return `# Routing evidence
Revision: synthetic-r1
Order-submitted events use delivery route A or delivery route B according to the tenant routing flag.
Route A targets CourierAlpha. Route B targets CourierBeta.
The value of tenant T17 routing flag is not present in this capture.
`;
  }
  return "";
}
