import { useState } from 'react';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { investigationCatalog, investigationCitations, investigationDetail, investigationEvents, investigationResult } from '../investigationFixtures';
import { completionCatalog, specWriterV2Detail, specWriterV2Result } from '../completionFixtures';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import type { InvestigationChat, InvestigationDetail } from '../investigationTypes';
import { useAgentChatDockStore } from '../agentChatDockStore';
import type { SavedResultNoticeCorner } from '../savedResultNoticePreference';
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
    preparingFollowUpTask: null, followUpError: null, noticeCorner: 'top-right' as SavedResultNoticeCorner,
    onNoticeCornerChange: fn() },
} satisfies Meta<typeof AgentChatsSurface>;
export default meta;
type Story = StoryObj<typeof meta>;

export const SavedResultNoticeCornerPreference: Story = {
  render: function CornerPreferenceStory(args) {
    const [noticeCorner, setNoticeCorner] = useState(args.noticeCorner);
    return <AgentChatsSurface {...args} state={useInvestigationStore()} noticeCorner={noticeCorner}
      onNoticeCornerChange={setNoticeCorner} />;
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const group = canvas.getByRole('group', { name: 'Saved result notice position' });
    for (const [selected, other] of [['Top Left', 'Top Right'], ['Bottom Right', 'Top Left'],
      ['Bottom Left', 'Bottom Right'], ['Top Right', 'Bottom Left']]) {
      await userEvent.click(within(group).getByRole('button', { name: selected }));
      await expect(within(group).getByRole('button', { name: selected })).toHaveAttribute('aria-pressed', 'true');
      await expect(within(group).getByRole('button', { name: other })).toHaveAttribute('aria-pressed', 'false');
    }
  },
};

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
    await expect(useAgentChatDockStore.getState().docks.find((item) => item.chat.task_id === 'child')?.turn)
      .toMatchObject({ investigation_id: 'child', conversation_id: 'family-a', revision: 5, status: 'failed' });
    const freshPin = useInvestigationStore.getState().conversationPinned!;
    for (const foreign of [{ ...freshPin, investigation_id: 'other' }, { ...freshPin, conversation_id: 'family-b' },
      { ...freshPin, revision: 3 }]) {
      useInvestigationStore.setState({ conversationPinned: foreign });
      await waitFor(() => expect(pinned.getByRole('button', { name: /Saved question child/ })).toHaveTextContent('Failed'));
      await expect(pinned.getByText('2026-09-10T10:01:00Z')).toBeVisible();
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

let savedAnswerCitationReads: string[];
let savedAnswerSearches: unknown[];
let releaseSavedAnswerCitation: (() => void) | undefined;

// AC-0397/0404: validated saved answers stay with their task and cited proposal.
function savedAnswersFixture(gapTriager = false, palette = false) {
    savedAnswerCitationReads = []; savedAnswerSearches = []; releaseSavedAnswerCitation = undefined;
    const details = [
      { ...specWriterV2Detail('answer-a'), question: 'Question for answer-a' },
      investigationDetail('answer-b', { question: 'Question for answer-b' }),
    ];
    if (gapTriager) Object.assign(details[0], { specialist_id: 'gap-triager@2',
      specialist: completionCatalog.specialists.find((role) => role.id === 'gap-triager@2')! });
    const results = details.map((detail, index) => {
      const result = index === 0 ? specWriterV2Result(detail.investigation_id) : investigationResult(detail.investigation_id);
      result.findings[0].statement = `Saved finding only for ${detail.investigation_id}`;
      if (gapTriager && index === 0) result.narrative!.text = 'Proposed investigation: verify the unresolved boundary with focused evidence.';
      return result;
    });
    mockIPC((command, payload) => {
      const args = (payload ?? {}) as { investigationId?: string; citationId?: string; afterSequence?: number; search?: string; cursor?: string };
      const index = details.findIndex((detail) => detail.investigation_id === args.investigationId);
      const detail = details[index]; const result = results[index];
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result') return result;
      if (command === 'investigation_events') return { investigation_id: args.investigationId,
        items: investigationEvents(args.investigationId!).filter((event) => event.sequence > (args.afterSequence ?? 0)), next_sequence: 9, has_more: false };
      if (command === 'get_investigation_chat' && detail) return {
        chat_id: `chat:${detail.investigation_id}`, task_id: detail.investigation_id,
        producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
          prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
          confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
          provider_id: detail.provider.provider_id, model: detail.provider.model, protocol_version: null },
        messages: [{ id: `question:${detail.investigation_id}`, ordinal: 0, author: 'user', message: { kind: 'user_question', question: detail.question } },
          { id: `answer:${detail.investigation_id}`, ordinal: 1, author: 'specialist', message: { kind: 'result_reference',
            task_id: detail.investigation_id, result_id: result.result_id, result_hash: 'e'.repeat(64) } }],
      };
      if (command === 'read_investigation_citation') {
        savedAnswerCitationReads.push(`${args.investigationId}:${args.citationId}`);
        return new Promise((resolve) => { releaseSavedAnswerCitation = () => resolve({ investigation_id: args.investigationId,
          citation_id: args.citationId, citation: investigationCitations[0], status: 'available', text: `Retained source for ${args.investigationId}` }); });
      }
      if (command === 'list_investigations') savedAnswerSearches.push(payload);
      if (command === 'list_investigations' && palette && args.cursor) return { items: [], next_cursor: null };
      if (command === 'list_investigations' && palette && args.search === 'Spec Writer') return { items: [], next_cursor: null };
      if (command === 'list_investigations' && args.search === 'Domain analyst') return {
        items: [details[1]], next_cursor: palette ? 'older-name-page' : null, name_match_ids: [details[1].investigation_id] };
      if (command === 'list_investigations' && args.search === 'caller limit') return {
        items: [details[0]], next_cursor: null, answer_match_ids: [details[0].investigation_id] };
      if (command === 'list_investigations') return { items: details, next_cursor: null,
        ...(args.search ? { answer_match_ids: details.map((item) => item.investigation_id) } : {}) };
      if (/^(start|cancel|delete)_investigation$/.test(command)) throw new Error('Read-only saved answers must not dispatch');
      return null;
    });
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, history: details, catalog: investigationCatalog,
      composer: { ...initial.composer, question: 'Keep composer draft' },
      pendingStart: { schema_version: 1, request_nonce: 'd'.repeat(36), specialist_id: 'domain-analyst@3',
        question: 'Keep pending request', scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' } }, true);
    return () => { releaseSavedAnswerCitation?.(); clearMocks(); };
}

export const SavedAnswersStayWithTheirPane: Story = {
  beforeEach: () => savedAnswersFixture(),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const pending = useInvestigationStore.getState().pendingStart;
    for (const id of ['answer-a', 'answer-b']) await userEvent.click(canvas.getByRole('button', { name: new RegExp(`Question for ${id}`) }));
    const paneA = within(await canvas.findByRole('region', { name: 'Saved chat answer-a' }));
    const paneB = within(await canvas.findByRole('region', { name: 'Saved chat answer-b' }));
    await expect(paneA.getByText('Saved finding only for answer-a')).toBeVisible();
    await expect(paneA.queryByText('Saved finding only for answer-b')).not.toBeInTheDocument();
    await expect(paneB.getByText('Saved finding only for answer-b')).toBeVisible();
    const message = paneA.getByRole('article', { name: 'Specialist final message' });
    await expect(message).toHaveTextContent('Spec Writer final message · spec-writer@2');
    await expect(message).toHaveTextContent('Proposed design · T3 / InferredWeak');
    await expect(message).toHaveTextContent('Validate the caller limit before invoking processing; refuse an oversized request without starting the operation.');
    await expect(message).not.toHaveTextContent('Saved finding only for answer-a');
    await expect(paneA.getByRole('button', { name: 'Inspect citation C1' })).toBeVisible();
    await expect(paneA.getByText('Proposed design')).toBeVisible();
    await expect(paneB.getByText('Implemented behavior')).toBeVisible();
    await expect(paneA.queryByText(/Review state unavailable|Follow-up selection is a human opinion/)).not.toBeInTheDocument();
    await expect(canvasElement.querySelectorAll('[id^="saved-chat-findings-"]')).toHaveLength(2);
    await expect(canvasElement.querySelector('#investigation-findings')).toBeNull();
    await userEvent.click(within(message).getByRole('button', { name: 'Inspect message citation C1' }));
    await waitFor(() => expect(savedAnswerCitationReads).toEqual(['answer-a:C1']));
    await expect(paneA.getByText('Reading historical citation…')).toBeVisible();
    await expect(paneB.queryByRole('complementary', { name: 'Historical citation' })).not.toBeInTheDocument();
    releaseSavedAnswerCitation?.();
    await expect(await paneA.findByText('Retained source for answer-a')).toBeVisible();
    await userEvent.click(paneB.getByRole('button', { name: 'Inspect citation C1' }));
    await waitFor(() => expect(savedAnswerCitationReads).toEqual(['answer-a:C1', 'answer-b:C1']));
    await userEvent.click(canvas.getByRole('button', { name: /Question for answer-a/ }));
    await waitFor(() => expect(useAgentChatDockStore.getState().citation).toBeNull());
    releaseSavedAnswerCitation?.();
    await expect(paneA.queryByText('Retained source for answer-b')).not.toBeInTheDocument();
    await expect(paneB.queryByText('Reading historical citation…')).not.toBeInTheDocument();
    await expect(useInvestigationStore.getState().composer.question).toBe('Keep composer draft');
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
  },
};

// AC-0400: actual coordinator reads, exact answers, persisted edges and unchanged four-slot admission.
export const LoadedConversationBranches: Story = {
  args: { onReplyHost: fn() },
  beforeEach: () => {
    const links = [ ['root', null, 'branch'], ['left', 'root', 'branch'], ['right', 'root', 'branch'],
      ['leaf', 'left', 'branch'], ['other', null, 'other-conversation'],
      ['cross', 'root', 'cross-conversation'], ['self', 'self', 'malformed'],
      ['cycle-a', 'cycle-b', 'cycle'], ['cycle-b', 'cycle-a', 'cycle'] ] as const;
    const details = links.map(([id, parent_id, conversation_id]) => investigationDetail(id, {
      question: `Saved question ${id}`, parent_id, conversation_id,
    }));
    mockIPC((command, payload) => {
      const args = (payload ?? {}) as { investigationId?: string; citationId?: string; afterSequence?: number };
      const detail = details.find((item) => item.investigation_id === args.investigationId);
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result' && detail) {
        const result = investigationResult(detail.investigation_id);
        result.findings[0].statement = `Exact finding ${detail.investigation_id}`; return result;
      }
      if (command === 'investigation_events' && detail) return { investigation_id: detail.investigation_id,
        items: investigationEvents(detail.investigation_id).filter((event) => event.sequence > (args.afterSequence ?? 0)), next_sequence: 9, has_more: false };
      if (command === 'get_investigation_chat' && detail) return {
        chat_id: detail.investigation_id === 'root' ? 'conversation-branch' : `chat:${detail.investigation_id}`,
        task_id: detail.investigation_id,
        producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
          prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
          confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
          provider_id: detail.provider.provider_id, model: detail.provider.model, protocol_version: null },
        messages: [{ id: `q:${detail.investigation_id}`, ordinal: 0, author: 'user', message: { kind: 'user_question', question: detail.question } },
          { id: `r:${detail.investigation_id}`, ordinal: 1, author: 'specialist', message: { kind: 'result_reference',
            task_id: detail.investigation_id, result_id: `result:${detail.investigation_id}`, result_hash: 'e'.repeat(64) } }],
      };
      if (command === 'read_investigation_citation') return { investigation_id: args.investigationId,
        citation_id: args.citationId, citation: investigationCitations[0], status: 'available', text: `Exact source ${args.investigationId}` };
      if (command === 'investigation_consent' || command === 'get_investigation_finding_reviews') return null;
      throw new Error(`Unexpected branch fixture command ${command}`);
    });
    useInvestigationStore.setState(state({ history: details }), true);
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const open = async (id: string) => {
      await userEvent.click(canvas.getByRole('button', { name: new RegExp(`^Saved question ${id}`) }));
      return within(await canvas.findByRole('region', { name: `Saved chat ${id}` }));
    };
    const left = await open('left');
    await expect(left.getByText(/unloaded in this conversation/)).toBeVisible();
    await open('right'); await open('leaf');
    await userEvent.click(left.getByRole('button', { name: 'Open unloaded parent root' }));
    await canvas.findByRole('region', { name: 'Saved chat root' });
    await expect(canvas.getAllByRole('heading', { name: 'Conversation branch' })).toHaveLength(1);
    for (const id of ['root', 'left', 'right', 'leaf']) {
      const pane = within(canvas.getByRole('region', { name: `Saved chat ${id}` }));
      await expect(pane.getByRole('listitem', { name: 'User question' })).toHaveTextContent(`Saved question ${id}`);
      await expect(pane.getByText(`Exact finding ${id}`)).toBeVisible();
      await expect(pane.getByText(/domain-analyst@3/)).toBeVisible();
    }
    await expect(left.getByText(/2 loaded sibling forks/)).toBeVisible();
    await expect(within(canvas.getByRole('region', { name: 'Saved chat leaf' })).getByText(/Follow-up to left/)).toBeVisible();
    await userEvent.click(left.getByRole('button', { name: 'Inspect citation C1' }));
    await expect(await left.findByText('Exact source left')).toBeVisible();
    // The root chat ID deliberately collides with the old conversation-heading key.
    const rootPaneElement = canvas.getByRole('region', { name: 'Saved chat root' });
    const rootReplyHost = rootPaneElement.querySelector('[data-reply-task="root"]');
    expect(rootReplyHost).toBeInTheDocument();
    await open('other');
    expect(canvas.getByRole('region', { name: 'Saved chat root' }).querySelector('[data-reply-task="root"]')).toBe(rootReplyHost);
    await expect(canvas.queryByRole('region', { name: 'Saved chat left' })).not.toBeInTheDocument();
    await expect(canvas.getAllByRole('region', { name: /^Saved chat / })).toHaveLength(4);
    const cross = await open('cross');
    await expect(cross.getByText(/parent is docked in another conversation/))
      .toHaveTextContent(/Follow-up to root.*parent is docked in another conversation/);
    await expect(cross.getByRole('button', { name: 'Focus / refresh parent root' })).toBeVisible();
    await expect(canvas.getByRole('region', { name: 'Saved chat root' })).toBeVisible();
    const self = await open('self');
    await expect(self.getByText('Invalid self-parent link · self')).toBeVisible();
    await expect(self.queryByRole('button', { name: /parent self/ })).not.toBeInTheDocument();
    await userEvent.click(self.getByRole('button', { name: 'Close self' }));
    await expect(canvas.queryByRole('heading', { name: 'Conversation malformed' })).not.toBeInTheDocument();
    await open('cycle-a'); const cycle = await open('cycle-b');
    await expect(cycle.getByText(/Follow-up to cycle-a.*loaded in this conversation/)).toBeVisible();
    await expect(canvas.getAllByRole('heading', { name: 'Conversation cycle' })).toHaveLength(1);
    await expect(canvas.getAllByRole('region', { name: /^Saved chat cycle-/ })).toHaveLength(2);
  },
};

let completeProgress = () => {};
let releaseProgressChat = () => {};
// AC-0401: real host events and committed typed findings, never simulated streaming.
export const FocusedDurableProgress: Story = {
  beforeEach: () => {
    let complete = false; const id = 'progress-task'; const calls: string[] = [];
    completeProgress = () => { complete = true; };
    const detail = () => investigationDetail(id, { has_result: complete, status: complete ? 'completed' : 'running',
      revision: complete ? 9 : 4, last_event_sequence: complete ? 9 : 4 });
    mockIPC((command, payload) => {
      calls.push(command);
      if (command === 'get_investigation') return detail();
      if (command === 'investigation_result') return complete ? investigationResult(id) : null;
      if (command === 'investigation_events') return { investigation_id: id,
        items: investigationEvents(id).filter((event) => event.sequence > ((payload as { afterSequence: number }).afterSequence) &&
          event.sequence <= detail().last_event_sequence), next_sequence: detail().last_event_sequence, has_more: false };
      if (command === 'get_investigation_chat') {
        const saved = detail(); const producer = saved.specialist; const provider = saved.provider;
        const chat: InvestigationChat = { chat_id: 'chat:progress', task_id: id,
          producer: { specialist_id: producer.id, specialist_version: producer.version, prompt_fingerprint: producer.prompt_fingerprint,
            tier: producer.tier, confidence_tier: producer.confidence_tier, provider_mode: provider.mode,
            provider_id: provider.provider_id, model: provider.model, protocol_version: null },
          messages: [{ id: 'progress-question', ordinal: 0, author: 'user', message: { kind: 'user_question', question: saved.question } }] };
        if (complete) {
          chat.messages.push({ id: 'progress-answer', ordinal: 1, author: 'specialist', message: {
            kind: 'result_reference', task_id: id, result_id: `result:${id}`, result_hash: 'e'.repeat(64) } });
          return new Promise((resolve) => { releaseProgressChat = () => resolve(chat); });
        }
        return chat;
      }
      return null;
    });
    useInvestigationStore.setState(state({ history: [detail()] }), true);
    return () => { releaseProgressChat(); clearMocks(); expect(calls.filter((command) => command === 'get_investigation_chat')).toHaveLength(2); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: /What stock behavior/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat progress-task' }));
    await expect(pane.getByRole('region', { name: 'Investigation activity' })).toHaveTextContent('Model request started');
    completeProgress();
    useInvestigationStore.getState().invalidate({ investigation_id: 'progress-task', revision: 9, last_event_sequence: 9 });
    await waitFor(() => expect(useInvestigationStore.getState().detail?.revision).toBe(9));
    await expect(pane.queryByRole('region', { name: 'Investigation activity' })).not.toBeInTheDocument();
    await expect(pane.getByText('Refreshing focused task; retained snapshot shown.')).toBeVisible();
    await expect(pane.queryByText('A local stock guard is present')).not.toBeInTheDocument();
    releaseProgressChat();
    await expect(await pane.findByText('A local stock guard is present')).toBeVisible();
    await expect(pane.getByRole('region', { name: 'Investigation activity' })).toHaveTextContent('Findings saved');
    await expect(pane.getByRole('listitem', { name: 'Specialist result reference' })).toHaveTextContent('result:progress-task');
  },
};

// AC-0402: answer-only search opens the actual validated saved chat/findings.
export const SavedAnswerHistorySearch: Story = {
  ...SavedAnswersStayWithTheirPane,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('searchbox'), 'Saved finding');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getAllByText('Answer match')).toHaveLength(2);
    await userEvent.click(canvas.getByRole('button', { name: /Question for answer-a/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat answer-a' }));
    await expect(pane.getByText('Saved finding only for answer-a')).toBeVisible();
    await expect(pane.getByRole('button', { name: 'Inspect citation C1' })).toBeVisible();
  },
};

// AC-0405: narrative-only evidence uses the existing marker and exact dock reader.
export const SavedNarrativeHistorySearch: Story = {
  ...SavedAnswersStayWithTheirPane,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const pending = useInvestigationStore.getState().pendingStart;
    await userEvent.type(canvas.getByRole('searchbox'), 'caller limit');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getAllByText('Answer match')).toHaveLength(1);
    await expect(canvas.queryByRole('button', { name: /Question for answer-b/ })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: /Question for answer-a/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat answer-a' }));
    const message = within(pane.getByRole('article', { name: 'Specialist final message' }));
    await expect(message.getByText(/Validate the caller limit/)).toBeVisible();
    await expect(pane.getByText(/Spec Writer final message · spec-writer@2/)).toBeVisible();
    await userEvent.click(message.getByRole('button', { name: 'Inspect message citation C1' }));
    await waitFor(() => expect(savedAnswerCitationReads).toEqual(['answer-a:C1']));
    releaseSavedAnswerCitation?.();
    await expect(await pane.findByText('Retained source for answer-a')).toBeVisible();
    await expect(useInvestigationStore.getState().composer.question).toBe('Keep composer draft');
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
  },
};

// AC-0406: the real coordinator binds the new producer, result and citation.
export const GapTriagerCitedFinalMessage: Story = {
  beforeEach: () => savedAnswersFixture(true),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const pending = useInvestigationStore.getState().pendingStart;
    await userEvent.click(canvas.getByRole('button', { name: /Question for answer-a/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat answer-a' }));
    const message = within(pane.getByRole('article', { name: 'Specialist final message' }));
    await expect(message.getByText(/Gap Triager final message · gap-triager@2/)).toBeVisible();
    await expect(message.getByText(/Proposed investigation: verify/)).toBeVisible();
    await expect(message.getByText('Proposed design · T3 / InferredWeak · saved final output')).toBeVisible();
    await userEvent.click(message.getByRole('button', { name: 'Inspect message citation C1' }));
    await waitFor(() => expect(savedAnswerCitationReads).toEqual(['answer-a:C1']));
    releaseSavedAnswerCitation?.();
    await expect(await pane.findByText('Retained source for answer-a')).toBeVisible();
    await expect(useInvestigationStore.getState().composer.question).toBe('Keep composer draft');
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
  },
};

// AC-0408: name-only matches use the real coordinator and exact serialized dock read.
export const SavedSpecialistNameSearch: Story = {
  ...SavedAnswersStayWithTheirPane,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const pending = useInvestigationStore.getState().pendingStart;
    await userEvent.type(canvas.getByRole('searchbox'), 'Domain analyst');
    await userEvent.click(canvas.getByRole('button', { name: 'Search history' }));
    await expect(canvas.getByText('Name match')).toBeVisible();
    await expect(canvas.queryByText('Answer match')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: /Question for answer-b/ }));
    await expect(await within(await canvas.findByRole('region', { name: 'Saved chat answer-b' })).findByText('Saved finding only for answer-b')).toBeVisible();
    await expect(useInvestigationStore.getState().composer.question).toBe('Keep composer draft');
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
  },
};

// AC-0409: real shared search, name-only history and off-page validated open snapshots.
export const OpenAndSavedSearchPalette: Story = {
  ...SavedAnswersStayWithTheirPane, beforeEach: () => savedAnswersFixture(false, true),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement); const pending = useInvestigationStore.getState().pendingStart;
    await userEvent.click(canvas.getByRole('button', { name: /Question for answer-a/ }));
    await canvas.findByRole('region', { name: 'Saved chat answer-a' });
    useInvestigationStore.setState({ historyActiveOnly: true });
    await userEvent.click(canvas.getByRole('button', { name: 'Search open + saved chats' }));
    let dialog = within(await canvas.findByRole('dialog', { name: 'Search open and saved chats' }));
    await userEvent.type(dialog.getByRole('searchbox'), 'Domain analyst');
    await userEvent.click(dialog.getByRole('button', { name: 'Search all saved chats' }));
    await expect(savedAnswerSearches).toEqual([{ search: 'Domain analyst' }]);
    const hit = await dialog.findByRole('button', { name: /Question for answer-b/ });
    await expect(hit).toHaveTextContent('HISTORY'); await expect(hit).toHaveTextContent('Name match');
    await userEvent.click(dialog.getByRole('button', { name: 'Load older saved matches' }));
    await waitFor(() => expect(savedAnswerSearches).toHaveLength(2));
    await expect(savedAnswerSearches[1]).toEqual({ search: 'Domain analyst', cursor: 'older-name-page' });
    await userEvent.click(dialog.getByRole('button', { name: /Question for answer-b/ }));
    await canvas.findByRole('region', { name: 'Saved chat answer-b' });
    await userEvent.click(canvas.getByRole('button', { name: 'Search open + saved chats' }));
    dialog = within(await canvas.findByRole('dialog', { name: 'Search open and saved chats' }));
    const openB = dialog.getByRole('button', { name: /Question for answer-b/ });
    await expect(openB).toHaveTextContent('OPEN'); await expect(openB).toHaveTextContent('Last observed open snapshot');
    await expect(within(openB).queryByText('Name match')).not.toBeInTheDocument();
    await userEvent.clear(dialog.getByRole('searchbox')); await userEvent.type(dialog.getByRole('searchbox'), 'Spec Writer');
    await userEvent.click(dialog.getByRole('button', { name: 'Search all saved chats' }));
    await waitFor(() => expect(useInvestigationStore.getState().history).toEqual([]));
    await expect(dialog.getByRole('button', { name: /Question for answer-a/ })).toHaveTextContent('OPEN');
    await userEvent.clear(dialog.getByRole('searchbox')); await userEvent.type(dialog.getByRole('searchbox'), 'caller limit');
    await userEvent.click(dialog.getByRole('button', { name: 'Search all saved chats' }));
    await expect(dialog.getByRole('button', { name: /Question for answer-a/ })).toHaveTextContent('Last observed open snapshot');
    await userEvent.click(dialog.getByRole('button', { name: 'Close chat search' }));
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
    await expect(useInvestigationStore.getState().composer.question).toBe('Keep composer draft');
  },
};
