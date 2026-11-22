import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { CapturedSource } from './CapturedSource';
import type { CapturedDescription, ObservedDescription } from '../primarySourceStore';

const captureDescription: CapturedDescription = {
  graph_snapshot_id: 'snapshot:fixture',
  fact: { kind: 'node', id: 'rule:local/src_example@rules.ts#exit' },
  receipt_id: 'ts-primary-v1:fixture', emitted_fact_digest: 'fact-digest',
  source_id: 'src_example', repo_key: 'local/src_example',
  scope: 'primary_source_only', input_closure: 'input_closure_not_established',
  ranges: [{ index: 0, path: 'rules.ts', byte_start: 30, byte_end: 43 }],
};

const meta = {
  title: 'Atlas/CapturedSource', component: CapturedSource,
  args: { description: captureDescription, text: null, loading: false, error: null, onRead: fn() },
} satisfies Meta<typeof CapturedSource>;
export default meta;
type Story = StoryObj<typeof meta>;

export const SelectsStoredRange: Story = {
  // AC-0154: a stored range index and receipt bind inspection, not caller offsets.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Full input coverage and business interpretation are not established/)).toBeVisible();
    await userEvent.selectOptions(canvas.getByLabelText('Captured cited range'), '0');
    await expect(args.onRead).toHaveBeenCalledWith(0);
  },
};

export const OriginalSource: Story = {
  args: { text: { receipt_id: captureDescription.receipt_id, range_index: 0, text: 'return false;', path: 'rules.ts', byte_start: 30, byte_end: 43 } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByTestId('captured-source-text')).toHaveTextContent('return false;');
  },
};

export const DifferentReceiptNeverRenders: Story = {
  // AC-0154: even a stale parent payload cannot display another receipt's text.
  args: { text: { receipt_id: 'another-receipt', range_index: 0, text: 'WRONG SOURCE', path: 'rules.ts', byte_start: 30, byte_end: 43 } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).queryByText('WRONG SOURCE')).not.toBeInTheDocument();
  },
};

export const ForgottenSource: Story = {
  args: { error: 'Captured source is unavailable. Receipt and review history is preserved.' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('Captured source is unavailable');
    await expect(canvas.queryByTestId('captured-source-text')).not.toBeInTheDocument();
  },
};

export const RequirementNeedsExplicitAction: Story = {
  args: { onRequire: fn() },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const require = canvas.getByRole('button', { name: 'Require this source in an investigation' });
    await expect(require).toBeDisabled();
    await userEvent.selectOptions(canvas.getByLabelText('Captured cited range'), '0');
    await expect(args.onRead).toHaveBeenCalledWith(0);
    await expect(args.onRequire).not.toHaveBeenCalled();
    await userEvent.click(require);
    await expect(args.onRequire).toHaveBeenCalledWith(0);
  },
};

const observedDescription: ObservedDescription = {
  fact: { kind: 'node', id: 'res:local/src_example@aws_sqs_queue.jobs' },
  receipt_id: 'obs-input-v1:fixture', emitted_fact_digest: 'fact-digest',
  source_id: 'input/src_example', registered_source_id: 'src_example', repo_key: 'local/src_example',
  scope: 'observed_input_only', input_closure: 'input_closure_not_established',
  ranges: [{ index: 0, path: 'state.json', byte_start: 64, byte_end: 84 }],
};

export const ObservedInWithoutDeclaration: Story = {
  // AC-1049 (ADR-0079): a fact only state observed cites that input, labelled
  // as an observation, and still says it has no declaring source.
  args: { description: null, observed: observedDescription, onReadObserved: fn(),
    observedText: { receipt_id: observedDescription.receipt_id, range_index: 0, text: '"aws_sqs_queue.jobs"', path: 'state.json', byte_start: 64, byte_end: 84 } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/No retained primary-source binding is available/)).toBeVisible();
    await expect(canvas.getByRole('heading', { name: 'Observed in' })).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: /state\.json · bytes 64–84/ }));
    await expect(args.onReadObserved).toHaveBeenCalledWith(0);
    await expect(canvas.getByTestId('observed-source-text')).toHaveTextContent('"aws_sqs_queue.jobs"');
  },
};

export const ObservedTextOfAnotherReceiptNeverRenders: Story = {
  args: { observed: observedDescription,
    observedText: { receipt_id: 'obs-input-v1:other', range_index: 0, text: 'WRONG OBSERVATION', path: 'state.json', byte_start: 64, byte_end: 84 } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { name: 'Observed in' })).toBeVisible();
    await expect(canvas.queryByText('WRONG OBSERVATION')).not.toBeInTheDocument();
  },
};
