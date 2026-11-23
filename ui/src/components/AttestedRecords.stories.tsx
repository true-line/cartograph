import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { AttestedView } from '../attested';
import { AttestedRecords } from './AttestedRecords';

// Placeholder contributors only (ADR-0090 decision 4).
const AGENT_ONLY: AttestedView = {
  record_id: 'attested-v1:a1',
  citation_id: 'A-00000000000000a1',
  target: { kind: 'gap', gap_id: 'gap:login-owner', slot: 'owner' },
  statement: 'The identity team owns the login page.',
  standing: 'agent_attested_unconfirmed',
  label: 'agent-attested, unconfirmed',
  attestation_count: 2,
  contributors: [
    { display_name: 'domain-analyst@29 (local/fixture)', kind: 'agent', verb: 'stated', run: 'investigation-1' },
    { display_name: 'domain-analyst@29 (local/other)', kind: 'agent', verb: 'agreed', run: 'investigation-2' },
  ],
};
const CONFIRMED: AttestedView = {
  ...AGENT_ONLY,
  record_id: 'attested-v1:a2',
  citation_id: 'A-00000000000000a2',
  standing: 'engineer_backed',
  label: 'attested',
  attestation_count: 3,
  contributors: [
    ...AGENT_ONLY.contributors,
    { display_name: 'Placeholder Engineer', kind: 'engineer', verb: 'confirmed', run: null },
  ],
};

const meta = {
  title: 'Attested/AttestedRecords',
  component: AttestedRecords,
  args: { records: [AGENT_ONLY, CONFIRMED], onReview: fn() },
} satisfies Meta<typeof AttestedRecords>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LabelledWithContributorsAndReview: Story = {
  // AC-1081 (T-1081): each record carries its own label, never a tier, and
  // names every contributor with what they did; the engineer confirms an
  // agent's record and withdraws a confirmed one.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const rows = canvas.getAllByTestId('attested-row');
    await expect(rows).toHaveLength(2);
    const labels = canvas.getAllByTestId('attested-label').map((label) => label.textContent);
    await expect(labels).toEqual(['agent-attested, unconfirmed', 'attested']);
    await expect(canvas.getAllByTestId('attested-contributors')[1]).toHaveTextContent(
      'domain-analyst@29 (local/fixture) stated · domain-analyst@29 (local/other) agreed · Placeholder Engineer confirmed',
    );
    await expect(canvas.getAllByText(/the Gap stays open/)).toHaveLength(2);
    const first = within(rows[0]);
    await expect(first.queryByRole('button', { name: 'Withdraw' })).toBeNull();
    await userEvent.click(first.getByRole('button', { name: 'Confirm' }));
    await expect(args.onReview).toHaveBeenCalledWith(AGENT_ONLY, 'confirmed');
    const second = within(rows[1]);
    await expect(second.queryByRole('button', { name: 'Confirm' })).toBeNull();
    await userEvent.click(second.getByRole('button', { name: 'Withdraw' }));
    await expect(args.onReview).toHaveBeenCalledWith(CONFIRMED, 'withdrew');
  },
};

export const ReadOnly: Story = {
  args: { onReview: undefined },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).queryAllByRole('button')).toHaveLength(0);
  },
};

export const Empty: Story = {
  args: { records: [], empty: 'No attested answer for this Gap.' },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText('No attested answer for this Gap.')).toBeVisible();
  },
};

export const NeedsEngineer: Story = {
  args: { needsEngineer: true, error: 'no_engineer', onSetEngineer: fn() },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('textbox'), 'Placeholder Engineer');
    await userEvent.click(canvas.getByRole('button', { name: 'Save' }));
    await expect(args.onSetEngineer).toHaveBeenCalledWith('Placeholder Engineer');
  },
};
