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
  /** Mermaid text from the spec compiler, or null with no backend. */
  mermaid: string | null;
  /** Compiled topology + deployment diagram projection (US-0036, AC-0246). */
  view?: SpecDiagramView | null;
  /** Exported topology artifact for content-identity verification (AC-0246). */
  artifact?: SpecArtifact | null;
  pageSize?: number;
  onSelectNode?: (node: GraphNode) => void;
  onSelectEdge?: (edge: GraphEdge) => void;
}

/** True when the artifact has actual resources (not just the header line). */
function hasMermaidContent(mermaid: string | null): mermaid is string {
  return mermaid !== null && mermaid.trim() !== 'flowchart LR';
}

/**
 * Resource/topology and deployment view (SPEC-00 §7, US-0036). Renders the
 * compiled topology and T1 deployment roles as an interactive diagram bound to
 * the exported projection's content identity, with copyable Mermaid text kept
 * as a secondary action.
 */
export function TopologyCard({
  mermaid,
  view,
  artifact,
  pageSize,
  onSelectNode,
  onSelectEdge,
}: TopologyCardProps) {
  const [copied, setCopied] = useState(false);

  const hasDiagram = Boolean(view && view.nodes.length > 0);
  const hasValidDiagram = Boolean(view && view.nodes.length > 0 && verifyDiagramViewIdentity(view, artifact));
  const hasMermaid = hasMermaidContent(mermaid);

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

          {hasMermaid && (
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
                  {mermaid}
                </pre>
              </details>
              <p style={{ marginTop: '0.75rem' }}>
                <button
                  type="button"
                  onClick={() => {
                    void navigator.clipboard?.writeText(mermaid).then(() => {
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
