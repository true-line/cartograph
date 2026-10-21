import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import App from './App';
import { useAppStore, type SpecBundle } from './store';
import { useWorkPacketStore } from './workPacketStore';
import { useInvestigationStore } from './investigationStore';
import { usePrimarySourceStore } from './primarySourceStore';
import { packetBundleFixture, packetDecisionFixtures, packetExportFixture, packetFixture, packetGraphFixture, packetRequestFixture, packetReviewedFixture, packetReviewedExportFixture } from './workPacketFixtures';
import { packetFactKey, type GeneratePacketRequest, type PacketDetail } from './workPacketTypes';

let calls: { command: string; args: unknown }[] = [];
let saved: PacketDetail | null = null;
let conflict = false;
let corrupt = false;
let specExportOverride: ((mode: SpecBundle['mode']) => SpecBundle | Promise<SpecBundle>) | null = null;
let lastExport: { packet_id: string | undefined; file_name: string; content: string } | null = null;
const reads = new Set(['list_jobs', 'list_nodes', 'list_flows', 'list_flow_anchors', 'system_contents', 'list_plugins', 'list_findings', 'ingest_history', 'extractor_coverage', 'list_evals', 'get_settings', 'escalation_capabilities']);
function installPacketCore() {
  calls = []; saved = null; conflict = false; corrupt = false; lastExport = null; specExportOverride = null;
  useWorkPacketStore.setState(useWorkPacketStore.getInitialState(), true);
  usePrimarySourceStore.setState(usePrimarySourceStore.getInitialState(), true);
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  useAppStore.setState({ ...useAppStore.getInitialState(), view: 'atlas', specMode: packetBundleFixture.mode }, true);
  mockIPC((command, args) => {
    calls.push({ command, args });
    if (command.startsWith('plugin:event|')) return 1;
    if (reads.has(command)) return [];
    switch (command) {
      case 'ping': return { app: 'cartograph', version: 'fixture' };
      case 'graph_stats': return { nodes: packetGraphFixture.nodes.length, edges: packetGraphFixture.edges.length };
      case 'atlas_snapshot': return packetGraphFixture;
      case 'export_spec': return specExportOverride?.((args as { mode: SpecBundle['mode'] }).mode) ?? packetBundleFixture;
      case 'export_spec_with_views': return Promise.resolve(specExportOverride?.((args as { mode: SpecBundle['mode'] }).mode) ?? packetBundleFixture).then((bundle) => ({ bundle, views: null }));
      case 'list_assertion_decisions': return packetDecisionFixtures;
      case 'describe_captured_source': return null;
      case 'prepare_work_packet_selection': {
        const selected = args as { facts: Parameters<typeof packetFactKey>[0][]; interpretationDecisions: string[] };
        const facts = packetRequestFixture.facts.filter((item) => selected.facts.some((fact) => packetFactKey(fact) === packetFactKey(item.reference)));
        if (facts.length !== new Set(selected.facts.map(packetFactKey)).size) throw { code: 'selection_conflict' };
        return { snapshot_id: packetRequestFixture.expected_snapshot_id, facts,
          interpretation_decisions: packetRequestFixture.interpretation_decisions.filter((item) => selected.interpretationDecisions.includes(item.content_hash)) };
      }
      case 'generate_work_packet': {
        const request = JSON.parse((args as { requestJson: string }).requestJson) as GeneratePacketRequest;
        expect({ ...request, supersedes: request.supersedes ?? [] }).toEqual({ ...packetRequestFixture, supersedes: packetRequestFixture.supersedes ?? [] });
        saved = structuredClone(packetFixture); return saved;
      }
      case 'list_work_packets': return saved ? [{ packet_id: saved.packet_id, objective: saved.body.intent.objective, review: saved.review, execution: 'unoffered' }] : [];
      case 'get_work_packet': if (corrupt) throw { code: 'invalid_input' }; return saved;
      case 'review_work_packet': {
        const request = (args as { request: { packet_id: string; expected_revision: number; decision: 'reviewed' | 'changes_requested'; note?: string } }).request;
        expect(request.packet_id).toBe(saved?.packet_id);
        expect(request.expected_revision).toBe(saved?.review.revision);
        if (!saved) throw { code: 'invalid_input' };
        if (conflict) {
          saved = { ...saved, review: { status: 'changes_requested', revision: saved.review.revision + 1, note: 'Concurrent review retained.' } };
          throw { code: 'review_conflict', current_review: saved.review };
        }
        expect(request.decision).toBe('reviewed');
        expect(request.note).toBe('Locally reviewed; execution remains unoffered.');
        saved = structuredClone(packetReviewedFixture); return saved;
      }
      case 'assess_work_packet': return saved;
      case 'export_work_packet': lastExport = { packet_id: saved?.packet_id, file_name: `cartograph-work-packet-${saved?.packet_id.split(':')[1]}.md`,
        content: saved?.review.status === 'reviewed' ? packetReviewedExportFixture : packetExportFixture }; return lastExport;
      case 'export_topology': case 'export_flows': case 'adapter_inventory': case 'findings_summary': case 'egress_summary': case 'get_ingest_parallelism': case 'cloud_disclosure': return null;
      default: throw new Error(`Unexpected packet product boundary: ${command}`);
    }
  });
  return () => clearMocks();
}
const meta = { title: 'Work packets/App product route', component: App, beforeEach: installPacketCore } satisfies Meta<typeof App>;
export default meta;
type Story = StoryObj<typeof meta>;

async function selectAndGenerate(canvasElement: HTMLElement) {
  const canvas = within(canvasElement);
  const node = packetGraphFixture.nodes[0];
  const name = typeof node.props.method === 'string' && typeof node.props.path === 'string' ? `${node.props.method} ${node.props.path}` : typeof node.props.name === 'string' ? node.props.name : node.id;
  await userEvent.click(await canvas.findByRole('button', { name }, { timeout: 5_000 }));
  await userEvent.click(canvas.getByRole('button', { name: 'Add selected fact to packet' }));
  await waitFor(() => expect(useWorkPacketStore.getState().facts).toEqual(packetRequestFixture.facts));
  await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
  await canvas.findByRole('heading', { name: 'Spec Workbench' }, { timeout: 5_000 });
  for (const artifact of packetRequestFixture.artifacts) {
    const preview = packetBundleFixture.artifacts.find((item) => item.id === artifact.id)!;
    await userEvent.click(canvas.getByRole('button', { name: new RegExp(preview.title) }));
    await userEvent.click(canvas.getByRole('button', { name: 'Add artifact to packet' }));
    await waitFor(() => expect(useWorkPacketStore.getState().artifacts).toContainEqual(artifact));
  }
  const decisionButtons = canvas.queryAllByRole('button', { name: 'Add historical decision to packet' });
  for (const button of decisionButtons) { await userEvent.click(button); await waitFor(() => expect(useWorkPacketStore.getState().busy).toBe(false)); }
  await userEvent.click(canvas.getByRole('button', { name: 'Prepare requested intent' }));
  const form = within(canvas.getByRole('form', { name: 'Requested work packet intent' }));
  await userEvent.click(form.getByRole('checkbox', { name: 'Unknown / not supplied: objective' }));
  const objective = packetRequestFixture.intent.objective;
  if (typeof objective === 'object' && objective.state === 'supplied') {
    await userEvent.click(form.getByLabelText('Requested objective'));
    await userEvent.paste(objective.value);
  }
  await userEvent.click(form.getByRole('button', { name: 'Generate and open saved packet' }));
  await waitFor(() => expect(canvas.getByTestId('packet-body-identity')).toHaveTextContent(packetFixture.packet_id));
}

// Seeds the state a later step of the packet flow starts from, through the same mocks and fixtures the full
// select → generate path produces, so each story below stays well inside the 15 s default test budget (#687).
function seedSavedPacket(packet: PacketDetail) {
  return () => {
    saved = structuredClone(packet);
    useWorkPacketStore.setState({ detail: structuredClone(packet) });
    useAppStore.setState({ view: 'packets' });
  };
}

async function expectPacketBody(canvasElement: HTMLElement) {
  const canvas = within(canvasElement);
  for (const group of ['identity', 'intent', 'evidence', 'work', 'acceptance', 'handoff', 'return']) await expect(canvas.getByRole('region', { name: `Packet ${group}` })).toBeInTheDocument();
  const evidence = within(canvas.getByRole('region', { name: 'Packet evidence' }));
  await expect(evidence.getAllByText('rejected').length).toBeGreaterThan(0);
  await expect(evidence.getAllByText('true').length).toBeGreaterThan(0);
  const objective = packetRequestFixture.intent.objective;
  if (objective.state === 'supplied') await expect(within(canvas.getByRole('region', { name: 'Packet intent' })).getByText(objective.value)).toBeInTheDocument();
}

async function expectNoAuthorityOrActiveContent(canvasElement: HTMLElement) {
  await expect(calls.some((call) => /publish|provider|shell|target_write|grant_cloud|start_investigation/.test(call.command))).toBe(false);
  await expect(canvasElement.querySelector('.packet-detail script, .packet-detail iframe, .packet-detail a')).toBeNull();
}

export const SelectAndGeneratePacket: Story = {
  // AC-0252: the mock responses are real PacketStore fixture outputs; selection and generation run through the UI once.
  play: async ({ canvasElement }) => {
    await selectAndGenerate(canvasElement);
    await expectPacketBody(canvasElement);
    await expect(useWorkPacketStore.getState().detail).toEqual(packetFixture);
    await expect(calls.filter((call) => call.command === 'generate_work_packet')).toHaveLength(1);
    await expectNoAuthorityOrActiveContent(canvasElement);
  },
};

export const ReviewAndReopenSavedPacket: Story = {
  // AC-0252: starts from the saved fixture packet that generation returns; the complete body survives review and reopening.
  beforeEach: seedSavedPacket(packetFixture),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByTestId('packet-body-identity', undefined, { timeout: 5_000 });
    await expect(useWorkPacketStore.getState().detail).toEqual(packetFixture);
    // The complete body survives reopening; no regeneration is used by list/get.
    await userEvent.click(canvas.getByLabelText('Review note'));
    await userEvent.paste('Locally reviewed; execution remains unoffered.');
    await userEvent.click(canvas.getByRole('button', { name: 'Mark reviewed' }));
    await waitFor(() => expect(useWorkPacketStore.getState().detail?.review).toMatchObject({ status: 'reviewed', revision: 1 }));
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh saved packets' }));
    await waitFor(() => expect(canvas.getByRole('navigation', { name: 'Saved work packets' }).querySelector('button')).not.toBeNull());
    await userEvent.click(within(canvas.getByRole('navigation', { name: 'Saved work packets' })).getByRole('button'));
    await waitFor(() => expect(useWorkPacketStore.getState().detail?.body).toEqual(packetFixture.body));
    await expect(useWorkPacketStore.getState().detail?.freshness).toEqual(packetFixture.freshness);
    await expect(useWorkPacketStore.getState().detail?.execution).toBe('unoffered');
    await expect(useWorkPacketStore.getState().detail).toEqual(packetReviewedFixture);
    await expect(calls.filter((call) => call.command === 'generate_work_packet')).toHaveLength(0);
    await expectNoAuthorityOrActiveContent(canvasElement);
  },
};

export const ExportReviewedPacket: Story = {
  // AC-0252: the mock export is the exact Markdown PacketStore emits for the reviewed fixture packet.
  beforeEach: seedSavedPacket(packetReviewedFixture),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByTestId('packet-body-identity', undefined, { timeout: 5_000 });
    await userEvent.click(canvas.getByRole('button', { name: 'Export packet Markdown' }));
    await waitFor(() => expect(canvas.getByText('Packet export prepared; local download requested.')).toBeInTheDocument());
    await expect(lastExport).toEqual({ packet_id: packetFixture.packet_id, file_name: `cartograph-work-packet-${packetFixture.packet_id.split(':')[1]}.md`, content: packetReviewedExportFixture });
    await expect(packetReviewedExportFixture).toContain(packetFixture.packet_id);
    await expectPacketBody(canvasElement);
    await expect(calls.filter((call) => call.command === 'generate_work_packet')).toHaveLength(0);
    await expectNoAuthorityOrActiveContent(canvasElement);
  },
};

export const ReviewConflictAndCorruptReopen: Story = {
  // AC-0250/0252: conflict reloads the winning revision; a rejected stored body never replaces it.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await selectAndGenerate(canvasElement); conflict = true;
    await userEvent.click(canvas.getByRole('button', { name: 'Mark reviewed' }));
    await waitFor(() => expect(canvas.getAllByRole('alert')[0]).toHaveTextContent(/Review conflict.*revision 1/));
    await expect(useWorkPacketStore.getState().detail?.review).toMatchObject({ status: 'changes_requested', revision: 1 });
    await expect(useWorkPacketStore.getState().detail?.body).toEqual(packetFixture.body);
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh saved packets' }));
    await waitFor(() => expect(canvas.getByRole('navigation', { name: 'Saved work packets' }).querySelector('button')).not.toBeNull());
    corrupt = true;
    await userEvent.click(within(canvas.getByRole('navigation', { name: 'Saved work packets' })).getByRole('button'));
    await waitFor(() => expect(canvas.getAllByRole('alert')[0]).toHaveTextContent(/Packet input is invalid/));
    await expect(useWorkPacketStore.getState().detail?.body).toEqual(packetFixture.body);
  },
};

export const ArtifactSelectionUsesRenderedModeAfterRacingRequests: Story = {
  // AC-0248: delayed mode responses never rebind the artifact shown in the workbench.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
    await canvas.findByRole('heading', { name: 'Spec Workbench' }, { timeout: 5_000 });
    const renderedMode = packetBundleFixture.mode;
    const otherMode = renderedMode === 'verified-only' ? 'best-effort' : 'verified-only';
    const deferred = () => {
      let resolve!: (value: SpecBundle) => void;
      const promise = new Promise<SpecBundle>((success) => { resolve = success; });
      return { promise, resolve };
    };
    let older = deferred(), latest = deferred();
    specExportOverride = (mode) => mode === otherMode ? older.promise : latest.promise;
    let first = useAppStore.getState().setSpecMode(otherMode);
    let second = useAppStore.getState().setSpecMode(renderedMode);
    older.resolve({ ...packetBundleFixture, mode: otherMode }); await first;
    await expect(useAppStore.getState().specBusy).toBe(true);
    await expect(canvas.getByRole('button', { name: 'Add artifact to packet' })).toBeDisabled();
    latest.resolve(packetBundleFixture); await second;
    // Reverse completion order as well: an obsolete successful export cannot win last.
    older = deferred(); latest = deferred();
    first = useAppStore.getState().setSpecMode(otherMode);
    second = useAppStore.getState().setSpecMode(renderedMode);
    latest.resolve(packetBundleFixture); await second;
    older.resolve({ ...packetBundleFixture, mode: otherMode }); await first;
    await expect(useAppStore.getState().specBundle?.mode).toBe(renderedMode);
    // A failed current request retains the prior rendered bundle. Its actual mode is authoritative.
    specExportOverride = () => Promise.reject('The requested mode could not be compiled.');
    await useAppStore.getState().setSpecMode(otherMode);
    await expect(useAppStore.getState().specMode).toBe(otherMode);
    await expect(useAppStore.getState().specBundle?.mode).toBe(renderedMode);
    await userEvent.click(canvas.getByRole('button', { name: 'Add artifact to packet' }));
    await waitFor(() => expect(useWorkPacketStore.getState().artifacts).toEqual([{
      id: packetBundleFixture.artifacts[0].id, schema: packetBundleFixture.artifacts[0].schema,
      content_id: packetBundleFixture.artifacts[0].content_id, mode: renderedMode,
    }]));
  },
};
