import type {
  ComputedCuratedRead, CuratedFindingListing, CuratedRecord, CuratedRecordState, CuratedRead,
} from '../store';
import { TierBadge } from './TierBadge';

export interface CuratedSectionProps {
  /** The host's curated read, returned with the bundle on screen; `null`
   *  when there is none. */
  read: CuratedRead | null;
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
          <td className="curated-state"><code>{record.state}</code></td>
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

const idList = (ids: string[]) => (ids.length === 0 ? '—' : ids.join(', '));

function FindingListingTable({ rows }: { rows: CuratedFindingListing[] }) {
  const caption = `Accepted findings that are not eligible (${rows.length})`;
  return (
    <div className="md-table-wrap" role="region" aria-label={caption} tabIndex={0}>
      <table className="curated-table">
        <caption>
          {caption}. These findings assert nothing and their statements are not shown. The list
          is the same in both export modes.
        </caption>
        <thead>
          <tr>
            <th scope="col">Finding</th>
            <th scope="col">Investigation</th>
            <th scope="col">Role</th>
            <th scope="col">State</th>
            <th scope="col">Reason</th>
            <th scope="col">Changed facts</th>
            <th scope="col">Missing facts</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.finding_ref}>
              <th scope="row">
                <code title={row.finding_ref}>{row.finding_id}</code>
              </th>
              <td><code>{row.investigation_id}</code></td>
              <td><code>{row.role}</code></td>
              <td className="curated-state"><code>{row.state}</code></td>
              <td>{row.reason_sentence} ({row.reason})</td>
              <td>{idList(row.changed)}</td>
              <td>{idList(row.missing)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * Findings accepted as curated (ADR-0074 decision 11, AC-0846). Every
 * state, reason sentence and count is the host's, and so is the split by
 * export mode: this draws what the read returned. Nothing is drawn for a
 * host that sends no finding fields.
 */
function CuratedFindings({ read }: { read: ComputedCuratedRead }) {
  const counts = read.finding_counts;
  if (!counts) return null;
  const shown = read.findings ?? [];
  const listed = read.findings_listed ?? [];
  return (
    <section className="curated-findings" aria-labelledby="curated-findings-title">
      <h4 id="curated-findings-title">Findings accepted as curated</h4>
      <p className="muted">
        Investigation findings a person accepted as curated. A finding is one statement an agent
        produced, with the facts it cites. It is T3 / InferredWeak and is not a recovered fact.
      </p>
      <dl className="curated-counts" aria-label="Curated finding counts">
        <div><dt>Accepted</dt><dd>{counts.accepted}</dd></div>
        <div><dt>Eligible</dt><dd>{counts.eligible}</dd></div>
        <div><dt>Stale</dt><dd>{counts.stale}</dd></div>
        <div><dt>Unresolvable</dt><dd>{counts.unresolvable}</dd></div>
      </dl>
      {counts.accepted === 0 && (
        <p className="muted" data-testid="curated-findings-empty">
          No finding is accepted as curated. Accept one in its investigation to list it here.
        </p>
      )}
      {read.export_mode === 'verified-only' && counts.accepted > 0 && (
        <p className="curated-notice" role="note" data-testid="curated-findings-omitted">
          No finding is shown: this is a <code>verified-only</code> read, and every finding is
          InferredWeak. Eligible findings left out: {read.findings_omitted ?? 0}.
        </p>
      )}
      {shown.length > 0 && (
        <ol className="curated-finding-list" aria-label={`Curated findings (${shown.length})`}>
          {shown.map((finding) => (
            <li key={finding.finding_ref}>
              <p>
                <strong>{finding.title}</strong>{' '}
                <span className="curated-tier">{finding.tier}</span>{' '}
                <TierBadge tier={finding.confidence} />
              </p>
              <p>{finding.statement}</p>
              <p className="muted">
                {finding.kind} · T3 / InferredWeak · role <code>{finding.role}</code> · human-reviewed: accepted as
                curated · source support: captured {finding.source_support.captured}, unverified{' '}
                {finding.source_support.unverified} · finding{' '}
                <code title={finding.finding_ref}>{finding.finding_id}</code> of{' '}
                <code>{finding.investigation_id}</code>
              </p>
              <p>
                Cites{' '}
                {finding.citations.map((id, index) => (
                  <span key={id}>{index > 0 && ', '}<code>{id}</code></span>
                ))}
              </p>
              {finding.limitations.length > 0 && (
                <ul>
                  {finding.limitations.map((item, index) => <li key={index}>{`Limitation: ${item}`}</li>)}
                </ul>
              )}
            </li>
          ))}
        </ol>
      )}
      {listed.length > 0 && <FindingListingTable rows={listed} />}
    </section>
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

          {read.export_mode === 'verified-only' && (
            <p className="curated-notice" role="note" data-testid="curated-omitted">
              No curated item is shown: this is a <code>verified-only</code> read, and every
              curated item is InferredWeak. Curated items left out: {read.items_omitted}. A{' '}
              <code>best-effort</code> export includes them, each marked.
            </p>
          )}

          <CuratedFindings read={read} />

          {/* Closed until asked for, so the records do not push the
              documents off the screen; the counts above stay in view. */}
          <details className="curated-records">
            <summary>
              Records and decisions: {read.items.length} curated {read.items.length === 1 ? 'item' : 'items'} shown,{' '}
              {read.listed.length} listed with a reason, {read.assertion_decisions.stale.length} Workbench{' '}
              {read.assertion_decisions.stale.length === 1 ? 'decision' : 'decisions'} matching nothing
            </summary>
          {read.items.length > 0 && (
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
                        <td className="curated-state"><code>{entry.state}</code></td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
          </details>
        </>
      )}
    </section>
  );
}
