import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { NOT_COMPUTED_READ, curatedReadFixture } from './curatedFixtures';
import {
  curatedDraftStates, curatedReadStatesDrafts, scopeLabel, sectionDraftQuestion,
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
});
