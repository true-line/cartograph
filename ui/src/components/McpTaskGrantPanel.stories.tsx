// AC-0320: no client or numeric limit implies authority.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { McpTaskGrantPanel } from './McpTaskGrantPanel';
const meta = {
  title: 'Surfaces/McpTaskGrantPanel', component: McpTaskGrantPanel,
  args: { owner: { selected_client: null, task_dispatch_available: true, task_grant: null },
    clients: ['client-a'], selected: 'client-a', busy: false, canActivate: true, error: null,
    onSelect: fn(), onCreate: fn(), onPage: fn(), onGrant: fn() },
} satisfies Meta<typeof McpTaskGrantPanel>;
export default meta;
type Story = StoryObj<typeof meta>;
export const ExplicitLimits: Story = {
  play: async ({ canvasElement, args }) => {
    const c = within(canvasElement);
    const grant = c.getByRole('button', { name: 'Enable investigation access' });
    await expect(grant).toBeDisabled();
    await expect(args.onGrant).not.toHaveBeenCalled();
    const field = c.getByLabelText('Maximum request identities');
    for (const invalid of ['0', '-1', '1.5', '1e3', '9007199254740992']) {
      await userEvent.clear(field); await userEvent.type(field, invalid);
      await expect(grant).toBeDisabled();
    }
    await userEvent.clear(field); await userEvent.type(field, '4');
    await userEvent.click(grant);
    await expect(args.onGrant).toHaveBeenCalledWith('client-a', 4);
  },
};
export const Unavailable: Story = { args: { owner: null }, play: async ({ canvasElement }) => {
  for (const button of within(canvasElement).getAllByRole('button')) await expect(button).toBeDisabled();
} };
