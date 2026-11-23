import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, within } from 'storybook/test';
import type { RepoCoverage } from '../coverage';
import { CoverageInventory } from './CoverageInventory';

/** The inventory on its own (#1769). WorkspaceSurface's stories cover its
 *  placement before the investigation entry (AC-0238), refused plugin files
 *  (AC-0658) and the packet success/empty/conflict paths (AC-0371). */

const LIMITED: RepoCoverage = {
  inventory: 'coverage@1',
  repo: 'acme/api',
  commit: '0123456789abcdef',
  layer_hints: ['server'],
  producers: [
    {
      id: 't0.adapter-go',
      kind: 'deterministic',
      covers: 'Go',
      state: 'ran',
      files: 7,
      nodes: 21,
      edges: 14,
      inputs: [],
      stopping_reason: 'completed over 7 files of acme/api@0123456789abcdef for layers server',
    },
    {
      id: 't0.adapter-python',
      kind: 'deterministic',
      covers: 'Python',
      state: 'not_applicable',
      files: 0,
      nodes: 0,
      edges: 0,
      inputs: [],
      stopping_reason: 'no Python files in acme/api@0123456789abcdef',
    },
  ],
  missing_inputs: [
    {
      input: 'terraform.tfstate',
      producer: 't1.tfstate',
      reason: 'no state file was provided for acme/api',
    },
  ],
  unsupported: [
    {
      capability: 'Ruby',
      occurrences: 2,
      first_site: 'lib/a.rb:1',
      reason: 'no installed producer covers Ruby: a tool limitation, not a system gap',
    },
  ],
  limited: true,
};

const SCOPED: RepoCoverage = {
  inventory: 'coverage@1',
  repo: 'acme/web',
  commit: 'fedcba9876543210',
  commit_note: 'working directory',
  layer_hints: [],
  producers: [
    {
      id: 't0.adapter-ts',
      kind: 'deterministic',
      covers: 'TypeScript',
      state: 'ran',
      files: 12,
      nodes: 40,
      edges: 31,
      inputs: [],
      stopping_reason: 'completed over 12 files of acme/web@fedcba9876543210',
    },
  ],
  missing_inputs: [],
  unsupported: [],
  limited: false,
};

const meta = {
  title: 'Surfaces/CoverageInventory',
  component: CoverageInventory,
  args: { coverage: [LIMITED, SCOPED] },
  beforeEach: () => () => clearMocks(),
} satisfies Meta<typeof CoverageInventory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LimitedRepoOpensScopedRepoCollapsed: Story = {
  // AC-0238: a limited repo opens to its producers, missing inputs and
  // unsupported capabilities; a repo complete within its scope stays closed
  // until asked and states that verdict.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const inventory = canvas.getByRole('region', { name: 'Coverage inventory' });
    const [limited, scoped] = Array.from(
      inventory.querySelectorAll<HTMLDetailsElement>('details.coverage-repo'),
    );
    await expect(limited.open).toBe(true);
    await expect(limited).toHaveTextContent(
      'Limited coverage · 1 producer ran · 1 missing input · 1 unsupported capability',
    );
    await expect(limited).toHaveTextContent('Nothing to read for: Python.');
    await expect(limited).toHaveTextContent(
      'terraform.tfstate: no state file was provided for acme/api',
    );
    await expect(limited).toHaveTextContent('Ruby ×2, first at lib/a.rb:1');

    await expect(scoped.open).toBe(false);
    await expect(scoped).toHaveTextContent(
      'acme/web @fedcba987654 (working directory) Complete within the declared scope only · 1 producer ran',
    );
    await userEvent.click(within(scoped).getByText('acme/web'));
    await expect(scoped.open).toBe(true);
    await expect(scoped).toHaveTextContent('Ran t0.adapter-ts (TypeScript)');
  },
};

export const DraftPacketErrorIsStatedInPlainWords: Story = {
  // AC-0371: a typed host refusal is shown as words, not a code, and only on
  // the repo it was drafted for.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const seen: string[] = [];
    mockIPC((command, args) => {
      if (command === 'draft_adapter_work_packets') {
        seen.push(String((args as { repo?: string })?.repo));
        throw { code: 'unpinned_revision' };
      }
      throw new Error(`unexpected command: ${command}`);
    });
    const [limitedButton] = canvas.getAllByRole('button', { name: 'Draft adapter work packet' });
    await userEvent.click(limitedButton);
    await expect(seen).toEqual(['acme/api']);
    const alert = await canvas.findByRole('alert');
    await expect(alert).toHaveTextContent(
      'This repo was recovered from a working directory, not a clean git checkout',
    );
    await expect(alert).not.toHaveTextContent('unpinned_revision');
    await expect(canvas.getAllByRole('alert')).toHaveLength(1);
    await expect(limitedButton).toBeEnabled();
  },
};

export const SaveDraftedPacketForReview: Story = {
  // AC-1350: a drafted adapter packet is saved for review by its content id,
  // once, and the saved packet id is shown.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const seen: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'draft_adapter_work_packets') {
        return [{ language: 'Ruby', content_id: 'work-packet-v1:' + 'a'.repeat(64), markdown: '# Ruby adapter' }];
      }
      if (command === 'save_adapter_work_packet') {
        seen.push(args);
        return { packet_id: 'work-packet-v1:' + 'a'.repeat(64) };
      }
      throw new Error(`unexpected command: ${command}`);
    });
    const [limitedButton] = canvas.getAllByRole('button', { name: 'Draft adapter work packet' });
    await userEvent.click(limitedButton);
    const save = await canvas.findByRole('button', { name: 'Save for review' });
    await userEvent.click(save);
    await expect(await canvas.findByRole('status')).toHaveTextContent('Saved for review as work-packet-v1:' + 'a'.repeat(64));
    await expect(seen).toEqual([{ repo: 'acme/api', contentId: 'work-packet-v1:' + 'a'.repeat(64) }]);
    await expect(save).toBeDisabled();
  },
};

export const NothingRecordedRendersNothing: Story = {
  args: { coverage: [] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.queryByTestId('coverage-inventory')).not.toBeInTheDocument();
  },
};
