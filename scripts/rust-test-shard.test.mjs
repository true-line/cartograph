import assert from 'node:assert/strict';
import test from 'node:test';
import { DEFAULT_WEIGHT, loadWeights, parseShard, parseTestList, parseTotals, selectShard, verifyRun, weightsFromReport } from './rust-test-shard.mjs';

const listing = [
  'jobs::tests::b: test',
  'investigations::worker::tests::slow_two: test',
  'investigations::worker::tests::slow_one: test',
  'a::bench_it: benchmark',
  'jobs::tests::a: test',
  'paths::tests::only: test',
  '',
  '5 tests, 1 benchmark',
].join('\n');

test('a shard is <index>/<count> with the index inside the count', () => {
  assert.deepEqual(parseShard('2/4'), { index: 2, count: 4 });
  for (const bad of ['0/4', '5/4', '1/0', '1', '1/4/2', ' 1/4', '-1/4', undefined]) {
    assert.throws(() => parseShard(bad), /shard/u, String(bad));
  }
});

test('the test list is sorted, holds tests only, and refuses duplicates', () => {
  assert.deepEqual(parseTestList(listing), [
    'investigations::worker::tests::slow_one',
    'investigations::worker::tests::slow_two',
    'jobs::tests::a',
    'jobs::tests::b',
    'paths::tests::only',
  ]);
  assert.throws(() => parseTestList('a: test\na: test\n'), /duplicate/u);
});

test('shards partition the list and split neighbouring tests (#1924)', () => {
  const names = Array.from({ length: 1263 }, (_, n) => `m::t${String(n).padStart(4, '0')}`);
  for (const count of [1, 2, 4, 7]) {
    const shards = Array.from({ length: count }, (_, i) => selectShard(names, { index: i + 1, count }));
    assert.deepEqual(shards.flat().sort(), names);
    assert.equal(new Set(shards.flat()).size, names.length);
    const sizes = shards.map((shard) => shard.length);
    assert.ok(Math.max(...sizes) - Math.min(...sizes) <= 1);
  }
  const [first, second] = [1, 2].map((index) => selectShard(parseTestList(listing), { index, count: 2 }));
  assert.ok(first.includes('investigations::worker::tests::slow_one'));
  assert.ok(second.includes('investigations::worker::tests::slow_two'));
});

const result = (passed, failed, ignored) =>
  `running ${passed + failed + ignored} tests\n` +
  `test result: ${failed ? 'FAILED' : 'ok'}. ${passed} passed; ${failed} failed; ${ignored} ignored; 0 measured; 900 filtered out; finished in 1.00s\n`;

test('a shard passes only when libtest accounts for exactly its selection', () => {
  const selected = ['a', 'b', 'c'];
  assert.deepEqual(parseTotals(result(2, 0, 1)), { ran: 3, failed: 0, results: 1 });
  assert.equal(verifyRun(selected, result(2, 0, 1), 0), null);
  assert.match(verifyRun(selected, result(2, 0, 0), 0), /selected 3 tests but libtest accounted for 2/u);
  assert.match(verifyRun(selected, result(4, 0, 0), 0), /accounted for 4/u);
  assert.match(verifyRun(selected, result(2, 1, 0), 101), /exited 101 with 1 failed/u);
  assert.match(verifyRun(selected, result(3, 0, 0), 1), /exited 1/u);
  assert.match(verifyRun(selected, '', 0), /one test binary result, saw 0/u);
  assert.match(verifyRun(selected, result(3, 0, 0) + result(0, 0, 0), 0), /saw 2/u);
});

test('measured weights spread the slow tests and still partition the list (#1924)', () => {
  const names = Array.from({ length: 40 }, (_, n) => `m::t${String(n).padStart(2, '0')}`);
  // The four slowest tests sit on one shard under round-robin by name.
  const weights = { 'm::t00': 90, 'm::t04': 60, 'm::t08': 40, 'm::t12': 38 };
  assert.deepEqual(selectShard(names, { index: 1, count: 4 }).filter((name) => name in weights), Object.keys(weights));
  const shards = [1, 2, 3, 4].map((index) => selectShard(names, { index, count: 4 }, weights));
  assert.deepEqual(shards.flat().sort(), names);
  assert.equal(new Set(shards.flat()).size, names.length);
  for (const shard of shards) assert.equal(shard.filter((name) => name in weights).length, 1);
  const loads = shards.map((shard) => shard.reduce((sum, name) => sum + (weights[name] ?? DEFAULT_WEIGHT), 0));
  assert.ok(Math.max(...loads) - Math.min(...loads) <= 90 - 38 + DEFAULT_WEIGHT * names.length);
  // Each shard keeps the sorted order libtest is given.
  for (const shard of shards) assert.deepEqual(shard, [...shard].sort());
});

test('weights come from libtest\'s JSON report and only list tests of two seconds or more', () => {
  const report = [
    '{ "type": "suite", "event": "started", "test_count": 3 }',
    '{ "type": "test", "event": "started", "name": "b::slow" }',
    '{ "type": "test", "name": "b::slow", "event": "ok", "exec_time": 12.345 }',
    '{ "type": "test", "name": "a::quick", "event": "ok", "exec_time": 0.2 }',
    '{ "type": "test", "name": "a::fail", "event": "failed", "exec_time": 30 }',
    '{ "type": "test", "name": "a::slow", "event": "ok", "exec_time": 2 }',
    '{ "type": "test", "name": "a::second", "event": "ok", "exec_time": 1.9 }',
    'not json',
  ].join('\n');
  assert.equal(weightsFromReport(report), `${JSON.stringify({ seconds: { 'a::slow': 2, 'b::slow': 12.3 } }, null, 2)}\n`);
});

test('the committed weights name tests in sorted order with positive seconds', () => {
  const weights = loadWeights();
  const names = Object.keys(weights);
  assert.ok(names.length > 0);
  assert.deepEqual(names, [...names].sort());
  for (const name of names) assert.ok(weights[name] >= 2, name);
});
