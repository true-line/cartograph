// AC-0586 (#1763): enter, replace and remove a provider key; the key is never shown.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { ProviderKeys, type ConnectionCheck, type ProviderKeyStatus } from './ProviderKeys';

const none: ProviderKeyStatus = {
  provider: 'anthropic', label: 'Anthropic', env_var: 'ANTHROPIC_API_KEY', source: 'none', stored: false, store_error: null,
};
let current = none;
let received: string[] = [];
let checkResult: ConnectionCheck | Error = { provider: 'anthropic', outcome: 'accepted', bytes_sent: 96 };
let checksMade = 0;
let releaseCheck: (() => void) | null = null;
let holdCheck = false;

const meta = {
  title: 'Surfaces/ProviderKeys', component: ProviderKeys, args: { available: true },
  beforeEach: () => {
    current = none;
    received = [];
    checkResult = { provider: 'anthropic', outcome: 'accepted', bytes_sent: 96 };
    checksMade = 0;
    releaseCheck = null;
    holdCheck = false;
    mockIPC((command, args) => {
      const payload = args as { provider?: string; key?: string };
      if (command === 'provider_key_status') return [current];
      if (command === 'set_provider_key') {
        if ((payload.key ?? '').includes(' ')) throw new Error('A key is one line with no spaces.');
        received.push(payload.key ?? '');
        current = { ...current, source: current.source === 'environment' ? 'environment' : 'stored', stored: true };
        return current;
      }
      if (command === 'remove_provider_key') {
        current = { ...current, source: current.source === 'environment' ? 'environment' : 'none', stored: false };
        return current;
      }
      if (command === 'check_provider_connection') {
        checksMade += 1;
        if (checkResult instanceof Error) throw checkResult;
        const result = checkResult;
        if (holdCheck) return new Promise((resolve) => { releaseCheck = () => resolve(result); });
        return result;
      }
      throw new Error(`unexpected command ${command}`);
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof ProviderKeys>;
export default meta;
type Story = StoryObj<typeof meta>;

export const SaveReplaceAndRemove: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByText('No key. The cloud lane stays closed.')).toBeVisible();
    const save = c.getByRole('button', { name: 'Save key' });
    await expect(save).toBeDisabled();
    const input = c.getByLabelText('API key');
    await expect(input).toHaveAttribute('type', 'password');
    await userEvent.type(input, 'sk-ant-story-key');
    await userEvent.click(save);
    await expect(await c.findByText('A key is saved in the system credential store.')).toBeVisible();
    await expect(received).toEqual(['sk-ant-story-key']);
    // The field is cleared and the page never shows what was typed.
    await expect(c.getByLabelText('Replace key', { selector: 'input' })).toHaveValue('');
    await expect(canvasElement.textContent).not.toContain('sk-ant-story-key');
    await userEvent.type(c.getByLabelText('Replace key', { selector: 'input' }), 'sk-ant-second');
    await userEvent.click(c.getByRole('button', { name: 'Replace key' }));
    await waitFor(() => expect(received).toEqual(['sk-ant-story-key', 'sk-ant-second']));
    await userEvent.click(c.getByRole('button', { name: 'Remove key' }));
    await expect(await c.findByText('No key. The cloud lane stays closed.')).toBeVisible();
    await expect(c.queryByRole('button', { name: 'Remove key' })).toBeNull();
  },
};

export const EnvironmentVariableTakesPrecedence: Story = {
  beforeEach: () => { current = { ...none, source: 'environment', stored: true }; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByText(
      'Using the ANTHROPIC_API_KEY environment variable. A saved key is also present; the variable takes precedence.',
    )).toBeVisible();
  },
};

export const RefusedKeyShowsTheReason: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.type(await c.findByLabelText('API key'), 'two words');
    await userEvent.click(c.getByRole('button', { name: 'Save key' }));
    await expect(await c.findByRole('alert')).toHaveTextContent('A key is one line with no spaces.');
    await expect(c.getByText('No key. The cloud lane stays closed.')).toBeVisible();
  },
};

export const NoBackend: Story = {
  args: { available: false },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(c.getByText('Provider keys live in the core — connect a backend to change them.')).toBeVisible();
  },
};

// AC-0590 (#1795): the check says what is sent, reports the outcome and the recorded bytes, never the key.
export const CheckConnectionAccepted: Story = {
  beforeEach: () => { current = { ...none, source: 'stored', stored: true }; },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(await c.findByText(/sends one request for the provider's model list/)).toBeVisible();
    await userEvent.click(await c.findByRole('button', { name: 'Check connection' }));
    await expect(await c.findByTestId('provider-check-anthropic')).toHaveTextContent(
      'The provider accepted the key. 96 bytes were sent and added to the egress record.',
    );
    await expect(checksMade).toBe(1);
    // Replacing the key drops the earlier result.
    await userEvent.type(c.getByLabelText('Replace key', { selector: 'input' }), 'sk-ant-second');
    await userEvent.click(c.getByRole('button', { name: 'Replace key' }));
    await waitFor(() => expect(c.queryByTestId('provider-check-anthropic')).toBeNull());
  },
};

export const CheckConnectionRefused: Story = {
  beforeEach: () => {
    current = { ...none, source: 'stored', stored: true };
    checkResult = { provider: 'anthropic', outcome: 'refused', bytes_sent: 96 };
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(await c.findByRole('button', { name: 'Check connection' }));
    await expect(await c.findByTestId('provider-check-anthropic')).toHaveTextContent('The provider refused the key.');
  },
};

export const CheckConnectionUnreachable: Story = {
  beforeEach: () => {
    current = { ...none, source: 'environment' };
    checkResult = { provider: 'anthropic', outcome: 'unreachable', bytes_sent: 96 };
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(await c.findByRole('button', { name: 'Check connection' }));
    await expect(await c.findByTestId('provider-check-anthropic')).toHaveTextContent(
      'The provider could not be reached, so the key is unconfirmed.',
    );
  },
};

export const CheckConnectionNeedsAKeyAndARecord: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    // With no key there is nothing to check.
    await expect(await c.findByRole('button', { name: 'Check connection' })).toBeDisabled();
    await userEvent.type(c.getByLabelText('API key'), 'sk-ant-story-key');
    await userEvent.click(c.getByRole('button', { name: 'Save key' }));
    checkResult = new Error('the egress record could not be written, so nothing was sent');
    await waitFor(() => expect(c.getByRole('button', { name: 'Check connection' })).toBeEnabled());
    await userEvent.click(c.getByRole('button', { name: 'Check connection' }));
    await expect(await c.findByTestId('provider-check-anthropic')).toHaveTextContent(
      'The check did not run: Error: the egress record could not be written, so nothing was sent',
    );
  },
};

/** A check still running when the key is replaced must not report on the new key; the egress total is refreshed. */
export const ReplacedKeyDropsARunningCheck: Story = {
  args: { onEgressRecorded: fn() },
  beforeEach: () => { current = { ...none, source: 'stored', stored: true }; holdCheck = true; },
  play: async ({ canvasElement, args }) => {
    const c = within(canvasElement);
    await userEvent.click(await c.findByRole('button', { name: 'Check connection' }));
    await expect(await c.findByTestId('provider-check-anthropic')).toHaveTextContent('Checking…');
    await userEvent.type(c.getByLabelText('Replace key', { selector: 'input' }), 'sk-ant-second');
    await userEvent.click(c.getByRole('button', { name: 'Replace key' }));
    await waitFor(() => expect(c.queryByTestId('provider-check-anthropic')).toBeNull());
    releaseCheck?.();
    await waitFor(() => expect(args.onEgressRecorded).toHaveBeenCalledTimes(1));
    await expect(c.queryByTestId('provider-check-anthropic')).toBeNull();
  },
};
