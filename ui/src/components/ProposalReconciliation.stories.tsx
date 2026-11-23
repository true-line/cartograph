import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ProposalReconciliation } from './ProposalReconciliation';
import type { ReconciliationView } from '../reconciliation';
import { reconciliationHistory, reconciliationResult } from '../reconciliationFixtures';

const VIEW: ReconciliationView = {
  proposalId: 'proposal:fixture', requestGeneration: 1, loading: false, reconciling: false, error: null,
  history: reconciliationHistory('changed', [reconciliationResult('b', 'changed', 9), reconciliationResult('a')]),
};

const meta = {
  title: 'Evidence/ProposalReconciliation', component: ProposalReconciliation,
  args: { onLoad: fn(), onReconcile: fn() },
} satisfies Meta<typeof ProposalReconciliation>;
export default meta;
type Story = StoryObj<typeof meta>;

export const ChangedSinceStagedWithHistory: Story = {
  // AC-1074: the derived state and every recorded result, newest first.
  args: { state: VIEW },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('reconciliation-state')).toHaveTextContent('Changed since it was staged');
    const results = canvas.getAllByRole('listitem').filter((item) => item.parentElement?.tagName === 'OL');
    await expect(results).toHaveLength(2);
    await expect(results[0]).toHaveTextContent('job 9');
    await expect(canvas.getByText(/Changed: the cited bytes differ/)).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Reconcile now' }));
    await expect(args.onReconcile).toHaveBeenCalledOnce();
  },
};

export const StaleIsNotCurrent: Story = {
  // AC-1074: a result read before the recovery moved is not shown as current.
  args: { state: { ...VIEW, history: reconciliationHistory('reconciliation_stale', [reconciliationResult('a')]) } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('reconciliation-state')).toHaveTextContent('the recovery has moved since');
  },
};

export const NeverReconciled: Story = {
  args: { state: { ...VIEW, history: reconciliationHistory('awaiting_reconciliation') } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Not yet reconciled')).toBeInTheDocument();
    await expect(canvas.getByText(/No reconciliation has been recorded/)).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Show results' }));
    await expect(args.onLoad).toHaveBeenCalledOnce();
  },
};

export const FailureRecordsNothing: Story = {
  // AC-1074: a failed read shows no earlier result as current.
  args: { state: { ...VIEW, history: null, error: 'The current recovery could not be read, so nothing was recorded. No proposal or review was changed.' } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent('nothing was recorded');
    await expect(canvas.queryByTestId('reconciliation-state')).not.toBeInTheDocument();
  },
};

export const Reconciling: Story = {
  args: { state: { ...VIEW, loading: true, reconciling: true, history: null } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Reconciling…' })).toBeDisabled();
  },
};
