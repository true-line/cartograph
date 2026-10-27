import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { investigationCitations } from '../investigationFixtures';
import { InvestigationMessageMeta } from './InvestigationMessageMeta';

const meta = { title: 'Investigations/MessageEvidence', component: InvestigationMessageMeta } satisfies Meta<typeof InvestigationMessageMeta>;
export default meta;
type Story = StoryObj<typeof meta>;

// AC-0414: unique references, complete persisted origins and honest absence.
export const CountsRequireCompleteMetadata: Story = {
  render: () => <>{[
    { name: 'Mixed', citationIds: ['C1', 'C2', 'C1'], citations: investigationCitations },
    { name: 'Zero', citationIds: [], citations: [] },
    { name: 'Absent' },
    { name: 'Missing', citationIds: ['C1', 'missing'], citations: investigationCitations },
    { name: 'Duplicate records', citationIds: ['C1'], citations: [investigationCitations[0], investigationCitations[0]] },
    { name: 'Origin mismatch', citationIds: ['C2'], citations: [{ ...investigationCitations[1], source: investigationCitations[0].source }] },
  ].map(({ name, ...props }) => <section key={name} aria-label={name}><InvestigationMessageMeta {...props} /></section>)}</>,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('region', { name: 'Mixed' })).toHaveTextContent('1 source citations · 1 graph citations');
    await expect(canvas.getByRole('region', { name: 'Zero' })).toHaveTextContent('0 source citations · 0 graph citations');
    for (const name of ['Absent', 'Missing', 'Duplicate records', 'Origin mismatch']) {
      await expect(canvas.getByRole('region', { name })).toHaveTextContent('Citation metadata unavailable');
    }
    const group = within(canvas.getByRole('region', { name: 'Mixed' })).getByRole('group');
    await expect(group).toHaveTextContent('T3 / Inferred (weak)');
    await expect(group.querySelector('.tier-inferredweak')).toBeVisible();
    const token = document.createElement('span');
    token.style.color = 'var(--tier-inferred-weak)'; group.append(token);
    expect(getComputedStyle(group.querySelector('.tier-inferredweak')!).color).toBe(getComputedStyle(token).color);
    token.remove();
  },
};
