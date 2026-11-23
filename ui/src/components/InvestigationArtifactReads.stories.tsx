import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { InvestigationArtifactReads } from './InvestigationArtifactReads';

const content = `spec-artifact-v1:${'b'.repeat(64)}`;
const meta = { title: 'Investigations/ArtifactReads', component: InvestigationArtifactReads,
  args: { error: null, reads: [
    { artifact_id: 'topology', title: 'Topology', content_id: content, offset: 0, next_offset: 4096, response_bytes: 4300 },
    { artifact_id: 'topology', title: 'Topology', content_id: content, offset: 4096, next_offset: null, response_bytes: 1200 },
  ] },
} satisfies Meta<typeof InvestigationArtifactReads>;
export default meta;
type Story = StoryObj<typeof meta>;

export const SavedPagesInLedgerOrder: Story = {
  // AC-1816: each saved page names its document and exact span, and the
  // section says pages are context, not citations or today's document.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const items = canvas.getAllByRole('listitem');
    await expect(items).toHaveLength(2);
    await expect(items[0]).toHaveTextContent('Topology · bytes 0–4,096, continues');
    await expect(items[1]).toHaveTextContent('Topology · from byte 4,096 to the end');
    await expect(canvas.getByText(/not citations/)).toBeVisible();
  },
};

export const NoReads: Story = {
  args: { reads: [] },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText('No compiled documents were read.')).toBeVisible();
  },
};

export const LoadError: Story = {
  args: { reads: null, error: 'store unavailable' },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByRole('alert')).toHaveTextContent('store unavailable');
  },
};
