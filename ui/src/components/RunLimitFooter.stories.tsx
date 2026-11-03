// AC-0564 (#1643): the composer footer follows the saved task limit while it stays mounted.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, waitFor, within } from 'storybook/test';
import { RunLimitFooter } from './RunLimitFooter';
import type { RunLimits } from '../runLimits';

const shipped: RunLimits = {
  active_seconds: 600, request_timeout_ms: 180_000, default_active_seconds: 600, default_request_timeout_ms: 180_000,
  max_active_seconds: 3_600, max_request_timeout_ms: 600_000, customized: false,
};
let stored = shipped;

const meta = {
  title: 'Surfaces/RunLimitFooter', component: RunLimitFooter,
  args: { specialistId: 'domain-analyst@18', limits: { model_invocations: 64, tool_actions: 56, active_seconds: 600 } },
  beforeEach: () => {
    stored = shipped;
    mockIPC((command) => {
      if (command === 'get_run_limits') return stored;
      throw new Error(`unexpected command ${command}`);
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof RunLimitFooter>;
export default meta;
type Story = StoryObj<typeof meta>;

// Limits saved in the Configure window reach a composer that stayed mounted when the app window regains focus.
export const ReloadsSavedLimitOnFocus: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await waitFor(() => expect(c.getByText(/600s active execution/)).toBeInTheDocument());
    stored = { ...shipped, active_seconds: 3_600, customized: true };
    window.dispatchEvent(new Event('focus'));
    await waitFor(() => expect(c.getByText(/3600s active execution/)).toBeInTheDocument());
  },
};

// Older specialist versions keep the policy value even when a limit is saved.
export const OlderSpecialistKeepsPolicyLimit: Story = {
  args: { specialistId: 'domain-analyst@3' },
  beforeEach: () => { stored = { ...shipped, active_seconds: 3_600, customized: true }; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await waitFor(() => expect(c.getByText(/600s active execution/)).toBeInTheDocument());
    window.dispatchEvent(new Event('focus'));
    await expect(c.queryByText(/3600s/)).toBeNull();
  },
};
