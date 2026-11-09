import type { InvestigationObligation, InvestigationObligationChoices, InvestigationObligationEnvelope, InvestigationPolicy, InvestigationCatalog, SpecialistId } from './investigationTypes';

const hash = (value: unknown): value is string => typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
const context = (value: unknown) => typeof value === 'string' && /^context-v2:[0-9a-f]{64}$/.test(value);
const graph = (value: unknown) => typeof value === 'string' && /^context-v1:[0-9a-f]{64}$/.test(value);
const closed = (value: object, fields: string[]) => Object.keys(value).length === fields.length && Object.keys(value).every((key) => fields.includes(key));

/** Host validates content identities; these guards reject incompatible selection responses. */
export function validObligation(value: InvestigationObligation): boolean {
  if (!value || !closed(value, ['kind', 'obligation_id', 'target']) || !hash(value.obligation_id) || !value.target) return false;
  if (value.kind === 'source') return value.target.schema_version === 1 && hash(value.target.target_id) && graph(value.target.graph_snapshot_id);
  return value.kind === 'inventory' && closed(value.target, ['context_id', 'repo', 'inventory_hash']) && context(value.target.context_id) &&
    typeof value.target.repo === 'string' && value.target.repo.length > 0 && value.target.repo.length <= 256 && hash(value.target.inventory_hash);
}
export function validObligationChoices(value: InvestigationObligationChoices | null): value is InvestigationObligationChoices {
  return Boolean(value && closed(value, ['schema_version', 'graph_snapshot_id', 'context_id', 'inventories']) && value.schema_version === 1 &&
    graph(value.graph_snapshot_id) && context(value.context_id) && Array.isArray(value.inventories) && value.inventories.length <= 4096 &&
    new Set(value.inventories.map((item) => item.repo)).size === value.inventories.length && value.inventories.every((item) => {
      if (!item || typeof item.repo !== 'string' || !item.repo.length) return false;
      if (item.status === 'recorded') return closed(item, ['repo', 'status', 'obligation']) && item.obligation?.kind === 'inventory' &&
        validObligation(item.obligation) && item.obligation.target.context_id === value.context_id && item.obligation.target.repo === item.repo;
      return (item.status === 'stale' || item.status === 'unrecorded') && closed(item, ['repo', 'status']);
    }));
}
export function obligationEnvelope(values: InvestigationObligation[], policy?: Pick<InvestigationPolicy, 'envelope_schema_version' | 'envelope_contract' | 'max_completion_obligations' | 'envelope_bytes'>): InvestigationObligationEnvelope {
  return { schema_version: policy?.envelope_schema_version ?? 1, contract: (policy?.envelope_contract ?? 'investigation-completion-obligations@1') as InvestigationObligationEnvelope['contract'], obligations: [...values].sort((a, b) =>
    a.kind.localeCompare(b.kind) || a.obligation_id.localeCompare(b.obligation_id)) };
}
export function validObligationEnvelope(value: InvestigationObligationEnvelope, policy?: Pick<InvestigationPolicy, 'envelope_schema_version' | 'envelope_contract' | 'max_completion_obligations' | 'envelope_bytes'>): boolean {
  return Boolean(value && closed(value, ['schema_version', 'contract', 'obligations']) && value.schema_version === (policy?.envelope_schema_version ?? 1) &&
    value.contract === (policy?.envelope_contract ?? 'investigation-completion-obligations@1') && Array.isArray(value.obligations) && value.obligations.length >= 1 &&
    value.obligations.length <= (policy?.max_completion_obligations ?? 12) && new TextEncoder().encode(JSON.stringify(value)).length <= (policy?.envelope_bytes ?? 8192) && value.obligations.every(validObligation) &&
    new Set(value.obligations.map((item) => `${item.kind}:${item.obligation_id}`)).size === value.obligations.length &&
    value.obligations.every((item, index) => index === 0 ||
      value.obligations[index - 1].kind < item.kind || (value.obligations[index - 1].kind === item.kind &&
        value.obligations[index - 1].obligation_id < item.obligation_id)));
}

/** Only the host's advertised compatible policies can enable a new role. */
export function investigationPolicy(catalog: InvestigationCatalog | null, profile: string, specialist: SpecialistId): (Omit<InvestigationPolicy, 'limits'> & { limits?: InvestigationPolicy['limits'] }) | undefined {
  const advertised = catalog?.policies?.find((policy) => policy.limits.profile === profile && policy.specialist_ids.includes(specialist));
  if (advertised) {
    const admission = specialist.endsWith('@10') || specialist.endsWith('@11') || specialist === 'domain-analyst@12' || specialist === 'domain-analyst@18' || specialist === 'domain-analyst@19' || specialist === 'domain-analyst@20' || specialist === 'domain-analyst@27' || specialist === 'domain-analyst@28';
    const progressive = specialist.endsWith('@9');
    const modern = specialist.endsWith('@8') || progressive || admission;
    if (advertised.request_schema_version !== (admission ? 7 : progressive ? 6 : modern ? 5 : 4) || advertised.envelope_schema_version !== (modern ? 2 : 1) ||
        advertised.envelope_contract !== `investigation-completion-obligations@${modern ? 2 : 1}` ||
        (modern && profile !== 'investigation-v2') || (!modern && profile !== 'investigation-v1') ||
        ![advertised.max_completion_obligations, advertised.request_bytes, advertised.envelope_bytes].every((value) => Number.isSafeInteger(value) && value > 0)) return undefined;
    return advertised;
  }
  if (specialist.endsWith('@8') || specialist.endsWith('@9') || specialist.endsWith('@10') || specialist.endsWith('@11') || specialist === 'domain-analyst@12' || specialist === 'domain-analyst@18' || specialist === 'domain-analyst@19' || specialist === 'domain-analyst@20' || specialist === 'domain-analyst@27' || specialist === 'domain-analyst@28' || profile !== 'investigation-v1') return undefined;
  return { limits: catalog?.limits, request_schema_version: 4, envelope_schema_version: 1,
    envelope_contract: 'investigation-completion-obligations@1', max_completion_obligations: 12,
    request_bytes: 8192, envelope_bytes: 8192, specialist_ids: [specialist] };
}
