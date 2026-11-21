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
// Usage: rust-test-shard.mjs <index>/<count> -- <cargo test target arguments>
//   e.g. rust-test-shard.mjs 2/4 -- -p app --bin app
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';

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
// Tests absent from the file weigh DEFAULT_WEIGHT. Regenerate with
//   RUSTC_BOOTSTRAP=1 target/debug/deps/app-<hash> -Z unstable-options \
//     --report-time --format json > report.json   (from src-tauri/)
//   node scripts/rust-test-shard.mjs --weights-from report.json > scripts/rust-test-weights.json
export const WEIGHTS_PATH = fileURLToPath(new URL('./rust-test-weights.json', import.meta.url));
export const DEFAULT_WEIGHT = 0.2;
// Faster tests stay at the default: listing them would only grow the file.
const MIN_LISTED_SECONDS = 2;

export function loadWeights(file = WEIGHTS_PATH) {
  return existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')).seconds : {};
}

// libtest's JSON report (one event per line) to a weights file.
export function weightsFromReport(report) {
  const seconds = {};
  for (const line of report.split('\n')) {
    if (!line.trim().startsWith('{')) continue;
    const event = JSON.parse(line);
    if (event.type === 'test' && event.event === 'ok' && event.exec_time >= MIN_LISTED_SECONDS) {
      seconds[event.name] = Math.round(event.exec_time * 10) / 10;
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
    return `cargo test exited ${status} with ${totals.failed} failed`;
  }
  return null;
}

function list(cargoArguments) {
  const listed = spawnSync('cargo', ['test', ...cargoArguments, '--', '--list', '--format', 'terse'], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  if (listed.status !== 0) throw new Error(`cargo test --list exited ${listed.status}`);
  return parseTestList(listed.stdout);
}

function run(cargoArguments, selected) {
  // libtest takes each selected name as an exact filter. A shard of a few
  // hundred names is tens of kilobytes, far inside Linux's argument limit.
  return new Promise((resolve, reject) => {
    const child = spawn('cargo', ['test', ...cargoArguments, '--', '--exact', ...selected], {
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
  if (argv[0] === '--weights-from' && argv.length === 2) {
    process.stdout.write(weightsFromReport(readFileSync(argv[1], 'utf8')));
    return;
  }
  const separator = argv.indexOf('--');
  if (separator !== 1 || argv.length < 3) {
    throw new Error('usage: rust-test-shard.mjs <index>/<count> -- <cargo test target arguments>');
  }
  const shard = parseShard(argv[0]);
  const cargoArguments = argv.slice(separator + 1);
  const names = list(cargoArguments);
  const selected = selectShard(names, shard, loadWeights());
  if (selected.length === 0) throw new Error(`shard ${shard.index}/${shard.count} selects no test of ${names.length}`);
  console.log(`shard ${shard.index}/${shard.count}: ${selected.length} of ${names.length} tests`);
  const { output, status } = await run(cargoArguments, selected);
  const problem = verifyRun(selected, output, status);
  if (problem) throw new Error(`shard ${shard.index}/${shard.count}: ${problem}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
