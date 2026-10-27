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
    await expect(await dialog.findByText(/Shared history selection changed/)).toBeVisible();
    await expect(dialog.queryByRole('button', { name: /Needle saved question/ })).not.toBeInTheDocument();
    useInvestigationStore.setState({ historySearch: 'needle', historyError: 'Search refused' });
    await expect(await dialog.findByRole('alert')).toHaveTextContent('Search refused');
    await expect(dialog.queryByText(/No open or saved chats match/)).not.toBeInTheDocument();
  },
};
