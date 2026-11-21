import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { AgentPoolPanel } from './AgentPoolPanel';
import type { Job } from '../store';

function job(overrides: Partial<Job> & Pick<Job, 'id' | 'kind' | 'status'>): Job {
  return { execution_tracking: 'recorded', created_at: '2026-11-21T10:00:00Z', updated_at: `2026-11-21T10:0${overrides.id}:00Z`, ...overrides };
}

const JOBS: Job[] = [
  job({ id: 6, kind: 'investigation-v1:inv', status: 'running', investigation_id: 'inv-0001', stage: 'model' }),
  job({ id: 5, kind: 'section-compose-gated:spec:3.2:area:local', status: 'queued' }),
  job({ id: 4, kind: 'ingest-source-v1:src_1', status: 'running' }),
  job({ id: 3, kind: 'area-ask:cloud', status: 'done' }),
  job({ id: 2, kind: 'escalate-class:12:local', status: 'failed', error: 'provider unavailable' }),
];

const meta = {
  title: 'Surfaces/AgentPoolPanel',
  component: AgentPoolPanel,
  args: {
    jobs: JOBS,
    providers: [
      { mode: 'local', provider_id: 'ollama', model: 'qwen3:8b', endpoint: 'http://127.0.0.1:11434', deployment: null, available: true, unavailable_reason: null },
      { mode: 'cloud', provider_id: 'anthropic', model: 'claude-opus-4-8', endpoint: 'https://api.anthropic.com', deployment: null, available: false, unavailable_reason: 'no API key saved' },
    ],
    awaitingCuration: 3,
    onOpenJobs: fn(),
    onOpenCuration: fn(),
    onViewInvestigation: fn(),
  },
} satisfies Meta<typeof AgentPoolPanel>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Busy: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const active = canvas.getByRole('region', { name: 'Queued and running agent work' });
    await expect(within(active).getByText('Specialist investigation')).toBeVisible();
    await expect(within(active).getByText('spec § 3.2')).toBeVisible();
    // Ingest is not agent work.
    await expect(canvas.queryByText(/src_1/)).toBeNull();
    await expect(canvas.getByText('cloud · anthropic · claude-opus-4-8: unavailable (no API key saved)')).toBeVisible();
    await expect(canvas.getByTestId('agent-pool-curation')).toHaveTextContent('3 staged proposals are waiting');
    await userEvent.click(within(active).getByRole('button', { name: 'View investigation' }));
    await expect(args.onViewInvestigation).toHaveBeenCalledWith('inv-0001');
    await userEvent.click(canvas.getByRole('button', { name: 'Open curation' }));
    await expect(args.onOpenCuration).toHaveBeenCalled();
  },
};

export const Idle: Story = {
  args: { jobs: [], providers: null, awaitingCuration: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('agent-pool-idle')).toBeVisible();
    await expect(canvas.getByText('The provider catalog has not loaded.')).toBeVisible();
    await expect(canvas.getByTestId('agent-pool-curation')).toHaveTextContent('Staged proposals have not loaded.');
  },
};
