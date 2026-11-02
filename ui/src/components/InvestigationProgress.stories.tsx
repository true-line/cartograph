import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { InvestigationProgress } from './InvestigationProgress';
import { investigationDetail, investigationEvents } from '../investigationFixtures';

const detail = investigationDetail();
const running = investigationEvents().slice(0, 4); // last recorded event: model_started at 10:00:03Z
const meta = { title: 'Investigations/Progress', component: InvestigationProgress,
  args: { status: 'running', cancelRequested: false, events: running, usage: detail.usage, limits: detail.limits,
    now: Date.parse('2026-09-10T10:00:17Z') } } satisfies Meta<typeof InvestigationProgress>;
export default meta;
type Story = StoryObj<typeof meta>;

export const WaitingForModel: Story = {
  // AC-0561: an active task states its current step, how long it has taken and the calls so far.
  play: async ({ canvasElement }) => {
    const status = within(canvasElement).getByRole('status', { name: 'Investigation progress' });
    await expect(status).toHaveTextContent('Waiting for the model to reply');
    await expect(status).toHaveTextContent('this step 14 s');
    await expect(status).toHaveTextContent('Working for 17 s');
    await expect(status).toHaveTextContent(`Model calls: ${detail.usage.model_invocations} of ${detail.limits.model_invocations}`);
    await expect(status).toHaveTextContent('Help → Debug Viewer');
  },
};

export const RunningAction: Story = {
  args: { events: investigationEvents().slice(0, 5) },
  // AC-0561: an executing tool is named in plain words.
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByRole('status')).toHaveTextContent('Running: reading source evidence');
  },
};

export const RefusedProposal: Story = {
  args: { events: [...running, { ...running[3], sequence: 5, kind: 'finish_refused', summary: 'Finish refused.' }] },
  // AC-0561: a refused proposal is reported as not executed.
  play: async ({ canvasElement }) => {
    const status = within(canvasElement).getByRole('status');
    await expect(status).toHaveTextContent('refused (not executed)');
    // A refusal can be charged without running anything: executed counts completed tools only.
    await expect(status).toHaveTextContent(`actions charged: ${detail.usage.tool_actions} of ${detail.limits.tool_actions}`);
    await expect(status).toHaveTextContent('(executed: 0)');
  },
};

export const RefusedQuery: Story = {
  args: { events: [...running, { ...running[3], sequence: 5, kind: 'query_refused', summary: 'Query refused.' }] },
  // AC-0561 (#1632 review): a refused graph query — the v0.60.0 pilot failure — is shown as not executed.
  play: async ({ canvasElement }) => {
    const status = within(canvasElement).getByRole('status');
    await expect(status).toHaveTextContent('query was refused (not executed)');
    await expect(status).toHaveTextContent('(executed: 0)');
  },
};

export const HiddenWhenFinished: Story = {
  args: { status: 'completed', events: investigationEvents() },
  // AC-0561: a finished task shows no live progress.
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).queryByRole('status')).not.toBeInTheDocument();
  },
};
