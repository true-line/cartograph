import type {
  EvalResult,
  ExtractorCoverage,
  FindingsSummary,
  IngestRecord,
  TierDistribution,
} from '../store';

export interface ProvenanceSurfaceProps {
  /** Register headline — the same summary Workspace and Gaps quote. */
  findings: FindingsSummary | null;
  /** Tier counts from the shared atlas selector. */
  distribution: TierDistribution;
  /** Per-extractor coverage for the latest ingest (#119). */
  coverage: ExtractorCoverage[];
  /** Paired-eval calibration records, newest first. */
  evals: EvalResult[];
  /** Ingest history, newest first (#119). */
  history: IngestRecord[];
}

type Evaluation = NonNullable<EvalResult['evaluation']>;
const REASON_LABELS: Record<Evaluation['families'][number]['reason'], string> = {
  calibration_only: 'Calibration only',
  missing_evidence: 'Missing independent evidence',
  incompatible_evidence: 'Incompatible evidence',
  insufficient_evidence: 'Insufficient evidence',
  below_floor: 'Below precision floor',
  independently_evaluated: 'Evaluation complete',
};
function evaluationLabel(evaluation: EvalResult['evaluation']): string {
  if (evaluation?.mode === 'visible_mechanism_evaluation') return 'VISIBLE MECHANISM ONLY';
  if (evaluation?.mode === 'independent_evaluation') return 'INDEPENDENT EVALUATION';
  return 'CALIBRATION ONLY';
}
function familyStatus(evaluation: EvalResult['evaluation'], status: Evaluation['families'][number]['status']): string {
  if (status === 'abstained') return 'ABSTAINED';
  if (status === 'calibration_only') return 'CALIBRATION ONLY';
  return evaluation?.mode === 'visible_mechanism_evaluation' ? 'MECHANISM EVALUATED' : 'INDEPENDENTLY EVALUATED';
}

const TIER_SEGMENTS = [
  { key: 'confirmed', label: 'Confirmed', className: 'seg-confirmed' },
  { key: 'inferredStrong', label: 'Inferred Strong', className: 'seg-inferred-strong' },
  { key: 'inferredWeak', label: 'Inferred Weak', className: 'seg-inferred-weak' },
  { key: 'gap', label: 'Gap', className: 'seg-gap' },
  // Facts with no valid provenance are still facts — the chart must
  // account for every one of the N it claims to describe.
  { key: 'unattributed', label: 'Unattributed', className: 'seg-unattributed' },
] as const;

/** Determinism from displayed history: verified only when repeated
 *  repo/commit ingests exist AND every repeated group's hashes agree;
 *  a repeated group with divergent hashes is a violation, not noise. */
function determinismState(history: IngestRecord[]): 'verified' | 'violated' | 'unproven' {
  const byCommit = new Map<string, Set<string>>();
  for (const record of history) {
    const key = `${record.repo}@${record.commit_sha}`;
    const hashes = byCommit.get(key) ?? new Set<string>();
    hashes.add(record.content_hash);
    byCommit.set(key, hashes);
  }
  const repeats = [...byCommit.entries()].filter(
    ([key]) =>
      history.filter((record) => `${record.repo}@${record.commit_sha}` === key).length >= 2,
  );
  if (repeats.some(([, hashes]) => hashes.size > 1)) return 'violated';
  if (repeats.length > 0) return 'verified';
  return 'unproven';
}

function pct(part: number, total: number): number {
  return total === 0 ? 0 : (part / total) * 100;
}

/** Provenance & Eval (handoff screenshot 06): tier distribution, extractor
 *  coverage, calibration and evaluation evidence, and evidence health over re-ingests. Every
 *  count reads the shared selectors/commands — no chart invents numbers,
 *  and none of them encodes by color alone (each carries text + aria). */
export function ProvenanceSurface({
  findings,
  distribution,
  coverage,
  evals,
  history,
}: ProvenanceSurfaceProps) {
  const facts = distribution.total;
  const unsupported = findings?.unsupported ?? 0;
  // Oldest → newest so the history chart reads left to right in time.
  const timeline = [...history].slice(0, 8).reverse();
  const determinism = determinismState(timeline);

  return (
    <section className="prov-surface" aria-label="Provenance and evaluation">
      <header className="ingest-hero">
        <h2>Provenance &amp; Evaluation</h2>
        <p className="muted">
          Tier distribution, extractor coverage, calibration, evaluation evidence, and evidence health
          over time.
        </p>
      </header>

      <h3 className="settings-section-title" role="heading" aria-level={3}>
        Tier distribution · {facts} graph facts · {unsupported} unsupported patterns
      </h3>
      <div
        className="tier-bar"
        role="img"
        aria-label={`Tier distribution of ${facts} graph facts: ${distribution.confirmed} Confirmed, ${distribution.inferredStrong} Inferred Strong, ${distribution.inferredWeak} Inferred Weak, ${distribution.gap} Gap, ${distribution.unattributed} unattributed; plus ${unsupported} unsupported patterns in the register`}
      >
        {TIER_SEGMENTS.map((segment) => (
          <div
            key={segment.key}
            className={`tier-bar-seg ${segment.className}`}
            style={{ width: `${pct(distribution[segment.key], facts)}%` }}
          />
        ))}
      </div>
      <ul className="tier-legend">
        {TIER_SEGMENTS.map((segment) => (
          <li key={segment.key}>
            <span className={`legend-dot ${segment.className}`} aria-hidden="true" />
            {segment.label} <strong>{distribution[segment.key]}</strong>
          </li>
        ))}
        <li>
          <span className="legend-dot seg-unsupported" aria-hidden="true" />
          Unsupported <strong>{unsupported}</strong>
          <span className="muted"> (register finding, not a graph fact)</span>
        </li>
      </ul>

      <div className="prov-columns">
        <div>
          <h3 className="settings-section-title" role="heading" aria-level={3}>
            Extractor coverage
          </h3>
          {coverage.length === 0 ? (
            <p className="muted">No ingest recorded yet — coverage lands with the first run.</p>
          ) : (
            <ul className="coverage-rows">
              {coverage.map((row) => (
                <li key={row.extractor} className="coverage-row">
                  <code className="coverage-name">{row.extractor}</code>
                  <div
                    className="coverage-bar"
                    role="img"
                    aria-label={
                      row.coverage_pct === null
                        ? `${row.extractor}: ${row.facts} facts, scope not applicable this ingest`
                        : `${row.extractor}: ${row.facts} facts from ${row.files_with_facts} of ${row.files_in_scope} files (${Math.round(row.coverage_pct)}% coverage)`
                    }
                  >
                    {row.coverage_pct !== null && (
                      <div
                        className="coverage-bar-fill"
                        style={{ width: `${Math.min(row.coverage_pct, 100)}%` }}
                      />
                    )}
                  </div>
                  <span className="coverage-tail">
                    {row.coverage_pct === null ? 'n/a' : `${Math.round(row.coverage_pct)}%`} ·{' '}
                    {row.facts} facts
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div>
          <h3 className="settings-section-title" role="heading" aria-level={3}>
            Semantic calibration and evaluation
          </h3>
          {evals.length === 0 ? (
            <p className="muted">
              No calibration recorded — semantic proposals remain staged until evaluated.
            </p>
          ) : (
            <ul className="eval-cards">
              {evals.slice(0, 4).map((result) => (
                <li
                  key={result.id}
                  className="eval-card"
                >
                  <span className="eval-provider">
                    <code>{result.provider}</code>
                    <span className="eval-badge">{evaluationLabel(result.evaluation)}</span>
                  </span>
                  {(!result.evaluation || result.evaluation.mode === 'calibration_only') && <code className="eval-numbers">
                    Calibration P {result.precision.toFixed(2)} · R {result.recall.toFixed(2)} · floor{' '}
                    {result.precision_floor.toFixed(2)}
                  </code>}
                  <span className="muted">
                    {result.proposals} staged proposals · {result.approved} recorded admissions
                  </span>
                  {result.evaluation?.mode === 'visible_mechanism_evaluation' && (
                    <p className="muted">Visible public fixtures test the evaluation mechanism; they do not establish model quality.</p>
                  )}
                  {(!result.evaluation || result.evaluation.mode === 'calibration_only') && (
                    <p className="muted">Calibration measurements do not establish independent evaluation quality.</p>
                  )}
                  {result.evaluation && result.evaluation.families.length === 0 && (
                    <p className="muted">No proposal families assessed.</p>
                  )}
                  {result.evaluation && result.evaluation.families.length > 0 && (
                    <ul aria-label="Per-family evaluation">
                      {result.evaluation.families.map((family) => (
                        <li key={JSON.stringify(family.family)}>
                          <strong>{family.family.kind} · {family.family.subtype ?? 'no subtype'}</strong>
                          {' · '}{familyStatus(result.evaluation, family.status)}
                          {' · '}{REASON_LABELS[family.reason]}
                          <div>{family.staged} staged · {family.admitted} admitted · {family.ambiguous} ambiguous</div>
                          {family.metrics && (
                            <div>P {family.metrics.precision.toFixed(2)} · R {family.metrics.recall.toFixed(2)}
                              {' · '}{family.metrics.unknown} unknown
                              {' · '}{family.metrics.no_match_false_admissions} no-match false admissions
                              {family.metrics.retrieval_recall !== null && <> · retrieval R {family.metrics.retrieval_recall.toFixed(2)}</>}
                            </div>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>

      <h3 className="settings-section-title" role="heading" aria-level={3}>
        Evidence health over re-ingests
      </h3>
      {timeline.length === 0 ? (
        <p className="muted">No ingest history yet.</p>
      ) : (
        <>
          <div className="history-chart">
            {timeline.map((record) => {
              const max = Math.max(...timeline.map((entry) => entry.graph_facts), 1);
              return (
                <div
                  key={record.id}
                  className="history-col"
                  role="img"
                  aria-label={`Ingest ${record.id} of ${record.repo} at ${record.commit_sha}: ${record.graph_facts} facts — ${record.confirmed} Confirmed, ${record.inferred_strong} Inferred Strong, ${record.inferred_weak} Inferred Weak, ${record.gap} Gap; hash ${record.content_hash.slice(0, 8)}`}
                >
                  <div
                    className="history-stack"
                    style={{ height: `${pct(record.graph_facts, max)}%` }}
                  >
                    <div
                      className="tier-bar-seg seg-gap"
                      style={{ height: `${pct(record.gap, record.graph_facts)}%` }}
                    />
                    <div
                      className="tier-bar-seg seg-inferred-weak"
                      style={{ height: `${pct(record.inferred_weak, record.graph_facts)}%` }}
                    />
                    <div
                      className="tier-bar-seg seg-inferred-strong"
                      style={{ height: `${pct(record.inferred_strong, record.graph_facts)}%` }}
                    />
                    <div
                      className="tier-bar-seg seg-confirmed"
                      style={{ height: `${pct(record.confirmed, record.graph_facts)}%` }}
                    />
                  </div>
                  <code className="history-label">
                    #{record.id} @{record.commit_sha.slice(0, 7)}
                  </code>
                  <code className="history-hash" title={record.content_hash}>
                    {record.content_hash.slice(0, 8)}
                  </code>
                </div>
              );
            })}
          </div>
          {determinism === 'violated' ? (
            <p className="error-text prov-note" data-testid="determinism-note">
              Determinism violated in this history: a repeated ingest of the same commit produced
              a different content hash. Re-ingest should be byte-identical — investigate before
              trusting exports.
            </p>
          ) : (
            <p className="muted prov-note" data-testid="determinism-note">
              {determinism === 'verified'
                ? 'Determinism verified in this history: repeated ingests of the same commit carry identical content hashes.'
                : 'Re-ingesting the same commit yields an identical graph — equal content hashes in this history are the proof.'}
            </p>
          )}
        </>
      )}
    </section>
  );
}
