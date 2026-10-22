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


// AC-0290: packaged guidance is independent of service activation and configuration.
export const DiscoverSkillWhileDisabled: Story = {
  beforeEach: () => {
    mockIPC((command) => {
      if (command === 'mcp_reads_status') return 'disabled';
      if (command === 'mcp_reads_client_skill') return {
        status: 'available', app_version: '0.20.0',
        skill_path: '/fixture/resources/skills/read-cartograph-context/SKILL.md',
        files: [
          { path: 'SKILL.md', content: '# Read context progressively' },
          { path: 'references/connection.md', content: 'Install through the chosen client workflow.' },
          { path: 'references/requests.md', content: 'Request only needed pages.' },
          { path: 'references/measurement.md', content: 'Report token counts as unknown unless measured.' },
        ],
      };
      throw new Error('Skill discovery must not activate or configure a service');
    });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText('Read service: disabled')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Show packaged client skill' }));
    await expect(await canvas.findByLabelText('Packaged SKILL.md')).toHaveTextContent('Read context progressively');
    await expect(canvas.getByText('Client skill · Cartograph 0.20.0')).toBeVisible();
    await expect(canvas.getByText('/fixture/resources/skills/read-cartograph-context/SKILL.md')).toBeVisible();
    await expect(canvas.getByText('Request only needed pages.')).not.toBeVisible();
    await userEvent.click(canvas.getByText('references/requests.md'));
    await expect(canvas.getByText('Request only needed pages.')).toBeVisible();
    await expect(canvas.getByText('references/connection.md')).toBeVisible();
    await expect(canvas.getByText('references/measurement.md')).toBeVisible();
    await expect(canvas.getByText('Read service: disabled')).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Show client configuration' })).toBeDisabled();
  },
};

export const MissingSkillPreservesRunningService: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText('Read service: running')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Show client configuration' }));
    await expect(await canvas.findByLabelText('MCP client configuration')).toBeVisible();
    mockIPC((command) => {
      if (command === 'mcp_reads_client_skill') return { status: 'unavailable' };
      throw new Error('Skill lookup must not change service state');
    });
    await userEvent.click(canvas.getByRole('button', { name: 'Show packaged client skill' }));
    await expect(await canvas.findByRole('alert')).toHaveTextContent('packaged client skill is unavailable');
    await expect(canvas.getByText('Read service: running')).toBeVisible();
    await expect(canvas.getByLabelText('MCP client configuration')).toBeVisible();
  },
};
