import { REPOSITORY_URL } from '../helpNotes';
import { CoverageMatrix } from './CoverageMatrix';
import type {
  AdapterLookup,
  AdapterSource,
  IngestSource,
  PatternFinding,
  PreflightProgress,
  PreflightReport,
  SystemPreflight,
} from '../store';

export interface PreflightSurfaceProps {
  source: IngestSource;
  target: string;
  /** Local detection output; null while busy or for remote targets. */
  report: PreflightReport | null;
  /** A system manifest's per-member preview (#1655); null otherwise. */
  system?: SystemPreflight | null;
  busy: boolean;
  /** Live progress of the running scan (#235); null before the first ping. */
  progress?: PreflightProgress | null;
  error: string | null;
  /** Disabled when there is no live backend to recover with. */
  canRecover: boolean;
  onBack: () => void;
  /** Stop the running scan; offered only while busy. */
  onCancel: () => void;
  onRunRecovery: () => void;
}

/** Request-adapter lane (#201): a prefilled tracker issue names the
 *  adapter — the unsupported finding's resolution path, not a dead end. */
function requestAdapterUrl(language: string): string {
  const title = encodeURIComponent(`Adapter request: ${language}`);
  return `${REPOSITORY_URL}/issues/new?title=${title}&labels=adapter-request`;
}

function sourceName(source: AdapterSource): string {
  return source.kind === 'builtin' ? source.adapter : `plugin ${source.id}`;
}

/** Adapter lookup (#1101, epic #998): for each unsupported or partial cell,
 *  which installed source declares it. Only a verified claim covers a cell;
 *  a declared but ungated plugin and an undeclared cell are said plainly. */
function AdapterLookupList({ lookups }: { lookups: AdapterLookup[] }) {
  return (
    <ul className="preflight-items adapter-lookup">
      {lookups.map((lookup) => {
        const { language, framework, capability } = lookup.cell;
        return (
          <li key={`${language}.${framework}.${capability}`}>
            <code>
              {language} · {framework} · {capability}
            </code>{' '}
            {lookup.status === 'covered_by' ? (
              <>
                <span aria-hidden="true">✓</span> covered by{' '}
                {lookup.sources.map(sourceName).join(', ')}
              </>
            ) : lookup.status === 'declared_unverified' ? (
              <>
                <span aria-hidden="true">~</span> declared by{' '}
                {lookup.sources.map(sourceName).join(', ')}, conformance gate not passed — run it
                in Settings
              </>
            ) : (
              <>
                <span aria-hidden="true">✗</span> no installed adapter declares this cell —
                after recovery, an adapter work packet can be drafted for it
              </>
            )}
          </li>
        );
      })}
    </ul>
  );
}

function FindingList({ items, marker }: { items: PatternFinding[]; marker: string }) {
  return (
    <ul className="preflight-items">
      {items.map((finding) => (
        <li key={`${finding.path}:${finding.line}:${finding.kind}`}>
          <span aria-hidden="true">{marker}</span> {finding.message}{' '}
          <code>
            {finding.path}:{finding.line}
          </code>
          {finding.request_adapter && (
            <>
              {' '}
              <a
                href={requestAdapterUrl(finding.request_adapter)}
                target="_blank"
                rel="noreferrer"
                className="adapter-request-link"
              >
                Request this adapter
              </a>
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

/** One repository's detection result: the three-way classification and
 *  coverage cards, shared by a local target and each manifest member. */
function ReportCards({ report }: { report: PreflightReport }) {
  return (
    <div className="preflight-cards">
      <div className="preflight-card">
        <h3>
          <span className="material-symbols-outlined ok" aria-hidden="true">
            terminal
          </span>
          Languages
        </h3>
        {report.languages.length === 0 ? (
          <p className="muted">No source files detected.</p>
        ) : (
          <ul className="preflight-items">
            {report.languages.map((lang) => (
              <li key={lang.language}>
                <span aria-hidden="true">{lang.adapter ? '✓' : '~'}</span> {lang.language} —{' '}
                {lang.adapter ? (
                  <code>{lang.adapter}</code>
                ) : (
                  <em>no adapter — surfaces under Unsupported patterns</em>
                )}{' '}
                <span className="muted">({lang.files} files)</span>
                <CoverageMatrix language={lang.language} cells={lang.coverage ?? []} />
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="preflight-card">
        <h3>
          <span className="material-symbols-outlined ok" aria-hidden="true">
            deployed_code
          </span>
          Frameworks &amp; adapters
        </h3>
        {report.frameworks.length === 0 ? (
          <p className="muted">No framework markers detected.</p>
        ) : (
          <ul className="preflight-items">
            {report.frameworks.map((framework) => (
              <li key={framework}>
                <span aria-hidden="true">✓</span> {framework}
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="preflight-card gap">
        <h3>
          <span className="material-symbols-outlined" aria-hidden="true">
            link_off
          </span>
          Potential system gaps
        </h3>
        {report.potential_gaps.length === 0 ? (
          <p className="muted">None detected.</p>
        ) : (
          <>
            <FindingList items={report.potential_gaps} marker="~" />
            <p className="preflight-note gap-note">
              Evidence exists but is not statically resolvable. These are predicted System
              Gaps — each gets an explicit Gap with a Resolution Strategy after recovery,
              never a guess.
            </p>
          </>
        )}
      </div>

      <div className="preflight-card unsupported">
        <h3>
          <span className="material-symbols-outlined" aria-hidden="true">
            block
          </span>
          Unsupported patterns
        </h3>
        {report.unsupported.length === 0 ? (
          <p className="muted">None detected.</p>
        ) : (
          <>
            <FindingList items={report.unsupported} marker="~" />
            <p className="preflight-note unsupported-note">
              No adapter covers these constructs — a tool limitation, not a System Gap.
              Recovery proceeds around them and lists them explicitly.
            </p>
          </>
        )}
      </div>

      {report.adapter_lookup && report.adapter_lookup.length > 0 && (
        <div className="preflight-card unsupported">
          <h3>
            <span className="material-symbols-outlined" aria-hidden="true">
              extension
            </span>
            Adapter lookup
          </h3>
          <AdapterLookupList lookups={report.adapter_lookup} />
        </div>
      )}
    </div>
  );
}

/** A system manifest's members in manifest order (#1655, AC-0705): each
 *  local member's own report, each remote or managed one explicitly
 *  deferred. A preview: nothing is registered or recorded. */
function SystemMembers({ system }: { system: SystemPreflight }) {
  return (
    <div className="preflight-system">
      <p className="muted preflight-preview-note">
        Preview only — no source is registered and no finding is recorded. Recovery scans each
        repository again and records its findings.
      </p>
      {system.members.map((member, index) => (
        <section
          key={`${index}:${member.entry}`}
          className="preflight-member"
          aria-label={`Repository ${member.entry}`}
        >
          <h3 className="preflight-member-title">
            <code>{member.entry}</code>
            {member.kind === 'local' && member.path !== member.entry && (
              <span className="muted"> → {member.path}</span>
            )}
          </h3>
          {member.kind === 'local' ? (
            <ReportCards report={member.report} />
          ) : (
            <div className="preflight-card">
              <h3>
                <span className="material-symbols-outlined" aria-hidden="true">
                  cloud_off
                </span>
                Detection deferred
              </h3>
              <p className="muted">
                A remote or managed repository is not fetched by preflight. Recovery detects it
                against its clone — before any parsing, still fully on-device.
              </p>
            </div>
          )}
        </section>
      ))}
    </div>
  );
}

/** Step 2 of the ingest flow (handoff §Preflight): local detection with the
 *  three-way classification from first contact. Potential system gaps and
 *  unsupported patterns are separate cards with separate meanings — an
 *  unsupported item is a tool limitation and never "becomes a Gap". */
export function PreflightSurface({
  source,
  target,
  report,
  system = null,
  busy,
  progress = null,
  error,
  canRecover,
  onBack,
  onCancel,
  onRunRecovery,
}: PreflightSurfaceProps) {
  // A preflight that ended in an error (a missing path, a cancel) has no
  // report to back a recovery, so recovery is demoted from the primary
  // action (#246). It stays available: recovery scans the target itself and
  // fails closed if it cannot read it.
  const failed = !busy && report === null && system === null && error !== null;
  return (
    <section className="ingest-flow" aria-label="Preflight checks">
      <header className="ingest-hero">
        <h2>Preflight checks</h2>
        <p className="muted">
          Detect languages, frameworks, and adapters. Runs locally —{' '}
          <strong className="local-safe">0 bytes egress</strong>.
        </p>
        <p className="preflight-target">
          <code>{target}</code>
        </p>
      </header>

      {busy && (
        <div className="preflight-progress">
          <p className="muted" role="status">
            <span className="material-symbols-outlined spinning" aria-hidden="true">
              progress_activity
            </span>{' '}
            {progress && progress.total === null ? (
              <>
                Finding files… {progress.done} so far
                {progress.path && (
                  <>
                    {' '}
                    <code>{progress.path}</code>
                  </>
                )}
              </>
            ) : progress ? (
              <>
                Checking file {progress.done + 1} of {progress.total}{' '}
                <code>{progress.path}</code>
              </>
            ) : (
              'Detecting…'
            )}
          </p>
          <button type="button" className="secondary-button" onClick={onCancel}>
            Cancel
          </button>
        </div>
      )}
      {error && <p className="error-text">{error}</p>}
      {failed && (
        <p className="muted preflight-failed-note">
          Preflight did not complete. Go back to check the target, or run recovery anyway — it
          scans the target itself and stops with an error if it cannot read it.
        </p>
      )}

      {!busy && !error && report === null && system === null && (
        <div className="preflight-card">
          <h3>
            <span className="material-symbols-outlined" aria-hidden="true">
              cloud_off
            </span>
            Detection deferred
          </h3>
          <p className="muted">
            {source === 'github'
              ? 'A remote repository is detected against its local clone at the start of recovery — before any parsing, still fully on-device.'
              : 'A system manifest is detected per declared repo at the start of recovery — before any parsing, still fully on-device.'}
          </p>
        </div>
      )}

      {report && <ReportCards report={report} />}
      {system && <SystemMembers system={system} />}

      <footer className="flow-actions">
        <button type="button" className="secondary-button" onClick={onBack}>
          Back
        </button>
        <span className="flow-actions-right">
          <button
            type="button"
            className="secondary-button"
            disabled
            title="Tier-selective recovery lands with escalation orchestration (#120)"
          >
            Structure only
          </button>
          <button
            type="button"
            className={failed ? 'secondary-button' : undefined}
            onClick={onRunRecovery}
            disabled={!canRecover || busy}
          >
            <span className="material-symbols-outlined" aria-hidden="true">
              play_arrow
            </span>
            Run full recovery
          </button>
        </span>
      </footer>
    </section>
  );
}
