import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { CompletionAssessment } from './CompletionAssessment';
import { completionDetail } from '../completionFixtures';

const meta = { title: 'Investigations/CompletionAssessment', component: CompletionAssessment,
  args: { completion: completionDetail().completion!, outcome: 'supported' } } satisfies Meta<typeof CompletionAssessment>;
export default meta;
type Story = StoryObj<typeof meta>;
export const SupportedStillRequiresReview: Story = {
  // AC-0266: supported completion records exposure, never stronger fact confidence.
  play: async ({ canvasElement }) => { const canvas = within(canvasElement);
    await expect(canvas.getByText('Supported completion')).toBeVisible();
    await expect(canvas.getByText(/Findings remain T3/)).toBeVisible(); },
};
export const InsufficientAfterUnavailableAttempt: Story = {
  args: { outcome: 'insufficient_evidence', completion: { ...completionDetail().completion!, assessment:
    completionDetail().completion!.assessment.map((row) => ({ ...row, acquired: false, exactly_supplied: false, last_attempt_outcome: 'unavailable' })) } },
  // AC-0266: an admitted empty answer is distinct from acquired evidence.
  play: async ({ canvasElement }) => { const canvas = within(canvasElement);
    await expect(canvas.getByText('Completed with insufficient evidence')).toBeVisible();
    await expect(canvas.getAllByText('Last completed attempt: Evidence unavailable.')).toHaveLength(2);
    await expect(canvas.getAllByText(/Acquired: No/)).toHaveLength(2); },
};
export const AcquiredButOmittedIsSeparate: Story = {
  args: { outcome: 'insufficient_evidence', completion: { ...completionDetail().completion!, assessment:
    completionDetail().completion!.assessment.map((row) => ({ ...row, exactly_supplied: false, unmet: null })) } },
  // AC-0266: acquisition alone is never described as exact finishing exposure.
  play: async ({ canvasElement }) => { const canvas = within(canvasElement);
    await expect(canvas.getAllByText('Acquired earlier, not exactly supplied at finish.')).toHaveLength(2); },
};
export const HostExhaustionHasNoModelResult: Story = {
  args: { outcome: 'obligations_unmet', completion: { ...completionDetail().completion!, refusal_count: 2,
    assessment: completionDetail().completion!.assessment.map((row) => ({ ...row, attempted: false, acquired: false, exactly_supplied: false, unmet: 'not_attempted', last_attempt_outcome: undefined })),
    assessment_basis: { kind: 'terminal_observation', finishing_attempts_hash: 'e'.repeat(64), last_refusal_id: 'f'.repeat(64),
      last_refusal_basis: { step_id: 'finish:2', input_ledger_hash: 'c'.repeat(64), disclosure_hash: `investigation-disclosure-v4:${'d'.repeat(64)}`, finish_hash: 'f'.repeat(64) } } } },
  // AC-0266: host exhaustion is terminal observation, not a model finish/result.
  play: async ({ canvasElement }) => { const canvas = within(canvasElement);
    await expect(canvas.getByText('Completion obligations unmet — no result was admitted')).toBeVisible();
    await expect(canvas.getByText(/Durable terminal observations/)).toBeVisible(); },
};

export const FirstRefusalAwaitsNextStep: Story = {
  args: { outcome: undefined, completion: { ...completionDetail().completion!, refusal_count: 1,
    assessment: completionDetail().completion!.assessment.map((row) => ({ ...row, exactly_supplied: false, unmet: 'acquired_not_supplied' })) } },
  // AC-0266: a recoverable refusal retains the actual prior finish assessment without a result.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No completion outcome recorded.')).toBeVisible();
    await expect(canvas.getByText('Finish refusals: 1 / 2.')).toBeVisible();
    await expect(canvas.getByText('Assessed against the actual finishing disclosure.')).toBeVisible();
  },
};
