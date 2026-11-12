import type { CuratedRecord, CuratedRecordState, CuratedSectionRead } from '../store';
import { TierBadge } from './TierBadge';

export interface CuratedSectionProps {
  /** The host's curated read for the bundle on screen; `null` when it was
   *  not read. */
  read: CuratedSectionRead | null;
  busy?: boolean;
  onRefresh?: () => void;
}

/** A readable prefix of a revision or hash; the full value stays in the
 *  element's title and in "Full identifiers". */
export function shortIdentity(value: string): string {
  const at = value.lastIndexOf(':');
  return at < 0 ? value.slice(0, 12) : `${value.slice(0, at)}:${value.slice(at + 1, at + 13)}`;
}

/** The order the host assigns states in (ADR-0061 decision 4). */
const LISTED_STATES: CuratedRecordState[] = [
  'unverified', 'already-covered', 'unresolvable', 'stale', 'conflicted',
];

function RecordRows({ records, last }: {
  records: CuratedRecord[];
  last: (record: CuratedRecord) => string;
}) {
  return (
    <>
      {records.map((record) => (
        <tr key={record.record_id}>
          <th scope="row"><code>{record.record_id}</code></th>
          <td>
            <code>{record.source_id}</code> —{record.edge_label}→ <code>{record.target_id}</code>
          </td>
          <td><code>{record.gap_id}</code></td>
          <td>
            {/* R-INT-2: the tier and confidence the record was produced
                with, in words and with the shared badge. */}
            <span className="curated-tier">{record.tier}</span>{' '}
            <TierBadge tier={record.confidence} />
          </td>
          <td>{record.review_status}</td>
          <td>captured {record.source_support.captured}, unverified {record.source_support.unverified}</td>
          <td>{record.basis_summary}</td>
          <td><code>{record.state}</code></td>
          <td>{last(record)}</td>
        </tr>
      ))}
    </>
  );
}

function RecordTable({ caption, lastHeading, records, last }: {
  caption: string;
  lastHeading: string;
  records: CuratedRecord[];
  last: (record: CuratedRecord) => string;
}) {
  return (
    <div className="md-table-wrap" role="region" aria-label={caption} tabIndex={0}>
      <table className="curated-table">
        <caption>{caption}</caption>
        <thead>
          <tr>
            <th scope="col">Record</th>
            <th scope="col">Proposed edge</th>
            <th scope="col">Gap</th>
            <th scope="col">Tier and confidence</th>
            <th scope="col">Review</th>
            <th scope="col">Source support</th>
            <th scope="col">Basis</th>
            <th scope="col">State</th>
            <th scope="col">{lastHeading}</th>
          </tr>
        </thead>
        <tbody>
          <RecordRows records={records} last={last} />
        </tbody>
      </table>
    </div>
  );
}

/**
 * The curated projection in the Spec Workbench (SPEC-12 slice 3, ADR-0061,
 * AC-0655). Presentational: every state, reason sentence, count and the
 * revision is the host's. Curated items are InferredWeak proposals shown
 * beside the recovered facts, never as one of them (R-INT-2).
 */
export function CuratedSection({ read, busy, onRefresh }: CuratedSectionProps) {
  return (
    <section className="curated-section" aria-labelledby="curated-section-title">
      <header className="curated-section-header">
        <div>
          <h3 id="curated-section-title">Curated items</h3>
          <p className="muted">
            Accepted agent proposals, as the host reads them now. A curated item is shown beside
            the recovered facts and is not one of them: nothing is written to the graph and no
            Gap is resolved by it.
          </p>
        </div>
        {onRefresh && (
          <button type="button" className="secondary-button" disabled={busy} onClick={onRefresh}>
            Refresh curated items
          </button>
        )}
      </header>

      {read === null && (
        <p className="muted" data-testid="curated-unread">
          The curated projection has not been read. Nothing is stated here about accepted
          proposals.
        </p>
      )}

      {read?.status === 'changed_while_reading' && (
        <p className="curated-notice" role="status" data-testid="curated-changed">
          A review or the graph changed while the curated items were read, so they are not
          shown beside a bundle they do not describe. Refresh to read both again.
        </p>
      )}

      {read?.status === 'not_computed' && (
        <div className="curated-notice" role="status" data-testid="curated-not-computed">
          <p>
            <strong>The curated projection was not computed.</strong> No accepted proposal is
            shown here, neither as a curated item nor in a list. That does not mean there are
            none. Every other document is unaffected.
          </p>
          <p>
            Reason: {read.sentence} (<code>{read.reason}</code>)
          </p>
        </div>
      )}

      {read?.status === 'computed' && (
        <>
          <dl className="curated-identity">
            <div><dt>View</dt><dd><code>{read.view}</code></dd></div>
            <div>
              <dt>Curated revision</dt>
              <dd>
                <code title={read.revision} data-testid="curated-revision">
                  {shortIdentity(read.revision)}
                </code>
              </dd>
            </div>
            <div>
              <dt>Graph basis</dt>
              <dd><code title={read.graph_basis}>{shortIdentity(read.graph_basis)}</code></dd>
            </div>
            <div><dt>Export mode</dt><dd><code>{read.export_mode}</code></dd></div>
          </dl>
          <details className="curated-full-identity">
            <summary>Full identifiers</summary>
            <p>Curated revision: <code>{read.revision}</code></p>
            <p>Graph basis: <code>{read.graph_basis}</code></p>
            <p>Decision digest: <code>{read.assertion_decisions.digest}</code></p>
          </details>

          <dl className="curated-counts" aria-label="Curated record counts">
            <div><dt>Accepted</dt><dd>{read.counts.accepted}</dd></div>
            <div><dt>Eligible (curated items)</dt><dd>{read.counts.eligible}</dd></div>
            <div><dt>Unverified</dt><dd>{read.counts.unverified}</dd></div>
            <div><dt>Already covered</dt><dd>{read.counts.already_covered}</dd></div>
            <div><dt>Unresolvable</dt><dd>{read.counts.unresolvable}</dd></div>
            <div><dt>Stale</dt><dd>{read.counts.stale}</dd></div>
            <div><dt>Conflicted</dt><dd>{read.counts.conflicted}</dd></div>
          </dl>

          {read.counts.accepted === 0 && (
            <p className="muted" data-testid="curated-empty">
              No accepted proposal is recorded. There is no curated item and nothing is listed.
            </p>
          )}

          {read.export_mode === 'verified-only' ? (
            <p className="curated-notice" role="note" data-testid="curated-omitted">
              No curated item is shown: this is a <code>verified-only</code> read, and every
              curated item is InferredWeak. Curated items left out: {read.items_omitted}. A{' '}
              <code>best-effort</code> export includes them, each marked.
            </p>
          ) : read.items.length > 0 && (
            <RecordTable
              caption={`Curated items (${read.items.length})`}
              lastHeading="Note"
              records={read.items}
              last={() => 'InferredWeak curated item, not a recovered fact; a verified-only export omits it'}
            />
          )}

          {read.listed.length > 0 && (
            <section aria-labelledby="curated-listed-title">
              <h4 id="curated-listed-title">Accepted records that are not curated items</h4>
              <p className="muted">
                These records assert nothing. Each has one state and the reason for it, and they
                are the same in both export modes.
              </p>
              {LISTED_STATES.map((state) => {
                const records = read.listed.filter((record) => record.state === state);
                return records.length === 0 ? null : (
                  <RecordTable
                    key={state}
                    caption={`State ${state} (${records.length})`}
                    lastHeading="Reason"
                    records={records}
                    last={(record) => `${record.reason_sentence} (${record.reason})`}
                  />
                );
              })}
            </section>
          )}

          <section aria-labelledby="curated-decisions-title">
            <h4 id="curated-decisions-title">Workbench decisions</h4>
            <p className="muted">
              Rejections applied: {read.assertion_decisions.rejections_applied}. Decisions that
              match no current statement: {read.assertion_decisions.stale.length}.
            </p>
            {read.assertion_decisions.stale.length > 0 && (
              <div
                className="md-table-wrap"
                role="region"
                aria-label="Workbench decisions that match no current statement"
                tabIndex={0}
              >
                <table className="curated-table">
                  <caption>
                    Decisions that match no current statement. No statement is produced with
                    that content hash now, so the decision has no effect.
                  </caption>
                  <thead>
                    <tr>
                      <th scope="col">Content hash</th>
                      <th scope="col">Decision</th>
                      <th scope="col">State</th>
                    </tr>
                  </thead>
                  <tbody>
                    {read.assertion_decisions.stale.map((entry) => (
                      <tr key={entry.content_hash}>
                        <th scope="row">
                          <code title={entry.content_hash}>{shortIdentity(entry.content_hash)}</code>
                        </th>
                        <td>{entry.decision}</td>
                        <td><code>{entry.state}</code></td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </>
      )}
    </section>
  );
}
