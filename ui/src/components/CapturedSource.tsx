import type { CapturedDescription, CapturedText } from '../primarySourceStore';

export interface CapturedSourceProps {
  description: CapturedDescription | null;
  text: CapturedText | null;
  loading: boolean;
  error: string | null;
  onRead: (index: number) => void;
  onRequire?: (index: number) => void;
  requirementBusy?: boolean;
  requirementError?: string | null;
}

/** Captured source has its own identity and never replaces a live source window. */
export function CapturedSource({ description, text, loading, error, onRead, onRequire, requirementBusy, requirementError }: CapturedSourceProps) {
  const [chosen, setChosen] = useState<{ receipt: string; index: number } | null>(null);
  const rangeIndex = chosen?.receipt === description?.receipt_id ? chosen?.index : undefined;
  return (
    <section className="captured-source" aria-label="Captured primary source">
      <h3>Captured primary source</h3>
      <p className="muted">Full input coverage and business interpretation are not established.</p>
      {error && <p role="status">{error}</p>}
      {!description && !loading && !error && <p className="muted">No retained primary-source binding is available for this fact.</p>}
      {description && <>
        <p><code>{description.repo_key}</code></p>
        <label>
          Cited range
          <select aria-label="Captured cited range" defaultValue="" key={description.receipt_id}
            disabled={loading || requirementBusy} onChange={(event) => {
              if (event.target.value !== '') {
                const index = Number(event.target.value);
                setChosen({ receipt: description.receipt_id, index });
                onRead(index);
              }
            }}>
            <option value="" disabled>Choose a captured range</option>
            {description.ranges.map((range) => <option key={range.index} value={range.index}>
              {range.index + 1}. {range.path} · bytes {range.byte_start}–{range.byte_end}
            </option>)}
          </select>
        </label>
        {onRequire && <>
          <button type="button" className="secondary-button"
            disabled={loading || requirementBusy || rangeIndex === undefined || !description.graph_snapshot_id}
            onClick={() => { if (rangeIndex !== undefined) onRequire(rangeIndex); }}>
            {requirementBusy ? 'Binding source requirement…' : 'Require this source in an investigation'}
          </button>
          <p className="muted">Add an explicit requirement to the draft. Viewing source does not count as specialist inspection.</p>
          {requirementError && <p role="alert">{requirementError}</p>}
        </>}
        <details><summary>Capture reference</summary><code>{description.receipt_id}</code></details>
      </>}
      {loading && <p role="status">Loading captured source…</p>}
      {text && description && text.receipt_id === description.receipt_id && <>
        <p>{text.path} · bytes {text.byte_start}–{text.byte_end}</p>
        <pre data-testid="captured-source-text">{text.text}</pre>
      </>}
    </section>
  );
}
import { useState } from 'react';
