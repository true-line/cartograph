import assert from 'node:assert/strict';
import test from 'node:test';
import { DEFAULT_WEIGHT, groupOf, loadGroups, loadWeights, parseShard, parseTestList, parseTotals, selectShard, parseExecutable, parseTimes, verifyCoverage, verifyRun, weightsFromTimes } from './rust-test-shard.mjs';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

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

test('libtest --report-time lines give each passed test its seconds', () => {
  const output = [
    'running 3 tests',
    'test b::slow ... ok <12.345s>',
    'test a::quick ... ok <0.200s>',
    'test a::fail ... FAILED <3.000s>',
    'test a::ignored ... ignored',
    'test a::plain ... ok',
    '',
  ].join('\n');
  assert.deepEqual(parseTimes(output), { 'b::slow': 12.345, 'a::quick': 0.2 });
});

test('weights merge the shard time files and only list tests of two seconds or more', () => {
  const files = [{ 'b::slow': 12.345, 'a::quick': 0.2 }, { 'a::slow': 2, 'a::second': 1.9 }];
  assert.equal(weightsFromTimes(files), `${JSON.stringify({ seconds: { 'a::slow': 2, 'b::slow': 12.3 } }, null, 2)}\n`);
});

test('the shard runs the one test executable cargo built, from its package directory', () => {
  const message = (fields) => JSON.stringify({ reason: 'compiler-artifact', ...fields });
  const messages = [
    message({ profile: { test: false }, executable: null, manifest_path: '/w/crates/x/Cargo.toml' }),
    message({ profile: { test: true }, executable: '/w/target/debug/deps/app-1', manifest_path: '/w/src-tauri/Cargo.toml' }),
    JSON.stringify({ reason: 'build-finished', success: true }),
    '',
  ].join('\n');
  assert.deepEqual(parseExecutable(messages), { executable: '/w/target/debug/deps/app-1', directory: '/w/src-tauri' });
  assert.throws(() => parseExecutable(messages.split('\n').slice(0, 1).join('\n')), /expected one test executable, cargo built 0/u);
});

test('the committed weights name tests in sorted order with positive seconds', () => {
  const weights = loadWeights();
  const names = Object.keys(weights);
  assert.ok(names.length > 0);
  assert.deepEqual(names, [...names].sort());
  for (const name of names) assert.ok(weights[name] >= 2, name);
});

const groups = {
  acceptance: { shards: 2, prefixes: ['investigations::worker::tests::acceptance::'] },
  core: { shards: 1, prefixes: ['investigations::', 'jobs::'] },
};

test('a test belongs to the group with its longest matching prefix, and an unassigned test fails (#1924)', () => {
  const owners = groupOf(['investigations::worker::tests::acceptance::a', 'investigations::worker::tests::b', 'jobs::c'], groups);
  assert.deepEqual([...owners.values()], ['acceptance', 'core', 'core']);
  assert.throws(() => groupOf(['jobs::c', 'paths::tests::only'], groups), /1 tests are not assigned to a group.*paths::tests::only/u);
});

test('the groups file refuses a prefix in two groups, a bad prefix and a bad shard count', () => {
  const file = (body) => {
    const target = path.join(mkdtempSync(path.join(tmpdir(), 'groups-')), 'groups.json');
    writeFileSync(target, JSON.stringify({ groups: body }));
    return target;
  };
  assert.deepEqual(loadGroups(file(groups)), groups);
  assert.throws(() => loadGroups(file({ a: { shards: 1, prefixes: ['x::'] }, b: { shards: 1, prefixes: ['x::'] } })), /in groups a and b/u);
  assert.throws(() => loadGroups(file({ a: { shards: 1, prefixes: ['x'] } })), /must end with ::/u);
  assert.throws(() => loadGroups(file({ a: { shards: 0, prefixes: ['x::'] } })), /positive shard count/u);
  assert.throws(() => loadGroups(file({ a: { shards: 1, prefixes: [] } })), /no prefixes/u);
});

test('coverage passes only when every listed test ran in exactly one shard of every group (#1924)', () => {
  const acceptance = ['investigations::worker::tests::acceptance::a', 'investigations::worker::tests::acceptance::b'];
  const core = ['jobs::c', 'jobs::d'];
  const shard = (group, index, count, listed, selected) => ({ group, shard: `${index}/${count}`, listed, selected });
  const good = [
    shard('acceptance', 1, 2, [...acceptance, 'jobs::c'], [acceptance[0]]),
    shard('acceptance', 2, 2, [...acceptance, 'jobs::c'], [acceptance[1]]),
    shard('core', 1, 1, core, core),
  ];
  assert.deepEqual(verifyCoverage(good, groups), []);
  // A module left out of its own group's build: listed elsewhere, never run.
  const missing = [good[0], good[1], shard('core', 1, 1, ['jobs::d'], ['jobs::d'])];
  assert.deepEqual(verifyCoverage(missing, groups), ['jobs::c ran in 0 shards']);
  // A test two groups both select.
  const twice = [shard('acceptance', 1, 2, acceptance, acceptance), good[1], good[2]];
  assert.deepEqual(verifyCoverage(twice, groups), [
    'investigations::worker::tests::acceptance::b ran in 2 shards (acceptance 1/2, acceptance 2/2)',
  ]);
  // A lost shard file.
  assert.match(verifyCoverage(good.slice(1), groups)[0], /group acceptance reported shards \[2\/2\], expected \[1\/2,2\/2\]/u);
});

test('the committed groups cover every top-level test module once', () => {
  const committed = loadGroups();
  assert.deepEqual(Object.keys(committed).sort(), ['acceptance', 'core']);
  const prefixes = Object.values(committed).flatMap(({ prefixes: list }) => list);
  assert.deepEqual(prefixes.filter((prefix) => prefix.split('::').length === 2), [...committed.core.prefixes].sort());
});
