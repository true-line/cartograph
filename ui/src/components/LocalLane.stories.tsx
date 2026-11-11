// AC-0590 (#1795): the local lane says whether the server runs, which models are missing and how to fetch them.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, within } from 'storybook/test';
import { LocalLane, type LocalLaneStatus } from './LocalLane';

const model = (name: string, purpose: string, installed: boolean) => ({
  model: name, purpose, installed, pull_command: installed ? null : `ollama pull ${name}`,
});
const stopped: LocalLaneStatus = { reachable: false, start_command: 'ollama serve', models: [] };
const oneMissing: LocalLaneStatus = {
  reachable: true, start_command: null,
  models: [
    model('nomic-embed-text', 'T2 similarity embeddings', true),
    model('qwen3:1.7b', 'candidate pre-ranking and triage summaries', false),
    model('qwen3:8b', 'T3 proposal generation (propose-only)', true),
  ],
};
const complete: LocalLaneStatus = { ...oneMissing, models: oneMissing.models.map((entry) => model(entry.model, entry.purpose, true)) };
let current: LocalLaneStatus | Error = stopped;

const meta = {
  title: 'Surfaces/LocalLane', component: LocalLane, args: { available: true },
  beforeEach: () => {
    current = stopped;
    mockIPC((command) => {
      if (command === 'local_lane_status') {
        if (current instanceof Error) throw current;
        return current;
      }
      throw new Error(`unexpected command ${command}`);
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof LocalLane>;
export default meta;
type Story = StoryObj<typeof meta>;

/** Server stopped: the start command is shown; starting it and checking again lists the models. */
export const ServerStoppedThenStarted: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const summary = await c.findByTestId('local-lane-summary');
    await expect(summary).toHaveTextContent('The local model server did not answer. Start it with ollama serve, then check again.');
    current = oneMissing;
    await userEvent.click(c.getByRole('button', { name: 'Check local lane' }));
    await expect(await c.findByText('The local model server is running. 1 of 3 models is missing.')).toBeVisible();
    await expect(c.getByTestId('local-model-qwen3:1.7b')).toHaveTextContent('Missing. Run ollama pull qwen3:1.7b');
    await expect(c.getByTestId('local-model-nomic-embed-text')).toHaveTextContent('Installed.');
  },
};

export const EveryModelInstalled: Story = {
  beforeEach: () => { current = complete; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByText('The local model server is running and every model is installed.')).toBeVisible();
    await expect(canvasElement.textContent).not.toContain('ollama pull');
  },
};

export const CheckFailed: Story = {
  beforeEach: () => { current = new Error('worker unavailable'); },
  play: async ({ canvasElement }) => {
    await expect(await within(canvasElement).findByRole('alert')).toHaveTextContent('worker unavailable');
  },
};

export const NoBackend: Story = {
  args: { available: false },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(c.getByText('The local lane is checked by the core — connect a backend to check it.')).toBeVisible();
    await expect(c.getByRole('button', { name: 'Check local lane' })).toBeDisabled();
  },
};
