import { useState } from 'react';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fireEvent, fn, userEvent, waitFor, within } from 'storybook/test';
import { investigationCatalog, investigationDetail, investigationEvents } from '../investigationFixtures';
import { completionCatalog, completionChoices, inventoryObligation, gapTriagerDetail, gapTriagerResult } from '../completionFixtures';
import { useInvestigationStore } from '../investigationStore';
import { investigationPolicy, obligationEnvelope } from '../investigationObligations';
import { InvestigationQuickDispatch } from './InvestigationQuickDispatch';

let retriedNonce: string | undefined;
let finishCatalogLoad: (() => void) | undefined;
let finishStart: (() => void) | undefined;
let completionRequests: Array<Record<string, unknown>> = [];
let pendingChoiceResolvers: Array<(value: typeof completionChoices) => void> = [];
let refreshedCompletionChoices: typeof completionChoices;
let completionChoiceReads = 0;
const pressMeta = (key: string, code: string) => fireEvent.keyDown(window, { key, code, metaKey: true, bubbles: true });

const meta = {
  title: 'InvestigationQuickDispatch',
  component: InvestigationQuickDispatch,
  args: { fallbackEnabled: true, openRequest: 0, onNavigate: fn(), onAcceptedTask: fn() },
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
    }, { preserveComposer: true, deferOpen: true });
    await expect(args.onAcceptedTask).toHaveBeenCalledWith('inv-confirmed');
    await expect(args.onNavigate).not.toHaveBeenCalled();
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
    await expect(args.onAcceptedTask).toHaveBeenCalledWith('retry-confirmed');
    await expect(args.onNavigate).not.toHaveBeenCalled();
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
    // The actual store reused the retained nonce when retrying admission.
    await expect(useInvestigationStore.getState().pendingStart).toBeNull();
    fireEvent.keyDown(window, { key: 'Escape', code: 'Escape', bubbles: true });
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
  },
};

export const UnrelatedPendingRequestKeepsInvestigationsRecovery: Story = {
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, catalog: investigationCatalog, pendingStart: {
      schema_version: 1, request_nonce: 'unrelated-pending-nonce', specialist_id: 'domain-analyst@2',
      question: 'Recover this unrelated request.', scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1',
    } }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await userEvent.click(canvas.getByRole('button', { name: 'Open Investigations' }));
    await expect(args.onNavigate).toHaveBeenCalledWith('investigations');
    await expect(args.onAcceptedTask).not.toHaveBeenCalled();
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
    await expect(args.onAcceptedTask).not.toHaveBeenCalled();
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

export const ExternalLaunchOpensWhenFallbackIsDisabled: Story = {
  args: { fallbackEnabled: false, openRequest: 1 },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('dialog', { name: 'Quick investigation dispatch' })).toBeVisible();
  },
};

export const CannotDismissWhileStartIsInFlightButCanCloseUnknownOutcome: Story = {
  beforeEach: () => {
    finishStart = undefined;
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, catalog: investigationCatalog }, true);
    mockIPC((command) => {
      if (command === 'start_investigation') return new Promise((resolve) => {
        finishStart = () => resolve(null);
      });
      return null;
    });
    return () => { finishStart = undefined; clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await userEvent.type(canvas.getByLabelText('Question'), 'Keep the start outcome recoverable.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    const dialog = canvas.getByRole('dialog', { name: 'Quick investigation dispatch' });
    const close = canvas.getByRole('button', { name: 'Close quick investigation' });
    await waitFor(() => expect(close).toBeDisabled());
    // Once the store records an uncertain request, the dialog switches to its
    // recovery view; Close stays disabled until the start call settles.
    await expect(canvas.queryByRole('button', { name: 'Cancel' })).not.toBeInTheDocument();
    fireEvent.keyDown(window, { key: 'Escape', code: 'Escape', bubbles: true });
    const backdrop = canvasElement.querySelector('.quick-dispatch-backdrop');
    if (!backdrop) throw new Error('Quick dispatch backdrop missing.');
    fireEvent.mouseDown(backdrop);
    await expect(dialog).toBeVisible();

    finishStart?.();
    await waitFor(() => expect(close).toBeEnabled());
    await expect(useInvestigationStore.getState().pendingStart?.question).toBe('Keep the start outcome recoverable.');
    await userEvent.click(close);
    await expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument();
    await expect(useInvestigationStore.getState().pendingStart?.question).toBe('Keep the start outcome recoverable.');
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

export const CompletionDispatchUsesOnlyLocalRecordedChoices: Story = {
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    const accepted = gapTriagerDetail('quick-completion-confirmed');
    completionRequests = [];
    mockIPC((command, payload) => {
      if (command === 'investigation_obligation_choices') return completionChoices;
      if (command === 'start_investigation') {
        completionRequests.push(structuredClone((payload as { request: Record<string, unknown> }).request));
        if (completionRequests.length === 1) throw new Error('Uncertain start acknowledgement');
        return accepted;
      }
      if (command === 'get_investigation') return accepted;
      if (command === 'investigation_result') return gapTriagerResult(accepted.investigation_id);
      if (command === 'get_investigation_chat' || command === 'investigation_consent') return null;
      if (command === 'investigation_events') { const items = investigationEvents(accepted.investigation_id);
        return { investigation_id: accepted.investigation_id, items, next_sequence: items.length, has_more: false }; }
      return null;
    });
    useInvestigationStore.setState({ ...initial, catalog: completionCatalog, pendingStart: null, starting: false,
      composer: { ...initial.composer, question: 'Preserve the full composer question.', parent: {
        conversation_id: 'full-composer-conversation', parent_id: 'full-composer-parent' } },
      draftObligations: [{ ...inventoryObligation, obligation_id: 'full-composer-obligation' }],
      start: initial.start, retryStart: initial.retryStart }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist'), 'gap-triager@2');
    await canvas.findByText('acme/api');
    await expect(canvas.queryByText('example/stale')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await expect(canvas.getByRole('button', { name: 'Remove inventory acme/api' })).toHaveAttribute('aria-pressed', 'true');
    await userEvent.type(canvas.getByLabelText('Question'), 'Draft a cited proposal.');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeEnabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(canvas.getByText(/outcome is uncertain/)).toBeVisible();
    const close = canvas.getByRole('button', { name: 'Close quick investigation' });
    await userEvent.click(close);
    pressMeta(' ', 'Space');
    await expect(canvas.getByRole('button', { name: 'Retry same request' })).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Retry same request' }));
    await waitFor(() => expect(useInvestigationStore.getState().selectedId).toBeNull());
    const state = useInvestigationStore.getState();
    expect(completionRequests).toHaveLength(2);
    expect(completionRequests[1]).toEqual(completionRequests[0]);
    expect(completionRequests[0]).toMatchObject({ schema_version: 4, specialist_id: 'gap-triager@2',
      obligations: { schema_version: 1, obligations: [inventoryObligation] },
      expected_graph_revision: completionChoices.graph_snapshot_id, expected_context_id: completionChoices.context_id,
      conversation_id: null, parent_id: null, question: 'Draft a cited proposal.' });
    expect(completionRequests[0]).not.toHaveProperty('preserveComposer');
    expect(completionRequests[0]).not.toHaveProperty('completionChoices');
    await expect(state.composer.question).toBe('Preserve the full composer question.');
    await expect(state.composer.parent).toEqual({ conversation_id: 'full-composer-conversation', parent_id: 'full-composer-parent' });
    await expect(state.draftObligations[0].obligation_id).toBe('full-composer-obligation');
    await expect(args.onAcceptedTask).toHaveBeenCalledWith('quick-completion-confirmed');
    await expect(args.onNavigate).not.toHaveBeenCalled();
  },
};

export const ParentBoundFollowUpUsesFreshChoices: Story = {
  args: { openRequest: 1, followUpContext: { parent_id: 'saved-parent', conversation_id: 'saved-conversation',
    specialist_id: 'gap-triager@1', scope: { type: 'all' }, provider_mode: 'local',
    saved_provider_id: 'ollama', saved_model: 'fixture-parent-model' } },
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    const accepted = { ...gapTriagerDetail('follow-up-child'), parent_id: 'saved-parent', conversation_id: 'saved-conversation' };
    completionRequests = [];
    completionChoiceReads = 0;
    mockIPC((command, payload) => {
      if (command === 'investigation_obligation_choices') { completionChoiceReads += 1; return structuredClone(completionChoices); }
      if (command === 'start_investigation') {
        completionRequests.push(structuredClone((payload as { request: Record<string, unknown> }).request));
        if (completionRequests.length === 1) throw { code: 'refresh_required', definitive: true };
        if (completionRequests.length === 2) throw new Error('Fixture start acknowledgement was lost');
        return accepted;
      }
      return null;
    });
    useInvestigationStore.setState({ ...initial, catalog: completionCatalog, pendingStart: null, starting: false,
      composer: { ...initial.composer, question: 'Preserve the full composer question.',
        parent: { conversation_id: 'must-not-copy', parent_id: 'must-not-copy' } },
      draftObligations: [{ obligation_id: 'must-not-copy' } as never],
      draftInspectionRequirements: [{ target_id: 'must-not-copy' } as never] }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const dialog = await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await expect(dialog).toHaveTextContent('Following saved-parent in conversation saved-conversation');
    await expect(canvas.getByLabelText('Specialist')).toHaveValue('gap-triager@1');
    await expect(canvas.getByLabelText('Specialist')).toBeDisabled();
    await expect(canvas.getByText(/Scope: Recovered system · captured from the saved parent/)).toBeVisible();
    await expect(dialog).toHaveTextContent(/Saved producer: gap-triager@1 · ollama · fixture-parent-model/);
    await expect(dialog).toHaveTextContent(/Current configured provider for local: ollama · fixture-local-model/);
    await canvas.findByText('acme/api');
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Check the parent finding against the recorded inventory.');
    await expect(canvas.getByRole('button', { name: 'Start follow-up task' })).toBeEnabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up task' }));
    await waitFor(() => expect(completionRequests).toHaveLength(1));
    await expect(canvas.getByRole('alert')).toHaveTextContent(/completion context changed/);
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Require inventory acme/api' })).toBeEnabled());
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up task' }));
    await waitFor(() => expect(completionRequests).toHaveLength(2));
    await expect(canvas.getByText(/outcome could not be confirmed/)).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Retry same request' })).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Retry same request' }));
    await waitFor(() => expect(completionRequests).toHaveLength(3));
    expect(completionChoiceReads).toBe(2);
    expect(completionRequests[0]).toMatchObject({ specialist_id: 'gap-triager@1', provider_mode: 'local',
      conversation_id: 'saved-conversation', parent_id: 'saved-parent', scope: { type: 'all' },
      obligations: { obligations: [{ kind: 'inventory', obligation_id: inventoryObligation.obligation_id }] } });
    expect(completionRequests[1]).toMatchObject({ conversation_id: 'saved-conversation', parent_id: 'saved-parent' });
    expect(completionRequests[2]).toEqual(completionRequests[1]);
    expect(completionRequests[0]).not.toHaveProperty('inspection_requirements');
    expect(useInvestigationStore.getState().composer.parent).toEqual({ conversation_id: 'must-not-copy', parent_id: 'must-not-copy' });
    await expect(args.onAcceptedTask).toHaveBeenCalledWith('follow-up-child');
  },
};

export const ParentFollowUpDoesNotClaimAnUnrelatedPendingRequest: Story = {
  args: { openRequest: 1, followUpContext: { parent_id: 'saved-parent', conversation_id: 'saved-conversation',
    specialist_id: 'domain-analyst@3', scope: { type: 'all' }, provider_mode: 'local',
    saved_provider_id: 'ollama', saved_model: 'fixture-parent-model' } },
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    useInvestigationStore.setState({ ...initial, catalog: investigationCatalog, pendingStart: {
      schema_version: 1, request_nonce: 'b'.repeat(36), specialist_id: 'domain-analyst@3', question: 'Unrelated pending request',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1',
      conversation_id: 'another-conversation', parent_id: 'another-parent' } }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' })).toHaveTextContent('A different investigation request is pending');
    await expect(canvas.queryByRole('button', { name: 'Retry same request' })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Open Investigations' }));
    await expect(args.onNavigate).toHaveBeenCalledWith('investigations');
  },
};

export const SavedParentWithUnsupportedRoleDoesNotFallback: Story = {
  args: { openRequest: 1, followUpContext: { parent_id: 'saved-parent', conversation_id: 'saved-conversation',
    specialist_id: 'domain-analyst@2', scope: { type: 'all' }, provider_mode: 'local',
    saved_provider_id: 'ollama', saved_model: 'fixture-local-model' } },
  beforeEach: () => {
    const start = fn(async () => null);
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: investigationCatalog, start }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const dialog = await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await expect(dialog).toHaveTextContent('not supported for Quick Dispatch');
    await expect(canvas.getByLabelText('Specialist')).toHaveValue('domain-analyst@2');
    await expect(canvas.getByLabelText('Provider')).toHaveValue('local');
    await expect(canvas.getByRole('button', { name: 'Start follow-up task' })).toBeDisabled();
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalled();
  },
};

export const UnavailableSavedParentProviderDoesNotFallback: Story = {
  args: { openRequest: 1, followUpContext: { parent_id: 'saved-parent', conversation_id: 'saved-conversation',
    specialist_id: 'gap-triager@1', scope: { type: 'all' }, provider_mode: 'local',
    saved_provider_id: 'ollama', saved_model: 'retired-model' } },
  beforeEach: () => {
    const catalog = structuredClone(completionCatalog);
    catalog.providers[0] = { ...catalog.providers[0], available: false, unavailable_reason: 'Fixture provider retired' };
    const start = fn(async () => null);
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog, start }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const dialog = await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await expect(dialog).toHaveTextContent('Fixture provider retired');
    await expect(canvas.getByLabelText('Specialist')).toHaveValue('gap-triager@1');
    await expect(canvas.getByLabelText('Provider')).toHaveValue('local');
    await expect(canvas.getByRole('button', { name: 'Start follow-up task' })).toBeDisabled();
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalled();
  },
};

export const CapturedParentIdsCountTowardRequestLimit: Story = {
  args: { openRequest: 1, followUpContext: { parent_id: 'parent-1234567890abcdef', conversation_id: 'conversation-1234567890abcdef',
    specialist_id: 'gap-triager@1', scope: { type: 'all' }, provider_mode: 'local',
    saved_provider_id: 'ollama', saved_model: 'fixture-parent-model' } },
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    const policy = investigationPolicy(completionCatalog, 'investigation-v1', 'gap-triager@1');
    if (!policy) throw new Error('Fixture completion policy is unavailable');
    const parent = { parent_id: 'parent-1234567890abcdef', conversation_id: 'conversation-1234567890abcdef' };
    const request = (withParent: boolean) => ({ schema_version: policy.request_schema_version, request_nonce: '0'.repeat(36),
      specialist_id: 'gap-triager@1', obligations: obligationEnvelope([inventoryObligation], policy),
      question: 'Check the saved parent.', scope: { type: 'all' as const }, provider_mode: 'local', limit_profile: 'investigation-v1',
      expected_graph_revision: completionChoices.graph_snapshot_id, expected_context_id: completionChoices.context_id,
      conversation_id: withParent ? parent.conversation_id : null, parent_id: withParent ? parent.parent_id : null });
    const parentBytes = new TextEncoder().encode(JSON.stringify(request(true))).length;
    const withoutParentBytes = new TextEncoder().encode(JSON.stringify(request(false))).length;
    const requestBytes = parentBytes - 1;
    if (withoutParentBytes > requestBytes) throw new Error('Fixture parent IDs do not isolate the admission boundary');
    const catalog = structuredClone(completionCatalog);
    catalog.policies = [{ limits: completionCatalog.limits, ...policy, request_bytes: requestBytes,
      specialist_ids: ['gap-triager@1'] }];
    mockIPC((command) => command === 'investigation_obligation_choices' ? structuredClone(completionChoices) : null);
    const start = fn(async () => null);
    useInvestigationStore.setState({ ...initial, catalog, pendingStart: null, starting: false, start }, true);
    return () => useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const dialog = await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await canvas.findByText('acme/api');
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Check the saved parent.');
    await expect(canvas.getByRole('alert')).toHaveTextContent('complete request exceeds the existing');
    await expect(canvas.getByRole('button', { name: 'Start follow-up task' })).toBeDisabled();
    await expect(dialog).toHaveTextContent('Following');
    await expect(useInvestigationStore.getState().start).not.toHaveBeenCalled();
  },
};

export const CompletionChoicesDiscardOlderScopeResponse: Story = {
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    pendingChoiceResolvers = [];
    mockIPC((command) => {
      if (command === 'investigation_obligation_choices') return new Promise<typeof completionChoices>((resolve) => pendingChoiceResolvers.push(resolve));
      return null;
    });
    useInvestigationStore.setState({ ...initial, catalog: completionCatalog }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    // The default role's current analyst is a completion role too, so opening reads choices once.
    await waitFor(() => expect(pendingChoiceResolvers).toHaveLength(1));
    await userEvent.selectOptions(canvas.getByLabelText('Specialist'), 'spec-writer@2');
    await waitFor(() => expect(pendingChoiceResolvers).toHaveLength(2));
    useInvestigationStore.getState().updateComposer({ scopeType: 'neighborhood', anchor: 'rule:new-scope', hops: 1 });
    await waitFor(() => expect(pendingChoiceResolvers).toHaveLength(3));
    const newer = structuredClone(completionChoices);
    newer.context_id = `context-v2:${'9'.repeat(64)}`;
    newer.inventories[0].repo = 'acme/new-scope';
    newer.inventories[0].obligation!.target.context_id = newer.context_id;
    newer.inventories[0].obligation!.target.repo = 'acme/new-scope';
    newer.inventories[0].obligation!.target.inventory_hash = '8'.repeat(64);
    pendingChoiceResolvers[2](newer);
    await canvas.findByText('acme/new-scope');
    pendingChoiceResolvers[1](completionChoices);
    pendingChoiceResolvers[0](completionChoices);
    await Promise.resolve();
    await waitFor(() => expect(canvas.getByText('acme/new-scope')).toBeVisible());
    await expect(canvas.queryByText('acme/api')).not.toBeInTheDocument();
    await expect(canvas.getByText(/Scope: Fact neighborhood · rule:new-scope · 1 hop/)).toBeVisible();
  },
};

export const DefinitiveContextRefreshRequiresNewSelection: Story = {
  beforeEach: () => {
    const initial = useInvestigationStore.getInitialState();
    const accepted = gapTriagerDetail('quick-refreshed-completion');
    refreshedCompletionChoices = structuredClone(completionChoices);
    refreshedCompletionChoices.context_id = `context-v2:${'9'.repeat(64)}`;
    refreshedCompletionChoices.inventories[0].repo = 'acme/refreshed';
    refreshedCompletionChoices.inventories[0].obligation = { kind: 'inventory', obligation_id: 'f'.repeat(64), target: {
      context_id: refreshedCompletionChoices.context_id, repo: 'acme/refreshed', inventory_hash: '8'.repeat(64) } };
    completionRequests = [];
    mockIPC((command, payload) => {
      if (command === 'investigation_obligation_choices') return completionRequests.length === 0 ? completionChoices : refreshedCompletionChoices;
      if (command === 'start_investigation') {
        completionRequests.push(structuredClone((payload as { request: Record<string, unknown> }).request));
        if (completionRequests.length === 1) throw { code: 'refresh_required', definitive: true };
        return accepted;
      }
      if (command === 'get_investigation') return accepted;
      if (command === 'investigation_result') return gapTriagerResult(accepted.investigation_id);
      if (command === 'get_investigation_chat' || command === 'investigation_consent') return null;
      if (command === 'investigation_events') { const items = investigationEvents(accepted.investigation_id);
        return { investigation_id: accepted.investigation_id, items, next_sequence: items.length, has_more: false }; }
      return null;
    });
    useInvestigationStore.setState({ ...initial, catalog: completionCatalog }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist'), 'gap-triager@2');
    await canvas.findByText('acme/api');
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Recommend the next verification.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(await canvas.findByRole('alert')).toHaveTextContent('Refresh choices');
    await expect(useInvestigationStore.getState().startRejection).toBe('refresh_required');
    await canvas.findByText('acme/refreshed');
    await expect(canvas.getByRole('button', { name: 'Require inventory acme/refreshed' })).toHaveAttribute('aria-pressed', 'false');
    await expect(canvas.queryByRole('button', { name: 'Require inventory acme/api' })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/refreshed' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await waitFor(() => expect(completionRequests).toHaveLength(2));
    expect(completionRequests[0]).toMatchObject({ expected_context_id: completionChoices.context_id,
      obligations: { obligations: [inventoryObligation] } });
    expect(completionRequests[1]).toMatchObject({ expected_context_id: refreshedCompletionChoices.context_id,
      obligations: { obligations: [refreshedCompletionChoices.inventories[0].obligation] } });
  },
};

export const DefinitiveInvalidInputKeepsSelection: Story = {
  // AC-0385: only a stale-context refusal invalidates the explicit inventory selection.
  beforeEach: () => {
    completionChoiceReads = 0;
    mockIPC((command) => {
      if (command === 'investigation_obligation_choices') { completionChoiceReads += 1; return completionChoices; }
      if (command === 'start_investigation') throw { code: 'invalid_input', definitive: true, message: 'Question is not acceptable.' };
      return null;
    });
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: completionCatalog,
      startRejection: 'refresh_required' }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist'), 'gap-triager@2');
    await canvas.findByText('acme/api');
    const readsBeforeStart = completionChoiceReads;
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.type(canvas.getByLabelText('Question'), 'Keep my explicit selection.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(await canvas.findByRole('alert')).toHaveTextContent('rejected before admission');
    await expect(canvas.getByRole('button', { name: 'Remove inventory acme/api' })).toHaveAttribute('aria-pressed', 'true');
    await expect(canvas.getByLabelText('Question')).toHaveValue('Keep my explicit selection.');
    expect(useInvestigationStore.getState().startRejection).toBe('invalid_input');
    expect(useInvestigationStore.getState().pendingStart).toBeNull();
    // The stale reason seeded above is cleared by this attempt; generic input
    // refusal does not fetch a replacement context behind the user's back.
    expect(completionChoiceReads).toBe(readsBeforeStart);
  },
};

export const CompletionSelectionStopsAtPolicyCount: Story = {
  // AC-0385: the advertised item cap blocks additions while preserving removal.
  beforeEach: () => {
    const cappedChoices = structuredClone(completionChoices);
    cappedChoices.inventories = Array.from({ length: 13 }, (_, index) => {
      const repo = `acme/cap-${String(index + 1).padStart(2, '0')}`;
      const hex = (index + 1).toString(16).padStart(64, '0');
      return { repo, status: 'recorded' as const, obligation: { kind: 'inventory' as const,
        obligation_id: hex, target: { context_id: cappedChoices.context_id, repo, inventory_hash: hex } } };
    });
    const policy = { limits: { ...completionCatalog.limits, profile: 'investigation-v1' as const },
      request_schema_version: 4 as const, envelope_schema_version: 1 as const,
      envelope_contract: 'investigation-completion-obligations@1' as const,
      max_completion_obligations: 12, request_bytes: 8192, envelope_bytes: 8192,
      specialist_ids: ['gap-triager@2' as const] };
    mockIPC((command) => command === 'investigation_obligation_choices' ? cappedChoices : null);
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: {
      ...completionCatalog, policies: [policy],
    } }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await userEvent.selectOptions(canvas.getByLabelText('Specialist'), 'gap-triager@2');
    await canvas.findByText('acme/cap-01');
    await userEvent.type(canvas.getByLabelText('Question'), 'Use exactly the advertised limit.');
    for (let index = 1; index <= 12; index += 1) {
      await userEvent.click(canvas.getByRole('button', { name: `Require inventory acme/cap-${String(index).padStart(2, '0')}` }));
    }
    await expect(canvas.getByText('Selected 12 / 12 inventory obligations.')).toBeVisible();
    const cappedChoice = canvas.getByRole('button', { name: 'Require inventory acme/cap-13' });
    await expect(cappedChoice).toBeDisabled();
    const remove = canvas.getByRole('button', { name: 'Remove inventory acme/cap-01' });
    await expect(remove).toBeEnabled();
    await userEvent.click(remove);
    await expect(canvas.getByRole('button', { name: 'Require inventory acme/cap-13' })).toBeEnabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Require inventory acme/cap-13' }));
    await expect(canvas.getByText('Selected 12 / 12 inventory obligations.')).toBeVisible();
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeEnabled();
  },
};

export const CompletionRolesRequireHostCatalogSupport: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    await expect(canvas.queryByRole('option', { name: /spec-writer@1/ })).not.toBeInTheDocument();
    await expect(canvas.queryByRole('option', { name: /gap-triager@1/ })).not.toBeInTheDocument();
  },
};

// AC-0398: one instance moves portal placement, preserves real completion retry and rejects detached hosts.
export const InlineReplyPlacementAndCancellation: Story = {
  ...ParentBoundFollowUpUsesFreshChoices,
  render: function InlinePlacement(args) {
    const [a, setA] = useState<HTMLDivElement | null>(null);
    const [b, setB] = useState<HTMLDivElement | null>(null);
    const [request, setRequest] = useState(1);
    const [second, setSecond] = useState(false);
    const parent = { ...args.followUpContext!, ...(second ? { parent_id: 'second-parent', conversation_id: 'second-conversation' } : {}) };
    return <><div ref={setA} data-testid="inline-a" /><div ref={setB} data-testid="inline-b" />
      <button type="button" onClick={() => { setSecond(true); setRequest((value) => value + 1); }}>Open second pane</button>
      <button type="button" onClick={() => { setSecond(false); setRequest((value) => value + 1); }}>Open first pane</button>
      {a && b && <InvestigationQuickDispatch {...args} followUpContext={parent} openRequest={request}
        inlineReply={{ taskId: parent.parent_id, host: second ? b : a }} />}</>;
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const hostA = canvas.getByTestId('inline-a'); const hostB = canvas.getByTestId('inline-b');
    let reply = within(await within(hostA).findByRole('region', { name: 'Reply to saved-parent' }));
    await expect(canvas.queryByRole('dialog')).not.toBeInTheDocument();
    await expect(reply.getByLabelText('Question')).toHaveFocus();
    await expect(reply.getByText(/not all earlier questions or answers/)).toBeVisible();
    await reply.findByText('acme/api');
    await expect(completionChoiceReads).toBe(1);
    await userEvent.click(reply.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Open second pane' }));
    await expect(await within(hostB).findByLabelText('Question')).toHaveFocus();
    await within(hostB).findByText('acme/api');
    await expect(completionChoiceReads).toBe(2);
    await expect(within(hostB).getByRole('button', { name: 'Require inventory acme/api' })).toHaveAttribute('aria-pressed', 'false');
    await userEvent.click(canvas.getByRole('button', { name: 'Open first pane' }));
    reply = within(await within(hostA).findByRole('region', { name: 'Reply to saved-parent' }));
    await expect(reply.getByLabelText('Question')).toHaveFocus();
    await reply.findByText('acme/api');
    await expect(completionChoiceReads).toBe(3);
    await userEvent.click(reply.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.type(reply.getByLabelText('Question'), 'A real completion reply.');
    await userEvent.click(reply.getByRole('button', { name: 'Send reply task' }));
    await waitFor(() => expect(completionRequests).toHaveLength(1));
    await waitFor(() => expect(reply.getByRole('button', { name: 'Require inventory acme/api' })).toBeEnabled());
    await userEvent.click(reply.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.click(reply.getByRole('button', { name: 'Send reply task' }));
    await waitFor(() => expect(completionRequests).toHaveLength(2));
    const pending = structuredClone(useInvestigationStore.getState().pendingStart);
    const retry = reply.getByRole('button', { name: 'Retry same request' });
    await expect(retry).toBeEnabled();
    const container = hostA.parentElement!;
    hostA.remove(); fireEvent.click(retry);
    await expect(completionRequests).toHaveLength(2);
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
    container.prepend(hostA);
    await userEvent.click(retry);
    await waitFor(() => expect(completionRequests).toHaveLength(3));
    expect(completionRequests[2]).toEqual(completionRequests[1]);
    await expect(args.onAcceptedTask).toHaveBeenCalledWith('follow-up-child');
    await userEvent.click(canvas.getByRole('button', { name: 'Open second pane' }));
    const second = within(await within(hostB).findByRole('region', { name: 'Reply to second-parent' }));
    await expect(second.getByLabelText('Question')).toHaveFocus();
    await second.findByText('acme/api');
    await userEvent.click(second.getByRole('button', { name: 'Require inventory acme/api' }));
    await userEvent.type(second.getByLabelText('Question'), 'Do not submit from a detached host.');
    const send = second.getByRole('button', { name: 'Send reply task' });
    await expect(send).toBeEnabled();
    hostB.remove(); fireEvent.click(send);
    await expect(completionRequests).toHaveLength(3); container.append(hostB);
    pressMeta(' ', 'Space');
    await expect(await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' })).toBeVisible();
    await expect(canvas.getAllByRole('dialog')).toHaveLength(1);
    await expect(canvas.queryByRole('region', { name: 'Reply to second-parent' })).not.toBeInTheDocument();
  },
};

// AC-1402 (ADR-0071): quick dispatch offers each role once and runs its current version.
export const QuickDispatchOffersRolesNotVersions: Story = {
  beforeEach: () => {
    mockIPC((command) => command === 'investigation_obligation_choices' ? completionChoices : null);
    useInvestigationStore.setState({ ...useInvestigationStore.getInitialState(), catalog: completionCatalog }, true);
    return () => { clearMocks(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); };
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    pressMeta(' ', 'Space');
    const select = canvas.getByLabelText('Specialist');
    const labels = within(select).getAllByRole('option').map((option) => option.textContent);
    await expect(labels).toEqual([...new Set(labels)]);
    await expect(labels).toContain('Spec writer');
    await expect(within(select).queryByRole('option', { name: /@\d/ })).not.toBeInTheDocument();
    await userEvent.selectOptions(select, 'Spec writer');
    await expect(select).toHaveValue('spec-writer@2');
  },
};
