import { useState } from 'react';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { investigationCatalog, investigationDetail, investigationEvents, investigationResult } from '../investigationFixtures';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import type { InvestigationChat, InvestigationDetail } from '../investigationTypes';
import { useAgentChatDockStore } from '../agentChatDockStore';
import { AgentChatsSurface } from './AgentChatsSurface';

const state = (overrides: Partial<InvestigationState> = {}): InvestigationState => ({
  ...useInvestigationStore.getInitialState(), catalog: investigationCatalog, ...overrides,
});
const meta = {
  title: 'Surfaces/AgentChatsSurface', component: AgentChatsSurface,
  beforeEach: () => {
    useAgentChatDockStore.setState({ docks: [], focusedTask: null, pendingTask: null, error: null, fallbackTask: null });
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    return () => { clearMocks(); useAgentChatDockStore.setState({ docks: [], focusedTask: null, pendingTask: null, error: null, fallbackTask: null });
      useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  render: function AgentChatsStory(args) { return <AgentChatsSurface {...args} state={useInvestigationStore()} />; },
  args: { state: state(), onOpenResult: fn(), onFollowUp: fn(), onCancelFollowUp: fn(),
    preparingFollowUpTask: null, followUpError: null },
} satisfies Meta<typeof AgentChatsSurface>;
export default meta;
type Story = StoryObj<typeof meta>;

let taskReads: string[] = [];
let starts = 0;
let firstPage: InvestigationDetail;
let olderPage: InvestigationDetail;
let pendingPages: InvestigationDetail[];
let selectedResult: ReturnType<typeof investigationResult>;
let selectedChat: InvestigationChat;

// AC-0389: real coordinator validation, four task/chat identities and unchanged retry state.
export const SelectTaskAndOpenExactResult: Story = {
  beforeEach: () => {
    taskReads = [];
    starts = 0;
    firstPage = investigationDetail('chat-first', { question: 'First saved specialist question', has_result: false,
      status: 'running', revision: 4, last_event_sequence: 4, invocation_pending: true });
    pendingPages = [firstPage, ...['third', 'fourth'].map((name) => investigationDetail(`chat-${name}`, {
      ...firstPage, investigation_id: `chat-${name}`, conversation_id: `conversation-chat-${name}`, question: `${name} pending specialist question`,
    }))];
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
        return { items: pendingPages, next_cursor: 'older-page' };
      }
      const id = args?.investigationId ?? '';
      const pending = pendingPages.find((item) => item.investigation_id === id);
      if (command === 'get_investigation') { taskReads.push(id); return id === olderPage.investigation_id ? olderPage : pending; }
      if (command === 'investigation_result') return id === olderPage.investigation_id ? selectedResult : null;
      if (command === 'get_investigation_chat') return id === olderPage.investigation_id ? selectedChat : {
        ...selectedChat, chat_id: `chat:${id}`, task_id: id,
        messages: [{ id: 'first-question', ordinal: 0, author: 'user', message: { kind: 'user_question', question: pending?.question } }],
      };
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id,
        items: investigationEvents(id).filter((event) => event.sequence > (args?.afterSequence ?? 0) &&
          event.sequence <= (pending?.last_event_sequence ?? olderPage.last_event_sequence)),
        next_sequence: id === olderPage.investigation_id ? olderPage.last_event_sequence : firstPage.last_event_sequence, has_more: false };
      if (/^(start|cancel|delete)_investigation$/.test(command)) { starts += 1; return null; }
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
    await waitFor(() => expect(canvasElement.querySelector('.agent-chat-docks-1')).toBeInTheDocument());
    const olderPane = within(await canvas.findByRole('region', { name: 'Saved chat chat-older' }));
    await expect(olderPane.getByText(/Saved producer:/)).toHaveTextContent(/domain-analyst@3 · version 3/);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear search' }));
    await userEvent.click(await canvas.findByRole('button', { name: /First saved specialist question/ }));
    await expect(await canvas.findByRole('region', { name: 'Saved chat chat-first' })).toBeVisible();
    await expect(canvasElement.querySelector('.agent-chat-docks-2')).toBeInTheDocument();
    for (const [name, count] of [['third', 3], ['fourth', 4]] as const) {
      await userEvent.click(canvas.getByRole('button', { name: new RegExp(`${name} pending specialist question`) }));
      await expect(await canvas.findByRole('region', { name: `Saved chat chat-${name}` })).toBeVisible();
      await expect(canvasElement.querySelector(`.agent-chat-docks-${count}`)).toBeInTheDocument();
    }
    await expect(olderPane.getByText(/Last observed snapshot/)).toBeVisible();
    await userEvent.click(olderPane.getByRole('button', { name: 'Close chat-older' }));
    await expect(canvas.queryByRole('region', { name: 'Saved chat chat-older' })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Load older saved tasks' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Older saved specialist question/ }));
    const transcript = within(await canvas.findByRole('region', { name: 'Saved chat chat-older' }));
    await expect(transcript.getByRole('listitem', { name: 'User question' })).toHaveTextContent('Older saved specialist question');
    await expect(transcript.getByRole('listitem', { name: 'Specialist result reference' })).toHaveTextContent(selectedResult.result_id);
    await userEvent.click(transcript.getByRole('link', { name: 'Open this task’s findings and citations' }));
    await expect(args.onOpenResult).toHaveBeenCalledTimes(1);
    await expect(args.onOpenResult).toHaveBeenCalledWith('chat-older');
    await userEvent.click(transcript.getByRole('button', { name: 'Follow-up task' }));
    await expect(args.onFollowUp).toHaveBeenCalledWith('chat-older');
    await expect(taskReads).toEqual(['chat-older', 'chat-first', 'chat-third', 'chat-fourth', 'chat-older']);
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
    mockIPC((command, payload) => {
      const id = (payload as { investigationId?: string })?.investigationId;
      if (command === 'get_investigation') return legacy;
      if (command === 'investigation_result') return investigationResult(legacy.investigation_id);
      if (command === 'get_investigation_chat' || command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id, items: investigationEvents(legacy.investigation_id),
        next_sequence: legacy.last_event_sequence, has_more: false };
      return null;
    });
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: /Older saved task without a transcript/ }));
    await expect(await canvas.findByText('No saved conversation is available for this older investigation.')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Open this investigation to inspect its findings' }));
    await expect(args.onOpenResult).toHaveBeenCalledTimes(1);
    await expect(args.onOpenResult).toHaveBeenCalledWith('legacy-chat-task');
  },
};

let conversationReads: string[];
let conversationChatReads: string[];
let refreshConversationChild = false;
let conversationPages: { conversationId?: string; cursor?: string }[];
let releaseOldConversation: (() => void) | undefined;
let failConversation = false;
let showNewChild = false;

// AC-0396: real paged coordinator plus exact serialized dock handoff; no sibling reads.
export const ConversationTurnsFromCapturedPane: Story = {
  beforeEach: () => {
    conversationReads = []; conversationChatReads = []; refreshConversationChild = false;
    conversationPages = []; releaseOldConversation = undefined;
    failConversation = false; showNewChild = false;
    const turn = (id: string, parent_id: string | null = null, conversation_id = 'family-a') => investigationDetail(id, {
      question: `Saved question ${id}`, conversation_id, parent_id, has_result: false,
      status: 'running', revision: 4, last_event_sequence: 4, invocation_pending: true });
    const details = [turn('parent'), turn('child', 'parent'), turn('sibling', 'parent'),
      turn('new-child', 'child'), turn('other', null, 'family-b')];
    let holdFirstA = true;
    mockIPC((command, payload) => {
      const args = (payload ?? {}) as { investigationId?: string; conversationId?: string; cursor?: string; afterSequence?: number };
      if (command === 'list_investigation_conversation') {
        conversationPages.push({ conversationId: args.conversationId, cursor: args.cursor });
        if (args.conversationId === 'family-b') return { items: [], next_cursor: null };
        if (holdFirstA) {
          holdFirstA = false;
          return new Promise((resolve) => { releaseOldConversation = () => resolve({ items: [details[0]], next_cursor: null }); });
        }
        if (failConversation) throw new Error('Saved turns unavailable');
        return args.cursor ? { items: [details[2]], next_cursor: null }
          : { items: showNewChild ? [details[3], details[0]] : [details[0]], next_cursor: 'older-turns' };
      }
      const saved = details.find((item) => item.investigation_id === args.investigationId);
      const detail = saved?.investigation_id === 'child' && refreshConversationChild ? { ...saved,
        status: 'failed' as const, revision: 5, last_event_sequence: 5, invocation_pending: false,
        actions: { can_cancel: false, can_follow_up: false }, updated_at: '2026-09-10T10:01:00Z' } : saved;
      if (command === 'get_investigation') { conversationReads.push(args.investigationId!); return detail; }
      if (command === 'investigation_events') return { investigation_id: args.investigationId,
        items: [...investigationEvents(args.investigationId!).slice(0, 4), ...(detail?.revision === 5 ? [{
          investigation_id: detail.investigation_id, sequence: 5, revision: 5, kind: 'failed', tool: null,
          summary: 'Invocation failed.', step_id: 'step-1', created_at: detail.updated_at }] : [])]
          .filter((event) => event.sequence > (args.afterSequence ?? 0)),
        next_sequence: detail?.last_event_sequence ?? 4, has_more: false };
      if (command === 'get_investigation_chat' && detail) {
        conversationChatReads.push(detail.investigation_id);
        return {
        chat_id: `chat:${detail.investigation_id}`, task_id: detail.investigation_id,
        producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
          prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
          confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
          provider_id: detail.provider.provider_id, model: detail.provider.model, protocol_version: null },
        messages: [{ id: `question:${detail.investigation_id}`, ordinal: 0, author: 'user',
          message: { kind: 'user_question', question: detail.question } }],
        };
      }
      if (command === 'list_investigations') return { items: [details[1], details[4]], next_cursor: null };
      if (/^(start|cancel|delete)_investigation$/.test(command)) throw new Error('Read-only navigation must not dispatch');
      return null;
    });
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, history: [details[1], details[4]], catalog: investigationCatalog,
      composer: { ...initial.composer, question: 'Retain composer question' },
      pendingStart: { schema_version: 1, request_nonce: 'c'.repeat(36), specialist_id: 'domain-analyst@3',
        question: 'Retain same request', scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' } }, true);
    return () => { releaseOldConversation?.(); clearMocks(); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const originalPending = useInvestigationStore.getState().pendingStart;
    await userEvent.click(canvas.getByRole('button', { name: /Saved question child/ }));
    const childPane = within(await canvas.findByRole('region', { name: 'Saved chat child' }));
    await userEvent.click(childPane.getByRole('button', { name: 'Conversation turns' }));
    await expect(canvas.getByText('Loading conversation turns…')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: /Saved question other/ }));
    const otherPane = within(await canvas.findByRole('region', { name: 'Saved chat other' }));
    await expect(canvas.getByText('Conversation detail is no longer active. Refresh to reload this captured conversation.')).toBeVisible();
    await userEvent.click(otherPane.getByRole('button', { name: 'Conversation turns' }));
    const turns = within(canvas.getByRole('region', { name: 'Conversation turns' }));
    await expect(turns.getByText('family-b')).toBeVisible();
    await expect(turns.getByText('No saved turns in this conversation page.')).toBeVisible();
    releaseOldConversation?.();
    await waitFor(() => expect(useInvestigationStore.getState().conversationItems).toEqual([]));
    await expect(turns.queryByText('Saved question parent')).not.toBeInTheDocument();
    await userEvent.click(childPane.getByRole('button', { name: 'Conversation turns' }));
    await expect(turns.getByText('family-a')).toBeVisible();
    const pinned = within(turns.getByLabelText('Selected conversation turn'));
    await expect(pinned.getByRole('button', { name: /Saved question child/ })).toHaveTextContent('domain-analyst@3 · Running');
    await expect(pinned.getByText('Follow-up to parent')).toBeVisible();
    await expect(conversationReads).toEqual(['child', 'other']);
    await userEvent.click(pinned.getByRole('button', { name: /Saved question child/ }));
    await waitFor(() => expect(useInvestigationStore.getState().loading).toBe(false));
    refreshConversationChild = true;
    useInvestigationStore.getState().invalidate({ investigation_id: 'child', revision: 5, last_event_sequence: 5 });
    await waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    await expect(pinned.getByRole('button', { name: /Saved question child/ })).toHaveTextContent('Failed');
    await expect(pinned.getByText('2026-09-10T10:01:00Z')).toBeVisible();
    const freshPin = useInvestigationStore.getState().conversationPinned!;
    for (const foreign of [{ ...freshPin, investigation_id: 'other' }, { ...freshPin, conversation_id: 'family-b' },
      { ...freshPin, revision: 3 }]) {
      useInvestigationStore.setState({ conversationPinned: foreign });
      await waitFor(() => expect(pinned.getByRole('button', { name: /Saved question child/ })).toHaveTextContent('Running'));
      await expect(useInvestigationStore.getState().conversationPinned).toEqual(foreign);
    }
    useInvestigationStore.setState({ conversationPinned: freshPin });
    await waitFor(() => expect(pinned.getByRole('button', { name: /Saved question child/ })).toHaveTextContent('Failed'));
    await expect(conversationChatReads).toEqual(['child', 'other', 'child', 'child']);
    await userEvent.click(turns.getByRole('button', { name: 'Load older turns' }));
    await expect(turns.getByRole('button', { name: /Saved question sibling/ })).toBeVisible();
    await expect(conversationPages.at(-1)).toEqual({ conversationId: 'family-a', cursor: 'older-turns' });
    await userEvent.click(turns.getByRole('button', { name: /Saved question sibling/ }));
    await expect(await canvas.findByRole('region', { name: 'Saved chat sibling' })).toBeVisible();
    await expect(conversationReads).toEqual(['child', 'other', 'child', 'child', 'sibling']);
    await userEvent.click(turns.getByRole('button', { name: 'Open parent parent' }));
    await expect(await canvas.findByRole('region', { name: 'Saved chat parent' })).toBeVisible();
    await expect(conversationReads).toEqual(['child', 'other', 'child', 'child', 'sibling', 'parent']);
    showNewChild = true;
    await userEvent.click(turns.getByRole('button', { name: 'Refresh conversation turns' }));
    await expect(turns.getByRole('button', { name: /Saved question new-child/ })).toBeVisible();
    failConversation = true;
    await userEvent.click(turns.getByRole('button', { name: 'Refresh conversation turns' }));
    await expect(turns.getByRole('alert')).toHaveTextContent('Conversation turns could not be loaded');
    await expect(turns.getByRole('button', { name: /Saved question sibling/ })).toBeVisible();
    await userEvent.click(turns.getByRole('button', { name: 'Close conversation turns' }));
    await expect(canvas.queryByRole('region', { name: 'Conversation turns' })).not.toBeInTheDocument();
    await expect(useInvestigationStore.getState().conversationId).toBeNull();
    await expect(useInvestigationStore.getState().composer.question).toBe('Retain composer question');
    await expect(useInvestigationStore.getState().pendingStart).toEqual(originalPending);
  },
};

// AC-0396: leaving a destination only releases its own conversation navigator.
export const ConversationOwnershipAcrossVisits: Story = {
  ...ConversationTurnsFromCapturedPane,
  render: function ConversationVisit(args) {
    const [shown, setShown] = useState(true);
    const liveState = useInvestigationStore();
    return <><button type="button" onClick={() => setShown(!shown)}>Toggle Agent Chats visit</button>
      {shown && <AgentChatsSurface {...args} state={liveState} />}</>;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await useInvestigationStore.getState().openConversation('family-b');
    const other = useInvestigationStore.getState().history.find((item) => item.investigation_id === 'other')!;
    useInvestigationStore.setState({ conversationItems: [other], conversationPinned: other, conversationCursor: 'saved-page',
      conversationPageBookmarks: [{ cursor: 'saved-bookmark', ids: ['other'] }] });
    const existing = useInvestigationStore.getState();
    await userEvent.click(canvas.getByRole('button', { name: 'Toggle Agent Chats visit' }));
    await expect(useInvestigationStore.getState().conversationId).toBe('family-b');
    await expect(useInvestigationStore.getState().conversationItems).toBe(existing.conversationItems);
    await expect(useInvestigationStore.getState().conversationCursor).toBe(existing.conversationCursor);
    await expect(useInvestigationStore.getState().conversationPinned).toBe(existing.conversationPinned);
    await expect(useInvestigationStore.getState().conversationPageBookmarks).toBe(existing.conversationPageBookmarks);
    await userEvent.click(canvas.getByRole('button', { name: 'Toggle Agent Chats visit' }));
    await userEvent.click(canvas.getByRole('button', { name: /Saved question child/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat child' }));
    await userEvent.click(pane.getByRole('button', { name: 'Conversation turns' }));
    await expect(canvas.getByText('Loading conversation turns…')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Toggle Agent Chats visit' }));
    await expect(useInvestigationStore.getState().conversationId).toBeNull();
    releaseOldConversation?.();
    await waitFor(() => expect(useInvestigationStore.getState().conversationItems).toEqual([]));
    await expect(useInvestigationStore.getState().conversationId).toBeNull();
  },
};
