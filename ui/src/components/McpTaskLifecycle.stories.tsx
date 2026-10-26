// AC-0320/0325: actual Settings controller fences grants and stale observations.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import { McpSettings } from './McpSettings';
const meta = { title: 'Surfaces/McpTaskLifecycle', component: McpSettings, args: { available: true },
  beforeEach: () => () => clearMocks() } satisfies Meta<typeof McpSettings>;
export default meta;
type Story = StoryObj<typeof meta>;
export const ExplicitGrantThenRotate: Story = {
  beforeEach: () => {
    let active = false; let granted = false;
    mockIPC((command, args) => {
      if (command === 'mcp_reads_status') return active ? 'running' : 'disabled';
      if (command === 'mcp_reads_owner_status') return { selected_client: 'client-a', task_dispatch_available: true,
        task_grant: granted ? { client_id: 'client-a', maximum_identities: 3 } : null };
      if (command === 'mcp_tasks_enable') {
        expect(args).toEqual({ policy: { fixture: true }, clientId: 'client-a', maximumIdentities: 3 });
        active = true; granted = true; return null;
      }
      if (command === 'mcp_reads_rotate') { granted = false; return null; }
      throw new Error('unexpected command');
    });
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByText('Task grant: none')).toBeVisible();
    await userEvent.click(c.getByText('Advanced resource policy'));
    await userEvent.type(c.getByLabelText('Resource policy JSON'), '{{"fixture":true}');
    await userEvent.type(c.getByLabelText('Maximum request identities'), '3');
    await userEvent.click(c.getByRole('button', { name: 'Enable investigation access' }));
    await expect(await c.findByText('Task grant: client-a · 3 request identities')).toBeVisible();
    await userEvent.click(c.getByRole('button', { name: 'Rotate client credentials' }));
    await expect(await c.findByText('Task grant: none')).toBeVisible();
    await expect(c.getByLabelText('Investigation client')).toHaveValue('client-a');
    await expect(c.getByRole('button', { name: 'Enable investigation access' })).toBeDisabled();
  },
};
let stale: { resolve: (value: unknown) => void; first: Promise<unknown>; calls: number };
export const StaleOwnerReply: Story = {
  beforeEach: () => {
    let resolve!: (value: unknown) => void;
    const first = new Promise((done) => { resolve = done; });
    stale = { resolve, first, calls: 0 };
    mockIPC((command) => {
      if (command === 'mcp_reads_status') return 'disabled';
      if (command === 'mcp_reads_owner_status') {
        stale.calls += 1;
        return stale.calls === 1 ? first : { selected_client: null, task_dispatch_available: true, task_grant: null };
      }
      throw new Error('unexpected command');
    });
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await waitFor(() => expect(stale.calls).toBe(1));
    await userEvent.click(c.getByRole('button', { name: 'Refresh read service' }));
    await expect(await c.findByText('Task grant: none')).toBeVisible();
    stale.resolve({ selected_client: 'stale-client', task_dispatch_available: true,
      task_grant: { client_id: 'stale-client', maximum_identities: 99 } });
    await stale.first;
    await waitFor(() => expect(c.getByText('Task grant: none')).toBeVisible());
    await expect(c.queryByText(/99 request identities/)).toBeNull();
  },
};
