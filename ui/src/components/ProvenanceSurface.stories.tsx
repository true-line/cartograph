import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { ProvenanceSurface } from './ProvenanceSurface';
import type { EvalResult, ExtractorCoverage, IngestRecord } from '../store';

const COVERAGE: ExtractorCoverage[] = [
  {
    extractor: 't0.adapter-ts',
    files_in_scope: 14,
    files_with_facts: 12,
    facts: 83,
    coverage_pct: 85.7,
  },
  {
    extractor: 't0.iac-terraform',
    files_in_scope: 4,
    files_with_facts: 4,
    facts: 34,
    coverage_pct: 100,
  },
  {
    extractor: 't1.dynamic',
    files_in_scope: 0,
    files_with_facts: 2,
    facts: 6,
    coverage_pct: null,
  },
];

const EVALS: EvalResult[] = [
  {
    id: 2,
    provider: 'ollama:nomic-embed-text@slm-catalog@1',
    precision_floor: 0.8,
    similarity_threshold: 0.74,
    precision: 0.86,
    recall: 0.79,
    passed: true,
    proposals: 24,
    approved: 19,
  },
  {
    id: 1,
    provider: 'ollama:qwen3:8b@slm-catalog@1',
    precision_floor: 0.75,
    similarity_threshold: 0.7,
    precision: 0.68,
    recall: 0.55,
    passed: false,
    proposals: 12,
    approved: 0,
  },
];

function record(id: number, overrides: Partial<IngestRecord> = {}): IngestRecord {
  return {
    id,
    job_id: id,
    repo: 'collidingscopes/image-trail',
    commit_sha: 'a1b9f30',
    confirmed: 98,
    inferred_strong: 22,
    inferred_weak: 11,
    gap: 3,
    unsupported: 2,
    no_evidence: 1,
    graph_facts: 134,
    content_hash: 'e'.repeat(64),
    created_at: `2026-07-1${id}T10:00:00Z`,
    ...overrides,
  };
}

const meta = {
  title: 'Surfaces/ProvenanceSurface',
  component: ProvenanceSurface,
  args: {
    findings: {
      gaps: 3,
      unsupported: 2,
      no_evidence: 1,
      drift: 1,
      open_findings: 6,
      graph_facts: 134,
    },
    distribution: {
      confirmed: 98,
      inferredStrong: 22,
      inferredWeak: 11,
      gap: 3,
      unattributed: 0,
      total: 134,
    },
    coverage: COVERAGE,
    evals: EVALS,
    history: [record(2), record(1)],
  },
} satisfies Meta<typeof ProvenanceSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FullPicture: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Header tally quotes the shared sources — same numbers as Workspace.
    await expect(
      canvas.getByText('Tier distribution · 134 graph facts · 2 unsupported patterns'),
    ).toBeInTheDocument();
    // The stacked bar is an image with a complete textual description —
    // nothing depends on color alone.
    await expect(
      canvas.getByRole('img', {
        name: /134 graph facts: 98 Confirmed, 22 Inferred Strong, 11 Inferred Weak, 3 Gap, 0 unattributed; plus 2 unsupported patterns/,
      }),
    ).toBeInTheDocument();
    // The legend keeps register findings distinct from graph facts.
    await expect(canvas.getByText('(register finding, not a graph fact)')).toBeInTheDocument();

    // Coverage bars carry the full description per extractor.
    await expect(
      canvas.getByRole('img', {
        name: 't0.adapter-ts: 83 facts from 12 of 14 files (86% coverage)',
      }),
    ).toBeInTheDocument();

    // AC-0277: historical scalar passed=true remains calibration, never a gate grant.
    await expect(canvas.getAllByText('CALIBRATION ONLY')).toHaveLength(2);
    await expect(canvas.queryByText('GATE PASS')).not.toBeInTheDocument();
    await expect(canvas.getByText('Calibration P 0.86 · R 0.79 · floor 0.80')).toBeInTheDocument();
    await expect(canvas.getByText('12 staged proposals · 0 recorded admissions')).toBeInTheDocument();
  },
};

export const DeterminismObservedInHistory: Story = {
  // Two ingests of the same commit with equal hashes: the invariant is
  // verified from data, and the footer says so.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole('img', { name: /Ingest 1 of collidingscopes\/image-trail/ }),
    ).toBeInTheDocument();
    await expect(canvas.getByTestId('determinism-note')).toHaveTextContent(
      'Determinism verified in this history: repeated ingests of the same commit carry identical content hashes.',
    );
  },
};

export const HashDivergenceIsNotClaimedVerified: Story = {
  // Different commits (or same commit, different hash) must not claim the
  // invariant was verified — the footer states the expectation instead.
  args: {
    history: [
      record(2, { commit_sha: 'b2c4d10', content_hash: 'f'.repeat(64) }),
      record(1),
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('determinism-note')).toHaveTextContent(
      'Re-ingesting the same commit yields an identical graph — equal content hashes in this history are the proof.',
    );
  },
};

export const UnattributedFactsAreAccounted: Story = {
  // Review fix on #139: a fact with no valid provenance is still one of
  // the N facts the chart claims to describe — never silently omitted.
  args: {
    distribution: {
      confirmed: 98,
      inferredStrong: 22,
      inferredWeak: 11,
      gap: 3,
      unattributed: 4,
      total: 138,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole('img', { name: /138 graph facts: .*, 4 unattributed;/ }),
    ).toBeInTheDocument();
    const legendEntry = canvas.getByText('Unattributed').closest('li') as HTMLElement;
    await expect(within(legendEntry).getByText('4')).toBeInTheDocument();
  },
};

export const DivergentRepeatIsFlagged: Story = {
  // Review fix on #139: same commit twice with different hashes must not
  // read as verified — it is a determinism violation, stated loudly.
  args: {
    history: [
      record(3, { content_hash: 'f'.repeat(64) }),
      record(2),
      record(1),
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('determinism-note')).toHaveTextContent(
      /Determinism violated in this history/,
    );
  },
};

export const NotApplicableCoverageIsExplicit: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // An extractor outside this ingest's scope reads n/a, never 0%.
    await expect(
      canvas.getByRole('img', {
        name: 't1.dynamic: 6 facts, scope not applicable this ingest',
      }),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/n\/a · 6 facts/)).toBeInTheDocument();
  },
};

export const EmptyState: Story = {
  args: {
    findings: null,
    distribution: {
      confirmed: 0,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 0,
      unattributed: 0,
      total: 0,
    },
    coverage: [],
    evals: [],
    history: [],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText('No ingest recorded yet — coverage lands with the first run.'),
    ).toBeInTheDocument();
    await expect(
      canvas.getByText('No calibration recorded — semantic proposals remain staged until evaluated.'),
    ).toBeInTheDocument();
    await expect(canvas.getByText('No ingest history yet.')).toBeInTheDocument();
  },
};

const EVALUATED: NonNullable<EvalResult['evaluation']> = {
  schema_version: 1,
  mode: 'independent_evaluation',
  families: [
    {
      family: { kind: 'Channel', subtype: 'sqs-queue' },
      status: 'independently_evaluated', reason: 'independently_evaluated',
      staged: 3, admitted: 2, ambiguous: 1,
      receipt_digest: 'a'.repeat(64),
      metrics: { precision: 1, recall: 0.8, true_positives: 4, false_positives: 0, false_negatives: 1, unknown: 2, no_match_false_admissions: 0, retrieval_recall: 0.9 },
    },
    {
      family: { kind: 'Call', subtype: null },
      status: 'abstained', reason: 'missing_evidence', staged: 4, admitted: 0, ambiguous: 0,
      receipt_digest: null, metrics: null,
    },
  ],
};
export const PerFamilyEvaluationAndAbstention: Story = {
  // AC-0277: an evaluated family never lends its receipt to another family.
  args: { evals: [{ ...EVALS[0], proposals: 7, approved: 2, evaluation: EVALUATED }] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('INDEPENDENT EVALUATION')).toBeInTheDocument();
    await expect(canvas.getByText('3 staged · 2 admitted · 1 ambiguous')).toBeInTheDocument();
    await expect(canvas.getByText('4 staged · 0 admitted · 0 ambiguous')).toBeInTheDocument();
    await expect(canvas.getByText(/ABSTAINED.*Missing independent evidence/)).toBeInTheDocument();
    await expect(canvas.queryByText('GATE PASS')).not.toBeInTheDocument();
  },
};
export const VisibleFixturesAreMechanismOnly: Story = {
  args: { evals: [{ ...EVALS[0], proposals: 7, approved: 2, evaluation: { ...EVALUATED, mode: 'visible_mechanism_evaluation' } }] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('VISIBLE MECHANISM ONLY')).toBeInTheDocument();
    await expect(canvas.getByText(/they do not establish model quality/)).toBeInTheDocument();
    await expect(canvas.queryByText('INDEPENDENT EVALUATION')).not.toBeInTheDocument();
    await expect(canvas.queryByText('GATE PASS')).not.toBeInTheDocument();
  },
};
export const EvaluatedFamilyBelowFloor: Story = {
  args: { evals: [{ ...EVALS[1], proposals: 3, approved: 0, evaluation: { schema_version: 1, mode: 'independent_evaluation', families: [{ ...EVALUATED.families[0], staged: 3, admitted: 0, ambiguous: 0, reason: 'below_floor', metrics: { precision: 0.5, recall: 0.5, true_positives: 1, false_positives: 1, false_negatives: 1, unknown: 0, no_match_false_admissions: 1, retrieval_recall: null } }] } }] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Below precision floor/)).toBeInTheDocument();
    await expect(canvas.getByText('3 staged · 0 admitted · 0 ambiguous')).toBeInTheDocument();
    await expect(canvas.queryByText('GATE PASS')).not.toBeInTheDocument();
  },
};
