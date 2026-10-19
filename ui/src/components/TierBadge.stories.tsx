import type { Meta, StoryObj } from '@storybook/react-vite';
import { TierBadge } from './TierBadge';

const meta = {
  title: 'Atlas/TierBadge',
  component: TierBadge,
} satisfies Meta<typeof TierBadge>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Confirmed: Story = { args: { tier: 'Confirmed' } };
export const InferredStrong: Story = { args: { tier: 'InferredStrong' } };
export const InferredWeak: Story = { args: { tier: 'InferredWeak' } };
export const Gap: Story = { args: { tier: 'Gap' } };

// AC-0244: the producing method is a separate label beside the tier.
export const RuleDerivedSemantic: Story = {
  args: { tier: 'InferredWeak', provenance: { method: { kind: 'rule', rule: 'projection.api-catalog', version: '1' } } },
};
export const ModelAfterAcceptance: Story = {
  args: { tier: 'InferredWeak', provenance: { method: { kind: 'model', model: 'local-llm' } } },
};
export const LegacyUnrecorded: Story = { args: { tier: 'Confirmed', provenance: {} } };
