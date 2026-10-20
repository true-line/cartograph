import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { WorkPackets, WorkPacketTray } from './WorkPackets';
import { useWorkPacketStore } from '../workPacketStore';
import { packetFixture, packetRequestFixture } from '../workPacketFixtures';

const state = { ...useWorkPacketStore.getInitialState(), snapshotId: packetRequestFixture.expected_snapshot_id,
  facts: packetRequestFixture.facts, artifacts: packetRequestFixture.artifacts, decisions: packetRequestFixture.interpretation_decisions,
  detail: packetFixture, history: [{ packet_id: packetFixture.packet_id, review: packetFixture.review }],
  load: fn(async () => {}), removeFact: fn(), removeArtifact: fn(), removeDecision: fn(), clearTray: fn() };
const meta = { title: 'Work packets/Saved packet', component: WorkPackets, args: { state } } satisfies Meta<typeof WorkPackets>;
export default meta;
type Story = StoryObj<typeof meta>;

export const CompleteSavedPresentation: Story = {
  // AC-0249/0252: seven complete groups share the exact saved fixture identity and explicit omissions.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('packet-body-identity')).toHaveTextContent(packetFixture.packet_id);
    for (const group of ['identity', 'intent', 'evidence', 'work', 'acceptance', 'handoff', 'return']) {
      await expect(canvas.getByRole('region', { name: `Packet ${group}` })).toBeInTheDocument();
    }
    await expect(canvas.getAllByText('Unknown / missing').length).toBeGreaterThan(10);
    await expect(canvas.getByText('producer input/configuration closure is not established')).toBeInTheDocument();
    await expect(canvasElement.querySelector('.packet-detail a, .packet-detail script, .packet-detail iframe')).toBeNull();
  },
};

export const RemovableSelectionTray: Story = {
  render: ({ state }) => <WorkPacketTray state={state} onOpen={fn()} />,
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('complementary', { name: 'Work packet selection' })).toHaveTextContent(packetRequestFixture.expected_snapshot_id);
    const remove = canvas.getAllByRole('button', { name: /^Remove fact/ })[0];
    await userEvent.click(remove);
    await expect(args.state.removeFact).toHaveBeenCalledTimes(1);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear selection' }));
    await expect(args.state.clearTray).toHaveBeenCalledTimes(1);
  },
};
