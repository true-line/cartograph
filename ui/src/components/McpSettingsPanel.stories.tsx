import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { McpSettingsPanel } from './McpSettingsPanel';
const meta = {
  title: 'Surfaces/McpSettingsPanel', component: McpSettingsPanel,
  args: { status: 'disabled', busy: false, available: true, error: null, configuration: null,
    onEnable: fn(), onDisable: fn(), onRefresh: fn(), onConfiguration: fn() },
} satisfies Meta<typeof McpSettingsPanel>;
export default meta;
type Story = StoryObj<typeof meta>;
export const ExplicitActivation: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(args.onEnable).not.toHaveBeenCalled();
    await expect(canvas.getByRole('button', { name: 'Enable read access' })).toBeDisabled();
    await userEvent.click(canvas.getByText('Advanced resource policy'));
    await userEvent.click(canvas.getByText('Required policy fields'));
    await expect(canvas.getByText(/max_source_registry_bytes/)).toBeVisible();
    await expect(canvas.getByText(/max_corpus_bytes/)).toBeVisible();
    await userEvent.type(canvas.getByLabelText('Resource policy JSON'), 'invalid JSON');
    await expect(canvas.getByRole('button', { name: 'Enable read access' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh read service' }));
    await expect(args.onRefresh).toHaveBeenCalledOnce();
  },
};
export const Running: Story = {
  args: { status: 'running', configuration: '{"mcpServers":{"cartograph":{"command":"/Applications/Cartograph.app/Contents/MacOS/cartograph-mcp","args":["--record","/private/runtime/connection.json"]}}}' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Enable read access' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Disable read access' }));
    await expect(args.onDisable).toHaveBeenCalledOnce();
  },
};
export const Draining: Story = { args: { status: 'draining' } };
