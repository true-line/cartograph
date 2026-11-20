import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, userEvent, within } from 'storybook/test';
import { ArchitectureCanvas } from './ArchitectureCanvas';
import { ARCHITECTURE_FIXTURE } from '../architectureFixture';

// #2049 slice 2: C4 levels 1 to 3 of the recovered system, each box and
// arrow backed by the facts it rests on.
const ARTIFACT = {
  id: ARCHITECTURE_FIXTURE.artifact_id,
  schema: ARCHITECTURE_FIXTURE.schema,
  content_id: ARCHITECTURE_FIXTURE.content_id,
};

const meta = {
  title: 'Spec/ArchitectureCanvas',
  component: ArchitectureCanvas,
  args: { view: ARCHITECTURE_FIXTURE, artifact: ARTIFACT },
} satisfies Meta<typeof ArchitectureCanvas>;

export default meta;
type Story = StoryObj<typeof meta>;

export const SystemContext: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('tab', { name: /System context/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    // R-INT-2: the diagram itself says how inferred boxes and arrows are drawn.
    await expect(canvas.getByTestId('architecture-legend')).toHaveTextContent('Inferred (weak)');
    await userEvent.click(canvas.getByRole('button', { name: 'SimpleLogin' }));
    await expect(canvas.getByRole('button', { name: 'Show containers' })).toBeVisible();
  },
};

export const ContainersWithEvidence: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('tab', { name: /Containers/ }));
    await userEvent.click(canvas.getByRole('button', { name: 'Web app' }));
    const evidence = canvas.getByTestId('architecture-node-evidence');
    await expect(evidence).toHaveTextContent('server.py bytes 120..480');
    await expect(evidence).toHaveTextContent('39 more facts in architecture.md');
    await userEvent.click(within(evidence).getByRole('button', { name: 'Show components' }));
    await expect(canvas.getByRole('tab', { name: /Components/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(canvas.getByRole('button', { name: 'Billing' })).toBeVisible();
  },
};

export const GapIsShownAsGap: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Mail relay' }));
    await expect(canvas.getByTestId('architecture-node-evidence')).toHaveTextContent(
      'No fact recovered: shown as a Gap.',
    );
  },
};

export const IdentityMismatchRefused: Story = {
  args: { artifact: { ...ARTIFACT, content_id: 'sha256:other' } },
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).getByTestId('architecture-identity-mismatch'),
    ).toBeVisible();
  },
};
