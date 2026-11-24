import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { QuickDispatchHint } from './QuickDispatchHint';

const meta = {
  title: 'Shell/QuickDispatchHint',
  component: QuickDispatchHint,
  args: { fallbackEnabled: true, mac: true, onOpen: fn(), onDismiss: fn() },
} satisfies Meta<typeof QuickDispatchHint>;
export default meta;
type Story = StoryObj<typeof meta>;

// AC-1702: the pill names the shortcut, opens quick dispatch and can be hidden.
export const OpensAndHides: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Cmd+Space')).toBeVisible();
    await expect(canvas.getByText('Cmd+J')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: /Dispatch to an agent/ }));
    await expect(args.onOpen).toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('button', { name: 'Hide the quick dispatch hint' }));
    await expect(args.onDismiss).toHaveBeenCalled();
  },
};

export const WithoutFallback: Story = {
  args: { fallbackEnabled: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Cmd+Space')).toBeVisible();
    await expect(canvas.queryByText('Cmd+J')).toBeNull();
  },
};

// Windows and Linux name Ctrl+J only: Ctrl+Space switches input methods there.
export const WindowsShowsCtrlJ: Story = {
  args: { mac: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Ctrl+J')).toBeVisible();
    await expect(canvas.queryByText(/Cmd/)).toBeNull();
    await expect(canvas.queryByText('Ctrl+Space')).toBeNull();
  },
};
