import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { groupGapClasses } from '../gapClasses';
import { GapsDriftSurface } from './GapsDriftSurface';
import type { Provenance, RegisterFinding, SpecAssertion } from '../store';

function gapProvenance(tier: string): Provenance {
  return {
    tier,
    confidence_tier: 'Gap',
    evidence: [
      {
        repo: 'local/image-trail',
        path: 'src/background.ts',
        byte_start: 100,
        byte_end: 180,
        commit_sha: 'workdir',
      },
    ],
    extractor_id: 't0.adapter-ts',
    content_hash: 'c'.repeat(64),
  };
}

/** The handoff's three example gaps (screenshot 05). */
const GAPS: SpecAssertion[] = [
  {
    id: 'node:gap:executeScript',
    subject_id: 'gap:executeScript',
    subject_kind: 'Gap',
    summary: 'chrome.scripting.executeScript — injected function body not linkable',
    provenance: gapProvenance('Deterministic'),
  },
  {
    id: 'node:gap:sync-host',
    subject_id: 'gap:sync-host',
    subject_kind: 'Gap',
    summary: 'Remote sync target — endpoint host computed from config at runtime',
    provenance: gapProvenance('Dynamic'),
  },
  {
    id: 'node:gap:offscreen',
    subject_id: 'gap:offscreen',
    subject_kind: 'Gap',
    summary: 'Offscreen document lifecycle — teardown path not statically provable',
    provenance: gapProvenance('Semantic'),
  },
];

// Raw registers restate findings in other shapes: a flow-hop assertion
// repeats a gap inside its flow; CONFLICTS edges support a drift node.
// The tally counts neither — the surface must filter them out (#137 review).
const FLOW_HOP_RESTATEMENT: SpecAssertion = {
  id: 'flow:ep:capture:1:CALLS:sym:sync',
  subject_id: 'sym:capture CALLS sym:sync',
  subject_kind: 'FlowHop',
  summary: 'CALLS: capture → sync (unresolved)',
  provenance: gapProvenance('Deterministic'),
};

const DRIFT_SUPPORT_EDGE: SpecAssertion = {
  id: 'edge:adr:sync CONFLICTS ch:events',
  subject_id: 'adr:sync CONFLICTS ch:events',
  subject_kind: 'CONFLICTS',
  summary: 'adr:sync CONFLICTS ch:events',
  provenance: { ...gapProvenance('Deterministic'), confidence_tier: 'InferredStrong' },
};

const DRIFT: SpecAssertion[] = [
  {
    id: 'node:drift:sync-batching',
    subject_id: 'drift:sync-batching',
    subject_kind: 'Drift',
    summary: 'ADR-0002 declares batched sync; observed per-event POST on capture',
    provenance: { ...gapProvenance('Deterministic'), confidence_tier: 'InferredStrong' },
  },
  DRIFT_SUPPORT_EDGE,
];

const REGISTER_FINDINGS: RegisterFinding[] = [
  {
    id: 1,
    kind: 'unsupported',
    detector: 'preflight@1',
    repo: 'local/image-trail',
    path: 'src/wasm/filter.wasm',
    line: 1,
    message: 'WebAssembly image filter module — no WASM adapter',
    created_at: '2026-07-14T10:00:00Z',
  },
  {
    id: 2,
    kind: 'unsupported',
    detector: 'preflight@1',
    repo: 'local/image-trail',
    path: 'src/legacy.js',
    line: 120,
    message: 'Inline eval() — dynamic code construction',
    created_at: '2026-07-14T10:00:00Z',
  },
  {
    id: 3,
    kind: 'no-evidence',
    detector: 'preflight@1',
    repo: 'local/image-trail',
    path: 'README.md',
    line: 1,
    message: 'Retention policy for captured frames — no evidence in code or config',
    created_at: '2026-07-14T10:00:00Z',
  },
];

const meta = {
  title: 'Surfaces/GapsDriftSurface',
  component: GapsDriftSurface,
  args: {
    summary: {
      gaps: 3,
      unsupported: 2,
      no_evidence: 1,
      drift: 1,
      open_findings: 6,
      graph_facts: 134,
    },
    gaps: [...GAPS, FLOW_HOP_RESTATEMENT],
    drift: DRIFT,
    registerFindings: REGISTER_FINDINGS,
    onOpenGap: fn(),
  },
} satisfies Meta<typeof GapsDriftSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ThreeLanesNeverConflate: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Header tally quotes the shared register summary — the same numbers
    // the lanes below add up to.
    await expect(canvas.getByText('6 open findings')).toBeInTheDocument();
    await expect(canvas.getByText('3 gaps')).toBeInTheDocument();
    await expect(canvas.getByText('2 unsupported')).toBeInTheDocument();

    // Lane headers reconcile with the tally — the raw register's flow-hop
    // restatement (a 4th assertion) is filtered out, not double-counted.
    await expect(canvas.getByText('System gaps · 3')).toBeInTheDocument();
    // #164: each register term explains itself in place.
    await expect(canvas.getAllByRole('button', { name: 'What is system gap?' })).toHaveLength(2);
    await expect(
      canvas.getByRole('button', { name: 'What is unsupported pattern?' }),
    ).toBeInTheDocument();
    await expect(
      canvas.getByRole('button', { name: 'What is no-evidence question?' }),
    ).toBeInTheDocument();
    await expect(canvas.queryByText(/capture → sync \(unresolved\)/)).not.toBeInTheDocument();
    await expect(canvas.getByText('Unsupported patterns · 2')).toBeInTheDocument();
    await expect(canvas.getByText('No evidence found · 1')).toBeInTheDocument();

    // Gaps get ids, a next-tier tail, and open their resolution seam.
    await expect(canvas.getByText('G-01')).toBeInTheDocument();
    await expect(canvas.getByText('T1 next')).toBeInTheDocument();
    await userEvent.click(
      canvas.getByText(/executeScript — injected function body not linkable/),
    );
    await expect(args.onOpenGap).toHaveBeenCalledWith(GAPS[0]);

    // Unsupported rows are explicitly tool limitations, not gaps — static
    // rows with a file tail, and the lane says so.
    await expect(canvas.getByText(/a tool limitation, not a System Gap/)).toBeInTheDocument();
    await expect(canvas.getByText('src/wasm/filter.wasm:1')).toBeInTheDocument();
    // No-evidence absence is stated, not implied.
    await expect(
      canvas.getByText(/Retention policy for captured frames/),
    ).toBeInTheDocument();
  },
};

export const EscalationTierGrouping: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('tab', { name: 'By escalation tier' }));

    // Gaps group under the next ladder rung derived from the tier that
    // established them: Deterministic→T1, Dynamic→T2, Semantic→T3 —
    // one gap under each.
    await expect(canvas.getByText('T1', { selector: 'code' })).toBeInTheDocument();
    await expect(canvas.getAllByText(/next escalation · 1 open/)).toHaveLength(3);
    const t1Section = canvas.getByText('T1', { selector: 'code' })
      .closest('div')?.parentElement as HTMLElement;
    await expect(
      within(t1Section).getByText(/executeScript/),
    ).toBeInTheDocument();

    // The integrity rule is stated on the surface.
    await expect(
      canvas.getByText(/T2\/T3 escalations propose only — they never overwrite T0\/T1/),
    ).toBeInTheDocument();

    // Rows stay actionable in this projection too.
    await userEvent.click(canvas.getByText(/endpoint host computed from config/));
    await expect(args.onOpenGap).toHaveBeenCalledWith(GAPS[1]);
  },
};

export const DriftTab: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('tab', { name: 'Drift' }));
    // The headline counts drift nodes only — the CONFLICTS support edge in
    // the raw register is the same finding, not a second one.
    await expect(canvas.getByText('ADR / code drift · 1')).toBeInTheDocument();
    await expect(
      canvas.getByText(/ADR-0002 declares batched sync; observed per-event POST/),
    ).toBeInTheDocument();
    await expect(canvas.getByText('drift:sync-batching')).toBeInTheDocument();
    await expect(
      canvas.queryByText('adr:sync CONFLICTS ch:events'),
    ).not.toBeInTheDocument();
  },
};

/** A real-scale register shape: hundreds of instances, a handful of causes. */
const SCALE_GAPS: SpecAssertion[] = [
  ...Array.from({ length: 300 }, (_, index) => ({
    id: `node:gap:msg-${index}`,
    subject_id: `gap:msg-${index}`,
    subject_kind: 'Gap',
    summary: 'Gap: runtime-computed message identity',
    provenance: gapProvenance('Deterministic'),
  })),
  ...Array.from({ length: 40 }, (_, index) => ({
    id: `node:gap:call-${index}`,
    subject_id: `gap:call-${index}`,
    subject_kind: 'Gap',
    summary: 'Gap: unresolved Java import target',
    provenance: { ...gapProvenance('Deterministic'), extractor_id: 't0.adapter-java' },
  })),
  // Unresolved placeholder nodes (#237) group by their reason, never by
  // the internal "unresolved Module edge" label.
  ...Array.from({ length: 20 }, (_, index) => ({
    id: `node:mod:pkg.Missing${index}`,
    subject_id: `mod:pkg.Missing${index}`,
    subject_kind: 'Module',
    summary: 'Module: import of a repository package with no unique declaration',
    provenance: { ...gapProvenance('Deterministic'), extractor_id: 't0.adapter-java' },
  })),
  ...Array.from({ length: 5 }, (_, index) => ({
    id: `edge:sym:a${index} CALLS gap:x${index}`,
    subject_id: `sym:a${index} CALLS gap:x${index}`,
    subject_kind: 'CALLS',
    summary: `sym:a${index} CALLS gap:x${index}`,
    provenance: gapProvenance('Deterministic'),
  })),
  // Supporting edges of listed gap nodes (#241): folded into their node's
  // finding, never a row or a class of their own.
  ...Array.from({ length: 3 }, (_, index) => ({
    id: `edge:sym:p${index} CALLS gap:msg-${index}`,
    subject_id: `sym:p${index} CALLS gap:msg-${index}`,
    subject_kind: 'CALLS',
    summary: `CALLS: runtime-computed message identity`,
    provenance: gapProvenance('Deterministic'),
  })),
];

export const ClassesGroupAtScale: Story = {
  // #167/AC-0082: at scale the gap lane triages by cause class — grouped
  // deterministically (count desc), expanding to paged instances; the
  // per-gap Resolution Strategy path is unchanged. #241: the 3 supporting
  // edges of listed gap nodes fold into their node's finding (the edge
  // class stays ×5 standalone edge-gaps, never ×8), so nothing is counted
  // or classed twice.
  args: {
    summary: {
      gaps: 365,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 365,
      graph_facts: 40_000,
    },
    gaps: SCALE_GAPS,
    drift: [],
    registerFindings: [],
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // 365 findings read as 4 causes, largest first — the 3 supporting
    // edges fold into the ×300 class's nodes instead of inflating it.
    await expect(canvas.getByText('System gaps · 365 — 4 causes')).toBeInTheDocument();
    const classes = within(canvas.getByLabelText('Gap classes'));
    const heads = classes.getAllByRole('button', { expanded: false });
    await expect(heads).toHaveLength(4);
    await expect(heads[0]).toHaveTextContent('×300');
    await expect(heads[0]).toHaveTextContent('runtime-computed message identity');
    await expect(heads[1]).toHaveTextContent('×40');
    await expect(heads[1]).toHaveTextContent('unresolved Java import target');
    await expect(heads[1]).toHaveTextContent('t0.adapter-java');
    await expect(heads[2]).toHaveTextContent('×20');
    await expect(heads[2]).toHaveTextContent(
      'import of a repository package with no unique declaration',
    );
    await expect(heads[2]).not.toHaveTextContent('unresolved Module edge');
    await expect(heads[3]).toHaveTextContent('×5');
    await expect(heads[3]).toHaveTextContent('unresolved CALLS edge');

    // Grouping is deterministic: shuffled input yields the same class order.
    const shuffledOrder = groupGapClasses([...SCALE_GAPS].reverse()).map((c) => c.key);
    await expect(shuffledOrder).toEqual(groupGapClasses(SCALE_GAPS).map((c) => c.key));

    // Expanding a class pages its instances and keeps the per-gap path.
    await userEvent.click(heads[0]);
    const shown = classes.getAllByRole('button', { name: /runtime-computed message identity/ });
    // 1 head + 50 first-page instances.
    await expect(shown.length).toBe(51);
    await expect(classes.getByRole('button', { name: /Show more \(250 remaining\)/ })).toBeInTheDocument();
    await userEvent.click(shown[1]);
    await expect(args.onOpenGap).toHaveBeenCalled();
  },
};

export const ClassEscalatesAsOneBatch: Story = {
  // AC-0089 (#167): a whole class escalates locally in one run producing
  // per-instance staged proposals with accept/reject; flow-blocking
  // classes rank above larger classes that block nothing.
  args: {
    summary: {
      gaps: 365,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 365,
      graph_facts: 40_000,
    },
    gaps: SCALE_GAPS,
    drift: [],
    registerFindings: [],
    // Every Java-import gap truncates a flow; the 300-member class blocks
    // nothing — the 40-member class must rank first.
    flowGapIds: Array.from({ length: 40 }, (_, index) => `gap:call-${index}`),
    onEscalateClass: fn(async (gapIds: string[]) => ({
      proposals: gapIds.slice(0, 2).map((gapId) => ({
        proposal_id: `proposal:${gapId}`,
        review_revision: 0,
        review_decision: null,
        review_note: null,
        evidence_binding: 'working_tree_unverified' as const,
        context_status: 'awaiting_reconciliation' as const,
        created_at: '2026-09-10T12:00:00Z',
        reviewed_at: null,
        gap_id: gapId,
        source_id: 'sym:App#import',
        target_id: 'sym:util#helper',
        edge_label: 'CALLS',
        annotation: 'names align',
        basis_hash: `basis-${gapId}`,
        provenance: { ...gapProvenance('Agentic'), confidence_tier: 'InferredWeak' as const },
      })),
      failures: [{ gap_id: gapIds[2] ?? 'gap:none', error: 'no candidates' }],
      cancelled: false,
    })),
    onDecideProposal: fn(async () => true),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const classes = within(canvas.getByLabelText('Gap classes'));
    const heads = classes.getAllByRole('button', { expanded: false });
    // Flow impact outranks raw count (slice 3).
    await expect(heads[0]).toHaveTextContent('×40');
    await expect(heads[0]).toHaveTextContent('blocks 40 flow hops');
    await expect(heads[1]).toHaveTextContent('×300');

    await userEvent.click(heads[0]);
    await userEvent.click(
      classes.getByRole('button', { name: /Escalate class locally \(40 instances\)/ }),
    );
    await expect(args.onEscalateClass).toHaveBeenCalledTimes(1);
    const status = await canvas.findByRole('status');
    await expect(status).toHaveTextContent('2 staged proposals · 1 failed');

    // Per-instance accept goes through the decision-record path and the
    // row leaves the undecided list.
    const proposals = within(status).getByRole('list', { name: 'Staged class proposals' });
    const accepts = within(proposals).getAllByRole('button', { name: 'Accept' });
    await expect(accepts).toHaveLength(2);
    await userEvent.click(accepts[0]);
    await expect(args.onDecideProposal).toHaveBeenCalledWith(
      expect.objectContaining({ gap_id: 'gap:call-0' }),
      'accepted',
    );
    await waitFor(() =>
      expect(within(status).getAllByRole('button', { name: 'Accept' })).toHaveLength(1),
    );

    // An edge-gap class cannot batch-escalate — the runner assembles tasks
    // from real Gap nodes only, so the button never renders there.
    const edgeHead = classes.getByRole('button', { name: /unresolved CALLS edge/ });
    await userEvent.click(edgeHead);
    await expect(
      classes.queryByRole('button', { name: /Escalate class locally \(5 instances\)/ }),
    ).not.toBeInTheDocument();
    await expect(within(status).getByRole('list', { name: 'Failed instances' })).toHaveTextContent(
      'gap:call-10: no candidates',
    );
  },
};

export const CleanRegister: Story = {
  args: {
    summary: {
      gaps: 0,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 0,
      graph_facts: 134,
    },
    gaps: [],
    drift: [],
    registerFindings: [],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Empty lanes state their emptiness — absence is a claim, not a blank.
    await expect(canvas.getByText('No unresolved facts.')).toBeInTheDocument();
    await expect(canvas.getByText('None detected.')).toBeInTheDocument();
    await expect(canvas.getByText('None recorded.')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('tab', { name: 'Drift' }));
    await expect(canvas.getByText('No ADR/code conflicts recovered.')).toBeInTheDocument();
  },
};

export const FailedBatchReviewKeepsProposalPending: Story = {
  // AC-0129: a failed host write keeps the instance reviewable and displays the failure.
  args: {
    ...ClassEscalatesAsOneBatch.args,
    onDecideProposal: fn(async () => false),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const classes = within(canvas.getByLabelText('Gap classes'));
    await userEvent.click(classes.getAllByRole('button', { expanded: false })[0]);
    await userEvent.click(classes.getByRole('button', { name: /Escalate class locally \(40 instances\)/ }));
    const proposals = within(await canvas.findByRole('list', { name: 'Staged class proposals' }));
    await expect(proposals.getAllByText('Inferred (weak)')).toHaveLength(2);
    await userEvent.click(proposals.getAllByRole('button', { name: 'Accept' })[0]);
    await expect(args.onDecideProposal).toHaveBeenCalledTimes(1);
    await expect(await canvas.findByRole('alert')).toHaveTextContent('Review was not saved');
    await expect(proposals.getAllByRole('button', { name: 'Accept' })).toHaveLength(2);
    await expect(proposals.getAllByRole('button', { name: 'Accept' })[0]).toBeEnabled();
  },
};

export const InterpretationBacklogIsSeparateFromSystemGaps: Story = {
  // AC-0359 (#994): established interpretation-seam omissions render in their
  // own lane while adapter omissions and unclassified fallbacks stay in
  // System gaps, with nothing suppressed.
  args: {
    summary: {
      gaps: 5,
      interpretation_backlog: 2,
      unsupported: 2,
      no_evidence: 1,
      drift: 1,
      open_findings: 8,
      graph_facts: 134,
    },
    gaps: [
      {
        id: 'node:gap:return-interp',
        subject_id: 'gap:return-interp',
        subject_kind: 'Gap',
        summary: "Gap: The caller's interpretation of this local return or throw is unknown.",
        provenance: gapProvenance('Deterministic'),
        omission: {
          class: 'unresolved_relationship',
          seam: 'interpretation',
          detail: "The caller's interpretation of this local return or throw is unknown.",
        },
      },
      {
        id: 'node:gap:rule-incomplete',
        subject_id: 'gap:rule-incomplete',
        subject_kind: 'Gap',
        summary: 'Gap: Source-rule interpretation remains incomplete.',
        provenance: gapProvenance('Deterministic'),
        omission: {
          class: 'unresolved_relationship',
          seam: 'interpretation',
          detail: 'Source-rule interpretation remains incomplete.',
        },
      },
      {
        id: 'node:gap:unclassified-reason',
        subject_id: 'gap:unclassified-reason',
        subject_kind: 'Gap',
        summary: 'Gap: novel custom stop reason',
        provenance: gapProvenance('Deterministic'),
        omission: {
          class: 'unresolved_relationship',
          seam: 'interpretation',
          detail: 'unclassified: novel custom stop reason',
        },
      },
      {
        id: 'node:gap:unresolved-call',
        subject_id: 'gap:unresolved-call',
        subject_kind: 'Gap',
        summary: 'Gap: unresolved call target',
        provenance: gapProvenance('Deterministic'),
        omission: {
          class: 'unresolved_relationship',
          seam: 'adapter',
          detail: 'unresolved call target',
        },
      },
      {
        id: 'node:gap:dynamic-dispatch',
        subject_id: 'gap:dynamic-dispatch',
        subject_kind: 'Gap',
        summary: 'Gap: dynamic dispatch - needs type hierarchy',
        provenance: gapProvenance('Deterministic'),
        omission: {
          class: 'unresolved_relationship',
          seam: 'adapter',
          detail: 'dynamic dispatch - needs type hierarchy',
        },
      },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText(/3 system gaps, 2 interpretation backlog/),
    ).toBeInTheDocument();
    await expect(canvas.getByText('System gaps · 3')).toBeInTheDocument();
    await expect(canvas.getByText('Interpretation backlog · 2')).toBeInTheDocument();

    const systemLane = canvas.getByText('System gaps · 3').closest('.register-lane') as HTMLElement;
    const backlogLane = canvas
      .getByText('Interpretation backlog · 2')
      .closest('.register-lane') as HTMLElement;
    const systemRows = systemLane.nextElementSibling as HTMLElement;
    const backlogRows = backlogLane.nextElementSibling as HTMLElement;

    await expect(within(systemRows).getByText(/novel custom stop reason/)).toBeInTheDocument();
    await expect(within(systemRows).getByText(/unresolved call target/)).toBeInTheDocument();
    await expect(within(systemRows).getByText(/dynamic dispatch/)).toBeInTheDocument();
    await expect(within(backlogRows).queryByText(/novel custom stop reason/)).not.toBeInTheDocument();
    await expect(
      within(backlogRows).getByText(
        /The caller's interpretation of this local return or throw is unknown\./,
      ),
    ).toBeInTheDocument();
    await expect(
      within(backlogRows).getByText(/Source-rule interpretation remains incomplete\./),
    ).toBeInTheDocument();
  },
};

// #1601 pilot: in a narrow window a class head's fixed tails (extractor,
// omission chip, tier) took the whole row, so the label shrank to one
// character per line and read as vertical text. The label keeps a readable
// width and the tails wrap below it instead.
export const NarrowClassLabelStaysReadable: Story = {
  decorators: [
    (StoryComponent) => (
      <div style={{ width: 340 }}>
        <StoryComponent />
      </div>
    ),
  ],
  args: {
    summary: {
      gaps: 340,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 340,
      graph_facts: 40_000,
    },
    gaps: SCALE_GAPS.slice(0, 340),
    drift: [],
    registerFindings: [],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const classes = within(canvas.getByLabelText('Gap classes'));
    const heads = classes.getAllByRole('button', { expanded: false });
    for (const head of heads) {
      const label = head.querySelector('.register-text');
      await expect(label).not.toBeNull();
      const box = label!.getBoundingClientRect();
      // At least about 12 characters wide, never a one-character column.
      await expect(box.width).toBeGreaterThanOrEqual(120);
      const lineHeight = parseFloat(getComputedStyle(label!).lineHeight) || 20;
      await expect(box.height).toBeLessThanOrEqual(lineHeight * 3);
    }
  },
};
