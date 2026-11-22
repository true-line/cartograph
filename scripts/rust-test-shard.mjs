#!/usr/bin/env node
// Runs one shard of a Rust test binary's tests under plain libtest (#1924).
//
// The `app` unit-test binary holds most of the Rust job's wall time. Splitting
// its tests across parallel CI jobs divides that time without changing how
// any test runs: each shard is one `cargo test` process with libtest's own
// threading, so in-process fixtures behave exactly as in an unsharded run.
//
// Every shard assigns the binary's sorted test list the same way, longest
// measured test first to the least loaded shard (rust-test-weights.json), so
// the shards partition the list by construction and the slow tests spread.
// After the run the shard checks libtest's own totals against its selection:
// a test that was selected and did not run (or one that ran unselected) fails
// the shard instead of passing silently.
//
// Usage: rust-test-shard.mjs <index>/<count> [--group <name>] -- <cargo test target arguments>
//   e.g. rust-test-shard.mjs 2/4 --group core -- -p app --bin app
//
// With --group, the app binary is built with APP_TEST_GROUP=<name>, which
// leaves the other group's test modules out (src-tauri/build.rs), and the
// shard runs only the tests app-test-groups.json assigns to that group. A
// test the file assigns to no group fails every shard. Each shard writes the
// tests its binary listed and the ones it selected to SHARD_LIST_FILE; the
// coverage job (--verify-coverage) checks that every test listed by any
// group's build ran in exactly one shard.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

// Test groups of the app binary (#1924): each group's shards build only its
// tests. A test belongs to the group with the longest matching name prefix.
export const GROUPS_PATH = fileURLToPath(new URL('./app-test-groups.json', import.meta.url));

export function loadGroups(file = GROUPS_PATH) {
  const { groups } = JSON.parse(readFileSync(file, 'utf8'));
  const owners = new Map();
  for (const [name, { shards, prefixes }] of Object.entries(groups)) {
    if (!/^[a-z]+$/u.test(name)) throw new Error(`group name ${JSON.stringify(name)} is not lowercase letters`);
    if (!Number.isInteger(shards) || shards < 1) throw new Error(`group ${name} needs a positive shard count`);
    if (!Array.isArray(prefixes) || prefixes.length === 0) throw new Error(`group ${name} lists no prefixes`);
    for (const prefix of prefixes) {
      if (!prefix.endsWith('::')) throw new Error(`prefix ${JSON.stringify(prefix)} must end with ::`);
      if (owners.has(prefix)) throw new Error(`prefix ${prefix} is in groups ${owners.get(prefix)} and ${name}`);
      owners.set(prefix, name);
    }
  }
  return groups;
}

// The group of each name; throws on a name no prefix covers.
export function groupOf(names, groups) {
  const prefixes = Object.entries(groups)
    .flatMap(([group, { prefixes: list }]) => list.map((prefix) => ({ prefix, group })))
    .sort((a, b) => b.prefix.length - a.prefix.length);
  const unassigned = [];
  const result = new Map();
  for (const name of names) {
    const match = prefixes.find(({ prefix }) => name.startsWith(prefix));
    if (match) result.set(name, match.group);
    else unassigned.push(name);
  }
  if (unassigned.length > 0) {
    throw new Error(
      `${unassigned.length} tests are not assigned to a group in scripts/app-test-groups.json, e.g. ${unassigned.slice(0, 5).join(', ')}`,
    );
  }
  return result;
}

// Shard list files ({group, shard, listed, selected}) from every shard of one
// run: every test any group's build listed ran in exactly one shard, and each
// group's shards are all present.
export function verifyCoverage(files, groups) {
  const problems = [];
  const listed = new Set();
  const ran = new Map();
  const seen = new Map();
  for (const { group, shard, listed: names, selected } of files) {
    if (!(group in groups)) problems.push(`shard file for unknown group ${group}`);
    seen.set(group, [...(seen.get(group) ?? []), shard]);
    for (const name of names) listed.add(name);
    for (const name of selected) ran.set(name, [...(ran.get(name) ?? []), `${group} ${shard}`]);
  }
  for (const [group, { shards }] of Object.entries(groups)) {
    const got = (seen.get(group) ?? []).sort().join(',');
    const want = Array.from({ length: shards }, (_, index) => `${index + 1}/${shards}`).sort().join(',');
    if (got !== want) problems.push(`group ${group} reported shards [${got}], expected [${want}]`);
  }
  for (const name of [...listed].sort()) {
    const where = ran.get(name) ?? [];
    if (where.length !== 1) problems.push(`${name} ran in ${where.length} shards${where.length ? ` (${where.join(', ')})` : ''}`);
  }
  for (const name of ran.keys()) if (!listed.has(name)) problems.push(`${name} ran but no build listed it`);
  return problems;
}

export function parseShard(text) {
  const match = /^([1-9]\d*)\/([1-9]\d*)$/u.exec(text ?? '');
  if (!match) throw new Error(`shard must be <index>/<count>, got ${JSON.stringify(text)}`);
  const [index, count] = [Number(match[1]), Number(match[2])];
  if (index > count) throw new Error(`shard index ${index} exceeds count ${count}`);
  return { index, count };
}

// `cargo test -- --list --format terse` prints one `<name>: test` line per
// test (ignored ones included) and `<name>: benchmark` per benchmark.
export function parseTestList(output) {
  const names = [];
  for (const line of output.split('\n')) {
    const match = /^(.+): test$/u.exec(line.trimEnd());
    if (match) names.push(match[1]);
  }
  const sorted = [...new Set(names)].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  if (sorted.length !== names.length) throw new Error('test list holds duplicate names');
  return sorted;
}

// Measured seconds per test of the app binary (#1924), so the slow replay
// tests spread across shards instead of landing together by name order.
// Tests absent from the file weigh DEFAULT_WEIGHT. Each CI shard writes the
// times libtest reported for its tests to SHARD_TIMES_FILE, which ci.yml keeps
// as an `app-test-times-*` artifact; regenerate the weights from all of one
// run's files (one per shard) with
//   node scripts/rust-test-shard.mjs --weights-from times-1.json times-2.json ... > scripts/rust-test-weights.json
export const WEIGHTS_PATH = fileURLToPath(new URL('./rust-test-weights.json', import.meta.url));
export const DEFAULT_WEIGHT = 0.2;
// Faster tests stay at the default: listing them would only grow the file.
const MIN_LISTED_SECONDS = 2;

export function loadWeights(file = WEIGHTS_PATH) {
  return existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')).seconds : {};
}

// libtest's `--report-time` lines: `test <name> ... ok <1.234s>`.
export function parseTimes(output) {
  const times = {};
  for (const [, name, seconds] of output.matchAll(/^test (\S+) \.\.\. ok <(\d+(?:\.\d+)?)s>$/gmu)) {
    times[name] = Number(seconds);
  }
  return times;
}

// Shard time files ({name: seconds}) to a weights file.
export function weightsFromTimes(files) {
  const seconds = {};
  for (const times of files) {
    for (const [name, value] of Object.entries(times)) {
      if (value >= MIN_LISTED_SECONDS) seconds[name] = Math.round(value * 10) / 10;
    }
  }
  const sorted = Object.fromEntries(Object.keys(seconds).sort().map((name) => [name, seconds[name]]));
  return `${JSON.stringify({ seconds: sorted }, null, 2)}\n`;
}

// Every shard computes the same assignment from the same sorted list, so the
// shards partition it by construction. Longest first, each test goes to the
// least loaded shard (ties to the lowest index); with equal weights this is
// plain round-robin over the sorted names.
export function selectShard(names, { index, count }, weights = {}) {
  const weight = (name) => weights[name] ?? DEFAULT_WEIGHT;
  const order = names
    .map((name, position) => ({ name, position, weight: weight(name) }))
    .sort((a, b) => b.weight - a.weight || a.position - b.position);
  const load = Array(count).fill(0);
  const shardOf = new Map();
  for (const { name, weight: seconds } of order) {
    let target = 0;
    for (let shard = 1; shard < count; shard += 1) if (load[shard] < load[target]) target = shard;
    load[target] += seconds;
    shardOf.set(name, target);
  }
  return names.filter((name) => shardOf.get(name) === index - 1);
}

// Sums every `test result:` line of a libtest run.
export function parseTotals(output) {
  const totals = { ran: 0, failed: 0, results: 0 };
  const line = /^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; \d+ filtered out/gmu;
  for (const [, passed, failed, ignored, measured] of output.matchAll(line)) {
    totals.ran += Number(passed) + Number(failed) + Number(ignored) + Number(measured);
    totals.failed += Number(failed);
    totals.results += 1;
  }
  return totals;
}

export function verifyRun(selected, output, status) {
  const totals = parseTotals(output);
  if (totals.results !== 1) {
    return `expected one test binary result, saw ${totals.results}`;
  }
  if (totals.ran !== selected.length) {
    return `selected ${selected.length} tests but libtest accounted for ${totals.ran}`;
  }
  if (status !== 0 || totals.failed !== 0) {
    return `the test binary exited ${status} with ${totals.failed} failed`;
  }
  return null;
}

function list(cargoArguments, env) {
  const listed = spawnSync('cargo', ['test', ...cargoArguments, '--', '--list', '--format', 'terse'], {
    encoding: 'utf8',
    env,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  if (listed.status !== 0) throw new Error(`cargo test --list exited ${listed.status}`);
  return parseTestList(listed.stdout);
}

// The one test executable `cargo test` built for these arguments, and the
// package directory cargo runs it from. `--list` built it already, so this
// compiles nothing.
export function parseExecutable(messages) {
  const found = [];
  for (const line of messages.split('\n')) {
    if (!line.startsWith('{')) continue;
    const message = JSON.parse(line);
    if (message.reason === 'compiler-artifact' && message.profile?.test && message.executable) {
      found.push({ executable: message.executable, directory: path.dirname(message.manifest_path) });
    }
  }
  if (found.length !== 1) throw new Error(`expected one test executable, cargo built ${found.length}`);
  return found[0];
}

function executable(cargoArguments, env) {
  const built = spawnSync('cargo', ['test', ...cargoArguments, '--no-run', '--message-format=json'], {
    encoding: 'utf8',
    env,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  if (built.status !== 0) throw new Error(`cargo test --no-run exited ${built.status}`);
  return parseExecutable(built.stdout);
}

// The binary runs directly, from its package directory as cargo would run it,
// so that RUSTC_BOOTSTRAP reaches only libtest: given to cargo, it would
// change the build and recompile every crate. libtest needs it for
// `--report-time`, which times each test for the shard weights.
function run({ executable: binary, directory }, selected) {
  // libtest takes each selected name as an exact filter. A shard of a few
  // hundred names is tens of kilobytes, far inside Linux's argument limit.
  return new Promise((resolve, reject) => {
    const child = spawn(binary, ['-Z', 'unstable-options', '--report-time', '--exact', ...selected], {
      cwd: directory,
      env: { ...process.env, RUSTC_BOOTSTRAP: '1' },
      stdio: ['ignore', 'pipe', 'inherit'],
    });
    let output = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      output += chunk;
      process.stdout.write(chunk);
    });
    child.on('error', reject);
    child.on('close', (status) => resolve({ output, status: status ?? 1 }));
  });
}

async function main(argv) {
  if (argv[0] === '--weights-from' && argv.length >= 2) {
    process.stdout.write(weightsFromTimes(argv.slice(1).map((file) => JSON.parse(readFileSync(file, 'utf8')))));
    return;
  }
  if (argv[0] === '--verify-coverage' && argv.length >= 2) {
    const files = argv.slice(1).map((file) => JSON.parse(readFileSync(file, 'utf8')));
    const problems = verifyCoverage(files, loadGroups());
    if (problems.length > 0) throw new Error(`app test coverage:\n${problems.join('\n')}`);
    const total = new Set(files.flatMap(({ listed }) => listed)).size;
    console.log(`app test coverage: ${total} tests, each ran in exactly one of ${files.length} shards`);
    return;
  }
  const separator = argv.indexOf('--');
  const options = argv.slice(1, separator);
  const grouped = options.length === 2 && options[0] === '--group';
  if (separator < 1 || argv.length < separator + 2 || !(options.length === 0 || grouped)) {
    throw new Error('usage: rust-test-shard.mjs <index>/<count> [--group <name>] -- <cargo test target arguments>');
  }
  const shard = parseShard(argv[0]);
  const cargoArguments = argv.slice(separator + 1);
  const env = { ...process.env };
  let names;
  let label = `shard ${shard.index}/${shard.count}`;
  if (grouped) {
    const group = options[1];
    const groups = loadGroups();
    if (!(group in groups)) throw new Error(`unknown test group ${group}`);
    if (groups[group].shards !== shard.count) {
      throw new Error(`group ${group} has ${groups[group].shards} shards, not ${shard.count}`);
    }
    env.APP_TEST_GROUP = group;
    label = `${group} ${label}`;
    const listed = list(cargoArguments, env);
    const owners = groupOf(listed, groups);
    names = listed.filter((name) => owners.get(name) === group);
    if (process.env.SHARD_LIST_FILE) {
      const selected = selectShard(names, shard, loadWeights());
      writeFileSync(
        process.env.SHARD_LIST_FILE,
        `${JSON.stringify({ group, shard: `${shard.index}/${shard.count}`, listed, selected }, null, 2)}\n`,
      );
    }
  } else {
    names = list(cargoArguments, env);
  }
  const selected = selectShard(names, shard, loadWeights());
  if (selected.length === 0) throw new Error(`${label} selects no test of ${names.length}`);
  console.log(`${label}: ${selected.length} of ${names.length} tests`);
  const { output, status } = await run(executable(cargoArguments, env), selected);
  if (process.env.SHARD_TIMES_FILE) {
    writeFileSync(process.env.SHARD_TIMES_FILE, `${JSON.stringify(parseTimes(output), null, 2)}\n`);
  }
  const problem = verifyRun(selected, output, status);
  if (problem) throw new Error(`${label}: ${problem}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
