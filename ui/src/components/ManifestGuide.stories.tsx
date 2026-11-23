import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ManifestGuide } from './ManifestGuide';

const meta = {
  title: 'Ingest/ManifestGuide', component: ManifestGuide,
  args: { preview: fn().mockResolvedValue('[[repos]]\nurl = "../api"'), save: fn().mockResolvedValue('/new/cartograph.system.toml'), onSaved: fn() },
} satisfies Meta<typeof ManifestGuide>;
export default meta;
type Story = StoryObj<typeof meta>;

// AC-0002: editable rows preserve exact strings; saving follows a preview only.
export const CreateAndSave: Story = {
  play: async ({ canvasElement, args }) => {
    args.preview.mockResolvedValue('[[repos]]\nurl = "../api"');
    args.save.mockResolvedValue('/new/cartograph.system.toml');
    const c = within(canvasElement);
    await expect(c.getByRole('button', { name: 'Save new manifest' })).toBeDisabled();
    await userEvent.type(c.getByLabelText('Repository 1'), '../api');
    await userEvent.click(c.getByRole('button', { name: 'Add configuration' }));
    await userEvent.type(c.getByLabelText('Configuration key 1'), 'SETTINGS.apiUrl');
    await userEvent.type(c.getByLabelText('Configuration value 1'), 'https://api.example.test');
    await userEvent.click(c.getByRole('button', { name: 'Add service' }));
    await userEvent.type(c.getByLabelText('Service name 1'), 'api');
    await userEvent.selectOptions(c.getByLabelText('Service repository 1'), '../api');
    const urls = ' https://api.example.test  \n\n  https://backup.example.test \n';
    await userEvent.type(c.getByLabelText('Service base URLs 1 (one per line)'), urls);
    await expect(c.getByLabelText('Service base URLs 1 (one per line)')).toHaveValue(urls);
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await expect(args.preview).toHaveBeenCalledWith({ repos: [{ url: '../api' }], env: [{ key: 'SETTINGS.apiUrl', value: 'https://api.example.test' }], services: [{ name: 'api', repo: '../api', base_urls: ['https://api.example.test', 'https://backup.example.test'] }], gateway_routes: [] });
    await c.findByLabelText('Manifest preview');
    await userEvent.click(c.getByRole('button', { name: 'Save new manifest' }));
    await expect(args.save).toHaveBeenCalledWith(args.preview.mock.calls.at(-1)?.[0]);
    await expect(c.getByLabelText('Service base URLs 1 (one per line)')).toHaveValue(urls);
    await expect(args.onSaved).toHaveBeenCalledWith('/new/cartograph.system.toml');
  },
};
export const CancelRetainsDraft: Story = {
  args: { save: fn().mockResolvedValue(null) },
  play: async ({ canvasElement, args }) => {
    args.preview.mockResolvedValue('[[repos]]\nurl = "../api"');
    args.save.mockResolvedValue(null);
    const c = within(canvasElement);
    await userEvent.type(c.getByLabelText('Repository 1'), '../api');
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await c.findByLabelText('Manifest preview');
    await userEvent.click(c.getByRole('button', { name: 'Save new manifest' }));
    await c.findByText('Save cancelled. Your draft is still here.');
    await expect(args.onSaved).not.toHaveBeenCalled();
    await expect(c.getByLabelText('Repository 1')).toHaveValue('../api');
    await userEvent.type(c.getByLabelText('Repository 1'), '-edited');
    await expect(c.queryByLabelText('Manifest preview')).not.toBeInTheDocument();
    await expect(c.getByRole('button', { name: 'Save new manifest' })).toBeDisabled();
  },
};
export const ValidationRefusal: Story = {
  args: { preview: fn().mockRejectedValue('A configuration key is declared more than once.') },
  play: async ({ canvasElement, args }) => {
    args.preview.mockRejectedValue('A configuration key is declared more than once.');
    const c = within(canvasElement);
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await c.findByText('A configuration key is declared more than once.');
    await expect(c.getByRole('button', { name: 'Save new manifest' })).toBeDisabled();
    await expect(args.save).not.toHaveBeenCalled();
  },
};
export const SaveFailure: Story = {
  args: { save: fn().mockRejectedValue('Choose an unused writable path.') },
  play: async ({ canvasElement, args }) => {
    args.preview.mockResolvedValue('[[repos]]\nurl = "../api"');
    args.save.mockRejectedValue('Choose an unused writable path.');
    const c = within(canvasElement);
    await userEvent.type(c.getByLabelText('Repository 1'), '../api');
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await c.findByLabelText('Manifest preview');
    await userEvent.click(c.getByRole('button', { name: 'Save new manifest' }));
    await c.findByText('Choose an unused writable path.');
    await expect(args.onSaved).not.toHaveBeenCalled();
    await expect(c.getByLabelText('Repository 1')).toHaveValue('../api');
  },
};

export const GatewayAndRemoveRows: Story = {
  play: async ({ canvasElement, args }) => {
    args.preview.mockResolvedValue('repos = []');
    const c = within(canvasElement);
    await userEvent.type(c.getByLabelText('Repository 1'), '../api');
    await userEvent.click(c.getByRole('button', { name: 'Add repository' }));
    await userEvent.type(c.getByLabelText('Repository 2'), '../unused');
    await userEvent.click(c.getByRole('button', { name: 'Remove repository 2' }));
    await userEvent.click(c.getByRole('button', { name: 'Add configuration' }));
    await userEvent.click(c.getByRole('button', { name: 'Remove configuration 1' }));
    await userEvent.click(c.getByRole('button', { name: 'Add service' }));
    await userEvent.click(c.getByRole('button', { name: 'Remove service 1' }));
    await userEvent.click(c.getByRole('button', { name: 'Add gateway route' }));
    await userEvent.type(c.getByLabelText('Gateway base URL 1'), 'https://gateway.example.test');
    await userEvent.type(c.getByLabelText('Gateway prefix 1'), 'api');
    await userEvent.type(c.getByLabelText('Gateway service name 1'), 'api');
    await userEvent.click(c.getByLabelText('Strip gateway prefix 1'));
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await expect(args.preview).toHaveBeenLastCalledWith({ repos: [{ url: '../api' }], env: [], services: [], gateway_routes: [{ base_url: 'https://gateway.example.test', prefix: '/api', service: 'api', strip_prefix: true }] });
    await c.findByLabelText('Manifest preview');
    await userEvent.click(c.getByRole('button', { name: 'Remove gateway 1' }));
    await expect(c.queryByLabelText('Manifest preview')).not.toBeInTheDocument();
  },
};

export const MissingSaveReceipt: Story = {
  play: async ({ canvasElement, args }) => {
    args.preview.mockResolvedValue('repos = []');
    args.save.mockResolvedValue('');
    const c = within(canvasElement);
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await c.findByLabelText('Manifest preview');
    await userEvent.click(c.getByRole('button', { name: 'Save new manifest' }));
    await c.findByText('Error: Save did not return a file path.');
    await expect(args.onSaved).not.toHaveBeenCalled();
  },
};

// #2474: the guide suggests the keys the last recovery's client calls could
// not resolve; adding one starts a configuration row with that exact key.
export const SuggestionsFromLastRecovery: Story = {
  args: {
    suggestions: {
      keys: [
        { key: 'window.API_URL', calls: 15, sites: ['static/js/a.js', 'static/js/b.js', 'static/js/c.js', 'static/js/d.js'] },
        { key: 'VITE_API', calls: 1, sites: ['web/api.ts'] },
      ],
      computedSites: ['static/js/legacy.js'],
    },
  },
  play: async ({ canvasElement, args }) => {
    const c = within(canvasElement);
    const panel = within(c.getByRole('region', { name: 'Suggested from the last recovery' }));
    await expect(panel.getByText(/15 calls in static\/js\/a\.js, static\/js\/b\.js, static\/js\/c\.js and 1 more files/)).toBeInTheDocument();
    await expect(panel.getByText(/1 file builds a whole fetch URL at runtime \(static\/js\/legacy\.js\)/)).toBeInTheDocument();
    await userEvent.click(panel.getByRole('button', { name: 'Add window.API_URL' }));
    await expect(c.getByLabelText('Configuration key 1')).toHaveValue('window.API_URL');
    await expect(panel.getByRole('button', { name: 'window.API_URL added' })).toBeDisabled();
    await userEvent.type(c.getByLabelText('Repository 1'), '../api');
    await userEvent.type(c.getByLabelText('Configuration value 1'), 'https://app.example.test');
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await expect(args.preview).toHaveBeenCalledWith(expect.objectContaining({
      env: [{ key: 'window.API_URL', value: 'https://app.example.test' }],
    }));
  },
};

export const NoSuggestionsWithoutClientGaps: Story = {
  args: { suggestions: { keys: [], computedSites: [] } },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(c.queryByRole('region', { name: 'Suggested from the last recovery' })).not.toBeInTheDocument();
  },
};
