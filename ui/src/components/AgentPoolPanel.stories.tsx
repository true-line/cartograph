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
      { mode: 'cloud', provider_id: 'anthropic', model: 'cloud-model', endpoint: 'https://api.anthropic.com', deployment: null, available: false, unavailable_reason: 'no API key saved' },
    ],
    awaitingCuration: 3,
    usage: [
      { job_id: 3, calls: 1, unreported_calls: 0, input_tokens: 24120, output_tokens: 512, request_bytes: 99000, model: 'cloud-model' },
      { job_id: 5, calls: 4, unreported_calls: 4, input_tokens: 0, output_tokens: 0, request_bytes: 40000, model: 'qwen3:8b' },
    ],
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
    await expect(canvas.getByText('cloud · anthropic · cloud-model: unavailable (no API key saved)')).toBeVisible();
    await expect(canvas.getByTestId('agent-pool-curation')).toHaveTextContent('3 staged proposals are waiting');
    // AC-1703: reported use per job and in total; unreported counts are not estimated.
    await expect(canvas.getByTestId('agent-pool-usage-3')).toHaveTextContent('1 call to cloud-model: 24,120 input and 512 output tokens, as the provider reported.');
    await expect(canvas.getByTestId('agent-pool-usage-5')).toHaveTextContent('4 calls to qwen3:8b: no token count reported');
    await expect(canvas.getByTestId('agent-pool-usage-total')).toHaveTextContent('Listed work: 5 calls to 2 models: 24,120 input and 512 output tokens; 4 calls reported no count');
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
    await expect(canvas.getByTestId('agent-pool-usage-total')).toHaveTextContent('No model use is recorded for the listed work.');
  },
};

// AC-1703: a failed usage read is said, and no per-job counts are shown.
export const UsageUnreadable: Story = {
  args: { usage: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('agent-pool-usage-total')).toHaveTextContent('Reported model use could not be read.');
    await expect(canvas.queryByTestId('agent-pool-usage-3')).toBeNull();
  },
};

// AC-1704: pause and per-mode limits are sent to the host as one controls value.
export const Controls: Story = {
  args: {
    controls: { paused: false, local_limit: 2, cloud_limit: 0 },
    onChangeControls: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('agent-pool-controls')).toHaveTextContent('New agent work starts with at most 2 local and no cloud limit at a time.');
    await userEvent.click(canvas.getByRole('button', { name: 'Pause new agent work' }));
    await expect(args.onChangeControls).toHaveBeenCalledWith({ paused: true, local_limit: 2, cloud_limit: 0 });
    await userEvent.selectOptions(canvas.getByRole('combobox', { name: 'Cloud at a time' }), '3');
    await expect(args.onChangeControls).toHaveBeenLastCalledWith({ paused: false, local_limit: 2, cloud_limit: 3 });
  },
};

export const Paused: Story = {
  args: {
    controls: { paused: true, local_limit: 0, cloud_limit: 0 },
    controlsError: 'The pool controls were not saved: store busy',
    onChangeControls: fn(),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('agent-pool-controls')).toHaveTextContent('New agent work waits until the pool is resumed. Work already running continues.');
    await expect(canvas.getByRole('button', { name: 'Resume agent work' })).toBeVisible();
    await expect(canvas.getByRole('alert')).toHaveTextContent('store busy');
  },
};

export const LocalServerStopped: Story = {
  args: {
    localServer: { reachable: false, start_command: 'ollama serve', models: [] },
    onCheckLocalServer: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('agent-pool-local-server')).toHaveTextContent('The local model server did not answer. Start it with ollama serve.');
    await userEvent.click(canvas.getByRole('button', { name: 'Check local server' }));
    await expect(args.onCheckLocalServer).toHaveBeenCalled();
  },
};
