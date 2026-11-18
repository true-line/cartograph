import assert from 'node:assert/strict';
import test from 'node:test';
import { parseShard, parseTestList, parseTotals, selectShard, verifyRun } from './rust-test-shard.mjs';

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
