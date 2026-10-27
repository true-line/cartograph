import { StrictMode, useState } from 'react';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { page } from '@vitest/browser/context';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import type { SurfaceView } from '../views';
import { investigationDetail, investigationResult } from '../investigationFixtures';
import { useInvestigationStore } from '../investigationStore';
import type { InvestigationChat } from '../investigationTypes';
import { createSavedResultNoticeStore, savedResultNoticeKey } from '../savedResultNoticeStore';
import { SavedResultNotices } from './SavedResultNotices';

const taskId = 'notice-task-01';
const detail = investigationDetail(taskId, { status: 'completed', has_result: true });
const result = investigationResult(taskId);
function savedChatFor(id: string, question = `Question for ${id}`): InvestigationChat {
  return {
    chat_id: `chat:${id}`, task_id: id,
    producer: { specialist_id: detail.specialist_id, specialist_version: 3, prompt_fingerprint: 'a'.repeat(64),
    tier: 'Agentic', confidence_tier: 'InferredWeak', provider_mode: 'local', provider_id: 'ollama', model: 'fixture', protocol_version: null },
    messages: [
      { id: `question:${id}`, ordinal: 0, author: 'user', message: { kind: 'user_question', question } },
      { id: `result:${id}`, ordinal: 1, author: 'specialist', message: { kind: 'result_reference', task_id: id,
        result_id: `result:${id}`, result_hash: 'b'.repeat(64) } },
    ],
  };
}
function pendingChatFor(id: string): InvestigationChat {
  const saved = savedChatFor(id);
  return { ...saved, messages: [saved.messages[0]] };
}
const chat = savedChatFor(taskId, detail.question);

let openedNoticeChats: string[] = [];

function NoticeHarness({ store }: { store: ReturnType<typeof createSavedResultNoticeStore> }) {
  const [view, setView] = useState<SurfaceView>('workspace');
  const [openCount, setOpenCount] = useState(0);
  const openTask = (id: string) => {
    setOpenCount((count) => count + 1);
    setView('investigations');
    if (openCount === 0) {
      // A failed open selected the task but did not load its validated records.
      useInvestigationStore.setState({ selectedId: id, loading: false, detail: null, result: null,
        chat: null, chatStatus: 'error' });
    } else if (openCount === 1) {
      // A late successful read for another selection must not consume this notice.
      useInvestigationStore.setState({ selectedId: 'another-task', loading: false,
        detail, result, chat, chatStatus: 'saved' });
    } else {
      useInvestigationStore.setState({ selectedId: id, loading: false, detail, result, chat, chatStatus: 'saved' });
    }
  };
  return <SavedResultNotices view={view} onOpenChat={(id) => openedNoticeChats.push(id)} onOpenFindings={openTask} store={store} />;
}

const meta = {
  title: 'Shell/SavedResultNotices',
  component: SavedResultNotices,
  parameters: { layout: 'fullscreen' },
} satisfies Meta<typeof SavedResultNotices>;
export default meta;
type Story = StoryObj<typeof meta>;

let activeStoryStore: ReturnType<typeof createSavedResultNoticeStore> | null = null;

export const DismissalFailedOpenAndExactVisibleRead: Story = {
  args: { view: 'workspace', onOpenChat: () => {}, onOpenFindings: () => {} },
  render: () => {
    const store = createSavedResultNoticeStore();
    openedNoticeChats = [];
    activeStoryStore = store;
    return <NoticeHarness store={store} />;
  },
  play: async ({ canvasElement }) => {
    const store = activeStoryStore!;
    const canvas = within(canvasElement);
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    let reads = 0;
    mockIPC((command, args) => {
      expect(command).toBe('get_investigation_chat');
      expect(args).toEqual({ investigationId: taskId });
      reads += 1;
      return reads === 1 ? pendingChatFor(taskId) : chat;
    });
    store.getState().observe({ investigation_id: taskId, revision: 1, last_event_sequence: 4 });
    await waitFor(() => expect(reads).toBe(1));
    store.getState().observe({ investigation_id: taskId, revision: 2, last_event_sequence: 5 });
    await waitFor(() => expect(canvas.getByText(/Saved result ready/)).toBeTruthy());
    await userEvent.click(canvas.getByRole('button', { name: 'Dismiss saved result notice' }));
    await userEvent.click(canvas.getByRole('button', { name: /New results/ }));
    await userEvent.click(canvas.getByRole('button', { name: `Open chat for ${taskId}: ${detail.question}` }));
    expect(openedNoticeChats).toEqual([taskId]);
    expect(store.getState().unread).toHaveLength(1); // Open chat does not consume findings unread.
    const savedItem = () => canvas.getByRole('button', { name: `Open findings for ${taskId}: ${detail.question}` });

    await userEvent.click(savedItem());
    expect(store.getState().unread).toHaveLength(1); // failed open does not consume
    await userEvent.click(savedItem());
    expect(store.getState().unread).toHaveLength(1); // another selected task cannot consume it
    await userEvent.click(savedItem());
    await waitFor(() => expect(store.getState().unread).toEqual([]));
    expect(canvas.queryByRole('button', { name: /New results/ })).toBeNull();
    clearMocks();
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
};

export const ToastsKeepTheirOwnDeadlines: Story = {
  args: { view: 'workspace', onOpenChat: () => {}, onOpenFindings: () => {} },
  render: () => {
    const store = createSavedResultNoticeStore();
    const notice = { task_id: 'task-a', result_id: 'result:task-a', question: 'Question for task-a' };
    store.setState({ unread: [notice], toasts: [savedResultNoticeKey(notice)] });
    activeStoryStore = store;
    return <StrictMode><SavedResultNotices view="workspace" onOpenChat={() => {}} onOpenFindings={() => {}} store={store} /></StrictMode>;
  },
  play: async ({ canvasElement }) => {
    const store = activeStoryStore!;
    const canvas = within(canvasElement);
    let taskBReads = 0;
    mockIPC((_command, args) => {
      const id = (args as { investigationId: string }).investigationId;
      if (id !== 'task-b') return savedChatFor(id);
      taskBReads += 1;
      return taskBReads === 1 ? pendingChatFor(id) : savedChatFor(id);
    });
    const openButton = (id: string) => canvas.getByRole('button', { name: new RegExp(`Open findings for ${id}`) });
    await waitFor(() => expect(openButton('task-a')).toBeTruthy());
    await new Promise((resolve) => setTimeout(resolve, 3_000));
    store.getState().observe({ investigation_id: 'task-b', revision: 1, last_event_sequence: 1 });
    await waitFor(() => expect(taskBReads).toBe(1));
    store.getState().observe({ investigation_id: 'task-b', revision: 2, last_event_sequence: 2 });
    await waitFor(() => expect(openButton('task-b')).toBeTruthy());
    store.getState().setCorner('bottom-left');
    await new Promise((resolve) => setTimeout(resolve, 2_600));
    await waitFor(() => expect(canvas.queryByRole('button', { name: /Open findings for task-a/ })).toBeNull());
    expect(openButton('task-b')).toBeTruthy();
  },
};

export const CornerChangesKeepNoticeIdentity: Story = {
  args: { view: 'workspace', onOpenChat: () => {}, onOpenFindings: () => {} },
  render: () => {
    const store = createSavedResultNoticeStore();
    const notice = { task_id: taskId, result_id: result.result_id, question: detail.question };
    store.setState({ unread: [notice], toasts: [savedResultNoticeKey(notice)] });
    activeStoryStore = store;
    return <SavedResultNotices view="workspace" onOpenChat={() => {}} onOpenFindings={() => {}} store={store} />;
  },
  play: async ({ canvasElement }) => {
    const store = activeStoryStore!;
    const canvas = within(canvasElement);
    const stack = canvas.getByLabelText('Saved result notifications');
    const key = savedResultNoticeKey(store.getState().unread[0]);
    const width = window.innerWidth;
    const height = window.innerHeight;
    try {
      await page.viewport(360, 640);
      for (const corner of ['top-left', 'bottom-right', 'bottom-left', 'top-right'] as const) {
        store.getState().setCorner(corner);
        await waitFor(() => expect(stack).toHaveClass(`saved-result-notices-${corner}`));
        const bounds = stack.getBoundingClientRect();
        const styles = getComputedStyle(stack);
        const side = corner.endsWith('left') ? 'left' : 'right';
        await expect(Number.parseFloat(styles.getPropertyValue(side))).toBe(16);
        await expect(bounds.left).toBeGreaterThanOrEqual(0);
        await expect(bounds.right).toBeLessThanOrEqual(window.innerWidth);
        await expect(bounds.top).toBeGreaterThanOrEqual(50);
        await expect(bounds.bottom).toBeLessThanOrEqual(window.innerHeight - 30);
        await expect(store.getState().toasts).toEqual([key]);
        await expect(store.getState().unread).toHaveLength(1);
      }
    } finally {
      await page.viewport(width, height);
    }
  },
};
