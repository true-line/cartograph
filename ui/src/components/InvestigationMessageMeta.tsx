import type { InvestigationCitation } from '../investigationTypes';
import { TierBadge } from './TierBadge';

/** Presentation of already validated saved metadata, never a source read. */
export function InvestigationMessageMeta({ citationIds, citations }: {
  citationIds?: string[];
  citations?: InvestigationCitation[];
}) {
  const ids = new Set(citationIds);
  const records = citations?.filter((citation) => ids.has(citation.citation_id));
  const complete = citationIds !== undefined && records !== undefined && records.length === ids.size &&
    new Set(records.map((citation) => citation.citation_id)).size === ids.size && records.every((citation) =>
      citation.origin?.kind === 'graph_metadata' ? citation.source === null :
        (citation.origin?.kind === 'captured_primary_source' || citation.origin?.kind === 'working_tree_unverified') && citation.source != null);
  const graphCount = records?.filter((citation) => citation.origin?.kind === 'graph_metadata').length ?? 0;
  return <p className="muted" role="group" aria-label="Specialist message evidence">
    T3 / <TierBadge tier="InferredWeak" /> · <span className="material-symbols-outlined" aria-hidden="true">link</span>{' '}
    {complete ? `${ids.size - graphCount} source citations · ${graphCount} graph citations` : 'Citation metadata unavailable'}
  </p>;
}
