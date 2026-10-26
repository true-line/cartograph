import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { investigationCatalog, investigationDetail, investigationEvents, investigationResult } from '../investigationFixtures';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import type { InvestigationChat, InvestigationDetail } from '../investigationTypes';
import { AgentChatsSurface } from './AgentChatsSurface';

const state = (overrides: Partial<InvestigationState> = {}): InvestigationState => ({
  ...useInvestigationStore.getInitialState(), catalog: investigationCatalog, ...overrides,
});
const meta = {
  title: 'Surfaces/AgentChatsSurface', component: AgentChatsSurface,
  beforeEach: () => {
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  render: function AgentChatsStory(args) { return <AgentChatsSurface {...args} state={useInvestigationStore()} />; },
  args: { state: state(), onOpenResult: fn() },
} satisfies Meta<typeof AgentChatsSurface>;
export default meta;
type Story = StoryObj<typeof meta>;

let taskReads: string[] = [];
let starts = 0;
let firstPage: InvestigationDetail;
let olderPage: InvestigationDetail;
let selectedResult: ReturnType<typeof investigationResult>;
let selectedChat: InvestigationChat;

export const SelectTaskAndOpenExactResult: Story = {
  beforeEach: () => {
    taskReads = [];
    starts = 0;
    firstPage = investigationDetail('chat-first', { question: 'First saved specialist question' });
    olderPage = investigationDetail('chat-older', { question: 'Older saved specialist question' });
    selectedResult = investigationResult(olderPage.investigation_id);
    selectedChat = {
      chat_id: `chat:${olderPage.investigation_id}`, task_id: olderPage.investigation_id,
      producer: { specialist_id: olderPage.specialist.id, specialist_version: olderPage.specialist.version,
        prompt_fingerprint: olderPage.specialist.prompt_fingerprint, tier: olderPage.specialist.tier,
        confidence_tier: olderPage.specialist.confidence_tier, provider_mode: olderPage.provider.mode,
        provider_id: olderPage.provider.provider_id, model: olderPage.provider.model,
        protocol_version: olderPage.provider.protocol_version ?? null },
      messages: [
        { id: 'older-question', ordinal: 0, author: 'user', message: { kind: 'user_question', question: olderPage.question } },
        { id: 'older-result', ordinal: 1, author: 'specialist', message: { kind: 'result_reference',
          task_id: olderPage.investigation_id, result_id: selectedResult.result_id, result_hash: 'e'.repeat(64) } },
      ],
    };
    mockIPC((command, payload) => {
      const args = payload as { cursor?: string; search?: string; investigationId?: string; afterSequence?: number } | undefined;
      if (command === 'list_investigations') {
        if (args?.search) return { items: [olderPage], next_cursor: null };
        if (args?.cursor) return { items: [olderPage], next_cursor: null };
        return { items: [firstPage], next_cursor: 'older-page' };
      }
      const id = args?.investigationId ?? '';
      if (command === 'get_investigation') { taskReads.push(id); return id === olderPage.investigation_id ? olderPage : null; }
      if (command === 'investigation_result') return id === olderPage.investigation_id ? selectedResult : null;
      if (command === 'get_investigation_chat') return id === olderPage.investigation_id ? selectedChat : null;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id,
        items: investigationEvents(id).filter((event) => event.sequence > (args?.afterSequence ?? 0)),
        next_sequence: id === olderPage.investigation_id ? olderPage.last_event_sequence : 0, has_more: false };
      if (command === 'start_investigation') { starts += 1; return null; }
      return null;
    });
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, catalog: investigationCatalog,
      composer: { ...initial.composer, question: 'Leave this composer draft untouched' },
      pendingStart: { schema_version: 1, request_nonce: 'n'.repeat(36), specialist_id: 'domain-analyst@3',
        question: 'Retry this exact admitted request', scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' } }, true);
    return () => clearMocks();
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const store = useInvestigationStore.getState();
    await store.loadHistory();
    await expect(canvas.getByText('First saved specialist question')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Load older saved tasks' }));
    await expect(canvas.getByText('Older saved specialist question')).toBeVisible();
    await userEvent.type(canvas.getByRole('searchbox'), 'older saved specialist');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getByText('Older saved specialist question')).toBeVisible();
    await expect(canvas.queryByText('First saved specialist question')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: /Older saved specialist question/ }));
    await waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    await expect(canvas.getByText(/Saved producer:/)).toHaveTextContent(/domain-analyst@3 · version 3/);
    const transcript = within(canvas.getByRole('region', { name: 'Saved investigation chat' }));
    await expect(transcript.getByRole('listitem', { name: 'User question' })).toHaveTextContent('Older saved specialist question');
    await expect(transcript.getByRole('listitem', { name: 'Specialist result reference' })).toHaveTextContent(selectedResult.result_id);
    await userEvent.click(transcript.getByRole('link', { name: 'Open this task’s findings and citations' }));
    await expect(args.onOpenResult).toHaveBeenCalledTimes(1);
    await expect(args.onOpenResult).toHaveBeenCalledWith('chat-older');
    await expect(taskReads).toEqual(['chat-older']);
    const finalState = useInvestigationStore.getState();
    await expect(finalState.composer.question).toBe('Leave this composer draft untouched');
    await expect(finalState.pendingStart?.request_nonce).toBe('n'.repeat(36));
    await expect(starts).toBe(0);
  },
};

export const LegacyTaskRoutesToFindings: Story = {
  beforeEach: () => {
    const legacy = investigationDetail('legacy-chat-task', { question: 'Older saved task without a transcript' });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), history: [legacy],
      selectedId: legacy.investigation_id, detail: legacy, result: investigationResult(legacy.investigation_id),
      chat: null, chatStatus: 'legacy', loading: false });
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No saved conversation is available for this older investigation.')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Open this investigation to inspect its findings' }));
    await expect(args.onOpenResult).toHaveBeenCalledTimes(1);
    await expect(args.onOpenResult).toHaveBeenCalledWith('legacy-chat-task');
  },
};
