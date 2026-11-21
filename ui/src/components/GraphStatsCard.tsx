import { useState } from 'react';
import { createRepositoryLabeler } from '../repositoryLabels';
import type { GraphStats, SystemRepo } from '../store';

export interface GraphStatsCardProps {
  stats: GraphStats | null;
  /** Repos in the current system (#162) — the clear confirmation names
   * exactly what is being thrown away. */
  systemContents?: SystemRepo[];
  canClear: boolean;
  clearing: boolean;
  error: string | null;
  onClear: () => void;
  /** New project (#1994): remove everything tied to this system and keep
   * the application's settings. Absent where the host cannot do it. */
  newProject?: {
    can: boolean;
    starting: boolean;
    error: string | null;
    onStart: () => void;
  };
}

/** Node/edge counts plus a confirmed clear action, phrased in system terms
 * (#162): the user thinks in projects/systems, not storage layers. */
export function GraphStatsCard({
  stats,
  systemContents,
  canClear,
  clearing,
  error,
  onClear,
  newProject,
}: GraphStatsCardProps) {
  const [confirming, setConfirming] = useState(false);
  const [confirmingNew, setConfirmingNew] = useState(false);
  const repoLabel = createRepositoryLabeler(systemContents);

  return (
    <section className="card">
      <h2>System graph</h2>
      <div className="stat" data-testid="graph-node-count">
        {stats ? stats.nodes : '—'}
      </div>
      <div className="stat-label">nodes</div>
      <div className="stat" data-testid="graph-edge-count">
        {stats ? stats.edges : '—'}
      </div>
      <div className="stat-label">edges</div>
      {!confirming ? (
        <button
          className="clear-graph-button"
          type="button"
          disabled={!canClear || clearing}
          onClick={() => setConfirming(true)}
        >
          {clearing ? 'Clearing…' : 'Clear system'}
        </button>
      ) : (
        <div className="clear-confirmation" role="alert">
          <p>
            Remove every recovered fact
            {systemContents && systemContents.length > 0
              ? ` for ${systemContents.map((entry) => repoLabel(entry.repo)).join(', ')}`
              : ' in this system'}
            ? Job history and settings are kept.
          </p>
          <div className="clear-confirmation-actions">
            <button type="button" className="secondary-button" onClick={() => setConfirming(false)}>
              Keep system
            </button>
            <button
              type="button"
              className="danger-button"
              disabled={clearing}
              onClick={() => {
                setConfirming(false);
                onClear();
              }}
            >
              Confirm clear
            </button>
          </div>
        </div>
      )}
      {error && <p className="error-text">{error}</p>}
      {newProject &&
        (!confirmingNew ? (
          <button
            className="new-project-button secondary-button"
            type="button"
            disabled={!newProject.can || newProject.starting}
            onClick={() => setConfirmingNew(true)}
          >
            {newProject.starting ? 'Restarting…' : 'New project…'}
          </button>
        ) : (
          <div className="clear-confirmation" role="alert">
            <p>
              Start a new project? This sets aside
              {systemContents && systemContents.length > 0
                ? ` ${systemContents.map((entry) => repoLabel(entry.repo)).join(', ')}`
                : ' the loaded system'}{' '}
              with its recovered facts, findings, investigations and chats, section drafts,
              reviews, decisions, job history and retained source. Switch back to it from
              Projects at any time.
            </p>
            <p>
              Settings, provider keys, run limits, the local model and installed adapters are
              shared by every project. Cartograph restarts with an empty project.
            </p>
            <div className="clear-confirmation-actions">
              <button
                type="button"
                className="secondary-button"
                onClick={() => setConfirmingNew(false)}
              >
                Keep project
              </button>
              <button
                type="button"
                className="secondary-button"
                disabled={newProject.starting}
                onClick={() => {
                  setConfirmingNew(false);
                  newProject.onStart();
                }}
              >
                Start new project
              </button>
            </div>
          </div>
        ))}
      {newProject?.error && <p className="error-text">{newProject.error}</p>}
    </section>
  );
}
