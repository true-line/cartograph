import { useEffect, useRef, useState } from 'react';
import { ManifestGuide, type ManifestGuideProps } from './ManifestGuide';
import { dropSetStatus, type DropItem, type DropStatus } from '../dropIntake';
import type { IngestSource } from '../store';

export interface ConnectSurfaceProps {
  manifestGuide?: Omit<ManifestGuideProps, "onSaved">;
  source: IngestSource;
  target: string;
  /** Disabled when there is no live backend to preflight against. */
  canPreflight: boolean;
  onSourceChange: (source: IngestSource) => void;
  onTargetChange: (target: string) => void;
  onBack: () => void;
  onPreflight: () => void;
  /** Dropped folders and their outcomes (#587); null before any drop. */
  drop?: DropItem[] | null;
  /** True while folders are dragged over the window. */
  dropActive?: boolean;
  onDismissDrop?: () => void;
}

const STATUS_LABEL: Record<DropStatus, string> = {
  rejected: 'Rejected',
  queued: 'Queued',
  ingesting: 'Ingesting',
  ingested: 'Ingested',
  failed: 'Failed',
};

/** Per-folder intake outcomes (AC-0237): every dropped path says whether it
 *  was accepted, rejected or failed, and the set never claims completion
 *  while anything is unresolved. */
function DropOutcomes({ items, onDismiss }: { items: DropItem[]; onDismiss?: () => void }) {
  const status = dropSetStatus(items);
  return (
    <section className="drop-outcomes" aria-label="Dropped folders" data-testid="drop-outcomes">
      <header>
        <strong data-testid="drop-set-status">{status.label}</strong>
        {onDismiss && status.unresolved === 0 ? (
          <button type="button" className="secondary-button" onClick={onDismiss}>
            Dismiss
          </button>
        ) : null}
      </header>
      <ul>
        {items.map((item, index) => (
          <li key={`${index}:${item.path}`} className={`drop-item drop-${item.status}`} data-testid="drop-item">
            <span className={`drop-status drop-status-${item.status}`}>{STATUS_LABEL[item.status]}</span>
            <code className="drop-path">{item.path}</code>
            {item.source ? (
              <span className="muted">{item.source === 'manifest' ? 'system manifest' : 'local folder'}</span>
            ) : null}
            {item.repos.length ? (
              <span className="drop-repos" data-testid="drop-repos">
                {item.repos.join(' · ')}
              </span>
            ) : null}
            {item.reason ? <span className="drop-reason">{item.reason}</span> : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

const SOURCES: { id: IngestSource; label: string; icon: string; placeholder: string }[] = [
  {
    id: 'github',
    label: 'GitHub',
    icon: 'code',
    placeholder: 'https://github.com/owner/repo',
  },
  {
    id: 'local',
    label: 'Local folder',
    icon: 'folder',
    placeholder: '/path/to/project',
  },
  {
    id: 'manifest',
    label: 'System manifest',
    icon: 'description',
    placeholder: '/path/to/cartograph.system.toml',
  },
];

/** Step 1 of the ingest flow (handoff §Connect): pick a target. The local-only
 *  reassurance strip states the egress contract before any work starts. */
export function ConnectSurface({
  source,
  manifestGuide,
  target,
  canPreflight,
  onSourceChange,
  onTargetChange,
  onBack,
  onPreflight,
  drop = null,
  dropActive = false,
  onDismissDrop,
}: ConnectSurfaceProps) {
  const [creating, setCreating] = useState(false);
  const active = SOURCES.find((s) => s.id === source) ?? SOURCES[0];
  const input = useRef<HTMLInputElement>(null);
  // Re-opening Connect keeps the last target (#246): focus it with the whole
  // value selected, so typing a new path replaces it instead of appending.
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  const canSubmit = canPreflight && target.trim() !== '';
  return (
    <form
      className="ingest-flow"
      aria-label="Connect a target"
      onSubmit={(e) => {
        // Enter in the target field submits (#246); a disabled Preflight
        // stays disabled for Enter too.
        e.preventDefault();
        if (canSubmit && !creating) onPreflight();
      }}
    >
      <header className="ingest-hero">
        <h2>Connect a target</h2>
        <p className="muted">
          Point Cartograph at a system. It recovers structure, flows, and a provenance-tagged
          spec — everything runs on-device.
        </p>
      </header>
      <div className="source-picker" role="radiogroup" aria-label="Source kind">
        {SOURCES.map((s) => (
          <button
            key={s.id}
            type="button"
            role="radio"
            aria-checked={s.id === source}
            className={`source-option${s.id === source ? ' active' : ''}`}
            onClick={() => { setCreating(false); onSourceChange(s.id); }}
          >
            <span className="material-symbols-outlined" aria-hidden="true">
              {s.icon}
            </span>
            {s.label}
          </button>
        ))}
      </div>
      <label className="target-field">
        <span className="target-label">Target</span>
        <input
          ref={input}
          type="text"
          value={target}
          placeholder={active.placeholder}
          onChange={(e) => onTargetChange(e.target.value)}
        />
      </label>
      {source === 'manifest' && manifestGuide && canPreflight && <>
        <button type="button" onClick={() => setCreating(!creating)}>{creating ? 'Close manifest guide' : 'Create new manifest'}</button>
        {creating && <ManifestGuide {...manifestGuide} onSaved={onTargetChange} />}
      </>}
      <p
        className={`drop-hint${dropActive ? ' drop-hint-active' : ''}`}
        data-testid="drop-hint"
      >
        <span className="material-symbols-outlined" aria-hidden="true">
          drive_folder_upload
        </span>
        {canPreflight
          ? 'Or drop folders here. Each ingests like a typed local path, or as a system when it holds a cartograph.system.toml.'
          : 'Dropping folders needs the desktop app.'}
      </p>
      {drop?.length ? <DropOutcomes items={drop} onDismiss={onDismissDrop} /> : null}
      <p className="reassure-strip">
        <span className="material-symbols-outlined" aria-hidden="true">
          verified_user
        </span>
        Local-only preflight. Nothing leaves the device unless you opt a tier into cloud in
        Settings.
      </p>
      <footer className="flow-actions">
        <button type="button" className="secondary-button" onClick={onBack}>
          Back
        </button>
        <button type="submit" disabled={!canSubmit || creating}>
          <span className="material-symbols-outlined" aria-hidden="true">
            arrow_forward
          </span>
          Preflight
        </button>
      </footer>
    </form>
  );
}
