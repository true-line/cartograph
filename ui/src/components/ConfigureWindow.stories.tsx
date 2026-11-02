// AC-0562 (#1633): Configure → Run Limits edits only the two time limits, bounded by [default, max].
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import { ConfigureWindow } from './ConfigureWindow';
import { tauriRunLimitsApi, type RunLimits } from '../runLimits';

const shipped: RunLimits = {
  active_seconds: 600, request_timeout_ms: 180_000, default_active_seconds: 600, default_request_timeout_ms: 180_000,
  max_active_seconds: 3_600, max_request_timeout_ms: 600_000, customized: false,
};
let calls: Array<[string, unknown]> = [];
let stored = shipped;
let loadFailure: string | null = null;

const meta = {
  title: 'Surfaces/ConfigureWindow', component: ConfigureWindow, args: { api: tauriRunLimitsApi },
  beforeEach: () => {
    calls = []; stored = shipped; loadFailure = null;
    mockIPC((command, args) => {
      calls.push([command, args]);
      if (command === 'get_run_limits') { if (loadFailure) throw new Error(loadFailure); return stored; }
      if (command === 'set_run_limits') {
        const { activeSeconds, requestTimeoutMs } = args as { activeSeconds: number; requestTimeoutMs: number };
        stored = { ...stored, active_seconds: activeSeconds, request_timeout_ms: requestTimeoutMs, customized: true };
        return stored;
      }
      if (command === 'reset_run_limits') { stored = shipped; return stored; }
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
