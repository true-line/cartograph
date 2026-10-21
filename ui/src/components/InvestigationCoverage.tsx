import type { InvestigationCoverageAttestation, InvestigationCoverageContext } from '../investigationTypes';

export interface InvestigationCoverageProps {
  context: InvestigationCoverageContext | undefined;
  attestation: InvestigationCoverageAttestation | undefined;
}

/** Saved preparation-time qualification, never a lookup of current inventories. */
export function InvestigationCoverage({ context, attestation }: InvestigationCoverageProps) {
  return <section className="investigation-section" aria-label="Saved investigation coverage">
    <h3>Saved ingest coverage</h3>
    <p className="muted">This describes the recorded ingest when the investigation was prepared.
      It does not establish complete support or current freshness. Conclusions still need review.</p>
    {!context ? <p>Coverage is not prepared yet.</p> : <>
      <p>Coverage is not exhaustive.</p>
      <ul>{context.coverage.repos.map((repo) => <li key={repo.repo}>
        <strong>{repo.repo}</strong> · {repo.status === 'recorded' ? 'Recorded at preparation' :
          repo.status === 'stale' ? 'Stale at preparation' : 'Unrecorded at preparation'}
        {repo.status === 'recorded' && repo.counts && <>
          <p>{repo.limited ? 'Limited recorded coverage.' : 'No limitation recorded; complete support is not established.'}</p>
          <p>Recorded revision: <code>{repo.commit}</code></p>
          <p>Excluded producers: {repo.counts.not_selected} · Missing inputs: {repo.counts.missing_input}
            {' · '}Nothing to read: {repo.counts.not_applicable} · Unsupported capabilities: {repo.counts.unsupported}</p>
          <p>Recorded layers: {repo.layer_hints?.length ? repo.layer_hints.join(', ') : 'All layers selected'}</p>
        </>}
        {repo.status === 'stale' && <p>Registration or configuration no longer matched at preparation. Recorded counts are unavailable for this qualification.</p>}
        {repo.status === 'unrecorded' && <p>No ingest inventory was recorded for this repository. Missing coverage is not evidence of absent behavior.</p>}
        <p>Compared at preparation: {repo.compared.length ? repo.compared.map((check) =>
          check === 'registration' ? 'registration' : check === 'plugins' ? 'plugin configuration' : 'layer selection').join(', ') : 'No comparisons recorded'}</p>
      </li>)}</ul>
      <p className="muted">These counts do not disclose individual stopping reasons. Inventory reads do not count as source inspection.</p>
      {attestation && <p>The saved coverage qualification was available to the specialist when it finished.</p>}
      <details><summary>Exact saved coverage identity</summary>
        <p>Context <code>{context.context_id}</code></p>
        <p>Coverage <code>{context.coverage.coverage_id}</code></p>
        {attestation && <p>Final representation <code>{attestation.representation_hash}</code></p>}
        {context.coverage.repos.map((repo) => repo.inventory_hash && <p key={repo.repo}>
          {repo.repo} inventory <code>{repo.inventory_hash}</code>
        </p>)}
      </details>
    </>}
  </section>;
}
