import { invoke } from '@tauri-apps/api/core';
import { isCompletionSpecialist } from './investigationFamilies';
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { tierDistribution, useAppStore } from './store';
import { createRepositoryLabeler, systemDisplayName } from './repositoryLabels';
import { curatedRecordStatuses } from './curated';
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
import { ProjectsCard } from './components/ProjectsCard';
import { JobsSurface } from './components/JobsSurface';
import { ConnectSurface } from './components/ConnectSurface';
import { PreflightSurface } from './components/PreflightSurface';
import { RecoverSurface } from './components/RecoverSurface';
import { SettingsSurface } from './components/SettingsSurface';
import { McpSettings } from './components/McpSettings';
import { ProviderKeys } from './components/ProviderKeys';
import { LocalLane } from './components/LocalLane';
import { KnowledgePhaseNotice } from './components/KnowledgePhaseNotice';
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
import {
  composeOutcomeText, gatedRunText, runSectionCompose, runSectionComposeGated, sectionComposePreview, sectionDraftQuestion,
  sectionRedraftQuestion, useSectionDocumentStore,
} from './sectionDocument';
import { EgressConsentDialog, type EgressPreview } from './components/EgressConsentDialog';
import type { SectionAddress, StoredSectionDraft } from './sectionDocument';
import { useAreaExpert } from './areaExpert';
import { useSpecialistActivityStore } from './specialistActivityStore';
import { InvestigationQuickDispatch } from './components/InvestigationQuickDispatch';
import { AgentPoolPanel } from './components/AgentPoolPanel';
import {
  awaitingCuration, chatSessions, loadAgentPoolControls, loadJobUsage, saveAgentPoolControls,
  type AgentPoolControls, type JobUsage,
} from './agentPool';
import type { LocalLaneStatus } from './components/LocalLane';
import { SavedResultNotices } from './components/SavedResultNotices';
import { useSavedResultNoticeStore } from './savedResultNoticeStore';
import { AgentChatsSurface } from './components/AgentChatsSurface';
import { useAgentChatDockStore } from './agentChatDockStore';
import { readQuickDispatchFallback, readQuickDispatchHint, writeQuickDispatchFallback, writeQuickDispatchHint } from './quickDispatchPreference';
import { QuickDispatchHint } from './components/QuickDispatchHint';
import type { InvestigationChanged, InvestigationFollowUpContext } from './investigationTypes';
import type { Job, PreflightProgress, SpecArtifact, SpecAssertion, SpecBundle } from './store';
import {
  exportSpecBundle,
  isSameArtifact,
  resolveArtifactForCopy,
  type ReadSpecArtifact,
  type ReportSpecError,
} from './specExport';

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

/** Views where the dispatch hint would cover setup or settings work. */
const HINTLESS_VIEWS = new Set<string>(['settings', 'help', 'connect', 'preflight', 'recover']);

/** Surfaces a refused export or copy (e.g. a stale artifact identity) in
 *  the Workbench's error banner. */
const reportSpecError: ReportSpecError = (message) => useAppStore.setState({ specError: message });

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
  const noticeCorner = useSavedResultNoticeStore((state) => state.corner);
  const knownUnreadSavedResults = useSavedResultNoticeStore((state) => state.unread.length);
  const unreadChecksIncomplete = useSavedResultNoticeStore((state) => state.incomplete);
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
    specCurated,
    reloadSpec,
    specMode,
    curation,
    specBusy,
    specError,
    ingestBusy,
    ingestSummary,
    recordedCoverage,
    recordedCoverageError,
    ingestError,
    ingestSource,
    ingestTarget,
    preflight,
    systemPreflight,
    preflightBusy,
    preflightError,
    preflightProgress,
    clearBusy,
    clearError,
    newProjectBusy,
    newProjectError,
    projects,
    projectsBusy,
    projectsError,
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
    startNewProject,
    switchProject,
    renameProject,
    deleteProject,
    setSpecMode,
    readSpecArtifact,
    readSpecArtifacts,
    readSelectedFlowView,
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
    withdrawProposal,
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
  // The state the host gave each accepted record (AC-0655), for the proposal
  // history and the open review dialog.
  const curatedStates = useMemo(() => curatedRecordStatuses(specCurated), [specCurated]);

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [helpTopic, setHelpTopic] = useState('concepts');
  const [legendOpen, setLegendOpen] = useState(false);
  const [atlasLayer, setAtlasLayer] = useState('All layers');
  const [dropActive, setDropActive] = useState(false);
  const [quickDispatchFallbackEnabled, setQuickDispatchFallbackEnabled] = useState(readQuickDispatchFallback);
  // Provider-reported usage per agent job (#252, AC-1703), read for the agent pool.
  // Null when the host could not read it, so stale counts are never shown.
  const [jobUsage, setJobUsage] = useState<JobUsage[] | null>([]);
  // AC-1710: chats open in a pane are marked in the pool.
  const chatDocks = useAgentChatDockStore((state) => state.docks);
  // The host's agent pool controls (AC-1704); null until read.
  const [poolControls, setPoolControls] = useState<AgentPoolControls | null>(null);
  const [poolControlsError, setPoolControlsError] = useState<string | null>(null);
  // Shown at once so a second change builds on the first; only the latest
  // save's answer is applied, and a failed save re-reads what is stored.
  const poolControlsSave = useRef(0);
  const changePoolControls = useCallback((next: AgentPoolControls) => {
    setPoolControlsError(null);
    setPoolControls(next);
    const save = ++poolControlsSave.current;
    const latest = () => save === poolControlsSave.current;
    void saveAgentPoolControls(next).then(
      (stored) => { if (stored && latest()) setPoolControls(stored); },
      (error: unknown) => {
        if (!latest()) return;
        setPoolControlsError(`The pool controls were not saved: ${String(error)}`);
        void loadAgentPoolControls().then((stored) => { if (latest()) setPoolControls(stored); }, () => undefined);
      },
    );
  }, []);
  // The local model server check in the agent pool (#252, AC-1708): on
  // demand only, and only the latest check's answer is shown.
  const [localServer, setLocalServer] = useState<LocalLaneStatus | null>(null);
  const [localServerChecking, setLocalServerChecking] = useState(false);
  const [localServerError, setLocalServerError] = useState<string | null>(null);
  const localServerCheck = useRef(0);
  const checkLocalServer = useCallback(() => {
    const check = ++localServerCheck.current;
    setLocalServerError(null);
    setLocalServerChecking(true);
    invoke<LocalLaneStatus>('local_lane_status').then(
      (status) => { if (check === localServerCheck.current) { setLocalServer(status); setLocalServerError(null); } },
      (error: unknown) => { if (check === localServerCheck.current) setLocalServerError(String(error)); },
    ).finally(() => { if (check === localServerCheck.current) setLocalServerChecking(false); });
  }, []);
  // The closed-state dispatch hint (#251, AC-1702) shows until the first confirmed dispatch.
  const [quickDispatchHintShown, setQuickDispatchHintShown] = useState(readQuickDispatchHint);
  const showQuickDispatchHint = useCallback((shown: boolean) => {
    setQuickDispatchHintShown(shown);
    writeQuickDispatchHint(shown);
  }, []);
  const [quickDispatchOpenRequest, setQuickDispatchOpenRequest] = useState(0);
  const [followUpCancelRequest, setFollowUpCancelRequest] = useState(0);
  const [followUpContext, setFollowUpContext] = useState<InvestigationFollowUpContext | null>(null);
  const [preparingFollowUpTask, setPreparingFollowUpTask] = useState<string | null>(null);
  const replyHosts = useRef(new Map<string, HTMLDivElement>());
  const inlineReplyTask = useRef<string | null>(null);
  const [inlineReply, setInlineReply] = useState<{ taskId: string; host: HTMLDivElement } | null>(null);
  const [followUpError, setFollowUpError] = useState<string | null>(null);
  const followUpCaptureGeneration = useRef(0);
  const preparingFollowUpTaskRef = useRef<string | null>(null);
  const activeFollowUpParentRef = useRef<string | null>(null);

  const cancelFollowUpCapture = useCallback((taskId?: string) => {
    if (taskId && preparingFollowUpTaskRef.current !== taskId && activeFollowUpParentRef.current !== taskId) return;
    ++followUpCaptureGeneration.current;
    preparingFollowUpTaskRef.current = null;
    activeFollowUpParentRef.current = null;
    setPreparingFollowUpTask(null);
    setFollowUpContext(null);
    inlineReplyTask.current = null; setInlineReply(null);
    setFollowUpCancelRequest((request) => request + 1);
  }, []);

  const registerReplyHost = useCallback((taskId: string, host: HTMLDivElement | null) => {
    if (host) replyHosts.current.set(taskId, host);
    else replyHosts.current.delete(taskId);
    if (!host && inlineReplyTask.current === taskId) {
      cancelFollowUpCapture(taskId); inlineReplyTask.current = null; setInlineReply(null);
    }
  }, [cancelFollowUpCapture]);

  const openStandaloneQuickDispatch = useCallback(() => {
    cancelFollowUpCapture();
    setFollowUpError(null);
    setQuickDispatchOpenRequest((request) => request + 1);
  }, [cancelFollowUpCapture]);

  const prepareParentFollowUp = useCallback((taskId: string, inline = false) => {
    const currentStore = useInvestigationStore.getState();
    if (currentStore.pendingStart) {
      setFollowUpError('Recover the pending investigation request before starting a follow-up.');
      return;
    }
    const dockStore = useAgentChatDockStore.getState();
    if (!dockStore.docks.some((item) => item.chat.task_id === taskId)) {
      setFollowUpError('Reopen this saved chat before preparing a follow-up.');
      return;
    }
    const host = inline ? replyHosts.current.get(taskId) : undefined;
    if (inline && !host?.isConnected) return;
    cancelFollowUpCapture();
    if (host) { inlineReplyTask.current = taskId; setInlineReply({ taskId, host }); }
    const generation = ++followUpCaptureGeneration.current;
    setFollowUpError(null);
    activeFollowUpParentRef.current = null;
    setFollowUpContext(null);
    preparingFollowUpTaskRef.current = taskId;
    setPreparingFollowUpTask(taskId);
    void dockStore.openForFollowUp(taskId).then((state) => {
      if (generation !== followUpCaptureGeneration.current) return;
      preparingFollowUpTaskRef.current = null;
      setPreparingFollowUpTask(null);
      const detail = state?.detail;
      const chat = state?.chat;
      const result = state?.result;
      const reference = chat?.messages[1]?.message;
      if (!state || state.selectedId !== taskId || state.chatStatus !== 'saved' || state.loading ||
          !detail || !detail.actions.can_follow_up || detail.investigation_id !== taskId || !detail.has_result ||
          !result || result.investigation_id !== taskId || !chat || chat.task_id !== taskId ||
          !detail.conversation_id || chat.messages.length !== 2 || reference?.kind !== 'result_reference' ||
          reference.task_id !== taskId || reference.result_id !== result.result_id || chat.messages[0]?.message.kind !== 'user_question') {
        cancelFollowUpCapture();
        setFollowUpError(state?.error ?? state?.chatError ?? 'Refresh this completed saved task before starting a follow-up.');
        return;
      }
      setFollowUpContext({
        parent_id: detail.investigation_id, conversation_id: detail.conversation_id,
        specialist_id: detail.specialist.id,
        scope: detail.scope.type === 'all' ? { type: 'all' } : detail.scope.type === 'area' ? { type: 'area', id: detail.scope.id }
          : { type: 'neighborhood', anchor: detail.scope.anchor, hops: detail.scope.hops },
        provider_mode: detail.provider.mode, saved_provider_id: chat.producer.provider_id, saved_model: chat.producer.model,
      });
      activeFollowUpParentRef.current = taskId;
      setQuickDispatchOpenRequest((request) => request + 1);
    }).catch(() => {
      if (generation !== followUpCaptureGeneration.current) return;
      preparingFollowUpTaskRef.current = null;
      setPreparingFollowUpTask(null);
      cancelFollowUpCapture();
      setFollowUpError('The saved parent task could not be refreshed. Try again before starting a follow-up.');
    });
  }, [cancelFollowUpCapture]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (view !== 'agent-chats') cancelFollowUpCapture();
    if (view !== 'agent-chats') useAgentChatDockStore.getState().cancelNavigationIntent();
    if (view === 'investigations' || view === 'agent-chats') {
      if (view === 'investigations') {
        useSpecialistActivityStore.getState().setVisible(true);
      }
      // Investigations needs the catalog to start work; the agent pool shows its providers.
      void useInvestigationStore.getState().loadCatalog();
      if (view === 'agent-chats') void useAppStore.getState().loadStagedProposals();
      void useInvestigationStore.getState().loadHistory();
    }
    return () => useSpecialistActivityStore.getState().setVisible(false);
  }, [cancelFollowUpCapture, view]);

  // The agent pool's reported model use (AC-1703), re-read as job rows change.
  useEffect(() => {
    if (view !== 'agent-chats') return;
    let current = true;
    void loadJobUsage().then(
      (rows) => { if (current) setJobUsage(rows); },
      () => { if (current) setJobUsage(null); },
    );
    return () => { current = false; };
  }, [jobs, view]);

  useEffect(() => {
    if (view !== 'agent-chats') return;
    let current = true;
    void loadAgentPoolControls().then((stored) => { if (current) setPoolControls(stored); }, () => undefined);
    return () => { current = false; };
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
        // ADR-0074: a curation review changes what the curated reads show,
        // so a bundle on screen is read again.
        const stopCurations = await listen<import('./findingCurations').FindingCurationChanged>('investigation-curation://changed', (event) => {
          useInvestigationStore.getState().invalidateCuration(event.payload);
          if (useAppStore.getState().specBundle) void useAppStore.getState().reloadSpec();
        });
        const stopReviewEvents = () => { stopReviews(); stopCurations(); };
        if (disposed) stopReviewEvents(); else unlisten = stopReviewEvents;
        const stop = await listen<InvestigationChanged>('investigation://changed', (event) => {
          useInvestigationStore.getState().invalidate(event.payload);
          useSpecialistActivityStore.getState().invalidate();
          useSavedResultNoticeStore.getState().observe(event.payload);
        });
        if (disposed) stop(); else unlisten = () => { stop(); stopReviewEvents(); };
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
    // Workspace names coverage sources from the registry too (#2471): a source
    // with no recovered facts is not in the system contents.
    if (view === 'settings' || view === 'workspace') void usePrimarySourceStore.getState().loadSources();
  }, [view, systemContents]);

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

  // The Workbench document view (SPEC-15 slice 7, AC-0669): read while the
  // Workbench is open, and the curated projection is read again after a
  // review so an accepted draft's state is the host's.
  const sectionDocument = useSectionDocumentStore();
  useEffect(() => {
    if (view === 'spec' && backend === 'up') void useSectionDocumentStore.getState().load();
  }, [view, backend]);
  // "Draft this section" opens the investigation composer on the Section
  // Writer with the one question that names the address. The reviewer still
  // chooses the evidence, the provider and the start there.
  const draftSection = useCallback(async (request: SectionAddress | StoredSectionDraft) => {
    try {
      // A re-draft names the accepted draft; the host checks its state.
      const question = 'draft_id' in request
        ? await sectionRedraftQuestion(request)
        : await sectionDraftQuestion(request);
      useInvestigationStore.getState().updateComposer({ specialistId: 'section-writer@1', question });
      navigate('investigations');
    } catch (error) {
      reportSpecError(error instanceof Error ? error.message : String(error));
    }
  }, [navigate]);

  // "Compose from the knowledge graph" (#1991, AC-0862): the exact payload
  // is shown for one-action consent; only that payload is sent. The staged
  // draft then waits for review like any other.
  const [composing, setComposing] = useState<{ address: SectionAddress; preview: EgressPreview } | null>(null);
  const [composeBusy, setComposeBusy] = useState(false);
  const [composeNotice, setComposeNotice] = useState<string | null>(null);
  const composeSection = useCallback(async (address: SectionAddress) => {
    setComposeNotice(null);
    setComposeBusy(true);
    try {
      setComposing({ address, preview: await sectionComposePreview(address) });
    } catch (error) {
      reportSpecError(error instanceof Error ? error.message : String(error));
    } finally {
      setComposeBusy(false);
    }
  }, []);
  const consentCompose = useCallback(async (preview: EgressPreview) => {
    if (!composing) return;
    const { address } = composing;
    setComposing(null);
    setComposeBusy(true);
    setComposeNotice(`Composing section ${address.section}…`);
    try {
      const outcome = await runSectionCompose(address, preview);
      setComposeNotice(`Section ${address.section}: ${composeOutcomeText(outcome)}`);
      await useSectionDocumentStore.getState().load();
    } catch (error) {
      setComposeNotice(null);
      reportSpecError(error instanceof Error ? error.message : String(error));
    } finally {
      setComposeBusy(false);
    }
  }, [composing]);

  // Gated §1.5 drafting on the local model (#2083, AC-0980): no payload
  // leaves the machine, so it starts at once.
  const composeGated = useCallback(async (address: SectionAddress) => {
    setComposeBusy(true);
    setComposeNotice(`Drafting section ${address.section} locally, ten rule rows at a time…`);
    try {
      const drafts = await runSectionComposeGated(address);
      setComposeNotice(`Section ${address.section}: ${gatedRunText(drafts)}`);
      await useSectionDocumentStore.getState().load();
    } catch (error) {
      setComposeNotice(null);
      reportSpecError(error instanceof Error ? error.message : String(error));
    } finally {
      setComposeBusy(false);
    }
  }, []);

  // Ask an area's expert (#2057, AC-0932): the same one-action consent.
  const areaExpert = useAreaExpert(view === 'spec' && backend === 'up');

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
      : view === 'investigations' && investigations.detail?.scope.type === 'area'
        ? { kind: 'trail', label: 'Investigation · Area' }
      : { kind: 'system', label: 'Whole system' };
  // Atlas names repositories as Workspace does, not by `local/src_<hash>` keys (#1601).
  const atlasRepoLabel = useMemo(() => createRepositoryLabeler(systemContents), [systemContents]);
  // Evidence-backed contents survive relaunch; synthetic nodes name no system
  // (AC-0085). The breadcrumb names the recovered system, as Workspace does (#2469).
  const systemName = useMemo(() => systemDisplayName(systemContents), [systemContents]);
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
              loading={backend === 'unknown'}
              systemContents={systemContents}
              registeredSources={primarySource.sources}
              summary={ingestSummary}
              recordedCoverage={recordedCoverage}
              recordedCoverageError={recordedCoverageError}
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
                newProject={{
                  can:
                    backend === 'up' &&
                    !ingestBusy &&
                    !jobs.some((job) => job.status === 'queued' || job.status === 'running'),
                  starting: newProjectBusy,
                  error: newProjectError,
                  onStart: () => void startNewProject(),
                }}
              />
              <ProjectsCard
                projects={projects}
                loadedRepos={systemContents.map((entry) => entry.display_name ?? entry.repo)}
                canSwitch={
                  backend === 'up' &&
                  !ingestBusy &&
                  !jobs.some((job) => job.status === 'queued' || job.status === 'running')
                }
                busy={projectsBusy}
                error={projectsError}
                onSwitch={(id) => void switchProject(id)}
                onRename={(id, name) => void renameProject(id, name)}
                onDelete={(id) => void deleteProject(id)}
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
        return <>
          <AgentPoolPanel
            jobs={jobs}
            providers={investigations.catalog?.providers ?? null}
            usage={jobUsage}
            controls={poolControls}
            controlsError={poolControlsError}
            onChangeControls={changePoolControls}
            localServer={localServer}
            localServerChecking={localServerChecking}
            localServerError={localServerError}
            onCheckLocalServer={backend === 'up' ? checkLocalServer : undefined}
            chats={investigations.historyError || (investigations.historyLoading && investigations.history.length === 0)
              ? null : chatSessions(investigations.history, chatDocks.map((item) => item.turn.conversation_id))}
            onOpenChat={(id) => useAgentChatDockStore.getState().open(id)}
            awaitingCuration={stagedError || (stagedLoading && stagedProposals.length === 0) ? null : awaitingCuration(stagedProposals)}
            onOpenJobs={() => navigate('jobs')}
            onOpenCuration={() => navigate('gaps')}
            onViewInvestigation={(id) => { navigate('investigations'); void investigations.open(id); }}
          />
          <AgentChatsSurface state={investigations} onOpenResult={(id) => {
          navigate('investigations');
          void useInvestigationStore.getState().open(id);
        }} onFollowUp={prepareParentFollowUp} onReplyHost={registerReplyHost} onCancelFollowUp={cancelFollowUpCapture}
        preparingFollowUpTask={preparingFollowUpTask} followUpError={followUpError} noticeCorner={noticeCorner}
        onNoticeCornerChange={(corner) => useSavedResultNoticeStore.getState().setCorner(corner)} />
        </>;
      case 'atlas':
        return (
          <Suspense fallback={<section className="atlas-card">Loading Atlas graph…</section>}>
            <AtlasCanvas
              snapshot={atlas}
              onSelect={(node) => void select(node)}
              onSelectEdge={selectEdgeSubject}
              onLayerChange={setAtlasLayer}
              repoLabel={atlasRepoLabel}
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
              readSelectedFlowView={readSelectedFlowView}
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
              curated={specCurated}
              onRefreshCurated={() => void reloadSpec()}
              sections={{
                document: sectionDocument.document,
                busy: sectionDocument.busy || specBusy || composeBusy,
                notice: composeNotice,
                error: sectionDocument.error,
                canReview: backend === 'up',
                onReload: () => void sectionDocument.load(),
                onReview: (draft, decision, note) =>
                  void sectionDocument.review(draft, decision, note).then((recorded) => {
                    if (recorded) void reloadSpec();
                  }),
                onDraft: (address) => void draftSection(address),
                onRedraft: (draft) => void draftSection(draft),
                onCompose: (address) => void composeSection(address),
                onComposeGated: (address) => void composeGated(address),
              }}
              expert={{
                areas: areaExpert.areas,
                selected: areaExpert.selected,
                exchanges: areaExpert.exchanges,
                busy: areaExpert.busy,
                error: areaExpert.error,
                canAsk: backend === 'up',
                onReload: areaExpert.reload,
                onSelect: areaExpert.select,
                onAsk: areaExpert.ask,
              }}
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
              onExportBundle={(bundle) => void exportSpecBundle(bundle, readSpecArtifacts, reportSpecError)}
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
              recordStates: curatedStates,
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
            manifestGuide={{
              preview: (draft) => invoke<string>('preview_system_manifest', { draft }),
              save: (draft) => invoke<string | null>('save_system_manifest', { draft }),
            }}
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
            system={systemPreflight}
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
            onOpenQuickDispatch={openStandaloneQuickDispatch}
            quickDispatchHintShown={quickDispatchHintShown}
            onQuickDispatchHintChange={showQuickDispatchHint}
          />
          <ProviderKeys available={backend === 'up'} onEgressRecorded={() => {
            invoke<import('./store').EgressSummary | null>('egress_summary')
              .then((egress) => useAppStore.setState({ egress }), () => { /* the status bar keeps its last total */ });
          }} />
          <LocalLane available={backend === 'up'} />
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
        knownUnreadSavedResults={knownUnreadSavedResults}
        unreadChecksIncomplete={unreadChecksIncomplete}
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
          <KnowledgePhaseNotice available={backend === 'up'} />
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
      <InvestigationQuickDispatch fallbackEnabled={quickDispatchFallbackEnabled} openRequest={quickDispatchOpenRequest}
        followUpContext={followUpContext} inlineReply={inlineReply}
        onStandalone={openStandaloneQuickDispatch} onCloseParent={cancelFollowUpCapture}
        followUpCancelRequest={followUpCancelRequest}
        onNavigate={navigate} onAcceptedTask={(taskId) => {
          showQuickDispatchHint(false);
          activeFollowUpParentRef.current = null;
          setFollowUpContext(null);
          inlineReplyTask.current = null; setInlineReply(null);
          useAgentChatDockStore.getState().requestOpenFromNotice(taskId);
          navigate('agent-chats');
        }} />
      {quickDispatchHintShown && !HINTLESS_VIEWS.has(view) && (
        <QuickDispatchHint fallbackEnabled={quickDispatchFallbackEnabled}
          onOpen={openStandaloneQuickDispatch} onDismiss={() => showQuickDispatchHint(false)} />
      )}
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
              observed={capturedSelectionMatches ? primarySource.observed : null}
              observedText={capturedSelectionMatches ? primarySource.observedText : null}
              observedError={capturedSelectionMatches ? primarySource.observedError : null}
              onReadObserved={(index) => void primarySource.readObservedRange(index)}
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
      {composing && (
        <EgressConsentDialog
          preview={composing.preview}
          busy={composeBusy}
          cancelLabel="Cancel"
          onCancel={() => setComposing(null)}
          onConsent={(preview) => void consentCompose(preview)}
        />
      )}
      {areaExpert.pending && (
        <EgressConsentDialog
          preview={areaExpert.pending.preview}
          busy={areaExpert.busy}
          cancelLabel="Cancel"
          onCancel={areaExpert.cancel}
          onConsent={areaExpert.consent}
        />
      )}
      {escalation && (
        <ResolutionStrategyModal
          state={escalation}
          recordState={escalation.proposal ? curatedStates.get(escalation.proposal.proposal_id) : undefined}
          onRun={(strategyId) => void runStrategy(strategyId)}
          onConsent={(preview) => void consentAndRun(preview)}
          onDismissPreview={dismissPreview}
          onDecide={(decision) => void decideProposal(decision)}
          onClose={closeResolution}
          onAssessBasis={() => void assessStagedBasis()}
          onWithdraw={() => void withdrawProposal()}
          onResolveAgain={(gapId) => void openResolution(gapId)}
        />
      )}
    </div>
  );
}
