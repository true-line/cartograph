import { useState } from 'react';
import { REVIEW_LABEL, contributorLine, reviewActions, type AttestedView, type ReviewVerb } from '../attested';

export interface AttestedRecordsProps {
  /** The records to show: all of them, or those answering one Gap. */
  records: AttestedView[];
  busy?: boolean;
  error?: string | null;
  /** Hidden when absent: the engineer's confirm, dispute and withdraw. */
  onReview?: (view: AttestedView, verb: ReviewVerb) => void;
  /** Shown when there is no record. */
  empty?: string;
  /** A review needs the project's engineer, and there is none yet. */
  needsEngineer?: boolean;
  onSetEngineer?: (displayName: string) => void;
}

function EngineerForm({ busy, onSetEngineer }: { busy: boolean; onSetEngineer: (displayName: string) => void }) {
  const [name, setName] = useState('');
  return (
    <form
      className="attested-engineer"
      onSubmit={(event) => {
        event.preventDefault();
        if (name.trim()) onSetEngineer(name.trim());
      }}
    >
      <label>
        Your name, as this project's engineer. It stays in this project's data.
        <input value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <button type="submit" disabled={busy || !name.trim()}>Save</button>
    </form>
  );
}

function targetText(view: AttestedView): string {
  return view.target.kind === 'gap'
    ? `Answers ${view.target.gap_id} (${view.target.slot}); the Gap stays open`
    : `${view.target.address.document} §${view.target.address.section}`;
}

/** Attested records (ADR-0090, AC-1081). They are what an engineer or an
 *  agent states, not what was recovered from the system, so they never take
 *  a tier badge: each carries its own label and names its contributors. */
export function AttestedRecords({
  records, busy = false, error = null, onReview, empty = 'No attested records.', needsEngineer = false, onSetEngineer,
}: AttestedRecordsProps) {
  return (
    <section className="attested-records" aria-label="Attested records">
      {needsEngineer && onSetEngineer ? (
        <EngineerForm busy={busy} onSetEngineer={onSetEngineer} />
      ) : (
        error && <p className="attested-error" role="alert">{error}</p>
      )}
      {records.length === 0 ? (
        <p className="attested-empty">{empty}</p>
      ) : (
        <ul className="attested-rows">
          {records.map((view) => (
            <li key={view.record_id} className={`attested-row attested-${view.standing}`} data-testid="attested-row">
              <span className="attested-label" data-testid="attested-label">{view.label}</span>
              <code className="attested-id">{view.citation_id}</code>
              <p className="attested-statement">{view.statement}</p>
              <p className="attested-target">{targetText(view)}</p>
              <p className="attested-contributors" data-testid="attested-contributors">{contributorLine(view)}</p>
              {onReview && (
                <span className="attested-actions">
                  {reviewActions(view).map((verb) => (
                    <button key={verb} type="button" disabled={busy} onClick={() => onReview(view, verb)}>
                      {REVIEW_LABEL[verb]}
                    </button>
                  ))}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
