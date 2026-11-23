import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { WorkPackets, WorkPacketTray } from './WorkPackets';
import { useWorkPacketStore } from '../workPacketStore';
import { packetFixture, packetRequestFixture, packetReviewedFixture } from '../workPacketFixtures';

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

const submitted = {
  ...packetFixture, execution: 'submitted' as const,
  dispatch: { reviewed_revision: 1, branch: 'cartograph/packet-0123456789ab', base_revision: 'a'.repeat(40),
    head_revision: 'b'.repeat(40), stop: 'end_turn', changed_paths: ['src/adapter.rs'], changed_paths_omitted: 0, refused_requests: 2 },
};
export const SubmittedDispatch: Story = {
  // AC-1346: a dispatched packet shows its execution state and the local branch it came back on, unpushed.
  args: { state: { ...state, detail: submitted } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('packet-execution')).toHaveTextContent('submitted');
    const dispatch = canvas.getByRole('region', { name: 'Packet dispatch' });
    await expect(dispatch).toHaveTextContent('cartograph/packet-0123456789ab');
    await expect(canvas.getByTestId('packet-dispatch-head')).toHaveTextContent('b'.repeat(40));
    await expect(dispatch).toHaveTextContent('1 changed path · 2 agent requests refused. Not pushed or merged');
    await expect(dispatch).toHaveTextContent('src/adapter.rs');
    await expect(canvas.queryByText('unoffered')).toBeNull();
  },
};

export const DispatchOnlyAReviewedPacket: Story = {
  // AC-1350: Dispatch is offered only on a reviewed, never-dispatched packet.
  args: { state: { ...state, detail: packetReviewedFixture, dispatch: fn(async () => {}) } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const button = canvas.getByRole('button', { name: 'Dispatch to ACP agent' });
    await expect(button).toBeEnabled();
    await userEvent.click(button);
    await expect(args.state.dispatch).toHaveBeenCalledTimes(1);
    await expect(canvas.getByRole('region', { name: 'Packet review' })).toHaveTextContent('nothing is pushed or merged');
  },
};

export const DispatchWithheldBeforeReviewAndAfterDispatch: Story = {
  // AC-1350: a draft packet offers no dispatch; SubmittedPacketOffersNoSecondDispatch covers a dispatched one.
  args: { state: { ...state, dispatch: fn(async () => {}) } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Dispatch to ACP agent' })).toBeDisabled();
  },
};

export const SubmittedPacketOffersNoSecondDispatch: Story = {
  args: { state: { ...state, detail: { ...submitted, review: packetReviewedFixture.review }, dispatch: fn(async () => {}) } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Dispatch to ACP agent' })).toBeDisabled();
  },
};
