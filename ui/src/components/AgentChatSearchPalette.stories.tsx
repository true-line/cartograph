import { useState } from 'react';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { useInvestigationStore } from '../investigationStore';
import { investigationDetail } from '../investigationFixtures';
import { AgentChatSearchPalette } from './AgentChatSearchPalette';

const meta = {
  title: 'Surfaces/AgentChatSearchPalette', component: AgentChatSearchPalette,
  args: { state: useInvestigationStore.getInitialState(), docks: [], query: '', onQuery: fn(), onClose: fn(), onOpen: fn() },
  beforeEach: () => {
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    mockIPC(() => ({ items: [investigationDetail('found', { question: 'Needle saved question' })], next_cursor: null }));
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  render: function PaletteStory(args) {
    const [query, setQuery] = useState('');
    return <AgentChatSearchPalette {...args} state={useInvestigationStore()} query={query} onQuery={setQuery} />;
  },
} satisfies Meta<typeof AgentChatSearchPalette>;
export default meta;
type Story = StoryObj<typeof meta>;

// AC-0409: stale/error shared history is never presented as current successful results.
export const KeyboardAndSharedQueryOwnership: Story = {
  play: async ({ canvasElement, args }) => {
    const dialog = within(within(canvasElement).getByRole('dialog'));
    const input = dialog.getByRole('searchbox');
    await waitFor(() => expect(input).toHaveFocus());
    await userEvent.type(input, 'needle');
    await userEvent.click(dialog.getByRole('button', { name: 'Search all saved chats' }));
    const row = await dialog.findByRole('button', { name: /Needle saved question/ });
    row.focus(); await userEvent.tab(); await expect(input).toHaveFocus();
    await userEvent.keyboard('{ArrowUp}{Enter}');
    await expect(args.onOpen).toHaveBeenCalledTimes(1);
    await expect(args.onOpen).toHaveBeenCalledWith('found');
    await expect(args.onClose).toHaveBeenCalledOnce();
    useInvestigationStore.setState({ historySearch: 'other' });
    await waitFor(() => expect(dialog.getByText(/Shared history selection changed/)).toBeVisible());
    await expect(dialog.queryByRole('button', { name: /Needle saved question/ })).not.toBeInTheDocument();
    useInvestigationStore.setState({ historySearch: 'needle', historyError: 'Search refused' });
    await expect(await dialog.findByRole('alert')).toHaveTextContent('Search refused');
    await expect(dialog.queryByText(/No open or saved chats match/)).not.toBeInTheDocument();
  },
};

// AC-0409: a failed continuation cannot hide validated, query-owned selectable rows.
export const RetainsValidatedMatchesWhenContinuationFails: Story = {
  play: async ({ canvasElement, args }) => {
    let rejectOlder: ((error: Error) => void) | undefined;
    const olderQueries: unknown[] = [];
    mockIPC((command, payload) => {
      expect(command).toBe('list_investigations');
      const query = payload as { search?: string; cursor?: string };
      if (query.cursor) {
        olderQueries.push(payload);
        return olderQueries.length === 1 ? new Promise((_resolve, reject) => { rejectOlder = reject; })
          : { items: [], next_cursor: null };
      }
      if (query.search === 'replacement') throw new Error('Replacement search refused');
      return { items: [investigationDetail('found', { question: 'Needle saved question' })], next_cursor: 'older' };
    });
    const dialog = within(within(canvasElement).getByRole('dialog'));
    const input = dialog.getByRole('searchbox');
    await userEvent.type(input, 'needle');
    await userEvent.click(dialog.getByRole('button', { name: 'Search all saved chats' }));
    await dialog.findByRole('button', { name: /Needle saved question/ });
    await userEvent.click(dialog.getByRole('button', { name: 'Load older saved matches' }));
    await expect(dialog.getByRole('status')).toHaveTextContent('Searching saved chats');
    await expect(dialog.getByRole('button', { name: /Needle saved question/ })).toBeEnabled();
    rejectOlder!(new Error('Continuation refused'));
    await expect(await dialog.findByRole('alert')).toHaveTextContent('could not be completed');
    await expect(useInvestigationStore.getState().history).toHaveLength(1);
    await userEvent.click(dialog.getByRole('button', { name: /Needle saved question/ }));
    await expect(args.onOpen).toHaveBeenCalledWith('found');
    await expect(dialog.getByRole('button', { name: 'Load older saved matches' })).toBeEnabled();
    await expect(dialog.queryByText(/No open or saved chats match/)).not.toBeInTheDocument();
    await userEvent.click(dialog.getByRole('button', { name: 'Load older saved matches' }));
    await waitFor(() => expect(useInvestigationStore.getState().historyLoading).toBe(false));
    await expect(olderQueries).toEqual([{ search: 'needle', cursor: 'older' }, { search: 'needle', cursor: 'older' }]);
    await expect(useInvestigationStore.getState()).toMatchObject({ historyError: null, nextCursor: null });
    await userEvent.clear(input); await userEvent.type(input, 'replacement');
    await userEvent.click(dialog.getByRole('button', { name: 'Search all saved chats' }));
    await expect(await dialog.findByRole('alert')).toHaveTextContent('could not be completed');
    await expect(useInvestigationStore.getState().history).toEqual([]);
    await expect(dialog.queryByRole('button', { name: /Needle saved question/ })).not.toBeInTheDocument();
    await expect(dialog.queryByText(/No open or saved chats match/)).not.toBeInTheDocument();
  },
};
