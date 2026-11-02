import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { InvestigationActivity } from './InvestigationActivity';
import { investigationEvents } from '../investigationFixtures';

const meta = { title: 'Investigations/Activity', component: InvestigationActivity,
  args: { events: investigationEvents() } } satisfies Meta<typeof InvestigationActivity>;
export default meta;
type Story = StoryObj<typeof meta>;

export const DurableOrderedActivity: Story = {
  // AC-0190: labels reflect recorded steps; a reservation is not tool completion.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const rows = canvas.getAllByRole('listitem');
    await expect(rows).toHaveLength(9);
    await expect(rows[5]).toHaveTextContent('Source validation budget reserved');
    await expect(rows[5]).not.toHaveTextContent('Tool completed');
    await expect(rows[6]).toHaveTextContent('Tool completed');
    await expect(rows.map((row) => row.querySelector('.investigation-sequence')?.textContent))
      .toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9']);
    await expect(canvas.queryByRole('progressbar')).not.toBeInTheDocument();
  },
};

export const Empty: Story = {
  args: { events: [] },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText('No activity loaded yet.')).toBeVisible();
  },
};

export const CapacityRefusal: Story = {
  args: { events: [{ ...investigationEvents()[0], kind: 'capacity_refused',
    summary: 'The proposed acquisition could not be admitted within the proven continuation capacity.' }] },
  // AC-0287: a refused acquisition is visible without implying tool execution.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Evidence request exceeds continuation capacity')).toBeVisible();
    await expect(canvas.queryByText('Tool completed')).not.toBeInTheDocument();
    await expect(canvas.queryByText('Unsupported event')).not.toBeInTheDocument();
  },
};

export const ProposedVersusExecuted: Story = {
  args: { events: [
    ...investigationEvents().slice(3, 4),
    { ...investigationEvents()[3], sequence: 5, kind: 'model_completed', summary: 'Model replied.' },
    { ...investigationEvents()[3], sequence: 6, kind: 'action_admitted', summary: 'Admitted read_evidence.' },
    { ...investigationEvents()[4], sequence: 7 },
    { ...investigationEvents()[6], sequence: 8 },
    { ...investigationEvents()[3], sequence: 9, kind: 'capacity_refused', summary: 'Refused.' },
  ] },
  // AC-0561: each step says whether the model's action was proposed, running, executed or not executed.
  play: async ({ canvasElement }) => {
    const rows = within(canvasElement).getAllByRole('listitem');
    await expect(rows.map((row) => row.querySelector('.investigation-stage')?.textContent?.trim())).toEqual([
      'Waiting for model', 'Model replied', 'Proposed by model · accepted', 'Running', 'Executed', 'Not executed']);
  },
};
