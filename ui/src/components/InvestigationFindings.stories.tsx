import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { InvestigationFindings } from './InvestigationFindings';
import { investigationCitations, investigationResult } from '../investigationFixtures';

const meta = { title: 'Investigations/Findings', component: InvestigationFindings,
  args: { result: investigationResult(), citations: investigationCitations, citationId: null, read: null,
    reviews: investigationResult().findings.map((finding, index) => ({ subject: { investigation_id: 'inv-fixture', result_id: investigationResult().result_id, finding_id: finding.finding_id, subject_id: `investigation-finding-review-v1:${String(index).repeat(64)}` }, revision: 0, decision: null, reviewed_at: null })),
    onReview: fn(), onRefreshReviews: fn(), loading: false, error: null, onRead: fn(), onClose: fn() } } satisfies Meta<typeof InvestigationFindings>;
export default meta;
type Story = StoryObj<typeof meta>;

export const TypedCitedWeakFindings: Story = {
  // AC-0189/0190/0295: only qualified follow-up controls; producing tier stays weak.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Implemented behavior')).toBeVisible();
    await expect(canvas.getByText('Proposed design')).toBeVisible();
    await expect(canvas.getAllByText(/T3 · InferredWeak/)).toHaveLength(2);
    await expect(canvas.queryByRole('button', { name: /accept/i })).not.toBeInTheDocument();
    await userEvent.click(canvas.getAllByRole('button', { name: 'Use for follow-up' })[0]);
    await expect(args.onReview).toHaveBeenCalledWith(investigationResult().findings[0].finding_id, 'accepted_for_follow_up');
    await userEvent.click(canvas.getAllByRole('button', { name: 'Do not use for follow-up' })[1]);
    await expect(args.onReview).toHaveBeenCalledWith(investigationResult().findings[1].finding_id, 'rejected_for_follow_up');
    await userEvent.click(canvas.getByRole('button', { name: 'Inspect citation C1' }));
    await expect(args.onRead).toHaveBeenCalledWith('C1');
  },
};

export const OriginalHistoricalSource: Story = {
  args: { citationId: 'C1', read: { investigation_id: 'inv-fixture', citation_id: 'C1',
    citation: investigationCitations[0], status: 'available', text: 'ORIGINAL RETAINED SOURCE <script>not executed</script>' } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const inspector = within(canvas.getByRole('complementary', { name: 'Historical citation' }));
    await expect(inspector.getByText('Retained historical source')).toBeVisible();
    await expect(inspector.getByText(/ORIGINAL RETAINED SOURCE <script>/)).toBeVisible();
    await expect(canvasElement.querySelector('script')).toBeNull();
    await expect(inspector.getByText(/complete input coverage and business meaning are not established/)).toBeVisible();
    await userEvent.click(inspector.getByRole('button', { name: 'Close citation' }));
    await expect(args.onClose).toHaveBeenCalled();
  },
};

export const ForgottenSource: Story = {
  args: { citationId: 'C1', read: { investigation_id: 'inv-fixture', citation_id: 'C1',
    citation: investigationCitations[0], status: 'unavailable', text: null } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Retained source is unavailable')).toBeVisible();
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await expect(canvas.getByText(/Current graph facts and checkout bytes are never substituted/)).toBeVisible();
  },
};

export const GraphMetadataOnly: Story = {
  args: { citationId: 'C2', read: { investigation_id: 'inv-fixture', citation_id: 'C2',
    citation: investigationCitations[1], status: 'metadata_only', text: null } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Historical graph metadata only')).toBeVisible();
    await expect(canvas.getByText(/Original graph properties were not archived/)).toBeVisible();
  },
};

export const InsufficientEvidence: Story = {
  args: { result: { ...investigationResult(), findings: [], knowledge_completeness: 'insufficient_evidence' } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText('Insufficient evidence to support an answer.')).toBeVisible();
    await expect(within(canvasElement).queryByRole('button', { name: 'Use for follow-up' })).not.toBeInTheDocument();
  },
};

export const ReviewUnavailable: Story = {
  args: { reviewError: 'Review state unavailable. Refresh reviews before making a decision.' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('A local stock guard is present')).toBeVisible();
    await expect(canvas.getAllByRole('button', { name: 'Use for follow-up' })[0]).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh reviews' }));
    await expect(args.onRefreshReviews).toHaveBeenCalled();
  },
};
export const ConflictWinner: Story = {
  args: { reviews: [{ subject: { investigation_id: 'inv-fixture', result_id: investigationResult().result_id, finding_id: investigationResult().findings[0].finding_id, subject_id: `investigation-finding-review-v1:${'0'.repeat(64)}` }, revision: 1, decision: 'rejected_for_follow_up', reviewed_at: '2026-10-21T00:00:00Z' }], reviewError: 'Another decision won. The saved winner is shown. Refresh reviews, then deliberately choose again.' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Rejected for follow-up')).toBeVisible();
    await expect(canvas.getByText(/Review revision 1/)).toBeVisible();
    await expect(canvas.getAllByRole('button', { name: 'Use for follow-up' })[0]).toBeDisabled();
  },
};
export const SavedAcceptedDecision: Story = {
  args: { reviews: [{ subject: { investigation_id: 'inv-fixture', result_id: investigationResult().result_id, finding_id: investigationResult().findings[0].finding_id, subject_id: `investigation-finding-review-v1:${'0'.repeat(64)}` }, revision: 2, decision: 'accepted_for_follow_up', reviewed_at: '2026-10-21T00:00:00Z' }] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Accepted for follow-up')).toBeVisible();
    await expect(canvas.getAllByText(/T3 · InferredWeak/)).toHaveLength(2);
    await expect(canvas.getByText(/Review revision 2/)).toBeVisible();
  },
};
