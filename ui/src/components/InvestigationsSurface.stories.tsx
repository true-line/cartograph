import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { InvestigationsSurface } from './InvestigationsSurface';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import { investigationCatalog, investigationConsent, investigationDetail, investigationEvents, investigationResult, legacyInvestigationDetail, legacyInvestigationResult } from '../investigationFixtures';
import type { InvestigationDetail } from '../investigationTypes';

const state = (overrides: Partial<InvestigationState> = {}): InvestigationState => ({
  ...useInvestigationStore.getInitialState(), catalog: investigationCatalog, history: [investigationDetail()],
  selectedId: 'inv-fixture', detail: investigationDetail(), result: investigationResult(), events: investigationEvents(),
  start: fn(async () => {}), retryStart: fn(async () => {}), open: fn(async () => {}),
  approve: fn(async () => {}), decline: fn(async () => {}), cancel: fn(async () => {}),
  hideConsent: fn(), showConsent: fn(), loadHistory: fn(async () => {}), loadCatalog: fn(async () => {}),
  refreshSelected: fn(async () => {}), readCitation: fn(async () => {}), clearCitation: fn(), ...overrides,
});

const meta = { title: 'Surfaces/InvestigationsSurface', component: InvestigationsSurface,
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

export const LegacyHistoryUsesCurrentFollowUp: Story = {
  // AC-0180/0190: preserve saved identities while new follow-ups use the current role.
  args: { state: state({ detail: legacyInvestigationDetail(2, 'inv-fixture'),
    history: [legacyInvestigationDetail(2, 'inv-fixture')], result: legacyInvestigationResult('inv-fixture') }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('domain-analyst@2')[0]).toBeVisible();
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Ask a follow-up' }));
    await expect(canvas.getByRole('radio', { name: /Domain analyst/ })).toBeChecked();
    await userEvent.type(canvas.getByLabelText('Follow-up question'), 'Inspect this evidence again.');
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
