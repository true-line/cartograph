import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import {
  answerText, areaAskPreview, loadAreaChoices, refusalText, runAreaAsk, turnsOf, usageText,
} from './areaExpert';
import type { AskOutcome } from './areaExpert';

// AC-0932 (T-0932): the topic expert's host calls and the turns sent back.
const ANSWER: AskOutcome = {
  outcome: 'answered', area: 'subsystem:cluster:alias', area_name: 'Alias', role: 'topic-expert@1',
  job_id: '7', usage: { model: 'claude-opus', input_tokens: 24120, output_tokens: 512, request_bytes: 99000 },
  statements: [{ statement: 'A deleted alias goes to the trash.', basis: 'stated', citations: ['F-00000000000000a1'] }],
  not_known: ['How long the trash is kept.'], ask_instead: ['Mailbox'], left_out: 0,
};
const UNCOUNTED = { model: 'scripted-model', input_tokens: null, output_tokens: null, request_bytes: 2049 };
const PREVIEW = {
  provider_id: 'anthropic', locality: 'Cloud' as const, tier: 'Agentic' as const,
  action_id: 'area-ask:subsystem:cluster:alias',
  payload: { system: 's', prompt: 'p', spans: [] }, payload_hash: 'hash-1', redaction_count: 0,
};

describe('area expert', () => {
  beforeEach(() => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  });
  afterEach(() => {
    clearMocks();
    vi.unstubAllGlobals();
  });

  it('previews, then asks with the approved payload and the earlier turns', async () => {
    const commands: [string, unknown][] = [];
    mockIPC((command, args) => {
      commands.push([command, args]);
      if (command === 'area_choices') return [{ area: ANSWER.area, name: 'Alias', files: 12, sections: ['1.3'], briefing_bytes: 900 }];
      if (command === 'area_ask_preview') return PREVIEW;
      return ANSWER;
    });
    expect((await loadAreaChoices()).map((choice) => choice.name)).toEqual(['Alias']);
    const turns = turnsOf([
      { question: 'What is deleted?', outcome: ANSWER },
      { question: 'And then?', outcome: { outcome: 'no_answer', reason: 'reply_not_the_schema', usage: UNCOUNTED } },
    ]);
    expect(turns).toEqual([{
      question: 'What is deleted?',
      answer: 'A deleted alias goes to the trash.\nNot known: How long the trash is kept.\nAsk instead: Mailbox',
    }]);
    expect(await areaAskPreview(ANSWER.area, 'Who may delete?', turns)).toEqual(PREVIEW);
    expect(await runAreaAsk(ANSWER.area, 'Who may delete?', turns, PREVIEW)).toEqual(ANSWER);
    expect(commands.slice(1)).toEqual([
      ['area_ask_preview', { area: ANSWER.area, question: 'Who may delete?', turns }],
      ['run_area_ask', {
        area: ANSWER.area, question: 'Who may delete?', turns, mode: 'cloud', approvedPayloadHash: 'hash-1',
      }],
    ]);
  });

  it('puts a closed refusal into words and keeps an unknown one as sent', () => {
    expect(refusalText('area_not_found')).toBe('This area is no longer in the knowledge overlay. Reload the areas.');
    expect(refusalText('provider_unreachable')).toBe('provider_unreachable');
    expect(answerText({ outcome: 'no_answer', reason: 'reply_not_the_schema', usage: UNCOUNTED })).toBe('');
  });

  it('states what a call used as the provider reported it', () => {
    expect(usageText(ANSWER.outcome === 'answered' ? ANSWER.usage : UNCOUNTED))
      .toBe('claude-opus: 24,120 input and 512 output tokens, as the provider reported.');
    expect(usageText(UNCOUNTED)).toBe('scripted-model: 3 KB sent; the provider reported no token count.');
  });
});
