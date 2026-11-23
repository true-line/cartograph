import actualAdmission from '../fixtures/completion-v10-supported.json';
import { acquisitionCatalog, admissionCatalog, admissionDetail, admissionResult, queryHistoryCatalog, queryHistoryDetail, queryHistoryResult,
  expandedCompletionCatalog, completionCatalog, completionChoices, completionDetail, completionResult,
  gapTriagerDetail, gapTriagerResult, inventoryObligation, sourceObligation, specWriterDetail, specWriterResult,
  specWriterV2Detail, specWriterV2Result } from '../completionFixtures';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { InvestigationsSurface } from './InvestigationsSurface';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import { useSpecialistActivityStore } from '../specialistActivityStore';
import backendQualified from '../fixtures/investigation-coverage-backend.json';
import { qualifiedCatalog, qualifiedDetail, qualifiedResult, inspectionAssessment, inspectionCatalog, inspectionDetail, inspectionTarget, investigationCatalog, investigationCitations, investigationConsent, investigationDetail, investigationEvents, investigationResult, legacyInvestigationDetail, legacyInvestigationResult } from '../investigationFixtures';
import type { InvestigationChat, InvestigationDetail, InvestigationResult, InvestigationSpecialist, SpecialistId } from '../investigationTypes';

const state = (overrides: Partial<InvestigationState> = {}): InvestigationState => ({
  ...useInvestigationStore.getInitialState(), catalog: investigationCatalog, history: [investigationDetail()],
  selectedId: 'inv-fixture', detail: investigationDetail(), result: investigationResult(), events: investigationEvents(),
  start: fn(async () => null), retryStart: fn(async () => null), open: fn(async () => {}),
  approve: fn(async () => {}), decline: fn(async () => {}), cancel: fn(async () => {}),
  hideConsent: fn(), showConsent: fn(), loadHistory: fn(async () => {}), loadCatalog: fn(async () => {}),
  refreshSelected: fn(async () => {}), readCitation: fn(async () => {}), clearCitation: fn(), ...overrides,
});

const meta = { title: 'Surfaces/InvestigationsSurface', component: InvestigationsSurface,
  beforeEach: () => {
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  render: function ComposerStory(args) {
    const { composer, updateComposer } = useInvestigationStore();
    return <InvestigationsSurface {...args} activity={useSpecialistActivityStore()}
      onRefreshActivity={() => void useSpecialistActivityStore.getState().refresh()}
      state={{ ...args.state, composer, updateComposer }} />;
  },
  args: { state: state(), anchors: [], canStart: true } } satisfies Meta<typeof InvestigationsSurface>;
export default meta;
type Story = StoryObj<typeof meta>;
let conversationDetailReads: string[] = [];
let openedActivityDetailReads: string[] = [];
let openedActivityResultReads: string[] = [];
let openedActivityChatReads: string[] = [];
let openedActivityCitationReads: string[] = [];
let activityRefreshResponse: (() => unknown) | null = null;

const productionSpecialistIds = [
  'domain-analyst@3', 'evidence-auditor@3', 'domain-analyst@4', 'evidence-auditor@4',
  'domain-analyst@5', 'evidence-auditor@5', 'domain-analyst@6', 'evidence-auditor@6',
  'domain-analyst@7', 'evidence-auditor@7', 'domain-analyst@10', 'evidence-auditor@10',
  'spec-writer@1', 'gap-triager@1',
] as const satisfies readonly SpecialistId[];

const productionRosterCatalog = {
  ...investigationCatalog,
  specialists: productionSpecialistIds.map((id): InvestigationSpecialist => {
    const [family, version] = id.split('@');
    const name = family === 'domain-analyst' ? 'Domain analyst' : family === 'evidence-auditor' ? 'Evidence auditor' :
      family === 'spec-writer' ? 'Spec writer' : 'Gap triager';
    return { id, name, version: Number(version), prompt_fingerprint: `fixture:${id}`,
      purpose: 'Production roster entry for the complete empty-activity observation story.',
      operations: ['query_context', 'read_evidence', 'finish'], tier: 'Agentic', confidence_tier: 'InferredWeak' };
  }),
};

export const ScopedSpecialistQuestion: Story = {
  // AC-0180/0190: exact host specialist, chosen graph scope and provider form the request.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('radio', { name: /Evidence auditor/ }));
    await userEvent.selectOptions(canvas.getByLabelText('Scope'), 'neighborhood');
    await userEvent.type(canvas.getByLabelText('Anchor fact ID'), 'rule:stock');
    await userEvent.selectOptions(canvas.getByLabelText('Hops'), '2');
    await userEvent.type(canvas.getByLabelText('Question'), 'Which evidence supports this rule?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith({ specialist_id: 'evidence-auditor@3',
      question: 'Which evidence supports this rule?', scope: { type: 'neighborhood', anchor: 'rule:stock', hops: 2 },
      provider_mode: 'local', limit_profile: investigationCatalog.limits.profile });
    await expect(canvas.getByText('Findings cover the inspected scope; knowledge remains partial.')).toBeVisible();
  },
};

export const PerStepCloudConsent: Story = {
  // AC-0186/0190: close, decline and approve are separate exact-step actions.
  args: { state: state({ consent: investigationConsent(), consentOpen: true,
    detail: investigationDetail('inv-fixture', { provider_mode: 'cloud', status: 'awaiting_consent', revision: 4,
      actions: { can_cancel: true, can_follow_up: false } }) }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const dialog = within(canvas.getByRole('dialog', { name: 'Review exact model payload' }));
    await expect(dialog.getByText('payload:inv-fixture:step-1')).toBeVisible();
    await expect(dialog.getByText(/Retained fixture input approved for this step/)).toBeVisible();
    await expect(dialog.getByText(/actual deadline may narrow/)).toBeVisible();
    await expect(dialog.queryByRole('button', { name: 'Keep local' })).not.toBeInTheDocument();
    await userEvent.click(dialog.getByRole('button', { name: 'Review later' }));
    await expect(args.state.hideConsent).toHaveBeenCalledTimes(1);
    await expect(args.state.approve).not.toHaveBeenCalled();
    await expect(args.state.decline).not.toHaveBeenCalled();
    await userEvent.click(dialog.getByRole('button', { name: 'Decline this action' }));
    await expect(args.state.decline).toHaveBeenCalledTimes(1);
  },
};

export const ScopeIdentity: Story = {
  // #2475: the whole-graph identity is labelled as such; the scope's own
  // snapshot shows whether the scope narrowed it.
  args: { state: state({ detail: investigationDetail('inv-fixture') }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Recovered graph:/)).toHaveTextContent('snapshot:original');
    await expect(canvas.getByText(/Scope snapshot:/)).toHaveTextContent('scope:original');
    await expect(canvas.getByText(/narrower than the recovered graph/)).toBeVisible();
    await expect(canvas.queryByText(/Prepared graph/)).not.toBeInTheDocument();
  },
};

export const WholeSystemScopeIdentity: Story = {
  args: { state: state({ detail: investigationDetail('inv-fixture', { scope: { type: 'all' },
    scope_snapshot_id: 'snapshot:original' }) }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/the whole recovered graph/)).toBeVisible();
  },
};

export const CancelledOutstandingInvocation: Story = {
  args: { state: state({ detail: investigationDetail('inv-fixture', { status: 'cancelled', cancel_requested: true,
    invocation_pending: true, actions: { can_cancel: false, can_follow_up: false } }) }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/A model request is outstanding/)).toBeVisible();
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await expect(canvas.queryByRole('button', { name: 'Ask a follow-up' })).not.toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: 'Request cancellation' })).not.toBeInTheDocument();
  },
};

export const UnknownOutcomeRequiresNewTask: Story = {
  args: { state: state({ result: null, detail: investigationDetail('inv-fixture', { status: 'outcome_unknown', has_result: false }) }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/will not be replayed automatically/)).toBeVisible();
    await expect(canvas.queryByRole('button', { name: /^(Retry|Resume)$/ })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Ask a follow-up' }));
    await userEvent.type(canvas.getByLabelText('Follow-up question'), 'Re-examine the evidence with a new basis.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ parent_id: 'inv-fixture',
      conversation_id: 'conversation-inv-fixture', question: 'Re-examine the evidence with a new basis.' }));
  },
};

export const HistoryAndReconnection: Story = {
  args: { state: state({ error: 'The investigation could not be refreshed consistently. Saved findings and activity remain visible; refresh to reconnect.',
    nextCursor: 'opaque-next-page' }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh selected investigation' }));
    await expect(args.state.refreshSelected).toHaveBeenCalledTimes(1);
    await userEvent.click(canvas.getByRole('button', { name: 'Load older investigations' }));
    await expect(args.state.loadHistory).toHaveBeenCalledWith(true);
  },
};

export const SearchesPersistedHistory: Story = {
  // AC-0368: the surface asks the backend for persisted matches and never searches only loaded rows.
  beforeEach: () => {
    const matching = investigationDetail('older-match', { question: 'Legacy needle in a saved question' });
    const nonmatching = investigationDetail('unrelated', { question: 'Unrelated saved question' });
    mockIPC((command, args) => {
      const search = (args as { search?: unknown } | undefined)?.search;
      if (command === 'list_investigations') return { items: search ? [matching] : [matching, nonmatching], next_cursor: null,
        ...(search === 'guarded exit' ? { answer_match_ids: [matching.investigation_id] } : {}),
        ...(search === 'Domain analyst' ? { name_match_ids: [matching.investigation_id] } : {}) };
      if (command === 'get_investigation') return matching;
      if (command === 'investigation_result') return investigationResult(matching.investigation_id);
      if (command === 'investigation_events') return { investigation_id: matching.investigation_id,
        items: investigationEvents(matching.investigation_id), next_sequence: 9, has_more: false };
      return null;
    });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog,
      history: [], selectedId: null, detail: null }, true);
    return () => clearMocks();
  },
  render: function SearchHistoryStory() {
    return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('searchbox'), 'LEGACY NEEDLE');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getByText('Legacy needle in a saved question')).toBeVisible();
    await expect(canvas.queryByText('Unrelated saved question')).not.toBeInTheDocument();
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['older-match']);
  },
};

export const ActiveInvestigations: Story = {
  // AC-0372: Active is selected by the host and opens the exact saved task.
  beforeEach: () => {
    const active = investigationDetail('awaiting-consent-active', { status: 'awaiting_consent', provider_mode: 'cloud',
      has_result: false, revision: 12, updated_at: '2026-10-25T14:30:00Z',
      actions: { can_cancel: true, can_follow_up: false } });
    mockIPC((command, args) => {
      if (command === 'list_investigations') {
        expect(args).toEqual({ activeOnly: true });
        return { items: [active], next_cursor: null };
      }
      if (command === 'get_investigation') return active;
      if (command === 'investigation_result') return null;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: active.investigation_id,
        items: investigationEvents(active.investigation_id), next_sequence: active.last_event_sequence, has_more: false };
      return null;
    });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog }, true);
    return () => clearMocks();
  },
  render: function ActiveHistoryStory() {
    return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Active investigations' }));
    const row = await canvas.findByRole('button', { name: /What stock behavior is supported/ });
    await expect(row).toHaveTextContent('Awaiting cloud consent');
    await expect(row).toHaveTextContent('Provider: cloud');
    await expect(row).toHaveTextContent('Updated 2026-10-25T14:30:00Z');
    await userEvent.click(row);
    await waitFor(() => expect(useInvestigationStore.getState().selectedId).toBe('awaiting-consent-active'));
    await waitFor(() => expect(useInvestigationStore.getState().detail?.investigation_id).toBe('awaiting-consent-active'));
  },
};

export const SpecialistTaskActivity: Story = {
  // AC-0381: activity shows saved provider identity, not current catalog values.
  beforeEach: () => {
    activityRefreshResponse = null;
    const active = [
      investigationDetail('domain-running-activity', { specialist_id: 'domain-analyst@3', status: 'running', has_result: false,
        provider: { ...investigationCatalog.providers[0], provider_id: 'saved-local-provider', model: 'saved-local-model' } }),
      investigationDetail('domain-queued-activity', { specialist_id: 'domain-analyst@3', status: 'queued', has_result: false }),
      investigationDetail('auditor-preparing-activity', { specialist_id: 'evidence-auditor@3', status: 'preparing', has_result: false }),
      investigationDetail('auditor-consent-activity', { specialist_id: 'evidence-auditor@3', status: 'awaiting_consent',
        cancel_requested: true, has_result: false, provider_mode: 'cloud',
        provider: { ...investigationCatalog.providers[1], provider_id: 'saved-cloud-provider', model: 'saved-cloud-model' } }),
      investigationDetail('old-domain-activity', { specialist_id: 'domain-analyst@2', status: 'queued', has_result: false }),
    ];
    let activityReads = 0;
    mockIPC((command, args) => {
      if (command === 'list_specialist_task_activity') {
        expect(args).toEqual({ cursor: null });
        const page = { items: active.map((summary, index) => ({ summary,
          ...(index === 0 ? { reported_cache_read_tokens: '18446744073709551615' }
            : index === 1 ? { reported_cache_read_tokens: '0' }
              : index === 2 ? { reported_cache_read_tokens: null } : {}),
          ...(index === 0 ? { reported_usage: { input_tokens: '18446744073709551615', output_tokens: null } }
            : index === 3 ? { reported_usage: { input_tokens: null, output_tokens: '42' } } : {}), saved_provider: {
          mode: summary.provider.mode, provider_id: summary.provider.provider_id, model: summary.provider.model,
        } })), next_cursor: 'older-active-page' };
        return activityReads++ === 0 ? page : activityRefreshResponse?.() ?? page;
      }
      return null;
    });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog }, true);
    useSpecialistActivityStore.getState().setVisible(true);
    return () => { useSpecialistActivityStore.getState().setVisible(false); clearMocks(); };
  },
  render: function ActivityStory() {
    return <InvestigationsSurface state={useInvestigationStore()} activity={useSpecialistActivityStore()}
      onRefreshActivity={() => void useSpecialistActivityStore.getState().refresh()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const activityElement = canvas.getByRole('region', { name: 'Active specialist tasks' });
    const activityPanel = within(activityElement);
    await canvas.findByText(/Running/);
    await canvas.findByText(/Preparing context/);
    await canvas.findByText(/Awaiting cloud consent/);
    const domain = canvas.getByRole('heading', { name: /domain-analyst@3/ }).closest('section');
    const auditor = canvas.getByRole('heading', { name: /evidence-auditor@3/ }).closest('section');
    expect(domain).not.toBeNull();
    expect(auditor).not.toBeNull();
    expect(activityElement.querySelectorAll('.specialist-activity-group')).toHaveLength(2);
    await expect(domain).toHaveTextContent('Running');
    await expect(domain).toHaveTextContent('Queued');
    await expect(domain).toHaveTextContent('Saved provider: local / saved-local-provider · saved-local-model');
    await expect(auditor).toHaveTextContent('Awaiting cloud consent');
    await expect(auditor).toHaveTextContent('Cancellation requested');
    await expect(auditor).toHaveTextContent('Saved provider: cloud / saved-cloud-provider · saved-cloud-model');
    // AC-0417: saved reports stay with their distinct model rows, including unknown/absent values.
    await expect(domain).toHaveTextContent('Reported tokens so far: input 18446744073709551615; output Unknown.');
    await expect(auditor).toHaveTextContent('Reported tokens so far: input Unknown; output 42.');
    await expect(activityPanel.getAllByText(/Reported tokens unavailable/)).toHaveLength(2);
    await expect(domain).toHaveTextContent('May be incomplete; not a cost estimate.');
    // AC-0419: different saved tasks retain exact cache values, unknown and absence.
    await expect(domain).toHaveTextContent('Provider-reported cache-read tokens so far: 18446744073709551615.');
    await expect(domain).toHaveTextContent('Provider-reported cache-read tokens so far: 0.');
    await expect(auditor).toHaveTextContent('Provider-reported cache-read tokens so far: Unknown.');
    await expect(auditor).toHaveTextContent('Provider-reported cache-read tokens so far: Unavailable.');
    await expect(activityPanel.getByText('Partial activity counts: queued 1, preparing 1, running 1, awaiting consent 1; cancellation requested 1 of 4 active tasks shown. More active tasks may exist.')).toBeVisible();
    await expect(activityPanel.findByText(/active task list is incomplete/i)).resolves.toBeVisible();
    await expect(activityPanel.getAllByRole('status')).toHaveLength(1);
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().selectedId).toBeNull();

    const tasks = useSpecialistActivityStore.getState().tasks;
    let resolveRefresh!: (page: { items: typeof tasks; next_cursor: string | null }) => void;
    activityRefreshResponse = () => new Promise((resolve) => { resolveRefresh = resolve; });
    const refreshPromise = useSpecialistActivityStore.getState().refresh();
    await waitFor(() => expect(useSpecialistActivityStore.getState().refreshing).toBe(true));
    await waitFor(() => {
      const refreshedDomain = canvas.getByRole('heading', { name: /domain-analyst@3/ }).closest('section');
      expect(refreshedDomain).not.toBeNull();
      expect(refreshedDomain).toHaveTextContent('Running');
      expect(refreshedDomain).not.toHaveTextContent(/refreshing/i);
    });
    await expect(activityPanel.getByText('Last observed partial activity counts: queued 1, preparing 1, running 1, awaiting consent 1; cancellation requested 1 of 4 active tasks shown. More active tasks may exist.')).toBeVisible();
    await expect(activityPanel.getByText(/Reported tokens so far: input 18446744073709551615/)).toBeVisible();
    await expect(activityPanel.getByText(/Provider-reported cache-read tokens so far: 18446744073709551615/)).toBeVisible();
    expect(useSpecialistActivityStore.getState().tasks[0].reported_cache_read_tokens).toBe('18446744073709551615');
    await expect(activityPanel.getAllByRole('status')).toHaveLength(1);
    await expect(activityPanel.getAllByRole('button', { name: /Open investigation:/ })).toHaveLength(4);
    resolveRefresh({ items: tasks.filter((task) => task.summary.specialist_id !== 'domain-analyst@2'), next_cursor: null });
    await refreshPromise;
    await expect(activityPanel.findByText('Shown activity counts: queued 1, preparing 1, running 1, awaiting consent 1; cancellation requested 1 of 4 active tasks shown.')).resolves.toBeVisible();

    useSpecialistActivityStore.setState({ status: 'loading', tasks, error: null, refreshing: true });
    await expect(activityPanel.findByText('Checking activity…')).resolves.toBeVisible();
    await expect(activityPanel.getAllByRole('status')).toHaveLength(1);
    expect(activityElement.querySelector('.specialist-activity-counts')).toBeNull();
    expect(activityElement.querySelectorAll('.specialist-activity-group')).toHaveLength(0);
    useSpecialistActivityStore.setState({ status: 'preview', tasks, error: null, refreshing: false });
    await expect(activityPanel.findByText('Activity shown in desktop app')).resolves.toBeVisible();
    await expect(activityPanel.getAllByRole('status')).toHaveLength(1);
    expect(activityElement.querySelector('.specialist-activity-counts')).toBeNull();
    expect(activityElement.querySelectorAll('.specialist-activity-group')).toHaveLength(0);
    activityRefreshResponse = () => { throw new Error('refresh failed'); };
    await useSpecialistActivityStore.getState().refresh();
    await expect(activityPanel.findByText('Activity unknown')).resolves.toBeVisible();
    expect(useSpecialistActivityStore.getState().tasks[0].reported_cache_read_tokens).toBe('18446744073709551615');
    expect(activityPanel.queryByText(/Provider-reported cache-read tokens so far:/)).toBeNull();
    await expect(activityPanel.getAllByRole('status')).toHaveLength(1);
    expect(activityElement.querySelector('.specialist-activity-counts')).toBeNull();
    expect(activityPanel.queryByRole('button', { name: /Open investigation:/ })).toBeNull();
    expect(activityPanel.queryByText('No active tasks')).toBeNull();
  },
};

export const CompleteEmptyProductionRoster: Story = {
  beforeEach: () => {
    mockIPC((command, args) => {
      if (command === 'list_specialist_task_activity') {
        expect(args).toEqual({ cursor: null });
        return { items: [], next_cursor: null };
      }
      return null;
    });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: productionRosterCatalog }, true);
    useSpecialistActivityStore.getState().setVisible(true);
    return () => { useSpecialistActivityStore.getState().setVisible(false); clearMocks(); };
  },
  render: function EmptyActivityStory() {
    return <InvestigationsSurface state={useInvestigationStore()} activity={useSpecialistActivityStore()}
      onRefreshActivity={() => void useSpecialistActivityStore.getState().refresh()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const activityElement = canvas.getByRole('region', { name: 'Active specialist tasks' });
    const activityPanel = within(activityElement);
    await expect(productionRosterCatalog.specialists).toHaveLength(14);
    await expect(activityPanel.findByText('No active tasks')).resolves.toBeVisible();
    await expect(activityPanel.findByText('Shown activity counts: queued 0, preparing 0, running 0, awaiting consent 0; cancellation requested 0 of 0 active tasks shown.')).resolves.toBeVisible();
    await expect(activityPanel.getAllByText('No active tasks')).toHaveLength(1);
    await expect(activityElement.querySelectorAll('.specialist-activity-group')).toHaveLength(0);
    await expect(activityPanel.getAllByRole('status')).toHaveLength(1);
  },
};

export const OpenActiveInvestigation: Story = {
  // AC-0379: opening one activity row uses that task's existing saved-detail path.
  beforeEach: () => {
    const first = investigationDetail('active-first', { specialist_id: 'domain-analyst@3', status: 'running', has_result: false,
      question: 'First active question', updated_at: '2026-10-26T10:00:00Z' });
    const second = investigationDetail('active-second', { specialist_id: 'domain-analyst@3', status: 'awaiting_consent', has_result: false,
      question: 'Second active question', updated_at: '2026-10-26T10:01:00Z' });
    const savedSecond = investigationDetail('active-second', { specialist_id: 'domain-analyst@3', status: 'completed', has_result: true,
      question: 'Second active question', updated_at: '2026-10-26T10:02:00Z' });
    const result = investigationResult(savedSecond.investigation_id);
    const chat: InvestigationChat = { chat_id: `chat:${savedSecond.investigation_id}`, task_id: savedSecond.investigation_id,
      producer: { specialist_id: savedSecond.specialist.id, specialist_version: savedSecond.specialist.version,
        prompt_fingerprint: savedSecond.specialist.prompt_fingerprint, tier: savedSecond.specialist.tier,
        confidence_tier: savedSecond.specialist.confidence_tier, provider_mode: savedSecond.provider.mode,
        provider_id: savedSecond.provider.provider_id, model: savedSecond.provider.model,
        protocol_version: savedSecond.provider.protocol_version ?? null },
      messages: [{ id: 'active-second-question', ordinal: 0, author: 'user',
        message: { kind: 'user_question', question: savedSecond.question } },
      { id: 'active-second-result', ordinal: 1, author: 'specialist',
        message: { kind: 'result_reference', task_id: savedSecond.investigation_id,
          result_id: result.result_id, result_hash: 'e'.repeat(64) } }] };
    openedActivityDetailReads = [];
    openedActivityResultReads = [];
    openedActivityChatReads = [];
    openedActivityCitationReads = [];
    const pendingStart = { schema_version: 1 as const, request_nonce: 'preserved-pending-nonce', specialist_id: 'domain-analyst@3' as const,
      question: 'Uncertain start retained for retry.', scope: { type: 'all' as const }, provider_mode: 'local' as const,
      limit_profile: 'investigation-v1' as const };
    const composer = { ...useInvestigationStore.getInitialState().composer, specialistId: 'evidence-auditor@3' as const,
      question: 'Keep the composer draft intact.' };
    mockIPC((command, payload) => {
      const args = payload as { investigationId?: string; citationId?: string; afterSequence?: number } | undefined;
      if (command === 'list_specialist_task_activity') return { items: [first, second].map((summary) => ({
        summary, saved_provider: { mode: summary.provider_mode, provider_id: summary.provider.provider_id, model: summary.provider.model },
      })), next_cursor: null };
      if (command === 'get_investigation') {
        if (args?.investigationId) openedActivityDetailReads.push(args.investigationId);
        return args?.investigationId === savedSecond.investigation_id ? savedSecond : null;
      }
      if (command === 'investigation_result') {
        if (args?.investigationId) openedActivityResultReads.push(args.investigationId);
        return args?.investigationId === savedSecond.investigation_id ? result : null;
      }
      if (command === 'get_investigation_chat') {
        if (args?.investigationId) openedActivityChatReads.push(args.investigationId);
        return args?.investigationId === savedSecond.investigation_id ? chat : null;
      }
      if (command === 'investigation_events') {
        const events = investigationEvents(savedSecond.investigation_id);
        return { investigation_id: savedSecond.investigation_id,
          items: events.filter((event) => event.sequence > (args?.afterSequence ?? 0)), next_sequence: events.length, has_more: false };
      }
      if (command === 'investigation_consent') return null;
      if (command === 'read_investigation_citation') {
        if (args?.investigationId) openedActivityCitationReads.push(args.investigationId);
        const citation = investigationCitations.find((item) => item.citation_id === args?.citationId);
        return citation && args?.investigationId === savedSecond.investigation_id ? { investigation_id: savedSecond.investigation_id,
          citation_id: citation.citation_id, status: 'metadata_only', citation, text: null } : null;
      }
      return null;
    });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog,
      pendingStart, composer, draftObligations: [inventoryObligation], history: [] }, true);
    useSpecialistActivityStore.getState().setVisible(true);
    return () => { useSpecialistActivityStore.getState().setVisible(false); clearMocks();
      useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  render: function ActiveTaskNavigationStory() {
    return <InvestigationsSurface state={useInvestigationStore()} activity={useSpecialistActivityStore()}
      onRefreshActivity={() => void useSpecialistActivityStore.getState().refresh()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const firstOpen = await canvas.findByRole('button', { name: 'Open investigation: First active question' });
    const secondOpen = canvas.getByRole('button', { name: 'Open investigation: Second active question' });
    await expect(firstOpen.closest('label')).toBeNull();
    await expect(firstOpen.closest('fieldset')).toBeNull();
    await expect(secondOpen).toBeEnabled();
    await expect(canvas.getByText(/First active question/)).toBeVisible();
    await expect(canvas.getByText(/Second active question/)).toBeVisible();
    await expect(canvas.getByText(/Awaiting cloud consent/)).toBeVisible();
    const beforeOpen = useInvestigationStore.getState();
    const expectedPendingStart = beforeOpen.pendingStart;
    const expectedComposer = beforeOpen.composer;
    const expectedObligations = beforeOpen.draftObligations;
    await userEvent.click(secondOpen);
    await waitFor(() => expect(useInvestigationStore.getState().selectedId).toBe('active-second'));
    await waitFor(() => expect(useInvestigationStore.getState().detail?.investigation_id).toBe('active-second'));
    const transcript = within(await canvas.findByRole('region', { name: 'Saved investigation chat' }));
    await expect(transcript.getByRole('listitem', { name: 'User question' })).toHaveTextContent('Second active question');
    await expect(canvas.getByRole('region', { name: 'Investigation findings' })).toBeVisible();
    expect(useInvestigationStore.getState().selectedId).toBe('active-second');
    expect(useInvestigationStore.getState().pendingStart).toEqual(expectedPendingStart);
    expect(useInvestigationStore.getState().composer).toEqual(expectedComposer);
    expect(useInvestigationStore.getState().draftObligations).toEqual(expectedObligations);
    expect(openedActivityDetailReads).toEqual(['active-second']);
    expect(openedActivityResultReads).toEqual(['active-second']);
    await waitFor(() => expect(openedActivityChatReads).toEqual(['active-second']));
    const finding = canvas.getByRole('region', { name: 'Investigation findings' });
    await userEvent.click(within(finding).getByRole('button', { name: 'Inspect citation C1' }));
    await waitFor(() => expect(useInvestigationStore.getState().citationRead?.investigation_id).toBe('active-second'));
    expect(openedActivityCitationReads).toEqual(['active-second']);
  },
};

let releaseEarlier: (value: InvestigationDetail) => void = () => {};
let earlierRequest: Promise<void> | null = null;

export const LateTaskSelectionCannotReplaceCurrent: Story = {
  // AC-0190: the actual dedicated store must guard selection, not only presentation.
  beforeEach: () => {
    const delayed = new Promise<InvestigationDetail>((resolve) => { releaseEarlier = resolve; });
    const earlier = investigationDetail('earlier', { question: 'Earlier scope question' });
    const current = investigationDetail('current', { question: 'Current scope question' });
    earlierRequest = null;
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog,
      history: [earlier, current] }, true);
    mockIPC((command, payload) => {
      const args = payload as { investigationId: string; afterSequence?: number };
      const id = args.investigationId;
      if (command === 'get_investigation') return id === 'earlier' ? delayed : current;
      if (command === 'investigation_result') return investigationResult(id);
      if (command === 'get_investigation_chat') return null;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id,
        items: investigationEvents(id).filter((event) => event.sequence > (args.afterSequence ?? 0)),
        next_sequence: 9, has_more: false };
      throw new Error('Unexpected fixture command');
    });
    // Retain the original call promise so the test awaits its actual late delivery.
    const open = useInvestigationStore.getState().open;
    useInvestigationStore.setState({ open: (id) => {
      const request = open(id);
      if (id === 'earlier') earlierRequest = request;
      return request;
    } });
    return () => { releaseEarlier(earlier); clearMocks();
      useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  render: function StoreSelectionStory() {
    const liveState = useInvestigationStore();
    return <InvestigationsSurface state={liveState} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: /^Earlier scope question/ }));
    await waitFor(() => expect(useInvestigationStore.getState().selectedId).toBe('earlier'));
    await userEvent.click(canvas.getByRole('button', { name: /^Current scope question/ }));
    await waitFor(() => expect(useInvestigationStore.getState().result?.investigation_id).toBe('current'));
    releaseEarlier(investigationDetail('earlier', { question: 'STALE DETAIL MUST NOT REPLACE SELECTION' }));
    await earlierRequest;
    await expect(canvas.getByRole('heading', { name: 'Current scope question' })).toBeVisible();
    await expect(canvas.queryByText('STALE DETAIL MUST NOT REPLACE SELECTION')).not.toBeInTheDocument();
    await expect(useInvestigationStore.getState().events.every((event) => event.investigation_id === 'current')).toBe(true);
  },
};

export const LegacyHistoryRequiresExplicitReplacement: Story = {
  // AC-0190, AC-1062: a retired saved version reopens labelled retired and requires explicit replacement.
  args: { state: state({ detail: legacyInvestigationDetail(2, 'inv-fixture'),
    history: [legacyInvestigationDetail(2, 'inv-fixture')], result: legacyInvestigationResult('inv-fixture') }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('domain-analyst@2')[0]).toBeVisible();
    await expect(canvas.getByText(/retired: this run reopens read-only/)).toBeVisible();
    await expect(canvas.getByText('domain-analyst@2 · retired')).toBeVisible();
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Ask a follow-up' }));
    await expect(canvas.getByText(/Choose an available specialist explicitly/)).toBeVisible();
    await expect(canvas.getByRole('radio', { name: /Domain analyst/ })).not.toBeChecked();
    await userEvent.type(canvas.getByLabelText('Follow-up question'), 'Inspect this evidence again.');
    await expect(canvas.getByRole('button', { name: 'Start follow-up' })).toBeDisabled();
    await expect(args.state.start).not.toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('radio', { name: /Domain analyst/ }));
    await expect(canvas.getAllByText('domain-analyst@2')[0]).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'domain-analyst@3', parent_id: 'inv-fixture' }));
  },
};

const turnProgressSpecialists = [
  { ...investigationCatalog.specialists[0], id: 'domain-analyst@4' as const, version: 4,
    prompt_fingerprint: 'prompt:domain-v4', purpose: 'Inspect evidence with explicit turn progress.' },
  { ...investigationCatalog.specialists[1], id: 'evidence-auditor@4' as const, version: 4,
    prompt_fingerprint: 'prompt:audit-v4', purpose: 'Audit evidence with explicit turn progress.' },
];
const turnProgressCatalog = { ...investigationCatalog,
  specialists: [...investigationCatalog.specialists, ...turnProgressSpecialists] };

export const TurnProgressRequiresExplicitSelection: Story = {
  // AC-0260: catalog additions do not silently change the default specialist.
  args: { state: state({ catalog: turnProgressCatalog }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await expect(canvas.getByLabelText('Specialist version')).not.toHaveValue('domain-analyst@4');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@4');
    await userEvent.type(canvas.getByLabelText('Question'), 'Audit the retained evidence.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'evidence-auditor@4', question: 'Audit the retained evidence.' }));
  },
};

export const TurnProgressFollowUpKeepsSelectedVersion: Story = {
  // AC-0260: a new follow-up retains an available explicit version, not an old default.
  args: { state: state({ catalog: turnProgressCatalog,
    detail: investigationDetail('inv-fixture', { specialist_id: 'domain-analyst@4',
      specialist: turnProgressSpecialists[0] }) }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Ask a follow-up' }));
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@4');
    await userEvent.type(canvas.getByLabelText('Follow-up question'), 'Inspect the missing evidence.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'domain-analyst@4', parent_id: 'inv-fixture' }));
  },
};

export const TurnProgressFollowUpWaitsForCatalog: Story = {
  // AC-0260: unavailable/loading catalog must not downgrade a saved explicit V4.
  beforeEach: () => {
    useInvestigationStore.setState(state({ catalog: null, catalogLoading: true,
      detail: investigationDetail('inv-fixture', { specialist_id: 'domain-analyst@4',
        specialist: turnProgressSpecialists[0] }) }), true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  render: function FollowUpCatalogStory() {
    const liveState = useInvestigationStore();
    return <InvestigationsSurface state={liveState} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const followUp = canvas.getByRole('button', { name: 'Ask a follow-up' });
    await expect(followUp).toBeDisabled();
    await userEvent.click(followUp);
    await expect(canvas.queryByLabelText('Follow-up question')).not.toBeInTheDocument();
    useInvestigationStore.setState({ catalogLoading: false, catalogError: 'Catalog unavailable.' });
    await waitFor(() => expect(canvas.getByRole('alert')).toHaveTextContent('Catalog unavailable.'));
    await expect(followUp).toBeDisabled();
    await userEvent.click(followUp);
    await expect(canvas.queryByLabelText('Follow-up question')).not.toBeInTheDocument();
    useInvestigationStore.setState({ catalog: turnProgressCatalog, catalogError: null });
    await waitFor(() => expect(followUp).toBeEnabled());
    await userEvent.click(followUp);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@4');
    await userEvent.type(canvas.getByLabelText('Follow-up question'), 'Inspect the retained evidence.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await expect(useInvestigationStore.getState().start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'domain-analyst@4', parent_id: 'inv-fixture' }));
  },
};

export const ExplicitRequiredSourcesUseV5: Story = {
  // AC-0261: adding a target never silently upgrades the default V3 specialist.
  args: { state: state({ catalog: inspectionCatalog, draftInspectionRequirements: [inspectionTarget] }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect the required stock source.');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@5');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@5',
      inspection_requirements: [inspectionTarget] }));
  },
};

export const CompletedExecutionCanHaveUnmetRequirements: Story = {
  args: { state: state({ detail: inspectionDetail('inv-fixture', { specialist_id: 'domain-analyst@5',
    inspection_requirements: [inspectionTarget], inspection_assessment: inspectionAssessment(),
    inspection_status: 'unmet', inspection_requirement_count: 1 }),
  history: [inspectionDetail('inv-fixture')], result: { ...investigationResult(), schema_version: 3, inspection_assessment: inspectionAssessment() } }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Execution completed', { selector: 'p[role="status"]' })).toBeVisible();
    await expect(canvas.getByText(/Requirement unmet:/)).toBeVisible();
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await expect(canvas.getByText(/Source inspections: Required sources missing · 1 required/)).toBeVisible();
  },
};

export const RemoveRequirementsAndFollowUpExplicitly: Story = {
  beforeEach: () => {
    useInvestigationStore.setState({ ...state(), catalog: inspectionCatalog,
      draftInspectionRequirements: [inspectionTarget] }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  render: function RequiredSourcesStory() {
    return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Remove required source src/stock.ts' }));
    await expect(canvas.getByText('No source requirements selected.')).toBeVisible();
    useInvestigationStore.getState().addInspectionRequirement(inspectionTarget);
    await waitFor(() => expect(useInvestigationStore.getState().draftInspectionRequirements).toHaveLength(1));
    await userEvent.click(canvas.getByRole('button', { name: 'Ask a follow-up' }));
    await expect(canvas.getByText('No source requirements selected.')).toBeVisible();
  },
};

export const RequiredSourcesKeepExistingRequestBudget: Story = {
  args: { state: state({ catalog: inspectionCatalog, draftInspectionRequirements: [{ ...inspectionTarget,
    fact: { kind: 'node', id: 'large-fact-id:'.repeat(700) } }] }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@5');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect the required source.');
    await expect(canvas.getByText(/This request exceeds the 8,192-byte limit/)).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
  },
};


export const TerminalInspectionWithoutResultIsNotAssessed: Story = {
  args: { state: state({ result: null,
    detail: inspectionDetail('inv-fixture', { status: 'failed', has_result: false,
      inspection_status: 'not_assessed', inspection_assessment: undefined }),
    history: [inspectionDetail('inv-fixture', { status: 'failed', has_result: false,
      inspection_status: 'not_assessed', inspection_assessment: undefined })] }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Failed', { selector: 'p[role="status"]' })).toBeVisible();
    await expect(canvas.getByText('Source inspections: Not assessed · 1 required')).toBeVisible();
    await expect(canvas.getByText('Source inspections: Assessment unavailable')).toBeVisible();
    await expect(canvas.queryByText(/Requirement met:/)).not.toBeInTheDocument();
    await expect(canvas.queryByText(/Requirement unmet:/)).not.toBeInTheDocument();
  },
};

export const CoverageOnlyV6RequiresExplicitSelection: Story = {
  // AC-0239: adding V6 leaves the app default and legacy request shape unchanged.
  args: { state: state({ catalog: qualifiedCatalog }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@6');
    await userEvent.type(canvas.getByLabelText('Question'), 'What do the recorded limits establish?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith({ specialist_id: 'domain-analyst@6',
      question: 'What do the recorded limits establish?', scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
  },
};

export const V6TargetsStayExplicitAndIndependent: Story = {
  args: { state: state({ catalog: qualifiedCatalog, draftInspectionRequirements: [inspectionTarget],
    selectedId: 'inv-qualified', detail: qualifiedDetail('inv-qualified', {
      inspection_requirements: [inspectionTarget], inspection_requirement_count: 1, inspection_status: 'unmet',
      inspection_assessment: { ...inspectionAssessment(), disclosure_hash: qualifiedResult().disclosure_hash! } }),
    result: qualifiedResult('inv-qualified', { inspection_assessment: { ...inspectionAssessment(), disclosure_hash: qualifiedResult().disclosure_hash! } }) }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@6');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect this captured source and its ingest limits.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'evidence-auditor@6',
      inspection_requirements: [inspectionTarget] }));
    await expect(canvas.getByText(/saved coverage qualification was available/)).toBeVisible();
    await expect(canvas.getByText(/Requirement unmet:/)).toBeVisible();
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
  },
};

export const SavedV6HistoryUsesOriginalQualification: Story = {
  args: { state: state({ selectedId: 'inv-qualified', detail: qualifiedDetail(), result: qualifiedResult(),
    history: [qualifiedDetail()] }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Coverage-qualified · saved basis shown on open')).toBeVisible();
    await expect(canvas.getByRole('region', { name: 'Saved investigation coverage' })).toBeVisible();
    await expect(canvas.getByText('Stale at preparation', { exact: false })).toBeVisible();
    await expect(canvas.getByText('Unrecorded at preparation', { exact: false })).toBeVisible();
    await expect(canvas.getByText('Coverage is not exhaustive.')).toBeVisible();
    await expect(within(canvas.getByRole('region', { name: 'Source inspection obligations' }))
      .getByText('Source inspections: Not assessed')).toBeVisible();
  },
};

export const ActualSavedV6BackendFixture: Story = {
  // AC-0239: actual Rust worker/firewall/store DTO, no live provider assertion.
  args: { state: state({ selectedId: backendQualified.detail.investigation_id,
    detail: backendQualified.detail as InvestigationDetail, result: backendQualified.result as InvestigationResult,
    history: [backendQualified.detail as InvestigationDetail] }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Execution completed', { selector: 'p[role="status"]' })).toBeVisible();
    await expect(canvas.getByText('Insufficient evidence to support an answer.')).toBeVisible();
    await expect(canvas.getByText('Limited recorded coverage.')).toBeVisible();
    await expect(canvas.queryByText(/Requirement met:/)).not.toBeInTheDocument();
    await userEvent.click(canvas.getByText('Exact saved coverage identity'));
    await expect(canvas.getByText(backendQualified.detail.coverage_context.context_id)).toBeVisible();
    await expect(canvas.getByText(backendQualified.result.coverage.representation_hash)).toBeVisible();
  },
};

export const CatalogRefreshRequiresExplicitReplacement: Story = {
  // AC-0190: catalog recovery cannot replace a selection that disappeared.
  beforeEach: () => {
    useInvestigationStore.setState(state({ catalog: turnProgressCatalog }), true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  render: function RefreshSelectionStory() {
    return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@4');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect retained evidence.');
    useInvestigationStore.setState({ catalogLoading: true });
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled());
    useInvestigationStore.setState({ catalog: investigationCatalog, catalogLoading: false });
    await waitFor(() => expect(canvas.getByText(/Choose an available specialist explicitly/)).toBeVisible());
    await expect(canvas.getByLabelText('Specialist version')).not.toHaveValue('domain-analyst@3');
    await expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@4');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
    useInvestigationStore.setState({ catalog: null, catalogError: 'Catalog unavailable.' });
    await waitFor(() => expect(canvas.getByRole('alert')).toHaveTextContent('Catalog unavailable.'));
    useInvestigationStore.setState({ catalog: investigationCatalog, catalogError: null });
    await waitFor(() => expect(canvas.getByText(/Choose an available specialist explicitly/)).toBeVisible());
    await expect(canvas.getByLabelText('Specialist version')).not.toHaveValue('domain-analyst@3');
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalled();
    useInvestigationStore.setState({ catalog: turnProgressCatalog });
    await waitFor(() => expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@4'));
    await expect(canvas.queryByText(/Choose an available specialist explicitly/)).not.toBeInTheDocument();
    await expect(canvas.getByLabelText('Specialist version')).not.toHaveValue('domain-analyst@3');
    useInvestigationStore.setState({ catalog: investigationCatalog });
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled());
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@3');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(useInvestigationStore.getState().start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'evidence-auditor@3' }));
  },
};

export const V7RequiresExplicitMixedSelection: Story = {
  // AC-0266: explicit source and recorded-inventory choices share one caller envelope.
  beforeEach: () => {
    useInvestigationStore.setState(state({ catalog: completionCatalog, history: [], selectedId: null, detail: null, result: null,
      draftInspectionRequirements: [sourceObligation.target] }), true);
    mockIPC((command) => command === 'investigation_obligation_choices' ? completionChoices : command === 'resolve_investigation_source_obligation' ? sourceObligation : null);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  render: function MixedCompletionStory() { return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />; },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@7');
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh completion choices' }));
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Require inventory acme/api' })).toBeEnabled());
    await expect(canvas.getByRole('button', { name: 'Require inventory example/stale' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Require selected sources for completion' }));
    await waitFor(() => expect(useInvestigationStore.getState().draftObligations).toHaveLength(2));
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect this selected evidence.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(useInvestigationStore.getState().start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'domain-analyst@7', obligations: { schema_version: 1, contract: 'investigation-completion-obligations@1',
        obligations: [inventoryObligation, sourceObligation] } }));
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalledWith(expect.objectContaining({ inspection_requirements: expect.anything() }));
  },
};

export const SpecWriterStandardFamily: Story = {
  // AC-0382: the saved role is available only in Standard and does not silently fall back.
  args: { state: state({ catalog: expandedCompletionCatalog, detail: null, result: null, selectedId: null, history: [],
    obligationChoices: completionChoices, draftObligations: [inventoryObligation] }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // ADR-0071: the version fixes its profile, so a writer never runs under Expanded.
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'spec-writer@1');
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('spec-writer@1');
    await expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'spec-writer@1', limitProfile: 'investigation-v1' });
    const question = canvas.getByLabelText('Question');
    await expect(question).toHaveAttribute('placeholder', 'What specification should the cited evidence support, and what remains unresolved?');
    await userEvent.clear(question);
    await userEvent.type(question, 'Draft a cited proposal for the guarded limited-input behavior.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'spec-writer@1', limit_profile: 'investigation-v1',
      obligations: expect.objectContaining({ schema_version: 1 }),
    }));
    await expect(args.state.start).not.toHaveBeenCalledWith(expect.objectContaining({ inspection_requirements: expect.anything() }));
  },
};

export const SpecWriterSavedCompletion: Story = {
  // AC-0382: saved specification findings retain their cited result under the exact role identity.
  args: { state: state({ catalog: completionCatalog, detail: specWriterDetail(), result: specWriterResult(),
    selectedId: 'inv-spec-writer', history: [specWriterDetail()] }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion.')).toBeVisible();
    await expect(canvas.getByText('Completion obligations')).toBeVisible();
    await expect(canvas.getByText('Reject limited inputs before guarded processing')).toBeVisible();
    const finding = canvas.getByRole('region', { name: 'Investigation findings' });
    const citation = args.state.result!.findings[0].citation_ids[0];
    await userEvent.click(within(finding).getByRole('button', { name: new RegExp(`Inspect citation ${citation}`) }));
    await expect(args.state.readCitation).toHaveBeenCalledWith(citation);
  },
};

// AC-0404: the saved narrative uses the same task-bound findings and citation renderer.
export const SpecWriterV2NarrativeUsesSharedFindingsRenderer: Story = {
  args: { state: state({ catalog: completionCatalog, detail: specWriterV2Detail(), result: specWriterV2Result(),
    selectedId: 'inv-spec-writer-v2', history: [specWriterV2Detail()] }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const message = canvas.getByRole('article', { name: 'Specialist final message' });
    await expect(message).toHaveTextContent('Spec Writer final message · spec-writer@2');
    await expect(message).toHaveTextContent('Proposed design · T3 / InferredWeak');
    await expect(message).toHaveTextContent('Validate the caller limit before invoking processing; refuse an oversized request without starting the operation.');
    await expect(canvas.getByText('Reject limited inputs before guarded processing')).toBeVisible();
    await userEvent.click(within(message).getByRole('button', { name: 'Inspect message citation C1' }));
    await expect(args.state.readCitation).toHaveBeenCalledWith('C1');
  },
};

export const GapTriagerStandardFamily: Story = {
  // AC-0384: exact Gap Triager selection is unavailable in Expanded and restores without substitution.
  args: { state: state({ catalog: expandedCompletionCatalog, detail: null, result: null, selectedId: null, history: [],
    obligationChoices: completionChoices, draftObligations: [inventoryObligation] }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'gap-triager@1');
    await expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'gap-triager@1', limitProfile: 'investigation-v1' });
    const question = canvas.getByLabelText('Question');
    await expect(question).toHaveAttribute('placeholder', 'Which recorded gap should be investigated next, and what evidence would reduce it?');
    await userEvent.type(question, 'What evidence should reduce the recorded gap?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({
      specialist_id: 'gap-triager@1', limit_profile: 'investigation-v1',
      obligations: expect.objectContaining({ schema_version: 1 }),
    }));
  },
};

// AC-0406: exact @2 selection, ordinary Standard dispatch and Expanded refusal.
export const GapTriagerV2StandardFamily: Story = {
  ...GapTriagerStandardFamily,
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'gap-triager@2');
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('gap-triager@2');
    await userEvent.type(canvas.getByLabelText('Question'), 'Which evidence should reduce this gap?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'gap-triager@2', limit_profile: 'investigation-v1' }));
  },
};

export const GapTriagerSavedCompletion: Story = {
  // AC-0384: saved recommendation preserves the proposed design and citation.
  args: { state: state({ catalog: completionCatalog, detail: gapTriagerDetail(), result: gapTriagerResult(),
    selectedId: 'inv-gap-triager', history: [gapTriagerDetail()] }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion.')).toBeVisible();
    await expect(canvas.getByText('Verify the guarded boundary before treating the gap as closed')).toBeVisible();
    const finding = canvas.getByRole('region', { name: 'Investigation findings' });
    const citation = args.state.result!.findings[0].citation_ids[0];
    await userEvent.click(within(finding).getByRole('button', { name: new RegExp(`Inspect citation ${citation}`) }));
    await expect(args.state.readCitation).toHaveBeenCalledWith(citation);
  },
};

export const V7FollowUpWaitsAndClearsBothKinds: Story = {
  // AC-0266: catalog loading cannot clear a draft; an explicit follow-up clears both partitions.
  beforeEach: () => {
    useInvestigationStore.setState(state({ catalog: completionCatalog, catalogLoading: true,
      detail: completionDetail(), result: completionResult(), history: [completionDetail()],
      draftObligations: [inventoryObligation, sourceObligation], obligationChoices: completionChoices,
      draftInspectionRequirements: [sourceObligation.target] }), true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  render: function FollowUpCompletionStory() { return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />; },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement); const followUp = canvas.getByRole('button', { name: 'Ask a follow-up' });
    await expect(followUp).toBeDisabled(); await userEvent.click(followUp);
    await expect(useInvestigationStore.getState().draftObligations).toHaveLength(2);
    useInvestigationStore.setState({ catalogLoading: false });
    await waitFor(() => expect(followUp).toBeEnabled()); await userEvent.click(followUp);
    await expect(useInvestigationStore.getState().draftObligations).toEqual([]);
    await expect(useInvestigationStore.getState().draftInspectionRequirements).toEqual([]);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@7');
    useInvestigationStore.setState({ catalog: investigationCatalog });
    await waitFor(() => expect(canvas.getByText(/Choose an available specialist explicitly/)).toBeVisible());
    await expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@7');
    await expect(canvas.getByRole('button', { name: 'Start follow-up' })).toBeDisabled();
  },
};

export const ExplicitExpandedBudget: Story = {
  // AC-0281: expanded admission requires explicit budget selection and fresh obligations.
  args: { state: state({ catalog: expandedCompletionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@7' });
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect all required evidence.');
    // ADR-0071: choosing an Expanded version is the budget choice.
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@8');
    await expect(canvas.getByText(/Expanded investigations allow more evidence/)).toBeVisible();
    await expect(canvas.getByText(/Choose 1–56 combined obligations/)).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@8',
      limit_profile: 'investigation-v2', obligations: { schema_version: 2, contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } }));
  },
};

export const ExpandedStartUncertain: Story = {
  args: { state: state({ catalog: expandedCompletionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null,
    pendingStart: { schema_version: 5, specialist_id: 'domain-analyst@8', limit_profile: 'investigation-v2',
      request_nonce: 'retained-nonce', question: 'Frozen expanded investigation.', scope: { type: 'all' }, provider_mode: 'local',
      obligations: { schema_version: 2, contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] },
      expected_context_id: completionChoices.context_id, expected_graph_revision: completionChoices.graph_snapshot_id,
      conversation_id: null, parent_id: null }, startError: 'Admission could not be confirmed. Retry the same request.' }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Hydration represents the frozen composer retained by an uncertain host response.
    useInvestigationStore.setState({ composer: { ...useInvestigationStore.getState().composer,
      specialistId: 'domain-analyst@8', limitProfile: 'investigation-v2', question: 'Frozen expanded investigation.' } });
    await waitFor(() => expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@8'));
    await expect(canvas.getByLabelText('Specialist version')).toBeDisabled();
    await expect(canvas.getByLabelText('Question')).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Retry same start request' }));
    await expect(args.state.retryStart).toHaveBeenCalledTimes(1);
    await expect(args.state.start).not.toHaveBeenCalled();
  },
};


export const ExpandedBudgetRequiresCompatibleSpecialist: Story = {
  // AC-0281: switching a legacy role to expanded budget explains the required explicit selection.
  args: { state: state({ catalog: expandedCompletionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect required evidence.');
    // ADR-0071: the budget follows the chosen version, so no incompatible pair can be selected.
    await expect(canvas.queryByLabelText('Investigation budget')).not.toBeInTheDocument();
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@8');
    await expect(useInvestigationStore.getState().composer.limitProfile).toBe('investigation-v2');
    await expect(canvas.queryByText(/is unavailable for this investigation budget/)).not.toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeEnabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@8',
      limit_profile: 'investigation-v2', obligations: expect.objectContaining({ schema_version: 2 }) }));
  },
};


export const ExplicitQueryHistoryFamily: Story = {
  // AC-0286: current expanded choice starts @9; the numeric budget and Standard default stay unchanged.
  args: { state: state({ catalog: queryHistoryCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(useInvestigationStore.getState().composer.limitProfile).toBeUndefined();
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@7');
    await userEvent.type(canvas.getByLabelText('Question'), 'Audit the explicitly selected evidence.');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@9');
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('evidence-auditor@9');
    await expect(canvas.queryByRole('option', { name: /^evidence-auditor@8\b/ })).not.toBeInTheDocument();
    await expect(canvas.getByText(/Choose 1–56 combined obligations/)).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'evidence-auditor@9',
      limit_profile: 'investigation-v2', obligations: expect.objectContaining({ schema_version: 2 }) }));
  },
};

export const HistoricalExpandedRoleNeedsExplicitReplacement: Story = {
  // AC-0286: reopened @8 follow-up intent never silently becomes @9.
  args: { state: state({ catalog: queryHistoryCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@8', limitProfile: 'investigation-v2',
      question: 'Follow the saved role.', parent: { conversation_id: 'old-conversation', parent_id: 'old-task' } });
    await waitFor(() => expect(canvas.getByText(/is unavailable for this investigation budget/)).toBeVisible());
    await expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@8');
    await expect(canvas.getByRole('button', { name: 'Start follow-up' })).toBeDisabled();
    await expect(args.state.start).not.toHaveBeenCalled();
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@9');
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@9', parent_id: 'old-task' }));
  },
};

export const QueryHistorySavedCompletion: Story = {
  args: { state: state({ catalog: queryHistoryCatalog, detail: queryHistoryDetail(), result: queryHistoryResult(),
    selectedId: 'inv-query-history', history: [queryHistoryDetail()] }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion.')).toBeVisible();
    await expect(canvas.getByText('Completion obligations')).toBeVisible();
  },
};


export const ExplicitNextPromptAdmission: Story = {
  // AC-0287: only an explicit Expanded selection chooses the advertised @10 family.
  args: { state: state({ catalog: admissionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@3');
    await userEvent.type(canvas.getByLabelText('Question'), 'Audit bounded continuation evidence.');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@10');
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('evidence-auditor@10');
    await expect(canvas.queryByRole('option', { name: /^evidence-auditor@9\b/ })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'evidence-auditor@10',
      limit_profile: 'investigation-v2', obligations: expect.objectContaining({ schema_version: 2 }) }));
  },
};

export const SavedQueryHistoryNeedsExplicitNextFamilyFollowup: Story = {
  args: { state: state({ catalog: admissionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: queryHistoryDetail(), result: queryHistoryResult(), selectedId: 'inv-query-history' }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion.')).toBeVisible();
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@9', limitProfile: 'investigation-v2',
      question: 'Inspect a fresh basis.', parent: { conversation_id: 'original-conversation', parent_id: 'original-parent' } });
    await waitFor(() => expect(canvas.getByText(/is unavailable for this investigation budget/)).toBeVisible());
    await expect(canvas.getByRole('button', { name: 'Start follow-up' })).toBeDisabled();
    await expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@9');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@10');
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@10',
      parent_id: 'original-parent', conversation_id: 'original-conversation' }));
    await expect(args.state.detail?.specialist_id).toBe('domain-analyst@9');
  },
};

export const NextPromptSavedCompletion: Story = {
  args: { state: state({ catalog: admissionCatalog, detail: admissionDetail(), result: admissionResult(),
    selectedId: 'inv-admission', history: [admissionDetail()] }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion.')).toBeVisible();
    await expect(canvas.getByText('Completion obligations')).toBeVisible();
  },
};


export const ActualNextPromptWorkerReopened: Story = {
  // AC-0287: actual Rust MockRuntime worker -> public durable DTO -> UI; not a live-provider acceptance claim.
  beforeEach: () => {
    const detail = actualAdmission.detail as unknown as InvestigationDetail;
    mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? actualAdmission.result :
      command === 'investigation_events' ? { investigation_id: detail.investigation_id, items: actualAdmission.events,
        next_sequence: actualAdmission.events.at(-1)!.sequence, has_more: false } : null);
    useInvestigationStore.setState({ catalog: admissionCatalog });
    return () => clearMocks();
  },
  render: function ActualWorkerHistory() {
    const state = useInvestigationStore();
    return <InvestigationsSurface state={state} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    await useInvestigationStore.getState().open(actualAdmission.detail.investigation_id);
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion.')).toBeVisible();
    await expect(canvas.getByText(actualAdmission.result.findings[0].title)).toBeVisible();
    const coverage = within(canvas.getByRole('region', { name: 'Saved investigation coverage' }));
    await expect(coverage.getByText('Coverage is not exhaustive.')).toBeVisible();
    await expect(coverage.getByText(/Recorded at preparation/)).toBeVisible();
    await expect(coverage.getByText('Limited recorded coverage.')).toBeVisible();
    await expect(coverage.getByText(actualAdmission.detail.coverage_context.coverage.repos[0].repo)).toBeVisible();
    await expect(coverage.getByText('The saved coverage qualification was available to the specialist when it finished.')).toBeVisible();
    await expect(useInvestigationStore.getState().detail).toEqual(actualAdmission.detail);
    await expect(useInvestigationStore.getState().result).toEqual(actualAdmission.result);
    await expect(useInvestigationStore.getState().events).toEqual(actualAdmission.events);
    await expect(canvas.getByText(/Findings cover the inspected scope; knowledge remains partial/)).toBeVisible();
  },
};

export const PersistedChatUsesSelectedFindingsAndCitation: Story = {
  // AC-0378: transcript references reuse this selected task's one findings/citation renderer.
  beforeEach: () => {
    const detail = investigationDetail('chat-selected');
    const result = investigationResult(detail.investigation_id);
    const chat: InvestigationChat = { chat_id: `chat:${detail.investigation_id}`, task_id: detail.investigation_id,
      producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
        prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
        confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
        provider_id: detail.provider.provider_id, model: detail.provider.model,
        protocol_version: detail.provider.protocol_version ?? null },
      messages: [{ id: 'saved-user', ordinal: 0, author: 'user',
        message: { kind: 'user_question', question: detail.question } },
      { id: 'saved-result', ordinal: 1, author: 'specialist',
        message: { kind: 'result_reference', task_id: detail.investigation_id,
          result_id: result.result_id, result_hash: 'd'.repeat(64) } }] };
    mockIPC((command, payload) => {
      const args = payload as { investigationId?: string; citationId?: string; afterSequence?: number } | undefined;
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result') return result;
      if (command === 'get_investigation_chat') return chat;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: detail.investigation_id,
        items: investigationEvents(detail.investigation_id).filter((event) => event.sequence > (args?.afterSequence ?? 0)),
        next_sequence: detail.last_event_sequence, has_more: false };
      if (command === 'read_investigation_citation') {
        const citation = investigationCitations.find((item) => item.citation_id === args?.citationId);
        return citation ? { investigation_id: detail.investigation_id, citation_id: citation.citation_id,
          status: 'metadata_only', citation, text: null } : null;
      }
      return null;
    });
    useInvestigationStore.setState({ catalog: investigationCatalog, history: [detail] });
    return () => clearMocks();
  },
  render: function PersistedChatSurface() {
    return <InvestigationsSurface state={useInvestigationStore()} anchors={[]} canStart />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await useInvestigationStore.getState().open('chat-selected');
    const transcript = within(canvas.getByRole('region', { name: 'Saved investigation chat' }));
    await expect(transcript.getByText(/Saved producer:/)).toHaveTextContent(/version 3/);
    await expect(transcript.getByRole('listitem', { name: 'User question' })).toHaveTextContent('You');
    await expect(transcript.getByRole('listitem', { name: 'Specialist result reference' })).toHaveTextContent(/Saved result/);
    await expect(canvas.getAllByRole('region', { name: 'Investigation findings' })).toHaveLength(1);
    const findingsLink = transcript.getByRole('link', { name: 'View admitted findings and citations' });
    await expect(findingsLink).toHaveAttribute('href', '#investigation-findings');
    await expect(canvasElement.querySelectorAll('#investigation-findings')).toHaveLength(1);
    const findings = within(canvas.getByRole('region', { name: 'Investigation findings' }));
    await userEvent.click(findings.getByRole('button', { name: 'Inspect citation C1' }));
    await waitFor(() => expect(useInvestigationStore.getState().citationRead?.investigation_id).toBe('chat-selected'));
    await expect(findings.getByRole('complementary', { name: 'Historical citation' })).toBeVisible();
    await expect(canvas.getAllByRole('region', { name: 'Investigation findings' })).toHaveLength(1);
  },
};

export const ConversationTurnsNavigation: Story = {
  beforeEach: () => {
    conversationDetailReads = [];
    const parent = investigationDetail('parent-turn', { conversation_id: 'conversation-one', question: 'Parent investigation question' });
    const child = investigationDetail('child-turn', { conversation_id: 'conversation-one', parent_id: 'parent-turn', question: 'Follow-up investigation question' });
    const older = investigationDetail('older-turn', { conversation_id: 'conversation-one', parent_id: 'parent-turn', question: 'Older sibling question', created_at: '2026-09-09T10:00:00Z' });
    mockIPC((command, args) => {
      const namedArgs = args as Record<string, unknown> | undefined;
      if (command === 'list_investigation_conversation') return namedArgs?.cursor
        ? { items: [older], next_cursor: null } : { items: [parent], next_cursor: 'older-page' };
      const id = namedArgs?.investigationId as string;
      if (command === 'get_investigation') { conversationDetailReads.push(id); return id === 'parent-turn' ? parent : child; }
      if (command === 'investigation_result') return investigationResult(id);
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id, items: investigationEvents(id), next_sequence: 9, has_more: false };
      return null;
    });
    useInvestigationStore.setState({ selectedId: 'child-turn', detail: child, result: investigationResult('child-turn'),
      history: [child], conversationId: null, conversationItems: [], conversationPinned: null });
    return () => clearMocks();
  },
  render: function ConversationTurnsStory(args) {
    const liveState = useInvestigationStore();
    return <InvestigationsSurface {...args} state={liveState} />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Open conversation turns' }));
    await expect(canvas.getByRole('region', { name: 'Conversation turns' })).toBeVisible();
    const selectedTurn = canvas.getByLabelText('Selected conversation turn');
    await expect(selectedTurn).toBeVisible();
    await expect(within(selectedTurn).getByText(/follow-up to parent-turn/)).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Load older turns' }));
    await expect(canvas.getByRole('button', { name: /Older sibling question/ })).toBeVisible();
    await expect(conversationDetailReads).toEqual([]);
    await userEvent.click(canvas.getByRole('button', { name: /Parent investigation question/ }));
    await waitFor(() => expect(useInvestigationStore.getState().selectedId).toBe('parent-turn'));
    await expect(useInvestigationStore.getState().detail?.investigation_id).toBe('parent-turn');
    await expect(conversationDetailReads).toEqual(['parent-turn']);
  },
};

// AC-0402: the second history consumer accepts and opens the same answer-only evidence.
export const SavedAnswerHistorySearch: Story = {
  ...SearchesPersistedHistory,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('searchbox'), 'guarded exit');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getByText('Answer match')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: /Legacy needle in a saved question/ }));
    await expect(await canvas.findByText('The inspected callable has a guarded exit. Its broader inventory policy remains unresolved.')).toBeVisible();
  },
};

// AC-0408: both history consumers label saved-name evidence honestly.
export const SavedSpecialistNameSearch: Story = {
  ...SearchesPersistedHistory,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('searchbox'), 'Domain analyst');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getByText('Name match')).toBeVisible();
    await expect(canvas.queryByText('Answer match')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: /Legacy needle in a saved question/ }));
    await expect(await canvas.findByText('The inspected callable has a guarded exit. Its broader inventory policy remains unresolved.')).toBeVisible();
  },
};

// AC-0414: missing narrative support is unavailable rather than a smaller count.
export const NarrativeMetadataRejectsMissingSupport: Story = {
  ...SpecWriterV2NarrativeUsesSharedFindingsRenderer,
  args: { state: state({ catalog: completionCatalog, selectedId: 'inv-spec-writer-v2', detail: specWriterV2Detail(),
    result: { ...specWriterV2Result(), narrative: { text: 'Saved message', finding_ids: ['missing-finding'] } } }) },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByRole('group', { name: 'Specialist message evidence' })).toHaveTextContent('Citation metadata unavailable');
  },
};


export const ExplicitAcquisitionVersion: Story = {
  // AC-0553: append the exact opt-in version, preserve initial and budget defaults.
  args: { state: state({ catalog: acquisitionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await expect(useInvestigationStore.getState().composer.limitProfile).toBeUndefined();
    await expect(args.state.start).not.toHaveBeenCalled();
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@18');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect explicitly offered acquisition choices.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@18',
      limit_profile: 'investigation-v2', obligations: expect.objectContaining({ schema_version: 2 }) }));
  },
};

export const RolesRunTheirCurrentVersion: Story = {
  // AC-1402 (ADR-0071): a role choice runs that role's current (else newest offered) version under the
  // profile that version uses; an older version stays an explicit choice.
  args: { state: state({ catalog: admissionCatalog, obligationChoices: completionChoices,
    draftObligations: [inventoryObligation], detail: null, selectedId: null }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.queryByLabelText('Investigation budget')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('radio', { name: /Evidence auditor/ }));
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('evidence-auditor@10');
    await expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'evidence-auditor@10', limitProfile: 'investigation-v2' });
    await expect(canvas.getByText(/Expanded budget ·/)).toBeVisible();
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@3');
    await expect(canvas.getByRole('radio', { name: /Evidence auditor/ })).toBeChecked();
    await expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'evidence-auditor@3', limitProfile: 'investigation-v1' });
    await userEvent.click(canvas.getByRole('radio', { name: /Domain analyst/ }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Which behavior is evidenced?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(args.state.start).toHaveBeenCalledWith(expect.objectContaining({ specialist_id: 'domain-analyst@10',
      limit_profile: 'investigation-v2', obligations: expect.objectContaining({ schema_version: 2 }) }));
  },
};
