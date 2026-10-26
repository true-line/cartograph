// AC-0320/0321: registry operations never authorize tasks.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { McpTaskSettings } from './McpTaskSettings';
const meta = {
  title: 'Surfaces/McpTaskSettings', component: McpTaskSettings,
  args: { available: true, serviceBusy: false, status: 'disabled', policy: { fixture: true }, onGrant: fn() },
  beforeEach: () => {
    mockIPC((command, args) => {
      if (command === 'mcp_reads_owner_status') return { selected_client: null, task_dispatch_available: true, task_grant: null };
      if (command === 'mcp_task_client_create') { expect(args).toEqual({ maximumClients: 2 }); return 'client-a'; }
      if (command === 'mcp_task_clients') return ['client-b'];
      throw new Error('unexpected command');
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof McpTaskSettings>;
export default meta;
type Story = StoryObj<typeof meta>;
export const CreateThenGrant: Story = {
  play: async ({ canvasElement, args }) => {
    const c = within(canvasElement);
    await waitFor(() => expect(c.getByLabelText('Maximum registered clients')).toBeEnabled());
    await userEvent.type(c.getByLabelText('Maximum registered clients'), '2');
    await userEvent.click(c.getByRole('button', { name: 'Create client' }));
    await waitFor(() => expect(c.getByLabelText('Investigation client')).toHaveValue('client-a'));
    await expect(args.onGrant).not.toHaveBeenCalled();
    await userEvent.type(c.getByLabelText('Maximum request identities'), '3');
    await userEvent.click(c.getByRole('button', { name: 'Enable investigation access' }));
    await expect(args.onGrant).toHaveBeenCalledWith({ fixture: true }, 'client-a', 3);
  },
};
export const RegistryFailure: Story = {
  play: async ({ canvasElement, args }) => {
    const c = within(canvasElement);
    await waitFor(() => expect(c.getByLabelText('Maximum registered clients')).toBeEnabled());
    mockIPC(() => { throw new Error('full'); });
    await userEvent.type(c.getByLabelText('Maximum registered clients'), '2');
    await userEvent.click(c.getByRole('button', { name: 'Create client' }));
    await expect(await c.findByRole('alert')).toHaveTextContent('could not be updated');
    await expect(args.onGrant).not.toHaveBeenCalled();
  },
};
