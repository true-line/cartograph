import { artifactSpan } from '../investigationArtifactReads';
import type { InvestigationArtifactReadView } from '../investigationTypes';

export interface InvestigationArtifactReadsProps {
  reads: InvestigationArtifactReadView[] | null;
  error: string | null;
}

/** Saved compiled-document pages the specialist read (AC-1816). Context the
 * model was shown, never citations, and not a reading of the document today. */
export function InvestigationArtifactReads({ reads, error }: InvestigationArtifactReadsProps) {
  return <section className="investigation-section" aria-label="Compiled documents read">
    <h3>Compiled documents read</h3>
    <p className="muted">Pages of Cartograph documents compiled from this run's frozen facts. They were context for the
      specialist, not citations, and they show what it was given then, not the document today.</p>
    {error ? <p className="error-text" role="alert">Saved document reads could not be loaded: {error}</p>
      : reads === null ? <p role="status">Loading saved document reads…</p>
      : reads.length === 0 ? <p>No compiled documents were read.</p>
      : <ol>{reads.map((read, index) => <li key={index}>
          <strong>{read.title}</strong> · {artifactSpan(read)}
          <details><summary>Exact saved page identity</summary>
            <p>Artifact <code>{read.artifact_id}</code></p>
            <p>Content <code>{read.content_id}</code> · {read.response_bytes.toLocaleString('en-US')} bytes supplied</p>
            <p>Supplied page <code>{read.response_hash}</code></p>
          </details>
        </li>)}</ol>}
  </section>;
}
