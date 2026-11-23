import { useMemo, useState } from 'react';
import type {
  AssertionDecision,
  AssertionDecisionRecord,
  CuratedRead,
  GraphEdge,
  GraphNode,
  SpecArtifact,
  SpecAssertion,
  SpecBundle,
  SpecDiagramView,
  SpecExportMode,
  SpecViewBundle,
  Tier,
} from '../store';
import {
  STRUCTURED_VIEW_UNAVAILABLE_NOTICE,
  SpecDiagramCanvas,
  verifyDiagramViewIdentity,
} from './SpecDiagramView';
import { renderArtifactMarkdown } from '../markdownView';
import { TierBadge, methodLabel } from './TierBadge';
import { CuratedSection } from './CuratedSection';
import { SectionDocumentView } from './SectionDocumentView';
import type { SectionDocumentViewProps } from './SectionDocumentView';
import { AreaExpertPanel } from './AreaExpertPanel';
import { ArchitectureCanvas } from './ArchitectureCanvas';
import type { AreaExpertPanelProps } from './AreaExpertPanel';
import { AttestedRecords, type AttestedRecordsProps } from './AttestedRecords';

export interface SpecWorkbenchProps {
  bundle: SpecBundle | null;
  views?: SpecViewBundle | null;
  /** The curated projection `bundle` was compiled with, from the same host
   *  response (SPEC-12 slice 3, AC-0655); `null` or absent when none. */
  curated?: CuratedRead | null;
  onRefreshCurated?: () => void;
  /** The document view (SPEC-15 slice 7, AC-0669); absent when the host
   *  surface does not offer it. Its accepted-draft states come from
   *  `curated`, and a citation opens the fact index of `bundle`. */
  sections?: Omit<SectionDocumentViewProps, 'curated' | 'onOpenCitation'>;
  /** Ask the expert of one subsystem area (#2057, AC-0932). */
  expert?: Omit<AreaExpertPanelProps, 'onOpenCitation'>;
  /** Attested records (ADR-0090, AC-1081): shown apart from recovered
   *  content, with the engineer's review. */
  attested?: AttestedRecordsProps;
  pageSize?: number;
  mode: SpecExportMode;
  decisions: AssertionDecisionRecord[];
  busy: boolean;
  error: string | null;
  canCurate: boolean;
  packetBusy?: boolean;
  onOpenPackets?: () => void;
  onAddArtifact?: (artifact: SpecArtifact) => void;
  onAddDecision?: (record: AssertionDecisionRecord) => void;
  onModeChange: (mode: SpecExportMode) => void;
  onCurate: (assertion: SpecAssertion, decision: AssertionDecision, note?: string) => void;
  onCopyArtifact: (artifact: SpecArtifact) => void;
  onExportBundle: (bundle: SpecBundle) => void;
  onSelectNode?: (node: GraphNode) => void;
  onSelectEdge?: (edge: GraphEdge) => void;
}

/** A readable prefix of an artifact's content identity; the full value is
 *  kept in the element's title (AC-0242). */
export function shortContentId(contentId: string): string {
  const [scheme, digest] = contentId.split(':', 2);
  return digest ? `${scheme}:${digest.slice(0, 12)}` : contentId;
}

function isInferred(tier: Tier): boolean {
  return tier === 'InferredStrong' || tier === 'InferredWeak';
}

function producerLabel(tier: string): string {
  switch (tier) {
    case 'Deterministic':
      return 'T0';
    case 'Dynamic':
      return 'T1';
    case 'Semantic':
      return 'T2';
    case 'Agentic':
      return 'T3';
    default:
      return tier;
  }
}

/** Doc-list count chip (handoff 04): unit named per document, alert tone for
 * the registers whose counts demand attention. Units match what the
 * assertions actually are (#145 review): one per recovered US/ADR, but the
 * matrix's are relation links and the dossier's are hops — never relabeled
 * as ACs or flows the compiler did not count. */
export function docChip(artifact: SpecArtifact): { text: string; tone: 'default' | 'alert' } {
  // The true total (#488) — `artifact.assertions` may be capped to a preview
  // when it crossed IPC, but the chip always counts every instance.
  const count = artifact.assertions_total;
  // Structured indexes (#240, #487) list the instances of their Markdown
  // artifact, where those instances are counted once.
  if (artifact.format === 'json') return { text: 'index', tone: 'default' };
  switch (artifact.file_name) {
    case 'user_stories.md':
      return { text: `${count} US`, tone: 'default' };
    case 'US-TM.md':
      return { text: `${count} links`, tone: 'default' };
    case 'flow_dossiers.md':
      return { text: `${count} hops`, tone: 'default' };
    case 'adrs.md':
      return { text: `${count} ADR`, tone: 'default' };
    case 'rule-evidence.md':
      // Includes observations and their GOVERNS/DEPENDS_ON relationships.
      return { text: `${count} assertions`, tone: 'default' };
    case 'gap_register.md':
    case 'drift_register.md':
    case 'security.md':
      return { text: `${count} assertion rows`, tone: count > 0 ? 'alert' : 'default' };
    default:
      return { text: `${count}`, tone: 'default' };
  }
}

function decisionLabel(decision: AssertionDecision): string {
  switch (decision) {
    case 'accepted':
      return 'Accepted';
    case 'rejected':
      return 'Rejected';
    case 'annotated':
      return 'Annotated';
  }
}

/** Official spec artifacts, provenance, curation, and security review (US-0012/US-0015). */
export function SpecWorkbench({
  bundle,
  views,
  curated,
  onRefreshCurated,
  sections,
  expert,
  attested,
  pageSize,
  mode,
  decisions,
  busy,
  error,
  canCurate,
  packetBusy,
  onOpenPackets,
  onAddArtifact,
  onAddDecision,
  onModeChange,
  onCurate,
  onCopyArtifact,
  onExportBundle,
  onSelectNode,
  onSelectEdge,
}: SpecWorkbenchProps) {
  const [selectedId, setSelectedId] = useState('');
  const [artifactView, setArtifactView] = useState<'rendered' | 'source'>('rendered');
  const [notes, setNotes] = useState<Record<string, string>>({});
  // The citation a reviewer last opened from a draft: its evidence is its
  // row in the fact index of this bundle.
  const [openedCitation, setOpenedCitation] = useState<string | null>(null);
  const factIndex = bundle?.artifacts.find((artifact) => artifact.id === 'fact-index') ?? null;
  const decisionByHash = useMemo(
    () => new Map(decisions.map((record) => [record.assertion.provenance.content_hash, record])),
    [decisions],
  );
  const selected =
    bundle?.artifacts.find((artifact) => artifact.id === selectedId) ?? bundle?.artifacts[0] ?? null;
  const isDiagramArtifact = Boolean(
    selected &&
      (selected.id === 'topology' ||
        selected.id === 'flow-dossiers' ||
        selected.id === 'data-model'),
  );
  const selectedDiagramView = useMemo<SpecDiagramView | null>(() => {
    if (!selected || !isDiagramArtifact || !views) return null;
    if (selected.id === 'topology') return views.topology ?? null;
    if (selected.id === 'flow-dossiers') return views.flows ?? null;
    if (selected.id === 'data-model') return views.data_model ?? null;
    return null;
  }, [selected, isDiagramArtifact, views]);
  const architectureView =
    selected?.id === 'architecture' && views?.architecture ? views.architecture : null;
  const hasValidDiagramView = Boolean(
    selectedDiagramView && verifyDiagramViewIdentity(selectedDiagramView, selected),
  );
  const selectedMarkdownContent =
    selected && !isDiagramArtifact && selected.format === 'markdown' ? selected.content : null;
  const renderedMarkdown = useMemo(
    () => (selectedMarkdownContent !== null ? renderArtifactMarkdown(selectedMarkdownContent) : null),
    [selectedMarkdownContent],
  );

  return (
    <section className="card spec-workbench-card" aria-labelledby="spec-workbench-title">
      <header className="spec-workbench-header">
        <div>
          <h2 id="spec-workbench-title">Spec Workbench</h2>
          <p className="muted">
            Review official artifacts, curate inferred assertions, and export without obscuring provenance.
          </p>
        </div>
        <div className="spec-workbench-controls">
          {onOpenPackets && <button type="button" onClick={onOpenPackets}>Agent work packets</button>}
          <div className="flow-mode-toggle" aria-label="Official spec export mode">
            {(['verified-only', 'best-effort'] as const).map((item) => (
              <button
                key={item}
                type="button"
                className={mode === item ? 'active' : ''}
                aria-pressed={mode === item}
                disabled={busy}
                onClick={() => onModeChange(item)}
              >
                {item}
              </button>
            ))}
          </div>
          <button
            type="button"
            disabled={!bundle || busy}
            onClick={() => bundle && onExportBundle(bundle)}
          >
            Export bundle
          </button>
        </div>
      </header>

      {error && <p className="error spec-workbench-error">{error}</p>}
      {!bundle ? (
        <p className="muted spec-workbench-empty">
          No compiled spec is available — connect to the core and ingest a repository set.
        </p>
      ) : (
        <>
          <div className="spec-workbench-summary" role="status">
            <span>{bundle.artifacts.length} artifacts</span>
            <span>{bundle.assertion_count} visible assertions</span>
            <span className="tier-gap">Unresolved findings: {bundle.gap_count}</span>
            <span>{bundle.drift_count} drift findings</span>
            <span>{bundle.security_count} security findings</span>
            {busy && <span>Refreshing…</span>}
          </div>

          <CuratedSection read={curated ?? null} busy={busy} onRefresh={onRefreshCurated} />

          {sections && (
            <SectionDocumentView
              {...sections}
              curated={curated ?? null}
              onOpenCitation={factIndex ? (citationId) => {
                setSelectedId(factIndex.id);
                setArtifactView('rendered');
                setOpenedCitation(citationId);
              } : undefined}
            />
          )}
          {expert && (
            <AreaExpertPanel
              {...expert}
              onOpenCitation={factIndex ? (citationId) => {
                setSelectedId(factIndex.id);
                setArtifactView('rendered');
                setOpenedCitation(citationId);
              } : undefined}
            />
          )}
          {attested && (
            <section className="card spec-attested">
              <h3>Attested · {attested.records.length}</h3>
              <p className="muted">
                Stated by the engineer or by agents, not recovered from the system. The
                best-effort export prints them with their label; verified-only leaves them out and
                counts them.
              </p>
              <AttestedRecords {...attested} />
            </section>
          )}
          {openedCitation && factIndex && selected?.id === factIndex.id && (
            <p className="curated-notice spec-opened-citation" role="status">
              Showing <code>{factIndex.file_name}</code>. The row for <code>{openedCitation}</code>{' '}
              lists the fact and its evidence.
            </p>
          )}

          <div className="spec-workbench-layout">
            <nav className="spec-artifact-nav" aria-label="Official spec artifacts">
              {bundle.artifacts.map((artifact) => (
                <button
                  key={artifact.id}
                  type="button"
                  className={selected?.id === artifact.id ? 'active' : ''}
                  aria-current={selected?.id === artifact.id ? 'page' : undefined}
                  onClick={() => setSelectedId(artifact.id)}
                >
                  <span>{artifact.title}</span>
                  <small>{artifact.file_name}</small>
                  {(() => {
                    const chip = docChip(artifact);
                    return (
                      <strong className={`spec-doc-chip ${chip.tone}`}>{chip.text}</strong>
                    );
                  })()}
                </button>
              ))}
              <div className="spec-curation-log">
                <h3>Curation log</h3>
                {decisions.length === 0 ? (
                  <p className="muted">No decisions recorded.</p>
                ) : (
                  <ul>
                    {decisions.map((record) => (
                      <li key={record.assertion.provenance.content_hash}>
                        <strong>{decisionLabel(record.decision)}</strong>
                        <span>{record.assertion.summary}</span>
                        {record.note && <small>{record.note}</small>}
                        {onAddDecision && <button type="button" disabled={packetBusy || busy} onClick={() => onAddDecision(record)}>Add historical decision to packet</button>}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </nav>

            {selected && (
              <article className="spec-artifact-detail" aria-labelledby="selected-artifact-title">
                <header>
                  <div>
                    <p className="eyebrow">{selected.format} artifact</p>
                    <h3 id="selected-artifact-title">{selected.title}</h3>
                    <code>{selected.file_name}</code>
                    <p className="muted spec-artifact-identity" data-testid="spec-artifact-identity">
                      <span>{selected.schema}</span> · <code title={selected.content_id}>{shortContentId(selected.content_id)}</code>
                    </p>
                  </div>
                  {onAddArtifact && <button type="button" disabled={packetBusy || busy} onClick={() => onAddArtifact(selected)}>Add artifact to packet</button>}
                  <button type="button" onClick={() => onCopyArtifact(selected)}>
                    Copy artifact
                  </button>
                </header>

                {selected.file_name === 'gap_register.md' && (
                  <p className="muted">
                    Unresolved findings count distinct gaps. Assertion rows include supporting
                    evidence and can refer to the same finding.
                  </p>
                )}

                {selected.file_name === 'rule-evidence.md' && (
                  <p className="muted">
                    Source observations only. Complete execution predicates and consumer
                    effects are not established.
                  </p>
                )}

                <p className="spec-lock-banner" role="note">
                  Confirmed (T0/T1) assertions are read-only; only proposed T2/T3 assertions can
                  be curated (R-INT-1).
                </p>

                {architectureView ? (
                  <>
                    <ArchitectureCanvas
                      key={`${selected.id}:${selected.content_id}`}
                      view={architectureView}
                      artifact={selected}
                    />
                    <details className="spec-artifact-source-details">
                      <summary>Copyable Mermaid / Markdown source</summary>
                      <pre className="spec-artifact-source" data-testid="spec-artifact-source">
                        {selected.content}
                      </pre>
                    </details>
                  </>
                ) : selectedDiagramView ? (
                  <>
                    <SpecDiagramCanvas
                      key={`${selected.id}:${selected.content_id}`}
                      view={selectedDiagramView}
                      artifact={selected}
                      title={`${selected.title} diagram`}
                      pageSize={pageSize}
                      onSelectNode={onSelectNode}
                      onSelectEdge={onSelectEdge}
                    />
                    <details className="spec-artifact-source-details" open={!hasValidDiagramView}>
                      <summary>Copyable Mermaid / Markdown source</summary>
                      <pre className="spec-artifact-source" data-testid="spec-artifact-source">
                        {selected.content}
                      </pre>
                    </details>
                  </>
                ) : isDiagramArtifact ? (
                  <>
                    <p
                      className="muted spec-diagram-unavailable"
                      data-testid="diagram-view-unavailable"
                    >
                      {STRUCTURED_VIEW_UNAVAILABLE_NOTICE}
                    </p>
                    <pre className="spec-artifact-source" data-testid="spec-artifact-source">
                      {selected.content}
                    </pre>
                  </>
                ) : selected.format === 'markdown' ? (
                  <>
                    <div className="spec-artifact-view-toggle" role="group" aria-label="Artifact view">
                      <button
                        type="button"
                        className={artifactView === 'rendered' ? 'active' : ''}
                        aria-pressed={artifactView === 'rendered'}
                        onClick={() => setArtifactView('rendered')}
                      >
                        Rendered
                      </button>
                      <button
                        type="button"
                        className={artifactView === 'source' ? 'active' : ''}
                        aria-pressed={artifactView === 'source'}
                        onClick={() => setArtifactView('source')}
                      >
                        Source
                      </button>
                    </div>
                    {artifactView === 'rendered' ? (
                      <div
                        className="spec-artifact-rendered md-view"
                        data-testid="spec-artifact-rendered"
                      >
                        {renderedMarkdown}
                      </div>
                    ) : (
                      <pre className="spec-artifact-source" data-testid="spec-artifact-source">
                        {selected.content}
                      </pre>
                    )}
                  </>
                ) : (
                  <pre className="spec-artifact-source" data-testid="spec-artifact-source">
                    {selected.content}
                  </pre>
                )}
                {selected.content_truncated && (
                  <p className="muted spec-artifact-truncated-note" data-testid="spec-artifact-truncated-note">
                    Showing a preview — the complete artifact is{' '}
                    {selected.content_byte_len.toLocaleString()} bytes. Use Copy artifact or Export
                    bundle for the complete text.
                  </p>
                )}

                <section className="spec-assertions" aria-labelledby="spec-assertions-title">
                  <header>
                    <h3 id="spec-assertions-title">Assertions and inline provenance</h3>
                    <span className="muted">{selected.assertions_total} in this artifact</span>
                  </header>
                  {selected.assertions_truncated && (
                    <p className="muted spec-assertions-truncated-note" data-testid="spec-assertions-truncated-note">
                      Showing the first {selected.assertions.length.toLocaleString()} of{' '}
                      {selected.assertions_total.toLocaleString()} assertions. Export the bundle for
                      the complete list.
                    </p>
                  )}
                  {selected.assertions.length === 0 ? (
                    <p className="muted">No graph-backed assertions were recovered for this artifact.</p>
                  ) : (
                    <ol>
                      {selected.assertions.map((assertion) => {
                        const provenance = assertion.provenance;
                        const record = decisionByHash.get(provenance.content_hash);
                        const curatable = isInferred(provenance.confidence_tier);
                        const note = notes[provenance.content_hash] ?? record?.note ?? '';
                        return (
                          <li key={assertion.id} className={`spec-assertion tier-border-${provenance.confidence_tier.toLowerCase()}`}>
                            <div className="spec-assertion-heading">
                              <div>
                                <p className="eyebrow">{assertion.subject_kind}</p>
                                <strong>{assertion.summary}</strong>
                              </div>
                              <TierBadge tier={provenance.confidence_tier} provenance={provenance} />
                            </div>
                            <dl className="spec-provenance">
                              <div><dt>Producer</dt><dd>{provenance.tier}</dd></div>
                              <div><dt>Method</dt><dd>{methodLabel(provenance.method)}</dd></div>
                              <div><dt>Extractor</dt><dd><code>{provenance.extractor_id}</code></dd></div>
                              <div className="wide"><dt>Content hash</dt><dd><code>{provenance.content_hash}</code></dd></div>
                              <div className="wide">
                                <dt>Evidence</dt>
                                <dd>
                                  {provenance.evidence.length === 0 ? (
                                    <span>None recorded — treated as unresolved.</span>
                                  ) : (
                                    <ul>
                                      {provenance.evidence.map((evidence) => (
                                        <li key={`${evidence.repo}:${evidence.path}:${evidence.byte_start}`}>
                                          <code>{evidence.repo}/{evidence.path}</code>
                                          <span>bytes {evidence.byte_start}..{evidence.byte_end}</span>
                                          <span>@ {evidence.commit_sha}</span>
                                        </li>
                                      ))}
                                    </ul>
                                  )}
                                </dd>
                              </div>
                            </dl>

                            {provenance.confidence_tier === 'Confirmed' && (
                              // The lock is visible and explained, not hidden
                              // (#108): deterministic facts are never
                              // curatable (R-INT-1), and the disabled state
                              // is real in the a11y tree.
                              <div className="spec-curation-controls locked">
                                <div>
                                  {(['Accept', 'Reject', 'Annotate'] as const).map((action) => (
                                    <button
                                      key={action}
                                      type="button"
                                      disabled
                                      aria-disabled="true"
                                      className={action === 'Reject' ? 'danger' : undefined}
                                    >
                                      {action}
                                    </button>
                                  ))}
                                  <span className="spec-lock-note">
                                    Confirmed {producerLabel(provenance.tier)} — locked, read-only
                                  </span>
                                </div>
                              </div>
                            )}
                            {curatable && (
                              <div className="spec-curation-controls">
                                <label>
                                  Annotation
                                  <textarea
                                    value={note}
                                    placeholder="Add evidence-backed context…"
                                    disabled={!canCurate || busy}
                                    onChange={(event) => setNotes((current) => ({
                                      ...current,
                                      [provenance.content_hash]: event.target.value,
                                    }))}
                                  />
                                </label>
                                <div>
                                  <button
                                    type="button"
                                    disabled={!canCurate || busy || provenance.evidence.length === 0}
                                    onClick={() => onCurate(assertion, 'accepted', note || undefined)}
                                  >
                                    Accept
                                  </button>
                                  <button
                                    type="button"
                                    className="danger"
                                    disabled={!canCurate || busy || provenance.evidence.length === 0}
                                    onClick={() => onCurate(assertion, 'rejected', note || undefined)}
                                  >
                                    Reject
                                  </button>
                                  <button
                                    type="button"
                                    disabled={!canCurate || busy || provenance.evidence.length === 0 || !note.trim()}
                                    onClick={() => onCurate(assertion, 'annotated', note)}
                                  >
                                    Annotate
                                  </button>
                                  {record && (
                                    <span className={`curation-decision decision-${record.decision}`}>
                                      {decisionLabel(record.decision)}
                                    </span>
                                  )}
                                </div>
                              </div>
                            )}
                          </li>
                        );
                      })}
                    </ol>
                  )}
                </section>
              </article>
            )}
          </div>
        </>
      )}
    </section>
  );
}
