import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ProjectsCard } from './ProjectsCard';

const meta = {
  title: 'Shell/ProjectsCard',
  component: ProjectsCard,
  args: {
    projects: [
      { id: '0123456789abcdef', name: null, repos: [], active: true },
      { id: 'fedcba9876543210', name: 'Login service', repos: ['simple-login/app'], active: false },
      { id: '00112233aabbccdd', name: null, repos: ['vendure/vendure'], active: false },
    ],
    loadedRepos: ['acme/shop'],
    canSwitch: true,
    busy: false,
    error: null,
    onSwitch: fn(),
    onRename: fn(),
    onDelete: fn(),
  },
} satisfies Meta<typeof ProjectsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const SwitchProject: Story = {
  // AC-1014 (#1994): every project is listed, the loaded one marked; a
  // switch says it restarts and runs only once confirmed.
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    const rows = canvas.getAllByRole('listitem');
    await expect(rows[0]).toHaveTextContent('acme/shop Loaded');
    await expect(rows[1]).toHaveTextContent('Login service');
    await expect(rows[2]).toHaveTextContent('vendure/vendure');
    // The loaded project cannot be switched to or deleted.
    await expect(within(rows[0]).queryByRole('button', { name: 'Switch' })).toBeNull();
    await expect(within(rows[0]).queryByRole('button', { name: 'Delete' })).toBeNull();
    await userEvent.click(within(rows[1]).getByRole('button', { name: 'Switch' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Switch to Login service? The loaded project is set aside as it is, and Cartograph restarts with Login service loaded.',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Keep' }));
    await expect(args.onSwitch).not.toHaveBeenCalled();
    await userEvent.click(within(rows[1]).getByRole('button', { name: 'Switch' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Switch and restart' }));
    await expect(args.onSwitch).toHaveBeenCalledWith('fedcba9876543210');
  },
};

export const RenameAndDelete: Story = {
  // AC-1014 (#1994): a project is renamed in place; a parked one is
  // deleted only once confirmed.
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    const rows = canvas.getAllByRole('listitem');
    await userEvent.click(within(rows[0]).getByRole('button', { name: 'Rename' }));
    const input = canvas.getByRole('textbox', { name: 'Name for acme/shop' });
    await userEvent.type(input, 'Shop');
    await userEvent.click(canvas.getByRole('button', { name: 'Save' }));
    await expect(args.onRename).toHaveBeenCalledWith('0123456789abcdef', 'Shop');
    await userEvent.click(within(rows[2]).getByRole('button', { name: 'Delete' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent('Delete vendure/vendure?');
    await userEvent.click(canvas.getByRole('button', { name: 'Delete project' }));
    await expect(args.onDelete).toHaveBeenCalledWith('00112233aabbccdd');
  },
};

export const SwitchWhileWorkRuns: Story = {
  // AC-1014 (#1994): switching is unavailable while a job is queued or
  // running, since it restarts Cartograph.
  args: { canSwitch: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    for (const button of canvas.getAllByRole('button', { name: 'Switch' })) {
      await expect(button).toBeDisabled();
    }
  },
};
