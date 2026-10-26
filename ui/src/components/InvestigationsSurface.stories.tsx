import actualAdmission from '../fixtures/completion-v10-supported.json';
import { admissionCatalog, admissionDetail, admissionResult, queryHistoryCatalog, queryHistoryDetail, queryHistoryResult, expandedCompletionCatalog, completionCatalog, completionChoices, completionDetail, completionResult, inventoryObligation, sourceObligation } from '../completionFixtures';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { InvestigationsSurface } from './InvestigationsSurface';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import { useSpecialistActivityStore } from '../specialistActivityStore';
import backendQualified from '../fixtures/investigation-coverage-backend.json';
import { qualifiedCatalog, qualifiedDetail, qualifiedResult, inspectionAssessment, inspectionCatalog, inspectionDetail, inspectionTarget, investigationCatalog, investigationCitations, investigationConsent, investigationDetail, investigationEvents, investigationResult, legacyInvestigationDetail, legacyInvestigationResult } from '../investigationFixtures';
import type { InvestigationChat, InvestigationDetail, InvestigationResult } from '../investigationTypes';

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
      return command === 'list_investigations'
        ? { items: search ? [matching] : [matching, nonmatching], next_cursor: null } : null;
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
  // AC-0379: the roster uses exact specialist versions and reports durable task states.
  beforeEach: () => {
    const active = [
      investigationDetail('domain-running-activity', { specialist_id: 'domain-analyst@3', status: 'running', has_result: false }),
      investigationDetail('auditor-consent-activity', { specialist_id: 'evidence-auditor@3', status: 'awaiting_consent',
        cancel_requested: true, has_result: false }),
      investigationDetail('old-domain-activity', { specialist_id: 'domain-analyst@2', status: 'queued', has_result: false }),
    ];
    mockIPC((command, args) => {
      if (command === 'list_investigations') {
        expect(args).toEqual({ activeOnly: true });
        return { items: active, next_cursor: null };
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
    await canvas.findByText(/Running/);
    await canvas.findByText(/Awaiting cloud consent/);
    const domain = canvas.getByText(/domain-analyst@3/).closest('label');
    const auditor = canvas.getByText(/evidence-auditor@3/).closest('label');
    expect(domain).not.toBeNull();
    expect(auditor).not.toBeNull();
    await expect(domain).toHaveTextContent('Running');
    await expect(domain).not.toHaveTextContent('Queued');
    await expect(auditor).toHaveTextContent('Awaiting cloud consent');
    await expect(auditor).toHaveTextContent('Cancellation requested');
    await expect(canvas.findByText(/active task list is incomplete/i)).resolves.toBeVisible();
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().selectedId).toBeNull();
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
  // AC-0190: an unavailable saved version requires explicit replacement.
  args: { state: state({ detail: legacyInvestigationDetail(2, 'inv-fixture'),
    history: [legacyInvestigationDetail(2, 'inv-fixture')], result: legacyInvestigationResult('inv-fixture') }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('domain-analyst@2')[0]).toBeVisible();
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).toBeChecked();
    await expect(canvas.getByRole('radio', { name: /domain-analyst@4/ })).not.toBeChecked();
    await userEvent.click(canvas.getByRole('radio', { name: /evidence-auditor@4/ }));
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@4/ })).toBeChecked();
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@4/ })).toBeChecked();
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).toBeChecked();
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect the required stock source.');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@5/ }));
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
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@5/ }));
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).toBeChecked();
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@6/ }));
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
    await userEvent.click(canvas.getByRole('radio', { name: /evidence-auditor@6/ }));
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
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@4/ }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect retained evidence.');
    useInvestigationStore.setState({ catalogLoading: true });
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled());
    useInvestigationStore.setState({ catalog: investigationCatalog, catalogLoading: false });
    await waitFor(() => expect(canvas.getByText(/Choose an available specialist explicitly/)).toBeVisible());
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).not.toBeChecked();
    await expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@4');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
    useInvestigationStore.setState({ catalog: null, catalogError: 'Catalog unavailable.' });
    await waitFor(() => expect(canvas.getByRole('alert')).toHaveTextContent('Catalog unavailable.'));
    useInvestigationStore.setState({ catalog: investigationCatalog, catalogError: null });
    await waitFor(() => expect(canvas.getByText(/Choose an available specialist explicitly/)).toBeVisible());
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).not.toBeChecked();
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalled();
    useInvestigationStore.setState({ catalog: turnProgressCatalog });
    await waitFor(() => expect(canvas.getByRole('radio', { name: /domain-analyst@4/ })).toBeChecked());
    await expect(canvas.queryByText(/Choose an available specialist explicitly/)).not.toBeInTheDocument();
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).not.toBeChecked();
    useInvestigationStore.setState({ catalog: investigationCatalog });
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled());
    await userEvent.click(canvas.getByRole('radio', { name: /evidence-auditor@3/ }));
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).toBeChecked();
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@7/ }));
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@7/ })).toBeChecked();
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
    await userEvent.selectOptions(canvas.getByLabelText('Investigation budget'), 'investigation-v2');
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
    await waitFor(() => expect(canvas.getByLabelText('Investigation budget')).toHaveValue('investigation-v2'));
    await expect(canvas.getByLabelText('Investigation budget')).toBeDisabled();
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
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).toBeChecked();
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect required evidence.');
    await userEvent.selectOptions(canvas.getByLabelText('Investigation budget'), 'investigation-v2');
    await expect(canvas.getByText(/is unavailable for this investigation budget/)).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
    for (const radio of canvas.getAllByRole('radio')) await expect(radio).not.toBeChecked();
    await expect(args.state.start).not.toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@8/ }));
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
    await expect(canvas.getByLabelText('Investigation budget')).toHaveValue('investigation-v1');
    await expect(canvas.getAllByRole('option', { name: /^Expanded/ })).toHaveLength(1);
    await userEvent.click(canvas.getByRole('radio', { name: /evidence-auditor@7/ }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Audit the explicitly selected evidence.');
    await userEvent.selectOptions(canvas.getByLabelText('Investigation budget'), 'investigation-v2');
    await expect(canvas.getByRole('radio', { name: /evidence-auditor@9/ })).toBeChecked();
    await expect(canvas.queryByRole('radio', { name: /evidence-auditor@8/ })).not.toBeInTheDocument();
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
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@9/ }));
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
    await expect(canvas.getByLabelText('Investigation budget')).toHaveValue('investigation-v1');
    await expect(canvas.getByRole('radio', { name: /domain-analyst@3/ })).toBeChecked();
    await userEvent.click(canvas.getByRole('radio', { name: /evidence-auditor@3/ }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Audit bounded continuation evidence.');
    await userEvent.selectOptions(canvas.getByLabelText('Investigation budget'), 'investigation-v2');
    await expect(canvas.getByRole('radio', { name: /evidence-auditor@10/ })).toBeChecked();
    await expect(canvas.queryByRole('radio', { name: /evidence-auditor@9/ })).not.toBeInTheDocument();
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
    await userEvent.click(canvas.getByRole('radio', { name: /domain-analyst@10/ }));
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
