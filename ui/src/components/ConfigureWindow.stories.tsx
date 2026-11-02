// AC-0562 (#1633): Configure → Run Limits edits only the two time limits, bounded by [default, max].
// AC-0564 (#1643): the local completion model is chosen from installed Ollama models.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import { ConfigureWindow } from './ConfigureWindow';
import { tauriRunLimitsApi, type LocalModelChoice, type RunLimits } from '../runLimits';

const shipped: RunLimits = {
  active_seconds: 600, request_timeout_ms: 180_000, default_active_seconds: 600, default_request_timeout_ms: 180_000,
  max_active_seconds: 3_600, max_request_timeout_ms: 600_000, customized: false,
};
let calls: Array<[string, unknown]> = [];
let stored = shipped;
let loadFailure: string | null = null;
const defaultModel: LocalModelChoice = { model: 'qwen3:8b', default_model: 'qwen3:8b', customized: false };
let model = defaultModel;
let installed: Array<{ name: string; digest: string }> | string = [
  { name: 'qwen3:8b', digest: '500a1f067a9f0000' }, { name: 'qwen3:30b-a3b', digest: 'ad815644918f0000' }];

const meta = {
  title: 'Surfaces/ConfigureWindow', component: ConfigureWindow, args: { api: tauriRunLimitsApi },
  beforeEach: () => {
    calls = []; stored = shipped; loadFailure = null; model = defaultModel;
    installed = [{ name: 'qwen3:8b', digest: '500a1f067a9f0000' }, { name: 'qwen3:30b-a3b', digest: 'ad815644918f0000' }];
    mockIPC((command, args) => {
      calls.push([command, args]);
      if (command === 'get_run_limits') { if (loadFailure) throw new Error(loadFailure); return stored; }
      if (command === 'set_run_limits') {
        const { activeSeconds, requestTimeoutMs } = args as { activeSeconds: number; requestTimeoutMs: number };
        stored = { ...stored, active_seconds: activeSeconds, request_timeout_ms: requestTimeoutMs, customized: true };
        return stored;
      }
      if (command === 'reset_run_limits') { stored = shipped; return stored; }
      if (command === 'get_local_model') return model;
      if (command === 'list_local_models') { if (typeof installed === 'string') throw new Error(installed); return installed; }
      if (command === 'set_local_model') {
        const name = (args as { model: string }).model;
        if (typeof installed === 'string' || !installed.some((entry) => entry.name === name)) throw new Error(`'${name}' is not installed in Ollama on this Mac.`);
        model = { ...model, model: name, customized: name !== model.default_model };
        return model;
      }
      if (command === 'reset_local_model') { model = defaultModel; return model; }
      throw new Error(`unexpected command ${command}`);
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof ConfigureWindow>;
export default meta;
type Story = StoryObj<typeof meta>;

export const RaiseLimitsThenRestore: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const task = await c.findByLabelText(/Task time limit/);
    const call = c.getByLabelText(/Per model call timeout/);
    await expect(task).toHaveValue(10);
    await expect(call).toHaveValue(3);
    await expect(c.getByRole('button', { name: 'Restore defaults' })).toBeDisabled();
    await userEvent.clear(task); await userEvent.type(task, '60');
    await userEvent.clear(call); await userEvent.type(call, '10');
    await userEvent.click(c.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(c.getByRole('status')).toHaveTextContent('New investigations use these limits'));
    await expect(calls).toContainEqual(['set_run_limits', { activeSeconds: 3_600, requestTimeoutMs: 600_000 }]);
    await userEvent.click(c.getByRole('button', { name: 'Restore defaults' }));
    await waitFor(() => expect(c.getByLabelText(/Task time limit/)).toHaveValue(10));
    await expect(c.getByLabelText(/Per model call timeout/)).toHaveValue(3);
  },
};

export const OutOfRangeIsNotSaved: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const task = await c.findByLabelText(/Task time limit/);
    await userEvent.clear(task); await userEvent.type(task, '61');
    await expect(c.getByText('Must be between 10 and 60 minutes.')).toBeInTheDocument();
    await expect(c.getByRole('button', { name: 'Save' })).toBeDisabled();
    await userEvent.clear(task); await userEvent.type(task, '5');
    await expect(c.getByRole('button', { name: 'Save' })).toBeDisabled();
    await expect(calls.some(([command]) => command === 'set_run_limits')).toBe(false);
  },
};

export const SaveRefusedByHost: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const call = await c.findByLabelText(/Per model call timeout/);
    mockIPC((command) => {
      if (command === 'set_run_limits') throw new Error('request_timeout_ms must be 180000..=600000, got 600001');
      return stored;
    });
    await userEvent.clear(call); await userEvent.type(call, '5');
    await userEvent.click(c.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(c.getByRole('alert')).toHaveTextContent('request_timeout_ms must be'));
  },
};

// Stored values rejected on read must remain repairable through the host reset.
export const InvalidStoredLimitsCanBeReset: Story = {
  beforeEach: () => { loadFailure = 'Stored run limits are invalid'; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByRole('alert')).toHaveTextContent('Stored run limits are invalid');
    await userEvent.click(c.getByRole('button', { name: 'Restore defaults' }));
    await expect(await c.findByLabelText(/Task time limit/)).toHaveValue(10);
    await expect(c.getByLabelText(/Per model call timeout/)).toHaveValue(3);
    await expect(calls.some(([command]) => command === 'reset_run_limits')).toBe(true);
  },
};

// Valid wire values need not be multiples of a tenth of a minute.
export const FractionalMinutesPreserveWireValues: Story = {
  beforeEach: () => {
    stored = { ...shipped, active_seconds: 601, request_timeout_ms: 180_001, customized: true };
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const task = await c.findByLabelText(/Task time limit/);
    const call = c.getByLabelText(/Per model call timeout/);
    await userEvent.click(c.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(calls).toContainEqual(['set_run_limits', { activeSeconds: 601, requestTimeoutMs: 180_001 }]));
    await userEvent.clear(call); await userEvent.type(call, '4');
    await userEvent.click(c.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(calls).toContainEqual(['set_run_limits', { activeSeconds: 601, requestTimeoutMs: 240_000 }]));
    await userEvent.clear(task); await userEvent.type(task, '11');
    await userEvent.click(c.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(calls).toContainEqual(['set_run_limits', { activeSeconds: 660, requestTimeoutMs: 240_000 }]));
  },
};

// AC-0564: pick an installed model, then return to the default.
export const ChooseInstalledLocalModel: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const select = await c.findByLabelText(/Completion model/);
    await waitFor(() => expect(c.getByText('Installed digest 500a1f067a9f.')).toBeInTheDocument());
    await expect(c.getByRole('button', { name: 'Use this model' })).toBeDisabled();
    await expect(c.getByRole('button', { name: 'Use default model' })).toBeDisabled();
    await userEvent.selectOptions(select, 'qwen3:30b-a3b');
    await expect(c.getByText('Installed digest ad815644918f.')).toBeInTheDocument();
    await userEvent.click(c.getByRole('button', { name: 'Use this model' }));
    await waitFor(() => expect(c.getByRole('status')).toHaveTextContent('New investigations use qwen3:30b-a3b'));
    await expect(calls).toContainEqual(['set_local_model', { model: 'qwen3:30b-a3b' }]);
    await userEvent.click(c.getByRole('button', { name: 'Use default model' }));
    await waitFor(() => expect(c.getByLabelText(/Completion model/)).toHaveValue('qwen3:8b'));
  },
};

// AC-0564: a saved model that is no longer installed is shown as such, never silently replaced.
export const SavedModelNotInstalled: Story = {
  beforeEach: () => { model = { model: 'llama3:70b', default_model: 'qwen3:8b', customized: true }; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const select = await c.findByLabelText(/Completion model/);
    await expect(select).toHaveValue('llama3:70b');
    await waitFor(() => expect(c.getByRole('option', { name: 'llama3:70b (not installed)' })).toBeInTheDocument());
    await expect(c.getByText('Not installed in Ollama on this Mac.')).toBeInTheDocument();
  },
};

// AC-0564: with Ollama unreachable the saved model still shows, and the reason is stated.
export const OllamaUnreachable: Story = {
  beforeEach: () => { installed = 'Ollama is not reachable on this Mac, so installed models cannot be listed.'; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByLabelText(/Completion model/)).toHaveValue('qwen3:8b');
    await waitFor(() => expect(c.getByText(/Ollama is not reachable on this Mac/)).toBeInTheDocument());
  },
};
