import type { InvestigationChat } from '../investigationTypes';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { InvestigationChatTranscript } from './InvestigationChatTranscript';

const saved: InvestigationChat = {
  chat_id: 'chat:fixture', task_id: 'inv-fixture',
  producer: { specialist_id: 'domain-analyst@3', specialist_version: 3, prompt_fingerprint: 'a'.repeat(64),
    tier: 'Agentic', confidence_tier: 'InferredWeak', provider_mode: 'local', provider_id: 'ollama',
    model: 'fixture-model', protocol_version: 'bounded-json-actions-v2' },
  messages: [{ id: 'message:user', ordinal: 0, author: 'user',
    message: { kind: 'user_question', question: 'What stock behavior is supported, and what remains unknown?' } },
  { id: 'message:result', ordinal: 1, author: 'specialist',
    message: { kind: 'result_reference', task_id: 'inv-fixture', result_id: 'result:inv-fixture', result_hash: 'b'.repeat(64) } }],
};

const savedQuestion = saved.messages[0].message.kind === 'user_question' ? saved.messages[0].message.question : '';

const meta = { title: 'Surfaces/InvestigationChatTranscript', component: InvestigationChatTranscript,
  args: { chat: saved, status: 'saved', error: null, onRefresh: fn() } } satisfies Meta<typeof InvestigationChatTranscript>;
export default meta;
type Story = StoryObj<typeof meta>;

export const SavedQuestionAndResultReference: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const producer = canvas.getByText(/Saved producer:/);
    await expect(producer).toHaveTextContent(/domain-analyst@3 · version 3 · local · ollama\/fixture-model/);
    const userMessage = within(canvas.getByRole('listitem', { name: 'User question' }));
    await expect(userMessage.getByText('You')).toBeVisible();
    await expect(userMessage.getByText(savedQuestion)).toBeVisible();
    const resultMessage = within(canvas.getByRole('listitem', { name: 'Specialist result reference' }));
    await expect(resultMessage.getByText('result:inv-fixture')).toBeVisible();
    await expect(resultMessage.getByRole('link', { name: 'View admitted findings and citations' })).toHaveAttribute('href', '#investigation-findings');
  },
};

export const PendingQuestionHasNoFabricatedResult: Story = {
  args: { chat: { ...saved, messages: [saved.messages[0]] } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('listitem', { name: 'User question' })).toBeVisible();
    await expect(canvas.queryByRole('listitem', { name: 'Specialist result reference' })).not.toBeInTheDocument();
    await expect(canvas.queryByRole('link', { name: 'View admitted findings and citations' })).not.toBeInTheDocument();
  },
};

export const LegacyAbsenceIsExplicit: Story = {
  args: { chat: null, status: 'legacy' },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText(/No saved conversation is available for this older investigation/)).toBeVisible();
  },
};

export const BrowserPreviewIsNotLegacyAbsence: Story = {
  args: { chat: null, status: 'preview' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/available when Cartograph is running in the desktop app/)).toBeVisible();
    await expect(canvas.queryByText(/older investigation/)).not.toBeInTheDocument();
  },
};

export const TranscriptReadErrorCanRefresh: Story = {
  args: { chat: null, status: 'error', error: 'The saved transcript did not match this task and result. Refresh to reconnect.' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent(/did not match this task and result/);
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh conversation' }));
    await expect(args.onRefresh).toHaveBeenCalledTimes(1);
  },
};
