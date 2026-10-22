import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, within } from 'storybook/test';
import { McpSettings } from './McpSettings';
const meta = {
  title: 'Surfaces/McpSettings', component: McpSettings, args: { available: true },
  beforeEach: () => {
    let active = true;
    mockIPC((command) => {
      if (command === 'mcp_reads_status') return active ? 'running' : 'disabled';
      if (command === 'mcp_reads_disable') { active = false; return null; }
      if (command === 'mcp_reads_client_configuration') return { mcpServers: { cartograph: { command: '/fixture/cartograph-mcp', args: [] } } };
      throw new Error('unexpected command');
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof McpSettings>;
export default meta;
type Story = StoryObj<typeof meta>;
export const ConfigureThenDisable: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText('Read service: running')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Show client configuration' }));
    await expect(await canvas.findByLabelText('MCP client configuration')).toHaveTextContent('/fixture/cartograph-mcp');
    await userEvent.click(canvas.getByRole('button', { name: 'Disable read access' }));
    await expect(await canvas.findByText('Read service: disabled')).toBeVisible();
    await expect(canvas.queryByLabelText('MCP client configuration')).toBeNull();
  },
};

export const FailedRefreshClearsLastKnownStatus: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText('Read service: running')).toBeVisible();
    mockIPC(() => { throw new Error('unavailable'); });
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh read service' }));
    await expect(await canvas.findByText('Read service: unavailable')).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Show client configuration' })).toBeDisabled();
    await expect(canvas.getByRole('button', { name: 'Enable read access' })).toBeDisabled();
  },
};
