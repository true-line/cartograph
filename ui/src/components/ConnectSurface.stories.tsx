import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ConnectSurface } from './ConnectSurface';

const meta = {
  title: 'Ingest/ConnectSurface',
  component: ConnectSurface,
  args: {
    source: 'github',
    target: '',
    canPreflight: true,
    onSourceChange: fn(),
    onTargetChange: fn(),
    onBack: fn(),
    onPreflight: fn(),
  },
} satisfies Meta<typeof ConnectSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PickSourceAndTarget: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // The local-only contract is stated before any work starts.
    await expect(
      canvas.getByText(/Local-only preflight\. Nothing leaves the device/),
    ).toBeInTheDocument();

    // Segmented source selector: three options, GitHub active.
    const github = canvas.getByRole('radio', { name: 'GitHub' });
    await expect(github).toHaveAttribute('aria-checked', 'true');
    await userEvent.click(canvas.getByRole('radio', { name: 'Local folder' }));
    await expect(args.onSourceChange).toHaveBeenCalledWith('local');
    await userEvent.click(canvas.getByRole('radio', { name: 'System manifest' }));
    await expect(args.onSourceChange).toHaveBeenCalledWith('manifest');

    // An empty target cannot preflight.
    await expect(canvas.getByRole('button', { name: /preflight/i })).toBeDisabled();
    await userEvent.type(canvas.getByRole('textbox'), 'github.com/acme/shop');
    await expect(args.onTargetChange).toHaveBeenCalled();

    await userEvent.click(canvas.getByRole('button', { name: 'Back' }));
    await expect(args.onBack).toHaveBeenCalled();
  },
};

export const ReadyToPreflight: Story = {
  args: { source: 'local', target: '/repos/image-trail' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('textbox')).toHaveValue('/repos/image-trail');

    // #161: the ingest column centers in the window (auto inline margins
    // under a readable cap) instead of hugging the left edge.
    const flow = canvasElement.querySelector('.ingest-flow') as HTMLElement;
    const style = getComputedStyle(flow);
    await expect(style.maxWidth).toContain('860px');
    await expect(style.marginLeft).toBe(style.marginRight);

    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    await expect(args.onPreflight).toHaveBeenCalled();
  },
};

export const NoBackend: Story = {
  args: { target: 'github.com/acme/shop', canPreflight: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: /preflight/i })).toBeDisabled();
  },
};

/** AC-0224 (#246): re-opening Connect keeps the last target, but focused with
 *  the whole value selected, so typing a new path replaces it rather than
 *  appending to it. */
export const ReopenSelectsPreviousTarget: Story = {
  args: { source: 'local', target: '/Users/me/Code/spring-petclinic' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const input = canvas.getByRole('textbox') as HTMLInputElement;
    await expect(input).toHaveFocus();
    await expect(input.selectionStart).toBe(0);
    await expect(input.selectionEnd).toBe(input.value.length);
  },
};

/** AC-0224 (#246): Enter in the target field submits the preflight. */
export const EnterSubmitsPreflight: Story = {
  args: { source: 'local', target: '/repos/image-trail' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('textbox'), '{Enter}');
    await expect(args.onPreflight).toHaveBeenCalledOnce();
  },
};

/** AC-0224 (#246): Enter never submits what the Preflight button would not —
 *  an empty target or no backend. */
export const EnterRespectsDisabledPreflight: Story = {
  args: { source: 'local', target: '   ' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('textbox'), '{Enter}');
    await expect(args.onPreflight).not.toHaveBeenCalled();
  },
};

export const EnterWithoutBackend: Story = {
  args: { source: 'local', target: '/repos/image-trail', canPreflight: false },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('textbox'), '{Enter}');
    await expect(args.onPreflight).not.toHaveBeenCalled();
  },
};

// AC-0002: Enter in the guide must not submit Connect's surrounding form.
export const ManifestGuideDoesNotPreflight: Story = {
  args: { source: 'manifest', target: '/existing/manifest.toml', manifestGuide: { preview: fn().mockResolvedValue('repos = []'), save: fn().mockResolvedValue(null) } },
  play: async ({ canvasElement, args }) => {
    const c = within(canvasElement);
    await userEvent.click(c.getByRole('button', { name: 'Create new manifest' }));
    await userEvent.type(c.getByLabelText('Repository 1'), '../api{enter}');
    await expect(args.onPreflight).not.toHaveBeenCalled();
    await expect(c.getByRole('button', { name: 'Preflight' })).toBeDisabled();
    await userEvent.click(c.getByRole('button', { name: 'Close manifest guide' }));
    await expect(c.getByRole('button', { name: 'Preflight' })).toBeEnabled();
  },
};

const abandonedSave = fn<() => Promise<string | null>>();
const abandonedPreview = fn<() => Promise<string>>();
export const AbandonedManifestSave: Story = {
  args: { source: 'manifest', target: '/original/manifest.toml', manifestGuide: { preview: abandonedPreview, save: abandonedSave } },
  play: async ({ canvasElement, args }) => {
    let finish: (path: string) => void = () => { throw new Error('save not started'); };
    const pending = new Promise<string>(resolve => { finish = resolve; });
    abandonedPreview.mockResolvedValue('repos = []');
    abandonedSave.mockReturnValue(pending);
    const c = within(canvasElement);
    await userEvent.click(c.getByRole('button', { name: 'Create new manifest' }));
    await userEvent.click(c.getByRole('button', { name: 'Preview manifest' }));
    await c.findByLabelText('Manifest preview');
    await userEvent.click(c.getByRole('button', { name: 'Save new manifest' }));
    await expect(c.getByLabelText('Repository 1')).toBeDisabled();
    await userEvent.click(c.getByRole('button', { name: 'Close manifest guide' }));
    finish('/late/manifest.toml');
    await pending;
    await expect(args.onTargetChange).not.toHaveBeenCalled();
    await expect(args.onPreflight).not.toHaveBeenCalled();
  },
};
