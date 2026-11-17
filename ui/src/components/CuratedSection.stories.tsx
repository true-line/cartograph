import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { CuratedSection } from './CuratedSection';
import {
  CURATED_GRAPH_BASIS, CURATED_REVISION, NOT_COMPUTED_READ, UNVERIFIED_RECORD,
  curatedReadFixture, emptyCuratedRead,
} from '../curatedFixtures';

// SPEC-12 slice 3 (ADR-0061), AC-0655: the Workbench shows the curated
// projection with its DR-4 fields, and every state, reason sentence, count
// and revision is the host's.
const meta = {
  title: 'Spec/CuratedSection',
  component: CuratedSection,
  args: { read: curatedReadFixture(), busy: false, onRefresh: fn() },
} satisfies Meta<typeof CuratedSection>;
export default meta;
type Story = StoryObj<typeof meta>;

export const ItemsAndListedRecordsInSeveralStates: Story = {
  // AC-0655: items and listed records carry every DR-4 field; listed records
  // are grouped by state with the host's reason sentence; a curated item is
  // marked InferredWeak in words and with the shared badge (R-INT-2).
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 3, name: 'Curated items' })).toBeVisible();

    // Revision: shortened, with the full value in the title and in the list.
    const revision = canvas.getByTestId('curated-revision');
    await expect(revision).toHaveTextContent('curated-revision-v1:9a9a9a9a9a9a');
    await expect(revision).not.toHaveTextContent(CURATED_REVISION);
    await expect(revision).toHaveAttribute('title', CURATED_REVISION);
    await userEvent.click(canvas.getByText('Full identifiers'));
    await expect(canvas.getByText(CURATED_REVISION)).toBeVisible();
    await expect(canvas.getByText(CURATED_GRAPH_BASIS)).toBeVisible();

    // View and counts.
    const identity = within(canvas.getByText('View').closest('dl') as HTMLElement);
    await expect(identity.getByText('curated')).toBeVisible();
    await expect(identity.getByText('best-effort')).toBeVisible();
    const counts = canvas.getByLabelText('Curated record counts');
    await expect(counts).toHaveTextContent('Accepted4');
    await expect(counts).toHaveTextContent('Eligible (curated items)1');
    await expect(counts).toHaveTextContent('Unverified1');
    await expect(counts).toHaveTextContent('Stale1');
    await expect(counts).toHaveTextContent('Conflicted1');
    await expect(counts).toHaveTextContent('Unresolvable0');

    // The records are closed until asked for; the summary counts them.
    const records = canvas.getByText(/^Records and decisions:/);
    await expect(records).toHaveTextContent(
      '1 curated item shown, 3 listed with a reason, 1 Workbench decision matching nothing',
    );
    await expect(records.closest('details')).not.toHaveAttribute('open');
    await userEvent.click(records);

    // The item: every DR-4 field, marked InferredWeak.
    const items = within(canvas.getByRole('table', { name: 'Curated items (1)' }));
    const item = within(items.getByRole('row', { name: /proposal:eligible/ }));
    await expect(item.getByRole('rowheader')).toHaveTextContent('proposal:eligible');
    await expect(item.getByText(/sym:capture/).closest('td')).toHaveTextContent('sym:capture —CALLS→ sym:send');
    await expect(item.getByText('gap:sync')).toBeVisible();
    await expect(item.getByText('Agentic')).toBeVisible();
    await expect(item.getByText('Inferred (weak)')).toHaveClass('tier-badge', 'tier-inferredweak');
    await expect(item.getByText('accepted')).toBeVisible();
    await expect(item.getByText('captured 2, unverified 0')).toBeVisible();
    await expect(item.getByText(/graph unchanged; associations unchanged/)).toBeVisible();
    await expect(item.getByText('eligible')).toBeVisible();
    await expect(item.getByText(/InferredWeak curated item, not a recovered fact/)).toBeVisible();
    // Nothing in the section is shown as confirmed.
    await expect(canvasElement.querySelector('.tier-confirmed')).toBeNull();

    // Listed records: one table per state, in the host's state order, each
    // row with the reason sentence and its code.
    const captions = [...canvasElement.querySelectorAll('caption')].map((node) => node.textContent);
    await expect(captions.slice(0, 4)).toEqual([
      'Curated items (1)', 'State unverified (1)', 'State stale (1)', 'State conflicted (1)',
    ]);
    const unverified = within(canvas.getByRole('table', { name: 'State unverified (1)' }));
    await expect(unverified.getByRole('row', { name: /proposal:legacy/ })).toHaveTextContent(
      `${UNVERIFIED_RECORD.reason_sentence} (legacy_record)`,
    );
    await expect(unverified.getByText('captured 0, unverified 2')).toBeVisible();
    await expect(unverified.getByText('Inferred (weak)')).toBeVisible();
    const stale = within(canvas.getByRole('table', { name: 'State stale (1)' }));
    await expect(stale.getByRole('row', { name: /proposal:moved/ })).toHaveTextContent(
      'The graph has changed since the proposal was made. (graph_changed)',
    );
    const conflicted = within(canvas.getByRole('table', { name: 'State conflicted (1)' }));
    await expect(conflicted.getByRole('row', { name: /proposal:other-target/ })).toHaveTextContent(
      'Another accepted record proposes a different edge for the same Gap.',
    );

    // Workbench decisions that match nothing.
    await expect(canvas.getByText(/Rejections applied: 1\. Decisions that match no current statement: 1\./)).toBeVisible();
    const decisions = within(canvas.getByRole('table', { name: /Decisions that match no current statement/ }));
    const decision = decisions.getByRole('row', { name: /d1d1d1d1d1d1/ });
    await expect(decision).toHaveTextContent('rejected');
    await expect(decision).toHaveTextContent('stale');

    // Keyboard reachable: the wide tables are focusable scroll regions, and
    // refresh is a button.
    await expect(canvas.getByRole('region', { name: 'Curated items (1)' })).toHaveAttribute('tabindex', '0');
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh curated items' }));
    await expect(args.onRefresh).toHaveBeenCalledOnce();
  },
};

export const NoAcceptedProposal: Story = {
  // AC-0655: a computed projection with no accepted proposal says so, and
  // still shows its revision and counts.
  args: { read: emptyCuratedRead() },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('curated-empty')).toHaveTextContent('No accepted proposal is recorded.');
    await expect(canvas.getByTestId('curated-revision')).toHaveTextContent('curated-revision-v1:000000000000');
    await expect(canvas.getByLabelText('Curated record counts')).toHaveTextContent('Accepted0');
    await userEvent.click(canvas.getByText(/^Records and decisions: 0 curated items shown, 0 listed/));
    await expect(canvas.queryByRole('table')).not.toBeInTheDocument();
    await expect(canvas.queryByTestId('curated-not-computed')).not.toBeInTheDocument();
    await expect(canvas.getByText(/Rejections applied: 0\. Decisions that match no current statement: 0\./)).toBeVisible();
  },
};

export const NotComputed: Story = {
  // AC-0655: a projection that was not computed shows the host's reason
  // sentence and nothing else: no revision, no count, no record, and no
  // claim that there are no accepted proposals.
  args: { read: NOT_COMPUTED_READ },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const notice = canvas.getByTestId('curated-not-computed');
    await expect(notice).toHaveTextContent('The curated projection was not computed.');
    await expect(notice).toHaveTextContent(
      'Reason: More than 256 proposals are accepted, which is the most one export reads.',
    );
    await expect(notice).toHaveTextContent('(too_many_accepted)');
    await expect(canvas.queryByTestId('curated-revision')).not.toBeInTheDocument();
    await expect(canvas.queryByLabelText('Curated record counts')).not.toBeInTheDocument();
    await expect(canvas.queryByTestId('curated-empty')).not.toBeInTheDocument();
    await expect(canvas.queryByRole('table')).not.toBeInTheDocument();
  },
};

export const VerifiedOnlyLeavesItemsOutWithTheCount: Story = {
  // AC-0655, R-INT-5: a verified-only read shows no curated item, only how
  // many were left out. Listed records are the same in both modes.
  args: { read: curatedReadFixture('verified-only') },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const omitted = canvas.getByTestId('curated-omitted');
    await expect(omitted).toHaveTextContent('No curated item is shown');
    await expect(omitted).toHaveTextContent('Curated items left out: 1.');
    await userEvent.click(canvas.getByText(/^Records and decisions: 0 curated items shown, 3 listed/));
    await expect(canvas.queryByText('proposal:eligible')).not.toBeInTheDocument();
    await expect(canvas.queryByRole('table', { name: /Curated items/ })).not.toBeInTheDocument();
    // The count of eligible records is still reported, and so is the revision.
    await expect(canvas.getByLabelText('Curated record counts')).toHaveTextContent('Eligible (curated items)1');
    await expect(canvas.getByTestId('curated-revision')).toHaveAttribute('title', CURATED_REVISION);
    await expect(canvas.getByRole('table', { name: 'State unverified (1)' })).toBeVisible();
    await expect(canvas.getByRole('table', { name: 'State stale (1)' })).toBeVisible();
  },
};

export const NotRead: Story = {
  // AC-0655: with no read (no core), nothing is stated about accepted
  // proposals.
  args: { read: null, onRefresh: undefined },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('curated-unread')).toHaveTextContent('has not been read');
    await expect(canvas.queryByRole('button')).not.toBeInTheDocument();
  },
};
