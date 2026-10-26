import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fireEvent, fn, userEvent, waitFor, within } from 'storybook/test';
import { investigationCatalog, investigationDetail } from '../investigationFixtures';
import { useInvestigationStore } from '../investigationStore';
import { InvestigationQuickDispatch } from './InvestigationQuickDispatch';

let retriedNonce: string | undefined;
let finishCatalogLoad: (() => void) | undefined;
const pressMeta = (key: string, code: string) => fireEvent.keyDown(window, { key, code, metaKey: true, bubbles: true });

const meta = {
  title: 'InvestigationQuickDispatch',
  component: InvestigationQuickDispatch,
  args: { fallbackEnabled: true, onNavigate: fn() },
  beforeEach: () => {
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog,
      startError: null, pendingStart: null, starting: false, composer: { ...useInvestigationStore.getInitialState().composer,
        limitProfile: 'investigation-v2', parent: { conversation_id: 'must-not-copy', parent_id: 'must-not-copy' } },
      draftObligations: [{ obligation_id: 'must-not-copy' } as never], draftInspectionRequirements: [{ target_id: 'must-not-copy' } as never],
      start: fn(async () => 'inv-confirmed'), retryStart: fn(async () => 'inv-confirmed') }, true);
    mockIPC(() => null);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
} satisfies Meta<typeof InvestigationQuickDispatch>;
export default meta;
type Story = StoryObj<typeof meta>;

export const KeyboardAndDispatchLifecycle: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const launcher = document.createElement('button');
    launcher.textContent = 'launcher';
    canvasElement.append(launcher);
    launcher.focus();
    pressMeta('k', 'KeyK');
    pressMeta('1', 'Digit1');
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
    pressMeta(' ', 'Space');
    canvas.getByRole('dialog', { name: 'Quick investigation dispatch' });
    await expect(canvas.getByText(/Scope: Recovered system/)).toBeVisible();
    await expect(canvas.getByLabelText('Question')).toHaveFocus();
    await userEvent.type(canvas.getByLabelText('Question'), 'Explain the recovered behavior.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(useInvestigationStore.getState().start).toHaveBeenCalledWith({
      specialist_id: 'domain-analyst@3', question: 'Explain the recovered behavior.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1',
    });
    await expect(args.onNavigate).toHaveBeenCalledWith('investigations');
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
    await expect(launcher).toHaveFocus();
    launcher.remove();
  },
};

export const UnavailableRuntimeAndSameRequestRetry: Story = {
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    const retry = initial.retryStart;
    retriedNonce = undefined;
    mockIPC((command, payload) => {
      if (command === 'start_investigation') {
        retriedNonce = (payload as { request: { request_nonce: string } }).request.request_nonce;
        return investigationDetail('retry-confirmed', { has_result: false, status: 'queued', last_event_sequence: 0,
          actions: { can_cancel: true, can_follow_up: false } });
      }
      if (command === 'get_investigation') throw new Error('Saved detail temporarily unavailable');
      return null;
    });
    useInvestigationStore.setState({ ...initial, catalog: investigationCatalog, pendingStart: {
      schema_version: 1, request_nonce: 'a'.repeat(36), specialist_id: 'domain-analyst@3', question: 'Keep this exact question',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1',
    }, retryStart: fn(() => retry()), start: initial.start, loadCatalog: fn(async () => {}) }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    pressMeta('j', 'KeyJ');
    await expect(canvas.getByRole('dialog', { name: 'Quick investigation dispatch' })).toBeVisible();
    await expect(canvas.getByText(/outcome is uncertain/)).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Close quick investigation' })).toHaveFocus();
    await userEvent.click(canvas.getByRole('button', { name: 'Retry same request' }));
    await expect(useInvestigationStore.getState().retryStart).toHaveBeenCalledTimes(1);
    await expect(useInvestigationStore.getState().selectedId).toBe('retry-confirmed');
    await expect(useInvestigationStore.getState().pendingStart).toBeNull();
    await expect(retriedNonce).toBe('a'.repeat(36));
    await expect(args.onNavigate).toHaveBeenCalledWith('investigations');
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
    // The actual store reused the retained nonce when retrying admission.
    await expect(useInvestigationStore.getState().pendingStart).toBeNull();
    fireEvent.keyDown(window, { key: 'Escape', code: 'Escape', bubbles: true });
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
  },
};

export const DefinitiveRejectionPreservesQuestion: Story = {
  beforeEach: () => {
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog }, true);
    mockIPC(() => { throw { code: 'invalid_input', definitive: true, message: 'Fixture rejected before admission' }; });
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    const input = canvas.getByLabelText('Question');
    await userEvent.type(input, 'Keep my question after a definitive rejection.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent('No investigation was started');
    await expect(input).toHaveValue('Keep my question after a definitive rejection.');
    await expect(args.onNavigate).not.toHaveBeenCalled();
  },
};

export const DisabledFallbackDoesNotOpen: Story = {
  args: { fallbackEnabled: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta('j', 'KeyJ');
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
  },
};

export const CatalogFailureIsRequestedOnce: Story = {
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    const loadCatalog = fn(() => new Promise<void>((resolve) => {
      finishCatalogLoad = () => {
        useInvestigationStore.setState({ catalogLoading: false, catalogError: 'Catalog unavailable' });
        resolve();
      };
      useInvestigationStore.setState({ catalogLoading: true });
    }));
    useInvestigationStore.setState({ ...initial, catalog: null, catalogLoading: false, catalogError: null, loadCatalog }, true);
    return () => { finishCatalogLoad = undefined; useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    const close = canvas.getByRole('button', { name: 'Close quick investigation' });
    close.focus();
    const store = useInvestigationStore.getState();
    await expect(store.loadCatalog).toHaveBeenCalledTimes(1);
    // Let the pending catalog request fail after focus has moved inside the dialog.
    finishCatalogLoad?.();
    await waitFor(() => expect(canvas.getByRole('alert')).toHaveTextContent('Specialist catalog unavailable'));
    await expect(useInvestigationStore.getState().loadCatalog).toHaveBeenCalledTimes(1);
    await expect(close).toHaveFocus();
  },
};

export const UnavailableProviderDoesNotFallback: Story = {
  beforeEach: () => {
    const catalog = structuredClone(investigationCatalog);
    catalog.providers[0] = { ...catalog.providers[0], available: false, unavailable_reason: 'Fixture provider offline' };
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, catalog, start: fn(async () => null) }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await expect(canvas.getByText(/Choose an available provider or use the full composer/)).toBeVisible();
    await expect(canvas.getByLabelText('Provider')).toHaveValue('local');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeDisabled();
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalled();
  },
};
