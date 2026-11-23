import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fn, userEvent, within } from 'storybook/test';
import { WorkspaceSurface } from './WorkspaceSurface';
import type { FindingsSummary, SpecBundle, SystemRepo, TierDistribution } from '../store';

/** The handoff's image-trail outcome (screenshot 01). */
const FINDINGS: FindingsSummary = {
  gaps: 3,
  unsupported: 2,
  no_evidence: 1,
  drift: 1,
  open_findings: 6,
  graph_facts: 134,
};

const DISTRIBUTION: TierDistribution = {
  confirmed: 98,
  inferredStrong: 22,
  inferredWeak: 11,
  gap: 3,
  unattributed: 0,
  total: 134,
};

const BUNDLE = {
  mode: 'best-effort',
  assertion_count: 12,
  gap_count: 3,
  drift_count: 1,
  security_count: 0,
  artifacts: [
    'user_stories.md',
    'rule-evidence.md',
    'flow_dossiers.md',
    'US-TM.md',
    'topology.md',
    'gap_register.md',
    'adrs.md',
  ].map((fileName, index) => ({
    id: `artifact-${index}`,
    file_name: fileName,
    title: fileName,
    format: 'markdown' as const,
    content: `# ${fileName}`,
    schema: `cartograph.spec/${fileName}@1`,
    content_id: `spec-artifact-v1:fixture-${fileName}`,
    content_truncated: false,
    content_byte_len: `# ${fileName}`.length,
    assertions: [],
    assertions_truncated: false,
    assertions_total: 0,
  })),
} satisfies SpecBundle;

const meta = {
  title: 'Surfaces/WorkspaceSurface',
  component: WorkspaceSurface,
  args: {
    summary: {
      job_id: 3,
      files: 14,
      nodes: 80,
      edges: 54,
      layers: {
        ts: { files: 14, nodes: 80, edges: 54 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 0, nodes: 0, edges: 0 },
        java: { files: 0, nodes: 0, edges: 0 },            kotlin: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
        tools: { files: 0, nodes: 0, edges: 0 },
      },
      repo: 'collidingscopes/image-trail',
      commit_sha: 'a1b9f30fffffffffffffffffffffffffffffffff',
    },
    findings: FINDINGS,
    distribution: DISTRIBUTION,
    bundle: BUNDLE,
    onReingest: fn(),
    onTriageGaps: fn(),
    onProvenance: fn(),
    onOpenArtifact: fn(),
  },
} satisfies Meta<typeof WorkspaceSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const OpenInvestigations: Story = {
  // AC-0190: investigations are part of Workspace, with the stable rail intact.
  args: { onInvestigate: fn() },
  play: async ({ canvasElement, args }) => {
    await userEvent.click(within(canvasElement).getByRole('button', { name: 'Open investigations' }));
    await expect(args.onInvestigate).toHaveBeenCalledTimes(1);
  },
};

// #2170: the last ingest's totals and layer breakdown are on Workspace,
// folded under one line, with the repository named as Workspace labels it.
export const LastIngestBreakdown: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const details = canvas.getByRole('group', { name: 'Last ingest' });
    await expect(details).toHaveTextContent('Last ingest: 14 files → 80 nodes, 54 edges');
    await userEvent.click(within(details).getByText(/^Last ingest:/));
    await expect(canvas.getByTestId('ts-layer-summary')).toBeVisible();
    await expect(canvas.getByTestId('ts-layer-summary').textContent).toBe(
      '14 files · 80 nodes · 54 edges',
    );
  },
};

export const PartialRecovery: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Title row: system, mono commit chip, re-ingest.
    await expect(canvas.getByText('collidingscopes/image-trail')).toBeInTheDocument();
    await expect(canvas.getByText('@ a1b9f30')).toBeInTheDocument();

    // The honest tally: findings are listed explicitly, never guessed. The
    // badge reports the weakest tier present (11 InferredWeak facts here).
    await expect(canvas.getByText('Partial recovery')).toBeInTheDocument();
    await expect(canvas.getByText('InferredWeak overall')).toBeInTheDocument();
    const outcome = within(canvas.getByTestId('outcome-card'));
    await expect(outcome.getByText('6 open findings')).toBeInTheDocument();
    // #164: outcome jargon and artifact authority explain themselves.
    await expect(outcome.getByRole('button', { name: 'What is system gap?' })).toBeInTheDocument();
    await expect(
      canvas.getByRole('button', { name: 'What is recovery authority?' }),
    ).toBeInTheDocument();
    await expect(
      outcome.getByText(/3 gaps and 2 unsupported patterns \(plus 1 no-evidence\)/),
    ).toBeInTheDocument();

    // Provenance health reconciles with the same register summary the
    // outcome quotes — one source of truth.
    await expect(canvas.getByTestId('count-confirmed')).toHaveTextContent('98');
    await expect(canvas.getByText('73% of 134 facts')).toBeInTheDocument();
    await expect(canvas.getByTestId('count-gap')).toHaveTextContent(String(FINDINGS.gaps));
    await expect(canvas.getByTestId('count-unsupported')).toHaveTextContent(
      String(FINDINGS.unsupported),
    );

    // CTAs route to the register surfaces.
    await userEvent.click(canvas.getByRole('button', { name: 'Triage 3 gaps' }));
    await expect(args.onTriageGaps).toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('button', { name: 'Provenance & eval' }));
    await expect(args.onProvenance).toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('button', { name: /re-ingest/i }));
    await expect(args.onReingest).toHaveBeenCalled();
  },
};

export const ArtifactBadgeSemantics: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Two independent axes on generated artifacts: generation + authority.
    await expect(canvas.getAllByText('Artifact generated')).toHaveLength(6);
    await expect(canvas.getAllByText('Recovery: partial')).toHaveLength(5);

    // AC-0125: generation of source observations does not establish behavior.
    const rules = canvas.getByRole('button', { name: /Source rule evidence/ });
    await expect(within(rules).getByText('Artifact generated')).toBeInTheDocument();
    await expect(within(rules).getByText('Interpretation: incomplete')).toBeInTheDocument();
    await expect(within(rules).queryByText(/Recovery:/)).not.toBeInTheDocument();
    await userEvent.click(rules);
    await expect(args.onOpenArtifact).toHaveBeenCalled();

    // The gap register shows exactly ONE completion-style badge, never two.
    const register = canvas
      .getByText('Gap register')
      .closest('.artifact-card') as HTMLElement;
    await expect(within(register).getByText('6 open findings')).toBeInTheDocument();
    await expect(within(register).queryByText('Artifact generated')).not.toBeInTheDocument();
    await expect(within(register).queryByText(/Recovery:/)).not.toBeInTheDocument();

    await userEvent.click(canvas.getByText('Flow dossiers'));
    await expect(args.onOpenArtifact).toHaveBeenCalled();
  },
};

export const FullyConfirmedRecovery: Story = {
  args: {
    findings: { ...FINDINGS, gaps: 0, unsupported: 0, no_evidence: 0, open_findings: 0 },
    distribution: {
      confirmed: 134,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 0,
      unattributed: 0,
      total: 134,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Full recovery')).toBeInTheDocument();
    await expect(canvas.getByText('Confirmed overall')).toBeInTheDocument();
    await expect(canvas.getAllByText('Recovery: authoritative')).toHaveLength(5);
    // AC-0125: even a confirmed graph cannot upgrade local source observations
    // to a complete behavioral interpretation.
    const rules = within(canvas.getByRole('button', { name: /Source rule evidence/ }));
    await expect(rules.getByText('Interpretation: incomplete')).toBeInTheDocument();
    await expect(rules.queryByText('Recovery: authoritative')).not.toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Triage 0 gaps' })).toBeInTheDocument();
  },
};

export const ConfirmedWithGapsIsNotInferred: Story = {
  // AC-0057 (#245): a fully deterministic recovery with open gaps is
  // partial, but the tier badge states what the recovered facts carry —
  // Confirmed, never an Inferred tier no fact holds (R-INT-2/R-INT-4).
  args: {
    findings: {
      gaps: 203,
      interpretation_backlog: 120,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 203,
      graph_facts: 1265,
    },
    distribution: {
      confirmed: 1062,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 203,
      unattributed: 0,
      total: 1265,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Partial recovery')).toBeInTheDocument();
    await expect(canvas.getByText('Confirmed overall')).toBeInTheDocument();
    await expect(canvas.queryByText(/Inferred\w+ overall/)).not.toBeInTheDocument();
    await expect(
      canvas.getByText(
        /203 gaps \(120 of them an interpretation backlog, not extraction failures\)/,
      ),
    ).toBeInTheDocument();
  },
};

export const UnsupportedOnlyIsNotAuthoritative: Story = {
  // Review fix on #136: unsupported/no-evidence findings without gaps must
  // still keep artifacts from claiming authoritative recovery.
  args: {
    findings: {
      gaps: 0,
      unsupported: 2,
      no_evidence: 0,
      drift: 0,
      open_findings: 2,
      graph_facts: 134,
    },
    distribution: {
      confirmed: 134,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 0,
      unattributed: 0,
      total: 134,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('Recovery: partial')).toHaveLength(5);
    await expect(canvas.queryByText('Recovery: authoritative')).not.toBeInTheDocument();
  },
};

export const ManifestRecoveryShowsRepoIdentities: Story = {
  // Review fix on #136: a manifest recovery lists exact per-repo identities,
  // never a false mutable-workdir chip.
  args: {
    summary: {
      job_id: 5,
      files: 5,
      nodes: 40,
      edges: 60,
      layers: {
        ts: { files: 3, nodes: 25, edges: 38 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 2, nodes: 15, edges: 22 },
        java: { files: 0, nodes: 0, edges: 0 },            kotlin: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
        tools: { files: 0, nodes: 0, edges: 0 },
      },
      repos: ['acme/shop@a1b2c3d4e5f6', 'local/infra@workdir'],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('2 repos as one system')).toBeInTheDocument();
    await expect(
      canvas.getByText('acme/shop@a1b2c3d4e5f6 · local/infra@workdir'),
    ).toBeInTheDocument();
    await expect(canvas.queryByText('@ workdir')).not.toBeInTheDocument();
  },
};

export const StackedSystemContentsAreStated: Story = {
  // AC-0085 (#162): two stacked ingests are never silent — the Workspace
  // names every repo in the system and states the merge behavior.
  args: {
    systemContents: [
      { repo: 'acme/shop', commit: 'a1b2c3d4e5f6' },
      { repo: 'local/infra', commit: 'workdir' },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const contents = canvas.getByTestId('system-contents');
    await expect(contents).toHaveTextContent(
      'System contents: acme/shop @ a1b2c3d · local/infra @ workdir',
    );
    await expect(contents).toHaveTextContent(
      'new ingests merge into this system; Clear system starts over',
    );
  },
};

export const RecordedGraphHashIsShownWithItsRegistration: Story = {
  // AC-0593 (#1765): each registration shows the graph content hash its
  // latest ingest recorded; a registration with none recorded shows no hash.
  args: {
    systemContents: [
      {
        repo: 'acme/shop',
        commit: 'a1b2c3d4e5f6',
        graph_hash: '9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0',
      },
      { repo: 'local/infra', commit: 'workdir' },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('system-contents')).toHaveTextContent(
      'System contents: acme/shop @ a1b2c3d (graph 9f8e7d6c) · local/infra @ workdir',
    );
    const hashes = canvas.getAllByTestId('system-graph-hash');
    await expect(hashes).toHaveLength(1);
    await expect(hashes[0]).toHaveAttribute(
      'title',
      'Graph content hash 9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0',
    );
  },
};

export const NoRecoveryYet: Story = {
  args: { summary: null, findings: null, bundle: null },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No recovery yet')).toBeInTheDocument();
    await expect(canvas.queryByTestId('prov-health')).not.toBeInTheDocument();

    // #161: the empty landing flows with the window — a fluid, centered
    // column (auto inline margins under a wide cap), never a fixed block
    // pinned to the left half of a stretched window.
    const landing = canvasElement.querySelector('.workspace-landing') as HTMLElement;
    const style = getComputedStyle(landing);
    await expect(style.maxWidth).toContain('1280px');
    // margin-inline: auto — equal margins on both sides at any width.
    await expect(style.marginLeft).toBe(style.marginRight);

    await userEvent.click(canvas.getByRole('button', { name: 'Connect a target' }));
    await expect(args.onReingest).toHaveBeenCalled();
  },
};

export const RegisteredRepositoryDisplayNames: Story = {
  // AC-0144: operational labels use exact repo membership, never the first
  // registration. A host without a display label still shows the repository key.
  args: {
    summary: { ...meta.args.summary, repo: 'local/src_22222222222222222222222222222222' },
    systemContents: [
      { repo: 'local/src_11111111111111111111111111111111', display_name: 'Other project', commit: 'workdir' },
      { repo: 'local/src_22222222222222222222222222222222', display_name: 'Billing service', commit: 'workdir' },
      { repo: 'acme/infra', commit: 'a1b2c3d4e5f6' },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { name: 'Billing service' })).toBeInTheDocument();
    await expect(canvas.getByTestId('system-contents')).toHaveTextContent(
      'Other project @ workdir · Billing service @ workdir · acme/infra @ a1b2c3d',
    );
    await expect(canvas.queryByText(/local\/src_/)).not.toBeInTheDocument();
  },
};

const SAME_NAMED_SOURCES: SystemRepo[] = [
  { repo: 'local/src_11111111111111111111111111111111', display_name: 'mirror', commit: 'workdir' },
  { repo: 'local/src_22222222222222222222222222222222', display_name: ' mirror ', commit: 'workdir' },
  { repo: 'acme/shop', display_name: 'shop', commit: 'a1b2c3d4e5f6' },
  { repo: 'other/shop', display_name: 'shop', commit: 'b2c3d4e5f6a1' },
  { repo: 'acme/billing', display_name: 'Billing service', commit: 'workdir' },
  { repo: 'legacy/mirror', commit: 'workdir' },
  { repo: 'legacy/blank', display_name: '  ', commit: 'workdir' },
];

export const SameNamedRepositoriesRemainDistinct: Story = {
  // AC-0144: equal local basenames and GitHub repository names retain their
  // human labels and exact keys. Unique names and missing labels stay concise.
  args: {
    summary: { ...meta.args.summary, repo: 'local/src_22222222222222222222222222222222' },
    systemContents: SAME_NAMED_SOURCES,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', {
      name: 'mirror (local/src_22222222222222222222222222222222)',
    })).toBeInTheDocument();
    await expect(canvas.getByTestId('system-contents')).toHaveTextContent(
      'System contents: mirror (local/src_11111111111111111111111111111111) @ workdir · '
      + 'mirror (local/src_22222222222222222222222222222222) @ workdir · '
      + 'shop (acme/shop) @ a1b2c3d · shop (other/shop) @ b2c3d4e · '
      + 'Billing service @ workdir · legacy/mirror @ workdir · legacy/blank @ workdir',
    );
  },
};

export const ManifestLabelsUseExactRepositoryMembership: Story = {
  // AC-0144: revision labels share the same disambiguation. A same-basename
  // repository absent from current membership never borrows another's label.
  args: {
    summary: {
      ...meta.args.summary,
      repo: undefined,
      commit_sha: undefined,
      repos: ['acme/shop@a1b2c3d4e5f6', 'other/shop@b2c3d4e5f6a1', 'unknown/shop@workdir'],
    },
    systemContents: SAME_NAMED_SOURCES,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { name: '3 repos as one system' })).toBeInTheDocument();
    await expect(canvas.getByText(
      'shop (acme/shop)@a1b2c3d4e5f6 · shop (other/shop)@b2c3d4e5f6a1 · unknown/shop@workdir',
    )).toBeInTheDocument();
  },
};

export const MissingArtifactIsVisible: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: BUNDLE.artifacts.filter(
        (artifact) => !['topology.md', 'rule-evidence.md'].includes(artifact.file_name),
      ),
    },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // A missing artifact is stated, not implied by absence.
    const topology = canvas
      .getByText('Topology / resource map')
      .closest('.artifact-card') as HTMLElement;
    await expect(within(topology).getByText('Not generated')).toBeInTheDocument();
    // AC-0125: older/missing bundles retain an explicit missing card and the
    // established navigation into the Spec Workbench.
    const rules = canvas.getByRole('button', { name: /Source rule evidence/ });
    await expect(within(rules).getByText('Not generated')).toBeInTheDocument();
    await expect(within(rules).queryByText('Artifact generated')).not.toBeInTheDocument();
    await userEvent.click(rules);
    await expect(args.onOpenArtifact).toHaveBeenCalled();
  },
};

const COVERAGE = [
  {
    inventory: "coverage@1",
    repo: "acme/api",
    commit: "0123456789abcdef",
    layer_hints: ["server"],
    producers: [
      {
        id: "t0.adapter-go",
        kind: "deterministic" as const,
        covers: "Go",
        state: "ran" as const,
        files: 7,
        nodes: 21,
        edges: 14,
        inputs: [],
        stopping_reason:
          "completed over 7 files of acme/api@0123456789abcdef for layers server",
      },
      {
        id: "t0.iac-terraform",
        kind: "deterministic" as const,
        covers: "Terraform",
        state: "not_selected" as const,
        files: 0,
        nodes: 0,
        edges: 0,
        inputs: [],
        stopping_reason:
          "not selected: layer hints [server] exclude infra and cloud",
      },
      {
        id: "t0.adapter-python",
        kind: "deterministic" as const,
        covers: "Python",
        state: "not_applicable" as const,
        files: 0,
        nodes: 0,
        edges: 0,
        inputs: [],
        stopping_reason: "no Python files in acme/api@0123456789abcdef",
      },
    ],
    missing_inputs: [],
    unsupported: [
      {
        capability: "Ruby",
        occurrences: 2,
        first_site: "lib/a.rb:1",
        reason:
          "no installed producer covers Ruby: a tool limitation, not a system gap",
      },
    ],
    limited: true,
  },
];

export const CoverageInventoryBeforeInterpretation: Story = {
  // AC-0238: each repo's producers, exclusions and unsupported capabilities
  // are listed with their stopping reasons ahead of the investigation entry.
  args: {
    onInvestigate: fn(),
    summary: { ...meta.args.summary, coverage: COVERAGE },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const inventory = canvas.getByTestId("coverage-inventory");
    await expect(inventory).toHaveTextContent(
      "Limited coverage · 1 producer ran · 1 excluded by layer hints · 1 unsupported capability",
    );
    await expect(inventory).toHaveTextContent(
      "not selected: layer hints [server] exclude infra and cloud",
    );
    await expect(inventory).toHaveTextContent("Nothing to read for: Python.");
    await expect(inventory).toHaveTextContent("Ruby ×2, first at lib/a.rb:1");
    const entry = canvas.getByRole("button", { name: "Open investigations" });
    await expect(
      inventory.compareDocumentPosition(entry) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  },
};

export const RecordedCoverageRestoredAfterRelaunch: Story = {
  // AC-0700 (#1656): after a relaunch there is no ingest summary; the
  // recorded inventory is shown as recorded, not newly validated, and a stale
  // one is named stale with nothing read from it.
  args: {
    summary: null,
    recordedCoverage: [
      {
        repo: "acme/api",
        status: "recorded",
        inventory_hash: "blake3:0123456789abcdef0123",
        compared: ["registration", "plugins"],
        inventory: COVERAGE[0],
      },
      {
        repo: "acme/web",
        status: "stale",
        inventory_hash: "blake3:fedcba9876543210fedc",
        compared: ["registration", "plugins"],
        inventory: null,
      },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const inventory = canvas.getByTestId("coverage-inventory");
    await expect(inventory).toHaveTextContent(
      "Limited coverage · 1 producer ran · 1 excluded by layer hints · 1 unsupported capability",
    );
    await expect(inventory).toHaveTextContent(
      "source inputs and everything else are as recorded, not newly validated",
    );
    await expect(inventory).not.toHaveTextContent("acme/web");
    const stale = canvas.getByTestId("stale-coverage");
    await expect(stale).toHaveTextContent(
      "acme/web blake3:fedcba987654: stale, not current.",
    );
    await expect(stale).toHaveTextContent("compared: registration, plugins");
  },
};

export const RecordedCoverageReadFailureShowsNoCards: Story = {
  // AC-0700: a failed read is stated and shows no inventory card.
  args: {
    summary: null,
    recordedCoverage: [],
    recordedCoverageError: "incompatible coverage schema",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole("alert")).toHaveTextContent(
      "Recorded coverage could not be read: incompatible coverage schema.",
    );
    await expect(canvas.queryByTestId("coverage-inventory")).toBeNull();
    await expect(canvas.queryByTestId("stale-coverage")).toBeNull();
  },
};

const REFUSED_COVERAGE = [
  {
    ...COVERAGE[0],
    producers: [
      ...COVERAGE[0].producers,
      {
        id: "t0.plugin-zig@0123456789ab",
        kind: "deterministic" as const,
        covers: "gated plugin",
        state: "ran" as const,
        files: 2,
        nodes: 5,
        edges: 3,
        inputs: [],
        stopping_reason:
          "read 2 of the 5 files its extensions claim in acme/api@0123456789abcdef; the host refused its extraction of 3 (2 malformed fact, 1 bound exceeded), and no fact from those files by this plugin joined the graph",
        refusals: {
          malformed_fact: 2,
          guest_error: 0,
          bound_exceeded: 1,
          files: [
            { path: "lib/m.zig", reason: "malformed_fact" as const },
            { path: "src/z.zig", reason: "bound_exceeded" as const },
          ],
          omitted: 1,
        },
      },
    ],
    unsupported: [
      ...COVERAGE[0].unsupported,
      {
        capability: "plugin-malformed-fact",
        occurrences: 2,
        first_site: "lib/m.zig",
        reason:
          "2 file(s) were refused for a gated plugin because the plugin emitted a fact the host does not accept; no fact from them by that plugin joined the graph, so that plugin does not cover them: a plugin failure, not a system gap",
      },
      {
        capability: "plugin-bound-exceeded",
        occurrences: 1,
        first_site: "src/z.zig",
        reason:
          "1 file(s) were refused for a gated plugin because the plugin exceeded its fuel, memory or table bound; no fact from them by that plugin joined the graph, so that plugin does not cover them: a plugin failure, not a system gap",
      },
    ],
  },
];

export const CoverageInventoryNamesFilesAPluginWasRefusedOn: Story = {
  // AC-0658: a gated plugin refused on some files is shown with the refused
  // count by class, each listed file as not covered with its reason class,
  // and the files past the listing bound as a count.
  args: {
    summary: { ...meta.args.summary, coverage: REFUSED_COVERAGE },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const inventory = canvas.getByTestId("coverage-inventory");
    await expect(inventory).toHaveTextContent(
      "Limited coverage · 2 producers ran · 1 excluded by layer hints · 1 unsupported capability · 3 files refused for a plugin",
    );
    await expect(inventory).toHaveTextContent(
      "Ran, files refused t0.plugin-zig@0123456789ab (gated plugin): read 2 of the 5 files its extensions claim in acme/api@0123456789abcdef; the host refused its extraction of 3 (2 malformed fact, 1 bound exceeded)",
    );
    const refused = within(
      within(inventory).getByRole("list", {
        name: "Files refused for t0.plugin-zig@0123456789ab",
      }),
    ).getAllByRole("listitem");
    await expect(refused.map((item) => item.textContent)).toEqual([
      "lib/m.zig: not covered, the plugin emitted a fact the host does not accept",
      "src/z.zig: not covered, the plugin exceeded its fuel, memory or table bound",
      "1 more refused files are counted but not listed",
    ]);
    // The refused files are listed with their plugin, not as a capability
    // no producer covers.
    await expect(inventory).not.toHaveTextContent("plugin-malformed-fact");
    await expect(inventory).toHaveTextContent("Ruby ×2, first at lib/a.rb:1");
  },
};

export const DraftAdapterWorkPacketsInCoverage: Story = {
  // AC-0371 (#1101): drafting adapter work packets for a recovered repo shows
  // each returned packet's language and Markdown in a read-only <pre> with a
  // Copy button, shows "Every looked-up cell has a declaring adapter" when
  // empty, and shows errors as text.
  args: {
    summary: { ...meta.args.summary, coverage: COVERAGE },
  },
  beforeEach: () => () => clearMocks(),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const inventory = within(canvas.getByTestId("coverage-inventory"));
    const button = inventory.getByRole("button", {
      name: "Draft adapter work packet",
    });

    const seenRepos: string[] = [];
    mockIPC((command, args) => {
      if (command === "draft_adapter_work_packets") {
        seenRepos.push(String((args as { repo?: string })?.repo));
        return [
          {
            language: "Ruby",
            content_id: "work-packet-v1:ruby-fixture",
            markdown:
              "# Work Packet: Add adapter coverage for Ruby (imports, calls, types)\n",
          },
        ];
      }
      throw new Error(`unexpected command: ${command}`);
    });
    await userEvent.click(button);
    await expect(seenRepos).toEqual(["acme/api"]);
    await expect(
      await inventory.findByLabelText("Ruby adapter work packet"),
    ).toHaveTextContent(
      "# Work Packet: Add adapter coverage for Ruby (imports, calls, types)",
    );
    await expect(
      inventory.getByRole("button", { name: "Copy" }),
    ).toBeInTheDocument();

    mockIPC((command) => {
      if (command === "draft_adapter_work_packets") return [];
      throw new Error(`unexpected command: ${command}`);
    });
    await userEvent.click(button);
    await expect(
      await inventory.findByText(
        "Every looked-up cell has a declaring adapter",
      ),
    ).toBeInTheDocument();
    await expect(
      inventory.queryByLabelText("Ruby adapter work packet"),
    ).not.toBeInTheDocument();

    mockIPC((command) => {
      if (command === "draft_adapter_work_packets") {
        // The host rejects with its typed error object.
        throw { code: "selection_conflict" };
      }
      throw new Error(`unexpected command: ${command}`);
    });
    await userEvent.click(button);
    await expect(await inventory.findByRole("alert")).toHaveTextContent(
      "The checkout has changed since it was recovered.",
    );
    await expect(
      inventory.queryByText("Every looked-up cell has a declaring adapter"),
    ).not.toBeInTheDocument();
  },
};

const LOCAL_KEY = 'local/src_536205a518ecd1d02e552e71e72ece1d';
const LOCAL_SOURCE: SystemRepo[] = [{ repo: LOCAL_KEY, display_name: 'simplelogin', commit: 'workdir' }];

export const RelaunchNamesThePersistedSystem: Story = {
  // #2469: after a relaunch there is no ingest summary; the heading names the
  // persisted system, not "Ingested system".
  args: { summary: null, systemContents: LOCAL_SOURCE },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 2, name: 'simplelogin' })).toBeInTheDocument();
    await expect(canvas.queryByText('Ingested system')).not.toBeInTheDocument();
  },
};

export const FirstReadSaysItIsLoading: Story = {
  // #2469: while the first read of the persisted system runs, the landing
  // says so instead of claiming there is no system.
  args: { summary: null, findings: null, bundle: null, systemContents: [], loading: true },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 2, name: 'Loading the recovered system…' })).toBeInTheDocument();
    await expect(canvas.queryByText('No system yet')).not.toBeInTheDocument();
  },
};

export const CoverageNamesLocalSourcesByFolder: Story = {
  // #2471: the coverage inventory names a local source by its folder, in the
  // heading and in the producers' reasons; the exact key is the heading's title.
  args: {
    summary: {
      ...meta.args.summary,
      repo: LOCAL_KEY,
      coverage: [{
        ...COVERAGE[0],
        repo: LOCAL_KEY,
        producers: COVERAGE[0].producers.map((producer) => ({
          ...producer,
          stopping_reason: producer.stopping_reason.replace('acme/api', LOCAL_KEY),
        })),
      }],
    },
    systemContents: LOCAL_SOURCE,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const inventory = canvas.getByTestId('coverage-inventory');
    await expect(inventory).toHaveTextContent('completed over 7 files of simplelogin@0123456789abcdef');
    await expect(inventory).not.toHaveTextContent(/local\/src_/);
    await expect(within(inventory).getByText('simplelogin')).toHaveAttribute('title', LOCAL_KEY);
  },
};

export const RecordedCoverageNamesLocalSources: Story = {
  // #2471: restored and stale inventories use the same readable names.
  args: {
    summary: null,
    systemContents: LOCAL_SOURCE,
    recordedCoverage: [{
      repo: LOCAL_KEY,
      status: 'stale',
      inventory_hash: 'blake3:fedcba9876543210fedc',
      compared: ['registration', 'plugins'],
      inventory: null,
    }],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const stale = canvas.getByTestId('stale-coverage');
    await expect(stale).toHaveTextContent('simplelogin blake3:fedcba987654: stale, not current.');
    await expect(within(stale).getByText('simplelogin')).toHaveAttribute('title', LOCAL_KEY);
  },
};

export const CoverageNamesASourceWithNoFacts: Story = {
  // #2471: a source whose ingest recovered no evidence-bearing fact is not in
  // the system contents; its heading and coverage still read by its registered name.
  args: {
    summary: {
      ...meta.args.summary,
      repo: LOCAL_KEY,
      coverage: [{ ...COVERAGE[0], repo: LOCAL_KEY }],
    },
    systemContents: [],
    registeredSources: [{ repo_key: LOCAL_KEY, display_name: 'empty-folder' }],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 2, name: 'empty-folder' })).toBeInTheDocument();
    const inventory = canvas.getByTestId('coverage-inventory');
    await expect(within(inventory).getByText('empty-folder')).toHaveAttribute('title', LOCAL_KEY);
    await expect(canvasElement).not.toHaveTextContent(/local\/src_/);
  },
};
