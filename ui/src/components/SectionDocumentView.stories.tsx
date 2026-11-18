import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { SectionDocumentView } from './SectionDocumentView';
import { NOT_COMPUTED_READ } from '../curatedFixtures';
import {
  ACTOR_FACT, CAPABILITY_FACT, ELIGIBLE_DRAFT, ELIGIBLE_ROW, MISSING_FACT, NEW_FACT, PENDING_DRAFT,
  REDRAFT_CHANGE, REDRAFT_DRAFT, RETURNED_DRAFT, STALE_DRAFT, STALE_ROW, SUBJECT_SCOPE, SUCCESSOR_DRAFT,
  UNRESOLVABLE_DRAFT, UNRESOLVABLE_ROW, curatedReadWithDrafts, sectionDocumentFixture,
} from '../sectionDocumentFixtures';

// SPEC-15 slice 7 (ADR-0064), AC-0669: the Workbench shows each section of
// D1 with its kind, its drafted claims with their annotations and openable
// citations, the state and reason of an accepted draft, and the draft and
// review actions. Every kind, review and state is the host's.
const meta = {
  title: 'Spec/SectionDocumentView',
  component: SectionDocumentView,
  args: {
    document: sectionDocumentFixture(),
    curated: curatedReadWithDrafts(),
    busy: false,
    error: null,
    canReview: true,
    onReload: fn(),
    onDraft: fn(),
    onRedraft: fn(),
    onReview: fn(),
    onOpenCitation: fn(),
  },
} satisfies Meta<typeof SectionDocumentView>;
export default meta;
type Story = StoryObj<typeof meta>;

async function open(canvasElement: HTMLElement) {
  const canvas = within(canvasElement);
  await userEvent.click(canvas.getByText(/^Sections and drafts:/));
  return canvas;
}

export const SectionsWithKindsAndDrafts: Story = {
  // AC-0669: every section with its kind; a drafted claim with its kind,
  // tier, role and citations; P and H sections offer no draft.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 3, name: 'Product specification sections' })).toBeVisible();
    const summary = canvas.getByText(/^Sections and drafts:/);
    await expect(summary).toHaveTextContent('8 sections, 5 drafts (2 awaiting review, 2 accepted)');
    await expect(summary.closest('details')).not.toHaveAttribute('open');
    await userEvent.click(summary);

    const capabilities = within(canvas.getByRole('region', { name: /1\.3 Capabilities/ }));
    await expect(capabilities.getByText('P+A')).toHaveAttribute(
      'title', 'Projected and authored: a compiled frame, then drafted prose.',
    );
    const pending = within(capabilities.getByRole('article', { name: `Draft ${PENDING_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(pending.getByTestId('draft-review')).toHaveTextContent('Awaiting review');
    await expect(pending.getByText('Whole system')).toBeVisible();
    await expect(pending.getByText('An administrator can check out an order.')).toBeVisible();
    await expect(pending.getByText('(Implemented)')).toBeVisible();
    await expect(pending.getByText('(Documented intent)')).toBeVisible();
    // R-INT-2: the tier in words and with the shared badge, and the role.
    await expect(pending.getByText('T3')).toBeVisible();
    await expect(pending.getByText('section-writer@1')).toBeVisible();
    await expect(pending.getByText(/Not accepted: no document prints it\./)).toBeVisible();
    await expect(pending.queryByTestId('draft-state')).not.toBeInTheDocument();

    // A citation opens its evidence.
    const citations = pending.getAllByRole('button', { name: `Open evidence for ${CAPABILITY_FACT}` });
    await userEvent.click(citations[0]);
    await expect(args.onOpenCitation).toHaveBeenCalledWith(CAPABILITY_FACT);
    await userEvent.click(pending.getByRole('button', { name: `Open evidence for ${ACTOR_FACT}` }));
    await expect(args.onOpenCitation).toHaveBeenLastCalledWith(ACTOR_FACT);

    // P and H sections: compiled or left blank, never drafted.
    const flows = within(canvas.getByRole('region', { name: /1\.6 Business flows/ }));
    await expect(flows.getByText(/nothing is drafted here/)).toBeVisible();
    await expect(flows.queryByRole('button', { name: 'Draft this section' })).not.toBeInTheDocument();
    const nonGoals = within(canvas.getByRole('region', { name: /1\.7 Out of scope/ }));
    await expect(nonGoals.getByText('Left blank for your team. Cartograph never fills it.')).toBeVisible();
    await expect(nonGoals.queryByRole('button')).not.toBeInTheDocument();

    // A drafted section with no draft says so.
    const rules = within(canvas.getByRole('region', { name: /1\.5 Business rules/ }));
    await expect(rules.getByTestId('section-not-drafted')).toHaveTextContent('Not drafted.');
  },
};

export const AcceptedDraftShowsTheHostStateAndReason: Story = {
  // AC-0669: an accepted draft shows the state and the reason sentence of
  // the curated read; acceptance does not raise the tier.
  play: async ({ canvasElement }) => {
    const canvas = await open(canvasElement);
    const eligible = within(canvas.getByRole('article', { name: `Draft ${ELIGIBLE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(eligible.getByTestId('draft-review')).toHaveTextContent('Accepted');
    await expect(eligible.getByTestId('draft-state')).toHaveTextContent(
      'State: eligible. Every fact this draft cites is unchanged; a best-effort export prints it. (all_conditions_hold)',
    );
    await expect(eligible.getByText(/Human-reviewed: accepted\. Acceptance does not raise the tier\./)).toBeVisible();
    await expect(eligible.getByText('T3')).toBeVisible();

    const stale = within(canvas.getByRole('article', { name: `Draft ${STALE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(stale.getByText(`Subject ${SUBJECT_SCOPE}`)).toBeVisible();
    await expect(stale.getByTestId('draft-state')).toHaveTextContent(
      'State: stale. A fact this draft cites changed after the draft was written; a best-effort export prints it marked stale. (cited_fact_changed)',
    );
  },
};

export const StaleDraftNamesItsChangedAndMissingFacts: Story = {
  // SPEC-17 slice 3 (ADR-0045), AC-0673: a stale or unresolvable accepted
  // draft shows the host's changed and missing ids under its state, marks
  // the claims that cite a changed fact, and offers a re-draft.
  args: {
    document: sectionDocumentFixture([ELIGIBLE_DRAFT, STALE_DRAFT, UNRESOLVABLE_DRAFT, PENDING_DRAFT]),
    curated: curatedReadWithDrafts([ELIGIBLE_ROW, STALE_ROW, UNRESOLVABLE_ROW]),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = await open(canvasElement);
    const stale = within(canvas.getByRole('article', { name: `Draft ${STALE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(stale.getByTestId('draft-changed')).toHaveTextContent(
      `Changed since this draft was accepted: ${CAPABILITY_FACT}`,
    );
    await expect(stale.queryByTestId('draft-missing')).not.toBeInTheDocument();
    // Both claims cite the changed capability fact.
    await expect(stale.getAllByText('Cites a changed fact.')).toHaveLength(2);
    await userEvent.click(stale.getByRole('button', { name: `Open changed fact ${CAPABILITY_FACT}` }));
    await expect(args.onOpenCitation).toHaveBeenLastCalledWith(CAPABILITY_FACT);
    await userEvent.click(stale.getByRole('button', { name: 'Re-draft this section' }));
    await expect(args.onRedraft).toHaveBeenLastCalledWith(STALE_DRAFT);

    const gone = within(canvas.getByRole('article', { name: `Draft ${UNRESOLVABLE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(gone.getByTestId('draft-state')).toHaveTextContent('State: unresolvable.');
    await expect(gone.getByTestId('draft-changed')).toHaveTextContent(ACTOR_FACT);
    await expect(gone.getByTestId('draft-missing')).toHaveTextContent(`No longer in the graph: ${MISSING_FACT}`);
    // A missing fact has no evidence to open.
    await expect(gone.queryByRole('button', { name: new RegExp(MISSING_FACT) })).not.toBeInTheDocument();
    // Only the claim citing the changed actor fact is marked.
    await expect(gone.getAllByText('Cites a changed fact.')).toHaveLength(1);
    await userEvent.click(gone.getByRole('button', { name: 'Re-draft this section' }));
    await expect(args.onRedraft).toHaveBeenLastCalledWith(UNRESOLVABLE_DRAFT);

    // An eligible or unreviewed draft names no fact and offers no re-draft.
    for (const draft of [ELIGIBLE_DRAFT, PENDING_DRAFT]) {
      const card = within(canvas.getByRole('article', { name: `Draft ${draft.draft_id.slice(0, 12)}` }));
      await expect(card.queryByTestId('draft-changed')).not.toBeInTheDocument();
      await expect(card.queryByTestId('draft-missing')).not.toBeInTheDocument();
      await expect(card.queryByText('Cites a changed fact.')).not.toBeInTheDocument();
      await expect(card.queryByRole('button', { name: 'Re-draft this section' })).not.toBeInTheDocument();
    }
  },
};

export const ReDraftIsShownBesideTheAcceptedDraftItReplaces: Story = {
  // SPEC-17 slice 4 (ADR-0045), AC-0674: a re-draft names the accepted
  // draft it replaces, which stays in place until the re-draft is accepted;
  // like any successor it is accepted or rejected, never returned.
  args: {
    document: sectionDocumentFixture([STALE_DRAFT, REDRAFT_DRAFT]),
    curated: curatedReadWithDrafts([STALE_ROW]),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = await open(canvasElement);
    const redraft = within(canvas.getByRole('article', { name: `Draft ${REDRAFT_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(redraft.getByTestId('draft-redraft-of')).toHaveTextContent(
      `Re-draft of accepted draft ${STALE_DRAFT.draft_id.slice(0, 12)}. That draft stays in place until this one is accepted.`,
    );
    await expect(redraft.queryByText(/Replaces returned draft/)).not.toBeInTheDocument();
    await expect(redraft.queryByRole('button', { name: 'Return' })).not.toBeInTheDocument();
    await userEvent.click(redraft.getByRole('button', { name: 'Accept' }));
    await expect(args.onReview).toHaveBeenLastCalledWith(REDRAFT_DRAFT, 'accepted', null);
    // The accepted draft is still shown with its state.
    const stale = within(canvas.getByRole('article', { name: `Draft ${STALE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(stale.getByTestId('draft-state')).toHaveTextContent('State: stale.');
  },
};

export const ReDraftShowsWhatItChanges: Story = {
  // SPEC-17 slice 5 (ADR-0045 decision 7), AC-0675: beside a draft that
  // replaces another, the host's comparison: claims added, removed and
  // kept, and cited facts added, removed and changed, before review.
  args: {
    document: sectionDocumentFixture(
      [STALE_DRAFT, REDRAFT_DRAFT, PENDING_DRAFT],
      { [REDRAFT_DRAFT.draft_id]: REDRAFT_CHANGE },
    ),
    curated: curatedReadWithDrafts([STALE_ROW]),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = await open(canvasElement);
    const redraft = within(canvas.getByRole('article', { name: `Draft ${REDRAFT_DRAFT.draft_id.slice(0, 12)}` }));
    const change = redraft.getByTestId('draft-change');
    await expect(change).toHaveTextContent(
      'Change from the draft it replaces: 1 claim added, 1 removed, 1 kept; 3 cited facts differ',
    );
    await expect(change).not.toHaveAttribute('open');
    await userEvent.click(within(change).getByText(/^Change from the draft it replaces/));
    const detail = within(change);
    await expect(detail.getByText('Added')).toBeVisible();
    await expect(detail.getByText('Checkout validates the cart and the stock.')).toBeVisible();
    await expect(detail.getByText('Removed')).toBeVisible();
    await expect(detail.getByText('Checkout validates the cart before payment.')).toBeVisible();
    await expect(detail.getByText('Kept')).toBeVisible();
    await expect(detail.getByText('The README says refunds are planned.')).toBeVisible();
    // A fact the re-draft cites opens; one it no longer cites is text.
    await userEvent.click(detail.getByRole('button', { name: `Open evidence for ${NEW_FACT}` }));
    await expect(args.onOpenCitation).toHaveBeenLastCalledWith(NEW_FACT);
    await userEvent.click(detail.getByRole('button', { name: `Open evidence for ${CAPABILITY_FACT}` }));
    await expect(args.onOpenCitation).toHaveBeenLastCalledWith(CAPABILITY_FACT);
    await expect(detail.getByText(/No longer cited:/)).toHaveTextContent(ACTOR_FACT);
    await expect(detail.queryByRole('button', { name: `Open evidence for ${ACTOR_FACT}` })).not.toBeInTheDocument();
    // It is shown before review: the review actions are still offered.
    await expect(redraft.getByRole('button', { name: 'Accept' })).toBeEnabled();

    // A draft that replaces none, or whose change the host did not send,
    // shows no comparison.
    for (const draft of [STALE_DRAFT, PENDING_DRAFT]) {
      const card = within(canvas.getByRole('article', { name: `Draft ${draft.draft_id.slice(0, 12)}` }));
      await expect(card.queryByTestId('draft-change')).not.toBeInTheDocument();
    }
  },
};

export const ReviewActionsSendTheDecisionAndNote: Story = {
  // AC-0669: accept, reject and return are sent with the draft as read and
  // the optional note; the current decision is not offered again and a
  // successor cannot be returned.
  play: async ({ canvasElement, args }) => {
    const canvas = await open(canvasElement);
    const pending = within(canvas.getByRole('article', { name: `Draft ${PENDING_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(pending.getByRole('button', { name: 'Accept' })).toBeEnabled();
    await expect(pending.getByRole('button', { name: 'Reject' })).toBeEnabled();
    await expect(pending.getByRole('button', { name: 'Return' })).toBeEnabled();
    await userEvent.click(pending.getByRole('button', { name: 'Accept' }));
    await expect(args.onReview).toHaveBeenLastCalledWith(PENDING_DRAFT, 'accepted', null);
    await userEvent.type(pending.getByLabelText('Review note (optional)'), '  Cite the handler.  ');
    await userEvent.click(pending.getByRole('button', { name: 'Return' }));
    await expect(args.onReview).toHaveBeenLastCalledWith(PENDING_DRAFT, 'returned', 'Cite the handler.');

    const accepted = within(canvas.getByRole('article', { name: `Draft ${ELIGIBLE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(accepted.queryByRole('button', { name: 'Accept' })).not.toBeInTheDocument();
    await userEvent.click(accepted.getByRole('button', { name: 'Reject' }));
    await expect(args.onReview).toHaveBeenLastCalledWith(ELIGIBLE_DRAFT, 'rejected', null);

    const returned = within(canvas.getByRole('article', { name: `Draft ${RETURNED_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(returned.getByTestId('draft-review')).toHaveTextContent('Returned');
    await expect(returned.getByText('Review note: Cite the checkout handler.')).toBeVisible();
    await expect(returned.queryByRole('button', { name: 'Return' })).not.toBeInTheDocument();

    const successor = within(canvas.getByRole('article', { name: `Draft ${SUCCESSOR_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(successor.getByText(/Replaces returned draft/)).toBeVisible();
    await expect(successor.queryByRole('button', { name: 'Return' })).not.toBeInTheDocument();
    await expect(successor.getByRole('button', { name: 'Accept' })).toBeEnabled();
  },
};

export const DraftRequestNamesTheSectionAndScope: Story = {
  // AC-0669: "Draft this section" sends the address: the system scope, or
  // one subject by citation id where the section is written per subject.
  play: async ({ canvasElement, args }) => {
    const canvas = await open(canvasElement);
    const purpose = within(canvas.getByRole('region', { name: /1\.1 Purpose and scope/ }));
    await expect(purpose.queryByRole('textbox', { name: /by citation id/ })).not.toBeInTheDocument();
    await userEvent.click(purpose.getByRole('button', { name: 'Draft this section' }));
    await expect(args.onDraft).toHaveBeenLastCalledWith({
      catalogue_version: '1.0', document: 'D1', section: '1.1', scope: 'system',
    });

    const rules = within(canvas.getByRole('region', { name: /1\.5 Business rules/ }));
    const subject = rules.getByRole('textbox', { name: /One BusinessRule by citation id/ });
    await userEvent.type(subject, 'rule-1');
    await expect(rules.getByRole('alert')).toHaveTextContent('F- followed by 16 lowercase hex digits');
    await expect(rules.getByRole('button', { name: 'Draft this section' })).toBeDisabled();
    await userEvent.clear(subject);
    await userEvent.type(subject, SUBJECT_SCOPE);
    await expect(rules.queryByRole('alert')).not.toBeInTheDocument();
    await userEvent.click(rules.getByRole('button', { name: 'Draft this section' }));
    await expect(args.onDraft).toHaveBeenLastCalledWith({
      catalogue_version: '1.0', document: 'D1', section: '1.5', scope: SUBJECT_SCOPE,
    });
  },
};

export const AcceptedStateIsNotStatedWithoutACuratedRead: Story = {
  // AC-0669: when the curated projection was not computed, an accepted
  // draft's state is not guessed.
  args: { curated: NOT_COMPUTED_READ },
  play: async ({ canvasElement }) => {
    const canvas = await open(canvasElement);
    const accepted = within(canvas.getByRole('article', { name: `Draft ${ELIGIBLE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(accepted.queryByTestId('draft-state')).not.toBeInTheDocument();
    await expect(accepted.getByTestId('draft-state-unread')).toHaveTextContent(
      'State not shown: the curated projection was not read or not computed.',
    );
  },
};

export const AcceptedAfterTheReadOnScreen: Story = {
  // AC-0669: a draft accepted after the curated read on screen has no state
  // until the next read.
  args: { curated: curatedReadWithDrafts([]) },
  play: async ({ canvasElement }) => {
    const canvas = await open(canvasElement);
    const accepted = within(canvas.getByRole('article', { name: `Draft ${ELIGIBLE_DRAFT.draft_id.slice(0, 12)}` }));
    await expect(accepted.getByTestId('draft-state-unread')).toHaveTextContent('Refresh it to see the state.');
  },
};

export const CoreUnavailableDisablesReviewAndRequest: Story = {
  args: { canReview: false, error: 'The core is not reachable.' },
  play: async ({ canvasElement }) => {
    const canvas = await open(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent('The core is not reachable.');
    for (const button of canvas.getAllByRole('button', { name: /^(Accept|Reject|Return|Draft this section)$/ })) {
      await expect(button).toBeDisabled();
    }
    await expect(canvas.getByRole('button', { name: 'Reload sections' })).toBeEnabled();
  },
};

export const NotRead: Story = {
  // AC-0669: with no document view, nothing is stated about drafts.
  args: { document: null, onReload: undefined },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('document-unread')).toHaveTextContent('has not been read');
    await expect(canvas.queryByRole('button')).not.toBeInTheDocument();
  },
};
