import type { ProducingMethod, Tier } from '../store';

export const TIER_LABELS: Record<Tier, string> = {
  Confirmed: 'Confirmed',
  InferredStrong: 'Inferred (strong)',
  InferredWeak: 'Inferred (weak)',
  Gap: 'Gap',
};

/** Short badge text for a producing method (AC-0244). */
export function methodBadge(method: ProducingMethod | undefined): string {
  switch (method?.kind) {
    case 'rule':
      return 'rule-derived';
    case 'model':
      return 'model';
    case 'human':
      return 'human';
    default:
      // "producer", not "method": beside an Endpoint, "method" reads as the HTTP verb (#1601).
      return 'producer not recorded';
  }
}

/** Full method text: `rule name@version`, `model id`, `human actor` or `not recorded`. */
export function methodLabel(method: ProducingMethod | undefined): string {
  switch (method?.kind) {
    case 'rule':
      return `rule ${method.rule}@${method.version}`;
    case 'model':
      return `model ${method.model}`;
    case 'human':
      return `human ${method.actor}`;
    default:
      return 'not recorded';
  }
}

/**
 * Confidence-tier badge (R-INT-2): inferred content must never be visually
 * indistinguishable from confirmed. Colors come from
 * docs/design/production-ui/README.md (confidence-tier palette).
 *
 * Pass `provenance` to show the producing method beside the tier; it is a
 * separate label and never changes the tier shown (AC-0244).
 */
export function TierBadge({
  tier,
  provenance,
}: {
  tier: Tier;
  provenance?: { method?: ProducingMethod };
}) {
  const badge = <span className={`tier-badge tier-${tier.toLowerCase()}`}>{TIER_LABELS[tier]}</span>;
  if (!provenance) return badge;
  return (
    <span className="tier-badge-group">
      {badge}
      <span className="method-badge" title={methodLabel(provenance.method)}>
        {methodBadge(provenance.method)}
      </span>
    </span>
  );
}
