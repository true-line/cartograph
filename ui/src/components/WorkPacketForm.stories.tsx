import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { WorkPacketForm } from './WorkPacketForm';
import { useWorkPacketStore } from '../workPacketStore';
import type { GeneratePacketRequest } from '../workPacketTypes';

const meta = { title: 'Work packets/Requested intent', component: WorkPacketForm, args: {
  state: { ...useWorkPacketStore.getInitialState(), snapshotId: 'context-v1:host-observation',
    facts: [{ reference: { kind: 'node', id: 'rule:stock' }, expected_digest: 'host-fact-digest' }], generate: fn(async () => {}) },
  repositories: ['local/shop'],
} } satisfies Meta<typeof WorkPacketForm>;
export default meta;
type Story = StoryObj<typeof meta>;

export const ExplicitUnknowns: Story = {
  // AC-0249: an incomplete requested draft preserves every omission; actor/journey are not synthesized.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Generate and open saved packet' }));
    const request = (args.state.generate as ReturnType<typeof fn>).mock.calls[0][0] as GeneratePacketRequest;
    await expect(request.intent.objective).toMatchObject({ state: 'missing', omission: { class: 'missing_input', seam: 'interpretation' } });
    await expect(request.acceptance.positive_cases).toMatchObject({ state: 'missing' });
    await expect(request.acceptance.execution_evidence).toEqual([]);
    await expect(request.intent).not.toHaveProperty('requested_actor');
    await expect(request.intent).not.toHaveProperty('requested_journey');
    await expect(request.handoff.authorization_references).toMatchObject({ state: 'missing' });
  },
};

export const TypedComponentsCasesAndBudgets: Story = {
  // AC-0248/0249: editors create the strict protocol shapes without a raw JSON form.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('checkbox', { name: 'Unknown / not supplied: objective' }));
    await userEvent.type(canvas.getByLabelText('Requested objective'), 'Request a stock reservation change');
    await userEvent.click(canvas.getByText('Work', { selector: 'summary' }));
    await userEvent.click(canvas.getByRole('checkbox', { name: 'Unknown / not supplied: affected components' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Add affected components item' }));
    await userEvent.type(canvas.getByLabelText('Optional repository path'), 'src/stock.ts');
    await userEvent.click(canvas.getByText('Acceptance', { selector: 'summary' }));
    await userEvent.click(canvas.getByRole('checkbox', { name: 'Unknown / not supplied: positive cases' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Add positive cases item' }));
    await userEvent.type(canvas.getByLabelText('Requested case description'), 'Stock can be reserved');
    await userEvent.type(canvas.getByLabelText('Ordered steps (one per line)'), 'Prepare stock{Enter}Request reservation');
    await userEvent.type(canvas.getByLabelText('Expected evidence (one per line)'), 'Reservation response');
    await userEvent.click(canvas.getByText('Handoff', { selector: 'summary' }));
    await userEvent.click(canvas.getByRole('checkbox', { name: 'Unknown / not supplied: budgets' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Add budgets item' }));
    await userEvent.type(canvas.getByLabelText('Requested resource'), 'Review time');
    await userEvent.type(canvas.getByLabelText('Requested limit'), '30 minutes');
    await userEvent.click(canvas.getByRole('button', { name: 'Generate and open saved packet' }));
    await expect(args.state.generate).toHaveBeenCalledWith(expect.objectContaining({
      intent: expect.objectContaining({ objective: { state: 'supplied', value: 'Request a stock reservation change', origin: { kind: 'requested_intent' } } }),
      work: expect.objectContaining({ affected_components: { state: 'supplied', value: [{ repo_key: 'local/shop', path: 'src/stock.ts', fact_references: [] }], origin: { kind: 'requested_intent' } } }),
      acceptance: expect.objectContaining({ positive_cases: { state: 'supplied', value: [{ id: 'positive_cases-1', description: 'Stock can be reserved', steps: ['Prepare stock', 'Request reservation'], expected_evidence: ['Reservation response'], evidence_ids: [] }], origin: { kind: 'requested_intent' } }, execution_evidence: [] }),
      handoff: expect.objectContaining({ budgets: { state: 'supplied', value: [{ resource: 'Review time', limit: '30 minutes' }], origin: { kind: 'requested_intent' } } }),
    }));
  },
};
