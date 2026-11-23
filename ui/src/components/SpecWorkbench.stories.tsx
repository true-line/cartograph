import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { SpecArtifact, SpecAssertion, SpecBundle, SpecViewBundle } from '../store';
import { STRUCTURED_VIEW_UNAVAILABLE_NOTICE } from './SpecDiagramView';
import { SpecWorkbench } from './SpecWorkbench';
import { CURATED_REVISION, curatedReadFixture } from '../curatedFixtures';

const CONFIRMED: SpecAssertion = {
  id: 'node:cap:orders',
  subject_id: 'cap:orders',
  subject_kind: 'Capability',
  summary: 'Capability: Place orders',
  provenance: {
    tier: 'Deterministic',
    confidence_tier: 'Confirmed',
    evidence: [{
      repo: 'acme/shop',
      path: 'src/orders.ts',
      byte_start: 18,
      byte_end: 72,
      commit_sha: 'abc123',
    }],
    extractor_id: 't0.capability',
    content_hash: 'a'.repeat(64),
  },
};

const INFERRED: SpecAssertion = {
  id: 'node:adr:queue',
  subject_id: 'adr:queue',
  subject_kind: 'ADR',
  summary: 'ADR: asynchronous fulfillment',
  provenance: {
    tier: 'Semantic',
    confidence_tier: 'InferredStrong',
    evidence: [{
      repo: 'acme/shop',
      path: 'src/fulfillment.ts',
      byte_start: 90,
      byte_end: 148,
      commit_sha: 'abc123',
    }, {
      repo: 'acme/infra',
      path: 'orders.tf',
      byte_start: 10,
      byte_end: 88,
      commit_sha: 'def456',
    }],
    extractor_id: 't2.semantic',
    content_hash: 'b'.repeat(64),
  },
};

const GAP: SpecAssertion = {
  id: 'node:gap:channel',
  subject_id: 'gap:channel',
  subject_kind: 'Gap',
  summary: 'Gap: runtime-computed channel identity',
  provenance: {
    tier: 'Deterministic',
    confidence_tier: 'Gap',
    evidence: [{
      repo: 'acme/shop',
      path: 'src/publish.ts',
      byte_start: 44,
      byte_end: 80,
      commit_sha: 'abc123',
    }],
    extractor_id: 't0.events',
    content_hash: 'c'.repeat(64),
  },
};

const FOUND_ADR: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:adr:found:no-sync',
  subject_id: 'adr:found:no-sync',
  subject_kind: 'ADR',
  summary: 'ADR: No synchronous order calls',
  provenance: {
    ...CONFIRMED.provenance,
    evidence: [{
      repo: 'acme/shop',
      path: 'docs/adr/ADR-0007-no-sync.md',
      byte_start: 0,
      byte_end: 180,
      commit_sha: 'abc123',
    }],
    extractor_id: 't0.adr-markdown',
    content_hash: 'd'.repeat(64),
  },
};

const DRIFT: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:drift:no-sync',
  subject_id: 'drift:no-sync',
  subject_kind: 'Drift',
  summary: 'Drift: No synchronous order calls forbids CALLS',
  provenance: {
    ...CONFIRMED.provenance,
    extractor_id: 't0.adr-drift',
    content_hash: 'e'.repeat(64),
  },
};

const UNAUTHENTICATED: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:finding:security:admin',
  subject_id: 'finding:security:admin',
  subject_kind: 'Finding',
  summary: 'Finding: No authentication check recovered: GET /admin',
  provenance: {
    ...CONFIRMED.provenance,
    extractor_id: 't0.security-projection',
    content_hash: 'f'.repeat(64),
  },
};

const OVER_BROAD_GRANT: SpecAssertion = {
  ...INFERRED,
  id: 'node:finding:security:grant',
  subject_id: 'finding:security:grant',
  subject_kind: 'Finding',
  summary: 'Finding: Over-broad IAM grant: admin policy → orders/*',
  provenance: {
    ...INFERRED.provenance,
    confidence_tier: 'InferredWeak',
    extractor_id: 't2.security-projection',
    content_hash: '1'.repeat(64),
  },
};

const SOURCE_RULE: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:rule:guarded-return',
  subject_id: 'rule:guarded-return',
  subject_kind: 'BusinessRule',
  summary: 'Guarded local exit observation; behavioral interpretation not established',
  provenance: {
    ...CONFIRMED.provenance,
    extractor_id: 't0.ts',
    content_hash: '2'.repeat(64),
  },
};

const RULE_DEPENDENCY: SpecAssertion = {
  ...GAP,
  id: 'edge:rule:guarded-return DEPENDS_ON gap:consumer',
  subject_id: 'rule:guarded-return DEPENDS_ON gap:consumer',
  subject_kind: 'DEPENDS_ON',
  summary: 'Source-rule DEPENDS_ON relationship',
  provenance: {
    ...GAP.provenance,
    extractor_id: 't0.ts',
    content_hash: '3'.repeat(64),
  },
};

function artifact(
  id: string,
  fileName: string,
  title: string,
  assertions: SpecAssertion[] = [],
): SpecArtifact {
  const content = `# ${title}\n\n## Assertions and inline provenance\n`;
  return {
    id,
    file_name: fileName,
    title,
    format: 'markdown',
    content,
    schema: `cartograph.spec/${fileName}@1`,
    content_id: `spec-artifact-v1:fixture-${fileName}`,
    content_truncated: false,
    content_byte_len: content.length,
    assertions,
    assertions_truncated: false,
    assertions_total: assertions.length,
  };
}

const SOURCE_RULE_CONTENT =
  '# Source rule evidence\n\nGuarded local exit observations.\n\nComplete execution predicate: not established.\n\nConsumer effect: not established.\n\nLocal effect: Return(false).\n';

const SOURCE_RULE_ARTIFACT: SpecArtifact = {
  ...artifact('rule-evidence', 'rule-evidence.md', 'Source rule evidence', [
    SOURCE_RULE,
    RULE_DEPENDENCY,
  ]),
  content: SOURCE_RULE_CONTENT,
  content_byte_len: SOURCE_RULE_CONTENT.length,
};

const BUNDLE: SpecBundle = {
  mode: 'best-effort',
  artifacts: [
    artifact('user-stories', 'user_stories.md', 'User stories', [CONFIRMED, INFERRED]),
    SOURCE_RULE_ARTIFACT,
    artifact('us-tm', 'US-TM.md', 'US traceability matrix'),
    artifact('flow-dossiers', 'flow_dossiers.md', 'Flow dossiers', [CONFIRMED]),
    artifact('topology', 'topology.md', 'Resource topology', [CONFIRMED]),
    artifact('data-model', 'data_model.md', 'Data model', [CONFIRMED]),
    artifact('adrs', 'adrs.md', 'Architecture decisions', [INFERRED]),
    artifact('gap-register', 'gap_register.md', 'Gap register', [GAP]),
    artifact('drift-register', 'drift_register.md', 'Drift register'),
    {
      ...artifact(
        'security-view',
        'security.md',
        'Security findings',
        [UNAUTHENTICATED, OVER_BROAD_GRANT],
      ),
      content: '# Security findings\n\n| Finding | Type | Subject | Resource scope | Actions | US / AC | Confidence |\n|---|---|---|---|---|---|---|\n| No authentication check recovered: GET /admin | no_auth_check_recovered | ep:admin | GET /admin | — | US-0015 / AC-0041 | Confirmed |\n| Over-broad IAM grant | over_broad_grant | res:admin GRANTS res:orders | arn:aws:s3:::orders/* | s3:Get* | US-0015 / AC-0042 | InferredWeak |\n',
    },
  ],
  assertion_count: 11,
  gap_count: 1,
  drift_count: 0,
  security_count: 2,
};

const meta = {
  title: 'Spec/SpecWorkbench',
  component: SpecWorkbench,
  args: {
    bundle: BUNDLE,
    mode: 'best-effort',
    decisions: [],
    busy: false,
    error: null,
    canCurate: true,
    onModeChange: fn(),
    onCurate: fn(),
    onCopyArtifact: fn(),
    onExportBundle: fn(),
  },
} satisfies Meta<typeof SpecWorkbench>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FullArtifactSetAndInlineProvenance: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await expect(within(nav).getAllByRole('button')).toHaveLength(10);
    await expect(canvas.getByText('10 artifacts')).toBeInTheDocument();
    await expect(canvas.getByText('Capability: Place orders')).toBeInTheDocument();
    await expect(canvas.getByText('ADR: asynchronous fulfillment')).toBeInTheDocument();
    await expect(canvas.getByText('t2.semantic')).toBeInTheDocument();
    await expect(canvas.getByText('b'.repeat(64))).toBeInTheDocument();
    await expect(canvas.getByText('acme/infra/orders.tf')).toBeInTheDocument();
    await userEvent.click(within(nav).getByRole('button', { name: /Gap register/ }));
    await expect(canvas.getByText('Gap: runtime-computed channel identity')).toBeInTheDocument();
    await expect(canvas.queryByText('None recorded — treated as unresolved.')).not.toBeInTheDocument();

    // Without host views from the core, diagram artifacts show the unavailable
    // notice and raw source rather than fabricating a client-derived diagram.
    await userEvent.click(within(nav).getByRole('button', { name: /Resource topology/ }));
    await expect(canvas.queryByTestId('spec-diagram-canvas')).not.toBeInTheDocument();
    await expect(canvas.getByTestId('diagram-view-unavailable')).toHaveTextContent(
      STRUCTURED_VIEW_UNAVAILABLE_NOTICE,
    );
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent('# Resource topology');

    // AC-0125: the new artifact is navigable and copyable with its source-only
    // qualifier, actual assertion units, and locked T0 observation intact.
    const rulesLink = within(nav).getByRole('button', { name: /Source rule evidence/ });
    await expect(within(rulesLink).getByText('2 assertions')).toBeInTheDocument();
    await userEvent.click(rulesLink);
    await expect(rulesLink).toHaveAttribute('aria-current', 'page');
    const detail = within(canvas.getByRole('article', { name: 'Source rule evidence' }));
    await expect(detail.getByText(/Source observations only/)).toHaveTextContent(
      'Complete execution predicates and consumer effects are not established.',
    );
    await userEvent.click(detail.getByRole('button', { name: 'Source' }));
    await expect(detail.getByTestId('spec-artifact-source')).toHaveTextContent(
      'Consumer effect: not established.',
    );
    const observed = within(detail.getByText(SOURCE_RULE.summary).closest('li') as HTMLElement);
    await expect(observed.getByRole('button', { name: 'Accept' })).toBeDisabled();
    await expect(observed.getByText('Confirmed T0 — locked, read-only')).toBeInTheDocument();
    await userEvent.click(detail.getByRole('button', { name: 'Copy artifact' }));
    await expect(args.onCopyArtifact).toHaveBeenCalledWith(SOURCE_RULE_ARTIFACT);
  },
};

export const LocalConstInitializersKeepObservationAuthority: Story = {
  // AC-0170: the stored artifact is displayed and copied with its qualification;
  // initializer syntax does not become a curatable or complete business rule.
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: [{
        ...SOURCE_RULE_ARTIFACT,
        content: '# Source rule evidence\n\nSame-callable branch condition: (allowed)\n\n### Local const initializers\n\nInitializer as written; value at use and business meaning are not established.\n\nBinding: binding:allowed\n\nDeclaration source: fixture:rules.ts bytes 20..65 @ workdir\n\nUse sources: fixture:rules.ts bytes 72..79 @ workdir\n\nStored initializer: item.enabled !== false\n\nStructured expression evidence: Binary StrictNotEqual; runtime value unresolved\n\nConsumer effect: not established.\n',
      }],
      assertion_count: SOURCE_RULE_ARTIFACT.assertions.length,
    },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Source' }));
    const source = canvas.getByTestId('spec-artifact-source');
    await expect(source).toHaveTextContent('Same-callable branch condition: (allowed)');
    await expect(source).toHaveTextContent('Initializer as written; value at use and business meaning are not established.');
    await expect(source).toHaveTextContent('Stored initializer: item.enabled !== false');
    await expect(source).toHaveTextContent('rules.ts bytes 20..65 @ workdir');
    await expect(source).toHaveTextContent('runtime value unresolved');
    await expect(canvas.getByText(/Source observations only/)).toBeInTheDocument();
    const observed = within(canvas.getByText(SOURCE_RULE.summary).closest('li') as HTMLElement);
    await expect(observed.getByRole('button', { name: 'Accept' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Copy artifact' }));
    await expect(args.onCopyArtifact).toHaveBeenCalledWith(args.bundle?.artifacts[0]);
  },
};

export const LegacyRuleDefinitionsWereNotCollected: Story = {
  // AC-0170: v1 history remains visible without implying a fresh empty analysis.
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: [{
        ...SOURCE_RULE_ARTIFACT,
        content: '# Source rule evidence\n\n### Local const initializers\n\nInitializer as written; value at use and business meaning are not established.\n\nLocal-definition evidence was not collected in this version 1 observation.\n',
      }],
      assertion_count: SOURCE_RULE_ARTIFACT.assertions.length,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Source' }));
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent('Local-definition evidence was not collected in this version 1 observation.');
    await expect(canvas.getByText(/Source observations only/)).toHaveTextContent('Complete execution predicates and consumer effects are not established.');
    await expect(canvas.getByText(SOURCE_RULE.summary)).toBeInTheDocument();
  },
};

export const SecurityFindings: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await userEvent.click(within(nav).getByRole('button', { name: /Security findings/ }));
    await expect(canvas.getByText('2 security findings')).toBeInTheDocument();
    await expect(canvas.getByText('Finding: No authentication check recovered: GET /admin')).toBeInTheDocument();
    await expect(
      canvas.getByText('Finding: Over-broad IAM grant: admin policy → orders/*'),
    ).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Source' }));
    const source = canvas.getByTestId('spec-artifact-source');
    await expect(source).toHaveTextContent('US-0015 / AC-0041');
    await expect(source).toHaveTextContent('US-0015 / AC-0042');
    await expect(source).toHaveTextContent('arn:aws:s3:::orders/*');
  },
};

export const AcceptRejectAndAnnotate: Story = {
  play: async ({ args, canvasElement }) => {
    // Scope to the inferred block: the confirmed block above it renders the
    // same three controls locked (#108), so unscoped queries are ambiguous.
    const block = within(canvasElement)
      .getByText('ADR: asynchronous fulfillment')
      .closest('li') as HTMLElement;
    const canvas = within(block);
    await userEvent.click(canvas.getByRole('button', { name: 'Accept' }));
    await expect(args.onCurate).toHaveBeenCalledWith(INFERRED, 'accepted', undefined);
    const annotation = canvas.getByLabelText('Annotation');
    await userEvent.type(annotation, 'Matched to the queue declaration');
    await userEvent.click(canvas.getByRole('button', { name: 'Annotate' }));
    await expect(args.onCurate).toHaveBeenLastCalledWith(
      INFERRED,
      'annotated',
      'Matched to the queue declaration',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Reject' }));
    await expect(args.onCurate).toHaveBeenLastCalledWith(
      INFERRED,
      'rejected',
      'Matched to the queue declaration',
    );
  },
};

export const ConfirmedBlocksAreLockedNotHidden: Story = {
  // #108: T0 lock semantics — Confirmed blocks show the curation controls
  // truly disabled (real in the a11y tree) with an inline explanation;
  // inferred blocks stay curatable; Gap blocks offer no curation at all.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('note')).toHaveTextContent(
      /Confirmed \(T0\/T1\) assertions are read-only/,
    );

    const locked = within(
      canvas.getByText('Capability: Place orders').closest('li') as HTMLElement,
    );
    for (const name of ['Accept', 'Reject', 'Annotate']) {
      const control = locked.getByRole('button', { name });
      await expect(control).toBeDisabled();
      await expect(control).toHaveAttribute('aria-disabled', 'true');
    }
    await expect(locked.getByText('Confirmed T0 — locked, read-only')).toBeInTheDocument();

    const curatable = within(
      canvas.getByText('ADR: asynchronous fulfillment').closest('li') as HTMLElement,
    );
    await expect(curatable.getByRole('button', { name: 'Accept' })).toBeEnabled();
    await expect(curatable.queryByText(/locked, read-only/)).not.toBeInTheDocument();

    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    // Per-doc chips name what the assertions actually are (#145 review):
    // recovered stories are one-per-US, but the dossier's assertions are
    // hops — never relabeled as flows. Registers read as alerts.
    await expect(within(nav).getByText('2 US')).toBeInTheDocument();
    await expect(within(nav).getByText('1 hops')).toBeInTheDocument();
    const gapChip = within(nav).getByRole('button', { name: /Gap register/ });
    await expect(gapChip.querySelector('.spec-doc-chip')).toHaveClass('alert');

    await userEvent.click(within(nav).getByRole('button', { name: /Gap register/ }));
    const gapBlock = within(
      canvas.getByText('Gap: runtime-computed channel identity').closest('li') as HTMLElement,
    );
    await expect(gapBlock.queryByRole('button')).not.toBeInTheDocument();
  },
};

export const CuratedSectionShowsTheBundleRevision: Story = {
  // AC-0655 (SPEC-12 slice 3): the Workbench shows the curated projection of
  // the bundle on screen, between the summary and the artifacts, and offers
  // a refresh.
  args: {
    bundle: {
      ...BUNDLE,
      curated_basis: { revision: CURATED_REVISION, counts: curatedReadFixture().counts },
    },
    curated: curatedReadFixture(),
    onRefreshCurated: fn(),
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    const section = within(canvas.getByRole('region', { name: 'Curated items' }));
    await expect(section.getByTestId('curated-revision')).toHaveAttribute(
      'title',
      args.bundle?.curated_basis?.revision,
    );
    // The records are closed until asked for, so the artifacts stay in view.
    await expect(section.getByRole('table', { name: 'Curated items (1)' })).not.toBeVisible();
    await expect(section.getByLabelText('Curated record counts')).toHaveTextContent('Accepted4');
    await userEvent.click(section.getByText(/^Records and decisions:/));
    await expect(section.getByRole('table', { name: 'Curated items (1)' })).toBeVisible();
    await expect(section.getByRole('table', { name: 'State stale (1)' })).toBeVisible();
    // The artifacts stay reachable below it.
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await expect(within(nav).getAllByRole('button')).toHaveLength(10);
    await userEvent.click(section.getByRole('button', { name: 'Refresh curated items' }));
    await expect(args.onRefreshCurated).toHaveBeenCalledOnce();
  },
};

export const CuratedSectionUnreadStatesNothing: Story = {
  // AC-0655: without a curated read the section says so and lists nothing.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('curated-unread')).toHaveTextContent('has not been read');
    await expect(canvas.queryByTestId('curated-revision')).not.toBeInTheDocument();
  },
};

// AC-1081 (T-1081): attested records sit apart from recovered content, with
// their own label (no tier badge), contributors and the engineer's review.
export const AttestedRecordsAreApartAndReviewable: Story = {
  args: {
    attested: {
      records: [
        {
          record_id: 'attested-v1:a2',
          citation_id: 'A-00000000000000a2',
          target: {
            kind: 'section',
            address: { catalogue_version: 'v1', document: 'D1', section: '1.3', scope: 'system' },
          },
          statement: 'Orders over the limit need a second approval.',
          standing: 'engineer_backed',
          label: 'attested',
          attestation_count: 1,
          contributors: [{ display_name: 'Placeholder Engineer', kind: 'engineer', verb: 'stated', run: null }],
        },
      ],
      onReview: fn(),
    },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Attested · 1')).toBeInTheDocument();
    const row = within(canvas.getByTestId('attested-row'));
    await expect(row.getByTestId('attested-label')).toHaveTextContent('attested');
    await expect(row.getByText('D1 §1.3')).toBeInTheDocument();
    await expect(row.getByTestId('attested-contributors')).toHaveTextContent('Placeholder Engineer stated');
    await userEvent.click(row.getByRole('button', { name: 'Withdraw' }));
    await expect(args.attested?.onReview).toHaveBeenCalled();
  },
};

export const VerifiedOnlyExport: Story = {
  args: {
    bundle: { ...BUNDLE, mode: 'verified-only' },
    mode: 'verified-only',
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'verified-only' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await expect(canvas.getByText('Unresolved findings: 1')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'best-effort' }));
    await expect(args.onModeChange).toHaveBeenCalledWith('best-effort');
    await userEvent.click(canvas.getByRole('button', { name: 'Export bundle' }));
    await expect(args.onExportBundle).toHaveBeenCalledWith(args.bundle);
  },
};

export const WithPersistedDecision: Story = {
  args: {
    decisions: [{
      assertion: {
        subject_id: INFERRED.subject_id,
        summary: INFERRED.summary,
        provenance: INFERRED.provenance,
      },
      decision: 'annotated',
      note: 'Matched to the queue declaration',
      updated_at: '2026-07-13T18:00:00Z',
    }],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('Annotated').length).toBeGreaterThan(0);
    await expect(canvas.getAllByText('Matched to the queue declaration').length).toBeGreaterThan(0);
  },
};

export const FoundRecoveredAndDrift: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      drift_count: 1,
      artifacts: BUNDLE.artifacts.map((item) => {
        if (item.id === 'adrs') {
          return {
            ...artifact('adrs', 'adrs.md', 'Architecture decisions', [FOUND_ADR, INFERRED]),
            content: '# Found and recovered ADRs\n\n## No synchronous order calls\n\n**Origin:** found\n\n## Recovered: asynchronous fulfillment\n\n**Origin:** recovered\n**Status:** Recovered / Inferred\n',
          };
        }
        if (item.id === 'drift-register') {
          return {
            ...artifact('drift-register', 'drift_register.md', 'Drift register', [DRIFT]),
            content: '# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n| No synchronous order calls | adr:found:no-sync | sym:handler CALLS sym:remote | ep:orders | Confirmed |\n',
          };
        }
        return item;
      }),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await userEvent.click(within(nav).getByRole('button', { name: /Architecture decisions/ }));
    await expect(canvas.getByText('ADR: No synchronous order calls')).toBeInTheDocument();
    await expect(canvas.getByText('ADR: asynchronous fulfillment')).toBeInTheDocument();
    await expect(canvas.getByText('t0.adr-markdown')).toBeInTheDocument();
    await userEvent.click(within(nav).getByRole('button', { name: /Drift register/ }));
    await userEvent.click(canvas.getByRole('button', { name: 'Source' }));
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent(
      'sym:handler CALLS sym:remote',
    );
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent('ep:orders');
  },
};

export const TruncatedArtifactShowsCountedNote: Story = {
  // AC-0218 (#488): export_spec caps content/assertions per artifact before
  // they cross IPC. The Workbench must show a truncated preview with an
  // explicit counted note rather than rendering (or silently dropping) the
  // full multi-MB artifact, and the nav chip must still count every real
  // instance, not just the ones sent over IPC.
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: [
        {
          ...artifact('gap-register', 'gap_register.md', 'Gap register', [GAP]),
          content: 'first '.repeat(20),
          content_truncated: true,
          content_byte_len: 32_000_000,
          assertions_truncated: true,
          assertions_total: 4_200,
        },
        ...BUNDLE.artifacts.filter((item) => item.id !== 'gap-register'),
      ],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    // The chip counts the true total (#488), not the one assertion the IPC
    // preview actually carried.
    const gapChip = within(nav).getByRole('button', { name: /Gap register/ });
    await expect(within(gapChip).getByText('4200 assertion rows')).toBeInTheDocument();

    await userEvent.click(within(nav).getByRole('button', { name: /Gap register/ }));
    await userEvent.click(canvas.getByRole('button', { name: 'Source' }));
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent(
      'first first first',
    );
    const contentNote = canvas.getByTestId('spec-artifact-truncated-note');
    await expect(contentNote).toHaveTextContent('32,000,000');
    await expect(contentNote).toHaveTextContent('Copy artifact or Export bundle');

    const assertionsNote = canvas.getByTestId('spec-assertions-truncated-note');
    await expect(assertionsNote).toHaveTextContent('1');
    await expect(assertionsNote).toHaveTextContent('4,200');
    await expect(canvas.getByText('4200 in this artifact')).toBeInTheDocument();
  },
};

export const Empty: Story = {
  args: { bundle: null, canCurate: false },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText(/No compiled spec is available/)).toBeInTheDocument();
  },
};

const WORKBENCH_VIEWS: SpecViewBundle = {
  mode: 'best-effort',
  topology: {
    artifact_id: 'topology',
    schema: 'cartograph.spec/topology.md@1',
    content_id: 'spec-artifact-v1:fixture-topology.md',
    mode: 'best-effort',
    nodes: [
      {
        id: 'res:aws_sqs_queue.orders',
        label: 'Resource',
        props: {
          name: 'aws_sqs_queue.orders',
          prov: {
            tier: 'Dynamic',
            confidence_tier: 'Confirmed',
            evidence: [
              {
                repo: 'acme/infra',
                path: 'orders.tf',
                byte_start: 10,
                byte_end: 88,
                commit_sha: 'def456',
              },
            ],
            extractor_id: 't1.aws-describe',
            content_hash: '8'.repeat(64),
          },
        },
      },
      {
        id: 'gap:channel',
        label: 'Gap',
        props: {
          name: 'runtime-computed channel identity',
          reason: 'channel ARN constructed dynamically',
          prov: GAP.provenance,
        },
      },
    ],
    nodes_total: 2,
    edges: [
      {
        src: 'res:aws_sqs_queue.orders',
        dst: 'gap:channel',
        label: 'TRIGGERS',
        props: {
          gap_reason: 'channel ARN constructed dynamically',
          prov: GAP.provenance,
        },
      },
    ],
    edges_total: 1,
    flows_total: 0,
    deployments: [
      {
        resource_id: 'res:aws_sqs_queue.orders',
        source: 't1.aws-describe',
        provenance: {
          tier: 'Dynamic',
          confidence_tier: 'Confirmed',
          evidence: [
            {
              repo: 'acme/infra',
              path: 'orders.tf',
              byte_start: 10,
              byte_end: 88,
              commit_sha: 'def456',
            },
          ],
          extractor_id: 't1.aws-describe',
          content_hash: '8'.repeat(64),
        },
        backed_channels: ['orders.queue'],
      },
    ],
    truncated: false,
  },
  flows: {
    artifact_id: 'flow-dossiers',
    schema: 'cartograph.spec/flow_dossiers.md@1',
    content_id: 'spec-artifact-v1:fixture-flow_dossiers.md',
    mode: 'best-effort',
    nodes: [
      {
        id: 'ep:POST:/orders',
        label: 'Endpoint',
        props: {
          name: 'POST /orders',
          prov: CONFIRMED.provenance,
        },
      },
      {
        id: 'gap:channel',
        label: 'Gap',
        props: {
          name: 'runtime-computed channel identity',
          reason: 'channel ARN constructed dynamically',
          prov: GAP.provenance,
        },
      },
    ],
    nodes_total: 2,
    edges: [
      {
        src: 'ep:POST:/orders',
        dst: 'gap:channel',
        label: 'PUBLISHES',
        props: {
          gap_reason: 'channel ARN constructed dynamically',
          prov: GAP.provenance,
        },
      },
    ],
    edges_total: 1,
    flows_total: 1,
    truncated: false,
  },
  data_model: {
    artifact_id: 'data-model',
    schema: 'cartograph.spec/data_model.md@1',
    content_id: 'spec-artifact-v1:fixture-data_model.md',
    mode: 'best-effort',
    nodes: [
      {
        id: 'data:orders',
        label: 'DataEntity',
        props: {
          name: 'orders',
          prov: CONFIRMED.provenance,
        },
      },
      {
        id: 'sym:orders#create',
        label: 'Symbol',
        props: {
          name: 'createOrder',
          prov: CONFIRMED.provenance,
        },
      },
    ],
    nodes_total: 2,
    edges: [
      {
        src: 'sym:orders#create',
        dst: 'data:orders',
        label: 'WRITES',
        props: {
          prov: CONFIRMED.provenance,
        },
      },
    ],
    edges_total: 1,
    flows_total: 0,
    truncated: false,
  },
};

export const RenderedViewsMatchExportedIdentityAndSelectEvidence: Story = {
  // AC-0246, AC-0247 (T-0246, T-0247): topology, flow, and data-model views in
  // SpecWorkbench render as interactive diagrams bound to the exported
  // projection's schema + content_id, while keeping copyable source available
  // as a secondary action and opening evidence/tier/confidence on selection.
  args: {
    views: WORKBENCH_VIEWS,
    onSelectNode: fn(),
    onSelectEdge: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });

    // 1. Resource topology view
    await userEvent.click(within(nav).getByRole('button', { name: /Resource topology/ }));
    const topoDiagram = within(canvas.getByTestId('spec-diagram-topology'));
    await expect(topoDiagram.getByTestId('spec-diagram-canvas')).toBeInTheDocument();
    await expect(topoDiagram.getByTestId('diagram-content-identity')).toHaveAttribute(
      'data-content-id',
      'spec-artifact-v1:fixture-topology.md',
    );
    await expect(topoDiagram.getByTestId('diagram-deployments')).toHaveTextContent(
      'DEPLOYED (t1.aws-describe)',
    );
    const gapEdge = within(topoDiagram.getByLabelText('Diagram edges and hops')).getByRole(
      'button',
      { name: /GAP TRIGGERS \[UNRESOLVED: channel ARN constructed dynamically\]/ },
    );
    await expect(gapEdge).toHaveClass('unresolved');
    await userEvent.click(gapEdge);
    await expect(args.onSelectEdge).toHaveBeenCalledWith(WORKBENCH_VIEWS.topology.edges[0]);
    await expect(topoDiagram.getByTestId('diagram-selected-evidence')).toHaveTextContent(
      'acme/shop:src/publish.ts',
    );

    // Copyable source remains available as a secondary action.
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent(
      '# Resource topology',
    );

    // 2. Flow dossiers view
    await userEvent.click(within(nav).getByRole('button', { name: /Flow dossiers/ }));
    const flowDiagram = within(canvas.getByTestId('spec-diagram-flow-dossiers'));
    await expect(flowDiagram.getByTestId('spec-diagram-canvas')).toBeInTheDocument();
    await expect(flowDiagram.getByTestId('diagram-content-identity')).toHaveAttribute(
      'data-content-id',
      'spec-artifact-v1:fixture-flow_dossiers.md',
    );

    // 3. Data model view
    await userEvent.click(within(nav).getByRole('button', { name: /Data model/ }));
    const dataDiagram = within(canvas.getByTestId('spec-diagram-data-model'));
    await expect(dataDiagram.getByTestId('spec-diagram-canvas')).toBeInTheDocument();
    await expect(dataDiagram.getByTestId('diagram-content-identity')).toHaveAttribute(
      'data-content-id',
      'spec-artifact-v1:fixture-data_model.md',
    );
    const dataNode = within(dataDiagram.getByLabelText('Diagram nodes')).getByRole('button', {
      name: /T0 · orders/,
    });
    await userEvent.click(dataNode);
    await expect(args.onSelectNode).toHaveBeenCalledWith(WORKBENCH_VIEWS.data_model.nodes[0]);
    await expect(dataDiagram.getByTestId('diagram-selected-evidence')).toHaveTextContent(
      'Confirmed',
    );
  },
};

export const MarkdownArtifactsRenderByDefault: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: BUNDLE.artifacts.map((item) =>
        item.id === 'security-view'
          ? {
              ...item,
              content:
                '# Security findings\n\nInspect `GET /admin` exposure.\n\n| Finding | Confidence |\n|---|---|\n| No authentication check recovered: GET /admin | Confirmed |\n',
            }
          : item,
      ),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await userEvent.click(within(nav).getByRole('button', { name: /Security findings/ }));

    const renderedBtn = canvas.getByRole('button', { name: 'Rendered' });
    const sourceBtn = canvas.getByRole('button', { name: 'Source' });
    await expect(renderedBtn).toHaveAttribute('aria-pressed', 'true');
    await expect(sourceBtn).toHaveAttribute('aria-pressed', 'false');

    const rendered = within(canvas.getByTestId('spec-artifact-rendered'));
    await expect(rendered.getByRole('heading', { name: 'Security findings' })).toBeInTheDocument();
    const table = rendered.getByRole('table');
    await expect(table).toHaveTextContent('No authentication check recovered: GET /admin');
    await expect(table).toHaveTextContent('Confirmed');
    await expect(rendered.getByText('GET /admin').tagName).toBe('CODE');

    await userEvent.click(sourceBtn);
    await expect(sourceBtn).toHaveAttribute('aria-pressed', 'true');
    await expect(renderedBtn).toHaveAttribute('aria-pressed', 'false');
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent(
      '| Finding | Confidence |',
    );

    await userEvent.click(renderedBtn);
    await expect(renderedBtn).toHaveAttribute('aria-pressed', 'true');
    await expect(
      within(canvas.getByTestId('spec-artifact-rendered')).getByRole('table'),
    ).toHaveTextContent('No authentication check recovered: GET /admin');
  },
};


// AC-0067, AC-0218: one unresolved fact can have many supporting rows.
export const FindingAndAssertionRowCountsAreDistinct: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: [{
        ...artifact('gap-register', 'gap_register.md', 'Gap register', [GAP]),
        assertions_total: 13,
        assertions_truncated: true,
      }],
      assertion_count: 13,
      gap_count: 1,
      drift_count: 0,
      security_count: 0,
    },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Unresolved findings: 1')).toBeInTheDocument();
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await expect(within(nav).getByText('13 assertion rows')).toBeInTheDocument();
    await expect(canvas.getByText(/Assertion rows include supporting/)).toHaveTextContent(
      'can refer to the same finding',
    );
    await expect(canvas.getByTestId('spec-assertions-truncated-note')).toHaveTextContent(
      'Showing the first 1 of 13 assertions',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Export bundle' }));
    await expect(args.onExportBundle).toHaveBeenCalledWith(args.bundle);
  },
};
