// AC-0586 (#1763): enter, replace and remove a provider key; the key is never shown.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import { ProviderKeys, type ProviderKeyStatus } from './ProviderKeys';

const none: ProviderKeyStatus = {
  provider: 'anthropic', label: 'Anthropic', env_var: 'ANTHROPIC_API_KEY', source: 'none', stored: false, store_error: null,
};
let current = none;
let received: string[] = [];

const meta = {
  title: 'Surfaces/ProviderKeys', component: ProviderKeys, args: { available: true },
  beforeEach: () => {
    current = none;
    received = [];
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
