import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { GraphStatsCard } from './GraphStatsCard';

const meta = {
  title: 'Shell/GraphStatsCard',
  component: GraphStatsCard,
  args: { canClear: true, clearing: false, error: null, onClear: fn() },
} satisfies Meta<typeof GraphStatsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NoBackend: Story = {
  args: { stats: null, canClear: false },
};

export const Populated: Story = {
  // AC-0085 (#162): the destructive action speaks in system terms and the
  // confirmation names exactly what is removed and what survives.
  args: {
    stats: { nodes: 1284, edges: 4021 },
    systemContents: [
      { repo: 'acme/shop', commit: 'a1b2c3d4e5f6' },
      { repo: 'local/infra', commit: 'workdir' },
    ],
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Remove every recovered fact for acme/shop, local/infra? Job history and settings are kept.',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Keep system' }));
    await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await expect(args.onClear).toHaveBeenCalledOnce();
  },
};

export const PopulatedWithoutContents: Story = {
  // Degrades against a core without system_contents: system phrasing stays,
  // the repo list is simply absent.
  args: { stats: { nodes: 12, edges: 18 } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Remove every recovered fact in this system? Job history and settings are kept.',
    );
  },
};

export const Empty: Story = {
  args: { stats: { nodes: 0, edges: 0 }, canClear: false },
};

export const ClearFailed: Story = {
  args: { stats: { nodes: 12, edges: 18 }, error: 'storage: database is locked' },
};

export const RegisteredRepositoryLabelsInConfirmation: Story = {
  // AC-0144: destructive confirmation uses operational labels from SystemRepo.
  args: {
    stats: { nodes: 12, edges: 18 },
    systemContents: [
      { repo: 'local/src_11111111111111111111111111111111', display_name: 'Billing service', commit: 'workdir' },
      { repo: 'acme/infra', commit: 'workdir' },
    ],
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Remove every recovered fact for Billing service, acme/infra?',
    );
    await expect(canvas.getByRole('alert')).not.toHaveTextContent('local/src_');
    await userEvent.click(canvas.getByRole('button', { name: 'Keep system' }));
    await expect(args.onClear).not.toHaveBeenCalled();
  },
};

export const SameNamedRepositoriesRemainDistinct: Story = {
  // AC-0144: clear confirmation distinguishes every same-named local and
  // GitHub source by its exact key without expanding unique human labels.
  args: {
    stats: { nodes: 12, edges: 18 },
    systemContents: [
      { repo: 'local/src_11111111111111111111111111111111', display_name: 'mirror', commit: 'workdir' },
      { repo: 'local/src_22222222222222222222222222222222', display_name: ' mirror ', commit: 'workdir' },
      { repo: 'acme/shop', display_name: 'shop', commit: 'a1b2c3d4e5f6' },
      { repo: 'other/shop', display_name: 'shop', commit: 'b2c3d4e5f6a1' },
      { repo: 'acme/billing', display_name: 'Billing service', commit: 'workdir' },
      { repo: 'legacy/mirror', commit: 'workdir' },
      { repo: 'legacy/blank', display_name: '  ', commit: 'workdir' },
    ],
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Remove every recovered fact for mirror (local/src_11111111111111111111111111111111), '
      + 'mirror (local/src_22222222222222222222222222222222), '
      + 'shop (acme/shop), shop (other/shop), Billing service, legacy/mirror, legacy/blank? '
      + 'Job history and settings are kept.',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Keep system' }));
    await expect(args.onClear).not.toHaveBeenCalled();
  },
};

export const NewProject: Story = {
  // AC-1013, AC-1014 (#1994): New project names the repos it sets aside
  // and what every project shares, and starts only once confirmed.
  args: {
    stats: { nodes: 1284, edges: 4021 },
    systemContents: [
      { repo: 'acme/shop', commit: 'a1b2c3d4e5f6' },
      { repo: 'local/infra', commit: 'workdir' },
    ],
    newProject: { can: true, starting: false, error: null, onStart: fn() },
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'New project…' }));
    const alert = canvas.getByRole('alert');
    await expect(alert).toHaveTextContent('Start a new project? This sets aside acme/shop, local/infra');
    await expect(alert).toHaveTextContent(
      'Settings, provider keys, run limits, the local model and installed adapters are shared by every project.',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Keep project' }));
    await expect(args.newProject?.onStart).not.toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('button', { name: 'New project…' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Start new project' }));
    await expect(args.newProject?.onStart).toHaveBeenCalledOnce();
  },
};

export const NewProjectWhileWorkRuns: Story = {
  // AC-1013: refused while a job is queued or running.
  args: {
    stats: { nodes: 12, edges: 18 },
    newProject: { can: false, starting: false, error: null, onStart: fn() },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'New project…' })).toBeDisabled();
  },
};
