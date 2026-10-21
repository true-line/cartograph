import { useState } from 'react';
import type {
  GraphEdge,
  GraphNode,
  SpecArtifact,
  SpecDiagramView,
} from '../store';
import {
  STRUCTURED_VIEW_UNAVAILABLE_NOTICE,
  SpecDiagramCanvas,
  verifyDiagramViewIdentity,
} from './SpecDiagramView';

export interface TopologyCardProps {
  /** Compiled topology + deployment diagram projection (US-0036, AC-0246). */
  view?: SpecDiagramView | null;
  /** Exported topology artifact for content-identity verification and copyable source (AC-0246). */
  artifact?: SpecArtifact | null;
  pageSize?: number;
  onSelectNode?: (node: GraphNode) => void;
  onSelectEdge?: (edge: GraphEdge) => void;
}

/** True when the artifact has actual resources (not just the header/fence lines). */
function hasMermaidContent(content: string | null): content is string {
  if (content === null) return false;
  const trimmed = content
    .replace(/^#\s+Resource topology\s*/i, '')
    .replace(/^```mermaid\s*/i, '')
    .replace(/```\s*$/, '')
    .trim();
  return trimmed.length > 0 && trimmed !== 'flowchart LR';
}

/**
 * Resource/topology and deployment view (SPEC-00 §7, US-0036). Renders the
 * compiled topology and T1 deployment roles as an interactive diagram bound to
 * the exported projection's content identity, with copyable Mermaid text taken
 * from the matching official `topology` artifact.
 */
export function TopologyCard({
  view,
  artifact,
  pageSize,
  onSelectNode,
  onSelectEdge,
}: TopologyCardProps) {
  const [copied, setCopied] = useState(false);

  const hasDiagram = Boolean(view && view.nodes.length > 0);
  const hasValidDiagram = Boolean(view && view.nodes.length > 0 && verifyDiagramViewIdentity(view, artifact));
  const matchingArtifact =
    artifact &&
    (artifact.id === 'topology' || artifact.file_name === 'topology.md') &&
    (!view || verifyDiagramViewIdentity(view, artifact))
      ? artifact
      : null;
  const source = matchingArtifact?.content ?? null;
  const hasMermaid = Boolean(matchingArtifact && (hasDiagram || hasMermaidContent(source)));

  return (
    <section className="card">
      <h2>Topology map</h2>
      {!hasDiagram && !hasMermaid ? (
        <p className="muted">No resources recovered yet — ingest a repo with Terraform.</p>
      ) : (
        <>
          {view && hasDiagram && (
            <SpecDiagramCanvas
              view={view}
              artifact={artifact}
              title="Resource & deployment topology"
              pageSize={pageSize}
              onSelectNode={onSelectNode}
              onSelectEdge={onSelectEdge}
            />
          )}

          {matchingArtifact && hasMermaid && (
            <>
              {!hasDiagram && (
                <p
                  className="muted spec-diagram-unavailable"
                  data-testid="diagram-view-unavailable"
                >
                  {STRUCTURED_VIEW_UNAVAILABLE_NOTICE}
                </p>
              )}
              <details className="topology-mermaid-details" open={!hasValidDiagram}>
                <summary>Copyable Mermaid source</summary>
                <pre className="evidence-source topology-mermaid" data-testid="topology-mermaid">
                  {matchingArtifact.content}
                </pre>
              </details>
              {matchingArtifact.content_truncated && (
                <p
                  className="muted spec-artifact-truncated-note"
                  data-testid="spec-artifact-truncated-note"
                >
                  Showing a preview — the complete artifact is{' '}
                  {matchingArtifact.content_byte_len.toLocaleString()} bytes. Use Copy artifact or
                  Export bundle for the complete text.
                </p>
              )}
              <p style={{ marginTop: '0.75rem' }}>
                <button
                  type="button"
                  onClick={() => {
                    void navigator.clipboard?.writeText(matchingArtifact.content).then(() => {
                      setCopied(true);
                      setTimeout(() => setCopied(false), 1500);
                    });
                  }}
                >
                  {copied ? 'Copied' : 'Copy Mermaid'}
                </button>
              </p>
            </>
          )}
        </>
      )}
    </section>
  );
}
