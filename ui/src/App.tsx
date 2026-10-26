import { isCompletionSpecialist } from './investigationFamilies';
import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { tierDistribution, useAppStore } from './store';
import { railSurface, SURFACES, surfaceLabel, type SurfaceView } from './views';
import { NavRail } from './components/NavRail';
import { ShellHeader, type Scope } from './components/ShellHeader';
import { StatusBar } from './components/StatusBar';
import { GlobalProgress } from './components/GlobalProgress';
import { CommandPalette } from './components/CommandPalette';
import { HelpSurface } from './components/HelpSurface';
import { topicForView } from './helpTopics';
import { LegendPopover } from './components/LegendPopover';
import { RouteErrorBoundary } from './components/RouteErrorBoundary';
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsSurface } from './components/JobsSurface';
import { ConnectSurface } from './components/ConnectSurface';
import { PreflightSurface } from './components/PreflightSurface';
import { RecoverSurface } from './components/RecoverSurface';
import { SettingsSurface } from './components/SettingsSurface';
import { McpSettings } from './components/McpSettings';
import { WorkspaceSurface } from './components/WorkspaceSurface';
import { GapsDriftSurface } from './components/GapsDriftSurface';
import { ProvenanceSurface } from './components/ProvenanceSurface';
import { ResolutionStrategyModal } from './components/ResolutionStrategyModal';
import { EndpointsCard } from './components/EndpointsCard';
import { EvidencePanel } from './components/EvidencePanel';
import { CapturedSource } from './components/CapturedSource';
import { SourceRetention } from './components/SourceRetention';
import { WorkPackets, WorkPacketTray } from './components/WorkPackets';
import { useWorkPacketStore } from './workPacketStore';
import { sameFactKey, usePrimarySourceStore } from './primarySourceStore';
import { hasRecordedExecution, selectRecoveryJob } from './jobPresentation';
import { TopologyCard } from './components/TopologyCard';
import { InvestigationsSurface } from './components/InvestigationsSurface';
import { investigationIsActive, useInvestigationStore } from './investigationStore';
import { useSpecialistActivityStore } from './specialistActivityStore';
import { InvestigationQuickDispatch } from './components/InvestigationQuickDispatch';
import { SavedResultNotices } from './components/SavedResultNotices';
import { useSavedResultNoticeStore } from './savedResultNoticeStore';
import { AgentChatsSurface } from './components/AgentChatsSurface';
import { useAgentChatDockStore } from './agentChatDockStore';
import { readQuickDispatchFallback, writeQuickDispatchFallback } from './quickDispatchPreference';
import type { InvestigationChanged } from './investigationTypes';
import type { Job, PreflightProgress, SpecArtifact, SpecAssertion, SpecBundle } from './store';
import {
  isSameArtifact,
  resolveArtifactForCopy,
  withFullArtifacts,
  type ReadSpecArtifact,
  type ReportSpecError,
} from './specExport';
import { saveExport } from './saveExport';

const AtlasCanvas = lazy(() =>
  import('./components/AtlasCanvas').then(({ AtlasCanvas: Component }) => ({
    default: Component,
  })),
);
const FlowsCard = lazy(() =>
  import('./components/FlowsCard').then(({ FlowsCard: Component }) => ({
    default: Component,
  })),
);
const SpecWorkbench = lazy(() =>
  import('./components/SpecWorkbench').then(({ SpecWorkbench: Component }) => ({
    default: Component,
  })),
);

/** Surfaces a refused export or copy (e.g. a stale artifact identity) in
 *  the Workbench's error banner. */
const reportSpecError: ReportSpecError = (message) => useAppStore.setState({ specError: message });

/** Exports the bundle, or aborts (leaving `specError` for the Workbench to
 *  show) if any truncated artifact's full content couldn't be fetched — see
 *  `withFullArtifacts` in `specExport.ts`. Never downloads a file built from
 *  capped previews. */
async function exportSpecBundle(
  bundle: SpecBundle,
  readSpecArtifact: ReadSpecArtifact,
  reportError: ReportSpecError,
) {
  const artifacts = await withFullArtifacts(bundle.artifacts, readSpecArtifact, reportError);
  if (artifacts === null) return;
  const files = Object.fromEntries(artifacts.map((artifact) => [artifact.file_name, artifact.content]));
  const content = JSON.stringify({ ...bundle, artifacts, files }, null, 2);
  try {
    await saveExport(`cartograph-spec-${bundle.mode}.json`, content);
  } catch (error) {
    reportError(error instanceof Error ? error.message : typeof error === 'string' ? error : String(error));
  }
}

/** Copies the artifact's content, or aborts (leaving `specError` for the
 *  Workbench to show) if the on-demand full fetch failed — see
 *  `resolveArtifactForCopy` in `specExport.ts`. Never copies the capped
 *  preview in its place. */
async function copySpecArtifact(
  artifact: SpecArtifact,
  readSpecArtifact: ReadSpecArtifact,
  reportError: ReportSpecError,
) {
  const full = await resolveArtifactForCopy(artifact, readSpecArtifact, reportError);
  if (full === null) return;
  void navigator.clipboard?.writeText(full.content);
}

export default function App() {
  const packets = useWorkPacketStore();
  const primarySource = usePrimarySourceStore();
  const investigations = useInvestigationStore();
  const specialistActivity = useSpecialistActivityStore();
  const {
    view,
    recoverJobId,
    jobActionError,
    backend,
    version,
    stats,
    jobs,
    endpoints,
    atlas,
    flowList,
    flowAnchors,
    systemContents,
    adapters,
    plugins,
    setPluginEnabled,
    runPluginGate,
    specBundle,
    specViews,
    specMode,
    curation,
    specBusy,
    specError,
    ingestBusy,
    ingestSummary,
    ingestError,
    ingestSource,
    ingestTarget,
    preflight,
    preflightBusy,
    preflightError,
    preflightProgress,
    clearBusy,
    clearError,
    findings,
    registerFindings,
    escalationEdgeLabels,
    ingestHistory,
    coverage,
    evals,
    tierSettings,
    egress,
    ingestParallelism,
    disclosures,
    settingsError,
    selected,
    refresh,
    clearFinishedJobs,
    clearGraph,
    setSpecMode,
    readSpecArtifact,
    curateAssertion,
    select,
    clearSelection,
    setView,
    viewRecoveryJob,
    cancelJob,
    retryJob,
    applyJobEvent,
    applyJobDetail,
    setIngestSource,
    setIngestTarget,
    dropIntake,
    ingestDropped,
    dismissDropIntake,
    runPreflight,
    applyPreflightProgress,
    cancelPreflight,
    startRecovery,
    setTierEnabled,
    setIngestParallelism,
    setTierProvider,
    grantCloudConsent,
    revokeCloudConsent,
    escalation,
    openResolution,
    closeResolution,
    runStrategy,
    consentAndRun,
    dismissPreview,
    decideProposal,
    runClassEscalation,
    recordProposalDecision,
    stagedProposals,
    stagedNextCursor,
    stagedLoading,
    stagedError,
    loadStagedProposals,
    openStagedProposal,
    assessStagedBasis,
  } = useAppStore();

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [helpTopic, setHelpTopic] = useState('concepts');
  const [legendOpen, setLegendOpen] = useState(false);
  const [atlasLayer, setAtlasLayer] = useState('All layers');
  const [dropActive, setDropActive] = useState(false);
  const [quickDispatchFallbackEnabled, setQuickDispatchFallbackEnabled] = useState(readQuickDispatchFallback);
  const [quickDispatchOpenRequest, setQuickDispatchOpenRequest] = useState(0);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (view !== 'agent-chats') useAgentChatDockStore.getState().cancelNavigationIntent();
    if (view === 'investigations' || view === 'agent-chats') {
      if (view === 'investigations') {
        useSpecialistActivityStore.getState().setVisible(true);
        void useInvestigationStore.getState().loadCatalog();
      }
      void useInvestigationStore.getState().loadHistory();
    }
    return () => useSpecialistActivityStore.getState().setVisible(false);
  }, [view]);

  // Folder drop intake (#587): the webview reports dropped paths only while
  // Connect is open; elsewhere a drop does nothing.
  useEffect(() => {
    if (view !== 'connect') return;
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { getCurrentWebview } = await import('@tauri-apps/api/webview');
        const stop = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload;
          if (payload.type === 'enter' || payload.type === 'over') setDropActive(true);
          else if (payload.type === 'leave') setDropActive(false);
          else {
            setDropActive(false);
            void ingestDropped(payload.paths);
          }
        });
        if (disposed) stop(); else unlisten = stop;
      } catch { /* Browser previews have no webview bridge. */ }
    })();
    return () => { disposed = true; unlisten?.(); setDropActive(false); };
  }, [view, ingestDropped]);

  // Pushes are invalidations only. Reconnect/polling reads the durable journal;
  // no client timer fabricates a step or completion percentage.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stopReviews = await listen<import('./findingReviews').FindingReviewChanged>('investigation-review://changed', (event) =>
          useInvestigationStore.getState().invalidateReview(event.payload));
        if (disposed) stopReviews(); else unlisten = stopReviews;
        const stop = await listen<InvestigationChanged>('investigation://changed', (event) => {
          useInvestigationStore.getState().invalidate(event.payload);
          useSpecialistActivityStore.getState().invalidate();
          useSavedResultNoticeStore.getState().observe(event.payload);
        });
        if (disposed) stop(); else unlisten = () => { stop(); stopReviews(); };
      } catch { /* Browser previews have no event bridge. */ }
    })();
    return () => { disposed = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (view !== 'investigations' && view !== 'agent-chats') return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      let state = useInvestigationStore.getState();
      if (!state.historyLoading && (state.historyActiveOnly || state.history.some(investigationIsActive))) await state.loadHistory();
      if (disposed) return;
      state = useInvestigationStore.getState();
      const summary = state.detail ?? state.history.find((item) => item.investigation_id === state.selectedId);
      if (state.selectedId && !state.loading && (!summary || investigationIsActive(summary) || state.error)) await state.refreshSelected();
      if (!disposed) timer = setTimeout(() => void poll(), 2500);
    };
    const reconnect = () => {
      const state = useInvestigationStore.getState();
      if (!state.historyLoading) void state.loadHistory();
      if (!state.loading) void state.refreshSelected();
      useSpecialistActivityStore.getState().invalidate();
    };
    timer = setTimeout(() => void poll(), 2500);
    window.addEventListener('focus', reconnect);
    return () => { disposed = true; clearTimeout(timer); window.removeEventListener('focus', reconnect); };
  }, [view, investigations.selectedId]);

  // Live job transitions (#117): the core pushes every change, so progress
  // and the global bar stay current without polling. Outside Tauri (browser
  // preview, stories) there is no event bridge — refresh() still covers it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stop = await listen<Job>('job://changed', (event) => applyJobEvent(event.payload));
        if (disposed) stop();
        else unlisten = stop;
      } catch {
        // No event bridge available.
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyJobEvent]);

  // Live "what it's doing right now" pings (#209): a separate, non-durable
  // event from job://changed — best-effort, so a missed one just means the
  // detail line is stale for a moment, never a stuck/incorrect job state.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stop = await listen<{ id: number; detail: string }>('job://detail', (event) =>
          applyJobDetail(event.payload.id, event.payload.detail),
        );
        if (disposed) stop();
        else unlisten = stop;
      } catch {
        // No event bridge available.
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyJobDetail]);

  // Live preflight progress (#235): best-effort like job://detail — a missed
  // ping only leaves the progress line a moment stale.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stop = await listen<PreflightProgress>('preflight://progress', (event) =>
          applyPreflightProgress(event.payload),
        );
        if (disposed) stop();
        else unlisten = stop;
      } catch {
        // No event bridge available.
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyPreflightProgress]);

  const selectedNode = selected?.node;
  const selectedFact = selected?.fact;
  const selectedRequest = selected?.requestVersion;
  const capturedSelectionMatches = selectedNode !== undefined && primarySource.subject === selectedNode &&
    sameFactKey(primarySource.subjectFact, selectedFact ?? { kind: 'node', id: selectedNode.id });
  useEffect(() => {
    if (selectedNode) void usePrimarySourceStore.getState().inspect(selectedNode, selectedFact);
    else usePrimarySourceStore.getState().clear();
  }, [selectedNode, selectedFact, selectedRequest]);

  useEffect(() => {
    if (view === 'settings') void usePrimarySourceStore.getState().loadSources();
  }, [view]);

  const navigate = useCallback(
    (next: SurfaceView) => {
      setView(next);
      // AtlasCanvas remounts with its filter reset to All layers, so the
      // scope chip must forget the layer too or it lies on return (#143).
      if (next !== 'atlas') setAtlasLayer('All layers');
      setPaletteOpen(false);
    },
    [setView],
  );

  // ⌘K / Ctrl+K toggles the palette; ⌘1–⌘8 jump straight to a surface;
  // '?' or F1 opens Help on the current surface's topic (#154).
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = target?.closest('input, textarea, select, [contenteditable]');
      if ((event.key === '?' || event.key === 'F1') && !typing && !event.metaKey && !event.ctrlKey) {
        event.preventDefault();
        setHelpTopic(topicForView(view));
        navigate('help');
        return;
      }
      if (!(event.metaKey || event.ctrlKey)) return;
      if (event.key.toLowerCase() === 'k') {
        event.preventDefault();
        setPaletteOpen((open) => !open);
      } else if (event.key >= '1' && event.key <= '8') {
        event.preventDefault();
        navigate(SURFACES[Number(event.key) - 1].id);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [navigate, view]);

  // Native Help menu (#154): the Rust shell emits help://open.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stop = await listen('help://open', () => {
          setHelpTopic(topicForView(view));
          navigate('help');
        });
        if (cancelled) stop();
        else unlisten = stop;
      } catch {
        // No event bridge outside Tauri.
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [navigate, view]);

  // Gaps/Drift is a resolution surface, not a preview: it must never lose
  // sight of a real gap because export_spec's IPC preview capped the
  // register's assertion list (#488). Backfill the full list on demand —
  // only while the surface is actually open, so a truncated 157 MB register
  // never crosses IPC a second time just because a refresh happened.
  // Tagging the backfill with the exact `specBundle` it was fetched for
  // (rather than clearing eagerly) means a new bundle is never served a
  // stale full list: once `specBundle` changes, the tag no longer matches
  // and `registerArtifact` falls back to that new bundle's own (correctly
  // truncated) list until the fresh fetch resolves.
  const [fullRegisterAssertions, setFullRegisterAssertions] = useState<{
    bundle: SpecBundle | null;
    data: Record<string, SpecAssertion[]>;
  }>({ bundle: null, data: {} });
  useEffect(() => {
    if (view !== 'gaps' || !specBundle) return;
    const truncated = specBundle.artifacts.filter(
      (artifact) =>
        (artifact.id === 'gap-register' || artifact.id === 'drift-register') &&
        artifact.assertions_truncated,
    );
    if (truncated.length === 0) return;
    let cancelled = false;
    void Promise.all(
      truncated.map(async (artifact) => {
        const full = await readSpecArtifact(artifact.id);
        // AC-0242: never show register rows from a different compilation.
        return [artifact.id, full && isSameArtifact(artifact, full) ? full : null] as const;
      }),
    ).then((results) => {
      if (cancelled) return;
      setFullRegisterAssertions((current) => {
        const data = { ...(current.bundle === specBundle ? current.data : {}) };
        for (const [id, full] of results) {
          if (full) data[id] = full.assertions;
        }
        return { bundle: specBundle, data };
      });
    });
    return () => {
      cancelled = true;
    };
  }, [specBundle, view, readSpecArtifact]);

  const busy =
    ingestBusy || specBusy || clearBusy || investigations.history.some((item) => item.invocation_pending) ||
    jobs.some((job) => hasRecordedExecution(job) && job.status === 'running');
  const scope: Scope = selected
    ? { kind: 'trail', label: 'Single evidence trail' }
    : view === 'atlas' && atlasLayer !== 'All layers'
      ? { kind: 'layer', label: `Atlas · ${atlasLayer}` }
      : view === 'investigations' && investigations.detail?.scope.type === 'neighborhood'
        ? { kind: 'trail', label: `Investigation · ${investigations.detail.scope.hops} hop(s)` }
      : { kind: 'system', label: 'Whole system' };
  const systemName = ingestSummary ? 'Ingested system' : null;
  const status = ingestBusy
    ? 'Ingesting…'
    : backend === 'up'
      ? 'Ready'
      : backend === 'browser'
        ? 'No core (browser preview)'
        : 'Connecting…';
  // Without a core nothing can egress, so the local-only line stays honest.
  const egressLine = egress?.label ?? 'Local-only · 0 bytes egress';
  const selectEdgeSubject = (edge: {
    src: string;
    label: string;
    dst: string;
    props: Record<string, unknown>;
  }) =>
    void select(
      {
        id: `${edge.src} ${edge.label} ${edge.dst}`,
        label: edge.label,
        props: edge.props,
      },
      0,
      { kind: 'edge', source: edge.src, label: edge.label, destination: edge.dst },
    );

  const surface = (() => {
    switch (view) {
      case 'workspace':
        return (
          <>
            <WorkspaceSurface
              systemContents={systemContents}
              summary={ingestSummary}
              findings={findings}
              distribution={tierDistribution(atlas)}
              bundle={specBundle}
              onReingest={() => navigate('connect')}
              onTriageGaps={() => navigate('gaps')}
              onProvenance={() => navigate('prov')}
              onOpenArtifact={() => navigate('spec')}
              onInvestigate={() => navigate('investigations')}
            />
            <div className="card-grid utility">
              <GraphStatsCard
                stats={stats}
                systemContents={systemContents}
                canClear={backend === 'up' && !ingestBusy && (stats?.nodes ?? 0) > 0}
                clearing={clearBusy}
                error={clearError}
                onClear={() => void clearGraph()}
              />
            </div>
            <div className="card-grid artifacts">
              <EndpointsCard endpoints={endpoints} onSelect={(node) => void select(node)} />
              <TopologyCard
                view={specViews?.topology ?? null}
                artifact={
                  specBundle?.artifacts.find(
                    (item) => item.id === 'topology' || item.file_name === 'topology.md',
                  ) ?? null
                }
                onSelectNode={(node) => void select(node)}
                onSelectEdge={selectEdgeSubject}
              />
            </div>
          </>
        );
      case 'investigations':
        return <InvestigationsSurface state={investigations} activity={specialistActivity}
          onRefreshActivity={() => void useSpecialistActivityStore.getState().refresh()}
          anchors={atlas.nodes} canStart={backend === 'up'} />;
      case 'agent-chats':
        return <AgentChatsSurface state={investigations} onOpenResult={(id) => {
          navigate('investigations');
          void useInvestigationStore.getState().open(id);
        }} />;
      case 'atlas':
        return (
          <Suspense fallback={<section className="atlas-card">Loading Atlas graph…</section>}>
            <AtlasCanvas
              snapshot={atlas}
              onSelect={(node) => void select(node)}
              onSelectEdge={selectEdgeSubject}
              onLayerChange={setAtlasLayer}
            />
          </Suspense>
        );
      case 'flows':
        return (
          <Suspense
            fallback={<section className="card flow-inspector-card">Loading Flow Inspector…</section>}
          >
            <FlowsCard
              flows={flowList}
              anchors={flowAnchors}
              mode={specMode}
              onModeChange={(mode) => void setSpecMode(mode)}
              view={specViews?.flows ?? null}
              artifact={
                specBundle?.artifacts.find(
                  (item) => item.id === 'flow-dossiers' || item.file_name === 'flow_dossiers.md',
                ) ?? null
              }
              onSelectNode={(node) => void select(node)}
              onSelectHop={(hop) =>
                // Hops are evidence subjects like edges: the drawer reads the
                // hop's own provenance via a synthetic subject.
                void select({
                  id: `${hop.src} ${hop.label} ${hop.dst}`,
                  label: hop.label,
                  props: {
                    name: `${hop.src_name} ${hop.label} ${hop.dst_name}`,
                    prov: hop.provenance,
                  },
                })
              }
              onOpenResolution={(gapId) => void openResolution(gapId)}
            />
          </Suspense>
        );
      case 'packets':
        return <WorkPackets state={packets} repositories={systemContents.map((repo) => repo.repo)} />;
      case 'spec':
        return (
          <Suspense
            fallback={<section className="card spec-workbench-card">Loading Spec Workbench…</section>}
          >
            <SpecWorkbench
              bundle={specBundle}
              views={specViews}
              mode={specMode}
              decisions={curation}
              busy={specBusy}
              error={specError}
              canCurate={backend === 'up'}
              packetBusy={packets.busy}
              onOpenPackets={() => navigate('packets')}
              onAddArtifact={(artifact) => {
                if (!specBundle) return;
                void packets.addArtifact({ id: artifact.id, schema: artifact.schema, content_id: artifact.content_id, mode: specBundle.mode });
              }}
              onAddDecision={(record) => void packets.addDecision(record.assertion.provenance.content_hash)}
              onModeChange={(mode) => void setSpecMode(mode)}
              onCurate={(assertion, decision, note) =>
                void curateAssertion(assertion, decision, note)
              }
              onCopyArtifact={(artifact) => void copySpecArtifact(artifact, readSpecArtifact, reportSpecError)}
              onExportBundle={(bundle) => void exportSpecBundle(bundle, readSpecArtifact, reportSpecError)}
              onSelectNode={(node) => void select(node)}
              onSelectEdge={selectEdgeSubject}
            />
          </Suspense>
        );
      case 'gaps': {
        const registerArtifact = (file: string) => {
          const artifact = specBundle?.artifacts.find((item) => item.file_name === file);
          if (!artifact) return [];
          if (!artifact.assertions_truncated) return artifact.assertions;
          if (fullRegisterAssertions.bundle !== specBundle) return artifact.assertions;
          return fullRegisterAssertions.data[artifact.id] ?? artifact.assertions;
        };
        return (
          <GapsDriftSurface
            summary={findings}
            gaps={registerArtifact('gap_register.md')}
            drift={registerArtifact('drift_register.md')}
            registerFindings={registerFindings}
            allowedEdgeLabels={escalationEdgeLabels}
            proposalHistory={{
              proposals: stagedProposals,
              loading: stagedLoading,
              error: stagedError,
              hasMore: stagedNextCursor !== null,
              onRefresh: () => void loadStagedProposals(),
              onLoadMore: () => void loadStagedProposals(true),
              onOpen: openStagedProposal,
            }}
            flowGapIds={flowList.flatMap((flow) =>
              flow.hops
                .filter((hop) => hop.gap_reason !== null || hop.confidence === 'Gap')
                .map((hop) => hop.dst),
            )}
            onEscalateClass={(gapIds) => runClassEscalation(gapIds)}
            onDecideProposal={(proposal, decision) => recordProposalDecision(proposal, decision)}
            onOpenGap={(assertion) => {
              // A gap node opens its Resolution Strategy (#120 runner); an
              // edge/flow gap has no node to escalate, so its row opens the
              // evidence drawer from the assertion's own provenance.
              if (assertion.id.startsWith('node:')) {
                void openResolution(assertion.subject_id);
                return;
              }
              const node = atlas.nodes.find(
                (candidate) => candidate.id === assertion.subject_id,
              ) ?? {
                id: assertion.subject_id,
                label: assertion.subject_kind,
                props: { name: assertion.summary, prov: assertion.provenance },
              };
              void select(node);
            }}
          />
        );
      }
      case 'prov':
        return (
          <ProvenanceSurface
            findings={findings}
            distribution={tierDistribution(atlas)}
            coverage={coverage}
            evals={evals}
            history={ingestHistory}
          />
        );
      case 'jobs':
        return (
          <JobsSurface
            jobs={jobs}
            actionError={jobActionError ?? investigations.actionError}
            canClear={backend === 'up'}
            onClearFinished={() => void clearFinishedJobs()}
            onCancel={(id) => void cancelJob(id)}
            onRetry={(id) => void retryJob(id)}
            onViewInvestigation={(id) => { navigate('investigations'); void investigations.open(id); }}
            onCancelInvestigation={(id) => void investigations.cancel(id)}
            onViewLive={(id) => viewRecoveryJob(id)}
          />
        );
      case 'connect':
        return (
          <ConnectSurface
            source={ingestSource}
            target={ingestTarget}
            canPreflight={backend === 'up'}
            onSourceChange={setIngestSource}
            onTargetChange={setIngestTarget}
            onBack={() => navigate('workspace')}
            onPreflight={() => void runPreflight()}
            drop={dropIntake}
            dropActive={dropActive}
            onDismissDrop={dismissDropIntake}
          />
        );
      case 'preflight':
        return (
          <PreflightSurface
            source={ingestSource}
            target={ingestTarget}
            report={preflight}
            busy={preflightBusy}
            progress={preflightProgress}
            error={preflightError}
            canRecover={backend === 'up'}
            onBack={() => navigate('connect')}
            onCancel={() => void cancelPreflight()}
            onRunRecovery={() => void startRecovery()}
          />
        );
      case 'recover':
        return (
          <RecoverSurface
            job={selectRecoveryJob(jobs, recoverJobId)}
            busy={recoverJobId === null && ingestBusy}
            error={ingestError}
            onBack={() => navigate('connect')}
            onBackground={() => navigate('jobs')}
          />
        );
      case 'help':
        return <HelpSurface topic={helpTopic} onTopicChange={setHelpTopic} />;
      case 'settings':
        return (
          <>
          <SettingsSurface
            tiers={tierSettings}
            egressLabel={egressLine}
            disclosures={disclosures}
            adapters={adapters}
            plugins={plugins}
            onTogglePlugin={(plugin, enabled) => void setPluginEnabled(plugin, enabled)}
            onRunGate={(plugin) => void runPluginGate(plugin)}
            error={settingsError}
            canEdit={backend === 'up'}
            onToggleTier={(tier, enabled) => void setTierEnabled(tier, enabled)}
            onProviderChange={(tier, provider) => void setTierProvider(tier, provider)}
            onGrantConsent={(tier) => void grantCloudConsent(tier)}
            onRevokeConsent={(tier) => void revokeCloudConsent(tier)}
            parallelism={ingestParallelism}
            onParallelismChange={(setting) => void setIngestParallelism(setting)}
            quickDispatchFallbackEnabled={quickDispatchFallbackEnabled}
            onQuickDispatchFallbackChange={(enabled) => { setQuickDispatchFallbackEnabled(enabled); writeQuickDispatchFallback(enabled); }}
            onOpenQuickDispatch={() => setQuickDispatchOpenRequest((request) => request + 1)}
          />
          <McpSettings available={backend === 'up'} />
          <SourceRetention sources={primarySource.sources} preview={primarySource.preview}
            busy={primarySource.retentionBusy} error={primarySource.retentionError}
            message={primarySource.retentionMessage}
            onPreview={(id) => void primarySource.previewSource(id)}
            onDismiss={primarySource.dismissPreview} onForget={() => void primarySource.forget()} />
          </>
        );
    }
  })();

  return (
    <div className="shell">
      <GlobalProgress active={busy} />
      <NavRail
        active={railSurface(view)}
        onNavigate={navigate}
        onOpenPalette={() => setPaletteOpen(true)}
      />
      <div className="shell-main">
        <ShellHeader
          system={systemName}
          surface={surfaceLabel(view)}
          scope={scope}
          onShowLegend={() => setLegendOpen(true)}
          onShowHelp={() => {
            setHelpTopic(topicForView(view));
            navigate('help');
          }}
        />
        <main className="shell-content">
          <WorkPacketTray state={packets} showError={view !== 'packets'} onOpen={() => navigate('packets')} />
          <RouteErrorBoundary view={view}>{surface}</RouteErrorBoundary>
        </main>
        <StatusBar
          status={status}
          busy={busy}
          egress={egressLine}
          onOpenRecovery={ingestBusy ? () => navigate('recover') : undefined}
        >
          <StatusBadge backend={backend} version={version} />
        </StatusBar>
      </div>
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        onNavigate={navigate}
      />
      <InvestigationQuickDispatch fallbackEnabled={quickDispatchFallbackEnabled} openRequest={quickDispatchOpenRequest} onNavigate={navigate} />
      <SavedResultNotices view={view} onOpenChat={(taskId) => {
        useAgentChatDockStore.getState().requestOpenFromNotice(taskId);
        navigate('agent-chats');
      }} onOpenFindings={(taskId) => {
        navigate('investigations');
        void useInvestigationStore.getState().open(taskId);
      }} />
      <LegendPopover open={legendOpen} onClose={() => setLegendOpen(false)} />
      {selected && (
        <div className="evidence-area">
          <EvidencePanel
            node={selected.node}
            source={selected.source}
            evidenceIndex={selected.evidenceIndex}
            captured={<CapturedSource key={primarySource.selection} description={capturedSelectionMatches ? primarySource.description : null}
              text={capturedSelectionMatches ? primarySource.text : null}
              loading={!capturedSelectionMatches || primarySource.loading} error={capturedSelectionMatches ? primarySource.error : null}
              onRead={(index) => void primarySource.readRange(index)}
              requirementBusy={primarySource.requirementBusy}
              requirementError={primarySource.requirementError ?? investigations.inspectionDraftError}
              onRequire={(index) => {
                if (!capturedSelectionMatches) return;
                void primarySource.resolveInspectionTarget(index).then(async (target) => {
                  if (!target) return;
                  const state = useInvestigationStore.getState();
                  const added = isCompletionSpecialist(state.composer.specialistId) ? await state.addSourceObligation(target) : state.addInspectionRequirement(target);
                  if (added) {
                    clearSelection();
                    navigate('investigations');
                  }
                });
              }} />}
            onAddToPacket={selected.fact || atlas.nodes.some((node) => node.id === selected.node.id)
              ? () => void packets.addFact(selected.fact ?? { kind: 'node', id: selected.node.id }) : undefined}
            packetBusy={packets.busy}
            onClose={clearSelection}
            onShowEvidence={(index) => void select(selected.node, index, selected.fact)}
            onOpenResolution={
              // Only a real Gap node has strategies to derive; a synthetic
              // edge/flow subject would ask the backend for a node it does
              // not have — the CTA stays disabled for those (#142 review).
              atlas.nodes.some((candidate) => candidate.id === selected.node.id)
                ? (node) => void openResolution(node.id)
                : undefined
            }
          />
        </div>
      )}
      {escalation && (
        <ResolutionStrategyModal
          state={escalation}
          onRun={(strategyId) => void runStrategy(strategyId)}
          onConsent={(preview) => void consentAndRun(preview)}
          onDismissPreview={dismissPreview}
          onDecide={(decision) => void decideProposal(decision)}
          onClose={closeResolution}
          onAssessBasis={() => void assessStagedBasis()}
        />
      )}
    </div>
  );
}
