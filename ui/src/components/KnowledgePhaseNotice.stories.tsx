// AC-0588 (#1782): a knowledge phase that skipped or left out work says so, with the named reason.
import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import { KnowledgePhaseNotice, type KnowledgeReport, type KnowledgeStatus } from './KnowledgePhaseNotice';

const ran: KnowledgeReport = {
  embedded: 12, omitted: 0, omission: null, derived: 4, oversized: 0,
  derivedOmission: null, producerOmissions: [], published: true,
};
let last: KnowledgeStatus | null = null;

const meta = {
  title: 'Surfaces/KnowledgePhaseNotice', component: KnowledgePhaseNotice, args: { available: true },
  beforeEach: () => {
    last = null;
    window.localStorage.removeItem('cartograph.knowledgePhase.dismissed');
    mockIPC((command) => {
      if (command === 'knowledge_phase_status') return last;
      throw new Error(`unexpected command ${command}`);
    }, { shouldMockEvents: true });
    return () => clearMocks();
  },
} satisfies Meta<typeof KnowledgePhaseNotice>;
export default meta;
type Story = StoryObj<typeof meta>;

/** A phase that did everything shows nothing. */
export const NothingSkipped: Story = {
  beforeEach: () => { last = { report: ran, error: null }; },
  play: async ({ canvasElement }) => {
    await new Promise((resolve) => setTimeout(resolve, 50));
    await expect(within(canvasElement).queryByRole('status')).toBeNull();
  },
};

/** The outcome of a phase that finished before the window opened is still shown. */
export const BaseTooLarge: Story = {
  beforeEach: () => {
    last = { report: { ...ran, embedded: 0, derived: 0, published: false, omission: 'knowledge-base-bounds' }, error: null };
  },
  play: async ({ canvasElement }) => {
    const notice = await within(canvasElement).findByRole('status', { name: 'Knowledge phase' });
    await expect(notice).toHaveTextContent('larger than the knowledge phase can read');
    await expect(notice).toHaveTextContent('knowledge-base-bounds');
  },
};

/** A live event replaces the notice; every named reason is listed; dismissal holds until the outcome changes. */
export const ReasonsArriveAndDismiss: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await new Promise((resolve) => setTimeout(resolve, 50));
    await expect(canvas.queryByRole('status')).toBeNull();
    await emit('knowledge-phase', {
      report: {
        ...ran, embedded: 0, omission: 'semantic-tier-disabled', derivedOmission: 'knowledge-derived-rejected',
        producerOmissions: ['flow-trace-limit'], omitted: 2, oversized: 1,
      },
      error: null,
    } satisfies KnowledgeStatus);
    const notice = await canvas.findByRole('status', { name: 'Knowledge phase' });
    await expect(notice).toHaveTextContent('Semantic tier (T2) is off');
    await expect(notice).toHaveTextContent('knowledge-derived-rejected');
    await expect(notice).toHaveTextContent('One knowledge producer was skipped (flow-trace-limit)');
    await expect(notice).toHaveTextContent('2 units were past the embedding cap');
    await expect(notice).toHaveTextContent('1 candidate node was too large');
    await userEvent.click(within(notice).getByRole('button', { name: 'Dismiss' }));
    await waitFor(() => expect(canvas.queryByRole('status')).toBeNull());
    await emit('knowledge-phase', { report: null, error: 'store closed' } satisfies KnowledgeStatus);
    await expect(await canvas.findByRole('status', { name: 'Knowledge phase' })).toHaveTextContent('The knowledge phase failed: store closed');
  },
};
