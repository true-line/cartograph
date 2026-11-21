// #1768: one tracking issue per scheduled lane. Pure; no network.
import test from 'node:test';
import assert from 'node:assert/strict';

import { BOT, findIssue, plan, withRun } from './scheduled-issue.mjs';

const issue = (fields) => ({ number: 1, state: 'open', title: 'Scheduled: x', user: { login: BOT }, ...fields });

test('the lane issue is the open one the Actions bot opened with the exact title', () => {
  assert.equal(findIssue([issue({ number: 7 })], 'Scheduled: x').number, 7);
  assert.equal(findIssue([issue({ user: { login: 'someone' } })], 'Scheduled: x'), null);
  assert.equal(findIssue([issue({ title: 'Scheduled: x (copy)' })], 'Scheduled: x'), null);
  assert.equal(findIssue([issue({ pull_request: {} })], 'Scheduled: x'), null);
  assert.equal(findIssue([issue({ state: 'closed' })], 'Scheduled: x'), null);
});

test('a miss opens or comments; a pass closes an open issue and otherwise does nothing', () => {
  assert.equal(plan('open', null), 'create');
  assert.equal(plan('open', issue()), 'comment');
  assert.equal(plan('close', issue()), 'close');
  assert.equal(plan('close', null), 'none');
  assert.throws(() => plan('reopen', null), /open or close/u);
});

test('the body links its run and stays within the API limit', () => {
  assert.match(withRun('report\n', 'https://example.test/run/1'), /report\n\nRun: https:\/\/example\.test\/run\/1\n$/u);
  assert.ok(withRun('x'.repeat(70_000), '').length < 61_000);
});
