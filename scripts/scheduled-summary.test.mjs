// #1770: the scheduled macOS lane's summary names what failed. Pure.
import test from 'node:test';
import assert from 'node:assert/strict';

import { failureLines, parseStep, summarize } from './scheduled-summary.mjs';

test('a step is label, outcome and optional log', () => {
  assert.deepEqual(parseStep('Workspace=success=/tmp/w.log'), { label: 'Workspace', outcome: 'success', log: '/tmp/w.log' });
  assert.deepEqual(parseStep('Pin=='), { label: 'Pin', outcome: 'skipped', log: '' });
  assert.throws(() => parseStep('Pin'), /label/u);
});

test('failed tests and panics are quoted ahead of the log tail', () => {
  const log = ['running 3 tests', 'test a::one ... ok', 'test a::two ... FAILED', "thread 'a::two' panicked at src/a.rs:9:5:", 'failures:', 'test result: FAILED. 1 passed; 1 failed'].join('\n');
  const lines = failureLines(log);
  assert.equal(lines[0], 'test a::two ... FAILED');
  assert.match(lines[1], /panicked at/u);
  assert.equal(lines.at(-1), 'test result: FAILED. 1 passed; 1 failed');
});

test('the lane passes only when every part succeeded; a skipped part does not pass', () => {
  const logs = { '/w': 'test x ... FAILED\n' };
  const failed = summarize('Lane', [parseStep('A=success='), parseStep('B=failure=/w'), parseStep('C=skipped=')], (file) => logs[file] ?? '');
  assert.equal(failed.passed, false);
  assert.match(failed.text, /\| B \| failure \|/u);
  assert.match(failed.text, /### B\n\n```text\ntest x \.\.\. FAILED/u);
  assert.equal(summarize('Lane', [parseStep('A=success=')]).passed, true);
  assert.equal(summarize('Lane', [parseStep('A=success='), parseStep('C=skipped=')]).passed, false);
});
