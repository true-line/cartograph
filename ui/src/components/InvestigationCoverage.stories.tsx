import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { InvestigationCoverage } from './InvestigationCoverage';
import { qualifiedCoverageContext, qualifiedResult } from '../investigationFixtures';

const meta = { title: 'Investigations/SavedCoverage', component: InvestigationCoverage,
  args: { context: qualifiedCoverageContext, attestation: qualifiedResult().coverage },
} satisfies Meta<typeof InvestigationCoverage>;
export default meta;
type Story = StoryObj<typeof meta>;

export const HistoricalRecordedStaleAndUnrecorded: Story = {
  // AC-0239: saved qualification retains every repo, including those with no facts.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('acme/api').closest('li')).toHaveTextContent('Recorded at preparation');
    await expect(canvas.getByText('Stale at preparation', { exact: false })).toBeVisible();
    await expect(canvas.getByText('Unrecorded at preparation', { exact: false })).toBeVisible();
    await expect(canvas.getByText('Limited recorded coverage.')).toBeVisible();
    await expect(canvas.getByText(/Excluded producers: 1 · Missing inputs: 1/)).toBeVisible();
    await expect(canvas.getByText('Recorded layers: server')).toBeVisible();
    await expect(canvas.getByText('Coverage is not exhaustive.')).toBeVisible();
    await expect(canvas.getByText(/does not establish complete support or current freshness/)).toBeVisible();
    await expect(canvas.getByText(/Inventory reads do not count as source inspection/)).toBeVisible();
  },
};

export const NoRecordedLimitationIsNotCompleteSupport: Story = {
  args: { context: { ...qualifiedCoverageContext, coverage: { ...qualifiedCoverageContext.coverage,
    repos: [{ ...qualifiedCoverageContext.coverage.repos[0], limited: false,
      counts: { not_selected: 0, missing_input: 0, not_applicable: 2, unsupported: 0 } }] } } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText('No limitation recorded; complete support is not established.')).toBeVisible();
  },
};

export const BeforePreparationHasNoInventedQualification: Story = {
  args: { context: undefined, attestation: undefined },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Coverage is not prepared yet.')).toBeVisible();
    await expect(canvas.queryByText('Recorded at preparation', { exact: false })).not.toBeInTheDocument();
  },
};
