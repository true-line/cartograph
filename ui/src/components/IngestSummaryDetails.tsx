import type { IngestSummary } from '../store';

export interface IngestSummaryDetailsProps {
  summary: IngestSummary;
  /** The display name of a repository id, as Workspace labels it; the id
   *  itself when omitted. */
  repoLabel?: (repo: string) => string;
}

/** `identity@revision` with its identity labeled. */
function labelRevision(revision: string, repoLabel: (repo: string) => string): string {
  const separator = revision.lastIndexOf('@');
  return separator < 0
    ? repoLabel(revision)
    : `${repoLabel(revision.slice(0, separator))}${revision.slice(separator)}`;
}

/** The last ingest's outcome on Workspace (#2170): what was cloned or
 *  listed, the totals, merged occurrences when any (AC-0196), the breakdown
 *  by source layer and a re-ingest's delta (US-0001). */
export function IngestSummaryDetails({
  summary,
  repoLabel = (repo) => repo,
}: IngestSummaryDetailsProps) {
  return (
    <details className="card ingest-details" aria-label="Last ingest">
      <summary>
        Last ingest: {summary.files} files → {summary.nodes} nodes, {summary.edges} edges
      </summary>
      <div className="ingest-summary">
        <p className="muted">
          {summary.repos && summary.repos.length > 0 && (
            <>
              <code data-testid="system-repos">
                {summary.repos.map((revision) => labelRevision(revision, repoLabel)).join(', ')}
              </code>{' '}
              —{' '}
            </>
          )}
          {summary.repo && summary.commit_sha && (
            <>
              <code data-testid="cloned-repo">
                {repoLabel(summary.repo)} @ {summary.commit_sha.slice(0, 12)}
              </code>{' '}
              —{' '}
            </>
          )}
          Job #{summary.job_id}: {summary.files} files → {summary.nodes} nodes,{' '}
          {summary.edges} edges.
        </p>
        {summary.merged &&
          (summary.merged.nodes > 0 ||
            summary.merged.edges > 0 ||
            summary.merged.node_collisions > 0) && (
            // AC-0196: collapsing repeated occurrences is stated, never silent.
            <p className="muted" data-testid="merged-summary">
              Merged: {summary.merged.edges} repeated relation sites ·{' '}
              {summary.merged.nodes} repeated node ids · {summary.merged.node_collisions}{' '}
              identity collisions (distinct declarations sharing one id)
            </p>
          )}
        <dl className="layer-summary" aria-label="Ingest breakdown by source layer">
          <div>
            <dt>TypeScript</dt>
            <dd data-testid="ts-layer-summary">
              {summary.layers.ts.files} files · {summary.layers.ts.nodes} nodes ·{' '}
              {summary.layers.ts.edges} edges
            </dd>
          </div>
          <div>
            <dt>Python</dt>
            <dd data-testid="python-layer-summary">
              {summary.layers.python.files} files · {summary.layers.python.nodes} nodes ·{' '}
              {summary.layers.python.edges} edges
            </dd>
          </div>
          <div>
            <dt>Go</dt>
            <dd data-testid="go-layer-summary">
              {summary.layers.go.files} files · {summary.layers.go.nodes} nodes ·{' '}
              {summary.layers.go.edges} edges
            </dd>
          </div>
          <div>
            <dt>Java</dt>
            <dd data-testid="java-layer-summary">
              {summary.layers.java.files} files · {summary.layers.java.nodes} nodes ·{' '}
              {summary.layers.java.edges} edges
            </dd>
          </div>
          <div>
            <dt>Kotlin</dt>
            <dd data-testid="kotlin-layer-summary">
              {summary.layers.kotlin.files} files · {summary.layers.kotlin.nodes} nodes ·{' '}
              {summary.layers.kotlin.edges} edges
            </dd>
          </div>
          {summary.layers.jvm_classes && summary.layers.jvm_classes.files > 0 ? (
            <div>
              <dt>JVM classes</dt>
              <dd data-testid="jvm-classes-layer-summary">
                {summary.layers.jvm_classes.files} files · {summary.layers.jvm_classes.nodes}{' '}
                nodes · {summary.layers.jvm_classes.edges} edges
              </dd>
            </div>
          ) : null}
          {summary.layers.js_bundles && summary.layers.js_bundles.files > 0 ? (
            <div>
              <dt>JS bundle sources</dt>
              <dd data-testid="js-bundles-layer-summary">
                {summary.layers.js_bundles.files} files · {summary.layers.js_bundles.nodes}{' '}
                nodes · {summary.layers.js_bundles.edges} edges
              </dd>
            </div>
          ) : null}
          <div>
            <dt>Terraform</dt>
            <dd data-testid="tf-layer-summary">
              {summary.layers.tf.files} files · {summary.layers.tf.nodes} nodes ·{' '}
              {summary.layers.tf.edges} edges
            </dd>
          </div>
          <div>
            <dt>WebExtension</dt>
            <dd data-testid="webext-layer-summary">
              {summary.layers.webext.files} manifests · {summary.layers.webext.nodes} nodes ·{' '}
              {summary.layers.webext.edges} edges
            </dd>
          </div>
          <div>
            <dt>Toolchain</dt>
            <dd data-testid="tools-layer-summary">
              {summary.layers.tools.files} configs · {summary.layers.tools.nodes} nodes ·{' '}
              {summary.layers.tools.edges} edges
            </dd>
          </div>
          {summary.layers.schedules && (
            <div>
              <dt>Schedules</dt>
              <dd data-testid="schedules-layer-summary">
                {summary.layers.schedules.files} files · {summary.layers.schedules.nodes} nodes ·{' '}
                {summary.layers.schedules.edges} edges
              </dd>
            </div>
          )}
          {summary.layers.pipelines && (
            <div>
              <dt>Pipelines</dt>
              <dd data-testid="pipelines-layer-summary">
                {summary.layers.pipelines.files} files · {summary.layers.pipelines.nodes} nodes ·{' '}
                {summary.layers.pipelines.edges} edges
              </dd>
            </div>
          )}
          {summary.layers.environments && (
            <div>
              <dt>Environments</dt>
              <dd data-testid="environments-layer-summary">
                {summary.layers.environments.files} files ·{' '}
                {summary.layers.environments.nodes} nodes · {summary.layers.environments.edges}{' '}
                edges
              </dd>
            </div>
          )}
          {summary.layers.events && (
            <div>
              <dt>Events and calls</dt>
              <dd data-testid="events-layer-summary">
                {summary.layers.events.files} files · {summary.layers.events.nodes} nodes ·{' '}
                {summary.layers.events.edges} edges
              </dd>
            </div>
          )}
        </dl>
        {summary.delta && (
          <p className="muted" data-testid="delta-summary">
            Delta: {summary.delta.recomputed_files} recomputed · {summary.delta.reused_files}{' '}
            reused · {summary.delta.deleted_files} removed
          </p>
        )}
      </div>
    </details>
  );
}
