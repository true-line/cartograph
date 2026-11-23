import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { NOT_COMPUTED_READ, curatedReadFixture } from './curatedFixtures';
import {
  composeOutcomeText, curatedDraftStates, curatedReadStatesDrafts, runSectionCompose,
  gatedRunText, runSectionComposeGated, scopeLabel,
  sectionComposePreview, sectionDraftQuestion, sectionRedraftQuestion,
  useSectionDocumentStore,
} from './sectionDocument';
import {
  ELIGIBLE_DRAFT, ELIGIBLE_ROW, PENDING_DRAFT, STALE_DRAFT, curatedReadWithDrafts,
  sectionDocumentFixture,
} from './sectionDocumentFixtures';

// SPEC-15 slice 7, AC-0669: the document view is the host's, a review is a
// compare-and-set the host confirms, and an accepted draft's state comes
// only from the curated read.

beforeEach(() => {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  useSectionDocumentStore.setState({ document: null, busy: false, error: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

describe('accepted draft states from the curated read (AC-0669)', () => {
  it('lists only the drafts the host returned, by draft id', () => {
    const states = curatedDraftStates(curatedReadWithDrafts());
    expect(states.size).toBe(2);
    expect(states.get(ELIGIBLE_DRAFT.draft_id)).toEqual(ELIGIBLE_ROW);
    expect(states.get(STALE_DRAFT.draft_id)?.state).toBe('stale');
    expect(states.get(PENDING_DRAFT.draft_id)).toBeUndefined();
  });

  it('states nothing when the read is absent, not computed or predates drafts', () => {
    expect(curatedDraftStates(null).size).toBe(0);
    expect(curatedReadStatesDrafts(null)).toBe(false);
    expect(curatedDraftStates(NOT_COMPUTED_READ).size).toBe(0);
    expect(curatedReadStatesDrafts(NOT_COMPUTED_READ)).toBe(false);
    expect(curatedDraftStates(curatedReadFixture()).size).toBe(0);
    expect(curatedReadStatesDrafts(curatedReadFixture())).toBe(false);
    expect(curatedReadStatesDrafts(curatedReadWithDrafts([]))).toBe(true);
  });

  it('names a scope in words', () => {
    expect(scopeLabel('system')).toBe('Whole system');
    expect(scopeLabel('F-00000000000000c3')).toBe('Subject F-00000000000000c3');
  });
});

describe('document view store (AC-0669)', () => {
  it('loads the host document view', async () => {
    const commands: [string, unknown][] = [];
    mockIPC((command, args) => {
      commands.push([command, args]);
      return sectionDocumentFixture();
    });
    await useSectionDocumentStore.getState().load();
    const state = useSectionDocumentStore.getState();
    expect(commands).toEqual([['section_document', { document: 'D1' }]]);
    expect(state.document?.sections).toHaveLength(8);
    expect(state.busy).toBe(false);
    expect(state.error).toBeNull();
  });

  it('keeps the last document and reports the error when a read fails', async () => {
    useSectionDocumentStore.setState({ document: sectionDocumentFixture() });
    mockIPC(() => { throw new Error('draft store: locked'); });
    await useSectionDocumentStore.getState().load();
    const state = useSectionDocumentStore.getState();
    expect(state.document?.document).toBe('D1');
    expect(state.error).toContain('draft store: locked');
    expect(state.busy).toBe(false);
  });

  it('records a review at the revision read, then reads the document again', async () => {
    const commands: [string, unknown][] = [];
    const accepted = { ...PENDING_DRAFT, review_revision: 1, review_decision: 'accepted' as const };
    mockIPC((command, args) => {
      commands.push([command, args]);
      if (command === 'review_section_draft') return accepted;
      return sectionDocumentFixture([accepted]);
    });
    const recorded = await useSectionDocumentStore.getState().review(PENDING_DRAFT, 'accepted', 'Checked.');
    expect(recorded).toBe(true);
    expect(commands).toEqual([
      ['review_section_draft', {
        draftId: PENDING_DRAFT.draft_id, expectedRevision: 0, decision: 'accepted', note: 'Checked.',
      }],
      ['section_document', { document: 'D1' }],
    ]);
    const state = useSectionDocumentStore.getState();
    expect(state.error).toBeNull();
    expect(state.document?.sections[2].drafts[0].review_decision).toBe('accepted');
  });

  it('reports a refused review and still reads the document again', async () => {
    const commands: string[] = [];
    mockIPC((command) => {
      commands.push(command);
      if (command === 'review_section_draft') throw new Error('stale review revision: expected 0, current 1');
      return sectionDocumentFixture();
    });
    const recorded = await useSectionDocumentStore.getState().review(PENDING_DRAFT, 'rejected', null);
    expect(recorded).toBe(false);
    expect(commands).toEqual(['review_section_draft', 'section_document']);
    expect(useSectionDocumentStore.getState().error).toContain('stale review revision');
    expect(useSectionDocumentStore.getState().document).not.toBeNull();
  });

  it('does not accept a confirmation for another draft or revision', async () => {
    mockIPC((command) => {
      if (command === 'review_section_draft') return { ...PENDING_DRAFT, review_revision: 5, review_decision: 'accepted' };
      return sectionDocumentFixture();
    });
    expect(await useSectionDocumentStore.getState().review(PENDING_DRAFT, 'accepted', null)).toBe(false);
    expect(useSectionDocumentStore.getState().error).toContain('did not confirm this review');
  });

  it('asks the host for the one drafting question of an address', async () => {
    const commands: [string, unknown][] = [];
    mockIPC((command, args) => {
      commands.push([command, args]);
      return 'Draft section 1.3 "Capabilities" of document D1 (catalogue 1.0) for scope system.';
    });
    const question = await sectionDraftQuestion({
      catalogue_version: '1.0', document: 'D1', section: '1.3', scope: 'system',
    });
    expect(question).toContain('Draft section 1.3');
    expect(commands).toEqual([['section_draft_question', {
      catalogueVersion: '1.0', document: 'D1', section: '1.3', scope: 'system',
    }]]);
  });

  // AC-0674: the re-draft request names the draft; the host decides.
  it('asks the host for the re-draft question of one accepted draft', async () => {
    const commands: [string, unknown][] = [];
    mockIPC((command, args) => {
      commands.push([command, args]);
      return 'Draft section 1.3 "Capabilities" of document D1 (catalogue 1.0) for scope system.';
    });
    expect(await sectionRedraftQuestion(STALE_DRAFT)).toContain('Draft section 1.3');
    expect(commands).toEqual([['section_redraft_question', { draftId: STALE_DRAFT.draft_id }]]);
  });

  it('puts a refused re-draft into words and keeps an unknown reason as sent', async () => {
    mockIPC(() => { throw 'successor_pending'; });
    await expect(sectionRedraftQuestion(STALE_DRAFT)).rejects.toThrow(
      'A re-draft of this draft is already stored. Review it first.',
    );
    mockIPC(() => { throw 'draft_not_stale'; });
    await expect(sectionRedraftQuestion(STALE_DRAFT)).rejects.toThrow('nothing to re-draft');
    mockIPC(() => { throw 'store is locked'; });
    await expect(sectionRedraftQuestion(STALE_DRAFT)).rejects.toThrow('store is locked');
  });
});

// #1991, AC-0862: a composition shows the exact payload first and runs only
// for the payload hash the user approved.
describe('section composition (AC-0862)', () => {
  const address = { catalogue_version: '1.0', document: 'D1', section: '1.5', scope: 'system' };
  const preview = {
    provider_id: 'anthropic', locality: 'Cloud' as const, tier: 'Agentic' as const,
    action_id: 'section-compose:D1:1.5:system',
    payload: { system: 's', prompt: 'p', spans: [] }, payload_hash: 'hash-1', redaction_count: 0,
  };

  it('previews and then runs the approved payload for the address', async () => {
    const commands: [string, unknown][] = [];
    mockIPC((command, args) => {
      commands.push([command, args]);
      if (command === 'section_compose_preview') return preview;
      return { outcome: 'staged', draft_id: 'draft-1', claims: 12, left_out: 1 };
    });
    expect(await sectionComposePreview(address)).toEqual(preview);
    const outcome = await runSectionCompose(address, preview);
    expect(composeOutcomeText(outcome)).toBe('Composed 12 claims for review. 1 claim was left out.');
    expect(commands).toEqual([
      ['section_compose_preview', { catalogueVersion: '1.0', document: 'D1', section: '1.5', scope: 'system' }],
      ['run_section_compose', {
        catalogueVersion: '1.0', document: 'D1', section: '1.5', scope: 'system',
        mode: 'cloud', approvedPayloadHash: 'hash-1',
      }],
    ]);
  });

  it('puts a closed refusal into words and keeps an unknown one as sent', () => {
    expect(composeOutcomeText({ outcome: 'no_draft', reason: 'graph_changed', claims: 0, left_out: 0 }))
      .toBe('The graph changed while the section was being composed. Compose it again.');
    expect(composeOutcomeText({ outcome: 'no_draft', reason: 'citation_not_admitted', claims: 0, left_out: 2 }))
      .toBe('citation_not_admitted 2 claims were left out.');
  });
});

// #2083, AC-0980: gated §1.5 drafting runs on the local model with no
// consent step and reports its coverage.
describe('gated section composition (AC-0980)', () => {
  const address = { catalogue_version: '1.0', document: 'D1', section: '1.5', scope: 'system' };
  const gated = {
    chunks: 3, rows: 24, gap_rows_skipped: 0, repeated_rows_skipped: 0, rows_covered: 23, calls: 9, first_try_passed: 1, retries: 2,
    sent_back: 1, audit_dropped: 0, audit_failed: 0, failures: {},
  };

  it('runs the gated command for the address and says how much it covered', async () => {
    const commands: [string, unknown][] = [];
    mockIPC((command, args) => {
      commands.push([command, args]);
      return [{ scope: 'system', area: null, outcome: 'staged', draft_id: 'draft-2', claims: 14, left_out: 1, gated }];
    });
    const drafts = await runSectionComposeGated(address);
    expect(gatedRunText(drafts)).toBe(
      'Composed 14 claims for review. 1 claim was left out. 23 of 24 rule rows covered in 9 local calls.',
    );
    expect(commands).toEqual([
      ['run_section_compose_gated', { catalogueVersion: '1.0', document: 'D1', section: '1.5', scope: 'system' }],
    ]);
  });

  it('sums a run drafted area by area and names the areas that staged nothing', () => {
    const drafts = [
      { scope: 'F-00000000000000a1', area: 'Alias', outcome: 'staged' as const, draft_id: 'd1', claims: 20, left_out: 0, gated },
      { scope: 'F-00000000000000a2', area: 'Phone', outcome: 'no_draft' as const, reason: 'no_claim_admitted', claims: 0, left_out: 2, gated },
      { scope: 'system', area: null, outcome: 'staged' as const, draft_id: 'd2', claims: 3, left_out: 0, gated },
    ];
    expect(gatedRunText(drafts)).toBe(
      'Composed 23 claims in 2 drafts for review, by area. Nothing was staged for Phone. '
      + '69 of 72 rule rows covered in 27 local calls.',
    );
  });

  it('puts the gated refusals into words', () => {
    expect(composeOutcomeText({ outcome: 'no_draft', reason: 'no_claim_admitted', claims: 0, left_out: 3, gated }))
      .toBe('No drafted claim passed the gates, so nothing was staged. 3 claims were left out. '
        + '23 of 24 rule rows covered in 9 local calls.');
  });
});
